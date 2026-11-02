//! The status line: a slim strip at the foot of each pane that has a tab, saying where the caret
//! is and what the file is. The left side holds the caret's line and column (and the selection's
//! size), then the word count of a Markdown or plain-text file and the sum of the numbers a selection holds; the right side the language, the indentation, the encoding and the line endings.
//! The writing goal's hairline (`writing_goal.rs`) lies along the strip's top edge.
//!
//! It is a view of its own, cached by the editor pane, so it is rebuilt only when its text
//! changes (a caret move, a tab switch, a new encoding), never per frame: it follows the editor
//! and the document and compares what it would show with what it shows before it notifies.
//!
//! A field is clickable only where a command exists: the position opens Go to line, the
//! indentation a menu of File's Convert indentation items, the encoding File's Reopen with encoding
//! and Convert to choices (Convert to alone for an Untitled page), the line endings Format's Line endings choices,
//! the word count a pop-up with the characters, lines and reading time, the sum a copy of it.
//!
//! Words are counted on the background executor, about `word_count::DEBOUNCE` after an edit, and an
//! answer for text that has since changed is dropped (`text_revision()`); nothing counts per frame.
//! A selection is counted (words and numbers) once per change of the selection, in place when small
//! and in the background otherwise.

use std::{
    collections::HashMap,
    fmt::Write as _,
    ops::Range,
    hash::{DefaultHasher, Hash as _, Hasher as _},
    time::Duration,
};

use encoding_rs::Encoding;
use gpui_kit::component::{
    ActiveTheme as _,
    input::{EditorState, InputEvent, Rope, RopeExt as _},
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    control,
    hover::FadeHover as _,
    motion::{self, Fade},
    tooltip,
    app::ShowGoToLine,
    document::{Document, DocumentChanged},
    encoding::{self, Endings},
    language::{EDGE_LINES, LINE_CHARS},
    native_menu::{self, Entry, Item},
    config_check::{self, ConfigWatch},
    config_path::{self, TrailWatch},
    theme,
    word_count::{self, Stats, Tally},
    writing_goal::{self, GoalEvent, GoalField},
    tokens::{ALPHA_BUTTON_HOVER, ALPHA_BUTTON_PRESSED, ALPHA_PRIMARY, ALPHA_SECONDARY, ALPHA_STATUS_DIM, FADE_QUICK, RADIUS_STATUS_FIELD, STATUS_FIELD_HEIGHT, STATUS_FIELD_PAD, STATUS_GAP, STATUS_HEIGHT, STATUS_INSET, STATUS_POSITION_MIN, STATUS_SYMBOL, TEXT_CAPTION, chrome_ground, tabular},
};

/// What an owner entry holds: it can say whether the document behind it is still there.
trait Alive {
    fn alive(&self) -> bool;
}

impl Alive for WeakEntity<Document> {
    fn alive(&self) -> bool {
        self.upgrade().is_some()
    }
}

/// The document each editor was built for, so a pane that is handed an editor can find the file
/// it shows. Weak: a closed document drops out on the next registration or lookup.
struct Owners<W = WeakEntity<Document>>(HashMap<EntityId, W>);

impl<W> Default for Owners<W> {
    fn default() -> Self {
        Self(HashMap::new())
    }
}

impl<W: Alive + Clone + 'static> Global for Owners<W> {}

impl<W: Alive + Clone> Owners<W> {
    fn insert(&mut self, editor: EntityId, owner: W) {
        self.0.retain(|_, kept| kept.alive());
        self.0.insert(editor, owner);
    }

    fn find(&mut self, editor: EntityId) -> Option<W> {
        self.0.retain(|_, kept| kept.alive());
        self.0.get(&editor).cloned()
    }
}

/// `document` built `editor`: the status line of a pane showing it reads the file from here.
pub fn own(editor: &Entity<EditorState>, document: WeakEntity<Document>, cx: &mut App) {
    cx.default_global::<Owners>().insert(editor.entity_id(), document);
}

/// How many editors the status line's owner table holds, stale ones included until the next
/// registration prunes them, for the control socket's `tab-state`.
pub fn owner_count(cx: &App) -> usize {
    cx.try_global::<Owners>().map_or(0, |owners| owners.0.len())
}

fn owner(editor: &Entity<EditorState>, cx: &mut App) -> Option<Entity<Document>> {
    cx.default_global::<Owners>().find(editor.entity_id())?.upgrade()
}

/// The strips alive, so a menu command can find the one showing an editor (`ask_goal`).
#[derive(Default)]
struct Strips(Vec<WeakEntity<StatusLine>>);

impl Global for Strips {}

impl Strips {
    fn add(&mut self, strip: WeakEntity<StatusLine>) {
        self.0.retain(|kept| kept.upgrade().is_some() && kept.entity_id() != strip.entity_id());
        self.0.push(strip);
    }
}

/// Format > Writing goal…: opens the goal field over the strip that shows `editor`.
pub fn ask_goal(editor: &Entity<EditorState>, window: &mut Window, cx: &mut App) {
    let strips = cx.default_global::<Strips>().0.clone();
    for strip in strips.into_iter().filter_map(|strip| strip.upgrade()) {
        if strip.read(cx).editor.as_ref().is_some_and(|kept| kept.entity_id() == editor.entity_id()) {
            strip.update(cx, |strip, cx| strip.open_goal(window, cx));
            return;
        }
    }
}

/// What the strip shows; a refresh builds the strings again only when this changed.
#[derive(Clone, PartialEq)]
struct Fields {
    line: u32,
    column: u32,
    selected: usize,
    /// The words of the whole file; `None` for code, a file too big to count, or before the first count lands.
    words: Option<usize>,
    /// The words of the selection, `None` with no selection or none counted yet.
    selected_words: Option<usize>,
    /// The sum of the numbers in the selection, `None` with fewer than two.
    sum: Option<Tally>,
    /// The writing goal in force (the menu's, else the front matter's), with the words it counts; `None` without one or for a file with no count.
    goal: Option<usize>,
    language: &'static str,
    spaces: bool,
    width: usize,
    encoding: &'static Encoding,
    endings: Endings,
    reopenable: bool,
    /// How many carets the editor has, 1 for one.
    cursors: usize,
}

/// The strings of `Fields`.
#[derive(Clone, Default)]
struct Text {
    position: SharedString,
    /// The position's short form, for a pane too narrow for `position`.
    short: SharedString,
    language: SharedString,
    indent: SharedString,
    encoding: SharedString,
    endings: SharedString,
    words: SharedString,
    /// The Words field's tip: its static line, or the goal's progress while one is set.
    words_tip: SharedString,
    sum: SharedString,
    pill: SharedString,
    cursors: SharedString,
    trail: SharedString,
}

/// How long a big selection waits before it is counted: a drag asks again at every step.
const SELECTION_PAUSE: Duration = Duration::from_millis(100);

/// How long a config pill takes to fade in when the file stops parsing.
const PILL_FADE: f32 = 0.25;

/// How long "Copied" stays on the sum or the path trail after a click; a second click starts the hold again.
const COPIED_HOLD: Duration = Duration::from_secs(1);

/// The word a clicked label says while its copy is confirmed.
const COPIED: &str = "Copied";

/// One label's copy confirmation: "Copied" fades in over the label's own text (`FADE_QUICK`), holds for `COPIED_HOLD`, and fades back. The fade turns around from where it is drawn; `held` flips at the click and at the end of the hold, and the timer is the only thing awake during the hold.
struct Confirm {
    fade: Fade,
    held: bool,
    timer: Option<Task<()>>,
}

impl Confirm {
    fn new() -> Self {
        Self { fade: Fade::with(0., FADE_QUICK), held: false, timer: None }
    }

    /// The level to draw at `now` (0 text, 1 "Copied"), asking for frames only while it moves.
    fn level(&mut self, now: std::time::Instant, window: &mut Window) -> f32 {
        self.fade.aim(f32::from(self.held), now);
        if self.fade.is_moving(now) {
            window.request_animation_frame();
        }
        self.fade.level(now)
    }
}

/// The longest trail shown, in characters: the outer segments give way to "…" past it.
const TRAIL_CHARS: usize = 40;

/// How long the goal's hairline takes to catch up with the count, and how long it glows once on reaching the goal.
const GOAL_FILL: f32 = 0.35;
const GOAL_GLOW: f32 = 0.9;

/// The width of the Writing goal field.
const GOAL_FIELD_WIDTH: Pixels = px(240.);

/// A finished count of the whole file: its numbers and the front matter's goal.
#[derive(Clone, Copy)]
struct Counted {
    stats: Stats,
    goal: Option<usize>,
    /// The text revision it counted.
    revision: u64,
}

/// How many left editors keep their count (finch#m7dd): the tab you came from, and the one before.
const RECENT: usize = 2;

/// The counts of the last `RECENT` editors this strip followed, so a tab switched back to at the
/// revision it was left at shows its words on the first frame and asks for no count. Bounded: two
/// small `Counted` values and their editor ids, which hold no editor alive.
#[derive(Default)]
struct Recent(Vec<(EntityId, Counted)>);

impl Recent {
    /// Keeps `counted` for the editor `id` just left (nothing kept when it had none), oldest out.
    fn keep(&mut self, id: EntityId, counted: Option<Counted>) {
        self.0.retain(|(kept, _)| *kept != id);
        if let Some(counted) = counted {
            self.0.push((id, counted));
            if self.0.len() > RECENT {
                self.0.remove(0);
            }
        }
    }

    /// The kept count of editor `id` when it describes `revision`; a count of other text is dropped.
    fn take(&mut self, id: EntityId, revision: u64) -> Option<Counted> {
        let at = self.0.iter().position(|(kept, _)| *kept == id)?;
        let (_, counted) = self.0.remove(at);
        (counted.revision == revision).then_some(counted)
    }

    /// The strip moves from editor `left` (holding `counted`) to `to`: keeps the left one's count under its own id, and returns the count `to` kept for `revision`.
    fn swap(&mut self, left: Option<EntityId>, counted: Option<Counted>, to: EntityId, revision: u64) -> Option<Counted> {
        if let Some(left) = left {
            self.keep(left, counted);
        }
        self.take(to, revision)
    }
}

/// A selection by its byte range, the text revision it was made in and whether the file is prose (which decides what is counted).
type SelectionKey = (Range<usize>, u64, bool);

/// A finished count of a selection.
struct Selection {
    key: SelectionKey,
    /// `None` for a file that isn't prose, or a selection too big to count.
    words: Option<usize>,
    /// `None` with fewer than two numbers, or a selection too big to scan.
    tally: Option<Tally>,
}

/// The words (of prose) and the numbers of the text of a selection.
fn analyse(text: &str, prose: bool) -> (Option<usize>, Option<Tally>) {
    let words = prose.then(|| word_count::count_chunks([text]).words);
    let tally = if text.len() <= word_count::NUMBER_SCAN_MAX { word_count::tally(text) } else { None };
    (words, tally)
}

/// Sets the count the waiting task will take (moving its deadline); true when no task was waiting, so the caller starts one.
fn arm_count(next: &mut Option<(std::time::Instant, Rope, u64)>, due: std::time::Instant, rope: Rope, revision: u64) -> bool {
    next.replace((due, rope, revision)).is_none()
}

/// The text of a selection on its way to being counted. A short one is copied out where it is picked; a longer one is only a handle on the editor's rope (a clone shares its nodes) and the range, so a drag step copies nothing, and the text is built where it is counted, off the main thread, after the pause.
enum Chosen {
    Copied(SharedString),
    Held(Rope, Range<usize>),
}

impl Chosen {
    fn of(state: &EditorState, rope: &Rope, range: &Range<usize>, chars: usize) -> Self {
        if chars <= word_count::DIRECT_CHARS {
            Self::Copied(state.selected_value())
        } else {
            Self::Held(rope.clone(), range.clone())
        }
    }

    /// The bytes copied out on the thread that picks.
    #[cfg(test)]
    fn copied(&self) -> usize {
        match self {
            Self::Copied(text) => text.len(),
            Self::Held(..) => 0,
        }
    }

    fn text(&self) -> std::borrow::Cow<'_, str> {
        match self {
            Self::Copied(text) => std::borrow::Cow::Borrowed(text),
            Self::Held(rope, range) => std::borrow::Cow::Owned(rope.slice(range.start.min(rope.len())..range.end.min(rope.len())).to_string()),
        }
    }
}

pub struct StatusLine {
    /// Weak: the strip doesn't keep a closed tab's editor or document alive.
    editor: Option<WeakEntity<EditorState>>,
    document: Option<WeakEntity<Document>>,
    fields: Option<Fields>,
    text: Text,
    /// The language of the file as last worked out: it needs a look at the text for a page with
    /// no known type, so it is read again only when the path changes or an edit lands near the
    /// start or end of the text, where a `#!` line or modeline sits, and then only if the lines it reads changed.
    language: &'static str,
    /// The path the language was read for: `None` until it has been read, `Some(None)` for a page with no path (an Untitled tab).
    path_of_language: Option<Option<std::path::PathBuf>>,
    /// An edit landed since the language was read.
    edited: bool,
    /// `edge_hash` of the text the language was last worked out from; `None` until it has been.
    edge: Option<u64>,
    /// The widths of the file's fields (language, indentation, encoding, line endings, word count, number sum), measured again when their text changes. The last two are 0 while they show nothing.
    measured: Option<[Pixels; 9]>,
    /// The widths of the long and the short position text, measured again when the position changes.
    position_widths: Option<(Pixels, Pixels)>,
    /// The strip's width as last drawn, `None` before the first frame.
    room: Option<Pixels>,
    /// The pane holds the keyboard in the key window, as last drawn.
    key: bool,
    /// The text's lit level (1 lit, 0 dim) easing toward `key`; `None` before the first frame, which draws it landed.
    lit: Option<Fade>,
    /// The editor handle the focus watch follows and its subscriptions.
    watched: Option<FocusHandle>,
    watch: Vec<Subscription>,
    _subscriptions: Vec<Subscription>,
    /// The count of the whole file, and the revision a count was last asked for (so an edit asks once).
    counted: Option<Counted>,
    asked: Option<u64>,
    recent: Recent,
    count_task: Option<Task<()>>,
    /// The count the waiting task will take: when it is due, the text and its revision.
    count_next: Option<(std::time::Instant, Rope, u64)>,
    /// The same for the selection.
    sel: Option<Selection>,
    sel_asked: Option<SelectionKey>,
    sel_task: Option<Task<()>>,
    /// The writing goal's field, made at the first Format > Writing goal.
    goal_field: Option<(Entity<GoalField>, Subscription)>,
    /// How far the goal's hairline is drawn (0 to 1), easing toward the count's share of the goal; `None` with no goal.
    fill: Option<Fade>,
    /// The goal was cleared from the menu: the hairline fades out from where it is drawn, in `goal_color`, and is dropped when it lands.
    leaving: bool,
    goal_color: Hsla,
    /// When the goal was reached just now (the glow runs `GOAL_GLOW` from there), and a reach waiting for the next frame to start it.
    glow: Option<std::time::Instant>,
    glow_pending: bool,
    /// The config pill's check (JSON, TOML, YAML), and the error it last showed with the fade that brings a new one in.
    config: ConfigWatch,
    pill_error: Option<config_check::ConfigError>,
    pill_fade: Option<Fade>,
    /// The caret's place in the file, and the trail the strip last showed.
    trail: TrailWatch,
    shown_trail: Option<config_path::Trail>,
    /// The "Copied" confirmations of the sum and the path trail.
    copied_sum: Confirm,
    copied_trail: Confirm,
}

impl StatusLine {
    pub fn new() -> Self {
        Self {
            editor: None,
            document: None,
            fields: None,
            text: Text::default(),
            language: "text",
            path_of_language: None,
            edited: false,
            edge: None,
            measured: None,
            position_widths: None,
            room: None,
            key: true,
            lit: None,
            watched: None,
            watch: Vec::new(),
            _subscriptions: Vec::new(),
            counted: None,
            recent: Recent::default(),
            asked: None,
            count_task: None,
            count_next: None,
            sel: None,
            sel_asked: None,
            sel_task: None,
            goal_field: None,
            fill: None,
            leaving: false,
            goal_color: Hsla::default(),
            glow: None,
            glow_pending: false,
            config: ConfigWatch::default(),
            trail: TrailWatch::default(),
            pill_error: None,
            pill_fade: None,
            shown_trail: None,
            copied_sum: Confirm::new(),
            copied_trail: Confirm::new(),
        }
    }

    /// Follows `editor` (and the file it shows) from now on.
    pub fn set(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        if self.editor.as_ref().is_some_and(|kept| kept.entity_id() == editor.entity_id()) {
            return;
        }
        let document = owner(editor, cx);
        let mut subscriptions = vec![
            cx.observe(editor, |this, _, cx| this.refresh(cx)),
            cx.subscribe(editor, |this, _, event: &InputEvent, _| {
                if matches!(event, InputEvent::Change) {
                    this.edited = true;
                }
            }),
        ];
        if let Some(document) = &document {
            subscriptions.push(cx.subscribe(document, |this, _, _: &DocumentChanged, cx| {
                // The document says something changed (a rename, a page that now reads as a patch): the language is read again.
                this.path_of_language = None;
                this.refresh(cx)
            }));
        }
        let left = self.editor.as_ref().map(|left| left.entity_id());
        self.editor = Some(editor.downgrade());
        self.document = document.map(|document| document.downgrade());
        self.path_of_language = None;
        self.edited = true;
        self.edge = None;
        self.fields = None;
        // An editor back at the revision it was left at has its count already: the first refresh draws the words and asks for none.
        self.counted = self.recent.swap(left, self.counted, editor.entity_id(), editor.read(cx).text_revision());
        self.asked = self.counted.map(|counted| counted.revision);
        self.count_task = None;
        self.count_next = None;
        self.sel = None;
        self.sel_asked = None;
        self.sel_task = None;
        self.fill = None;
        self.leaving = false;
        self.glow = None;
        self.glow_pending = false;
        let me = cx.entity().downgrade();
        cx.default_global::<Strips>().add(me);
        self._subscriptions = subscriptions;
        self.refresh(cx);
    }

    /// Reads what the strip would show and redraws only when it differs from what it shows.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (Some(editor), Some(document)) = (self.editor.as_ref().and_then(WeakEntity::upgrade), self.document.as_ref().and_then(WeakEntity::upgrade)) else { return };
        let state = editor.read(cx);
        let position = state.cursor_position();
        let range = state.selected_range();
        let tab = state.tab_setting();
        let selected = if range.is_empty() { 0 } else { state.selected_text().len_chars() };
        let cursors = state.cursor_count().max(1);
        let text = state.text();
        let rows = text.lines_len();
        let near_edge = position.line < EDGE_LINES as u32 || position.line as usize + EDGE_LINES >= rows;
        let doc = document.read(cx);
        let path_changed = self.path_of_language.as_ref().map(|path| path.as_deref()) != Some(doc.path.as_deref());
        // An edit leaves the language alone unless it touched what the check reads, so the check waits for the first edit near an end and then runs only if those lines changed.
        let edge = (!path_changed && self.edited && near_edge).then(|| edge_hash(text));
        if path_changed || edge.is_some_and(|edge| self.edge != Some(edge)) {
            self.language = doc.language(cx);
            self.path_of_language = Some(doc.path.clone());
            self.edge = Some(edge.unwrap_or_else(|| edge_hash(text)));
        }
        if edge.is_some() || path_changed {
            self.edited = false;
        }
        let (encoding, endings, reopenable) = (doc.encoding(), doc.endings(), doc.can_reopen());
        let lenient = self.language == "json" && doc.path.as_deref().and_then(|path| path.file_name()).and_then(|name| name.to_str()).is_some_and(config_check::lenient_json);
        let (spaces, width) = (!tab.hard_tabs, tab.tab_size);
        // The word count is asked again when the text changed and arrives later; the selection's is
        // asked when the selection or the text did.
        let revision = state.text_revision();
        let prose = word_count::counts_words(self.language) && text.len() <= word_count::MAX_BYTES;
        let count_from = (prose && self.asked != Some(revision)).then(|| text.clone());
        let key = (!range.is_empty()).then(|| (range.clone(), revision, prose));
        // A selection past what is counted (the numbers' cap in a code file) isn't copied out.
        let reach = if prose { word_count::MAX_BYTES } else { word_count::NUMBER_SCAN_MAX };
        let selection = key.as_ref().filter(|key| self.sel_asked.as_ref() != Some(*key)).map(|_| (selected <= reach).then(|| Chosen::of(state, text, &range, selected)));
        if !prose {
            self.counted = None;
            self.asked = None;
            self.count_task = None;
            self.count_next = None;
        } else if let Some(rope) = count_from {
            self.ask_count(rope, revision, cx);
        }
        match (&key, selection) {
            (Some(key), Some(chosen)) => self.ask_selection(key.clone(), chosen, selected, cx),
            (None, _) => {
                self.sel = None;
                self.sel_asked = None;
                self.sel_task = None;
            }
            _ => {}
        }
        let found = key.as_ref().and_then(|key| self.sel.as_ref().filter(|sel| &sel.key == key));
        let (selected_words, sum) = (found.and_then(|sel| sel.words), found.and_then(|sel| sel.tally));
        let goal = if prose { writing_goal::session(&document, cx).or(self.counted.and_then(|counted| counted.goal)) } else { None };
        // The config pill and the trail follow the editor's tree: asked on every change and caret move, never per frame. A pill or trail that changed at once redraws even when no field did.
        let lang = config_check::Lang::from_name(self.language);
        let config_changed = self.config.schedule(&editor, lang, lenient, cx);
        let trail_changed = self.trail.refresh(&editor, lang, cx);
        let fields = Fields {
            line: position.line + 1,
            column: position.character + 1,
            selected,
            words: self.counted.filter(|_| prose).map(|counted| counted.stats.words),
            selected_words,
            sum,
            goal,
            language: self.language,
            spaces,
            width,
            encoding,
            endings,
            reopenable,
            cursors,
        };
        if self.fields.as_ref() == Some(&fields) {
            if config_changed || trail_changed {
                cx.notify();
            }
            return;
        }
        let was = self.fields.replace(fields.clone());
        // Reaching the goal by writing (not by a new goal or the first count) lights the hairline once; the next frame starts it.
        if let (Some(was), Some(goal), Some(words)) = (&was, fields.goal, fields.words) {
            if was.goal == fields.goal && was.words.is_some_and(|before| before < goal) && words >= goal {
                self.glow_pending = true;
            }
        }
        let mut position = String::with_capacity(40);
        let mut short = String::with_capacity(32);
        let _ = write!(position, "Line {}, Column {}", fields.line, fields.column);
        let _ = write!(short, "Ln {}, Col {}", fields.line, fields.column);
        if fields.selected > 0 {
            let _ = write!(position, " ({} selected)", fields.selected);
            let _ = write!(short, " ({} selected)", fields.selected);
        }
        self.text.position = position.into();
        self.text.short = short.into();
        self.position_widths = None;
        // The right side changes far less often than the caret moves: each string is built again only when its own inputs changed.
        if was.as_ref().is_none_or(|was| was.language != fields.language) {
            self.text.language = language_name(fields.language).into();
        }
        if was.as_ref().is_none_or(|was| was.spaces != fields.spaces || was.width != fields.width) {
            self.text.indent = if fields.spaces { format!("{} Spaces", fields.width).into() } else { "Tabs".into() };
        }
        if was.as_ref().is_none_or(|was| was.encoding != fields.encoding) {
            self.text.encoding = encoding::label(fields.encoding).into_owned().into();
        }
        if was.as_ref().is_none_or(|was| was.endings != fields.endings) {
            self.text.endings = match fields.endings {
                Endings::Lf => "LF",
                Endings::Crlf => "CRLF",
                Endings::Mixed => "Mixed",
            }
            .into();
        }
        if was.as_ref().is_none_or(|was| was.sum != fields.sum) {
            self.text.sum = fields.sum.map(|tally| tally.label()).unwrap_or_default().into();
        }
        if was.as_ref().is_none_or(|was| was.words != fields.words || was.selected_words != fields.selected_words) {
            self.text.words = fields.words.map(|total| word_count::label(fields.selected_words, total)).unwrap_or_default().into();
        }
        if was.as_ref().is_none_or(|was| was.words != fields.words || was.goal != fields.goal) {
            self.text.words_tip = word_count::tip(fields.words, fields.goal).into();
        }
        if was.as_ref().is_none_or(|was| was.cursors != fields.cursors) {
            self.text.cursors = if fields.cursors > 1 { format!("{} cursors", fields.cursors).into() } else { SharedString::default() };
        }
        if was.as_ref().is_none_or(|was| was.language != fields.language || was.spaces != fields.spaces || was.width != fields.width || was.encoding != fields.encoding || was.endings != fields.endings || was.words != fields.words || was.selected_words != fields.selected_words || was.sum != fields.sum || was.cursors != fields.cursors) {
            self.measured = None;
        }
        cx.notify();
    }

    /// Counts the whole file in the background, after the pause an edit asks for (none for the first count), and keeps the answer if the text hasn't changed since.
    fn ask_count(&mut self, rope: Rope, revision: u64, cx: &mut Context<Self>) {
        self.asked = Some(revision);
        let wait = if self.counted.is_some() { word_count::DEBOUNCE } else { Duration::ZERO };
        // One task per burst: each edit moves the deadline and the text the task will count.
        if !arm_count(&mut self.count_next, std::time::Instant::now() + wait, rope, revision) {
            return;
        }
        self.count_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let Ok(Some(due)) = this.update(cx, |this, _| this.count_next.as_ref().map(|next| next.0)) else { return };
                let now = std::time::Instant::now();
                if due > now {
                    cx.background_executor().timer(due - now).await;
                    continue;
                }
                break;
            }
            let Ok(Some((_, rope, revision))) = this.update(cx, |this, _| this.count_next.take()) else { return };
            let counted = cx
                .background_executor()
                .spawn(async move {
                    Counted { stats: word_count::count_chunks(rope.chunks()), goal: word_count::front_matter_goal_of(rope.chunks()), revision }
                })
                .await;
            this.update(cx, |this, cx| {
                let current = this.editor.as_ref().and_then(WeakEntity::upgrade).map(|editor| editor.read(cx).text_revision());
                if current == Some(revision) {
                    this.counted = Some(counted);
                    this.refresh(cx);
                }
            })
            .ok();
        }));
    }

    /// Counts what a selection holds: in place when it is small, else in the background after a short pause (a drag asks again at every step, and the older answer is dropped). `None` is a selection too big to count.
    fn ask_selection(&mut self, key: SelectionKey, chosen: Option<Chosen>, chars: usize, cx: &mut Context<Self>) {
        self.sel_asked = Some(key.clone());
        let prose = key.2;
        let Some(chosen) = chosen else {
            self.sel = Some(Selection { key, words: None, tally: None });
            self.sel_task = None;
            return;
        };
        if chars <= word_count::DIRECT_CHARS {
            let (words, tally) = analyse(&chosen.text(), prose);
            self.sel = Some(Selection { key, words, tally });
            self.sel_task = None;
            return;
        }
        self.sel_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SELECTION_PAUSE).await;
            let (words, tally) = cx.background_executor().spawn(async move { analyse(&chosen.text(), prose) }).await;
            this.update(cx, |this, cx| {
                if this.sel_asked.as_ref() == Some(&key) {
                    this.sel = Some(Selection { key, words, tally });
                    this.refresh(cx);
                }
            })
            .ok();
        }));
    }

    /// The pane's editor takes the keyboard, so the command a click sends is answered by it.
    fn focus_and(&self, window: &mut Window, cx: &mut App, then: impl FnOnce(&mut Window, &mut App)) {
        let Some(editor) = self.editor.as_ref().and_then(WeakEntity::upgrade) else { return };
        editor.update(cx, |state, cx| state.focus(window, cx));
        then(window, cx);
    }

    fn go_to_line(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.focus_and(window, cx, |window, cx| window.dispatch_action(Box::new(ShowGoToLine), cx));
    }

    /// The word count's pop-up: the characters, lines and reading time of the whole file, and the goal's progress when one is set, as greyed rows (they inform; nothing happens when picked).
    fn words_menu(&mut self, window: &mut Window) {
        let Some(stats) = self.counted.map(|counted| counted.stats) else { return };
        let goal = self.fields.as_ref().and_then(|fields| fields.goal);
        let entries = word_count::details(&stats, goal).into_iter().map(|row| Entry::Item(Item::new(row, ()).disabled(true))).collect();
        pop_up(entries, window, |(), _, _| {});
    }

    /// Click on the sum: puts the sum on the clipboard as a plain number, no thousands commas, and shows "Copied" on the field.
    fn copy_sum(&mut self, cx: &mut Context<Self>) {
        if let Some(tally) = self.fields.as_ref().and_then(|fields| fields.sum) {
            cx.write_to_clipboard(ClipboardItem::new_string(tally.plain_sum()));
            self.confirm(|this| &mut this.copied_sum, cx);
        }
    }

    /// Shows "Copied" on a label: the fade heads for the word at the next frame and the timer sends it back after `COPIED_HOLD`; a click while it holds starts the hold again (the old timer is dropped), and one during the fade back turns it around from where it is drawn.
    fn confirm(&mut self, pick: fn(&mut Self) -> &mut Confirm, cx: &mut Context<Self>) {
        let timer = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(COPIED_HOLD).await;
            this.update(cx, |this, cx| {
                pick(this).held = false;
                cx.notify();
            })
            .ok();
        });
        let confirm = pick(self);
        confirm.held = true;
        confirm.timer = Some(timer);
        cx.notify();
    }

    /// Click on the config pill: the caret goes to where the file stopped parsing.
    fn jump_to_error(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(editor), Some(offset)) = (self.editor.as_ref().and_then(WeakEntity::upgrade), self.config.jump_target()) else { return };
        config_check::jump_to(&editor, offset, window, cx);
    }

    /// Click on the trail copies the dotted path, Option-click a jq filter.
    fn copy_trail(&mut self, jq: bool, cx: &mut Context<Self>) {
        if let Some(trail) = self.trail.trail() {
            config_path::copy(&trail, jq, cx);
            self.confirm(|this| &mut this.copied_trail, cx);
        }
    }

    /// Format > Writing goal…: the field opens over the strip's left end holding the goal now in force.
    fn open_goal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.fields.as_ref().and_then(|fields| fields.goal);
        let (field, _) = self.goal_field.get_or_insert_with(|| {
            let field = cx.new(|cx| GoalField::new(window, cx));
            let subscription = cx.subscribe_in(&field, window, |this, _, event: &GoalEvent, window, cx| this.goal_event(event, window, cx));
            (field, subscription)
        });
        let field = field.clone();
        field.update(cx, |field, cx| field.open(current, window, cx));
    }

    /// What the goal field says: a goal that reads is kept on the document for the session (never in a file or in the settings), and closing gives the keyboard back to the editor.
    fn goal_event(&mut self, event: &GoalEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            GoalEvent::Set(goal) => {
                if let Some(document) = self.document.as_ref().and_then(WeakEntity::upgrade) {
                    // A goal set from the menu where none was drawn grows in from nothing; one cleared fades out. A goal that is already drawn (or still fading out) turns around from where it is.
                    if goal.is_some() && self.fill.is_none() && self.fields.as_ref().is_some_and(|fields| fields.goal.is_none()) {
                        self.fill = Some(Fade::with(0., GOAL_FILL));
                    }
                    self.leaving = goal.is_none() && self.fill.is_some();
                    writing_goal::set(&document, *goal, cx);
                    self.refresh(cx);
                }
            }
            GoalEvent::Done => {
                if let Some(editor) = self.editor.as_ref().and_then(WeakEntity::upgrade) {
                    editor.update(cx, |state, cx| state.focus(window, cx));
                }
            }
        }
    }

    /// The indentation field's menu: Edit's two Convert indentation items, the file's own style checked and greyed (it is already that).
    fn indent_menu(&mut self, window: &mut Window) {
        let Some(editor) = self.editor.as_ref().and_then(WeakEntity::upgrade) else { return };
        let spaces = self.fields.as_ref().is_some_and(|fields| fields.spaces);
        let entries = vec![
            Entry::Item(Item::new("Convert indentation to spaces", Indent::Spaces).checked(spaces).disabled(spaces)),
            Entry::Item(Item::new("Convert indentation to tabs", Indent::Tabs).checked(!spaces).disabled(!spaces)),
        ];
        pop_up(entries, window, move |choice, window, cx| {
            editor.update(cx, |state, cx| state.focus(window, cx));
            match choice {
                Indent::Spaces => window.dispatch_action(Box::new(gpui_kit::base::input::ConvertIndentationToSpaces), cx),
                Indent::Tabs => window.dispatch_action(Box::new(gpui_kit::base::input::ConvertIndentationToTabs), cx),
            }
        });
    }

    /// The encoding field's menu: File's Reopen with encoding (a page with a file) and Convert to lists as two submenus, the file's own checked in each, each ending in a More encodings submenu.
    fn encoding_menu(&mut self, window: &mut Window) {
        let (Some(fields), Some(document)) = (self.fields.clone(), self.document.as_ref().and_then(WeakEntity::upgrade)) else { return };
        let rows = |make: fn(&'static Encoding) -> EncodingChoice| -> Vec<Entry<EncodingChoice>> {
            let row = |&choice: &&'static Encoding| Entry::Item(Item::new(encoding::label(choice).into_owned(), make(choice)).checked(fields.encoding == choice));
            let mut entries: Vec<Entry<EncodingChoice>> = encoding::REOPEN_CHOICES.iter().map(row).collect();
            entries.push(Entry::Separator);
            entries.push(Entry::Submenu { title: "More encodings".into(), image: None, entries: encoding::MORE_CHOICES.iter().map(row).collect() });
            entries
        };
        let mut entries = Vec::new();
        if fields.reopenable {
            entries.push(Entry::Submenu { title: "Reopen with encoding".into(), image: None, entries: rows(EncodingChoice::Reopen) });
        }
        entries.push(Entry::Submenu { title: "Convert to".into(), image: None, entries: rows(EncodingChoice::Convert) });
        pop_up(entries, window, move |choice, window, cx| match choice {
            EncodingChoice::Reopen(encoding) => document.update(cx, |document, cx| document.reopen_as(encoding, window, cx)),
            EncodingChoice::Convert(encoding) => document.update(cx, |document, cx| document.convert_to(encoding, cx)),
        });
    }

    /// The line endings field's menu: the rows of Format > Line endings, the file's own checked and greyed (it is already that; a Mixed file has neither).
    fn endings_menu(&mut self, window: &mut Window) {
        let (Some(fields), Some(document)) = (self.fields.clone(), self.document.as_ref().and_then(WeakEntity::upgrade)) else { return };
        let entries = vec![
            Entry::Item(Item::new("LF", false).checked(fields.endings == Endings::Lf).disabled(fields.endings == Endings::Lf)),
            Entry::Item(Item::new("CRLF", true).checked(fields.endings == Endings::Crlf).disabled(fields.endings == Endings::Crlf)),
        ];
        pop_up(entries, window, move |crlf, window, cx| {
            document.update(cx, |document, cx| document.set_line_endings(crlf, window, cx));
        });
    }
}

#[derive(Clone, Copy)]
enum Indent {
    Spaces,
    Tabs,
}

/// A row of the encoding field's menu: read the file again as that encoding, or save in it from now on.
#[derive(Clone, Copy)]
enum EncodingChoice {
    Reopen(&'static Encoding),
    Convert(&'static Encoding),
}

/// Pops a native menu once the frame has drawn (nothing draws while it is open), two frames on as
/// the editor's right-click menu does, and hands the chosen row to `then`.
fn pop_up<T: Clone + 'static>(entries: Vec<Entry<T>>, window: &mut Window, then: impl FnOnce(T, &mut Window, &mut App) + 'static) {
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |window, cx| {
            window
                .spawn(cx, async move |cx| {
                    let Some(choice) = native_menu::pop_up(&entries, None) else { return };
                    cx.update(|window, cx| then(choice, window, cx)).ok();
                })
                .detach();
        });
    });
}

/// A hash of what the language check reads: the line count and the clipped text of the first and last `EDGE_LINES` lines. An edit that leaves it alone can't change the language.
fn edge_hash(rope: &Rope) -> u64 {
    let rows = rope.lines_len();
    let mut hasher = DefaultHasher::new();
    rows.hash(&mut hasher);
    for row in (0..rows.min(EDGE_LINES)).chain(rows.saturating_sub(EDGE_LINES)..rows) {
        for ch in rope.slice_line(row).chars().take(LINE_CHARS) {
            ch.hash(&mut hasher);
        }
        '\n'.hash(&mut hasher);
    }
    hasher.finish()
}

/// How a language of `language::for_path` reads on the strip.
fn language_name(language: &str) -> String {
    let name = match language {
        "text" => "Plain Text",
        "cpp" => "C++",
        "csharp" => "C#",
        "css" => "CSS",
        "html" => "HTML",
        "json" => "JSON",
        "javascript" => "JavaScript",
        "typescript" => "TypeScript",
        "tsx" => "TSX",
        "yaml" => "YAML",
        "toml" => "TOML",
        "php" => "PHP",
        "ejs" => "EJS",
        "erb" => "ERB",
        "graphql" => "GraphQL",
        "cmake" => "CMake",
        "make" => "Makefile",
        "proto" => "Protocol Buffers",
        "shell" => "Shell",
        other => {
            let mut chars = other.chars();
            return chars.next().map_or_else(String::new, |first| first.to_uppercase().chain(chars).collect());
        }
    };
    name.to_string()
}

/// Which of the strip's fields a pane is wide enough for: whole fields drop, never a cut letter.
/// To add a field: give it a slot in `widths` (`measure`, 0 while it shows nothing), a bit here, a place in `used`'s `kept` list and in `shown`'s dropping order, and a child in `render`.
#[derive(Clone, Copy, PartialEq)]
struct Shown {
    language: bool,
    indent: bool,
    encoding: bool,
    endings: bool,
    words: bool,
    sum: bool,
    pill: bool,
    cursors: bool,
    trail: bool,
}

/// How wide the strip is with `shown` kept and the position at `floor`. A field with no width (it shows nothing) takes no room.
fn used(widths: &[Pixels; 9], shown: &Shown, floor: Pixels) -> Pixels {
    let pad = STATUS_FIELD_PAD * 2.;
    let kept = [shown.language, shown.indent, shown.encoding, shown.endings, shown.words, shown.sum, shown.pill, shown.cursors, shown.trail];
    let mut used = (STATUS_INSET - STATUS_FIELD_PAD) * 2. + floor + pad;
    let mut count = 0;
    for (width, kept) in widths.iter().zip(kept) {
        if kept && *width > px(0.) {
            used += *width + pad;
            count += 1;
        }
    }
    if count > 0 {
        used += STATUS_GAP * count as f32;
    }
    used
}

/// The fields that fit in a strip `room` wide, given the text widths of language, indentation,
/// encoding, line endings, word count, number sum, config pill, cursor count and config trail. The position keeps `floor`; the trail goes first,
/// then the sum, the language, the word count, the cursor count, the line endings, the encoding, the indentation and the config pill last (an error is what the strip most has to say). `None`: the width isn't known yet.
fn shown(widths: &[Pixels; 9], room: Option<Pixels>, floor: Pixels) -> Shown {
    let mut shown = Shown { language: true, indent: true, encoding: true, endings: true, words: true, sum: true, pill: true, cursors: true, trail: true };
    let Some(room) = room else { return shown };
    for drop in [8, 5, 0, 4, 7, 3, 2, 1, 6] {
        if used(widths, &shown, floor) <= room {
            break;
        }
        match drop {
            8 => shown.trail = false,
            5 => shown.sum = false,
            0 => shown.language = false,
            4 => shown.words = false,
            7 => shown.cursors = false,
            3 => shown.endings = false,
            2 => shown.encoding = false,
            1 => shown.indent = false,
            _ => shown.pill = false,
        }
    }
    shown
}

/// Whether the position reads in its short form ("Ln 12, Col 3"): the long one ("Line 12, Column 3", `full` wide) doesn't fit the room the shown fields leave it.
fn compact(widths: &[Pixels; 9], shown: &Shown, room: Option<Pixels>, full: Pixels, floor: Pixels) -> bool {
    room.is_some_and(|room| full > floor + room - used(widths, shown, floor))
}

/// The least width the position keeps: its short form whole (so "(22 selected)" is never cut), and never less than `STATUS_POSITION_MIN`. The file's fields drop whole before the position goes under it.
fn floor(short: Pixels) -> Pixels {
    short.max(STATUS_POSITION_MIN)
}

impl StatusLine {
    /// The widths of the language, indentation, encoding, line endings, word count, number sum, config pill, cursor count and config trail as the strip sets them (the trail's includes its symbol and the gap after it).
    fn measure(&self, window: &Window, cx: &App) -> [Pixels; 9] {
        let mut widths = [&self.text.language, &self.text.indent, &self.text.encoding, &self.text.endings, &self.text.words, &self.text.sum, &self.text.pill, &self.text.cursors, &self.text.trail].map(|text| if text.is_empty() { px(0.) } else { self.measure_text(text, window, cx) });
        if widths[8] > px(0.) {
            widths[8] += px(STATUS_SYMBOL) + STATUS_GAP;
        }
        widths
    }

    fn measure_text(&self, text: &SharedString, window: &Window, cx: &App) -> Pixels {
        let mut style = window.text_style();
        style.font_family = cx.theme().font_family.clone();
        style.font_features = tabular();
        window.text_system().layout_line(text, TEXT_CAPTION, &[style.to_run(text.len())], None).width
    }

    /// The strip's width, from its last prepaint. True when that changes which fields show or
    /// which form the position takes: the caller asks for another frame, since a notify during
    /// prepaint wakes nothing.
    fn set_room(&mut self, width: Pixels) -> bool {
        if self.room == Some(width) {
            return false;
        }
        let before = self.layout();
        self.room = Some(width);
        self.layout() != before
    }

    /// The fields shown and whether the position is short, once everything is measured.
    fn layout(&self) -> Option<(Shown, bool)> {
        let (widths, (full, short)) = (self.measured?, self.position_widths?);
        let floor = floor(short);
        let shown = shown(&widths, self.room, floor);
        Some((shown, compact(&widths, &shown, self.room, full, floor)))
    }

    /// Wakes the strip when the keyboard enters or leaves `handle` (the pane's editor) and when the window turns key or not, so its dim can ease.
    fn watch_focus(&mut self, handle: &FocusHandle, window: &mut Window, cx: &mut Context<Self>) {
        if self.watched.as_ref() == Some(handle) {
            return;
        }
        self.watched = Some(handle.clone());
        self.watch = vec![
            cx.on_focus_in(handle, window, |_, _, cx| cx.notify()),
            cx.on_focus_out(handle, window, |_, _, _, cx| cx.notify()),
            cx.observe_window_activation(window, |_, _, cx| cx.notify()),
        ];
    }
}

impl Render for StatusLine {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let light = theme::is_light(cx);
        // The text eases between its dim and lit alpha as the pane gains or loses the keyboard and the window turns key or not, on the fade the line numbers use.
        if let Some(editor) = self.editor.as_ref().and_then(WeakEntity::upgrade) {
            let handle = editor.read(cx).focus_handle(cx);
            self.watch_focus(&handle, window, cx);
            self.key = control::window_key(window) && handle.contains_focused(window, cx);
        }
        let now = motion::drawn_now(window.window_handle().window_id());
        let to = f32::from(self.key);
        let fade = self.lit.get_or_insert(Fade::at(to));
        fade.aim(to, now);
        let level = fade.level(now);
        if fade.is_moving(now) {
            window.request_animation_frame();
        }
        let ink = theme::ink(ALPHA_STATUS_DIM + (ALPHA_SECONDARY - ALPHA_STATUS_DIM) * level, cx);
        let lit = theme::ink(ALPHA_PRIMARY, cx);
        let wash = theme::ink(1., cx);
        let rule = px(1. / window.scale_factor());
        // The pill and the trail follow their watches: a new text is measured again, and a pill that appears fades in from nothing (a changed message does not restart it).
        let error = self.config.error();
        if self.pill_error != error {
            self.text.pill = error.as_ref().map(|error| SharedString::from(error.pill())).unwrap_or_default();
            self.pill_fade = match (&self.pill_error, &error) {
                (None, Some(_)) => Some(Fade::with(0., PILL_FADE)),
                (_, Some(_)) => self.pill_fade.take(),
                (_, None) => None,
            };
            self.pill_error = error;
            self.measured = None;
        }
        let trail = self.trail.trail();
        if self.shown_trail != trail {
            self.text.trail = trail.as_ref().map(|trail| SharedString::from(trail.short_label(TRAIL_CHARS))).unwrap_or_default();
            self.shown_trail = trail;
            self.measured = None;
        }
        let pill_alpha = match self.pill_fade.as_mut() {
            Some(fade) => {
                fade.aim(1., now);
                if fade.is_moving(now) {
                    window.request_animation_frame();
                }
                fade.level(now)
            }
            None => 1.,
        };
        let pill_tip = self.pill_error.as_ref().map(|error| error.tooltip()).unwrap_or_default();
        let reopenable = self.fields.as_ref().is_some_and(|fields| fields.reopenable);
        let widths = match self.measured {
            Some(widths) => widths,
            None => {
                let widths = self.measure(window, cx);
                self.measured = Some(widths);
                widths
            }
        };
        let (full, short) = match self.position_widths {
            Some(both) => both,
            None => {
                let both = (self.measure_text(&self.text.position, window, cx), self.measure_text(&self.text.short, window, cx));
                self.position_widths = Some(both);
                both
            }
        };
        let floor = floor(short);
        let shown = shown(&widths, self.room, floor);
        let position = if compact(&widths, &shown, self.room, full, floor) { self.text.short.clone() } else { self.text.position.clone() };
        let sum_copied = self.copied_sum.level(now, window);
        let trail_copied = self.copied_trail.level(now, window);
        // A field: its text in a padded box. The text truncates with "…" where the box is squeezed, so a field shrinks only where `shrinks`. `copied` (0 to 1) crossfades the text to "Copied" in the same place: the box keeps the text's width, so nothing in the strip moves.
        let me = cx.entity_id();
        let field_led = |id: &'static str, lead: Option<&'static str>, text: &SharedString, shrinks: bool, copied: f32| {
            div()
                .id(id)
                .relative()
                .px(STATUS_FIELD_PAD)
                .h(STATUS_FIELD_HEIGHT)
                .min_w_0()
                .flex()
                .items_center()
                .when(lead.is_some(), |field| field.gap(STATUS_GAP))
                .when(shrinks, |field| field.flex_shrink(1.))
                .when(!shrinks, |field| field.flex_none())
                .rounded(RADIUS_STATUS_FIELD)
                .children(lead.map(|name| div().flex_none().w(px(STATUS_SYMBOL)).flex().items_center().justify_center().child(crate::symbols::icon(name, STATUS_SYMBOL))))
                .child(div().min_w_0().overflow_hidden().text_ellipsis().opacity(1. - copied).child(text.clone()))
                .when(copied > 0., |field| {
                    let lead_width = if lead.is_some() { px(STATUS_SYMBOL) + STATUS_GAP } else { px(0.) };
                    field.child(div().absolute().top_0().bottom_0().left(STATUS_FIELD_PAD + lead_width).flex().items_center().text_color(lit).opacity(copied).child(COPIED))
                })
                .children(control::track(id))
        };
        let field = |id: &'static str, text: &SharedString, shrinks: bool| field_led(id, None, text, shrinks, 0.);
        // A clickable field: a wash that fades in under the pointer and on to the press, and text that lifts with it.
        let button_led = |id: &'static str, lead: Option<&'static str>, text: &SharedString, shrinks: bool, copied: f32| {
            field_led(id, lead, text, shrinks, copied)
                .fade_hover((id, me), None, wash.opacity(ALPHA_BUTTON_HOVER))
                .press_fill(wash.opacity(ALPHA_BUTTON_PRESSED))
                .fade_ink(ink, lit)
        };
        let button = |id: &'static str, text: &SharedString, shrinks: bool| button_led(id, None, text, shrinks, 0.);
        // The writing goal's hairline lies over the strip's top rule, the bottom edge of the text: one quad from the left edge, as long as the count's share of the goal. It is the accent while the goal is ahead, glows once on reaching it (a short pulse that ends) and settles to the theme's green.
        let goal_line = match self.fields.as_ref().and_then(|fields| fields.goal.zip(fields.words)) {
            Some((goal, words)) => {
                self.leaving = false;
                let share = (words as f32 / goal as f32).min(1.);
                let fill = self.fill.get_or_insert(Fade::with(share, GOAL_FILL));
                fill.aim(share, now);
                let drawn = fill.level(now);
                if fill.is_moving(now) {
                    window.request_animation_frame();
                }
                if std::mem::take(&mut self.glow_pending) {
                    self.glow = Some(now);
                }
                let glow = self.glow.map(|since| now.saturating_duration_since(since).as_secs_f32() / GOAL_GLOW).filter(|glow| *glow < 1.);
                if glow.is_some() {
                    window.request_animation_frame();
                } else {
                    self.glow = None;
                }
                let settled = if words >= goal { motion::ease_out(glow.unwrap_or(1.)) } else { 0. };
                let color = crate::tab_bar::mix(crate::explorer::icons::accent(), cx.theme().green, settled);
                self.goal_color = color;
                let height = rule * (1. + 2. * glow.map_or(0., |glow| (glow * std::f32::consts::PI).sin()));
                Some(div().absolute().top_0().left_0().h(height).w(relative(drawn)).bg(color))
            }
            // A goal cleared from the menu fades out where it stands and drops once it lands; any other end (a file switched, a count lost) drops at once.
            None => match self.fill.as_mut().filter(|_| self.leaving) {
                Some(fill) => {
                    fill.aim(0., now);
                    self.glow = None;
                    self.glow_pending = false;
                    if fill.is_moving(now) {
                        window.request_animation_frame();
                        Some(div().absolute().top_0().left_0().h(rule).w(relative(fill.level(now))).bg(self.goal_color))
                    } else {
                        self.fill = None;
                        self.leaving = false;
                        None
                    }
                }
                None => {
                    self.fill = None;
                    self.leaving = false;
                    self.glow = None;
                    self.glow_pending = false;
                    None
                }
            },
        };
        let status = cx.entity();
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .whitespace_nowrap()
            .flex()
            .items_center()
            .justify_between()
            // Forgets the rects of fields the narrow pane dropped, so `debug rects` shows only what is drawn.
            .children(control::fresh("status-"))
            // The padding keeps a clickable field's wash inside the inset: its own padding is part of it.
            .px(STATUS_INSET - STATUS_FIELD_PAD)
            .bg(chrome_ground(light))
            .text_size(TEXT_CAPTION)
            .text_color(ink)
            .font_family(cx.theme().font_family.clone())
            .font_features(tabular())
            .child(div().absolute().top_0().left_0().right_0().h(rule).bg(cx.theme().border))
            .children(goal_line)
            // The strip's width, for the fields a narrow pane drops (`shown`).
            .child(
                canvas(
                    move |bounds, window, cx| {
                        if status.update(cx, |this, _| this.set_room(bounds.size.width)) {
                            window.request_animation_frame();
                        }
                    },
                    |_, (), _, _| {},
                )
                    .absolute()
                    .inset_0(),
            )
            // In a narrow pane the position gives way first: the file's fields keep their room.
            .child(
                div().min_w_0().flex_shrink(1.).flex().gap(STATUS_GAP).child(
                    button("status-position", &position, true)
                        .tooltip(tooltip::tip(tooltip::with_key("Go to line", "\u{2318}L")))
                        .tooltip_show_delay(tooltip::DELAY)
                        .on_click(cx.listener(|this, _, window, cx| this.go_to_line(window, cx))),
                )
                // The config pill: amber, fading in when the file stops parsing and gone at once when it parses again.
                .when(shown.pill && !self.text.pill.is_empty(), |group| {
                    group.child(
                        field("status-config", &self.text.pill, false)
                            .fade_hover(("status-config", me), None, wash.opacity(ALPHA_BUTTON_HOVER))
                            .press_fill(wash.opacity(ALPHA_BUTTON_PRESSED))
                            .text_color(config_check::pill_ink(cx).opacity(pill_alpha))
                            .tooltip(tooltip::tip(pill_tip.clone()))
                            .tooltip_show_delay(tooltip::DELAY)
                            .on_click(cx.listener(|this, _, window, cx| this.jump_to_error(window, cx))),
                    )
                })
                .when(shown.words && !self.text.words.is_empty(), |group| {
                    group.child(
                        button("status-words", &self.text.words, false)
                            .tooltip(tooltip::tip(self.text.words_tip.clone()))
                            .tooltip_show_delay(tooltip::DELAY)
                            .on_click(cx.listener(|this, _, window, _| this.words_menu(window))),
                    )
                })
                .when(shown.sum && !self.text.sum.is_empty(), |group| {
                    group.child(
                        button_led("status-sum", None, &self.text.sum, false, sum_copied)
                            .tooltip(tooltip::tip("Copy the sum"))
                            .tooltip_show_delay(tooltip::DELAY)
                            .on_click(cx.listener(|this, _, _, cx| this.copy_sum(cx))),
                    )
                })
                .when(shown.cursors && !self.text.cursors.is_empty(), |group| group.child(field("status-cursors", &self.text.cursors, false)))
                .when(shown.trail && !self.text.trail.is_empty(), |group| {
                    group.child(
                        button_led("status-trail", Some("list.bullet.indent"), &self.text.trail, true, trail_copied)
                            .tooltip(tooltip::tip("Copy the path (Option-click: a jq filter)"))
                            .tooltip_show_delay(tooltip::DELAY)
                            .on_click(cx.listener(|this, event: &ClickEvent, _, cx| this.copy_trail(event.modifiers().alt, cx))),
                    )
                }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .flex()
                    .items_center()
                    .gap(STATUS_GAP)
                    .when(shown.language, |group| group.child(field("status-language", &self.text.language, false)))
                    .when(shown.indent, |group| {
                        group.child(
                            button("status-indent", &self.text.indent, false)
                                .tooltip(tooltip::tip("Convert indentation"))
                                .tooltip_show_delay(tooltip::DELAY)
                                .on_click(cx.listener(|this, _, window, _| this.indent_menu(window))),
                        )
                    })
                    .when(shown.encoding, |group| {
                        group.child(
                            button("status-encoding", &self.text.encoding, false)
                                .tooltip(tooltip::tip(if reopenable { "Reopen with or convert to encoding" } else { "Convert to encoding" }))
                                .tooltip_show_delay(tooltip::DELAY)
                                .on_click(cx.listener(|this, _, window, _| this.encoding_menu(window))),
                        )
                    })
                    .when(shown.endings, |group| {
                        group.child(
                            button("status-endings", &self.text.endings, false)
                                .tooltip(tooltip::tip("Line endings"))
                                .tooltip_show_delay(tooltip::DELAY)
                                .on_click(cx.listener(|this, _, window, _| this.endings_menu(window))),
                        )
                    }),
            )
            // The Writing goal field floats above the strip's left end, outside its clip.
            .children(self.goal_field.as_ref().map(|(field, _)| deferred(div().absolute().left(STATUS_INSET - STATUS_FIELD_PAD).bottom(STATUS_HEIGHT + px(6.)).w(GOAL_FIELD_WIDTH).child(field.clone()))))
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::input::Rope;
    use gpui_kit::{EntityId, Pixels, px};

    use super::{Alive, Chosen, Counted, Owners, Recent, STATUS_POSITION_MIN, Shown, analyse, arm_count, compact, edge_hash, floor, shown};

    fn counted(words: usize, revision: u64) -> Counted {
        Counted { stats: crate::word_count::Stats { words, ..Default::default() }, goal: None, revision }
    }

    /// The real order of `set`: unedited A, B, C with different counts each show their own count on every return, never another tab's.
    #[test]
    fn each_tab_gets_back_its_own_count() {
        let ids = [EntityId::from(1), EntityId::from(2), EntityId::from(3)];
        let words = [10usize, 20, 30];
        let mut recent = Recent::default();
        // What the strip holds: the editor it follows and that editor's count.
        let (mut at, mut held): (Option<EntityId>, Option<Counted>) = (None, None);
        for step in [0usize, 1, 0, 1, 2, 0, 2] {
            let back = recent.swap(at, held, ids[step], 5);
            // A tab seen before shows its own words; a first visit counts (as the background count would) and holds the result.
            held = back.or_else(|| Some(counted(words[step], 5)));
            assert_eq!(held.map(|counted| counted.stats.words), Some(words[step]), "step to tab {step}");
            at = Some(ids[step]);
        }
        // The one just left is kept under its own id, never under the tab moved to.
        let mut recent = Recent::default();
        assert!(recent.swap(Some(ids[0]), Some(counted(10, 5)), ids[1], 5).is_none());
        assert_eq!(recent.0.len(), 1);
        assert_eq!(recent.0[0].0, ids[0]);
    }

    /// Set A, B, A at one revision: the way back finds A's count and asks for none; a changed revision or a third tab pushing it out asks again.
    #[test]
    fn switching_back_asks_no_count() {
        let (a, b, c) = (EntityId::from(1), EntityId::from(2), EntityId::from(3));
        let mut recent = Recent::default();
        let mut asks = 0;
        // The strip follows A (counted at revision 4), then B (none yet), then A again.
        recent.keep(a, Some(counted(120, 4)));
        if recent.take(b, 9).is_none() {
            asks += 1;
        }
        recent.keep(b, Some(counted(7, 9)));
        let back = recent.take(a, 4);
        eprintln!("counts asked over set A, B, A: {asks} (the return asked none)");
        assert_eq!(back.map(|counted| counted.stats.words), Some(120));
        assert_eq!(asks, 1);
        // A different revision is not used.
        recent.keep(a, Some(counted(120, 4)));
        assert!(recent.take(a, 5).is_none());
        // Bounded at two: the oldest goes when a third is kept.
        recent.keep(a, Some(counted(1, 1)));
        recent.keep(b, Some(counted(2, 1)));
        recent.keep(c, Some(counted(3, 1)));
        assert!(recent.take(a, 1).is_none());
        assert!(recent.take(c, 1).is_some());
    }

    /// An edit per key asks for a count at every revision: one task serves the burst, and its deadline and text move with each ask.
    #[test]
    fn a_burst_of_edits_starts_one_count_task() {
        let (start, rope) = (std::time::Instant::now(), Rope::from("word"));
        let mut next = None;
        let tasks = (0..100u64).filter(|rev| arm_count(&mut next, start + std::time::Duration::from_millis(*rev), rope.clone(), *rev)).count();
        eprintln!("count tasks over 100 revisions: {tasks}");
        assert_eq!(tasks, 1);
        assert_eq!(next.map(|(due, _, rev)| (due, rev)), Some((start + std::time::Duration::from_millis(99), 99)));
    }

    /// A drag over a ~1 MB selection asks again at every step: held as a rope handle nothing is copied where it is picked (the old path copied each step's whole text), and the text built later counts right.
    #[test]
    fn a_long_drag_copies_nothing_where_it_is_picked() {
        let rope = Rope::from("word ".repeat(250_000).as_str());
        let (mut picked, mut before) = (0, 0);
        for step in 0..200 {
            let range = 0..500_000 + step * 2_500;
            let chosen = Chosen::Held(rope.clone(), range.clone());
            picked += chosen.copied();
            before += rope.slice(range.clone()).to_string().len();
            assert_eq!(analyse(&chosen.text(), true).0, Some(range.len().div_ceil(5)));
        }
        eprintln!("bytes copied on the main thread over 200 steps: before {before}, after {picked}");
        assert_eq!(picked, 0);
        assert!(before > 100_000_000);
    }

    #[test]
    fn language_is_rechecked_only_when_an_edge_line_changed() {
        let lines: Vec<String> = (0..100).map(|row| format!("line {row}")).collect();
        let hash_of = |lines: &[String]| edge_hash(&Rope::from(lines.join("\n").as_str()));
        let before = hash_of(&lines);
        let mut mid = lines.clone();
        mid[50].push_str(" edited in the middle");
        assert_eq!(hash_of(&mid), before);
        for row in [0, 4, 95, 99] {
            let mut edge = lines.clone();
            edge[row].push_str(" edited at the edge");
            assert_ne!(hash_of(&edge), before, "row {row}");
        }
        // Text past the clipped width isn't looked at, so it isn't hashed.
        let mut long = lines.clone();
        long[0] = "x".repeat(crate::language::LINE_CHARS);
        let long_before = hash_of(&long);
        long[0].push_str("tail");
        assert_eq!(hash_of(&long), long_before);
        // A new line count moves the edges.
        let mut longer = lines;
        longer.push("one more".into());
        assert_ne!(hash_of(&longer), before);
    }

    /// The four fields of the file; the word count shows nothing (width 0), so it takes no room.
    const WIDTHS: [Pixels; 9] = [px(30.), px(60.), px(70.), px(20.), px(0.), px(0.), px(0.), px(0.), px(0.)];

    fn count(shown: Shown) -> (bool, bool, bool, bool) {
        (shown.language, shown.indent, shown.encoding, shown.endings)
    }

    #[test]
    fn fields_drop_language_first_then_endings_then_encoding_then_indent() {
        assert_eq!(count(shown(&WIDTHS, None, STATUS_POSITION_MIN)), (true, true, true, true));
        assert_eq!(count(shown(&WIDTHS, Some(px(2000.)), STATUS_POSITION_MIN)), (true, true, true, true));
        // Everything: 12 + (72 + 12) + (180 + 4 * 12) + 4 * 4 = 340 wide.
        assert_eq!(count(shown(&WIDTHS, Some(px(340.)), STATUS_POSITION_MIN)), (true, true, true, true));
        assert_eq!(count(shown(&WIDTHS, Some(px(339.)), STATUS_POSITION_MIN)), (false, true, true, true));
        assert_eq!(count(shown(&WIDTHS, Some(px(293.)), STATUS_POSITION_MIN)), (false, true, true, false));
        assert_eq!(count(shown(&WIDTHS, Some(px(258.)), STATUS_POSITION_MIN)), (false, true, true, false));
        assert_eq!(count(shown(&WIDTHS, Some(px(257.)), STATUS_POSITION_MIN)), (false, true, false, false));
        assert_eq!(count(shown(&WIDTHS, Some(px(172.)), STATUS_POSITION_MIN)), (false, true, false, false));
        assert_eq!(count(shown(&WIDTHS, Some(px(171.)), STATUS_POSITION_MIN)), (false, false, false, false));
    }

    #[test]
    fn the_word_count_drops_after_the_language_and_before_the_line_endings() {
        let widths = [px(30.), px(60.), px(70.), px(20.), px(50.), px(0.), px(0.), px(0.), px(0.)];
        let kept = |room: f32| {
            let shown = shown(&widths, Some(px(room)), STATUS_POSITION_MIN);
            (shown.language, shown.words, shown.endings, shown.indent)
        };
        // Everything: the 340 of four fields and 50 + 12 + 4 more for the words.
        assert_eq!(kept(406.), (true, true, true, true));
        assert_eq!(kept(405.), (false, true, true, true));
        assert_eq!(kept(360.), (false, true, true, true));
        assert_eq!(kept(359.), (false, false, true, true));
        assert_eq!(kept(293.), (false, false, false, true));
    }

    #[test]
    fn the_number_sum_drops_before_everything_else() {
        let widths = [px(30.), px(60.), px(70.), px(20.), px(50.), px(100.), px(0.), px(0.), px(0.)];
        let kept = |room: f32| {
            let shown = shown(&widths, Some(px(room)), STATUS_POSITION_MIN);
            (shown.sum, shown.language, shown.words)
        };
        // The 406 of the five fields and 100 + 12 + 4 more for the sum.
        assert_eq!(kept(522.), (true, true, true));
        assert_eq!(kept(521.), (false, true, true));
        assert_eq!(kept(406.), (false, true, true));
        assert_eq!(kept(405.), (false, false, true));
    }

    #[test]
    fn position_goes_short_where_the_long_form_would_not_fit() {
        let all = shown(&WIDTHS, Some(px(340.)), STATUS_POSITION_MIN);
        // At 340 the position has its 72 and nothing more.
        assert!(compact(&WIDTHS, &all, Some(px(340.)), px(85.), STATUS_POSITION_MIN));
        assert!(!compact(&WIDTHS, &all, Some(px(340.)), px(72.), STATUS_POSITION_MIN));
        assert!(!compact(&WIDTHS, &all, Some(px(360.)), px(85.), STATUS_POSITION_MIN));
        assert!(!compact(&WIDTHS, &all, None, px(85.), STATUS_POSITION_MIN));
    }

    #[test]
    fn fields_drop_whole_before_a_selection_count_in_the_position_is_cut() {
        // A short position with "(226 selected)" is 157 wide: more than the 72 it keeps otherwise.
        let floor = floor(px(157.));
        assert_eq!(floor, px(157.));
        assert_eq!(super::floor(px(40.)), STATUS_POSITION_MIN);
        // Everything is 340 wide at the 72 floor, so 85 more with this one.
        let kept = |room: f32| count(shown(&WIDTHS, Some(px(room)), floor));
        assert_eq!(kept(425.), (true, true, true, true));
        assert_eq!(kept(424.), (false, true, true, true));
        // The position keeps its whole short form in the room the fields leave, down to the last field.
        for room in [171., 200., 260., 300., 424., 500.] {
            let shown = shown(&WIDTHS, Some(px(room)), floor);
            assert!(super::used(&WIDTHS, &shown, floor) <= px(room) || count(shown) == (false, false, false, false));
        }
    }

    #[derive(Clone)]
    struct Probe(std::rc::Weak<()>);

    impl Alive for Probe {
        fn alive(&self) -> bool {
            self.0.strong_count() > 0
        }
    }

    #[test]
    fn closed_documents_leave_the_owner_list_on_the_next_lookup_and_it_stays_flat() {
        let mut owners = Owners::<Probe>::default();
        for round in 0..20u64 {
            let documents: Vec<std::rc::Rc<()>> = (0..50).map(|_| std::rc::Rc::new(())).collect();
            for (n, document) in documents.iter().enumerate() {
                owners.insert(EntityId::from(round * 100 + n as u64), Probe(std::rc::Rc::downgrade(document)));
            }
            drop(documents);
            assert!(owners.find(EntityId::from(0)).is_none());
            assert_eq!(owners.0.len(), 0, "round {round}");
        }
        let kept = std::rc::Rc::new(());
        owners.insert(EntityId::from(7), Probe(std::rc::Rc::downgrade(&kept)));
        assert!(owners.find(EntityId::from(7)).is_some(), "a live document is still found");
    }
}
