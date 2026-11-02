//! Pinned scope lines: deep inside a long function, table or YAML block, the lines that open the
//! enclosing scopes stay at the top of the editor in a quieter ink, up to three levels, outermost
//! first (finch#zkqh). Each slides up and away as its scope's last line passes under it, and a
//! click glides the caret to it.
//!
//! The scopes are the editor's fold candidates, so Rust, Python, JSON, YAML and TOML are covered
//! with no language server. The editor knows nothing of this: the caret layer's view paints it
//! from the editor's scroll offset in its own prepaint, over the caret (so the caret and the
//! current-line band are covered where they scroll under it), like the explorer's pinned folders
//! (`explorer/sticky.rs`). Nothing here notifies; a scroll already rebuilds the layer in the
//! editor's frame, and at rest no frame is asked for.
//!
//! Which scopes pin depends only on the top row, whether it is scrolled part way, and the editor's
//! candidates and projection, so the walk (parent links, one binary search and a few steps up per
//! level) runs when one of those changes and never on a frame that only moved a few pixels. The
//! slide is arithmetic on the cached rows from the live scroll offset. No text past 4 MB pins.
//!
//! Header lines are drawn as plain text in the quieter ink, not with the highlighter's colors:
//! the highlighter only colors the visible lines, and one ink for the whole header is the point.

use std::{cell::RefCell, ops::Range, rc::Rc, time::Instant};

use gpui_kit::base::input::FoldRange;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{EditorState, RopeExt as _};
use gpui_kit::*;

use gpui_kit::base::ScrollbarHandle as _;

use crate::{motion::{Fade, drawn_now}, scrollbar::Feed, theme, tokens};

/// The most scope lines pinned at once.
const MAX_LEVELS: usize = 3;
/// A viewport under this many rows pins one line, and under `ONE_LEVEL_ROWS` none: the editor keeps
/// the caret three rows from the top edge only on a viewport of `THREE_LEVEL_ROWS` rows or more, and
/// the header must never be taller than that clearance (the caret's reveal would fight it).
const THREE_LEVEL_ROWS: usize = 24;
const ONE_LEVEL_ROWS: usize = 8;
/// A header line is cut to this many characters before it is shaped.
const MAX_CHARS: usize = 300;
/// Spaces a tab takes in a header line.
const TAB: &str = "    ";
/// The header's text ink, as an alpha of the foreground.
const INK_ALPHA: f32 = 0.45;
/// The shadow under the header: its height and its black at the top edge.
const SHADOW: f32 = 6.;
const SHADOW_ALPHA: f32 = 0.18;
const SHADOW_LIGHT: f32 = 0.7;
/// The shadow comes in over this much scroll past the first pin.
const SHADOW_IN: f32 = 8.;

const NONE: u32 = u32::MAX;

/// How many scope lines a viewport of `visible` rows may pin.
fn levels(visible: usize) -> usize {
    if visible >= THREE_LEVEL_ROWS {
        MAX_LEVELS
    } else if visible >= ONE_LEVEL_ROWS {
        1
    } else {
        0
    }
}

/// For each candidate (sorted by start line), the nearest earlier candidate whose lines reach its
/// start, or `NONE`: one pass with a stack of the scopes still open.
fn parents(candidates: &[FoldRange]) -> Vec<u32> {
    let mut parents = Vec::with_capacity(candidates.len());
    let mut open: Vec<u32> = Vec::new();
    for (ix, candidate) in candidates.iter().enumerate() {
        while open.last().is_some_and(|&top| candidates[top as usize].end_line < candidate.start_line) {
            open.pop();
        }
        parents.push(open.last().copied().unwrap_or(NONE));
        open.push(ix as u32);
    }
    parents
}

/// The scopes around `line` into `out`, outermost first: the candidates that reach it and span at
/// least three lines. The last candidate starting at or before `line` is found by binary search,
/// and the rest are its parents; the ones that end before `line` are stepped over.
fn chain(candidates: &[FoldRange], parents: &[u32], line: usize, out: &mut Vec<usize>) {
    out.clear();
    let mut at = candidates.partition_point(|candidate| candidate.start_line <= line).checked_sub(1).map_or(NONE, |at| at as u32);
    while at != NONE {
        let candidate = candidates[at as usize];
        if candidate.end_line >= line && candidate.end_line > candidate.start_line + 1 {
            out.push(at as usize);
        }
        at = parents[at as usize];
    }
    out.reverse();
}

/// One pinned scope: its first line, and the display rows of that line and of its last.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Pinned {
    line: usize,
    start_row: usize,
    end_row: usize,
}

/// What the walk asks of the editor's projection.
struct Rows<'a> {
    /// The buffer line shown on a display row.
    line_at: &'a dyn Fn(usize) -> usize,
    /// The display rows a buffer line fills; `None` when a fold hides it.
    rows_of: &'a dyn Fn(usize) -> Option<Range<usize>>,
    /// How many display rows there are.
    count: usize,
}

/// The scopes pinned when `top` is the display row at the top of the viewport, `frac` when the page
/// is scrolled part way into it. Slot `i` rests `i` rows down and holds the `i`th scope of the
/// chain around row `top + i`, once that scope's first line has scrolled above where the slot
/// rests (the line is `frac` of a row up, so it counts a row more); a scope's first line hidden
/// by a fold, or a chain that no longer continues what the slots above hold, ends the stack.
fn pinned(candidates: &[FoldRange], parents: &[u32], rows: &Rows, top: usize, frac: bool, cap: usize) -> Vec<Pinned> {
    let (mut held, mut around) = (Vec::<usize>::new(), Vec::<usize>::new());
    let mut out = Vec::new();
    for slot in 0..cap {
        let row = top + slot;
        if row >= rows.count {
            break;
        }
        chain(candidates, parents, (rows.line_at)(row), &mut around);
        let above = top + slot + usize::from(frac);
        let reached = around.iter().take_while(|&&ix| (rows.rows_of)(candidates[ix].start_line).is_some_and(|span| span.start < above)).count();
        if reached <= slot || around[..slot] != held[..] {
            break;
        }
        let ix = around[slot];
        let candidate = candidates[ix];
        let Some(start) = (rows.rows_of)(candidate.start_line) else { break };
        let end_row = (rows.rows_of)(candidate.end_line).map_or(rows.count, |span| span.start);
        held.push(ix);
        out.push(Pinned { line: candidate.start_line, start_row: start.start, end_row });
    }
    out
}

/// Where each pinned line's top sits below the text's top, for a page scrolled `scrolled` points
/// with rows `lh` tall: its slot, or higher once its scope's last row has come up under it (the
/// header's bottom never passes that row's bottom). The ones above are drawn on top, so the pushed
/// header slides under its parent.
fn slots(stack: &[Pinned], scrolled: f32, lh: f32) -> Vec<f32> {
    stack.iter().enumerate().map(|(slot, pin)| (slot as f32 * lh).min(pin.end_row as f32 * lh - scrolled)).collect()
}

/// What decides which scopes pin: when none of it changed the cached stack is the answer.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    top: usize,
    frac: bool,
    generations: (u64, u64),
    rows: usize,
    revision: u64,
    cap: usize,
}

/// A header line shaped for one text revision, size and ink.
struct Shaped {
    line: usize,
    revision: u64,
    size: Pixels,
    color: Hsla,
    shaped: ShapedLine,
}

/// The layer's memory between frames, kept by the caret layer for the editor it follows.
#[derive(Default)]
pub struct Cache {
    /// Parent links of the candidates of this generation.
    parents: Vec<u32>,
    parents_for: Option<u64>,
    key: Option<Key>,
    stack: Vec<Pinned>,
    shaped: Vec<Shaped>,
    /// The hover wash's fade of each pinned line that is lit or fading, by line.
    hovers: Vec<(usize, Fade)>,
    /// The line the pointer was over at the last paint.
    hovered: Option<usize>,
    /// The pointer left the window since it last moved over it.
    away: bool,
    /// How many times the walk ran, for the tests.
    walks: usize,
}

impl Cache {
    pub fn shared() -> Rc<RefCell<Cache>> {
        Rc::default()
    }

    /// The pinned scopes for `key`, walking only when it differs from the last call's.
    fn stack_for(&mut self, key: Key, candidates: &[FoldRange], rows: &Rows) -> &[Pinned] {
        if self.key != Some(key) {
            if self.parents_for != Some(key.generations.0) || self.parents.len() != candidates.len() {
                self.parents = parents(candidates);
                self.parents_for = Some(key.generations.0);
            }
            self.walks += 1;
            self.stack = pinned(candidates, &self.parents, rows, key.top, key.frac, key.cap);
            self.key = Some(key);
        }
        &self.stack
    }

    /// `line` shaped in the header's ink, from the last frame's shaping when nothing about it changed.
    fn line(&mut self, state: &EditorState, line: usize, font: &Font, size: Pixels, color: Hsla, window: &mut Window) -> ShapedLine {
        let revision = state.text_revision();
        if let Some(kept) = self.shaped.iter().find(|kept| kept.line == line && kept.revision == revision && kept.size == size && kept.color == color) {
            return kept.shaped.clone();
        }
        let text: String = state.text().slice_line(line).chars().filter(|c| !matches!(c, '\n' | '\r')).take(MAX_CHARS).map(|c| if c == '\t' { TAB.to_string() } else { c.to_string() }).collect();
        let text: SharedString = text.into();
        let run = TextRun { len: text.len(), font: font.clone(), color, background_color: None, underline: None, strikethrough: None };
        let shaped = window.text_system().shape_line(text, size, &[run], None);
        self.shaped.retain(|kept| kept.line != line);
        self.shaped.push(Shaped { line, revision, size, color, shaped: shaped.clone() });
        shaped
    }

    /// Forgets shaped lines the stack no longer holds.
    fn trim(&mut self) {
        let stack = &self.stack;
        self.shaped.retain(|kept| stack.iter().any(|pin| pin.line == kept.line));
    }
}

/// One pinned line to draw this frame.
struct Row {
    top: Pixels,
    line: usize,
    shaped: ShapedLine,
}

/// A frame's header: what the paint draws and what a click hits.
struct Drawn {
    editor: WeakEntity<EditorState>,
    rows: Vec<Row>,
    /// The text's left edge and the line height.
    text_left: Pixels,
    line_height: Pixels,
    /// The header's top (the editor's top edge: nothing draws above it) and bottom.
    top: Pixels,
    bottom: Pixels,
    /// Where the header paints and hits: the pane's width, minus where Find floats over it.
    regions: Vec<Bounds<Pixels>>,
    hitboxes: Vec<Hitbox>,
    ground: Hsla,
    shadow: Hsla,
    /// The hover wash's ink at full level, and the cache that keeps the rows' fades.
    wash: Hsla,
    cache: Rc<RefCell<Cache>>,
}

/// The header's element: fill the caret layer's bounds, paint nothing when no scope pins.
pub fn layer(editor: WeakEntity<EditorState>, feed: Feed, cache: Rc<RefCell<Cache>>) -> impl IntoElement {
    let kept = cache.clone();
    canvas(
        move |bounds, window, cx| prepare(&editor, &feed, &cache, bounds, window, cx),
        move |_, drawn, window, cx| match drawn {
            Some(drawn) => paint(drawn, window, cx),
            None => {
                // Nothing pins: a header that pins again starts from rest, not from the old hover.
                let mut kept = kept.borrow_mut();
                kept.hovers.clear();
                kept.hovered = None;
            }
        },
    )
    .absolute()
    .inset_0()
}

fn prepare(editor: &WeakEntity<EditorState>, feed: &Feed, cache: &Rc<RefCell<Cache>>, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) -> Option<Drawn> {
    let entity = editor.upgrade()?;
    let state = entity.read(cx);
    if state.text().len() > crate::language::MAX_HIGHLIGHT_BYTES {
        return None;
    }
    let (look, gutter) = (state.drawn_look()?, state.gutter_width()?);
    let (input, offset) = (state.input_bounds(), state.scroll_offset());
    let lh = f32::from(look.line_height);
    // The header rests at the editor's top edge, where the text clips; the text starts `inset` below it.
    let edge = feed.viewport_bounds();
    if edge.size.height <= px(0.) {
        return None;
    }
    let inset = f32::from(input.origin.y - edge.origin.y);
    let scrolled = -f32::from(offset.y) - inset;
    if scrolled <= 0. || lh <= 0. {
        return None;
    }
    let cap = levels((f32::from(input.size.height) / lh) as usize);
    if cap == 0 {
        return None;
    }
    let top = (scrolled / lh) as usize;
    let (rows, revision) = (state.display_row_count(), state.text_revision());
    let key = Key { top, frac: scrolled - top as f32 * lh > 0.01, generations: state.fold_generations(), rows, revision, cap };

    let light = theme::is_light(cx);
    let theme = cx.theme();
    let (ground, ink) = (theme.background, tokens::ink(light, theme.foreground, INK_ALPHA));
    let kept = cache.clone();
    let mut cache = cache.borrow_mut();
    let view = Rows { line_at: &|row| state.buffer_line_at_display_row(row), rows_of: &|line| state.display_rows_of_line(line), count: rows };
    let stack = cache.stack_for(key, state.fold_candidates(), &view).to_vec();
    if stack.is_empty() {
        return None;
    }
    cache.trim();
    let (tops, text_top) = (slots(&stack, scrolled, lh), edge.origin.y);
    let rows: Vec<Row> = stack
        .iter()
        .zip(&tops)
        .map(|(pin, &y)| Row { top: text_top + px(y), line: pin.line, shaped: cache.line(state, pin.line, &look.font, look.size, ink, window) })
        .collect();
    let bottom = rows.iter().map(|row| row.top + look.line_height).fold(text_top, Pixels::max);
    // The shadow rides with the lowest row, coming in over the first points of scroll past the first pin.
    let into = ((scrolled - lh * stack[0].start_row as f32) / SHADOW_IN).clamp(0., 1.);
    let shadow = gpui::black().opacity(SHADOW_ALPHA * if light { SHADOW_LIGHT } else { 1. } * into);

    let regions = regions(bounds, text_top, bottom, state.caret_cover());
    let hitboxes = regions.iter().map(|region| window.insert_hitbox(*region, HitboxBehavior::BlockMouseExceptScroll)).collect();
    let wash = tokens::ink(light, theme.foreground, if light { tokens::ALPHA_HOVER_LIGHT } else { tokens::ALPHA_HOVER });
    Some(Drawn { editor: editor.clone(), rows, text_left: input.origin.x + gutter + offset.x, line_height: look.line_height, top: text_top, bottom, regions, hitboxes, ground, shadow, wash, cache: kept })
}

/// The header's area, `top` to `bottom` across `bounds`, as the rectangles `cover` (Find's bar) leaves.
fn regions(bounds: Bounds<Pixels>, top: Pixels, bottom: Pixels, cover: Option<Bounds<Pixels>>) -> Vec<Bounds<Pixels>> {
    let area = Bounds::from_corners(point(bounds.left(), top), point(bounds.right(), bottom));
    let Some(cover) = cover.filter(|cover| cover.intersects(&area)) else {
        return vec![area];
    };
    let mut out = Vec::new();
    let mut add = |left: Pixels, top: Pixels, right: Pixels, bottom: Pixels| {
        if right > left && bottom > top {
            out.push(Bounds::from_corners(point(left, top), point(right, bottom)));
        }
    };
    add(area.left(), area.top(), cover.left(), area.bottom());
    add(cover.left(), cover.bottom().max(area.top()), area.right(), area.bottom());
    add(cover.right().min(area.right()), area.top(), area.right(), cover.bottom().min(area.bottom()));
    out
}

fn paint(drawn: Drawn, window: &mut Window, cx: &mut App) {
    let Drawn { editor, rows, text_left, line_height, top, bottom, regions, hitboxes, ground, shadow, wash, cache } = drawn;
    let now = drawn_now(window.window_handle().window_id());
    let targets: Vec<(Pixels, usize)> = rows.iter().map(|row| (row.top, row.line)).collect();
    let away = cache.borrow().away;
    let over = (!away && hitboxes.iter().any(|hit| hit.is_hovered(window))).then(|| row_at(&targets, top, line_height, window.mouse_position().y)).flatten();
    let lit = hover_levels(&mut cache.borrow_mut(), &rows, over, now, window);
    for (region, hitbox) in regions.iter().zip(&hitboxes) {
        window.set_cursor_style(CursorStyle::PointingHand, hitbox);
        // The mask runs a shadow's height below the region, which only the shadow draws in.
        let mask = Bounds::from_corners(region.origin, point(region.right(), region.bottom() + px(SHADOW)));
        window.with_content_mask(Some(ContentMask { bounds: mask }), |window| {
            // Deepest first: a header pushed up slides under the one above it, which is opaque.
            for (row, &level) in rows.iter().zip(&lit[..rows.len().min(MAX_LEVELS)]).rev() {
                let area = Bounds::new(point(region.left(), row.top), size(region.size.width, line_height));
                window.paint_quad(fill(area, ground));
                if level > 0. {
                    // Like the tree's pinned rows: a point in from the row's top and bottom, rounded.
                    let band = Bounds::new(point(region.left(), row.top + tokens::ROW_WASH_INSET_Y), size(region.size.width, line_height - tokens::ROW_WASH_INSET_Y * 2.));
                    window.paint_quad(fill(band, wash.opacity(level)).corner_radii(Corners::all(tokens::RADIUS_ROW)));
                }
                _ = row.shaped.paint(point(text_left, row.top), line_height, TextAlign::Left, None, window, cx);
            }
            let edge = Bounds::new(point(region.left(), bottom), size(region.size.width, px(SHADOW)));
            window.paint_quad(fill(edge, linear_gradient(180., linear_color_stop(shadow, 0.), linear_color_stop(shadow.opacity(0.), 1.))));
        });
    }
    // The layer paints inside the caret layer's view: only that view needs the frame.
    let view = window.current_view();
    let hit = Rc::new((hitboxes, targets));
    window.on_mouse_event({
        let (hit, cache) = (hit.clone(), cache.clone());
        // A pointer moving from one row to the next stays inside the one hitbox, so nothing else asks for the frame that moves the wash.
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let over = hit.0.iter().any(|hitbox| hitbox.is_hovered(window)).then(|| row_at(&hit.1, top, line_height, event.position.y)).flatten();
            let mut kept = cache.borrow_mut();
            if kept.hovered != over || kept.away {
                kept.away = false;
                cx.notify(view);
            }
        }
    });
    // GPUI keeps the last pointer position when the pointer leaves the window, so the hover would stay lit.
    window.on_mouse_event({
        let cache = cache.clone();
        move |_: &MouseExitEvent, phase, _, cx| {
            let mut kept = cache.borrow_mut();
            if phase.capture() && !kept.away {
                kept.away = true;
                if kept.hovered.is_some() {
                    cx.notify(view);
                }
            }
        }
    });
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left || !hit.0.iter().any(|hitbox| hitbox.is_hovered(window)) {
            return;
        }
        if let Some(line) = row_at(&hit.1, top, line_height, event.position.y) {
            editor.update(cx, |state, cx| state.glide_to_line(line, window, cx)).ok();
        }
        cx.stop_propagation();
    });
}

/// The line pinned under height `y`: the topmost row there is the shallowest, since the deeper ones slide under it.
fn row_at(targets: &[(Pixels, usize)], top: Pixels, line_height: Pixels, y: Pixels) -> Option<usize> {
    targets.iter().find(|(row, _)| y >= (*row).max(top) && y < *row + line_height).map(|(_, line)| *line)
}

/// How lit each row's hover wash is at `now` (0 to 1), turning around from where it is drawn. A fade that has landed on rest is dropped; the next frame is asked for only while one moves.
fn hover_levels(cache: &mut Cache, rows: &[Row], hovered: Option<usize>, now: Instant, window: &mut Window) -> [f32; MAX_LEVELS] {
    cache.hovered = hovered;
    cache.hovers.retain(|(line, _)| rows.iter().any(|row| row.line == *line));
    let mut moving = false;
    let mut levels = [0.; MAX_LEVELS];
    for (slot, row) in rows.iter().enumerate().take(MAX_LEVELS) {
        let to = if hovered == Some(row.line) { 1. } else { 0. };
        let at = match cache.hovers.iter().position(|(line, _)| *line == row.line) {
            Some(at) => at,
            None if to == 0. => continue,
            None => {
                cache.hovers.push((row.line, Fade::at(0.)));
                cache.hovers.len() - 1
            }
        };
        let fade = &mut cache.hovers[at].1;
        fade.aim(to, now);
        moving |= fade.is_moving(now);
        levels[slot] = fade.level(now);
    }
    cache.hovers.retain(|(_, fade)| fade.level(now) > 0. || fade.is_moving(now));
    if moving {
        window.request_animation_frame();
    }
    levels
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Bounds, point, px, size};

    use super::{Cache, FoldRange, Key, Pinned, Rows, chain, levels, parents, pinned, regions, slots};

    /// A document of `lines` lines whose display rows are its lines.
    fn view(lines: usize) -> Rows<'static> {
        Rows { line_at: &|row| row, rows_of: &|line| Some(line..line + 1), count: lines }
    }

    fn scopes(ranges: &[(usize, usize)]) -> (Vec<FoldRange>, Vec<u32>) {
        let candidates: Vec<FoldRange> = ranges.iter().map(|&(start, end)| FoldRange::new(start, end)).collect();
        let parents = parents(&candidates);
        (candidates, parents)
    }

    #[test]
    fn the_chain_runs_outermost_first_and_steps_over_siblings() {
        // impl { fn a { .. } fn b { if { .. } } }
        let (candidates, parents) = scopes(&[(0, 40), (2, 10), (12, 38), (14, 20), (22, 30)]);
        let mut out = Vec::new();
        chain(&candidates, &parents, 25, &mut out);
        assert_eq!(out, [0, 2, 4]);
        chain(&candidates, &parents, 21, &mut out);
        assert_eq!(out, [0, 2]);
        chain(&candidates, &parents, 11, &mut out);
        assert_eq!(out, [0]);
    }

    #[test]
    fn a_scope_pins_once_its_first_line_has_scrolled_away() {
        let (candidates, parents) = scopes(&[(10, 60)]);
        let rows = view(100);
        assert!(pinned(&candidates, &parents, &rows, 10, false, 3).is_empty());
        assert_eq!(pinned(&candidates, &parents, &rows, 10, true, 3).len(), 1);
        assert_eq!(pinned(&candidates, &parents, &rows, 11, false, 3), [Pinned { line: 10, start_row: 10, end_row: 60 }]);
        // Past the scope's last row nothing pins.
        assert!(pinned(&candidates, &parents, &rows, 61, false, 3).is_empty());
    }

    #[test]
    fn nested_scopes_stack_to_the_cap_outermost_first() {
        let (candidates, parents) = scopes(&[(0, 90), (2, 80), (4, 70), (6, 60)]);
        let rows = view(100);
        let lines = |top, cap| pinned(&candidates, &parents, &rows, top, false, cap).iter().map(|pin| pin.line).collect::<Vec<_>>();
        assert_eq!(lines(30, 3), [0, 2, 4]);
        assert_eq!(lines(30, 1), [0]);
        // Two rows down only the outer scopes' lines have scrolled above their slots.
        assert_eq!(lines(2, 3), [0, 2]);
    }

    #[test]
    fn a_pushed_header_rises_and_the_one_above_stays() {
        let (candidates, parents) = scopes(&[(0, 90), (2, 52)]);
        let rows = view(100);
        // Row 50 at the top, 5 points into it: the inner scope's last row (52) is at 2 rows up.
        let stack = pinned(&candidates, &parents, &rows, 50, true, 3);
        assert_eq!(stack.len(), 2);
        let tops = slots(&stack, 50. * 20. + 5., 20.);
        assert_eq!(tops, [0., 20.]);
        // The last row at the second slot: the header's bottom is at the last row's bottom.
        let tops = slots(&stack, 51. * 20. + 5., 20.);
        assert_eq!(tops, [0., 15.]);
    }

    #[test]
    fn a_fold_hiding_a_scope_line_ends_the_stack() {
        let (candidates, parents) = scopes(&[(0, 90), (2, 80)]);
        let rows = Rows { line_at: &|row| row, rows_of: &|line| (line != 2).then(|| line..line + 1), count: 100 };
        assert_eq!(pinned(&candidates, &parents, &rows, 30, false, 3).len(), 1);
    }

    #[test]
    fn the_walk_runs_once_per_key() {
        let (candidates, _) = scopes(&[(0, 90), (2, 80)]);
        let rows = view(100);
        let mut cache = Cache::default();
        let key = |top| Key { top, frac: false, generations: (1, 1), rows: 100, revision: 0, cap: 3 };
        for _ in 0..50 {
            assert_eq!(cache.stack_for(key(30), &candidates, &rows).len(), 2);
        }
        assert_eq!(cache.walks, 1);
        cache.stack_for(key(31), &candidates, &rows);
        assert_eq!(cache.walks, 2);
        assert_eq!(cache.parents_for, Some(1));
    }

    #[test]
    fn small_viewports_pin_less() {
        assert_eq!(levels(40), 3);
        assert_eq!(levels(12), 1);
        assert_eq!(levels(5), 0);
    }

    #[test]
    fn find_leaves_the_header_around_it() {
        let bounds = Bounds::new(point(px(0.), px(0.)), size(px(100.), px(400.)));
        let area = regions(bounds, px(10.), px(70.), None);
        assert_eq!(area.len(), 1);
        let cover = Bounds::new(point(px(60.), px(0.)), size(px(40.), px(30.)));
        let around = regions(bounds, px(10.), px(70.), Some(cover));
        assert_eq!(around.len(), 2);
        assert!(around.iter().all(|region| !region.intersects(&cover)));
    }
}
