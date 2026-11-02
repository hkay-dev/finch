//! Go to Symbol (Cmd-Ctrl-J): a field floating over the top middle of the active pane that finds a
//! function, type, Markdown heading, TOML table or top-level YAML key of the open file by name. It
//! stands on Open Quickly's float, field and rows and ranks the same way (`filter::rank`): the list
//! is `OPEN_QUICKLY_ROWS` tall and scrolls under the wheel and the arrows, with the scrollbar overlay's
//! edge fade showing while more rows wait.
//!
//! The list is the file's outline (`outline`), built on the background executor from a snapshot when
//! the field opens; nothing runs before that. As the pick moves (the arrows, or a new best match while
//! typing) the editor behind it moves its caret to the item's line, and the editor's own reveal glides
//! a far jump. Escape puts the caret and the scroll back where they were, Return keeps the caret on
//! the line, and a click elsewhere leaves the editor where it is.

use std::{
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{EditorState, Escape, InputEvent, InputState, MoveDown, MoveUp, Position, RopeExt as _},
};
use gpui_kit::*;
use tree_sitter::Tree;

use crate::{
    explorer::{
        filter::{self, needle},
        row::name_label,
    },
    float_field::{Arrival, Height, Reveal, float_field, selected_wash},
    hover::FadeHover,
    motion::{Fade, Glide},
    outline::{self, Kind, Symbol},
    scrollbar::{self, Overlay, Scroller},
    shake::Shake,
    symbols, theme, tooltip,
    tokens::{
        ALPHA_NAME_DIM, ALPHA_PRIMARY, ALPHA_SECONDARY, FILTER_PAD_X, FILTER_SYMBOL, FIND_PAD, OPEN_QUICKLY_ROWS, OVERLAY_ENTER_RISE, RADIUS_FLOAT_CAPSULE, RADIUS_ROW, ROW_GAP,
        ROW_HEIGHT, ROW_WASH_INSET_Y, SELECT_RESPONSE, TEXT_BODY, TEXT_BODY_LINE, tabular,
    },
};

/// Most matches kept for a query.
const MAX_MATCHES: usize = 500;

/// What the field tells the window.
pub enum GoToSymbolEvent {
    /// The field closed (a pick, Escape or a click elsewhere): the keyboard goes back to the editor.
    Done,
}

/// The SF Symbol for a kind (each is drawn at launch: `symbols::LAUNCH`).
pub fn symbol_for(kind: Kind) -> &'static str {
    match kind {
        Kind::Function => "function",
        Kind::Struct => "s.square",
        Kind::Enum => "e.square",
        Kind::Trait => "t.square",
        Kind::Impl => "i.square",
        Kind::Class => "c.square",
        Kind::Heading => "number",
        Kind::Table => "tablecells",
        Kind::Key => "key",
    }
}

/// One listed symbol: its place in the outline and the characters of its name the query matched.
struct Row {
    ix: usize,
    hits: Vec<usize>,
}

/// Where the editor was when the field opened, to go back to on Escape.
struct Saved {
    cursor: usize,
    scroll: Point<Pixels>,
    /// Whether the caret's line was on screen: if not, the scroll is put back exactly, whatever the distance.
    on_screen: bool,
}

/// Every symbol of `symbols` matching `query` the way Open Quickly matches a name, best first (all of them in file order for an empty query).
fn rank(symbols: &[Symbol], query: &str) -> Vec<Row> {
    let needle = needle(query);
    if needle.is_empty() {
        return symbols.iter().enumerate().map(|(ix, _)| Row { ix, hits: Vec::new() }).collect();
    }
    let mut folded = Vec::new();
    let mut found: Vec<((u8, u32, usize), Vec<usize>)> = symbols
        .iter()
        .enumerate()
        .filter_map(|(ix, symbol)| filter::rank(&needle, &symbol.name, &mut folded).map(|(tier, len, hits)| ((tier, len, ix), hits)))
        .collect();
    if found.len() > MAX_MATCHES {
        found.select_nth_unstable_by_key(MAX_MATCHES, |(key, _)| *key);
        found.truncate(MAX_MATCHES);
    }
    found.sort_unstable_by_key(|(key, _)| *key);
    found.into_iter().map(|((.., ix), hits)| Row { ix, hits }).collect()
}

pub struct GoToSymbol {
    input: Entity<InputState>,
    reveal: Reveal,
    editor: Option<Entity<EditorState>>,
    saved: Option<Saved>,
    /// The outline, `None` while it is being built. Never bigger than `outline::MAX_BYTES` of text.
    symbols: Option<Arc<[Symbol]>>,
    too_big: bool,
    /// Bumped by each build, search and close: work that started under an older number drops its result.
    latest: Arc<AtomicU64>,
    task: Option<Task<()>>,
    rows: Vec<Row>,
    /// The query the rows answer; `None` before the first answer for the text in the field.
    answered: Option<String>,
    selected: usize,
    /// The rows' scroll, and the scrollbar's hand-scroll clock.
    scroll: UniformListScrollHandle,
    scroller: Scroller,
    /// The pick's wash gliding to `selected`, in rows from the top of the list; `None` once it rests there.
    wash: Option<Glide>,
    /// The rows' scroll gliding to the top row that shows the pick: the glide, the top it ends on and the top last set, all in rows.
    scroll_glide: Option<(Glide, f32, f32)>,
    /// The outline just landed: the first answer's list fades in and grows (`arrive` starts it at the next draw, `arrival` runs
    /// it). Typing again drops it.
    landing: bool,
    arrive: bool,
    /// A frame was painted with no list under the field since the float opened.
    bare: bool,
    arrival: Option<Arrival>,
    /// The list's height easing between row counts while typing narrows or widens it.
    height: Option<Height>,
    /// The wash's alpha easing between key and not key; made on first draw.
    key: Option<Fade>,
    /// Whether the editor has been moved by a pick yet; the first answer only selects the symbol the caret is in.
    moved: bool,
    shake: Option<Shake>,
    _subscription: Subscription,
}

impl EventEmitter<GoToSymbolEvent> for GoToSymbol {}

impl GoToSymbol {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Go to symbol"));
        crate::control::name_field("go to symbol", input.focus_handle(cx));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => this.search(cx),
            InputEvent::PressEnter { .. } => this.commit(window, cx),
            // A click elsewhere ends it, but never the window losing the key (the field is still the focus then).
            InputEvent::Blur if this.reveal.open && crate::control::window_key(window) => {
                // Stepping left the caret on a line a fold may hide: open it, once the editor is not mid-update.
                if let Some(editor) = this.editor.clone() {
                    window.defer(cx, move |_, cx| {
                        editor.update(cx, |state, cx| {
                            let at = state.cursor();
                            let row = state.text().offset_to_point(at).row;
                            state.unfold_at(Position::new(row as u32, 0), cx);
                        });
                    });
                }
                this.shut(window, cx)
            }
            _ => {}
        });
        // Created at the first use, so it starts shut and the first `open` turns it toward 1 from 0.
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        Self {
            input,
            reveal: Reveal::shut(now),
            editor: None,
            saved: None,
            symbols: None,
            too_big: false,
            latest: Arc::default(),
            task: None,
            rows: Vec::new(),
            answered: None,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            scroller: Scroller::default(),
            wash: None,
            scroll_glide: None,
            landing: false,
            arrive: false,
            bare: false,
            arrival: None,
            height: None,
            key: None,
            moved: false,
            shake: None,
            _subscription: subscription,
        }
    }

    /// Shows the field, empty, with the keyboard in it, and builds the outline of `editor`'s text (`lang` is its highlighter language).
    /// Shown already, it selects what is typed.
    pub fn open(&mut self, editor: Entity<EditorState>, lang: &'static str, window: &mut Window, cx: &mut Context<Self>) {
        if self.reveal.open {
            self.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
            return;
        }
        self.reveal.turn(true, crate::motion::drawn_now(window.window_handle().window_id()));
        self.saved = Some(editor.read_with(cx, |state, _| {
            let rows = state.visible_row_range();
            Saved { cursor: state.cursor(), scroll: state.scroll_offset(), on_screen: rows.is_none_or(|rows| rows.contains(&(state.cursor_position().line as usize))) }
        }));
        self.too_big = editor.read(cx).text().len() > outline::MAX_BYTES;
        self.latest.fetch_add(1, Relaxed);
        (self.symbols, self.answered, self.moved) = (None, None, false);
        (self.landing, self.arrive, self.arrival, self.bare) = (false, false, None, false);
        self.rows.clear();
        (self.selected, self.wash, self.scroll_glide) = (0, None, None);
        self.scroll.0.borrow().base_handle.set_offset(point(px(0.), px(0.)));
        self.editor = Some(editor.clone());
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.build(editor, lang, cx);
        cx.notify();
    }

    /// Builds the outline in the background from a snapshot of the text: a text format by its line scan, code from the tree the
    /// highlighter holds (waited for a while if it is behind the text, never parsed here).
    fn build(&mut self, editor: Entity<EditorState>, lang: &'static str, cx: &mut Context<Self>) {
        if self.too_big {
            self.symbols = Some(Arc::from([]));
            return;
        }
        let number = self.latest.load(Relaxed);
        self.task = Some(cx.spawn(async move |this, cx| {
            let mut built = None;
            if outline::is_text_format(lang) {
                let text = cx.update(|cx| editor.read(cx).text().clone());
                built = cx.background_executor().spawn(async move { outline::from_text(lang, &text) }).await;
            } else {
                let snapshot = crate::config_check::when_parsed(&editor, cx, |cx| editor.read(cx).with_current_syntax_tree(|tree, text| tree.downcast_ref::<Tree>().map(|tree| (tree.clone(), text.clone()))).flatten()).await;
                if let Some((tree, text)) = snapshot {
                    built = Some(cx.background_executor().spawn(async move { outline::from_tree(&tree, &text) }).await);
                }
            }
            this.update(cx, |this, cx| {
                if this.latest.load(Relaxed) == number {
                    this.symbols = Some(built.unwrap_or_default().into());
                    this.landing = true;
                    this.search(cx);
                }
            })
            .ok();
        }));
    }

    /// Ranks the outline against the field's text in the background; a newer key drops it. With the outline still on its way, its arrival runs this.
    fn search(&mut self, cx: &mut Context<Self>) {
        let Some(symbols) = self.symbols.clone() else { return };
        let query = self.input.read(cx).value().trim().to_string();
        let number = self.latest.fetch_add(1, Relaxed) + 1;
        let latest = self.latest.clone();
        self.task = Some(cx.spawn(async move |this, cx| {
            let rows = cx
                .background_executor()
                .spawn({
                    let query = query.clone();
                    async move { rank(&symbols, &query) }
                })
                .await;
            this.update(cx, |this, cx| {
                if latest.load(Relaxed) == number {
                    this.land(query, rows, cx);
                }
            })
            .ok();
        }));
    }

    /// Lists `rows` as the answer to `query`. The pick is the best match, or with no query the symbol the caret is in; the editor
    /// follows it, except at the first answer, when it is already there.
    fn show(&mut self, query: String, rows: Vec<Row>, cx: &mut Context<Self>) {
        let at_caret = query.is_empty().then(|| self.caret_line(cx)).flatten();
        self.rows = rows;
        self.answered = Some(query);
        (self.wash, self.scroll_glide) = (None, None);
        (self.arrival, self.arrive) = (None, false);
        self.selected = match (at_caret, &self.symbols) {
            (Some(line), Some(symbols)) => self.rows.iter().rposition(|row| symbols[row.ix].line <= line).unwrap_or(0),
            _ => 0,
        };
        self.show_row(self.selected, true);
        if self.moved {
            self.preview(cx);
        }
        self.moved = true;
        cx.notify();
    }

    /// `show` for the answer that follows the outline landing: its list fades in once.
    fn land(&mut self, query: String, rows: Vec<Row>, cx: &mut Context<Self>) {
        self.show(query, rows, cx);
        self.arrive = std::mem::take(&mut self.landing) && self.bare;
    }

    /// Scrolls the rows to bring row `at` into view a row clear of the edge fade: by the least that does it, or with `center` to the middle.
    fn show_row(&self, at: usize, center: bool) {
        let to = self.top_for(at, center, self.top_row());
        self.scroll.0.borrow().base_handle.set_offset(point(px(0.), -(ROW_HEIGHT * to)));
    }

    /// The row at the top of the list as it is drawn.
    fn top_row(&self) -> f32 {
        -f32::from(self.scroll.0.borrow().base_handle.offset().y) / f32::from(ROW_HEIGHT)
    }

    /// The top row that shows row `at` a row clear of the edge fade from a list whose top is `top`: the least move, or with `center` the middle.
    fn top_for(&self, at: usize, center: bool, top: f32) -> f32 {
        let last_top = self.rows.len().saturating_sub(OPEN_QUICKLY_ROWS) as f32;
        let (row, rows) = (at as f32, OPEN_QUICKLY_ROWS as f32);
        let to = if center {
            at.saturating_sub(OPEN_QUICKLY_ROWS / 2) as f32
        } else if row - 1. < top {
            row - 1.
        } else if row + 2. > top + rows {
            row + 2. - rows
        } else {
            top
        };
        to.clamp(0., last_top)
    }

    /// The line the caret was on when the field opened, counted from 1.
    fn caret_line(&self, cx: &App) -> Option<u32> {
        let (editor, saved) = (self.editor.as_ref()?, self.saved.as_ref()?);
        let rope = editor.read(cx).text();
        Some(rope.offset_to_point(saved.cursor.min(rope.len())).row as u32 + 1)
    }

    /// The symbol under the pick.
    fn picked(&self) -> Option<&Symbol> {
        self.symbols.as_ref().zip(self.rows.get(self.selected)).map(|(symbols, row)| &symbols[row.ix])
    }

    /// Puts the editor's caret at the start of `line` (from 1); the editor reveals it, gliding a far jump.
    /// `open` is a commit: it opens the fold the line sits in and washes the row it lands on once (on arrival when the view
    /// glides there). A preview leaves the fold shut and washes nothing, so arrowing through the list stays quiet.
    fn go(&self, line: u32, open: bool, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref() else { return };
        editor.update(cx, |state, cx| {
            let rope = state.text();
            let row = (line.saturating_sub(1) as usize).min(rope.lines_len().saturating_sub(1));
            let at = rope.line_start_offset(row);
            if open {
                state.unfold_at(Position::new(row as u32, 0), cx);
            }
            state.set_selected_range(at..at, cx);
            if open {
                state.wash_landing(cx);
            } else {
                state.forgo_landing_wash();
            }
        });
    }

    fn preview(&self, cx: &mut Context<Self>) {
        if let Some(line) = self.picked().map(|symbol| symbol.line) {
            self.go(line, false, cx);
        }
    }

    /// Escape's undo: the caret and the scroll the editor had. A scroll near the one it has now glides back on the editor's
    /// reveal spring; a far one is left to the editor's glide to the caret (unless the caret was off screen: then the scroll
    /// goes back exactly).
    fn restore(&self, cx: &mut Context<Self>) {
        let (Some(editor), Some(saved)) = (self.editor.as_ref(), self.saved.as_ref()) else { return };
        editor.update(cx, |state, cx| {
            let at = saved.cursor.min(state.text().len());
            let near = match (state.visible_row_range(), state.line_height()) {
                (Some(rows), Some(height)) => (state.scroll_offset().y - saved.scroll.y).abs() <= height * rows.len() as f32,
                _ => true,
            };
            // A glide running from a preview turns around first, keeping its speed: the selection change below drops it.
            if near {
                state.glide_scroll_offset(saved.scroll, cx);
            }
            state.set_selected_range(at..at, cx);
            state.forgo_landing_wash();
            if near {
                // Again, for a caret that was off screen at the saved scroll: its reveal has just moved the target.
                state.glide_scroll_offset(saved.scroll, cx);
            } else if !saved.on_screen {
                state.set_scroll_offset(saved.scroll, cx);
            }
        });
    }

    /// Up or Down: moves the pick one row, stopping at the ends.
    fn step(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.rows.len().saturating_sub(1);
        let to = if down { (self.selected + 1).min(last) } else { self.selected.saturating_sub(1) };
        self.pick(to, window, cx);
    }

    fn pick(&mut self, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        if to == self.selected || to >= self.rows.len() {
            return;
        }
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let (from, speed) = self.wash.map_or((self.selected as f32, 0.), |wash| (wash.level(now), wash.velocity(now)));
        // The wash glides inside the scrolling rows, so it is as smooth as the pick moves while the list brings the row into view.
        self.wash = Some(Glide::over(from, to as f32, SELECT_RESPONSE, now).with_speed(speed));
        self.selected = to;
        // The rows glide to the top that shows the pick on the same spring, from where they are drawn now.
        let (drawn, target, speed) = match self.scroll_glide {
            Some((glide, target, _)) => (glide.level(now), target, glide.velocity(now)),
            None => {
                let top = self.top_row();
                (top, top, 0.)
            }
        };
        let top = self.top_for(to, false, target);
        if top != drawn || self.scroll_glide.is_some() {
            self.scroll_glide = Some((Glide::over(drawn, top, SELECT_RESPONSE, now).with_speed(speed), top, drawn));
        }
        self.preview(cx);
        cx.notify();
    }

    /// Return: keeps the caret on the pick's line and closes. Text the background ranking hasn't answered yet is answered here
    /// first; with nothing to go to it beeps and stays.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().trim().to_string();
        if self.answered.as_ref() != Some(&query)
            && let Some(symbols) = self.symbols.clone()
        {
            self.latest.fetch_add(1, Relaxed);
            self.task = None;
            let rows = rank(&symbols, &query);
            self.show(query.clone(), rows, cx);
        }
        let line = self.picked().map(|symbol| symbol.line).filter(|_| self.answered.as_ref() == Some(&query));
        match line {
            Some(line) => {
                self.go(line, true, cx);
                self.close(window, cx);
            }
            None => {
                crate::preview::render::beep();
                self.shake = Some(Shake::new(crate::motion::drawn_now(window.window_handle().window_id())));
                cx.notify();
            }
        }
    }

    /// Escape: the editor goes back to the caret and scroll it had, and the field closes.
    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reveal.open {
            self.restore(cx);
        }
        self.close(window, cx);
    }

    /// Leaves the field and gives the keyboard back to the editor.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reveal.open {
            self.shut(window, cx);
            cx.emit(GoToSymbolEvent::Done);
        }
    }

    /// Turns the reveal shut and lets the outline go. The keyboard stays where it went: a blur closes the field because a click or a
    /// key put it elsewhere, so it never takes it back.
    fn shut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reveal.turn(false, crate::motion::drawn_now(window.window_handle().window_id()));
        self.latest.fetch_add(1, Relaxed);
        (self.symbols, self.task, self.editor, self.saved) = (None, None, None, None);
        // A new Vec, not `clear`: the rows' capacity goes with them.
        self.rows = Vec::new();
        cx.notify();
    }
}

impl Render for GoToSymbol {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let (level, moving) = self.reveal.level(now);
        if self.wash.is_some_and(|wash| wash.is_done(now)) {
            self.wash = None;
        }
        // The list's scroll glides on the wash's spring, so list and wash move together. A wheel or a drag of the knob that moved the
        // rows since the last frame takes the scroll over.
        if let Some((glide, to, set)) = self.scroll_glide {
            let base = self.scroll.0.borrow().base_handle.clone();
            let drawn = -f32::from(base.offset().y) / f32::from(ROW_HEIGHT);
            if (drawn - set).abs() > 0.02 {
                self.scroll_glide = None;
            } else if glide.is_done(now) {
                base.set_offset(point(px(0.), -(ROW_HEIGHT * to)));
                self.scroll_glide = None;
            } else {
                let level = glide.level(now);
                base.set_offset(point(px(0.), -(ROW_HEIGHT * level)));
                self.scroll_glide = Some((glide, to, level));
            }
        }
        if moving || self.wash.is_some() || self.scroll_glide.is_some() {
            window.request_animation_frame();
        }
        if !self.reveal.open && level <= 0. {
            return div().into_any_element();
        }
        let (ink, light) = (cx.theme().sidebar_foreground, theme::is_light(cx));
        let wash = selected_wash(&mut self.key, ink, light, window, cx);
        let theme = cx.theme();
        let (fill, border) = tooltip::float_fill(theme.background, ink, light);
        let family = theme.font_family.clone();
        let primary = theme::ink(ALPHA_PRIMARY, cx);
        let muted = theme::ink(ALPHA_SECONDARY, cx);
        let field = float_field(&self.input, Some("magnifyingglass"), 0., cx).w_full();

        // The list under the field: nothing until the outline has an answer, a line when there is nothing to list.
        let list = match (&self.answered, &self.symbols) {
            _ if self.too_big => Some(message("Too big to list symbols", muted)),
            (Some(query), Some(symbols)) if self.rows.is_empty() => Some(message(if query.is_empty() || symbols.is_empty() { "No symbols" } else { "No symbols match" }, muted)),
            (Some(_), Some(_)) => {
                let selected = self.selected;
                // Only the rows in view are built: the list asks for a range, and every row is one `ROW_HEIGHT`.
                let rows = uniform_list(
                    "go-to-symbol-rows",
                    self.rows.len(),
                    cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                        let Some(symbols) = this.symbols.as_ref() else { return Vec::new() };
                        range
                            .filter_map(|at| {
                                let (row, symbol) = (this.rows.get(at)?, &symbols[this.rows.get(at)?.ix]);
                                Some(
                                    div()
                                        .id(("go-to-symbol-row", at))
                                        .fade_row(("go-to-symbol-row", at), ink, light, at == selected)
                                        .w_full()
                                        .h(ROW_HEIGHT)
                                        .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
                                        .flex()
                                        .items_center()
                                        .child(symbols::icon(symbol_for(symbol.kind), FILTER_SYMBOL).text_color(muted).flex_none())
                                        .child(name_label(&SharedString::from(symbol.name.clone()), true, primary.opacity(ALPHA_NAME_DIM), Some((&row.hits, primary))).flex_initial())
                                        .child(div().flex_1())
                                        .child(div().flex_none().pl(ROW_GAP).text_color(muted).font_features(tabular()).child(symbol.line.to_string()))
                                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                                            this.pick(at, window, cx);
                                            this.commit(window, cx);
                                        }))
                                        .into_any_element(),
                                )
                            })
                            .collect()
                    }),
                )
                .track_scroll(&self.scroll)
                .size_full();
                // The pick's wash sits under the rows and follows the list's scroll as it paints, so a wheel never trails it.
                let (shown, scroll) = (self.wash.map_or(self.selected as f32, |wash| wash.level(now)), self.scroll.clone());
                let wash = canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let offset = scroll.0.borrow().base_handle.offset().y;
                        let at = point(bounds.origin.x, bounds.origin.y + offset + ROW_HEIGHT * shown + ROW_WASH_INSET_Y);
                        let rect = Bounds::new(at, size(bounds.size.width, ROW_HEIGHT - ROW_WASH_INSET_Y * 2.));
                        window.paint_quad(gpui_kit::fill(rect, wash).corner_radii(Corners::all(RADIUS_ROW)));
                        if crate::control::rects_enabled() {
                            // `rects`: the wash, and the top of the list's first row (it moves up as the rows scroll).
                            crate::control::record(window, "symbols-wash", rect);
                            crate::control::record(window, "symbols-rows", Bounds::new(point(bounds.origin.x, bounds.origin.y + offset), bounds.size));
                        }
                    },
                )
                .absolute()
                .inset_0();
                // A wheel or a drag of the knob scrolls the rows; the overlay fades the edge while more wait and paints the knob.
                Some(
                    div()
                        .relative()
                        .h(ROW_HEIGHT * self.rows.len().min(OPEN_QUICKLY_ROWS) as f32)
                        .overflow_hidden()
                        .child(scrollbar::watch(&self.scroller))
                        .child(wash)
                        .child(rows)
                        .child(scrollbar::overlay(Overlay { handle: Rc::new(self.scroll.clone()), scroller: self.scroller.clone(), ground: Some(fill) }))
                        .into_any_element(),
                )
            }
            _ => None,
        };
        // The first list after the outline landed fades in and grows the float to its height, but only when a frame was painted
        // without it; an answer before the first frame shows at once. A later answer lands at once.
        self.bare |= list.is_none();
        if std::mem::take(&mut self.arrive) {
            self.arrival = list.is_some().then(|| Arrival::start(self.rows.len().clamp(1, OPEN_QUICKLY_ROWS), now));
        }
        if self.arrival.as_ref().is_some_and(|arrival| arrival.is_done(now)) {
            self.arrival = None;
        }
        // Typing that narrows or widens the list eases its height between row counts, the rows laid out whole and the clip
        // moving; an arrival runs its own height and takes it at once.
        let rows = self.rows.len().clamp(1, OPEN_QUICKLY_ROWS);
        let drawn = match (&list, self.arrival.is_some()) {
            (None, _) => {
                self.height = None;
                rows as f32
            }
            (Some(_), true) => {
                self.height.get_or_insert_with(|| Height::new(rows)).snap(rows);
                rows as f32
            }
            (Some(_), false) => self.height.get_or_insert_with(|| Height::new(rows)).follow(rows, now),
        };
        let moving = self.height.as_ref().is_some_and(Height::is_moving);
        let list = match (&self.arrival, list) {
            (Some(arrival), Some(list)) => {
                window.request_animation_frame();
                Some(arrival.wrap(list, now))
            }
            (None, Some(list)) if moving => {
                window.request_animation_frame();
                Some(Height::clip(list, drawn))
            }
            (_, list) => list,
        };

        let body = tooltip::float(RADIUS_FLOAT_CAPSULE, fill, border, light, false)
            .id("go-to-symbol")
            .occlude()
            .w_full()
            .p(FIND_PAD)
            .flex()
            .flex_col()
            .gap(FIND_PAD)
            .font_family(family)
            .text_size(TEXT_BODY)
            .line_height(TEXT_BODY_LINE)
            .text_color(primary)
            .child(field)
            .children(list)
            .child(
                canvas(
                    |_, _, _| {},
                    |bounds, _, window, _| {
                        if crate::control::rects_enabled() {
                            // `rects`: the card as drawn, so its height easing between row counts can be read.
                            crate::control::record(window, "go-to-symbol", bounds);
                        }
                    },
                )
                .absolute()
                .size_full(),
            )
            // The field answers these itself, ahead of the window's editor.
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.cancel(window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveUp, window, cx| {
                this.step(false, window, cx);
                cx.stop_propagation();
            }))
            .capture_action(cx.listener(|this, _: &MoveDown, window, cx| {
                this.step(true, window, cx);
                cx.stop_propagation();
            }));
        // The shake moves the whole float sideways and asks for frames only while it runs.
        let swing = self.shake.and_then(|shake| shake.offset(now));
        if swing.is_some() {
            window.request_animation_frame();
        } else {
            self.shake = None;
        }
        let place = div().relative().left(swing.unwrap_or_default()).mt(px(OVERLAY_ENTER_RISE * (1. - level))).opacity(level).child(body);
        if self.reveal.open {
            place.into_any_element()
        } else {
            // A leaving float takes no clicks and no focus: a cover over it eats them until it is gone.
            place.child(div().id("go-to-symbol-cover").absolute().inset_0().occlude()).into_any_element()
        }
    }
}

/// A one-row note in the list's place.
fn message(text: &'static str, muted: Hsla) -> AnyElement {
    div().h(ROW_HEIGHT).px(FILTER_PAD_X).flex().items_center().text_color(muted).child(text).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{Kind, Symbol, rank};

    fn symbols(names: &[&str]) -> Vec<Symbol> {
        names.iter().enumerate().map(|(ix, name)| Symbol { kind: Kind::Function, name: name.to_string(), line: ix as u32 + 1 }).collect()
    }

    #[test]
    fn empty_query_lists_every_symbol_in_file_order() {
        let found = rank(&symbols(&["b", "a", "c"]), "");
        assert_eq!(found.iter().map(|row| row.ix).collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn ranks_prefix_then_word_start_then_inside_then_scattered() {
        let list = symbols(&["xx_parse_it", "reparse", "parse_all", "p_a_r_s_e", "other"]);
        let found = rank(&list, "parse");
        assert_eq!(found.iter().map(|row| list[row.ix].name.as_str()).collect::<Vec<_>>(), vec!["parse_all", "xx_parse_it", "reparse", "p_a_r_s_e"]);
        assert_eq!(found[0].hits, vec![0, 1, 2, 3, 4]);
    }
}
