//! File > Open draft…: a float over the top middle of the active pane, in Open quickly's style (its
//! field, reveal, wash and list), that lists the scrap drafts newest first. Each row shows the
//! draft's first line in the editor's font (or "Empty draft"), when it was last changed and its
//! word count. Typing narrows the list to the drafts whose first lines and text hold every word
//! typed. Return or a click opens the pick (`DraftListEvent::Open`; the window opens it as an
//! Untitled page backed by that draft, or brings its tab forward), Escape closes. Cmd-Delete
//! moves the pick to the Trash and its row slides out; a draft open in a tab (in any window) is
//! never trashed, its row says so instead.
//!
//! The drafts folder and the first few KB of each draft are read on the background executor when
//! the float opens (`drafts::list`), never at launch, and let go when it closes.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{DeleteToBeginningOfLine, Escape, InputEvent, InputState, MoveDown, MoveUp},
};
use gpui_kit::*;

use crate::{
    document::page_title,
    drafts::{self, Listed, Local},
    float_field::{Reveal, float_field, selected_wash},
    hover::FadeHover,
    motion::{Fade, Glide},
    scrollbar::{self, Overlay, Scroller},
    theme, tooltip,
    tokens::{
        ALPHA_PRIMARY, ALPHA_SECONDARY, FILTER_PAD_X, FIND_PAD, OPEN_QUICKLY_ROWS, OVERLAY_ENTER_RISE, RADIUS_FLOAT_CAPSULE, RADIUS_ROW, ROW_GAP, ROW_HEIGHT,
        ROW_WASH_INSET_Y, SELECT_RESPONSE, TEXT_BODY, TEXT_BODY_LINE, tabular,
    },
};

/// How long a trashed row takes to slide out and close its gap, in seconds.
const SLIDE: f32 = 0.2;
/// How far a trashed row slides to the right on its way out.
const SLIDE_DISTANCE: f32 = 36.;
const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// What the float tells the window.
pub enum DraftListEvent {
    /// Return or a click on a draft: open it (or bring its tab forward).
    Open(PathBuf),
    /// Cmd-Delete on a draft: the window says whether it is open in a tab now (`trash_checked`).
    Trash(PathBuf),
    /// The float closed (a pick, Escape or a click elsewhere): the keyboard goes back to the editor.
    Done,
}

/// A draft with the words its row and its filter use, made on the background executor.
struct Row {
    draft: Listed,
    title: SharedString,
    empty: bool,
    date: SharedString,
    words: SharedString,
    /// The lowercased head, which the typed words are looked for in.
    haystack: String,
}

/// What the background read hands back.
struct Loaded {
    rows: Vec<Row>,
    /// The resolved paths of the drafts open in tabs (a draft named here is never trashed).
    open: HashSet<PathBuf>,
}

pub struct DraftList {
    input: Entity<InputState>,
    reveal: Reveal,
    /// The drafts, newest first; `None` while the read runs.
    loaded: Option<Loaded>,
    /// Indexes into the rows that the typed words match, in list order.
    shown: Vec<usize>,
    selected: usize,
    /// The rows' scroll, and the scrollbar's hand-scroll clock.
    scroll: UniformListScrollHandle,
    scroller: Scroller,
    /// The pick's wash gliding to `selected`, in rows from the top of `shown`; `None` once it rests there.
    wash: Option<Glide>,
    /// The wash's alpha easing between key and not key; made on first draw.
    key: Option<Fade>,
    /// A pick that is open in a tab was asked to be trashed: its row says it can't be.
    notice: Option<PathBuf>,
    /// A draft in the Trash whose row is sliding out, and when it started.
    leaving: Option<(PathBuf, Instant)>,
    /// The move to the Trash is under way.
    trashing: bool,
    /// Bumped by each read and close: work that started under an older number drops its result.
    latest: Arc<AtomicU64>,
    task: Option<Task<()>>,
    trash_task: Option<Task<()>>,
    _subscription: Subscription,
}

impl EventEmitter<DraftListEvent> for DraftList {}

impl DraftList {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Open draft"));
        crate::control::name_field("open draft", input.focus_handle(cx));
        let subscription = cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
            InputEvent::Change => this.refilter(cx),
            InputEvent::PressEnter { .. } => this.commit(window, cx),
            // A click elsewhere ends it, but never the window losing the key (the field is still the focus then).
            InputEvent::Blur if this.reveal.open && crate::control::window_key(window) => this.shut(window, cx),
            _ => {}
        });
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        Self {
            input,
            reveal: Reveal::shut(now),
            loaded: None,
            shown: Vec::new(),
            selected: 0,
            scroll: UniformListScrollHandle::new(),
            scroller: Scroller::default(),
            wash: None,
            key: None,
            notice: None,
            leaving: None,
            trashing: false,
            latest: Arc::default(),
            task: None,
            trash_task: None,
            _subscription: subscription,
        }
    }

    /// Shows the float, empty, with the keyboard in it, and reads the drafts folder. `open_files` are
    /// the files of the tabs open in every window. Shown already, it selects what is typed.
    pub fn open(&mut self, open_files: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        if self.reveal.open {
            // Tabs may have opened or closed since: the rows' open marks follow.
            if let Some(loaded) = &mut self.loaded {
                loaded.open = open_files.iter().map(|file| file.canonicalize().unwrap_or_else(|_| file.clone())).collect();
            }
            self.input.update(cx, |input, cx| {
                input.focus(window, cx);
                input.select_all(window, cx);
            });
            return;
        }
        self.reveal.turn(true, crate::motion::drawn_now(window.window_handle().window_id()));
        self.forget();
        self.input.update(cx, |input, cx| {
            input.set_value("", window, cx);
            input.focus(window, cx);
        });
        let (latest, folder) = (self.latest.clone(), drafts::folder());
        let number = latest.load(Relaxed);
        self.task = Some(cx.spawn(async move |this, cx| {
            let loaded = cx.background_executor().spawn(async move { read(&folder, &open_files) }).await;
            this.update(cx, |this, cx| {
                if latest.load(Relaxed) == number {
                    this.loaded = Some(loaded);
                    this.refilter(cx);
                }
            })
            .ok();
        }));
        cx.notify();
    }

    pub fn is_open(&self) -> bool {
        self.reveal.open
    }

    /// Drops the list and any work under way.
    fn forget(&mut self) {
        self.latest.fetch_add(1, Relaxed);
        self.release();
        self.task = None;
        self.scroll.0.borrow().base_handle.set_offset(point(px(0.), px(0.)));
    }

    /// Frees the rows and what hangs off them (a new Vec, so no capacity stays behind).
    fn release(&mut self) {
        (self.loaded, self.notice, self.leaving) = (None, None, None);
        (self.shown, self.selected, self.wash) = (Vec::new(), 0, None);
    }

    /// Lists the rows the field's words match, the first one picked and its wash there at once.
    fn refilter(&mut self, cx: &mut Context<Self>) {
        let Some(loaded) = &self.loaded else { return };
        let query = self.input.read(cx).value().to_lowercase();
        self.shown = (0..loaded.rows.len()).filter(|&ix| matches(&loaded.rows[ix].haystack, &query)).collect();
        (self.selected, self.wash, self.notice) = (0, None, None);
        self.scroll.0.borrow().base_handle.set_offset(point(px(0.), px(0.)));
        cx.notify();
    }

    /// Up or Down: moves the pick one row, stopping at the ends.
    fn step(&mut self, down: bool, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.shown.len().saturating_sub(1);
        let to = if down { (self.selected + 1).min(last) } else { self.selected.saturating_sub(1) };
        self.pick(to, window, cx);
    }

    /// Picks row `to` of the list; the wash glides there from where it is drawn, and the rows scroll to keep it in view.
    fn pick(&mut self, to: usize, window: &mut Window, cx: &mut Context<Self>) {
        if to == self.selected {
            return;
        }
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let (from, speed) = self.wash.map_or((self.selected as f32, 0.), |wash| (wash.level(now), wash.velocity(now)));
        self.selected = to;
        self.notice = None;
        // The wash glides inside the scrolling rows, so it is as smooth as the pick moves while the list brings the row into view.
        self.wash = Some(Glide::over(from, to as f32, SELECT_RESPONSE, now).with_speed(speed));
        self.show_row(to);
        cx.notify();
    }

    /// Scrolls the rows by the least that brings row `at` into view a row clear of the edge fade.
    fn show_row(&self, at: usize) {
        let top = -f32::from(self.scroll.0.borrow().base_handle.offset().y) / f32::from(ROW_HEIGHT);
        let last_top = self.shown.len().saturating_sub(OPEN_QUICKLY_ROWS) as f32;
        let (row, rows) = (at as f32, OPEN_QUICKLY_ROWS as f32);
        let to = if row - 1. < top {
            row - 1.
        } else if row + 2. > top + rows {
            row + 2. - rows
        } else {
            top
        };
        self.scroll.0.borrow().base_handle.set_offset(point(px(0.), -(ROW_HEIGHT * to.clamp(0., last_top))));
    }

    /// The row the pick is on, when the list has one and none is on its way out.
    fn picked(&self) -> Option<&Row> {
        let loaded = self.loaded.as_ref()?;
        let row = &loaded.rows[*self.shown.get(self.selected)?];
        (self.leaving.as_ref().map(|(path, _)| path) != Some(&row.draft.path)).then_some(row)
    }

    /// Return: opens the pick and closes; with nothing picked it beeps and stays.
    fn commit(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.picked().map(|row| row.draft.path.clone()) {
            Some(path) => {
                cx.emit(DraftListEvent::Open(path));
                self.close(window, cx);
            }
            None => crate::preview::render::beep(),
        }
    }

    /// Cmd-Delete: asks the window whether the pick is open in a tab now (`DraftListEvent::Trash`), which
    /// answers with `trash_checked`. Replies with the draft asked about, or why not.
    fn trash_picked(&mut self, cx: &mut Context<Self>) -> Result<PathBuf, String> {
        if self.trashing || self.leaving.is_some() {
            return Err("a draft is already on its way to the Trash".into());
        }
        let path = self.picked().map(|row| row.draft.path.clone()).ok_or("no draft is picked")?;
        cx.emit(DraftListEvent::Trash(path.clone()));
        Ok(path)
    }

    /// The window's answer for `path`: open in a tab, so its row says it stays; else it moves to the
    /// Trash in the background and its row slides out when the move lands.
    pub fn trash_checked(&mut self, path: PathBuf, open: bool, cx: &mut Context<Self>) {
        if self.trashing || self.leaving.is_some() {
            return;
        }
        if open {
            self.notice = Some(path);
            cx.notify();
            return;
        }
        self.trashing = true;
        let target = path;
        self.trash_task = Some(cx.spawn(async move |this, cx| {
            let moved = cx.background_executor().spawn({
                let target = target.clone();
                async move { crate::file_ops::trash(&target) }
            });
            let moved = moved.await;
            this.update(cx, |this, cx| {
                this.trashing = false;
                match moved {
                    Ok(_) => this.leaving = Some((target, Instant::now())),
                    Err(err) => {
                        eprintln!("finch: can't move the draft {} to the Trash: {err}", target.display());
                        crate::preview::render::beep();
                    }
                }
                cx.notify();
            })
            .ok();
        }));
    }

    /// The slide has finished: the row leaves the list and the pick stays on the row that came up under it.
    fn finish_leaving(&mut self) {
        let Some((path, _)) = self.leaving.take() else { return };
        let Some(loaded) = &mut self.loaded else { return };
        let Some(at) = loaded.rows.iter().position(|row| row.draft.path == path) else { return };
        loaded.rows.remove(at);
        self.shown.retain(|&ix| ix != at);
        for ix in &mut self.shown {
            if *ix > at {
                *ix -= 1;
            }
        }
        self.selected = self.selected.min(self.shown.len().saturating_sub(1));
        self.show_row(self.selected);
        self.wash = None;
    }

    /// Leaves the float and gives the keyboard back to the editor.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.reveal.open {
            self.shut(window, cx);
            cx.emit(DraftListEvent::Done);
        }
    }

    /// Turns the reveal shut and lets the list go. The keyboard stays where it went: a blur closes
    /// the float because a click or a key put it elsewhere, so it never takes it back.
    fn shut(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reveal.turn(false, crate::motion::drawn_now(window.window_handle().window_id()));
        // The rows stay while the float fades out; `render` lets them go when the fade lands.
        self.latest.fetch_add(1, Relaxed);
        self.task = None;
        cx.notify();
    }

    /// For the control socket: the list as JSON, `{"loaded":true,"selected":N,"rows":[…]}` (the pick
    /// counted from 1, 0 for none), or `{"loaded":false}` while the folder is still being read.
    pub fn snapshot(&self) -> String {
        let Some(loaded) = &self.loaded else { return r#"{"loaded":false}"#.into() };
        let rows: Vec<String> = self
            .shown
            .iter()
            .map(|&ix| {
                let row = &loaded.rows[ix];
                let title = if row.empty { "" } else { row.title.as_ref() };
                format!(
                    r#"{{"title":{title:?},"empty":{},"date":{:?},"words":{:?},"open":{},"path":{:?}}}"#,
                    row.empty,
                    row.date.as_ref(),
                    row.words.as_ref(),
                    loaded.open.contains(&row.draft.real),
                    row.draft.path.display().to_string()
                )
            })
            .collect();
        let selected = if self.shown.is_empty() { 0 } else { self.selected + 1 };
        format!(r#"{{"loaded":true,"selected":{selected},"rows":[{}]}}"#, rows.join(","))
    }

    /// For the control socket: types `text` into the field.
    pub fn filter(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_value(text.to_string(), window, cx));
        self.refilter(cx);
    }

    /// For the control socket: picks row `number` (from 1) of the list.
    pub fn select(&mut self, number: usize, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        if number == 0 || number > self.shown.len() {
            return Err(format!("the list has {} rows", self.shown.len()));
        }
        self.pick(number - 1, window, cx);
        Ok(())
    }

    /// For the control socket: presses Return; replies with the draft it opened.
    pub fn submit(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<PathBuf, String> {
        let path = self.picked().map(|row| row.draft.path.clone()).ok_or("no draft is picked")?;
        self.commit(window, cx);
        Ok(path)
    }

    /// For the control socket: presses Cmd-Delete.
    pub fn trash(&mut self, cx: &mut Context<Self>) -> Result<PathBuf, String> {
        self.trash_picked(cx)
    }
}

/// Reads the drafts in `folder` and makes their rows; `open_files` are resolved to tell which are open.
fn read(folder: &Path, open_files: &[PathBuf]) -> Loaded {
    let seconds = |time: SystemTime| time.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
    let now = seconds(SystemTime::now());
    let (today, yesterday) = (drafts::local(now), drafts::local(now - 24 * 60 * 60));
    let rows = drafts::list(folder)
        .into_iter()
        .map(|mut draft| {
            let line = first_line(&draft.head);
            let date = date_label(drafts::local(seconds(draft.modified)), today, yesterday);
            // The head is read once more here and never again, so its text becomes the haystack instead of being kept beside it.
            let haystack = std::mem::take(&mut draft.head).to_lowercase();
            Row {
                empty: line.is_none(),
                title: line.unwrap_or_default().into(),
                date: date.into(),
                words: words_label(draft.words, draft.more).into(),
                haystack,
                draft,
            }
        })
        .collect();
    let open = open_files.iter().map(|file| file.canonicalize().unwrap_or_else(|_| file.clone())).collect();
    Loaded { rows, open }
}

/// The title a draft's head gives its row (the shared page rule: front matter skipped, `#`s dropped, `#!` as written; cut to 200 characters). A page the rule can't name (front matter and nothing else) shows its first non-blank line; `None` only for a draft with no text.
fn first_line(head: &str) -> Option<String> {
    page_title(head.chars()).map(|title| title.text).or_else(|| {
        let line = head.lines().map(str::trim).find(|line| !line.is_empty())?;
        Some(line.chars().take(200).collect())
    })
}

/// Whether every word of `query` (lowercase) is somewhere in `haystack` (lowercase); no words match everything.
fn matches(haystack: &str, query: &str) -> bool {
    query.split_whitespace().all(|word| haystack.contains(word))
}

/// `Today 14:05`, `Yesterday 14:05`, else the date (`Oct 30`, with the year when it isn't this one's).
fn date_label(then: Local, today: Local, yesterday: Local) -> String {
    let day = |local: Local| (local.year, local.month, local.day);
    if day(then) == day(today) {
        format!("Today {:02}:{:02}", then.hour, then.minute)
    } else if day(then) == day(yesterday) {
        format!("Yesterday {:02}:{:02}", then.hour, then.minute)
    } else if then.year == today.year {
        format!("{} {}", MONTHS[(then.month as usize).clamp(1, 12) - 1], then.day)
    } else {
        format!("{} {}, {}", MONTHS[(then.month as usize).clamp(1, 12) - 1], then.day, then.year)
    }
}

/// `1 word`, `42 words`; `more` marks a count of only the start of a long draft.
fn words_label(words: usize, more: bool) -> String {
    match (words, more) {
        (1, false) => "1 word".into(),
        (n, false) => format!("{n} words"),
        (n, true) => format!("{n}+ words"),
    }
}

impl Render for DraftList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let (level, moving) = self.reveal.level(now);
        if self.wash.is_some_and(|wash| wash.is_done(now)) {
            self.wash = None;
        }
        // A row that has slid out leaves the list this frame; one still sliding is `slide` of the way gone.
        let slide = self.leaving.as_ref().map(|(_, since)| (now.saturating_duration_since(*since).as_secs_f32() / SLIDE).min(1.));
        if slide == Some(1.) {
            self.finish_leaving();
        }
        let slide = self.leaving.as_ref().and(slide).map(crate::motion::ease_in);
        if moving || self.wash.is_some() || slide.is_some() {
            window.request_animation_frame();
        }
        if !self.reveal.open && level <= 0. {
            // The close fade has landed: the rows were drawn to its last frame, and now the list lets go of them.
            self.release();
            return div().into_any_element();
        }
        let (ink, light) = (cx.theme().sidebar_foreground, theme::is_light(cx));
        let wash = selected_wash(&mut self.key, ink, light, window, cx);
        let theme = cx.theme();
        let (fill, border) = tooltip::float_fill(theme.background, ink, light);
        let family = theme.font_family.clone();
        let editor = crate::settings::get(cx).editor.drawn_family(cx).unwrap_or_else(|| theme.mono_font_family.clone());
        let primary = theme::ink(ALPHA_PRIMARY, cx);
        let muted = theme::ink(ALPHA_SECONDARY, cx);
        let field = float_field(&self.input, Some("magnifyingglass"), 0., cx).w_full();

        // The list under the field: a line while nothing is read or nothing matches, else the rows.
        let list = match &self.loaded {
            None => None,
            Some(loaded) if self.shown.is_empty() => Some(
                div()
                    .h(ROW_HEIGHT)
                    .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
                    .flex()
                    .items_center()
                    .text_color(muted)
                    .child(if loaded.rows.is_empty() { "No drafts" } else { "No drafts match" })
                    .into_any_element(),
            ),
            Some(_) => {
                let selected = self.selected;
                // Only the rows in view are built: the list asks for a range, and every row is one `ROW_HEIGHT`. A row on its way to
                // the Trash fades and slides out where it is while the rows under it climb into its place.
                let rows = uniform_list(
                    "draft-list-rows",
                    self.shown.len(),
                    cx.processor(move |this: &mut Self, range: std::ops::Range<usize>, _, cx| {
                        let Some(loaded) = this.loaded.as_ref() else { return Vec::new() };
                        let leaving_at = this.leaving.as_ref().and_then(|(path, _)| this.shown.iter().position(|&ix| loaded.rows[ix].draft.path == *path));
                        range
                            .filter_map(|at| {
                                let row = &loaded.rows[*this.shown.get(at)?];
                                let e = slide.filter(|_| leaving_at == Some(at)).unwrap_or(0.);
                                let climb = slide.filter(|_| leaving_at.is_some_and(|gone| at > gone)).unwrap_or(0.);
                                let said = this.notice.as_ref() == Some(&row.draft.path);
                                let content = if said {
                                    div().flex_1().min_w_0().text_color(muted).truncate().child("Open in a tab, so it stays. Close the tab first.").into_any_element()
                                } else {
                                    let title = if row.empty {
                                        div().flex_1().min_w_0().text_color(muted).truncate().child("Empty draft")
                                    } else {
                                        div().flex_1().min_w_0().font_family(editor.clone()).truncate().child(row.title.clone())
                                    };
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .flex()
                                        .items_center()
                                        .child(title)
                                        .child(div().flex_none().pl(ROW_GAP).text_color(muted).font_features(tabular()).whitespace_nowrap().child(row.date.clone()))
                                        .child(div().flex_none().w(px(76.)).pl(ROW_GAP).flex().justify_end().text_color(muted).font_features(tabular()).whitespace_nowrap().child(row.words.clone()))
                                        .into_any_element()
                                };
                                Some(
                                    div()
                                        .id(("draft-list-row", at))
                                        .fade_row(("draft-list-row", at), ink, light, at == selected)
                                        .top(-ROW_HEIGHT * climb)
                                        .w_full()
                                        .h(ROW_HEIGHT)
                                        .overflow_hidden()
                                        .opacity(1. - e)
                                        .child(
                                            div()
                                                .relative()
                                                .left(px(SLIDE_DISTANCE * e))
                                                .h(ROW_HEIGHT)
                                                .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
                                                .flex()
                                                .items_center()
                                                .child(content),
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
                let gone = self.leaving.as_ref().and(slide).unwrap_or(0.);
                let (shown, scroll) = (self.wash.map_or(self.selected as f32, |wash| wash.level(now)), self.scroll.clone());
                let wash = canvas(
                    |_, _, _| {},
                    move |bounds, _, window, _| {
                        let offset = scroll.0.borrow().base_handle.offset().y;
                        let at = point(bounds.origin.x, bounds.origin.y + offset + ROW_HEIGHT * shown + ROW_WASH_INSET_Y);
                        let rect = Bounds::new(at, size(bounds.size.width, (ROW_HEIGHT - ROW_WASH_INSET_Y * 2.) * (1. - gone)));
                        window.paint_quad(gpui_kit::fill(rect, wash.opacity(1. - gone)).corner_radii(Corners::all(RADIUS_ROW)));
                    },
                )
                .absolute()
                .inset_0();
                // A short list closes the gap of a row that slid out; a longer one keeps its height and scrolls.
                let count = self.shown.len();
                let height = ROW_HEIGHT * (count.min(OPEN_QUICKLY_ROWS) as f32 - if count <= OPEN_QUICKLY_ROWS { gone } else { 0. });
                // A wheel or a drag of the knob scrolls the rows; the overlay fades the edge while more wait and paints the knob.
                Some(
                    div()
                        .relative()
                        .h(height)
                        .overflow_hidden()
                        .child(scrollbar::watch(&self.scroller))
                        .child(wash)
                        .child(rows)
                        .child(scrollbar::overlay(Overlay { handle: Rc::new(self.scroll.clone()), scroller: self.scroller.clone(), ground: Some(fill) }))
                        .into_any_element(),
                )
            }
        };

        let body = tooltip::float(RADIUS_FLOAT_CAPSULE, fill, border, light, false)
            .id("draft-list")
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
            }))
            // Cmd-Delete is the field's "delete to the line's start": here it trashes the pick.
            .capture_action(cx.listener(|this, _: &DeleteToBeginningOfLine, _, cx| {
                if this.trash_picked(cx).is_err() && this.notice.is_none() {
                    crate::preview::render::beep();
                }
                cx.stop_propagation();
            }));
        let place = div().mt(px(OVERLAY_ENTER_RISE * (1. - level))).opacity(level).child(body);
        if self.reveal.open {
            place.into_any_element()
        } else {
            // A leaving float takes no clicks and no focus: a cover over it eats them until it is gone.
            place.child(div().id("draft-list-cover").absolute().inset_0().occlude()).into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Local, date_label, first_line, matches, words_label};

    fn at(year: i32, month: i32, day: i32, hour: i32, minute: i32) -> Local {
        Local { year, month, day, hour, minute }
    }

    #[test]
    fn first_line_skips_blank_lines_and_empty_drafts_have_none() {
        assert_eq!(first_line("\n  \n  hello there  \nsecond").as_deref(), Some("hello there"));
        assert_eq!(first_line(" \n\t\n"), None);
        assert_eq!(first_line(""), None);
        assert_eq!(first_line("---\ntitle: \ntags: []\n---\n").as_deref(), Some("---"));
        assert_eq!(first_line("# Grocery ideas").as_deref(), Some("Grocery ideas"));
        assert_eq!(first_line("---\ntitle: Plan\n---\nbody").as_deref(), Some("Plan"));
        assert_eq!(first_line("---\ntags: a\n---\n\nHello").as_deref(), Some("Hello"));
        assert_eq!(first_line("#!/bin/sh\necho").as_deref(), Some("#!/bin/sh"));
        assert!(first_line(&"a".repeat(1000)).is_some_and(|line| line.len() < 1000));
    }

    #[test]
    fn typed_words_all_have_to_be_in_the_text() {
        let text = "shopping list\nmilk, eggs and bread";
        assert!(matches(text, ""));
        assert!(matches(text, "  "));
        assert!(matches(text, "eggs shopping"));
        assert!(!matches(text, "eggs butter"));
    }

    #[test]
    fn dates_read_today_yesterday_then_the_day() {
        let (today, yesterday) = (at(2026, 11, 1, 16, 0), at(2026, 10, 31, 16, 0));
        assert_eq!(date_label(at(2026, 11, 1, 14, 5), today, yesterday), "Today 14:05");
        assert_eq!(date_label(at(2026, 10, 31, 9, 0), today, yesterday), "Yesterday 09:00");
        assert_eq!(date_label(at(2026, 10, 30, 9, 0), today, yesterday), "Oct 30");
        assert_eq!(date_label(at(2025, 12, 2, 9, 0), today, yesterday), "Dec 2, 2025");
    }

    #[test]
    fn word_counts_say_when_only_the_start_was_counted() {
        assert_eq!(words_label(1, false), "1 word");
        assert_eq!(words_label(0, false), "0 words");
        assert_eq!(words_label(12, false), "12 words");
        assert_eq!(words_label(900, true), "900+ words");
    }
}

#[cfg(test)]
mod held {
    use super::{drafts, read};

    /// 2,000 drafts of more than the 8 KB head: a row holds the head's lowercase once (before: the head and its lowercase), and
    /// the filter still finds a draft by a word in its text.
    #[test]
    fn a_row_holds_the_head_once() {
        let dir = std::env::temp_dir().join(format!("finch-draft-list-held-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for ix in 0..2_000 {
            let text = format!("Draft Number {ix}\n{}", "Some Mixed Case words go here. ".repeat(300));
            std::fs::write(dir.join(format!("draft-{ix:04}.md")), text).unwrap();
        }
        // What a row held before: the draft with its head, and the lowercased copy beside it.
        let (old, before) = crate::test_alloc::held(|| drafts::list(&dir).into_iter().map(|draft| (draft.head.to_lowercase(), draft)).collect::<Vec<_>>());
        let (loaded, after) = crate::test_alloc::held(|| read(&dir, &[]));
        eprintln!("2000 drafts: a row held {} bytes before, {} now", before / 2_000, after / 2_000);
        assert_eq!(loaded.rows.len(), 2_000);
        assert!(loaded.rows.iter().all(|row| row.draft.head.is_empty() && row.haystack.contains("some mixed case words")));
        assert_eq!(old.len(), 2_000);
        assert!(after * 10 < before * 6, "{after} bytes held now, {before} before");
        let (_, freed) = crate::test_alloc::held(|| drop(loaded));
        assert!(freed < -(after * 9 / 10), "dropping the list frees its rows ({freed} bytes of {after})");
        drop(old);
        std::fs::remove_dir_all(&dir).ok();
    }
}
