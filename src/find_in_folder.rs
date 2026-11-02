//! Find in folder (Shift-Option-Cmd-F): a field floating over the top middle of the active pane that
//! lists every match of its text in the text files under the tree's root. It stands on Open Quickly's
//! float, field and rows, and on Go to symbol's scrolling list: Up and Down move the pick (its wash
//! glides between rows), Return or a click opens the file in the active pane (or brings its tab
//! forward) and the window lands on the match with Find's halo (`FindInFolderEvent::Open`), Escape
//! closes.
//!
//! The search is `folder_search::search` on the background executor. It starts about
//! `word_count::DEBOUNCE` after the last key, a newer key cancels the run before it (the engine
//! reads its flag between lines), and its hits reach the list in batches, one `BATCH` apart, so the
//! list redraws a few times a second however fast hits come. The three options are the in-file
//! Find's (the editor's search session: the buttons here flip the same ones the find bar and Edit >
//! Find do), and hidden files follow the explorer's Show hidden files.

use std::{
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering::Relaxed},
    },
    time::Duration,
};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{EditorState, Escape, InputEvent, InputState, MoveDown, MoveUp, Position, RopeExt as _},
};
use gpui_kit::*;

use crate::{
    explorer::{
        icons,
        row::{name_label, path_label},
    },
    find_bar::option_button,
    float_field::{Reveal, float_field, selected_wash},
    folder_search::{self, End, FolderQuery, Hit, MAX_HITS},
    hover::FadeHover,
    motion::{Fade, Glide},
    scrollbar::{self, Overlay, Scroller},
    shake::Shake,
    theme, tooltip,
    tokens::{
        ALPHA_NAME_DIM, ALPHA_PRIMARY, ALPHA_SECONDARY, FILTER_PAD_X, FIND_PAD, ICON, OVERLAY_ENTER_RISE, RADIUS_FLOAT_CAPSULE, RADIUS_ROW, ROW_GAP, ROW_HEIGHT, ROW_WASH_INSET_Y,
        SELECT_RESPONSE, TEXT_BODY, TEXT_BODY_LINE, tabular,
    },
};

/// The float's width: a file name, its folder, the line number and a line of context on a row.
pub const FIND_IN_FOLDER_WIDTH: Pixels = px(520.);

/// One hit's row: the file's name and folder on the first line, the line's text on the second.
const HIT_ROW_HEIGHT: Pixels = px(40.);

/// The rows the list shows at once; more scroll.
const HIT_ROWS: usize = 7;

/// How long hits gather before the list takes them.
const BATCH: Duration = Duration::from_millis(80);

/// What the float tells the window.
pub enum FindInFolderEvent {
    /// Return or a click on a hit: open its file and select the match.
    Open(Hit),
    /// The float closed (a pick, Escape or a click elsewhere): the keyboard goes back to the editor.
    Done,
}

/// One listed hit with what its row draws.
struct Row {
    hit: Hit,
    name: SharedString,
    /// The file's folder below the root, "" in the root itself.
    folder: SharedString,
    /// The match's characters in `hit.preview`, for the row's bold.
    marks: Vec<usize>,
    line: SharedString,
}

impl Row {
    fn new(hit: Hit, root: &std::path::Path) -> Self {
        let name = hit.path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let folder = hit.path.parent().and_then(|parent| parent.strip_prefix(root).ok()).map(|below| below.to_string_lossy().into_owned()).unwrap_or_default();
        let (start, end) = (hit.preview_range.start, hit.preview_range.end);
        let before = hit.preview.get(..start).map_or(0, |text| text.chars().count());
        let inside = hit.preview.get(start..end).map_or(0, |text| text.chars().count());
        Self { name: name.into(), folder: folder.into(), marks: (before..before + inside).collect(), line: (hit.line + 1).to_string().into(), hit }
    }
}

/// How the search stands, which decides the line under the list.
enum Status {
    /// Nothing to search for: the field is empty, or there is no folder.
    Idle,
    /// Waiting for the debounce, or reading.
    Searching,
    Done,
    /// The list stopped at `MAX_HITS`.
    Capped,
    /// The pattern doesn't compile: what is wrong with it.
    Bad(SharedString),
}

/// What the background run hands over between batches.
#[derive(Default)]
struct Feed {
    hits: Vec<Hit>,
    end: Option<End>,
}

/// Which of the three options a button flips.
#[derive(Clone, Copy)]
enum Toggle {
    Case,
    Word,
    Regex,
}

pub struct FindInFolder {
    input: Entity<InputState>,
    reveal: Reveal,
    /// The folder searched; `None` with no folder open.
    root: Option<std::path::PathBuf>,
    show_hidden: bool,
    /// The editor whose find options and text this float shares, when the window has one.
    editor: Option<Entity<EditorState>>,
    match_case: bool,
    whole_word: bool,
    regex: bool,
    /// The text last searched, so the field echoing a value this float set doesn't search again.
    queried: String,
    cancel: Arc<AtomicBool>,
    task: Option<Task<()>>,
    rows: Vec<Row>,
    status: Status,
    /// Return came before any hit did: the first hit opens when it lands.
    commit_waiting: bool,
    selected: usize,
    scroll: UniformListScrollHandle,
    scroller: Scroller,
    /// The pick's wash gliding to `selected`, in rows from the top of the list; `None` once it rests there.
    wash: Option<Glide>,
    /// The rows' scroll gliding to the top row that shows the pick: the glide, the top it ends on and the top last set, in rows.
    scroll_glide: Option<(Glide, f32, f32)>,
    /// The wash's alpha easing between key and not key; made on first draw.
    key: Option<Fade>,
    shake: Option<Shake>,
    _subscription: Subscription,
}

impl EventEmitter<FindInFolderEvent> for FindInFolder {}

/// `text` as a regular expression that matches it literally.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

impl FindInFolder {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Find in folder"));
        crate::control::name_field("find in folder", input.focus_handle(cx));
        let subscription = cx.subscribe_in(&input, window, |this, input, event: &InputEvent, window, cx| match event {
            InputEvent::Change => {
                if input.read(cx).value() != this.queried {
                    this.search(true, window, cx);
                }
            }
            InputEvent::PressEnter { .. } => this.commit(window, cx),
            // A click elsewhere ends it, but never the window losing the key (the field is still the focus then).
            InputEvent::Blur if this.reveal.open && crate::control::window_key(window) => this.shut(window, cx),
            _ => {}
        });
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        Self {
            input,
            reveal: Reveal::shut(now),
            root: None,
            show_hidden: false,
            editor: None,
            match_case: false,
            whole_word: false,
            regex: false,
            queried: String::new(),
            cancel: Arc::default(),
            task: None,
            rows: Vec::new(),
            status: Status::Idle,
            commit_waiting: false,
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            scroller: Scroller::default(),
            wash: None,
            scroll_glide: None,
            key: None,
            shake: None,
            _subscription: subscription,
        }
    }

    /// Shows the float with the keyboard in it. The field starts with the editor's selection (a one-line one, as Use Selection
    /// for Find takes it) or else the find bar's text, and the search starts at once. Shown already, it selects what is typed.
    pub fn open(&mut self, root: Option<std::path::PathBuf>, show_hidden: bool, editor: Option<Entity<EditorState>>, window: &mut Window, cx: &mut Context<Self>) {
        if self.reveal.open {
            self.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
            return;
        }
        self.reveal.turn(true, crate::motion::drawn_now(window.window_handle().window_id()));
        (self.root, self.show_hidden, self.editor) = (root, show_hidden, editor);
        self.read_options(cx);
        let seed = self
            .editor
            .as_ref()
            .map(|editor| {
                let state = editor.read(cx);
                let selected = state.selected_text().to_string();
                if selected.is_empty() || selected.contains('\n') {
                    state.search_session().query.clone()
                } else if self.regex {
                    escape(&selected)
                } else {
                    selected
                }
            })
            .unwrap_or_default();
        self.input.update(cx, |input, cx| {
            input.set_value(seed, window, cx);
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        self.search(false, window, cx);
    }

    pub fn is_open(&self) -> bool {
        self.reveal.open
    }

    /// Takes the three options from the editor's search, where the find bar keeps them.
    fn read_options(&mut self, cx: &App) {
        if let Some(editor) = &self.editor {
            let session = editor.read(cx).search_session();
            (self.match_case, self.whole_word, self.regex) = (!session.case_insensitive, session.whole_word, session.regex);
        }
    }

    /// A button was pressed: the editor's option flips (the bar and the Find menu follow), or with no editor the float's own.
    fn toggle(&mut self, option: Toggle, window: &mut Window, cx: &mut Context<Self>) {
        match self.editor.clone() {
            Some(editor) => {
                match option {
                    Toggle::Case => crate::app::toggle_find_option_in(&editor, true, cx),
                    Toggle::Word => crate::app::toggle_find_option_in(&editor, false, cx),
                    Toggle::Regex => crate::app::toggle_find_regex_in(&editor, cx),
                }
                self.read_options(cx);
            }
            None => match option {
                Toggle::Case => self.match_case = !self.match_case,
                Toggle::Word => self.whole_word = !self.whole_word,
                Toggle::Regex => self.regex = !self.regex,
            },
        }
        self.search(false, window, cx);
    }

    /// Stops the run under way and empties the list.
    fn clear(&mut self) {
        self.cancel.store(true, Relaxed);
        (self.task, self.commit_waiting) = (None, false);
        self.rows.clear();
        (self.selected, self.wash, self.scroll_glide) = (0, None, None);
        self.scroll.0.borrow().base_handle.set_offset(point(px(0.), px(0.)));
    }

    fn query(&self, text: String) -> FolderQuery {
        FolderQuery { text, match_case: self.match_case, whole_word: self.whole_word, regex: self.regex, show_hidden: self.show_hidden }
    }

    /// Starts searching the field's text, dropping the run before it. With `debounce` it waits `word_count::DEBOUNCE` first
    /// (a key came), so a word typed fast searches once.
    fn search(&mut self, debounce: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.clear();
        let text = self.input.read(cx).value().to_string();
        self.queried = text.clone();
        self.read_options(cx);
        let Some(root) = self.root.clone().filter(|_| !text.is_empty()) else {
            self.status = Status::Idle;
            cx.notify();
            return;
        };
        self.status = Status::Searching;
        let query = self.query(text);
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = cancel.clone();
        let feed = Arc::new(Mutex::new(Feed::default()));
        let background = cx.background_executor().clone();
        self.task = Some(cx.spawn_in(window, async move |this, cx| {
            if debounce {
                background.timer(crate::word_count::DEBOUNCE).await;
            }
            let worker = background.spawn({
                let (feed, root) = (feed.clone(), root.clone());
                async move {
                    let end = folder_search::search(&root, &query, &cancel, &mut |hit| feed.lock().unwrap().hits.push(hit));
                    feed.lock().unwrap().end = Some(end);
                }
            });
            loop {
                background.timer(BATCH).await;
                let (hits, end) = {
                    let mut feed = feed.lock().unwrap();
                    (std::mem::take(&mut feed.hits), feed.end.clone())
                };
                let finished = end.is_some();
                let landed = this.update_in(cx, |this, window, cx| this.arrive(hits, end, window, cx));
                if landed.is_err() || finished {
                    break;
                }
            }
            drop(worker);
        }));
    }

    /// A batch of hits (and perhaps the end of the run) reached the list.
    fn arrive(&mut self, hits: Vec<Hit>, end: Option<End>, window: &mut Window, cx: &mut Context<Self>) {
        if matches!(end, Some(End::Cancelled)) {
            return;
        }
        self.take(hits, end);
        if std::mem::take(&mut self.commit_waiting) {
            if self.rows.is_empty() && matches!(self.status, Status::Searching) {
                self.commit_waiting = true;
            } else {
                self.commit(window, cx);
            }
        }
        cx.notify();
    }

    /// Adds `hits` to the list and, with the run's `end`, settles the status.
    fn take(&mut self, hits: Vec<Hit>, end: Option<End>) {
        if let Some(root) = self.root.clone() {
            self.rows.extend(hits.into_iter().map(|hit| Row::new(hit, &root)));
        }
        match end {
            Some(End::Done) => self.status = Status::Done,
            Some(End::Capped) => self.status = Status::Capped,
            Some(End::BadPattern(why)) => self.status = Status::Bad(why.into()),
            Some(End::Cancelled) | None => {}
        }
    }

    /// Scrolls the rows to bring row `at` into view a row clear of the edge fade.
    fn top_row(&self) -> f32 {
        -f32::from(self.scroll.0.borrow().base_handle.offset().y) / f32::from(HIT_ROW_HEIGHT)
    }

    /// The top row that shows row `at` a row clear of the edge fade from a list whose top is `top`: the least move.
    fn top_for(&self, at: usize, top: f32) -> f32 {
        let last_top = self.rows.len().saturating_sub(HIT_ROWS) as f32;
        let (row, rows) = (at as f32, HIT_ROWS as f32);
        let to = if row - 1. < top {
            row - 1.
        } else if row + 2. > top + rows {
            row + 2. - rows
        } else {
            top
        };
        to.clamp(0., last_top)
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
        let top = self.top_for(to, target);
        if top != drawn || self.scroll_glide.is_some() {
            self.scroll_glide = Some((Glide::over(drawn, top, SELECT_RESPONSE, now).with_speed(speed), top, drawn));
        }
        cx.notify();
    }

    /// Return: opens the pick and closes. Before the first hit has landed it waits for it; with nothing to open it beeps and stays.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.rows.is_empty() && matches!(self.status, Status::Searching) {
            self.commit_waiting = true;
            return;
        }
        match self.rows.get(self.selected) {
            Some(row) => {
                cx.emit(FindInFolderEvent::Open(row.hit.clone()));
                self.close(window, cx);
            }
            None => {
                crate::preview::render::beep();
                self.shake = Some(Shake::new(crate::motion::drawn_now(window.window_handle().window_id())));
                cx.notify();
            }
        }
    }

    /// Leaves the float and gives the keyboard back to the editor.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reveal.open {
            self.shut(window, cx);
            cx.emit(FindInFolderEvent::Done);
        }
    }

    /// Turns the reveal shut and lets the search and its list go. The keyboard stays where it went: a blur closes the float
    /// because a click or a key put it elsewhere, so it never takes it back.
    fn shut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reveal.turn(false, crate::motion::drawn_now(window.window_handle().window_id()));
        self.clear();
        (self.editor, self.queried) = (None, String::new());
        cx.notify();
    }

    /// For the control socket: types `text` into the open float, searches it here and now, and presses Return. Replies with
    /// the `path:line` it opened (the line from 1).
    pub fn submit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<String, String> {
        self.input.update(cx, |input, cx| input.set_value(text.to_string(), window, cx));
        self.clear();
        self.queried = text.to_string();
        self.read_options(cx);
        let root = self.root.clone().ok_or("Find in folder needs an open folder")?;
        let mut hits = Vec::new();
        let end = folder_search::search(&root, &self.query(text.to_string()), &AtomicBool::new(false), &mut |hit| hits.push(hit));
        self.take(hits, Some(end));
        let opened = self.rows.get(self.selected).map(|row| format!("{}:{}", row.hit.path.display(), row.hit.line + 1));
        self.commit(window, cx);
        opened.filter(|_| !self.reveal.open).ok_or_else(|| "nothing matches".into())
    }
}

/// A one-row note in the list's place.
fn message(text: impl Into<SharedString>, muted: Hsla) -> Div {
    div().h(ROW_HEIGHT).px(FILTER_PAD_X).flex().items_center().text_color(muted).child(text.into())
}

impl Render for FindInFolder {
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
            let drawn = -f32::from(base.offset().y) / f32::from(HIT_ROW_HEIGHT);
            if (drawn - set).abs() > 0.02 {
                self.scroll_glide = None;
            } else if glide.is_done(now) {
                base.set_offset(point(px(0.), -(HIT_ROW_HEIGHT * to)));
                self.scroll_glide = None;
            } else {
                let level = glide.level(now);
                base.set_offset(point(px(0.), -(HIT_ROW_HEIGHT * level)));
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

        // The field and the three options beside it, the find bar's own buttons.
        let key = self.input.entity_id().as_u64() as usize;
        let option = |id: &'static str, symbol: &'static str, tip: String, on: bool, which: Toggle, cx: &mut Context<Self>| {
            option_button(id, key, symbol, tip, on, cx.listener(move |this, _: &ClickEvent, window, cx| this.toggle(which, window, cx)), cx)
        };
        let field = float_field(&self.input, Some("magnifyingglass"), 0., cx).flex_1().min_w_0();
        let head = div()
            .flex()
            .items_center()
            .gap(FIND_PAD / 2.)
            .child(field)
            .child(option("find-in-folder-case", "textformat", tooltip::with_key("Match case", "\u{2325}\u{2318}C"), self.match_case, Toggle::Case, cx))
            .child(option("find-in-folder-word", "textformat.abc.dottedunderline", tooltip::with_key("Whole word", "\u{2325}\u{2318}W"), self.whole_word, Toggle::Word, cx))
            .child(option("find-in-folder-regex", "ellipsis.curlybraces", tooltip::with_key("Regular expression", "\u{2325}\u{2318}R"), self.regex, Toggle::Regex, cx));

        // The list under the field: the hits, or a line saying why there are none; nothing for an empty field.
        let mut notes: Vec<Div> = Vec::new();
        if self.root.is_none() {
            notes.push(message("Find in folder needs an open folder", muted));
        } else {
            match &self.status {
                Status::Bad(why) => notes.push(message(why.clone(), muted)),
                Status::Done if self.rows.is_empty() => notes.push(message("No results", muted)),
                Status::Capped => notes.push(message(format!("Stopped at {MAX_HITS} results"), muted)),
                _ => {}
            }
        }
        let list = (!self.rows.is_empty()).then(|| {
            let selected = self.selected;
            // Only the rows in view are built: the list asks for a range, and every row is one `HIT_ROW_HEIGHT`.
            let rows = uniform_list(
                "find-in-folder-rows",
                self.rows.len(),
                cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                    range
                        .filter_map(|at| {
                            let row = this.rows.get(at)?;
                            Some(
                                div()
                                    .id(("find-in-folder-row", at))
                                    .fade_row(("find-in-folder-row", at), ink, light, at == selected)
                                    .w_full()
                                    .h(HIT_ROW_HEIGHT)
                                    .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .h(ROW_HEIGHT)
                                            .child(div().flex_none().child(icons::file(&row.name, f32::from(ICON), muted)))
                                            .child(name_label(&row.name, false, primary.opacity(ALPHA_NAME_DIM), None).flex_initial())
                                            .child(path_label(&row.folder, muted))
                                            .child(div().flex_none().pl(ROW_GAP).text_color(muted).font_features(tabular()).child(row.line.clone())),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .h(HIT_ROW_HEIGHT - ROW_HEIGHT)
                                            .child(div().w(ICON).flex_none())
                                            .child(name_label(&SharedString::from(row.hit.preview.clone()), true, muted, Some((&row.marks, primary)))),
                                    )
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
                    let at = point(bounds.origin.x, bounds.origin.y + offset + HIT_ROW_HEIGHT * shown + ROW_WASH_INSET_Y);
                    let rect = Bounds::new(at, size(bounds.size.width, HIT_ROW_HEIGHT - ROW_WASH_INSET_Y * 2.));
                    window.paint_quad(gpui_kit::fill(rect, wash).corner_radii(Corners::all(RADIUS_ROW)));
                },
            )
            .absolute()
            .inset_0();
            div()
                .relative()
                .h(HIT_ROW_HEIGHT * self.rows.len().min(HIT_ROWS) as f32)
                .overflow_hidden()
                .child(scrollbar::watch(&self.scroller))
                .child(wash)
                .child(rows)
                .child(scrollbar::overlay(Overlay { handle: Rc::new(self.scroll.clone()), scroller: self.scroller.clone(), ground: Some(fill) }))
        });

        let body = tooltip::float(RADIUS_FLOAT_CAPSULE, fill, border, light, false)
            .id("find-in-folder")
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
            .child(head)
            .children(list)
            .children(notes)
            // The field answers these itself, ahead of the window's editor.
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                this.close(window, cx);
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
            place.child(div().id("find-in-folder-cover").absolute().inset_0().occlude()).into_any_element()
        }
    }
}

/// Lands the window on `hit`: the file open in the active pane (or its tab brought forward), the match selected and given Find's halo.
pub fn land(editor: &Entity<EditorState>, hit: &Hit, window: &mut Window, cx: &mut App) {
    let line = hit.line;
    let editor = editor.clone();
    let range = editor.update(cx, |state, cx| {
        let rope = state.text();
        if line >= rope.lines_len() {
            return None;
        }
        let start = rope.line_start_offset(line);
        let range = start + hit.byte_range.start..start + hit.byte_range.end;
        if range.end > rope.line_end_offset(line) {
            return None;
        }
        state.unfold_at(Position::new(line as u32, 0), cx);
        state.start_search_halo(range.clone(), cx);
        state.set_selected_range(range.clone(), cx);
        Some(range)
    });
    // A document that hasn't drawn has no layout to scroll by: the selection is made again two frames on, if it is still the one made.
    if let Some(range) = range {
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |_, cx| {
                editor.update(cx, |state, cx| {
                    if state.selected_range() == range {
                        state.set_selected_range(range, cx);
                    }
                });
            });
        });
    }
}
