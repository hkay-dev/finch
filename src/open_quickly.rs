//! Open Quickly (Shift-Cmd-O): a field floating over the top middle of the active pane that finds a
//! file under the tree's root by name. Each key matches the name the way the explorer's filter does
//! (`filter::Index::files`, on the background executor) and lists the best `OPEN_QUICKLY_ROWS`
//! files under the field: icon, name with the matched letters bold, and its folder. Up and Down
//! move the pick (its wash glides between rows), Return or a click opens it as a tab
//! (`OpenQuicklyEvent::Open`), Escape closes. The tree itself is never touched.
//!
//! The root's index is the one the `[[` hub holds (`note_links::share`), read on every core with the tree's ignore
//! rules the first time and kept until a file comes or goes under the root or the hub's idle timer lets it go, so
//! opening again reads nothing. It stands on Go to line's float and capsule and
//! rises and leaves the same way (`Reveal`); the workspace places it and draws it. A walk that hits `INDEX_ENTRY_CAP`
//! stops there, and the list's last row says the files past it aren't listed.

use std::{
    cell::Cell,
    collections::VecDeque,
    fs,
    io::Read as _,
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::Instant,
};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{Escape, InputEvent, InputState, MoveDown, MoveUp, RopeExt as _},
};
use gpui_kit::*;

use crate::{
    encoding,
    explorer::{
        filter::{Index, Pick, index_cut_note},
        icons,
        row::{name_label, path_label},
    },
    float_field::{Growth, Height, Reveal, float_field, selected_wash},
    hover::FadeHover,
    motion::{Fade, Glide, ease_out},
    shake::Shake,
    snippet::snippet,
    theme, tooltip,
    tokens::{
        ALPHA_NAME_DIM, ALPHA_PRIMARY, ALPHA_SECONDARY, FADE_QUICK, FILTER_PAD_X, FIND_PAD, ICON, OPEN_PEEK_BYTES, OPEN_PEEK_CACHED, OPEN_PEEK_GAP, OPEN_PEEK_LINES,
        OPEN_PEEK_MIN_WIDTH, OPEN_PEEK_WIDTH, OPEN_QUICKLY_ROWS, OVERLAY_ENTER_RISE, PEEK_COLUMNS, PEEK_EDGE, PEEK_TEXT_MAX, RADIUS_FLOAT_CAPSULE, RADIUS_ROW, RADIUS_TOOLTIP, ROW_HEIGHT,
        ROW_WASH_INSET_Y, SELECT_RESPONSE, SNIPPET_LINE, TEXT_BODY, TEXT_BODY_LINE, TOOLTIP_PAD_X, TOOLTIP_PAD_Y,
    },
    workspace::Workspace,
};

/// Darwin's `O_NONBLOCK`: a file that turned into a pipe is never waited on (the same guard `encoding` reads with).
const O_NONBLOCK: i32 = 0x0004;

/// The text of one peek line: tabs as four spaces, cut at `PEEK_COLUMNS` characters so a huge line never copies in whole.
fn clip(line: impl Iterator<Item = char>) -> SharedString {
    let line: String = line.filter(|c| !matches!(c, '\n' | '\r')).take(PEEK_COLUMNS).collect();
    SharedString::from(line.replace('\t', "    "))
}

/// The first `OPEN_PEEK_LINES` lines of the file at `path`, read from its first `OPEN_PEEK_BYTES` bytes. `None` for what Open
/// can't show as text: not a regular file, past `OPEN_LIMIT`, binary (`encoding::decode`'s test) or empty.
fn head_lines(path: &Path) -> Option<Arc<[SharedString]>> {
    let length = fs::metadata(path).ok().filter(|meta| meta.is_file() && meta.len() <= encoding::OPEN_LIMIT)?.len();
    // A regular path can become a pipe between stat and open; never wait for a writer.
    let file = fs::OpenOptions::new().read(true).custom_flags(O_NONBLOCK).open(path).ok()?;
    file.metadata().ok().filter(|meta| meta.is_file())?;
    let mut bytes = Vec::with_capacity(OPEN_PEEK_BYTES);
    file.take(OPEN_PEEK_BYTES as u64).read_to_end(&mut bytes).ok()?;
    let cut = length > bytes.len() as u64;
    // A character cut in two at the end of the head isn't part of the text.
    if let Err(error) = std::str::from_utf8(&bytes)
        && error.error_len().is_none()
    {
        bytes.truncate(error.valid_up_to());
    }
    let text = encoding::decode(bytes).ok()?.text;
    let mut rows: Vec<&str> = text.split('\n').collect();
    // The head's last row is a stub when the file goes on, and an empty one after a closing newline.
    if rows.len() > 1 && (cut || rows.last() == Some(&"")) {
        rows.pop();
    }
    let lines: Vec<SharedString> = rows.into_iter().take(OPEN_PEEK_LINES).map(|row| clip(row.chars())).collect();
    (lines.iter().any(|line| !line.is_empty())).then(|| lines.into())
}

/// What the float tells the window.
pub enum OpenQuicklyEvent {
    /// Return or a click on a file: open it as a tab.
    Open(PathBuf),
    /// The float closed (a pick, Escape or a click elsewhere): the keyboard goes back to the editor.
    Done,
}

pub struct OpenQuickly {
    input: Entity<InputState>,
    reveal: Reveal,
    root: PathBuf,
    show_hidden: bool,
    /// The files under `root`: the `[[` hub's index, shared; `None` while it is on its way.
    index: Option<Arc<Index>>,
    /// Bumped by each read, search and close: work that started under an older number drops its result.
    latest: Arc<AtomicU64>,
    task: Option<Task<()>>,
    picks: Vec<Pick>,
    /// The query the picks answer; `None` before the first answer for the text in the field.
    answered: Option<String>,
    /// Return came before the background search answered: the search opens the first pick when it lands.
    commit_waiting: bool,
    /// The list emptied while its growth was still running: the rows it held and the pick, drawn shrinking away from where
    /// they were until `growth` lands at nothing.
    leaving: Option<(Vec<Pick>, usize)>,
    /// The last frame painted had no list under the field.
    bare: bool,
    growth: Option<Growth>,
    /// The list's height easing between row counts while typing narrows or widens it.
    height: Option<Height>,
    selected: usize,
    /// The pick's wash gliding to `selected`, in rows from the top; `None` once it rests there.
    wash: Option<Glide>,
    /// The wash's alpha easing between key and not key; made on first draw.
    key: Option<Fade>,
    /// The sideways shake a Return with nothing to open gives the field, beside the beep.
    shake: Option<Shake>,
    window: AnyWindowHandle,
    /// The first lines of the highlighted file, shown in the peek card; the lines it cross-fades from while `fade` runs
    /// (the time it began). `None` while nothing is highlighted or the file has no text to show.
    card: Option<Arc<[SharedString]>>,
    fading_from: Option<Arc<[SharedString]>>,
    fade: Option<Instant>,
    /// Whether the card is wanted. `card_fade` takes the whole card (fill, border, shadow and text) in and out, so the lines stay in `card` while it leaves.
    card_on: bool,
    card_fade: Fade,
    /// The last few files' first lines read (`None`: not text), oldest first.
    cache: VecDeque<(PathBuf, Option<Arc<[SharedString]>>)>,
    /// The file the peek is heading for, and the task that reads it; a newer highlight replaces the task, which drops its read.
    wanted: Option<PathBuf>,
    peek_task: Option<Task<()>>,
    /// Where the float's body was last laid out, to see whether the peek fits beside it.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    _subscription: Subscription,
}

impl EventEmitter<OpenQuicklyEvent> for OpenQuickly {}

impl OpenQuickly {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Open quickly"));
        crate::control::name_field("open quickly", input.focus_handle(cx));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => this.search(cx),
            InputEvent::PressEnter { .. } => this.commit(window, cx),
            // A click elsewhere ends it, but never the window losing the key (the field is still the focus then).
            InputEvent::Blur if this.reveal.open && crate::control::window_key(window) => this.shut(window, cx),
            _ => {}
        });
        // Created at the first Shift-Cmd-O, so it starts shut and the first `open` turns it toward 1 from 0.
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        Self {
            input,
            reveal: Reveal::shut(now),
            root: PathBuf::new(),
            show_hidden: false,
            index: None,
            latest: Arc::default(),
            task: None,
            picks: Vec::new(),
            answered: None,
            commit_waiting: false,
            leaving: None,
            bare: false,
            growth: None,
            height: None,
            selected: 0,
            wash: None,
            key: None,
            shake: None,
            window: window.window_handle(),
            card: None,
            fading_from: None,
            fade: None,
            card_on: false,
            card_fade: Fade::at(0.),
            cache: VecDeque::new(),
            wanted: None,
            peek_task: None,
            bounds: Rc::default(),
            _subscription: subscription,
        }
    }

    /// Shows the float, empty, with the keyboard in it, and reads the files under `root`. Shown
    /// already, it selects what is typed.
    pub fn open(&mut self, root: PathBuf, show_hidden: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.reveal.open {
            self.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
            return;
        }
        self.reveal.turn(true, crate::motion::drawn_now(window.window_handle().window_id()));
        (self.root, self.show_hidden) = (root, show_hidden);
        self.forget();
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        self.read(cx);
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.reveal.open
    }

    /// Drops the list, the index and any work under way.
    fn forget(&mut self) {
        self.latest.fetch_add(1, Relaxed);
        (self.index, self.task, self.answered, self.commit_waiting) = (None, None, None, false);
        (self.leaving, self.growth, self.bare) = (None, None, false);
        (self.wanted, self.peek_task, self.card, self.fading_from, self.fade) = (None, None, None, None, None);
        (self.card_on, self.card_fade) = (false, Fade::at(0.));
        self.cache.clear();
        self.picks.clear();
        self.selected = 0;
    }

    /// Gets the index in the background, then answers what was typed meanwhile. The index is the one the `[[` hub holds for
    /// the root (its walk, when it holds none, is this one's), so the next open reads nothing unless a file came or went.
    /// A window with no workspace on the root reads for itself.
    fn read(&mut self, cx: &mut Context<Self>) {
        let latest = self.latest.clone();
        let number = latest.load(Relaxed);
        let (root, show_hidden, window) = (self.root.clone(), self.show_hidden, self.window);
        self.task = Some(cx.spawn(async move |this, cx| {
            // Asked from a task: the workspace that opened the float is being updated, so it can't be read until that ends.
            let shared = cx.update_window(window, |_, window, cx| crate::note_links::share(&root, show_hidden, window, cx)).ok().flatten();
            let held = match shared {
                Some(waiting) => waiting.await.ok().flatten(),
                None => None,
            };
            let index = match held {
                Some(index) => index,
                None => {
                    let quit = latest.clone();
                    let read = cx.background_executor().spawn(async move { Index::read(&root, show_hidden, &|| quit.load(Relaxed) != number) }).await;
                    let Some(index) = read else { return };
                    Arc::new(index)
                }
            };
            this.update(cx, |this, cx| {
                if this.latest.load(Relaxed) == number {
                    this.index = Some(index);
                    this.search(cx);
                }
            })
            .ok();
        }));
    }

    /// Matches the field's text against the index in the background; a newer key stops it. With
    /// the index still on its way, the read's end runs this.
    fn search(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.index.clone() else { return };
        let query = self.input.read(cx).value().trim().to_string();
        self.commit_waiting = false;
        let number = self.latest.fetch_add(1, Relaxed) + 1;
        if query.is_empty() {
            self.task = None;
            self.show(query, Vec::new(), cx);
            return;
        }
        let (latest, root) = (self.latest.clone(), self.root.clone());
        self.task = Some(cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn({
                    let query = query.clone();
                    async move { index.files(&root, &query, OPEN_QUICKLY_ROWS, &|| latest.load(Relaxed) != number) }
                })
                .await;
            let Some(picks) = found else { return };
            let waiting = this
                .update(cx, |this, cx| {
                    if this.latest.load(Relaxed) != number {
                        return false;
                    }
                    this.show(query, picks, cx);
                    std::mem::take(&mut this.commit_waiting)
                })
                .unwrap_or(false);
            if waiting {
                let window = this.read_with(cx, |this, _| this.window).ok();
                if let Some(window) = window {
                    window.update(cx, |_, window, cx| this.update(cx, |this, cx| this.commit(window, cx)).ok()).ok();
                }
            }
        }));
    }

    /// Lists `picks` as the answer to `query`, the first one picked and its wash there at once.
    fn show(&mut self, query: String, picks: Vec<Pick>, cx: &mut Context<Self>) {
        // Emptied while the list was still growing: it turns round and shrinks away from where it is drawn.
        let now = crate::motion::drawn_now(self.window.window_id());
        if picks.is_empty() && query.is_empty() && !self.picks.is_empty() && let Some(growth) = self.growth.as_mut().filter(|growth| !growth.is_done(now) && !growth.leaving()) {
            growth.aim(0., now);
            self.leaving = Some((std::mem::take(&mut self.picks), self.selected));
        }
        if !picks.is_empty() {
            self.leaving = None;
        }
        (self.picks, self.answered, self.selected, self.wash) = (picks, Some(query), 0, None);
        self.want(cx);
        cx.notify();
    }

    /// Starts reading the highlighted file's first lines, dropping a read for the one the highlight left. A tab open on the file
    /// gives its in-memory text; else the last few reads answer at once; else the file's head is read on the background executor.
    fn want(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.picks.get(self.selected).map(|pick| pick.path.clone()) else {
            (self.wanted, self.peek_task, self.fading_from, self.fade, self.card_on) = (None, None, None, None, false);
            return;
        };
        if self.wanted.as_ref() == Some(&path) {
            return;
        }
        self.wanted = Some(path.clone());
        self.peek_task = Some(cx.spawn(async move |this, cx| {
            let open = this.update(cx, |this, cx| this.open_lines(&path, cx)).ok().flatten();
            let lines = match open {
                Some(lines) => Some(lines),
                None => match this.update(cx, |this, _| this.cache.iter().find(|(kept, _)| *kept == path).map(|(_, lines)| lines.clone())).ok().flatten() {
                    Some(lines) => lines,
                    None => {
                        let read = cx.background_executor().spawn({
                            let path = path.clone();
                            async move { head_lines(&path) }
                        });
                        let lines = read.await;
                        this.update(cx, |this, _| {
                            this.cache.push_back((path.clone(), lines.clone()));
                            if this.cache.len() > OPEN_PEEK_CACHED {
                                this.cache.pop_front();
                            }
                        })
                        .ok();
                        lines
                    }
                },
            };
            this.update(cx, |this, cx| this.arrive(&path, lines, cx)).ok();
        }));
    }

    /// The first lines of the tab this window holds open on `path`, from its editor's text; `None` when no tab shows it or its editor isn't built.
    fn open_lines(&self, path: &Path, cx: &App) -> Option<Arc<[SharedString]>> {
        let workspace = self.window.read(cx, |root: Entity<gpui_kit::base::Root>, cx| root.read(cx).view().clone().downcast::<Workspace>().ok()).ok().flatten()?;
        let documents = workspace.read(cx).documents(cx);
        let editor = documents.iter().find_map(|document| {
            let document = document.read(cx);
            document.path.as_deref().filter(|open| *open == path).and(document.built_editor().cloned())
        })?;
        let rope = editor.read(cx).text();
        let lines = (0..rope.lines_len().min(OPEN_PEEK_LINES)).map(|row| clip(rope.slice_line(row).chars())).collect::<Vec<_>>();
        Some(lines.into())
    }

    /// A read has come back for `path`: the card cross-fades to it if the highlight is still there (`None`: nothing to show).
    fn arrive(&mut self, path: &Path, lines: Option<Arc<[SharedString]>>, cx: &mut Context<Self>) {
        if self.wanted.as_deref() != Some(path) {
            return;
        }
        match lines {
            Some(lines) => {
                let now = crate::motion::drawn_now(self.window.window_id());
                // A card already showing (or fading out) crossfades its text; none showing fades in whole with its text at once.
                if self.card.is_some() && self.card_fade.level(now) > 0. {
                    self.fading_from = self.card.replace(lines);
                    self.fade = Some(now);
                } else {
                    (self.card, self.fading_from, self.fade) = (Some(lines), None, None);
                }
                self.card_on = true;
            }
            None => (self.card_on, self.fading_from, self.fade) = (false, None, None),
        }
        cx.notify();
    }

    /// Up or Down: moves the pick one row, stopping at the ends; the wash glides there from where it is drawn.
    fn step(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.picks.len().saturating_sub(1);
        let to = if down { (self.selected + 1).min(last) } else { self.selected.saturating_sub(1) };
        self.pick(to, window, cx);
    }

    fn pick(&mut self, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        if to == self.selected {
            return;
        }
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let (from, speed) = self.wash.map_or((self.selected as f32, 0.), |wash| (wash.level(now), wash.velocity(now)));
        self.selected = to;
        self.wash = Some(Glide::over(from, to as f32, SELECT_RESPONSE, now).with_speed(speed));
        self.want(cx);
        cx.notify();
    }

    /// Return: opens the pick and closes. Text the background search hasn't answered yet makes
    /// Return wait: the search opens the first pick when it lands, so a fast Return opens what was
    /// typed and nothing is matched here on the main thread. With no match, it beeps and stays.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.input.read(cx).value().trim().to_string();
        if self.answered.as_ref() != Some(&query) && self.index.is_some() {
            self.commit_waiting = true;
            return;
        }
        match self.picks.get(self.selected).filter(|_| self.answered.as_ref() == Some(&query)) {
            Some(pick) => {
                cx.emit(OpenQuicklyEvent::Open(pick.path.clone()));
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
            cx.emit(OpenQuicklyEvent::Done);
        }
    }

    /// Turns the reveal shut and lets the index go. The keyboard stays where it went: a blur closes
    /// the float because a click or a key put it elsewhere, so it never takes it back.
    fn shut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reveal.turn(false, crate::motion::drawn_now(window.window_handle().window_id()));
        self.latest.fetch_add(1, Relaxed);
        (self.index, self.task, self.peek_task, self.commit_waiting) = (None, None, None, false);
        cx.notify();
    }

    /// For the control socket: types `text` into the open float and presses Return, as keys would
    /// (reading the folder first if that is still under way). Replies with the file it opened.
    pub fn submit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<PathBuf, String> {
        if self.index.is_none() {
            self.latest.fetch_add(1, Relaxed);
            self.task = None;
            self.index = Some(Arc::new(Index::read(&self.root, self.show_hidden, &|| false).ok_or("the folder couldn't be read")?));
        }
        self.input.update(cx, |input, cx| input.set_value(text.to_string(), window, cx));
        // The control socket wants the answer now, so this one matches here instead of waiting.
        let query = self.input.read(cx).value().trim().to_string();
        if self.answered.as_ref() != Some(&query)
            && let Some(index) = self.index.clone()
        {
            self.latest.fetch_add(1, Relaxed);
            self.task = None;
            let picks = index.files(&self.root, &query, OPEN_QUICKLY_ROWS, &|| false).unwrap_or_default();
            self.show(query, picks, cx);
        }
        self.commit(window, cx);
        match (self.reveal.open, self.picks.get(self.selected)) {
            (false, Some(pick)) => Ok(pick.path.clone()),
            _ => Err("no file matches".into()),
        }
    }
}

/// The list's last row when the file index stopped at its cap: says so in plain words, cut with an ellipsis in a narrow float.
fn cut_row(muted: Hsla) -> Div {
    div().h(ROW_HEIGHT).px(FILTER_PAD_X).flex().items_center().text_color(muted).child(div().min_w_0().truncate().child(index_cut_note()))
}

impl Render for OpenQuickly {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let (level, moving) = self.reveal.level(now);
        if self.wash.is_some_and(|wash| wash.is_done(now)) {
            self.wash = None;
        }
        if moving || self.wash.is_some() {
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

        // The peek beside the list: the highlighted file's first lines, crossfading from the last card; frames only while it runs.
        let mix = self.fade.map(|since| (now.saturating_duration_since(since).as_secs_f32() / FADE_QUICK).min(1.));
        if mix.is_some_and(|mix| mix < 1.) {
            window.request_animation_frame();
        } else {
            (self.fade, self.fading_from) = (None, None);
        }
        self.card_fade.aim(f32::from(self.card_on), now);
        let card_alpha = self.card_fade.level(now);
        if self.card_fade.is_moving(now) {
            window.request_animation_frame();
        } else if !self.card_on {
            (self.card, self.fading_from, self.fade) = (None, None, None);
        }
        let bounds = self.bounds.get();
        let room = f32::from(window.viewport_size().width - bounds.right()) - OPEN_PEEK_GAP - PEEK_EDGE;
        let peek = self.card.as_ref().filter(|_| bounds.size.width > px(0.) && room >= OPEN_PEEK_MIN_WIDTH).map(|lines| {
            let editor = &crate::settings::get(cx).editor;
            let family = editor.font_family.clone().map(SharedString::from).unwrap_or_else(|| theme.mono_font_family.clone());
            let size = editor.font_size.map(px).unwrap_or(theme.mono_font_size).min(px(PEEK_TEXT_MAX));
            let (text, tint) = (theme.foreground, Hsla { a: 0., ..ink });
            let layer = |lines: &Arc<[SharedString]>, alpha: f32| {
                div().absolute().top_0().left_0().right_0().opacity(alpha).child(snippet(lines, usize::MAX, family.clone(), size, text, tint))
            };
            let level = mix.map_or(1., ease_out);
            tooltip::float(RADIUS_TOOLTIP, fill, border, light, false)
                .absolute()
                .opacity(card_alpha)
                .top_0()
                .left(bounds.size.width + px(OPEN_PEEK_GAP))
                .w(px(room.min(OPEN_PEEK_WIDTH)))
                .px(px(TOOLTIP_PAD_X))
                .py(px(TOOLTIP_PAD_Y))
                .child(
                    div()
                        .relative()
                        .h(size * SNIPPET_LINE * OPEN_PEEK_LINES as f32)
                        .overflow_hidden()
                        .children(self.fading_from.as_ref().filter(|_| mix.is_some()).map(|from| layer(from, 1. - level)))
                        .child(layer(lines, level)),
                )
        });

        // The list under the field: nothing until the text has an answer, a line when nothing matches.
        let (shown, selected) = match &self.leaving {
            Some((picks, selected)) => (picks.as_slice(), *selected),
            None => (self.picks.as_slice(), self.selected),
        };
        // The walk stopped at its cap: the list's last row says so (never on the emptied list that is leaving).
        let cut = self.leaving.is_none() && self.index.as_ref().is_some_and(|index| index.capped());
        let mut list = match &self.answered {
            Some(query) if !query.is_empty() && shown.is_empty() => Some(
                div()
                    .child(
                        div()
                            .h(ROW_HEIGHT)
                            .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
                            .flex()
                            .items_center()
                            .text_color(muted)
                            .child("No files match"),
                    )
                    .children(cut.then(|| cut_row(muted)))
                    .into_any_element(),
            ),
            Some(_) if !shown.is_empty() => {
                let rows = shown.iter().enumerate().map(|(ix, pick)| {
                    div()
                        .id(("open-quickly-row", ix))
                        .fade_row(("open-quickly-row", ix), ink, light, ix == selected)
                        .h(ROW_HEIGHT)
                        .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
                        .flex()
                        .items_center()
                        .child(div().flex_none().child(icons::file(&pick.name, f32::from(ICON), muted)))
                        .child(name_label(&pick.name, false, primary.opacity(ALPHA_NAME_DIM), Some((&pick.hits, primary))).flex_initial())
                        .child(path_label(&pick.folder, muted))
                        .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.pick(ix, window, cx);
                            this.commit(window, cx);
                        }))
                });
                let row = self.wash.map_or(selected as f32, |wash| wash.level(now));
                let top = ROW_HEIGHT * row + ROW_WASH_INSET_Y;
                Some(
                    div()
                        .relative()
                        .child(div().absolute().left_0().right_0().top(top).h(ROW_HEIGHT - ROW_WASH_INSET_Y * 2.).rounded(RADIUS_ROW).bg(wash))
                        .children(rows)
                        .children(cut.then(|| cut_row(muted)))
                        .into_any_element(),
                )
            }
            _ => None,
        };
        // A list that goes from no rows to rows grows the float to its height and fades in, starting at the first frame it is
        // painted; later typing that only changes the rows lands at once. A list emptied mid-growth shrinks away from where it
        // is drawn (`leaving`), and a letter typed again turns it round. Frames only while it moves.
        let rows = shown.len().max(1) + usize::from(cut && list.is_some());
        if list.is_some() && self.leaving.is_none() {
            match self.growth.as_mut() {
                Some(growth) => growth.aim(1., now),
                None if self.bare => self.growth = Some(Growth::start(now)),
                None => {}
            }
        }
        if self.growth.as_ref().is_some_and(|growth| growth.is_done(now)) {
            self.growth = None;
            if self.leaving.take().is_some() {
                list = None;
            }
        }
        self.bare = list.is_none();
        // The list's height eases between row counts (typing that narrows or widens it), the rows laid out whole and the clip
        // moving; growth and leaving run their own height and take it at once.
        let drawn = match (&list, self.growth.is_some()) {
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
        let list = match (&self.growth, list) {
            (Some(growth), Some(list)) => {
                window.request_animation_frame();
                Some(growth.wrap(list, drawn, now))
            }
            (None, Some(list)) if moving => {
                window.request_animation_frame();
                Some(Height::clip(list, drawn))
            }
            (_, list) => list,
        };

        let body = tooltip::float(RADIUS_FLOAT_CAPSULE, fill, border, light, false)
            .id("open-quickly")
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
            .child({
                let cell = self.bounds.clone();
                canvas(
                    move |bounds, _, _| cell.set(bounds),
                    |bounds, _, window, _| {
                        if crate::control::rects_enabled() {
                            // `rects`: the card as drawn, so its height easing between row counts can be read.
                            crate::control::record(window, "open-quickly", bounds);
                        }
                    },
                )
                .absolute()
                .size_full()
            })
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
        let place = div().relative().left(swing.unwrap_or_default()).mt(px(OVERLAY_ENTER_RISE * (1. - level))).opacity(level).child(body).children(peek);
        if self.reveal.open {
            place.into_any_element()
        } else {
            // A leaving float takes no clicks and no focus: a cover over it eats them until it is gone.
            place.child(div().id("open-quickly-cover").absolute().inset_0().occlude()).into_any_element()
        }
    }
}
