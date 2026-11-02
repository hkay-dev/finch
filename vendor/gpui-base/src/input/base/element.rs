use crate::input::{InputExtras as _, InputModeKind};
use gpui::{BorderStyle, Corners};
use gpui::Half;
use gpui::{
    AnyElement, App, Bounds, Edges, Element, ElementId, ElementInputHandler, Entity,
    GlobalElementId,
};
use gpui::{
    HighlightStyle, Hitbox, HitboxBehavior, Hsla, InteractiveElement, IntoElement, LayoutId,
    LongPressEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, ParentElement as _, Path, Pixels,
    Point, Position, ShapedLine, SharedString, Size, Style, Styled as _, TextAlign, TextRun,
    TextStyle, TouchDragEvent, TouchPhase, UnderlineStyle, Window, fill, outline, point, px, relative, size,
};
use ropey::Rope;
use smallvec::SmallVec;
use sum_tree::Bias;
use std::{ops::Range, rc::Rc, sync::Arc};

/// Run `change`, which may re-wrap rows, and scroll so the line at the top of the viewport stays
/// where it was.
fn hold_top_line<M: InputModeKind>(
    state: &mut InputBaseState<M>,
    line_height: Pixels,
    change: impl FnOnce(&mut InputBaseState<M>),
) {
    let mut scroll = state
        .deferred_scroll_offset
        .unwrap_or_else(|| state.scroll_handle.offset());
    if let Some(glide_y) = state.glide_y {
        scroll.y = glide_y;
    }
    let top = ((-scroll.y).max(px(0.)) / line_height) as usize;
    let top = top.min(state.display_map.display_row_count().saturating_sub(1));
    let anchor = state.display_map.display_row_to_buffer_line(top);
    let before = state.display_map.buffer_line_to_display_row(anchor);
    state.display_map.set_wrap_focus(anchor);
    change(state);
    let after = state.display_map.buffer_line_to_display_row(anchor);
    if top > 0 && after != before {
        let mut offset = scroll;
        let shift = line_height * after as f32 - line_height * before as f32;
        offset.y -= shift;
        if let Some(glide_y) = state.glide_y.as_mut() {
            *glide_y -= shift;
            if let Some(glide) = state.reveal_glide.as_mut() {
                glide.shift(-shift.as_f32());
            }
        } else if state.deferred_scroll_offset.is_some() {
            state.deferred_scroll_offset = Some(offset);
        } else {
            state.scroll_handle.set_offset(offset);
        }
    }
}

/// Run `change`, which re-lays the rows for a new line height (a font size change), and scroll so
/// the line at the top of the viewport stays at the top, the same distance into its first row.
fn hold_top_line_resized<M: InputModeKind>(
    state: &mut InputBaseState<M>,
    old_line_height: Pixels,
    line_height: Pixels,
    change: impl FnOnce(&mut InputBaseState<M>),
) {
    let mut scroll = state
        .deferred_scroll_offset
        .unwrap_or_else(|| state.scroll_handle.offset());
    if let Some(glide_y) = state.glide_y {
        scroll.y = glide_y;
    }
    let down = (-scroll.y).max(px(0.));
    let top = ((down / old_line_height) as usize).min(state.display_map.display_row_count().saturating_sub(1));
    let anchor = state.display_map.display_row_to_buffer_line(top);
    let before = state.display_map.buffer_line_to_display_row(anchor);
    let into = (down - old_line_height * before as f32).max(px(0.));
    state.display_map.set_wrap_focus(anchor);
    change(state);
    if down <= px(0.) {
        return;
    }
    let after = state.display_map.buffer_line_to_display_row(anchor);
    let mut offset = scroll;
    offset.y = -(line_height * after as f32 + into * (line_height / old_line_height));
    let shift = offset.y - scroll.y;
    if let Some(glide_y) = state.glide_y.as_mut() {
        *glide_y += shift;
        if let Some(glide) = state.reveal_glide.as_mut() {
            glide.shift(shift.as_f32());
        }
    } else if state.deferred_scroll_offset.is_some() {
        state.deferred_scroll_offset = Some(offset);
    } else {
        state.scroll_handle.set_offset(offset);
    }
}

impl<M: InputModeKind> InputBaseState<M> {
    /// The width the editor element had when it last drew, if it has.
    pub fn drawn_width(&self) -> Option<Pixels> {
        self.last_layout.as_ref().map(|layout| layout.content_width)
    }

    /// Where the text column drew last: the x its text starts at from the pane's left (gutter and
    /// column offset), the column offset alone, and the wrap width (`None`: no wrapping).
    pub fn drawn_column(&self) -> Option<(Pixels, Pixels, Option<Pixels>)> {
        self.last_layout.as_ref().map(|layout| (layout.line_number_width, layout.column_offset, layout.wrap_width))
    }

    /// Lets go of the shaped lines of the last drawn screen and the highlight styles queried for
    /// it, keeping its measures (line height, widths, wrap width, caret box), which `prelayout`,
    /// `drawn_width` and the resize checks read. Nothing reads either while the editor is hidden;
    /// the next draw shapes the lines fresh and asks the highlighter once for its screen.
    pub fn release_layout(&mut self) {
        *self.highlight_cache.get_mut() = None;
        *self.search_scope_cache.get_mut() = None;
        if let Some(layout) = self.last_layout.as_mut() {
            if !layout.lines.is_empty() {
                layout.lines = Rc::new(Vec::new());
                layout.visible_buffer_lines = Vec::new();
                layout.visible_line_byte_offsets = Vec::new();
            }
        }
    }

    /// Wraps a hidden editor for an element `width` wide in `look`, for up to `budget` once the
    /// width and look are set, so the frame that shows it finds its rows ready. A look with another
    /// font or line height than the editor last drew with is applied the way `prepaint` would
    /// (`set_font`, the top line held). Returns whether rows still wait to be wrapped. An editor
    /// that has never drawn is left alone.
    pub fn prelayout(&mut self, width: Pixels, look: &DrawnLook, budget: std::time::Duration, window: &mut Window, cx: &mut App) -> bool {
        let Some(old_line_height) = self.last_layout.as_ref().map(|layout| layout.line_height) else {
            return false;
        };
        if !self.is_multi_line() || !self.soft_wrap {
            return false;
        }
        let gutter = TextElement::<M>::layout_line_numbers(self, line_number_len(self.text.lines_len()), window);
        let advance = {
            let font_id = window.text_system().resolve_font(&look.font);
            window.text_system().layout_width(font_id, look.size, '0')
        };
        let wrap_width = self.text_column(width, gutter, advance).0;
        // Wrapped already at this width, font and line height, with nothing waiting: a reading
        // width holds the wrap width still while the pane resizes, so there is nothing to redo.
        if self.display_map.wrap_width() == wrap_width
            && old_line_height == look.line_height
            && self.drawn_font.as_ref().is_some_and(|(font, size)| *font == look.font && *size == look.size)
            && !self.display_map.wrap_pending()
        {
            return false;
        }
        let wrap = |state: &mut InputBaseState<M>| {
            state.display_map.set_font(look.font.clone(), look.size, cx);
            state.display_map.on_layout_changed(wrap_width, cx);
            state.display_map.step_wrap(budget, cx);
        };
        if old_line_height != look.line_height {
            hold_top_line_resized(self, old_line_height, look.line_height, wrap);
        } else {
            hold_top_line(self, look.line_height, wrap);
        }
        self.drawn_font = Some((look.font.clone(), look.size));
        if let Some(layout) = self.last_layout.as_mut() {
            layout.wrap_width = wrap_width;
            layout.line_height = look.line_height;
        }
        self.display_map.wrap_pending()
    }

    /// The font, text size and line height of the editor's last draw, if it has drawn.
    pub fn drawn_look(&self) -> Option<DrawnLook> {
        let (font, size) = self.drawn_font.clone()?;
        let line_height = self.last_layout.as_ref()?.line_height;
        Some(DrawnLook { font, size, line_height })
    }

    /// The text size of the editor's last draw, if it has drawn.
    pub fn drawn_size(&self) -> Option<Pixels> {
        self.drawn_font.as_ref().map(|(_, size)| *size)
    }
}

/// A pane wider than this holds its text to `reading_measure_wide` characters (code in a very wide
/// window); `reading_measure` holds in every pane.
pub(super) const READING_WIDE_PANE: Pixels = px(1400.);

impl<M: InputModeKind> InputBaseState<M> {
    /// The one place the wrap width is decided, shared by `prelayout` (hidden tabs), `prepaint`
    /// and everything that wraps: for a pane `pane_width` wide with a gutter `gutter` wide and a
    /// font whose "0" is `advance` wide, returns the width to wrap at (`None`: no wrapping) and the
    /// x offset that centers the column in the room right of the gutter (zero when the column
    /// doesn't apply). Without a reading measure, or when the pane is no wider than it, the width
    /// is the room left of the gutter and today's. The width only changes when the room is
    /// narrower than the measure, so a wider window or a panel sliding past it never re-wraps.
    pub(super) fn text_column(&self, pane_width: Pixels, gutter: Pixels, advance: Pixels) -> (Option<Pixels>, Pixels) {
        if !self.is_multi_line() || !self.soft_wrap {
            return (None, px(0.));
        }
        let room = pane_width - gutter - RIGHT_MARGIN;
        let chars = self
            .reading_measure
            .or_else(|| (pane_width > READING_WIDE_PANE).then_some(self.reading_measure_wide).flatten())
            .filter(|chars| *chars > 0.);
        match chars {
            Some(chars) if advance * chars < room => {
                let measure = advance * chars;
                let offset = if self.reading_centered { (room - measure) * 0.5 } else { px(0.) };
                (Some(measure), offset)
            }
            _ => (Some(room), px(0.)),
        }
    }
}

/// What a draw set an editor's text in, which `prelayout` gives to editors that are hidden.
#[derive(Clone)]
pub struct DrawnLook {
    pub font: gpui::Font,
    pub size: Pixels,
    pub line_height: Pixels,
}

/// How long a frame spends soft-wrapping a big text; the rest of the pass runs on later frames.
const WRAP_SLICE: std::time::Duration = std::time::Duration::from_millis(4);

use crate::{
    Scrollbar,
    input::{
        RopeExt as _,
        FoldRange,
        apca_contrast::{apca_contrast, ensure_minimum_contrast},
        invisibles,
        caret_layer::{Band, Number},
        blink_cursor::{CURSOR_WIDTH, CaretGlide, Instant},
        display_map::LineLayout,
        system_look,
    },
};

use super::{
    InputBaseState, PrepaintCaretGeometry, RangeDecorationStyle, TextDecoration,
    state::{PATCH_ADDED, PATCH_FILE, PATCH_HEADER, PATCH_HUNK, PATCH_REMOVED, PatchRows},
    layout::{LastLayout, WhitespaceIndicators},
    mode::LayoutMode,
    tab_stops::{expand_tabs, expanded_width, tab_stop_width},
};

fn diagnostic_highlight_style(
    severity: crate::input::DiagnosticSeverity,
    colors: crate::input::DiagnosticColors,
) -> HighlightStyle {
    let color = match severity {
        crate::input::DiagnosticSeverity::Error => colors.error,
        crate::input::DiagnosticSeverity::Warning => colors.warning,
        crate::input::DiagnosticSeverity::Info => colors.info,
        crate::input::DiagnosticSeverity::Hint => colors.hint,
    };
    HighlightStyle {
        underline: Some(UnderlineStyle {
            color: Some(color),
            thickness: px(1.),
            wavy: true,
        }),
        ..Default::default()
    }
}

const BOTTOM_MARGIN_ROWS: usize = 3;
pub(super) const RIGHT_MARGIN: Pixels = px(10.);
pub(super) const LINE_NUMBER_RIGHT_MARGIN: Pixels = px(6.);
/// Selection and match corners round to this share of the line height, as in Zed.
const SELECTION_CORNER_RADIUS: f32 = 0.15;
/// How much of the match wash Find in selection's scope shows, so it reads as a field under the hits.
const SEARCH_SCOPE_FILL: f32 = 0.5;
/// The hollow caret's box: its alpha at rest, and its corner radius.
const HOLLOW_ALPHA: f32 = 0.6;
const HOLLOW_RADIUS: Pixels = px(2.);
/// A hidden-character tag's corner radius, the app's `RADIUS_TINT`.
const HIDDEN_TAG_RADIUS: Pixels = px(3.);
const FOLD_ICON_WIDTH: Pixels = px(14.);
const FOLD_ICON_HITBOX_WIDTH: Pixels = px(18.);
const MAX_HIGHLIGHT_LINE_LENGTH: usize = 10_000;
const MIN_LINE_NUMBER_DIGITS: usize = 3;
/// The gutter's text glide: the caret's critically damped spring (62 rad/s, about 100 ms).
const GUTTER_GLIDE: crate::motion::Spring =
    crate::motion::Spring::new(std::time::Duration::from_millis(100)).with_epsilon(0.1);
const MAX_LINE_NUMBER_DIGITS: usize = 7;
/// The least width of the gutter's right-click target, so a gutter with nothing left in it still
/// takes the click and the numbers can be turned back on.
const GUTTER_MENU_EDGE: Pixels = px(12.);
const MAX_DISPLAYED_LINE_NUMBER: usize = 9_999_999;

fn line_number_len(total_lines: usize) -> usize {
    (total_lines.max(1).ilog10() as usize + 1).clamp(MIN_LINE_NUMBER_DIGITS, MAX_LINE_NUMBER_DIGITS)
}

fn displayed_line_number(number: usize) -> usize {
    number.min(MAX_DISPLAYED_LINE_NUMBER)
}

/// The gap between a patch's old and new number columns.
const PATCH_COLUMN_GAP: Pixels = px(10.);

/// The width of one figure column of a patch's gutter, `digits` wide.
fn patch_column_width(digits: usize, window: &mut Window) -> Pixels {
    window
        .text_system()
        .shape_line(
            "0".repeat(digits).into(),
            system_look::GUTTER_SIZE,
            &[TextRun {
                len: digits,
                font: system_look::gutter_font(),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        )
        .width
}

/// What a patch row's whole line is washed with: green for added, red for removed, a bar for
/// the file's header lines (stronger on the first), a faint blue for a hunk line. `ink` is the
/// text's color, so the bars follow the theme; nothing for plain lines.
fn patch_wash(kind: u8, ink: Hsla) -> Option<Hsla> {
    Some(match kind {
        PATCH_ADDED => gpui::hsla(0.37, 0.62, 0.46, 0.17),
        PATCH_REMOVED => gpui::hsla(0.0, 0.72, 0.52, 0.16),
        PATCH_FILE => ink.opacity(0.12),
        PATCH_HEADER => ink.opacity(0.07),
        PATCH_HUNK => gpui::hsla(0.58, 0.55, 0.55, 0.09),
        _ => return None,
    })
}

/// A patch's map while it is the text's own, with what the prepaint made of it.
struct PatchPaint {
    rows: Rc<PatchRows>,
    /// The old-side number of each visible line, empty where it has none.
    old: Vec<ShapedLine>,
    /// Where the old column's right edge sits, from the gutter's left edge.
    old_right: Pixels,
    /// The file icons of the visible header bars, prepainted.
    icons: Vec<AnyElement>,
}
const FOLD_CHEVRON_RIGHT_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m9 18 6-6-6-6"/></svg>"#;
const FOLD_CHEVRON_DOWN_SVG: &[u8] = br#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="m6 9 6 6 6-6"/></svg>"#;

fn compose_decorations(
    mut styles: Vec<(Range<usize>, HighlightStyle)>,
    decorations: impl IntoIterator<Item = (Range<usize>, HighlightStyle)>,
    visible_byte_range: Range<usize>,
) -> Option<Vec<(Range<usize>, HighlightStyle)>> {
    let mut visible_decorations = decorations
        .into_iter()
        .filter_map(|(range, style)| {
            let range =
                range.start.max(visible_byte_range.start)..range.end.min(visible_byte_range.end);
            (!range.is_empty()).then_some((range, style))
        })
        .peekable();

    if visible_decorations.peek().is_none() {
        return (!styles.is_empty()).then_some(styles);
    }
    if styles.is_empty() {
        styles.push((visible_byte_range.clone(), HighlightStyle::default()));
    }

    // Decorations are application-authored overrides and must win over syntax
    // and semantic highlighting when both set the same style property.
    Some(gpui::combine_highlights(styles, visible_decorations).collect())
}

fn compose_decoration_collections<'a>(
    mut styles: Vec<(Range<usize>, HighlightStyle)>,
    collections: impl IntoIterator<Item = &'a [TextDecoration]>,
    visible_byte_range: Range<usize>,
) -> Option<Vec<(Range<usize>, HighlightStyle)>> {
    // Apply collections in reverse creation order so the first collection is
    // composed last and retains its documented precedence.
    let collections = collections.into_iter().collect::<Vec<_>>();
    for decorations in collections.into_iter().rev() {
        styles = compose_decorations(
            styles,
            decorations
                .iter()
                .map(|decoration| (decoration.range.clone(), decoration.style)),
            visible_byte_range.clone(),
        )
        .unwrap_or_default();
    }

    (!styles.is_empty()).then_some(styles)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct EditorScrollbarLayout {
    bounds: Bounds<Pixels>,
    scroll_size: Size<Pixels>,
    // Excludes editor padding from scroll range while the track still includes it.
    scroll_viewport_size: Size<Pixels>,
}

/// What the unwrapped width of the longest line depends on. While it holds,
/// prepaint reuses the stored width instead of copying the line and asking
/// the text system for it again on every frame. The line's own text (a hash)
/// stands in for the document revision, so an edit on another row keeps the
/// stored width and an edit on this row re-measures it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct LongestLineKey {
    content_hash: u64,
    row: usize,
    len: usize,
    font: gpui::Font,
    text_size: Pixels,
    wrap_width: Option<Pixels>,
    tab_size: usize,
}

#[cfg(test)]
thread_local! {
    static LONGEST_LINE_HASHES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static LONGEST_LINE_SHAPES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Hash of a line's text, independent of how the rope chunks it.
fn line_content_hash(line: &ropey::RopeSlice<'_>) -> u64 {
    use std::hash::Hasher;
    #[cfg(test)]
    LONGEST_LINE_HASHES.with(|hashes| hashes.set(hashes.get() + 1));
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    for chunk in line.chunks() {
        hasher.write(chunk.as_bytes());
    }
    hasher.finish()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct EditorScrollbarSnapshot {
    layout: EditorScrollbarLayout,
    cursor_scroll_offset: Point<Pixels>,
    soft_wrap: bool,
}

impl EditorScrollbarSnapshot {
    fn new<M: InputModeKind>(
        input_bounds: Bounds<Pixels>,
        last_layout: &LastLayout,
        scroll_size: Size<Pixels>,
        cursor_scroll_offset: Point<Pixels>,
        state: &InputBaseState<M>,
    ) -> Self {
        Self {
            layout: EditorScrollbarLayout::new(
                input_bounds,
                last_layout.line_number_width - last_layout.column_offset,
                scroll_size,
                state.editor_paddings,
            ),
            cursor_scroll_offset,
            soft_wrap: state.soft_wrap,
        }
    }
}

impl EditorScrollbarLayout {
    fn new(
        input_bounds: Bounds<Pixels>,
        line_number_width: Pixels,
        scroll_size: Size<Pixels>,
        paddings: Edges<Pixels>,
    ) -> Self {
        let left = if line_number_width == px(0.) {
            px(0.)
        } else {
            paddings.left + line_number_width - LINE_NUMBER_RIGHT_MARGIN
        };

        let bounds = Bounds::new(
            point(
                input_bounds.origin.x + left,
                input_bounds.origin.y - paddings.top,
            ),
            size(
                input_bounds.size.width - left + paddings.right,
                input_bounds.size.height + paddings.top + paddings.bottom,
            ),
        );

        Self {
            bounds,
            scroll_size: size(
                scroll_size.width - left + paddings.right + RIGHT_MARGIN,
                scroll_size.height,
            ),
            scroll_viewport_size: size(bounds.size.width, input_bounds.size.height),
        }
    }
}

pub(super) struct EditorScrollbar<M: InputModeKind> {
    state: Entity<InputBaseState<M>>,
}

impl<M: InputModeKind> EditorScrollbar<M> {
    pub(super) fn new(state: Entity<InputBaseState<M>>) -> Self {
        Self { state }
    }
}

impl<M: InputModeKind> IntoElement for EditorScrollbar<M> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl<M: InputModeKind> Element for EditorScrollbar<M> {
    type RequestLayoutState = ();
    type PrepaintState = Option<AnyElement>;

    fn id(&self) -> Option<ElementId> {
        Some("editor-scrollbar".into())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.position = Position::Absolute;
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();

        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let state = self.state.read(cx);
        let Some(snapshot) = state.editor_scrollbar_snapshot.get() else {
            return None;
        };
        let scroll_handle = state.scroll_handle.clone();

        if scroll_handle.offset() != snapshot.cursor_scroll_offset {
            scroll_handle.set_offset(snapshot.cursor_scroll_offset);
        }

        let renderer = state.scrollbar_renderer.clone();
        let mut scrollbar = if let Some(renderer) = renderer {
            let presentation = super::EditorScrollbarContext {
                handle: scroll_handle,
                viewport: snapshot.layout.bounds,
                scroll_viewport: snapshot.layout.scroll_viewport_size,
                content: snapshot.layout.scroll_size,
                axis: if snapshot.soft_wrap {
                    crate::ScrollbarAxis::Vertical
                } else {
                    crate::ScrollbarAxis::Both
                },
            };
            renderer(presentation, window, cx)
        } else {
            if !snapshot.soft_wrap {
                Scrollbar::new(&scroll_handle)
            } else {
                Scrollbar::vertical(&scroll_handle)
            }
            .viewport_bounds(snapshot.layout.bounds)
            .scroll_viewport_size(snapshot.layout.scroll_viewport_size)
            .scroll_size(snapshot.layout.scroll_size)
            .into_any_element()
        };

        scrollbar.prepaint_as_root(
            snapshot.layout.bounds.origin,
            snapshot.layout.bounds.size.into(),
            window,
            cx,
        );
        Some(scrollbar)
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        if let Some(scrollbar) = prepaint.as_mut() {
            scrollbar.paint(window, cx);
        }
    }
}

// Prepaint and retained state must use the same range, including after content shrinks.
pub(super) fn clamp_horizontal_scroll_offset(
    offset: Pixels,
    scroll_width: Pixels,
    input_width: Pixels,
    text_align: TextAlign,
) -> Pixels {
    let caret_clearance = if text_align == TextAlign::Left {
        px(0.)
    } else {
        -CURSOR_WIDTH
    };
    let min_offset = (input_width - scroll_width + caret_clearance).min(caret_clearance);
    offset.clamp(min_offset, px(0.))
}

fn clamp_vertical_scroll_offset(
    scroll_top: Pixels,
    scroll_height: Pixels,
    input_height: Pixels,
    ceiling: Pixels,
) -> Pixels {
    scroll_top.clamp((input_height - scroll_height).min(px(0.)), ceiling)
}

fn editor_gutter_bounds(
    input_bounds: Bounds<Pixels>,
    line_number_width: Pixels,
    ghost_lines_height: Pixels,
    paddings: Edges<Pixels>,
) -> Bounds<Pixels> {
    Bounds {
        origin: point(
            input_bounds.origin.x - paddings.left,
            input_bounds.origin.y - paddings.top,
        ),
        size: size(
            line_number_width + paddings.left,
            input_bounds.size.height + ghost_lines_height + paddings.top + paddings.bottom,
        ),
    }
}

/// Where a right click opens the gutter menu: the gutter, and at least `GUTTER_MENU_EDGE` of the
/// text's left edge when the gutter has shrunk to nothing (numbers hidden, no fold arrows).
fn gutter_menu_bounds(gutter: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds { origin: gutter.origin, size: size(gutter.size.width.max(GUTTER_MENU_EDGE), gutter.size.height) }
}

use super::MASK_CHAR;

/// Convert a byte offset in the original text to a byte offset in the masked display string.
///
/// The masked string consists of `MASK_CHAR` repeated once per character in the original text.
/// Since `MASK_CHAR` may be multi-byte in UTF-8, the byte offset in the masked string is
/// `char_index * MASK_CHAR.len_utf8()`.
fn masked_display_offset(text: &Rope, original_offset: usize) -> usize {
    text.offset_to_char_index(original_offset) * MASK_CHAR.len_utf8()
}

/// Move the IME marked range (tracked against the original text) into the display text
/// coordinate space, so that a run boundary can't land inside a multi-byte `MASK_CHAR`
/// and panic text shaping on a non-char-boundary slice.
fn ime_marked_display_range(
    text: &Rope,
    marked_range: Option<Range<usize>>,
    masked: bool,
) -> Option<Range<usize>> {
    let marked = marked_range?;
    if masked {
        Some(masked_display_offset(text, marked.start)..masked_display_offset(text, marked.end))
    } else {
        Some(marked)
    }
}

/// Viewport capacity in display rows, independent of logical buffer lines.
///
/// Shared by both scroll-into-view paths (`layout_cursors` and
/// `scroll_to_with_padding`) so they agree on the visible-line count even
/// when soft-wrapped lines make logical and display rows diverge.
pub(super) fn viewport_visible_lines(viewport_height: Pixels, line_height: Pixels) -> usize {
    (viewport_height / line_height) as usize
}

/// Minimum pixel padding the cursor is kept clear of the viewport's
/// top/bottom edges before auto-scroll engages. Backs
/// [`InputBaseState::cursor_surrounding_lines`].
///
/// Auto-grow uses one line. Otherwise `None` falls back to the historical
/// heuristic ([`BOTTOM_MARGIN_ROWS`] lines, or one line on small
/// viewports); `Some(n)` uses `n` lines. The result is saturated against
/// half the viewport so an oversized override can't invert the
/// top/bottom thresholds into a scroll feedback loop. `visible_lines` is the
/// viewport capacity in display rows, independent of logical buffer lines.
pub(super) fn cursor_surrounding_padding(
    is_auto_grow: bool,
    override_lines: Option<usize>,
    visible_lines: usize,
    line_height: Pixels,
) -> Pixels {
    if is_auto_grow {
        return line_height;
    }
    let raw = match override_lines {
        Some(lines) => lines as f32 * line_height,
        None => {
            if visible_lines < BOTTOM_MARGIN_ROWS * 8 {
                line_height
            } else {
                BOTTOM_MARGIN_ROWS * line_height
            }
        }
    };
    // Saturate against half the viewport so top + bottom margins can coexist.
    let viewport_half = (visible_lines as f32 * line_height).half();
    raw.min(viewport_half)
}

/// Reveal the whole visual row without changing the usual edge clearance.
/// Reserve the row's height below its insertion position before allocating
/// top padding. A viewport shorter than a row can only align its start.
/// `ceiling` is the furthest the text may sit below the viewport's top (`0`,
/// or the typewriter's room above the first line).
pub(super) fn reveal_row(
    offset: Pixels,
    row_top: Pixels,
    line_height: Pixels,
    viewport_height: Pixels,
    edge_padding: Pixels,
    ceiling: Pixels,
) -> Pixels {
    let row_height = line_height.min(viewport_height);
    let bottom = edge_padding.max(row_height).min(viewport_height);
    let top = edge_padding.min(viewport_height - bottom);
    // Edge padding never pulls the text below the viewport's top: a first row stays where it is.
    offset
        .clamp((top - row_top).min(px(0.)), viewport_height - bottom - row_top)
        .min(ceiling)
}

/// Where the typewriter holds the caret's row: its top, from the viewport's top, so the row's
/// middle sits 45% down. `0` when the viewport is shorter than a line.
pub(super) fn typewriter_band(viewport_height: Pixels, line_height: Pixels) -> Pixels {
    (viewport_height * 0.45 - line_height / 2.).max(px(0.))
}

/// A row more than a viewport outside the view lands centered; nearer rows
/// keep the minimal `revealed` offset. Shared by both reveal paths.
pub(super) fn center_if_far(
    was: Pixels,
    revealed: Pixels,
    row_top: Pixels,
    line_height: Pixels,
    viewport_height: Pixels,
) -> Pixels {
    let view_top = -was;
    if row_top + line_height < view_top - viewport_height
        || row_top > view_top + viewport_height * 2.
    {
        -(row_top + (line_height - viewport_height) / 2.)
    } else {
        revealed
    }
}

/// Pixel height of the empty area below the last line in the editor's
/// scrollable region. Backs [`InputBaseState::scroll_beyond_last_line`].
///
/// `0` outside code-editor mode. Inside it, `None` is half the viewport
/// (floored at [`BOTTOM_MARGIN_ROWS`] line-heights); `Some(n)` is exactly
/// `n` line-heights.
pub(super) fn empty_bottom_height(
    is_code_editor: bool,
    override_rows: Option<usize>,
    viewport_height: Pixels,
    line_height: Pixels,
) -> Pixels {
    if !is_code_editor {
        return px(0.);
    }
    match override_rows {
        Some(rows) => rows as f32 * line_height,
        None => viewport_height.half().max(BOTTOM_MARGIN_ROWS * line_height),
    }
}

/// Layout information for fold icons.
struct FoldIconLayout {
    /// Hitbox for the line number area (used for hover detection)
    line_number_hitbox: Hitbox,
    /// Whether the pointer was over the gutter when the arrows were laid out.
    gutter_hovered: bool,
    /// List of (display_row, is_folded, icon_element) pairs for each fold candidate
    icons: Vec<(usize, bool, gpui::AnyElement)>,
    /// The `...` chips of the folded lines in view.
    chips: Vec<FoldChip>,
    /// Whether any line in view opens a block, so the pointer's entry to the gutter matters.
    candidates: bool,
    /// The window y span of a gliding fold's block still under its curtain.
    curtain: Option<(Pixels, Pixels)>,
}

pub(super) struct TextElement<M: InputModeKind> {
    pub(crate) state: Entity<InputBaseState<M>>,
    placeholder: SharedString,
}

impl<M: InputModeKind> TextElement<M> {
    pub(super) fn new(state: Entity<InputBaseState<M>>) -> Self {
        Self {
            state,
            placeholder: SharedString::default(),
        }
    }

    /// Set the placeholder text of the input field.
    pub(super) fn placeholder(mut self, placeholder: impl Into<SharedString>) -> Self {
        self.placeholder = placeholder.into();
        self
    }

    fn paint_mouse_listeners(&mut self, hitbox: &Hitbox, window: &mut Window, _: &mut App) {
        // Text with room to scroll keeps the wheel from the scroll areas
        // around it; its own listener scrolls it in the bubble phase.
        window.on_mouse_event({
            let state = self.state.clone();
            let hitbox = hitbox.id;
            move |event: &gpui::ScrollEvent, phase, window, cx| {
                if phase.capture()
                    && hitbox.should_handle_scroll(window)
                    && state
                        .read(cx)
                        .takes_scroll(event.delta.pixel_delta(window.line_height()))
                {
                    crate::scroll_bounce::claim_scroll(hitbox, cx);
                }
            }
        });

        // Every touch is offered as a drag first; that is how a tap's mouse
        // events are later told apart from a mouse's.
        window.on_mouse_event(move |event: &TouchDragEvent, phase, _, cx| {
            if phase.capture() && event.phase == TouchPhase::Started {
                crate::GlobalState::note_touch(cx);
            }
        });

        // A long press is touch's way to select: the word under the finger,
        // then whatever the finger sweeps over. Claiming it keeps the moves
        // out of the pan recognizer, so the input does not scroll instead.
        window.on_mouse_event({
            let state = self.state.clone();
            let hitbox = hitbox.clone();
            move |event: &LongPressEvent, phase, window, cx| {
                if !phase.bubble() {
                    return;
                }
                if event.phase == TouchPhase::Started {
                    if window.default_prevented() || !hitbox.is_hovered(window) {
                        return;
                    }
                    if !state.update(cx, |state, cx| state.on_long_press(event, window, cx)) {
                        return;
                    }
                    window.capture_long_press(&state);
                } else if !window.has_long_press_capture(&state) {
                    return;
                } else {
                    state.update(cx, |state, cx| {
                        state.on_long_press(event, window, cx);
                    });
                }
                window.prevent_default();
                cx.stop_propagation();
            }
        });

        window.on_mouse_event({
            let state = self.state.clone();

            move |event: &MouseMoveEvent, _, window, cx| {
                if event.pressed_button == Some(MouseButton::Left) {
                    state.update(cx, |state, cx| {
                        state.on_drag_move(event, window, cx);
                    });
                }
            }
        });

        window.on_mouse_event({
            let state = self.state.clone();
            move |event: &MouseUpEvent, phase, _, cx| {
                if !phase.bubble() {
                    return;
                }

                // Stop auto-scroll when mouse up, and also stop selecting.
                state.update(cx, |state, cx| {
                    state.settle_text_press(event.position, cx);
                    state.auto_scroll.stop();
                    state.selecting = false;
                    state.column_select_start = None;
                });
            }
        });
    }

    /// Lays out the carets and updates the scroll position for the active one.
    fn layout_cursors(
        &self,
        last_layout: &LastLayout,
        bounds: &mut Bounds<Pixels>,
        scroll_size: Size<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> (Vec<CursorRenderInfo>, Point<Pixels>, Option<usize>, bool) {
        let state = self.state.read(cx);

        let line_height = last_layout.line_height;
        let visible_range = &last_layout.visible_range;
        let lines = &last_layout.lines;
        let line_number_width = last_layout.line_number_width;

        let active_id = state.active_selection().id;
        let mut scroll_offset = state.scroll_handle.offset();
        let mut current_row = None;
        let mut resize_reveal_owned = false;
        let mut cursor_infos: Vec<CursorRenderInfo> = Vec::with_capacity(state.selections.len());

        // Padding kept between the cursor and the viewport's top/bottom
        // edges, used by the auto-scroll-into-view computation below.
        let top_bottom_margin = cursor_surrounding_padding(
            state.mode.is_auto_grow(),
            state.cursor_surrounding_lines,
            viewport_visible_lines(bounds.size.height, line_height),
            line_height,
        );

        // Keep a visible, focused caret visible when its viewport reflows, but
        // do not undo deliberate scrolling away from an unchanged selection.
        let resized = state.last_bounds.is_some_and(|old| old.size != bounds.size)
            || state
                .last_layout
                .as_ref()
                .is_some_and(|old| old.line_height != line_height);
        let reveal_on_resize = resized
            && state.deferred_scroll_offset.is_none()
            && state.focus_handle.is_focused(window)
            && state.prepaint_caret_geometry.is_some_and(|old| {
                // An unshaped caret has provisional x until the notified
                // repaint. Consecutive resize frames retain ownership only
                // while no intervening scroll changed the viewport.
                if old.resize_reveal_owned {
                    return old.scroll_offset() == scroll_offset;
                }
                let caret = old.cursor_bounds();
                let viewport = old.input_bounds();
                let top = caret.top() + scroll_offset.y;
                // Cached x already contains its painted scroll; account only
                // for a wheel offset change since that geometry was produced.
                let left = caret.left() + scroll_offset.x - old.scroll_offset().x;
                top >= viewport.top()
                    && top + caret.size.height <= viewport.bottom()
                    && left >= viewport.left()
                    && left + caret.size.width <= viewport.right()
            });

        // Resolve a cursor or selection endpoint to a content-space position.
        let visible_buffer_lines = &last_layout.visible_buffer_lines;
        let caret_for = |row: usize, offset: usize, affinity: bool| -> Point<Pixels> {
            // y of the top of buffer line `row` in content space.
            let top = line_height * state.display_map.buffer_line_to_display_row(row);
            let line_origin = point(px(0.), top);

            if let Some(vi) = visible_buffer_lines.iter().position(|&bl| bl == row) {
                let line = &lines[vi];
                let line_start = last_layout.visible_line_byte_offsets[vi];
                let local = offset.saturating_sub(line_start);
                if let Some(pos) = line.position_for_index(local, last_layout, affinity) {
                    return line_origin + pos;
                }
            }
            if state.masked {
                return line_origin;
            }
            point(
                px(0.),
                line_height * state.display_map.offset_to_display_row(offset, affinity),
            )
        };

        let cursor_height = 0.85 * line_height;


        for selection in state.selections.iter() {
            let is_active = selection.id == active_id;

            let mut selected_range = *selection;
            let mut cursor = selection.cursor_offset();
            if is_active && state.ime_marked_range.is_some() {
                // The clause is underlined, not highlighted. Keep its caret
                // at the IME-selected position rather than the preedit's end.
                selected_range = (cursor..cursor).into();
            }
            let is_selected_all = selected_range.len() == state.text.len();

            // Buffer rows from the raw (pre-mask) offsets, used to locate the cursor line.
            let cursor_row = state.text.offset_to_point(cursor).row;

            // Skip inactive cursors that are far outside the visible range. The
            // active cursor is always processed so scroll tracking keeps working.
            if !is_active
                && (cursor_row + 2 < visible_range.start || cursor_row > visible_range.end + 2)
            {
                continue;
            }

            let sel_start_row = state.text.offset_to_point(selected_range.start).row;
            let sel_end_row = state.text.offset_to_point(selected_range.end).row;
            if state.masked {
                selected_range.start = masked_display_offset(&state.text, selected_range.start);
                selected_range.end = masked_display_offset(&state.text, selected_range.end);
                cursor = masked_display_offset(&state.text, cursor);
            }

            let affinity = is_active && state.cursor_line_end_affinity;
            let cursor_pos = caret_for(cursor_row, cursor, affinity);
            let cursor_start = caret_for(sel_start_row, selected_range.start, false);
            let cursor_end = caret_for(sel_end_row, selected_range.end, false);

            if is_active {
                current_row = Some(cursor_row);

                let selection_changed = state.last_selected_range != Some(selected_range);
                let auto_scrolling = state.auto_scroll.is_active();
                if (selection_changed || reveal_on_resize || state.reveal_far_centered)
                    && !is_selected_all
                {
                    // Resize-follow owns only vertical visibility. An unshaped
                    // target has no reliable x; retain the existing x offset.
                    if (selection_changed || state.reveal_far_centered) && !auto_scrolling {
                        // Right-aligned carets are clamped instead of scrolling.
                        let safety_margin = match last_layout.text_align {
                            TextAlign::Left => RIGHT_MARGIN,
                            TextAlign::Right => px(0.),
                            TextAlign::Center => CURSOR_WIDTH,
                        };
                        scroll_offset.x = if scroll_offset.x + cursor_pos.x
                            > (bounds.size.width - line_number_width - safety_margin)
                        {
                            bounds.size.width - line_number_width - safety_margin - cursor_pos.x
                        } else if scroll_offset.x + cursor_pos.x < px(0.) {
                            -cursor_pos.x
                        } else {
                            scroll_offset.x
                        };
                    }

                    // Vertical cursor-follow is suppressed while auto-scroll manages the y axis,
                    // to prevent fighting the background scroll task.
                    if !auto_scrolling {
                        scroll_offset.y = reveal_row(
                            scroll_offset.y,
                            cursor_pos.y,
                            line_height,
                            bounds.size.height,
                            top_bottom_margin,
                            state.top_inset(bounds.size.height, line_height),
                        );
                        // A caret already past the top at the first layout (a reopened or
                        // restored tab) lands centered like a far jump, with no glide.
                        if (state.reveal_far_centered || state.last_layout.is_none())
                            && state.is_multi_line()
                        {
                            scroll_offset.y = center_if_far(
                                state.scroll_handle.offset().y,
                                scroll_offset.y,
                                cursor_pos.y,
                                line_height,
                                bounds.size.height,
                            );
                        }
                        resize_reveal_owned = reveal_on_resize;
                    }

                    // For selection to move scroll
                    if selection.reversed {
                        if selection_changed && scroll_offset.x + cursor_start.x < px(0.) {
                            // selection start is out of left
                            scroll_offset.x = -cursor_start.x;
                        }
                        if !auto_scrolling && scroll_offset.y + cursor_start.y < px(0.) {
                            // selection start is out of top
                            scroll_offset.y = -cursor_start.y;
                        }
                    } else {
                        // TODO: Consider to remove this part,
                        // maybe is not necessary (But selection_reversed is needed).
                        if selection_changed && scroll_offset.x + cursor_end.x <= px(0.) {
                            // selection end is out of left
                            scroll_offset.x = -cursor_end.x;
                        }
                        if !auto_scrolling && scroll_offset.y + cursor_end.y <= px(0.) {
                            // selection end is out of top
                            scroll_offset.y = -cursor_end.y;
                        }
                    }
                }
            }

            // Apply the final horizontal offset to every caret after cursor-follow and
            // deferred scrolling have been resolved, regardless of selection order.
            let cursor_x = bounds.left() + cursor_pos.x + line_number_width;
            // A caret in an editor's layer needs the width of the character it sits on, for the hollow
            // box and its crossfade with the bar: at the head of the selection, if there is one.
            let cell_width = if state.caret_layer.is_some() && state.is_multi_line() && !state.masked {
                let end = state.next_boundary(cursor);
                let next = caret_for(cursor_row, end, false);
                if end > cursor && next.y == cursor_pos.y && next.x > cursor_pos.x {
                    next.x - cursor_pos.x
                } else {
                    // At a line end: a cell as wide as a space.
                    last_layout.space_width.max(px(1.))
                }
            } else {
                px(0.)
            };
            cursor_infos.push(CursorRenderInfo {
                bounds: Bounds::new(
                    point(
                        cursor_x,
                        bounds.top() + cursor_pos.y + ((line_height - cursor_height) / 2.),
                    ),
                    size(CURSOR_WIDTH, cursor_height),
                ),
                is_active,
                collapsed: selected_range.is_empty(),
                offset: cursor,
                total: state.selections.len(),
                cell_width,
            });
        }

        if let Some(deferred_scroll_offset) = state.deferred_scroll_offset {
            scroll_offset = deferred_scroll_offset;
        }
        // A glide in progress sets the vertical offset over any reveal's.
        if let Some(glide_y) = state.glide_y {
            scroll_offset.y = glide_y;
        }
        // So does one along x.
        if let Some(glide_x) = state.glide_x {
            scroll_offset.x = glide_x;
        }
        scroll_offset.x = clamp_horizontal_scroll_offset(
            scroll_offset.x,
            scroll_size.width,
            bounds.size.width,
            last_layout.text_align,
        );
        for info in &mut cursor_infos {
            info.bounds.origin.x += scroll_offset.x;
            // Right-aligned text keeps the caret inside the viewport edge.
            if last_layout.text_align == TextAlign::Right {
                info.bounds.origin.x = info.bounds.origin.x.min(bounds.right() - CURSOR_WIDTH);
            }
        }
        scroll_offset.y = clamp_vertical_scroll_offset(
            scroll_offset.y,
            scroll_size.height,
            bounds.size.height,
            state.top_inset(bounds.size.height, line_height),
        );

        bounds.origin = bounds.origin + scroll_offset;

        (
            cursor_infos,
            scroll_offset,
            current_row,
            resize_reveal_owned,
        )
    }

    /// Layout the match range to a Path. `radius` rounds every corner of the
    /// outline; each corner is clamped to half of the step it turns.
    pub(crate) fn layout_match_range(
        range: Range<usize>,
        last_layout: &LastLayout,
        bounds: &Bounds<Pixels>,
        radius: Pixels,
    ) -> Option<Path<Pixels>> {
        let corners = Self::layout_range_corners(&range, last_layout)?;
        let origin = bounds.origin + point(last_layout.line_number_width, px(0.));
        let mut builder = gpui::PathBuilder::fill();
        // Rows that do not overlap their neighbour are separate shapes, as in
        // Zed's highlighted ranges, so the outline never pinches or crosses.
        let mut start = 0;
        for ix in 1..=corners.len() {
            let split = ix == corners.len() || {
                let (above, below) = (&corners[ix - 1], &corners[ix]);
                below.top_right.x <= above.top_left.x || below.top_left.x >= above.top_right.x
            };
            if split {
                let points = frame_outline_points(&corners[start..ix]);
                push_rounded_outline(&mut builder, points, origin, radius);
                start = ix;
            }
        }
        builder.build().ok()
    }

    /// Project a buffer range through the visible (non-folded) shaped lines.
    /// Half-open intersections give soft-wrap ends their trailing affinity and
    /// prevent ranges on later lines from painting an earlier line's first glyph.
    fn layout_range_corners(
        range: &Range<usize>,
        last_layout: &LastLayout,
    ) -> Option<Vec<Corners<Point<Pixels>>>> {
        let range = range.start.max(last_layout.visible_range_offset.start)
            ..range.end.min(last_layout.visible_range_offset.end);
        if range.is_empty() {
            return None;
        }

        let mut y = last_layout.visible_top;
        let mut corners = Vec::new();
        for (&line_offset, line) in last_layout
            .visible_line_byte_offsets
            .iter()
            .zip(last_layout.lines.iter())
        {
            let mut offset = line_offset;
            let alignment = last_layout.alignment_offset(line.longest_width);
            for (row, shaped) in line.wrapped_lines.iter().enumerate() {
                let end = offset + shaped.len;
                let start_ix = range.start.max(offset);
                let end_ix = range.end.min(end);
                let is_last = row + 1 == line.wrapped_lines.len();
                // A selected newline has a one-space cell, including empty lines.
                // A wrap boundary is not a newline and must not get such a cell.
                let newline = is_last && range.start <= end && range.end > end;
                if start_ix < end_ix || newline {
                    let indent = if row == 0 { px(0.) } else { line.wrap_indent };
                    let left = alignment + indent + shaped.x_for_index(start_ix.min(end) - offset);
                    let right = alignment
                        + indent
                        + if newline {
                            shaped.width + last_layout.space_width
                        } else {
                            shaped.x_for_index(end_ix - offset)
                        };
                    corners.push(Corners {
                        top_left: point(left, y),
                        top_right: point(right, y),
                        bottom_left: point(left, y + last_layout.line_height),
                        bottom_right: point(right, y + last_layout.line_height),
                    });
                }
                offset = end;
                y += last_layout.line_height;
            }
        }
        (!corners.is_empty()).then_some(corners)
    }

    fn layout_range_decorations(
        &self,
        last_layout: &LastLayout,
        bounds: &Bounds<Pixels>,
        window: &Window,
        content_mask: Bounds<Pixels>,
        cx: &App,
    ) -> (Vec<(Path<Pixels>, Hsla)>, Vec<(Path<Pixels>, Hsla)>) {
        let state = self.state.read(cx);
        let radius = last_layout.line_height * SELECTION_CORNER_RADIUS;
        let mut fills = Vec::new();
        let mut frames = Vec::new();
        // Query separate buffer spans across folds instead of scanning annotations
        // in the hidden text between the first and last visible buffer offsets.
        let mut visible_spans: Vec<Range<usize>> = Vec::new();
        for (&offset, line) in last_layout
            .visible_line_byte_offsets
            .iter()
            .zip(last_layout.lines.iter())
        {
            let end = (offset + line.len() + 1).min(last_layout.visible_range_offset.end);
            if let Some(previous) = visible_spans.last_mut().filter(|span| span.end == offset) {
                previous.end = end;
            } else if offset < end {
                visible_spans.push(offset..end);
            }
        }
        for decoration in state.extras.range_decorations(&visible_spans) {
            match decoration.style() {
                RangeDecorationStyle::Fill => {
                    let color = decoration
                        .color()
                        .unwrap_or(state.editor_style.foreground.opacity(0.12));
                    let range = decoration.range().clone();
                    if let Some(path) = Self::layout_match_range(range, last_layout, bounds, radius)
                    {
                        fills.push((path, color));
                    }
                }
                RangeDecorationStyle::Frame => {
                    let color = decoration.color().unwrap_or(state.editor_style.foreground);
                    let Some(corners) = Self::layout_range_corners(decoration.range(), last_layout)
                    else {
                        continue;
                    };
                    let origin = bounds.origin + point(last_layout.line_number_width, px(0.));
                    let corners = pad_frame_corners(&corners, px(1.));
                    let points = frame_outline_points(&corners)
                        .into_iter()
                        .map(|point| origin + point)
                        .collect::<Vec<_>>();
                    let (stroke_width, points) =
                        snap_frame_outline(&points, px(1.), window.scale_factor());
                    let points = clamp_frame_to_content_mask(points, stroke_width, content_mask);
                    let Some(first) = points.first().copied() else {
                        continue;
                    };
                    let mut builder = gpui::PathBuilder::stroke(stroke_width);
                    builder.move_to(first);
                    for point in points.iter().skip(1) {
                        builder.line_to(*point);
                    }
                    builder.close();
                    if let Ok(path) = builder.build() {
                        frames.push((path, color));
                    }
                }
            }
        }
        (fills, frames)
    }

    fn layout_search_matches(
        &self,
        last_layout: &LastLayout,
        bounds: &Bounds<Pixels>,
        cx: &mut App,
    ) -> Vec<(Path<Pixels>, bool)> {
        let state = self.state.read(cx);
        if !state.search_session.is_active() {
            return vec![];
        }
        let radius = last_layout.line_height * SELECTION_CORNER_RADIUS;
        let ranges = state.search_session.matcher.matched_ranges();
        let current_match_ix = state.search_session.matcher.current_match_index();

        // Matches are sorted and do not overlap, so only the ones that reach
        // into the visible range need a layout.
        let visible_range = &last_layout.visible_range_offset;
        let first = ranges.partition_point(|range| range.end <= visible_range.start);
        let mut paths = Vec::new();
        for (index, range) in ranges
            .iter()
            .enumerate()
            .skip(first)
            .take_while(|(_, range)| range.start < visible_range.end)
        {
            let laid_out = Self::layout_match_range(range.clone(), last_layout, bounds, radius);
            if let Some(path) = laid_out {
                paths.push((path, current_match_ix == index));
            }
        }

        paths
    }

    /// Find in selection: the parts of the search's scope on screen, one path each. Only the ranges
    /// that reach into the visible range are laid out.
    fn layout_search_scope(
        &self,
        last_layout: &LastLayout,
        bounds: &Bounds<Pixels>,
        cx: &mut App,
    ) -> Option<Rc<Vec<Path<Pixels>>>> {
        let state = self.state.read(cx);
        let mut cache = state.search_scope_cache.borrow_mut();
        let Some(scope) = state.search_session.matcher.shared_scope()
            .filter(|_| state.search_session.is_active()) else {
                *cache = None;
                return None;
            };
        if let Some(entry) = cache.as_ref() {
            let old = &entry.layout;
            if Arc::ptr_eq(&entry.scope, scope)
                && entry.bounds == *bounds
                && old.visible_range == last_layout.visible_range
                && old.visible_buffer_lines == last_layout.visible_buffer_lines
                && old.visible_line_byte_offsets == last_layout.visible_line_byte_offsets
                && old.visible_top == last_layout.visible_top
                && old.visible_range_offset == last_layout.visible_range_offset
                && old.line_height == last_layout.line_height
                && old.wrap_width == last_layout.wrap_width
                && old.wrapping_indent == last_layout.wrapping_indent
                && old.line_number_width == last_layout.line_number_width
                && old.space_width == last_layout.space_width
                && old.text_align == last_layout.text_align
                && old.content_width == last_layout.content_width
                && old.lines.len() == last_layout.lines.len()
                && old.lines.iter().zip(last_layout.lines.iter())
                    .zip(last_layout.visible_line_byte_offsets.iter()).all(|((a, b), &offset)| {
                        if a.wrap_indent != b.wrap_indent || a.longest_width != b.longest_width
                            || a.wrapped_lines.len() != b.wrapped_lines.len() {
                            return false;
                        }
                        let mut offset = offset;
                        a.wrapped_lines.iter().zip(b.wrapped_lines.iter()).all(|(a, b)| {
                            if a.len != b.len || a.width != b.width {
                                return false;
                            }
                            let end = offset + a.len;
                            let first = scope.partition_point(|range| range.end <= offset);
                            // Compare only scope edges, not every glyph. This also catches a
                            // font, highlight or inline-token reshape with unchanged total width.
                            let same = scope[first..].iter().take_while(|range| range.start < end)
                                .all(|range| {
                                    [range.start, range.end].into_iter().all(|edge| {
                                        let ix = edge.saturating_sub(offset).min(a.len);
                                        a.x_for_index(ix) == b.x_for_index(ix)
                                    })
                                });
                            offset = end;
                            same
                        })
                    })
            {
                return Some(entry.paths.clone());
            }
        }
        let radius = last_layout.line_height * SELECTION_CORNER_RADIUS;
        let visible_range = &last_layout.visible_range_offset;
        let first = scope.partition_point(|range| range.end <= visible_range.start);
        let paths = Rc::new(scope[first..]
            .iter()
            .take_while(|range| range.start < visible_range.end)
            .filter_map(|range| Self::layout_match_range(range.clone(), last_layout, bounds, radius))
            .collect::<Vec<_>>());
        *cache = Some(Box::new(SearchScopeCache {
            scope: scope.clone(), layout: last_layout.clone(), bounds: *bounds, paths: paths.clone(),
        }));
        Some(paths)
    }

    /// The matching-bracket highlight: both halves of each pair next to a
    /// caret. The pairs come from the state's cache, found when the text or
    /// a caret changed, so a frame with the caret away from a bracket does
    /// no more than compare the cache key.
    fn layout_bracket_pairs(
        &self,
        last_layout: &LastLayout,
        bounds: &Bounds<Pixels>,
        cx: &mut App,
    ) -> Vec<Path<Pixels>> {
        self.state.update(cx, |state, _| state.refresh_bracket_pairs());
        let pairs = self.state.read(cx).bracket_pairs();
        let mut paths = Vec::new();
        let radius = last_layout.line_height * SELECTION_CORNER_RADIUS;
        for &(bracket, partner) in pairs {
            for offset in [bracket, partner] {
                if let Some(path) =
                    Self::layout_match_range(offset..offset + 1, last_layout, bounds, radius)
                {
                    paths.push(path);
                }
            }
        }
        paths
    }

    fn layout_hover_highlight(
        &self,
        last_layout: &LastLayout,
        bounds: &Bounds<Pixels>,
        cx: &mut App,
    ) -> Option<Path<Pixels>> {
        let state = self.state.read(cx);
        let Some(symbol_range) = state.extras.hover_symbol_range() else {
            return None;
        };
        let radius = last_layout.line_height * SELECTION_CORNER_RADIUS;

        Self::layout_match_range(symbol_range, last_layout, bounds, radius)
    }

    /// The backgrounds text is drawn over, sorted and apart: color swatches
    /// first, then the selection, the current find match and the other
    /// matches in whatever room the ones before leave. Text over each is
    /// recolored to stay readable (`split_runs_by_bg_segments`), as the fill
    /// the paint pass will draw there looks once blended over the background.
    /// Nothing is built, and nothing recolored, when no highlight is visible.
    fn layout_background_segments(
        &self,
        document_colors: Vec<(Range<usize>, Hsla)>,
        last_layout: &LastLayout,
        window: &Window,
        cx: &App,
    ) -> Vec<(Range<usize>, Hsla)> {
        let state = self.state.read(cx);
        if state.masked {
            return document_colors;
        }
        let visible = &last_layout.visible_range_offset;
        let style = &state.editor_style;
        let key_window = crate::input::window_key(window);
        let dark = style.background.l < 0.5;
        let over_background = |fill: Hsla| style.background.blend(fill);

        let mut segments = document_colors;
        segments.sort_by_key(|(range, _)| range.start);

        // One layer: the parts of `ranges` that no earlier layer covers, in
        // `color`, added and sorted once.
        let add_layer = |segments: &mut Vec<(Range<usize>, Hsla)>,
                         ranges: &mut dyn Iterator<Item = Range<usize>>,
                         color: Hsla| {
            let mut gaps: Vec<(Range<usize>, Hsla)> = Vec::new();
            for range in ranges {
                let range = range.start.max(visible.start)..range.end.min(visible.end);
                if range.is_empty() {
                    continue;
                }
                let first = segments.partition_point(|(taken, _)| taken.end <= range.start);
                let mut free = range.start;
                for (taken, _) in segments[first..]
                    .iter()
                    .take_while(|(taken, _)| taken.start < range.end)
                {
                    if free < taken.start {
                        gaps.push((free..taken.start, color));
                    }
                    free = free.max(taken.end);
                }
                if free < range.end {
                    gaps.push((free..range.end, color));
                }
            }
            if !gaps.is_empty() {
                segments.append(&mut gaps);
                segments.sort_by_key(|(range, _)| range.start);
            }
        };

        if state.has_selection_focus(window, cx) {
            let active_id = state.active_selection().id;
            let mut selected = state
                .selections
                .iter()
                .filter_map(|selection| {
                    let (start, end) = if selection.start < selection.end {
                        (selection.start, selection.end)
                    } else {
                        (selection.end, selection.start)
                    };
                    // IME composition shows no selection highlight.
                    let composing = selection.id == active_id
                        && state
                            .ime_marked_range
                            .as_ref()
                            .is_some_and(|marked| !marked.is_empty());
                    (start < end && !composing).then_some(start..end)
                })
                .peekable();
            // The accent is read from AppKit only when a selection will use it.
            if selected.peek().is_some() {
                let fill = over_background(system_look::selection_fill(
                    style.foreground,
                    style.selection,
                    dark,
                    key_window,
                ));
                add_layer(&mut segments, &mut selected, fill);
            }
        }

        // Find matches: the shared find fills, the current match first so it wins.
        if state.search_session.is_active() {
            let ranges = state.search_session.matcher.matched_ranges();
            let current = state.search_session.matcher.current_match_index();
            let first = ranges.partition_point(|range| range.end <= visible.start);
            // The system colors are read only when a match is on screen.
            if ranges.get(first).is_some_and(|range| range.start < visible.end) {
                let fills = crate::theme_tokens::find_fills(style.foreground, dark, key_window);
                if let Some(range) = ranges
                    .get(current)
                    .filter(|range| range.end > visible.start && range.start < visible.end)
                {
                    let current_fill = over_background(fills.current);
                    add_layer(&mut segments, &mut std::iter::once(range.clone()), current_fill);
                }
                let mut others = ranges
                    .iter()
                    .enumerate()
                    .skip(first)
                    .take_while(|(_, range)| range.start < visible.end)
                    .filter(|(index, _)| *index != current)
                    .map(|(_, range)| range.clone());
                add_layer(&mut segments, &mut others, over_background(fills.matches));
            }
        }

        segments
    }

    fn layout_document_colors(
        &self,
        document_colors: &[(Range<usize>, Hsla)],
        last_layout: &LastLayout,
        bounds: &Bounds<Pixels>,
        _cx: &mut App,
    ) -> Vec<(Path<Pixels>, Hsla)> {
        let mut paths = vec![];
        for (range, color) in document_colors.iter() {
            if let Some(path) = Self::layout_match_range(range.clone(), last_layout, bounds, px(0.)) {
                paths.push((path, *color));
            }
        }

        paths
    }

    fn layout_selections(
        &self,
        last_layout: &LastLayout,
        bounds: &mut Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<Path<Pixels>> {
        let state = self.state.read(cx);
        if !state.has_selection_focus(window, cx) {
            return vec![];
        }
        let radius = last_layout.line_height * SELECTION_CORNER_RADIUS;

        let active_id = state.active_selection().id;
        let mut paths = Vec::new();

        for selection in state.selections.iter() {
            let is_active = selection.id == active_id;
            let mut selected_range = *selection;

            // IME composition replaces the active selection with a collapsed
            // caret, so it never paints a selection highlight.
            if is_active {
                if let Some(ime_marked_range) = &state.ime_marked_range {
                    if !ime_marked_range.is_empty() {
                        let cursor = selection.cursor_offset();
                        selected_range = (cursor..cursor).into();
                    }
                }
            }
            if selected_range.is_empty() {
                continue;
            }

            if state.masked {
                selected_range.start = masked_display_offset(&state.text, selected_range.start);
                selected_range.end = masked_display_offset(&state.text, selected_range.end);
            }

            let (start_ix, end_ix) = if selected_range.start < selected_range.end {
                (selected_range.start, selected_range.end)
            } else {
                (selected_range.end, selected_range.start)
            };

            let range = start_ix.max(last_layout.visible_range_offset.start)
                ..end_ix.min(last_layout.visible_range_offset.end);

            if range.is_empty() {
                continue;
            }

            // A selected token shows its own selected state, so the text
            // highlight stops at its edges instead of painting behind it.
            let mut start = range.start;
            if state.tokens_visible() {
                let spans = state.token_spans();
                let first = spans.partition_point(|s| s.range().end <= range.start);
                for span in spans[first..]
                    .iter()
                    .take_while(|s| s.range().start < range.end)
                {
                    if start < span.range().start {
                        paths.extend(Self::layout_match_range(
                            start..span.range().start,
                            last_layout,
                            bounds,
                            radius,
                        ));
                    }
                    start = span.range().end.max(start);
                }
            }
            if start < range.end {
                paths.extend(Self::layout_match_range(
                    start..range.end,
                    last_layout,
                    bounds,
                    radius,
                ));
            }
        }

        paths
    }

    /// Calculate the visible range of lines in the viewport.
    ///
    /// Returns
    ///
    /// - visible_range: The visible range is based on unwrapped lines (Zero based).
    /// - visible_buffer_lines: Indices of non-hidden buffer lines within the visible range.
    /// - visible_top: The top position of the first visible line in the scroll viewport.
    fn calculate_visible_range(
        &self,
        state: &InputBaseState<M>,
        line_height: Pixels,
        input_height: Pixels,
    ) -> (Range<usize>, Vec<usize>, Pixels) {
        // Add extra rows to avoid showing empty space when scroll to bottom.
        let extra_rows = 1;
        if state.is_single_line() {
            return (0..1, vec![0], px(0.));
        }

        let total_lines = state.display_map.wrap_row_count();
        let display_count = state.display_map.display_row_count();
        let buffer_line_count = state.display_map.buffer_line_count();
        if display_count == 0 || buffer_line_count == 0 {
            return (0..0, Vec::new(), px(0.));
        }

        let mut scroll_top = if let Some(glide_y) = state.glide_y {
            glide_y
        } else if let Some(deferred_scroll_offset) = state.deferred_scroll_offset {
            deferred_scroll_offset.y
        } else {
            state.scroll_handle.offset().y
        };
        // Clamp as `layout_cursors` does, so we lay out the rows we paint.
        // Ghost lines are left out: a tighter clamp only lays out extra rows
        // above, and the last row stays in range.
        let scroll_height = line_height * total_lines
            + empty_bottom_height(
                state.is_code_editor(),
                state.scroll_beyond_last_line,
                input_height,
                line_height,
            );
        scroll_top = clamp_vertical_scroll_offset(
            scroll_top,
            scroll_height,
            input_height,
            state.top_inset(input_height, line_height),
        );

        // Display rows are uniformly `line_height` tall, so the visible window maps
        // directly to a display-row range.
        let viewport_top = (-scroll_top).max(px(0.));
        let viewport_bottom = viewport_top + input_height;
        let line_height_f = f32::from(line_height);
        let first_display =
            ((f32::from(viewport_top) / line_height_f).floor() as usize).min(display_count - 1);
        let last_display =
            ((f32::from(viewport_bottom) / line_height_f).ceil() as usize).min(display_count - 1);

        let start_line = state.display_map.display_row_to_buffer_line(first_display);
        let end_line = state.display_map.display_row_to_buffer_line(last_display);

        // y of the top of the first visible buffer line (in content space).
        let visible_top = match state
            .display_map
            .buffer_line_to_display_row_range(start_line)
        {
            Some(range) => line_height * range.start,
            None => line_height * first_display,
        };

        let visible_range = start_line..(end_line + 1 + extra_rows).min(buffer_line_count);

        // Collect non-hidden buffer lines within the visible range
        let mut visible_buffer_lines = Vec::with_capacity(visible_range.len());
        for ix in visible_range.clone() {
            if state.display_map.visible_wrap_row_count_for_buffer_line(ix) > 0 {
                visible_buffer_lines.push(ix);
            }
        }

        (visible_range, visible_buffer_lines, visible_top)
    }

    /// Return the gutter's width: the widest figures of the gutter's own font,
    /// then the margin (and the fold icons' column when folding).
    fn layout_line_numbers(
        state: &InputBaseState<M>,
        line_number_len: usize,
        window: &mut Window,
    ) -> Pixels {
        let mut line_number_width = if state.shows_line_numbers() {
            // A patch has an old and a new column, as wide as its widest number.
            let (line_number_len, columns) = match state.patch_rows.as_ref() {
                Some(rows) => (rows.digits, 2.),
                None => (line_number_len, 1.),
            };
            // Tabular figures share one width, so "0" stands for any digit.
            let widest = window.text_system().shape_line(
                "0".repeat(line_number_len).into(),
                system_look::GUTTER_SIZE,
                &[TextRun {
                    len: line_number_len,
                    font: system_look::gutter_font(),
                    color: gpui::black(),
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }],
                None,
            );

            widest.width * columns
                + if columns > 1. { PATCH_COLUMN_GAP } else { px(0.) }
                + LINE_NUMBER_RIGHT_MARGIN
        } else if state.is_code_editor() {
            LINE_NUMBER_RIGHT_MARGIN
        } else {
            px(0.)
        };

        if state.mode.is_folding() {
            // Add extra space for fold icons
            line_number_width += FOLD_ICON_HITBOX_WIDTH
        }

        line_number_width
    }

    /// Layout shaped lines for whitespace indicators (space and tab).
    ///
    /// Returns `WhitespaceIndicators` with shaped lines for space and tab characters.
    fn layout_whitespace_indicators(
        state: &InputBaseState<M>,
        text_size: Pixels,
        style: &TextStyle,
        window: &mut Window,
        _cx: &App,
    ) -> Option<WhitespaceIndicators> {
        if !state.show_whitespaces {
            return None;
        }

        let invisible_color = state
            .editor_style
            .editor_invisible
            .unwrap_or(state.editor_style.muted_foreground);


        let space_text = SharedString::new_static("·");
        let space = window.text_system().shape_line(
            space_text.clone(),
            text_size,
            &[TextRun {
                len: space_text.len(),
                font: style.font(),
                color: invisible_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );

        let tab_text = SharedString::new_static("→");
        let tab = window.text_system().shape_line(
            tab_text.clone(),
            text_size,
            &[TextRun {
                len: tab_text.len(),
                font: style.font(),
                color: invisible_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );

        Some(WhitespaceIndicators { space, tab })
    }

    /// Compute inline completion ghost lines for rendering.
    ///
    /// Returns (first_line, ghost_lines) where:
    /// - first_line: Shaped text for the first line (goes after cursor on same line)
    /// - ghost_lines: Shaped lines for subsequent lines (shift content down)
    fn layout_inline_completion(
        state: &InputBaseState<M>,
        visible_range: &Range<usize>,
        font_size: Pixels,
        window: &mut Window,
        _cx: &App,
    ) -> (Option<ShapedLine>, Vec<ShapedLine>) {
        // Must be focused to show inline completion
        if !state.focus_handle.is_focused(window) {
            return (None, vec![]);
        }

        let Some(completion_item) = state.extras.inline_completion_item() else {
            return (None, vec![]);
        };

        // Get cursor row from cursor position
        let cursor_row = state.cursor_position().line as usize;

        // Only show if cursor row is visible
        if cursor_row < visible_range.start || cursor_row >= visible_range.end {
            return (None, vec![]);
        }

        let completion_text = &completion_item.insert_text;
        let completion_color = state.editor_style.muted_foreground.opacity(0.5);

        let text_style = window.text_style();
        let font = text_style.font();

        let lines: Vec<&str> = completion_text.split('\n').collect();
        if lines.is_empty() {
            return (None, vec![]);
        }

        // Shape first line (goes after cursor)
        let first_text: SharedString = lines[0].to_string().into();
        let first_line = if !first_text.is_empty() {
            let first_run = TextRun {
                len: first_text.len(),
                font: font.clone(),
                color: completion_color,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            Some(
                window
                    .text_system()
                    .shape_line(first_text, font_size, &[first_run], None),
            )
        } else {
            None
        };

        // Shape ghost lines (lines 2+ that shift content down)
        let ghost_lines: Vec<ShapedLine> = lines[1..]
            .iter()
            .map(|line_text| {
                let text: SharedString = line_text.to_string().into();
                let len = text.len().max(1); // Ensure at least 1 for empty lines
                let run = TextRun {
                    len,
                    font: font.clone(),
                    color: completion_color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                // Use space for empty lines so they take up height
                let shaped_text = if text.is_empty() { " ".into() } else { text };
                window
                    .text_system()
                    .shape_line(shaped_text, font_size, &[run], None)
            })
            .collect();

        (first_line, ghost_lines)
    }

    /// Return (line_number_width, line_number_len)
    /// Layout fold icon hitboxes during prepaint phase.
    ///
    /// This creates hitboxes for the fold icon area, positioned to the right of line numbers.
    /// Icons are created and prepainted here to avoid panics.
    fn layout_fold_icons(
        &self,
        origin_x: Pixels,
        gutter_width: Pixels,
        bounds: &Bounds<Pixels>,
        viewport_bottom: Pixels,
        last_layout: &LastLayout,
        window: &mut Window,
        cx: &mut App,
    ) -> FoldIconLayout {
        // First pass: collect fold information from state
        struct FoldInfo {
            buffer_line: usize,
            is_folded: bool,
            display_row: usize,
            offset_y: Pixels,
            /// Folded lines: where the `...` chip starts, across from the text's left edge and
            /// down from the line's top.
            chip_at: Option<Point<Pixels>>,
        }

        let line_number_hitbox = window.insert_hitbox(
            Bounds::new(
                point(origin_x, bounds.origin.y + last_layout.visible_top),
                size(gutter_width, bounds.size.height),
            ),
            HitboxBehavior::Normal,
        );
        let gutter_hovered = line_number_hitbox.bounds.contains(&window.mouse_position());

        let mut icon_layout = FoldIconLayout {
            line_number_hitbox,
            gutter_hovered,
            icons: vec![],
            chips: vec![],
            candidates: false,
            curtain: None,
        };
        if !self.state.read(cx).mode.is_folding() {
            return icon_layout;
        }

        // The arrows fade with the pointer and a fold that just toggled glides, both on the
        // frame's time, from here: they ask for frames only while they move.
        let now = crate::motion::frame_time(window.window_handle().window_id());
        let reduce_motion = cx.reduce_motion();
        let (arrow_level, arrows_moving, glide) = self.state.update(cx, |state, _| {
            let motion = &mut state.fold_motion;
            if reduce_motion {
                motion.arrows = KeyEase::at(gutter_hovered);
            }
            let (level, moving) = motion.arrows.step(gutter_hovered, now);
            motion.watch(
                state.display_map.folded_ranges(),
                state.display_map.fold_generation(),
                state.document_revision,
                now,
            );
            if reduce_motion {
                motion.glide = None;
            }
            (level, moving, motion.glide_at(now))
        });
        if arrows_moving || glide.is_some() {
            window.request_animation_frame();
        }

        let mut glide_bottom = None;
        let mut opening_rows = 0;
        let fold_infos: Vec<FoldInfo> = {
            let state = self.state.read(cx);

            let mut infos = Vec::with_capacity(last_layout.visible_buffer_lines.len());
            let mut offset_y = last_layout.visible_top;

            for (line, &buffer_line) in last_layout
                .lines
                .iter()
                .zip(last_layout.visible_buffer_lines.iter())
            {
                let rows = line.wrapped_lines.len();
                if state.display_map.is_fold_candidate(buffer_line) {
                    let is_folded = state.display_map.is_folded_at(buffer_line);
                    let chip_at = is_folded.then(|| {
                        let last = line.wrapped_lines.last();
                        let indent = if rows > 1 { line.wrap_indent } else { px(0.) };
                        point(
                            last_layout.alignment_offset(line.longest_width)
                                + last.map_or(px(0.), |last| last.width)
                                + indent,
                            rows.saturating_sub(1) * last_layout.line_height,
                        )
                    });
                    infos.push(FoldInfo {
                        buffer_line,
                        is_folded,
                        display_row: buffer_line,
                        offset_y,
                        chip_at,
                    });
                }

                offset_y += rows * last_layout.line_height;
                if let Some((glide, _)) = glide {
                    if glide.line == buffer_line {
                        glide_bottom = Some(offset_y);
                    } else if glide.opening && buffer_line > glide.line && buffer_line < glide.end_line {
                        opening_rows += rows;
                    }
                }
            }

            infos
        }; // state is dropped here
        icon_layout.candidates = !fold_infos.is_empty();

        // A gliding fold draws its block's rows under a curtain that sweeps away (opening, from the
        // top down) or draws back (closing, up from the bottom): the rows below never move.
        if let (Some((glide, covered)), Some(top)) = (glide, glide_bottom) {
            let top = bounds.origin.y + top;
            let rows = if glide.opening { opening_rows } else { glide.rows };
            let span = (rows * last_layout.line_height).min(viewport_bottom - top).max(px(0.));
            icon_layout.curtain = Some(if glide.opening {
                (top + span * (1. - covered), top + span)
            } else {
                (top, top + span * covered)
            });
        }

        // The chips of folded lines: where the text of the first line ends.
        let chip_height = (last_layout.line_height - px(6.)).max(px(8.));
        for info in fold_infos.iter() {
            if let Some(at) = info.chip_at {
                let chip = Bounds::new(
                    point(
                        bounds.origin.x + last_layout.line_number_width + at.x + FOLD_CHIP_GAP,
                        bounds.origin.y + info.offset_y + at.y + (last_layout.line_height - chip_height).half(),
                    ),
                    size(FOLD_CHIP_WIDTH, chip_height),
                );
                // A chip scrolled under the gutter is gone.
                if chip.left() < origin_x + last_layout.line_number_width {
                    continue;
                }
                icon_layout.chips.push(FoldChip {
                    buffer_line: info.buffer_line,
                    bounds: chip,
                    hitbox: window.insert_hitbox(chip, HitboxBehavior::Normal),
                });
            }
        }

        // Second pass: create and prepaint icons
        let line_height = last_layout.line_height;
        let line_number_width =
            gutter_width - LINE_NUMBER_RIGHT_MARGIN - FOLD_ICON_HITBOX_WIDTH;
        let icon_relative_pos = point(
            (FOLD_ICON_HITBOX_WIDTH - FOLD_ICON_WIDTH).half(),
            (line_height - FOLD_ICON_WIDTH).half(),
        );

        for (ix, info) in fold_infos.iter().enumerate() {
            // An open block's arrow shows only while the pointer is near; a folded line's stays.
            if !info.is_folded && arrow_level <= 0. && !arrows_moving {
                continue;
            }
            // Position fold icon to the right of line numbers.
            // Use origin_x (unscrolled) so icons stay fixed in the gutter during horizontal scroll.
            let fold_icon_bounds = Bounds::new(
                point(
                    origin_x + icon_relative_pos.x + line_number_width,
                    bounds.origin.y + icon_relative_pos.y + info.offset_y,
                ),
                size(FOLD_ICON_HITBOX_WIDTH, line_height),
            );

            // Create and prepaint icon
            let child = self
                .state
                .read(cx)
                .editor_style
                .fold_icon_renderer
                .as_ref()
                .map(|render| render(ix, info.is_folded))
                .unwrap_or_else(|| {
                    let (path, svg) = if info.is_folded {
                        ("input-fold-chevron-right", FOLD_CHEVRON_RIGHT_SVG)
                    } else {
                        ("input-fold-chevron-down", FOLD_CHEVRON_DOWN_SVG)
                    };
                    gpui::canvas(
                        |_, _, _| {},
                        move |bounds, _, window, cx| {
                            let color = window.text_style().color;
                            let _ = window.paint_svg(
                                bounds,
                                path.into(),
                                Some(svg),
                                gpui::TransformationMatrix::default(),
                                color,
                                cx,
                            );
                        },
                    )
                    .size(FOLD_ICON_WIDTH)
                    .into_any_element()
                });
            let mut icon = gpui::div()
                .id(("fold", ix))
                .size(FOLD_ICON_WIDTH)
                .opacity(if info.is_folded { 1. } else { arrow_level })
                .child(child)
                .on_mouse_down(MouseButton::Left, {
                    let state = self.state.clone();
                    let buffer_line = info.buffer_line;
                    move |_, _: &mut Window, cx: &mut App| {
                        cx.stop_propagation();

                        state.update(cx, |state, cx| {
                            state.display_map.toggle_fold(buffer_line);
                            cx.notify();
                        });
                    }
                })
                .into_any_element();

            icon.prepaint_as_root(
                fold_icon_bounds.origin,
                fold_icon_bounds.size.into(),
                window,
                cx,
            );

            icon_layout
                .icons
                .push((info.display_row, info.is_folded, icon));
        }

        icon_layout
    }

    /// Paint the fold gutter's arrows, chips and glide curtain.
    ///
    /// - An open block's arrow fades in while the pointer is over the gutter and out when it
    ///   leaves (the fade is applied in layout); a folded line's arrow stays.
    /// - A folded line ends in a rounded `...` chip, and a click on it opens the fold.
    /// - A fold that just toggled draws its block's rows under a curtain of the editor's ground.
    fn paint_fold_icons(
        &mut self,
        fold_icon_layout: &mut FoldIconLayout,
        foreground: Hsla,
        ground: Hsla,
        gutter_ground: Hsla,
        gutter_bounds: Bounds<Pixels>,
        input_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let was_hovered = fold_icon_layout.gutter_hovered;
        if fold_icon_layout.candidates {
            // Mouse moves do not repaint the editor, so repaint when the pointer
            // enters or leaves the gutter to start the arrows fading.
            let gutter = fold_icon_layout.line_number_hitbox.bounds;
            let entity_id = self.state.entity_id();
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase.bubble() && gutter.contains(&event.position) != was_hovered {
                    cx.notify(entity_id);
                }
            });
        }
        for (_, _, icon) in fold_icon_layout.icons.iter_mut() {
            icon.paint(window, cx);
        }

        if !fold_icon_layout.chips.is_empty() {
            let dark = ground.l < 0.5;
            let fill_color = foreground.opacity(if dark { 0.16 } else { 0.12 });
            let dot_color = foreground.opacity(0.55);
            for chip in &fold_icon_layout.chips {
                window.paint_quad(
                    fill(chip.bounds, fill_color).corner_radii(Corners::all(chip.bounds.size.height.half())),
                );
                let step = (chip.bounds.size.width - FOLD_CHIP_DOT * 3.) / 4.;
                for dot in 0..3 {
                    let at = point(
                        chip.bounds.origin.x + step * (dot + 1) as f32 + FOLD_CHIP_DOT * dot as f32,
                        chip.bounds.origin.y + (chip.bounds.size.height - FOLD_CHIP_DOT).half(),
                    );
                    window.paint_quad(
                        fill(Bounds::new(at, size(FOLD_CHIP_DOT, FOLD_CHIP_DOT)), dot_color)
                            .corner_radii(Corners::all(FOLD_CHIP_DOT.half())),
                    );
                }
                window.set_cursor_style(gpui::CursorStyle::PointingHand, &chip.hitbox);
            }
            let chips = std::mem::take(&mut fold_icon_layout.chips);
            let state = self.state.clone();
            window.on_mouse_event(move |event: &gpui::MouseDownEvent, phase, window, cx| {
                if !phase.bubble() || event.button != MouseButton::Left {
                    return;
                }
                if let Some(FoldChip { buffer_line, .. }) = chips.iter().find(|chip| chip.hitbox.is_hovered(window)) {
                    cx.stop_propagation();
                    state.update(cx, |state, cx| {
                        state.display_map.set_folded(*buffer_line, false);
                        cx.notify();
                    });
                }
            });
        }

        if let Some((top, bottom)) = fold_icon_layout.curtain.filter(|(top, bottom)| bottom > top) {
            let left = gutter_bounds.origin.x;
            let band = |x: Pixels, width: Pixels| Bounds::new(point(x, top), size(width, bottom - top));
            let right = input_bounds.origin.x + input_bounds.size.width;
            window.paint_quad(fill(band(left, right - left), ground));
            if gutter_ground != ground {
                window.paint_quad(fill(band(left, gutter_bounds.size.width), gutter_ground));
            }
        }
    }

    fn token_element(
        &self,
        token: &super::InlineTokenContext,
        window: &mut Window,
        cx: &mut App,
    ) -> AnyElement {
        use gpui::{
            InteractiveElement as _, StatefulInteractiveElement as _, prelude::FluentBuilder as _,
        };
        let presentation = self.state.read(cx).token_presentation.clone();
        let child = presentation.render(token, window, cx);
        // Preserve interaction state only for the same token occurrence and
        // hover eligibility. Replacements and re-enabling start fresh.
        let start = token.range().start;
        let down_state = self.state.clone();
        let click_state = self.state.clone();
        let move_state = self.state.clone();
        let hover_state = self.state.clone();
        let hover_token = token.token().clone();
        let disabled = token.is_disabled();
        let hover_enabled = presentation.has_hover_listener() && !disabled;
        let id = gpui::ElementId::from(("inline-token", start));
        let id = gpui::ElementId::from((id, token.token().id().clone()));
        let id = gpui::ElementId::from((id, token.token().text().clone()));
        let id = gpui::ElementId::from((id, token.token().label().clone()));
        let id = gpui::ElementId::from((
            id,
            gpui::SharedString::from(if hover_enabled {
                "hover-enabled"
            } else {
                "hover-disabled"
            }),
        ));
        let accessible = presentation.has_listener() && !disabled;
        let accessible_state = self.state.clone();
        gpui::div()
            .id(id)
            .flex()
            .items_center()
            .h(token.line_height())
            .max_w(token.available_width())
            .overflow_hidden()
            // A token is an object, not text: the arrow, never the I-beam.
            .cursor_default()
            .when(accessible, |this| {
                this.role(gpui::Role::Button)
                    .aria_label(token.token().label().clone())
                    .on_a11y_action(gpui::AccessibleAction::Click, move |_, window, cx| {
                        let state = accessible_state.read(cx);
                        let activation = state
                            .token_spans()
                            .iter()
                            .find(|s| s.range().start == start)
                            .and_then(|span| {
                                state.range_to_bounds(&span.range()).and_then(|bounds| {
                                    state.token_activation(
                                        start,
                                        bounds,
                                        gpui::ClickEvent::Keyboard(gpui::KeyboardClickEvent {
                                            bounds,
                                            ..Default::default()
                                        }),
                                    )
                                })
                            });
                        if let Some((listener, event)) = activation {
                            listener(&event, window, cx);
                        }
                    })
            })
            .capture_any_mouse_down(move |_, window, cx| {
                if disabled {
                    window.prevent_default();
                    cx.stop_propagation();
                }
            })
            .capture_any_mouse_up(move |_, window, cx| {
                if disabled {
                    window.prevent_default();
                    cx.stop_propagation();
                }
            })
            .capture_key_down(move |_, _, cx| {
                if disabled {
                    cx.stop_propagation();
                }
            })
            .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                if window.default_prevented() {
                    return;
                }
                down_state.update(cx, |state, cx| {
                    state.pressed_token = None;
                    if event.click_count == 2 {
                        if let Some(span) = state
                            .token_spans()
                            .iter()
                            .find(|s| s.range().start == start)
                        {
                            state.set_selected_range(span.range(), cx);
                            state.focus(window, cx);
                            window.prevent_default();
                        }
                    } else if event.click_count == 1
                        && !event.modifiers.shift
                        && !event.modifiers.alt
                    {
                        state.pressed_token =
                            Some((start, state.document_revision, event.position));
                    }
                });
            })
            .on_mouse_move(move |event, _, cx| {
                move_state.update(cx, |state, _| {
                    if state.pressed_token.as_ref().is_some_and(|(_, _, p)| {
                        (event.position.x - p.x).abs() + (event.position.y - p.y).abs() > px(4.)
                    }) {
                        state.pressed_token = None;
                    }
                });
            })
            .on_click(move |event, window, cx| {
                let activation = click_state.update(cx, |state, cx| {
                    let (pressed, revision, _) = state.pressed_token.take()?;
                    if pressed != start || revision != state.document_revision {
                        return None;
                    }
                    if event.modifiers().shift || event.modifiers().alt {
                        return None;
                    }
                    let span = state
                        .token_spans()
                        .iter()
                        .find(|s| s.range().start == start)?
                        .clone();
                    // A click selects the whole token; opening it is the
                    // listener's decision.
                    state.set_selected_range(span.range(), cx);
                    let bounds = state.range_to_bounds(&span.range())?;
                    state.token_activation(start, bounds, event.clone())
                });
                if let Some((listener, event)) = activation {
                    listener(&event, window, cx);
                }
            })
            .on_hover(move |hovered, window, cx| {
                // Hover never selects or edits; it only reports presence so the
                // application can show a tooltip or run custom logic.
                let hovered = *hovered;
                let activation = hover_state.update(cx, |state, _| {
                    // Anchor to the real laid-out token rect, not the
                    // range-to-bounds guess: a token at a soft-wrap boundary
                    // would otherwise report a zero/negative width or a
                    // two-row height.
                    let bounds = if hovered {
                        state.token_bounds.get(&start).copied()?
                    } else {
                        // Exits use entry geometry even if an earlier callback
                        // removed the token and its current bounds.
                        Bounds::default()
                    };
                    state.token_hover(start, bounds, hovered, Some(&hover_token))
                });
                if let Some((listener, event)) = activation {
                    listener(&event, window, cx);
                }
            })
            .child(child)
            .into_any_element()
    }

    fn measure_tokens(
        &self,
        width: Pixels,
        line_height: Pixels,
        viewport: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) -> std::collections::HashMap<usize, (AnyElement, Size<Pixels>)> {
        let style = window.text_style();
        let state = self.state.read(cx);
        let key = (
            style.font(),
            style.font_size.to_pixels(window.rem_size()),
            width,
            line_height,
            state.is_single_line() || !state.soft_wrap,
        );
        if !state.tokens_visible() {
            if state.token_layout_cache.is_none() {
                let mut exit = self.state.update(cx, |state, _| {
                    state.token_bounds.clear();
                    state.reconcile_token_hover()
                });
                while let Some((listener, event)) = exit {
                    listener(&event, window, cx);
                    exit = self
                        .state
                        .update(cx, |state, _| state.reconcile_token_hover());
                }
                return Default::default();
            }
            self.state.update(cx, |state, cx| {
                state.display_map.set_inline_metrics(Rc::from([]), cx)
            });
            let mut exit = self.state.update(cx, |state, _| {
                state.token_bounds.clear();
                state.reconcile_token_hover()
            });
            while let Some((listener, event)) = exit {
                listener(&event, window, cx);
                exit = self
                    .state
                    .update(cx, |state, _| state.reconcile_token_hover());
            }
            return Default::default();
        }
        let revision = state.document_revision;
        let cache = state.token_layout_cache.as_ref();
        let all = cache.is_none_or(|cache| {
            cache.key.as_ref() != Some(&key)
                || cache.revision != revision
                || state
                    .token_spans()
                    .iter()
                    .any(|span| !cache.widths.contains_key(span.token()))
        });
        let (visible, _, _) = self.calculate_visible_range(state, line_height, viewport);
        let start = state.text.line_start_offset(visible.start);
        let end = state.text.line_end_offset(visible.end.saturating_sub(1));
        let spans = state.token_spans();
        let first = if all {
            0
        } else {
            spans.partition_point(|s| s.range().end <= start)
        };
        let contexts: Vec<_> = spans[first..]
            .iter()
            .take_while(|s| all || s.range().start <= end)
            .filter(|span| {
                let range = span.range();
                (range.start <= end && range.end >= start)
                    || cache.is_none_or(|cache| {
                        cache.key.as_ref() != Some(&key) || !cache.widths.contains_key(span.token())
                    })
            })
            .map(|span| state.token_context(span, line_height, width))
            .collect();
        let mut elements = std::collections::HashMap::new();
        let mut measured = Vec::new();
        for token in contexts {
            let mut element = self.token_element(&token, window, cx);
            let size = element.layout_as_root(
                size(
                    gpui::AvailableSpace::MaxContent,
                    gpui::AvailableSpace::Definite(line_height),
                ),
                window,
                cx,
            );
            measured.push((token.token().clone(), size.width.min(width).max(px(1.))));
            elements.insert(token.range().start, (element, size));
        }
        self.state.update(cx, |state, cx| {
            let mut cache = state.token_layout_cache.take().unwrap_or_default();
            let mut changed = cache.key.as_ref() != Some(&key) || cache.revision != revision;
            if cache.key.as_ref() != Some(&key) {
                cache.widths.clear();
            }
            for (token, width) in measured {
                changed |= cache.widths.get(&token) != Some(&width);
                cache.widths.insert(token, width);
            }
            cache.key = Some(key);
            if changed {
                let spans = state.token_spans();
                let tokens: std::collections::HashSet<_> =
                    spans.iter().map(|s| s.token()).collect();
                cache.widths.retain(|token, _| tokens.contains(token));
                cache.metrics = spans
                    .iter()
                    .filter_map(|span| {
                        cache
                            .widths
                            .get(span.token())
                            .map(|width| (span.range(), *width))
                    })
                    .collect();
                cache.revision = revision;
                if state.is_single_line() || !state.soft_wrap {
                    let mut rows: Vec<_> = spans
                        .iter()
                        .map(|s| state.text.offset_to_point(s.range().start).row)
                        .collect();
                    rows.push(state.display_map.longest_row());
                    rows.sort_unstable();
                    rows.dedup();
                    cache.unwrapped_width = rows
                        .into_iter()
                        .map(|row| {
                            let start = state.text.line_start_offset(row);
                            let text = state.text.slice_line(row).to_string();
                            let mut offset = 0;
                            let mut width = px(0.);
                            let first = spans.partition_point(|s| s.range().end <= start);
                            for span in spans[first..]
                                .iter()
                                .take_while(|s| s.range().start < start + text.len())
                            {
                                let local = span.range().start - start..span.range().end - start;
                                let part = &text[offset..local.start];
                                width += window
                                    .text_system()
                                    .shape_line(
                                        part.to_owned().into(),
                                        style.font_size.to_pixels(window.rem_size()),
                                        &[style.to_run(part.len())],
                                        None,
                                    )
                                    .width;
                                width +=
                                    cache.widths.get(span.token()).copied().unwrap_or_default();
                                offset = local.end;
                            }
                            let part = &text[offset..];
                            width
                                + window
                                    .text_system()
                                    .shape_line(
                                        part.to_owned().into(),
                                        style.font_size.to_pixels(window.rem_size()),
                                        &[style.to_run(part.len())],
                                        None,
                                    )
                                    .width
                        })
                        .max()
                        .unwrap_or_default();
                }
            }
            let metrics = cache.metrics.clone();
            state.token_layout_cache = Some(cache);
            state.display_map.set_inline_metrics(metrics, cx);
            if state.mode.is_auto_grow() {
                let rows = state.mode.rows();
                state.mode.update_auto_grow(&state.display_map);
                if state.mode.rows() != rows {
                    cx.notify();
                }
            }
        });
        elements
    }

    fn layout_token_lines(
        state: &InputBaseState<M>,
        last_layout: &LastLayout,
        font_size: Pixels,
        runs: &[TextRun],
        window: &mut Window,
    ) -> Vec<LineLayout> {
        use crate::input::display_map::{InlineFragment, InputLine};
        let spans = state.token_spans();
        let cache = state
            .token_layout_cache
            .as_ref()
            .expect("tokens measured before shaping");
        let mut run_offset = 0;
        last_layout
            .visible_buffer_lines
            .iter()
            .map(|&row| {
                let line_start = state.text.line_start_offset(row);
                let text: String = state.text.slice_line(row).into();
                let ranges = if state.is_single_line() {
                    smallvec::smallvec![0..text.len()]
                } else {
                    state
                        .display_map
                        .line(row)
                        .expect("prepared line")
                        .wrapped_lines
                        .iter()
                        .collect::<SmallVec<[_; 1]>>()
                };
                let mut lines: SmallVec<[InputLine; 1]> = SmallVec::new();
                for range in ranges {
                    let mut fragments = Vec::new();
                    let mut offset = range.start;
                    let mut x = px(0.);
                    let first =
                        spans.partition_point(|s| s.range().end <= line_start + range.start);
                    for span in spans[first..]
                        .iter()
                        .take_while(|s| s.range().start < line_start + range.end)
                    {
                        let local = span.range().start - line_start..span.range().end - line_start;
                        if offset < local.start {
                            let part = offset..local.start;
                            let shaped = window.text_system().shape_line(
                                text[part.clone()].to_owned().into(),
                                font_size,
                                &runs_for_range(runs, run_offset, &part),
                                None,
                            );
                            let width = shaped.width;
                            fragments.push(InlineFragment {
                                range: part.start - range.start..part.end - range.start,
                                x,
                                width,
                                text: Some(shaped),
                            });
                            x += width;
                        }
                        let width = cache.widths.get(span.token()).copied().unwrap_or_default();
                        fragments.push(InlineFragment {
                            range: local.start - range.start..local.end - range.start,
                            x,
                            width,
                            text: None,
                        });
                        x += width;
                        offset = local.end;
                    }
                    if offset < range.end {
                        let part = offset..range.end;
                        let shaped = window.text_system().shape_line(
                            text[part.clone()].to_owned().into(),
                            font_size,
                            &runs_for_range(runs, run_offset, &part),
                            None,
                        );
                        let width = shaped.width;
                        fragments.push(InlineFragment {
                            range: part.start - range.start..part.end - range.start,
                            x,
                            width,
                            text: Some(shaped),
                        });
                    }
                    lines.push(InputLine::inline(text[range].to_owned().into(), fragments));
                }
                let (indent, hang) = state
                    .display_map
                    .line(row)
                    .map_or((0, 0), |line| (line.indent, line.hang));
                let wrap_indent = if (indent > 0 || hang > 0) && lines.len() > 1 {
                    let bytes = text
                        .char_indices()
                        .nth(indent as usize)
                        .map_or(text.len(), |(ix, _)| ix);
                    lines[0].x_for_index(bytes) + last_layout.space_width * hang as f32
                } else {
                    px(0.)
                };
                run_offset += text.len() + 1;
                LineLayout::new()
                    .inline_lines(lines)
                    .wrap_indent(wrap_indent)
            })
            .collect()
    }

    /// Asks the scroll containers around a focused input to bring it into
    /// view, once when it gains focus and again whenever the fully visible
    /// part of the window changes, so manual scrolling in between is left
    /// alone. Ancestors must support autoscroll requests; inset-only keyboard
    /// avoidance additionally requires the ancestor to use the fully visible
    /// viewport for its scrolling bounds and scroll range. Stock GPUI's List
    /// uses layout bounds and does not avoid inset-only occlusion.
    ///
    /// A single-line input asks for all of itself. A taller one asks for the
    /// line with the caret, or its top part when it has no caret.
    fn request_reveal(
        &self,
        input_bounds: Bounds<Pixels>,
        cursor_infos: &[CursorRenderInfo],
        cursor_scroll_offset: Point<Pixels>,
        line_height: Pixels,
        window: &mut Window,
        cx: &mut App,
    ) {
        /// Room left around the request for the field's padding and border.
        const REVEAL_MARGIN: Pixels = px(8.);
        /// How much of a caretless multi-line input is revealed.
        const REVEAL_MAX_HEIGHT: Pixels = px(120.);

        let focused = self.state.read(cx).focus_handle.is_focused(window);
        let viewport = window.fully_visible_bounds();
        let reveal = self.state.update(cx, |state, _| {
            let target = focused.then_some(viewport);
            let changed = state.revealed_viewport != target;
            state.revealed_viewport = target;
            focused && changed
        });
        if !reveal || input_bounds.size.height <= px(0.) {
            return;
        }

        let (top, height) = if !self.state.read(cx).is_multi_line() {
            (input_bounds.top(), input_bounds.size.height)
        } else if let Some(caret) = cursor_infos.iter().find(|info| info.is_active) {
            let top = (caret.bounds.top() + cursor_scroll_offset.y)
                .clamp(input_bounds.top(), input_bounds.bottom());
            (top, line_height.min(input_bounds.bottom() - top))
        } else {
            (
                input_bounds.top(),
                input_bounds.size.height.min(REVEAL_MAX_HEIGHT),
            )
        };
        let request = Bounds::new(
            point(input_bounds.left(), top),
            size(input_bounds.size.width, height),
        );
        window.request_autoscroll(request.dilate(REVEAL_MARGIN));
    }

    fn prepaint_tokens(
        &self,
        layout: &LastLayout,
        bounds: Bounds<Pixels>,
        mut measured: std::collections::HashMap<usize, (AnyElement, Size<Pixels>)>,
        window: &mut Window,
        cx: &mut App,
    ) -> Vec<AnyElement> {
        let state = self.state.read(cx);
        if !state.tokens_visible() {
            let mut exit = self.state.update(cx, |state, _| {
                state.token_bounds.clear();
                state.reconcile_token_hover()
            });
            while let Some((listener, event)) = exit {
                listener(&event, window, cx);
                exit = self
                    .state
                    .update(cx, |state, _| state.reconcile_token_hover());
            }
            return vec![];
        }
        let width = state
            .token_layout_cache
            .as_ref()
            .and_then(|c| c.key.as_ref())
            .map_or(bounds.size.width, |k| k.2);
        let mut placements = Vec::new();
        let mut y = layout.visible_top;
        for (ix, &row) in layout.visible_buffer_lines.iter().enumerate() {
            let start = state.text.line_start_offset(row);
            let end = state.text.line_end_offset(row);
            let spans = state.token_spans();
            let first = spans.partition_point(|s| s.range().end <= start);
            for span in spans[first..].iter().take_while(|s| s.range().start < end) {
                if let Some(position) =
                    layout.lines[ix].position_for_index(span.range().start - start, layout, false)
                {
                    placements.push((
                        state.token_context(span, layout.line_height, width),
                        bounds.origin + position + point(layout.line_number_width, y),
                    ));
                }
            }
            y += layout.lines[ix].size(layout.line_height).height;
        }
        let mut out = Vec::new();
        let mut token_bounds = std::collections::HashMap::new();
        for (token, origin) in placements {
            let (mut element, element_size) = match measured.remove(&token.range().start) {
                Some(found) => found,
                None => {
                    let mut element = self.token_element(&token, window, cx);
                    let element_size = element.layout_as_root(
                        size(
                            gpui::AvailableSpace::MaxContent,
                            gpui::AvailableSpace::Definite(layout.line_height),
                        ),
                        window,
                        cx,
                    );
                    (element, element_size)
                }
            };
            element.prepaint_at(origin, window, cx);
            token_bounds.insert(token.range().start, Bounds::new(origin, element_size));
            out.push(element);
        }
        let mut exit = self.state.update(cx, |state, _| {
            state.token_bounds = token_bounds;
            state.reconcile_token_hover()
        });
        while let Some((listener, event)) = exit {
            listener(&event, window, cx);
            exit = self
                .state
                .update(cx, |state, _| state.reconcile_token_hover());
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn layout_lines(
        state: &InputBaseState<M>,
        display_text: &Rope,
        last_layout: &LastLayout,
        font_size: Pixels,
        runs: &[TextRun],
        bg_segments: &[(Range<usize>, Hsla)],
        faded: &[(Range<usize>, f32)],
        whitespace_indicators: Option<WhitespaceIndicators>,
        // Buffer byte ranges of the hidden characters shown as tags.
        hidden: &mut Vec<Range<usize>>,
        window: &mut Window,
    ) -> Vec<LineLayout> {
        let is_single_line = state.is_single_line();
        if state.tokens_visible() {
            return Self::layout_token_lines(state, last_layout, font_size, runs, window);
        }

        let tab_stop = tab_stop_width(
            window.text_system(),
            &window.text_style().font(),
            font_size,
            state.mode.tab_size().tab_size,
        );

        if is_single_line {
            let text: SharedString = display_text.to_string().into();
            let aligned_runs = align_runs_to_char_boundaries(&text, runs);
            let line_runs = aligned_runs.as_deref().unwrap_or(runs);
            let mut shaped_line = window
                .text_system()
                .shape_line(text, font_size, line_runs, None);
            expand_tabs(&mut shaped_line, tab_stop);

            let line_layout = LineLayout::new()
                .lines(smallvec::smallvec![shaped_line])
                .with_background(has_background(line_runs))
                .with_whitespaces(whitespace_indicators);
            return vec![line_layout];
        }

        // Empty to use placeholder, the placeholder is not in the wrapper map.
        if state.text.len() == 0 {
            let placeholder_text = display_text.to_string();
            let mut placeholder_lines = SmallVec::new();
            let mut line_has_background = false;

            for (line, line_runs) in placeholder_line_runs(&placeholder_text, runs) {
                let shaped_line = window.text_system().shape_line(
                    line.to_string().into(),
                    font_size,
                    &line_runs,
                    None,
                );
                line_has_background |= has_background(&line_runs);
                placeholder_lines.push(shaped_line);
            }

            // Keep placeholder lines in a single layout to stay parallel with visible_* metadata.
            let line_layout = LineLayout::new()
                .lines(placeholder_lines)
                .with_background(line_has_background)
                .with_whitespaces(whitespace_indicators);
            return vec![line_layout];
        }

        let mut lines = Vec::with_capacity(last_layout.visible_buffer_lines.len());
        // run_offset tracks position in the runs vec coordinate space (only visible line bytes).
        // This is separate from the visible_text offset because runs from highlight_lines
        // only cover visible (non-folded) lines.
        let mut run_offset = 0;

        for (vi, &buffer_line) in last_layout.visible_buffer_lines.iter().enumerate() {
            let line_text: String = display_text.slice_line(buffer_line).into();
            let line_item = state
                .display_map
                .line(buffer_line)
                .expect("line should exists in wrapper");

            debug_assert_eq!(line_item.len(), line_text.len());

            let mut wrapped_lines: SmallVec<[ShapedLine; 1]> = SmallVec::with_capacity(1);
            let mut line_has_background = false;

            for range in &line_item.wrapped_lines {
                let line_runs = runs_for_range(runs, run_offset, &range);
                let line_runs = if bg_segments.is_empty() {
                    line_runs
                } else {
                    split_runs_by_bg_segments(
                        last_layout.visible_line_byte_offsets[vi] + (range.start),
                        &line_runs,
                        bg_segments,
                        faded,
                        state.editor_style.background,
                    )
                };

                // Hidden and direction-changing characters show as symbols
                // or rounded tags (gpui-editor#ytm8, #88sw). Plain lines pay
                // one pass over their bytes.
                let row_text = &line_text[range.clone()];
                let invisible = invisibles::scan(row_text);
                let sub_line = if invisible.is_empty() {
                    SharedString::from(row_text.to_string())
                } else {
                    let row_start = last_layout.visible_line_byte_offsets[vi] + range.start;
                    for &(index, c) in &invisible {
                        let at = row_start + index;
                        match hidden.last_mut() {
                            Some(last) if last.end == at => last.end = at + c.len_utf8(),
                            _ => hidden.push(at..at + c.len_utf8()),
                        }
                    }
                    invisibles::stand_in(row_text, &invisible)
                };
                let line_runs =
                    align_runs_to_char_boundaries(&sub_line, &line_runs).unwrap_or(line_runs);
                let mut shaped_line = window
                    .text_system()
                    .shape_line(sub_line, font_size, &line_runs, None);
                if !invisible.is_empty() {
                    invisibles::splice_symbols(
                        &mut shaped_line,
                        &invisible,
                        &line_runs,
                        font_size,
                        window,
                    );
                }
                expand_tabs(&mut shaped_line, tab_stop);

                line_has_background |= has_background(&line_runs);
                wrapped_lines.push(shaped_line);
            }

            // Use the first visual line's indentation width for continuation lines.
            let wrap_indent = if (line_item.indent > 0 || line_item.hang > 0)
                && wrapped_lines.len() > 1
            {
                let indent_byte_len = line_text
                    .char_indices()
                    .nth(line_item.indent as usize)
                    .map(|(ix, _)| ix)
                    .unwrap_or(line_text.len());
                wrapped_lines[0].x_for_index(indent_byte_len)
                    + last_layout.space_width * line_item.hang as f32
            } else {
                px(0.)
            };

            let line_layout = LineLayout::new()
                .lines(wrapped_lines)
                .wrap_indent(wrap_indent)
                .with_background(line_has_background)
                .with_whitespaces(whitespace_indicators.clone());
            lines.push(line_layout);

            // +1 for the `\n`
            run_offset += line_text.len() + 1;
        }

        lines
    }

    /// First usize is the offset of skipped.
    fn highlight_lines(
        &mut self,
        visible_buffer_lines: &[usize],
        _visible_top: Pixels,
        visible_byte_range: Range<usize>,
        base: Hsla,
        now: std::time::Instant,
        cx: &mut App,
    ) -> Option<Vec<(Range<usize>, HighlightStyle)>> {
        let state = self.state.read(cx);
        let text = &state.text;
        let is_multi_line = state.is_multi_line();

        let (mut highlighter, diagnostics) = match &state.mode {
            LayoutMode::CodeEditor {
                highlighter,
                diagnostics,
                ..
            } => (highlighter.borrow_mut(), diagnostics),
            _ => {
                return (!state.masked)
                    .then(|| {
                        compose_decoration_collections(
                            Vec::new(),
                            state.extras.decoration_layers().into_iter(),
                            visible_byte_range,
                        )
                    })
                    .flatten();
            }
        };
        let Some(highlighter) = highlighter.as_mut() else {
            return (!state.masked)
                .then(|| {
                    compose_decoration_collections(
                        Vec::new(),
                        state.extras.decoration_layers().into_iter(),
                        visible_byte_range,
                    )
                })
                .flatten();
        };

        // The query below is the costly part and depends only on the visible
        // lines, the text, the parse the highlighter holds and the theme, so
        // the last answer is reused while all of them match. Highlighters
        // that report no generation are never cached.
        let generation = highlighter.syntax_generation();
        let mut entry = state.highlight_cache.borrow_mut().take().unwrap_or_default();
        let hit = generation.is_some()
            && entry.generation == generation
            && entry.revision == state.document_revision
            && entry.len == text.len()
            && entry.multi_line == is_multi_line
            && entry.lines == visible_buffer_lines
            && entry
                .resolver
                .as_ref()
                .is_some_and(|r| Arc::ptr_eq(r, &state.editor_style.highlight_styles));
        if !hit {
            entry.styles.clear();
            entry.group_ranges.clear();
            entry.lines.clear();
            entry.lines.extend_from_slice(visible_buffer_lines);
            entry.generation = generation;
            entry.revision = state.document_revision;
            entry.len = text.len();
            entry.multi_line = is_multi_line;
            entry.resolver = Some(state.editor_style.highlight_styles.clone());
        }
        let HighlightCache {
            styles: cached_styles,
            group_ranges,
            ..
        } = &mut *entry;
        // Byte ranges of the flushed line groups; the composed styles are
        // clipped to these at the end so the resulting runs cover exactly the
        // visible (non-folded) lines' bytes.

        // Helper to flush a contiguous range of lines. These ranges are disjoint,
        // so appending avoids repeatedly cloning and recombining prior styles.
        let mut flush_range =
            |start_line: usize, end_line: usize, skip: bool, styles: &mut Vec<_>| {
                let byte_start = text.line_start_offset(start_line);
                let byte_end = if is_multi_line {
                    // +1 for `\n`
                    text.line_start_offset(end_line + 1)
                } else {
                    text.line_end_offset(end_line)
                };
                let range_styles = if skip {
                    vec![(byte_start..byte_end, HighlightStyle::default())]
                } else {
                    let range = byte_start..byte_end;
                    let resolver = state.editor_style.highlight_styles.as_ref();
                    let semantic = state.semantic_palette.as_deref();
                    let mut variables = Vec::new();
                    let range_styles = if semantic.is_some() {
                        highlighter.styles_with_variables(&range, resolver, &mut variables)
                    } else {
                        highlighter.styles(&range, resolver)
                    };
                    // Rainbow brackets and semantic variable colors, computed
                    // here with the query so a cached frame does nothing new.
                    // The palette color is set on the runs explicitly: it must
                    // beat the theme's color for the same bytes.
                    let overlay = crate::input::highlighting::palette_overlay(
                        &**highlighter,
                        &range,
                        state.rainbow_palette.as_deref(),
                        semantic,
                        &variables,
                    );
                    if overlay.is_empty() {
                        range_styles
                    } else {
                        crate::input::highlighting::apply_overlay(range_styles, &overlay)
                    }
                };

                group_ranges.push(byte_start..byte_end);
                styles.extend(range_styles);
            };

        // Group contiguous visible lines into ranges and call styles() once per range
        let mut visible_iter = visible_buffer_lines.iter().peekable();
        let mut range_start: Option<usize> = None;

        while let Some(&line) = visible_iter.next() {
            if hit {
                break;
            }
            // Check if this line is too long for highlighting
            let line_len = text.slice_line(line).len();
            if line_len > MAX_HIGHLIGHT_LINE_LENGTH {
                // Flush any accumulated range first
                if let Some(start) = range_start.take() {
                    flush_range(start, line - 1, false, cached_styles);
                }

                flush_range(line, line, true, cached_styles);
                continue;
            }

            range_start.get_or_insert(line);

            // Check if next line is contiguous, if so keep accumulating
            if visible_iter
                .peek()
                .map(|&&next| next == line + 1)
                .unwrap_or(false)
            {
                continue;
            }

            // Flush the contiguous range
            let start_line = range_start.take().unwrap();
            flush_range(start_line, line, false, cached_styles);
        }

        // The cache holds the highlighter's own colours; while a language change eases in, the
        // copy drawn this frame is mixed from the plain text colour toward them, so the cache
        // stays right and a frame with no fade does nothing more than the clone.
        let mix = if state.text.len() == 0 || cx.reduce_motion() {
            state.syntax_mix.disarm();
            1.
        } else {
            state.syntax_mix.level(|| cached_styles.iter().any(|(_, style)| style.color.is_some()), generation.is_some(), now)
        };
        let mut styles: Vec<(Range<usize>, HighlightStyle)> =
            Vec::with_capacity(cached_styles.len() + 1);
        if mix < 1. {
            styles.extend(cached_styles.iter().cloned().map(|(range, mut style)| {
                style.color = style.color.map(|color| mix_fill(base, color, mix));
                (range, style)
            }));
        } else {
            styles.extend(cached_styles.iter().cloned());
        }

        let diagnostic_styles: Vec<_> = diagnostics
            .range(visible_byte_range.clone())
            .map(|entry| {
                (
                    entry.range.clone(),
                    diagnostic_highlight_style(entry.severity, state.editor_style.diagnostics),
                )
            })
            .collect();

        // Range semantic tokens, resolved from the LSP provider's cached
        // result through the active highlight theme so it shares the same
        // colour vocabulary as the tree-sitter path. Empty Vec when no
        // provider is set, so `combine_highlights` short-circuits.
        let custom_styles = state.extras.semantic_token_styles(
            text,
            &visible_byte_range,
            state.editor_style.highlight_styles.as_ref(),
        );

        // hover definition style
        if let Some(hover_style) = M::hover_definition_style(self.state.read(cx), cx) {
            styles.push(hover_style);
        }

        // Compose tree-sitter, semantic, application, then diagnostic styles.
        styles = gpui::combine_highlights(custom_styles, styles).collect();
        if !state.masked {
            styles = compose_decoration_collections(
                styles,
                state.extras.decoration_layers().into_iter(),
                visible_byte_range.clone(),
            )
            .unwrap_or_default();
        }
        styles = gpui::combine_highlights(diagnostic_styles, styles).collect();

        // Some sources reach outside the flushed groups — a diagnostic
        // straddling the viewport edge, or any range inside a folded region —
        // which would inject extra bytes into the runs coordinate space and
        // shift every later run boundary (see `layout_lines`). Clip the
        // composed styles back to the groups.
        styles = clip_styles_to_ranges(styles, group_ranges);
        *state.highlight_cache.borrow_mut() = Some(entry);

        Some(styles)
    }
}

/// One visible screen of scope geometry, shared by successive blink/selection frames.
/// Colours are resolved at paint time. A hidden editor releases this with its shaped lines.
pub(super) struct SearchScopeCache {
    scope: Arc<Vec<Range<usize>>>,
    layout: LastLayout,
    bounds: Bounds<Pixels>,
    paths: Rc<Vec<Path<Pixels>>>,
}

/// The last highlight query for the visible lines and what it was asked
/// under. Reused while the visible lines, text, parse and theme all match,
/// so a sub-line wheel step or a blink relayout runs no query. One entry per
/// editor; the buffers are refilled in place.
#[derive(Default)]
pub(super) struct HighlightCache {
    generation: Option<u64>,
    revision: u64,
    len: usize,
    multi_line: bool,
    lines: Vec<usize>,
    /// Held so the theme it was queried with cannot be freed and its address
    /// reused by a different one.
    resolver: Option<crate::input::SharedHighlightStyleResolver>,
    styles: Vec<(Range<usize>, HighlightStyle)>,
    group_ranges: Vec<Range<usize>>,
}

/// Layout data for a single caret, produced by [`TextElement::layout_cursors`].
#[derive(Clone, Debug, PartialEq)]
pub(super) struct CursorRenderInfo {
    pub(super) bounds: Bounds<Pixels>,
    pub(super) is_active: bool,
    /// Nothing is selected at this caret: a caret hides over a selection.
    pub(super) collapsed: bool,
    /// The text offset the caret sits at, so a layout move isn't taken for a caret move.
    pub(super) offset: usize,
    /// How many selections the editor has, in view or not.
    pub(super) total: usize,
    /// Width of the character the caret sits on, for a hollow caret; zero otherwise.
    cell_width: Pixels,
}

/// How long the wash over changed rows fades: under 400 ms, on the app's `ease_out`.
const WASH_LENGTH: f32 = 0.3;
/// The wash's ink at full level, as a share of the selection fill's alpha. Mirrors nothing in the
/// app: a vendored soft fill, quieter than the selection so the text stays the loudest thing.
const WASH_INK: f32 = 0.5;

/// What the app hears of a wash as it paints (`set_wash_probe`): the rect of each row run drawn and the
/// alpha it is drawn at. Set only when the app records element rects; unset, the paint is one `get`.
static WASH_PROBE: std::sync::OnceLock<fn(&Window, &[Bounds<Pixels>], f32)> = std::sync::OnceLock::new();

/// Has the app told of every wash painted from now on (the first call wins).
pub fn set_wash_probe(probe: fn(&Window, &[Bounds<Pixels>], f32)) {
    WASH_PROBE.set(probe).ok();
}

/// How long a find halo takes to rise, then to settle: `HOVER_IN` and a little over `HOVER_OUT`,
/// 0.34 s in all, on the app's `ease_out`.
const HALO_RISE: f32 = 0.12;
const HALO_FALL: f32 = 0.22;
/// How far the halo reaches past the match at its peak. Mirrors nothing in the app: a vendored
/// step, about a fifth of a line.
const HALO_GROW: Pixels = px(4.);

/// The ring a Find Next or Previous gives the match it lands on, once: it rises, then settles. The
/// clock starts at the first frame that draws it, and a halo cut off by another jump hands over
/// the level it was drawn at, so the new one rises from there.
pub(super) struct Halo {
    range: Range<usize>,
    revision: u64,
    /// When the jump happened, so a halo that was never drawn (a hidden tab) is dropped, not
    /// played late.
    created: std::time::Instant,
    since: Option<std::time::Instant>,
    /// The level the halo started from (the one the halo it replaced was drawn at).
    carry: f32,
    /// The level drawn last.
    drawn: f32,
}

impl Halo {
    pub(super) fn new(range: Range<usize>, revision: u64, carry: f32) -> Self {
        Self { range, revision, created: std::time::Instant::now(), since: None, carry, drawn: carry }
    }

    /// The level drawn last (0 to 1).
    pub(super) fn level(&self) -> f32 {
        self.drawn
    }
}

impl<M: InputModeKind> InputBaseState<M> {
    /// The match to ring and the level to draw it at (0 to 1) at `now`, or `None` once the halo
    /// is over, the document changed, or there never was one.
    pub(super) fn step_halo(&mut self, now: std::time::Instant) -> Option<(Range<usize>, f32)> {
        // The halo rings what a step landed on: the session's current match while a search is
        // showing, the selection (a step with the bar closed selects its match) otherwise. A
        // changed query, option, scope or a closed search leaves it ringing nothing, so it goes.
        let rings = if self.search_session.is_active() {
            let matcher = &self.search_session.matcher;
            matcher.matched_ranges().get(matcher.current_match_index()).cloned()
        } else {
            Some(self.selected_range())
        };
        let halo = self.halo.as_mut()?;
        if rings.as_ref() != Some(&halo.range) {
            self.halo = None;
            return None;
        }
        let total = HALO_RISE + HALO_FALL;
        if halo.since.is_none() && now.saturating_duration_since(halo.created).as_secs_f32() >= total {
            self.halo = None;
            return None;
        }
        let since = *halo.since.get_or_insert(now);
        let t = now.saturating_duration_since(since).as_secs_f32();
        if halo.revision != self.document_revision || t >= total {
            self.halo = None;
            return None;
        }
        let level = if t < HALO_RISE {
            halo.carry + (1. - halo.carry) * super::blink_cursor::ease_out(t / HALO_RISE)
        } else {
            1. - super::blink_cursor::ease_out((t - HALO_RISE) / HALO_FALL)
        };
        halo.drawn = level;
        Some((halo.range.clone(), level))
    }
}

/// The extents a paste or a history replay wrote, as they stand once it ends.
#[derive(Default)]
pub(super) struct WashCapture {
    pub(super) ranges: Vec<Range<usize>>,
    pub(super) inserted_lines: bool,
    pub(super) removed_lines: bool,
    capped: bool,
}

impl WashCapture {
    /// Keep wash bookkeeping bounded independently of the document's edit count.
    pub(super) const MAX_EDITS: usize = 256;

    /// Notes one edit: `range` of the old `text` became `new_text`. Earlier extents behind it shift.
    pub(super) fn record(&mut self, text: &Rope, range: &Range<usize>, new_text: &str) {
        if self.capped {
            return;
        }
        if self.ranges.len() == Self::MAX_EDITS {
            self.ranges.clear();
            self.capped = true;
            return;
        }
        let range = text.clip_offset(range.start, Bias::Left)..text.clip_offset(range.end, Bias::Right);
        self.inserted_lines |= new_text.contains('\n');
        self.removed_lines |= text.offset_to_point(range.start).row != text.offset_to_point(range.end).row;
        let delta = new_text.len() as isize - range.len() as isize;
        for earlier in &mut self.ranges {
            if earlier.start >= range.end {
                earlier.start = earlier.start.saturating_add_signed(delta);
                earlier.end = earlier.end.saturating_add_signed(delta);
            } else if earlier.end >= range.end {
                earlier.end = earlier.end.saturating_add_signed(delta);
            }
        }
        self.ranges.push(range.start..range.start + new_text.len());
    }
}

/// The rows a paste or undo changed, washing out once. Ends on the next edit (`revision`) or when
/// its fade is done; the clock starts at the first frame that draws it.
pub(super) struct Wash {
    ranges: Vec<Range<usize>>,
    revision: u64,
    /// When the edit happened, so a wash that was never drawn (a hidden tab) is dropped, not
    /// played late.
    created: std::time::Instant,
    since: Option<std::time::Instant>,
    /// How long this wash fades, in seconds (`WASH_LENGTH` unless a caller asked for another).
    length: f32,
    /// A wash that isn't tied to an edit (a welcome back) ends when the view scrolls.
    ends_on_scroll: bool,
}

impl Wash {
    pub(super) fn new(ranges: Vec<Range<usize>>, revision: u64) -> Self {
        Self::with_length(ranges, revision, WASH_LENGTH)
    }

    pub(super) fn with_length(ranges: Vec<Range<usize>>, revision: u64, length: f32) -> Self {
        Self { ranges, revision, created: std::time::Instant::now(), since: None, length, ends_on_scroll: false }
    }

    pub(super) fn ending_on_scroll(mut self) -> Self {
        self.ends_on_scroll = true;
        self
    }

    pub(super) fn ends_on_scroll(&self) -> bool {
        self.ends_on_scroll
    }

    /// Whether this wash is of `revision` and one of its rows holds `offset`.
    pub(super) fn covers(&self, revision: u64, offset: usize) -> bool {
        self.revision == revision && self.ranges.iter().any(|r| r.start <= offset && offset <= r.end)
    }

    /// A command that edits again while this wash is under half way (a held Move) keeps it
    /// running over its new rows instead of restarting it: returns whether it did.
    pub(super) fn follow(&mut self, ranges: &mut Vec<Range<usize>>, revision: u64, now: std::time::Instant) -> bool {
        let begun = self.since.unwrap_or(self.created);
        if self.ends_on_scroll || now.saturating_duration_since(begun).as_secs_f32() >= self.length / 2. {
            return false;
        }
        self.ranges = std::mem::take(ranges);
        self.revision = revision;
        true
    }
}

impl<M: InputModeKind> InputBaseState<M> {
    /// The rows to wash and the level to draw them at (1 down to 0) at `now`, or `None` once the
    /// wash is over, the document changed, or there never was one.
    pub(super) fn step_wash(&mut self, now: std::time::Instant) -> Option<(Vec<Range<usize>>, f32)> {
        let wash = self.wash.as_mut()?;
        if wash.since.is_none() && now.saturating_duration_since(wash.created).as_secs_f32() >= wash.length {
            self.wash = None;
            return None;
        }
        let since = *wash.since.get_or_insert(now);
        let u = now.saturating_duration_since(since).as_secs_f32() / wash.length;
        if wash.revision != self.document_revision || u >= 1. {
            self.wash = None;
            return None;
        }
        let level = 1. - super::blink_cursor::ease_out(u);
        Some((wash.ranges.clone(), level))
    }
}

/// How long a Copy's lift of the selection fill fades: about a quarter second, on `ease_out`.
const LIFT_LENGTH: f32 = 0.25;
/// How much alpha a Copy adds to the selection fill at the peak of its lift, before it eases back
/// to the fill at rest. Mirrors nothing in the app: a vendored step, enough to read as a flash.
const LIFT_STEP: f32 = 0.22;

/// A Copy's lift of the selection fill, once. Ends on the next edit (`revision`), when the
/// selection moves, or when its fade is done; the clock starts at the first frame that draws it.
pub(super) struct Lift {
    revision: u64,
    selections: Vec<(usize, usize)>,
    /// When the copy happened, so a lift that was never drawn (a hidden tab) is dropped, not
    /// played late.
    created: std::time::Instant,
    since: Option<std::time::Instant>,
}

impl Lift {
    pub(super) fn new(selections: Vec<(usize, usize)>, revision: u64) -> Self {
        Self { revision, selections, created: std::time::Instant::now(), since: None }
    }
}

impl<M: InputModeKind> InputBaseState<M> {
    /// The level to lift the selection fill at (1 down to 0) at `now`, or `None` once the lift is
    /// over, the document or the selection changed, or there never was one.
    pub(super) fn step_lift(&mut self, now: std::time::Instant) -> Option<f32> {
        let lift = self.lift.as_mut()?;
        let same = lift.revision == self.document_revision
            && lift.selections.len() == self.selections.len()
            && self.selections.iter().zip(&lift.selections).all(|(sel, &(start, end))| sel.start == start && sel.end == end);
        if !same || lift.since.is_none() && now.saturating_duration_since(lift.created).as_secs_f32() >= LIFT_LENGTH {
            self.lift = None;
            return None;
        }
        let since = *lift.since.get_or_insert(now);
        let u = now.saturating_duration_since(since).as_secs_f32() / LIFT_LENGTH;
        if u >= 1. {
            self.lift = None;
            return None;
        }
        Some(1. - super::blink_cursor::ease_out(u))
    }
}

/// How long the selection, the current-line band and the find fills take to take the key look:
/// the app's `HOVER_IN` (0.12 s), the fades every wash eases on.
const KEY_IN: f32 = 0.12;
/// How long they take to take the not-key look: the app's `HOVER_OUT` (0.2 s).
const KEY_OUT: f32 = 0.2;

/// How key a window draws, 1 key to 0 not key, easing on a flip and turning around from the level
/// drawn if it flips again. One per window, shared by every input in it, stepped once per frame
/// time. Asks for no frame at rest.
#[derive(Clone, Copy)]
struct KeyEase {
    from: f32,
    to: f32,
    since: Option<std::time::Instant>,
}

impl KeyEase {
    fn at(key: bool) -> Self {
        let level = if key { 1. } else { 0. };
        Self { from: level, to: level, since: None }
    }

    fn level(&self, now: std::time::Instant) -> f32 {
        let Some(since) = self.since else { return self.to };
        let span = if self.to > self.from { KEY_IN } else { KEY_OUT };
        let u = now.saturating_duration_since(since).as_secs_f32() / span;
        if u >= 1. {
            return self.to;
        }
        self.from + (self.to - self.from) * super::blink_cursor::ease_out(u)
    }

    /// The level drawn at `now` for a window that is `key` at `now`, and whether it is still
    /// easing, so the next frame is wanted.
    fn step(&mut self, key: bool, now: std::time::Instant) -> (f32, bool) {
        let to = if key { 1. } else { 0. };
        if to != self.to {
            self.from = self.level(now);
            self.to = to;
            self.since = Some(now);
        }
        let level = self.level(now);
        if self.since.is_some() && level == self.to {
            self.since = None;
            self.from = self.to;
        }
        (level, self.since.is_some())
    }
}

/// How key `window` draws at `now` (1 key, 0 not key) and whether that is still easing. A window
/// seen for the first time starts at rest on its look, so a launch doesn't fade. The ease lives
/// in the window's slot in `motion`, dropped when the window closes.
fn key_level(window: &Window, now: std::time::Instant) -> (f32, bool) {
    let key = crate::input::window_key(window);
    crate::motion::with_window_slot(
        window.window_handle().window_id(),
        || KeyEase::at(key),
        |ease| ease.step(key, now),
    )
}

/// How long a fold takes to glide shut or open, on the fade curve.
const FOLD_GLIDE: f32 = 0.2;

/// A fold that has just opened or shut, gliding: the lines it hides (`rows`), where it began and
/// how much of its block the curtain covered then (`from`, 0 to 1). Opening and shutting both
/// draw the curtain away, so a fold toggled again mid-glide starts from the cover drawn: the
/// curtain never jumps, and the block's drawn height turns around from where it was.
#[derive(Clone, Copy)]
struct FoldGlide {
    line: usize,
    /// The fold's closing line, which stays shown.
    end_line: usize,
    rows: usize,
    opening: bool,
    from: f32,
    since: std::time::Instant,
}

impl FoldGlide {
    /// How much of the block the curtain covers at `now` (1 all, 0 none, eased) and whether the
    /// glide is done.
    fn covered(&self, now: std::time::Instant) -> (f32, bool) {
        let u = now.saturating_duration_since(self.since).as_secs_f32() / FOLD_GLIDE;
        (self.from * (1. - super::blink_cursor::ease_out(u.min(1.))), u >= 1.)
    }
}

/// What the fold gutter animates, kept per editor and stepped on the frame's time: the arrows'
/// fade with the pointer, and the glide of the one fold that last opened or shut. Both end, so a
/// still editor asks for no frame.
pub(super) struct FoldMotion {
    arrows: KeyEase,
    /// The folds drawn last frame, the generation they had and the document they were in, to
    /// spot one opening or shutting.
    seen: Vec<FoldRange>,
    seen_generation: u64,
    seen_revision: u64,
    watching: bool,
    glide: Option<FoldGlide>,
}

impl Default for FoldMotion {
    fn default() -> Self {
        Self {
            arrows: KeyEase::at(false),
            seen: Vec::new(),
            seen_generation: 0,
            seen_revision: 0,
            watching: false,
            glide: None,
        }
    }
}

impl FoldMotion {
    /// Notes the folds of this frame. In an unchanged document, exactly one fold more is a shut
    /// and glides; so are one or more folds fewer when they nest in one outer fold (a click, or a
    /// jump into code under nested folds, which opens every fold above its line): the glide is the
    /// outer fold's. An edit shifting folds, fold-all and a first look do not. A fold toggled
    /// again while it glides turns around from the height drawn. The list is read only when its
    /// generation moved.
    fn watch(&mut self, folded: &[FoldRange], generation: u64, revision: u64, now: std::time::Instant) {
        if self.watching && self.seen_generation == generation {
            self.seen_revision = revision;
            return;
        }
        let toggled = if self.watching && self.seen_revision == revision { self.toggled(folded) } else { None };
        self.glide = toggled.and_then(|(range, opening)| {
            let rows = range.end_line.saturating_sub(range.start_line + 1);
            let from = match self.glide {
                Some(prev) if prev.line == range.start_line && prev.opening != opening => prev.covered(now).0,
                _ => 1.,
            };
            (rows > 0).then_some(FoldGlide {
                line: range.start_line,
                end_line: range.end_line,
                rows,
                opening,
                from,
                since: now,
            })
        });
        self.seen.clear();
        self.seen.extend_from_slice(folded);
        self.seen_generation = generation;
        self.seen_revision = revision;
        self.watching = true;
    }

    /// The fold that `folded` shut or opened against the folds seen last frame, and whether it
    /// opened: see `watch`.
    fn toggled(&self, folded: &[FoldRange]) -> Option<(FoldRange, bool)> {
        if folded.len() == self.seen.len() + 1 {
            let at = (0..self.seen.len())
                .find(|&ix| folded[ix] != self.seen[ix])
                .unwrap_or(self.seen.len());
            return Some((folded[at], false));
        }
        // The folds still shut are a subset of the ones seen, in the same order; the rest opened,
        // and must form a chain, each nested in the one opened before it (outermost first).
        let mut outer: Option<FoldRange> = None;
        let mut inner: Option<FoldRange> = None;
        let mut kept = 0;
        for fold in &self.seen {
            if folded.get(kept) == Some(fold) {
                kept += 1;
                continue;
            }
            if inner.is_some_and(|o| fold.start_line < o.start_line || fold.end_line > o.end_line) {
                return None;
            }
            outer.get_or_insert(*fold);
            inner = Some(*fold);
        }
        (kept == folded.len()).then_some(outer).flatten().map(|range| (range, true))
    }

    /// The glide at `now`: its fold and how much of its block the curtain covers (1 all, 0 none,
    /// eased); `None` once it ends.
    fn glide_at(&mut self, now: std::time::Instant) -> Option<(FoldGlide, f32)> {
        let glide = self.glide?;
        let (covered, done) = glide.covered(now);
        if done {
            self.glide = None;
            return None;
        }
        Some((glide, covered))
    }
}

/// A folded line's `...` chip: where it sits (window coordinates) and what a click on it opens.
struct FoldChip {
    buffer_line: usize,
    bounds: Bounds<Pixels>,
    hitbox: Hitbox,
}

const FOLD_CHIP_WIDTH: Pixels = px(24.);
const FOLD_CHIP_GAP: Pixels = px(6.);
const FOLD_CHIP_DOT: Pixels = px(2.5);

/// `from` to `to` by `t` in RGB weighted by alpha, so a faint fill doesn't pull the middle grey.
fn mix_fill(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let (a, b) = (gpui::Rgba::from(from), gpui::Rgba::from(to));
    let alpha = a.a + (b.a - a.a) * t;
    if alpha <= 0. {
        return Hsla { h: 0., s: 0., l: 0., a: 0. };
    }
    let channel = |x: f32, y: f32| (x * a.a + (y * b.a - x * a.a) * t) / alpha;
    gpui::Rgba {
        r: channel(a.r, b.r),
        g: channel(a.g, b.g),
        b: channel(a.b, b.b),
        a: alpha,
    }
    .into()
}

/// A fill that differs between key and not key, at `level`: either look at rest, the mix between.
fn key_fill(level: f32, fill: impl Fn(bool) -> Hsla) -> Hsla {
    if level >= 1. {
        fill(true)
    } else if level <= 0. {
        fill(false)
    } else {
        mix_fill(fill(false), fill(true), level)
    }
}

/// The faint line an empty Untitled page shows ("Start typing…"): set by the app with its words,
/// ink alpha, fade length and easing. It shows while the text is empty and fades out on the first
/// character and back in when the text empties again, turning around from the level drawn.
#[derive(Default)]
pub(super) struct Hint {
    text: SharedString,
    /// The hint is wanted (the page is Untitled); the text is kept when it isn't, so a hint still showing fades out.
    on: bool,
    alpha: f32,
    length: f32,
    ease: Option<fn(f32) -> f32>,
    from: f32,
    to: f32,
    since: Option<std::time::Instant>,
    /// A first frame has chosen the level, with no fade (the page is new).
    seen: bool,
}

impl Hint {
    /// Says what the hint is; true when that changed anything. Empty `text` turns it off.
    pub(super) fn set(&mut self, text: SharedString, alpha: f32, length: f32, ease: fn(f32) -> f32) -> bool {
        let on = !text.is_empty();
        let changed = self.on != on || (on && self.text != text) || self.alpha != alpha || self.length != length;
        self.on = on;
        if on {
            self.text = text;
        }
        self.alpha = alpha;
        self.length = length;
        self.ease = Some(ease);
        changed
    }

    pub(super) fn alpha(&self) -> f32 {
        self.alpha
    }

    pub(super) fn text(&self) -> SharedString {
        self.text.clone()
    }

    fn level(&self, now: std::time::Instant) -> f32 {
        let (Some(since), Some(ease)) = (self.since, self.ease) else { return self.to };
        let u = now.saturating_duration_since(since).as_secs_f32() / self.length;
        if u >= 1. { self.to } else { self.from + (self.to - self.from) * ease(u) }
    }

    /// The level to draw at `now` and whether another frame is wanted, given whether the text is empty.
    pub(super) fn step(&mut self, empty: bool, now: std::time::Instant) -> (f32, bool) {
        if self.text.is_empty() {
            return (0., false);
        }
        let target = if self.on && empty { 1. } else { 0. };
        if !self.seen {
            self.seen = true;
            (self.from, self.to) = (target, target);
        } else if target != self.to {
            (self.from, self.to, self.since) = (self.level(now), target, Some(now));
        }
        let moving = self.since.is_some_and(|since| now.saturating_duration_since(since).as_secs_f32() < self.length);
        (self.level(now), moving)
    }
}

/// How far the syntax colours show after the language changed: the plain text colour at 0, the
/// highlighter's at 1. The app arms it (`EditorState::fade_syntax`) when a save or rename
/// really changes the language; its clock starts on the first frame the highlighter has
/// colours to show (the parse lands after the swap), runs `length` seconds on `ease`, and
/// disarms itself, so an editor at rest does no work and asks for no frames. Cells, because
/// layout reads the state only borrowed.
#[derive(Default)]
pub(super) struct SyntaxMix {
    length: std::cell::Cell<f32>,
    ease: std::cell::Cell<Option<fn(f32) -> f32>>,
    armed: std::cell::Cell<bool>,
    since: std::cell::Cell<Option<std::time::Instant>>,
}

impl SyntaxMix {
    pub(super) fn arm(&self, length: f32, ease: fn(f32) -> f32) {
        self.length.set(length);
        self.ease.set(Some(ease));
        self.since.set(None);
        self.armed.set(true);
    }

    pub(super) fn disarm(&self) {
        self.armed.set(false);
        self.since.set(None);
    }

    /// A frame is wanted: the clock runs.
    pub(super) fn moving(&self) -> bool {
        self.armed.get() && self.since.get().is_some()
    }

    /// The share of the syntax colours to draw at `now`. `colored` says whether the highlighter
    /// has any colours on screen (asked only before the clock starts); `parsed` that its first
    /// parse landed. A parse that colours nothing on screen disarms the fade, so it can't fire
    /// later while typing.
    fn level(&self, colored: impl FnOnce() -> bool, parsed: bool, now: std::time::Instant) -> f32 {
        if !self.armed.get() {
            return 1.;
        }
        let Some(since) = self.since.get() else {
            if colored() {
                self.since.set(Some(now));
            } else if parsed {
                self.disarm();
                return 1.;
            }
            return 0.;
        };
        let (length, ease) = (self.length.get(), self.ease.get());
        let u = now.saturating_duration_since(since).as_secs_f32() / length.max(f32::EPSILON);
        if u >= 1. {
            self.disarm();
            1.
        } else {
            ease.map_or(u, |ease| ease(u))
        }
    }
}

pub(super) struct PrepaintState {
    /// The lines of entire lines.
    last_layout: LastLayout,
    token_elements: Vec<AnyElement>,
    /// A patch's rows, old-side numbers and file icons while its map is the text's own.
    patch: Option<PatchPaint>,
    /// The lines only contains the visible lines in the viewport, based on `visible_range`.
    ///
    /// The child is the soft lines.
    line_numbers: Option<Vec<SmallVec<[ShapedLine; 1]>>>,
    /// Size of the scrollable area by entire lines.
    scroll_size: Size<Pixels>,
    /// Caret bounds for every selection, active flagged.
    cursor_infos: Vec<CursorRenderInfo>,
    cursor_scroll_offset: Point<Pixels>,
    /// row index (zero based), no wrap, same line as the cursor.
    current_row: Option<usize>,
    selection_paths: Vec<Path<Pixels>>,
    hover_highlight_path: Option<Path<Pixels>>,
    bracket_paths: Vec<Path<Pixels>>,
    hidden_paths: Vec<Path<Pixels>>,
    search_match_paths: Vec<(Path<Pixels>, bool)>,
    /// Find in selection: the text on screen the search is limited to.
    search_scope_paths: Option<Rc<Vec<Path<Pixels>>>>,
    document_color_paths: Vec<(Path<Pixels>, Hsla)>,
    range_decoration_fills: Vec<(Path<Pixels>, Hsla)>,
    range_decoration_frames: Vec<(Path<Pixels>, Hsla)>,
    /// The rows a paste or undo just changed, and how much of the wash is left (1 down to 0).
    wash_paths: Vec<Path<Pixels>>,
    wash_level: f32,
    /// The find halo: a quad per row of the match it rings, and its level (0 to 1).
    halo_quads: Vec<Bounds<Pixels>>,
    halo_radius: Pixels,
    halo_level: f32,
    /// How much of a Copy's lift of the selection fill is left (1 down to 0).
    lift_level: f32,
    /// How key the window draws, 1 key to 0 not key, easing on a flip.
    key_level: f32,
    hover_definition_hitbox: Option<Hitbox>,
    /// The gutter's right-click target, when the host set a gutter menu.
    gutter_menu_hitbox: Option<Hitbox>,
    indent_guides: Vec<Bounds<Pixels>>,
    /// The whole input, for deciding whether a long press started in it.
    hitbox: Hitbox,
    bounds: Bounds<Pixels>,
    /// Fold icon layout data
    fold_icon_layout: FoldIconLayout,
    // Inline completion rendering data
    /// Shaped ghost lines to paint after cursor row (completion lines 2+)
    ghost_lines: Vec<ShapedLine>,
    /// First line of inline completion (painted after cursor on same line)
    ghost_first_line: Option<ShapedLine>,
    ghost_lines_height: Pixels,
    /// How far a gutter number sits below the code line's top, so both share a baseline.
    line_number_dy: Pixels,
    /// Where a gutter number's right edge sits, from the gutter's left edge.
    line_number_right: Pixels,
    /// The gutter's true width: the text may still be gliding to its edge.
    gutter_width: Pixels,
    /// The empty page's hint, shaped for this frame while it shows.
    hint: Option<ShapedLine>,
}

impl PrepaintState {
    /// Returns all cursor infos adjusted for scroll offset.
    fn cursor_infos_with_scroll(&self) -> Vec<CursorRenderInfo> {
        self.cursor_infos
            .iter()
            .map(|info| {
                let mut bounds = info.bounds;
                bounds.origin.y += self.cursor_scroll_offset.y;
                CursorRenderInfo {
                    bounds,
                    is_active: info.is_active,
                    collapsed: info.collapsed,
                    offset: info.offset,
                    total: info.total,
                    cell_width: info.cell_width,
                }
            })
            .collect()
    }
}

/// The current-line band's color: the theme's, half as strong in a disabled input, and 0.7 of
/// itself while the window isn't key, so it dims with the selection. `key_level` is 1 in a key
/// window and 0 in any other, and between while the window eases from one to the other.
fn band_color(color: Option<Hsla>, disabled: bool, key_level: f32) -> Option<Hsla> {
    let key_band = 0.7 + 0.3 * key_level;
    color
        .map(|color| if disabled { color.opacity(0.5) } else { color })
        .map(|color| color.opacity(key_band))
}

/// Paints the current-line band with its top at `top` (window coordinates): the layer moves it
/// with the caret (gpui-editor#g7fw), across the gutter to the pane's right edge.
pub(super) fn paint_band(band: &Band, top: Pixels, strength: f32, window: &mut Window) {
    window.paint_quad(fill(
        Bounds::new(
            point(band.left, top),
            size(band.width, band.height),
        ),
        band.color.opacity(strength),
    ));
}

/// Align a caret's left edge to a whole device pixel.
///
/// Text positions are fractional, and a caret at a fractional x is spread over
/// an extra device pixel by antialiasing, which draws it faint. Rounding its x
/// to the device grid keeps a resting caret crisp; a gliding one is not snapped.
fn snap_caret_to_device_pixels(bounds: Bounds<Pixels>, scale_factor: f32) -> Bounds<Pixels> {
    let x = (bounds.origin.x * scale_factor).round() / scale_factor;
    Bounds::new(point(x, bounds.origin.y), bounds.size)
}

/// Paints each caret in `infos` as a still, faint outline around the character
/// it sits on (the head of the selection, if there is one): the caret of an
/// editor that isn't focused. No blink, no glide, so it never asks for another
/// frame.
pub(super) fn paint_hollow_carets(
    infos: &[CursorRenderInfo],
    scroll: Point<Pixels>,
    color: Hsla,
    // How much of the box shows: 1 at rest, less while it crossfades with the bar.
    alpha: f32,
    window: &mut Window,
) {
    let scale = window.scale_factor();
    for info in infos.iter().filter(|info| info.cell_width > px(0.)) {
        let origin = point(info.bounds.origin.x, info.bounds.origin.y + scroll.y);
        let mut cell = snap_caret_to_device_pixels(
            Bounds::new(origin, size(info.cell_width, info.bounds.size.height)),
            scale,
        );
        // The right edge lands on a device pixel too, so its border is crisp.
        let right = ((origin.x + info.cell_width) * scale).round() / scale;
        cell.size.width = right - cell.origin.x;
        window.paint_quad(
            outline(cell, color.opacity(HOLLOW_ALPHA * alpha), BorderStyle::Solid)
                .corner_radii(Corners::all(HOLLOW_RADIUS)),
        );
    }
}

/// Paints every caret in `infos` as of `now` (the active one glides or springs through
/// `glide`; `typing` is an edit moving it, which springs), at the blink's `(opacity, still
/// fading)`. A caret hides over a selection. Returns whether another frame is wanted: a glide
/// runs, or a shown caret is still fading. The bar is on the device grid in every frame, so the
/// frame it lands in doesn't hop.
pub(super) fn paint_carets(
    infos: &[CursorRenderInfo],
    scroll: Point<Pixels>,
    color: Hsla,
    (blink, fading): (f32, bool),
    glide: &mut CaretGlide,
    now: Instant,
    typing: bool,
    window: &mut Window,
) -> bool {
    let scale = window.scale_factor();
    let mut moving = false;
    let mut drawn = false;
    // The count of every selection, not of those in view, so a scroll never reads as new carets.
    glide.births.begin(infos.first().map_or(0, |info| info.total));
    for info in infos {
        // The glide runs in content space, so scrolling never glides it.
        let mut origin = point(info.bounds.origin.x - scroll.x, info.bounds.origin.y);
        let gliding;
        let mut weight = 1.;
        if info.is_active {
            (origin, gliding) = glide.step(origin, info.offset, scroll, now, typing, scale);
        } else if info.collapsed {
            // A caret added by a click or a drag fades in as the blink does.
            (weight, gliding) = glide.births.weight(info.offset, now);
        } else {
            gliding = false;
        }
        if !info.collapsed || blink <= 0. {
            continue;
        }
        moving |= gliding;
        let caret = snap_caret_to_device_pixels(
            Bounds::new(
                point(origin.x + scroll.x, origin.y + scroll.y),
                info.bounds.size,
            ),
            scale,
        );
        drawn = true;
        window.paint_quad(
            fill(caret, color.opacity(blink * weight)).corner_radii(Corners::all(CURSOR_WIDTH / 2.)),
        );
    }
    glide.births.end();
    // No fade frames while every caret hides over a selection.
    moving || (fading && drawn)
}

impl<M: InputModeKind> IntoElement for TextElement<M> {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// A debug function to print points as SVG path.
#[allow(unused)]
fn print_points_as_svg_path(
    line_corners: &Vec<gpui::Corners<Pixels>>,
    points: &Vec<Point<Pixels>>,
) {
    for corners in line_corners {
        println!(
            "tl: ({}, {}), tr: ({}, {}), bl: ({}, {}), br: ({}, {})",
            corners.top_left.as_f32() as i32,
            corners.top_left.as_f32() as i32,
            corners.top_right.as_f32() as i32,
            corners.top_right.as_f32() as i32,
            corners.bottom_left.as_f32() as i32,
            corners.bottom_left.as_f32() as i32,
            corners.bottom_right.as_f32() as i32,
            corners.bottom_right.as_f32() as i32,
        );
    }

    if points.len() > 0 {
        println!(
            "M{},{}",
            points[0].x.as_f32() as i32,
            points[0].y.as_f32() as i32
        );
        for p in points.iter().skip(1) {
            println!("L{},{}", p.x.as_f32() as i32, p.y.as_f32() as i32);
        }
    }
}

fn frame_outline_points(corners: &[Corners<Point<Pixels>>]) -> Vec<Point<Pixels>> {
    let rects = corners
        .iter()
        .map(|corners| (corners.top_left, corners.bottom_right))
        .collect::<Vec<_>>();
    let mut points = Vec::with_capacity(rects.len() * 4);
    let first = rects[0];
    let last = rects[rects.len() - 1];
    points.push(first.0);
    points.push(point(first.1.x, first.0.y));
    for pair in rects.windows(2) {
        let current = pair[0];
        let next = pair[1];
        points.push(current.1);
        points.push(point(next.1.x, current.1.y));
        points.push(point(next.1.x, next.1.y));
    }
    if points.last() != Some(&last.1) {
        points.push(last.1);
    }
    points.push(point(last.0.x, last.1.y));
    for pair in rects.windows(2).rev() {
        let current = pair[1];
        let next = pair[0];
        points.push(point(current.0.x, current.0.y));
        points.push(point(next.0.x, current.0.y));
        points.push(point(next.0.x, next.0.y));
    }
    if points.last() != points.first() {
        points.push(points[0]);
    }
    points
}

/// Add `vertices` (a closed rectilinear outline) to `builder` as one sub-path
/// with every corner a quadratic curve. A curve never takes more than half of
/// either segment it joins, so short steps shrink their corners instead of
/// overlapping them.
fn push_rounded_outline(
    builder: &mut gpui::PathBuilder,
    mut vertices: Vec<Point<Pixels>>,
    origin: Point<Pixels>,
    radius: Pixels,
) {
    // Drop repeated points and the closing duplicate, then the collinear
    // points, so every vertex left is a real corner.
    vertices.dedup();
    if vertices.len() > 1 && vertices.first() == vertices.last() {
        vertices.pop();
    }
    let count = vertices.len();
    let first = vertices.first().copied();
    let mut previous = vertices.last().copied();
    let mut kept = 0;
    for ix in 0..count {
        let (a, b) = (previous.unwrap(), vertices[ix]);
        let c = if ix + 1 < count {
            vertices[ix + 1]
        } else {
            first.unwrap()
        };
        previous = Some(b);
        let cross = (b.x.as_f32() - a.x.as_f32()) * (c.y.as_f32() - b.y.as_f32())
            - (b.y.as_f32() - a.y.as_f32()) * (c.x.as_f32() - b.x.as_f32());
        if cross != 0. {
            vertices[kept] = b;
            kept += 1;
        }
    }
    vertices.truncate(kept);
    let n = vertices.len();
    if n < 3 {
        return;
    }

    let radius = radius.as_f32().max(0.);
    // The point `distance` from `from` toward `to`, in the outline's space.
    let toward = |from: Point<Pixels>, to: Point<Pixels>, distance: f32| {
        let (dx, dy) = (to.x.as_f32() - from.x.as_f32(), to.y.as_f32() - from.y.as_f32());
        let length = dx.hypot(dy);
        origin
            + point(
                px(from.x.as_f32() + dx / length * distance),
                px(from.y.as_f32() + dy / length * distance),
            )
    };
    let half = |a: Point<Pixels>, b: Point<Pixels>| {
        (b.x.as_f32() - a.x.as_f32()).hypot(b.y.as_f32() - a.y.as_f32()) / 2.
    };
    for ix in 0..n {
        let prev = vertices[(ix + n - 1) % n];
        let corner = vertices[ix];
        let next = vertices[(ix + 1) % n];
        let r = radius.min(half(prev, corner)).min(half(corner, next));
        let enter = toward(corner, prev, r);
        if ix == 0 {
            builder.move_to(enter);
        } else {
            builder.line_to(enter);
        }
        builder.curve_to(toward(corner, next, r), origin + corner);
    }
    builder.close();
}

fn pad_frame_corners(
    corners: &[Corners<Point<Pixels>>],
    horizontal_padding: Pixels,
) -> Vec<Corners<Point<Pixels>>> {
    corners
        .iter()
        .map(|corners| Corners {
            top_left: point(corners.top_left.x - horizontal_padding, corners.top_left.y),
            top_right: point(
                corners.top_right.x + horizontal_padding,
                corners.top_right.y,
            ),
            bottom_left: point(
                corners.bottom_left.x - horizontal_padding,
                corners.bottom_left.y,
            ),
            bottom_right: point(
                corners.bottom_right.x + horizontal_padding,
                corners.bottom_right.y,
            ),
        })
        .collect()
}

fn snap_frame_outline(
    points: &[Point<Pixels>],
    stroke_width: Pixels,
    scale_factor: f32,
) -> (Pixels, Vec<Point<Pixels>>) {
    let physical_width = ((stroke_width.as_f32() * scale_factor).abs() - 0.5)
        .ceil()
        .max(1.);
    let stroke_width = px(physical_width / scale_factor);
    let center_offset = if physical_width % 2. == 0. { 0. } else { 0.5 };
    let snap = |value: Pixels| {
        px(
            ((value.as_f32() * scale_factor - center_offset).round() + center_offset)
                / scale_factor,
        )
    };
    (
        stroke_width,
        points
            .iter()
            .map(|point| point.map(snap))
            .collect::<Vec<_>>(),
    )
}

fn clamp_frame_to_content_mask(
    points: Vec<Point<Pixels>>,
    stroke_width: Pixels,
    content_mask: Bounds<Pixels>,
) -> Vec<Point<Pixels>> {
    let half_width = stroke_width / 2.;
    let min_x = content_mask.left() + half_width;
    let max_x = (content_mask.right() - half_width).max(min_x);
    points
        .into_iter()
        .map(|p| point(p.x.max(min_x).min(max_x), p.y))
        .collect()
}

impl<M: InputModeKind> Element for TextElement<M> {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let state = self.state.read(cx);
        let line_height = window.line_height();

        let mut style = Style::default();
        style.size.width = relative(1.).into();
        if state.is_multi_line() {
            style.flex_grow = 1.0;
            style.size.height = relative(1.).into();
            // At least `rows` tall (auto grow: the content's rows, capped at
            // `max_rows`); a taller parent still fills it.
            let rows = state.mode.max_rows().min(state.mode.rows());
            style.min_size.height = (rows * line_height).into();
        } else {
            // For single-line inputs, the minimum height should be the line height
            style.size.height = line_height.into();
        };

        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let style = window.text_style();
        let font = style.font();
        let text_size = style.font_size.to_pixels(window.rem_size());
        // Past the end of a line there are no glyphs to hit-test against, so a pointer
        // out there is measured in spaces instead.
        let space_width = {
            let font_id = window.text_system().resolve_font(&font);
            window.text_system().layout_width(font_id, text_size, ' ')
        };
        // One character of the reading column's measure.
        let reading_advance = {
            let font_id = window.text_system().resolve_font(&font);
            window.text_system().layout_width(font_id, text_size, '0')
        };

        // Text stretched past an end is drawn that far from where it scrolled
        // to, inside the frame that clips it, and springs back over the next
        // frames. Scrolling and everything measured from it stay exact.
        let mut stretch = self.state.update(cx, |state, cx| {
            // A new font size changes the line height and the wrap: the line at the top stays there.
            let old_line_height = state.last_layout.as_ref().map(|layout| layout.line_height);
            let line_height = window.line_height();
            if state.drawn_font.as_ref().is_none_or(|(drawn, size)| *drawn != font || *size != text_size) {
                state.drawn_font = Some((font.clone(), text_size));
            }
            match old_line_height.filter(|old| *old != line_height) {
                Some(old) => hold_top_line_resized(state, old, line_height, |state| state.display_map.set_font(font, text_size, cx)),
                None => state.display_map.set_font(font, text_size, cx),
            }
            let tab_size = state.mode.tab_size().tab_size;
            state.display_map.set_tab_size(tab_size, cx);
            state.display_map.ensure_text_prepared(&state.text, cx);
            // The wheel's glide moves the text before anything is laid out from its offset.
            if state.step_wheel(window, cx) {
                window.request_animation_frame();
            }
            // A far jump (search, undo) and typing past the bottom margin glide: the offset it
            // shows this frame wins below.
            state.settle_pointer();
            state.begin_undo_glide(cx);
            state.begin_typing_glide(cx);
            if state.step_reveal(window, cx) {
                window.request_animation_frame();
            }
            if cx.reduce_motion() {
                state.overscroll.reset();
            } else if state.overscroll.step(crate::motion::frame_time(window.window_handle().window_id())) {
                window.request_animation_frame();
            }
            state.overscroll.offset()
        });

        // The empty page's hint fades with the text going empty or not, on the frame's time.
        let hint_now = crate::motion::frame_time(window.window_handle().window_id());
        // Rows a paste or undo just changed wash out once, on the same frame time.
        let (hint_level, hint_alpha, hint_text, hint_moving, wash, halo) = self.state.update(cx, |state, _| {
            let (level, moving) = state.hint.step(state.text.len() == 0, hint_now);
            (level, state.hint.alpha(), state.hint.text(), moving, state.step_wash(hint_now), state.step_halo(hint_now))
        });
        if hint_moving || wash.is_some() || halo.is_some() {
            window.request_animation_frame();
        }
        let (wash_ranges, wash_level) = wash.unwrap_or_default();
        let lift_level = self.state.update(cx, |state, _| state.step_lift(hint_now));
        if lift_level.is_some() {
            window.request_animation_frame();
        }
        let lift_level = lift_level.unwrap_or_default();
        // The selection, band and find fills ease between their key and not-key looks.
        let (key_level, key_moving) = key_level(window, hint_now);
        if key_moving {
            window.request_animation_frame();
        }

        // A digit gained or lost (999 to 1,000) moves the gutter's edge. The text is wrapped and
        // measured at the new width in this frame, never per glide frame; the position it is
        // drawn at (and everything measured from it: selections, carets, guides, hits) springs
        // from where it was, on the caret's critically damped spring, and turns around from the
        // drawn spot when the width changes again. The gutter paints over the text it overlaps.
        let gutter_width = {
            let state = self.state.read(cx);
            Self::layout_line_numbers(state, line_number_len(state.text.lines_len()), window)
        };
        let spring_id = {
            let state = self.state.read(cx);
            state.shows_line_numbers().then(|| state.gutter_spring_id.clone())
        };
        let drawn_line_number_width = if let Some(id) = spring_id {
            crate::motion::spring_by_id(&id, gutter_width, GUTTER_GLIDE, window, cx)
        } else {
            gutter_width
        };
        // The reading column: the one wrap width every site shares, and where the column starts
        // from the gutter's edge. It glides when the target moves for any reason but the window's
        // width (the setting, the font, a gutter digit); a window that changed width since the
        // last draw (an edge dragged) centers at once, so the column keeps the middle of the room
        // on every frame instead of trailing the edge; a panel or split sliding the pane inside
        // an unchanged window still glides.
        let (wrap_width, column_target, column_id, pane_resized) = {
            let state = self.state.read(cx);
            let (wrap_width, target) = state.text_column(bounds.size.width, gutter_width, reading_advance);
            let resized = state.last_layout.as_ref().is_some_and(|layout| layout.viewport_width != window.viewport_size().width);
            (wrap_width, target, state.column_spring_id.clone(), resized)
        };
        let column_offset = if wrap_width.is_some() {
            let glide = if pane_resized { GUTTER_GLIDE.with_travel(false) } else { GUTTER_GLIDE };
            crate::motion::spring_by_id(&column_id, column_target, glide, window, cx)
        } else {
            px(0.)
        };
        let state = self.state.read(cx);
        let text = state.text.clone();
        let is_empty = text.len() == 0;
        let placeholder = self.placeholder.clone();

        let text_style = window.text_style();
        let disabled = state.disabled;
        let dim = |color: Hsla| if disabled { color.opacity(0.5) } else { color };
        let fg = dim(text_style.color);
        let (display_text, text_color) = if is_empty {
            (
                &Rope::from(placeholder.as_str()),
                dim(state.editor_style.muted_foreground),
            )
        } else if state.masked {
            (
                &Rope::from(MASK_CHAR.to_string().repeat(text.chars().count())),
                fg,
            )
        } else {
            (&text, fg)
        };

        // The gutter lays out at its new width at once; only where the text is drawn glides.
        let line_number_width = gutter_width;

        let mut bounds = bounds;

        let wrapping_indent = state.wrapping_indent;
        let wrap_width_changed = state
            .last_layout
            .as_ref()
            .map(|l| l.wrap_width != wrap_width)
            .unwrap_or(true);

        let wrapping_indent_changed = state
            .last_layout
            .as_ref()
            .map(|l| l.wrapping_indent != wrapping_indent)
            .unwrap_or(true);

        if wrap_width_changed || wrapping_indent_changed {
            // Rows that wrap at once (few were stale) move the content like a pass slice does.
            let line_height = window.line_height();
            self.state.update(cx, |state, cx| {
                hold_top_line(state, line_height, |state| {
                    state.display_map.on_layout_changed(wrap_width, cx);
                    state.display_map.set_wrapping_indent(wrapping_indent, cx);
                });
            });
        }

        // A pass wrapping a big text runs a few milliseconds a frame, the viewport first. Rows
        // wrapped above the viewport move the content, so the top line is held in place.
        if self.state.read(cx).display_map.wrap_pending() {
            let line_height = window.line_height();
            self.state.update(cx, |state, cx| {
                hold_top_line(state, line_height, |state| {
                    state.display_map.step_wrap(WRAP_SLICE, cx);
                });
            });
            if self.state.read(cx).display_map.wrap_pending() {
                window.request_animation_frame();
            }
        }

        let line_height = window.line_height();
        let token_elements = self.measure_tokens(
            wrap_width.unwrap_or(bounds.size.width - line_number_width - RIGHT_MARGIN).max(px(1.)),
            line_height,
            bounds.size.height,
            window,
            cx,
        );
        let state = self.state.read(cx);

        let (visible_range, visible_buffer_lines, visible_top) =
            self.calculate_visible_range(&state, line_height, bounds.size.height);
        let visible_start_offset = state.text.line_start_offset(visible_range.start);
        let visible_end_offset = state
            .text
            .line_end_offset(visible_range.end.saturating_sub(1));

        let highlight_styles = self.highlight_lines(
            &visible_buffer_lines,
            visible_top,
            visible_start_offset..visible_end_offset,
            fg,
            hint_now,
            cx,
        );
        if self.state.read(cx).syntax_mix.moving() {
            window.request_animation_frame();
        }

        let state = self.state.read(cx);

        let visible_line_byte_offsets: Vec<usize> = visible_buffer_lines
            .iter()
            .map(|&bl| state.text.line_start_offset(bl))
            .collect();

        // For password input (masked: true), convert byte offsets to masked display byte offsets so that
        // layout_match_range and position_for_index work in the correct coordinate space.
        let (visible_line_byte_offsets, visible_range_offset) = if state.masked {
            let offsets = visible_line_byte_offsets
                .iter()
                .map(|&o| masked_display_offset(&text, o))
                .collect();
            let range_offset = masked_display_offset(&text, visible_start_offset)
                ..masked_display_offset(&text, visible_end_offset);
            (offsets, range_offset)
        } else {
            (
                visible_line_byte_offsets,
                visible_start_offset..visible_end_offset,
            )
        };

        let mut last_layout = LastLayout {
            visible_range,
            visible_buffer_lines,
            visible_line_byte_offsets,
            visible_top,
            visible_range_offset,
            line_height,
            wrap_width,
            wrapping_indent,
            line_number_width: drawn_line_number_width + column_offset,
            column_offset,
            space_width,
            lines: Rc::new(vec![]),
            cursor_bounds: None,
            text_align: state.text_align,
            content_width: bounds.size.width,
            viewport_width: window.viewport_size().width,
        };

        let run = TextRun {
            len: display_text.len(),
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let marked_run = TextRun {
            len: 0,
            font: style.font(),
            color: text_color,
            background_color: None,
            underline: Some(UnderlineStyle {
                thickness: px(1.),
                color: Some(text_color),
                wavy: false,
            }),
            strikethrough: None,
        };

        let ime_marked_range = ime_marked_display_range(
            &text,
            state.ime_marked_range.as_ref().map(|m| m.start..m.end),
            state.masked,
        );
        let ime_clause_range = ime_marked_display_range(
            &text,
            state.ime_marked_range.as_ref().and_then(|marked| {
                let selection = state.selected_range();
                let start = selection.start.max(marked.start);
                let end = selection.end.min(marked.end);
                (start < end).then_some(start..end)
            }),
            state.masked,
        );

        let mut faded: Vec<(Range<usize>, f32)> = Vec::new();
        let runs = if let (false, Some(highlight_styles)) = (is_empty, highlight_styles) {
            let mut runs = Vec::with_capacity(highlight_styles.len() + 2);

            for (range, style) in &highlight_styles {
                let mut run = text_style.clone().highlight(*style).to_run(range.len());
                if disabled {
                    run.color = run.color.opacity(0.5);
                }
                // A focus dim lowered this run's alpha: keep what it was, so text over a
                // selection or find fill can take it back (and only that much).
                if style.fade_out.is_some() {
                    let mut unfaded = text_style
                        .clone()
                        .highlight(HighlightStyle { fade_out: None, ..*style })
                        .color
                        .a;
                    if disabled {
                        unfaded *= 0.5;
                    }
                    faded.push((range.clone(), unfaded));
                }

                runs.extend(split_run_for_ime_underline(
                    run,
                    range.clone(),
                    ime_marked_range.clone(),
                    ime_clause_range.clone(),
                    marked_run.underline,
                ));
            }
            runs
        } else {
            split_run_for_ime_underline(
                run,
                0..display_text.len(),
                ime_marked_range,
                ime_clause_range,
                marked_run.underline,
            )
            .into_vec()
        };

        let document_colors = state
            .extras
            .document_color_swatches(&text, &last_layout.visible_range);

        let bg_segments = self.layout_background_segments(
            document_colors.clone(),
            &last_layout,
            window,
            cx,
        );

        // Create shaped lines for whitespace indicators before layout
        let whitespace_indicators =
            Self::layout_whitespace_indicators(&state, text_size, &text_style, window, cx);

        let mut hidden_ranges = Vec::new();
        let lines = Self::layout_lines(
            &state,
            &display_text,
            &last_layout,
            text_size,
            &runs,
            &bg_segments,
            &faded,
            whitespace_indicators,
            &mut hidden_ranges,
            window,
        );

        let mut longest_line_width = wrap_width.unwrap_or(px(0.));
        // 1. Single line
        // 2. Multi-line with soft wrap disabled.
        if state.is_single_line() || !state.soft_wrap {
            // The longest line by byte length is only a candidate: a shorter
            // line can render wider (wide glyphs, proportional fonts). The
            // visible lines have already been shaped, so keep their widest
            // measured width as a lower bound. Caching it with the candidate
            // keeps the scroll extent stable while scrolling the line away.
            // Empty inputs lay out the placeholder, which is not scrollable content.
            let widest_laid_out_line = if is_empty {
                px(0.)
            } else {
                lines
                    .iter()
                    .map(|line| line.longest_width)
                    .max()
                    .unwrap_or(px(0.))
            };

            let longest_row = state.display_map.longest_row();
            let longest_line = state.text.slice_line(longest_row);
            // The line is hashed only when an edit or a new longest row could
            // have changed it; a frame at rest compares integers.
            let revision = state.document_revision;
            let cached = state.longest_line_width.take();
            let content_hash = match &cached {
                Some((cached_key, at, _)) if *at == revision && cached_key.row == longest_row => {
                    cached_key.content_hash
                }
                _ => line_content_hash(&longest_line),
            };
            let key = LongestLineKey {
                content_hash,
                row: longest_row,
                len: longest_line.len(),
                font: style.font(),
                text_size,
                wrap_width,
                tab_size: state.mode.tab_size().tab_size,
            };
            let cached = cached.filter(|(cached_key, _, _)| *cached_key == key);
            longest_line_width = match cached {
                Some((_, _, width)) => width.max(widest_laid_out_line),
                None => {
                    #[cfg(test)]
                    LONGEST_LINE_SHAPES.with(|shapes| shapes.set(shapes.get() + 1));
                    let longest_line: SharedString = longest_line.to_string().into();
                    let tab_stop =
                        tab_stop_width(window.text_system(), &key.font, text_size, key.tab_size);
                    let shaped = window.text_system().shape_line(
                        longest_line.clone(),
                        text_size,
                        &[TextRun {
                            len: longest_line.len(),
                            font: key.font.clone(),
                            color: gpui::black(),
                            background_color: None,
                            underline: None,
                            strikethrough: None,
                        }],
                        wrap_width,
                    );
                    expanded_width(&shaped, &longest_line, tab_stop).max(widest_laid_out_line)
                }
            };
            state
                .longest_line_width
                .set(Some((key, revision, longest_line_width)));
        }
        if state.tokens_visible() {
            longest_line_width = if let Some(width) = wrap_width {
                width
            } else {
                state
                    .token_layout_cache
                    .as_ref()
                    .map_or(px(0.), |cache| cache.unwrapped_width)
            };
        }
        last_layout.lines = Rc::new(lines);

        let (ghost_first_line, ghost_lines) = Self::layout_inline_completion(
            state,
            &last_layout.visible_range,
            text_size,
            window,
            cx,
        );
        let ghost_line_count = ghost_lines.len();
        let ghost_lines_height = ghost_line_count as f32 * line_height;

        let total_wrapped_lines = state.display_map.wrap_row_count();
        let empty_bottom_height = empty_bottom_height(
            state.is_code_editor(),
            state.scroll_beyond_last_line,
            bounds.size.height,
            line_height,
        );

        // Empty bottom and ghost lines both describe extra height past the
        // last content row, so take the max rather than summing — summing
        // left a band of empty space the cursor could never reach.
        let mut scroll_size = size(
            if longest_line_width + line_number_width + RIGHT_MARGIN > bounds.size.width {
                longest_line_width + line_number_width + RIGHT_MARGIN
            } else {
                longest_line_width
            },
            (total_wrapped_lines as f32 * line_height
                + empty_bottom_height.max(ghost_lines_height))
            .max(bounds.size.height),
        );

        // TODO: should be add some gap to right, to convenient to focus on boundary position
        if last_layout.text_align == TextAlign::Right || last_layout.text_align == TextAlign::Center
        {
            scroll_size.width = longest_line_width + line_number_width;
        }

        // `position_for_index` for example
        //
        // #### text
        //
        // Hello 世界，this is GPUI component.
        // The GPUI Component is a collection of UI components for
        // GPUI framework, including Button, Input, Checkbox, Radio,
        // Dropdown, Tab, and more...
        //
        // wrap_width: 444px, line_height: 20px
        //
        // #### lines[0]
        //
        // | index | pos              | line |
        // |-------|------------------|------|
        // | 5     | (37 px, 0.0)     | 0    |
        // | 38    | (261.7 px, 20.0) | 0    |
        // | 40    | None             | -    |
        //
        // #### lines[1]
        //
        // | index | position              | line |
        // |-------|-----------------------|------|
        // | 5     | (43.578125 px, 0.0)   | 0    |
        // | 56    | (422.21094 px, 0.0)   | 0    |
        // | 57    | (11.6328125 px, 20.0) | 1    |
        // | 114   | (429.85938 px, 20.0)  | 1    |
        // | 115   | (11.3125 px, 40.0)    | 2    |

        // Calculate the scroll offset to keep the cursor in view

        // Save the unscrolled x before layout_cursors modifies bounds.origin with scroll_offset.
        // Fold icons and their hitboxes must use this value so they stay fixed in the gutter
        // regardless of horizontal scroll position.
        let input_bounds = bounds;
        let original_x = bounds.origin.x;
        // Resolved content and viewport sizes can change during a gesture.
        // A non-scrolling input must not retain a drag that will no longer
        // receive its Ended packet through the overscroll path.
        if scroll_size.height <= input_bounds.size.height {
            self.state.update(cx, |state, _| state.overscroll.reset());
            stretch = 0.;
        }
        // Keep viewport geometry for scrollbars and hitboxes fixed; only
        // text, its carets, selections, and gutter contents are displaced.
        bounds.origin.y += px(stretch);

        let (cursor_infos, cursor_scroll_offset, current_row, resize_reveal_owned) =
            self.layout_cursors(&last_layout, &mut bounds, scroll_size, window, cx);
        // Completion/code-action menus position at the active caret.
        last_layout.cursor_bounds = cursor_infos
            .iter()
            .find(|info| info.is_active)
            .map(|info| info.bounds);

        // Stash the fresh caret geometry for caret-anchored popups. Their
        // deferred prepaint runs after this prepaint but before paint, so
        // they anchor to the current frame's caret instead of the previous
        // frame's. Only the positioning geometry is stashed here; the rest
        // of the paint-time state updates stay where they are.
        let caret_geometry = last_layout.cursor_bounds.map(|cursor_bounds| {
            let mut scroll_offset = cursor_scroll_offset;
            // Mirror update_scroll_offset's clamping with the fresh values,
            // so the stash matches what paint will publish.
            let state = self.state.read(cx);
            let safe_y_range = (-scroll_size.height + input_bounds.size.height).min(px(0.))
                ..state.top_inset(input_bounds.size.height, line_height);
            scroll_offset.y = if state.is_single_line() {
                px(0.)
            } else {
                scroll_offset.y.clamp(safe_y_range.start, safe_y_range.end)
            };
            scroll_offset.x = clamp_horizontal_scroll_offset(
                scroll_offset.x,
                scroll_size.width,
                input_bounds.size.width,
                last_layout.text_align,
            );
            let mut geometry =
                PrepaintCaretGeometry::new(cursor_bounds, line_height, input_bounds, scroll_offset);
            geometry.resize_reveal_owned = resize_reveal_owned;
            geometry
        });
        self.state.update(cx, |state, _| {
            state.prepaint_caret_geometry = caret_geometry;
        });

        let search_match_paths = self.layout_search_matches(&last_layout, &mut bounds, cx);
        let search_scope_paths = self.layout_search_scope(&last_layout, &bounds, cx);
        let selection_paths = self.layout_selections(&last_layout, &mut bounds, window, cx);
        let hover_highlight_path = self.layout_hover_highlight(&last_layout, &mut bounds, cx);
        let bracket_paths = self.layout_bracket_pairs(&last_layout, &bounds, cx);
        let hidden_paths = hidden_ranges
            .into_iter()
            .filter_map(|range| {
                Self::layout_match_range(range, &last_layout, &bounds, HIDDEN_TAG_RADIUS)
            })
            .collect::<Vec<_>>();
        let document_color_paths =
            self.layout_document_colors(&document_colors, &last_layout, &bounds, cx);
        let (range_decoration_fills, range_decoration_frames) = self.layout_range_decorations(
            &last_layout,
            &bounds,
            window,
            window.content_mask().bounds,
            cx,
        );
        let (halo_range, halo_level) = halo.unwrap_or_default();
        let halo_quads = Self::layout_range_corners(&halo_range, &last_layout)
            .map(|corners| {
                let origin = bounds.origin + point(last_layout.line_number_width, px(0.));
                corners
                    .iter()
                    .map(|c| Bounds::from_corners(origin + c.top_left, origin + c.bottom_right))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let halo_radius = last_layout.line_height * SELECTION_CORNER_RADIUS;
        let wash_paths = wash_ranges
            .into_iter()
            .filter_map(|range| {
                Self::layout_match_range(
                    range,
                    &last_layout,
                    &bounds,
                    last_layout.line_height * SELECTION_CORNER_RADIUS,
                )
            })
            .collect::<Vec<_>>();

        let state = self.state.read(cx);
        // The gutter is quiet: 11 pt in the system face with tabular figures, on
        // the code line's baseline, 0.4 ink for other lines and full ink for the
        // current one. A number is shaped bare and set flush right in its column.
        let gutter_font = system_look::gutter_font();
        let line_number_right = line_number_width
            - LINE_NUMBER_RIGHT_MARGIN
            - if state.mode.is_folding() {
                FOLD_ICON_HITBOX_WIDTH
            } else {
                px(0.)
            };
        let mut line_number_dy = px(0.);
        let patch_fresh = state
            .patch_rows
            .clone()
            .filter(|rows| rows.revision == state.document_revision);
        let mut patch_old: Vec<ShapedLine> = Vec::new();
        let line_numbers = if state.shows_line_numbers() {
            let mut line_numbers = Vec::with_capacity(last_layout.visible_buffer_lines.len());
            let dark = state.editor_style.background.l < 0.5;
            // `FontMetrics::descent` is negative, so this is ascent minus |descent|.
            let code_font_id = window.text_system().resolve_font(&text_style.font());
            let code_ascent_descent = window.text_system().ascent(code_font_id, text_size)
                + window.text_system().descent(code_font_id, text_size);
            let gutter_run = |len: usize, color: Hsla| {
                [TextRun {
                    len,
                    font: gutter_font.clone(),
                    color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                }]
            };
            let other_color = system_look::ink(state.editor_style.foreground, 0.4, dark);
            let current_color = state.editor_style.foreground;
            // With a band in a bound caret layer, the layer paints the current line's figure (the
            // editor skips it), dim to bright with the band's glide (gpui-editor#dgar). An editor
            // fading out after a tab switch, or held by a closing pane, has no bound layer and
            // keeps its own.
            let layer_number = state.caret_layer.as_ref().is_some_and(|layer| layer.bound())
                && state.patch_rows.is_none()
                && band_color(
                    state.editor_style.editor_active_line,
                    state.disabled,
                    key_level,
                )
                .is_some();

            // A patch shows two columns, the old and new line numbers worked out by the host;
            // the old column's figures are shaped here and painted with the gutter.
            let patch_mode = state.patch_rows.is_some();
            if patch_mode {
                let probe = window.text_system().shape_line(
                    "0".into(),
                    system_look::GUTTER_SIZE,
                    &gutter_run(1, other_color),
                    None,
                );
                line_number_dy = (code_ascent_descent - (probe.ascent - probe.descent)) / 2.;
            }
            let mut old_numbers: Vec<ShapedLine> = Vec::new();

            // build line numbers
            for (line, &buffer_line) in last_layout
                .lines
                .iter()
                .zip(last_layout.visible_buffer_lines.iter())
            {
                let (old, new) = patch_fresh
                    .as_ref()
                    .and_then(|rows| rows.rows.get(buffer_line))
                    .map_or((0, 0), |row| (row.1, row.2));
                let line_no: SharedString = if patch_mode {
                    if new > 0 {
                        displayed_line_number(new as usize).to_string().into()
                    } else {
                        SharedString::default()
                    }
                } else {
                    displayed_line_number(buffer_line + 1).to_string().into()
                };

                let color = if current_row == Some(buffer_line) {
                    current_color
                } else {
                    other_color
                };
                let runs = gutter_run(line_no.len(), color);

                let mut sub_lines: SmallVec<[ShapedLine; 1]> = SmallVec::new();
                let number = if line_no.is_empty() {
                    ShapedLine::default()
                } else {
                    window.text_system().shape_line(line_no, system_look::GUTTER_SIZE, &runs, None)
                };
                if patch_mode {
                    old_numbers.push(if old > 0 {
                        let text: SharedString = displayed_line_number(old as usize).to_string().into();
                        let runs = gutter_run(text.len(), color);
                        window.text_system().shape_line(text, system_look::GUTTER_SIZE, &runs, None)
                    } else {
                        ShapedLine::default()
                    });
                } else {
                    // Lines center their ascent and descent in the line height, so the
                    // baselines meet when the number drops by half the gap of
                    // `ascent - descent`; the code side is the code font's metrics.
                    line_number_dy = (code_ascent_descent - (number.ascent - number.descent)) / 2.;
                }
                // The layer paints the current line's figure, once, so it isn't doubled.
                sub_lines.push(if current_row == Some(buffer_line) && layer_number {
                    ShapedLine::default()
                } else {
                    number
                });
                for _ in 0..line.wrapped_lines.len().saturating_sub(1) {
                    sub_lines.push(ShapedLine::default());
                }
                line_numbers.push(sub_lines);
            }
            patch_old = old_numbers;
            Some(line_numbers)
        } else {
            None
        };

        let hover_definition_hitbox = M::hover_definition_hitbox(state, window, cx);
        let indent_guides =
            self.layout_indent_guides(state, &bounds, &last_layout, &text_style, window);
        state
            .editor_scrollbar_snapshot
            .set(Some(EditorScrollbarSnapshot::new(
                input_bounds,
                &last_layout,
                scroll_size,
                cursor_scroll_offset,
                state,
            )));
        let fold_icon_layout = self.layout_fold_icons(
            original_x,
            line_number_width,
            &bounds,
            input_bounds.bottom(),
            &last_layout,
            window,
            cx,
        );
        let hitbox = window.insert_hitbox(input_bounds, HitboxBehavior::Normal);
        // Where a right click opens the host's gutter menu. A hitbox, so a float over the editor
        // (a card, a field, a cover) that sits above it keeps its own clicks.
        let gutter_menu_hitbox = {
            let state = self.state.read(cx);
            (state.is_code_editor() && state.gutter_menu_handler().is_some()).then(|| {
                let gutter = editor_gutter_bounds(input_bounds, line_number_width, ghost_lines_height, state.editor_paddings);
                window.insert_hitbox(gutter_menu_bounds(gutter), HitboxBehavior::Normal)
            })
        };
        self.request_reveal(
            input_bounds,
            &cursor_infos,
            cursor_scroll_offset,
            line_height,
            window,
            cx,
        );

        // With a caret layer the carets are left for it to paint, over this
        // frame's geometry: it never draws a frame behind the text.
        let state = self.state.read(cx);
        if let Some(layer) = &state.caret_layer {
            // A one-line field keeps the caret it paints itself: no outline when it loses the
            // keyboard, and a glide on every move (motion.md 'Field caret'); only an editor springs.
            let field = state.is_single_line();
            let visible = state.focus_handle.is_focused(window) && state.show_cursor(window, cx);
            let hollow = !field && !state.show_cursor(window, cx) && crate::input::window_key(window) && !state.disabled;
            let mut clip = window.content_mask().bounds;
            if line_numbers.is_some() {
                // The gutter covers the caret where the text scrolls under it.
                let cut = (input_bounds.origin.x + line_number_width - clip.origin.x)
                    .max(px(0.));
                clip.origin.x += cut;
                clip.size.width = (clip.size.width - cut).max(px(0.));
            }
            let band = band_color(
                state.editor_style.editor_active_line,
                state.disabled,
                key_level,
            )
            .zip(line_numbers.as_ref().zip(current_row))
            .and_then(|(color, (_, row))| {
                // The line's top and height, as the paint pass walks them.
                let mut top = bounds.origin.y + last_layout.visible_top;
                let (mut height, mut found) = (px(0.), false);
                for (line, &buffer_line) in
                    last_layout.lines.iter().zip(&last_layout.visible_buffer_lines)
                {
                    let line_h = line.size(line_height).height;
                    if buffer_line == row {
                        (height, found) = (line_h, true);
                        break;
                    }
                    top += line_h;
                }
                let mask = window.content_mask().bounds;
                let left = input_bounds.origin.x - state.editor_paddings.left;
                found.then(|| Band {
                    color,
                    left,
                    // To the right edge of the pane, past the input's own margin.
                    width: (mask.right() - left).max(
                        input_bounds.size.width
                            + state.editor_paddings.left
                            + state.editor_paddings.right,
                    ),
                    top: top - cursor_scroll_offset.y,
                    row,
                    number: Number {
                        // A patch's two columns are the editor's own to paint.
                        text: if state.patch_rows.is_some() {
                            SharedString::default()
                        } else {
                            displayed_line_number(row + 1).to_string().into()
                        },
                        font: gutter_font.clone(),
                        color: state.editor_style.foreground,
                        dim: system_look::ink(
                            state.editor_style.foreground,
                            0.4,
                            state.editor_style.background.l < 0.5,
                        ),
                        right: input_bounds.origin.x + line_number_right,
                        dy: line_number_dy,
                        line_height,
                    },
                    height,
                    clip: mask,
                })
            });
            layer.record(
                visible,
                hollow,
                &cursor_infos,
                cursor_scroll_offset,
                state.editor_style.caret,
                clip,
                band,
                state.document_revision,
                !field,
                window,
            );
        }

        // The hint is shaped only while it shows any: the system face at the code's size.
        let hint = (hint_level > 0.).then(|| {
            let ink = self.state.read(cx).editor_style.muted_foreground.opacity(hint_alpha * hint_level);
            let run = TextRun {
                len: hint_text.len(),
                font: system_look::gutter_font(),
                color: ink,
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            window.text_system().shape_line(hint_text.clone(), text_size, &[run], None)
        });

        // A patch's old-side numbers, and the file icons of its header bars: the host's icon is
        // an element, prepainted here at the gutter's left on each visible `PATCH_FILE` row.
        let patch = patch_fresh.map(|rows| {
            let mut icons = Vec::new();
            if line_numbers.is_some() {
                let side = (line_height * 0.8).min(px(16.));
                let mut top = bounds.origin.y + last_layout.visible_top;
                for (line, &buffer_line) in last_layout.lines.iter().zip(last_layout.visible_buffer_lines.iter()) {
                    let height = line.size(line_height).height;
                    if rows.rows.get(buffer_line).is_some_and(|row| row.0 == PATCH_FILE) {
                        let ink = self.state.read(cx).editor_style.foreground.opacity(0.8);
                        if let Some(mut icon) = (rows.icon)(buffer_line, side, ink) {
                            let at = point(input_bounds.origin.x + px(6.), top + (height - side) / 2.);
                            icon.prepaint_as_root(at, size(side, side).into(), window, cx);
                            icons.push(icon);
                        }
                    }
                    top += height;
                }
            }
            // The old column ends one figure column and a gap left of the new one.
            let old_right = line_number_right - patch_column_width(rows.digits, window) - PATCH_COLUMN_GAP;
            PatchPaint { rows, old: patch_old, old_right, icons }
        });

        let token_elements = self.prepaint_tokens(&last_layout, bounds, token_elements, window, cx);
        PrepaintState {
            token_elements,
            hitbox,
            bounds,
            last_layout,
            scroll_size,
            line_numbers,
            cursor_infos,
            cursor_scroll_offset,
            current_row,
            selection_paths,
            search_match_paths,
            search_scope_paths,
            hover_highlight_path,
            bracket_paths,
            hidden_paths,
            hover_definition_hitbox,
            gutter_menu_hitbox,
            document_color_paths,
            range_decoration_fills,
            range_decoration_frames,
            wash_paths,
            wash_level,
            halo_quads,
            halo_radius,
            halo_level,
            lift_level,
            key_level,
            indent_guides,
            fold_icon_layout,
            ghost_first_line,
            ghost_lines,
            ghost_lines_height,
            line_number_dy,
            line_number_right,
            gutter_width: line_number_width,
            hint,
            patch,
        }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _: Option<&gpui::InspectorElementId>,
        input_bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (focus_handle, show_cursor, disabled, selected_range, editor_style, editor_paddings) = {
            let state = self.state.read(cx);
            (
                state.focus_handle.clone(),
                state.show_cursor(window, cx),
                state.disabled,
                *state.active_selection(),
                state.editor_style.clone(),
                state.editor_paddings,
            )
        };
        let focused = focus_handle.is_focused(window);
        let bounds = prepaint.bounds;
        let text_align = prepaint.last_layout.text_align;

        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.state.clone()),
            cx,
        );

        // Paint multi line text
        let line_height = window.line_height();
        let origin = bounds.origin;

        let invisible_top_padding = prepaint.last_layout.visible_top;
        // The band under the current line dims with the selection when the
        // window isn't key, and is painted before it, so it stays under it. An input with a
        // caret layer leaves the band to the layer, which moves it with the caret.
        let active_line_color = if self.state.read(cx).caret_layer.is_some() {
            None
        } else {
            band_color(editor_style.editor_active_line, disabled, prepaint.key_level)
        };
        let editor_background = if disabled {
            editor_style.background.opacity(0.5)
        } else {
            editor_style.background
        };

        // A patch's rows: added and removed lines washed green and red, header bars, across the
        // whole row. Plain fills of the row's geometry; no text run changes.
        if let Some(patch) = prepaint.patch.as_ref() {
            let left = input_bounds.origin.x - editor_paddings.left;
            let width = (window.content_mask().bounds.right() - left)
                .max(input_bounds.size.width + editor_paddings.left + editor_paddings.right);
            let mut top = origin.y + invisible_top_padding;
            for (line, &buffer_line) in prepaint
                .last_layout
                .lines
                .iter()
                .zip(prepaint.last_layout.visible_buffer_lines.iter())
            {
                let height = line.size(line_height).height;
                let kind = patch.rows.rows.get(buffer_line).map_or(0, |row| row.0);
                if let Some(wash) = patch_wash(kind, editor_style.foreground) {
                    window.paint_quad(fill(Bounds::new(point(left, top), size(width, height)), wash));
                }
                top += height;
                if !prepaint.ghost_lines.is_empty() && prepaint.current_row == Some(buffer_line) {
                    top += prepaint.ghost_lines_height;
                }
            }
        }

        // Paint active line
        let mut offset_y = px(0.);
        if let Some(line_numbers) = prepaint.line_numbers.as_ref() {
            offset_y += invisible_top_padding;

            // Each item is the normal lines.
            for (lines, &buffer_line) in line_numbers
                .iter()
                .zip(prepaint.last_layout.visible_buffer_lines.iter())
            {
                let is_active = prepaint.current_row == Some(buffer_line);
                let p = point(input_bounds.origin.x, origin.y + offset_y);
                let height = line_height * lines.len() as f32;
                // Paint the current line background
                if is_active {
                    if let Some(bg_color) = active_line_color {
                        window.paint_quad(fill(
                            Bounds::new(p, size(input_bounds.size.width + editor_paddings.right, height)),
                            bg_color,
                        ));
                    }
                }
                offset_y += height;
            }
        }

        // Keep scrollbar offset always be positive，Start from the left position
        let scroll_offset = if text_align == TextAlign::Right {
            (prepaint.scroll_size.width - prepaint.bounds.size.width).max(px(0.))
        } else if text_align == TextAlign::Center {
            (prepaint.scroll_size.width - prepaint.bounds.size.width)
                .half()
                .max(px(0.))
        } else {
            px(0.)
        };

        // Paint glyph backgrounds
        //
        // gpui's `paint` only draws glyphs, underlines, and strikethroughs, so decoration
        // backgrounds need their own pass. It runs ahead of the indent guides, selections,
        // and text so a highlighted range sits under them instead of covering them.
        let mut offset_y = invisible_top_padding;
        for (line, &buffer_line) in prepaint
            .last_layout
            .lines
            .iter()
            .zip(prepaint.last_layout.visible_buffer_lines.iter())
        {
            let p = point(
                origin.x + prepaint.last_layout.line_number_width + scroll_offset,
                origin.y + offset_y,
            );

            line.paint_background(
                p,
                line_height,
                text_align,
                Some(prepaint.last_layout.content_width),
                window,
                cx,
            );

            offset_y += line.size(line_height).height;

            // Ghost lines shift every later line down.
            if Some(buffer_line) == prepaint.current_row {
                offset_y += prepaint.ghost_lines_height;
            }
        }

        // Hidden-character tags: rounded, quieter on the light paper (gpui-editor#88sw).
        let hidden_fill = editor_style
            .foreground
            .opacity(if editor_style.background.l < 0.5 { 0.16 } else { 0.12 });
        for path in prepaint.hidden_paths.drain(..) {
            window.paint_path(path, hidden_fill);
        }

        // Paint indent guides
        let indent_guide_color = editor_style.border.opacity(0.85);
        for guide in prepaint.indent_guides.drain(..) {
            window.paint_quad(fill(guide, indent_guide_color));
        }

        // Application decorations sit below the user's selection.
        for (path, color) in &prepaint.range_decoration_fills {
            window.paint_path(path.clone(), *color);
        }
        for (path, color) in &prepaint.range_decoration_frames {
            window.paint_path(path.clone(), *color);
        }

        // Paint selections. A key window gets the Mac's accent; any other
        // window keeps the selection visible in the text's own ink, quieter; a window that just
        // flipped eases between the two.
        let key_window = crate::input::window_key(window);
        let dark = editor_style.background.l < 0.5;
        // The accent is read from AppKit only when a fill will use it.
        let selection = if !prepaint.selection_paths.is_empty() {
            key_fill(prepaint.key_level, |key| {
                system_look::selection_fill(editor_style.foreground, editor_style.selection, dark, key)
            })
        } else {
            editor_style.selection
        };
        // A Copy lifts the fill from its alpha at rest up a step and back, once.
        let selection = if prepaint.lift_level > 0. && key_window {
            Hsla { a: (selection.a + LIFT_STEP * prepaint.lift_level).min(1.), ..selection }
        } else {
            selection
        };
        // The wash over rows a multi-line paste or undo just changed, under the selection.
        if !prepaint.wash_paths.is_empty() {
            let ink = key_fill(prepaint.key_level, |key| {
                system_look::selection_fill(editor_style.foreground, editor_style.selection, dark, key)
            })
            .opacity(WASH_INK * prepaint.wash_level);
            if let Some(probe) = WASH_PROBE.get() {
                let drawn: Vec<Bounds<Pixels>> = prepaint.wash_paths.iter().map(|path| path.bounds).collect();
                probe(window, &drawn, ink.a);
            }
            for path in prepaint.wash_paths.drain(..) {
                window.paint_path(path, ink);
            }
        }
        // The halo under the matches: the current match's fill, reaching out and fading as it
        // settles. Blended onto the background first, so rows of a wrapped match that overlap
        // don't stack their alpha; it paints with the find bar closed too (Cmd-G).
        if !prepaint.halo_quads.is_empty() {
            let current = key_fill(prepaint.key_level, |key| {
                crate::theme_tokens::find_fills(editor_style.foreground, dark, key).current
            });
            let level = prepaint.halo_level;
            let ink = editor_style.background.blend(current.opacity(level));
            let reach = HALO_GROW * level;
            for quad in &prepaint.halo_quads {
                window.paint_quad(fill(quad.dilate(reach), ink).corner_radii(prepaint.halo_radius + reach));
            }
        }
        // Find: every match in the soft wash, then the current one in its own fill, last.
        if !prepaint.search_match_paths.is_empty() || prepaint.search_scope_paths.as_ref().is_some_and(|paths| !paths.is_empty()) {
            let fills = if prepaint.key_level >= 1. {
                crate::theme_tokens::find_fills(editor_style.foreground, dark, true)
            } else if prepaint.key_level <= 0. {
                crate::theme_tokens::find_fills(editor_style.foreground, dark, false)
            } else {
                let off = crate::theme_tokens::find_fills(editor_style.foreground, dark, false);
                let on = crate::theme_tokens::find_fills(editor_style.foreground, dark, true);
                crate::theme_tokens::FindFills {
                    matches: mix_fill(off.matches, on.matches, prepaint.key_level),
                    current: mix_fill(off.current, on.current, prepaint.key_level),
                }
            };
            // Find in selection: a faint wash over the text the search is limited to, under the matches.
            for path in prepaint.search_scope_paths.iter().flat_map(|paths| paths.iter()) {
                window.paint_path(path.clone(), fills.matches.opacity(SEARCH_SCOPE_FILL));
            }
            for (path, _) in prepaint.search_match_paths.iter().filter(|(_, active)| !*active) {
                window.paint_path(path.clone(), fills.matches);
            }
            for (path, _) in prepaint.search_match_paths.iter().filter(|(_, active)| *active) {
                window.paint_path(path.clone(), fills.current);
            }
        }
        if key_window {
            let secondary_selection = Hsla {
                s: 0.1,
                ..editor_style.selection
            };
            // Paint hover highlight
            if let Some(path) = prepaint.hover_highlight_path.take() {
                window.paint_path(path, secondary_selection);
            }
        }

        // Paint matching brackets, quieter than a selection. A window that is
        // not key keeps them, the way it keeps its selection.
        let bracket_fill = editor_style.foreground.opacity(0.16);
        for path in prepaint.bracket_paths.drain(..) {
            window.paint_path(path, bracket_fill);
        }
        for path in prepaint.selection_paths.drain(..) {
            window.paint_path(path, selection);
        }

        // Paint document colors
        for (path, color) in prepaint.document_color_paths.iter() {
            let color = if disabled { color.opacity(0.5) } else { *color };
            window.paint_path(path.clone(), color);
        }

        // The faint line of an empty page (`Hint`), under the caret and any text, never hit.
        if let Some(hint) = &prepaint.hint {
            _ = hint.paint(
                point(origin.x + prepaint.last_layout.line_number_width + scroll_offset, origin.y + invisible_top_padding),
                line_height,
                text_align,
                None,
                window,
                cx,
            );
        }

        // Paint text with inline completion ghost line support
        let mut offset_y = invisible_top_padding;
        let ghost_lines = &prepaint.ghost_lines;
        let has_ghost_lines = !ghost_lines.is_empty();

        // Track the y-position of the cursor row for positioning the first line suffix
        let mut cursor_row_y = None;

        for (line, &buffer_line) in prepaint
            .last_layout
            .lines
            .iter()
            .zip(prepaint.last_layout.visible_buffer_lines.iter())
        {
            let row = buffer_line;
            let line_y = origin.y + offset_y;
            let p = point(
                origin.x + prepaint.last_layout.line_number_width + (scroll_offset),
                line_y,
            );

            // Paint the actual line
            _ = line.paint(
                p,
                line_height,
                text_align,
                Some(prepaint.last_layout.content_width),
                window,
                cx,
            );
            offset_y += line.size(line_height).height;

            if Some(row) == prepaint.current_row {
                cursor_row_y = Some(line_y);
            }

            // After the cursor row, paint ghost lines (which shifts subsequent content down)
            if has_ghost_lines && Some(row) == prepaint.current_row {
                let ghost_x = origin.x + prepaint.last_layout.line_number_width;

                for ghost_line in ghost_lines {
                    let ghost_p = point(ghost_x, origin.y + offset_y);

                    // Paint semi-transparent background for ghost line
                    let ghost_bounds = Bounds::new(
                        ghost_p,
                        size(
                            bounds.size.width - prepaint.last_layout.line_number_width,
                            line_height,
                        ),
                    );
                    window.paint_quad(fill(ghost_bounds, editor_background));

                    // Paint ghost line text
                    _ = ghost_line.paint(
                        ghost_p,
                        line_height,
                        text_align,
                        Some(prepaint.last_layout.content_width),
                        window,
                        cx,
                    );
                    offset_y += line_height;
                }
            }
        }

        for element in &mut prepaint.token_elements {
            element.paint(window, cx);
        }

        // Paint the caret. The active one glides to each new spot over
        // `CARET_GLIDE`, the blink halves fade over `CARET_FADE`, and a caret
        // hides over a selection. A frame is asked for only while a glide or a
        // fade runs, so the time between blink halves draws nothing. An input
        // with a caret layer (`enable_caret_layer`) records its carets in
        // prepaint instead, and the layer paints them.
        if self.state.read(cx).caret_layer.is_some() {
            // Left to the layer.
        } else if focused && show_cursor {
            let now = crate::motion::frame_time(window.window_handle().window_id());
            let (blink, fading) = self.state.read(cx).blink_cursor.read(cx).alpha(now);
            let mut glide = self
                .state
                .update(cx, |state, _| std::mem::take(&mut state.caret_glide));
            let more = paint_carets(
                &prepaint.cursor_infos,
                prepaint.cursor_scroll_offset,
                editor_style.caret,
                (blink, fading),
                &mut glide,
                now,
                // Only the caret layer springs: it knows when an edit moved the caret.
                false,
                window,
            );
            self.state.update(cx, |state, _| state.caret_glide = glide);
            if more {
                window.request_animation_frame();
            }
        } else if self.state.read(cx).caret_glide.tracking() {
            // A blur forgets the spot, so the next focus lands instead of gliding.
            self.state.update(cx, |state, _| state.caret_glide.clear());
        }

        // Paint line numbers
        let mut offset_y = px(0.);
        if let Some(line_numbers) = prepaint.line_numbers.as_ref() {
            offset_y += invisible_top_padding;

            // The gutter is a fixed overlay above horizontally scrolling text.
            // Always give it an opaque editor background when the theme does not
            // provide a dedicated gutter color, and cover the complete gutter so
            // text cannot show through its right-side spacing/fold-icon area.
            let gutter_bg = editor_style
                .editor_gutter_background
                .unwrap_or(editor_background);
            let gutter_bounds = editor_gutter_bounds(
                input_bounds,
                prepaint.gutter_width,
                prepaint.ghost_lines_height,
                editor_paddings,
            );
            window.paint_quad(fill(gutter_bounds, gutter_bg));

            // Each item is the normal lines.
            for (row_ix, (lines, &buffer_line)) in line_numbers
                .iter()
                .zip(prepaint.last_layout.visible_buffer_lines.iter())
                .enumerate()
            {
                let p = point(input_bounds.origin.x, origin.y + offset_y);
                let is_active = prepaint.current_row == Some(buffer_line);

                let height = line_height * lines.len() as f32;
                // paint active line number background
                if is_active {
                    if let Some(bg_color) = active_line_color {
                        window.paint_quad(fill(
                            Bounds::new(
                                point(gutter_bounds.origin.x, p.y),
                                size(gutter_bounds.size.width, height),
                            ),
                            bg_color,
                        ));
                    }
                }

                // A patch row's wash and old-side number, over the gutter's own ground.
                if let Some(patch) = prepaint.patch.as_ref() {
                    let kind = patch.rows.rows.get(buffer_line).map_or(0, |row| row.0);
                    if let Some(wash) = patch_wash(kind, editor_style.foreground) {
                        window.paint_quad(fill(
                            Bounds::new(point(gutter_bounds.origin.x, p.y), size(gutter_bounds.size.width, height)),
                            wash,
                        ));
                    }
                    if let Some(old) = patch.old.get(row_ix).filter(|old| old.len > 0) {
                        _ = old.paint(
                            point(p.x + patch.old_right - old.width, p.y + prepaint.line_number_dy),
                            line_height,
                            TextAlign::Left,
                            None,
                            window,
                            cx,
                        );
                    }
                }

                for line in lines {
                    _ = line.paint(
                        point(
                            p.x + prepaint.line_number_right - line.width,
                            p.y + prepaint.line_number_dy,
                        ),
                        line_height,
                        TextAlign::Left,
                        None,
                        window,
                        cx,
                    );
                    offset_y += line_height;
                }

                // Add ghost line height after cursor row for line numbers alignment
                if !prepaint.ghost_lines.is_empty() && prepaint.current_row == Some(buffer_line) {
                    offset_y += prepaint.ghost_lines_height;
                }
            }
        }

        // The file icons of a patch's header bars, in the gutter's empty number column.
        if let Some(patch) = prepaint.patch.as_mut() {
            for icon in &mut patch.icons {
                icon.paint(window, cx);
            }
        }

        // Paint fold arrows, chips and the glide curtain
        let fold_gutter_bounds = editor_gutter_bounds(
            input_bounds,
            prepaint.gutter_width,
            prepaint.ghost_lines_height,
            editor_paddings,
        );
        self.paint_fold_icons(
            &mut prepaint.fold_icon_layout,
            editor_style.foreground,
            editor_background,
            editor_style.editor_gutter_background.unwrap_or(editor_background),
            fold_gutter_bounds,
            input_bounds,
            window,
            cx,
        );

        // A right click in the gutter opens the host's gutter menu and nothing else: the caret
        // stays and the text's menu stays shut. It takes the click with the numbers hidden too.
        if let Some(hitbox) = prepaint.gutter_menu_hitbox.clone()
            && let Some(handler) = self.state.read(cx).gutter_menu_handler()
        {
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if !phase.bubble()
                    || event.button != MouseButton::Right
                    || !hitbox.is_hovered(window)
                    || window.default_prevented()
                {
                    return;
                }
                window.prevent_default();
                let handler = handler.clone();
                let position = event.position;
                window.defer(cx, move |window, cx| handler(position, window, cx));
            });
        }

        self.state.update(cx, |state, cx| {
            let geometry_changed = state.last_bounds != Some(bounds)
                || state.input_bounds != input_bounds
                || state.scroll_size != prepaint.scroll_size
                || state.last_layout.as_ref().is_none_or(|layout| {
                    layout.cursor_bounds != prepaint.last_layout.cursor_bounds
                        || layout.line_height != prepaint.last_layout.line_height
                });
            state.last_layout = Some(prepaint.last_layout.clone());
            state.last_bounds = Some(bounds);
            state.last_cursor = Some(state.cursor());
            state.set_input_bounds(input_bounds, cx);
            state.last_selected_range = Some(selected_range);
            state.laid_out_revision = state.document_revision;
            state.reveal_far_centered = false;
            state.scroll_size = prepaint.scroll_size;
            state.update_scroll_offset(Some(prepaint.cursor_scroll_offset), cx);
            state.deferred_scroll_offset = None;
            state.glide_y = None;
            state.glide_x = None;

            // Layout consumers need changed geometry, not another notification
            // for every paint of an unchanged input.
            if geometry_changed {
                cx.notify();
            }
        });

        if let Some(hitbox) = prepaint.hover_definition_hitbox.as_ref()
            && !window.modifiers().alt
        {
            window.set_cursor_style(gpui::CursorStyle::PointingHand, &hitbox);
        }

        // Paint inline completion first line suffix (after cursor on same line)
        if focused {
            if let Some(first_line) = &prepaint.ghost_first_line {
                let active_cursor = prepaint
                    .cursor_infos_with_scroll()
                    .into_iter()
                    .find(|info| info.is_active);
                if let (Some(cursor_info), Some(cursor_row_y)) = (active_cursor, cursor_row_y) {
                    let cursor_bounds = cursor_info.bounds;
                    let first_line_x = cursor_bounds.origin.x + cursor_bounds.size.width;
                    let p = point(first_line_x, cursor_row_y);

                    // Paint background to cover any existing text
                    let bg_bounds = Bounds::new(p, size(first_line.width + px(4.), line_height));
                    window.paint_quad(fill(bg_bounds, editor_background));

                    // Paint first line completion text
                    _ = first_line.paint(p, line_height, text_align, None, window, cx);
                }
            }
        }

        self.paint_mouse_listeners(&prepaint.hitbox, window, cx);
    }
}

/// Split placeholder text into display lines and trim runs to each line.
fn placeholder_line_runs<'a>(
    display_text: &'a str,
    runs: &[TextRun],
) -> Vec<(&'a str, Vec<TextRun>)> {
    let mut result = Vec::new();
    let mut line_offset = 0;

    for line in display_text.split('\n') {
        let line_runs = runs_for_range(runs, line_offset, &(0..line.len()));
        debug_assert_eq!(
            line_runs.iter().map(|run| run.len).sum::<usize>(),
            line.len()
        );
        result.push((line, line_runs));
        // Advance in the whole-placeholder coordinate space, including the separator.
        line_offset += line.len() + 1;
    }

    result
}

/// Get the runs for the given range.
///
/// The range is the byte range of the wrapped line.
pub(super) fn runs_for_range(
    runs: &[TextRun],
    line_offset: usize,
    range: &Range<usize>,
) -> Vec<TextRun> {
    let mut result = vec![];
    let range = (line_offset + range.start)..(line_offset + range.end);
    let mut cursor = 0;

    for run in runs {
        let run_start = cursor;
        let run_end = cursor + run.len;

        if run_end <= range.start {
            cursor = run_end;
            continue;
        }

        if run_start >= range.end {
            break;
        }

        let start = range.start.max(run_start) - run_start;
        let end = range.end.min(run_end) - run_start;
        let len = end - start;

        if len > 0 {
            result.push(TextRun { len, ..run.clone() });
        }

        cursor = run_end;
    }

    result
}

/// Clip sorted `styles` to the sorted, disjoint `ranges`, splitting styles
/// that span several ranges and dropping coverage in the gaps between them.
fn clip_styles_to_ranges(
    styles: Vec<(Range<usize>, HighlightStyle)>,
    ranges: &[Range<usize>],
) -> Vec<(Range<usize>, HighlightStyle)> {
    let mut result = Vec::with_capacity(styles.len());
    let mut range_ix = 0;

    for (style_range, style) in styles {
        while range_ix < ranges.len() && ranges[range_ix].end <= style_range.start {
            range_ix += 1;
        }

        let mut ix = range_ix;
        while ix < ranges.len() && ranges[ix].start < style_range.end {
            let start = style_range.start.max(ranges[ix].start);
            let end = style_range.end.min(ranges[ix].end);
            if start < end {
                result.push((start..end, style));
            }
            ix += 1;
        }
    }

    result
}

/// Snap run boundaries to char boundaries of `text` and cap them at its
/// length. Style ranges are byte offsets that can drift off char boundaries
/// (a stale syntax tree, a range in the wrong coordinate space) and the
/// platform text system panics when a run splits a multi-byte character.
///
/// Returns `None` when the runs are already aligned (the common case).
pub(super) fn align_runs_to_char_boundaries(text: &str, runs: &[TextRun]) -> Option<Vec<TextRun>> {
    let mut end = 0;
    let aligned = runs.iter().all(|run| {
        end += run.len;
        end <= text.len() && text.is_char_boundary(end)
    });
    if aligned {
        return None;
    }

    let mut result = Vec::with_capacity(runs.len());
    let mut cursor = 0;
    let mut raw_end = 0;
    for run in runs {
        raw_end = (raw_end + run.len).min(text.len());
        let mut end = raw_end;
        while !text.is_char_boundary(end) {
            end += 1;
        }
        if end > cursor {
            result.push(TextRun {
                len: end - cursor,
                ..run.clone()
            });
            cursor = end;
        }
    }

    Some(result)
}

fn split_run_for_ime_underline(
    run: TextRun,
    run_range: Range<usize>,
    marked_range: Option<Range<usize>>,
    clause_range: Option<Range<usize>>,
    marked_underline: Option<UnderlineStyle>,
) -> SmallVec<[TextRun; 5]> {
    if run.len == 0 {
        return SmallVec::new();
    }

    let Some(marked) = marked_range else {
        return [run].into_iter().collect();
    };

    let intersection_start = run_range.start.max(marked.start);
    let intersection_end = run_range.end.min(marked.end);
    if intersection_start >= intersection_end {
        return [run].into_iter().collect();
    }

    let clause = clause_range.unwrap_or(marked.start..marked.start);
    let mut boundaries = [
        run_range.start,
        intersection_start,
        clause.start.clamp(intersection_start, intersection_end),
        clause.end.clamp(intersection_start, intersection_end),
        intersection_end,
        run_range.end,
    ];
    boundaries.sort_unstable();
    let mut runs = SmallVec::new();
    for bounds in boundaries.windows(2) {
        let [start, end] = [bounds[0], bounds[1]];
        if start == end {
            continue;
        }
        let mut part = run.clone();
        part.len = end - start;
        if start >= marked.start && end <= marked.end {
            part.underline = marked_underline.map(|mut underline| {
                if start >= clause.start && end <= clause.end {
                    underline.thickness *= 2.;
                }
                underline
            });
        }
        runs.push(part);
    }
    runs
}

/// Whether any of these runs paints a glyph background, used to skip the background
/// paint pass for lines without highlights.
fn has_background(runs: &[TextRun]) -> bool {
    runs.iter().any(|run| run.background_color.is_some())
}

/// Zed's default for text over a highlight background (APCA Lc).
const MIN_HIGHLIGHT_CONTRAST: f32 = 45.;

/// `text` moved only as far as it takes to read over `background`: lightness
/// first, then saturation, then black or white. Text that is quiet on purpose
/// (a comment) is asked for no more contrast than it has over `ground`, so a
/// match does not turn it white. Highlights recolor many runs of a few syntax
/// colors, so the answers are kept.
fn readable_over(text: Hsla, background: Hsla, ground: Hsla) -> Hsla {
    thread_local! {
        static ANSWERS: std::cell::RefCell<Vec<(Hsla, Hsla, Hsla, Hsla)>> =
            const { std::cell::RefCell::new(Vec::new()) };
    }
    ANSWERS.with_borrow_mut(|answers| {
        if let Some((_, _, _, readable)) = answers
            .iter()
            .find(|(t, b, g, _)| *t == text && *b == background && *g == ground)
        {
            return *readable;
        }
        let wanted = MIN_HIGHLIGHT_CONTRAST.min(apca_contrast(text, ground).abs());
        let readable = ensure_minimum_contrast(text, background, wanted);
        if answers.len() >= 256 {
            answers.clear();
        }
        answers.push((text, background, ground, readable));
        readable
    })
}

/// `runs` (starting at `start_offset`) with the text over each background
/// segment (sorted, apart) recolored to read there. Pieces of one run that
/// come out the same rejoin as they are pushed, so text that needed no change
/// shapes as it would without the split.
fn split_runs_by_bg_segments(
    start_offset: usize,
    runs: &[TextRun],
    bg_segments: &[(Range<usize>, Hsla)],
    faded: &[(Range<usize>, f32)],
    ground: Hsla,
) -> Vec<TextRun> {
    fn push_piece(result: &mut Vec<TextRun>, run: &TextRun, len: usize, color: Hsla) {
        match result.last_mut() {
            Some(last)
                if last.font == run.font
                    && last.color == color
                    && last.background_color == run.background_color
                    && last.underline == run.underline
                    && last.strikethrough == run.strikethrough =>
            {
                last.len += len;
            }
            _ => result.push(TextRun {
                len,
                color,
                ..run.clone()
            }),
        }
    }

    let mut result = Vec::with_capacity(runs.len());
    let mut run_start = start_offset;
    for run in runs {
        let run_end = run_start + run.len;
        let mut at = run_start;
        let first = bg_segments.partition_point(|(range, _)| range.end <= run_start);
        for (range, bg_color) in &bg_segments[first..] {
            if range.start >= run_end {
                break;
            }
            let from = at.max(range.start);
            let to = run_end.min(range.end);
            if from >= to {
                continue;
            }
            if at < from {
                push_piece(&mut result, run, from - at, run.color);
            }
            // A focus dim (`fade_out`) lowered the run's alpha: text over a fill (selection, find
            // match) takes back the alpha the dim took away, so it stays at full strength inside
            // faded text. Text outside a faded range keeps its own alpha.
            let ink = match faded.get(faded.partition_point(|(range, _)| range.end <= from)) {
                Some((range, alpha)) if range.start <= from => Hsla { a: *alpha, ..run.color },
                _ => run.color,
            };
            push_piece(&mut result, run, to - from, readable_over(ink, *bg_color, ground));
            at = to;
        }
        if at < run_end {
            push_piece(&mut result, run, run_end - at, run.color);
        }
        run_start = run_end;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{EditorMode, EditorState, FoldRange, RangeDecoration, Redo, Undo};
    use gpui::{
        AppContext as _, Context, EntityInputHandler as _, ListAlignment, ListOffset, ListState,
        Render, TestAppContext, VisualTestContext, div, list, prelude::FluentBuilder as _,
    };

    /// The longest line's cached width follows its text: an edit on another row keeps the key's
    /// hash, an edit on the row itself changes it, and chunking never matters (finch#kwn8).
    #[test]
    fn longest_line_hash_follows_the_line_text_only() {
        let long = "x".repeat(5_000);
        let mut rope = Rope::from(format!("short\n{long}\nend").as_str());
        let before = line_content_hash(&rope.slice_line(1));
        let at = rope.line_start_offset(0);
        rope.insert(at, "typed ");
        assert_eq!(line_content_hash(&rope.slice_line(1)), before);
        let mut pieced = Rope::new();
        for piece in long.as_bytes().chunks(7) {
            let end = pieced.len();
            pieced.insert(end, std::str::from_utf8(piece).unwrap());
        }
        assert_eq!(line_content_hash(&pieced.slice(0..pieced.len())), before);
        let at = rope.line_start_offset(1) + 2500;
        rope.insert(at, "y");
        assert_ne!(line_content_hash(&rope.slice_line(1)), before);
    }

    #[test]
    fn the_key_level_eases_on_a_flip_and_lands_without_asking_for_more() {
        use std::time::Duration;
        let t0 = std::time::Instant::now();
        let mut ease = KeyEase::at(true);
        assert_eq!(ease.step(true, t0), (1., false));
        // Losing key runs over KEY_OUT, strictly falling.
        let first = ease.step(false, t0);
        assert_eq!(first, (1., true));
        let mid = ease.step(false, t0 + Duration::from_millis(100));
        assert!(mid.1 && mid.0 < 1. && mid.0 > 0.);
        let end = ease.step(false, t0 + Duration::from_secs_f32(KEY_OUT));
        assert_eq!(end, (0., false));
        assert_eq!(ease.step(false, t0 + Duration::from_secs(5)), (0., false));
        // Gaining key runs over KEY_IN.
        let t1 = t0 + Duration::from_secs(6);
        ease.step(true, t1);
        assert!(ease.step(true, t1 + Duration::from_millis(60)).1);
        assert_eq!(ease.step(true, t1 + Duration::from_secs_f32(KEY_IN)), (1., false));
    }

    #[test]
    fn a_flip_mid_fade_turns_around_from_the_drawn_level() {
        use std::time::Duration;
        let t0 = std::time::Instant::now();
        let mut ease = KeyEase::at(true);
        ease.step(false, t0);
        let drawn = ease.step(false, t0 + Duration::from_millis(80)).0;
        let turned = ease.step(true, t0 + Duration::from_millis(80));
        assert!((turned.0 - drawn).abs() < 1e-3);
        assert!(turned.1);
        let later = ease.step(true, t0 + Duration::from_millis(120)).0;
        assert!(later > drawn && later < 1.);
    }

    #[test]
    fn a_fill_that_differs_by_key_mixes_between_its_looks() {
        let grey = Hsla { h: 0., s: 0., l: 0.5, a: 0.1 };
        let blue = Hsla { h: 0.6, s: 1., l: 0.5, a: 0.4 };
        let look = |key: bool| if key { blue } else { grey };
        assert_eq!(key_fill(1., look), blue);
        assert_eq!(key_fill(0., look), grey);
        let half = key_fill(0.5, look);
        assert!(half.a > 0.1 && half.a < 0.4);
    }

    #[test]
    fn line_number_column_stays_at_three_digits_then_grows_up_to_seven() {
        assert_eq!(line_number_len(1), 3);
        assert_eq!(line_number_len(9), 3);
        assert_eq!(line_number_len(10), 3);
        assert_eq!(line_number_len(999), 3);
        assert_eq!(line_number_len(1_000), 4);
        assert_eq!(line_number_len(999_999), 6);
        assert_eq!(line_number_len(9_999_999), 7);
        assert_eq!(line_number_len(10_000_000), 7);
    }

    #[test]
    fn displayed_line_number_stays_within_seven_digits() {
        assert_eq!(displayed_line_number(42), 42);
        assert_eq!(displayed_line_number(9_999_999), 9_999_999);
        assert_eq!(displayed_line_number(10_000_000), 9_999_999);
    }

    struct DecorationHarness(Entity<EditorState>);

    impl Render for DecorationHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    fn decoration_editor(
        cx: &mut TestAppContext,
        text: &str,
        wrap: bool,
    ) -> (Entity<EditorState>, gpui::WindowHandle<DecorationHarness>) {
        cx.update(crate::init);
        let mut editor = None;
        let window = cx.open_window(size(px(240.), px(140.)), |window, cx| {
            let state = cx.new(|cx| {
                EditorState::new(window, cx)
                    .soft_wrap(wrap)
                    .folding(true)
                    .default_value(text)
            });
            editor = Some(state.clone());
            DecorationHarness(state)
        });
        (editor.unwrap(), window)
    }

    #[gpui::test]
    fn search_scope_paths_over_sixty_blink_frames(cx: &mut TestAppContext) {
        let (editor, window) = decoration_editor(cx, "foo\noutside\nfoo", false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            editor.update(cx, |state, cx| {
                state.set_selected_range(0..3, cx);
                state.add_selection(12..15, cx);
                state.set_search_query("foo", true, cx);
                state.set_search_in_selection(true, cx);
                state.set_selected_range(1..1, cx);
            });
            let mut first = None;
            for _ in 0..60 {
                editor.update(cx, |_, cx| cx.notify());
                window.draw(cx).clear(cx);
                let paths = editor.read(cx).search_scope_cache.borrow().as_ref()
                    .expect("scope geometry").paths.clone();
                assert_eq!(paths.len(), 2);
                if let Some(first) = &first {
                    assert!(Rc::ptr_eq(first, &paths));
                } else {
                    first = Some(paths);
                }
            }
            editor.update(cx, |state, cx| {
                state.release_layout();
                assert!(state.search_scope_cache.borrow().is_none());
                cx.notify();
            });
            window.draw(cx).clear(cx);
            let refill = editor.read(cx).search_scope_cache.borrow().as_ref().unwrap().paths.clone();
            assert!(!Rc::ptr_eq(first.as_ref().unwrap(), &refill));
            editor.update(cx, |state, cx| state.set_search_in_selection(false, cx));
            window.draw(cx).clear(cx);
            assert!(editor.read(cx).search_scope_cache.borrow().is_none());
        });
    }

    /// A hidden tab's `release_layout` drops the highlight styles with the shaped lines, and the
    /// next draw asks the highlighter once for its screen; a plain redraw while showing reuses
    /// the answer (finch#r7qz) and runs no query.
    #[gpui::test]
    fn release_layout_drops_the_highlight_cache_and_the_next_draw_refills_it(
        cx: &mut TestAppContext,
    ) {
        use crate::input::{HighlightStyleResolver, InputEdit, InputHighlighter};
        use gpui::{Context, HighlightStyle, SharedString};
        use std::cell::Cell;

        struct Counting(Rc<Cell<usize>>);

        impl InputHighlighter for Counting {
            fn language(&self) -> SharedString {
                "counting".into()
            }

            fn update(
                &mut self,
                _: Option<InputEdit>,
                _: &Rope,
                _: bool,
                _: &mut Window,
                _: &mut Context<EditorState>,
            ) {
            }

            fn styles(
                &self,
                range: &Range<usize>,
                _: &dyn HighlightStyleResolver,
            ) -> Vec<(Range<usize>, HighlightStyle)> {
                self.0.set(self.0.get() + 1);
                vec![(range.clone(), HighlightStyle::default())]
            }

            fn fold_ranges(&self, _: &Rope) -> Vec<FoldRange> {
                Vec::new()
            }

            fn syntax_generation(&self) -> Option<u64> {
                Some(1)
            }
        }

        let queries = Rc::new(Cell::new(0));
        let (editor, window) = decoration_editor(cx, "one\ntwo\nthree", false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let counter = queries.clone();
            editor.update(cx, |state, cx| {
                state.set_highlighter_factory(
                    Rc::new(move |_: &str| {
                        let highlighter: Box<dyn InputHighlighter> = Box::new(Counting(counter.clone()));
                        Some(highlighter)
                    }),
                    cx,
                );
            });
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
        });
        let drawn = queries.get();
        assert!(drawn >= 1);
        cx.update(|window, cx| {
            editor.update(cx, |_, cx| cx.notify());
            window.draw(cx).clear(cx);
        });
        assert_eq!(queries.get(), drawn, "a redraw while showing reuses the cache");

        cx.update(|window, cx| {
            editor.update(cx, |state, _| state.release_layout());
            assert!(editor.read(cx).highlight_cache.borrow().is_none());
            assert!(editor.read(cx).last_layout.as_ref().unwrap().lines.is_empty());
            window.draw(cx).clear(cx);
            assert!(editor.read(cx).highlight_cache.borrow().is_some());
        });
        assert_eq!(queries.get(), drawn + 1, "a return asks the highlighter once");
    }

    /// The focus dim moves with the caret by replacing two `fade_out` decorations: the highlighter
    /// is asked nothing for it, and the dim never touches the run colors (finch#52yc).
    #[gpui::test]
    fn moving_the_focus_dim_runs_no_highlighter_query(cx: &mut TestAppContext) {
        use crate::input::{HighlightStyleResolver, InputEdit, InputHighlighter, TextDecoration};
        use gpui::{Context, HighlightStyle, SharedString};
        use std::cell::Cell;

        struct Counting(Rc<Cell<usize>>);

        impl InputHighlighter for Counting {
            fn language(&self) -> SharedString {
                "counting".into()
            }

            fn update(
                &mut self,
                _: Option<InputEdit>,
                _: &Rope,
                _: bool,
                _: &mut Window,
                _: &mut Context<EditorState>,
            ) {
            }

            fn styles(
                &self,
                range: &Range<usize>,
                _: &dyn HighlightStyleResolver,
            ) -> Vec<(Range<usize>, HighlightStyle)> {
                self.0.set(self.0.get() + 1);
                vec![(range.clone(), HighlightStyle::default())]
            }

            fn fold_ranges(&self, _: &Rope) -> Vec<FoldRange> {
                Vec::new()
            }

            fn syntax_generation(&self) -> Option<u64> {
                Some(1)
            }
        }

        let queries = Rc::new(Cell::new(0));
        let (editor, window) = decoration_editor(cx, "one\ntwo\n\nthree\nfour", false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let counter = queries.clone();
            editor.update(cx, |state, cx| {
                state.set_highlighter_factory(
                    Rc::new(move |_: &str| {
                        let highlighter: Box<dyn InputHighlighter> = Box::new(Counting(counter.clone()));
                        Some(highlighter)
                    }),
                    cx,
                );
            });
            let dim = |range: Range<usize>| {
                TextDecoration::new(range, HighlightStyle { fade_out: Some(0.67), ..Default::default() })
            };
            let collection = editor.update(cx, |state, cx| {
                state.create_decorations_collection(vec![dim(9..18)], cx)
            });
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
            let drawn = queries.get();
            assert!(drawn >= 1);
            // The caret crosses the paragraph boundary: the unit is now the first paragraph.
            collection.set(vec![dim(8..18)], cx);
            window.draw(cx).clear(cx);
            collection.set(vec![dim(0..7), dim(15..18)], cx);
            window.draw(cx).clear(cx);
            assert_eq!(queries.get(), drawn, "a dim set runs no highlighter query");
        });
    }

    /// With wrap off, typing on a row other than the longest shapes nothing and hashes the line
    /// once; a repaint at rest hashes nothing; typing on the longest row itself re-measures
    /// (finch#kwn8).
    #[gpui::test]
    fn typing_off_the_longest_row_keeps_its_width(cx: &mut TestAppContext) {
        let long = "x".repeat(400);
        let (editor, window) = decoration_editor(cx, &format!("a\n{long}\nb"), false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let counts = || {
            (
                LONGEST_LINE_HASHES.with(|hashes| hashes.get()),
                LONGEST_LINE_SHAPES.with(|shapes| shapes.get()),
            )
        };
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let settled = counts();
            for _ in 0..20 {
                editor.update(cx, |_, cx| cx.notify());
                window.draw(cx).clear(cx);
            }
            assert_eq!(counts(), settled, "a repaint at rest hashes and shapes nothing");

            for _ in 0..200 {
                editor.update(cx, |state, cx| {
                    state.set_selected_range(0..0, cx);
                    state.replace_text_in_range(None, "z", window, cx);
                });
                window.draw(cx).clear(cx);
            }
            let (hashes, shapes) = counts();
            assert_eq!(shapes, settled.1, "200 keys on another row shape nothing");
            assert_eq!(hashes, settled.0 + 200, "one hash per key, none per repaint");

            let at = 202 + 100;
            editor.update(cx, |state, cx| {
                state.set_selected_range(at..at, cx);
                state.replace_text_in_range(None, "y", window, cx);
            });
            window.draw(cx).clear(cx);
            assert_eq!(counts().1, shapes + 1, "a key on the longest row re-measures it");
        });
    }

    /// A list as tall as the window, whose second item is an input that
    /// starts 180 px down, so its bottom is cut off by the window's edge.
    struct RevealHarness {
        input: Entity<crate::input::InputState>,
        list: ListState,
        show_input: bool,
    }

    impl Render for RevealHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let input = self.input.clone();
            let show_input = self.show_input;
            list(self.list.clone(), move |ix, _, _| {
                if ix == 0 {
                    div().h(px(180.)).into_any_element()
                } else {
                    div()
                        .when(show_input, |this| this.child(input.clone()))
                        .into_any_element()
                }
            })
            .size_full()
        }
    }

    #[gpui::test]
    fn focused_input_asks_its_scroll_container_to_reveal_it(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let list_state = ListState::new(2, ListAlignment::Top, px(0.));
        let mut input = None;
        let window = cx.open_window(size(px(240.), px(200.)), |window, cx| {
            let state = cx.new(|cx| crate::input::InputState::new(window, cx));
            input = Some(state.clone());
            RevealHarness {
                input: state,
                list: list_state.clone(),
                show_input: true,
            }
        });
        let input = input.unwrap();
        let view = window.root(cx).unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, _| window.activate_window());
        cx.run_until_parked();
        let draw = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                window.refresh();
                window.draw(cx).clear(cx);
            })
        };
        let revealed = |cx: &mut VisualTestContext| {
            cx.update(|window, cx| {
                let bounds = input.read(cx).input_bounds();
                bounds.top() >= px(0.) && bounds.bottom() <= window.viewport_size().height
            })
        };
        let at_top = || {
            list_state.logical_scroll_top().item_ix == 0
                && list_state.logical_scroll_top().offset_in_item == px(0.)
        };

        draw(&mut cx);
        assert!(at_top());
        assert!(!revealed(&mut cx));

        // Gaining focus reveals the input.
        cx.update(|window, cx| input.update(cx, |state, cx| state.focus(window, cx)));
        draw(&mut cx);
        draw(&mut cx);
        assert!(!at_top());
        assert!(revealed(&mut cx));

        // Scrolling away while focused is left alone.
        list_state.scroll_to(ListOffset::default());
        draw(&mut cx);
        draw(&mut cx);
        assert!(at_top());

        // A temporarily unmounted input does not prepaint while unfocused.
        // Its blur event must still reset the reveal cache before remounting.
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.show_input = false;
                window.blur(cx);
                cx.notify();
            });
        });
        draw(&mut cx);
        input.read_with(&cx, |state, _| assert!(state.revealed_viewport.is_none()));
        cx.update(|window, cx| {
            view.update(cx, |view, cx| {
                view.show_input = true;
                input.update(cx, |state, cx| state.focus(window, cx));
                cx.notify();
            });
        });
        draw(&mut cx);
        draw(&mut cx);
        assert!(!at_top());
        assert!(revealed(&mut cx));

        list_state.scroll_to(ListOffset::default());
        draw(&mut cx);
        draw(&mut cx);
        assert!(at_top());

        // A smaller visible area (an on-screen keyboard) reveals it again.
        cx.simulate_resize(size(px(240.), px(195.)));
        draw(&mut cx);
        draw(&mut cx);
        assert!(!at_top());
        assert!(revealed(&mut cx));
    }

    struct TextareaHarness(Entity<crate::input::TextareaState>);

    #[gpui::test]
    fn scroll_bounce_keeps_the_scrollbar_viewport_fixed(cx: &mut TestAppContext) {
        let text = (0..60)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (editor, window) = decoration_editor(cx, &text, false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let viewport = editor
                .read(cx)
                .editor_scrollbar_snapshot
                .get()
                .unwrap()
                .layout
                .bounds;
            let text_origin = editor.read(cx).last_bounds.unwrap().origin;
            editor.update(cx, |state, cx| {
                state.set_scroll_bounce(true, cx);
                state.on_scroll_wheel(
                    &gpui::ScrollEvent {
                        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(60.))),
                        touch_phase: gpui::TouchPhase::Started,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
            });
            window.draw(cx).clear(cx);
            let state = editor.read(cx);
            let stretch = state.overscroll.offset();
            assert!(stretch > 0.);
            assert_eq!(
                state.editor_scrollbar_snapshot.get().unwrap().layout.bounds,
                viewport
            );
            assert_eq!(
                state.last_bounds.unwrap().origin.y,
                text_origin.y + px(stretch)
            );
        });
    }

    /// A scrollbar supplied through `set_scrollbar_renderer` gets the editor's own
    /// scroll range through the standard ScrollbarHandle contract, so a
    /// scrollbar can drag to the same end as wheel scrolling.
    #[gpui::test]
    fn scrollbar_renderer_receives_the_editors_scroll_range(cx: &mut TestAppContext) {
        use crate::ScrollbarHandle as _;

        let text = (0..60)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (editor, window) = decoration_editor(cx, &text, true);
        let presented = std::rc::Rc::new(std::cell::RefCell::new(None));
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            editor.update(cx, |state, cx| {
                state.set_scroll_bounce(false, cx);
                state.set_editor_paddings(Edges {
                    top: px(10.),
                    bottom: px(10.),
                    ..Default::default()
                });
                let presented = presented.clone();
                state.set_scrollbar_renderer(Some(std::rc::Rc::new(move |context, _, _| {
                    *presented.borrow_mut() = Some(context.clone());
                    Scrollbar::vertical(&context)
                        .mode(crate::ScrollbarMode::Always)
                        .into_any_element()
                })));
            });
            window.draw(cx).clear(cx);
            editor.update(cx, |state, cx| {
                state.on_scroll_wheel(
                    &gpui::ScrollEvent {
                        delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-10_000.))),
                        touch_phase: gpui::TouchPhase::Moved,
                        ..Default::default()
                    },
                    window,
                    cx,
                );
            });
            window.draw(cx).clear(cx);
        });

        let context = presented.borrow().clone().unwrap();
        let track = context.viewport_bounds();
        let wheel_end = context.offset();
        assert!(wheel_end.y < px(0.));
        assert_eq!(
            track.size.height - context.scroll_viewport_size().height,
            px(20.)
        );
        cx.update(|window, cx| {
            context.set_offset(Point::default());
            window.refresh();
            window.draw(cx).clear(cx);
        });

        let grab = point(track.right() - px(5.), track.top() + px(10.));
        let end = point(grab.x, track.bottom() - px(1.));
        cx.simulate_mouse_down(grab, gpui::MouseButton::Left, gpui::Modifiers::default());
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_mouse_move(
            end,
            Some(gpui::MouseButton::Left),
            gpui::Modifiers::default(),
        );
        cx.update(|window, cx| window.draw(cx).clear(cx));
        cx.simulate_mouse_up(end, gpui::MouseButton::Left, gpui::Modifiers::default());

        assert_eq!(
            editor.read_with(&cx, |state, _| state.scroll_offset()),
            wheel_end
        );
        assert_eq!(
            context.content_size().height - track.size.height,
            -wheel_end.y
        );
    }

    impl Render for TextareaHarness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    /// A textarea 100 px down a taller bouncing scroll area.
    struct NestedTextarea(Entity<crate::input::TextareaState>, gpui::ScrollHandle);
    use gpui::StatefulInteractiveElement as _;

    impl Render for NestedTextarea {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            crate::ScrollBounce::new(
                "outer",
                &self.1,
                div()
                    .id("outer-viewport")
                    .w(px(240.))
                    .h(px(300.))
                    .overflow_y_scroll()
                    .track_scroll(&self.1)
                    .child(div().h(px(100.)))
                    .child(div().h(px(120.)).child(self.0.clone()))
                    .child(div().h(px(600.))),
            )
            .enabled(true)
        }
    }

    #[gpui::test]
    fn a_textarea_keeps_its_scroll_from_the_area_around_it(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let text = (1..=40)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let outer = gpui::ScrollHandle::new();
        let mut textarea = None;
        let window = cx.open_window(size(px(240.), px(400.)), |window, cx| {
            let state =
                cx.new(|cx| crate::input::TextareaState::new(window, cx).default_value(text));
            textarea = Some(state.clone());
            NestedTextarea(state, outer.clone())
        });
        let textarea = textarea.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let swipe = |cx: &mut VisualTestContext, y: f32| {
            cx.simulate_event(gpui::ScrollEvent {
                position: point(px(100.), px(y)),
                delta: gpui::ScrollDelta::Pixels(point(px(0.), px(-40.))),
                touch_phase: gpui::TouchPhase::Moved,
                ..Default::default()
            });
            cx.update(|window, cx| window.draw(cx).clear(cx));
        };
        swipe(&mut cx, 160.);
        let scrolled = cx.update(|_, cx| textarea.read(cx).scroll_handle.offset().y);
        assert!(scrolled < px(0.), "the textarea scrolls");
        assert_eq!(outer.offset().y, px(0.), "the area around it does not");
        swipe(&mut cx, 40.);
        assert_eq!(outer.offset().y, px(-40.), "outside the textarea it does");
    }

    #[gpui::test]
    fn set_scroll_offset_past_the_end_is_clamped(cx: &mut TestAppContext) {
        cx.update(crate::init);
        let text = (1..=40)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut textarea = None;
        let window = cx.open_window(size(px(240.), px(400.)), |window, cx| {
            let state =
                cx.new(|cx| crate::input::TextareaState::new(window, cx).default_value(text));
            textarea = Some(state.clone());
            TextareaHarness(state)
        });
        let textarea = textarea.unwrap();
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            textarea.update(cx, |state, cx| {
                state.set_scroll_offset(point(px(0.), px(-10_000.)), cx)
            });
            window.draw(cx).clear(cx);

            let state = textarea.read(cx);
            let painted = state
                .editor_scrollbar_snapshot
                .get()
                .unwrap()
                .cursor_scroll_offset;
            let furthest = (state.input_bounds.size.height - state.scroll_size.height).min(px(0.));
            assert!(furthest < px(0.));
            assert_eq!(painted.y, furthest);

            let layout = state.last_layout.as_ref().unwrap();
            let rows = viewport_visible_lines(state.input_bounds.size.height, layout.line_height);
            assert!(layout.visible_range.len() >= rows);
        });
    }

    #[gpui::test]
    fn set_scroll_offset_past_the_end_is_clamped_in_a_code_editor(cx: &mut TestAppContext) {
        let text = (1..=40)
            .map(|line| format!("line {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        let (editor, window) = decoration_editor(cx, &text, false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            editor.update(cx, |state, cx| {
                state.set_scroll_offset(point(px(0.), px(-10_000.)), cx)
            });
            window.draw(cx).clear(cx);

            let state = editor.read(cx);
            let painted = state
                .editor_scrollbar_snapshot
                .get()
                .unwrap()
                .cursor_scroll_offset;
            // The furthest offset includes the room below the last line.
            let furthest = state.input_bounds.size.height - state.scroll_size.height;
            assert!(-furthest > state.input_bounds.size.height);
            assert_eq!(painted.y, furthest);
        });
    }

    #[gpui::test]
    fn selection_paths_follow_the_input_and_its_menu_focus(cx: &mut TestAppContext) {
        let (editor, window) = decoration_editor(cx, "alpha beta", false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let menu_focus = cx.focus_handle();
            let other_focus = cx.focus_handle();
            editor.update(cx, |state, cx| {
                state.set_selected_range(0..5, cx);
                state.focus(window, cx);
            });

            let selection_paths = |window: &mut Window, cx: &mut App| {
                window.draw(cx).clear(cx);
                let state = editor.read(cx);
                let layout = state.last_layout.clone().unwrap();
                let mut bounds = state.input_bounds;
                TextElement::new(editor.clone()).layout_selections(&layout, &mut bounds, window, cx)
            };
            let focused_paths = selection_paths(window, cx).len();
            assert!(focused_paths > 0);

            editor.update(cx, |state, cx| {
                state.set_selection_focus(Some(menu_focus.clone()), cx);
            });
            menu_focus.focus(window, cx);
            assert!(!editor.read(cx).focus_handle.is_focused(window));
            assert_eq!(selection_paths(window, cx).len(), focused_paths);
            // The opening frame can still use the input's old focus. Check
            // another frame with the menu already focused as well.
            assert_eq!(selection_paths(window, cx).len(), focused_paths);

            other_focus.focus(window, cx);
            assert!(selection_paths(window, cx).is_empty());
            assert_eq!(editor.read(cx).selected_range(), 0..5);

            menu_focus.focus(window, cx);
            editor.update(cx, |state, cx| state.set_selection_focus(None, cx));
            assert!(selection_paths(window, cx).is_empty());
            editor.update(cx, |state, cx| state.focus(window, cx));
            assert_eq!(selection_paths(window, cx).len(), focused_paths);

            // Retaining the input must not keep a removed menu alive.
            let weak_menu = menu_focus.downgrade();
            editor.update(cx, |state, cx| {
                state.set_selection_focus(Some(menu_focus.clone()), cx);
            });
            menu_focus.focus(window, cx);
            drop(menu_focus);
            assert!(weak_menu.upgrade().is_none());
            assert!(selection_paths(window, cx).is_empty());
        });
    }

    #[gpui::test]
    fn horizontal_scroll_is_clamped_before_text_and_caret_layout(cx: &mut TestAppContext) {
        let (editor, window) = decoration_editor(cx, "short text", false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let state = editor.read(cx);
            let layout = state.last_layout.clone().unwrap();
            let input_bounds = state.input_bounds;
            let scroll_size = input_bounds.size;
            let element = TextElement::new(editor.clone());
            let mut bounds = input_bounds;
            let (baseline, _, _, _) =
                element.layout_cursors(&layout, &mut bounds, scroll_size, window, cx);

            // Both a stale offset after deletion and a deferred scroll target must
            // be limited before either text or carets are positioned this frame.
            for deferred in [None, Some(point(px(-80.), px(0.)))] {
                editor.update(cx, |state, _| {
                    state.scroll_handle.set_offset(point(px(-40.), px(0.)));
                    state.deferred_scroll_offset = deferred;
                });
                let mut bounds = input_bounds;
                let (carets, offset, _, _) =
                    element.layout_cursors(&layout, &mut bounds, scroll_size, window, cx);
                assert_eq!(offset.x, px(0.));
                assert_eq!(bounds.origin.x, input_bounds.origin.x);
                assert_eq!(carets.len(), baseline.len());
                for (caret, original) in carets.iter().zip(&baseline) {
                    assert_eq!(caret.bounds.origin.x, original.bounds.origin.x);
                }
                editor.update(cx, |state, cx| {
                    state.scroll_size = scroll_size;
                    state.update_scroll_offset(Some(offset), cx);
                    assert_eq!(state.scroll_handle.offset().x, offset.x);
                });
            }
        });
    }

    /// Scroll to the far right and assert the caret at `end` lands inside the
    /// viewport, so the line ending there can be revealed completely.
    fn assert_horizontal_scroll_reaches_line_end(
        editor: &Entity<EditorState>,
        end: usize,
        window: &mut Window,
        cx: &mut App,
    ) {
        let (layout, input_bounds, scroll_size) = {
            let state = editor.read(cx);
            (
                state.last_layout.clone().expect("layout"),
                state.input_bounds,
                state.scroll_size,
            )
        };

        editor.update(cx, |state, cx| {
            state.set_selected_range(end..end, cx);
            state.deferred_scroll_offset = Some(point(px(-1_000_000.), px(0.)));
        });

        let element = TextElement::new(editor.clone());
        let mut bounds = input_bounds;
        let (carets, offset, _, _) =
            element.layout_cursors(&layout, &mut bounds, scroll_size, window, cx);
        let caret = carets.first().expect("a caret").bounds;
        assert!(
            caret.left() <= input_bounds.right() + px(1.),
            "scrolling to the end ({offset:?}) leaves the caret at {caret:?} past {:?}",
            input_bounds.right()
        );
    }

    #[gpui::test]
    fn horizontal_scroll_extent_excludes_placeholder(cx: &mut TestAppContext) {
        let (editor, window) = decoration_editor(cx, "", false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let empty_width = editor.read(cx).scroll_size.width;

            for placeholder in ["W".repeat(300), "Short hint".to_string()] {
                editor.update(cx, |state, cx| {
                    state.set_placeholder(placeholder, window, cx);
                });
                window.draw(cx).clear(cx);
                assert_eq!(
                    editor.read(cx).scroll_size.width,
                    empty_width,
                    "placeholder must not create horizontal scrollable content"
                );
            }
        });
    }

    #[gpui::test]
    fn horizontal_scroll_extent_covers_the_widest_laid_out_line(cx: &mut TestAppContext) {
        // The first line is longest by bytes, but the second renders wider.
        let narrow = format!("e{}", "\u{301}".repeat(200));
        let wide = "W".repeat(300);
        let text = format!("{narrow}\n{wide}");
        let (editor, window) = decoration_editor(cx, &text, false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            assert_horizontal_scroll_reaches_line_end(&editor, text.len(), window, cx);

            // Editing the wider line invalidates the cached measurement.
            let wider = "W".repeat(400);
            let edited = format!("{narrow}\n{wider}");
            editor.update(cx, |state, cx| state.set_value(edited.as_str(), window, cx));
            window.draw(cx).clear(cx);
            assert_horizontal_scroll_reaches_line_end(&editor, edited.len(), window, cx);

            // A larger font must widen the extent instead of reusing the old
            // measurement.
            let before = editor.read(cx).scroll_size.width;
            window.set_rem_size(px(24.));
            window.draw(cx).clear(cx);
            let after = editor.read(cx).scroll_size.width;
            assert!(
                after > before,
                "growing the font should widen the scroll extent: {after} > {before}"
            );
            assert_horizontal_scroll_reaches_line_end(&editor, edited.len(), window, cx);
        });
    }

    #[gpui::test]
    fn aligned_horizontal_scroll_clamps_deferred_targets(cx: &mut TestAppContext) {
        let (editor, window) = decoration_editor(cx, &"wide text ".repeat(80), false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let mut layout = editor.read(cx).last_layout.clone().unwrap();
            let input_bounds = editor.read(cx).input_bounds;
            let element = TextElement::new(editor.clone());

            for alignment in [TextAlign::Left, TextAlign::Right, TextAlign::Center] {
                layout.text_align = alignment;
                for overflow in [px(0.), px(120.)] {
                    let scroll_size =
                        size(input_bounds.size.width + overflow, input_bounds.size.height);
                    let minimum = if alignment == TextAlign::Left {
                        -overflow
                    } else {
                        -overflow - CURSOR_WIDTH
                    };
                    editor.update(cx, |state, _| {
                        state.text_align = alignment;
                        state.last_selected_range = Some(*state.active_selection());
                        state.deferred_scroll_offset = Some(point(minimum, px(0.)));
                    });
                    let mut bounds = input_bounds;
                    let (expected, _, _, _) =
                        element.layout_cursors(&layout, &mut bounds, scroll_size, window, cx);

                    for target in [minimum - px(100.), px(100.)] {
                        editor.update(cx, |state, _| {
                            state.deferred_scroll_offset = Some(point(target, px(0.)));
                        });
                        let mut bounds = input_bounds;
                        let (carets, offset, _, _) =
                            element.layout_cursors(&layout, &mut bounds, scroll_size, window, cx);
                        let expected_offset = if target < minimum { minimum } else { px(0.) };
                        assert_eq!(offset.x, expected_offset);
                        assert_eq!(bounds.left(), input_bounds.left() + expected_offset);
                        if target < minimum {
                            assert_eq!(carets[0].bounds, expected[0].bounds);
                        }
                        if alignment == TextAlign::Right {
                            assert!(carets[0].bounds.right() <= input_bounds.right());
                        }
                        editor.update(cx, |state, cx| {
                            state.scroll_size = scroll_size;
                            state.update_scroll_offset(Some(offset), cx);
                            assert_eq!(state.scroll_handle.offset().x, expected_offset);
                        });
                    }
                }
            }
        });
    }

    #[gpui::test]
    fn deferred_horizontal_scroll_moves_every_caret(cx: &mut TestAppContext) {
        use super::super::{cursor::CursorSelection, selection::CursorId};

        let (editor, window) = decoration_editor(cx, &"wide text ".repeat(80), false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let layout = editor.read(cx).last_layout.clone().unwrap();
            let input_bounds = editor.read(cx).input_bounds;
            let scroll_size = size(input_bounds.size.width + px(120.), input_bounds.size.height);
            let element = TextElement::new(editor.clone());

            for offsets in [[0, 2, 4], [0, 4, 2], [4, 0, 2]] {
                editor.update(cx, |state, _| {
                    state.selections.replace_all(
                        offsets
                            .into_iter()
                            .enumerate()
                            .map(|(id, offset)| {
                                CursorSelection::new(CursorId::new(id), offset, offset)
                            })
                            .collect(),
                    );
                    state.last_selected_range = Some(*state.active_selection());
                    state.scroll_handle.set_offset(point(px(0.), px(0.)));
                    state.deferred_scroll_offset = None;
                });
                let mut bounds = input_bounds;
                let (baseline, _, _, _) =
                    element.layout_cursors(&layout, &mut bounds, scroll_size, window, cx);
                editor.update(cx, |state, _| {
                    state.deferred_scroll_offset = Some(point(px(-60.), px(0.)));
                });
                let mut bounds = input_bounds;
                let (carets, offset, _, _) =
                    element.layout_cursors(&layout, &mut bounds, scroll_size, window, cx);
                assert_eq!(offset.x, px(-60.));
                assert_eq!(carets.len(), 3);
                for (caret, original) in carets.iter().zip(baseline) {
                    assert_eq!(caret.bounds.left(), original.bounds.left() - px(60.));
                }
            }
        });
    }

    #[gpui::test]
    fn editor_line_number_gutter_resizes_with_document_lines(cx: &mut TestAppContext) {
        let (editor, window) = decoration_editor(cx, &"x\n".repeat(8), false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let narrow = editor
                .read(cx)
                .last_layout
                .as_ref()
                .unwrap()
                .line_number_width;

            let longer_text = "x\n".repeat(99);
            editor.update(cx, |state, cx| {
                state.set_value(longer_text.as_str(), window, cx)
            });
            window.draw(cx).clear(cx);
            let middle = editor
                .read(cx)
                .last_layout
                .as_ref()
                .unwrap()
                .line_number_width;
            assert_eq!(middle, narrow, "one to three digits must share a width");

            let longest_text = "x\n".repeat(999);
            editor.update(cx, |state, cx| {
                state.set_value(longest_text.as_str(), window, cx)
            });
            window.draw(cx).clear(cx);
            let start = editor
                .read(cx)
                .last_layout
                .as_ref()
                .unwrap()
                .line_number_width;
            assert_eq!(start, narrow, "the text is first drawn where it was");

            cx.background_executor()
                .advance_clock(std::time::Duration::from_secs(1));
            window.draw(cx).clear(cx);
            let wide = editor
                .read(cx)
                .last_layout
                .as_ref()
                .unwrap()
                .line_number_width;

            assert!(
                wide > narrow,
                "more line-number digits must widen the gutter"
            );
        });
    }

    #[gpui::test]
    fn gutter_right_click_runs_the_gutter_handler_with_numbers_shown_or_hidden(cx: &mut TestAppContext) {
        use std::{cell::RefCell, rc::Rc};
        let (editor, window) = decoration_editor(cx, &"x\n".repeat(8), false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        let seen = Rc::new(RefCell::new(Vec::new()));
        cx.update(|window, cx| {
            let seen = seen.clone();
            editor.update(cx, |state, _| state.on_gutter_menu(Rc::new(move |at, _, _| seen.borrow_mut().push(at))));
            window.draw(cx).clear(cx);
        });
        let (origin, gutter, line) = editor.read_with(&cx, |state, _| {
            let layout = state.last_layout.as_ref().unwrap();
            (state.input_bounds.origin, layout.line_number_width, layout.line_height)
        });
        let in_gutter = origin + point(gutter / 2., line * 1.5);
        let in_text = origin + point(gutter + px(40.), line * 1.5);
        let right = |cx: &mut VisualTestContext, at| cx.simulate_mouse_down(at, gpui::MouseButton::Right, gpui::Modifiers::default());

        right(&mut cx, in_gutter);
        cx.run_until_parked();
        assert_eq!(*seen.borrow(), vec![in_gutter], "a right click on the numbers opens the gutter menu");

        right(&mut cx, in_text);
        cx.run_until_parked();
        assert_eq!(seen.borrow().len(), 1, "a right click in the text leaves the gutter alone");

        // Numbers hidden the way View > Show line numbers does it: the fold arrows' column and the margin still take the click.
        cx.update(|window, cx| editor.update(cx, |state, cx| state.set_line_number(false, window, cx)));
        cx.executor().advance_clock(std::time::Duration::from_secs(1));
        cx.update(|window, cx| window.draw(cx).clear(cx));
        let origin = editor.read_with(&cx, |state, _| state.input_bounds.origin);
        for x in [px(1.), px(5.), px(10.)] {
            let in_gutter = origin + point(x, line * 1.5);
            right(&mut cx, in_gutter);
            cx.run_until_parked();
            assert_eq!(seen.borrow().last(), Some(&in_gutter), "with the numbers hidden the gutter that is left still takes it at {x:?}");
        }
    }

    #[gpui::test]
    fn geometric_decorations_clip_scrolled_viewport_and_cull_offscreen_ranges(
        cx: &mut TestAppContext,
    ) {
        let text = "abcdefghij\n".repeat(100);
        let (editor, window) = decoration_editor(cx, &text, false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            editor.update(cx, |state, cx| {
                state.create_range_decorations_collection(
                    vec![
                        RangeDecoration::new(0..text.len()).with_style(RangeDecorationStyle::Fill),
                        RangeDecoration::new(0..text.len()),
                        RangeDecoration::new(0..3),
                        RangeDecoration::new(text.len() - 3..text.len()),
                    ],
                    cx,
                );
            });
            window.draw(cx).clear(cx);
            editor.update(cx, |state, cx| {
                let height = state.last_layout.as_ref().unwrap().line_height;
                state.set_scroll_offset(point(px(0.), -height * 40.), cx);
            });
            // Deferred scroll is committed by prepaint and reflected next frame.
            window.draw(cx).clear(cx);
            window.draw(cx).clear(cx);
            let state = editor.read(cx);
            let layout = state.last_layout.as_ref().unwrap();
            assert!(layout.visible_range_offset.start > 0);
            assert!(layout.visible_range_offset.end < text.len());
            let all =
                TextElement::<EditorMode>::layout_range_corners(&(0..text.len()), layout).unwrap();
            let clipped = TextElement::<EditorMode>::layout_range_corners(
                &layout.visible_range_offset,
                layout,
            )
            .unwrap();
            assert_eq!(all, clipped);
            assert!(!all.is_empty());
            assert!(TextElement::<EditorMode>::layout_range_corners(&(0..3), layout).is_none());
            // Also execute the production path building seam, not just the corner helper.
            let (fills, frames) = TextElement::new(editor.clone()).layout_range_decorations(
                layout,
                &state.input_bounds,
                window,
                state.input_bounds,
                cx,
            );
            assert_eq!(fills.len(), 1);
            assert_eq!(frames.len(), 1);
        });
    }

    #[gpui::test]
    fn geometric_decorations_use_shaped_wrap_boundaries_and_newline_cells(cx: &mut TestAppContext) {
        let text = "    héllo world héllo world héllo world héllo world\n\nlast";
        let (editor, window) = decoration_editor(cx, text, true);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            window.draw(cx).clear(cx);
            let state = editor.read(cx);
            let layout = state.last_layout.as_ref().unwrap();
            let first = &layout.lines[0];
            assert!(first.wrapped_lines.len() > 1);
            assert!(first.wrap_indent > px(0.));
            let boundary = first.wrapped_lines[0].len;
            let first_row =
                TextElement::<EditorMode>::layout_range_corners(&(0..boundary), layout).unwrap();
            assert_eq!(
                first_row.len(),
                1,
                "a wrap end must not open the next visual row"
            );
            assert_eq!(first_row[0].top_right.x, first.wrapped_lines[0].width);
            let rest =
                TextElement::<EditorMode>::layout_range_corners(&(boundary..first.len()), layout)
                    .unwrap();
            assert_eq!(rest.len(), first.wrapped_lines.len() - 1);
            assert_eq!(rest[0].top_left.y, layout.visible_top + layout.line_height);
            for (index, corners) in rest.iter().enumerate() {
                assert_eq!(
                    corners.top_right.x,
                    first.wrap_indent + first.wrapped_lines[index + 1].width
                );
                assert_eq!(
                    corners.bottom_left.y - corners.top_left.y,
                    layout.line_height
                );
            }
            let blank_offset = text.find("\n\n").unwrap() + 1;
            let blank = TextElement::<EditorMode>::layout_range_corners(
                &(blank_offset..blank_offset + 1),
                layout,
            )
            .unwrap();
            assert_eq!(blank.len(), 1);
            assert_eq!(
                blank[0].top_right.x - blank[0].top_left.x,
                layout.space_width
            );
            // A later-line decoration cannot paint the first row via saturating_sub.
            let last_offset = text.rfind("last").unwrap();
            let last =
                TextElement::<EditorMode>::layout_range_corners(&(last_offset..text.len()), layout)
                    .unwrap();
            assert_eq!(last.len(), 1);
            assert!(last[0].top_left.y > rest.last().unwrap().top_left.y);
        });
    }

    #[gpui::test]
    fn geometric_decorations_include_crlf_boundaries(cx: &mut TestAppContext) {
        let text = "one\r\ntwo\r\nthree";
        let (editor, window) = decoration_editor(cx, text, false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            editor.update(cx, |state, cx| {
                state.create_range_decorations_collection(
                    vec![
                        RangeDecoration::new(0..8),
                        RangeDecoration::new(3..5),
                        RangeDecoration::new(4..5),
                        RangeDecoration::new(5..8),
                        RangeDecoration::new(9..10),
                    ],
                    cx,
                );
            });
            window.draw(cx).clear(cx);
            let state = editor.read(cx);
            let layout = state.last_layout.as_ref().unwrap();
            // Shaping retains '\r'; only '\n' needs an additional newline cell.
            assert_eq!(layout.lines[0].len(), "one\r".len());
            assert_eq!(layout.visible_line_byte_offsets, vec![0, 5, 10]);
            let spanning =
                TextElement::<EditorMode>::layout_range_corners(&(0..8), layout).unwrap();
            assert_eq!(spanning.len(), 2);
            for range in [3..5, 4..5, 9..10] {
                let corners =
                    TextElement::<EditorMode>::layout_range_corners(&range, layout).unwrap();
                assert_eq!(corners.len(), 1);
                assert!(corners[0].top_right.x > corners[0].top_left.x);
            }
            let (fills, frames) = TextElement::new(editor.clone()).layout_range_decorations(
                layout,
                &state.input_bounds,
                window,
                state.input_bounds,
                cx,
            );
            assert!(fills.is_empty());
            assert_eq!(frames.len(), 5);
        });
    }

    #[gpui::test]
    fn geometric_decorations_project_folds_without_changing_tracked_ranges(
        cx: &mut TestAppContext,
    ) {
        let text = "top\nhidden one\nhidden two\nend\nlast";
        let (editor, window) = decoration_editor(cx, text, false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let hidden = 4..text.find("end").unwrap();
            let collection = editor.update(cx, |state, cx| {
                state.apply_highlighter_fold_candidates(vec![FoldRange::new(0, 3)], cx);
                state.display_map.set_folded(0, true);
                state.create_range_decorations_collection(
                    vec![RangeDecoration::new(hidden.clone())],
                    cx,
                )
            });
            window.draw(cx).clear(cx);
            {
                let state = editor.read(cx);
                let layout = state.last_layout.as_ref().unwrap();
                assert_eq!(layout.visible_buffer_lines, vec![0, 3, 4]);
                assert!(TextElement::<EditorMode>::layout_range_corners(&hidden, layout).is_none());
                let (_, frames) = TextElement::new(editor.clone()).layout_range_decorations(
                    layout,
                    &state.input_bounds,
                    window,
                    state.input_bounds,
                    cx,
                );
                assert!(frames.is_empty());
                let spanning =
                    TextElement::<EditorMode>::layout_range_corners(&(0..text.len()), layout)
                        .unwrap();
                assert_eq!(spanning.len(), 3);
                assert_eq!(spanning[1].top_left.y, spanning[0].bottom_left.y);
            }
            assert_eq!(collection.get_ranges(cx), vec![hidden.clone()]);
            editor.update(cx, |state, cx| {
                state.display_map.set_folded(0, false);
                cx.notify();
            });
            window.draw(cx).clear(cx);
            let state = editor.read(cx);
            let layout = state.last_layout.as_ref().unwrap();
            let revealed =
                TextElement::<EditorMode>::layout_range_corners(&hidden, layout).unwrap();
            assert_eq!(revealed.len(), 2);
            assert_eq!(collection.get_ranges(cx), vec![hidden]);
        });
    }

    #[gpui::test]
    fn geometric_decorations_track_edits_history_replacement_and_owner_lifetime(
        cx: &mut TestAppContext,
    ) {
        let (editor, window) = decoration_editor(cx, "abc def", false);
        let mut cx = VisualTestContext::from_window(window.into(), cx);
        cx.update(|window, cx| {
            let (first, second) = editor.update(cx, |state, cx| {
                (
                    state.create_range_decorations_collection(vec![RangeDecoration::new(4..7)], cx),
                    state.create_range_decorations_collection(vec![RangeDecoration::new(0..3)], cx),
                )
            });
            editor.update(cx, |state, cx| {
                state.set_selected_range(0..0, cx);
                state.replace_text_in_range(None, "\n", window, cx);
            });
            assert_eq!(first.get_ranges(cx), vec![5..8]);
            editor.update(cx, |state, cx| state.undo(&Undo, window, cx));
            assert_eq!(first.get_ranges(cx), vec![4..7]);
            editor.update(cx, |state, cx| state.redo(&Redo, window, cx));
            assert_eq!(first.get_ranges(cx), vec![5..8]);
            first.clear(cx);
            assert_eq!(second.get_ranges(cx), vec![1..4]);
            first.append(vec![RangeDecoration::new(5..8)], cx);
            editor.update(cx, |state, cx| state.replace_all("formatted", window, cx));
            assert_eq!(first.get_ranges(cx), vec![0..9]);
            editor.update(cx, |state, cx| state.undo(&Undo, window, cx));
            // Annotations are transformed, not snapshotted in undo history.
            assert_eq!(first.get_ranges(cx), vec![0..8]);
            editor.update(cx, |state, cx| state.set_value("new", window, cx));
            assert_eq!(first.get_ranges(cx), vec![0..3]);
            let clone = first.clone();
            first.dispose(cx);
            clone.append(vec![RangeDecoration::new(0..1)], cx);
            assert!(clone.get_ranges(cx).is_empty());
            assert_eq!(second.get_ranges(cx), vec![0..3]);
            editor.update(cx, |state, cx| state.set_value("", window, cx));
            assert!(second.get_ranges(cx).is_empty());
            window.draw(cx).clear(cx);
        });
    }

    #[test]
    fn frame_outline_is_continuous_across_different_line_widths() {
        let corners = [
            Corners {
                top_left: point(px(2.), px(0.)),
                top_right: point(px(20.), px(0.)),
                bottom_left: point(px(2.), px(10.)),
                bottom_right: point(px(20.), px(10.)),
            },
            Corners {
                top_left: point(px(0.), px(10.)),
                top_right: point(px(12.), px(10.)),
                bottom_left: point(px(0.), px(20.)),
                bottom_right: point(px(12.), px(20.)),
            },
        ];

        assert_eq!(
            frame_outline_points(&corners),
            vec![
                point(px(2.), px(0.)),
                point(px(20.), px(0.)),
                point(px(20.), px(10.)),
                point(px(12.), px(10.)),
                point(px(12.), px(20.)),
                point(px(0.), px(20.)),
                point(px(0.), px(10.)),
                point(px(2.), px(10.)),
                point(px(2.), px(0.)),
            ]
        );
    }

    #[test]
    fn frame_outline_keeps_horizontal_space_between_the_stroke_and_text() {
        let corners = [Corners {
            top_left: point(px(2.), px(0.)),
            top_right: point(px(20.), px(0.)),
            bottom_left: point(px(2.), px(10.)),
            bottom_right: point(px(20.), px(10.)),
        }];

        assert_eq!(
            pad_frame_corners(&corners, px(1.)),
            vec![Corners {
                top_left: point(px(1.), px(0.)),
                top_right: point(px(21.), px(0.)),
                bottom_left: point(px(1.), px(10.)),
                bottom_right: point(px(21.), px(10.)),
            }]
        );

        let points = frame_outline_points(&corners);
        assert_eq!(points.first(), points.last());
    }

    #[test]
    fn frame_outline_stroke_is_aligned_to_physical_pixels_at_each_scale_factor() {
        let points = vec![point(px(0.2), px(1.8)), point(px(10.7), px(20.3))];

        for (scale_factor, expected_width, expected_points) in [
            (
                1.,
                px(1.),
                vec![point(px(0.5), px(1.5)), point(px(10.5), px(20.5))],
            ),
            (
                1.5,
                px(2. / 3.),
                vec![
                    point(px(1. / 3.), px(5. / 3.)),
                    point(px(11.), px(61. / 3.)),
                ],
            ),
            (
                2.,
                px(1.),
                vec![point(px(0.), px(2.)), point(px(10.5), px(20.5))],
            ),
        ] {
            let (width, snapped) = snap_frame_outline(&points, px(1.), scale_factor);
            assert_eq!(width, expected_width);
            assert_eq!(snapped, expected_points);
        }
    }

    #[test]
    fn frame_outline_keeps_its_vertical_edges_inside_the_content_mask() {
        let points = vec![
            point(px(9.5), px(2.5)),
            point(px(20.5), px(2.5)),
            point(px(20.5), px(10.5)),
            point(px(9.5), px(10.5)),
            point(px(9.5), px(2.5)),
        ];
        let mask = Bounds::new(point(px(10.), px(0.)), size(px(10.), px(20.)));

        assert_eq!(
            clamp_frame_to_content_mask(points, px(1.), mask),
            vec![
                point(px(10.5), px(2.5)),
                point(px(19.5), px(2.5)),
                point(px(19.5), px(10.5)),
                point(px(10.5), px(10.5)),
                point(px(10.5), px(2.5)),
            ]
        );
    }

    #[test]
    fn test_plain_text_decorations_include_unstyled_gaps() {
        let decoration = HighlightStyle {
            background_color: Some(gpui::red()),
            ..Default::default()
        };
        let styles = compose_decorations(Vec::new(), [(2..5, decoration)], 0..10).unwrap();

        assert_eq!(
            styles
                .iter()
                .map(|(range, _)| range.clone())
                .collect::<Vec<_>>(),
            vec![0..2, 2..5, 5..10]
        );
        assert_eq!(styles[0].1, HighlightStyle::default());
        assert_eq!(styles[1].1.background_color, Some(gpui::red()));
        assert_eq!(styles[2].1, HighlightStyle::default());
    }

    #[test]
    fn test_first_decoration_collection_has_precedence() {
        let first = [TextDecoration::new(
            0..4,
            HighlightStyle {
                background_color: Some(gpui::red()),
                ..Default::default()
            },
        )];
        let second = [TextDecoration::new(
            0..4,
            HighlightStyle {
                background_color: Some(gpui::blue()),
                ..Default::default()
            },
        )];

        let styles =
            compose_decoration_collections(Vec::new(), [&first[..], &second[..]], 0..4).unwrap();

        assert_eq!(styles.len(), 1);
        assert_eq!(styles[0].1.background_color, Some(gpui::red()));
    }

    #[test]
    fn test_decorations_override_syntax_highlights() {
        let syntax = vec![(
            0..8,
            HighlightStyle {
                color: Some(gpui::blue()),
                ..Default::default()
            },
        )];
        let decoration = HighlightStyle {
            color: Some(gpui::red()),
            font_style: Some(gpui::FontStyle::Italic),
            ..Default::default()
        };

        let styles = compose_decorations(syntax, [(2..6, decoration)], 0..8).unwrap();

        assert_eq!(styles[1].0, 2..6);
        assert_eq!(styles[1].1.color, Some(gpui::red()));
        assert_eq!(styles[1].1.font_style, Some(gpui::FontStyle::Italic));
    }

    #[test]
    fn test_editor_scrollbar_layout_uses_current_scroll_size() {
        let input_bounds = Bounds::new(point(px(10.), px(20.)), size(px(300.), px(80.)));
        let paddings = Edges {
            top: px(2.),
            right: px(3.),
            bottom: px(5.),
            left: px(7.),
        };

        let layout =
            EditorScrollbarLayout::new(input_bounds, px(40.), size(px(1000.), px(200.)), paddings);

        assert_eq!(
            layout.bounds,
            Bounds::new(point(px(51.), px(18.)), size(px(262.), px(87.)))
        );
        assert_eq!(layout.scroll_size, size(px(972.), px(200.)));
        assert_eq!(layout.scroll_viewport_size, size(px(262.), px(80.)));

        let layout_without_gutter =
            EditorScrollbarLayout::new(input_bounds, px(0.), size(px(500.), px(120.)), paddings);

        assert_eq!(
            layout_without_gutter.bounds,
            Bounds::new(point(px(10.), px(18.)), size(px(303.), px(87.)))
        );
        assert_eq!(layout_without_gutter.scroll_size, size(px(513.), px(120.)));
        assert_eq!(
            layout_without_gutter.scroll_viewport_size,
            size(px(303.), px(80.))
        );
    }

    #[test]
    fn test_editor_gutter_covers_the_complete_fixed_column() {
        let input_bounds = Bounds::new(point(px(10.), px(20.)), size(px(300.), px(80.)));

        assert_eq!(
            editor_gutter_bounds(
                input_bounds,
                px(48.),
                px(16.),
                Edges {
                    top: px(2.),
                    right: px(3.),
                    bottom: px(5.),
                    left: px(7.),
                },
            ),
            Bounds::new(point(px(3.), px(18.)), size(px(55.), px(103.)))
        );
    }

    #[test]
    fn test_vertical_scroll_offset_is_clamped_to_current_viewport() {
        assert_eq!(
            clamp_vertical_scroll_offset(px(-260.), px(340.), px(160.), px(0.)),
            px(-180.)
        );
        assert_eq!(
            clamp_vertical_scroll_offset(px(-40.), px(340.), px(160.), px(0.)),
            px(-40.)
        );
        assert_eq!(
            clamp_vertical_scroll_offset(px(20.), px(340.), px(160.), px(0.)),
            px(0.)
        );
        assert_eq!(
            clamp_vertical_scroll_offset(px(-40.), px(100.), px(160.), px(0.)),
            px(0.)
        );
        // Room above the first line (typewriter) lets the top sit lower, never past the inset.
        assert_eq!(
            clamp_vertical_scroll_offset(px(20.), px(340.), px(160.), px(50.)),
            px(20.)
        );
        assert_eq!(
            clamp_vertical_scroll_offset(px(80.), px(100.), px(160.), px(50.)),
            px(50.)
        );
    }

    #[test]
    fn test_runs_for_range() {
        let run = TextRun {
            len: 0,
            font: gpui::font(".SystemUIFont"),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        // use hello this-is-test
        let runs = vec![
            // use
            TextRun {
                len: 3,
                ..run.clone()
            },
            // \s
            TextRun {
                len: 1,
                ..run.clone()
            },
            // hello
            TextRun {
                len: 5,
                ..run.clone()
            },
            // \s
            TextRun {
                len: 1,
                ..run.clone()
            },
            // this-is-test
            TextRun {
                len: 12,
                ..run.clone()
            },
        ];

        #[track_caller]
        fn assert_runs(actual: Vec<TextRun>, expected: &[usize]) {
            let left = actual.iter().map(|run| run.len).collect::<Vec<_>>();
            assert_eq!(left, expected);
        }

        assert_runs(runs_for_range(&runs, 0, &(0..0)), &[]);
        assert_runs(runs_for_range(&runs, 0, &(0..100)), &[3, 1, 5, 1, 12]);

        assert_runs(runs_for_range(&runs, 0, &(0..6)), &[3, 1, 2]);
        assert_runs(runs_for_range(&runs, 0, &(1..6)), &[2, 1, 2]);
        assert_runs(runs_for_range(&runs, 0, &(3..10)), &[1, 5, 1]);
        assert_runs(runs_for_range(&runs, 0, &(5..8)), &[3]);
        assert_runs(runs_for_range(&runs, 3, &(0..3)), &[1, 2]);
        assert_runs(runs_for_range(&runs, 3, &(2..10)), &[4, 1, 3]);
        assert_runs(runs_for_range(&runs, 9, &(0..8)), &[1, 7]);
    }

    #[test]
    fn test_split_runs_preserve_ime_underline_across_highlight_boundaries() {
        let underline = UnderlineStyle {
            thickness: px(1.),
            color: Some(gpui::black()),
            wavy: false,
        };

        let runs = [0..4, 4..10]
            .into_iter()
            .flat_map(|range| {
                split_run_for_ime_underline(
                    TextStyle::default().to_run(range.len()),
                    range,
                    Some(2..7),
                    None,
                    Some(underline),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            runs.iter()
                .map(|run| (run.len, run.underline.is_some()))
                .collect::<Vec<_>>(),
            vec![(2, false), (2, true), (3, true), (3, false)]
        );
    }

    #[test]
    fn test_split_run_applies_ime_underline_without_highlighting() {
        let underline = UnderlineStyle {
            thickness: px(1.),
            color: Some(gpui::black()),
            wavy: false,
        };

        let runs = split_run_for_ime_underline(
            TextStyle::default().to_run(10),
            0..10,
            Some(2..7),
            None,
            Some(underline),
        );

        assert_eq!(
            runs.iter()
                .map(|run| (run.len, run.underline.is_some()))
                .collect::<Vec<_>>(),
            vec![(2, false), (5, true), (3, false)]
        );
        assert!(
            split_run_for_ime_underline(TextStyle::default().to_run(0), 0..0, None, None, None)
                .is_empty()
        );
    }

    #[test]
    fn test_ime_converting_clause_is_thick_across_highlight_boundaries() {
        let underline = UnderlineStyle {
            thickness: px(1.),
            color: Some(gpui::black()),
            wavy: false,
        };
        let runs: Vec<_> = [0..5, 5..10].into_iter().flat_map(|range| {
            split_run_for_ime_underline(
                TextStyle::default().to_run(range.len()),
                range,
                Some(2..8),
                Some(4..7),
                Some(underline),
            )
        }).collect();
        assert_eq!(
            runs.iter().map(|run| (run.len, run.underline.map(|u| u.thickness)))
                .collect::<Vec<_>>(),
            vec![(2, None), (2, Some(px(1.))), (1, Some(px(2.))),
                 (2, Some(px(2.))), (1, Some(px(1.))), (2, None)],
        );
    }

    #[test]
    fn test_masked_ime_underline_splits_on_mask_char_boundaries() {
        let underline = UnderlineStyle {
            thickness: px(1.),
            color: Some(gpui::black()),
            wavy: false,
        };
        let text = Rope::from("abcdef");
        let mask_len = MASK_CHAR.len_utf8();

        assert_eq!(
            ime_marked_display_range(&text, Some(4..6), false),
            Some(4..6)
        );
        assert_eq!(ime_marked_display_range(&text, None, true), None);
        assert_eq!(
            ime_marked_display_range(&text, Some(4..6), true),
            Some(4 * mask_len..6 * mask_len)
        );

        let display_text = MASK_CHAR.to_string().repeat(text.chars().count());
        let runs = split_run_for_ime_underline(
            TextStyle::default().to_run(display_text.len()),
            0..display_text.len(),
            ime_marked_display_range(&text, Some(4..6), true),
            None,
            Some(underline),
        );

        let mut offset = 0;
        for run in &runs {
            assert!(display_text.is_char_boundary(offset));
            offset += run.len;
        }
        assert_eq!(offset, display_text.len());
    }

    #[test]
    fn test_placeholder_line_runs() {
        let run = TextRun {
            len: 0,
            font: gpui::font(".SystemUIFont"),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        let runs = vec![
            TextRun {
                len: 2,
                ..run.clone()
            },
            TextRun {
                len: 2,
                ..run.clone()
            },
            TextRun { len: 1, ..run },
        ];

        let placeholder_runs = placeholder_line_runs("ab\n\nc", &runs);

        let lines = placeholder_runs
            .iter()
            .map(|(line, _)| *line)
            .collect::<Vec<_>>();
        assert_eq!(lines, vec!["ab", "", "c"]);

        let run_lengths = placeholder_runs
            .iter()
            .map(|(_, line_runs)| line_runs.iter().map(|run| run.len).collect::<Vec<_>>())
            .collect::<Vec<_>>();
        assert_eq!(run_lengths, vec![vec![2], vec![], vec![1]]);
    }

    #[test]
    fn test_align_runs_to_char_boundaries() {
        let run = TextRun {
            len: 0,
            font: gpui::font(".SystemUIFont"),
            color: gpui::blue(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let runs = |lens: &[usize]| {
            lens.iter()
                .map(|&len| TextRun { len, ..run.clone() })
                .collect::<Vec<_>>()
        };
        let lens = |runs: &[TextRun]| runs.iter().map(|run| run.len).collect::<Vec<_>>();

        // "你好，世界" is 15 bytes; boundaries at 0, 3, 6, 9, 12, 15.
        let text = "你好，世界";

        // Aligned runs pass through untouched.
        assert_eq!(align_runs_to_char_boundaries(text, &runs(&[3, 12])), None);

        // A mid-char boundary (4) snaps up to the next boundary (6).
        let aligned = align_runs_to_char_boundaries(text, &runs(&[4, 11])).unwrap();
        assert_eq!(lens(&aligned), vec![6, 9]);

        // Several boundaries inside the same char collapse into one run.
        let aligned = align_runs_to_char_boundaries(text, &runs(&[1, 1, 13])).unwrap();
        assert_eq!(lens(&aligned), vec![3, 12]);

        // Overshooting runs are capped at the text length.
        let aligned = align_runs_to_char_boundaries(text, &runs(&[3, 20])).unwrap();
        assert_eq!(lens(&aligned), vec![3, 12]);

        // Undershooting runs keep the shortfall (the tail is left unshaped,
        // matching `runs_for_range` clipping).
        assert_eq!(align_runs_to_char_boundaries(text, &runs(&[3, 3])), None);
    }

    #[test]
    fn test_clip_styles_to_ranges() {
        let style = HighlightStyle::default();
        let styles = vec![(0..4, style), (4..10, style), (12..20, style)];

        // Ranges with a gap (a folded region) drop the gap coverage and split
        // styles that span a range edge.
        let clipped = clip_styles_to_ranges(styles.clone(), &[2..6, 14..18]);
        assert_eq!(
            clipped
                .iter()
                .map(|(range, _)| range.clone())
                .collect::<Vec<_>>(),
            vec![2..4, 4..6, 14..18]
        );

        // Styles fully inside the ranges are unchanged.
        let clipped = clip_styles_to_ranges(styles.clone(), &[0..20]);
        assert_eq!(
            clipped
                .iter()
                .map(|(range, _)| range.clone())
                .collect::<Vec<_>>(),
            vec![0..4, 4..10, 12..20]
        );

        // A single style spanning several ranges splits into one piece per range.
        let clipped = clip_styles_to_ranges(vec![(0..30, style)], &[5..10, 20..25]);
        assert_eq!(
            clipped
                .iter()
                .map(|(range, _)| range.clone())
                .collect::<Vec<_>>(),
            vec![5..10, 20..25]
        );

        assert!(clip_styles_to_ranges(styles, &[]).is_empty());
    }

    #[test]
    fn test_split_runs_by_bg_segments() {
        let run = TextRun {
            len: 0,
            font: gpui::font(".SystemUIFont"),
            color: gpui::blue(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        let runs = vec![
            TextRun {
                len: 5,
                ..run.clone()
            },
            TextRun {
                len: 7,
                ..run.clone()
            },
            TextRun {
                len: 24,
                ..run.clone()
            },
        ];

        let bg_segments = vec![(8..12, gpui::red()), (12..18, gpui::blue())];
        let ground = gpui::white();
        let result = split_runs_by_bg_segments(5, &runs, &bg_segments, &[], ground);
        // The pieces of one run that read the same rejoin.
        assert_eq!(
            result.iter().map(|run| run.len).collect::<Vec<_>>(),
            vec![3, 4, 6, 23]
        );
        assert_eq!(result[0].color, gpui::blue());
        assert_eq!(
            result[1].color,
            ensure_minimum_contrast(gpui::blue(), gpui::red(), MIN_HIGHLIGHT_CONTRAST)
        );
        assert_eq!(
            result[2].color,
            ensure_minimum_contrast(gpui::blue(), gpui::blue(), MIN_HIGHLIGHT_CONTRAST)
        );
        assert_eq!(result[3].color, gpui::blue());

        // Quiet text (low contrast at rest) is asked for no more than it has.
        let ground = gpui::black();
        let quiet = apca_contrast(gpui::blue(), ground).abs();
        assert!(quiet < MIN_HIGHLIGHT_CONTRAST);
        let result = split_runs_by_bg_segments(5, &runs, &bg_segments, &[], ground);
        assert_eq!(
            result.iter().map(|run| run.len).collect::<Vec<_>>(),
            vec![7, 6, 23]
        );
        assert_eq!(result[0].color, gpui::blue());
        assert_eq!(
            result[1].color,
            ensure_minimum_contrast(gpui::blue(), gpui::blue(), quiet)
        );
        assert_eq!(result[2].color, gpui::blue());

        // A translucent run with no fade keeps its alpha over a fill; a focus-faded one gets back
        // only the alpha the dim took away.
        let ground = gpui::white();
        let translucent = TextRun {
            len: 10,
            color: Hsla { a: 0.6, ..gpui::blue() },
            ..run.clone()
        };
        let fills = vec![(0..10, gpui::red())];
        let plain = split_runs_by_bg_segments(0, &[translucent.clone()], &fills, &[], ground);
        assert!(plain.iter().all(|run| run.color.a == 0.6));
        let dimmed = TextRun { color: Hsla { a: 0.2, ..gpui::blue() }, ..translucent };
        let back = split_runs_by_bg_segments(0, &[dimmed], &fills, &[(0..10, 0.6)], ground);
        assert!(back.iter().all(|run| run.color.a == 0.6));
    }

    #[test]
    fn test_empty_bottom_height_outside_code_editor() {
        // Single-line / plain-text / auto-grow modes never reserve empty
        // bottom space, regardless of any override.
        for override_rows in [None, Some(0), Some(3), Some(99)] {
            assert_eq!(
                empty_bottom_height(false, override_rows, px(800.), px(20.)),
                px(0.),
            );
        }
    }

    #[test]
    fn test_empty_bottom_height_code_editor_default() {
        // `None`: roughly half the viewport, floored at
        // `BOTTOM_MARGIN_ROWS * line_height` so the empty area never
        // collapses to "less than a few lines" on tiny viewports.
        let line_height = px(20.);

        // Viewport much taller than the floor → half-viewport wins.
        assert_eq!(
            empty_bottom_height(true, None, px(800.), line_height),
            px(400.),
        );

        // Viewport shorter than 2 × floor → floor wins.
        let floor = BOTTOM_MARGIN_ROWS * line_height;
        assert_eq!(empty_bottom_height(true, None, px(40.), line_height), floor);
    }

    #[test]
    fn test_empty_bottom_height_explicit_row_count() {
        // `Some(n)`: exactly `n` line-heights. Caller fully controls
        // the trailing empty space; viewport size doesn't amplify it.
        let line_height = px(20.);

        for rows in [0_usize, 1, 3, 8, 64] {
            let expected = rows as f32 * line_height;
            assert_eq!(
                empty_bottom_height(true, Some(rows), px(800.), line_height),
                expected,
            );
            // Tiny viewport: still exactly `n × line_height`, no floor
            // applied when caller supplied an explicit count.
            assert_eq!(
                empty_bottom_height(true, Some(rows), px(20.), line_height),
                expected,
            );
        }
    }

    #[test]
    fn row_reveal_contains_the_whole_row_and_has_a_stable_offset() {
        let line_height = px(20.);
        let row_top = px(1000.);
        for viewport_height in [px(20.), px(30.), px(600.)] {
            for auto_grow in [false, true] {
                for surrounding in [None, Some(0), Some(50)] {
                    let padding = cursor_surrounding_padding(
                        auto_grow,
                        surrounding,
                        viewport_visible_lines(viewport_height, line_height),
                        line_height,
                    );
                    for previous in [px(-1500.), px(-1000.), px(0.)] {
                        let offset =
                            reveal_row(previous, row_top, line_height, viewport_height, padding, px(0.));
                        assert!(row_top + offset >= px(0.));
                        assert!(row_top + offset + line_height <= viewport_height);
                        assert_eq!(
                            reveal_row(offset, row_top, line_height, viewport_height, padding, px(0.)),
                            offset,
                            "unchanged caret must not oscillate",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn undersized_row_reveal_aligns_the_start_without_oscillating() {
        let offset = reveal_row(px(0.), px(80.), px(20.), px(10.), px(60.), px(0.));
        assert_eq!(px(80.) + offset, px(0.));
        assert_eq!(
            reveal_row(offset, px(80.), px(20.), px(10.), px(60.), px(0.)),
            offset
        );
    }

    #[test]
    fn typewriter_band_sits_a_little_above_the_middle() {
        // 45% from the top, for the row's middle: the row's top is half a line higher.
        assert_eq!(typewriter_band(px(800.), px(20.)), px(350.));
        assert_eq!(typewriter_band(px(10.), px(20.)), px(0.));
    }

    #[test]
    fn a_row_inside_the_top_inset_keeps_its_offset() {
        // With room above the first line, a visible caret row doesn't snap the page back to 0.
        assert_eq!(reveal_row(px(300.), px(0.), px(20.), px(800.), px(60.), px(350.)), px(300.));
        assert_eq!(reveal_row(px(300.), px(0.), px(20.), px(800.), px(60.), px(0.)), px(0.));
    }
}
