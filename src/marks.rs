//! The shared driver for the marks that follow the text on screen: the focus dim, and the layers
//! that plug in after it (parts of speech, style marks, spell check, swatches, occurrence marks,
//! the word being read aloud).
//!
//! One `Driver` per editor, made where the editor is (`attach`, from `Document::editor`), kept in a
//! book keyed by the editor like `log_follow`'s. It watches the editor (`cx.observe`) and reduces
//! what a layer could care about to a small `Key`: the caret, the selection, how many carets,
//! `text_revision()` and the rows on screen widened by about a screen each way. Layers are woken
//! only when the key changes, and the rows part of the key moves only when the screen leaves the
//! widened window, so a scroll step costs one compare. The driver never notifies the editor itself
//! (a layer's `set` does, when its list differs), and nothing it does notifies it unless a layer
//! set a different list, so the observe can't loop. No layout yet (`visible_row_range` is `None`)
//! means nothing to do; a short retry picks the editor up once it has laid out, then stops.
//!
//! A layer owns its collection(s), implements `MarkLayer`, and costs nothing while it is off: when
//! no layer is `active` the observe returns before it reads the editor.
//!
//! The collections are created in the order of `layers`, and the first collection created wins a
//! property two of them set, so that list's order is the precedence.

use std::{
    any::Any,
    cell::{Cell, RefCell},
    collections::HashMap,
    future::Future,
    ops::Range,
    pin::Pin,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui_kit::component::input::{EditorState, HoverProvider, RangeDecoration, RangeDecorationCollection, Rope, RopeExt as _, TextDecoration, TextDecorationCollection};
use gpui_kit::*;

use crate::{focus, markdown_marks, parts_of_speech, spell, style_check, swatches};

/// What a layer's answer depends on; a layer is woken only when this changes.
#[derive(Clone, Debug, PartialEq)]
pub struct Key {
    /// The primary caret's byte offset (the other carets of a multi-cursor are not tracked).
    pub caret: usize,
    /// The active selection (empty at the caret).
    pub selection: Range<usize>,
    /// How many carets and selections the editor has.
    pub carets: usize,
    /// `EditorState::text_revision`.
    pub rev: u64,
    /// Buffer rows: the screen widened by about a screen each way.
    pub rows: Range<usize>,
}

/// What a layer is given when it is woken. Cheap to build: it borrows, the text is a rope handle.
/// `editor` and `bytes` are for the layers that follow the focus one.
#[allow(dead_code)]
pub struct View<'a> {
    pub editor: &'a Entity<EditorState>,
    pub key: &'a Key,
    /// The bytes of `key.rows`: the window a layer may scan and mark.
    pub bytes: Range<usize>,
    pub text: &'a Rope,
    /// The file's language (`language::for_text`), `"markdown"` and `"text"` being prose.
    pub lang: &'static str,
}

impl View<'_> {
    /// Prose: Markdown or plain text. Anything else is code.
    pub fn prose(&self) -> bool {
        matches!(self.lang, "markdown" | "text")
    }
}

/// One kind of mark. Implementors own their collections, made in `new` (called in `layers`' order).
pub trait MarkLayer: Any {
    /// The layer is on, or still holds marks it must clear; while none is, the driver does nothing.
    fn active(&self) -> bool;

    /// The key changed (or the layer was poked with `with_layer`): bring the marks up to date for
    /// `view`. Set only when the list differs (`TextSet`, `RangeSet`); never notify the editor.
    fn update(&mut self, view: &View, cx: &mut App);

    /// What pointing at byte `offset` says about a mark this layer put there ("On your list"), or
    /// `None`. The driver shows the first answer of the layers, in order.
    fn hover(&self, _offset: usize, _cx: &App) -> Option<SharedString> {
        None
    }

    /// The tab is hidden: let go of the scan results and caches that a return refills on its own.
    /// What is on screen (the held underlines) stays, so a return shows no flicker.
    fn release(&mut self) {}

    /// How many words (or marks) of scan results and caches the layer keeps, for `tab-state`.
    fn held(&self) -> usize {
        0
    }
}

/// The layers, one line each, in precedence order: the focus dim first, then parts of speech, style
/// marks and the spell underline (text styles), and the range fills after them (color swatches,
/// occurrence marks, the word being read aloud), which paint geometrically and don't touch text
/// runs. A layer's collections are created where its line runs, so the order of the lines is the
/// order of creation, and the first collection created wins a property that two of them set.
fn layers(editor: &Entity<EditorState>, cx: &mut App) -> Vec<Box<dyn MarkLayer>> {
    vec![
        Box::new(focus::Layer::new(editor, cx)),
        Box::new(parts_of_speech::Layer::new(editor, cx)),
        Box::new(style_check::Layer::new(editor, cx)),
        Box::new(markdown_marks::Layer::new(editor, cx)),
        Box::new(spell::Layer::new(editor, cx)),
        Box::new(swatches::Layer::new(editor, cx)),
        Box::new(crate::occurrences::Layer::new(editor, cx)),
        Box::new(crate::speech::Layer::new(editor, cx)),
    ]
}

/// A text-style collection that keeps the last list it was given and skips a `set` that wouldn't
/// change it (a `set` notifies the editor, and every notify re-lays out what it shows).
pub struct TextSet {
    collection: TextDecorationCollection,
    last: Vec<TextDecoration>,
}

impl TextSet {
    pub fn new(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        let collection = editor.update(cx, |state, cx| state.create_decorations_collection(Vec::new(), cx));
        Self { collection, last: Vec::new() }
    }

    /// The list on screen: what was last set, moved with the edits since (the editor keeps the
    /// ranges of a collection in step with the text). Empty ranges the editor dropped are
    /// reconciled by dropping them here too.
    pub fn held(&mut self, cx: &App) -> &[TextDecoration] {
        let ranges = self.collection.get_ranges(cx);
        if ranges.len() == self.last.len() {
            for (decoration, range) in self.last.iter_mut().zip(ranges) {
                decoration.range = range;
            }
        } else {
            self.last.clear();
            // Unknown: the next `set_if_changed` sets, whatever it is given.
            self.last.push(TextDecoration::new(usize::MAX..usize::MAX, Default::default()));
        }
        &self.last
    }

    /// The ranges on screen now, read without changing anything (for a hover).
    pub fn ranges(&self, cx: &App) -> Vec<Range<usize>> {
        self.collection.get_ranges(cx)
    }

    /// Sets `list` unless the collection already shows it; says whether it did.
    pub fn set_if_changed(&mut self, mut list: Vec<TextDecoration>, cx: &mut App) -> bool {
        list.retain(|decoration| !decoration.range.is_empty());
        if self.held(cx) == list.as_slice() {
            return false;
        }
        self.last = list.clone();
        self.collection.set(list, cx);
        true
    }
}

/// `TextSet` for geometric range fills (swatches, occurrence marks, the word read aloud).
#[allow(dead_code)]
pub struct RangeSet {
    collection: RangeDecorationCollection,
    last: Vec<RangeDecoration>,
}

#[allow(dead_code)]
impl RangeSet {
    pub fn new(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        let collection = editor.update(cx, |state, cx| state.create_range_decorations_collection(Vec::new(), cx));
        Self { collection, last: Vec::new() }
    }

    /// Sets `list` unless the collection already shows it; says whether it did.
    pub fn set_if_changed(&mut self, mut list: Vec<RangeDecoration>, cx: &mut App) -> bool {
        list.retain(|decoration| !decoration.range().is_empty());
        let held = self.collection.get_ranges(cx);
        let same = held.len() == list.len() && self.last.len() == list.len() && list.iter().zip(&held).zip(&self.last).all(|((new, range), old)| new.range() == range && new.style() == old.style() && new.color() == old.color());
        if same {
            return false;
        }
        self.last = list.clone();
        self.collection.set(list, cx);
        true
    }

    /// How many fills the set keeps a copy of, for `tab-state`.
    pub fn held(&self) -> usize {
        self.last.len()
    }

    /// Lets go of the copy; the fills on screen stay, and the next `set_if_changed` sets again unless
    /// both are empty.
    pub fn release(&mut self) {
        self.last = Vec::new();
    }
}

/// Background work for a layer, one job at a time: starting another drops the one running. The job
/// waits `wait`, then runs on the background executor over a clone of the text, and its answer is
/// handed to `done` only if the text is still the revision it was asked for. A call while the wait
/// is still going moves its end to `wait` from now and swaps in the newest job and inputs; the one
/// task already sleeping wakes at the new end and runs only that one, so a burst of keys costs one
/// task, not one per key.
#[derive(Default)]
#[allow(dead_code)]
pub struct Debounce {
    task: Option<Task<()>>,
    slot: Rc<RefCell<Slot<Go>>>,
}

/// A job ready to run: given the app handle, it checks the revision, runs and hands over the answer.
type Go = Box<dyn FnOnce(AsyncApp) -> Pin<Box<dyn Future<Output = ()>>>>;

/// The newest job of a `Debounce` and when its wait ends.
pub(crate) struct Slot<G> {
    deadline: Option<Instant>,
    go: Option<G>,
    /// A task is sleeping on this slot.
    waiting: bool,
}

impl<G> Default for Slot<G> {
    fn default() -> Self {
        Self { deadline: None, go: None, waiting: false }
    }
}

/// What the sleeping task does next.
pub(crate) enum Step<G> {
    Sleep(Duration),
    Run(Option<G>),
}

impl<G> Slot<G> {
    /// Takes the newest job and its deadline; true when no task is sleeping on the slot, so one has to be started.
    pub(crate) fn push(&mut self, deadline: Instant, go: G) -> bool {
        self.go = Some(go);
        self.deadline = Some(deadline);
        !std::mem::replace(&mut self.waiting, true)
    }

    /// The sleeping task woke at `now`: sleep on if the deadline moved, else hand over the job.
    pub(crate) fn next(&mut self, now: Instant) -> Step<G> {
        let left = self.deadline.map_or(Duration::ZERO, |deadline| deadline.saturating_duration_since(now));
        if !left.is_zero() {
            return Step::Sleep(left);
        }
        self.waiting = false;
        Step::Run(self.go.take())
    }

    pub(crate) fn clear(&mut self) {
        self.go = None;
        self.deadline = None;
        self.waiting = false;
    }
}

#[allow(dead_code)]
impl Debounce {
    pub fn run<T: Send + 'static>(&mut self, view: &View, wait: Duration, job: impl FnOnce(Rope) -> T + Send + 'static, done: impl FnOnce(T, &mut App) + 'static, cx: &mut App) {
        let (editor, rev, text) = (view.editor.clone(), view.key.rev, view.text.clone());
        let go: Go = Box::new(move |cx| {
            Box::pin(async move {
                if !cx.update(|cx| editor.read(cx).text_revision() == rev) {
                    return;
                }
                let answer = cx.background_executor().spawn(async move { job(text) }).await;
                cx.update(|cx| {
                    if editor.read(cx).text_revision() == rev {
                        done(answer, cx);
                    }
                });
            })
        });
        let deadline = cx.background_executor().now() + wait;
        if !self.slot.borrow_mut().push(deadline, go) {
            return;
        }
        let slot = self.slot.clone();
        self.task = Some(cx.spawn(async move |cx| {
            loop {
                let step = slot.borrow_mut().next(cx.background_executor().now());
                match step {
                    Step::Sleep(left) => cx.background_executor().timer(left).await,
                    Step::Run(go) => {
                        if let Some(go) = go {
                            go(cx.clone()).await;
                        }
                        return;
                    }
                }
            }
        }));
    }

    /// Drops the work waiting or running.
    pub fn cancel(&mut self) {
        self.task = None;
        self.slot.borrow_mut().clear();
    }
}

struct Driver {
    editor: WeakEntity<EditorState>,
    lang: Cell<&'static str>,
    layers: RefCell<Vec<Box<dyn MarkLayer>>>,
    /// The key the layers last saw.
    last: RefCell<Option<Key>>,
    /// The rows the layers last saw (the widened window); kept until the screen nears its edge.
    window: RefCell<Range<usize>>,
    /// A retry is waiting for the editor's first layout.
    retrying: Cell<bool>,
    /// The editor's tab is hidden and has let go of its layers' scan results.
    hidden: Cell<bool>,
    watch: RefCell<Option<Subscription>>,
    retry: RefCell<Option<Task<()>>>,
}

/// The most the first-layout retry waits: `RETRIES` looks, `RETRY_EVERY` apart.
const RETRIES: usize = 20;
const RETRY_EVERY: Duration = Duration::from_millis(50);

#[derive(Default)]
struct Book(HashMap<EntityId, Rc<Driver>>);

impl Global for Book {}

/// The editor's one hover provider: asks the editor's layers, in order, and shows the first answer.
struct Hover(WeakEntity<EditorState>);

impl HoverProvider for Hover {
    fn hover(&self, _text: &Rope, offset: usize, _window: &mut Window, cx: &mut App) -> Task<gpui_kit::Result<Option<lsp_types::Hover>>> {
        // The editor calls this inside its own update; a layer may read the editor, so the layers are
        // asked from a task, after that update has ended.
        let editor = self.0.clone();
        cx.spawn(async move |cx| {
            let said = cx.update(|cx| {
                let driver = editor.upgrade().and_then(|editor| cx.try_global::<Book>()?.0.get(&editor.entity_id()).cloned());
                driver.and_then(|driver| driver.layers.try_borrow().ok()?.iter().filter(|layer| layer.active()).find_map(|layer| layer.hover(offset, cx)))
            });
            let contents = said.map(|text| lsp_types::HoverContents::Markup(lsp_types::MarkupContent { kind: lsp_types::MarkupKind::PlainText, value: text.to_string() }));
            Ok(contents.map(|contents| lsp_types::Hover { contents, range: None }))
        })
    }
}

/// An editor was built for a file in language `lang`: it gets its driver and its layers.
pub fn attach(editor: &Entity<EditorState>, lang: &'static str, cx: &mut App) {
    editor.update(cx, |state, _| state.lsp_mut().hover_provider = Some(Rc::new(Hover(editor.downgrade()))));
    cx.default_global::<Book>().0.retain(|_, driver| driver.editor.upgrade().is_some());
    let layers = layers(editor, cx);
    let driver = Rc::new(Driver {
        editor: editor.downgrade(),
        lang: Cell::new(lang),
        layers: RefCell::new(layers),
        last: RefCell::new(None),
        window: RefCell::new(0..0),
        retrying: Cell::new(false),
        hidden: Cell::new(false),
        watch: RefCell::new(None),
        retry: RefCell::new(None),
    });
    let weak = Rc::downgrade(&driver);
    *driver.watch.borrow_mut() = Some(cx.observe(editor, move |editor, cx| {
        if let Some(driver) = weak.upgrade() {
            driver.tick(&editor, cx);
        }
    }));
    let id = editor.entity_id();
    cx.global_mut::<Book>().0.insert(id, driver);
    // A closed tab's editor leaves the book at once, not at the next attach.
    cx.observe_release(editor, move |_, cx| {
        cx.default_global::<Book>().0.remove(&id);
    })
    .detach();
}

/// The editor's tab hid (`true`) or showed (`false`). Hiding lets go of what the layers keep
/// between scans; the underlines on screen stay and the next scan refills the rest.
pub fn set_hidden(editor: &Entity<EditorState>, hidden: bool, cx: &mut App) {
    let Some(driver) = cx.try_global::<Book>().and_then(|book| book.0.get(&editor.entity_id()).cloned()) else { return };
    driver.hidden.set(hidden);
    if !hidden {
        // A notify while hidden left the key unseen: scan now, once, so the first frame has its marks.
        if driver.last.borrow().is_none() {
            driver.tick(editor, cx);
        }
        return;
    }
    if let Ok(mut layers) = driver.layers.try_borrow_mut() {
        layers.iter_mut().for_each(|layer| layer.release());
    }
}

/// For `tab-state`: the entries of the driver book, the words and marks the layers of all editors
/// keep, and the part of those that hidden tabs keep.
pub fn state_sizes(cx: &App) -> (usize, usize, usize) {
    let Some(book) = cx.try_global::<Book>() else { return (0, 0, 0) };
    let (mut all, mut hidden) = (0, 0);
    for driver in book.0.values() {
        let held: usize = driver.layers.try_borrow().map_or(0, |layers| layers.iter().map(|layer| layer.held()).sum());
        all += held;
        if driver.hidden.get() {
            hidden += held;
        }
    }
    (book.0.len(), all, hidden)
}

/// The file's language changed (Save As, a rename): layers see the new one from the next wake, which is now.
pub fn set_lang(editor: &Entity<EditorState>, lang: &'static str, cx: &mut App) {
    let Some(driver) = cx.try_global::<Book>().and_then(|book| book.0.get(&editor.entity_id())).cloned() else { return };
    if driver.lang.replace(lang) != lang {
        *driver.last.borrow_mut() = None;
        driver.tick(editor, cx);
    }
}

/// Runs `poke` on the editor's layer of type `L`; when it says it changed something, every layer
/// is woken again for the editor as it stands. `None` when the editor has no such layer.
pub fn with_layer<L: MarkLayer, R>(editor: &Entity<EditorState>, cx: &mut App, poke: impl FnOnce(&mut L) -> (R, bool)) -> Option<R> {
    let driver = cx.try_global::<Book>()?.0.get(&editor.entity_id())?.clone();
    let (answer, changed) = {
        let mut layers = driver.layers.borrow_mut();
        let layer = layers.iter_mut().find_map(|layer| (&mut **layer as &mut dyn Any).downcast_mut::<L>())?;
        poke(layer)
    };
    if changed {
        *driver.last.borrow_mut() = None;
        driver.tick(editor, cx);
    }
    Some(answer)
}

impl Driver {
    /// A hidden tab scans nothing, so it can't refill what `release` let go of: the key is marked
    /// unseen and `set_hidden(false)` scans once. True when the tick is to be skipped.
    fn hold_back(&self) -> bool {
        let hidden = self.hidden.get();
        if hidden {
            *self.last.borrow_mut() = None;
        }
        hidden
    }

    /// Wakes the layers if the key changed. Returns false when the editor has no layout yet.
    fn tick(self: &Rc<Self>, editor: &Entity<EditorState>, cx: &mut App) -> bool {
        if self.hold_back() {
            return true;
        }
        if !self.layers.borrow().iter().any(|layer| layer.active()) {
            return true;
        }

        let (text, key_parts, rows) = {
            let state = editor.read(cx);
            let text = state.text().clone();
            (text, (state.cursor(), state.selected_range(), state.cursor_count(), state.text_revision()), state.visible_row_range())
        };
        let Some(visible) = rows else {
            self.retry_later(editor, cx);
            return false;
        };
        let total = text.lines_len();
        let margin = visible.len().max(8);
        {
            let mut window = self.window.borrow_mut();
            let need = visible.start.saturating_sub(margin / 2)..(visible.end + margin / 2).min(total);
            if window.is_empty() || window.start > need.start || window.end < need.end {
                *window = visible.start.saturating_sub(margin)..(visible.end + margin).min(total);
            }
        }
        let rows = self.window.borrow().clone();
        let key = Key { caret: key_parts.0, selection: key_parts.1, carets: key_parts.2, rev: key_parts.3, rows };
        if self.last.borrow().as_ref() == Some(&key) {
            return true;
        }
        *self.last.borrow_mut() = Some(key.clone());
        let bytes = if total == 0 { 0..0 } else { text.line_start_offset(key.rows.start.min(total - 1))..text.line_end_offset(key.rows.end.saturating_sub(1).min(total - 1)) };
        let view = View { editor, key: &key, bytes, text: &text, lang: self.lang.get() };
        for layer in self.layers.borrow_mut().iter_mut().filter(|layer| layer.active()) {
            layer.update(&view, cx);
        }
        true
    }

    /// The editor hasn't laid out: look again shortly, a bounded number of times.
    fn retry_later(self: &Rc<Self>, editor: &Entity<EditorState>, cx: &mut App) {
        if self.retrying.replace(true) {
            return;
        }
        let (driver, editor) = (Rc::downgrade(self), editor.clone());
        *self.retry.borrow_mut() = Some(cx.spawn(async move |cx| {
            for _ in 0..RETRIES {
                cx.background_executor().timer(RETRY_EVERY).await;
                let Some(driver) = driver.upgrade() else { return };
                if cx.update(|cx| driver.tick(&editor, cx)) {
                    break;
                }
            }
            if let Some(driver) = driver.upgrade() {
                driver.retrying.set(false);
            }
        }));
    }
}

#[cfg(test)]
mod tests {
    use std::{
        cell::{Cell, RefCell},
        ops::Range,
        rc::Rc,
    };

    use gpui_kit::{App, WeakEntity};

    use super::{Driver, Key, MarkLayer, Slot, Step, View};

    /// A layer holding a scan list: `update` fills it, `release` empties it.
    struct Scan(Rc<Cell<usize>>);

    impl MarkLayer for Scan {
        fn active(&self) -> bool {
            true
        }
        fn update(&mut self, _view: &View, _cx: &mut App) {
            self.0.set(40);
        }
        fn release(&mut self) {
            self.0.set(0);
        }
        fn held(&self) -> usize {
            self.0.get()
        }
    }

    fn key(rows: Range<usize>) -> Key {
        Key { caret: 0, selection: 0..0, carets: 1, rev: 1, rows }
    }

    /// An editor that notifies while its tab is hidden is not scanned: the tick is held back and the
    /// key marked unseen, so the show scans exactly once; a shown tab is never held back.
    #[test]
    fn a_hidden_tab_skips_its_scan_and_marks_itself_unseen_for_the_show() {
        let held = Rc::new(Cell::new(0));
        let driver = Driver {
            editor: WeakEntity::new_invalid(),
            lang: Cell::new("text"),
            layers: RefCell::new(vec![Box::new(Scan(held.clone()))]),
            last: RefCell::new(Some(key(0..20))),
            window: RefCell::new(0..20),
            retrying: Cell::new(false),
            hidden: Cell::new(false),
            watch: RefCell::new(None),
            retry: RefCell::new(None),
        };
        assert!(!driver.hold_back(), "a shown tab ticks");
        assert!(driver.last.borrow().is_some());

        driver.hidden.set(true);
        driver.layers.borrow_mut().iter_mut().for_each(|layer| layer.release());
        assert!(driver.hold_back(), "a hidden tab does not scan");
        assert!(driver.hold_back(), "and a second notify holds back too");
        assert_eq!(held.get(), 0, "so it holds no scan list");
        assert!(driver.last.borrow().is_none(), "the show sees the key as unseen and scans once");
    }

    /// 200 keys, 20 ms apart, in four layers: each layer starts one task (before: 800) and, after the
    /// pause that follows the last key, runs exactly the newest job once.
    #[test]
    fn a_burst_of_keys_starts_one_task_per_layer_and_runs_the_newest_job_once() {
        use std::time::{Duration, Instant};
        let (wait, gap, start) = (Duration::from_millis(300), Duration::from_millis(20), Instant::now());
        let mut layers: Vec<Slot<usize>> = (0..4).map(|_| Slot::default()).collect();
        let mut spawned = 0;
        for key in 0..200 {
            let now = start + gap * key as u32;
            for slot in layers.iter_mut() {
                // The sleeping task wakes at its deadline; a deadline that moved sends it back to sleep.
                spawned += usize::from(slot.push(now + wait, key));
            }
        }
        assert_eq!(spawned, 4);
        let last = start + gap * 199;
        for slot in layers.iter_mut() {
            // The first wake, 300 ms after the first key, finds the deadline moved.
            assert!(matches!(slot.next(start + wait), Step::Sleep(left) if left == gap * 199));
            assert!(matches!(slot.next(last + wait), Step::Run(Some(199))), "the newest job runs, once");
            assert!(matches!(slot.next(last + wait), Step::Run(None)));
            // With the wait over, the next key starts a task again.
            assert!(slot.push(last + wait * 2, 200));
        }
    }
}
