//! Change bars: thin marks beside the line numbers for what differs from the last commit
//! (finch#d1c9), and the card a click on one opens, with the old lines.
//!
//! One `ChangeBars` view per pane, drawn as a cached sibling over the editor like `caret_layer.rs`.
//! It keeps the HEAD text of the file the pane shows, and only that one (`git cat-file`, read when
//! the tab opens or changes and when the explorer reports a new git answer; untracked, ignored and
//! renamed files have no HEAD blob, so no bars; nothing over 4 MB). A line diff of the buffer
//! against it (`compare::line_changes`) runs on the background executor about 400 ms after the
//! last edit, and its answer is dropped when the text moved on meanwhile (`text_revision`). The
//! view watches the editor's notifications and compares revisions, so a reload from disk, which
//! emits no change event, schedules the diff too. A diff that runs out of time shows no bars,
//! never "all changed". Bars of a stale answer don't flicker while typing: they keep their marks,
//! and an edit that adds or removes lines moves the marks below it by that many rows at once
//! (`shift_hunks`, the edit's row read off the caret), so they stay beside their lines until the
//! settled answer replaces them. The two editors left last keep their finished answer
//! (`Recent`), so a tab switched back to at the revision it was left at shows its bars on the first
//! frame with no diff. HEAD is read again all the same (the explorer's git answer reaches only files
//! in its folder), and a read that finds the same text keeps them; a new git answer (`refresh_head`)
//! drops them.
//!
//! The editor records its layout while it paints; the bars read it from `gutter_rows` in their
//! own paint, which comes after the editor's in the same frame, for the rows shown only (wrap and
//! folds are the editor's: a hidden line has no row). Nothing here notifies from inside the
//! editor and nothing asks for a frame: the view repaints when the editor does.

use std::{
    ffi::OsString,
    io::Read as _,
    ops::Range,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{EditorState, Rope, RopeExt as _},
};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    compare::{self, Change},
    hover::FadeHover as _,
    settings, theme, tooltip,
    tokens::{
        ALPHA_BUTTON_HOVER, ALPHA_BUTTON_PRESSED, ALPHA_HAIRLINE, ALPHA_HOVER, ALPHA_PRIMARY, ALPHA_SECONDARY, FILTER_PAD_X, FIND_FIELD_HEIGHT, LAYER_TOOLTIP,
        RADIUS_BUTTON, RADIUS_TOOLTIP, TEXT_SUBHEADLINE, TEXT_SUBHEADLINE_LINE, TOOLTIP_PAD_X, TOOLTIP_PAD_Y,
    },
};

/// How long after the last edit the diff starts.
const SETTLE: Duration = Duration::from_millis(400);

/// A file (HEAD blob or buffer) over this size gets no bars.
const LIMIT: usize = 4 * 1024 * 1024;

/// Moves the settled diff's deadline to `due`; true when no task was waiting, so the caller starts one.
fn arm(waiting: &mut Option<Instant>, due: Instant) -> bool {
    waiting.replace(due).is_none()
}

/// Moves the marks of a stale answer for an edit at row `at` that added (`delta > 0`) or removed
/// lines: the rows after the edit move by `delta`, a hunk holding row `at` grows with an insert, and
/// a delete takes the rows `at + 1..=at + |delta|` (the line ends it ate) out of every hunk, which
/// drops a hunk left with no rows. Sorted and apart in, sorted and apart out.
fn shift_hunks(hunks: &[Hunk], at: usize, delta: isize) -> Vec<Hunk> {
    let moved = delta.unsigned_abs();
    hunks
        .iter()
        .filter_map(|hunk| {
            let (start, end) = (hunk.lines.start, hunk.lines.end);
            let lines = if delta >= 0 {
                let start = if start > at { start + moved } else { start };
                let end = if end > at { end + moved } else { end };
                start..end
            } else {
                let past = |row: usize| if row <= at { row } else if row <= at + moved + 1 { at + 1 } else { row - moved };
                past(start)..past(end)
            };
            if hunk.kind != Kind::Removed && lines.is_empty() {
                return None;
            }
            Some(Hunk { lines, ..hunk.clone() })
        })
        .collect()
}

/// The bar's inset from the gutter's left edge, and its width.
const INSET: f32 = 2.;
const WIDTH: f32 = 3.;
/// A removal's notch: how far it reaches right and how tall it is.
const NOTCH: f32 = 5.;
/// A click this far either side of a bar still hits it.
const REACH: f32 = 3.;
/// The card's width, and how many old lines it shows before it says how many more.
const CARD_WIDTH: f32 = 360.;
const CARD_LINES: usize = 12;
/// The card's text size ceiling, as the peek card's.
const CARD_TEXT: f32 = 12.;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Lines the commit doesn't have.
    Added,
    /// Lines that replace others.
    Changed,
    /// Lines the commit has that the buffer lost.
    Removed,
}

/// One run that differs from HEAD, by buffer line.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    pub kind: Kind,
    /// The buffer's lines (0-based, end-exclusive). Empty for `Removed`, where `start` is the line
    /// the removal sits before (the buffer's line count when it was at the end).
    pub lines: Range<usize>,
    /// Where the old lines are in the HEAD text's lines.
    pub old: Range<usize>,
}

/// The HEAD text of a file, kept as the one string it was read as and where each line starts (CRLF
/// and LF alike, no final empty line), so the lines are slices of it and nothing is copied per line.
pub struct Head {
    text: String,
    /// `starts[i]` is the byte line `i` begins at; one more entry holds the text's end, so a line
    /// runs to the next start. Under `LIMIT`, so `u32`.
    starts: Vec<u32>,
}

impl Head {
    pub fn new(text: String) -> Self {
        let mut starts = Vec::with_capacity(text.bytes().filter(|&byte| byte == b'\n').count() + 2);
        starts.push(0);
        if !text.is_empty() {
            starts.extend(text.match_indices('\n').map(|(at, _)| at as u32 + 1));
            if !text.ends_with('\n') {
                starts.push(text.len() as u32);
            }
        }
        Self { text, starts }
    }

    /// How many lines the text has.
    pub fn len(&self) -> usize {
        self.starts.len() - 1
    }

    /// Line `i`, without its line ending.
    pub fn line(&self, i: usize) -> &str {
        let line = &self.text[self.starts[i] as usize..self.starts[i + 1] as usize];
        let line = line.strip_suffix('\n').unwrap_or(line);
        line.strip_suffix('\r').unwrap_or(line)
    }

    /// Every line, for the diff.
    fn lines(&self) -> Vec<&str> {
        (0..self.len()).map(|i| self.line(i)).collect()
    }

    /// The lines in `range`, joined with newlines.
    pub fn joined(&self, range: Range<usize>) -> String {
        let mut out = String::new();
        for i in range.clone() {
            if i > range.start {
                out.push('\n');
            }
            out.push_str(self.line(i));
        }
        out
    }
}

/// What a HEAD read means for the bars.
#[derive(Debug, PartialEq, Eq)]
enum Adopted {
    /// There is no HEAD text (the file is untracked, ignored, too big...).
    Missing,
    /// The text equals the one held (git touched the index, nothing committed): nothing to redo.
    Same,
    /// New text, now held: the buffer needs a diff against it.
    New,
}

/// What the last finished diff said, with the lines it was taken against.
struct Shown {
    /// The buffer's `text_revision` it describes.
    revision: u64,
    head: Arc<Head>,
    hunks: Vec<Hunk>,
}

/// How many left editors keep their answer (finch#pzap): the tab you came from, and the one before.
const RECENT: usize = 2;

/// The finished answers of the last `RECENT` editors a pane showed, by editor and path, so a tab
/// switched back to at the revision it was left at shows its bars at once. Bounded at two: each
/// holds a HEAD text (under `LIMIT`) and its marks, and a closed tab's entry goes at the next keep.
struct Recent<T>(Vec<(EntityId, Option<PathBuf>, T)>);

impl<T> Default for Recent<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<T> Recent<T> {
    /// Keeps `value` for the editor `id` showing `path`, dropping its older entry, entries `alive` refuses, and the oldest past the bound.
    fn keep(&mut self, id: EntityId, path: Option<PathBuf>, value: T, alive: impl Fn(&T) -> bool) {
        self.0.retain(|(kept, _, held)| *kept != id && alive(held));
        self.0.push((id, path, value));
        if self.0.len() > RECENT {
            self.0.remove(0);
        }
    }

    /// Takes the entry of `id` at `path`, if `fits` accepts it (the revision still matches).
    fn take(&mut self, id: EntityId, path: Option<&Path>, fits: impl FnOnce(&T) -> bool) -> Option<T> {
        let at = self.0.iter().position(|(kept, kept_path, _)| *kept == id && kept_path.as_deref() == path)?;
        let (.., value) = self.0.remove(at);
        fits(&value).then_some(value)
    }
}

/// An open card: which hunk, where it floats, and what to close it on.
struct Card {
    hunk: usize,
    at: Point<Pixels>,
    scroll: Point<Pixels>,
    revision: u64,
}

pub struct ChangeBars {
    /// Weak: the bars don't keep a closed tab's editor alive.
    editor: Option<WeakEntity<EditorState>>,
    path: Option<PathBuf>,
    head: Option<Arc<Head>>,
    shown: Option<Rc<Shown>>,
    /// The text revision last seen, to tell an edit or a reload from a scroll.
    seen: u64,
    /// The buffer's line count then, to tell what an edit added or removed.
    lines: usize,
    card: Option<Card>,
    reading: Option<Task<()>>,
    diffing: Option<Task<()>>,
    /// When the settled diff starts; `Some` while a task waits for it.
    diff_due: Option<Instant>,
    /// The finished answers of the editors left last, to show again on a switch back.
    recent: Recent<(WeakEntity<EditorState>, Rc<Shown>)>,
    _watch: Option<Subscription>,
}

impl ChangeBars {
    pub fn new() -> Self {
        Self { editor: None, path: None, head: None, shown: None, seen: 0, lines: 0, card: None, reading: None, diffing: None, diff_due: None, recent: Recent::default(), _watch: None }
    }

    /// Follows `editor`, which shows the file at `path` (`None` for a page with no file): reads the
    /// HEAD text when either changed. The workspace calls it on each sync; the same pair is no work.
    pub fn set(&mut self, editor: &Entity<EditorState>, path: Option<&Path>, cx: &mut Context<Self>) {
        let same_editor = self.editor.as_ref().is_some_and(|kept| kept.entity_id() == editor.entity_id());
        if same_editor && self.path.as_deref() == path {
            return;
        }
        // The editor left keeps its finished answer: no read or diff on the way, the marks are of the HEAD held, and they describe the text as it is now.
        if !same_editor && self.reading.is_none() {
            if let (Some(left), Some(shown), Some(head)) = (self.editor.as_ref().and_then(WeakEntity::upgrade), self.shown.clone(), self.head.as_ref()) {
                if shown.revision == left.read(cx).text_revision() && Arc::ptr_eq(&shown.head, head) {
                    self.recent.keep(left.entity_id(), self.path.clone(), (left.downgrade(), shown), |(editor, _)| editor.upgrade().is_some());
                }
            }
        }
        self.reset(cx);
        self.editor = Some(editor.downgrade());
        self.path = path.map(Path::to_path_buf);
        self.seen = editor.read(cx).text_revision();
        self.lines = editor.read(cx).text().lines_len();
        self._watch = Some(cx.observe(editor, |this, editor, cx| this.editor_moved(&editor, cx)));
        // Back at the revision it was left at: its bars are drawn from the first frame. HEAD is read again all the same (the explorer's answer only reaches files in its folder); a read that finds the same text is `Adopted::Same`, which keeps the bars and runs no diff.
        let seen = self.seen;
        if let Some((_, shown)) = self.recent.take(editor.entity_id(), path, |(_, shown)| shown.revision == seen) {
            self.head = Some(shown.head.clone());
            self.shown = Some(shown);
            cx.notify();
        }
        self.read_head(cx);
    }

    /// Lets go of the editor and everything taken from it, when the pane has no tab.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.recent = Recent::default();
        self.reset(cx);
    }

    fn reset(&mut self, cx: &mut Context<Self>) {
        let had = self.shown.is_some() || self.card.is_some();
        (self.editor, self.path, self.head, self.shown, self.card) = (None, None, None, None, None);
        (self.reading, self.diffing, self.diff_due, self._watch) = (None, None, None, None);
        if had {
            cx.notify();
        }
    }

    /// Reads HEAD again: git's answer changed (a commit, a checkout, a stage). The kept answers
    /// were taken against the old HEAD, so they go; their tabs read it when switched to.
    pub fn refresh_head(&mut self, cx: &mut Context<Self>) {
        self.recent = Recent::default();
        if self.path.is_some() {
            self.read_head(cx);
        }
    }

    /// The editor notified: a text change (typed or from disk) starts the settle clock again, a
    /// scroll closes the card (it floats at a window position), and the bars repaint with it.
    fn editor_moved(&mut self, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        let state = editor.read(cx);
        let (revision, scroll) = (state.text_revision(), state.scroll_offset());
        if self.card.as_ref().is_some_and(|card| card.scroll != scroll || card.revision != revision) {
            self.card = None;
        }
        if revision != self.seen {
            self.seen = revision;
            let lines = state.text().lines_len();
            let delta = lines as isize - std::mem::replace(&mut self.lines, lines) as isize;
            if delta != 0 {
                // The caret ends an insert on its last new row and a delete on the row it joined into.
                let at = (state.cursor_position().line as usize).saturating_sub(delta.max(0) as usize);
                if let Some(shown) = self.shown.take() {
                    let hunks = shift_hunks(&shown.hunks, at, delta);
                    self.shown = Some(Rc::new(Shown { revision: shown.revision, head: shown.head.clone(), hunks }));
                }
            }
            self.diff(SETTLE, cx);
        }
        cx.notify();
    }

    fn read_head(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        self.reading = Some(cx.spawn(async move |this, cx| {
            let head = cx.background_executor().spawn(async move { head_text(&path).map(Head::new) }).await;
            this.update(cx, |this, cx| {
                // Done: nothing is in flight, so a tab left from here has a settled answer to keep.
                this.reading = None;
                match this.adopt(head) {
                    Adopted::Missing => {
                        this.diffing = None;
                        this.diff_due = None;
                        if this.shown.take().is_some() {
                            this.card = None;
                            cx.notify();
                        }
                    }
                    Adopted::Same => {}
                    Adopted::New => this.diff(Duration::ZERO, cx),
                }
            })
            .ok();
        }));
    }

    /// Holds `read` as the HEAD text unless it equals the one held, which stays (so the shown
    /// answer, its card and any diff on the way still stand).
    fn adopt(&mut self, read: Option<Head>) -> Adopted {
        match read {
            None => {
                self.head = None;
                Adopted::Missing
            }
            Some(new) if self.head.as_ref().is_some_and(|held| held.text == new.text) => Adopted::Same,
            Some(new) => {
                self.head = Some(Arc::new(new));
                Adopted::New
            }
        }
    }

    /// Starts the diff after `wait`, replacing one that hasn't begun (or whose answer is no longer
    /// wanted): the work is on the background executor and its answer is kept only for the text
    /// revision it was taken from.
    fn diff(&mut self, wait: Duration, cx: &mut Context<Self>) {
        let (Some(editor), Some(head)) = (self.editor.clone(), self.head.clone()) else { return };
        // One task per burst of edits: each moves the deadline, and the task sleeps on to it.
        if wait.is_zero() {
            self.diff_due = None;
        } else if !arm(&mut self.diff_due, Instant::now() + wait) {
            return;
        }
        self.diffing = Some(cx.spawn(async move |this, cx| {
            while !wait.is_zero() {
                let Ok(Some(due)) = this.update(cx, |this, _| this.diff_due) else { return };
                let now = Instant::now();
                if due <= now {
                    this.update(cx, |this, _| this.diff_due = None).ok();
                    break;
                }
                cx.background_executor().timer(due - now).await;
            }
            let Some(editor) = editor.upgrade() else { return };
            let Ok((revision, rope)) = this.update(cx, |_, cx| {
                let state = editor.read(cx);
                (state.text_revision(), state.text().clone())
            }) else {
                return;
            };
            let against = head.clone();
            let hunks = cx.background_executor().spawn(async move { compute(&rope, &against) }).await;
            this.update(cx, |this, cx| {
                if editor.read(cx).text_revision() != revision {
                    return;
                }
                let hunks = hunks.unwrap_or_default();
                // The same marks again: nothing to draw, and an open card stays.
                if this.shown.as_ref().is_some_and(|shown| shown.hunks == hunks && Arc::ptr_eq(&shown.head, &head)) {
                    this.shown = Some(Rc::new(Shown { revision, head, hunks }));
                    return;
                }
                this.shown = Some(Rc::new(Shown { revision, head, hunks }));
                this.card = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// The line of the next (or previous) change after the one at `line` (0-based), if any.
    pub fn step(&self, line: usize, forward: bool) -> Option<usize> {
        let shown = self.shown.as_ref()?;
        let mut starts = shown.hunks.iter().map(|hunk| hunk.lines.start);
        if forward { starts.find(|&start| start > line) } else { starts.rev().find(|&start| start < line) }
    }

    fn open_card(&mut self, hunk: usize, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.as_ref().and_then(WeakEntity::upgrade) else { return };
        let state = editor.read(cx);
        if self.shown.as_ref().is_none_or(|shown| shown.revision != state.text_revision()) {
            return;
        }
        let card = Card { hunk, at, scroll: state.scroll_offset(), revision: state.text_revision() };
        // A click on the bar of the card shown puts it away.
        self.card = if self.card.as_ref().is_some_and(|open| open.hunk == hunk) { None } else { Some(card) };
        cx.notify();
    }

    fn close_card(&mut self, cx: &mut Context<Self>) {
        if self.card.take().is_some() {
            cx.notify();
        }
    }

    /// The hunk the open card is about, with the answer it belongs to.
    fn open_hunk(&self) -> Option<(Rc<Shown>, usize)> {
        let (shown, card) = (self.shown.clone()?, self.card.as_ref()?);
        (shown.revision == card.revision && card.hunk < shown.hunks.len()).then_some((shown, card.hunk))
    }

    /// Puts the old lines back in place of the hunk: one edit, so one Undo.
    fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some((shown, ix)), Some(editor)) = (self.open_hunk(), self.editor.as_ref().and_then(WeakEntity::upgrade)) else { return };
        self.card = None;
        let hunk = &shown.hunks[ix];
        editor.update(cx, |state, cx| {
            let rope = state.text().clone();
            let count = rope_lines(&rope);
            let (range, text, caret) = revert_edit(hunk, &shown.head, count, rope.len(), ends_with_newline(&rope), |row| rope.line_start_offset(row), |at| at >= 2 && rope.char_at(at - 2) == Some('\r'));
            state.replace_range(range, &text, caret, window, cx);
        });
        cx.notify();
    }

    fn copy_old(&mut self, cx: &mut Context<Self>) {
        if let Some((shown, ix)) = self.open_hunk() {
            let hunk = &shown.hunks[ix];
            cx.write_to_clipboard(ClipboardItem::new_string(shown.head.joined(hunk.old.clone())));
        }
        self.close_card(cx);
    }

    fn card_view(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let (shown, ix) = self.open_hunk()?;
        let (hunk, at) = (&shown.hunks[ix], self.card.as_ref()?.at);
        let old = hunk.old.clone();
        let theme = cx.theme();
        let ink = theme.sidebar_foreground;
        let light = theme::is_light(cx);
        let (fill, border) = tooltip::float_fill(theme.background, ink, light);
        let editor = &settings::get(cx).editor;
        let family = editor.drawn_family(cx).unwrap_or_else(|| theme.mono_font_family.clone());
        let size = editor.font_size.map(px).unwrap_or(theme.mono_font_size).min(px(CARD_TEXT));
        let (words, text, rule) = (theme::ink(ALPHA_PRIMARY, cx), theme::ink(ALPHA_SECONDARY, cx), Hsla { a: ALPHA_HAIRLINE, ..ink });
        let title = match (hunk.kind, old.len()) {
            (Kind::Added, _) => "New lines".to_string(),
            (_, 1) => "1 line before".to_string(),
            (_, n) => format!("{n} lines before"),
        };
        let mut lines: Vec<SharedString> = old.clone().take(CARD_LINES).map(|i| SharedString::from(shown.head.line(i).to_string())).collect();
        if old.len() > CARD_LINES {
            lines.push(format!("\u{2026} {} more", old.len() - CARD_LINES).into());
        }
        let button = |id: &'static str, label: &'static str, run: fn(&mut Self, &mut Window, &mut Context<Self>), cx: &mut Context<Self>| {
            let wash = theme::ink(1., cx);
            let rest = wash.opacity(ALPHA_HOVER);
            div()
                .flex_none()
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
                .on_click(cx.listener(move |this, _, window, cx| run(this, window, cx)))
        };
        let body = tooltip::float(RADIUS_TOOLTIP, fill, border, light, false)
            .id("change-card")
            .occlude()
            .w(px(CARD_WIDTH))
            .flex()
            .flex_col()
            .gap(px(TOOLTIP_PAD_Y))
            .px(px(TOOLTIP_PAD_X))
            .py(px(TOOLTIP_PAD_Y))
            .font_family(theme.font_family.clone())
            .text_size(TEXT_SUBHEADLINE)
            .line_height(TEXT_SUBHEADLINE_LINE)
            .text_color(words)
            .on_mouse_down_out(cx.listener(|this, _, _, cx| this.close_card(cx)))
            .child(div().min_w_0().child(title))
            .when(!lines.is_empty(), |card| {
                card.child(div().h(px(1.)).bg(rule)).child(crate::snippet::snippet(&lines, usize::MAX, family, size, text, Hsla { a: 0., ..ink }))
            })
            .child(
                div()
                    .flex()
                    .gap(px(TOOLTIP_PAD_Y))
                    .child(button("change-revert", "Revert", |this, window, cx| this.revert(window, cx), cx))
                    .when(!old.is_empty(), |row| row.child(button("change-copy", "Copy old text", |this, _, cx| this.copy_old(cx), cx))),
            );
        Some(deferred(anchored().position(at).snap_to_window_with_margin(px(8.)).child(body)).with_priority(LAYER_TOOLTIP).into_any_element())
    }
}

impl Render for ChangeBars {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let card = self.card_view(cx);
        let (editor, shown) = (self.editor.clone(), self.shown.clone());
        let (bars, inks) = (cx.entity().downgrade(), theme::git_inks(cx));
        div().size_full().child(canvas(|bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal), move |_, hitbox, window, cx| {
            if crate::control::rects_enabled() {
                crate::control::forget_prefix(window.window_handle().window_id(), "change-bar-");
            }
            let (Some(editor), Some(shown)) = (editor.and_then(|editor| editor.upgrade()), shown) else { return };
            let hits = paint(&editor, &shown, inks, window, cx);
            if crate::control::rects_enabled() {
                for (rect, ix) in &hits {
                    crate::control::record(window, format!("change-bar-{ix}"), *rect);
                }
            }
            if hits.is_empty() {
                return;
            }
            // Ahead of the editor's own handlers, which would take the click for the gutter's.
            window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture || event.button != MouseButton::Left || event.modifiers.modified() || !hitbox.is_hovered(window) {
                    return;
                }
                let Some(&(rect, hunk)) = hits.iter().find(|(rect, _)| rect.contains(&event.position)) else { return };
                let at = point(rect.right() + px(8.), rect.bottom() + px(2.));
                bars.update(cx, |this, cx| this.open_card(hunk, at, cx)).ok();
                cx.stop_propagation();
            });
        }).absolute().size_full()).children(card)
    }
}

/// Paints the bars of `shown` for the rows the editor shows; returns where to click for each hunk.
fn paint(editor: &Entity<EditorState>, shown: &Shown, inks: theme::GitInks, window: &mut Window, cx: &mut App) -> Vec<(Bounds<Pixels>, usize)> {
    let state = editor.read(cx);
    let mut rows: Vec<(usize, Pixels, Pixels)> = Vec::with_capacity(96);
    let Some((left, _, _, clip)) = state.gutter_rows(|row, top, height| rows.push((row, top, height))) else { return Vec::new() };
    if rows.is_empty() || shown.hunks.is_empty() {
        return Vec::new();
    }
    let line_count = rope_lines(state.text());
    let (first, last) = (rows[0].0, rows[rows.len() - 1].0);
    let x = left + px(INSET);
    let mut hits = Vec::new();
    // The hunks that touch the rows shown: sorted and apart, so the first is a search away.
    let from = shown.hunks.partition_point(|hunk| hunk.lines.end.max(hunk.lines.start + 1) <= first);
    window.with_content_mask(Some(ContentMask { bounds: clip }), |window| {
    for (ix, hunk) in shown.hunks.iter().enumerate().skip(from) {
        if hunk.lines.start > last + 1 {
            break;
        }
        match hunk.kind {
            Kind::Added | Kind::Changed => {
                let a = rows.partition_point(|row| row.0 < hunk.lines.start);
                let b = rows.partition_point(|row| row.0 < hunk.lines.end);
                if a >= b {
                    continue;
                }
                let (top, bottom) = (rows[a].1, rows[b - 1].1 + rows[b - 1].2);
                let ink = if hunk.kind == Kind::Added { inks.added } else { inks.modified };
                let bar = Bounds::new(point(x, top + px(1.)), size(px(WIDTH), (bottom - top - px(2.)).max(px(2.))));
                window.paint_quad(fill(bar, ink).corner_radii(Corners::all(px(WIDTH / 2.))));
                hits.push((Bounds::new(point(x - px(REACH), top), size(px(WIDTH + 2. * REACH), bottom - top)), ix));
            }
            Kind::Removed => {
                let at = hunk.lines.start;
                let y = match rows.binary_search_by_key(&at, |row| row.0) {
                    Ok(i) => rows[i].1,
                    // Past the last line: the notch sits under it.
                    Err(i) if i > 0 && at >= line_count && rows[i - 1].0 + 1 == at => rows[i - 1].1 + rows[i - 1].2,
                    Err(_) => continue,
                };
                let mut path = PathBuilder::fill();
                path.move_to(point(x - px(1.), y - px(NOTCH / 2.)));
                path.line_to(point(x + px(NOTCH), y));
                path.line_to(point(x - px(1.), y + px(NOTCH / 2.)));
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, inks.deleted);
                }
                hits.push((Bounds::new(point(x - px(REACH), y - px(NOTCH)), size(px(NOTCH + REACH * 2.), px(NOTCH * 2.))), ix));
            }
        }
    }
    });
    hits.into_iter()
        .filter_map(|(rect, ix)| {
            let r = rect.intersect(&clip);
            (r.size.width > px(0.) && r.size.height > px(0.)).then_some((r, ix))
        })
        .collect()
}

/// The hunks of the buffer against HEAD, `None` when it is too big or the diff ran out of time.
fn compute(rope: &Rope, head: &Head) -> Option<Vec<Hunk>> {
    if rope.len() > LIMIT {
        return None;
    }
    let text = rope.to_string();
    let a = compare::line_slices(&text);
    hunks(&a, &head.lines())
}

/// The runs of `buffer`'s lines that differ from `head`'s, `None` on the diff's deadline.
pub fn hunks(buffer: &[&str], head: &[&str]) -> Option<Vec<Hunk>> {
    compare::line_changes(buffer, head).map(|changes| changes.into_iter().map(hunk).collect())
}

fn hunk(change: Change) -> Hunk {
    let kind = if change.yours.is_empty() {
        Kind::Removed
    } else if change.disk.is_empty() {
        Kind::Added
    } else {
        Kind::Changed
    };
    Hunk { kind, lines: change.yours, old: change.disk }
}

/// How many lines the diff counts in `rope`: a final newline starts no extra line.
fn rope_lines(rope: &Rope) -> usize {
    let lines = rope.lines_len();
    if ends_with_newline(rope) { lines.saturating_sub(1) } else { lines }
}

fn ends_with_newline(rope: &Rope) -> bool {
    rope.len() > 0 && rope.char_at(rope.len() - 1) == Some('\n')
}

/// The edit that puts `head`'s old lines of `hunk` back: the bytes to replace, what to put there, and
/// where the caret goes. `count` is the buffer's line count as the diff counts it, `len` its bytes,
/// `final_newline` whether it ends in a newline, `start_of` the byte where a line starts and
/// `cr_before` whether a carriage return sits just before the newline that ends at that byte.
pub fn revert_edit(hunk: &Hunk, head: &Head, count: usize, len: usize, final_newline: bool, start_of: impl Fn(usize) -> usize, cr_before: impl Fn(usize) -> bool) -> (Range<usize>, String, usize) {
    let at = |row: usize| if row >= count { len } else { start_of(row) };
    let old = head.joined(hunk.old.clone());
    let (mut start, end) = (at(hunk.lines.start), at(hunk.lines.end));
    // The last line of a buffer with no final newline has no newline to take along.
    let bare_end = end == len && !final_newline;
    match hunk.kind {
        Kind::Removed if bare_end && len > 0 => (start..start, format!("\n{old}"), start + 1),
        Kind::Removed => (start..start, format!("{old}\n"), start),
        Kind::Added => {
            if bare_end && start > 0 {
                start -= if cr_before(start) { 2 } else { 1 };
            }
            (start..end, String::new(), start)
        }
        Kind::Changed => (start..end, if bare_end { old } else { format!("{old}\n") }, start),
    }
}

/// HEAD's text of the file at `path`, `None` when there is none (not in a repo, untracked, ignored,
/// renamed and not committed), it is over `LIMIT`, binary or not UTF-8. One `git cat-file`, never
/// taking the index lock; it stops reading at the limit.
fn head_text(path: &Path) -> Option<String> {
    let path = path.canonicalize().ok()?;
    let (dir, name) = (path.parent()?, path.file_name()?);
    let mut spec = OsString::from("HEAD:./");
    spec.push(name);
    let mut child = Command::new("git")
        .args(["cat-file", "blob"])
        .arg(spec)
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()
        .ok()?;
    let mut bytes = Vec::new();
    let read = child.stdout.take()?.take(LIMIT as u64 + 1).read_to_end(&mut bytes);
    if bytes.len() > LIMIT {
        let _ = child.kill();
    }
    let done = child.wait().ok()?;
    if read.is_err() || bytes.len() > LIMIT || !done.success() || bytes.iter().take(8000).any(|&byte| byte == 0) {
        return None;
    }
    let text = String::from_utf8(bytes).ok()?;
    Some(match text.strip_prefix('\u{feff}') {
        Some(rest) => rest.to_string(),
        None => text,
    })
}

#[cfg(test)]
mod tests {
    // Named, not `super::*`: that glob carries GPUI's own `test` attribute over the standard one.
    use super::{Adopted, ChangeBars, Head, Hunk, Kind, SETTLE, Shown, arm, compare, compute, hunks, revert_edit, shift_hunks};
    use gpui_kit::component::input::Rope;
    use crate::test_alloc::allocations;
    use std::{rc::Rc, sync::Arc, time::{Duration, Instant}};

    fn hunk_at(kind: Kind, lines: std::ops::Range<usize>) -> Hunk {
        Hunk { kind, lines, old: 0..0 }
    }

    #[test]
    fn a_return_above_a_hunk_moves_it_down() {
        let shifted = shift_hunks(&[hunk_at(Kind::Changed, 10..12), hunk_at(Kind::Removed, 20..20)], 3, 1);
        assert_eq!(shifted, vec![hunk_at(Kind::Changed, 11..13), hunk_at(Kind::Removed, 21..21)]);
    }

    #[test]
    fn a_return_below_a_hunk_leaves_it() {
        let hunks = [hunk_at(Kind::Added, 2..4), hunk_at(Kind::Removed, 5..5)];
        assert_eq!(shift_hunks(&hunks, 5, 1), hunks.to_vec());
    }

    #[test]
    fn a_hunk_holding_the_caret_grows() {
        assert_eq!(shift_hunks(&[hunk_at(Kind::Added, 4..7)], 5, 1), vec![hunk_at(Kind::Added, 4..8)]);
        assert_eq!(shift_hunks(&[hunk_at(Kind::Added, 4..7)], 6, 2), vec![hunk_at(Kind::Added, 4..9)]);
        assert_eq!(shift_hunks(&[hunk_at(Kind::Added, 4..7)], 4, 1), vec![hunk_at(Kind::Added, 4..8)]);
    }

    #[test]
    fn a_delete_moves_the_rows_below_up_and_shrinks_what_it_ate() {
        // Backspace at the start of row 4 joins it into row 3: row 4 is gone.
        assert_eq!(shift_hunks(&[hunk_at(Kind::Changed, 10..12)], 3, -1), vec![hunk_at(Kind::Changed, 9..11)]);
        assert_eq!(shift_hunks(&[hunk_at(Kind::Added, 2..8)], 3, -1), vec![hunk_at(Kind::Added, 2..7)]);
        assert_eq!(shift_hunks(&[hunk_at(Kind::Added, 2..4)], 3, -1), vec![hunk_at(Kind::Added, 2..4)]);
        assert_eq!(shift_hunks(&[hunk_at(Kind::Added, 3..5)], 3, -1), vec![hunk_at(Kind::Added, 3..4)]);
        assert_eq!(shift_hunks(&[hunk_at(Kind::Removed, 9..9)], 3, -2), vec![hunk_at(Kind::Removed, 7..7)]);
    }

    #[test]
    fn a_delete_that_takes_all_a_hunks_rows_drops_it() {
        let hunks = [hunk_at(Kind::Added, 1..2), hunk_at(Kind::Changed, 4..6), hunk_at(Kind::Added, 9..10)];
        // Rows 4 and 5 are eaten by a delete at row 3.
        assert_eq!(shift_hunks(&hunks, 3, -3), vec![hunk_at(Kind::Added, 1..2), hunk_at(Kind::Added, 6..7)]);
        assert_eq!(shift_hunks(&[hunk_at(Kind::Added, 4..5)], 3, -1), Vec::new());
    }

    #[test]
    fn a_removal_between_eaten_rows_lands_after_the_join() {
        assert_eq!(shift_hunks(&[hunk_at(Kind::Removed, 5..5)], 3, -3), vec![hunk_at(Kind::Removed, 4..4)]);
    }

    #[test]
    fn the_shifted_marks_agree_with_the_settled_diff_after_a_plain_return() {
        let head = "a\nb\nc\nd\ne\nf\ng\n";
        let before = "a\nb\nc\nd\nE\nf\nnew\ng\n";
        // Return at the end of row 1.
        let after = "a\nb\n\nc\nd\nE\nf\nnew\ng\n";
        let shifted = shift_hunks(&diff(before, head), 1, 1);
        let settled = diff(after, head);
        for hunk in &shifted {
            assert!(settled.contains(hunk), "{hunk:?} in {settled:?}");
        }
        assert_eq!(settled.len(), shifted.len() + 1, "the blank line is the one new mark");
    }

    #[test]
    fn a_burst_of_edits_starts_one_settle_task() {
        let (start, mut waiting) = (Instant::now(), None);
        let tasks = (0..100).filter(|i| arm(&mut waiting, start + Duration::from_millis(*i) + SETTLE)).count();
        eprintln!("settle tasks over 100 revisions: {tasks}");
        assert_eq!(tasks, 1);
        assert_eq!(waiting, Some(start + Duration::from_millis(99) + SETTLE));
    }

    fn diff(buffer: &str, head: &str) -> Vec<Hunk> {
        hunks(&compare::line_slices(buffer), &compare::line_slices(head)).unwrap()
    }

    /// `buffer` with `hunk` reverted, the way the editor would apply the edit.
    fn reverted(buffer: &str, head: &str, hunk: &Hunk) -> String {
        let head = Head::new(head.to_string());
        let starts: Vec<usize> = std::iter::once(0).chain(buffer.match_indices('\n').map(|(at, _)| at + 1)).collect();
        let count = compare::line_slices(buffer).len();
        let (range, text, caret) = revert_edit(hunk, &head, count, buffer.len(), buffer.ends_with('\n'), |row| starts[row].min(buffer.len()), |at| at >= 2 && buffer.as_bytes()[at - 2] == b'\r');
        assert!(caret <= buffer.len() - range.len() + text.len());
        format!("{}{}{}", &buffer[..range.start], text, &buffer[range.end..])
    }

    #[test]
    fn marks_added_changed_and_removed_lines() {
        let head = "a\nb\nc\nd\ne\nf\n";
        let buffer = "a\nB\nc\nnew\nd\nf\n";
        let found = diff(buffer, head);
        assert_eq!(
            found,
            vec![
                Hunk { kind: Kind::Changed, lines: 1..2, old: 1..2 },
                Hunk { kind: Kind::Added, lines: 3..4, old: 3..3 },
                Hunk { kind: Kind::Removed, lines: 5..5, old: 4..5 },
            ]
        );
    }

    #[test]
    fn a_clean_buffer_has_no_hunks_and_a_missing_final_newline_is_no_change() {
        assert!(diff("a\nb\n", "a\nb\n").is_empty());
        assert!(diff("a\nb", "a\nb\n").is_empty());
        assert!(diff("a\r\nb\r\n", "a\nb\n").is_empty());
    }

    #[test]
    fn a_removal_at_the_end_sits_past_the_last_line() {
        assert_eq!(diff("a\n", "a\nb\nc\n"), vec![Hunk { kind: Kind::Removed, lines: 1..1, old: 1..3 }]);
    }

    #[test]
    fn a_whole_new_buffer_is_one_added_hunk() {
        assert_eq!(diff("a\nb\n", ""), vec![Hunk { kind: Kind::Added, lines: 0..2, old: 0..0 }]);
    }

    #[test]
    fn reverting_each_kind_gives_the_commit_back() {
        for (buffer, head) in [
            ("a\nB\nc\n", "a\nb\nc\n"),
            ("a\nx\ny\nc\n", "a\nb\nc\n"),
            ("a\nnew\nb\n", "a\nb\n"),
            ("a\nb\nnew\n", "a\nb\n"),
            ("a\nb\nnew", "a\nb\n"),
            ("a\nc\n", "a\nb\nc\n"),
            ("a\n", "a\nb\nc\n"),
            ("a", "a\nb\nc\n"),
            ("a\nB", "a\nb"),
            ("", "a\nb\n"),
            ("one\n", ""),
        ] {
            let found = diff(buffer, head);
            assert_eq!(found.len(), 1, "{buffer:?} against {head:?}");
            let back = reverted(buffer, head, &found[0]);
            assert_eq!(compare::line_slices(&back), compare::line_slices(head), "{buffer:?} against {head:?} gave {back:?}");
        }
    }

    #[test]
    fn reverting_an_added_last_line_in_a_crlf_file_leaves_no_carriage_return() {
        let (buffer, head) = ("a\r\nb", "a\r\n");
        let found = diff(buffer, head);
        assert_eq!(found.len(), 1);
        assert_eq!(reverted(buffer, head, &found[0]), "a");
    }

    #[test]
    fn reverting_one_of_several_leaves_the_others() {
        let head = "a\nb\nc\nd\n";
        let buffer = "A\nb\nC\nd\n";
        let found = diff(buffer, head);
        assert_eq!(found.len(), 2);
        assert_eq!(reverted(buffer, head, &found[1]), "A\nb\nc\nd\n");
        assert_eq!(reverted(buffer, head, &found[0]), "a\nb\nC\nd\n");
    }

    #[test]
    fn steps_between_changes() {
        let shown = Shown { revision: 0, head: Arc::new(Head::new(String::new())), hunks: diff("A\nb\nC\nd\nE\n", "a\nb\nc\nd\ne\n") };
        let mut bars = ChangeBars::new();
        bars.shown = Some(Rc::new(shown));
        assert_eq!(bars.step(0, true), Some(2));
        assert_eq!(bars.step(2, true), Some(4));
        assert_eq!(bars.step(4, true), None);
        assert_eq!(bars.step(4, false), Some(2));
        assert_eq!(bars.step(0, false), None);
    }

    #[test]
    fn head_lines_are_the_lines_the_diff_counts() {
        for text in ["", "\n", "a", "a\n", "a\n\n", "a\r\nb\r\n", "a\r\nb", "\r\n", "x\n\ny", "caf\u{e9}\nz\n"] {
            let head = Head::new(text.to_string());
            assert_eq!(head.lines(), compare::line_slices(text), "{text:?}");
            assert_eq!(head.len(), compare::line_slices(text).len(), "{text:?}");
        }
        assert_eq!(Head::new("a\r\nb\nc\n".to_string()).joined(0..3), "a\nb\nc");
        assert_eq!(Head::new("a\nb\nc\n".to_string()).joined(1..1), "");
    }

    #[test]
    fn compute_borrows_its_lines_from_one_string() {
        let head_text: String = (0..100_000).map(|i| format!("line {i}\n")).collect();
        let buffer = head_text.replacen("line 500\n", "line five hundred\n", 1).replacen("line 90000\n", "", 1);
        let (rope, head) = (Rope::from(buffer.as_str()), Head::new(head_text.clone()));
        // What `compute` did before: a String per line, a flag per line and two slice vectors.
        let old_heads: Vec<String> = head_text.strip_suffix('\n').unwrap().split('\n').map(str::to_string).collect();
        let (old, before) = allocations(|| {
            let text = rope.to_string();
            let lines: Vec<String> = text.strip_suffix('\n').unwrap().split('\n').map(str::to_string).collect();
            let crs: Vec<bool> = lines.iter().map(|line| line.ends_with('\r')).collect();
            let a: Vec<&str> = lines.iter().map(String::as_str).collect();
            let b: Vec<&str> = old_heads.iter().map(String::as_str).collect();
            drop(crs);
            hunks(&a, &b)
        });
        let (new, after) = allocations(|| compute(&rope, &head));
        println!("compute on 100000 lines: {before} allocations before, {after} after");
        assert_eq!(new, old);
        assert_eq!(new.as_ref().map(Vec::len), Some(2));
        assert!(before > 100_000, "{before}");
        assert!(after < 100, "{after}");
    }

    #[test]
    fn the_same_head_read_again_keeps_the_held_one() {
        let mut bars = ChangeBars::new();
        assert_eq!(bars.adopt(Some(Head::new("a\nb\n".to_string()))), Adopted::New);
        let held = bars.head.clone().unwrap();
        assert_eq!(bars.adopt(Some(Head::new("a\nb\n".to_string()))), Adopted::Same);
        assert!(Arc::ptr_eq(&held, bars.head.as_ref().unwrap()));
        assert_eq!(bars.adopt(Some(Head::new("a\nc\n".to_string()))), Adopted::New);
        assert!(!Arc::ptr_eq(&held, bars.head.as_ref().unwrap()));
        assert_eq!(bars.adopt(None), Adopted::Missing);
        assert!(bars.head.is_none());
    }

    /// Set A, B, A at one revision each: the way back runs no diff (the bars come from the kept answer, and the HEAD read it still makes finds the same text), and a changed revision or path does.
    #[test]
    fn switching_back_diffs_nothing() {
        use super::Recent;
        use gpui_kit::EntityId;
        use std::path::Path;
        let (a, b) = (EntityId::from(1), EntityId::from(2));
        let (path_a, path_b) = (Path::new("/r/a.md"), Path::new("/r/b.md"));
        let shown = |revision| Rc::new(Shown { revision, head: Arc::new(Head::new("x\n".to_string())), hunks: vec![hunk_at(Kind::Added, 0..1)] });
        let mut recent: Recent<Rc<Shown>> = Recent::default();
        let (mut reads, mut diffs) = (0, 0);
        // A is shown first (revision 3): one read, one diff. A is left, B comes up (revision 8): another each.
        reads += 1;
        diffs += 1;
        recent.keep(a, Some(path_a.to_path_buf()), shown(3), |_| true);
        assert!(recent.take(b, Some(path_b), |kept| kept.revision == 8).is_none());
        reads += 1;
        diffs += 1;
        recent.keep(b, Some(path_b.to_path_buf()), shown(8), |_| true);
        // Back to A at revision 3: its answer returns, with its marks, and the way back still reads HEAD (one more read) but needs no diff.
        let back = recent.take(a, Some(path_a), |kept| kept.revision == 3);
        reads += 1;
        eprintln!("HEAD reads and diffs over set A, B, A: {reads} and {diffs}");
        assert_eq!(back.map(|kept| kept.hunks.len()), Some(1));
        assert_eq!((reads, diffs), (3, 2));
        // Edited while away (revision 4), or renamed: not used.
        recent.keep(a, Some(path_a.to_path_buf()), shown(3), |_| true);
        assert!(recent.take(a, Some(path_a), |kept| kept.revision == 4).is_none());
        recent.keep(a, Some(path_a.to_path_buf()), shown(3), |_| true);
        assert!(recent.take(a, Some(path_b), |kept| kept.revision == 3).is_none());
        // Never more than two.
        recent.keep(a, Some(path_a.to_path_buf()), shown(1), |_| true);
        recent.keep(b, Some(path_b.to_path_buf()), shown(1), |_| true);
        recent.keep(EntityId::from(3), None, shown(1), |_| true);
        assert_eq!(recent.0.len(), 2);
        assert!(recent.take(a, Some(path_a), |_| true).is_none());
    }
}
