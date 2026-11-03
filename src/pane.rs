//! A pane: a list of tabs, one document each, and the active one. It draws only the tab bar and
//! the hairline under it, as a band over the top of the pane's place in the window; the workspace
//! draws the active tab's editor (`panes::EditorPane`) under it and keeps it in line on
//! `PaneEvent::Changed`.
//!
//! Each tab keeps its own `Document`, so its edits, undo history and scroll stay as they were left
//! while another tab shows: switching only changes which editor is drawn. The window holds one pane
//! per leaf of its split tree (`panes::Tree`), each a cached sibling of the explorer and its own
//! editor, and listens for `PaneEvent::Changed` (the active tab, a title, an unsaved state or the
//! list changed) to bring the window title, the explorer, the editors and the preview in line.
//! `Activated` says the user picked a tab here, so this pane is the active one; `Emptied` says the
//! last tab went, and the window closes the pane unless it is the only one.

mod tab_list;

use std::{cell::RefCell, collections::{HashMap, HashSet}, path::{Path, PathBuf}, rc::Rc, time::Instant};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{EditorState, RopeExt as _};
use gpui_kit::*;

use crate::{
    document::{Document, DocumentChanged, DropTab, ShowTab},
    drag::{Dropped, Zone},
    marks,
    motion::{Fade, ScrollGlide},
    shake::Shake,
    tab_bar::{self, Motion, TabLabel},
    tokens::{PEEK_COLUMNS, PEEK_LINES, TAB_BAR_INSET, TAB_LIGHTS_CLEAR, TOP_BAND},
    start::Caret,
    native_menu::{self, Entry, Item, COMMAND, OPTION, SHIFT},
    workspace::{Direction, ask_unsaved_tabs},
};

/// A file over this many bytes keeps its highlighting (syntax tree and injection layers, about
/// 50 bytes of memory per byte of text) only while it is among the last `KEPT_HIGHLIGHTED` tabs
/// shown in its pane.
const BIG_FILE_BYTES: usize = 256 * 1024;
const KEPT_HIGHLIGHTED: usize = 2;

/// Sent when the pane's tabs or the active one changed in a way the window shows.
pub enum PaneEvent {
    Changed,
    /// A tab was shown here: the pane takes the window's focus.
    Activated,
    /// A tab's document needs it in front (a save that has to ask): the window shows that tab
    /// in this pane and makes the pane the active one.
    Show(EntityId),
    /// A tab's document asks to go without a question (its file is gone and the user said Close).
    Drop(EntityId),
    /// A tab holding a file was closed: where it was, for Reopen Closed Tab.
    Closed(ClosedTab),
    /// The tab's right-click menu asked to move it to the pane in that direction.
    MoveTab(EntityId, Direction),
    /// The tab's right-click menu asked to open it in a new window.
    NewWindow(EntityId),
    /// The tab's right-click menu asked to split its pane, the tab going into the new one.
    Split(EntityId, crate::settings::SplitAxis),
    /// The tab's right-click menu asked to show its file in the explorer.
    Reveal(EntityId),
    /// The last tab was closed.
    Emptied,
    /// Something was dropped on this pane's tab bar that is not one of its own tabs: a tab from
    /// another pane, or files. The window places it.
    Dropped(Dropped, Zone),
}

/// A closed tab's file, its slot in the bar, its caret and whether it was pinned.
pub struct ClosedTab {
    pub path: PathBuf,
    pub index: usize,
    pub caret: Option<Caret>,
    pub pinned: bool,
}

/// What a tab's right-click menu does, for the tab it opened on.
#[derive(Clone, Copy)]
enum TabCommand {
    Close(EntityId),
    CloseOthers(EntityId),
    CloseRight(EntityId),
    CloseLeft(EntityId),
    Pin(EntityId),
    Unpin(EntityId),
    Move(EntityId, Direction),
    NewWindow(EntityId),
    Split(EntityId, crate::settings::SplitAxis),
    Reveal(EntityId),
    ShowInFinder(EntityId),
    ShowPath(EntityId),
    CopyPath(EntityId),
}

impl EventEmitter<PaneEvent> for Pane {}

/// One tab: its document and the subscription that follows it.
struct Tab {
    document: Entity<Document>,
    /// A glance a single click in the explorer opened: the next such click takes this slot.
    /// Editing it, double-clicking it or dragging it keeps it.
    preview: bool,
    _subscription: Subscription,
}

pub struct Pane {
    tabs: Vec<Tab>,
    active: usize,
    /// The bar's motion: stepped only by this view's frames, which it asks for while a tab moves.
    motion: Motion,
    scroll: ScrollHandle,
    /// The strip's glide to a tab that came into play.
    glide: ScrollGlide,
    /// Where the glide in flight ends, so the tab list badge follows the view the strip settles in and not the frames on the way.
    glide_to: Option<f32>,
    /// Each tab's place in the last frame, for the drag.
    bounds: Rc<RefCell<Vec<Bounds<Pixels>>>>,
    /// The tab being dragged, while one is.
    dragging: Option<EntityId>,
    /// Where the lifted card sits from the pointer in the last drag move (the card's place with the pointer at the origin), so a tab let
    /// go outside the window can open its new window from the card.
    card: Option<Bounds<Pixels>>,
    /// Holds the keyboard while the pane has no tab.
    focus: FocusHandle,
    /// The pane in the window's top left corner: its bar leaves room for the traffic lights the
    /// explorer's drawn width doesn't cover.
    corner: bool,
    /// How tall the band holding the tab bar is: the tab centers in it and its rule sits at its foot.
    band: Pixels,
    /// The bar's right padding: `TAB_BAR_INSET`, or more where the window's preview button sits over it.
    right: Pixels,
    /// How wide the explorer is drawn now.
    left_edge: Pixels,
    /// The room the corner's top band leaves for the traffic lights and the explorer button.
    clear: Pixels,
    /// The tabs shown last, the showing one first: big files among them keep their highlighter.
    recent: Vec<EntityId>,
    /// Every tab, the one used last first: closing the showing tab returns to the next one here,
    /// and Ctrl-Tab walks it.
    history: History<EntityId>,
    /// The first `pinned` tabs are pinned: they stay at the front of the bar.
    pinned: usize,
    /// Whether a pane lies left, right, above and below this one (`Direction::ALL` order), set by the window.
    moves: [bool; 4],
    /// How many tabs the whole window holds, set by the window: a tab that is the window's only one has no window to leave for.
    window_tabs: usize,
    /// A Ctrl-Tab walk is under way: Control is still held and the history isn't reordered yet.
    switching: bool,
    /// Big tabs whose highlighter was let go while they were hidden; shown again, they get it back.
    dropped: HashSet<EntityId>,
    /// Where each tab sits among the strip's slots, kept between renders so a frame looks a tab up instead of searching the strip for it.
    slot_ix: HashMap<EntityId, usize>,
    /// How lit the active tab's mark is: 1 while this pane has the keyboard in the key window, 0
    /// when not, and the eased level between; `None` before the first frame.
    key_level: Option<f32>,
    /// The fade `key_level` is drawn from; `None` before the first frame.
    key_fade: Option<Fade>,
    /// The editor handle the focus watch follows and its subscriptions, which wake this pane when
    /// the keyboard enters or leaves it, and when the window turns key or not.
    watched: Option<FocusHandle>,
    watch: Vec<Subscription>,
    /// The tabs as the bar last drew them with exactly one tab: what a closing pane keeps showing.
    last_labels: Vec<TabLabel>,
    /// The number each open Untitled page shows after its name, held while it is open (`tab_bar::number_untitled`).
    untitled: HashMap<EntityId, u32>,
    /// The pane is closing and its last tab is gone: the bar draws `last_labels` until the pane does.
    leaving: bool,
    /// The sideways shake the bar gives a refusal the pane's tabs made (`shake`); the bar's own wrapper moves, never the editor.
    shake: Option<Shake>,
    /// The tab the pointer is on, whether it was pressed since it came, and a count that changes with every hover, so a rest's timer knows
    /// whether it is still the one that was waited for (`peek_hover`).
    peek_tab: Option<EntityId>,
    peek_pressed: bool,
    peek_turn: u32,
}

impl Pane {
    /// A pane with `document` as its one tab.
    pub fn new(document: Entity<Document>, cx: &mut Context<Self>) -> Self {
        let mut this = Self::empty(cx);
        this.push(document, cx);
        this
    }

    /// A pane with no tab.
    pub fn empty(cx: &mut Context<Self>) -> Self {
        // The editors draw their find bar with the app's view; set once, before any editor draws.
        crate::find_bar::ensure(cx);
        Self {
            tabs: Vec::new(),
            active: 0,
            motion: Motion::default(),
            scroll: ScrollHandle::new(),
            glide: ScrollGlide::default(),
            glide_to: None,
            bounds: Rc::default(),
            dragging: None,
            card: None,
            focus: cx.focus_handle(),
            corner: false,
            band: TOP_BAND,
            right: TAB_BAR_INSET,
            left_edge: px(0.),
            clear: TAB_LIGHTS_CLEAR,
            recent: Vec::new(),
            history: History::default(),
            pinned: 0,
            moves: [false; 4],
            window_tabs: 0,
            switching: false,
            dropped: HashSet::new(),
            slot_ix: HashMap::new(),
            key_level: None,
            key_fade: None,
            watched: None,
            watch: Vec::new(),
            last_labels: Vec::new(),
            untitled: HashMap::new(),
            leaving: false,
            shake: None,
            peek_tab: None,
            peek_pressed: false,
            peek_turn: 0,
        }
    }

    /// A refusal this pane's tabs made (the beep sounded): the tab bar shakes once. Only the bar's
    /// own container moves, so the cached editor under it is never rebuilt.
    pub fn shake(&mut self, window: &Window, cx: &mut Context<Self>) {
        self.shake = Some(Shake::new(crate::motion::drawn_now(window.window_handle().window_id())));
        cx.notify();
    }

    /// The window is closing this pane (its last tab just went): the bar keeps the last tab it
    /// drew instead of folding it away, so the pane leaves with its content.
    pub fn hold_bar(&mut self, cx: &mut Context<Self>) {
        self.leaving = true;
        cx.notify();
    }

    /// Says whether this is the pane in the window's top left corner.
    pub fn set_corner(&mut self, corner: bool, cx: &mut Context<Self>) {
        if self.corner != corner {
            self.corner = corner;
            cx.notify();
        }
    }

    /// Says how tall this pane's band is, `TOP_BAND` at the window's top and `PANE_BAND` below it.
    pub fn set_band(&mut self, band: Pixels, cx: &mut Context<Self>) {
        if self.band != band {
            self.band = band;
            cx.notify();
        }
    }

    /// Says how much room the bar's right end leaves: the window's preview button sits over it
    /// while the preview is shut, and the room closes with the preview's slide.
    pub fn set_right(&mut self, right: Pixels, cx: &mut Context<Self>) {
        if self.right != right {
            self.right = right;
            cx.notify();
        }
    }

    /// Tells the pane how wide the explorer is drawn now (it slides), so the corner pane's
    /// bar clears the traffic lights by what the explorer doesn't.
    pub fn set_left_edge(&mut self, edge: Pixels, cx: &mut Context<Self>) {
        if self.left_edge != edge {
            self.left_edge = edge;
            cx.notify();
        }
    }

    /// Tells the pane how much room the corner's top band keeps for the traffic lights and the
    /// explorer button now (it eases in and out of full screen).
    pub fn set_clear(&mut self, clear: Pixels, cx: &mut Context<Self>) {
        if self.clear != clear {
            self.clear = clear;
            cx.notify();
        }
    }

    /// Follows the focus of the handle that decides whether this pane is the one typing: a change
    /// of it, or of the window's key state, redraws the bar so its mark can ease.
    fn watch_focus(&mut self, handle: &FocusHandle, window: &mut Window, cx: &mut Context<Self>) {
        if self.watched.as_ref() == Some(handle) {
            return;
        }
        self.watched = Some(handle.clone());
        self.watch = vec![
            cx.on_focus_in(handle, window, |_, _, cx| cx.notify()),
            cx.on_focus_out(handle, window, |_, _, _, cx| cx.notify()),
            cx.observe_window_activation(window, |this, window, cx| {
                this.welcome_back(window, cx);
                cx.notify();
            }),
        ];
    }

    /// Tracks when the window left the front; on coming back after `WELCOME_BACK_AFTER` (or the
    /// test override `GPUI_EDITOR_WELCOME_BACK_SECS`) washes the caret line of the editor that
    /// has the keyboard, once. Reduce Motion skips it.
    fn welcome_back(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        thread_local! {
            /// When any Finch window last lost the key, for every pane: switching between Finch
            /// windows or a long Save dialog is not time away.
            static LEFT_FRONT: std::cell::Cell<Option<Instant>> = const { std::cell::Cell::new(None) };
        }
        if !window.is_window_active() {
            LEFT_FRONT.set(Some(Instant::now()));
            return;
        }
        let Some(left) = LEFT_FRONT.get() else {
            return;
        };
        let after = std::env::var("GPUI_EDITOR_WELCOME_BACK_SECS")
            .ok()
            .and_then(|secs| secs.parse::<f32>().ok())
            .and_then(|secs| std::time::Duration::try_from_secs_f32(secs).ok())
            .unwrap_or(crate::tokens::WELCOME_BACK_AFTER);
        if cx.reduce_motion() || left.elapsed() < after {
            return;
        }
        let Some(editor) = self.active_document().and_then(|document| document.read(cx).built_editor().cloned()) else {
            return;
        };
        if !editor.read(cx).focus_handle(cx).is_focused(window) {
            return;
        }
        editor.update(cx, |state, cx| state.wash_caret_line(crate::tokens::WELCOME_BACK_WASH, cx));
    }

    /// Eases the mark's level toward `key` (1 lit, 0 dim) on the shared fade and says whether it
    /// still moves.
    fn ease_key(&mut self, key: bool, now: Instant) -> bool {
        let to = f32::from(key);
        let fade = self.key_fade.get_or_insert(Fade::at(to));
        fade.aim(to, now);
        self.key_level = Some(fade.level(now));
        fade.is_moving(now)
    }

    /// Puts the keyboard in the showing tab's editor, or in the pane while it has none.
    pub fn focus_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.active_document() {
            Some(document) => {
                document.clone().update(cx, |document, cx| document.focus(window, cx));
            }
            None => self.focus.focus(window, cx),
        }
    }

    /// Takes the showing tab out without closing its document, to put it in another pane. The tab
    /// on its right (else on its left) shows next; nothing takes the keyboard.
    pub fn take_active(&mut self, cx: &mut Context<Self>) -> Option<Entity<Document>> {
        let id = self.active_document()?.entity_id();
        self.take(id, cx)
    }

    /// Takes tab `id` out without closing its document, to put it in another pane. It doesn't
    /// say `Emptied` when it was the last: the window that moves it decides what the pane does.
    pub fn take(&mut self, id: EntityId, cx: &mut Context<Self>) -> Option<Entity<Document>> {
        let ix = self.index_of(id)?;
        let tab = self.tabs.remove(ix);
        self.pinned = after_remove(ix, self.pinned);
        self.history.forget(id);
        if ix < self.active {
            self.active -= 1;
        }
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        // A tab leaving for another pane takes its colours with it, whichever pane shows it.
        if self.dropped.remove(&id) {
            let (editor, lang) = {
                let document = tab.document.read(cx);
                (document.built_editor().cloned(), document.language(cx))
            };
            if let Some(editor) = editor {
                Self::set_colours(&editor, lang, cx);
            }
        }
        forget_tab(&mut self.recent, &mut self.dropped, &id);
        self.settle_highlighters(cx);
        if self.dragging == Some(id) {
            self.dragging = None;
        }
        self.reveal();
        cx.emit(PaneEvent::Changed);
        cx.notify();
        Some(tab.document)
    }

    /// Puts `document` in a new tab at slot `at` (the end when past it, and never among the pins)
    /// and shows it.
    pub fn insert(&mut self, document: Entity<Document>, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.push(document, cx);
        if let Some(tab) = self.tabs.pop() {
            let at = at.max(self.pinned).min(self.tabs.len());
            self.tabs.insert(at, tab);
            self.show(at, window, cx);
        }
    }

    /// Puts `document` at the end of the tabs (of the pins, when `pinned`) without showing it, for a merge: the tab showing stays showing,
    /// and a tab never shown stays unbuilt.
    pub fn append(&mut self, document: Entity<Document>, pinned: bool, cx: &mut Context<Self>) {
        let id = document.entity_id();
        self.push(document, cx);
        if pinned {
            self.pin_id(id, cx);
        } else {
            cx.emit(PaneEvent::Changed);
            cx.notify();
        }
    }

    /// Puts `document` back as a pinned tab at slot `at` (the end of the pins when past them) and shows it.
    pub fn insert_pinned(&mut self, document: Entity<Document>, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.push(document, cx);
        if let Some(tab) = self.tabs.pop() {
            let at = at.min(self.pinned);
            self.tabs.insert(at, tab);
            self.pinned += 1;
            self.show(at, window, cx);
        }
    }

    /// Tells the pane which directions have a pane beside it and how many tabs the window holds, for its tabs' menus; `true` when that changed.
    pub fn set_moves(&mut self, moves: [bool; 4], window_tabs: usize) -> bool {
        let moved = std::mem::replace(&mut self.moves, moves) != moves;
        std::mem::replace(&mut self.window_tabs, window_tabs) != window_tabs || moved
    }

    /// How many tabs are pinned: the first `pinned_count` tabs.
    pub fn pinned_count(&self) -> usize {
        self.pinned
    }

    /// Whether the tab at `ix` is pinned.
    pub fn is_pinned(&self, ix: usize) -> bool {
        ix < self.pinned
    }

    /// Whether the tab holding `id` is pinned.
    pub fn is_pinned_id(&self, id: EntityId) -> bool {
        self.index_of(id).is_some_and(|ix| self.is_pinned(ix))
    }

    /// Pins tab `id`: it moves to the end of the pins and stays at the front. A preview tab
    /// becomes a kept tab.
    pub fn pin_id(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(id) else { return };
        if self.is_pinned(ix) {
            return;
        }
        let (to, pinned) = pin_move(ix, self.pinned);
        self.tabs[ix].preview = false;
        self.move_tab(ix, to);
        self.pinned = pinned;
        self.reveal();
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// Unpins tab `id`: it moves to just after the pins that stay.
    pub fn unpin_id(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(id) else { return };
        if !self.is_pinned(ix) {
            return;
        }
        let (to, pinned) = unpin_move(ix, self.pinned);
        self.move_tab(ix, to);
        self.pinned = pinned;
        self.reveal();
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// The slots of the group tab `ix` is in: the pinned tabs, or the others.
    fn group_of(&self, ix: usize) -> std::ops::Range<usize> {
        if ix < self.pinned { 0..self.pinned } else { self.pinned..self.tabs.len() }
    }

    /// Whether tab `id` can move `by` places (-1 earlier, 1 later) and stay among the tabs of its own group, pinned or not.
    pub fn can_step_tab(&self, id: EntityId, by: isize) -> bool {
        self.index_of(id).is_some_and(|ix| {
            let group = self.group_of(ix);
            usize::try_from(ix as isize + by).is_ok_and(|to| group.contains(&to))
        })
    }

    /// Moves tab `id` one place earlier (`by` -1) or later (1) within its group; the tabs glide as they do after a drop. `false` at the group's end.
    pub fn step_tab(&mut self, id: EntityId, by: isize, cx: &mut Context<Self>) -> bool {
        if !self.can_step_tab(id, by) {
            return false;
        }
        let Some(ix) = self.index_of(id) else { return false };
        self.move_tab(ix, (ix as isize + by) as usize);
        self.reveal();
        cx.emit(PaneEvent::Changed);
        cx.notify();
        true
    }

    /// Moves the tab at `from` to slot `to`; the showing tab stays the showing tab.
    fn move_tab(&mut self, from: usize, to: usize) {
        let active = self.active_document().map(Entity::entity_id);
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        if let Some(ix) = active.and_then(|id| self.index_of(id)) {
            self.active = ix;
        }
    }

    /// The document in the active tab; `None` in an empty pane.
    pub fn active_document(&self) -> Option<&Entity<Document>> {
        self.tabs.get(self.active).map(|tab| &tab.document)
    }

    /// The documents of every tab, in tab order.
    pub fn documents(&self) -> impl Iterator<Item = &Entity<Document>> {
        self.tabs.iter().map(|tab| &tab.document)
    }

    pub fn len(&self) -> usize {
        self.tabs.len()
    }

    /// How many entries the bookkeeping holds, for the control socket's `tab-state`: `recent`,
    /// `dropped`, `untitled` and `history`.
    pub fn state_sizes(&self) -> [usize; 4] {
        [self.recent.len(), self.dropped.len(), self.untitled.len(), self.history.order.len()]
    }

    /// The active tab's place in the list.
    pub fn active_index(&self) -> usize {
        self.active
    }

    /// The tab holding the file at `path`.
    pub fn index_of_path(&self, path: &Path, cx: &App) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.document.read(cx).path.as_deref() == Some(path))
    }

    fn index_of(&self, id: EntityId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.document.entity_id() == id)
    }

    /// The document id of the tab at `ix`.
    pub fn id_at(&self, ix: usize) -> Option<EntityId> {
        self.tabs.get(ix).map(|tab| tab.document.entity_id())
    }

    /// Runs `f` on this pane once the update in progress is over, for a click's changes.
    pub fn later(&mut self, window: &mut Window, cx: &mut Context<Self>, f: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static) {
        let pane = cx.entity();
        window.defer(cx, move |window, cx| pane.update(cx, |pane, cx| f(pane, window, cx)));
    }

    /// A save of the document in tab `id` landed (`ok`) or failed, `nth` of a save all's ripple (0 alone): the bar settles its dot.
    pub fn save_settled(&mut self, id: EntityId, ok: bool, nth: u32, cx: &mut Context<Self>) {
        if self.index_of(id).is_some() {
            self.motion.settle(id, ok, nth.min(crate::tokens::SAVE_STAGGER_MAX) as f32 * crate::tokens::SAVE_STAGGER);
            cx.notify();
        }
    }

    /// The pointer came onto tab `id` (`over`) or left it. Half a second on a tab opens its peek card; a card already up moves to the
    /// tab at once and glides there. A tab pressed since the pointer came keeps its card away until the pointer leaves it.
    pub fn peek_hover(&mut self, id: EntityId, over: bool, _: &mut Window, cx: &mut Context<Self>) {
        if self.index_of(id).is_none() {
            return;
        }
        self.peek_turn += 1;
        if !over {
            if self.peek_tab == Some(id) {
                self.peek_tab = None;
                self.peek_pressed = false;
                if self.motion.peek_close() {
                    cx.notify();
                }
            }
            return;
        }
        self.peek_tab = Some(id);
        self.peek_pressed = false;
        if cx.has_active_drag() {
            return;
        }
        if self.motion.peek_shows() {
            self.open_peek(id, cx);
            return;
        }
        let turn = self.peek_turn;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(crate::tokens::TOOLTIP_DELAY).await;
            this.update(cx, |pane, cx| {
                if pane.peek_turn == turn && pane.peek_tab == Some(id) && !pane.peek_pressed {
                    pane.open_peek(id, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Tab `id` was pressed (selected, dragged, a menu opened): its peek card goes and stays away until the pointer leaves it.
    pub fn peek_press(&mut self, id: EntityId, cx: &mut Context<Self>) {
        if self.peek_tab == Some(id) {
            self.peek_pressed = true;
            self.peek_turn += 1;
            if self.motion.peek_close() {
                cx.notify();
            }
        }
    }

    /// Opens the peek card on tab `id`: the tab's tip, and five lines of its text around the caret.
    fn open_peek(&mut self, id: EntityId, cx: &mut Context<Self>) {
        let (Some(label), Some(ix)) = (self.motion.label_of(id), self.index_of(id)) else { return };
        let header = tab_bar::tab_tip(&label.title, label.path.as_deref(), label.pinned, label.dirty, label.locked, label.conflict, label.gone);
        let (lines, caret_row) = peek_lines(self.tabs[ix].document.read(cx), cx);
        self.motion.peek_open(id, header, lines, caret_row);
        cx.notify();
    }

    /// Appends the tab without showing it.
    fn push(&mut self, document: Entity<Document>, cx: &mut Context<Self>) {
        let changed = cx.subscribe(&document, |this, document, _: &DocumentChanged, cx| {
            // The first edit keeps a preview tab.
            if document.read(cx).shows_dot() {
                this.keep_id(document.entity_id());
            }
            cx.emit(PaneEvent::Changed);
            cx.notify();
        });
        let show = cx.subscribe(&document, |_, document, _: &ShowTab, cx| cx.emit(PaneEvent::Show(document.entity_id())));
        let drop = cx.subscribe(&document, |_, document, _: &DropTab, cx| cx.emit(PaneEvent::Drop(document.entity_id())));
        let subscription = Subscription::join(Subscription::join(changed, show), drop);
        self.tabs.push(Tab { document, preview: false, _subscription: subscription });
        // A tab not shown yet is the least recent.
        let id = self.tabs[self.tabs.len() - 1].document.entity_id();
        self.history.append(id);
    }

    /// Opens `document` as this pane's preview tab and shows it: a clean preview tab already
    /// here gives its slot to it (same position); otherwise it is added like `add` does, marked
    /// as the preview.
    pub fn add_preview(&mut self, document: Entity<Document>, window: &mut Window, cx: &mut Context<Self>) {
        let old = self.tabs.iter().position(|tab| tab.preview && !tab.document.read(cx).shows_dot());
        let Some(ix) = old else {
            self.add(document, true, window, cx);
            if let Some(tab) = self.tabs.last_mut() {
                tab.preview = true;
            }
            return;
        };
        self.push(document, cx);
        let Some(mut tab) = self.tabs.pop() else { return };
        tab.preview = true;
        let old = std::mem::replace(&mut self.tabs[ix], tab);
        self.dropped.remove(&old.document.entity_id());
        self.history.forget(old.document.entity_id());
        self.show(ix, window, cx);
    }

    /// Makes tab `id` a normal tab.
    pub fn keep_id(&mut self, id: EntityId) {
        if let Some(ix) = self.index_of(id) {
            self.tabs[ix].preview = false;
        }
    }

    /// Makes the tab at `ix` a normal tab.
    pub fn keep(&mut self, ix: usize, cx: &mut Context<Self>) {
        if self.tabs.get(ix).is_some_and(|tab| tab.preview) {
            self.tabs[ix].preview = false;
            cx.notify();
        }
    }

    /// Whether the tab holding `id` is the preview tab.
    pub fn is_preview(&self, ix: usize) -> bool {
        self.tabs.get(ix).is_some_and(|tab| tab.preview)
    }

    /// Opens `document` in a new tab and shows it. With `replace_blank`, an active tab that is a
    /// never-edited Untitled page is closed by it (a page nothing can lead back to).
    pub fn add(&mut self, document: Entity<Document>, replace_blank: bool, window: &mut Window, cx: &mut Context<Self>) {
        let blank = replace_blank
            && self.active_document().is_some_and(|active| {
                let active = active.read(cx);
                active.is_blank()
            });
        let replaced = blank.then_some(self.active);
        self.push(document, cx);
        if let Some(ix) = replaced {
            let gone = self.tabs.remove(ix);
            self.pinned = after_remove(ix, self.pinned);
            self.history.forget(gone.document.entity_id());
        }
        self.show(self.tabs.len() - 1, window, cx);
    }

    /// Replaces every tab with `documents`, in order, and shows the one at `active`; the first
    /// `pinned` of them are pinned. For the tabs of a window that come back on relaunch: the
    /// window decides which pane takes the keyboard, so this one doesn't claim it.
    pub fn set_tabs(&mut self, documents: Vec<Entity<Document>>, active: usize, pinned: usize, cx: &mut Context<Self>) {
        if documents.is_empty() {
            return;
        }
        self.active = active.min(documents.len() - 1);
        self.pinned = pinned.min(documents.len());
        self.tabs.clear();
        self.history = History::default();
        for document in documents {
            self.push(document, cx);
        }
        if let Some(id) = self.active_document().map(Entity::entity_id) {
            self.history.touch(id);
        }
        self.reveal();
        self.settle_highlighters(cx);
        // The preview lines up with the editor at once, whichever file it last followed.
        if let Some(preview) = self.active_document().and_then(|document| document.read(cx).preview.as_ref()) {
            preview.stop_following();
        }
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// Shows tab `ix`; the editor takes the keyboard.
    pub fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix < self.tabs.len() {
            self.show(ix, window, cx);
        }
    }

    pub fn activate_id(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.index_of(id) {
            self.show(ix, window, cx);
        }
    }

    fn show(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        // Any other way of showing a tab ends a Ctrl-Tab walk where it stands.
        self.switching = false;
        if let Some(id) = self.tabs.get(ix).map(|tab| tab.document.entity_id()) {
            self.history.touch(id);
        }
        self.display(ix, window, cx);
    }

    /// Shows tab `ix` without touching the history: the steps of a Ctrl-Tab walk.
    fn display(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active = ix;
        self.reveal();
        if self.switching {
            // A walk only gives the tab it stands on its colours back; the rest settles once it ends.
            self.restore_active_colours(cx);
        } else {
            self.settle_highlighters(cx);
        }
        if let Some(document) = self.active_document().cloned() {
            let editor = document.update(cx, |document, cx| document.editor(window, cx));
            // The preview lines up with the editor at once, whichever file it last followed.
            if let Some(preview) = document.read(cx).preview.as_ref() {
                preview.stop_following();
            }
            // The Compare view takes the keyboard while it is open, else the editor.
            document.update(cx, |document, cx| document.focus(window, cx));
            // Also on a Ctrl-Tab step: the tab it stands on shows its matches, scanned in the background.
            editor.update(cx, |state, cx| state.restore_search_matches(cx));
        }
        cx.emit(PaneEvent::Activated);
        cx.emit(PaneEvent::Changed);
        cx.notify();
    }

    /// The hand scrolled the tab strip: a glide stops where it is.
    pub fn hand_scrolled(&mut self) {
        self.glide.cancel();
        self.glide_to = None;
    }

    /// Brings the active tab into view with the next frame, from where the strip is drawn now. A
    /// glide still running keeps its pace into the new one, so a second jump turns around from where
    /// the strip is drawn.
    fn reveal(&mut self) {
        self.glide.coast();
        self.motion.reveal();
    }

    /// The showing tab gets its highlighter back when it was dropped; nothing else changes.
    fn restore_active_colours(&mut self, cx: &mut Context<Self>) {
        let Some(document) = self.active_document().cloned() else { return };
        if !self.dropped.remove(&document.entity_id()) {
            return;
        }
        if let Some(editor) = document.read(cx).built_editor().cloned() {
            let lang = document.read(cx).language(cx);
            Self::set_colours(&editor, lang, cx);
        }
    }

    /// Gives the showing tab its highlighter back when it was dropped, and lets go of the one on
    /// every big hidden tab beyond the `KEPT_HIGHLIGHTED` shown last. Only the highlighter goes:
    /// text, cursor, scroll and undo stay with the editor. Every hidden tab also lets go of the
    /// shaped lines of its last screen; its next draw shapes them fresh. The matches of an open
    /// Find go with the tab's fade (`EditorPane::let_go`) and come back in `display`.
    fn settle_highlighters(&mut self, cx: &mut Context<Self>) {
        let live: Vec<EntityId> = self.tabs.iter().map(|tab| tab.document.entity_id()).collect();
        self.recent.retain(|id| live.contains(id));
        self.dropped.retain(|id| live.contains(id));
        if let Some(active) = self.active_document().map(Entity::entity_id) {
            self.recent.retain(|id| *id != active);
            self.recent.insert(0, active);
        }
        self.recent.truncate(KEPT_HIGHLIGHTED);
        let showing = self.active_document().map(Entity::entity_id);
        for tab in &self.tabs {
            let id = tab.document.entity_id();
            // A tab not built yet has no highlighter to settle.
            let Some(editor) = tab.document.read(cx).built_editor().cloned() else { continue };
            if Some(id) != showing {
                editor.update(cx, |state, _| state.release_layout());
                marks::set_hidden(&editor, true, cx);
            } else {
                marks::set_hidden(&editor, false, cx);
                // A tab shown by a move (`take`) restores its matches here; `display` calls it too and the second call no-ops.
                editor.update(cx, |state, cx| state.restore_search_matches(cx));
            }
            // The language is asked only by the branches that use it: it reads the file's edge lines.
            let document = &tab.document;
            let big = || editor.read(cx).text().len() > BIG_FILE_BYTES;
            match colour_change(self.recent.contains(&id), self.dropped.contains(&id), big, || document.read(cx).language(cx)) {
                Colours::Keep => {}
                Colours::Restore(lang) => {
                    self.dropped.remove(&id);
                    Self::set_colours(&editor, lang, cx);
                }
                Colours::Drop => {
                    Self::set_colours(&editor, "text", cx);
                    self.dropped.insert(id);
                }
            }
        }
        self.settle_previews(cx);
    }

    /// Keeps the parsed preview of the tabs shown last (`KEPT_HIGHLIGHTED`), the showing one
    /// included, whether or not the panel shows, and lets go of every other tab's, so a Markdown
    /// tab left behind holds no parsed tree. A tab that comes back parses its text anew.
    fn settle_previews(&mut self, cx: &mut Context<Self>) {
        for tab in &self.tabs {
            let warm = self.recent.contains(&tab.document.entity_id());
            tab.document.update(cx, |document, cx| document.keep_preview_warm(warm, cx));
        }
    }

    /// Sets the editor's highlighter language; "text" has no grammar, so its highlighter goes. A
    /// file past `language::MAX_HIGHLIGHT_BYTES` gets "text" whatever `lang` is.
    fn set_colours(editor: &Entity<EditorState>, lang: &'static str, cx: &mut Context<Self>) {
        let lang = crate::language::for_highlighter(lang, editor.read(cx).text().len());
        editor.update(cx, |state, cx| state.set_highlighter(lang, cx));
    }

    /// Cmd-Shift-] and Cmd-Shift-[ : the next or previous tab by position, wrapping around.
    pub fn step(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.tabs.len() as isize;
        if count > 1 {
            self.show((self.active as isize + step).rem_euclid(count) as usize, window, cx);
        }
    }

    /// Ctrl-Tab (`step` 1) and Ctrl-Shift-Tab (`step` -1): one step further back (or forward)
    /// in the recent list than the tab showing. The history keeps its order until `end_switch`.
    pub fn step_recent(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(current) = self.active_document().map(Entity::entity_id) else { return };
        let Some(id) = self.history.step_from(current, step) else { return };
        let Some(ix) = self.index_of(id) else { return };
        self.switching = true;
        self.display(ix, window, cx);
    }

    /// Control was let go: the tab a Ctrl-Tab walk landed on goes to the front of the history.
    pub fn end_switch(&mut self, cx: &mut Context<Self>) {
        if std::mem::take(&mut self.switching) {
            if let Some(id) = self.active_document().map(Entity::entity_id) {
                self.history.touch(id);
            }
            self.settle_highlighters(cx);
        }
    }

    /// Cmd-1 to Cmd-9: the `n`th tab, counting from 1; 9 is the last tab.
    pub fn show_nth(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        let ix = if n >= 9 { self.tabs.len().checked_sub(1) } else { n.checked_sub(1).filter(|ix| *ix < self.tabs.len()) };
        if let Some(ix) = ix {
            self.show(ix, window, cx);
        }
    }

    pub fn close_id(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(ix) = self.index_of(id) {
            self.close(ix, window, cx);
        }
    }

    /// Closes tab `ix`. One with unsaved edits asks Save / Don’t Save / Cancel for its file first.
    pub fn close(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.close_where(move |at| at == ix, window, cx);
    }

    /// Closes every tab but `id` and the pinned ones.
    pub fn close_others(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(keep) = self.index_of(id) {
            let pinned = self.pinned;
            self.close_where(move |ix| ix != keep && ix >= pinned, window, cx);
        }
    }

    /// Closes the tabs right of `id`, except the pinned ones.
    pub fn close_right(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(from) = self.index_of(id) {
            let pinned = self.pinned;
            self.close_where(move |ix| ix > from && ix >= pinned, window, cx);
        }
    }

    /// Closes the tabs left of `id`, except the pinned ones.
    pub fn close_left(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(from) = self.index_of(id) {
            let pinned = self.pinned;
            self.close_where(move |ix| ix < from && ix >= pinned, window, cx);
        }
    }

    /// Whether `close_others` for tab `id` would close something: a tab that is neither `id` nor pinned.
    pub fn can_close_others(&self, id: EntityId) -> bool {
        self.index_of(id).is_some_and(|keep| (self.pinned..self.tabs.len()).any(|ix| ix != keep))
    }

    /// Whether `close_right` for tab `id` would close something: an unpinned tab right of it.
    pub fn can_close_right(&self, id: EntityId) -> bool {
        self.index_of(id).is_some_and(|from| (from + 1).max(self.pinned) < self.tabs.len())
    }

    /// Whether `close_left` for tab `id` would close something: an unpinned tab left of it.
    pub fn can_close_left(&self, id: EntityId) -> bool {
        self.index_of(id).is_some_and(|from| self.pinned < from)
    }

    /// Closes the tabs `matching` picks by place. Unsaved edits are asked about once for all of
    /// them: Save, Don’t Save and Cancel for one file, Save all, Discard all and Cancel for
    /// several. Cancel closes nothing; a tab whose save fails stays open. A draft never asks.
    pub fn close_where(&mut self, matching: impl Fn(usize) -> bool, window: &mut Window, cx: &mut Context<Self>) {
        let documents: Vec<Entity<Document>> =
            self.tabs.iter().enumerate().filter(|(ix, _)| matching(*ix)).map(|(_, tab)| tab.document.clone()).collect();
        self.close_documents(documents, window, cx);
    }

    /// `close_where` for tabs already picked.
    fn close_documents(&mut self, documents: Vec<Entity<Document>>, window: &mut Window, cx: &mut Context<Self>) {
        // A draft's last edits are written first: a failed write is what makes it ask.
        for document in &documents {
            document.update(cx, |document, cx| document.flush_draft(cx));
        }
        // A save is writing: its dot is about to go, so wait for it, then close the same tabs.
        let saving: Vec<Entity<Document>> = documents.iter().filter(|document| document.read(cx).is_saving()).cloned().collect();
        if !saving.is_empty() {
            let landings: Vec<Task<bool>> = saving.iter().map(|document| document.update(cx, |document, cx| document.save_landed(window, cx))).collect();
            cx.spawn_in(window, async move |this, cx| {
                for landing in landings {
                    landing.await;
                }
                this.update_in(cx, |this, window, cx| {
                    // Tabs closed meanwhile are gone from the list; the rest close now.
                    let documents: Vec<Entity<Document>> = documents.into_iter().filter(|document| this.index_of(document.entity_id()).is_some()).collect();
                    this.close_documents(documents, window, cx);
                })
                .ok();
            })
            .detach();
            return;
        }
        let dirty: Vec<Entity<Document>> = documents.iter().filter(|document| document.read(cx).shows_dot()).cloned().collect();
        if dirty.is_empty() {
            let ids: Vec<EntityId> = documents.iter().map(Entity::entity_id).collect();
            self.remove_all(&ids, window, cx);
            return;
        }
        let names: Vec<crate::workspace::Unsaved> = dirty.iter().map(|document| (document.read(cx).title(), document.read(cx).path.clone())).collect();
        let answer = ask_unsaved_tabs(&names, window, cx);
        cx.spawn_in(window, async move |this, cx| {
            // Tabs that stay: those whose save failed.
            let mut kept: Vec<EntityId> = Vec::new();
            let discard = match answer.await {
                Some(0) => {
                    for document in &dirty {
                        let Ok(saved) = this.update_in(cx, |_, window, cx| document.update(cx, |document, cx| document.save(window, cx))) else { return };
                        if !saved.await {
                            kept.push(document.entity_id());
                        }
                    }
                    false
                }
                Some(1) => true,
                _ => return,
            };
            this.update_in(cx, |this, window, cx| {
                // A tab that took edits after the question (or while a Save As panel was open) was
                // not asked about: only Discard all closes tabs that are still unsaved.
                let ids: Vec<EntityId> = documents
                    .iter()
                    .filter(|document| !kept.contains(&document.entity_id()))
                    .filter(|document| !document.read(cx).shows_dot() || (discard && dirty.contains(document)))
                    .map(Entity::entity_id)
                    .collect();
                this.remove_all(&ids, window, cx);
            })
            .ok();
        })
        .detach();
    }

    /// Takes the tabs `ids` out. When the showing tab is one of them, the most recently used tab
    /// that stays shows first, so no closing tab is built on the way.
    fn remove_all(&mut self, ids: &[EntityId], window: &mut Window, cx: &mut Context<Self>) {
        let showing = self.active_document().map(Entity::entity_id);
        if showing.is_some_and(|showing| ids.contains(&showing)) {
            let stays = self.history.order.iter().copied().find(|id| !ids.contains(id));
            if let Some(ix) = stays.and_then(|id| self.index_of(id)) {
                self.show(ix, window, cx);
            }
        }
        for id in ids {
            self.remove(*id, window, cx);
        }
    }

    /// The rows of tab `id`'s right-click menu: what the native menu pops up and what `pick_tab_menu_row` picks from.
    fn tab_menu_entries(&self, id: EntityId, cx: &App) -> Vec<Entry<TabCommand>> {
        let pinned = self.is_pinned_id(id);
        // An Untitled page has no file to show or copy.
        let on_disk = self.index_of(id).is_some_and(|ix| self.tabs[ix].document.read(cx).path.is_some());
        // The menu's keys act on the showing tab, so only its menu writes them.
        let showing = self.tabs.get(self.active).is_some_and(|tab| tab.document.entity_id() == id);
        let hint = |item: Item<TabCommand>, key: &'static str, modifiers: usize| if showing { item.key(key, modifiers) } else { item };
        vec![
            Entry::Item(hint(Item::new("Close tab", TabCommand::Close(id)).symbol("xmark"), "w", COMMAND)),
            Entry::Item(Item::new("Close other tabs", TabCommand::CloseOthers(id)).symbol("xmark.square").disabled(!self.can_close_others(id))),
            Entry::Item(Item::new("Close tabs to the left", TabCommand::CloseLeft(id)).symbol("arrow.left.to.line").disabled(!self.can_close_left(id))),
            Entry::Item(Item::new("Close tabs to the right", TabCommand::CloseRight(id)).symbol("arrow.right.to.line").disabled(!self.can_close_right(id))),
            Entry::Separator,
            if pinned {
                Entry::Item(Item::new("Unpin tab", TabCommand::Unpin(id)).symbol("pin.slash"))
            } else {
                Entry::Item(Item::new("Pin tab", TabCommand::Pin(id)).symbol("pin"))
            },
            Entry::Separator,
            Entry::Item(Item::new("Move tab to left pane", TabCommand::Move(id, Direction::Left)).symbol("arrow.left.square").disabled(!self.moves[0])),
            Entry::Item(Item::new("Move tab to right pane", TabCommand::Move(id, Direction::Right)).symbol("arrow.right.square").disabled(!self.moves[1])),
            Entry::Item(Item::new("Move tab to upper pane", TabCommand::Move(id, Direction::Up)).symbol("arrow.up.square").disabled(!self.moves[2])),
            Entry::Item(Item::new("Move tab to lower pane", TabCommand::Move(id, Direction::Down)).symbol("arrow.down.square").disabled(!self.moves[3])),
            Entry::Item(Item::new("Move tab to new window", TabCommand::NewWindow(id)).symbol("arrow.up.forward.app").disabled(self.window_tabs < 2)),
            Entry::Separator,
            Entry::Item(hint(Item::new("Split right", TabCommand::Split(id, crate::settings::SplitAxis::Right)).symbol("rectangle.split.2x1").disabled(self.tabs.len() < 2), "\\", COMMAND)),
            Entry::Item(hint(Item::new("Split down", TabCommand::Split(id, crate::settings::SplitAxis::Down)).symbol("rectangle.split.1x2").disabled(self.tabs.len() < 2), "|", COMMAND)),
            Entry::Separator,
            Entry::Item(hint(Item::new("Show in Finder", TabCommand::ShowInFinder(id)).symbol("finder").disabled(!on_disk), "r", COMMAND | OPTION | SHIFT)),
            Entry::Item(Item::new("Reveal in explorer", TabCommand::Reveal(id)).symbol("scope").disabled(!on_disk)),
            Entry::Item(Item::new("Show path", TabCommand::ShowPath(id)).symbol("folder").disabled(!on_disk)),
            Entry::Item(hint(Item::new("Copy path", TabCommand::CopyPath(id)).symbol("document.on.document").disabled(!on_disk), "c", COMMAND | OPTION | SHIFT)),
        ]
    }

    /// Does what a click on a tab menu row does.
    fn run_tab_command(&mut self, command: TabCommand, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            TabCommand::Close(id) => self.close_id(id, window, cx),
            TabCommand::CloseOthers(id) => self.close_others(id, window, cx),
            TabCommand::CloseRight(id) => self.close_right(id, window, cx),
            TabCommand::CloseLeft(id) => self.close_left(id, window, cx),
            TabCommand::Pin(id) => self.pin_id(id, cx),
            TabCommand::Unpin(id) => self.unpin_id(id, cx),
            TabCommand::Move(id, direction) => cx.emit(PaneEvent::MoveTab(id, direction)),
            TabCommand::NewWindow(id) => cx.emit(PaneEvent::NewWindow(id)),
            TabCommand::Split(id, axis) => cx.emit(PaneEvent::Split(id, axis)),
            TabCommand::Reveal(id) => self.reveal_in_explorer(id, cx),
            TabCommand::ShowInFinder(id) => {
                if let Some(path) = self.path_of(id, cx) {
                    native_menu::show_in_finder(&path);
                }
            }
            TabCommand::CopyPath(id) => {
                if let Some(path) = self.path_of(id, cx) {
                    cx.write_to_clipboard(ClipboardItem::new_string(path.to_string_lossy().into_owned()));
                }
            }
            TabCommand::ShowPath(id) => {
                self.show_path_menu(id, window, cx);
            }
        }
    }

    /// Picks the row named `row` (case ignored) of tab `id`'s right-click menu, as a click on it would. An error names a row
    /// that isn't there (listing the ones that are) or is greyed out, which a click can't pick either.
    pub fn pick_tab_menu_row(&mut self, id: EntityId, row: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        self.peek_press(id, cx);
        let items: Vec<Item<TabCommand>> = self.tab_menu_entries(id, cx).into_iter().filter_map(|entry| if let Entry::Item(item) = entry { Some(item) } else { None }).collect();
        let Some(item) = items.iter().find(|item| item.title.eq_ignore_ascii_case(row.trim())) else {
            let names: Vec<&str> = items.iter().map(|item| item.title.as_str()).collect();
            return Err(format!("no such row; rows: {}", names.join(", ")));
        };
        if item.disabled {
            return Err(format!("{} is greyed out", item.title));
        }
        let command = item.run;
        self.run_tab_command(command, window, cx);
        Ok(())
    }

    /// Opens the right-click menu of tab `id` once the frame has drawn: nothing draws while a
    /// native menu is open. Two frames on, since a callback runs at the start of the frame after
    /// the one it was queued in.
    pub fn show_tab_menu(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        self.peek_press(id, cx);
        let entries = self.tab_menu_entries(id, cx);
        let this = cx.weak_entity();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                window
                    .spawn(cx, async move |cx| {
                        let Some(command) = native_menu::pop_up(&entries, None) else { return };
                        this.update_in(cx, |this, window, cx| this.run_tab_command(command, window, cx)).ok();
                    })
                    .detach();
            });
        });
    }

    /// Cmd-click on tab `id`: the file's path as Finder's title menu shows it, under the tab (the file, then each folder up to the
    /// disk, with their icons); a pick reveals it in Finder. An Untitled page has no path: nothing opens and it says so with false.
    pub fn show_path_menu(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(path) = self.path_of(id, cx) else { return false };
        let Some(tab) = self.index_of(id).and_then(|ix| self.bounds.borrow().get(ix).copied()) else { return false };
        let entries = native_menu::path_menu(&path);
        let at = (f32::from(tab.origin.x), f32::from(tab.origin.y + tab.size.height));
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |window, cx| {
                window
                    .spawn(cx, async move |_| {
                        if let Some(target) = native_menu::pop_up(&entries, Some(at)) {
                            native_menu::show_in_finder(&target);
                        }
                    })
                    .detach();
            });
        });
        true
    }

    /// The file tab `id` shows, if it has one.
    pub(crate) fn path_of(&self, id: EntityId, cx: &App) -> Option<PathBuf> {
        self.tabs[self.index_of(id)?].document.read(cx).path.clone()
    }

    /// Asks the window to show tab `id`'s file in the explorer, as its right-click menu does.
    pub fn reveal_in_explorer(&mut self, id: EntityId, cx: &mut Context<Self>) {
        if self.index_of(id).is_some() {
            cx.emit(PaneEvent::Reveal(id));
        }
    }

    /// Takes tab `id` out. Closing the showing tab shows the one used most recently before it,
    /// else the tab on its right (or on its left).
    pub fn remove(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(ix) = self.index_of(id) else { return };
        let was_active = ix == self.active;
        // A draft's last edits reach its file as its tab goes.
        self.tabs[ix].document.update(cx, |document, cx| document.flush_draft(cx));
        let closed = {
            let document = self.tabs[ix].document.read(cx);
            let pinned = self.is_pinned(ix);
            document.path.clone().map(|path| ClosedTab { path, index: ix, caret: document.caret(cx), pinned })
        };
        self.tabs.remove(ix);
        self.pinned = after_remove(ix, self.pinned);
        if let Some(closed) = closed {
            cx.emit(PaneEvent::Closed(closed));
        }
        forget_tab(&mut self.recent, &mut self.dropped, &id);
        self.history.forget(id);
        if ix < self.active {
            self.active -= 1;
        }
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        }
        if was_active && !self.tabs.is_empty() {
            if let Some(back) = self.history.latest().and_then(|id| self.index_of(id)) {
                self.active = back;
            }
            self.show(self.active, window, cx);
        } else {
            // With no editor to hold the keyboard, the pane does, so Cmd-T and Cmd-W still reach the window.
            if self.tabs.is_empty() {
                self.focus.focus(window, cx);
            }
            if self.tabs.is_empty() {
                cx.emit(PaneEvent::Emptied);
            }
            cx.emit(PaneEvent::Changed);
            cx.notify();
        }
    }

    /// The pointer at `pointer` while tab `id` is dragged (`pinned` in the pane it came from), with
    /// `strip` the strip's bounds. One of this pane's own tabs moves its slot to the one under the
    /// pointer; another pane's opens a gap where it would land.
    pub fn drag_over(&mut self, id: EntityId, pinned: bool, strip: Bounds<Pixels>, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(from) = self.index_of(id) else { return self.gap_over(pinned, strip, pointer, cx) };
        let x = pointer.x;
        let to = {
            let bounds = self.bounds.borrow();
            bounds.iter().position(|bounds| x < bounds.right()).unwrap_or(bounds.len().saturating_sub(1))
        };
        // A pinned tab stays among the pins and an unpinned one outside them.
        let to = drag_target(from, to, self.pinned);
        // Dragging a preview tab keeps it.
        self.keep_id(id);
        let mut changed = self.dragging.replace(id) != Some(id);
        if to != from && to < self.tabs.len() {
            // The active tab is the same document wherever it moved.
            let active = self.active_document().map(Entity::entity_id);
            let tab = self.tabs.remove(from);
            self.tabs.insert(to, tab);
            if let Some(ix) = active.and_then(|id| self.index_of(id)) {
                self.active = ix;
            }
            changed = true;
            crate::haptics::align();
        }
        if changed {
            cx.notify();
        }
    }

    /// A drag from elsewhere (another pane's tab, `pinned` there, or files) moved: while the pointer
    /// is over the strip (`strip`), a gap opens where the drop would land, and folds when it leaves.
    pub fn gap_over(&mut self, pinned: bool, strip: Bounds<Pixels>, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let changed = if strip.contains(&pointer) {
            // Where the window's drop puts it: a pinned tab at the end of the pins, anything else
            // at the slot under the pointer but never among them.
            let at = if pinned { self.pinned } else { self.drop_slot(pointer.x).max(self.pinned) };
            let opened = self.motion.open_gap(at.min(self.tabs.len()), !cx.has_active_drag());
            if opened {
                crate::haptics::align();
            }
            opened
        } else {
            self.motion.close_gap()
        };
        if changed {
            cx.notify();
        }
    }

    /// While tab `id` of this pane is dragged with the pointer at `pointer` and the lifted `card` at `card` from the pointer: its slot
    /// folds closed once the pointer is well away from the bar `strip` (or outside the window of size `viewport`), and opens again
    /// when it comes back near.
    pub fn fold_over(&mut self, id: EntityId, card: Bounds<Pixels>, strip: Bounds<Pixels>, pointer: Point<Pixels>, viewport: Size<Pixels>, cx: &mut Context<Self>) {
        if self.index_of(id).is_none() {
            return;
        }
        self.card = Some(card);
        let off = (strip.top() - pointer.y).max(pointer.y - strip.bottom()).max(px(0.));
        let outside = pointer.x < px(0.) || pointer.y < px(0.) || pointer.x > viewport.width || pointer.y > viewport.height;
        let away = outside || off > crate::tokens::TAB_FOLD_AWAY || (self.motion.is_folded() && off > crate::tokens::TAB_FOLD_BACK);
        if self.motion.fold_dragged(away) {
            cx.notify();
        }
    }

    /// The lifted card's place from the pointer in the last drag move of a tab of this pane.
    pub fn card(&self) -> Option<Bounds<Pixels>> {
        self.card
    }

    /// For the control socket's held drag of tab `id` away from the bar: as a real drag far from the bar sets it, the tab is the one
    /// dragged and its slot folds closed. `release_drag` ends it with `end_drag`.
    pub fn hold_away(&mut self, id: EntityId, cx: &mut Context<Self>) {
        if self.index_of(id).is_some() {
            self.dragging = Some(id);
            self.motion.fold_dragged(true);
            cx.notify();
        }
    }

    /// The tab of this pane being dragged now, while one is.
    pub fn dragging(&self) -> Option<EntityId> {
        self.dragging
    }

    /// A tab dragged in from another window was let go in the gap its drag opened: the tab that arrives takes the gap.
    pub fn take_gap(&mut self) {
        self.motion.land_gap();
    }

    /// Folds the gap a drag from another window opened, which left the bar or ended elsewhere.
    pub fn fold_gap(&mut self, cx: &mut Context<Self>) {
        if self.motion.close_gap() {
            cx.notify();
        }
    }

    /// The slot between the tabs the pointer at `x` is at.
    pub fn drop_slot(&self, x: Pixels) -> usize {
        self.bounds.borrow().iter().filter(|bounds| x > bounds.center().x).count()
    }

    /// The drag is over (dropped, or let go outside the bar).
    pub fn end_drag(&mut self, cx: &mut Context<Self>) {
        let unfolded = self.motion.fold_dragged(false);
        if self.dragging.take().is_some() || unfolded {
            cx.notify();
        }
    }

    /// The label and width the lifted-tab ghost of tab `id` draws with (the control socket's `drag-hold`).
    pub fn ghost_look(&self, id: EntityId, cx: &App) -> Option<(TabLabel, Pixels)> {
        let ix = self.index_of(id)?;
        let width = self.bounds.borrow().get(ix).map_or(crate::tokens::TAB_DROP_ROOM, |bounds| bounds.size.width);
        let label = self.motion.label_of(id).cloned().unwrap_or_else(|| {
            let (document, tab) = (self.tabs[ix].document.read(cx), &self.tabs[ix]);
            TabLabel {
                id,
                title: document.title().into(),
                from_line: document.draft_title().is_some(),
                folder: None,
                path: None,
                dirty: document.shows_dot(),
                conflict: document.in_conflict(),
                gone: document.is_gone(),
                locked: document.is_read_only(),
                preview: tab.preview,
                pinned: ix < self.pinned,
                saving: document.slow_save(),
            }
        });
        Some((label, width))
    }

    /// Where tab `id` is drawn in the window now, for a copy of it to carry across when it moves to another pane.
    pub fn tab_bounds(&self, id: EntityId) -> Option<Bounds<Pixels>> {
        let ix = self.index_of(id)?;
        self.bounds.borrow().get(ix).copied()
    }

    /// What a copy of tab `id` carries across to another pane starts from: its place, how far it has lifted and its
    /// speed. A copy still gliding home from an earlier carry starts from where it is drawn, at the pace it moves;
    /// a tab at rest starts from its slot.
    pub fn carry_start(&self, id: EntityId) -> Option<(Bounds<Pixels>, f32, [f32; 2])> {
        let slot = self.tab_bounds(id)?;
        Some(self.motion.chip(id, slot).unwrap_or((slot, 0., [0.; 2])))
    }

    /// Tab `id` was let go in this pane's bar at `now`, its ghost at `from` and `lift` of the way up:
    /// a copy of the ghost glides home into its slot. A tab from another pane isn't in the strip
    /// yet: its copy waits for the slot the window's move makes, then glides from where it was let go.
    pub fn land(&mut self, id: EntityId, from: Bounds<Pixels>, lift: f32, now: Instant, cx: &mut Context<Self>) {
        let waiting = self.index_of(id).is_none();
        self.motion.land(id, from, lift, [0.; 2], now, waiting);
        cx.notify();
    }

    /// Tab `id` was let go on a pane's edge or middle, its ghost at `from` and `lift` of the way up:
    /// the window just moved it here, so its slot isn't laid out yet. A copy of the ghost waits for
    /// the slot, then glides from where it was let go.
    pub fn land_after_drop(&mut self, id: EntityId, from: Bounds<Pixels>, lift: f32, speed: [f32; 2], now: Instant, cx: &mut Context<Self>) {
        self.motion.land(id, from, lift, speed, now, true);
        cx.notify();
    }

    /// Something was dropped on the bar with the pointer at `x`. One of this pane's own tabs was
    /// reordered as it moved, so only the drag ends; anything else goes to the window, with the
    /// slot between the tabs the pointer is at.
    pub fn drop_on_bar(&mut self, dropped: Dropped, x: Pixels, cx: &mut Context<Self>) {
        if let Dropped::Tab(id) = dropped {
            if self.index_of(id).is_some() {
                self.end_drag(cx);
                return;
            }
        }
        // The tab that arrives takes the gap the drag opened.
        self.motion.land_gap();
        cx.emit(PaneEvent::Dropped(dropped, Zone::Bar(self.drop_slot(x))));
    }
}

/// The home folder, read once: the tab and explorer row tips write paths under it with `~`.
pub(crate) fn home_folder() -> Option<&'static Path> {
    static HOME: std::sync::LazyLock<Option<PathBuf>> = std::sync::LazyLock::new(std::env::home_dir);
    HOME.as_deref()
}

impl Pane {
    /// Whether the bar already shows every tab as it is now, so a frame that only resizes builds no labels.
    fn tabs_shown(&self, cx: &App) -> bool {
        if self.leaving {
            return false;
        }
        let home = home_folder();
        let active = self.tabs.get(self.active).map(|tab| tab.document.entity_id());
        self.motion.unchanged(active, self.tabs.len(), |ix, label| {
            let tab = &self.tabs[ix];
            let document = tab.document.read(cx);
            let title_is = match document.path.as_deref().and_then(|path| path.file_name()) {
                Some(name) => label.title.as_ref() == name.to_string_lossy(),
                None => match document.draft_title() {
                    Some(line) => label.title.as_ref() == line,
                    None => tab_bar::is_untitled_title(&label.title, self.untitled.get(&label.id).copied().unwrap_or(1)),
                },
            };
            let path_is = match (document.path.as_deref(), label.path.as_ref()) {
                (Some(path), Some(shown)) => tab_bar::is_tip_path(shown, path, home),
                (None, None) => true,
                _ => false,
            };
            let folder_is = label.folder.as_ref().is_none_or(|shown| document.path.as_deref().and_then(Path::parent).and_then(Path::file_name).is_some_and(|folder| shown.as_ref() == folder.to_string_lossy()));
            tab.document.entity_id() == label.id
                && title_is
                && path_is
                && label.from_line == (document.path.is_none() && document.draft_title().is_some())
                && folder_is
                && label.dirty == document.shows_dot()
                && label.conflict == document.in_conflict()
                && label.gone == document.is_gone()
                && label.locked == document.is_read_only()
                && label.preview == tab.preview
                && label.pinned == (ix < self.pinned)
                && label.saving == document.slow_save()
        })
    }

    /// Each tab's label as the bar should show it now, reusing the strings already shown.
    fn build_labels(&mut self, cx: &mut Context<Self>) -> Vec<TabLabel> {
        let pinned = self.pinned;
        let motion = &self.motion;
        let mut slot_ix = std::mem::take(&mut self.slot_ix);
        motion.index_slots(&mut slot_ix);
        let mut labels: Vec<TabLabel> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let document = tab.document.read(cx);
                let id = tab.document.entity_id();
                // The title is the file's name (as `document::title` makes it), or "Untitled" and the page's number; each is built again only when it changed.
                let shown_label = slot_ix.get(&id).map(|&ix| motion.label_at(ix));
                let shown_title = shown_label.map(|label| &label.title);
                let title = match document.path.as_deref().and_then(|path| path.file_name()) {
                    Some(name) => {
                        let name = name.to_string_lossy();
                        match shown_title {
                            Some(shown) if shown.as_ref() == name => shown.clone(),
                            _ => SharedString::from(name.into_owned()),
                        }
                    }
                    None => match document.draft_title() {
                        // An Untitled page whose first line has settled shows it.
                        Some(line) => match shown_title {
                            Some(shown) if shown.as_ref() == line => shown.clone(),
                            _ => SharedString::from(line.to_owned()),
                        },
                        None => {
                            let number = self.untitled.get(&id).copied().unwrap_or(1);
                            match shown_title {
                                Some(shown) if tab_bar::is_untitled_title(shown, number) => shown.clone(),
                                _ => SharedString::from(tab_bar::untitled_title(number)),
                            }
                        }
                    },
                };
                // The tab's tip names the file by its full path, built again only when it changed.
                let path = document.path.as_deref().map(|path| {
                    let home = home_folder();
                    match shown_label.and_then(|label| label.path.as_ref()) {
                        Some(shown) if tab_bar::is_tip_path(shown, path, home) => shown.clone(),
                        _ => SharedString::from(tab_bar::tip_path(path, home)),
                    }
                });
                // A draft is always on disk, so it shows no dot unless its last write failed.
                TabLabel {
                    id,
                    title,
                    from_line: document.path.is_none() && document.draft_title().is_some(),
                    folder: None,
                    path,
                    dirty: document.shows_dot(),
                    conflict: document.in_conflict(),
                    gone: document.is_gone(),
                    locked: document.is_read_only(),
                    preview: tab.preview,
                    pinned: ix < pinned,
                    saving: document.slow_save(),
                }
            })
            .collect();
        // Files that share a name show the folder they are in after it, as the unsaved-changes alert does.
        if labels.len() > 1 {
            let twins = twin_flags(labels.iter().map(|label| &label.title));
            for ((label, tab), twin) in labels.iter_mut().zip(&self.tabs).zip(twins) {
                if !twin {
                    continue;
                }
                let Some(folder) = tab.document.read(cx).path.as_deref().and_then(Path::parent).and_then(Path::file_name) else { continue };
                let folder = folder.to_string_lossy();
                label.folder = Some(match slot_ix.get(&label.id).and_then(|&ix| motion.label_at(ix).folder.as_ref()) {
                    Some(shown) if shown.as_ref() == folder => shown.clone(),
                    _ => SharedString::from(folder.into_owned()),
                });
            }
        }
        self.slot_ix = slot_ix;
        // A pane that is closing keeps its bar as it was; one that has just its last tab remembers it.
        if self.leaving {
            self.last_labels.clone()
        } else {
            if labels.len() == 1 {
                self.last_labels = labels.clone();
            }
            labels
        }
    }
}

/// Which of `titles` another one repeats, in order: the tabs that show their folder (the strip) or put it in parentheses (the tab list).
fn twin_flags<'a, T: std::hash::Hash + Eq + 'a>(titles: impl Iterator<Item = &'a T> + Clone) -> Vec<bool> {
    let mut names: HashMap<&T, usize> = HashMap::new();
    for title in titles.clone() {
        *names.entry(title).or_default() += 1;
    }
    titles.map(|title| names[title] > 1).collect()
}

impl Render for Pane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        tab_bar::number_untitled(&mut self.untitled, self.tabs.iter().filter(|tab| tab.document.read(cx).path.is_none()).map(|tab| tab.document.entity_id()));
        // A frame with every tab as the bar shows it (a resize, a blink) builds no labels; the bar's own width is carried to this frame's.
        if !self.tabs_shown(cx) {
            let labels = self.build_labels(cx);
            self.motion.sync(labels, self.active);
        }
        self.motion.measure(window, cx);
        self.motion.fit(f32::from(window.viewport_size().width));
        let hairline = cx.theme().border;
        // One device pixel, laid on the grid: the top band is whole points, so its bottom edge is.
        let rule = px(1. / window.scale_factor());
        // A drag that ended anywhere but this strip folds the gap it opened here (a held `drag-hold` has no GPUI drag; its release folds it).
        if !cx.has_active_drag() && !self.motion.gap_staged() {
            self.motion.close_gap();
        }
        // The tabs move on this view's frames alone, and only while something moves.
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        let mut moving = self.motion.advance(now, self.dragging);
        // A jump to a tab glides the strip instead of snapping it; a tab already in view stays put.
        let offset = self.scroll.offset();
        if let Some(to) = self.motion.take_reveal(-f32::from(offset.x)) {
            self.glide.aim(-f32::from(offset.x), to, now);
            self.glide_to = Some(to);
        } else {
            self.glide.settle();
        }
        if let Some(at) = self.glide.step(now) {
            self.scroll.set_offset(point(px(-at), offset.y));
        }
        moving |= self.glide.is_moving();
        if !self.glide.is_moving() {
            self.glide_to = None;
        }
        let settled = self.glide_to.unwrap_or(-f32::from(self.scroll.offset().x));
        moving |= self.motion.advance_badge(settled, now);
        let over = self.motion.peek_id().and_then(|id| self.tab_bounds(id));
        moving |= self.motion.advance_peek(now, over, window.viewport_size().width);
        if moving {
            window.request_animation_frame();
        }
        // The corner pane's bar leaves room for the traffic lights and the explorer button the
        // explorer doesn't cover; the workspace eases that room in and out of full screen.
        let inset = if self.corner { (self.clear - self.left_edge).max(TAB_BAR_INSET) } else { TAB_BAR_INSET };
        // The mark is lit while this pane has the keyboard in the key window, and eases dim otherwise.
        let built = self.active_document().and_then(|document| document.read(cx).built_editor().cloned());
        let handle = match built {
            Some(editor) => editor.read(cx).focus_handle(cx),
            None => self.focus.clone(),
        };
        self.watch_focus(&handle, window, cx);
        let key = crate::control::window_key(window) && handle.contains_focused(window, cx);
        if self.ease_key(key, crate::motion::drawn_now(window.window_handle().window_id())) {
            window.request_animation_frame();
        }
        let bar = tab_bar::render(&self.motion, self.key_level.unwrap_or(1.), self.right, inset, self.band, &self.scroll, &self.bounds, cx);
        // A refusal shakes the bar's container sideways and asks for frames only while it runs.
        let swing = self.shake.and_then(|shake| shake.offset(now));
        let bar = if let Some(swing) = swing {
            window.request_animation_frame();
            div().absolute().top_0().left(swing).w_full().h(self.band).child(bar).into_any_element()
        } else {
            self.shake = None;
            bar.into_any_element()
        };
        // The band's hairline belongs to a pane with tabs; an empty pane's message is the window's to draw.
        let hairline = (!self.tabs.is_empty() || self.leaving).then(|| div().absolute().top(self.band - rule).left_0().right_0().h(rule).bg(hairline));
        div()
            .relative()
            .track_focus(&self.focus)
            // A press on the bar (the preview button under it, or the bar between tabs) leaves the
            // keyboard where it is; the tabs and the workspace move it themselves.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .size_full()
            .child(bar)
            // Over the bar, whose band is painted on a light theme.
            .children(hairline)
    }
}

/// Up to `PEEK_LINES` lines of `document` around its caret and the row the caret is on among them, read from the built editor's rope:
/// only those lines, each cut at `PEEK_COLUMNS`. A document not built has no rope to read yet, so no lines.
fn peek_lines(document: &Document, cx: &App) -> (Vec<SharedString>, usize) {
    let Some(editor) = document.built_editor() else { return (Vec::new(), 0) };
    let state = editor.read(cx);
    let (rope, at) = (state.text(), state.cursor_position().line as usize);
    let total = rope.lines_len();
    let first = at.saturating_sub(PEEK_LINES / 2).min(total.saturating_sub(PEEK_LINES));
    let lines = (first..(first + PEEK_LINES).min(total))
        .map(|row| {
            let line: String = rope.slice_line(row).chars().filter(|c| !matches!(c, '\n' | '\r')).take(PEEK_COLUMNS).collect();
            SharedString::from(line.replace('\t', "    "))
        })
        .collect();
    (lines, at.saturating_sub(first))
}

/// Where a tab at `ix` goes when pinned, with `pinned` tabs pinned before: the end of the pins.
/// Returns the slot and the new pinned count.
fn pin_move(ix: usize, pinned: usize) -> (usize, usize) {
    debug_assert!(ix >= pinned);
    (pinned, pinned + 1)
}

/// Where a pinned tab at `ix` goes when unpinned, with `pinned` tabs pinned: just after the pins
/// that stay. Returns the slot and the new pinned count.
fn unpin_move(ix: usize, pinned: usize) -> (usize, usize) {
    debug_assert!(ix < pinned);
    (pinned - 1, pinned - 1)
}

/// The slot a dragged tab may take: a pinned tab (`from` among the first `pinned`) stays among
/// them, an unpinned one stays after them. `to` is the slot under the pointer.
fn drag_target(from: usize, to: usize, pinned: usize) -> usize {
    if from < pinned { to.min(pinned - 1) } else { to.max(pinned) }
}

/// Drops a closed tab's id from the shown-last list and the let-go-highlighter set.
fn forget_tab<T: PartialEq + Eq + std::hash::Hash>(recent: &mut Vec<T>, dropped: &mut HashSet<T>, id: &T) {
    recent.retain(|kept| kept != id);
    dropped.remove(id);
}

/// The pinned count once the tab at `ix` is gone.
fn after_remove(ix: usize, pinned: usize) -> usize {
    if ix < pinned { pinned - 1 } else { pinned }
}

/// The order tabs were used in, most recent first. It holds every open tab: ones never shown sit
/// at the back in the order they came.
#[derive(Debug)]
struct History<T> {
    order: Vec<T>,
}

impl<T> Default for History<T> {
    fn default() -> Self {
        Self { order: Vec::new() }
    }
}

impl<T: Copy + PartialEq> History<T> {
    /// `id` was used: it goes to the front.
    fn touch(&mut self, id: T) {
        self.order.retain(|other| *other != id);
        self.order.insert(0, id);
    }

    /// A tab never used joins at the back, unless it is already listed.
    fn append(&mut self, id: T) {
        if !self.order.contains(&id) {
            self.order.push(id);
        }
    }

    /// `id` is gone (closed, or moved to another pane).
    fn forget(&mut self, id: T) {
        self.order.retain(|other| *other != id);
    }

    /// The tab used last.
    fn latest(&self) -> Option<T> {
        self.order.first().copied()
    }

    /// The tab `step` places further back than `from` (forward for a negative step), wrapping
    /// around. `None` with fewer than two tabs.
    fn step_from(&self, from: T, step: isize) -> Option<T> {
        let count = self.order.len() as isize;
        if count < 2 {
            return None;
        }
        let at = self.order.iter().position(|id| *id == from).unwrap_or(0) as isize;
        Some(self.order[(at + step).rem_euclid(count) as usize])
    }
}

/// What a tab's highlighter needs after a switch.
enum Colours {
    Keep,
    /// A dropped tab among the recent ones gets its language back.
    Restore(&'static str),
    /// A big tab left behind with a grammar loses it.
    Drop,
}

/// `lang` runs only when the answer needs the language: a recent tab that was dropped, or a
/// tab out of `recent` that is big enough to drop. Every other tab costs nothing.
fn colour_change(recent: bool, dropped: bool, big: impl FnOnce() -> bool, lang: impl FnOnce() -> &'static str) -> Colours {
    if recent {
        if dropped { Colours::Restore(lang()) } else { Colours::Keep }
    } else if !dropped && big() && lang() != "text" {
        Colours::Drop
    } else {
        Colours::Keep
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::collections::HashSet;

    use super::{Colours, KEPT_HIGHLIGHTED, after_remove, colour_change, drag_target, forget_tab, pin_move, unpin_move, History};

    /// A switch among 200 small text tabs asks no tab its language; a big one asks only itself.
    #[test]
    fn a_switch_among_text_tabs_asks_few_languages() {
        let asked = Cell::new(0);
        let lang = || {
            asked.set(asked.get() + 1);
            "text"
        };
        for tab in 0..200 {
            let recent = tab < KEPT_HIGHLIGHTED;
            assert!(matches!(colour_change(recent, false, || false, lang), Colours::Keep));
        }
        assert!(asked.get() <= 2, "asked {}", asked.get());
        // A big text tab out of `recent` is asked once, and stays as it is.
        assert!(matches!(colour_change(false, false, || true, lang), Colours::Keep));
        assert_eq!(asked.get(), 1);
        // A big Rust tab out of `recent` drops its grammar; a recent dropped one gets it back.
        assert!(matches!(colour_change(false, false, || true, || "rust"), Colours::Drop));
        assert!(matches!(colour_change(true, true, || false, || "rust"), Colours::Restore("rust")));
        assert!(matches!(colour_change(false, true, || true, || "rust"), Colours::Keep));
    }

    #[test]
    fn closed_tabs_leave_no_id_in_recent_or_dropped() {
        let (mut recent, mut dropped) = (Vec::new(), HashSet::new());
        for id in 0..50u32 {
            recent.insert(0, id);
            dropped.insert(id);
            forget_tab(&mut recent, &mut dropped, &id);
        }
        assert!(recent.is_empty() && dropped.is_empty());
    }

    /// Applies a move of the tab at `from` to slot `to` to a plain list.
    fn moved(tabs: &mut Vec<char>, from: usize, to: usize) {
        let tab = tabs.remove(from);
        tabs.insert(to, tab);
    }

    #[test]
    fn pinning_goes_to_the_end_of_the_pins() {
        let mut tabs = vec!['a', 'b', 'c', 'd'];
        let (to, pinned) = pin_move(2, 0);
        moved(&mut tabs, 2, to);
        assert_eq!((tabs.clone(), pinned), (vec!['c', 'a', 'b', 'd'], 1));
        let (to, pinned) = pin_move(3, pinned);
        moved(&mut tabs, 3, to);
        assert_eq!((tabs, pinned), (vec!['c', 'd', 'a', 'b'], 2));
    }

    #[test]
    fn unpinning_goes_just_after_the_pins() {
        let mut tabs = vec!['c', 'd', 'e', 'a', 'b'];
        let (to, pinned) = unpin_move(0, 3);
        moved(&mut tabs, 0, to);
        assert_eq!((tabs.clone(), pinned), (vec!['d', 'e', 'c', 'a', 'b'], 2));
        let (to, pinned) = unpin_move(1, pinned);
        moved(&mut tabs, 1, to);
        assert_eq!((tabs, pinned), (vec!['d', 'e', 'c', 'a', 'b'], 1));
    }

    #[test]
    fn the_last_pin_unpins_in_place() {
        assert_eq!(unpin_move(0, 1), (0, 0));
    }

    #[test]
    fn a_drag_stays_on_its_side_of_the_pins() {
        // Two pins: slots 0 and 1.
        assert_eq!(drag_target(0, 4, 2), 1);
        assert_eq!(drag_target(1, 0, 2), 0);
        assert_eq!(drag_target(3, 0, 2), 2);
        assert_eq!(drag_target(2, 5, 2), 5);
        // No pins: nothing to clamp.
        assert_eq!(drag_target(2, 0, 0), 0);
    }

    #[test]
    fn removing_a_pin_shrinks_the_prefix() {
        assert_eq!(after_remove(0, 2), 1);
        assert_eq!(after_remove(1, 2), 1);
        assert_eq!(after_remove(2, 2), 2);
        assert_eq!(after_remove(0, 0), 0);
    }

    fn history(ids: &[u32]) -> History<u32> {
        let mut history = History::default();
        for id in ids.iter().rev() {
            history.touch(*id);
        }
        history
    }

    #[test]
    fn touch_moves_to_front() {
        let mut h = history(&[1, 2, 3]);
        h.touch(3);
        assert_eq!(h.order, [3, 1, 2]);
    }

    #[test]
    fn closing_returns_to_the_last_used() {
        let mut h = history(&[1, 2, 3]);
        h.touch(2);
        h.forget(2);
        assert_eq!(h.latest(), Some(1));
    }

    #[test]
    fn append_goes_to_the_back_once() {
        let mut h = history(&[1, 2]);
        h.append(3);
        h.append(3);
        assert_eq!(h.order, [1, 2, 3]);
    }

    #[test]
    fn walk_goes_back_without_reordering() {
        let h = history(&[1, 2, 3, 4]);
        let first = h.step_from(1, 1).unwrap();
        assert_eq!(first, 2);
        assert_eq!(h.step_from(first, 1), Some(3));
        assert_eq!(h.order, [1, 2, 3, 4]);
    }

    #[test]
    fn walk_wraps_both_ways() {
        let h = history(&[1, 2, 3]);
        assert_eq!(h.step_from(3, 1), Some(1));
        assert_eq!(h.step_from(1, -1), Some(3));
    }

    #[test]
    fn one_tab_has_nowhere_to_walk() {
        assert_eq!(history(&[1]).step_from(1, 1), None);
    }
}
