//! Following a log file as another program writes it, and reading one at a glance.
//!
//! The editor keeps the follow state (`EditorState::following`, vendor): the caret and the view ride
//! the end while a file grows, and scrolling up, typing or moving the caret off the last line lets go.
//! Here live the pieces around it: the `Pace` that keeps a growing file's reloads 250 ms apart and
//! one at a time (`Workspace::reload_changed`), the "Following" chip in the editor's top right
//! (a float like the find float's: it fades in with a small rise, fades out and crossfades between
//! its two words, asking for frames only while it moves), and the log look.
//!
//! The log look is for a file named `.log` or one whose lines mostly begin with a timestamp
//! (decided once, from its first 200 lines, once it has five): ERROR and WARN lines take a light
//! tint, a leading timestamp dims, and a tick for each on the scrollbar. Only the lines on screen
//! and a screenful each way are tagged, from the scrollbar layer's prepaint (`sync`), and a
//! decoration is set only when the list differs from the last one (a `set` notifies the editor).
//! The ticks come from a scan of the whole text on the background executor, at most once per
//! `SCAN_EVERY`, mapped to display rows (soft wrap, folds) on the main thread when the scan or the
//! layout changes. Flagged lines a scan finds that the scan before didn't have (matched by the
//! content of their line head, so lines landing above old ticks don't make them new) are new: their
//! ticks fade in once over `ARRIVE` from the first frame they are drawn, never on a first scan or a
//! rescan of old lines, and frames are asked for only while the fade runs.

use std::{
    cell::{Cell, RefCell},
    collections::{HashMap, VecDeque, hash_map::DefaultHasher},
    hash::{Hash as _, Hasher as _},
    ops::Range,
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{BufferPoint, EditorState, Rope, RopeExt as _, TextDecoration, TextDecorationCollection},
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    motion, symbols, theme, tooltip,
    tokens::{ALPHA_PRIMARY, ALPHA_SECONDARY, ARRIVE, FADE_QUICK, FIELD_SYMBOL_GAP, FILTER_PAD_X, FIND_FIELD_HEIGHT, FIND_PAD, FIND_SYMBOL, MARK_MATCH_MIN, MARK_RADIUS, OVERLAY_ENTER, OVERLAY_ENTER_RISE, OVERLAY_EXIT, RADIUS_FLOAT_CAPSULE, SCROLL_INSET, SCROLL_KNOB, TEXT_BODY, TEXT_BODY_LINE},
};

/// A growing file is read at most this often.
pub const FOLLOW_TICK: Duration = Duration::from_millis(250);

/// The scan for scrollbar ticks runs at most this often.
const SCAN_EVERY: Duration = Duration::from_millis(300);

/// The longest the pace backs off to after a slow scan.
const SCAN_GAP_MAX: Duration = Duration::from_secs(3);

/// How much of a line is looked at for its level and timestamp.
const HEAD: usize = 120;

/// The lines tagged at once, whatever the window shows.
const TAG_LINES: usize = 600;

/// Most flagged lines the ticks keep (the newest); a larger file's scan still reads all of it, but holds at most twice this.
const TICK_LINES: usize = 50_000;

/// Most batches of new ticks that fade at once; an older one just lands.
const FADES_MAX: usize = 8;

/// A file larger than this has no ticks (the scan reads all of it).
const TICKS_MAX_BYTES: usize = 64 * 1024 * 1024;

/// A tint's strength over the ground.
const TINT_ALPHA: f32 = 0.11;

/// A timestamp's ink: the text's own, let down to this much of it.
const DIM_ALPHA: f32 = 0.5;

/// How one growing file's reloads are paced in a window: when the last began, whether one is
/// reading now, whether another came in meanwhile, and the wait that will start the next.
#[derive(Default)]
pub struct Pace {
    pub last: Option<Instant>,
    pub busy: bool,
    /// The `reload_changed` batch number of the read in flight while `busy`.
    pub reading: u64,
    pub again: bool,
    pub timer: Option<Task<()>>,
}

/// A line's level.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Error,
    Warn,
}

/// The length of the timestamp a line starts with, brackets included: `2024-05-01 12:00:00,123`,
/// `[2024-05-01T12:00:00.5Z]`, `12:00:00.250` or syslog's `May  1 12:00:00`.
pub fn timestamp_len(line: &str) -> Option<usize> {
    let b = line.as_bytes();
    let open = b.first() == Some(&b'[');
    let start = usize::from(open);
    let digits = |at: usize, n: usize| b.get(at..at + n).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    let end = if digits(start, 4) && b.get(start + 4) == Some(&b'-') && digits(start + 5, 2) && b.get(start + 7) == Some(&b'-') && digits(start + 8, 2) {
        if !matches!(b.get(start + 10), Some(b'T' | b' ')) {
            return None;
        }
        clock(b, start + 11)?
    } else if digits(start, 2) && b.get(start + 2) == Some(&b':') {
        clock(b, start)?
    } else {
        const MONTHS: [&[u8]; 12] = [b"Jan", b"Feb", b"Mar", b"Apr", b"May", b"Jun", b"Jul", b"Aug", b"Sep", b"Oct", b"Nov", b"Dec"];
        if open || !b.get(..3).is_some_and(|m| MONTHS.contains(&m)) || b.get(3) != Some(&b' ') {
            return None;
        }
        let mut at = 4;
        while b.get(at) == Some(&b' ') {
            at += 1;
        }
        let day = b[at.min(b.len())..].iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=2).contains(&day) || b.get(at + day) != Some(&b' ') {
            return None;
        }
        clock(b, at + day + 1)?
    };
    Some(if open && b.get(end) == Some(&b']') { end + 1 } else { end })
}

/// The end of a `12:00:00` clock at `at` with its fraction and zone, if there is one.
fn clock(b: &[u8], at: usize) -> Option<usize> {
    let digits = |at: usize, n: usize| b.get(at..at + n).is_some_and(|s| s.iter().all(u8::is_ascii_digit));
    if !(digits(at, 2) && b.get(at + 2) == Some(&b':') && digits(at + 3, 2) && b.get(at + 5) == Some(&b':') && digits(at + 6, 2)) {
        return None;
    }
    let mut end = at + 8;
    if matches!(b.get(end), Some(b'.' | b',')) {
        let fraction = b[end + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if fraction > 0 {
            end += 1 + fraction;
        }
    }
    match b.get(end) {
        Some(b'Z') => end += 1,
        Some(b'+' | b'-') if digits(end + 1, 2) => {
            end += 3;
            if b.get(end) == Some(&b':') && digits(end + 1, 2) {
                end += 3;
            } else if digits(end, 2) {
                end += 2;
            }
        }
        _ => {}
    }
    Some(end)
}

/// The level a line shows in its first bytes: ERROR, FATAL, CRITICAL, SEVERE, PANIC or WARN(ING) as
/// a word in capitals, or any case between brackets, quotes or after `=`, as in `[error]` or `level=warn`.
pub fn level(head: &str) -> Option<Level> {
    let b = head.as_bytes();
    let mut at = 0;
    while at < b.len() {
        if !b[at].is_ascii_alphabetic() {
            at += 1;
            continue;
        }
        let start = at;
        while at < b.len() && b[at].is_ascii_alphabetic() {
            at += 1;
        }
        let word = &head[start..at];
        let capitals = word.bytes().all(|c| c.is_ascii_uppercase());
        let framed = start > 0 && matches!(b[start - 1], b'[' | b'=' | b'"') && matches!(b.get(at), None | Some(b']' | b'"' | b' ' | b',' | b'}'));
        if !(capitals || framed) {
            continue;
        }
        match word.to_ascii_uppercase().as_str() {
            "ERROR" | "FATAL" | "CRITICAL" | "SEVERE" | "PANIC" => return Some(Level::Error),
            "WARN" | "WARNING" => return Some(Level::Warn),
            _ => {}
        }
    }
    None
}

/// The start of a line, up to `HEAD` bytes, cut at a character.
fn head<'a>(chunks: impl Iterator<Item = &'a str>, scratch: &'a mut String) -> &'a str {
    let mut chunks = chunks;
    let first = chunks.next().unwrap_or("");
    if first.len() >= HEAD {
        return &first[..first.floor_char_boundary(HEAD)];
    }
    let Some(mut next) = chunks.next() else { return first };
    #[cfg(test)]
    JOINS.with(|joins| joins.set(joins.get() + 1));
    scratch.clear();
    scratch.push_str(first);
    loop {
        scratch.push_str(next);
        match chunks.next() {
            Some(more) if scratch.len() < HEAD => next = more,
            _ => break,
        }
    }
    scratch.truncate(scratch.floor_char_boundary(HEAD));
    scratch
}

#[cfg(test)]
thread_local! {
    /// How many heads a test's thread joined from several chunks into the scratch buffer.
    static JOINS: Cell<usize> = const { Cell::new(0) };
}

/// Whether the text reads as a log by its first 200 lines: at least five that aren't blank and
/// four in five of them begin with a timestamp. `None` while there are fewer than five.
fn looks_like_log(text: &Rope) -> Option<bool> {
    let (mut lines, mut stamped) = (0, 0);
    let mut scratch = String::new();
    for line in text.iter_lines().take(200) {
        let head = head(line.chunks(), &mut scratch);
        if head.trim().is_empty() {
            continue;
        }
        lines += 1;
        stamped += usize::from(timestamp_len(head).is_some());
    }
    (lines >= 5).then(|| stamped * 5 >= lines * 4)
}

/// What is set in the tint collection, line by line: a range, its ink and its ground.
type Mark = (Range<usize>, Option<Hsla>, Option<Hsla>);

/// The tint and tick state of one editor.
pub struct Log {
    editor: WeakEntity<EditorState>,
    /// Whether the text reads as a log: a name settles it, otherwise the first lines once there are five.
    kind: Cell<Option<bool>>,
    collection: RefCell<Option<TextDecorationCollection>>,
    applied: RefCell<Applied>,
    ticks: RefCell<Ticks>,
}

#[derive(Default)]
struct Applied {
    rev: u64,
    window: Range<usize>,
    marks: Vec<Mark>,
}

#[derive(Default)]
struct Ticks {
    /// The text revision and length the last scan was asked for.
    want: Option<(u64, usize)>,
    /// A scan is waiting its turn or running.
    scanning: bool,
    /// When the last scan landed and how long it took; the next waits `scan_gap` of that.
    last_scan: Option<(Instant, Duration)>,
    /// The newest flagged buffer lines from the last scan (at most `TICK_LINES`; each with its text key and the batch it was born in, 0 for none), and how many scans have landed.
    lines: Arc<Vec<(u32, Level, u64, u64)>>,
    lines_rev: u64,
    /// Display rows, text lines and line height `rows` were mapped for.
    layout: Option<(usize, usize, u64, u32)>,
    /// Runs of display rows with the same level.
    rows: Vec<(u32, u32, Level, u64)>,
    rows_rev: u64,
    line_height: f32,
    /// Rows revision, track start, track, content and device pixel the quads were built for.
    pixels: Option<(u64, u32, u32, u32, u32)>,
    /// A quad's top, bottom, level and the batch it was born in (0 for none).
    quads: Vec<(f32, f32, Level, u64)>,
    /// New ticks still fading in, oldest first.
    batches: Vec<Batch>,
}

/// The flagged lines a scan found that the scan before didn't have, fading in together.
#[derive(Clone)]
struct Batch {
    /// The scan that found it (`Ticks::lines_rev` when it landed), also its tag on rows and quads.
    id: u64,
    /// The first frame its ticks were drawn; the fade runs from there.
    since: Option<Instant>,
}

#[derive(Default)]
struct Book(HashMap<EntityId, Rc<Log>>);

impl Global for Book {}

/// An editor was built for a file named `named_log` or not: it can show the log look when its text reads as a log.
pub fn attach(editor: &Entity<EditorState>, named_log: bool, cx: &mut App) {
    let book = cx.default_global::<Book>();
    book.0.retain(|_, log| log.editor.upgrade().is_some());
    let id = editor.entity_id();
    cx.observe_release(editor, move |_, cx| {
        cx.default_global::<Book>().0.remove(&id);
    })
    .detach();
    let book = cx.default_global::<Book>();
    book.0.insert(
        editor.entity_id(),
        Rc::new(Log {
            editor: editor.downgrade(),
            kind: Cell::new(named_log.then_some(true)),
            collection: RefCell::new(None),
            applied: RefCell::new(Applied::default()),
            ticks: RefCell::new(Ticks::default()),
        }),
    );
}

/// How many editors the book holds, for `tab-state`.
pub fn book_size(cx: &App) -> usize {
    cx.try_global::<Book>().map_or(0, |book| book.0.len())
}

fn log_of(editor: EntityId, cx: &App) -> Option<Rc<Log>> {
    cx.try_global::<Book>()?.0.get(&editor).cloned()
}

/// Brings the tints of `editor` up to date with what it shows and starts a scan for the ticks when
/// the text changed; call it every frame the editor draws its scrollbar. A file that isn't a log
/// costs one lookup. `view` is woken when a scan lands.
pub fn sync(editor: &Entity<EditorState>, view: EntityId, cx: &mut App) {
    let Some(log) = log_of(editor.entity_id(), cx) else { return };
    if log.kind.get() == Some(false) {
        return;
    }
    let (text, rev, rows, display, line_height) = {
        let state = editor.read(cx);
        (state.text().clone(), state.text_revision(), state.visible_row_range(), state.display_map().display_row_count(), state.line_height())
    };
    if log.kind.get().is_none() {
        let Some(kind) = looks_like_log(&text) else { return };
        log.kind.set(Some(kind));
        if !kind {
            return;
        }
    }
    if let Some(rows) = rows {
        log.tint(editor, &text, rev, rows, cx);
    }
    log.mark(editor, &text, rev, display, line_height, view, cx);
}

impl Log {
    /// Tags the lines on screen and a screenful each way: the level's tint over the line, the
    /// timestamp dimmed.
    fn tint(&self, editor: &Entity<EditorState>, text: &Rope, rev: u64, rows: Range<usize>, cx: &mut App) {
        let total = text.lines_len();
        let slack = rows.len().max(8) / 2;
        {
            let applied = self.applied.borrow();
            if applied.rev == rev && applied.window.start <= rows.start.saturating_sub(slack) && applied.window.end >= (rows.end + slack).min(total) && !applied.window.is_empty() {
                return;
            }
        }
        let margin = rows.len().max(8);
        let from = rows.start.saturating_sub(margin);
        let to = (rows.end + margin).min(total).min(from + TAG_LINES);
        let dim = cx.theme().background.blend(cx.theme().foreground.opacity(DIM_ALPHA));
        let (error, warn) = (cx.theme().danger.opacity(TINT_ALPHA), cx.theme().warning.opacity(TINT_ALPHA));
        let mut marks: Vec<Mark> = Vec::new();
        let mut scratch = String::new();
        for row in from..to {
            let (start, end) = (text.line_start_offset(row), text.line_end_offset(row));
            if start >= end {
                continue;
            }
            let line = head(text.slice_line(row).chunks(), &mut scratch);
            let ground = level(line).map(|level| if level == Level::Error { error } else { warn });
            let stamp = timestamp_len(line).map(|len| start + len.min(end - start));
            match (stamp, ground) {
                (Some(stamp), ground) => {
                    marks.push((start..stamp, Some(dim), ground));
                    if ground.is_some() && stamp < end {
                        marks.push((stamp..end, None, ground));
                    }
                }
                (None, Some(ground)) => marks.push((start..end, None, Some(ground))),
                (None, None) => {}
            }
        }
        {
            let mut applied = self.applied.borrow_mut();
            let same = applied.marks == marks;
            applied.rev = rev;
            applied.window = from..to;
            if same {
                return;
            }
            applied.marks = marks.clone();
        }
        let list: Vec<TextDecoration> = marks
            .iter()
            .map(|(range, color, background)| TextDecoration::new(range.clone(), HighlightStyle { color: *color, background_color: *background, ..Default::default() }))
            .collect();
        let made = self.collection.borrow().clone();
        match made {
            Some(collection) => collection.set(list, cx),
            None => {
                let collection = editor.update(cx, |state, cx| state.create_decorations_collection(list, cx));
                *self.collection.borrow_mut() = Some(collection);
            }
        }
    }

    /// Starts the scan for ticks when the text changed (at most once per `SCAN_EVERY`) and maps its
    /// lines to display rows when the scan or the layout changed.
    fn mark(self: &Rc<Self>, editor: &Entity<EditorState>, text: &Rope, rev: u64, display: usize, line_height: Option<Pixels>, view: EntityId, cx: &mut App) {
        if text.len() > TICKS_MAX_BYTES {
            return;
        }
        let ask = {
            let mut ticks = self.ticks.borrow_mut();
            let ask = ticks.want != Some((rev, text.len())) && !ticks.scanning;
            if ask {
                ticks.scanning = true;
            }
            ask
        };
        if ask {
            let (log, editor) = (self.clone(), editor.clone());
            let wait = self.ticks.borrow().last_scan.map_or(Duration::ZERO, |(at, cost)| (at + scan_gap(cost)).saturating_duration_since(Instant::now()));
            cx.spawn(async move |cx| {
                if !wait.is_zero() {
                    cx.background_executor().timer(wait).await;
                }
                let (text, rev) = cx.update(|cx| {
                    let state = editor.read(cx);
                    (state.text().clone(), state.text_revision())
                });
                let len = text.len();
                let began = Instant::now();
                let (old, first, id) = {
                    let ticks = log.ticks.borrow();
                    (ticks.lines.clone(), ticks.lines_rev == 0, ticks.lines_rev + 1)
                };
                // Flagged lines the scan before did not have (matched by their content, so a line landing above them doesn't make them new) are new, once a scan has landed: each carries the batch it was found in and they fade in together. A first scan, or a rescan of the same lines, brings none.
                let (lines, any_new) = cx
                    .background_executor()
                    .spawn(async move {
                        let lines = scan(&text);
                        if first { (lines, false) } else { mark_new(&old, lines, id) }
                    })
                    .await;
                let cost = began.elapsed();
                let mut ticks = log.ticks.borrow_mut();
                if any_new {
                    ticks.batches.push(Batch { id, since: None });
                    let extra = ticks.batches.len().saturating_sub(FADES_MAX);
                    ticks.batches.drain(..extra);
                }
                ticks.lines = Arc::new(lines);
                ticks.lines_rev += 1;
                ticks.want = Some((rev, len));
                ticks.last_scan = Some((Instant::now(), cost));
                ticks.scanning = false;
                drop(ticks);
                cx.update(|cx| cx.notify(view));
            })
            .detach();
        }
        let Some(line_height) = line_height else { return };
        let mut ticks = self.ticks.borrow_mut();
        let layout = (display, text.lines_len(), ticks.lines_rev, f32::from(line_height).to_bits());
        if ticks.layout == Some(layout) {
            return;
        }
        ticks.layout = Some(layout);
        ticks.line_height = f32::from(line_height);
        ticks.rows.clear();
        let identity = display == text.lines_len();
        let state = editor.read(cx);
        let map = state.display_map();
        let Ticks { lines, rows, batches, .. } = &mut *ticks;
        let flagged = lines.len().min(if identity { TICK_LINES } else { 5_000 });
        for &(line, level, _, born) in lines.iter().skip(lines.len() - flagged) {
            let row = if identity { line } else { map.buffer_pos_to_display_pos(BufferPoint::new(line as usize, 0)).row as u32 };
            let born = if batches.iter().any(|batch| batch.id == born) { born } else { 0 };
            match rows.last_mut() {
                Some(run) if run.2 == level && run.3 == born && row <= run.1 + 1 => run.1 = run.1.max(row),
                _ => rows.push((row, row, level, born)),
            }
        }
        ticks.rows_rev += 1;
    }
}

/// The newest `TICK_LINES` flagged lines of `text`, as buffer lines in order. Each line's head is a
/// slice of the rope's own chunk (a scratch buffer joins it only when it straddles chunks), so a
/// scan allocates for the result alone, and the result never holds more than twice what is drawn.
fn scan(text: &Rope) -> Vec<(u32, Level, u64, u64)> {
    let mut flagged: Vec<(u32, Level, u64, u64)> = Vec::new();
    let mut scratch = String::new();
    for (row, line) in text.iter_lines().enumerate() {
        let head = head(line.chunks(), &mut scratch);
        if let Some(level) = level(head) {
            if flagged.len() >= TICK_LINES * 2 {
                flagged.drain(..flagged.len() - TICK_LINES);
            }
            let mut hasher = DefaultHasher::new();
            head.hash(&mut hasher);
            flagged.push((u32::try_from(row).unwrap_or(u32::MAX), level, hasher.finish(), 0));
        }
    }
    flagged.drain(..flagged.len().saturating_sub(TICK_LINES));
    flagged
}

/// `new` with each tick's batch id set: a tick that matches one of `old`'s (by the content of its line
/// head, each old one at most once) keeps that one's id, so lines landing above old ticks (which shift
/// every line number) leave them as they were; any other tick, including one that repeats an old one's
/// words, gets `id`. Also whether any tick got `id`.
fn mark_new(old: &[(u32, Level, u64, u64)], mut new: Vec<(u32, Level, u64, u64)>, id: u64) -> (Vec<(u32, Level, u64, u64)>, bool) {
    let mut seen: HashMap<u64, VecDeque<u64>> = HashMap::with_capacity(old.len());
    for &(_, _, key, born) in old {
        seen.entry(key).or_default().push_back(born);
    }
    let mut any = false;
    for tick in &mut new {
        match seen.get_mut(&tick.2).and_then(VecDeque::pop_front) {
            Some(born) => tick.3 = born,
            None => {
                tick.3 = id;
                any = true;
            }
        }
    }
    (new, any)
}

/// How long to wait after a scan that took `cost` before the next may start: `SCAN_EVERY`, or three
/// times the cost when that is longer (scans then take a quarter of a core at most), never over `SCAN_GAP_MAX`.
fn scan_gap(cost: Duration) -> Duration {
    SCAN_EVERY.max(cost * 3).min(SCAN_GAP_MAX)
}

/// Draws the ticks of `editor`'s flagged lines on its scrollbar's vertical track, `span` long from
/// `bounds`' top, in the same column as Find's ticks. `start` is where the track begins (below the find bar).
pub fn paint_ticks(editor: EntityId, bounds: Bounds<Pixels>, content: f32, span: f32, start: f32, window: &mut Window, cx: &App) {
    let Some(log) = log_of(editor, cx) else { return };
    if log.kind.get() != Some(true) {
        return;
    }
    let mut ticks = log.ticks.borrow_mut();
    let inset = f32::from(SCROLL_INSET);
    let track = span - start - inset;
    if ticks.rows.is_empty() || ticks.line_height <= 0. || track <= 0. || content <= 0. {
        return;
    }
    let device = 1. / window.scale_factor();
    let key = (ticks.rows_rev, start.to_bits(), track.to_bits(), content.to_bits(), device.to_bits());
    if ticks.pixels != Some(key) {
        ticks.pixels = Some(key);
        let (min, end) = (f32::from(MARK_MATCH_MIN), start + track);
        let per_row = track * ticks.line_height / content;
        let Ticks { rows, quads, .. } = &mut *ticks;
        quads.clear();
        for &(first, last, level, born) in rows.iter() {
            let (mut top, mut bottom) = (start + per_row * first as f32, start + per_row * (last + 1) as f32);
            if bottom - top < min {
                let grow = (min - (bottom - top)) / 2.;
                (top, bottom) = (top - grow, bottom + grow);
            }
            if top < start {
                (top, bottom) = (start, bottom + start - top);
            }
            if bottom > end {
                (top, bottom) = ((top - (bottom - end)).max(start), end);
            }
            match quads.last_mut() {
                Some(prev) if prev.2 == level && prev.3 == born && top <= prev.1 + device => prev.1 = prev.1.max(bottom),
                _ => quads.push((top, bottom, level, born)),
            }
        }
    }
    let theme = cx.theme();
    let (error, warn) = (theme.danger, theme.warning);
    let across = SCROLL_KNOB;
    let x = bounds.right() - px(inset) - across;
    // Batches of new ticks start fading at the first frame their rows are drawn (rows built for a
    // scan that landed after them wait), and a finished one leaves a rebuild behind so its ticks
    // join their neighbours again.
    let now = motion::drawn_now(window.window_handle().window_id());
    let built = ticks.layout.map_or(0, |layout| layout.2);
    let mut moving = false;
    let mut landed = false;
    for batch in ticks.batches.iter_mut().filter(|batch| batch.id <= built) {
        let since = *batch.since.get_or_insert(now);
        if now.saturating_duration_since(since).as_secs_f32() < ARRIVE {
            moving = true;
        } else {
            landed = true;
        }
    }
    let drawn = ticks.batches.clone();
    if landed {
        ticks.batches.retain(|batch| batch.id > built || batch.since.is_some_and(|since| now.saturating_duration_since(since).as_secs_f32() < ARRIVE));
        ticks.layout = None;
    }
    if moving || landed {
        window.request_animation_frame();
    }
    for &(top, bottom, level, born) in &ticks.quads {
        let alpha = match drawn.iter().find(|batch| batch.id == born) {
            Some(batch) if born != 0 => batch.since.map_or(0., |since| motion::ease_out((now.saturating_duration_since(since).as_secs_f32() / ARRIVE).min(1.))),
            _ => 1.,
        };
        if alpha <= 0. {
            continue;
        }
        let rect = Bounds::new(point(x, bounds.origin.y + px(top)), size(across, px(bottom - top)));
        let ink = if level == Level::Error { error } else { warn };
        window.paint_quad(fill(rect, ink.opacity(alpha)).corner_radii(Corners::all(MARK_RADIUS.min(px((bottom - top) / 2.)))));
    }
}

/// Where the chip is between gone (0) and shown (1), on the find float's recipe: expo-out over
/// `OVERLAY_ENTER` toward 1, ease-in over `OVERLAY_EXIT` toward 0, turning around from the level
/// it is drawn at.
struct Reveal {
    from: f32,
    open: bool,
    since: Instant,
}

impl Reveal {
    /// The level drawn at `now`, and whether it is still moving.
    fn level(&self, now: Instant) -> (f32, bool) {
        let secs = now.saturating_duration_since(self.since).as_secs_f32();
        if self.open {
            let t = secs / OVERLAY_ENTER;
            if t >= 1. { (1., false) } else { (self.from + (1. - self.from) * motion::expo_out(t), true) }
        } else {
            let t = secs / OVERLAY_EXIT;
            if t >= 1. { (0., false) } else { (self.from * (1. - motion::ease_in(t)), true) }
        }
    }
}

/// The chip in the editor's top right while a file is followed: "Following" with the end riding
/// along, or "Follow" resting after the user let go (a click, End or Cmd-Down follows again). It
/// follows the shown editor's `tailing` and `following` and is its own view, so a change draws the
/// chip alone. It fades in with a small rise and fades out on a short ease-in, as the find float
/// does, and the two words crossfade over `FADE_QUICK` ("ing" grows and fades in beside "Follow",
/// the ink easing between its two strengths). A change mid-motion turns around from where the chip
/// is drawn, and it asks for frames only while something moves.
pub struct FollowChip {
    editor: Option<Entity<EditorState>>,
    watch: Option<Subscription>,
    /// `None` while the editor isn't tailing; else whether it follows.
    shown: Option<bool>,
    reveal: Reveal,
    /// How far the chip reads "Following": 0 is "Follow", 1 "Following". It keeps its level while the chip leaves, so the word doesn't change on the way out.
    word: motion::Fade,
}

impl FollowChip {
    pub fn new() -> Self {
        Self { editor: None, watch: None, shown: None, reveal: Reveal { from: 0., open: false, since: Instant::now() }, word: motion::Fade::with(0., FADE_QUICK) }
    }

    /// The editor of the tab showing now (`None` for a Compare view or an unbuilt tab).
    pub fn watch(&mut self, editor: Option<Entity<EditorState>>, cx: &mut Context<Self>) {
        if self.editor.as_ref().map(Entity::entity_id) == editor.as_ref().map(Entity::entity_id) {
            return;
        }
        self.watch = editor.as_ref().map(|editor| {
            cx.observe(editor, |this, editor, cx| {
                let now = Self::state_of(&editor, cx);
                if now != this.shown {
                    this.shown = now;
                    cx.notify();
                }
            })
        });
        self.shown = editor.as_ref().and_then(|editor| Self::state_of(editor, cx));
        self.editor = editor;
        cx.notify();
    }

    fn state_of(editor: &Entity<EditorState>, cx: &App) -> Option<bool> {
        let state = editor.read(cx);
        state.tailing().then(|| state.following())
    }
}

impl Render for FollowChip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = motion::drawn_now(window.window_handle().window_id());
        let open = self.shown.is_some();
        if self.reveal.open != open {
            self.reveal = Reveal { from: self.reveal.level(now).0, open, since: now };
        }
        let (level, mut moving) = self.reveal.level(now);
        if let Some(following) = self.shown {
            let to = if following { 1. } else { 0. };
            if level < 0.01 {
                // Arriving from nothing shows its word at once; only the chip fades in.
                self.word = motion::Fade::with(to, FADE_QUICK);
            } else {
                self.word.aim(to, now);
            }
        }
        if level <= 0. && !open {
            return div().into_any_element();
        }
        let follow = self.word.level(now);
        moving |= self.word.is_moving(now);
        if moving {
            window.request_animation_frame();
        }
        let theme = cx.theme();
        let light = theme::is_light(cx);
        let (fill, border) = tooltip::float_fill(theme.background, theme.sidebar_foreground, light);
        let family = theme.font_family.clone();
        let ink = crate::tab_bar::mix(theme::ink(ALPHA_SECONDARY, cx), theme::ink(ALPHA_PRIMARY, cx), follow);
        let label = div().flex().items_center().whitespace_nowrap();
        let label = if follow >= 1. {
            label.child("Following")
        } else if follow <= 0. {
            label.child("Follow")
        } else {
            let mut style = window.text_style();
            style.font_family = family.clone();
            let ing = window.text_system().layout_line("ing", TEXT_BODY, &[style.to_run(3)], None).width;
            label.child("Follow").child(div().w(ing * follow).overflow_hidden().opacity(follow).child("ing"))
        };
        let content = div()
            .h(FIND_FIELD_HEIGHT)
            .px(FILTER_PAD_X)
            .flex()
            .items_center()
            .gap(FIELD_SYMBOL_GAP)
            .text_size(TEXT_BODY)
            .line_height(TEXT_BODY_LINE)
            .child(symbols::icon("arrow.down.to.line", FIND_SYMBOL).flex_none())
            .child(label);
        let chip = tooltip::float(RADIUS_FLOAT_CAPSULE, fill, border, light, false)
            .id("log-follow")
            .p(FIND_PAD)
            .font_family(family)
            .text_color(ink)
            .block_mouse_except_scroll()
            .when(self.shown == Some(false), |chip| chip.cursor_pointer())
            .on_click(cx.listener(|this, _, _, cx| {
                // A chip on its way out takes no click.
                if this.shown.is_none() {
                    return;
                }
                if let Some(editor) = this.editor.clone() {
                    editor.update(cx, |state, cx| state.follow_end(cx));
                }
            }))
            .child(content);
        div().mt(px(OVERLAY_ENTER_RISE * (1. - level))).opacity(level).child(chip).into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::{HEAD, JOINS, Level, SCAN_EVERY, SCAN_GAP_MAX, TICK_LINES, head, level, looks_like_log, mark_new, scan, scan_gap, timestamp_len};
    use gpui_kit::component::input::{Rope, RopeExt as _};
    use std::{cell::Cell, time::Duration};

    #[test]
    fn timestamps_at_the_start_of_a_line() {
        assert_eq!(timestamp_len("2024-05-01 12:00:00,123 INFO x"), Some(23));
        assert_eq!(timestamp_len("2024-05-01T12:00:00.5Z INFO x"), Some(22));
        assert_eq!(timestamp_len("[2024-05-01 12:00:00] INFO"), Some(21));
        assert_eq!(timestamp_len("2024-05-01T12:00:00+02:00 x"), Some(25));
        assert_eq!(timestamp_len("12:00:00.250 x"), Some(12));
        assert_eq!(timestamp_len("May  1 12:00:00 host x"), Some(15));
        assert_eq!(timestamp_len("Jun 12 12:00:00 host x"), Some(15));
        assert_eq!(timestamp_len("2024-05-01 is a date"), None);
        assert_eq!(timestamp_len("the 12:00:00 train"), None);
        assert_eq!(timestamp_len(""), None);
    }

    #[test]
    fn levels_in_the_first_words() {
        assert_eq!(level("2024-05-01 12:00:00 ERROR failed"), Some(Level::Error));
        assert_eq!(level("2024-05-01 12:00:00 WARN slow"), Some(Level::Warn));
        assert_eq!(level("12:00:00 [error] x"), Some(Level::Error));
        assert_eq!(level("ts level=warn msg"), Some(Level::Warn));
        assert_eq!(level("{\"level\":\"error\"}"), Some(Level::Error));
        assert_eq!(level("2024-05-01 12:00:00 INFO no errors, warnings or ERR_CONNECTION"), None);
        assert_eq!(level("an error in prose"), None);
    }

    #[test]
    fn a_log_is_told_by_its_first_lines() {
        let log = "2024-05-01 12:00:00 a\n".repeat(6);
        assert_eq!(looks_like_log(&Rope::from(log.as_str())), Some(true));
        assert_eq!(looks_like_log(&Rope::from("just\nsome\nplain\ntext\nlines\nhere\n")), Some(false));
        assert_eq!(looks_like_log(&Rope::from("2024-05-01 12:00:00 a\n")), None);
    }

    /// The scan as it was: an owned `String` for every line's head.
    fn old_scan(text: &Rope) -> (Vec<(u32, Level)>, usize) {
        let (mut flagged, mut strings) = (Vec::new(), 0);
        for (row, line) in text.iter_lines().enumerate() {
            let mut out = String::new();
            for chunk in line.chunks() {
                out.push_str(chunk);
                if out.len() >= HEAD {
                    break;
                }
            }
            out.truncate(out.floor_char_boundary(HEAD));
            strings += 1;
            if let Some(level) = level(&out) {
                flagged.push((row as u32, level));
            }
        }
        (flagged, strings)
    }

    fn big_log(lines: usize) -> String {
        let mut text = String::new();
        for i in 0..lines {
            let word = match i % 50 {
                0 => "ERROR",
                1 => "WARN",
                _ => "INFO",
            };
            text.push_str(&format!("2024-05-01 12:00:{:02}.{:03} {word} request {i} handled by worker with some padding\n", i % 60, i % 1000));
        }
        text
    }

    #[test]
    fn a_scan_of_a_big_append_allocates_no_string_per_line_and_finds_what_it_did() {
        let rope = Rope::from(big_log(100_000).as_str());
        let (old, old_strings) = old_scan(&rope);
        let before = JOINS.with(Cell::get);
        let found = scan(&rope);
        let joined = JOINS.with(Cell::get) - before;
        println!("100000 lines: head Strings allocated before {old_strings}; after 0 (reused scratch), {joined} heads copied into it");
        assert!(old_strings >= 100_000);
        assert!(joined * 10 < old_strings, "{joined} heads were joined from several chunks");
        assert_eq!(found.iter().map(|tick| (tick.0, tick.1)).collect::<Vec<_>>(), old[old.len() - found.len()..]);
        assert_eq!(found.len(), old.len().min(TICK_LINES));
    }

    #[test]
    fn ticks_keep_only_the_newest_lines_that_can_be_drawn() {
        let rope = Rope::from("ERROR x\n".repeat(TICK_LINES * 2 + 123).as_str());
        let found = scan(&rope);
        assert_eq!(found.len(), TICK_LINES);
        assert_eq!(found.last().map(|line| line.0), Some((TICK_LINES * 2 + 122) as u32));
        assert_eq!(found[0].0, (TICK_LINES + 123) as u32);
    }

    fn ticks_of(text: &str) -> Vec<(u32, Level, u64, u64)> {
        scan(&Rope::from(text))
    }

    fn ids(ticks: &[(u32, Level, u64, u64)]) -> Vec<(u32, u64)> {
        ticks.iter().map(|tick| (tick.0, tick.3)).collect()
    }

    #[test]
    fn a_line_inserted_above_leaves_old_ticks_seen_and_a_real_new_tick_is_new() {
        let base = "2024-05-01 12:00:01 INFO a\n2024-05-01 12:00:02 ERROR b\n2024-05-01 12:00:03 WARN c\n";
        let old = ticks_of(base);
        let shifted = ticks_of(&format!("2024-05-01 12:00:00 INFO top\n{base}"));
        assert_eq!(shifted.iter().map(|tick| tick.0).collect::<Vec<_>>(), [2, 3]);
        let (marked, any) = mark_new(&old, shifted, 7);
        assert!(!any);
        assert_eq!(ids(&marked), [(2, 0), (3, 0)]);
        let grown = ticks_of(&format!("2024-05-01 12:00:00 INFO top\n{base}2024-05-01 12:00:04 ERROR d\n"));
        let (marked, any) = mark_new(&old, grown, 7);
        assert!(any);
        assert_eq!(ids(&marked), [(2, 0), (3, 0), (4, 7)]);
        // A new error above the old ticks: only it is new, the old ones keep their ids.
        let above = ticks_of(&format!("2024-05-01 12:00:00 ERROR z\n{base}"));
        let (marked, any) = mark_new(&old, above, 8);
        assert!(any);
        assert_eq!(ids(&marked), [(0, 8), (2, 0), (3, 0)]);
        // Old ticks keep the batch they were born in.
        let born = mark_new(&[], ticks_of(base), 5).0;
        let (marked, any) = mark_new(&born, ticks_of(&format!("2024-05-01 12:00:00 INFO top\n{base}")), 9);
        assert!(!any);
        assert_eq!(ids(&marked), [(2, 5), (3, 5)]);
        let same = ticks_of("ERROR x\nERROR x\n");
        let (marked, any) = mark_new(&same[..1], same.clone(), 3);
        assert!(any);
        assert_eq!(ids(&marked), [(0, 0), (1, 3)]);
        assert!(!mark_new(&same, same.clone(), 3).1);
    }

    #[test]
    fn a_head_that_straddles_chunks_is_joined_and_cut_at_a_character() {
        let mut scratch = String::new();
        let joined = head(["ab", "cd"].into_iter(), &mut scratch);
        assert_eq!(joined, "abcd");
        let long = "é".repeat(100);
        let cut = head([long.as_str()].into_iter(), &mut scratch);
        assert_eq!(cut.len(), HEAD);
        let cut = head(["x", long.as_str()].into_iter(), &mut scratch);
        assert_eq!(cut.len(), HEAD - 1);
    }

    #[test]
    fn the_pace_backs_off_by_what_a_scan_cost() {
        assert_eq!(scan_gap(Duration::from_millis(5)), SCAN_EVERY);
        assert_eq!(scan_gap(Duration::from_millis(100)), SCAN_EVERY);
        assert_eq!(scan_gap(Duration::from_millis(400)), Duration::from_millis(1200));
        assert_eq!(scan_gap(Duration::from_secs(5)), SCAN_GAP_MAX);
    }
}
