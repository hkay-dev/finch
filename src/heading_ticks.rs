//! The outline scrubber: in a Markdown note with three or more headings, a thin column of ticks
//! (one per heading, longer for higher levels) beside the scrollbar's track. It fades with the
//! scrollbar (a hand scroll or the pointer in the column shows it), a pill names the heading
//! nearest the pointer, a click glides there as Go to line does (the caret moves and the editor
//! reveals it), and the tick of the section the top of the view is in stays lit.
//!
//! The scrollbar's layer draws and listens, so nothing here notifies the cached editor and
//! nothing runs at rest. Headings come from a line scan on the background executor after a pause
//! (`PAUSE`), dropped when the text moved on (`text_revision` and length, then a scan number),
//! kept as a small list: nothing is scanned past `MAX_BYTES`, and at most `MAX_HEADINGS` are kept.
//! Display rows (soft wrap, folds) are mapped on the main thread when the list or the layout
//! changes, ticks' pixels when the track's size does. Painting reads the scroll offset live.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{BufferPoint, EditorState, Rope, RopeExt as _},
};
use gpui_kit::*;

use crate::{scrollbar::Scroller, tokens::{SCROLL_INSET, SCROLL_TRACK}};

/// Headings a note needs before the column shows.
const MIN_HEADINGS: usize = 3;
/// Nothing is scanned in a text longer than this.
const MAX_BYTES: usize = 4 * 1024 * 1024;
/// The list keeps this many headings at most.
const MAX_HEADINGS: usize = 4000;
/// The text must stand still this long before it is scanned.
const PAUSE: Duration = Duration::from_millis(250);
/// Lines a scan walks between checks for a newer request.
const SCAN_CHUNK: usize = 2048;
/// A line is read up to this many bytes.
const LINE_HEAD: usize = 400;
/// A heading's name in the pill is cut to this many characters.
const TITLE_MAX: usize = 60;

/// The pointer-sensitive column, wide enough to hit, right of which sits the scrollbar's track.
const STRIP: f32 = 22.;
/// A tick's height, and the longest tick (level 1) across; each level down is 2.5 shorter, to a floor.
const TICK_HEIGHT: f32 = 2.;
const TICK_LONG: f32 = 15.;
const TICK_SHORT: f32 = 6.;
/// The gap between the strip's right edge and the ticks' right edge.
const TICK_PAD: f32 = 3.;
/// Ticks closer than this join: the higher level stays.
const TICK_GAP: f32 = 2.5;
const TICK_ALPHA: f32 = 0.38;
const TICK_ALPHA_HOVER: f32 = 0.8;
const TICK_ALPHA_ACTIVE: f32 = 0.95;

const PILL_FONT: f32 = 11.;
const PILL_HEIGHT: f32 = 22.;
const PILL_PAD: f32 = 8.;
const PILL_GAP: f32 = 4.;

#[derive(Clone, Debug, PartialEq)]
struct Heading {
    /// Buffer line, from 0.
    line: u32,
    level: u8,
    title: String,
}

#[derive(Default)]
struct State {
    /// The editor is showing Markdown.
    on: bool,
    /// The text revision and length the last scan was asked for.
    want: Option<(u64, usize)>,
    /// The newest scan requested; an older one stops, and its result is dropped.
    scan: Arc<AtomicU64>,
    headings: Vec<Heading>,
    headings_rev: u64,
    /// Display row count, headings revision and line height the rows were mapped for.
    layout: Option<(usize, u64, u32)>,
    /// Each heading's display row.
    rows: Vec<u32>,
    rows_rev: u64,
    line_height: f32,
    /// Rows revision, track start and length, content and device pixel the ticks were built for.
    pixels: Option<(u64, u32, u32, u32, u32)>,
    /// Ticks as height from the view's top and the heading they stand for, in order.
    ticks: Vec<(f32, usize)>,
    /// The view's top in the window, when `ticks` were last drawn.
    top: f32,
    /// The pointer-sensitive column in window coordinates, set while the column has ticks to show.
    strip: Option<Bounds<Pixels>>,
    /// When the pending scan starts; `Some` while a task is waiting for it.
    due: Option<Instant>,
    /// The text and scan number the waiting task will scan.
    pending: Option<(u64, Rope)>,
    /// The ticks drew at the last paint (the scrollbar was visible).
    shown: bool,
    /// The heading under the pointer, while it is in the column.
    hover: Option<usize>,
    inside: bool,
}

impl State {
    /// The heading whose tick is nearest to `y` (window coordinates).
    fn nearest(&self, y: f32) -> Option<usize> {
        let y = y - self.top;
        self.ticks.iter().min_by(|a, b| (a.0 - y).abs().total_cmp(&(b.0 - y).abs())).map(|tick| tick.1)
    }

    fn tick_of(&self, heading: usize) -> Option<f32> {
        self.ticks.iter().find(|tick| tick.1 == heading).map(|tick| tick.0)
    }

    /// Asks for a scan of `text` after the pause, cancelling one running. One task serves a whole
    /// burst: each ask moves its deadline. Returns true when no task was waiting, so the caller
    /// starts one.
    fn ask(&mut self, text: &Rope, now: Instant) -> bool {
        let id = self.scan.fetch_add(1, Ordering::Relaxed) + 1;
        if text.len() > MAX_BYTES {
            self.pending = None;
            if !self.headings.is_empty() {
                self.headings.clear();
                self.headings_rev += 1;
            }
            return false;
        }
        self.pending = Some((id, text.clone()));
        self.due.replace(now + PAUSE).is_none()
    }
}

struct Outline {
    editor: WeakEntity<EditorState>,
    state: RefCell<State>,
    /// The editor showed Markdown the last time it was asked.
    markdown: Cell<bool>,
}

#[derive(Default)]
struct Book(HashMap<EntityId, Rc<Outline>>);

impl Global for Book {}

/// How many editors the book holds, for `tab-state`.
pub fn book_size(cx: &App) -> usize {
    cx.try_global::<Book>().map_or(0, |book| book.0.len())
}

fn outline_of(editor: EntityId, cx: &App) -> Option<Rc<Outline>> {
    cx.try_global::<Book>()?.0.get(&editor).cloned()
}

/// Brings the headings of `editor` up to date with its text: a changed text starts a scan after a
/// pause, and a landed scan wakes `view`. Call it every frame the scrollbar's layer prepaints; a
/// file that isn't Markdown costs one name check.
pub fn sync(editor: &Entity<EditorState>, view: EntityId, cx: &mut App) {
    if editor.read(cx).language_name().as_ref() != "markdown" {
        if let Some(outline) = outline_of(editor.entity_id(), cx) {
            if outline.markdown.replace(false) {
                let mut this = outline.state.borrow_mut();
                this.scan.fetch_add(1, Ordering::Relaxed);
                *this = State { scan: this.scan.clone(), ..State::default() };
            }
        }
        return;
    }
    let outline = match outline_of(editor.entity_id(), cx) {
        Some(outline) => outline,
        None => {
            let book = cx.default_global::<Book>();
            book.0.retain(|_, outline| outline.editor.upgrade().is_some());
            let outline = Rc::new(Outline { editor: editor.downgrade(), state: RefCell::default(), markdown: Cell::new(true) });
            book.0.insert(editor.entity_id(), outline.clone());
            let id = editor.entity_id();
            cx.observe_release(editor, move |_, cx| {
                cx.default_global::<Book>().0.remove(&id);
            })
            .detach();
            outline
        }
    };
    outline.markdown.set(true);
    let asked = {
        let state = editor.read(cx);
        let mut this = outline.state.borrow_mut();
        this.on = true;
        let (rev, text) = (state.text_revision(), state.text());
        let mut asked = false;
        if this.want != Some((rev, text.len())) {
            this.want = Some((rev, text.len()));
            asked = this.ask(text, Instant::now());
        }
        if let Some(line_height) = state.line_height() {
            let map = state.display_map();
            let layout = (map.display_row_count(), this.headings_rev, f32::from(line_height).to_bits());
            if this.layout != Some(layout) {
                this.layout = Some(layout);
                this.line_height = f32::from(line_height);
                let State { headings, rows, .. } = &mut *this;
                rows.clear();
                rows.extend(headings.iter().map(|heading| map.buffer_pos_to_display_pos(BufferPoint::new(heading.line as usize, 0)).row as u32));
                this.rows_rev += 1;
            }
        }
        asked
    };
    if !asked {
        return;
    }
    cx.spawn(async move |cx| {
        loop {
            let due = outline.state.borrow().due;
            let Some(due) = due else { return };
            let now = Instant::now();
            if due > now {
                cx.background_executor().timer(due - now).await;
                continue;
            }
            break;
        }
        let (latest, request) = {
            let mut this = outline.state.borrow_mut();
            this.due = None;
            (this.scan.clone(), this.pending.take())
        };
        let Some((id, text)) = request else { return };
        let check = latest.clone();
        let Some(found) = cx.background_executor().spawn(async move { scan(&text, id, &check) }).await else { return };
        cx.update(|cx| {
            let mut this = outline.state.borrow_mut();
            if latest.load(Ordering::Relaxed) != id || this.headings == found {
                return;
            }
            this.headings = found;
            this.headings_rev += 1;
            drop(this);
            cx.notify(view);
        });
    })
    .detach();
}

/// The headings of `text` in order: ATX (`# Title`) and setext (a line over `===` or `---`),
/// never inside a fenced code block, an indented code block or YAML front matter. `None` when a
/// newer scan has been asked for.
fn scan(text: &Rope, id: u64, latest: &AtomicU64) -> Option<Vec<Heading>> {
    let mut found = Vec::new();
    // The fence open: its character and length.
    let mut fence: Option<(u8, usize)> = None;
    let mut front = false;
    // The last line, when it could be the text of a setext heading.
    let mut paragraph: Option<(u32, String)> = None;
    for (row, line) in text.iter_lines().enumerate() {
        if row % SCAN_CHUNK == 0 && latest.load(Ordering::Relaxed) != id {
            return None;
        }
        let row = u32::try_from(row).unwrap_or(u32::MAX);
        // A line sitting in one chunk (nearly all do) is read in place; one that spans chunks is joined.
        let joined;
        let head = match line.as_str() {
            Some(head) => head,
            None => {
                let mut head = String::new();
                for chunk in line.chunks() {
                    head.push_str(chunk);
                    if head.len() >= LINE_HEAD {
                        break;
                    }
                }
                joined = head;
                &joined
            }
        };
        let head = &head[..head.floor_char_boundary(LINE_HEAD)];
        let line = head.trim_end();
        if row == 0 && line == "---" {
            front = true;
            continue;
        }
        if front {
            front = !(line == "---" || line == "...");
            continue;
        }
        let indent = if line.starts_with('\t') { 4 } else { line.bytes().take_while(|b| *b == b' ').count() };
        let body = line.trim_start_matches([' ', '\t']);
        if let Some((mark, len)) = fence {
            if indent < 4 && body.bytes().next() == Some(mark) {
                let run = body.bytes().take_while(|b| *b == mark).count();
                if run >= len && body[run..].trim().is_empty() {
                    fence = None;
                }
            }
            continue;
        }
        if body.is_empty() {
            paragraph = None;
            continue;
        }
        if indent >= 4 {
            // Indented code, or a lazy continuation of a paragraph.
            continue;
        }
        if let Some(open) = fence_open(body) {
            fence = Some(open);
            paragraph = None;
            continue;
        }
        if let Some((level, title)) = atx(body) {
            paragraph = None;
            let title = clean(title);
            if !title.is_empty() {
                found.push(Heading { line: row, level, title });
            }
        } else if let Some(level) = setext(body).filter(|_| paragraph.is_some()) {
            if let Some((line, text)) = paragraph.take() {
                found.push(Heading { line, level, title: clean(&text) });
            }
        } else if starts_block(body) {
            paragraph = None;
        } else if paragraph.is_none() {
            paragraph = Some((row, body.to_owned()));
        } else if let Some((_, text)) = &mut paragraph {
            // A paragraph over several lines: the heading starts at its first line and reads all of them.
            text.push(' ');
            text.push_str(body);
        }
        if found.len() >= MAX_HEADINGS {
            break;
        }
    }
    found.retain(|heading| !heading.title.is_empty());
    Some(found)
}

/// A fence's opening line: its character and length.
fn fence_open(body: &str) -> Option<(u8, usize)> {
    let mark = *body.as_bytes().first().filter(|b| matches!(**b, b'`' | b'~'))?;
    let run = body.bytes().take_while(|b| *b == mark).count();
    (run >= 3 && !(mark == b'`' && body[run..].contains('`'))).then_some((mark, run))
}

/// An ATX heading's level and text, closing `#`s dropped.
fn atx(body: &str) -> Option<(u8, &str)> {
    let hashes = body.bytes().take_while(|b| *b == b'#').count();
    if !(1..=6).contains(&hashes) {
        return None;
    }
    let rest = &body[hashes..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let rest = rest.trim();
    let open = rest.trim_end_matches('#');
    let title = if open.is_empty() || (open.len() != rest.len() && open.ends_with([' ', '\t'])) { open.trim_end() } else { rest };
    Some((hashes as u8, title))
}

/// The level a setext underline makes: 1 for `=`, 2 for `-`.
fn setext(body: &str) -> Option<u8> {
    let body = body.trim_end();
    match body.as_bytes().first()? {
        b'=' if body.bytes().all(|b| b == b'=') => Some(1),
        b'-' if body.bytes().all(|b| b == b'-') => Some(2),
        _ => None,
    }
}

/// A line that opens a block that isn't a paragraph a setext underline could turn into a heading:
/// a quote, a list item or a rule.
fn starts_block(body: &str) -> bool {
    let digits = body.bytes().take_while(u8::is_ascii_digit).count();
    body.starts_with('>')
        || ["- ", "* ", "+ "].iter().any(|marker| body.starts_with(marker))
        || (digits > 0 && digits < 10 && [". ", ") "].iter().any(|marker| body[digits..].starts_with(marker)))
        || body.starts_with("***")
        || body.starts_with("___")
        || body.starts_with('|')
        || setext(body).is_some()
}

/// A heading's name as the pill shows it: emphasis and code marks and link targets dropped, runs
/// of space joined, cut to `TITLE_MAX` characters.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len().min(TITLE_MAX * 4));
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '`' | '*' | '[' => {}
            '\\' => out.extend(chars.next()),
            ']' if chars.peek() == Some(&'(') => {
                for skipped in chars.by_ref() {
                    if skipped == ')' {
                        break;
                    }
                }
            }
            ']' => {}
            c => out.push(c),
        }
    }
    let mut words = out.split_whitespace();
    let mut title = String::new();
    if let Some(first) = words.next() {
        title.push_str(first);
        for word in words {
            title.push(' ');
            title.push_str(word);
        }
    }
    if title.chars().count() > TITLE_MAX {
        title = title.chars().take(TITLE_MAX - 1).collect();
        title.push('…');
    }
    title
}

/// Draws the column of `editor`'s headings beside the track of the vertical scrollbar, in the
/// scrollbar's own fade (`visible`, 0 to 1). `bounds` is the view, `scrolled` the points scrolled
/// past its top, `content` the content's height, `span` the track's length and `start` where it
/// begins (below the find bar). Nothing for a note with fewer than three headings, or one that
/// doesn't scroll (`scrolls`).
pub fn paint(editor: EntityId, bounds: Bounds<Pixels>, scrolled: f32, content: f32, span: f32, start: f32, scrolls: bool, visible: f32, window: &mut Window, cx: &mut App) {
    let Some(outline) = outline_of(editor, cx) else { return };
    let mut this = outline.state.borrow_mut();
    let inset = f32::from(SCROLL_INSET);
    let track = span - start - inset;
    if !this.on || !scrolls || this.headings.len() < MIN_HEADINGS || this.line_height <= 0. || track <= 0. || content <= 0. {
        this.strip = None;
        this.shown = false;
        return;
    }
    let device = 1. / window.scale_factor();
    let key = (this.rows_rev, start.to_bits(), track.to_bits(), content.to_bits(), device.to_bits());
    this.top = f32::from(bounds.origin.y);
    if this.pixels != Some(key) {
        this.pixels = Some(key);
        let per_row = track * this.line_height / content;
        let State { headings, rows, ticks, .. } = &mut *this;
        ticks.clear();
        let last = (start + track - TICK_HEIGHT).max(start);
        for (ix, (heading, &row)) in headings.iter().zip(rows.iter()).enumerate() {
            let y = (start + per_row * (row as f32 + 0.5) - TICK_HEIGHT / 2.).clamp(start, last);
            match ticks.last_mut() {
                Some(prev) if y - prev.0 < TICK_GAP => {
                    if heading.level < headings[prev.1].level {
                        *prev = (prev.0, ix);
                    }
                }
                _ => ticks.push((y, ix)),
            }
        }
    }
    let right = bounds.right() - SCROLL_TRACK;
    this.strip = Some(Bounds::new(point(right - px(STRIP), bounds.origin.y + px(start)), size(px(STRIP), px(track))));
    this.shown = visible > 0.;
    if visible <= 0. {
        return;
    }
    // The section the top of the view is in: the last heading at or above its first row.
    let top_row = scrolled / this.line_height;
    let active = this.rows.partition_point(|&row| row as f32 <= top_row).checked_sub(1);
    let lit = active.and_then(|heading| this.ticks.partition_point(|tick| tick.1 <= heading).checked_sub(1));
    let hover = this.hover.and_then(|heading| this.ticks.iter().position(|tick| tick.1 == heading));
    let ink = cx.theme().foreground;
    let x_right = right - px(TICK_PAD);
    for (ix, &(y, heading)) in this.ticks.iter().enumerate() {
        let level = this.headings[heading].level as f32;
        let across = (TICK_LONG - 2.5 * (level - 1.)).max(TICK_SHORT);
        let alpha = if Some(ix) == hover {
            TICK_ALPHA_HOVER
        } else if Some(ix) == lit {
            TICK_ALPHA_ACTIVE
        } else {
            TICK_ALPHA
        };
        let rect = Bounds::new(point(x_right - px(across), bounds.origin.y + px(y)), size(px(across), px(TICK_HEIGHT)));
        window.paint_quad(fill(rect, ink.opacity(alpha * visible)).corner_radii(Corners::all(px(TICK_HEIGHT / 2.))));
    }
    if let (Some(heading), Some(y)) = (this.hover.filter(|_| this.inside), this.hover.and_then(|heading| this.tick_of(heading))) {
        let title = SharedString::from(this.headings[heading].title.clone());
        let theme = cx.theme();
        let (ground, text, border) = (theme.popover, theme.popover_foreground, theme.border);
        let mut run = window.text_style().to_run(title.len());
        run.color = text.opacity(visible);
        let line = window.text_system().shape_line(title, px(PILL_FONT), &[run], None);
        let width = f32::from(line.width) + 2. * PILL_PAD;
        let strip_left = this.strip.map_or(right, |strip| strip.left());
        let center = bounds.origin.y + px(y + TICK_HEIGHT / 2.);
        let (low, high) = (bounds.origin.y, bounds.bottom() - px(PILL_HEIGHT));
        let top = (center - px(PILL_HEIGHT / 2.)).max(low).min(high.max(low));
        let pill = Bounds::new(point(strip_left - px(PILL_GAP + width), top), size(px(width), px(PILL_HEIGHT)));
        window.paint_quad(
            fill(pill, ground.opacity(visible))
                .corner_radii(Corners::all(px(PILL_HEIGHT / 2.)))
                .border_widths(px(1.))
                .border_color(border.opacity(visible)),
        );
        line.paint(point(pill.origin.x + px(PILL_PAD), pill.origin.y + px((PILL_HEIGHT - PILL_FONT * 1.2) / 2.)), px(PILL_FONT * 1.2), gpui::TextAlign::Left, None, window, cx).ok();
    }
}

/// The pointer in the column: arriving shows the scrollbar and holds it (leaving restarts the
/// hold), a slide changes the heading the pill names, and a click moves the caret to the heading
/// so the editor glides there as it does for Go to line. Register it in the layer's paint, next to
/// the scrollbar's own listeners.
pub fn listen(editor: EntityId, scroller: &Scroller, window: &mut Window) {
    let view = window.current_view();
    let moved = scroller.clone();
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase != DispatchPhase::Capture {
            return;
        }
        let Some(outline) = outline_of(editor, cx) else { return };
        let mut this = outline.state.borrow_mut();
        let inside = this.strip.is_some_and(|strip| strip.contains(&event.position)) && !moved.grabbing();
        let hover = if inside { this.nearest(f32::from(event.position.y)) } else { None };
        if inside == this.inside && hover == this.hover {
            return;
        }
        let arrived = inside && !this.inside;
        this.inside = inside;
        this.hover = hover;
        drop(this);
        moved.hold(inside);
        if arrived || !inside {
            moved.touch(window);
        }
        cx.notify(view);
    });

    window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
            return;
        }
        let Some(outline) = outline_of(editor, cx) else { return };
        let this = outline.state.borrow();
        if !this.shown || !this.strip.is_some_and(|strip| strip.contains(&event.position)) {
            return;
        }
        let Some(heading) = this.nearest(f32::from(event.position.y)) else { return };
        let line = this.headings[heading].line as usize;
        drop(this);
        if let Some(editor) = outline.editor.upgrade() {
            editor.update(cx, |state, cx| {
                let offset = state.text().line_start_offset(line);
                state.set_selected_range(offset..offset, cx);
            });
        }
        cx.stop_propagation();
    });
}

#[cfg(test)]
mod tests {
    use std::{sync::atomic::AtomicU64, time::{Duration, Instant}};

    use super::{Rope, State, TITLE_MAX, clean, scan};

    #[test]
    fn a_burst_of_revisions_starts_one_task() {
        let mut state = State::default();
        let (start, text) = (Instant::now(), Rope::from("# One\n"));
        let tasks = (0..100).filter(|i| state.ask(&text, start + Duration::from_millis(*i))).count();
        eprintln!("tasks over 100 revisions: {tasks}");
        assert_eq!(tasks, 1);
        assert_eq!(state.due, Some(start + Duration::from_millis(99) + super::PAUSE));
        // The task fires and takes the request: the next revision starts a new one.
        state.due = None;
        assert!(state.ask(&text, start));
    }

    fn headings(text: &str) -> Vec<(u32, u8, String)> {
        let found = scan(&Rope::from(text), 1, &AtomicU64::new(1)).unwrap();
        found.into_iter().map(|heading| (heading.line, heading.level, heading.title)).collect()
    }

    #[test]
    fn reads_atx_and_setext_headings() {
        let text = "# One\n\ntext\n\nTwo\n===\n\n## Three ##\n\nFour\n----\n\n###### Six\n####### no\n#no\n";
        assert_eq!(
            headings(text),
            vec![(0, 1, "One".into()), (4, 1, "Two".into()), (7, 2, "Three".into()), (9, 2, "Four".into()), (12, 6, "Six".into())]
        );
    }

    #[test]
    fn skips_code_front_matter_and_rules() {
        let text = "---\ntitle: x\n---\n# Real\n```sh\n# comment\n```\n    # indented\n~~~\n# also code\n~~~\ntext\n\n---\n- item\n---\n## After\n";
        assert_eq!(headings(text), vec![(3, 1, "Real".into()), (16, 2, "After".into())]);
    }

    #[test]
    fn cleans_marks_and_links() {
        assert_eq!(clean("The `code` and **bold** [link](http://x.y) end"), "The code and bold link end");
        assert_eq!(clean(&"a".repeat(100)).chars().count(), TITLE_MAX);
    }

    #[test]
    fn the_scan_copies_no_line() {
        // 20k lines: a heading every 200th, the rest list items (no paragraph is held).
        let text: String = (0..20_000).map(|i| if i % 200 == 0 { format!("## Section {i}\n") } else { format!("- item {i}\n") }).collect();
        let rope = Rope::from(text.as_str());
        let (found, allocations) = crate::test_alloc::allocations(|| scan(&rope, 1, &AtomicU64::new(1)).unwrap());
        eprintln!("heading scan allocations over 20000 lines: {allocations}");
        assert_eq!(found.len(), 100);
        assert!(allocations < 1_000, "{allocations} allocations");
    }
}
