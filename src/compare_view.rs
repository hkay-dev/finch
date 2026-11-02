//! The Compare view: your unsaved edits (left) beside the text now on disk (right), drawn over a
//! tab's editor area when its file changed on disk under unsaved edits and the user chose
//! Compare… in the alert. Each change is washed (the theme's green on yours, red on disk), the
//! shorter side gets filler rows, and a chip on the change's first row (also a click anywhere on a
//! side) picks which side the merge keeps; the unpicked side dims. Cancel (Escape) leaves,
//! Save as… keeps your whole text in a new file, Use these (Return) tells the document to put the
//! merged text in the editor.
//! A right-click on a side of a change opens a native menu (Copy that side's lines, Take yours, Take the other); each side
//! has a line-number column in the gutter's ink, as wide as the biggest number.
//!
//! The rows come from `compare::Comparison::rows`, built with the comparison off the main thread,
//! and are drawn through a `uniform_list`, so a large file draws only the rows in view. The lines
//! are shared `Arc<str>`s that become `SharedString` handles as drawn, so a frame allocates
//! nothing per row. The document owns the view and answers its events;
//! the workspace draws it over the pane's editor.

use std::{cell::Cell, ops::Range, rc::Rc};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::{
    compare::{Comparison, Side},
    hover::{by_lit, FadeHover as _},
    native_menu::{self, Entry, Item},
    scrollbar::{self, Overlay, Scroller},
    settings, tab_bar::mix, theme, tooltip,
    tokens::{
        ALPHA_BUTTON_HOVER, ALPHA_BUTTON_PRESSED, ALPHA_COMPARE_CHIP_ON, ALPHA_COMPARE_FILLER, ALPHA_COMPARE_NUMBER, ALPHA_COMPARE_UNPICKED, ALPHA_COMPARE_WASH, ALPHA_HOVER,
        ALPHA_OPTION_ON, ALPHA_PRIMARY, ALPHA_SECONDARY, COMPARE_CHIP_GAP, COMPARE_CHIP_HEIGHT, COMPARE_CHIP_ROOM, COMPARE_CONTEXT_ROWS, COMPARE_HEADER_HEIGHT, COMPARE_INSET, COMPARE_NAME_MIN, COMPARE_NUMBER_GAP, FILTER_PAD_X,
        FIND_FIELD_HEIGHT, RADIUS_BUTTON, RADIUS_COMPARE_CHIP, TEXT_BODY, TEXT_BODY_LINE, TEXT_SUBHEADLINE, TEXT_SUBHEADLINE_LINE, tabular,
    },
};

/// What the view tells the document.
pub enum CompareEvent {
    /// Escape or Cancel: leave the view, the edits and the file as they were.
    Cancel,
    /// Save as…: leave the view and keep the full text in a new file.
    SaveAs,
    /// Use these: the merged text, with the final newline the buffer had, and whether any change took the disk's side.
    Use(String, bool),
}

impl EventEmitter<CompareEvent> for CompareView {}

/// What a right-click menu item does: copy the lines of a side of a change, or take a side for it.
#[derive(Clone)]
enum MenuCommand {
    Copy(usize, Side),
    Take(usize, Side),
}

pub struct CompareView {
    name: SharedString,
    /// What the right column and its chips are called: "Disk", or an old version's time.
    other: SharedString,
    comparison: Comparison,
    /// One pick per change, `Yours` at first.
    picks: Vec<Side>,
    /// The buffer ended with a newline (or was empty and the disk's does): the merge ends with one too.
    final_newline: bool,
    /// The editor's code font, size, weight and line height, taken when the view opened.
    family: SharedString,
    size: Pixels,
    weight: f32,
    line_height: Pixels,
    scroll: UniformListScrollHandle,
    /// The scrollbar's hand-scroll clock; both sides scroll together under the one bar.
    scroller: Scroller,
    /// The row each change starts on, in order.
    starts: Vec<usize>,
    /// The change at the top of the list (counted in the header), `None` while the list sits above the first one.
    current: Option<usize>,
    /// The side of a change under the pointer, and the one held down, so every row of it lights together.
    hover: Option<(usize, Side)>,
    pressed: Option<(usize, Side)>,
    focus: FocusHandle,
    /// The width of the line-number column, from the biggest number.
    number_width: Pixels,
    /// The right padding of a change's first row on each side, from its chip's width.
    chip_rooms: [Pixels; 2],
    /// The header's width as the last frame laid it out, `None` until one has (the window's width stands in); `seed_width` gives the first frame the room the view opens into.
    header_width: Rc<Cell<Option<Pixels>>>,
}

impl CompareView {
    /// A view of `comparison` for the file `name`. `line_height` is the editor's, when it has drawn.
    pub fn new(name: String, comparison: Comparison, final_newline: bool, line_height: Option<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let editor = &settings::get(cx).editor;
        let family = editor.drawn_family(cx).unwrap_or_else(|| cx.theme().mono_font_family.clone());
        let size = editor.font_size.map(px).unwrap_or(cx.theme().mono_font_size);
        let weight = settings::get(cx).text.weight;
        let picks = vec![Side::Yours; comparison.changes.len()];
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        // Open with the first change in view and a few rows of context above it; the list leaves a change already showing where it is.
        let scroll = UniformListScrollHandle::new();
        let mut starts = Vec::with_capacity(comparison.changes.len());
        for (ix, row) in comparison.rows.iter().enumerate() {
            if row.change() == Some(starts.len()) {
                starts.push(ix);
            }
        }
        if let Some(&first) = starts.first() {
            scroll.scroll_to_item_with_offset(first, ScrollStrategy::Top, COMPARE_CONTEXT_ROWS);
        }
        let current = (!starts.is_empty()).then_some(0);
        let mut view = Self {
            name: name.into(),
            other: "Disk".into(),
            comparison,
            picks,
            final_newline,
            family,
            size,
            weight,
            line_height: line_height.unwrap_or_else(|| window.line_height()),
            scroll,
            scroller: Scroller::default(),
            starts,
            current,
            hover: None,
            pressed: None,
            focus,
            number_width: px(0.),
            chip_rooms: [COMPARE_CHIP_ROOM; 2],
            header_width: Rc::new(Cell::new(None)),
        };
        view.number_width = view.measure_numbers(window);
        view.chip_rooms = view.measure_chip_rooms(window, cx);
        view
    }

    /// The width of the place this view opens into (its tab's editor), so the first frame picks the header's parts for it; the header's own width replaces it once laid out.
    pub fn seed_width(&self, width: Pixels) {
        self.header_width.set(Some(width));
    }

    /// A view of the editor's text (left) against an old version of the file (right) titled
    /// `when` (its friendly time) instead of "Disk". `comparison` is `Comparison::new(editor text,
    /// version text)`. Use these takes the same road as the on-disk compare.
    pub fn old_version(name: String, when: String, comparison: Comparison, final_newline: bool, line_height: Option<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut view = Self::new(name, comparison, final_newline, line_height, window, cx);
        view.other = when.into();
        view.chip_rooms = view.measure_chip_rooms(window, cx);
        view
    }

    /// How many changes there are.
    pub fn changes(&self) -> usize {
        self.picks.len()
    }

    /// The side each change takes, in order.
    pub fn picks(&self) -> &[Side] {
        &self.picks
    }

    /// The change shown at the top, 1-based, for the header (and the control socket).
    pub fn current(&self) -> Option<usize> {
        self.current.map(|change| change + 1)
    }

    /// The change at the top of the list as it scrolls: the last one that starts at or above the row the context leaves for it.
    fn change_at_top(&self) -> Option<usize> {
        let top = self.scroll.0.borrow().base_handle.logical_scroll_top().0 + COMPARE_CONTEXT_ROWS;
        self.starts.partition_point(|start| *start <= top).checked_sub(1)
    }

    /// Next change and Previous change: scrolls the change after (or before) the current one to the top with
    /// `COMPARE_CONTEXT_ROWS` above it. Returns false at the last (or first) change, where nothing moves.
    pub fn step(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let target = match (self.current, forward) {
            (Some(current), true) => current + 1,
            (None, true) => 0,
            (Some(current), false) => match current.checked_sub(1) {
                Some(target) => target,
                None => return false,
            },
            (None, false) => return false,
        };
        let Some(&row) = self.starts.get(target) else { return false };
        self.scroll.scroll_to_item_strict_with_offset(row, ScrollStrategy::Top, COMPARE_CONTEXT_ROWS);
        self.current = Some(target);
        cx.notify();
        true
    }

    /// Takes `side` for change `change`.
    pub fn pick(&mut self, change: usize, side: Side, cx: &mut Context<Self>) {
        if let Some(pick) = self.picks.get_mut(change).filter(|pick| **pick != side) {
            *pick = side;
            cx.notify();
        }
    }

    /// The line numbers' column: as wide as the biggest number either side shows.
    fn measure_numbers(&self, window: &mut Window) -> Pixels {
        let biggest = SharedString::from(self.comparison.yours.len().max(self.comparison.disk.len()).max(1).to_string());
        let font = Font { family: ".SystemUIFont".into(), features: tabular(), fallbacks: None, weight: FontWeight::NORMAL, style: FontStyle::Normal };
        let run = TextRun { len: biggest.len(), font, color: Hsla::default(), background_color: None, underline: None, strikethrough: None };
        window.text_system().shape_line(biggest, TEXT_SUBHEADLINE, &[run], None).width
    }

    /// The room each side's first change row leaves for its chip: the chip's width (its label and padding), the inset it sits
    /// in and a small gap, never under `COMPARE_CHIP_ROOM`. An old version's time is wider than "Yours" or "Disk".
    fn measure_chip_rooms(&self, window: &mut Window, cx: &App) -> [Pixels; 2] {
        let family = cx.theme().font_family.clone();
        let room = |label: &SharedString| {
            let font = Font { family: family.clone(), features: FontFeatures::default(), fallbacks: None, weight: FontWeight::NORMAL, style: FontStyle::Normal };
            let run = TextRun { len: label.len(), font, color: Hsla::default(), background_color: None, underline: None, strikethrough: None };
            let width = window.text_system().shape_line(label.clone(), TEXT_SUBHEADLINE, &[run], None).width;
            (width + FILTER_PAD_X * 2. + COMPARE_INSET + COMPARE_CHIP_GAP).max(COMPARE_CHIP_ROOM)
        };
        [room(&SharedString::from("Yours")), room(&self.other)]
    }

    /// Takes `side` for every change.
    pub fn pick_all(&mut self, side: Side, cx: &mut Context<Self>) {
        if self.picks.iter().any(|pick| *pick != side) {
            self.picks.fill(side);
            cx.notify();
        }
    }

    pub fn cancel(&mut self, cx: &mut Context<Self>) {
        cx.emit(CompareEvent::Cancel);
    }

    pub fn save_as(&mut self, cx: &mut Context<Self>) {
        cx.emit(CompareEvent::SaveAs);
    }

    /// Use these: the merged text of the picks as they stand.
    pub fn use_these(&mut self, cx: &mut Context<Self>) {
        let (mut text, lines) = self.comparison.merge(&self.picks);
        if self.final_newline && lines > 0 {
            text.push('\n');
        }
        let from_disk = self.picks.contains(&Side::Disk);
        cx.emit(CompareEvent::Use(text, from_disk));
    }

    /// What the right-click menu of a side of change `change` holds: that side's lines to copy, and the side to take.
    fn menu_entries(&self, change: usize, side: Side) -> Vec<Entry<MenuCommand>> {
        let item = |title: String, run: MenuCommand, symbol: &'static str| Item::new(title, run).symbol(symbol);
        let lines = &self.comparison.changes[change];
        let empty = match side {
            Side::Yours => lines.yours.is_empty(),
            Side::Disk => lines.disk.is_empty(),
        };
        vec![
            Entry::Item(item("Copy".into(), MenuCommand::Copy(change, side), "doc.on.doc").disabled(empty)),
            Entry::Separator,
            Entry::Item(item("Take yours".into(), MenuCommand::Take(change, Side::Yours), "arrow.left.square").disabled(self.picks[change] == Side::Yours)),
            Entry::Item(item(format!("Take {}", self.other), MenuCommand::Take(change, Side::Disk), "arrow.right.square").disabled(self.picks[change] == Side::Disk)),
        ]
    }

    /// Opens the right-click menu of a side of a change (a native one) once the frame has drawn: nothing draws while a native
    /// menu is open, and a callback runs at the start of the frame after the one it was queued in, so two frames on.
    fn show_menu(&mut self, change: usize, side: Side, window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.menu_entries(change, side);
        let this = cx.weak_entity();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                window
                    .spawn(cx, async move |cx| {
                        let Some(command) = native_menu::pop_up(&entries, None) else { return };
                        this.update(cx, |this, cx| match command {
                            MenuCommand::Copy(change, side) => {
                                this.copy(change, side, cx);
                            }
                            MenuCommand::Take(change, side) => this.pick(change, side, cx),
                        })
                        .ok();
                    })
                    .detach();
            });
        });
    }

    /// Copy: puts the lines `side` has in change `change` (0-based) on the clipboard, joined with line breaks, and returns them.
    /// A side with no lines in the change (the other side added or removed them) copies nothing.
    pub fn copy(&self, change: usize, side: Side, cx: &mut App) -> Option<String> {
        let change = self.comparison.changes.get(change)?;
        let (lines, range) = match side {
            Side::Yours => (&self.comparison.yours, change.yours.clone()),
            Side::Disk => (&self.comparison.disk, change.disk.clone()),
        };
        if range.is_empty() {
            return None;
        }
        let text = lines[range].iter().map(|line| &**line).collect::<Vec<_>>().join("\n");
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        Some(text)
    }

    /// The rows `range` of the list, one element each.
    fn draw_rows(&mut self, range: Range<usize>, window: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme();
        let (border, ui) = (theme.border, theme.font_family.clone());
        let tints = [theme.green.opacity(ALPHA_COMPARE_WASH), theme.red.opacity(ALPHA_COMPARE_WASH)];
        let filler = theme::ink(ALPHA_COMPARE_FILLER, cx);
        let on = theme::ink(ALPHA_COMPARE_CHIP_ON, cx);
        let wash = theme::ink(ALPHA_BUTTON_HOVER, cx);
        let clear = wash.opacity(0.);
        let numbers = theme::ink(ALPHA_COMPARE_NUMBER, cx);
        let rule = px(1. / window.scale_factor());
        let chip_top = (self.line_height - COMPARE_CHIP_HEIGHT) / 2.;
        let mut out = Vec::with_capacity(range.len());
        for ix in range {
            let Some(&row) = self.comparison.rows.get(ix) else { break };
            let first = ix == 0 || self.comparison.rows[ix - 1].change() != row.change();
            let other = self.other.clone();
            let cell = |side: Side, line: Option<usize>, tint: Hsla, name: &'static str| -> AnyElement {
                let text = line.map(|line| match side {
                    Side::Yours => SharedString::from(self.comparison.yours[line].clone()),
                    Side::Disk => SharedString::from(self.comparison.disk[line].clone()),
                });
                let picked = row.change().map(|change| self.picks[change] == side);
                let number = line.map(|line| SharedString::from((line + 1).to_string()));
                let cell = div()
                    .id((name, ix))
                    .relative()
                    .flex()
                    .items_baseline()
                    // Both halves start from half the row, so a wider chip's padding on one side never moves the divider.
                    .flex_1()
                    .flex_basis(relative(0.5))
                    .min_w_0()
                    .h_full()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .px(COMPARE_INSET) // design: `.px` is the horizontal padding method, the value is a token
                    .when(row.change().is_some() && first, |cell| cell.pr(self.chip_rooms[usize::from(side == Side::Disk)]))
                    .when(row.change().is_some(), |cell| cell.bg(if text.is_some() { tint } else { filler }))
                    // The line's number, flush right in a column as wide as the biggest one, blank on filler rows.
                    .child(
                        div()
                            .flex_none()
                            .w(self.number_width)
                            .mr(COMPARE_NUMBER_GAP)
                            .text_right()
                            .font_family(".SystemUIFont")
                            .font_features(tabular())
                            .font_weight(FontWeight::NORMAL)
                            .text_size(TEXT_SUBHEADLINE)
                            .text_color(numbers)
                            .children(number),
                    )
                    // On the chip's row the text is cut with an ellipsis at the room the chip leaves, so a long no-wrap line stops short of it.
                    .child(div().flex_1().min_w_0().whitespace_nowrap().when(row.change().is_some() && first, |text| text.overflow_hidden().text_ellipsis()).children(text));
                let Some(change) = row.change() else { return cell.into_any_element() };
                // The side under the pointer lights as one, over every row of the change; a press shows at once.
                let mine = Some((change, side));
                let lit = self.hover == mine || self.pressed == mine;
                let held = self.pressed == mine;
                let other = other.clone();
                let ui = ui.clone();
                let cell = cell
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.hover = mine;
                        } else if this.hover == mine {
                            this.hover = None;
                        } else {
                            return;
                        }
                        cx.notify();
                    }))
                    .on_mouse_down(MouseButton::Left, cx.listener(move |this, _, _, cx| {
                        this.pressed = mine;
                        cx.notify();
                    }))
                    .on_mouse_up_out(MouseButton::Left, cx.listener(|this, _, _, cx| {
                        this.pressed = None;
                        cx.notify();
                    }))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.pressed = None;
                        this.pick(change, side, cx);
                    }))
                    .on_mouse_down(MouseButton::Right, cx.listener(move |this, _, window, cx| this.show_menu(change, side, window, cx)));
                by_lit(("compare-side", change, side == Side::Yours), lit, move |level| {
                    let level = if held { 1. } else { level };
                    // The unpicked side comes up toward full under the pointer; the picked one is already there.
                    let chip = match picked {
                        Some(true) => on,
                        _ => mix(clear, wash, level),
                    };
                    cell.when(picked == Some(false), |cell| cell.opacity(ALPHA_COMPARE_UNPICKED + (1. - ALPHA_COMPARE_UNPICKED) * level)).when(first, |cell| {
                        cell.child(
                            div()
                                .absolute()
                                .top(chip_top)
                                .right(COMPARE_INSET)
                                .h(COMPARE_CHIP_HEIGHT)
                                .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
                                .flex()
                                .items_center()
                                .rounded(RADIUS_COMPARE_CHIP)
                                .bg(chip)
                                .font_family(ui)
                                .text_size(TEXT_SUBHEADLINE)
                                .line_height(TEXT_SUBHEADLINE_LINE)
                                .child(if side == Side::Yours { SharedString::from("Yours") } else { other }),
                        )
                    })
                })
                .into_any_element()
            };
            out.push(
                div()
                    .flex()
                    .w_full()
                    .h(self.line_height)
                    .child(cell(Side::Yours, row.yours(), tints[0], "compare-yours"))
                    .child(div().flex_none().w(rule).h_full().bg(border))
                    .child(cell(Side::Disk, row.disk(), tints[1], "compare-disk"))
                    .into_any_element(),
            );
        }
        out
    }

    /// One text button of the header; `tip` is its tooltip.
    fn button(&self, id: &'static str, label: impl Into<SharedString>, tip: impl Into<SharedString>, lit: bool, cut: bool, run: impl Fn(&mut Self, &mut Context<Self>) + 'static, cx: &mut Context<Self>) -> AnyElement {
        let wash = theme::ink(1., cx);
        let rest = if lit { crate::explorer::icons::accent().opacity(ALPHA_OPTION_ON) } else { wash.opacity(ALPHA_HOVER) };
        // A cut label is one line that ends in "…" at the room the header leaves it.
        let label = div().when(cut, |label| label.min_w_0().overflow_hidden().whitespace_nowrap().text_ellipsis()).child(label.into());
        div()
            .flex_none()
            .when(cut, |button| button.flex_shrink(1.).min_w_0().overflow_hidden())
            .h(FIND_FIELD_HEIGHT)
            .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
            .flex()
            .items_center()
            .rounded(RADIUS_BUTTON)
            .bg(rest)
            .text_size(TEXT_SUBHEADLINE)
            .line_height(TEXT_SUBHEADLINE_LINE)
            .child(label)
            .id((id, 0usize))
            .fade_hover((id, 0usize), Some(rest), wash.opacity(ALPHA_BUTTON_HOVER))
            .press_fill(wash.opacity(ALPHA_BUTTON_PRESSED))
            .tooltip(tooltip::tip(tip))
            .tooltip_show_delay(tooltip::DELAY)
            .on_click(cx.listener(move |this, _, _, cx| run(this, cx)))
            .into_any_element()
    }
}

impl Focusable for CompareView {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for CompareView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let (ground, border, ink, ui) = (theme.background, theme.border, theme.foreground, theme.font_family.clone());
        let (primary, muted) = (theme::ink(ALPHA_PRIMARY, cx), theme::ink(ALPHA_SECONDARY, cx));
        let rule = px(1. / window.scale_factor());
        let changes = self.changes();
        let (full, short) = match changes {
            0 => ("No differences".to_string(), "No differences".to_string()),
            1 => ("1 change".to_string(), "1 change".to_string()),
            n => match self.current {
                Some(current) => (format!("{} of {n} changes", current + 1), format!("{} of {n}", current + 1)),
                None => (format!("{n} changes"), format!("{n} changes")),
            },
        };
        let (take_yours, take_other) = (["Take all yours".to_string(), "All yours".to_string()], [format!("Take all from {}", self.other), format!("All from {}", self.other)]);
        // What the header shows beside the name, widest first; the first set that fits with the name's least width wins. The count shortens, then the take-all labels
        // shorten, then the count goes, and last the old copy's label is cut with an ellipsis. The name keeps its room all the way.
        let measure = |text: &str, size: Pixels, features: FontFeatures, window: &mut Window| {
            let text = SharedString::from(text.to_string());
            let font = Font { family: ui.clone(), features, fallbacks: None, weight: FontWeight::NORMAL, style: FontStyle::Normal };
            let run = TextRun { len: text.len(), font, color: Hsla::default(), background_color: None, underline: None, strikethrough: None };
            window.text_system().shape_line(text, size, &[run], None).width
        };
        let button_width = |label: &str, window: &mut Window| measure(label, TEXT_SUBHEADLINE, FontFeatures::default(), window) + FILTER_PAD_X * 2.;
        let steady = ["Cancel", "Save as…", "Use these"].iter().map(|label| button_width(label, window)).fold(px(0.), |sum, width| sum + width);
        let children = if changes > 0 { 7. } else { 5. };
        let room = self.header_width.get().unwrap_or_else(|| window.viewport_size().width) - COMPARE_INSET * 2. - COMPARE_INSET * (children - 1.) - COMPARE_NAME_MIN - steady;
        let takes = |compact: usize, window: &mut Window| if changes > 0 { button_width(&take_yours[compact], window) + button_width(&take_other[compact], window) } else { px(0.) };
        let (full_w, short_w) = (measure(&full, TEXT_BODY, tabular(), window), measure(&short, TEXT_BODY, tabular(), window));
        let (wide, narrow) = (takes(0, window), takes(1, window));
        let (show, compact, cut) = if full_w + wide <= room {
            (full, 0, false)
        } else if short_w + wide <= room {
            (short, 0, false)
        } else if short_w + narrow <= room {
            (short, 1, false)
        } else if narrow <= room {
            (String::new(), 1, false)
        } else {
            (String::new(), 1, true)
        };
        let width = self.header_width.clone();
        let header = div()
            .relative()
            .flex_none()
            .flex()
            .items_center()
            .gap(COMPARE_INSET)
            .h(COMPARE_HEADER_HEIGHT)
            .px(COMPARE_INSET) // design: `.px` is the horizontal padding method, the value is a token
            .font_family(ui.clone())
            .text_size(TEXT_BODY)
            .line_height(TEXT_BODY_LINE)
            .child(
                canvas(
                    move |bounds, window, _| {
                        if width.get() != Some(bounds.size.width) {
                            width.set(Some(bounds.size.width));
                            window.request_animation_frame();
                        }
                    },
                    |_, (), _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(crate::explorer::row::name_label(&self.name, false, primary, None).flex_initial().min_w(COMPARE_NAME_MIN).pl(px(0.)).font_weight(FontWeight::SEMIBOLD))
            .child(div().flex_1().min_w_0().overflow_hidden().whitespace_nowrap().text_color(muted).font_features(tabular()).child(show))
            .when(changes > 0, |header| {
                header
                    .child(self.button("compare-take-yours", take_yours[compact].clone(), "Keep your text for every change", false, false, |this, cx| this.pick_all(Side::Yours, cx), cx))
                    .child(self.button("compare-take-other", take_other[compact].clone(), "Take the right side for every change", false, cut, |this, cx| this.pick_all(Side::Disk, cx), cx))
            })
            .child(self.button("compare-cancel", "Cancel", tooltip::with_key("Leave Compare", "\u{238b}"), false, false, |this, cx| this.cancel(cx), cx))
            .child(self.button("compare-save-as", "Save as…", "Save your text to a new file", false, false, |this, cx| this.save_as(cx), cx))
            .child(self.button("compare-use", "Use these", tooltip::with_key("Put the picks in the tab", "\u{21a9}"), true, false, |this, cx| this.use_these(cx), cx));
        // Which column is which, over the text.
        let columns = div()
            .flex_none()
            .flex()
            .h(COMPARE_CHIP_HEIGHT + COMPARE_INSET / 2.)
            .items_center()
            .font_family(ui)
            .text_size(TEXT_SUBHEADLINE)
            .line_height(TEXT_SUBHEADLINE_LINE)
            .text_color(muted)
            .child(div().flex_1().px(COMPARE_INSET).child("Yours")) // design: `.px` is the horizontal padding method, the value is a token
            .child(div().flex_none().w(rule))
            .child(div().flex_1().px(COMPARE_INSET).child(self.other.clone())); // design: `.px` is the horizontal padding method, the value is a token
        let rows = uniform_list("compare-rows", self.comparison.rows.len(), cx.processor(Self::draw_rows)).track_scroll(&self.scroll).size_full();
        div()
            .id("compare-view")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| match event.keystroke.key.as_str() {
                "escape" => this.cancel(cx),
                "enter" => this.use_these(cx),
                // Option-Cmd-Down and Up step between changes; the editor isn't focused here, so its bindings don't clash.
                "down" | "up" if event.keystroke.modifiers.platform && event.keystroke.modifiers.alt => {
                    if !this.step(event.keystroke.key == "down", cx) {
                        crate::preview::render::beep();
                    }
                }
                _ => {}
            }))
            .occlude()
            .size_full()
            .flex()
            .flex_col()
            .bg(ground)
            .text_color(ink)
            .child(header)
            .child(div().flex_none().h(rule).bg(border))
            .child(columns)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .overflow_hidden()
                    .child(scrollbar::watch(&self.scroller))
                    .font_family(self.family.clone())
                    .font_weight(FontWeight(self.weight))
                    .text_size(self.size)
                    .line_height(self.line_height)
                    .on_scroll(cx.listener(|_, _: &ScrollEvent, window, cx| {
                        // The list applies the wheel after this runs; the next frame has the new top.
                        let view = cx.entity();
                        window.on_next_frame(move |_, cx| {
                            view.update(cx, |this, cx| {
                                let at_top = this.change_at_top();
                                if this.current != at_top {
                                    this.current = at_top;
                                    cx.notify();
                                }
                            });
                        });
                    }))
                    .child(rows)
                    .child(scrollbar::overlay(Overlay { handle: Rc::new(self.scroll.clone()), scroller: self.scroller.clone(), ground: Some(ground) })),
            )
    }
}
