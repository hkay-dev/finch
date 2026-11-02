//! The window's root view: the folder tree on the left, the editor and an
//! optional right panel in one resizable row.
//!
//! The explorer is built from the window's `Start` and opens files here. The
//! right panel holds the markdown preview: it shows while Cmd-Shift-V has it
//! on and the showing document is markdown, drawn from that document's warm
//! parsed state. Cmd-B hides the tree. Each side panel's width is kept in the
//! settings file.

use std::{
    cell::Cell,
    collections::{HashMap, HashSet},
    future::Future,
    path::{Path, PathBuf},
    rc::Rc,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant, SystemTime},
};

use gpui_kit::base::{ResizableState, ResizablePanelEvent, ResizeHandleContext, ResizeHandleState, Root, h_resizable, resizable_panel};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, input::{EditorState, RopeExt as _}};
use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::{
    app::{
        CloseOtherTabs, CloseTabsToTheRight, CloseTabsToTheLeft, CloseTab, CloseWindow, FocusPaneDown, FocusPaneLeft, FocusPaneRight, FocusPaneUp, Minimize, MoveTabDown, MoveTabLeft, MoveTabRight, MoveTabToNewWindow, MoveTabUp, NewTab, NextRecentTab, NextTab, Open, PrevRecentTab, PrevTab,
        FocusNextPane, FocusPreviousPane, EvenOutPanes, MergePanes, MergeAllWindows, MoveTabEarlier, MoveTabLater,
        ReopenClosedTab, RevertToSaved, Save, SaveAll, SaveAs, ShowFindInFolder, ShowGoToLine, ShowGoToSymbol, ShowOpenQuickly, ShowTab1, TogglePinTab, NextChange, PreviousChange,
        ShowTab2, ShowTab3, ShowTab4, ShowTab5, ShowTab6, ShowTab7, ShowTab8, ShowTab9, SplitDown, SplitRight, ToggleExplorer, TogglePreview,
        ToggleMinimal, ToggleSoftWrap, CycleWrapWidth, Zoom,
    },
    control,
    document::{Document, Read, path_mtime, probe, title},
    encoding::{self, Decoded, GrowBase},
    drafts,
    drag::{DraggedFiles, Dropped, Zone},
    find_float::{FindFloat, Step},
    find_in_folder::{FIND_IN_FOLDER_WIDTH, FindInFolder, FindInFolderEvent},
    draft_list::{DraftList, DraftListEvent},
    change_bars::ChangeBars,
    go_to_line::{GoToLine, GoToLineEvent},
    go_to_symbol::{GoToSymbol, GoToSymbolEvent},
    open_quickly::{OpenQuickly, OpenQuicklyEvent},
    versions_list::{VersionsList, VersionsListEvent},
    undo_slider::{self, Close, UndoSlider, UndoSliderEvent},
    rename_field::{RENAME_WIDTH, RenameEvent, RenameField},
    empty_state, file_ops,
    explorer::{CollapseAllFolders, EnclosingFolder, Explorer, ExplorerEvent, FilesChanged, FocusExplorer, RevealCurrentFile, SortExplorer, ToggleHiddenFiles, ToggleIgnoredFiles},
    hover::{Ink, by_lit, icon_button, lit_level},
    menu,
    motion::{self, Fade, Slide},
    pane::{ClosedTab, Pane, PaneEvent},
    panes::{EditorPane, Landed, Node, PaneId, PreviewPane, Split, Tree},
    preview::{EditorLink, render::{OpenFile, ToggleTask}},
    settings::{self, SavedPanes, SplitAxis, WindowState},
    start::{Caret, Start},
    tab_bar::{self, TabDrag, mix},
    templates::{self, NewFromTemplate},
    log_follow::{FOLLOW_TICK, FollowChip, Pace},
    scratch, theme, tooltip, window_look,
};

use crate::tokens::{
    ALPHA_DROP, ALPHA_SECONDARY, ALPHA_SELECTED, ALPHA_SELECTED_UNFOCUSED, ALPHA_STATUS_DIM, DIVIDER_GRAB, DIVIDER_GRAB_LEAD, EDITOR_MIN, EMPTY_COMPACT_HEIGHT, FADE_QUICK,
    PANE_DROP_EDGE, PANE_DROP_INSET, PANE_DROP_RADIUS, PANE_EDGE, PANE_EDGE_ALPHA, PANE_EDGE_ALPHA_LIGHT, PANE_MIN, PANE_WASH,
    ALPHA_BUTTON_PRESSED, ALPHA_PRIMARY, EXPLORER_BUTTON_LEFT, FIND_INSET_RIGHT, FIND_INSET_TOP, GO_TO_LINE_WIDTH, ICON_BUTTON, ICON_BUTTON_INSET, ICON_BUTTON_SYMBOL, OPEN_QUICKLY_WIDTH, PANE_BAND, PANEL_WIDTH_MAX, PANEL_WIDTH_MIN, PREVIEW_INSET, RADIUS_BUTTON, TAB_BAR_INSET, TAB_LIGHTS_CLEAR, TAB_MIN_WIDTH, TOP_BAND,
    MINIMAL_EDGE, MINIMAL_ESCAPE, MINIMAL_TOP,
};

actions!(gpui_editor, [UseSelectionForFind, UseSelectionForReplace, RenameFile, DuplicateFile, MoveFile, ShowFileInFinder, CopyFilePath, ExportHtml, ShowDraftList, BrowseVersions, ShowUndoHistory, NewFileInExplorer, NewFolderInExplorer, MoveItemHere]);

/// How many closed tabs Cmd-Shift-T can bring back.
const CLOSED_KEPT: usize = 20;

/// One open file a batch of disk changes reaches: what the background read needs of it.
struct Reload {
    document: Entity<Document>,
    path: PathBuf,
    unsaved: bool,
    saved: Option<SystemTime>,
    encoding: &'static encoding_rs::Encoding,
    epoch: u64,
    /// Set when the file may have only grown: the text as read last, to compare the file's bytes with.
    base: Option<GrowBase>,
    /// The batch names this file itself, not only a folder above it.
    exact: bool,
    /// The tab never showed: its file is read when it does.
    unread: bool,
}

/// One pane of the window: its tab bar, its editor view, and what they leave for the window.
struct PaneSlot {
    id: PaneId,
    /// The tabs.
    pane: Entity<Pane>,
    /// The editor of the showing tab, with the preview button over it; drawn while the pane has a tab.
    editor: Entity<EditorPane>,
    /// What the editor view leaves for the preview pane and the input handler.
    link: Rc<EditorLink>,
    /// The showing editor's focus handle and state, for the window's input handler; `None`
    /// while the pane has no tab.
    input: Option<(FocusHandle, Entity<EditorState>)>,
    /// The change bars over the editor: what differs from the last commit (`change_bars.rs`).
    bars: Entity<ChangeBars>,
    /// The pane is closing after its last tab went: it keeps drawing the editor it had.
    holding: bool,
    /// Where the pane was drawn in the last frame.
    bounds: Rc<Cell<Bounds<Pixels>>>,
    /// The empty state's symbol and line: 1 while the pane is tall enough for them, fading to 0 as it gets short.
    fit: Cell<motion::Fade>,
    _subscription: Subscription,
}

/// One window's Minimal mode (View > Minimal mode): the tab bar, tree, status line, preview and window buttons ease away on one
/// level (`hide`), and the top edge brings the bar and buttons back while the pointer is in it (`reveal`). The window's
/// own state, not a setting: only the on or off flag is saved, once, when it flips.
struct Minimal {
    on: bool,
    /// The preview was asked for with Cmd-Shift-V while minimal (it stays hidden otherwise).
    preview: bool,
    /// The pointer is in the top edge strip, or in the band it brought back.
    peek: bool,
    /// When the last lone Escape was pressed, for the second one.
    escape: Option<Instant>,
    /// 1 while minimal, 0 otherwise, easing on the column slide's length.
    hide: Fade,
    /// 1 while the top band is brought back.
    reveal: Fade,
    /// The levels as of the frame being drawn, for the bars, buttons, band and status line.
    levels: (f32, f32),
    /// Whether the window buttons were last told to show, so AppKit hears it only when it flips.
    buttons: Option<bool>,
}

impl Minimal {
    fn at(on: bool) -> Self {
        let level = f32::from(on);
        Self {
            on,
            preview: false,
            peek: false,
            escape: None,
            hide: Fade::with(level, crate::tokens::COLUMN_SLIDE),
            reveal: Fade::with(0., crate::tokens::COLUMN_SLIDE),
            levels: (level, 0.),
            buttons: None,
        }
    }

    /// How much of the chrome shows: it eases away with `hide` and comes back with `reveal`.
    fn chrome(&self) -> f32 {
        (1. - self.levels.0).max(self.levels.1)
    }
}

/// Where a pane is looked for from another: Cmd-K and an arrow.
#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Left,
    Right,
    Up,
    Down,
}

impl Direction {
    /// Every direction, in the order the menus list them and `Pane::moves` holds them.
    pub(crate) const ALL: [Direction; 4] = [Direction::Left, Direction::Right, Direction::Up, Direction::Down];
}

/// A divider being dragged: which split it belongs to, and how far the pointer was from the line
/// when it was pressed, so the line stays under it.
#[derive(Clone, Copy)]
struct Drag {
    split: usize,
    offset: f32,
}

pub struct Workspace {
    /// Every pane in the window, in no order (the tree has the order).
    panes: Vec<PaneSlot>,
    /// How the panes are split.
    tree: Tree,
    /// The window title and edited dot last handed to the system, so `sync` sends them when they change (finch#vtz3).
    sent: (Option<String>, Option<bool>),
    /// The pane that has the keyboard; the preview, the title and the Cmd-key actions follow it.
    active: PaneId,
    /// The name the next pane gets.
    next_pane: PaneId,
    drag: Option<Drag>,
    /// A frame's settling of which panes lie beside which is queued: one callback for all the panes.
    moves_pending: Rc<Cell<bool>>,
    /// The pointer's place along the held divider's axis in its last moves, newest last, for the
    /// speed a stretch springs back with.
    trail: Vec<(Instant, f32)>,
    /// The split whose divider the pointer is over, which lights its line.
    divider_hover: Option<usize>,
    explorer: Entity<Explorer>,
    /// The preview of the showing document.
    preview_pane: Entity<PreviewPane>,
    /// What the active editor view leaves for the preview pane and the input handler.
    link: Rc<EditorLink>,
    /// The active pane's showing editor's focus handle and state, for the window's input handler;
    /// `None` while that pane has no tab.
    input: Option<(FocusHandle, Entity<EditorState>)>,
    /// The showing document is markdown, so the preview can show.
    has_preview: bool,
    /// The document the window last followed, to tell when another tab came forward.
    shown: (Option<EntityId>, Option<PathBuf>),
    /// The sidebar column's width and the theme as AppKit was last told.
    material: Option<(f32, Option<bool>)>,
    /// The room the corner pane's bar leaves on its left as last drawn (the traffic lights, less the explorer's width), which a divider drag keeps its tabs clear of.
    corner_inset: f32,
    /// The explorer and the preview, each sliding open or shut.
    left: Slide,
    right: Slide,
    /// The display the window is on and whether it is full screen, as last looked up: a resize
    /// reads these instead of asking the system again.
    display: Option<String>,
    full_screen: bool,
    /// 1 in full screen, 0 windowed, easing between on the explorer column's length: the explorer button and the corner pane's tab inset follow it, so the traffic lights' room closes and opens as one move.
    chrome: Fade,
    /// Minimal mode: the chrome's one transient state.
    minimal: Minimal,
    /// The first frame has set the columns at rest.
    slides_started: bool,
    /// The panes' focus light and the window's key level, easing.
    light: Light,
    /// Each pane's band height, easing between `TOP_BAND` and `PANE_BAND` as a pane opens above or closes, and the frame time it was read at.
    bands: Vec<(PaneId, Fade)>,
    band_now: Instant,
    /// The zone a drag is over, while one is, and the preview that glides to it and fades.
    drop_target: Option<(PaneId, Zone)>,
    /// Whether the explorer's and the preview's edges were at rest or least width in the last frame, and the window's width then.
    edge_pinned: ([bool; 2], f32),
    drop_preview: motion::Morph,
    /// A drag the control socket holds over a zone (`drag-hold`), drawn as a real drag would be.
    held: Option<Held>,
    /// The Go to line field, made at the first Cmd-L and kept (shut or leaving, it draws nothing).
    go_to_line: Option<(Entity<GoToLine>, Subscription)>,
    /// The Open Quickly float, made at the first Shift-Cmd-O and kept (shut or leaving, it draws nothing).
    open_quickly: Option<(Entity<OpenQuickly>, Subscription)>,
    find_in_folder: Option<(Entity<FindInFolder>, Subscription)>,
    /// The Go to symbol field, made at the first Cmd-Ctrl-J and kept (shut or leaving, it draws nothing).
    go_to_symbol: Option<(Entity<GoToSymbol>, Subscription)>,
    /// The File > Open draft float, made at the first opening and kept (shut or leaving, it draws nothing).
    draft_list: Option<(Entity<DraftList>, Subscription)>,
    /// The File > Browse versions picker, made at each opening and dropped at a pick or Escape.
    versions_list: Option<(Entity<VersionsList>, Subscription, Entity<Document>)>,
    /// The Edit > Undo history slider, made at each opening and dropped when it closes.
    undo_slider: Option<(Entity<UndoSlider>, Subscription, Entity<Document>)>,
    /// The File > Rename field, made at the first Rename and kept (shut or leaving, it draws nothing).
    rename_field: Option<(Entity<RenameField>, Subscription)>,
    /// The closed-bar Find next count float, made at the first step and kept (shut, it draws nothing).
    find_float: Option<Entity<FindFloat>>,
    /// The "Following" chip over the shown editor's top right (it draws nothing until the file is followed).
    follow_chip: Entity<FollowChip>,
    /// How each growing file's reloads are paced (`reload_changed`), by path.
    paces: HashMap<PathBuf, Pace>,
    /// The number of the newest `reload_changed` batch, and the newest one whose read landed for each
    /// document, so a slower older read can't land over a newer one.
    read_seq: u64,
    read_applied: HashMap<EntityId, u64>,
    layout: Entity<ResizableState>,
    /// Draws a tooltip fading out and watches the pointer for the tips.
    tooltips: Entity<tooltip::Layer>,
    /// Where the window started; the explorer is rooted at its root.
    start: Start,
    /// The last files closed, the newest last, with the pane each was in.
    closed: Vec<(PaneId, ClosedTab)>,
    /// This window, so reading the other windows can leave it out.
    window: AnyWindowHandle,
    /// The scratch window (`scratch.rs`): it stands in no saved layout, so it writes no window
    /// geometry, panel widths, open tabs or folder tree state to the settings, and shows no panels.
    scratch: bool,
    /// What View > Focus lights in this window (not saved): each editor shown here follows it.
    focus: crate::focus::Mode,
    _subscriptions: [Subscription; 8],
}

impl Workspace {
    pub fn new(start: Start, text: Option<Decoded>, mtime: Option<SystemTime>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let soft_wrap = settings::get(cx).editor.soft_wrap;
        let file = start.file.clone();
        let document = cx.new(|cx| match text {
            Some(text) => Document::new(file, text, mtime, soft_wrap, window, cx),
            None => Document::unread(file.expect("a deferred open has a file"), mtime, soft_wrap),
        });
        Self::with_document(start, document, false, window, cx)
    }

    /// A workspace whose one tab is `document` (`pinned` or not): what a new window takes when a tab
    /// is dragged out of another. The entity is the same, so nothing is read again.
    pub fn with_document(start: Start, document: Entity<Document>, pinned: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self::assemble(start, document, pinned, false, window, cx)
    }

    /// The scratch window's workspace: one blank Untitled page (a draft once typed in), no panels,
    /// and nothing it does is written to the settings.
    pub fn new_scratch(start: Start, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let soft_wrap = settings::get(cx).editor.soft_wrap;
        let document = cx.new(|cx| Document::new(None, Decoded::utf8(String::new()), None, soft_wrap, window, cx));
        Self::assemble(start, document, false, true, window, cx)
    }

    fn assemble(start: Start, document: Entity<Document>, pinned: bool, scratch: bool, window: &mut Window, cx: &mut Context<Self>) -> Self {
        if let Some(caret) = start.caret.filter(|_| start.file.is_some()) {
            document.update(cx, |document, cx| document.go_to(caret, window, cx));
        }
        let explorer = cx.new(|cx| Explorer::new(start.root.clone(), start.file.as_deref(), window, cx));
        let opens = cx.subscribe_in(&explorer, window, |this, _, event: &ExplorerEvent, window, cx| match event {
            // Deferred: the open reads and updates the explorer, which just emitted this.
            ExplorerEvent::Open(path) => {
                let (workspace, path) = (cx.entity(), path.clone());
                window.defer(cx, move |window, cx| {
                    workspace.update(cx, |this, cx| this.open_file(path, window, cx));
                });
            }
            ExplorerEvent::Preview(path) => {
                let (workspace, path) = (cx.entity(), path.clone());
                window.defer(cx, move |window, cx| workspace.update(cx, |this, cx| this.open_preview(path, window, cx)));
            }
            ExplorerEvent::OpenToSide(path) => {
                let (workspace, path) = (cx.entity(), path.clone());
                window.defer(cx, move |window, cx| workspace.update(cx, |this, cx| this.open_to_side(path, window, cx)));
            }
            ExplorerEvent::Root(root) => {
                this.start.root = root.clone();
                this.hand_root(cx);
                this.remember_tabs(cx);
            }
            ExplorerEvent::Renamed { from, to } => {
                this.retarget_tabs(from, to, cx);
                this.remember_tabs(cx);
                file_ops::retarget_elsewhere(window.window_handle(), from, to, cx);
            }
            ExplorerEvent::Leave => this.active_pane().update(cx, |pane, cx| pane.focus_editor(window, cx)),
            // Git's answer changed (a commit, a checkout): each pane's change bars read HEAD again.
            ExplorerEvent::Git => {
                for slot in &this.panes {
                    slot.bars.update(cx, |bars, cx| bars.refresh_head(cx));
                }
            }
            ExplorerEvent::Gone(path) => {
                this.close_tabs_under(path, window, cx);
                this.remember_tabs(cx);
                file_ops::close_elsewhere(window.window_handle(), path, cx);
            }
        });
        let changes = cx.subscribe_in(&explorer, window, |this, _, FilesChanged(paths): &FilesChanged, window, cx| {
            this.reload_changed(paths, false, window, cx)
        });
        let layout = cx.new(|_| ResizableState::default());
        let widths = cx.subscribe_in(&layout, window, |this, _, _: &ResizablePanelEvent, _, cx| this.remember_widths(cx));
        let bounds = cx.observe_window_bounds(window, |this, window, cx| {
            // Full screen in and out is a bounds change: the toolbar hides there (no 52 pt overlay),
            // and the bars redraw with the new insets. The toolbar is only touched when that state
            // changes, not on every frame of a resize. A size change already refreshes the whole
            // window (`Window::bounds_changed`) and a pure move draws nothing new, so only that
            // state change needs the root to redraw.
            let full = window.is_fullscreen();
            if this.full_screen != full {
                this.full_screen = full;
                window_look::sync_full_screen(window);
                cx.notify();
            }
            // Its size and place are nobody's choice to remember: the user's window geometry stays as it was.
            if this.scratch {
                return;
            }
            // A window growing from a torn-off card passes through sizes that are nobody's choice.
            if window_look::is_growing(window.window_handle().window_id()) {
                return;
            }
            let (state, bounds) = match window.window_bounds() {
                WindowBounds::Windowed(bounds) => (WindowState::Windowed, bounds),
                WindowBounds::Maximized(bounds) => (WindowState::Maximized, bounds),
                WindowBounds::Fullscreen(bounds) => (WindowState::Fullscreen, bounds),
            };
            // The geometry goes to a global no settings observer watches, and one debounced write
            // saves it when the resize goes quiet (`update_geometry`), so a frame of a resize wakes nobody.
            let placed = settings::Placed {
                width: bounds.size.width.into(),
                height: bounds.size.height.into(),
                x: bounds.origin.x.into(),
                y: bounds.origin.y.into(),
                state,
            };
            settings::update_geometry(cx, placed, &this.display);
        });
        // The display is looked up when the window moves to another one, not on every bounds change.
        let moved = cx.observe_window_display(window, |this, window, cx| {
            if this.scratch {
                return;
            }
            let display = display_uuid(window, cx);
            if this.display != display {
                this.display = display.clone();
                settings::update_later(cx, move |settings| settings.window.display = display);
            }
        });

        // A settings change tells the preview whether its panel is on: a hidden panel draws nothing, so the
        // pane isn't rendered to notice. Only the workspace knows Minimal mode's own say.
        let panel = cx.observe_global::<settings::Store>(|this, cx| {
            let on = this.preview_on(cx);
            this.preview_pane.update(cx, |pane, cx| pane.set_visible(on, cx));
        });

        let active = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                if std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some() {
                    window_look::deactivate_app();
                }
                control::note_active(window);
                // A file changed or deleted while the window was behind may be outside the
                // watched folder, or the watcher may have missed it.
                let open: Vec<PathBuf> = this.documents(cx).iter().filter_map(|document| document.read(cx).path.clone()).collect();
                this.reload_changed(&open, true, window, cx);
            } else {
                this.flush_drafts(cx);
            }
            // The focus light dims or lights with the window's key state.
            cx.notify();
        });

        let shown = (Some(document.entity_id()), document.read(cx).path.clone());
        let preview_pane = cx.new(|cx| PreviewPane::new(document.clone(), Rc::default(), cx));
        let editor = document.update(cx, |document, cx| document.editor(window, cx));
        let first = document.entity_id();
        let pane = cx.new(|cx| Pane::new(document, cx));
        if pinned {
            pane.update(cx, |pane, cx| pane.pin_id(first, cx));
        }
        let quit = cx.on_app_quit(|this, cx| {
            this.flush_drafts(cx);
            async {}
        });

        let mut this = Self {
            panes: Vec::new(),
            tree: Tree::new(0),
            sent: (None, None),
            active: 0,
            next_pane: 0,
            drag: None,
            moves_pending: Rc::default(),
            trail: Vec::new(),
            divider_hover: None,
            explorer,
            preview_pane,
            link: Rc::default(),
            input: None,
            has_preview: false,
            shown,
            material: None,
            corner_inset: f32::from(TAB_BAR_INSET),
            left: Slide::settled(false),
            right: Slide::settled(false),
            display: display_uuid(window, cx),
            full_screen: false,
            chrome: Fade::with(f32::from(window.is_fullscreen() || scratch), crate::tokens::COLUMN_SLIDE),
            minimal: Minimal::at(!scratch && settings::get(cx).window.minimal),
            slides_started: false,
            light: Light::default(),
            bands: Vec::new(),
            band_now: Instant::now(),
            drop_target: None,
            edge_pinned: ([true; 2], 0.),
            held: None,
            go_to_line: None,
            open_quickly: None,
            find_in_folder: None,
            go_to_symbol: None,
            draft_list: None,
            versions_list: None,
            undo_slider: None,
            rename_field: None,
            find_float: None,
            follow_chip: cx.new(|_| FollowChip::new()),
            paces: HashMap::new(),
            read_seq: 0,
            read_applied: HashMap::new(),
            drop_preview: motion::Morph::new(Instant::now()),
            tooltips: cx.new(|_| tooltip::Layer),
            start,
            closed: Vec::new(),
            window: window.window_handle(),
            scratch,
            focus: crate::focus::Mode::Off,
            layout,
            _subscriptions: [opens, changes, widths, bounds, moved, active, quit, panel],
        };
        // The first pane is the tree's one leaf.
        this.add_slot(pane, editor, window, cx);
        let workspace = cx.entity().downgrade();
        // AppKit never closes the window itself: a confirmed close fades it out and removes it,
        // the same as Cmd-Shift-W, so the red button fades too.
        window.on_window_should_close(cx, move |window, cx| {
            let confirmed = workspace.upgrade().is_none_or(|workspace| confirm_close(workspace, window, cx));
            if confirmed {
                finish_close(window, cx);
            }
            false
        });
        control::note_active(window);
        this.restore_tabs(window, cx);
        // The panes start blank: they learn the first document's preview button and state here.
        this.sync(window, cx);
        this
    }

    /// Whether the preview panel shows: it's on and the showing document is markdown.
    fn right_shows(&self, cx: &App) -> bool {
        self.preview_on(cx) && self.has_preview && !self.scratch
    }

    /// The preview is on: while minimal, only when asked for with Shift-Cmd-V; otherwise its setting says so.
    fn preview_on(&self, cx: &App) -> bool {
        if self.minimal.on { self.minimal.preview } else { settings::get(cx).preview.visible }
    }

    /// The document in the showing tab of the active pane; `None` while it has no tab.
    pub(crate) fn document(&self, cx: &App) -> Option<Entity<Document>> {
        self.active_pane().read(cx).active_document().cloned()
    }

    /// Tells each pane's editor view which editor shows (and whether the preview is open), the
    /// preview pane which document the active pane shows, and the window's input handler which
    /// editor takes the typed text.
    fn sync_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (active, corner) = (self.active, self.tree.corner());
        let visible = self.preview_on(cx);
        let mut following = None;
        for slot in &mut self.panes {
            let (id, is_active) = (slot.id, slot.id == active);
            slot.pane.update(cx, |pane, cx| {
                pane.set_corner(id == corner, cx);
                // (the band follows its own ease in `render`, in the frame it moves)
                // (the top right pane's right room follows the preview's slide in `render`)
            });
            match slot.pane.read(cx).active_document().cloned() {
                Some(document) => {
                    // A restored tab that wasn't built yet is built here, before it draws.
                    let editor = document.update(cx, |document, cx| document.editor(window, cx));
                    let has_preview = document.read(cx).has_preview();
                    crate::focus::apply(&editor, self.focus, cx);
                    slot.input = Some((editor.read(cx).focus_handle(cx), editor.clone()));
                    let path = document.read(cx).path.clone();
                    slot.bars.update(cx, |bars, cx| bars.set(&editor, path.as_deref(), cx));
                    slot.editor.update(cx, |pane, cx| {
                        pane.set_note(document.read(cx).path.clone());
                        pane.set(editor, visible && has_preview, is_active, document.read(cx).path.is_none(), crate::text_style::is_prose(document.read(cx).language(cx)), cx)
                    });
                    if is_active {
                        following = Some((document, has_preview));
                    }
                }
                None => {
                    slot.input = None;
                    slot.bars.update(cx, |bars, cx| bars.clear(cx));
                    // A closing pane keeps drawing the editor it had until it goes; any other fades it out and lets go of it.
                    if !slot.holding {
                        slot.editor.update(cx, |pane, cx| pane.fade_out(cx));
                        // Kit keeps the input that last had the keyboard until another input takes it, which
                        // would keep the closed tab's editor alive; this drops that record once it isn't focused.
                        window.focused_input(cx);
                    }
                }
            }
        }
        let slot = self.slot(active).unwrap_or(&self.panes[0]);
        let (link, input) = (slot.link.clone(), slot.input.clone());
        self.link = link;
        self.input = input;
        match following {
            Some((document, has_preview)) => {
                self.has_preview = has_preview;
                let link = self.link.clone();
                self.preview_pane.update(cx, |pane, cx| pane.set(document, link, visible, cx));
            }
            None => {
                self.has_preview = false;
                self.preview_pane.update(cx, |pane, cx| pane.follow_none(cx));
            }
        }
    }

    /// Writes the widths of the side panels that are showing. The panels are
    /// the first and third of the row.
    fn remember_widths(&mut self, cx: &mut Context<Self>) {
        if self.scratch {
            return;
        }
        let sizes = self.layout.read(cx).sizes().clone();
        let left = self.left_shows(cx).then(|| sizes.first().copied()).flatten();
        let right = self.right_shows(cx).then(|| sizes.get(2).copied()).flatten();
        settings::update(cx, |settings| {
            if let Some(width) = left {
                settings.layout.left_panel_width = width.into();
            }
            if let Some(width) = right {
                settings.layout.right_panel_width = width.into();
            }
        });
    }

    fn save(&mut self, _: &Save, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(document) = self.document(cx) {
            // A save of a file with edits settles its tab's dot when it lands, or swells it when it fails.
            let (edited, before) = (document.read(cx).shows_dot(), document.read(cx).write_errors());
            let save = document.update(cx, |document, cx| document.save(window, cx));
            if edited {
                cx.spawn(async move |this, cx| {
                    let ok = save.await;
                    this.update(cx, |this, cx| this.settle_save(&document, ok, 0, before, cx)).ok();
                })
                .detach();
            } else {
                save.detach();
            }
        }
    }

    /// Tells the panes' tab bars a save of `document` landed (`ok`) or failed, `nth` of a save all's ripple. A save that came back false
    /// without a new write error (a cancelled question or save panel) settles nothing. `errors` is the document's `write_errors` before the save.
    fn settle_save(&mut self, document: &Entity<Document>, ok: bool, nth: u32, errors: u32, cx: &mut Context<Self>) {
        if !ok && document.read(cx).write_errors() == errors {
            return;
        }
        let id = document.entity_id();
        for slot in &self.panes {
            slot.pane.update(cx, |pane, cx| pane.save_settled(id, ok, nth, cx));
        }
    }

    /// Cmd-B: shows or hides the folder tree and remembers the choice.
    fn toggle_explorer(&mut self, _: &ToggleExplorer, window: &mut Window, cx: &mut Context<Self>) {
        if self.scratch {
            return;
        }
        // Minimal mode has no tree: Cmd-B leaves it and shows the tree.
        if self.minimal.on {
            self.set_minimal(false, window, cx);
            settings::update_later(cx, |settings| settings.explorer.visible = true);
        } else {
            settings::update_later(cx, |settings| settings.explorer.visible = !settings.explorer.visible);
        }
        cx.defer(menu::refresh);
        cx.notify();
    }

    /// Shift-Cmd-E: puts the keyboard in the folder tree, showing the explorer first when it is hidden.
    fn focus_explorer(&mut self, _: &FocusExplorer, window: &mut Window, cx: &mut Context<Self>) {
        if self.scratch {
            return;
        }
        if self.minimal.on {
            self.set_minimal(false, window, cx);
        }
        let explorer = self.explorer.clone();
        if settings::get(cx).explorer.visible {
            explorer.update(cx, |explorer, cx| explorer.focus_tree(window, cx));
            return;
        }
        settings::update(cx, |settings| settings.explorer.visible = true);
        cx.defer(menu::refresh);
        cx.notify();
        // The panel draws on the next frame; the tree takes the keyboard once it is there.
        window.defer(cx, move |window, cx| explorer.update(cx, |explorer, cx| explorer.focus_tree(window, cx)));
    }

    /// Cmd-Up: roots the folder tree at the folder above its root.
    fn enclosing_folder(&mut self, _: &EnclosingFolder, _: &mut Window, cx: &mut Context<Self>) {
        self.explorer.update(cx, |explorer, cx| explorer.go_up(cx));
    }

    /// Shift-Cmd-J: opens the folders down to the showing file in the tree and selects it.
    fn reveal_current_file(&mut self, _: &RevealCurrentFile, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.document(cx).and_then(|document| document.read(cx).path.clone()) {
            self.reveal_in_explorer(&path, cx);
        }
    }

    /// Shows `path` in the tree. A file outside the tree's folder is first looked for under a folder link in the tree
    /// (read off the main thread) and selected at the link's spelling; otherwise it re-roots the tree the way opening that
    /// file from elsewhere would (its git root, else its own folder) and then reveals it; a file whose folder can't be
    /// read beeps.
    fn reveal_in_explorer(&mut self, path: &Path, cx: &mut Context<Self>) {
        let root = self.explorer.read(cx).root().to_path_buf();
        let path = path.to_path_buf();
        if path == root || path.starts_with(&root) {
            self.explorer.update(cx, |explorer, cx| explorer.reveal_file(&path, cx));
            return;
        }
        let search = {
            let (root, path) = (root.clone(), path.clone());
            cx.background_executor().spawn(async move { through_link(&root, &path) })
        };
        cx.spawn(async move |this, cx| {
            let held = search.await;
            this.update(cx, |this, cx| this.reveal_outside(&root, path, held, cx)).ok();
        })
        .detach();
    }

    /// The end of `reveal_in_explorer` for a file outside the tree once the link search is in: `held` is the file's
    /// spelling under a link in the tree, if the search found one. The tree may have changed root meanwhile, so a
    /// spelling found for another root is dropped.
    fn reveal_outside(&mut self, searched: &Path, mut path: PathBuf, held: Option<PathBuf>, cx: &mut Context<Self>) {
        let root = self.explorer.read(cx).root().to_path_buf();
        if let Some(held) = held.filter(|_| root == searched) {
            self.explorer.update(cx, |explorer, cx| explorer.reveal_file(&held, cx));
            return;
        }
        if path != root && !path.starts_with(&root) {
            if !path.parent().is_some_and(|parent| std::fs::read_dir(parent).is_ok()) {
                crate::preview::render::beep();
                return;
            }
            let start = crate::start::resolve(Some(&path), Path::new("/"), &settings::get(cx).explorer);
            // The start's file is the path with symlinks resolved, like the root above it.
            path = start.file.unwrap_or(path);
            self.explorer.update(cx, |explorer, cx| explorer.set_root(start.root, cx));
        }
        self.explorer.update(cx, |explorer, cx| explorer.reveal_file(&path, cx));
    }

    /// A tab's right-click menu > Reveal in explorer: shows the explorer when it is hidden (leaving minimal mode) and opens the folders
    /// down to the tab's file and selects it.
    fn reveal_tab(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.holder_of(id, cx).and_then(|holder| self.slot(holder)).and_then(|slot| slot.pane.read(cx).path_of(id, cx));
        let Some(path) = path.filter(|_| !self.scratch) else { return };
        if self.minimal.on {
            self.set_minimal(false, window, cx);
        }
        if !settings::get(cx).explorer.visible {
            settings::update(cx, |settings| settings.explorer.visible = true);
            cx.defer(menu::refresh);
            cx.notify();
        }
        self.reveal_in_explorer(&path, cx);
    }

    /// Shift-Cmd-.: shows or hides dot files in the tree and remembers the choice.
    fn toggle_hidden_files(&mut self, _: &ToggleHiddenFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.explorer.update(cx, |explorer, cx| explorer.toggle_hidden(cx));
        cx.defer(menu::refresh);
    }

    /// View > Show ignored files: shows or hides what `.gitignore` files leave out, dimmed, and remembers the choice.
    fn toggle_ignored_files(&mut self, _: &ToggleIgnoredFiles, _: &mut Window, cx: &mut Context<Self>) {
        self.explorer.update(cx, |explorer, cx| explorer.toggle_ignored(cx));
        cx.defer(menu::refresh);
    }

    /// View > Sort by: orders this window's tree that way, and new windows after it.
    fn sort_explorer(&mut self, action: &SortExplorer, _: &mut Window, cx: &mut Context<Self>) {
        self.explorer.update(cx, |explorer, cx| explorer.set_sort(action.sort, cx));
        cx.defer(menu::refresh);
    }

    /// Option-Cmd-Left: closes every folder of the tree.
    fn collapse_all_folders(&mut self, _: &CollapseAllFolders, _: &mut Window, cx: &mut Context<Self>) {
        self.explorer.update(cx, |explorer, cx| explorer.collapse_all(cx));
    }

    /// Cmd-Shift-V: shows or hides the markdown preview and remembers the choice.
    fn toggle_preview(&mut self, _: &TogglePreview, window: &mut Window, cx: &mut Context<Self>) {
        if self.scratch {
            return;
        }
        if self.minimal.on {
            // Minimal mode hides the preview without touching the setting; asking for it shows it for this stay in the mode.
            self.minimal.preview = !self.minimal.preview;
        } else {
            settings::update_later(cx, |settings| settings.preview.visible = !settings.preview.visible);
        }
        // Shown again (or for the first time), the preview lines up with the editor at once.
        if let Some(document) = self.document(cx) {
            if let Some(preview) = document.read(cx).preview.as_ref() {
                preview.stop_following();
            }
        }
        self.sync_panes(window, cx);
        cx.defer(menu::refresh);
        cx.notify();
    }

    /// A click on a link to a local markdown file in the preview: opens it, and when the link
    /// has a `#heading`, asks its preview to jump there once the file is parsed. A link with a
    /// caret (a compiler error's location) puts the caret there and washes the line, once the
    /// move has settled.
    fn open_linked(&mut self, action: &OpenFile, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(document) = self.open_file_at(action.path.clone(), action.caret, window, cx) else {
            return;
        };
        if action.caret.is_some() {
            // `Document::go_to` settles the caret two frames on; the wash follows it.
            let editor = document.read(cx).built_editor().cloned();
            window.on_next_frame(move |window, _| {
                window.on_next_frame(move |_, cx| {
                    if let Some(editor) = editor {
                        editor.update(cx, |editor, cx| editor.wash_caret_line(crate::tokens::WELCOME_BACK_WASH, cx));
                    }
                });
            });
        }
        let state = document.read(cx).preview.as_ref().map(|preview| preview.state.clone());
        if let (Some(anchor), Some(state)) = (&action.anchor, state) {
            state.update(cx, |state, _| state.set_pending_anchor(Some(anchor.clone())));
            window.refresh();
        }
    }

    /// A click on a task box in the preview: flips its `[ ]` in the open file's text, through
    /// the editor, and puts the focus in the editor, so Cmd-Z undoes the tick.
    fn toggle_task(&mut self, action: &ToggleTask, window: &mut Window, cx: &mut Context<Self>) {
        let Some(document) = self.document(cx) else { return };
        // A click from a preview that isn't this file's (fading out, or already switched) must not touch this file.
        if document.read(cx).preview.as_ref().map(|preview| preview.state.entity_id().as_u64()) != Some(action.preview) {
            return;
        }
        let Some(editor) = document.read(cx).built_editor().cloned() else {
            return;
        };
        if crate::preview::task::toggle_task(&editor, action.item_start..action.item_end, action.parsed_len, window, cx) {
            editor.update(cx, |editor, cx| editor.focus(window, cx));
        } else {
            // The text moved on since the preview was parsed: the tick changes nothing, and says so.
            crate::preview::render::beep();
        }
    }

    /// View > Soft wrap: wraps long lines in the open file or stops wrapping them.
    fn toggle_soft_wrap(&mut self, _: &ToggleSoftWrap, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(document) = self.document(cx) {
            document.update(cx, |document, cx| document.toggle_soft_wrap(window, cx));
        }
    }

    /// Option-Z: the open file wraps at the reading column or at the window's width, each press flipping.
    fn cycle_wrap_width(&mut self, _: &CycleWrapWidth, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(document) = self.document(cx) {
            document.update(cx, |document, cx| document.cycle_wrap_width(window, cx));
        }
    }

    /// View > Next change and Previous change: over a Compare view they step its changes; otherwise the caret goes to the first line of the next (or previous) change since the last commit,
    /// the way Go to line goes (its glide and centering).
    fn step_change(&mut self, forward: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some(document) = self.document(cx) else { return };
        // A Compare view over the tab steps its own changes.
        if let Some(view) = document.read(cx).compare_view().cloned() {
            if !view.update(cx, |view, cx| view.step(forward, cx)) {
                crate::preview::render::beep();
            }
            return;
        }
        let bars = self.slot(self.active).map(|slot| slot.bars.clone());
        let line = document.read(cx).caret(cx).and_then(|caret| bars?.read(cx).step(caret.line.saturating_sub(1) as usize, forward));
        match line {
            Some(line) => document.update(cx, |document, cx| document.go_to(Caret { line: line as u32 + 1, column: 1 }, window, cx)),
            None => crate::preview::render::beep(),
        }
    }

    /// Cmd-L: opens the Go to line field over the active editor, or takes the keyboard back to it.
    fn show_go_to_line(&mut self, _: &ShowGoToLine, window: &mut Window, cx: &mut Context<Self>) {
        if self.document(cx).is_none() {
            return;
        }
        let (field, _) = self.go_to_line.get_or_insert_with(|| {
            let field = cx.new(|cx| GoToLine::new(window, cx));
            let subscription = cx.subscribe_in(&field, window, |this, _, event: &GoToLineEvent, window, cx| this.go_to_line_event(event, window, cx));
            (field, subscription)
        });
        let field = field.clone();
        // The lines a file shows: the empty one after a closing newline isn't counted.
        let lines = self.document(cx).and_then(|document| document.read(cx).built_editor().cloned()).map_or(0, |editor| {
            let rope = editor.read(cx).text();
            let rows = rope.lines_len();
            (if rows > 1 && rope.line_len(rows - 1) == 0 { rows - 1 } else { rows }) as u32
        });
        field.update(cx, |field, cx| {
            field.set_lines(lines);
            field.open(window, cx);
        });
    }

    /// Find next or previous with the bar closed landed on `step` in `editor`: shows the count float.
    pub fn show_find_step(&mut self, editor: &Entity<EditorState>, step: Step, window: &mut Window, cx: &mut Context<Self>) {
        let float = self.find_float.get_or_insert_with(|| cx.new(|_| FindFloat::new(window))).clone();
        float.update(cx, |float, cx| float.show(editor, step, window, cx));
    }

    /// What the Go to line field says: Return on a place moves the caret there (the glide and the
    /// centering are `Document::go_to`'s), and closing gives the keyboard back to the editor.
    fn go_to_line_event(&mut self, event: &GoToLineEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            GoToLineEvent::Go { caret, found } => {
                if let Some(document) = self.document(cx) {
                    document.update(cx, |document, cx| document.go_to(*caret, window, cx));
                    // A line past the end lands on the last line without the found wash.
                    if *found && let Some(editor) = document.read(cx).built_editor().cloned() {
                        editor.update(cx, |editor, cx| editor.wash_landing(cx));
                    }
                }
            }
            GoToLineEvent::Done => {
                if let Some(editor) = self.document(cx).and_then(|document| document.read(cx).built_editor().cloned()) {
                    editor.update(cx, |editor, cx| editor.focus(window, cx));
                }
            }
        }
    }

    /// For the control socket: types `text` into the open Go to line field and presses Return.
    pub fn go_to_line_submit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let field = self.go_to_line.as_ref().map(|(field, _)| field.clone()).filter(|field| field.read(cx).is_open()).ok_or("the Go to line field isn't open")?;
        field.update(cx, |field, cx| field.submit(text, window, cx))
    }

    /// Shift-Cmd-O: opens the Open Quickly float over the active pane, finding files under the tree's root.
    fn show_open_quickly(&mut self, _: &ShowOpenQuickly, window: &mut Window, cx: &mut Context<Self>) {
        let (float, _) = self.open_quickly.get_or_insert_with(|| {
            let float = cx.new(|cx| OpenQuickly::new(window, cx));
            let subscription = cx.subscribe_in(&float, window, |this, _, event: &OpenQuicklyEvent, window, cx| this.open_quickly_event(event, window, cx));
            (float, subscription)
        });
        let float = float.clone();
        let root = self.explorer.read(cx).root().to_path_buf();
        let show_hidden = settings::get(cx).explorer.show_hidden;
        float.update(cx, |float, cx| float.open(root, show_hidden, window, cx));
    }

    /// Shift-Option-Cmd-F: opens the Find in folder float over the active pane, searching the tree's root for the find text
    /// (or the selection). A window with no folder (the scratch window) says so in the float.
    fn show_find_in_folder(&mut self, _: &ShowFindInFolder, window: &mut Window, cx: &mut Context<Self>) {
        let (float, _) = self.find_in_folder.get_or_insert_with(|| {
            let float = cx.new(|cx| FindInFolder::new(window, cx));
            let subscription = cx.subscribe_in(&float, window, |this, _, event: &FindInFolderEvent, window, cx| this.find_in_folder_event(event, window, cx));
            (float, subscription)
        });
        let float = float.clone();
        let root = Some(self.explorer.read(cx).root().to_path_buf()).filter(|root| !self.scratch && !root.as_os_str().is_empty());
        let show_hidden = settings::get(cx).explorer.show_hidden;
        let editor = self.document(cx).and_then(|document| document.read(cx).built_editor().cloned());
        float.update(cx, |float, cx| float.open(root, show_hidden, editor, window, cx));
    }

    /// What Find in folder says: a hit opens its file in the active pane (or brings its tab forward) with the match selected and
    /// given Find's halo, and closing gives the keyboard back to the editor in front.
    fn find_in_folder_event(&mut self, event: &FindInFolderEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            FindInFolderEvent::Open(hit) => {
                let caret = Caret { line: hit.line as u32 + 1, column: 1 };
                if let Ok(document) = self.open_file_at(hit.path.clone(), Some(caret), window, cx)
                    && let Some(editor) = document.read(cx).built_editor().cloned()
                {
                    crate::find_in_folder::land(&editor, hit, window, cx);
                }
            }
            FindInFolderEvent::Done => {
                if let Some(editor) = self.document(cx).and_then(|document| document.read(cx).built_editor().cloned()) {
                    editor.update(cx, |editor, cx| editor.focus(window, cx));
                }
            }
        }
    }

    /// For the control socket: types `text` into the open Find in folder float, searches and presses Return; replies with the
    /// `path:line` it opened.
    pub fn find_in_folder_submit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<String, String> {
        let float = self.find_in_folder.as_ref().map(|(float, _)| float.clone()).filter(|float| float.read(cx).is_open()).ok_or("Find in folder isn't open")?;
        float.update(cx, |float, cx| float.submit(text, window, cx))
    }

    /// Cmd-Ctrl-J: opens the Go to symbol field over the active editor, listing the open file's functions, types, headings, tables or keys.
    fn show_go_to_symbol(&mut self, _: &ShowGoToSymbol, window: &mut Window, cx: &mut Context<Self>) {
        let Some(document) = self.document(cx) else { return };
        let (lang, editor) = (document.read(cx).language(cx), document.read(cx).built_editor().cloned());
        let Some(editor) = editor else { return };
        let (field, _) = self.go_to_symbol.get_or_insert_with(|| {
            let field = cx.new(|cx| GoToSymbol::new(window, cx));
            let subscription = cx.subscribe_in(&field, window, |this, _, event: &GoToSymbolEvent, window, cx| this.go_to_symbol_event(event, window, cx));
            (field, subscription)
        });
        let field = field.clone();
        field.update(cx, |field, cx| field.open(editor, lang, window, cx));
    }

    /// Go to symbol closed: the keyboard goes back to the editor in front (the caret was moved by the field itself).
    fn go_to_symbol_event(&mut self, event: &GoToSymbolEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            GoToSymbolEvent::Done => {
                if let Some(editor) = self.document(cx).and_then(|document| document.read(cx).built_editor().cloned()) {
                    editor.update(cx, |editor, cx| editor.focus(window, cx));
                }
            }
        }
    }

    /// What Open Quickly says: a pick opens as a tab, and closing gives the keyboard back to the editor in front.
    fn open_quickly_event(&mut self, event: &OpenQuicklyEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            OpenQuicklyEvent::Open(path) => {
                self.open_file(path.clone(), window, cx);
            }
            OpenQuicklyEvent::Done => {
                if let Some(editor) = self.document(cx).and_then(|document| document.read(cx).built_editor().cloned()) {
                    editor.update(cx, |editor, cx| editor.focus(window, cx));
                }
            }
        }
    }

    /// For the control socket: types `text` into the open Open Quickly float and presses Return;
    /// replies with the file it opened.
    pub fn open_quickly_submit(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<PathBuf, String> {
        let float = self.open_quickly.as_ref().map(|(float, _)| float.clone()).filter(|float| float.read(cx).is_open()).ok_or("Open Quickly isn't open")?;
        float.update(cx, |float, cx| float.submit(text, window, cx))
    }

    /// File > Open draft: opens the draft list over the active pane. It reads the drafts folder in the
    /// background, and is told which draft files are open in tabs of any window (those are never trashed).
    fn show_draft_list(&mut self, _: &ShowDraftList, window: &mut Window, cx: &mut Context<Self>) {
        let (float, _) = self.draft_list.get_or_insert_with(|| {
            let float = cx.new(|cx| DraftList::new(window, cx));
            let subscription = cx.subscribe_in(&float, window, |this, _, event: &DraftListEvent, window, cx| this.draft_list_event(event, window, cx));
            (float, subscription)
        });
        let float = float.clone();
        let open = self.open_tab_files(window, cx);
        float.update(cx, |float, cx| float.open(open, window, cx));
    }

    /// File > Browse versions: opens the version picker over the active pane for the showing tab's
    /// file; a tab with no file refuses. A pick opens the Compare view of that version.
    fn browse_versions(&mut self, _: &BrowseVersions, window: &mut Window, cx: &mut Context<Self>) {
        let Some((document, path)) = self.document(cx).and_then(|document| Some((document.clone(), document.read(cx).path.clone()?))) else {
            self.refuse(window, cx);
            return;
        };
        let float = cx.new(|cx| VersionsList::new(path, window, cx));
        let subscription = cx.subscribe_in(&float, window, |this, _, event: &VersionsListEvent, window, cx| this.versions_list_event(event, window, cx));
        self.versions_list = Some((float, subscription, document));
        cx.notify();
    }

    /// Edit > Undo history: opens the slider over the showing tab's editor, or closes it keeping
    /// the text where it stands. A tab with nothing to scrub (or a read-only one) refuses.
    fn show_undo_history(&mut self, _: &ShowUndoHistory, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((slider, _, _)) = &self.undo_slider {
            let slider = slider.clone();
            slider.update(cx, |slider, cx| slider.shut(Close::Keep, window, cx, true));
            return;
        }
        let Some((document, editor)) = self.document(cx).and_then(|document| Some((document.clone(), document.read(cx).built_editor()?.clone()))) else {
            self.refuse(window, cx);
            return;
        };
        if !can_scrub(editor.read(cx)) {
            self.refuse(window, cx);
            return;
        }
        undo_slider::bind_keys(cx);
        let slider = cx.new(|cx| UndoSlider::new(&document, editor, window, cx));
        let subscription = cx.subscribe_in(&slider, window, |this, _, _: &UndoSliderEvent, _, cx| {
            this.undo_slider = None;
            cx.notify();
        });
        self.undo_slider = Some((slider, subscription, document));
        cx.notify();
    }

    /// For the control socket: the open Undo history slider.
    pub fn undo_slider(&self) -> Option<Entity<UndoSlider>> {
        self.undo_slider.as_ref().map(|(slider, _, _)| slider.clone())
    }

    /// What the version picker says: a pick closes it and opens that version in the Compare view
    /// of the tab it was opened for (read off the main thread); Escape closes it and gives the
    /// keyboard back to that tab.
    fn versions_list_event(&mut self, event: &VersionsListEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, _, document)) = self.versions_list.take() else { return };
        cx.notify();
        match event {
            VersionsListEvent::Pick { id, time } => document.update(cx, |document, cx| document.open_version(id.clone(), *time, window, cx)),
            VersionsListEvent::Close => document.update(cx, |document, cx| document.focus(window, cx)),
        }
    }

    /// For the control socket: the open version picker.
    pub fn versions_list_float(&self) -> Option<Entity<VersionsList>> {
        self.versions_list.as_ref().map(|(float, _, _)| float.clone())
    }

    /// The files of the tabs open in this window and every other one: a draft's own file for an Untitled page.
    fn open_tab_files(&self, window: &Window, cx: &App) -> Vec<PathBuf> {
        let mut open: Vec<PathBuf> = self.documents(cx).iter().filter_map(|document| document.read(cx).tab_file()).collect();
        for (_, workspace) in open_workspaces_except(Some(window.window_handle()), cx) {
            open.extend(workspace.read(cx).documents(cx).iter().filter_map(|document| document.read(cx).tab_file()));
        }
        open
    }

    /// What the draft list says: a pick opens as an Untitled page, Cmd-Delete trashes a draft no tab has
    /// open right now (asked here, not remembered from when the list opened), and closing gives the
    /// keyboard back to the editor in front.
    fn draft_list_event(&mut self, event: &DraftListEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            DraftListEvent::Open(path) => {
                self.open_draft(path.clone(), window, cx);
            }
            DraftListEvent::Trash(path) => {
                let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                let wanted = real(path);
                let open = self.open_tab_files(window, cx).iter().any(|file| real(file) == wanted);
                if let Some((float, _)) = &self.draft_list {
                    let float = float.clone();
                    float.update(cx, |float, cx| float.trash_checked(path.clone(), open, cx));
                }
            }
            DraftListEvent::Done => {
                if let Some(editor) = self.document(cx).and_then(|document| document.read(cx).built_editor().cloned()) {
                    editor.update(cx, |editor, cx| editor.focus(window, cx));
                }
            }
        }
    }

    /// Brings the tab backed by the draft at `path` forward, in this window or else in another one
    /// (that window comes forward too, except in a background launch), or opens the draft as an
    /// Untitled page in the active pane, as a relaunch brings it back. A draft open anywhere is never
    /// opened a second time, so two pages never write one file.
    pub(crate) fn open_draft(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<Document>> {
        let wanted = path.canonicalize().unwrap_or_else(|_| path.clone());
        if let Some(document) = self.show_draft_tab(&wanted, window, cx) {
            return Some(document);
        }
        for (handle, workspace) in open_workspaces_except(Some(window.window_handle()), cx) {
            let shown = cx.update_window(handle, |_, other, cx| {
                let shown = workspace.update(cx, |workspace, cx| workspace.show_draft_tab(&wanted, other, cx));
                if shown.is_some() && std::env::var_os("GPUI_EDITOR_BACKGROUND").is_none() {
                    other.activate_window();
                }
                shown
            });
            if let Ok(Some(document)) = shown {
                return Some(document);
            }
        }
        self.open_new_draft_page(path, window, cx)
    }

    /// Shows the tab of this window backed by the draft `wanted` (resolved), the active pane's first.
    /// `None` when no pane has it.
    fn show_draft_tab(&mut self, wanted: &Path, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<Document>> {
        let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let mut order = self.tree.live().to_vec();
        order.sort_by_key(|id| *id != self.active);
        for id in order {
            let Some(pane) = self.slot(id).map(|slot| slot.pane.clone()) else { continue };
            let open = pane.read(cx).documents().position(|document| document.read(cx).tab_file().is_some_and(|file| real(&file) == wanted));
            if let Some(ix) = open {
                self.active = id;
                pane.update(cx, |pane, cx| pane.activate(ix, window, cx));
                self.sync(window, cx);
                return pane.read(cx).active_document().cloned();
            }
        }
        None
    }

    /// Opens the draft at `path` as a new Untitled page in the active pane.
    fn open_new_draft_page(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<Document>> {
        let soft_wrap = settings::get(cx).editor.soft_wrap;
        match drafts::read(&path) {
            Ok(text) => {
                let document = cx.new(|_| Document::unbuilt_draft(path, text, soft_wrap));
                self.active_pane().clone().update(cx, |pane, cx| pane.add(document.clone(), true, window, cx));
                Some(document)
            }
            Err(_) => {
                self.alert_unreadable_drafts(vec![path], window, cx);
                None
            }
        }
    }

    /// For the control socket: the open draft list as JSON (see `DraftList::snapshot`).
    pub fn draft_list_float(&self, cx: &App) -> Result<Entity<DraftList>, String> {
        self.draft_list.as_ref().map(|(float, _)| float.clone()).filter(|float| float.read(cx).is_open()).ok_or_else(|| "the draft list isn't open".into())
    }

    /// For the control socket: the Compare view the active pane's tab shows, if it has one.
    pub fn active_compare(&self, cx: &App) -> Option<Entity<crate::compare_view::CompareView>> {
        self.document(cx)?.read(cx).compare_view().cloned()
    }

    /// The active pane's showing tab and its file; `None` for an Untitled page or no tab.
    fn file_tab(&self, cx: &App) -> Option<(Entity<Document>, PathBuf)> {
        let document = self.document(cx)?;
        let path = document.read(cx).path.clone()?;
        Some((document, path))
    }

    /// Whether File > Rename, Duplicate, Move to, Show in Finder and Copy path apply: the showing tab has a file on disk.
    pub(crate) fn has_file_tab(&self, cx: &App) -> bool {
        self.file_tab(cx).is_some()
    }

    /// Edit > Move item here (Option-Cmd-V): moves the clipboard's files into the folder of the tree's selected row (the tree
    /// follows the file in front), else the tree's folder. A name or the filter being typed keeps its own.
    fn move_item_here(&mut self, _: &MoveItemHere, window: &mut Window, cx: &mut Context<Self>) {
        if self.scratch || self.explorer.read(cx).is_typing() {
            return;
        }
        self.explorer.update(cx, |explorer, cx| explorer.move_files_here(None, window, cx));
    }

    /// File > New file in explorer and New folder in explorer: shows the explorer when it is hidden (leaving minimal mode), then
    /// opens the tree's new-item name field in the showing file's folder, else the tree's folder. Return keeps the name. A column
    /// that has to slide in first takes the keyboard off the editor at once and opens the field once the slide has landed.
    fn new_in_explorer(&mut self, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.scratch {
            return;
        }
        let showing = settings::get(cx).explorer.visible && !self.minimal.on;
        if self.minimal.on {
            self.set_minimal(false, window, cx);
        }
        let explorer = self.explorer.clone();
        let root = explorer.read(cx).root().to_path_buf();
        let at = self.file_tab(cx).map(|(_, path)| path).filter(|path| path.starts_with(&root)).unwrap_or(root);
        if showing {
            explorer.update(cx, |explorer, cx| explorer.new_item(&at, folder, None, window, cx).ok());
            return;
        }
        // Typing in the meantime must not land in the editor.
        window.blur(cx);
        settings::update(cx, |settings| settings.explorer.visible = true);
        cx.defer(menu::refresh);
        cx.notify();
        cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs_f32(crate::tokens::COLUMN_SLIDE)).await;
            this.update_in(cx, |this, window, cx| {
                // The column may have been hidden again meanwhile; then (or when the field can't open) the editor gets its keys back.
                if this.left_shows(cx) && explorer.update(cx, |explorer, cx| explorer.new_item(&at, folder, None, window, cx)).is_ok() {
                    return;
                }
                this.active_pane().clone().update(cx, |pane, cx| pane.focus_editor(window, cx));
            })
            .ok();
        })
        .detach();
    }

    /// The open file was renamed or moved: the tab follows it, as it does for the explorer's own
    /// renames (recent files, pins and saved tabs too, and the other windows' tabs).
    fn file_moved(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        let event = ExplorerEvent::Renamed { from: from.to_path_buf(), to: to.to_path_buf() };
        self.explorer.update(cx, |_, cx| cx.emit(event));
    }

    /// File > Rename: opens the name field over the active editor, filled with the file's name.
    fn rename_file(&mut self, _: &RenameFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, path)) = self.file_tab(cx) else { return };
        let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        let (field, _) = self.rename_field.get_or_insert_with(|| {
            let field = cx.new(|cx| RenameField::new(window, cx));
            let subscription = cx.subscribe_in(&field, window, |this, _, event: &RenameEvent, window, cx| this.rename_field_event(event, window, cx));
            (field, subscription)
        });
        let field = field.clone();
        field.update(cx, |field, cx| field.open(&name, window, cx));
    }

    /// What the Rename field says: Return renames (a refusal shows under the field), and closing
    /// gives the keyboard back to the editor.
    fn rename_field_event(&mut self, event: &RenameEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            RenameEvent::Submit(name) => {
                let _ = self.file_rename(name, window, cx);
            }
            RenameEvent::Done => {
                if let Some(editor) = self.document(cx).and_then(|document| document.read(cx).built_editor().cloned()) {
                    editor.update(cx, |editor, cx| editor.focus(window, cx));
                }
            }
        }
    }

    /// Renames the showing tab's file to `name` in its folder and the tab follows. The Rename
    /// field, when open, closes on success and shows the refusal otherwise. The error is the
    /// explorer's refusal for a taken, empty or invalid name.
    pub fn file_rename(&mut self, name: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let result = self.file_tab(cx).ok_or_else(|| "no file on disk is showing".to_string()).and_then(|(_, from)| {
            if from.file_name().is_some_and(|current| *current == *name) {
                return Ok(());
            }
            self.refuse_busy_file(&from, window, cx)?;
            let to = file_ops::rename(&from, name)?;
            self.file_moved(&from, &to, cx);
            Ok(())
        });
        let field = self.rename_field.as_ref().map(|(field, _)| field.clone()).filter(|field| field.read(cx).is_open());
        if let Some(field) = field {
            match &result {
                Ok(()) => field.update(cx, |field, cx| field.close(window, cx)),
                Err(why) => field.update(cx, |field, cx| field.refuse(why.clone(), cx)),
            }
        }
        result
    }

    /// File > Duplicate: copies the showing tab's file next to itself and opens the copy as the
    /// tab beside the original. A tab with unsaved edits copies its text, edits included, and
    /// stays unsaved: the copy is written off the main thread, so this returns `Ok` once it has
    /// started and a failure then comes as its own alert. A clean tab copies the file on disk.
    pub fn file_duplicate(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let (document, from) = self.file_tab(cx).ok_or("no file on disk is showing")?;
        let Some(job) = document.read(cx).duplicate_edits(cx) else {
            let to = file_ops::duplicate(&from)?;
            return self.open_copy(to, window, cx);
        };
        let job = job?;
        // Handed to the background now, with the write guard inside it: a task on the main thread
        // could still be waiting there when quit blocks that thread for the write count.
        let made = cx.background_executor().spawn(async move { job() });
        cx.spawn_in(window, async move |this, cx| {
            let made = made.await;
            this.update_in(cx, |this, window, cx| {
                if let Err(why) = made.and_then(|to| this.open_copy(to, window, cx)) {
                    let title = file_ops::failed_title("duplicate", &[from]);
                    let _ = window.prompt(PromptLevel::Critical, &title, Some(&why), &["OK"], cx);
                }
            })
            .ok();
        })
        .detach();
        Ok(())
    }

    /// Opens the Duplicate copy at `to` as the tab beside the showing one.
    fn open_copy(&mut self, to: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let document = self.read_document(to, window, cx).map_err(|_| "the copy was made but couldn’t be opened")?;
        self.active_pane().clone().update(cx, |pane, cx| {
            let at = pane.active_index() + 1;
            pane.insert(document, at, window, cx);
        });
        Ok(())
    }

    /// Moves the showing tab's file into the folder `dir`, and the tab follows.
    pub fn file_move(&mut self, dir: &Path, window: &Window, cx: &mut Context<Self>) -> Result<(), String> {
        let (_, from) = self.file_tab(cx).ok_or("no file on disk is showing")?;
        if !dir.is_dir() {
            return Err(format!("not a folder: {}", dir.display()));
        }
        self.refuse_busy_file(&from, window, cx)?;
        let to = file_ops::move_into(&from, dir)?;
        self.file_moved(&from, &to, cx);
        Ok(())
    }

    /// File > Duplicate.
    fn duplicate_file(&mut self, _: &DuplicateFile, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, from)) = self.file_tab(cx) else { return };
        if let Err(why) = self.file_duplicate(window, cx) {
            let title = file_ops::failed_title("duplicate", &[from]);
            let _ = window.prompt(PromptLevel::Critical, &title, Some(&why), &["OK"], cx);
        }
    }

    /// File > Show in Finder: selects the showing tab's file in a Finder window.
    fn show_file_in_finder(&mut self, _: &ShowFileInFinder, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, path)) = self.file_tab(cx) {
            crate::native_menu::show_in_finder(&path);
        }
    }

    /// File > Copy path: puts the showing tab's full path on the clipboard.
    fn copy_file_path(&mut self, _: &CopyFilePath, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((_, path)) = self.file_tab(cx) {
            cx.write_to_clipboard(ClipboardItem::new_string(path.to_string_lossy().into_owned()));
        }
    }

    /// File > Move to: asks for a folder, then moves the showing tab's file into it.
    fn move_file(&mut self, _: &MoveFile, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_tab(cx).is_none() {
            return;
        }
        let picked = cx.prompt_for_paths(PathPromptOptions { files: false, directories: true, multiple: false, prompt: Some("Move".into()) });
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else { return };
            let Some(dir) = paths.into_iter().next() else { return };
            this.update_in(cx, |this, window, cx| {
                let Some((_, from)) = this.file_tab(cx) else { return };
                let title = file_ops::failed_title("move", &[from]);
                if let Err(why) = this.file_move(&dir, window, cx) {
                    let _ = window.prompt(PromptLevel::Critical, &title, Some(&why), &["OK"], cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Cmd-E: the open file's selection becomes the find text, here and on the Mac's find
    /// pasteboard, without opening the find panel.
    fn use_selection_for_find(&mut self, _: &UseSelectionForFind, _: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.document(cx).and_then(|document| document.read(cx).built_editor().cloned()) else {
            return;
        };
        editor.update(cx, |editor, cx| editor.use_selection_for_find(cx));
    }

    /// Option-Cmd-E: the open file's selection fills the find bar's Replace field and the replace
    /// row shows, opening the bar when it is closed. The find text stays.
    fn use_selection_for_replace(&mut self, _: &UseSelectionForReplace, _: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.document(cx).and_then(|document| document.read(cx).built_editor().cloned()) else {
            return;
        };
        gpui_kit::component::input::use_selection_for_replace(&editor, cx);
    }

    /// File > Save As: asks where to save and saves the open file there.
    fn save_as(&mut self, _: &SaveAs, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(document) = self.document(cx) {
            document.update(cx, |document, cx| document.save_as(window, cx)).detach();
        }
    }

    /// The showing Markdown tab's current text (edits included), its file and the tree's folder;
    /// `None` for any other tab or no tab.
    fn export_source(&self, cx: &App) -> Option<(String, Option<PathBuf>, Option<PathBuf>)> {
        let document = self.document(cx)?;
        let document = document.read(cx);
        let editor = document.built_editor().filter(|_| document.has_preview())?;
        Some((editor.read(cx).text().to_string(), document.path.clone(), document.tree_root.clone()))
    }

    /// File > Export as HTML: asks where (the file's folder, `<name>.html`), then exports.
    fn export_html(&mut self, _: &ExportHtml, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, path, root)) = self.export_source(cx) else { return };
        let dir = path.as_deref().and_then(Path::parent).map(Path::to_path_buf).or(root).or_else(std::env::home_dir).unwrap_or_default();
        let name = format!("{}.html", crate::export_html::title_of(path.as_deref()));
        let picked = cx.prompt_for_new_path(&dir, Some(&name));
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(target))) = picked.await else { return };
            this.update_in(cx, |this, window, cx| this.export_html_to(target, window, cx)).ok();
        })
        .detach();
    }

    /// Writes the showing Markdown tab as one self-contained page at `target` (the control
    /// socket's `export-html` comes here too, since a native panel can't be driven). The page is
    /// built and written off the main thread; the Markdown file itself is never touched, and a
    /// failed write comes as an alert.
    pub fn export_html_to(&mut self, target: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let Some((text, path, _)) = self.export_source(cx) else { return };
        let base = path.as_deref().and_then(Path::parent).map(Path::to_path_buf);
        let title = crate::export_html::title_of(path.as_deref());
        cx.spawn_in(window, async move |this, cx| {
            let to = target.clone();
            let written = cx
                .background_executor()
                .spawn(async move { crate::export_html::write(&to, &crate::export_html::document(&text, &title, base.as_deref())) })
                .await;
            if let Err(err) = written {
                this.update_in(cx, |_, window, cx| {
                    let alert = file_ops::failed_title("export", &[target.clone()]);
                    let _ = window.prompt(PromptLevel::Critical, &alert, Some(&file_ops::explain("export", &target, &err)), &["OK"], cx);
                })
                .ok();
            }
        })
        .detach();
    }

    /// File > Save all (Option-Cmd-S): saves every file of this window with unsaved edits.
    fn save_all_files(&mut self, _: &SaveAll, window: &mut Window, cx: &mut Context<Self>) {
        self.save_all(window, cx).detach();
    }

    /// File > Revert to saved: asks, then puts the open file's text from disk back.
    fn revert_to_saved(&mut self, _: &RevertToSaved, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(document) = self.document(cx) {
            if document.read(cx).can_revert() {
                document.update(cx, |document, cx| document.revert(window, cx));
            } else {
                self.refuse(window, cx);
            }
        }
    }

    /// A refusal of a tab command: the beep, and the active pane's tab bar shakes beside it.
    fn refuse(&self, window: &Window, cx: &mut Context<Self>) {
        crate::preview::render::beep();
        self.active_pane().clone().update(cx, |pane, cx| pane.shake(window, cx));
    }

    /// Cmd-Shift-W: closes this window, asking about unsaved files first as its close button does.
    fn close_window(&mut self, _: &CloseWindow, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = cx.entity();
        // Deferred: `confirm_close` reads this workspace, which is being updated now.
        window.defer(cx, move |window, cx| {
            if confirm_close(workspace, window, cx) {
                finish_close(window, cx);
            }
        });
    }

    fn minimize(&mut self, _: &Minimize, window: &mut Window, _: &mut Context<Self>) {
        window.minimize_window();
    }

    fn zoom(&mut self, _: &Zoom, window: &mut Window, _: &mut Context<Self>) {
        window.zoom_window();
    }

    /// Opens `path` in a tab of the active pane: the tab already showing the file (in any pane)
    /// comes forward as it was left (unsaved edits, undo history and scroll included); otherwise
    /// the file is read from disk into a new tab. A file that can't be read leaves the current
    /// tab showing and says why. Returns the document showing the file, `None` when it can't be read.
    pub(crate) fn open_file(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<Document>> {
        self.open_file_at(path, None, window, cx).ok()
    }

    /// `open_file`, then the caret to `caret` (counted from 1) when given, also for a tab that was
    /// already open.
    pub(crate) fn open_file_at(&mut self, path: PathBuf, caret: Option<Caret>, window: &mut Window, cx: &mut Context<Self>) -> std::io::Result<Entity<Document>> {
        // A scrap draft is an Untitled page that writes its own file: a second tab on that file
        // would fight it, so it opens (or comes forward) as the page.
        if drafts::is_draft(&path) {
            let document = self.open_draft(path, window, cx).ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "the draft isn’t a text file"))?;
            // A page in another window can't be moved to a caret from this one.
            if let Some(caret) = caret.filter(|_| self.documents(cx).contains(&document)) {
                document.update(cx, |document, cx| document.go_to(caret, window, cx));
            }
            return Ok(document);
        }
        // The real path, so `..`, `/tmp`, letter case and symlinks all find the tab already open.
        let path = path.canonicalize().unwrap_or(path);
        if let Some(document) = self.show_open_anywhere(&path, true, caret, window, cx) {
            return Ok(document);
        }
        let document = self.read_document(path, window, cx)?;
        // A blank Untitled page showing is replaced by the file.
        self.active_pane().clone().update(cx, |pane, cx| pane.add(document.clone(), true, window, cx));
        if let Some(caret) = caret {
            document.update(cx, |document, cx| document.go_to(caret, window, cx));
        }
        Ok(document)
    }

    /// A single click on a file in the explorer: opens `path` as the active pane's preview tab,
    /// which the next such click replaces. A file already open in a tab just comes forward, and
    /// stays as that tab was (a preview stays a preview). A scrap draft opens as its Untitled page.
    pub(crate) fn open_preview(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if drafts::is_draft(&path) {
            self.open_draft(path, window, cx);
            return;
        }
        let path = path.canonicalize().unwrap_or(path);
        if self.show_open_anywhere(&path, false, None, window, cx).is_some() {
            return;
        }
        if let Ok(document) = self.read_document(path, window, cx) {
            self.active_pane().clone().update(cx, |pane, cx| pane.add_preview(document, window, cx));
        }
    }

    /// Opens `path` in the pane right of the active one, splitting right when there is none. A
    /// scrap draft opens as its Untitled page in the active pane.
    pub(crate) fn open_to_side(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        if drafts::is_draft(&path) {
            self.open_draft(path, window, cx);
            return;
        }
        let path = path.canonicalize().unwrap_or(path);
        if self.show_open_anywhere(&path, true, None, window, cx).is_some() {
            return;
        }
        let Ok(document) = self.read_document(path, window, cx) else { return };
        match self.neighbor(self.active, Direction::Right) {
            Some(id) => {
                let pane = self.slot(id).map(|slot| slot.pane.clone());
                if let Some(pane) = pane {
                    self.active = id;
                    pane.update(cx, |pane, cx| pane.add(document, true, window, cx));
                    self.sync(window, cx);
                }
            }
            None => {
                self.open_beside(document, false, self.active, SplitAxis::Right, false, window, cx);
            }
        }
    }

    /// `show_open` in this window, else in another one (that window comes forward too, except in a
    /// background launch): one file is one tab app-wide. `caret` goes to the tab in whichever
    /// window shows it. `path` is the real path.
    fn show_open_anywhere(&mut self, path: &Path, keep: bool, caret: Option<Caret>, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<Document>> {
        if let Some(document) = self.show_open(path, keep, window, cx) {
            if let Some(caret) = caret {
                document.update(cx, |document, cx| document.go_to(caret, window, cx));
            }
            return Some(document);
        }
        for (handle, workspace) in open_workspaces_except(Some(window.window_handle()), cx) {
            let shown = cx.update_window(handle, |_, other, cx| {
                let shown = workspace.update(cx, |workspace, cx| workspace.show_open(path, keep, other, cx));
                if let Some(document) = &shown {
                    if let Some(caret) = caret {
                        document.update(cx, |document, cx| document.go_to(caret, other, cx));
                    }
                    if std::env::var_os("GPUI_EDITOR_BACKGROUND").is_none() {
                        other.activate_window();
                    }
                }
                shown
            });
            if let Ok(Some(document)) = shown {
                return Some(document);
            }
        }
        None
    }

    /// Brings the tab showing `path` forward, in whichever pane has it (the active pane first),
    /// and makes that pane the active one; with `keep`, a preview tab there becomes a normal tab.
    /// `None` when no pane has the file open.
    fn show_open(&mut self, path: &Path, keep: bool, window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<Document>> {
        let mut order = self.tree.live().to_vec();
        order.sort_by_key(|id| *id != self.active);
        for id in order {
            let Some(slot) = self.slot(id) else { continue };
            let pane = slot.pane.clone();
            let open = pane.read(cx).index_of_path(path, cx);
            if let Some(ix) = open {
                self.active = id;
                pane.update(cx, |pane, cx| {
                    pane.activate(ix, window, cx);
                    if keep {
                        pane.keep(ix, cx);
                    }
                });
                self.sync(window, cx);
                return pane.read(cx).active_document().cloned();
            }
        }
        None
    }

    /// The document for the file at `path`, read from disk; a file that can't be read says why.
    fn read_document(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> std::io::Result<Entity<Document>> {
        // The time before the text, so a write in between shows as a change later.
        let mtime = path_mtime(Some(&path));
        match encoding::read_file(&path) {
            Ok(decoded) => {
                let soft_wrap = settings::get(cx).editor.soft_wrap;
                // `Document::new` titles the window itself, so the title last sent no longer says what it shows.
                self.sent.0 = None;
                Ok(cx.new(|cx| Document::new(Some(path), decoded, mtime, soft_wrap, window, cx)))
            }
            Err(err) if err.kind() == std::io::ErrorKind::FileTooLarge => {
                let soft_wrap = settings::get(cx).editor.soft_wrap;
                Ok(cx.new(|_| Document::unread(path, mtime, soft_wrap)))
            }
            Err(err) => {
                let detail = crate::file_ops::explain("open", &path, &err);
                let _ = window.prompt(PromptLevel::Critical, &file_ops::failed_title("open", &[path.clone()]), Some(&detail), &["OK"], cx);
                Err(err)
            }
        }
    }

    /// Opens an empty Untitled page in a new tab of the active pane and shows it.
    pub(crate) fn new_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<Document> {
        let soft_wrap = settings::get(cx).editor.soft_wrap;
        let document = cx.new(|cx| Document::new(None, Decoded::utf8(String::new()), None, soft_wrap, window, cx));
        // `Document::new` titles the window itself, so the title last sent no longer says what it shows.
        self.sent.0 = None;
        self.active_pane().clone().update(cx, |pane, cx| pane.add(document.clone(), false, window, cx));
        document
    }

    /// Cmd-T and Cmd-N.
    fn new_tab_action(&mut self, _: &NewTab, window: &mut Window, cx: &mut Context<Self>) {
        self.new_tab(window, cx);
    }

    /// A new Untitled page that starts with `text`, typed in as typed characters are (so it is a
    /// draft at once): the showing page takes it when it is blank, else a new tab does.
    pub(crate) fn new_page_with(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> Entity<Document> {
        let document = self.document(cx).filter(|document| document.read(cx).is_blank()).unwrap_or_else(|| self.new_tab(window, cx));
        if !text.is_empty() {
            self.type_text(text, window, cx);
        }
        document
    }

    /// File > New from template: a new Untitled page with the template's text, its `{{date}}`
    /// filled in and its `{{title}}` and `{{cursor}}` spots kept as tab stops (`tab_stops.rs`).
    fn new_from_template(&mut self, action: &NewFromTemplate, window: &mut Window, cx: &mut Context<Self>) {
        match templates::text(&action.template) {
            Ok(text) => {
                let expanded = crate::tab_stops::expand(&text, &crate::tab_stops::today());
                let document = self.new_page_with(&expanded.text, window, cx);
                let markdown = templates::is_markdown(&action.template);
                document.update(cx, |document, cx| document.set_markdown_template(markdown, cx));
                if !expanded.stops.is_empty() {
                    let editor = document.update(cx, |document, cx| document.editor(window, cx));
                    crate::tab_stops::begin(&editor, expanded, window, cx);
                }
            }
            Err(why) => {
                let _ = window.prompt(PromptLevel::Critical, "Couldn’t use the template", Some(&why), &["OK"], cx);
            }
        }
    }

    /// Closes the active pane's showing tab, asking first when it has unsaved edits. `false` when
    /// the pane has no tab.
    pub(crate) fn close_active_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let pane = self.active_pane().clone();
        if pane.read(cx).len() == 0 {
            return false;
        }
        pane.update(cx, |pane, cx| {
            pane.later(window, cx, |pane, window, cx| {
                let ix = pane.active_index();
                pane.close(ix, window, cx);
            })
        });
        true
    }

    /// Cmd-W: closes the showing tab; an empty pane closes the window.
    fn close_tab(&mut self, _: &CloseTab, window: &mut Window, cx: &mut Context<Self>) {
        // The scratch window's one page closes with its window, in one press.
        if self.scratch {
            self.close_window(&CloseWindow, window, cx);
            return;
        }
        if !self.close_active_tab(window, cx) {
            self.close_window(&CloseWindow, window, cx);
        }
    }

    /// Remembers a tab that closed in pane `pane`, the newest last; only the last `CLOSED_KEPT`
    /// are kept, and a file is listed once.
    fn remember_closed(&mut self, pane: PaneId, closed: &ClosedTab, _: &mut Context<Self>) {
        push_closed(&mut self.closed, pane, ClosedTab { path: closed.path.clone(), index: closed.index, caret: closed.caret, pinned: closed.pinned });
    }

    /// Cmd-Shift-T: reopens the file of the tab closed last whose file still exists and isn't
    /// open, in the pane it closed in (the active pane if that is gone), at its old slot and with
    /// its caret. Files that are gone are dropped from the list. `false` when nothing came back.
    pub(crate) fn reopen_closed_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        while let Some((pane_id, closed)) = self.closed.pop() {
            let open = self.panes.iter().any(|slot| slot.pane.read(cx).index_of_path(&closed.path, cx).is_some());
            if open || !closed.path.exists() {
                continue;
            }
            let Ok(document) = self.read_document(closed.path.clone(), window, cx) else { return false };
            let id = if self.tree.live().contains(&pane_id) { pane_id } else { self.active };
            let Some(pane) = self.slot(id).map(|slot| slot.pane.clone()) else { return false };
            self.active = id;
            pane.update(cx, |pane, cx| {
                if closed.pinned {
                    pane.insert_pinned(document.clone(), closed.index, window, cx)
                } else {
                    pane.insert(document.clone(), closed.index, window, cx)
                }
            });
            if let Some(caret) = closed.caret {
                document.update(cx, |document, cx| document.go_to(caret, window, cx));
            }
            self.sync(window, cx);
            return true;
        }
        false
    }

    /// Whether Cmd-Shift-T has a tab to bring back: a closed one that isn't open. A stat per closed
    /// path would run on every menu refresh, so a file that is gone shows here and is skipped
    /// (and dropped) by `reopen_closed_tab`.
    pub(crate) fn can_reopen_closed(&self, cx: &App) -> bool {
        self.closed
            .iter()
            .any(|(_, closed)| !self.panes.iter().any(|slot| slot.pane.read(cx).index_of_path(&closed.path, cx).is_some()))
    }

    /// File > Close other tabs: closes every tab but the showing one and the pinned ones.
    fn close_other_tabs(&mut self, _: &CloseOtherTabs, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().clone();
        let id = pane.read(cx).active_document().map(Entity::entity_id);
        match id.filter(|id| pane.read(cx).can_close_others(*id)) {
            Some(id) => pane.update(cx, |pane, cx| pane.close_others(id, window, cx)),
            None => self.refuse(window, cx),
        }
    }

    /// File > Close tabs to the right: closes the unpinned tabs right of the showing one.
    fn close_tabs_to_the_right(&mut self, _: &CloseTabsToTheRight, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().clone();
        let id = pane.read(cx).active_document().map(Entity::entity_id);
        match id.filter(|id| pane.read(cx).can_close_right(*id)) {
            Some(id) => pane.update(cx, |pane, cx| pane.close_right(id, window, cx)),
            None => self.refuse(window, cx),
        }
    }

    /// File > Close tabs to the left: closes the unpinned tabs left of the showing one.
    fn close_tabs_to_the_left(&mut self, _: &CloseTabsToTheLeft, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().clone();
        let id = pane.read(cx).active_document().map(Entity::entity_id);
        match id.filter(|id| pane.read(cx).can_close_left(*id)) {
            Some(id) => pane.update(cx, |pane, cx| pane.close_left(id, window, cx)),
            None => self.refuse(window, cx),
        }
    }

    /// How many tabs the window holds in all its panes.
    pub(crate) fn tab_count(&self, cx: &App) -> usize {
        self.panes.iter().map(|slot| slot.pane.read(cx).len()).sum()
    }

    /// Window > Move tab to new window: the showing tab opens its own window (what dragging it off the desktop does). A window
    /// holding only this tab has nothing to leave behind, so the key beeps.
    fn move_active_tab_to_new_window(&mut self, _: &MoveTabToNewWindow, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_pane().read(cx).active_document().map(Entity::entity_id) else {
            self.refuse(window, cx);
            return;
        };
        self.move_to_new_window(id, window, cx);
    }

    /// Opens tab `id` in a new window once this update is done (the move updates both windows); beeps when it is the window's only tab.
    fn move_to_new_window(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab_count(cx) < 2 {
            self.refuse(window, cx);
            return;
        }
        let source = self.window;
        cx.defer(move |cx| {
            if let Err(reason) = move_tab_to(source, id, Landing::New(None), cx) {
                eprintln!("finch: couldn't move the tab: {reason}");
            }
        });
    }

    /// Window > Move tab earlier or Move tab later: the showing tab trades places with its neighbour in its group (the pins, or the
    /// others). At the group's end the key beeps.
    fn step_active_tab(&mut self, by: isize, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().clone();
        let stepped = pane.update(cx, |pane, cx| pane.active_document().map(Entity::entity_id).is_some_and(|id| pane.step_tab(id, by, cx)));
        if !stepped {
            self.refuse(window, cx);
        }
    }

    /// Window > Pin tab or Unpin tab: flips the showing tab's pin.
    fn toggle_pin_tab(&mut self, _: &TogglePinTab, window: &mut Window, cx: &mut Context<Self>) {
        let pane = self.active_pane().clone();
        let Some(id) = pane.read(cx).active_document().map(Entity::entity_id) else {
            self.refuse(window, cx);
            return;
        };
        pane.update(cx, |pane, cx| if pane.is_pinned_id(id) { pane.unpin_id(id, cx) } else { pane.pin_id(id, cx) });
    }

    /// Cmd-Shift-[ : the previous tab by position.
    fn prev_tab(&mut self, _: &PrevTab, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane().clone().update(cx, |pane, cx| pane.step(-1, window, cx));
    }

    /// Cmd-Shift-] : the next tab by position.
    fn next_tab(&mut self, _: &NextTab, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane().clone().update(cx, |pane, cx| pane.step(1, window, cx));
    }

    /// Ctrl-Shift-Tab: one step forward in the recent list (see `Pane::step_recent`).
    fn prev_recent_tab(&mut self, _: &PrevRecentTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_recent(-1, window, cx);
    }

    /// Ctrl-Tab: one step back in the recent list.
    fn next_recent_tab(&mut self, _: &NextRecentTab, window: &mut Window, cx: &mut Context<Self>) {
        self.step_recent(1, window, cx);
    }

    pub(crate) fn step_recent(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane().clone().update(cx, |pane, cx| pane.step_recent(step, window, cx));
    }

    /// Control was let go (or the control socket says so): a Ctrl-Tab walk confirms its tab.
    pub(crate) fn end_switch(&mut self, cx: &mut Context<Self>) {
        for slot in &self.panes {
            slot.pane.update(cx, |pane, cx| pane.end_switch(cx));
        }
    }

    /// Shows the `n`th tab of the active pane, counting from 1; 9 is the last.
    pub(crate) fn show_nth(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane().clone().update(cx, |pane, cx| pane.show_nth(n, window, cx));
    }

    /// Shows the active pane's tab at `ix`, counting from 0.
    pub(crate) fn activate_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.active_pane().clone().update(cx, |pane, cx| pane.activate(ix, window, cx));
    }

    /// Types `text` into the showing editor at its cursor, for the control socket. `false` while
    /// the pane has no tab.
    pub fn type_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(document) = self.document(cx) else { return false };
        let editor = document.update(cx, |document, cx| document.editor(window, cx));
        editor.update(cx, |state, cx| state.insert(text.to_owned(), window, cx));
        true
    }

    /// The active pane's bookkeeping sizes (`Pane::state_sizes`, `EditorPane::state_sizes`) and
    /// its editor's wrap width, for the control socket's `tab-state`.
    pub fn state_sizes(&self, cx: &App) -> ([usize; 4], [usize; 4], Option<f32>) {
        let slot = self.slot(self.active).unwrap_or(&self.panes[0]);
        let (editor, wrap) = slot.editor.read(cx).state_sizes(cx);
        (slot.pane.read(cx).state_sizes(), editor, wrap)
    }

    /// Each tab of the active pane: its title and whether it has unsaved edits, and the showing
    /// tab's place, for the control socket.
    pub fn tab_summary(&self, cx: &App) -> (Vec<(String, bool, bool, bool)>, usize) {
        self.summary_of(self.active, cx)
    }

    fn summary_of(&self, id: PaneId, cx: &App) -> (Vec<(String, bool, bool, bool)>, usize) {
        let Some(slot) = self.slot(id) else { return (Vec::new(), 0) };
        let pane = slot.pane.read(cx);
        let tabs = pane
            .documents()
            .enumerate()
            .map(|(ix, document)| {
                let document = document.read(cx);
                (document.title(), document.shows_dot(), pane.is_preview(ix), pane.is_pinned(ix))
            })
            .collect();
        (tabs, pane.active_index())
    }

    /// Every pane's tabs as `tab_summary` gives them, in reading order, and the active pane's
    /// place among them (counting from 0), for the control socket.
    pub fn pane_summary(&self, cx: &App) -> (Vec<(Vec<(String, bool, bool, bool)>, usize)>, usize) {
        let live = self.tree.live();
        let panes = live.iter().map(|id| self.summary_of(*id, cx)).collect();
        (panes, live.iter().position(|id| *id == self.active).unwrap_or(0))
    }

    /// Cmd-\ and Cmd-Shift-\: moves the active pane's showing tab into a new pane beside it, on the
    /// right or below. A pane with one tab has none to give away and stays as it is (`false`).
    pub(crate) fn split(&mut self, axis: SplitAxis, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let from = self.active;
        let pane = self.active_pane().clone();
        if pane.read(cx).len() < 2 {
            return false;
        }
        let pinned = pane.read(cx).is_pinned(pane.read(cx).active_index());
        // A copy of the tab glides from its place in this strip to its slot in the new pane; one still gliding in from an earlier carry starts from where it is drawn, at its pace.
        let carried = pane.read(cx).active_document().map(Entity::entity_id).and_then(|id| pane.read(cx).carry_start(id).map(|start| (id, start)));
        let Some(document) = pane.update(cx, |pane, cx| pane.take_active(cx)) else { return false };
        // The moved editor is drawn by the new pane now, not fading out of this one.
        if let Some(slot) = self.slot(from) {
            // A tab never shown has no editor here to forget, and building one would read the file in this window.
            let moved = document.read(cx).built_editor().map(Entity::entity_id);
            slot.editor.clone().update(cx, |pane, _| {
                pane.blank();
                if let Some(moved) = moved {
                    pane.forget(moved);
                }
            });
        }
        let created = self.open_beside(document, pinned, from, axis, false, window, cx);
        if let Some((id, (from, lift, speed))) = carried {
            self.glide_to(created, id, from, lift, speed, motion::drawn_now(window.window_handle().window_id()), cx);
        }
        true
    }

    /// A tab's right-click menu > Split right or Split down: that tab comes forward in its pane, which takes the keyboard, and goes
    /// into a new pane beside it. A pane with only this tab beeps, as the keys do.
    fn split_tab(&mut self, id: EntityId, pane_id: PaneId, axis: SplitAxis, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pane) = self.slot(pane_id).map(|slot| slot.pane.clone()) else { return };
        pane.update(cx, |pane, cx| pane.activate_id(id, window, cx));
        self.set_active(pane_id, false, window, cx);
        if !self.split(axis, window, cx) {
            self.refuse(window, cx);
        }
    }

    /// Opens `document` in a new pane next to `from` (before it with `before`), which becomes the
    /// active pane. Returns the new pane's name.
    fn open_beside(&mut self, document: Entity<Document>, pinned: bool, from: PaneId, axis: SplitAxis, before: bool, window: &mut Window, cx: &mut Context<Self>) -> PaneId {
        let pane = cx.new(|cx| Pane::empty(cx));
        let editor = document.update(cx, |document, cx| document.editor(window, cx));
        let id = self.add_slot(pane.clone(), editor, window, cx);
        self.tree.split(from, id, axis, before, motion::drawn_now(window.window_handle().window_id()));
        self.active = id;
        let moved = document.entity_id();
        pane.update(cx, |pane, cx| {
            pane.add(document, false, window, cx);
            if pinned {
                pane.pin_id(moved, cx);
            }
        });
        self.sync(window, cx);
        id
    }

    /// The pane holding document `id`.
    fn holder_of(&self, id: EntityId, cx: &App) -> Option<PaneId> {
        self.panes.iter().find(|slot| slot.pane.read(cx).documents().any(|document| document.entity_id() == id)).map(|slot| slot.id)
    }

    /// Whether no pane in the window holds a tab.
    pub(crate) fn is_empty(&self, cx: &App) -> bool {
        self.panes.iter().all(|slot| slot.pane.read(cx).len() == 0)
    }

    /// Takes tab `id` out of whichever pane holds it, to put it in another window: its editor stops
    /// drawing here, and a pane left with no tab closes by the usual rule (the last one stays).
    /// Gives the document and whether the tab was pinned.
    pub(crate) fn release_tab(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) -> Option<(Entity<Document>, bool)> {
        let holder = self.holder_of(id, cx)?;
        let slot = self.slot(holder)?;
        let (pane, editor) = (slot.pane.clone(), slot.editor.clone());
        let pinned = pane.read(cx).is_pinned_id(id);
        let document = pane.update(cx, |pane, cx| pane.take(id, cx))?;
        // The moved editor is drawn by its new window now, not fading out of this one.
        editor.update(cx, |pane, _| pane.blank());
        // A tab never shown has no editor here: building one would read the file (and ask the big-file question) in this window.
        let moved_editor = document.read(cx).built_editor().cloned();
        let had_keyboard = moved_editor.as_ref().is_some_and(|editor| editor.read(cx).focus_handle(cx).is_focused(window));
        if let Some(moved) = moved_editor.map(|editor| editor.entity_id()) {
            editor.update(cx, |pane, _| pane.forget(moved));
        }
        if pane.read(cx).len() == 0 {
            self.close_pane(holder, false, window, cx);
        }
        self.sync(window, cx);
        if had_keyboard {
            self.active_pane().clone().update(cx, |pane, cx| pane.focus_editor(window, cx));
        }
        Some((document, pinned))
    }

    /// Puts a tab that came from another window in pane `target` at slot `at` (the end when past
    /// the last; pinned tabs stay pinned) and shows it. The document's window watches move here.
    pub(crate) fn adopt_tab(&mut self, document: Entity<Document>, pinned: bool, target: PaneId, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.tree.live().contains(&target) { target } else { self.active };
        let Some(pane) = self.slot(target).map(|slot| slot.pane.clone()) else { return };
        document.update(cx, |document, cx| document.rehome(window, cx));
        let id = document.entity_id();
        self.active = target;
        if at != usize::MAX {
            pane.update(cx, |pane, _| pane.take_gap());
        }
        let keeper = document.clone();
        pane.update(cx, |pane, cx| {
            pane.insert(document, at, window, cx);
            if pinned {
                pane.pin_id(id, cx);
            }
        });
        self.sync(window, cx);
        Self::settle_twin(keeper, cx);
    }

    /// Puts tabs that came from another window at the end of pane `target` (pinned ones pinned) without showing any, and keeps the tab
    /// showing there showing: a tab never shown stays unread. The documents' window watches move here. Twin copies of a file are the
    /// caller's to settle (`settle_merged_twins`).
    pub(crate) fn adopt_tabs(&mut self, tabs: Vec<(Entity<Document>, bool)>, target: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        for (document, _) in &tabs {
            document.update(cx, |document, cx| document.rehome(window, cx));
        }
        self.append_tabs(tabs, target, window, cx);
    }

    /// Appends `tabs` to pane `target` (the active pane when it is gone) with no tab shown on the way, then shows the tab the pane had
    /// showing (its first new one when it had none).
    fn append_tabs(&mut self, tabs: Vec<(Entity<Document>, bool)>, target: PaneId, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.tree.live().contains(&target) { target } else { self.active };
        let Some(pane) = self.slot(target).map(|slot| slot.pane.clone()) else { return };
        let showing = pane.read(cx).active_document().map(Entity::entity_id).or_else(|| tabs.first().map(|(document, _)| document.entity_id()));
        self.active = target;
        pane.update(cx, |pane, cx| {
            for (document, pinned) in tabs {
                pane.append(document, pinned, cx);
            }
        });
        if let Some(id) = showing {
            pane.update(cx, |pane, cx| pane.activate_id(id, window, cx));
        }
        self.sync(window, cx);
    }

    /// The file of `keeper` may already be a tab here or elsewhere. A clean moved tab gives way to a twin with edits; two with edits both
    /// stay; otherwise the usual twin rule (the clean twin closes). Runs once this update is done.
    fn settle_twin(keeper: Entity<Document>, cx: &mut Context<Self>) {
        if let Some(path) = keeper.read(cx).path.clone() {
            cx.defer(move |cx| {
                let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
                let wanted = real(&path);
                let twin_edited = open_workspaces(cx).iter().any(|(_, workspace)| {
                    workspace.read(cx).documents(cx).iter().any(|document| {
                        *document != keeper && {
                            let document = document.read(cx);
                            document.shows_dot() && document.path.as_deref().map(real).as_ref() == Some(&wanted)
                        }
                    })
                });
                if !twin_edited {
                    drop_twin_tabs(&keeper, &path, cx);
                } else if !keeper.read(cx).shows_dot() {
                    keeper.update(cx, |_, cx| cx.emit(crate::document::DropTab));
                }
            });
        }
    }

    /// The pane under `local` (a point in this window) and the slot a tab dropped there takes:
    /// between the tabs when `local` is over the pane's bar, else the end of its tabs.
    pub(crate) fn landing_at(&self, local: Point<Pixels>, cx: &App) -> Option<(PaneId, usize)> {
        let live = self.tree.live();
        let slot = self.panes.iter().filter(|slot| live.contains(&slot.id)).find(|slot| slot.bounds.get().contains(&local))?;
        let pane = slot.pane.read(cx);
        let in_bar = local.y < slot.bounds.get().origin.y + self.band(slot.id);
        Some((slot.id, if in_bar { pane.drop_slot(local.x).max(pane.pinned_count()) } else { usize::MAX }))
    }

    /// The mouse went up outside this window. A tab being dragged from it goes where it was let go:
    /// into the Finch window under the pointer (on any display), else a new window opened there. The
    /// move waits for this update to finish, since it updates other windows.
    fn let_go_outside(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if Bounds::new(Point::default(), window.viewport_size()).contains(&event.position) {
            return;
        }
        let Some(id) = self.panes.iter().find_map(|slot| slot.pane.read(cx).dragging()) else { return };
        let Some(point) = crate::app::screen_frame(window, cx).map(|frame| frame.origin + event.position) else { return };
        let (source, place) = (self.window, crate::app::dropped_placement(point, cx));
        let card = self.panes.iter().find_map(|slot| slot.pane.read(cx).card()).map(|card| Bounds { origin: point + card.origin, size: card.size });
        cx.defer(move |cx| {
            let over = window_under(source, point, cx).and_then(|(handle, workspace, local)| {
                let (pane, at) = workspace.read(cx).landing_at(local, cx)?;
                Some(Landing::Window { handle, pane, at })
            });
            // The gap the drag opened in another window's bar stays only where the tab lands in it.
            if let Some((handle, pane)) = FORWARDED.take() {
                if !matches!(&over, Some(Landing::Window { handle: to, pane: into, .. }) if (*to, *into) == (handle, pane)) {
                    fold_gap(handle, pane, cx);
                }
            }
            CARD.set(card);
            if let Err(reason) = move_tab_to(source, id, over.unwrap_or(Landing::New(place)), cx) {
                eprintln!("finch: couldn't move the tab: {reason}");
            }
        });
    }

    /// A tab is dragged over this window (`TabDrag` moves). While the pointer is outside it, the
    /// Finch window under the pointer gets the drag too, so its bar opens a gap and follows; the
    /// gap folds when the pointer leaves that bar or comes back. macOS keeps the whole drag with
    /// the window it began in, so the other window never sees these moves itself.
    fn drag_moved(&mut self, event: &DragMoveEvent<TabDrag>, window: &mut Window, cx: &mut Context<Self>) {
        let outside = !Bounds::new(Point::default(), window.viewport_size()).contains(&event.event.position);
        let point = outside.then(|| crate::app::screen_frame(window, cx).map(|frame| frame.origin + event.event.position)).flatten();
        if point.is_none() && FORWARDED.get().is_none() {
            return;
        }
        let (source, pinned) = (self.window, event.drag(cx).pinned);
        cx.defer(move |cx| forward_gap(source, pinned, point, cx));
    }

    /// A tab dragged out of another window is over this one at `local`: the pane under it opens a
    /// gap if `local` is over its bar (folds it if not). Gives that pane.
    fn gap_for(&mut self, local: Point<Pixels>, pinned: bool, cx: &mut Context<Self>) -> Option<PaneId> {
        let live = self.tree.live();
        let slot = self.panes.iter().filter(|slot| live.contains(&slot.id)).find(|slot| slot.bounds.get().contains(&local))?;
        let bounds = slot.bounds.get();
        let strip = Bounds::new(bounds.origin, size(bounds.size.width, self.band(slot.id)));
        let (id, pane) = (slot.id, slot.pane.clone());
        pane.update(cx, |pane, cx| pane.gap_over(pinned, strip, local, cx));
        Some(id)
    }

    /// The `n`th live pane in reading order, counting from 1.
    pub fn pane_at(&self, n: usize) -> Option<PaneId> {
        n.checked_sub(1).and_then(|ix| self.tree.live().get(ix).copied())
    }

    /// The document of pane `pane`'s `n`th tab, counting from 1.
    pub fn tab_id(&self, pane: PaneId, n: usize, cx: &App) -> Option<EntityId> {
        let slot = self.slot(pane)?;
        slot.pane.read(cx).documents().nth(n.checked_sub(1)?).map(Entity::entity_id)
    }

    /// A tab or files dropped on pane `target` at `zone`. On a bar or the middle they join that
    /// pane's tabs (a file already open elsewhere brings its tab along); on an edge they open in
    /// a new pane beside it. A pane that gives away its last tab closes like any emptied pane.
    /// Folders are ignored, and a drop that opens nothing beeps. Gives the pane the first tab landed in, `None` when nothing was placed.
    pub(crate) fn drop_on(&mut self, dropped: Dropped, target: PaneId, zone: Zone, window: &mut Window, cx: &mut Context<Self>) -> Option<PaneId> {
        let Some(target_pane) = self.slot(target).map(|slot| slot.pane.clone()).filter(|_| self.tree.live().contains(&target)) else { return None };
        // Image files dropped on the middle of a pane showing a saved Markdown file are copied in beside it (`paste_image.rs`), not opened.
        if let (Dropped::Files(paths), Zone::Middle) = (&dropped, zone) {
            let showing = target_pane.read(cx).active_document().cloned();
            let note = showing.as_ref().and_then(|document| document.read(cx).path.clone());
            if crate::paste_image::takes_drop(note.as_deref(), paths) {
                if let (Some(note), Some(editor)) = (note, showing.and_then(|document| document.read(cx).built_editor().cloned())) {
                    crate::paste_image::drop_files(editor, note, paths.clone(), window, cx);
                    return None;
                }
            }
        }
        // Each thing dropped, with the pane holding its tab (none for a file not open yet).
        let is_files = matches!(dropped, Dropped::Files(_));
        let mut items: Vec<(Entity<Document>, Option<PaneId>)> = Vec::new();
        // A dropped draft was raised in another window: something opened, so no beep.
        let mut raised = false;
        match dropped {
            Dropped::Tab(id) => {
                let Some(holder) = self.holder_of(id, cx) else { return None };
                let document = self.slot(holder).and_then(|slot| slot.pane.read(cx).documents().find(|document| document.entity_id() == id).cloned());
                let Some(document) = document else { return None };
                items.push((document, Some(holder)));
            }
            Dropped::Files(paths) => {
                let mut seen = HashSet::new();
                for path in paths {
                    let path = std::fs::canonicalize(&path).unwrap_or(path);
                    if !path.is_file() || !seen.insert(path.clone()) {
                        continue;
                    }
                    // A scrap draft is its Untitled page's file: it matches the page (here or in another window), never a file tab.
                    if drafts::is_draft(&path) {
                        let real = |file: &Path| file.canonicalize().unwrap_or_else(|_| file.to_path_buf());
                        let here = self.panes.iter().find_map(|slot| {
                            let pane = slot.pane.read(cx);
                            let document = pane.documents().find(|document| document.read(cx).tab_file().is_some_and(|file| real(&file) == path))?;
                            Some((document.clone(), slot.id))
                        });
                        if let Some((document, holder)) = here {
                            items.push((document, Some(holder)));
                        } else if self.open_tab_files(window, cx).iter().any(|file| real(file) == path) {
                            self.open_draft(path, window, cx);
                            raised = true;
                        } else {
                            match drafts::read(&path) {
                                Ok(text) => {
                                    let soft_wrap = settings::get(cx).editor.soft_wrap;
                                    items.push((cx.new(|_| Document::unbuilt_draft(path, text, soft_wrap)), None));
                                }
                                Err(_) => self.alert_unreadable_drafts(vec![path], window, cx),
                            }
                        }
                        continue;
                    }
                    let open = self.panes.iter().find_map(|slot| {
                        let pane = slot.pane.read(cx);
                        let ix = pane.index_of_path(&path, cx)?;
                        pane.documents().nth(ix).map(|document| (document.clone(), slot.id))
                    });
                    match open {
                        Some((document, holder)) => items.push((document, Some(holder))),
                        None => {
                            if let Ok(document) = self.read_document(path, window, cx) {
                                items.push((document, None));
                            }
                        }
                    }
                }
            }
        }
        if items.is_empty() {
            if is_files && !raised {
                crate::preview::render::beep();
                target_pane.update(cx, |pane, cx| pane.shake(window, cx));
            }
            return None;
        }
        // A pane with nothing left after the move has nothing to split from: its edge is its middle.
        let left_behind = target_pane.read(cx).documents().filter(|document| !items.iter().any(|(item, _)| item.entity_id() == document.entity_id())).count();
        let zone = if !matches!(zone, Zone::Bar(_) | Zone::Middle) && left_behind == 0 { Zone::Middle } else { zone };
        // The pane the first tab lands in.
        let mut landed = target;

        let mut documents = Vec::new();
        let mut emptied = Vec::new();
        for (document, holder) in items {
            let Some(holder) = holder else {
                documents.push((document, false));
                continue;
            };
            // A tab dropped on the middle of its own pane stays where it is.
            if zone == Zone::Middle && holder == target {
                self.active = target;
                target_pane.update(cx, |pane, cx| pane.activate_id(document.entity_id(), window, cx));
                continue;
            }
            let Some(slot) = self.slot(holder) else { continue };
            let (pane, editor) = (slot.pane.clone(), slot.editor.clone());
            let was_pinned = pane.read(cx).is_pinned_id(document.entity_id());
            let taken = pane.update(cx, |pane, cx| pane.take(document.entity_id(), cx));
            // The moved editor is drawn by its new pane now, not fading out of this one.
            editor.update(cx, |pane, _| pane.blank());
            // A tab never shown has no editor here to forget; building one would read the file in this window.
            if let Some(moved) = document.read(cx).built_editor().map(Entity::entity_id) {
                editor.update(cx, |pane, _| pane.forget(moved));
            }
            if pane.read(cx).len() == 0 && holder != target && !emptied.contains(&holder) {
                emptied.push(holder);
            }
            documents.extend(taken.map(|document| (document, was_pinned)));
        }

        match zone {
            Zone::Bar(_) | Zone::Middle => {
                let at = if let Zone::Bar(slot) = zone { slot } else { usize::MAX };
                self.active = target;
                for (n, (document, pinned)) in documents.into_iter().enumerate() {
                    let id = document.entity_id();
                    // A pinned tab lands pinned, at the end of the pins there.
                    target_pane.update(cx, |pane, cx| {
                        pane.insert(document, at.saturating_add(n), window, cx);
                        if pinned {
                            pane.pin_id(id, cx);
                        }
                    });
                }
            }
            edge => {
                let (axis, before) = match edge {
                    Zone::Left => (SplitAxis::Right, true),
                    Zone::Right => (SplitAxis::Right, false),
                    Zone::Top => (SplitAxis::Down, true),
                    _ => (SplitAxis::Down, false),
                };
                let mut documents = documents.into_iter();
                if let Some((first, pinned)) = documents.next() {
                    let created = self.open_beside(first, pinned, target, axis, before, window, cx);
                    landed = created;
                    if let Some(pane) = self.slot(created).map(|slot| slot.pane.clone()) {
                        for (document, pinned) in documents {
                            let id = document.entity_id();
                            pane.update(cx, |pane, cx| {
                                pane.add(document, false, window, cx);
                                if pinned {
                                    pane.pin_id(id, cx);
                                }
                            });
                        }
                    }
                }
            }
        }
        for id in emptied {
            self.close_pane(id, false, window, cx);
        }
        self.sync(window, cx);
        Some(landed)
    }

    /// For the control socket: drops `source` (a file path, or `tab:<pane>:<n>` for a tab,
    /// counting from 1) on the `n`th pane at `zone`.
    pub fn drop_by_name(&mut self, source: &str, pane: usize, zone: Zone, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let target = self.pane_at(pane).ok_or_else(|| format!("no pane {pane}"))?;
        let dropped = match source.strip_prefix("tab:") {
            Some(spec) => {
                let numbers: Vec<usize> = spec.split(':').filter_map(|word| word.parse().ok()).collect();
                let [from, n] = numbers[..] else { return Err("a tab is tab:<pane>:<tab>".into()) };
                let holder = self.pane_at(from).ok_or_else(|| format!("no pane {from}"))?;
                Dropped::Tab(self.tab_id(holder, n, cx).ok_or_else(|| format!("no tab {n} in pane {from}"))?)
            }
            None => Dropped::Files(vec![PathBuf::from(source)]),
        };
        // A tab held over a bar drops as the bar's own handler does it: its copy lands from where the ghost is, and a tab from another pane is handed on.
        if let (Dropped::Tab(id), Zone::Bar(_)) = (dropped.clone(), zone) {
            let held = self.held.as_ref().filter(|held| held.pane == target && held.tab == Some(id));
            if let (Some(bar), Some(holder), Some([x, y])) = (held.and_then(|held| held.bar.clone()), held.and_then(|held| held.lifted.clone()), self.held_point(target, zone)) {
                if let Some((_, width)) = holder.read(cx).ghost_look(id, cx) {
                    self.held = None;
                    let now = crate::motion::drawn_now(window.window_handle().window_id());
                    let from = Bounds::new(point(px(x), px(y)), size(width, crate::tokens::TAB_HEIGHT));
                    bar.update(cx, |pane, cx| {
                        pane.land(id, from, 1., now, cx);
                        pane.drop_on_bar(dropped, from.origin.x, cx);
                    });
                    cx.notify();
                    return Ok(());
                }
            }
        }
        let held = match (&dropped, zone) {
            (Dropped::Tab(_), Zone::Bar(_)) => None,
            (Dropped::Tab(id), _) => self.held.as_ref().filter(|held| held.pane == target && held.tab == Some(*id)).and_then(|held| held.lifted.clone()),
            _ => None,
        };
        // A tab held over a zone drops from where the ghost is: its copy glides into its new slot.
        if let (Dropped::Tab(id), Some(holder), Some([x, y])) = (dropped.clone(), held, self.held_point(target, zone)) {
            let width = holder.read(cx).ghost_look(id, cx).map(|(_, width)| width);
            if let Some(width) = width {
                self.release_drag(cx);
                let now = crate::motion::drawn_now(window.window_handle().window_id());
                let from = Bounds::new(point(px(x), px(y)), size(width, crate::tokens::TAB_HEIGHT));
                return match self.drop_on(dropped, target, zone, window, cx) {
                    Some(landed) => {
                        self.glide_to(landed, id, from, 1., [0.; 2], now, cx);
                        Ok(())
                    }
                    None => Err("nothing to place".into()),
                };
            }
        }
        if self.drop_on(dropped, target, zone, window, cx).is_some() { Ok(()) } else { Err("nothing to place".into()) }
    }

    /// Tab `id` was let go over pane `landed` with its ghost at `from`, `lift` of the way up: a copy
    /// of the ghost glides into the slot the tab takes there, waiting for it if the pane is new.
    fn glide_to(&mut self, landed: PaneId, id: EntityId, from: Bounds<Pixels>, lift: f32, speed: [f32; 2], now: Instant, cx: &mut Context<Self>) {
        if let Some(pane) = self.slot(landed).map(|slot| slot.pane.clone()) {
            pane.update(cx, |pane, cx| pane.land_after_drop(id, from, lift, speed, now, cx));
        }
    }

    /// For the control socket: holds a drag of `source` (as `drop_by_name` takes it) over `zone` of
    /// the `n`th pane until `release_drag`: the ghost under the pointer, the zone's preview and
    /// (over a bar) the dragged tab's slot, drawn through the state a real drag sets.
    pub fn hold_drag(&mut self, source: &str, pane: usize, zone: Zone, cx: &mut Context<Self>) -> Result<(), String> {
        let target = self.pane_at(pane).ok_or_else(|| format!("no pane {pane}"))?;
        self.release_drag(cx);
        let (ghost, lifted) = match source.strip_prefix("tab:") {
            Some(spec) => {
                let numbers: Vec<usize> = spec.split(':').filter_map(|word| word.parse().ok()).collect();
                let [from, n] = numbers[..] else { return Err("a tab is tab:<pane>:<tab>".into()) };
                let holder = self.pane_at(from).ok_or_else(|| format!("no pane {from}"))?;
                let id = self.tab_id(holder, n, cx).ok_or_else(|| format!("no tab {n} in pane {from}"))?;
                let holder = self.slot(holder).ok_or_else(|| format!("no pane {from}"))?.pane.clone();
                let (title, width) = holder.read(cx).ghost_look(id, cx).ok_or_else(|| format!("no tab {n} in pane {from}"))?;
                let ghost = crate::tab_bar::ghost(title, width, cx);
                (ghost, Some((holder, id)))
            }
            None => (crate::explorer::row::drag_card(Path::new(source), cx), None),
        };
        // Over a bar the pane under the pointer answers as its strip does on a real drag move: its
        // own tab takes the slot there, anything else opens a gap where it would land.
        let bar = match zone {
            Zone::Bar(_) => {
                let slot = self.slot(target).ok_or_else(|| format!("no pane {pane}"))?;
                let bounds = slot.bounds.get();
                let strip = Bounds { origin: bounds.origin, size: size(bounds.size.width, self.band(target)) };
                let target = slot.pane.clone();
                // Read before the update: the holder can be the target pane itself.
                let lifted = lifted.as_ref().map(|(holder, id)| (*id, holder.read(cx).is_pinned_id(*id)));
                target.update(cx, |pane, cx| match lifted {
                    Some((id, pinned)) => pane.drag_over(id, pinned, strip, strip.center(), cx),
                    None => pane.gap_over(false, strip, strip.center(), cx),
                });
                Some(target)
            }
            _ => None,
        };
        // Held over a pane's body or edge, the pointer is away from the bar: the lifted tab's own slot folds closed.
        if let (None, Some((holder, id))) = (&bar, &lifted) {
            let id = *id;
            holder.update(cx, |pane, cx| pane.hold_away(id, cx));
        }
        self.held = Some(Held { ghost, pane: target, zone, tab: lifted.as_ref().map(|(_, id)| *id), lifted: lifted.map(|(pane, _)| pane), bar });
        cx.notify();
        Ok(())
    }

    /// For the control socket: ends a held drag with no drop.
    pub fn release_drag(&mut self, cx: &mut Context<Self>) {
        if let Some(held) = self.held.take() {
            // The pointer leaves the strip, which folds any gap, and the lifted tab lands back.
            if let Some(bar) = &held.bar {
                bar.update(cx, |pane, cx| pane.gap_over(false, Bounds::default(), Point::default(), cx));
            }
            for pane in [held.lifted, held.bar].into_iter().flatten() {
                pane.update(cx, |pane, cx| pane.end_drag(cx));
            }
            cx.notify();
        }
    }

    /// Where the held drag's ghost sits for `zone` of `pane`, as `[x, y]`: the middle of the zone's
    /// preview, or of the pane's tab bar.
    fn held_point(&self, pane: PaneId, zone: Zone) -> Option<[f32; 2]> {
        if matches!(zone, Zone::Bar(_)) {
            let bounds = self.slot(pane)?.bounds.get();
            return Some([f32::from(bounds.center().x), f32::from(bounds.origin.y) + (f32::from(self.band(pane)) - f32::from(crate::tokens::TAB_HEIGHT)) / 2.]);
        }
        let [x, y, width, height] = self.drop_rect(pane, zone)?;
        Some([x + width / 2., y + height / 2.])
    }

    /// For the control socket: tells the open file at `path` what the watcher would when its file
    /// changed on disk (`gone` false) or was deleted (`gone` true), so the tab shows the conflict
    /// or the gone mark. Only a file with unsaved edits keeps such a mark.
    pub fn disk_state(&mut self, path: &Path, gone: bool, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        let document = self
            .documents(cx)
            .into_iter()
            .find(|document| document.read(cx).path.as_deref().is_some_and(|open| open == path))
            .ok_or_else(|| format!("{} is not open", path.display()))?;
        if !document.read(cx).unsaved {
            return Err("the file has no unsaved edits (type some first)".into());
        }
        let disk = crate::document::Disk { mtime: Some(SystemTime::now()), gone, read_only: false };
        document.update(cx, |document, cx| {
            let epoch = document.save_epoch();
            document.on_disk(disk, None, epoch, window, cx)
        });
        Ok(())
    }

    /// A pane's view, by name.
    fn slot(&self, id: PaneId) -> Option<&PaneSlot> {
        self.panes.iter().find(|slot| slot.id == id)
    }

    /// The active pane's tabs.
    pub(crate) fn active_pane(&self) -> &Entity<Pane> {
        &self.slot(self.active).unwrap_or(&self.panes[0]).pane
    }

    /// Every document open in the window, in every pane.
    pub(crate) fn documents(&self, cx: &App) -> Vec<Entity<Document>> {
        self.panes.iter().flat_map(|slot| slot.pane.read(cx).documents().cloned().collect::<Vec<_>>()).collect()
    }

    /// Closes the tab holding document `id`, in whichever pane has it, without asking. A pane
    /// left with no tab closes by the usual rule.
    pub(crate) fn close_document(&mut self, id: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let holder = self.panes.iter().find(|slot| slot.pane.read(cx).documents().any(|document| document.entity_id() == id)).map(|slot| slot.pane.clone());
        if let Some(pane) = holder {
            pane.update(cx, |pane, cx| pane.remove(id, window, cx));
        }
    }

    /// Adds the view of a pane (its tab bar, its editor view and what they leave for the window)
    /// and returns its name. It isn't in the tree yet.
    fn add_slot(&mut self, pane: Entity<Pane>, editor: Entity<EditorState>, window: &mut Window, cx: &mut Context<Self>) -> PaneId {
        let id = self.next_pane;
        self.next_pane += 1;
        let link = Rc::<EditorLink>::default();
        let preview = self.preview_pane.entity_id();
        let editor_pane = cx.new(|cx| EditorPane::new(editor, link.clone(), preview, cx));
        // The pane says when a tab, a title or an unsaved state changed, when a tab was picked in
        // it and when its last tab went.
        let subscription = cx.subscribe_in(&pane, window, move |this, pane, event: &PaneEvent, window, cx| match event {
            PaneEvent::Changed => this.sync(window, cx),
            PaneEvent::Activated => this.set_active(id, false, window, cx),
            PaneEvent::Show(document) => {
                // Deferred: the pane is still emitting, and showing the tab updates it.
                let (pane, document) = (pane.clone(), *document);
                cx.defer_in(window, move |_, window, cx| pane.update(cx, |pane, cx| pane.activate_id(document, window, cx)));
            }
            PaneEvent::Drop(document) => {
                let document = *document;
                cx.defer_in(window, move |this, window, cx| this.close_document(document, window, cx));
            }
            PaneEvent::Emptied => this.close_pane(id, true, window, cx),
            PaneEvent::Closed(closed) => this.remember_closed(id, closed, cx),
            PaneEvent::MoveTab(tab, direction) => this.move_tab(*tab, *direction, window, cx),
            PaneEvent::NewWindow(tab) => {
                let tab = *tab;
                cx.defer_in(window, move |this, window, cx| this.move_to_new_window(tab, window, cx));
            }
            PaneEvent::Split(tab, axis) => {
                let (tab, axis) = (*tab, *axis);
                cx.defer_in(window, move |this, window, cx| this.split_tab(tab, id, axis, window, cx));
            }
            PaneEvent::Reveal(tab) => {
                let tab = *tab;
                cx.defer_in(window, move |this, window, cx| this.reveal_tab(tab, window, cx));
            }
            PaneEvent::Dropped(dropped, zone) => {
                this.drop_on(dropped.clone(), id, *zone, window, cx);
            }
        });
        // An editor that finished fading in or out: this view stops drawing it, or the empty state under it.
        cx.subscribe(&editor_pane, |_, _, _: &Landed, cx| cx.notify()).detach();
        // A pane made while the window is minimal comes up minimal.
        if self.minimal.on {
            editor_pane.update(cx, |pane, cx| pane.set_minimal(true, cx));
        }
        let bars = cx.new(|_| ChangeBars::new());
        self.panes.push(PaneSlot { id, pane, editor: editor_pane, link, input: None, bars, holding: false, bounds: Rc::default(), fit: Cell::new(motion::Fade::with(1., FADE_QUICK)), _subscription: subscription });
        id
    }

    /// Makes `id` the active pane: its tab bar's edge lights, the title, the explorer's selection
    /// and the preview follow it. With `focus` the keyboard goes to its editor.
    fn set_active(&mut self, id: PaneId, focus: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.tree.live().contains(&id) {
            return;
        }
        let changed = self.active != id;
        self.active = id;
        if focus {
            if let Some(slot) = self.slot(id) {
                slot.pane.clone().update(cx, |pane, cx| pane.focus_editor(window, cx));
            }
        }
        if changed {
            self.sync(window, cx);
        }
    }

    /// A pane lost its last tab: it glides shut and goes, and its sibling takes the keyboard.
    /// The last pane in the window stays, empty.
    fn close_pane(&mut self, id: PaneId, keep_content: bool, window: &mut Window, cx: &mut Context<Self>) {
        let next = self.tree.sibling_of(id);
        if !self.tree.close(id, motion::drawn_now(window.window_handle().window_id())) {
            return;
        }
        // A pane that closed its last tab keeps drawing its editor and tab bar while it glides shut.
        // One whose tab moved to another pane has nothing to keep: the editor is drawn there now.
        if keep_content {
            if let Some(slot) = self.panes.iter_mut().find(|slot| slot.id == id) {
                slot.holding = true;
                slot.pane.update(cx, |pane, cx| pane.hold_bar(cx));
                slot.editor.update(cx, |editor, cx| editor.release_caret(cx));
            }
        }
        if self.active == id {
            // The sibling may itself be closing (two panes shut in one go): the keyboard goes to a pane that stays.
            let live = self.tree.live();
            if let Some(next) = next.filter(|next| live.contains(next)).or_else(|| live.first().copied()) {
                self.set_active(next, true, window, cx);
            }
        }
        self.remember_tabs(cx);
        cx.notify();
    }

    /// The pane nearest `from` in `direction` that overlaps it along the other axis, by where the
    /// panes were drawn last frame.
    fn neighbor(&self, from: PaneId, direction: Direction) -> Option<PaneId> {
        self.neighbor_among(&self.tree.live(), from, direction)
    }

    /// `neighbor` over `live`, the tree's live panes, for a caller that asks about several panes.
    fn neighbor_among(&self, live: &[PaneId], from: PaneId, direction: Direction) -> Option<PaneId> {
        let edges = |place: Bounds<Pixels>| (f32::from(place.left()), f32::from(place.top()), f32::from(place.right()), f32::from(place.bottom()));
        let (left, top, right, bottom) = edges(self.slot(from)?.bounds.get());
        let (mid_x, mid_y) = ((left + right) / 2., (top + bottom) / 2.);
        let overlap = |low: f32, high: f32, other_low: f32, other_high: f32| high.min(other_high) - low.max(other_low) > 1.;
        live.iter()
            .copied()
            .filter(|id| *id != from)
            .filter_map(|id| {
                let (l, t, r, b) = edges(self.slot(id)?.bounds.get());
                let (gap, cross) = match direction {
                    Direction::Right => (l - right, ((t + b) / 2. - mid_y).abs()),
                    Direction::Left => (left - r, ((t + b) / 2. - mid_y).abs()),
                    Direction::Down => (t - bottom, ((l + r) / 2. - mid_x).abs()),
                    Direction::Up => (top - b, ((l + r) / 2. - mid_x).abs()),
                };
                let across = match direction {
                    Direction::Left | Direction::Right => overlap(top, bottom, t, b),
                    Direction::Up | Direction::Down => overlap(left, right, l, r),
                };
                (gap >= -2. && across).then_some((id, gap, cross))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1).then(a.2.total_cmp(&b.2)))
            .map(|(id, _, _)| id)
    }

    /// Cmd-K then an arrow, or Window > Focus pane: the keyboard goes to the pane in that direction.
    /// A key with no pane there beeps.
    fn focus_direction(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        match self.neighbor(self.active, direction) {
            Some(id) => self.set_active(id, true, window, cx),
            None => self.refuse(window, cx),
        }
    }

    /// Window > Focus next pane or Focus previous pane: the keyboard goes to the next pane in reading order (top left first), wrapping
    /// round. A window with one pane beeps.
    fn focus_cycle(&mut self, step: isize, window: &mut Window, cx: &mut Context<Self>) {
        let live = self.tree.live();
        let Some(at) = live.iter().position(|id| *id == self.active).filter(|_| live.len() > 1) else {
            self.refuse(window, cx);
            return;
        };
        let next = (at as isize + step).rem_euclid(live.len() as isize) as usize;
        self.set_active(live[next], true, window, cx);
    }

    /// Whether a pane lies in `direction` of the active one, for the Window menu's pane items.
    pub(crate) fn can_go(&self, direction: Direction) -> bool {
        self.neighbor(self.active, direction).is_some()
    }

    /// Tells each pane which directions have a pane beside it, for its tabs' right-click menus;
    /// `true` when any pane's answer changed.
    fn settle_moves(&mut self, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        let live = self.tree.live();
        let window_tabs = self.tab_count(cx);
        for ix in 0..self.panes.len() {
            let (id, pane) = (self.panes[ix].id, self.panes[ix].pane.clone());
            let moves = Direction::ALL.map(|direction| self.neighbor_among(&live, id, direction).is_some());
            changed |= pane.update(cx, |pane, _| pane.set_moves(moves, window_tabs));
        }
        changed
    }

    /// Window > Move tab to ... pane: the showing tab goes to the pane in that direction the way a
    /// drop on its middle sends it (caret, scroll and unsaved text with it), and that pane takes the
    /// keyboard. A key with no pane there beeps.
    fn move_active_tab(&mut self, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.active_pane().read(cx).active_document().map(Entity::entity_id) else {
            self.refuse(window, cx);
            return;
        };
        self.move_tab(id, direction, window, cx);
    }

    /// Moves tab `id` to the pane beside the one holding it, in `direction`.
    fn move_tab(&mut self, id: EntityId, direction: Direction, window: &mut Window, cx: &mut Context<Self>) {
        let holder = self.holder_of(id, cx);
        let target = holder.and_then(|holder| self.neighbor(holder, direction));
        match target {
            Some(target) => {
                // A copy of the tab glides from its place in this strip to its slot in the other; one still gliding in from an earlier carry starts from where it is drawn, at its pace.
                let start = holder.and_then(|holder| self.slot(holder)).and_then(|slot| slot.pane.read(cx).carry_start(id));
                let landed = self.drop_on(Dropped::Tab(id), target, Zone::Middle, window, cx);
                if let (Some(landed), Some((from, lift, speed))) = (landed, start) {
                    self.glide_to(landed, id, from, lift, speed, motion::drawn_now(window.window_handle().window_id()), cx);
                }
            }
            None => self.refuse(window, cx),
        }
    }

    /// The `n`th pane in reading order, counting from 1, takes the keyboard. `false` when there is none.
    pub fn focus_pane(&mut self, n: usize, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let Some(id) = n.checked_sub(1).and_then(|ix| self.tree.live().get(ix).copied()) else { return false };
        self.set_active(id, true, window, cx);
        true
    }

    /// A press on a divider: the drag starts with the pointer's offset from the line, so the line
    /// stays under it as it moves.
    fn start_drag(&mut self, split: usize, at: Point<Pixels>, window: &Window, cx: &mut Context<Self>) {
        let now = motion::drawn_now(window.window_handle().window_id());
        let Some(found) = self.tree.split_mut(split) else { return };
        if found.closing.is_some() {
            return;
        }
        let place = found.bounds.get();
        let length = f32::from(match found.axis {
            SplitAxis::Right => place.size.width,
            SplitAxis::Down => place.size.height,
        });
        let share = drawn_share(found, now, length);
        // A pane still gliding stops where it is drawn.
        found.ratio = share;
        found.glide = None;
        found.stretch = None;
        let (offset, pointer) = match found.axis {
            SplitAxis::Right => (at.x - (place.origin.x + place.size.width * share), at.x),
            SplitAxis::Down => (at.y - (place.origin.y + place.size.height * share), at.y),
        };
        self.drag = Some(Drag { split, offset: f32::from(offset) });
        self.trail = vec![(Instant::now(), f32::from(pointer))];
        cx.notify();
    }

    /// The least width, in points, the first side of split `id` keeps so the corner pane's bar
    /// still has room for one tab beside the traffic lights: zero when the explorer covers the
    /// lights, or the split isn't on the corner's edge. A split inside the first side takes its
    /// share of it. The room is the bar's left inset, one tab's least width and the bar's inset
    /// on the right.
    fn corner_room(&self, id: usize) -> f32 {
        if self.corner_inset <= f32::from(TAB_BAR_INSET) {
            return 0.;
        }
        let mut chain = Vec::new();
        let mut node = &self.tree.root;
        while let Node::Split(split) = node {
            chain.push(&**split);
            node = &split.first;
        }
        let Some(at) = chain.iter().position(|split| split.id == id).filter(|&at| chain[at].axis == SplitAxis::Right) else { return 0. };
        let inner: f32 = chain[at + 1..].iter().filter(|split| split.axis == SplitAxis::Right).map(|split| split.ratio).product();
        (self.corner_inset + f32::from(TAB_MIN_WIDTH) + f32::from(TAB_BAR_INSET)) / inner.max(0.01)
    }


    /// The pointer moved while a divider is held: the line goes to it, with no glide. Past
    /// `PANE_MIN` (or `corner_room` for the corner's first side) it stretches instead: the drawn
    /// line follows `STRETCH_PULL` of the overshoot, at most `STRETCH_MAX` points, and `ratio`
    /// stays at the limit.
    fn drag_to(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(Drag { split, offset }) = self.drag else { return };
        let corner_room = self.corner_room(split);
        let Some(found) = self.tree.split_mut(split) else { return };
        let place = found.bounds.get();
        let (origin, length, pointer) = match found.axis {
            SplitAxis::Right => (f32::from(place.origin.x), f32::from(place.size.width), f32::from(at.x)),
            SplitAxis::Down => (f32::from(place.origin.y), f32::from(place.size.height), f32::from(at.y)),
        };
        if length <= 0. {
            return;
        }
        let hold = (PANE_MIN / length).min(0.5);
        // The first side of a split along the corner's edge also keeps the corner pane's tabs clear of the lights.
        let least = (PANE_MIN.max(corner_room) / length).min(1. - hold);
        let raw = (pointer - offset - origin) / length;
        found.ratio = raw.clamp(least, 1. - hold);
        let over = (raw - found.ratio) * length;
        let was_stretched = found.stretch.is_some();
        found.stretch = (over != 0.).then(|| found.ratio + over.signum() * (over.abs() * crate::tokens::STRETCH_PULL).min(crate::tokens::STRETCH_MAX) / length);
        // The line meets its least size: one tick as the stretch starts, none while it holds.
        if found.stretch.is_some() && !was_stretched {
            crate::haptics::align();
        }
        found.glide = None;
        self.trail.push((Instant::now(), pointer));
        if self.trail.len() > crate::tokens::TRAIL_SAMPLES {
            self.trail.remove(0);
        }
        cx.notify();
    }

    /// The divider was let go: a stretched one springs back on `SETTLE` from where it is drawn,
    /// carrying the pointer's speed, and the sizes are kept for the next launch.
    fn end_drag(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(Drag { split, .. }) = self.drag.take() else { return };
        let now = motion::drawn_now(window.window_handle().window_id());
        // The speed over the last `TRAIL_SPAN` seconds of movement; a pointer that rested is still.
        let trail = std::mem::take(&mut self.trail);
        let recent = crate::tokens::TRAIL_SPAN;
        let speed = match (trail.first(), trail.last()) {
            (Some(_), Some(&(last_at, last))) if last_at.elapsed().as_secs_f32() < recent => {
                let &(first_at, first) = trail.iter().find(|(at, _)| last_at.duration_since(*at).as_secs_f32() <= recent).unwrap_or(&trail[0]);
                let span = last_at.duration_since(first_at).as_secs_f32();
                if span > 0. { (last - first) / span } else { 0. }
            }
            _ => 0.,
        };
        if let Some(found) = self.tree.split_mut(split) {
            if let Some(drawn) = found.stretch.take() {
                let place = found.bounds.get();
                let length = f32::from(if found.axis == SplitAxis::Right { place.size.width } else { place.size.height });
                let speed = if length > 0. { speed / length } else { 0. };
                found.glide = Some(motion::Glide::over(drawn, found.ratio, crate::tokens::SETTLE, now).with_speed(speed));
            }
        }
        self.remember_tabs(cx);
        cx.notify();
    }

    /// A double-click on a divider: its split glides to even halves on the equalize spring, from
    /// where the line is drawn, and the new sizes are kept.
    fn equalize(&mut self, split: usize, window: &Window, cx: &mut Context<Self>) {
        let now = motion::drawn_now(window.window_handle().window_id());
        let Some(found) = self.tree.split_mut(split) else { return };
        if found.closing.is_some() {
            return;
        }
        let from = found.share(now);
        found.ratio = 0.5;
        found.stretch = None;
        found.glide = Some(motion::Glide::equalize(from, 0.5, now));
        self.drag = None;
        self.remember_tabs(cx);
        cx.notify();
    }

    /// Window > Even out panes: every divider does what a double-click on it does, so each split glides to even halves on the equalize
    /// spring from where it is drawn and the sizes are kept. A window with one pane beeps.
    fn even_out_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let splits = self.tree.split_ids();
        if splits.is_empty() {
            self.refuse(window, cx);
            return;
        }
        for split in splits {
            self.equalize(split, window, cx);
        }
    }

    /// Window > Merge panes: every other pane's tabs come into the active pane, in reading order, each at the end of its group (pins stay
    /// pinned, order kept), and the emptied panes glide shut. No tab is shown on the way (a tab never shown stays unread) and the active
    /// pane's own tab stays showing. A window with one pane beeps.
    fn merge_panes(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let live = self.tree.live();
        if live.len() < 2 {
            self.refuse(window, cx);
            return;
        }
        let target = self.active;
        let mut taken = Vec::new();
        let others: Vec<PaneId> = live.iter().copied().filter(|id| *id != target).collect();
        for id in &others {
            let Some(slot) = self.slot(*id) else { continue };
            let (pane, editor) = (slot.pane.clone(), slot.editor.clone());
            let tabs: Vec<EntityId> = pane.read(cx).documents().map(Entity::entity_id).collect();
            for tab in tabs {
                let pinned = pane.read(cx).is_pinned_id(tab);
                let Some(document) = pane.update(cx, |pane, cx| pane.take(tab, cx)) else { continue };
                // The moved editor is drawn by its new pane now, not fading out of this one; a tab never shown has none to forget.
                editor.update(cx, |pane, _| pane.blank());
                if let Some(moved) = document.read(cx).built_editor().map(Entity::entity_id) {
                    editor.update(cx, |pane, _| pane.forget(moved));
                }
                taken.push((document, pinned));
            }
        }
        self.append_tabs(taken, target, window, cx);
        for id in others {
            self.close_pane(id, false, window, cx);
        }
        self.sync(window, cx);
    }

    /// Window > Merge all windows: every tab of every other window comes into this one's active pane (text, caret, unsaved edits and an
    /// Untitled page's draft go with it, none shown on the way), and each emptied window closes. Tabs from another folder come across
    /// as they are. The scratch window stays out of it, and beeps. Waits for this update to finish, since it updates the other windows.
    fn merge_all_windows(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let front = self.window;
        let others = open_workspaces_except(Some(front), cx).iter().any(|(_, other)| !other.read(cx).is_scratch());
        if self.scratch || !others {
            self.refuse(window, cx);
            return;
        }
        let pane = self.active;
        cx.defer(move |cx| merge_windows_into(front, pane, cx));
    }

    /// Sets the share of the split whose divider is nearest pane `pane` (counting from 1, as `focus_pane`
    /// does), for the control socket: the same press, move and let go a divider drag runs, so the
    /// minimums hold and a share past them stretches and springs back. Replies with the share kept,
    /// `None` when there is no such pane or it has no split.
    pub fn set_share(&mut self, pane: usize, share: f32, window: &Window, cx: &mut Context<Self>) -> Option<f32> {
        let id = pane.checked_sub(1).and_then(|ix| self.tree.live().get(ix).copied())?;
        let split = nearest_split(&self.tree.root, id)?;
        let place = self.tree.split_mut(split)?.bounds.get();
        let center = place.center();
        self.start_drag(split, center, window, cx);
        let offset = self.drag.as_ref()?.offset;
        let found = self.tree.split_mut(split)?;
        let (origin, length) = match found.axis {
            SplitAxis::Right => (f32::from(place.origin.x), f32::from(place.size.width)),
            SplitAxis::Down => (f32::from(place.origin.y), f32::from(place.size.height)),
        };
        let pointer = px(origin + offset + length * share);
        let at = match found.axis {
            SplitAxis::Right => point(pointer, center.y),
            SplitAxis::Down => point(center.x, pointer),
        };
        self.drag_to(at, cx);
        let kept = self.tree.split_mut(split).map(|found| found.ratio);
        self.end_drag(window, cx);
        kept
    }

    /// Files changed on disk (or the window came back to the front): each open one among them
    /// that has no unsaved edits shows the new text. The files are read in the background, and
    /// `Document::reload` checks for unsaved edits again when the text lands, so typing in the
    /// meantime wins. One that has unsaved edits and a different time on disk from our last read or write
    /// is in conflict, and its tab says so. A file that is gone closes its tab when nothing is
    /// lost by it; a tab with unsaved edits stays, marked (`Document::on_disk`). With `stale_only`
    /// (the window came back, no watcher said anything) a clean file is read only when its time
    /// differs from our last read or write.
    fn reload_changed(&mut self, paths: &[PathBuf], stale_only: bool, window: &mut Window, cx: &mut Context<Self>) {
        // One set of the batch, a lookup per document: a busy folder sends thousands of paths.
        let batch: HashSet<&Path> = paths.iter().map(PathBuf::as_path).collect();
        let now = Instant::now();
        let mut changed: Vec<Reload> = Vec::new();
        self.read_seq += 1;
        let seq = self.read_seq;
        self.forget_closed(cx);
        let open_now = self.documents(cx);
        for open in open_now {
            let document = open.read(cx);
            // The file itself, or a folder above it: a dropped-events rescan or a moved or deleted
            // folder names only the folder.
            let Some(path) = document.path.as_ref().filter(|path| path.ancestors().any(|at| batch.contains(at))) else { continue };
            let exact = batch.contains(path.as_path());
            // A file that keeps growing is read one at a time and no more often than `FOLLOW_TICK`;
            // what comes in meanwhile waits for the one timer that reads it.
            if document.growing() && !stale_only {
                let pace = self.paces.entry(path.clone()).or_default();
                if pace.busy {
                    pace.again = true;
                    continue;
                }
                if let Some(wait) = pace.last.map(|last| (last + FOLLOW_TICK).saturating_duration_since(now)).filter(|wait| !wait.is_zero()) {
                    if pace.timer.is_none() {
                        let path = path.clone();
                        pace.timer = Some(cx.spawn_in(window, async move |this, cx| {
                            cx.background_executor().timer(wait).await;
                            this.update_in(cx, |this, window, cx| {
                                if let Some(pace) = this.paces.get_mut(&path) {
                                    pace.timer = None;
                                }
                                this.reload_changed(&[path], false, window, cx);
                            })
                            .ok();
                        }));
                    }
                    continue;
                }
                pace.last = Some(now);
                pace.busy = true;
                pace.reading = seq;
            }
            changed.push(Reload {
                document: open.clone(),
                path: path.clone(),
                unsaved: document.dirty() || document.awaiting_open(),
                saved: document.saved_mtime(),
                encoding: document.encoding(),
                epoch: document.save_epoch(),
                base: document.grow_base(cx),
                exact,
                unread: document.is_unread(),
            });
        }
        if changed.is_empty() {
            return;
        }
        cx.spawn_in(window, async move |this, cx| {
            let reads = cx
                .background_executor()
                .spawn(async move {
                    changed
                        .into_iter()
                        .map(|Reload { document, path, unsaved, saved, encoding: current, epoch, base, exact, unread }| {
                            // The time first: a write landing between the two reads leaves the
                            // time older than the text, which only ever errs toward a reload.
                            let disk = probe(&path);
                            // A folder-only match reads a clean file only when its time moved.
                            let unchanged = (stale_only || !exact) && disk.mtime == saved;
                            // A tab that never showed reads the file when it does; only its time is taken.
                            let text = if unsaved || disk.gone || unchanged || unread {
                                None
                            } else {
                                // A file that only grew reads just what it gained; anything else reads whole.
                                let tail = base.as_ref().and_then(|base| encoding::read_tail(&path, base).ok().flatten());
                                match tail {
                                    Some(tail) => Some(Read::Tail(tail)),
                                    None => encoding::reread_file(&path, current).ok().map(Read::Full),
                                }
                            };
                            (document, path, disk, text, epoch)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update_in(cx, |this, window, cx| {
                this.forget_closed(cx);
                let open_now = this.documents(cx);
                for (document, path, disk, text, epoch) in reads {
                    // A read of a file the tab no longer holds (renamed or saved as meanwhile) says
                    // nothing of this tab, and one older than what already landed would put old text back.
                    // One for a tab closed meanwhile has nowhere to land.
                    let current = document.read(cx).path.as_ref() == Some(&path) && open_now.contains(&document);
                    let newest = this.read_applied.get(&document.entity_id()).is_none_or(|&applied| applied < seq);
                    if !current || !newest {
                        // Only the paced read in flight frees the pace; another read may still be out.
                        let freed = this.paces.get_mut(&path).filter(|pace| pace.reading == seq).map(|pace| {
                            pace.busy = false;
                            std::mem::take(&mut pace.again)
                        });
                        if freed == Some(true) {
                            this.reload_changed(&[path], false, window, cx);
                        }
                        continue;
                    }
                    this.read_applied.insert(document.entity_id(), seq);
                    if document.update(cx, |document, cx| document.on_disk(disk, text, epoch, window, cx)) {
                        // Nothing is lost: no unsaved edits, or an empty page.
                        document.update(cx, |document, _| document.discard_edits());
                        this.close_document(document.entity_id(), window, cx);
                        this.paces.remove(&path);
                        this.read_applied.remove(&document.entity_id());
                        continue;
                    }
                    let (retry, growing) = document.update(cx, |document, _| (document.take_retry(), document.growing()));
                    if !growing && !this.paces.contains_key(&path) {
                        if retry {
                            this.reload_changed(&[path], false, window, cx);
                        }
                        continue;
                    }
                    let pace = this.paces.entry(path.clone()).or_default();
                    pace.busy = false;
                    pace.last = Some(Instant::now());
                    if std::mem::take(&mut pace.again) || retry {
                        this.reload_changed(&[path], false, window, cx);
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Drops the read and pacing records of documents no tab holds any more, so opening and
    /// closing tabs leaves nothing behind. A pace goes with its timer.
    fn forget_closed(&mut self, cx: &App) {
        let open = self.documents(cx);
        self.read_applied.retain(|id, _| open.iter().any(|document| document.entity_id() == *id));
        self.paces.retain(|path, _| open.iter().any(|document| document.read(cx).path.as_ref() == Some(path)));
    }

    /// Brings the window title, its edited dot, the tree's dots and its selection, the editor and
    /// the preview in line with the pane: its active tab and the unsaved state of every tab.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let document = self.document(cx);
        // The Following chip watches the tab showing now.
        let shown_editor = document.as_ref().and_then(|document| document.read(cx).built_editor().cloned());
        self.follow_chip.update(cx, |chip, cx| chip.watch(shown_editor, cx));
        let title = match &document {
            Some(document) => document.read(cx).title().to_string(),
            None => "Finch".to_string(),
        };
        if self.sent.0.as_deref() != Some(title.as_str()) {
            window.set_window_title(&title);
            self.sent.0 = Some(title);
        }
        // The same rule as the close and quit questions: any tab with unsaved edits, shown or not.
        let edited = self.panes.iter().any(|slot| slot.pane.read(cx).documents().any(|document| document.read(cx).shows_dot()));
        if self.sent.1 != Some(edited) {
            window.set_window_edited(edited);
            self.sent.1 = Some(edited);
        }
        // After this update: the bar reads the key window's documents.
        cx.defer(menu::refresh);
        self.settle_moves(cx);
        self.forget_closed(cx);
        self.hand_root(cx);
        let marked: HashSet<PathBuf> = self
            .documents(cx)
            .iter()
            .filter_map(|document| {
                let document = document.read(cx);
                document.path.clone().filter(|_| document.shows_dot())
            })
            .collect();
        self.explorer.update(cx, |explorer, cx| explorer.set_marked(marked, cx));
        // Another tab came forward, or the one showing took another path (Save As): its file is the
        // tree's selection, taken once its row shows. A draft has no file to follow.
        let now = (document.as_ref().map(Entity::entity_id), document.as_ref().and_then(|document| document.read(cx).path.clone()));
        if now != self.shown {
            self.shown = now.clone();
            // A file outside the tree's root or a hidden one has no row to follow.
            self.explorer.update(cx, |explorer, cx| explorer.follow_file(now.1.as_deref(), cx));
        }
        self.remember_tabs(cx);
        self.sync_panes(window, cx);
        cx.notify();
    }

    /// Tells every file of this window the folder its tree is rooted at, for Save As.
    fn hand_root(&self, cx: &mut Context<Self>) {
        for document in self.documents(cx) {
            if document.read(cx).tree_root.as_deref() != Some(self.start.root.as_path()) {
                document.update(cx, |document, _| document.tree_root = Some(self.start.root.clone()));
            }
        }
    }

    /// The names of the files in this window with unsaved edits.
    fn unsaved_names(&self, cx: &App) -> Vec<Unsaved> {
        self.documents(cx)
            .iter()
            .map(|document| document.read(cx))
            .filter(|document| document.shows_dot())
            .map(|document| (title(document.path.as_deref()), document.path.clone()))
            .collect()
    }

    /// Saves every file with unsaved edits, one at a time, so a file that has to ask (changed or
    /// deleted on disk) shows its tab and its question alone. The task says whether all of them
    /// reached the disk; it stops at the first that didn't.
    fn save_all(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Task<bool> {
        let unsaved: Vec<_> = self.documents(cx).into_iter().filter(|document| document.read(cx).shows_dot()).collect();
        cx.spawn_in(window, async move |this, cx| {
            for (nth, document) in unsaved.into_iter().enumerate() {
                let Ok((save, before)) = this.update_in(cx, |_, window, cx| {
                    let before = document.read(cx).write_errors();
                    (document.update(cx, |document, cx| document.save(window, cx)), before)
                }) else {
                    return false;
                };
                let ok = save.await;
                this.update(cx, |this, cx| this.settle_save(&document, ok, nth as u32, before, cx)).ok();
                if !ok {
                    return false;
                }
            }
            true
        })
    }

    /// The landings of the saves still writing in this window's files; empty when none is.
    fn saves_landing(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Vec<Task<bool>> {
        let saving: Vec<_> = self.documents(cx).into_iter().filter(|document| document.read(cx).is_saving()).collect();
        saving.into_iter().map(|document| document.update(cx, |document, cx| document.save_landed(window, cx))).collect()
    }

    /// Writes every Untitled page's draft that has edits waiting: the window closes, the app
    /// quits or the window loses focus.
    fn flush_drafts(&mut self, cx: &mut Context<Self>) {
        for document in self.documents(cx) {
            document.update(cx, |document, cx| document.flush_draft(cx));
        }
    }

    /// A pane's files (its tabs that have one, in order, without the preview tab: a glance isn't
    /// saved), the showing tab's place among them and how many of them are pinned.
    fn pane_files(&self, id: PaneId, cx: &App) -> (Vec<PathBuf>, usize, usize) {
        let Some(slot) = self.slot(id) else { return (Vec::new(), 0, 0) };
        let pane = slot.pane.read(cx);
        let saved = |(ix, document): (usize, &Entity<Document>)| if pane.is_preview(ix) { None } else { document.read(cx).tab_file() };
        let files: Vec<PathBuf> = pane.documents().enumerate().filter_map(saved).collect();
        let before = pane.documents().enumerate().take(pane.active_index()).filter_map(saved).count();
        let active = before.min(files.len().saturating_sub(1));
        let pinned = pane.documents().enumerate().take(pane.pinned_count()).filter_map(saved).count();
        (files, active, pinned)
    }

    /// Records the tree's root and the open tabs (every pane's files in reading order and the
    /// showing one's place, and the split layout when there is one) for the next launch on this
    /// root. This window is the one that last saved, so it wins. Writes only when something changed.
    fn remember_tabs(&self, cx: &mut Context<Self>) {
        if self.scratch {
            return;
        }
        let root = self.start.root.clone();
        let live = self.tree.live();
        let mut files: Vec<PathBuf> = Vec::new();
        let mut active = 0;
        for id in live.iter() {
            let (pane_files, pane_active, _) = self.pane_files(*id, cx);
            if *id == self.active {
                active = files.len() + pane_active;
            }
            files.extend(pane_files);
        }
        let active = active.min(files.len().saturating_sub(1));
        let layout = match self.tree.root {
            Node::Leaf(_) => None,
            Node::Split(_) => Some(self.tree.to_saved(&|id| {
                let (files, active, pinned) = self.pane_files(id, cx);
                SavedPanes::Pane { files, active, pinned }
            })),
        };
        // One pane keeps its pins in `pinned_tabs`; with several each pane in the layout has its own.
        let pinned_tabs = match (&*live, &layout) {
            ([only], None) => self.pane_files(*only, cx).2,
            _ => 0,
        };
        let active_pane = live.iter().position(|id| *id == self.active).unwrap_or(0);
        // Drafts another open window still holds stay in the list, so the next launch brings
        // them back too. Only those saved drafts this window doesn't show can be such a case, so
        // the other windows are read only when there is one (usually none); that window is read,
        // this one is mid-update.
        let unshown: Vec<PathBuf> = settings::get(cx)
            .explorer
            .open_files
            .iter()
            .filter(|file| drafts::is_draft(file) && !files.contains(file))
            .cloned()
            .collect();
        if !unshown.is_empty() {
            let held: Vec<PathBuf> = open_workspaces_except(Some(self.window), cx)
                .iter()
                .flat_map(|(_, workspace)| {
                    let workspace = workspace.read(cx);
                    workspace.documents(cx).iter().filter_map(|document| document.read(cx).tab_file()).collect::<Vec<_>>()
                })
                .collect();
            files.extend(unshown.into_iter().filter(|file| held.contains(file)));
        }
        let saved = &settings::get(cx).explorer;
        if saved.last_root.as_ref() == Some(&root)
            && saved.open_files == files
            && saved.active_tab == active
            && saved.pinned_tabs == pinned_tabs
            && saved.last_file.is_none()
            && saved.panes == layout
            && saved.active_pane == active_pane
        {
            return;
        }
        settings::update_later(cx, |settings| {
            settings.explorer.last_root = Some(root);
            settings.explorer.open_files = files;
            settings.explorer.active_tab = active;
            settings.explorer.pinned_tabs = pinned_tabs;
            settings.explorer.last_file = None;
            settings.explorer.panes = layout;
            settings.explorer.active_pane = active_pane;
        });
    }

    /// Drafts restore couldn't read (binary, or a read error) stay in the drafts folder, as a
    /// draft always does; the window says so once, and stderr names them.
    fn alert_unreadable_drafts(&self, files: Vec<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(first) = files.first().cloned() else { return };
        for file in &files {
            eprintln!("finch: can't open the draft {}; it stays in the drafts folder", file.display());
        }
        // Deferred: the window is still being built.
        let name = first.file_name().map_or_else(|| first.display().to_string(), |name| name.to_string_lossy().into_owned());
        let (title, detail) = match files.len() {
            1 => ("A draft couldn’t be opened".to_string(), format!("\u{201C}{name}\u{201D} is kept in the drafts folder.")),
            n => (format!("{n} drafts couldn’t be opened"), format!("They’re kept in the drafts folder, starting with \u{201C}{name}\u{201D}.")),
        };
        cx.defer_in(window, move |_, window, cx| {
            let answer = window.prompt(PromptLevel::Warning, &title, Some(&detail), &["OK", "Show in Finder"], cx);
            cx.spawn(async move |_, _| {
                if answer.await.ok() == Some(1) {
                    crate::native_menu::show_in_finder(&first);
                }
            })
            .detach();
        });
    }

    /// On a first launch for the root the tabs were saved for, opens them again in their panes
    /// and order with the saved one in front. The front file and the drafts that are gone are skipped here; any other
    /// remembered file isn't checked at launch, and is closed when the window comes forward or the tab is shown and its file is gone. A pane left with
    /// none goes. Only the process's first window does it, and a window opened on a file that
    /// wasn't among the tabs keeps just that.
    fn restore_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.scratch {
            return;
        }
        static RESTORED: AtomicBool = AtomicBool::new(false);
        if RESTORED.swap(true, Ordering::Relaxed) {
            return;
        }
        let real = |path: &PathBuf| path.canonicalize().unwrap_or_else(|_| path.clone());
        let saved = &settings::get(cx).explorer;
        let same_root = saved.last_root.as_ref().map(real) == Some(real(&self.start.root));
        let (files, active) = saved.saved_tabs();
        // Only the tab in front and the drafts are checked on disk now (the drafts folder is local).
        // A remembered file on a share that is asleep would block here for each tab; the others are
        // read when first shown, and a file gone by then says so like a failed open.
        let front_raw = self.start.file.clone().or_else(|| files.get(active).cloned());
        let front = front_raw.as_ref().map(real);
        let folders = [drafts::folder(), real(&drafts::folder())];
        let is_draft_file = |path: &Path| folders.iter().any(|folder| path.starts_with(folder)) && drafts::is_draft(path);
        let checked = |path: &Path| {
            is_draft_file(path) || front_raw.as_deref() == Some(path) || front.as_deref() == Some(path)
        };
        let resolve = |path: &PathBuf| if checked(path) { real(path) } else { path.clone() };
        let layout = saved.panes.clone();
        let active_pane = saved.active_pane;
        let pinned_tabs = saved.pinned_tabs;
        let mut paths: Vec<PathBuf> = Vec::new();
        for file in &files {
            let file = resolve(file);
            if (!checked(&file) || file.is_file()) && !paths.contains(&file) {
                paths.push(file);
            }
        }
        let mut shown = self.active_pane().read(cx).active_document().cloned();
        let soft_wrap = settings::get(cx).editor.soft_wrap;
        // Another root, or a window opened on a file that wasn't among the tabs: only the open
        // drafts come back, after what is showing, so no typed text is forgotten.
        if !same_root || self.start.file.as_ref().is_some_and(|file| !paths.contains(&real(file))) {
            let mut documents: Vec<_> = shown.clone().filter(|document| !document.read(cx).is_blank()).into_iter().collect();
            let mut unreadable: Vec<PathBuf> = Vec::new();
            for path in paths.into_iter().filter(|path| is_draft_file(path)) {
                match drafts::read(&path) {
                    Ok(text) => documents.push(cx.new(|_| Document::unbuilt_draft(path, text, soft_wrap))),
                    Err(_) => unreadable.push(path),
                }
            }
            self.alert_unreadable_drafts(unreadable, window, cx);
            if documents.len() > usize::from(shown.is_some_and(|document| !document.read(cx).is_blank())) {
                self.active_pane().clone().update(cx, |pane, cx| pane.set_tabs(documents, 0, 0, cx));
            }
            return;
        }
        // One pane when the settings file has no layout (written before panes, or with one pane).
        let layout = layout.unwrap_or(SavedPanes::Pane { files: files.clone(), active, pinned: pinned_tabs });
        // Files the layout doesn't name (a draft another window kept) join the first pane.
        let named: Vec<PathBuf> = layout.files().iter().map(resolve).collect();
        let mut leftover: Vec<PathBuf> = paths.iter().filter(|path| !named.contains(path)).cloned().collect();
        let mut reuse = true;
        let mut used: HashSet<PathBuf> = HashSet::new();
        let mut front_pane: Option<PaneId> = None;
        let mut unreadable: Vec<PathBuf> = Vec::new();
        let built = Tree::from_saved(&layout, &mut |pane_files: &[PathBuf], pane_active: usize, pinned: usize| {
            let mut pane_files = pane_files.to_vec();
            let wanted = pane_files.get(pane_active).map(resolve);
            pane_files.append(&mut leftover);
            let mut documents = Vec::new();
            // How many of the restored tabs are pinned: the saved pins that still have a file.
            let mut pinned_n = 0;
            let (mut at_front, mut at_wanted) = (None, None);
            for (n, file) in pane_files.iter().enumerate() {
                let path = resolve(file);
                if (checked(&path) && !path.is_file()) || !used.insert(path.clone()) {
                    continue;
                }
                let open = shown.take_if(|document| document.read(cx).path.as_ref().map(real).as_ref() == Some(&path));
                let document = match open {
                    Some(document) => document,
                    None => {
                        // The time first, so a write in between shows as a change later.
                        let mtime = if checked(&path) { path_mtime(Some(&path)) } else { None };
                        if is_draft_file(&path) {
                            // A draft is the only copy of its text, so it keeps it.
                            match drafts::read(&path) {
                                Ok(text) => cx.new(|_| Document::unbuilt_draft(path.clone(), text, soft_wrap)),
                                Err(_) => {
                                    unreadable.push(path);
                                    continue;
                                }
                            }
                        } else {
                            // A clean file keeps only its path; it is read when it first shows.
                            cx.new(|_| Document::unread(path.clone(), mtime, soft_wrap))
                        }
                    }
                };
                if front.as_ref() == Some(&path) {
                    at_front = Some(documents.len());
                }
                if wanted.as_ref() == Some(&path) {
                    at_wanted = Some(documents.len());
                }
                if n < pinned {
                    pinned_n += 1;
                }
                documents.push(document);
            }
            if documents.is_empty() {
                return None;
            }
            let pane = if reuse {
                reuse = false;
                self.panes[0].pane.clone()
            } else {
                let pane = cx.new(|cx| Pane::empty(cx));
                let at = at_front.or(at_wanted).unwrap_or(0);
                let editor = documents[at].update(cx, |document, cx| document.editor(window, cx));
                self.add_slot(pane.clone(), editor, window, cx);
                pane
            };
            let id = self.panes.iter().find(|slot| slot.pane == pane).map(|slot| slot.id)?;
            if at_front.is_some() {
                front_pane = Some(id);
            }
            let at = at_front.or(at_wanted).unwrap_or(0);
            pane.update(cx, |pane, cx| pane.set_tabs(documents, at, pinned_n, cx));
            Some(id)
        });
        self.alert_unreadable_drafts(unreadable, window, cx);
        let Some(tree) = built else { return };
        self.tree = tree;
        let ids = self.tree.leaves();
        self.panes.retain(|slot| ids.contains(&slot.id));
        let live = self.tree.live();
        self.active = front_pane.unwrap_or_else(|| live[active_pane.min(live.len() - 1)]);
        let pane = self.active_pane().clone();
        pane.update(cx, |pane, cx| pane.focus_editor(window, cx));
    }

    /// Whether this is the scratch window (`scratch.rs`).
    pub(crate) fn is_scratch(&self) -> bool {
        self.scratch
    }

    /// Whether the folder tree shows: Cmd-B hasn't hidden it.
    fn left_shows(&self, cx: &App) -> bool {
        settings::get(cx).explorer.visible && !self.minimal.on && !self.scratch
    }

    /// The explorer, for the control socket.
    pub fn explorer(&self) -> &Entity<Explorer> {
        &self.explorer
    }

    /// The middle of the editor pane in window points: the control socket's
    /// default place for a wheel event.
    pub fn editor_center(&self, window: &Window, cx: &App) -> Point<Pixels> {
        // The active pane's place in the last frame.
        if let Some(slot) = self.slot(self.active) {
            let place = slot.bounds.get();
            if place.size.width > px(0.) && place.size.height > px(0.) {
                return point(place.origin.x + place.size.width / 2., place.origin.y + self.band(self.active) / 2. + place.size.height / 2.);
            }
        }
        let size = window.viewport_size();
        let sizes = self.layout.read(cx).sizes().clone();
        let settings = settings::get(cx);
        let left = if self.left_shows(cx) {
            sizes.first().copied().unwrap_or_else(|| px(settings.layout.left_panel_width))
        } else {
            px(0.)
        };
        let right = if self.right_shows(cx) {
            sizes.get(2).copied().unwrap_or_else(|| px(settings.layout.right_panel_width))
        } else {
            px(0.)
        };
        point(left + (size.width - left - right) / 2., TOP_BAND + (size.height - TOP_BAND) / 2.)
    }

    /// The folder the window's tree is rooted at.
    pub(crate) fn root(&self) -> &std::path::Path {
        &self.start.root
    }
}

impl Workspace {
    /// How tall the tab bar over `pane` is: the window's `TOP_BAND` for a pane at the top, `PANE_BAND` below.
    fn bar_band(&self, pane: PaneId) -> Pixels {
        self.bands.iter().find(|(id, _)| *id == pane).map_or_else(|| if self.tree.at_top(pane) { TOP_BAND } else { PANE_BAND }, |(_, fade)| px(fade.level(self.band_now)))
    }

    /// How tall the band over `pane` is, the editor's top inset: the bar's height, easing to `MINIMAL_TOP` as Minimal mode takes the bar away.
    fn band(&self, pane: PaneId) -> Pixels {
        let bar = self.bar_band(pane);
        let hide = self.minimal.levels.0;
        if hide > 0. { bar + (MINIMAL_TOP - bar) * hide } else { bar }
    }

    /// Aims each pane's band at the height its place gives it (the closing panes keep theirs) and
    /// hands the eased height to its bar and editor in this frame; true while one moves. A pane first
    /// seen starts at rest on its band. The editor's inset and the status line's room follow Minimal mode's level in the same frame.
    fn step_bands(&mut self, now: Instant, cx: &mut Context<Self>) -> bool {
        let present = self.tree.leaves();
        self.bands.retain(|(id, _)| present.contains(id));
        let mut moving = false;
        let (hide, status) = (self.minimal.levels.0, 1. - self.minimal.levels.0);
        for id in present.iter().copied() {
            let to = f32::from(if self.tree.at_top(id) { TOP_BAND } else { PANE_BAND });
            let at = match self.bands.iter().position(|(pane, _)| *pane == id) {
                Some(at) => at,
                None => {
                    self.bands.push((id, Fade::with(to, crate::tokens::ARRIVE)));
                    self.bands.len() - 1
                }
            };
            let fade = &mut self.bands[at].1;
            fade.aim(to, now);
            moving |= fade.is_moving(now);
            let band = px(fade.level(now));
            let inset = if hide > 0. { band + (MINIMAL_TOP - band) * hide } else { band };
            if let Some(slot) = self.panes.iter().find(|slot| slot.id == id) {
                slot.pane.update(cx, |pane, cx| pane.set_band(band, cx));
                slot.editor.update(cx, |pane, cx| {
                    pane.set_band(inset, cx);
                    pane.set_status(status, cx);
                });
            }
        }
        self.band_now = now;
        moving
    }

    /// View > Minimal mode and Ctrl-Cmd-Return.
    fn toggle_minimal(&mut self, _: &ToggleMinimal, window: &mut Window, cx: &mut Context<Self>) {
        if self.scratch {
            cx.propagate();
            return;
        }
        self.set_minimal(!self.minimal.on, window, cx);
    }

    /// Turns Minimal mode on or off for this window: the one flag every surface reads (the fade, the editors' gutter and column,
    /// the hover reveal) flips here, and the saved flag is written once. The user's panel, explorer, preview and line-number settings are never touched.
    pub(crate) fn set_minimal(&mut self, on: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.scratch || self.minimal.on == on {
            return;
        }
        self.minimal.on = on;
        self.minimal.peek = false;
        self.minimal.preview = false;
        self.minimal.escape = None;
        settings::update_later(cx, move |settings| settings.window.minimal = on);
        self.sync_minimal_editors(cx);
        self.sync_panes(window, cx);
        cx.defer(menu::refresh);
        cx.notify();
    }

    /// Tells every pane's editor view whether the window is minimal: line numbers, the forced column and the scrollbar follow it.
    fn sync_minimal_editors(&self, cx: &mut Context<Self>) {
        let on = self.minimal.on;
        for slot in &self.panes {
            slot.editor.update(cx, |pane, cx| pane.set_minimal(on, cx));
        }
    }

    /// The pointer entered or left the top edge strip or the band it brought back: the only reveal notify, so a move inside either does nothing.
    fn set_peek(&mut self, peek: bool, cx: &mut Context<Self>) {
        if self.minimal.on && self.minimal.peek != peek {
            self.minimal.peek = peek;
            cx.notify();
        }
    }

    /// Whether the window is in Minimal mode, for the control socket.
    pub(crate) fn is_minimal(&self) -> bool {
        self.minimal.on
    }
}

/// The id of the display the window is on, as settings save it.
fn display_uuid(window: &Window, cx: &App) -> Option<String> {
    window.display(cx).and_then(|display| display.uuid().ok()).map(|uuid| uuid.to_string())
}

/// `view` as a cached child that fills its parent.
fn cached(view: impl Into<AnyView>) -> impl IntoElement {
    view.into().cached(StyleRefinement::default().size_full())
}

/// The share of `split` to draw along `length` points. At rest (no glide, stretch or closing child)
/// both sides keep at least `PANE_MIN` when the room allows, and share it evenly when it doesn't; the
/// stored `ratio` stays as it was, so growing the window brings the old layout back.
fn drawn_share(split: &Split, now: Instant, length: f32) -> f32 {
    let share = split.share(now);
    let moving = split.stretch.is_some() || split.closing.is_some() || split.glide.is_some_and(|glide| !glide.is_done(now));
    if moving || length <= 0. {
        return share;
    }
    if length < PANE_MIN * 2. {
        return 0.5;
    }
    share.clamp(PANE_MIN / length, 1. - PANE_MIN / length)
}

/// The id of the innermost split with pane `pane` somewhere under it, the one whose divider sits
/// nearest the pane.
fn nearest_split(node: &Node, pane: PaneId) -> Option<usize> {
    fn holds(node: &Node, pane: PaneId) -> bool {
        match node {
            Node::Leaf(id) => *id == pane,
            Node::Split(split) => holds(&split.first, pane) || holds(&split.second, pane),
        }
    }
    let Node::Split(split) = node else { return None };
    [&split.first, &split.second].into_iter().find(|child| holds(child, pane)).and_then(|child| nearest_split(child, pane)).or(Some(split.id))
}

/// A drag held for a look: the ghost, where it is, and the pane whose bar lifted its tab.
struct Held {
    ghost: AnyView,
    pane: PaneId,
    zone: Zone,
    /// The tab the drag carries, when it is a tab.
    tab: Option<EntityId>,
    lifted: Option<Entity<Pane>>,
    /// The pane whose bar the drag is held over.
    bar: Option<Entity<Pane>>,
}

/// The look of the panes' overlays: the divider line, the focused pane's edge and the wash over the others.
struct Paint {
    /// How far the light shows, 0 with one pane to 1 with more, easing between.
    many: f32,
    /// A drag is under way: each pane takes drops on its edges and middle.
    dropping: bool,
    hairline: Hsla,
    /// A divider's line while it is under the pointer or held.
    divider_lit: Hsla,
    /// The focused pane's edge at full strength, and the share of it the window's key state leaves.
    edge: Hsla,
    edge_scale: f32,
    /// One device pixel.
    edge_height: Pixels,
    /// How focused each pane shows now, 0 to 1.
    lit: Vec<(PaneId, f32)>,
    /// The wash over an unfocused pane, at full strength.
    wash: Hsla,
}

/// Adds `closed` as the newest entry of the reopen list, dropping any earlier entry for the same
/// file so a file is listed once, and keeps only the last `CLOSED_KEPT`.
fn push_closed(list: &mut Vec<(PaneId, ClosedTab)>, pane: PaneId, closed: ClosedTab) {
    list.retain(|(_, earlier)| earlier.path != closed.path);
    list.push((pane, closed));
    if list.len() > CLOSED_KEPT {
        list.remove(0);
    }
}

/// The top band's room for the traffic lights at `level` of full screen (0 windowed, 1 full
/// screen): the corner pane's bar clearance and the explorer button's left edge. Windowed, the
/// bar clears the lights and the button sits right of them; full screen has no lights, so the
/// button moves to the edge and the bar clears only it. Both ride one level, so they move together.
fn full_screen_chrome(level: f32) -> (Pixels, Pixels) {
    let windowed = (TAB_LIGHTS_CLEAR, EXPLORER_BUTTON_LEFT);
    let full = (ICON_BUTTON_INSET + ICON_BUTTON + TAB_BAR_INSET, ICON_BUTTON_INSET);
    (windowed.0 + (full.0 - windowed.0) * level, windowed.1 + (full.1 - windowed.1) * level)
}

/// How lit each pane's focus is and how key the window is, each on the shared `Fade` (the frame
/// clock's time, turning around from the drawn level).
#[derive(Default)]
struct Light {
    panes: Vec<(PaneId, Fade)>,
    /// 1 with the window key, 0 without; `None` before the first frame.
    key: Option<Fade>,
    /// 1 while more than one pane shows, 0 with one; the light shows only as far as this.
    many: Option<Fade>,
}

impl Light {
    /// Aims the panes at `active`, the window at `key` and the whole light at `many` (more than one
    /// pane shows) at `now`; true while anything moves. `present` is every pane still drawn, the
    /// closing ones included: those fade to nothing from the level they are drawn at while they
    /// glide shut, and go with the pane. It runs with one pane too, so the pane a split opens
    /// beside is lit and fades out as the new one fades in, and the light of a split that closes
    /// to one pane fades away; a pane first seen after the first frame starts unlit.
    fn step(&mut self, live: &[PaneId], present: &[PaneId], active: PaneId, key: bool, many: bool, now: Instant) -> bool {
        let first = self.key.is_none();
        self.panes.retain(|(id, _)| present.contains(id));
        let mut moving = false;
        for id in live {
            let to = f32::from(*id == active);
            let at = match self.panes.iter().position(|(pane, _)| pane == id) {
                Some(at) => at,
                None => {
                    self.panes.push((*id, Fade::at(if first { to } else { 0. })));
                    self.panes.len() - 1
                }
            };
            let fade = &mut self.panes[at].1;
            fade.aim(to, now);
            moving |= fade.is_moving(now);
        }
        for (id, fade) in &mut self.panes {
            if !live.contains(id) {
                fade.aim(0., now);
                moving |= fade.is_moving(now);
            }
        }
        let many_to = f32::from(many);
        let fade = self.many.get_or_insert(Fade::at(many_to));
        fade.aim(many_to, now);
        moving |= fade.is_moving(now);
        let key_to = f32::from(key);
        let fade = self.key.get_or_insert(Fade::at(key_to));
        fade.aim(key_to, now);
        moving | fade.is_moving(now)
    }

    /// Each pane's level at `now`.
    fn levels(&self, now: Instant) -> Vec<(PaneId, f32)> {
        self.panes.iter().map(|(id, fade)| (*id, fade.level(now))).collect()
    }
}

impl Workspace {
    /// What a pane takes while a drag is over it: the four edge bands (each opens a new pane on its
    /// side) and the middle (moves into this pane), all below the tab bar, which takes its own
    /// drops. Each band says which zone the pointer is in; the window draws the preview.
    fn drop_zones(&self, pane: PaneId, cx: &mut Context<Self>) -> impl IntoElement {
        let edge = PANE_DROP_EDGE;
        let zone = |zone: Zone, name: &'static str| {
            div()
                .id(SharedString::from(format!("drop-zone-{name}-{pane}")))
                .absolute()
                .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<TabDrag>, _, cx| this.drop_over(pane, zone, event.bounds, event.event.position, cx)))
                .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<DraggedFiles>, _, cx| this.drop_over(pane, zone, event.bounds, event.event.position, cx)))
                .on_drag_move(cx.listener(move |this, event: &DragMoveEvent<ExternalPaths>, _, cx| this.drop_over(pane, zone, event.bounds, event.event.position, cx)))
                .on_drop(cx.listener(move |this, drag: &TabDrag, window, cx| {
                    // The ghost's copy glides from where it was let go into its slot in the pane the tab landed in.
                    let now = crate::motion::drawn_now(window.window_handle().window_id());
                    let (from, lift) = drag.ghost(window.mouse_position(), now);
                    if let Some(landed) = this.drop_on(Dropped::Tab(drag.id), pane, zone, window, cx) {
                        this.glide_to(landed, drag.id, from, lift, [0.; 2], now, cx);
                    }
                }))
                .on_drop(cx.listener(move |this, files: &DraggedFiles, window, cx| {
                    this.drop_on(Dropped::Files(files.0.to_vec()), pane, zone, window, cx);
                }))
                .on_drop(cx.listener(move |this, paths: &ExternalPaths, window, cx| {
                    this.drop_on(Dropped::Files(paths.paths().to_vec()), pane, zone, window, cx);
                }))
        };
        let left = zone(Zone::Left, "left").top_0().bottom_0().left_0().w(relative(edge));
        let right = zone(Zone::Right, "right").top_0().bottom_0().right_0().w(relative(edge));
        let top = zone(Zone::Top, "top").top_0().left(relative(edge)).right(relative(edge)).h(relative(edge));
        let bottom = zone(Zone::Bottom, "bottom").bottom_0().left(relative(edge)).right(relative(edge)).h(relative(edge));
        let middle = zone(Zone::Middle, "middle").top(relative(edge)).bottom(relative(edge)).left(relative(edge)).right(relative(edge));
        div().absolute().top(self.band(pane)).left_0().right_0().bottom_0().child(left).child(right).child(top).child(bottom).child(middle)
    }

    /// A drag moved over a band of `pane`: the zone it marks is the drop target while the pointer
    /// is in it, and stops being the target when the pointer leaves it.
    fn drop_over(&mut self, pane: PaneId, zone: Zone, band: Bounds<Pixels>, pointer: Point<Pixels>, cx: &mut Context<Self>) {
        let target = if band.contains(&pointer) {
            Some((pane, zone))
        } else if self.drop_target == Some((pane, zone)) {
            None
        } else {
            return;
        };
        if target.is_some() && self.drop_target != target {
            crate::haptics::align();
        }
        if self.drop_target != target {
            self.drop_target = target;
            cx.notify();
        }
    }

    /// Where the drop preview goes for a drop on `zone` of `pane`, as `[x, y, width, height]`: the
    /// half of the pane on an edge's side, the whole pane under the bar for the middle, each a
    /// little inside.
    fn drop_rect(&self, pane: PaneId, zone: Zone) -> Option<[f32; 4]> {
        let bounds = self.slot(pane)?.bounds.get();
        let band = f32::from(self.band(pane));
        let (x, y, width, height) = (f32::from(bounds.origin.x), f32::from(bounds.origin.y) + band, f32::from(bounds.size.width), f32::from(bounds.size.height) - band);
        let [x, y, width, height] = match zone {
            Zone::Left => [x, y, width / 2., height],
            Zone::Right => [x + width / 2., y, width / 2., height],
            Zone::Top => [x, y, width, height / 2.],
            Zone::Bottom => [x, y + height / 2., width, height / 2.],
            Zone::Middle => [x, y, width, height],
            Zone::Bar(_) => return None,
        };
        Some([x + PANE_DROP_INSET, y + PANE_DROP_INSET, (width - 2. * PANE_DROP_INSET).max(0.), (height - 2. * PANE_DROP_INSET).max(0.)])
    }

    /// One node of the split tree as an element: a pane (its editor, its bar over the top, and
    /// the focus overlay) or a split (its two children, sized by the share the split draws now,
    /// and the divider between them).
    ///
    /// `lay` is the width the panes are laid out at while a column slides: each pane keeps the text width it
    /// has at that layout (so nothing rewraps per frame) and is clipped to the room the split gives it, so the
    /// divider moves with the slide.
    fn node(&self, node: &Node, now: Instant, paint: &Paint, lay: Option<f32>, cx: &mut Context<Self>) -> AnyElement {
        match node {
            Node::Leaf(id) => {
                let Some(slot) = self.slot(*id) else { return div().into_any_element() };
                let bounds = slot.bounds.clone();
                let this = cx.weak_entity();
                let pending = self.moves_pending.clone();
                let lit = paint.lit.iter().find(|(pane, _)| pane == id).map_or(0., |(_, lit)| *lit);
                let band = self.band(*id);
                let bar_band = self.bar_band(*id);
                let bar_level = if self.tree.at_top(*id) { self.minimal.chrome() } else { 1. - self.minimal.levels.0 };
                let (fading, entering) = {
                    let editor = slot.editor.read(cx);
                    (editor.fading(), editor.entering())
                };
                // A pane that gave its last tab away and glides shut has nothing to say: only a pane that stays shows the card.
                let empty = (slot.pane.read(cx).len() == 0 && !slot.holding && self.tree.live().contains(id)) || entering;
                // The pane moved or resized (a split or close gliding, the window resizing): which panes lie beside
                // which is read from these places once the frame is drawn, one callback for all the panes, and the
                // bar is taken again only when some pane's neighbours changed. Under a column's slide the place is the
                // room the split gives the pane, not the width its text is held at.
                let report = move || {
                    let (bounds, this, pending) = (bounds.clone(), this.clone(), pending.clone());
                    canvas(
                        move |place, _, cx| {
                            if bounds.replace(place) != place && !pending.replace(true) {
                                let this = this.clone();
                                let pending = pending.clone();
                                cx.defer(move |cx| {
                                    pending.set(false);
                                    if this.update(cx, |this, cx| this.settle_moves(cx)).unwrap_or(false) {
                                        menu::refresh(cx);
                                    }
                                });
                            }
                        },
                        |_, (), _, _| {},
                    )
                    .absolute()
                    .inset_0()
                };
                let pane = div()
                    .relative()
                    .size_full()
                    .overflow_hidden()
                    .children(crate::control::fresh("empty-state"))
                    // A pane with no tabs says what to do next, centered in the whole pane. It is drawn under the
                    // editor, which fades out over it and in from it.
                    .when(empty, |pane| {
                        let actions = vec![
                            empty_state::Action {
                                id: "empty-new-page",
                                label: "New tab",
                                key: Some("\u{2318}T"),
                                run: Rc::new(cx.listener(|this, _: &ClickEvent, window, cx| {
                                    this.new_tab(window, cx);
                                })),
                            },
                            empty_state::Action {
                                id: "empty-open",
                                label: "Open\u{2026}",
                                key: Some("\u{2318}O"),
                                run: Rc::new(|_, window, cx| window.dispatch_action(Box::new(Open), cx)),
                            },
                            empty_state::Action {
                                id: "empty-open-draft",
                                label: "Open draft\u{2026}",
                                key: Some("\u{2325}\u{2318}O"),
                                run: Rc::new(|_, window, cx| window.dispatch_action(Box::new(ShowDraftList), cx)),
                            },
                        ];
                        // A pane too short for the whole column fades the symbol and the line out and gives up their room.
                        let level = slot.fit.get().level(now);
                        pane.child(
                            div()
                                .absolute()
                                .inset_0()
                                .child(empty_state::fitted_at(("pane-empty", *id), "doc", "No open file", "Start a new tab, open a file, or open a draft.".into(), actions, level, cx))
                                .children(crate::control::track("empty-state")),
                        )
                    })
                    // The editor is drawn only while a tab shows (or an old one fades out); the tab bar is a band over its top.
                    .when(slot.input.is_some() || slot.holding || fading, |pane| {
                        // A pane that is closing keeps drawing the editor it had, with no caret, until it goes.
                        let pane = pane.child(cached(slot.editor.clone()));
                        if slot.input.is_some() {
                            // The caret is a view of its own over the editor, so a blink never rebuilds the editor.
                            let layer = AnyView::from(slot.editor.read(cx).layer()).cached(StyleRefinement::default().absolute().inset_0());
                            pane.child(layer).child(AnyView::from(slot.bars.clone()).cached(StyleRefinement::default().absolute().inset_0()))
                        } else {
                            pane
                        }
                    })
                    .when(self.minimal.levels.0 <= 0., |pane| {
                        pane.child(AnyView::from(slot.pane.clone()).cached(StyleRefinement::default().absolute().top_0().left_0().right_0().h(bar_band)))
                    })
                    // Minimal mode: the bar sits on the ground over the editor's smaller inset, fading with the chrome level (the top
                    // panes come back with the pointer at the edge), and is not drawn at all once it is gone.
                    .when(self.minimal.levels.0 > 0. && bar_level > 0., |pane| {
                        pane.child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .h(bar_band)
                                .occlude()
                                .on_mouse_down(MouseButton::Left, |event, window, _| {
                                    if event.click_count == 2 {
                                        window.titlebar_double_click();
                                    } else {
                                        window.start_window_move();
                                    }
                                })
                                .bg(cx.theme().background)
                                .opacity(bar_level)
                                .child(cached(slot.pane.clone())),
                        )
                    })
                    .when(lay.is_none(), |pane| pane.child(report()))
                    // Focus is light, not a border: a lit edge under the focused pane's bar, a wash over the rest.
                    .when(paint.many > 0. && lit > 0., |pane| {
                        let full = Hsla { a: paint.edge.a * lit * paint.many * paint.edge_scale, ..paint.edge };
                        let clear = Hsla { a: 0., ..full };
                        let ramp = |from: Hsla, to: Hsla| linear_gradient(90., linear_color_stop(from, 0.), linear_color_stop(to, 1.));
                        pane.child(
                            div()
                                .absolute()
                                .top(band - paint.edge_height)
                                .left_0()
                                .right_0()
                                .h(paint.edge_height)
                                .flex()
                                .child(div().h_full().w(relative(0.25)).bg(ramp(clear, full)))
                                .child(div().h_full().w(relative(0.5)).bg(full))
                                .child(div().h_full().w(relative(0.25)).bg(ramp(full, clear))),
                        )
                    })
                    .when(paint.many > 0. && lit < 1., |pane| {
                        pane.child(div().absolute().inset_0().bg(Hsla { a: paint.wash.a * (1. - lit) * paint.many, ..paint.wash }))
                    })
                    .when(paint.dropping, |pane| pane.child(self.drop_zones(*id, cx)));
                // The closed-bar Find next count and the "Following" chip sit in the active pane's top right corner (the
                // find float draws over the chip; each draws nothing until it has something to say). They lay out with the
                // pane in the frame being drawn, so a live resize never leaves them a point off the pane's edge.
                let active = *id == self.active;
                let at = move || div().absolute().right(FIND_INSET_RIGHT).top(band + FIND_INSET_TOP);
                let corner = active
                    .then(|| at().child(self.follow_chip.clone()).children(crate::control::track("follow-chip")))
                    .into_iter()
                    .chain(self.find_float.iter().filter(|_| active).map(|float| at().child(float.clone())));
                // Beside the pane's clip, not inside it: a narrow pane doesn't cut the count's left edge.
                match lay {
                    None => div().relative().size_full().child(pane).children(corner).into_any_element(),
                    Some(width) => div()
                        .relative()
                        .size_full()
                        .child(div().relative().size_full().overflow_hidden().child(div().absolute().top_0().bottom_0().left_0().w(px(width)).child(pane)).child(report()))
                        .children(corner)
                        .into_any_element(),
                }
            }
            Node::Split(split) => {
                let (id, bounds) = (split.id, split.bounds.clone());
                let side_by_side = split.axis == SplitAxis::Right;
                let (rest, lit_ink) = (paint.hairline, paint.divider_lit);
                // The line is lit while the pointer is over its strip or the divider is held, and eases both ways.
                let lit = self.divider_hover == Some(id) || self.drag.is_some_and(|held| held.split == id);
                let line = by_lit(("pane-divider", id), lit, move |level| theme::hairline(mix(rest, lit_ink, level), side_by_side).size_full());
                let grab = div()
                    .id(("divider", id))
                    .absolute()
                    .occlude()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_hover(cx.listener(move |this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.divider_hover = Some(id);
                        } else if this.divider_hover == Some(id) {
                            this.divider_hover = None;
                        }
                        cx.notify();
                    }))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            if event.click_count == 2 {
                                this.equalize(id, window, cx);
                            } else {
                                this.start_drag(id, event.position, window, cx);
                            }
                            cx.stop_propagation();
                        }),
                    );
                // A pane opening or a sibling closing is laid out at the widest share the glide passes
                // through (`Split::held`), and the moving divider clips it: the pane that shrinks keeps
                // the layout it had, the one that grows has the layout it lands on, so nothing rewraps
                // on the way and no frame shows bare ground. A stretched divider holds the squeezed pane
                // at its limit, so a stretch frame lays out only the growing neighbour.
                // The first frame of a new split has no bounds yet; the pane it was made from still has its own.
                let along = |place: Bounds<Pixels>| f32::from(if side_by_side { place.size.width } else { place.size.height });
                let kept = |node: &Node| match node {
                    Node::Leaf(id) => self.slot(*id).map_or(0., |slot| along(slot.bounds.get())),
                    Node::Split(_) => 0.,
                };
                let length = match along(split.bounds.get()) {
                    0. => kept(&split.first).max(kept(&split.second)),
                    known => known,
                };
                let share = drawn_share(split, now, length);
                let landing = if length > 0. { split.held(now, length) } else { (None, None) };
                let held_lay = lay.map(|lay| {
                    if side_by_side {
                        // The share at the layout width, not the room the slide leaves, so the text doesn't rewrap per frame.
                        let held = drawn_share(split, now, lay);
                        (lay * landing.0.unwrap_or(held), lay * landing.1.unwrap_or(1. - held))
                    } else {
                        (lay, lay)
                    }
                });
                let (first, second) = (self.node(&split.first, now, paint, held_lay.map(|lay| lay.0), cx), self.node(&split.second, now, paint, held_lay.map(|lay| lay.1), cx));
                let hold = |content: AnyElement, landing: Option<f32>, from_end: bool| match landing {
                    None => content,
                    Some(landing) => {
                        let held = div().absolute();
                        let held = if side_by_side { held.top_0().bottom_0().w(px(length * landing)) } else { held.left_0().right_0().h(px(length * landing)) };
                        let held = match (side_by_side, from_end) {
                            (true, false) => held.left_0(),
                            (true, true) => held.right_0(),
                            (false, false) => held.top_0(),
                            (false, true) => held.bottom_0(),
                        };
                        held.child(content).into_any_element()
                    }
                };
                let (first, second) = (hold(first, landing.0, false), hold(second, landing.1, true));
                let (first, second, grab) = if side_by_side {
                    (
                        div().absolute().top_0().bottom_0().left_0().w(relative(share)).child(first),
                        div().absolute().top_0().bottom_0().right_0().w(relative(1. - share)).child(second),
                        // The strip reaches `DIVIDER_GRAB_LEAD` left of the line and the rest right of it, off the left pane's
                        // scrollbar; the line stays where it was, in a box centered on it.
                        grab.top_0()
                            .bottom_0()
                            .left(relative(share))
                            .ml(-DIVIDER_GRAB_LEAD)
                            .w(DIVIDER_GRAB)
                            .cursor_col_resize()
                            .child(div().absolute().top_0().bottom_0().left_0().w(DIVIDER_GRAB_LEAD * 2.).child(line)),
                    )
                } else {
                    (
                        div().absolute().left_0().right_0().top_0().h(relative(share)).child(first),
                        div().absolute().left_0().right_0().bottom_0().h(relative(1. - share)).child(second),
                        grab.left_0()
                            .right_0()
                            .top(relative(share))
                            .mt(-DIVIDER_GRAB / 2.)
                            .h(DIVIDER_GRAB)
                            .cursor_row_resize()
                            .child(line),
                    )
                };
                div()
                    .relative()
                    .size_full()
                    .child(canvas(move |place, _, _| bounds.set(place), |_, (), _, _| {}).absolute().inset_0())
                    .child(first.overflow_hidden())
                    .child(second.overflow_hidden())
                    .child(grab)
                    .into_any_element()
            }
        }
    }
}

/// The window's root: it draws only the frame (the top band, the resizable row and its fills) and
/// embeds the explorer, each pane's bar and editor, and the preview as cached siblings, so a frame
/// one of them asks for rebuilds only that view. It reads no document and no settings it has to clone.
impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The frame's one tick of the window clock: everything this frame draws, here and in the
        // explorer, panes and tooltips, reads this reading.
        let window_id = window.window_handle().window_id();
        let now = motion::frame_now(window_id);
        // The editors' caret glide, wheel glide and rubber band step on the same reading.
        gpui_kit::base::motion::set_frame_time(window_id, now);
        // A pane that finished closing leaves the tree; the others learn their corner in a moment.
        let removed = self.tree.reap(now);
        if !removed.is_empty() {
            self.panes.retain(|slot| !removed.contains(&slot.id));
            let workspace = cx.entity();
            window.defer(cx, move |window, cx| workspace.update(cx, |this, cx| this.sync_panes(window, cx)));
        }
        // Frames come from here alone, and only while a pane opens or closes.
        if self.tree.moving(now) {
            window.request_animation_frame();
        }
        let settings = settings::get(cx);
        let (left_setting, right_setting) = (settings.layout.left_panel_width, settings.layout.right_panel_width);
        let left_shows = settings.explorer.visible && !self.minimal.on && !self.scratch;
        let shows = self.preview_on(cx) && self.has_preview && !self.scratch;
        // The columns slide on the frame clock, which keeps pace with the display. The first frame
        // starts them at rest, so a window opens with its columns already in place.
        let slid = now;
        if self.slides_started {
            self.left.aim(left_shows, slid);
            self.right.aim(shows, slid);
        } else {
            (self.left, self.right, self.slides_started) = (Slide::settled(left_shows), Slide::settled(shows), true);
        }
        // The widths a column is drawn at while it slides, snapped to device pixels so Taffy doesn't
        // round each edge on its own.
        let scale = window.scale_factor().max(1.);
        let snap = |width: f32| (width * scale).round() / scale;
        let (min_width, max_width) = (f32::from(PANEL_WIDTH_MIN), f32::from(PANEL_WIDTH_MAX));
        let (full_left, full_right) = (left_setting.clamp(min_width, max_width), right_setting.clamp(min_width, max_width));
        // The editor keeps `EDITOR_MIN` of the window: the preview gives way first, then the
        // explorer. Only what is drawn is clamped, the saved widths stay as they are.
        let room = f32::from(window.viewport_size().width) - f32::from(EDITOR_MIN);
        let full_right = (room - if left_shows { full_left } else { 0. }).min(full_right).max(0.);
        let full_left = (room - if shows { full_right } else { 0. }).min(full_left).max(min_width.min(full_left));
        self.left.shape(full_left, false);
        self.right.shape(full_right, true);
        let sliding = self.left.moving(slid) || self.right.moving(slid);
        if sliding {
            window.request_animation_frame();
        }
        let (left_px, right_px) = (snap(self.left.level(slid) * full_left), snap(self.right.level(slid) * full_right));
        let left_width = self.layout.read(cx).sizes().first().filter(|width| **width != gpui_kit::base::PANEL_MIN_SIZE).map_or(full_left, |width| f32::from(*width).clamp(min_width.min(full_left), full_left));
        // The explorer's or preview's edge ticks once when a drag brings the column to its rest width
        // (the widest it goes) or its least; a slide or a window resize moving it ticks nothing.
        {
            let sizes = self.layout.read(cx).sizes().clone();
            let viewport = f32::from(window.viewport_size().width);
            let steady = !sliding && self.edge_pinned.1 == viewport;
            let mut pinned = [true; 2];
            for (at, (shown, ix, full)) in [(left_shows, 0, full_left), (shows, 2, full_right)].into_iter().enumerate() {
                if let (true, Some(width)) = (shown, sizes.get(ix)) {
                    let width = f32::from(*width);
                    pinned[at] = width >= full - 0.5 || width <= min_width.min(full) + 0.5;
                }
            }
            if steady && (0..2).any(|at| pinned[at] && !self.edge_pinned.0[at]) {
                crate::haptics::align();
            }
            self.edge_pinned = (pinned, viewport);
        }
        // AppKit is told only when the column or the theme changes.
        let material = (if sliding { left_px } else if left_shows { left_width } else { 0. }, theme::pin(cx));
        if self.material != Some(material) {
            window_look::sidebar_material(window, material.0, material.1);
            self.material = Some(material);
        }
        // The corner pane's bar clears the traffic lights by what the explorer's drawn width doesn't.
        let corner = self.tree.corner();
        if let Some(slot) = self.panes.iter().find(|slot| slot.id == corner) {
            slot.pane.update(cx, |pane, cx| pane.set_left_edge(px(material.0), cx));
        }
        // The room the top band keeps for the lights eases in and out of full screen, on one level
        // for the pane bar's inset and the explorer button; it is also the clearance a divider drag keeps.
        self.chrome.aim(f32::from(window.is_fullscreen() || self.scratch), now);
        if self.chrome.is_moving(now) {
            window.request_animation_frame();
        }
        // Minimal mode: one level takes the tab bars, buttons, status line and top inset away together, on the column slide's own
        // length; another brings the top band back while the pointer is at the top edge. Frames come only while one moves.
        {
            let minimal = &mut self.minimal;
            minimal.hide.aim(f32::from(minimal.on), now);
            minimal.reveal.aim(f32::from(minimal.on && minimal.peek), now);
            minimal.levels = (minimal.hide.level(now), minimal.reveal.level(now));
            if minimal.hide.is_moving(now) || minimal.reveal.is_moving(now) {
                window.request_animation_frame();
            }
            // The window buttons fade on AppKit's own clock, told only when they flip (and not at all for a window that opens with them).
            let buttons = !minimal.on || minimal.peek;
            if !self.scratch && minimal.buttons != Some(buttons) && (minimal.buttons.is_some() || !buttons) {
                let seconds = if minimal.buttons.is_some() { crate::tokens::COLUMN_SLIDE } else { 0. };
                window_look::window_buttons(window, buttons, seconds, cx);
            }
            minimal.buttons = Some(buttons);
        }
        let chrome_level = self.minimal.chrome();
        let (clear, button_left) = full_screen_chrome(self.chrome.level(now));
        for slot in &self.panes {
            slot.pane.update(cx, |pane, cx| pane.set_clear(clear, cx));
        }
        self.corner_inset = (f32::from(clear) - material.0).max(f32::from(TAB_BAR_INSET));
        // The top right pane's bar leaves room for the preview button by how shut the preview is
        // drawn: the same level the column slides on, so the room follows it and turns with it.
        let top_right = self.tree.top_right();
        let right_room = if self.has_preview { tab_bar::button_room(self.right.level(slid)) } else { TAB_BAR_INSET };
        for slot in &self.panes {
            slot.pane.update(cx, |pane, cx| pane.set_right(if slot.id == top_right { right_room } else { TAB_BAR_INSET }, cx));
        }
        // Each pane's band eases to its place's height on the close or split glide's own clock, and
        // its bar and editor take it in this frame, so the two never disagree.
        if self.step_bands(now, cx) {
            window.request_animation_frame();
        }
        // An empty pane's symbol and line fade out as the pane gets short (the last frame's height; none yet is not short) and
        // back in as it grows; a pane with tabs just holds the level its height calls for.
        for slot in &self.panes {
            let height = slot.bounds.get().size.height;
            let to = if height > px(0.) && height < EMPTY_COMPACT_HEIGHT { 0. } else { 1. };
            let mut fit = slot.fit.get();
            if slot.pane.read(cx).len() == 0 {
                fit.aim(to, now);
            } else {
                fit = motion::Fade::with(to, FADE_QUICK);
            }
            if fit.is_moving(now) {
                window.request_animation_frame();
            }
            slot.fit.set(fit);
        }
        let theme = cx.theme();
        let (ground, hairline, divider_lit) = (theme.background, theme.border, theme.drag_border);
        let live = self.tree.live();
        let multi = live.len() > 1;
        // The focus light eases per pane on the shared fade, and the window's key state dims the edge
        // the way the explorer dims its selection.
        if self.light.step(&live, &self.tree.leaves(), self.active, control::window_key(window), multi, now) {
            window.request_animation_frame();
        }
        let key = self.light.key.map_or(1., |fade| fade.level(now));
        let dim = ALPHA_SELECTED_UNFOCUSED / ALPHA_SELECTED;
        // A drag the control socket holds counts as one under way, aimed at its zone.
        let dragging_over = cx.has_active_drag() || self.held.is_some();
        if let Some(held) = &self.held {
            self.drop_target = (!matches!(held.zone, Zone::Bar(_))).then_some((held.pane, held.zone));
        } else if !dragging_over {
            self.drop_target = None;
        }
        // The drop preview glides to the zone the drag is over and fades out when the drag leaves
        // every zone or ends; frames are asked for only while it moves.
        let zone = self.drop_target.and_then(|(pane, zone)| self.drop_rect(pane, zone));
        self.drop_preview.aim(zone, now);
        if self.drop_preview.is_moving(now) {
            window.request_animation_frame();
        }
        let preview_fade = self.drop_preview.opacity(now);
        let drop_preview = (preview_fade > 0.).then(|| {
            let [x, y, width, height] = self.drop_preview.rect(now);
            div()
                .absolute()
                .left(px(x))
                .top(px(y))
                .w(px(width))
                .h(px(height))
                .rounded(PANE_DROP_RADIUS)
                .bg(crate::explorer::icons::accent().opacity(ALPHA_DROP * preview_fade))
        });
        let held_ghost = self.held.as_ref().and_then(|held| {
            let at = self.held_point(held.pane, held.zone)?;
            Some(div().absolute().left(px(at[0])).top(px(at[1])).child(held.ghost.clone()))
        });
        let paint = Paint {
            many: self.light.many.map_or(0., |fade| fade.level(now)),
            // The edge bands exist only while something that can be dropped is being dragged.
            dropping: dragging_over,
            hairline,
            divider_lit,
            edge: theme::ink(if theme::is_light(cx) { PANE_EDGE_ALPHA_LIGHT } else { PANE_EDGE_ALPHA }, cx),
            edge_scale: dim + (1. - dim) * key,
            edge_height: px(f32::from(PANE_EDGE) / scale),
            lit: self.light.levels(now),
            wash: Hsla { a: PANE_WASH, ..ground },
        };

        // The band along the top the traffic lights sit in: it moves the
        // window, and a double-click zooms it by the system setting.
        let band = div()
            .id("top-band")
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .h(TOP_BAND + (MINIMAL_TOP - TOP_BAND) * self.minimal.levels.0)
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            });

        // The preview button, once for the window: fixed at the top right, centered on the
        // traffic lights' line, whether the preview is open or not, while the active pane's file
        // has a preview. `occlude` keeps a click on it from also starting a window move.
        let preview_button = (self.has_preview && !self.scratch && chrome_level > 0.).then(|| {
            // Out of key the glyph dims with the same eased level as the tabs and the status line.
            let (rest, lit) = (theme::ink(ALPHA_STATUS_DIM + (ALPHA_SECONDARY - ALPHA_STATUS_DIM) * key, cx), theme::ink(ALPHA_PRIMARY, cx));
            let label = tooltip::with_key(if shows { "Hide preview" } else { "Show preview" }, "⇧⌘V");
            let forced = tooltip::forced("preview-toggle", &label, point(ICON_BUTTON / 2., ICON_BUTTON / 2.), cx);
            // On while the preview is open, as the find bar's option buttons are (a wash behind the
            // glyph, the glyph at the primary ink), easing with the preview's slide (gpui-editor#5vea).
            let on = self.right.level(slid).clamp(0., 1.);
            let button = icon_button("toggle-preview", "sidebar.right", ICON_BUTTON, ICON_BUTTON_SYMBOL, Ink { rest: mix(rest, lit, on), lit, wash: theme::ink(1., cx) })
                .occlude()
                .tooltip(tooltip::tip(label))
                .tooltip_show_delay(tooltip::DELAY)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_preview(&TogglePreview, window, cx)))
                .children(forced)
                .children(crate::control::track("toggle-preview"));
            div()
                .absolute()
                .top((TOP_BAND - ICON_BUTTON) / 2.)
                .right(ICON_BUTTON_INSET)
                .rounded(RADIUS_BUTTON)
                .bg(theme::ink(ALPHA_BUTTON_PRESSED * on, cx))
                .when(chrome_level < 1., |button| button.opacity(chrome_level))
                .child(button)
        });

        // The explorer button, beside the traffic lights, in the same place whether the explorer
        // shows or not (in full screen, with no lights, at the left edge); it does what Cmd-B does.
        // On while the explorer is open, as the preview button is, easing with the explorer's slide.
        let explorer_button = (!self.scratch && chrome_level > 0.).then(|| {
            let (rest, lit) = (theme::ink(ALPHA_STATUS_DIM + (ALPHA_SECONDARY - ALPHA_STATUS_DIM) * key, cx), theme::ink(ALPHA_PRIMARY, cx));
            let label = tooltip::with_key(if left_shows { "Hide explorer" } else { "Show explorer" }, "⌘B");
            let forced = tooltip::forced("explorer-toggle", &label, point(ICON_BUTTON / 2., ICON_BUTTON / 2.), cx);
            let on = self.left.level(slid).clamp(0., 1.);
            let button = icon_button("toggle-explorer", "sidebar.left", ICON_BUTTON, ICON_BUTTON_SYMBOL, Ink { rest: mix(rest, lit, on), lit, wash: theme::ink(1., cx) })
                .occlude()
                .tooltip(tooltip::tip(label))
                .tooltip_show_delay(tooltip::DELAY)
                .on_click(cx.listener(|this, _, window, cx| this.toggle_explorer(&ToggleExplorer, window, cx)))
                .children(forced)
                .children(crate::control::track("toggle-explorer"));
            div()
                .absolute()
                .top((TOP_BAND - ICON_BUTTON) / 2.)
                .left(button_left)
                .rounded(RADIUS_BUTTON)
                .bg(theme::ink(ALPHA_BUTTON_PRESSED * on, cx))
                .when(chrome_level < 1., |button| button.opacity(chrome_level))
                .child(button)
        });

        // The active editor takes the window's typed text, from here and every frame: a cached pane
        // replays the input handler of its last fresh paint, which after a focus change is the
        // old one. This canvas paints last, so its handler is the one the window keeps. It also
        // watches the mouse for the rest: a press in a pane makes it the active one, and while a
        // divider is held the pointer moves it.
        let input_state = self.input.clone();
        let link = self.link.clone();
        let workspace = cx.entity();
        let (minimal_on, peeked) = (self.minimal.on, self.minimal.peek);
        let hits: Vec<(PaneId, Rc<Cell<Bounds<Pixels>>>)> =
            self.tree.live().iter().filter_map(|id| self.slot(*id).map(|slot| (*id, slot.bounds.clone()))).collect();
        let dragging = self.drag.as_ref().map(|drag| drag.split).and_then(|split| self.tree.split_mut(split)).map(|split| split.axis);
        let input = canvas(
            |_, _, _| (),
            move |_, (), window, cx| {
                if let Some((focus, editor)) = &input_state {
                    window.handle_input(focus, ElementInputHandler::new(link.bounds.get(), editor.clone()), cx);
                }
                // Minimal mode: the top edge strip (or, once it brought the band back, the band) is watched for the pointer entering
                // or leaving it. The listener only compares a height; the window is told, and redrawn, when that flips.
                if minimal_on {
                    let reveal = workspace.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase.capture() {
                            let inside = event.position.y < if peeked { TOP_BAND } else { MINIMAL_EDGE };
                            // A tab dragged out of the band stays revealed until it lands.
                            if inside != peeked && (inside || !cx.has_active_drag()) {
                                reveal.update(cx, |this, cx| this.set_peek(inside, cx));
                            }
                        }
                    });
                    let left = workspace.clone();
                    window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
                        if phase.capture() && peeked {
                            left.update(cx, |this, cx| this.set_peek(false, cx));
                        }
                    });
                }
                let pressed = workspace.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    let Some(id) = hits.iter().find(|(_, bounds)| bounds.get().contains(&event.position)).map(|(id, _)| *id) else { return };
                    pressed.update(cx, |this, cx| {
                        if this.active != id {
                            this.set_active(id, true, window, cx);
                        }
                    });
                });
                if let Some(axis) = dragging {
                    // The pointer can outrun the 8 pt strip: the resize cursor holds until the button is released.
                    window.set_window_cursor_style(if axis == SplitAxis::Right { CursorStyle::ResizeLeftRight } else { CursorStyle::ResizeUpDown });
                    let moved = workspace.clone();
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                        if phase == DispatchPhase::Capture {
                            moved.update(cx, |this, cx| this.drag_to(event.position, cx));
                        }
                    });
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                        if phase == DispatchPhase::Capture {
                            workspace.update(cx, |this, cx| this.end_drag(window, cx));
                        }
                    });
                }
            },
        )
        .absolute()
        .size_0();

        // While a column slides the panes are laid out at the wider of the width the slide lands on and the width it
        // started at, each held at its own left edge, and the split is drawn in the room the columns leave: the
        // divider moves with the slide, the moving columns cover or uncover the held text, and it wraps once, at landing.
        let viewport = f32::from(window.viewport_size().width);
        let landed = (viewport - if left_shows { left_width + 1. } else { 0. } - if shows { full_right } else { 0. }).max(0.);
        let begun = (viewport - snap(self.left.started_at() * full_left) - if self.left.started_at() > 0. { 1. } else { 0. } - snap(self.right.started_at() * full_right)).max(0.);
        let landed = landed.max(begun);
        let panes = self.node(&self.tree.root, now, &paint, sliding.then_some(landed), cx);

        // At rest the row is the kit's resizable group, whose dividers drag. While a column slides
        // it is a plain row of two clips and the editor: each column is drawn at its full width
        // inside a clip whose width runs 0 to full, so only the clips' edges move.
        // The explorer's hover and selection shapes are their own view, painted under the column
        // (the explorer has no ground), so a glide redraws them and not the explorer's rows.
        let shapes = self.explorer.read(cx).shapes_layer();
        // The text fields' carets are a view of their own over the column, so a blink redraws
        // it and not the explorer's rows.
        let carets = self.explorer.read(cx).caret_layer().cached(StyleRefinement::default().absolute().inset_0());
        // The preview's caret wash is a view beside the preview pane, over it, so its frames never render the pane.
        let wash = AnyView::from(self.preview_pane.read(cx).wash_layer()).cached(StyleRefinement::default().absolute().inset_0());
        let row = if sliding {
            // The explorer's clip sits on the sidebar material, so it fades by its own opacity and covers nothing; the preview's clip is covered in the page ground.
            let clip = |side: &'static str, width: f32, offset: f32, fade: f32, full: f32, rule: bool, cover: Option<Hsla>, behind: Option<AnyView>, column: AnyElement| {
                div()
                    .id(side)
                    .relative()
                    .flex_none()
                    .h_full()
                    .w(px(width))
                    .overflow_hidden()
                    .children(behind.map(|behind| div().absolute().inset_0().child(behind)))
                    .child(div().absolute().top_0().bottom_0().left(px(offset)).w(px(full)).child(column))
                    // The preview's column is drawn solid, so its cover is one quad in the ground at 1 - fade; the explorer's has none
                    // (the material is behind it) and fades by opacity.
                    .when_some(cover.filter(|_| fade < 1.), |clip, cover| clip.child(div().absolute().inset_0().bg(Hsla { a: 1. - fade, ..cover })))
                    .when(cover.is_none() && fade < 1., |clip| clip.opacity(fade))
                    // The preview's divider is the clip's own left edge, so it travels with the panel.
                    .when(rule && width > 0., |clip| clip.child(div().absolute().top_0().bottom_0().left_0().w(px(1. / scale)).bg(hairline)))
            };
            let left = clip(
                "left-column",
                left_px,
                left_px - full_left,
                self.left.fade(slid),
                full_left,
                false,
                None,
                Some(shapes.clone()),
                div().size_full().child(cached(self.explorer.clone())).child(carets).into_any_element(),
            );
            let right = clip(
                "right-column",
                right_px,
                0.,
                self.right.fade(slid),
                full_right,
                true,
                Some(ground),
                None,
                div()
                    .relative()
                    .size_full()
                    .bg(ground)
                    .px(PREVIEW_INSET)
                    .pt(TOP_BAND)
                    .pb(PREVIEW_INSET) // design: `.px` is the horizontal padding method, the value is a token
                    .child(cached(self.preview_pane.clone()))
                    .child(wash)
                    .into_any_element(),
            );
            // The editor's left edge keeps a point for the explorer's hairline, which is one device pixel.
            div()
                .flex()
                .size_full()
                .child(left)
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .overflow_hidden()
                        .bg(ground)
                        .child(div().absolute().top_0().bottom_0().left(px(left_px.min(1.))).right_0().child(panes))
                        .when(left_px > 0., |panel| panel.child(div().absolute().top_0().bottom_0().left_0().w(px(1. / scale)).bg(hairline))),
                )
                .child(right)
                .into_any_element()
        } else {
            h_resizable("workspace")
                .with_handle_appearance(Rc::new(|handle: &ResizeHandleContext, window: &mut Window, cx: &mut App| {
                    // A divider is its hairline in the theme's ink, easing to the lit ink while the pointer is over it or holds it.
                    let lit = lit_level(!matches!(handle.state(), ResizeHandleState::Idle), window, cx);
                    let colors = cx.theme();
                    Some(theme::hairline(mix(colors.border, colors.drag_border, lit), true).flex_none().w(px(1.)).h_full().into_any_element())
                }))
                .with_state(&self.layout)
                .child(
                    resizable_panel()
                        .visible(left_shows)
                        .size(px(left_setting)) // design: settings-driven panel width
                        .size_range(px(min_width.min(full_left))..px(full_left))
                        .flex_none()
                        .child(shapes)
                        .child(cached(self.explorer.clone()))
                        .child(carets),
                )
                .child(
                    resizable_panel().child(
                        div()
                            .relative()
                            .size_full()
                            .bg(ground)
                            .when(left_shows, |panel| panel.border_l_1().border_color(Hsla::transparent_black()))
                            .child(panes),
                    ),
                )
                .child(
                    resizable_panel()
                        .visible(shows)
                        .size(px(right_setting)) // design: settings-driven panel width
                        .size_range(px(min_width.min(full_right))..px(full_right))
                        .flex_none()
                        .when(shows, |panel| {
                            panel.child(div().relative().size_full().bg(ground).px(PREVIEW_INSET).pt(TOP_BAND).pb(PREVIEW_INSET).child(cached(self.preview_pane.clone())).child(wash)) // design: `.px` is the horizontal padding method, the value is a token
                        }),
                )
                .into_any_element()
        };

        // The Go to line field, centered over the active pane's editor under its tab band.
        let go_to_line = self.go_to_line.as_ref().map(|(field, _)| {
            let bounds = self.slot(self.active).map(|slot| slot.bounds.get()).unwrap_or_default();
            // The float's width, capped so a narrow pane keeps the find bar's side inset on both edges.
            let width = GO_TO_LINE_WIDTH.min((bounds.size.width - FIND_INSET_RIGHT * 2.).max(px(0.)));
            div()
                .absolute()
                .left(bounds.origin.x + (bounds.size.width - width) / 2.)
                .top(bounds.origin.y + self.band(self.active) + FIND_INSET_TOP)
                .w(width)
                .child(field.clone())
        });

        // The Open Quickly float, centered over the active pane under its tab band like Go to line.
        let open_quickly = self.open_quickly.as_ref().map(|(float, _)| {
            let bounds = self.slot(self.active).map(|slot| slot.bounds.get()).unwrap_or_default();
            let width = OPEN_QUICKLY_WIDTH.min((bounds.size.width - FIND_INSET_RIGHT * 2.).max(px(0.)));
            div()
                .absolute()
                .left(bounds.origin.x + (bounds.size.width - width) / 2.)
                .top(bounds.origin.y + self.band(self.active) + FIND_INSET_TOP)
                .w(width)
                .child(float.clone())
        });

        // The Find in folder float, centered over the active pane under its tab band like Open quickly.
        let find_in_folder = self.find_in_folder.as_ref().map(|(float, _)| {
            let bounds = self.slot(self.active).map(|slot| slot.bounds.get()).unwrap_or_default();
            let width = FIND_IN_FOLDER_WIDTH.min((bounds.size.width - FIND_INSET_RIGHT * 2.).max(px(0.)));
            div()
                .absolute()
                .left(bounds.origin.x + (bounds.size.width - width) / 2.)
                .top(bounds.origin.y + self.band(self.active) + FIND_INSET_TOP)
                .w(width)
                .child(float.clone())
        });

        // The Go to symbol field, centered over the active pane under its tab band like Open quickly.
        let go_to_symbol = self.go_to_symbol.as_ref().map(|(field, _)| {
            let bounds = self.slot(self.active).map(|slot| slot.bounds.get()).unwrap_or_default();
            let width = OPEN_QUICKLY_WIDTH.min((bounds.size.width - FIND_INSET_RIGHT * 2.).max(px(0.)));
            div()
                .absolute()
                .left(bounds.origin.x + (bounds.size.width - width) / 2.)
                .top(bounds.origin.y + self.band(self.active) + FIND_INSET_TOP)
                .w(width)
                .child(field.clone())
        });

        // The draft list float, centered over the active pane under its tab band like Open quickly.
        let draft_list = self.draft_list.as_ref().map(|(float, _)| {
            let bounds = self.slot(self.active).map(|slot| slot.bounds.get()).unwrap_or_default();
            let width = OPEN_QUICKLY_WIDTH.min((bounds.size.width - FIND_INSET_RIGHT * 2.).max(px(0.)));
            div()
                .absolute()
                .left(bounds.origin.x + (bounds.size.width - width) / 2.)
                .top(bounds.origin.y + self.band(self.active) + FIND_INSET_TOP)
                .w(width)
                .child(float.clone())
        });

        // The version picker, centered over the active pane under its tab band like Open quickly (it draws over a Compare view too).
        let versions_list = self.versions_list.as_ref().map(|(float, _, _)| {
            let bounds = self.slot(self.active).map(|slot| slot.bounds.get()).unwrap_or_default();
            let width = OPEN_QUICKLY_WIDTH.min((bounds.size.width - FIND_INSET_RIGHT * 2.).max(px(0.)));
            div()
                .absolute()
                .left(bounds.origin.x + (bounds.size.width - width) / 2.)
                .top(bounds.origin.y + self.band(self.active) + FIND_INSET_TOP)
                .w(width)
                .child(float.clone())
        });

        // The Rename field, centered over the active pane's editor under its tab band like Go to line.
        let rename_field = self.rename_field.as_ref().map(|(field, _)| {
            let bounds = self.slot(self.active).map(|slot| slot.bounds.get()).unwrap_or_default();
            let width = RENAME_WIDTH.min((bounds.size.width - FIND_INSET_RIGHT * 2.).max(px(0.)));
            div()
                .absolute()
                .left(bounds.origin.x + (bounds.size.width - width) / 2.)
                .top(bounds.origin.y + self.band(self.active) + FIND_INSET_TOP)
                .w(width)
                .child(field.clone())
        });

        // A Compare view covers the editor of each pane whose tab shows one, under its tab band.
        let compares: Vec<_> = self
            .panes
            .iter()
            .filter_map(|slot| {
                let view = slot.pane.read(cx).active_document()?.read(cx).compare_view()?.clone();
                let (bounds, band) = (slot.bounds.get(), self.band(slot.id));
                Some(div().absolute().left(bounds.origin.x).top(bounds.origin.y + band).w(bounds.size.width).h((bounds.size.height - band).max(px(0.))).child(view))
            })
            .collect();

        // The Undo history slider, at the foot of the active pane above its status line. It closes when another tab shows.
        let undo_slider = self.undo_slider.as_ref().map(|(slider, _, document)| {
            if self.document(cx).as_ref() != Some(document) {
                let slider = slider.clone();
                cx.defer_in(window, move |_, window, cx| slider.update(cx, |slider, cx| slider.shut(Close::Keep, window, cx, true)));
            }
            let bounds = self.slot(self.active).map(|slot| slot.bounds.get()).unwrap_or_default();
            let width = undo_slider::WIDTH.min((bounds.size.width - FIND_INSET_RIGHT * 2.).max(px(0.)));
            div()
                .absolute()
                .left(bounds.origin.x + (bounds.size.width - width) / 2.)
                .top(bounds.origin.y + bounds.size.height - crate::tokens::STATUS_HEIGHT - FIND_INSET_TOP - undo_slider::HEIGHT)
                .w(width)
                .child(slider.clone())
        });

        div()
            .key_context("EditorWindow")
            .on_action(cx.listener(Self::save))
            .on_action(cx.listener(Self::toggle_explorer))
            .on_action(cx.listener(Self::toggle_minimal))
            .on_action(cx.listener(Self::enclosing_folder))
            .on_action(cx.listener(Self::focus_explorer))
            .on_action(cx.listener(Self::reveal_current_file))
            .on_action(cx.listener(Self::toggle_hidden_files))
            .on_action(cx.listener(Self::toggle_ignored_files))
            .on_action(cx.listener(Self::sort_explorer))
            .on_action(cx.listener(Self::collapse_all_folders))
            .on_action(cx.listener(Self::toggle_preview))
            .on_action(cx.listener(Self::open_linked))
            .on_action(cx.listener(Self::toggle_task))
            .on_action(cx.listener(Self::toggle_soft_wrap))
            .on_action(cx.listener(Self::cycle_wrap_width))
            .on_action(cx.listener(Self::show_go_to_line))
            .on_action(cx.listener(|this, _: &NextChange, window, cx| this.step_change(true, window, cx)))
            .on_action(cx.listener(|this, _: &PreviousChange, window, cx| this.step_change(false, window, cx)))
            .on_action(cx.listener(Self::show_go_to_symbol))
            .on_action(cx.listener(Self::show_open_quickly))
            .on_action(cx.listener(Self::show_find_in_folder))
            .on_action(cx.listener(Self::show_draft_list))
            .on_action(cx.listener(Self::browse_versions))
            .on_action(cx.listener(Self::show_undo_history))
            .on_action(cx.listener(Self::rename_file))
            .on_action(cx.listener(Self::duplicate_file))
            .on_action(cx.listener(|this, _: &NewFileInExplorer, window, cx| this.new_in_explorer(false, window, cx)))
            .on_action(cx.listener(|this, _: &NewFolderInExplorer, window, cx| this.new_in_explorer(true, window, cx)))
            .on_action(cx.listener(Self::move_item_here))
            .on_action(cx.listener(Self::move_file))
            .on_action(cx.listener(Self::show_file_in_finder))
            .on_action(cx.listener(Self::copy_file_path))
            .on_action(cx.listener(Self::save_as))
            .on_action(cx.listener(Self::export_html))
            .on_action(cx.listener(Self::save_all_files))
            .on_action(cx.listener(Self::revert_to_saved))
            .on_action(cx.listener(Self::use_selection_for_find))
            .on_action(cx.listener(Self::use_selection_for_replace))
            .on_action(cx.listener(Self::close_window))
            .on_action(cx.listener(Self::minimize))
            .on_action(cx.listener(Self::zoom))
            .on_action(cx.listener(Self::new_tab_action))
            .on_action(cx.listener(Self::new_from_template))
            .on_action(cx.listener(Self::close_tab))
            .on_action(cx.listener(|this, _: &ReopenClosedTab, window, cx| {
                if !this.reopen_closed_tab(window, cx) {
                    this.refuse(window, cx);
                }
            }))
            .on_action(cx.listener(Self::close_other_tabs))
            .on_action(cx.listener(Self::close_tabs_to_the_right))
            .on_action(cx.listener(Self::close_tabs_to_the_left))
            .on_action(cx.listener(Self::move_active_tab_to_new_window))
            .on_action(cx.listener(|this, _: &FocusNextPane, window, cx| this.focus_cycle(1, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPreviousPane, window, cx| this.focus_cycle(-1, window, cx)))
            .on_action(cx.listener(|this, _: &EvenOutPanes, window, cx| this.even_out_panes(window, cx)))
            .on_action(cx.listener(|this, _: &MergePanes, window, cx| this.merge_panes(window, cx)))
            .on_action(cx.listener(|this, _: &MergeAllWindows, window, cx| this.merge_all_windows(window, cx)))
            .on_action(cx.listener(|this, _: &MoveTabEarlier, window, cx| this.step_active_tab(-1, window, cx)))
            .on_action(cx.listener(|this, _: &MoveTabLater, window, cx| this.step_active_tab(1, window, cx)))
            .on_action(cx.listener(Self::toggle_pin_tab))
            .on_action(cx.listener(Self::prev_tab))
            .on_action(cx.listener(Self::next_tab))
            .on_action(cx.listener(Self::prev_recent_tab))
            .on_action(cx.listener(Self::next_recent_tab))
            .on_modifiers_changed(cx.listener(|this, event: &ModifiersChangedEvent, _, cx| {
                if !event.modifiers.control {
                    this.end_switch(cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| {
                if !this.split(SplitAxis::Right, window, cx) {
                    this.refuse(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| {
                if !this.split(SplitAxis::Down, window, cx) {
                    this.refuse(window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &FocusPaneLeft, window, cx| this.focus_direction(Direction::Left, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneRight, window, cx| this.focus_direction(Direction::Right, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneUp, window, cx| this.focus_direction(Direction::Up, window, cx)))
            .on_action(cx.listener(|this, _: &FocusPaneDown, window, cx| this.focus_direction(Direction::Down, window, cx)))
            .on_action(cx.listener(|this, _: &MoveTabLeft, window, cx| this.move_active_tab(Direction::Left, window, cx)))
            .on_action(cx.listener(|this, _: &MoveTabRight, window, cx| this.move_active_tab(Direction::Right, window, cx)))
            .on_action(cx.listener(|this, _: &MoveTabUp, window, cx| this.move_active_tab(Direction::Up, window, cx)))
            .on_action(cx.listener(|this, _: &MoveTabDown, window, cx| this.move_active_tab(Direction::Down, window, cx)))
            .on_action(cx.listener(|this, _: &ShowTab1, window, cx| this.show_nth(1, window, cx)))
            .on_action(cx.listener(|this, _: &ShowTab2, window, cx| this.show_nth(2, window, cx)))
            .on_action(cx.listener(|this, _: &ShowTab3, window, cx| this.show_nth(3, window, cx)))
            .on_action(cx.listener(|this, _: &ShowTab4, window, cx| this.show_nth(4, window, cx)))
            .on_action(cx.listener(|this, _: &ShowTab5, window, cx| this.show_nth(5, window, cx)))
            .on_action(cx.listener(|this, _: &ShowTab6, window, cx| this.show_nth(6, window, cx)))
            .on_action(cx.listener(|this, _: &ShowTab7, window, cx| this.show_nth(7, window, cx)))
            .on_action(cx.listener(|this, _: &ShowTab8, window, cx| this.show_nth(8, window, cx)))
            .on_action(cx.listener(|this, _: &ShowTab9, window, cx| this.show_nth(9, window, cx)))
            .relative()
            .size_full()
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::let_go_outside))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                // Escape puts the text back as it was and closes the Undo history slider.
                if event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    if let Some((slider, _, _)) = this.undo_slider.as_ref() {
                        let slider = slider.clone();
                        cx.stop_propagation();
                        window.defer(cx, move |window, cx| slider.update(cx, |slider, cx| slider.shut(Close::Restore, window, cx, true)));
                        return;
                    }
                }
                // Escape slides the scratch window away; anything that used the key (the find bar, a menu, extra carets) took it before this.
                if this.scratch && event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    cx.stop_propagation();
                    scratch::dismiss(cx);
                }
                // Escape twice within half a second leaves Minimal mode; anything that used the key (the find bar, a popup, extra carets) took it before this.
                if this.minimal.on && event.keystroke.key == "escape" && !event.keystroke.modifiers.modified() {
                    let now = Instant::now();
                    if this.minimal.escape.take().is_some_and(|before| now.saturating_duration_since(before).as_secs_f32() <= MINIMAL_ESCAPE) {
                        cx.stop_propagation();
                        this.set_minimal(false, window, cx);
                    } else {
                        this.minimal.escape = Some(now);
                    }
                }
            }))
            .on_drag_move(cx.listener(Self::drag_moved))
            .children(control::frame_marker())
            .child(band)
            .child(self.tooltips.clone())
            .child(row)
            .children(preview_button)
            .children(explorer_button)
            .children(go_to_line)
            .children(go_to_symbol)
            .children(open_quickly)
            .children(find_in_folder)
            .children(draft_list)
            .children(rename_field)
            .children(compares)
            .children(versions_list)
            .children(undo_slider)
            .children(drop_preview)
            .children(held_ghost)
            .child(input)
    }
}

/// A file with unsaved edits as the alert names it: its title and where it lives.
pub(crate) type Unsaved = (String, Option<PathBuf>);

/// How many names an unsaved-files alert lists before it says "and N more": the close, quit and
/// Trash questions all use it.
pub(crate) const UNSAVED_NAMES_SHOWN: usize = 6;

/// The names the alert lists, one per line: at most `UNSAVED_NAMES_SHOWN`, then "and N more". A
/// title that two listed files share also names the folder it is in (“index.md” in docs).
fn unsaved_detail(files: &[Unsaved]) -> String {
    let mut lines: Vec<String> = files
        .iter()
        .take(UNSAVED_NAMES_SHOWN)
        .map(|(name, path)| {
            let twin = files.iter().filter(|(other, _)| other == name).count() > 1;
            let folder = path.as_deref().and_then(Path::parent).and_then(Path::file_name).map(|folder| folder.to_string_lossy().into_owned());
            match folder {
                Some(folder) if twin => format!("\u{201C}{name}\u{201D} in {folder}"),
                _ => format!("\u{201C}{name}\u{201D}"),
            }
        })
        .collect();
    if files.len() > UNSAVED_NAMES_SHOWN {
        lines.push(format!("and {} more", files.len() - UNSAVED_NAMES_SHOWN));
    }
    lines.join("\n")
}

/// Asks whether to save `names`, for closing tabs, closing a window and quitting. Resolves to the
/// button picked (0 Save, or Save all for several, 1 Don’t Save, or Discard all for several, 2
/// Cancel), or `None` if the window went away.
pub(crate) fn ask_unsaved_tabs(names: &[Unsaved], window: &mut Window, cx: &mut App) -> impl Future<Output = Option<usize>> + use<> {
    prompt_unsaved(names, if names.len() == 1 { "Don’t Save" } else { "Discard all" }, window, cx)
}

fn prompt_unsaved(names: &[Unsaved], discard: &str, window: &mut Window, cx: &mut App) -> impl Future<Output = Option<usize>> + use<> {
    let (message, detail) = match names {
        [(name, _)] => (
            format!("Do you want to save the changes to \u{201C}{name}\u{201D}?"),
            "Your changes will be lost if you don’t save them.".to_string(),
        ),
        _ => (
            format!("Do you want to save the changes to {} files?", names.len()),
            format!("{}\n\nYour changes will be lost if you don’t save them.", unsaved_detail(names)),
        ),
    };
    let save = if names.len() == 1 { "Save" } else { "Save all" };
    let buttons = [PromptButton::new(save), PromptButton::destructive(discard), PromptButton::cancel("Cancel")];
    let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &buttons, cx);
    async move { answer.await.ok() }
}

/// A confirmed close: the window fades out, or for the scratch window slides away and gives the
/// keyboard back to the app that was in front (`scratch::leaving`).
fn finish_close(window: &mut Window, cx: &mut App) {
    if scratch::is_scratch(window) {
        scratch::leaving(window, cx);
    } else {
        window_look::fade_out(window, cx);
    }
}

/// Closes `workspace`'s window as its close button does: after the save question when something is unsaved.
pub(crate) fn close(workspace: Entity<Workspace>, window: &mut Window, cx: &mut App) {
    if confirm_close(workspace, window, cx) {
        finish_close(window, cx);
    }
}

/// The window's close button: closes at once when nothing is unsaved,
/// otherwise asks first and closes the window itself on Save all (when every
/// save worked) or Don’t Save. Returns whether the window may close now.
fn confirm_close(workspace: Entity<Workspace>, window: &mut Window, cx: &mut App) -> bool {
    if window_look::is_closing(window.window_handle().window_id()) {
        return false;
    }
    workspace.update(cx, |workspace, cx| workspace.flush_drafts(cx));
    // A save is writing: closing now could leave the file cut short, so wait for it and ask again.
    let landings = workspace.update(cx, |workspace, cx| workspace.saves_landing(window, cx));
    if !landings.is_empty() {
        window
            .spawn(cx, async move |cx| {
                for landing in landings {
                    landing.await;
                }
                cx.update(|window, cx| {
                    if confirm_close(workspace, window, cx) {
                        finish_close(window, cx);
                    }
                })
                .ok();
            })
            .detach();
        return false;
    }
    let names = workspace.read(cx).unsaved_names(cx);
    if names.is_empty() {
        return true;
    }
    let answer = ask_unsaved_tabs(&names, window, cx);
    window
        .spawn(cx, async move |cx| {
            let close = match answer.await {
                Some(0) => {
                    let Ok(saved) = cx.update(|window, cx| workspace.update(cx, |workspace, cx| workspace.save_all(window, cx))) else {
                        return;
                    };
                    // The window took edits while the saves ran (a big file, a Save as panel): look again, and ask about anything new.
                    saved.await && cx.update(|window, cx| confirm_close(workspace, window, cx)).unwrap_or(false)
                }
                Some(1) => cx
                    .update(|window, cx| {
                        // Edits made since the question weren't discarded by the answer: ask again.
                        workspace.update(cx, |workspace, cx| workspace.flush_drafts(cx));
                        let new = workspace.read(cx).unsaved_names(cx).iter().any(|name| !names.contains(name));
                        !new || confirm_close(workspace, window, cx)
                    })
                    .unwrap_or(false),
                _ => false,
            };
            if close {
                cx.update(|window, cx| finish_close(window, cx)).ok();
            }
        })
        .detach();
    false
}

/// How the file showing in the key window (the first window while none is key) wraps, for the
/// View menu's checks: (wraps at all, wraps at the window's width); `None` with no window.
pub fn key_window_wraps(key: Option<&Entity<Workspace>>, cx: &App) -> Option<(bool, bool)> {
    let document = key?.read(cx).document(cx)?.read(cx);
    Some((document.soft_wrap(), document.wraps_full()))
}

/// Whether the key window is in Minimal mode, for the View menu's check.
pub fn key_window_minimal(key: Option<&Entity<Workspace>>, cx: &App) -> bool {
    key.is_some_and(|workspace| workspace.read(cx).is_minimal())
}

/// What View > Focus lights in the key window, for the menu's check; Off with no window.
pub fn key_window_focus(key: Option<&Entity<Workspace>>, cx: &App) -> crate::focus::Mode {
    key.map_or_else(Default::default, |workspace| workspace.read(cx).focus)
}

/// How the key window's tree is sorted, for View > Sort by's check; the saved order with no window.
pub fn key_window_sort(key: Option<&Entity<Workspace>>, cx: &App) -> crate::settings::ExplorerSort {
    key.map_or_else(|| crate::settings::get(cx).explorer.sort, |workspace| workspace.read(cx).explorer.read(cx).sort())
}

/// View > Focus: the key window lights `mode` in every editor it shows, and in the others as each comes forward.
pub fn set_key_focus(mode: crate::focus::Mode, cx: &mut App) {
    let Some(workspace) = key_workspace(cx) else { return };
    let handle = workspace.read(cx).window;
    handle
        .update(cx, |_, window, cx| {
            workspace.update(cx, |this, cx| {
                this.focus = mode;
                this.sync_panes(window, cx);
            })
        })
        .ok();
}

/// What the key window's tabs let the menu bar do, for the File and Window menus.
#[derive(Default)]
pub struct TabMenu {
    /// How many tabs the active pane has.
    pub tabs: usize,
    /// Whether the showing tab is pinned.
    pub pinned: bool,
    /// Whether Close other tabs would close something.
    pub can_close_others: bool,
    /// Whether Close tabs to the right would close something.
    pub can_close_right: bool,
    /// Whether Close tabs to the left would close something.
    pub can_close_left: bool,
    /// Whether Move tab earlier and Move tab later would move the showing tab.
    pub can_earlier: bool,
    pub can_later: bool,
    /// How many tabs the window holds in all its panes (Move tab to new window needs a second one).
    pub window_tabs: usize,
    /// Whether Reopen closed tab would bring a file back.
    pub can_reopen: bool,
    /// Whether a pane lies left, right, above and below the active one (the Move tab and Focus pane items).
    pub panes: [bool; 4],
    /// How many panes the window has (Focus next pane and Focus previous pane need a second one).
    pub pane_count: usize,
    /// How many windows are open (Merge all windows needs a second one).
    pub windows: usize,
}

/// The key window's tab state (the first window while none is key); empty with no window.
pub fn key_window_tab_menu(key: Option<&Entity<Workspace>>, cx: &App) -> TabMenu {
    let Some(workspace) = key else { return TabMenu::default() };
    let workspace = workspace.read(cx);
    let pane = workspace.active_pane().read(cx);
    let showing = pane.active_document().map(Entity::entity_id);
    TabMenu {
        tabs: pane.len(),
        pinned: showing.is_some_and(|id| pane.is_pinned_id(id)),
        can_close_others: showing.is_some_and(|id| pane.can_close_others(id)),
        can_close_right: showing.is_some_and(|id| pane.can_close_right(id)),
        can_close_left: showing.is_some_and(|id| pane.can_close_left(id)),
        can_earlier: showing.is_some_and(|id| pane.can_step_tab(id, -1)),
        can_later: showing.is_some_and(|id| pane.can_step_tab(id, 1)),
        window_tabs: workspace.tab_count(cx),
        can_reopen: workspace.can_reopen_closed(cx),
        panes: Direction::ALL.map(|direction| workspace.can_go(direction)),
        pane_count: workspace.tree.live().len(),
        windows: if workspace.scratch { 0 } else { open_workspaces(cx).iter().filter(|(_, other)| !other.read(cx).is_scratch()).count() },
    }
}

/// Whether the file showing in the key window has edits to revert, for File > Revert to saved.
pub fn key_window_can_revert(key: Option<&Entity<Workspace>>, cx: &App) -> bool {
    key.and_then(|workspace| workspace.read(cx).document(cx)).is_some_and(|document| document.read(cx).can_revert())
}

/// Whether the key window's showing tab has undo history to scrub in an editable editor, for Edit > Undo history.
pub fn key_window_can_undo_history(key: Option<&Entity<Workspace>>, cx: &App) -> bool {
    key.and_then(|workspace| workspace.read(cx).document(cx))
        .and_then(|document| document.read(cx).built_editor().cloned())
        .is_some_and(|editor| can_scrub(editor.read(cx)))
}

/// The one rule for Edit > Undo history, so the menu's grey and the handler's beep agree: some
/// step to go back to, and an editor that takes edits.
fn can_scrub(editor: &EditorState) -> bool {
    let (applied, waiting) = editor.history_counts();
    applied + waiting > 0 && editor.is_editable()
}

/// Whether the key window's showing tab has a file on disk, for File > Rename, Duplicate and Move to.
pub fn key_window_has_file(key: Option<&Entity<Workspace>>, cx: &App) -> bool {
    key.is_some_and(|workspace| workspace.read(cx).has_file_tab(cx))
}

/// Whether the key window has a folder tree, for File > New file in explorer and New folder in explorer (the scratch window has none).
pub fn key_window_has_tree(key: Option<&Entity<Workspace>>, cx: &App) -> bool {
    key.is_some_and(|workspace| !workspace.read(cx).scratch)
}

/// The key window's workspace, or the first one while none is key. Reads only the key window
/// when it is one of ours; every window is read only while none is key.
pub(crate) fn key_workspace(cx: &App) -> Option<Entity<Workspace>> {
    if let Some(workspace) = cx.active_window().and_then(|key| workspace_of(key, cx)) {
        return Some(workspace);
    }
    open_workspaces(cx).into_iter().next().map(|(_, workspace)| workspace)
}

/// Where a tab taken out of its window lands.
pub(crate) enum Landing {
    /// In another window's pane, at slot `at` (the end when past the last one).
    Window { handle: AnyWindowHandle, pane: PaneId, at: usize },
    /// In a new window, at `place` (cascaded when `None`).
    New(Option<(WindowBounds, Option<DisplayId>)>),
}

/// Moves the tab holding document `id` out of window `source` to `landing`: the document entity
/// leaves one workspace and joins another, so its text, caret, scroll, undo and unsaved edits
/// (an Untitled page's draft too) go with it, never read from disk again. A window left with no
/// tab closes. Call it with no window being updated (`cx.defer`).
pub(crate) fn move_tab_to(source: AnyWindowHandle, id: EntityId, landing: Landing, cx: &mut App) -> Result<(), String> {
    let card = CARD.take();
    let source_workspace = workspace_of(source, cx).ok_or("that window is closing")?;
    let target = match &landing {
        Landing::Window { handle, .. } if *handle == source => return Err("that tab is already in that window".into()),
        Landing::Window { handle, .. } => Some(workspace_of(*handle, cx).ok_or("that window is closing")?),
        Landing::New(_) => None,
    };
    let root = source_workspace.read(cx).root().to_path_buf();
    let taken = source.update(cx, |_, window, cx| source_workspace.update(cx, |workspace, cx| workspace.release_tab(id, window, cx))).map_err(|err| err.to_string())?;
    let Some((document, pinned)) = taken else { return Err("no such tab".into()) };
    let background = std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some();
    match (landing, target) {
        (Landing::Window { handle, pane, at }, Some(workspace)) => {
            handle
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| workspace.adopt_tab(document, pinned, pane, at, window, cx));
                    if !background {
                        window.activate_window();
                    }
                })
                .map_err(|err| err.to_string())?;
        }
        (Landing::New(place), _) => {
            crate::app::open_tab_window(document, pinned, root, place, card, background, cx);
        }
        (Landing::Window { .. }, None) => unreachable!("a window landing has its workspace"),
    }
    if source_workspace.read(cx).is_empty(cx) {
        source.update(cx, |_, window, cx| window_look::fade_out(window, cx)).ok();
    }
    Ok(())
}

/// Window > Merge all windows: moves every tab of every window but `front` (and the scratch window) into `front`'s pane `pane`, window
/// by window in the order they are open and each window's panes in reading order. Each window's tabs are taken out and put in at the
/// end as they are (`adopt_tabs`): none is shown, so a tab never shown stays unread, and the pane's own tab stays showing. A window
/// left with no tab closes. Call it with no window being updated (`cx.defer`).
fn merge_windows_into(front: AnyWindowHandle, pane: PaneId, cx: &mut App) {
    let Some(target) = workspace_of(front, cx) else { return };
    let background = std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some();
    let mut moved: Vec<Entity<Document>> = Vec::new();
    for (handle, workspace) in open_workspaces_except(Some(front), cx) {
        if workspace.read(cx).is_scratch() {
            continue;
        }
        let taken = handle.update(cx, |_, window, cx| {
            workspace.update(cx, |workspace, cx| {
                let ids: Vec<EntityId> = {
                    let live = workspace.tree.live();
                    live.iter().filter_map(|id| workspace.slot(*id)).flat_map(|slot| slot.pane.read(cx).documents().map(Entity::entity_id).collect::<Vec<_>>()).collect()
                };
                ids.into_iter().filter_map(|id| workspace.release_tab(id, window, cx)).collect::<Vec<_>>()
            })
        });
        let Ok(taken) = taken else { continue };
        moved.extend(taken.iter().map(|(document, _)| document.clone()));
        if taken.is_empty() {
            continue;
        }
        front
            .update(cx, |_, window, cx| {
                target.update(cx, |workspace, cx| workspace.adopt_tabs(taken, pane, window, cx));
                if !background {
                    window.activate_window();
                }
            })
            .ok();
        if workspace.read(cx).is_empty(cx) {
            handle.update(cx, |_, window, cx| window_look::fade_out(window, cx)).ok();
        }
    }
    // Every file moved is settled once, after all the windows are in: a file open in two other windows must keep one copy, not lose both.
    cx.defer(move |cx| settle_merged_twins(front, moved, cx));
}

/// After Merge all windows: a file that has several copies open (the front window's own, or from the windows merged) keeps one tab. Every
/// copy with edits stays; with none edited the first copy (the front window's, then window by window) stays; the clean rest close.
fn settle_merged_twins(front: AnyWindowHandle, moved: Vec<Entity<Document>>, cx: &mut App) {
    let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    // The front window now holds every copy, the moved ones after its own.
    let Some(all) = workspace_of(front, cx).map(|workspace| workspace.read(cx).documents(cx)) else { return };
    let mut seen = HashSet::new();
    for document in &moved {
        let Some(path) = document.read(cx).path.as_deref().map(real) else { continue };
        if !seen.insert(path.clone()) {
            continue;
        }
        let copies: Vec<&Entity<Document>> = all.iter().filter(|copy| copy.read(cx).path.as_deref().map(real).as_ref() == Some(&path)).collect();
        let edited: Vec<bool> = copies.iter().map(|copy| copy.read(cx).shows_dot()).collect();
        for at in twins_to_close(&edited) {
            copies[at].update(cx, |_, cx| cx.emit(crate::document::DropTab));
        }
    }
}

/// Which copies of one file close, given whether each has edits, in order: every copy with edits stays; with none edited the first stays;
/// the clean rest close.
fn twins_to_close(edited: &[bool]) -> Vec<usize> {
    let keep_first = !edited.iter().any(|edited| *edited);
    (0..edited.len()).filter(|at| !edited[*at] && !(keep_first && *at == 0)).collect()
}

thread_local! {
    /// The pane in another window whose bar a tab dragged out of its own window holds a gap open in (one drag at a time).
    static FORWARDED: Cell<Option<(AnyWindowHandle, PaneId)>> = const { Cell::new(None) };
    /// Where the card of a tab let go outside its window was, in global screen points: the window a drop opens grows from it. Taken by
    /// the next `move_tab_to`, so a move that isn't a drop (the control socket's) never uses a stale one.
    static CARD: Cell<Option<Bounds<Pixels>>> = const { Cell::new(None) };
}

/// The Finch window but `source` under `point` (global screen points), its workspace and `point` in its own coordinates.
fn window_under(source: AnyWindowHandle, point: Point<Pixels>, cx: &mut App) -> Option<(AnyWindowHandle, Entity<Workspace>, Point<Pixels>)> {
    let stack = cx.window_stack().unwrap_or_else(|| cx.windows());
    let candidates: Vec<_> = stack.into_iter().filter(|handle| *handle != source).filter_map(|handle| Some((handle, workspace_of(handle, cx)?))).collect();
    candidates.into_iter().find_map(|(handle, workspace)| {
        handle
            .update(cx, |_, window, cx| {
                let frame = crate::app::screen_frame(window, cx)?;
                frame.contains(&point).then(|| (handle, workspace.clone(), point - frame.origin))
            })
            .ok()
            .flatten()
    })
}

/// Folds the gap a drag opened in `pane` of window `handle`.
fn fold_gap(handle: AnyWindowHandle, pane: PaneId, cx: &mut App) {
    let Some(workspace) = workspace_of(handle, cx) else { return };
    handle
        .update(cx, |_, _, cx| {
            let pane = workspace.read(cx).slot(pane).map(|slot| slot.pane.clone());
            if let Some(pane) = pane {
                pane.update(cx, |pane, cx| pane.fold_gap(cx));
            }
        })
        .ok();
}

/// A tab dragged out of window `source` is at `point` (global screen points; `None` once it is
/// back inside): the window under it opens a gap in the bar under the pointer, and the gap that
/// was open elsewhere folds. Only a real change notifies a pane.
fn forward_gap(source: AnyWindowHandle, pinned: bool, point: Option<Point<Pixels>>, cx: &mut App) {
    let over = point.and_then(|point| window_under(source, point, cx));
    let now = over.and_then(|(handle, workspace, local)| {
        let pane = handle.update(cx, |_, _, cx| workspace.update(cx, |workspace, cx| workspace.gap_for(local, pinned, cx))).ok().flatten()?;
        Some((handle, pane))
    });
    if let Some((handle, pane)) = FORWARDED.replace(now).filter(|before| Some(*before) != now) {
        fold_gap(handle, pane, cx);
    }
}

/// Every open window's workspace.
pub(crate) fn open_workspaces(cx: &App) -> Vec<(AnyWindowHandle, Entity<Workspace>)> {
    open_workspaces_except(None, cx)
}

/// Every open window's workspace but `skip`'s: a window being updated can't be read.
pub(crate) fn open_workspaces_except(skip: Option<AnyWindowHandle>, cx: &App) -> Vec<(AnyWindowHandle, Entity<Workspace>)> {
    cx.windows().into_iter().filter(|handle| Some(*handle) != skip).filter_map(|handle| Some((handle, workspace_of(handle, cx)?))).collect()
}

/// One window's workspace; none for a window that is closing or isn't a workspace.
fn workspace_of(handle: AnyWindowHandle, cx: &App) -> Option<Entity<Workspace>> {
    if window_look::is_closing(handle.window_id()) {
        return None;
    }
    handle.read(cx, |root: Entity<Root>, cx| root.read(cx).view().clone().downcast::<Workspace>().ok()).ok().flatten()
}

/// After `keeper` was saved to `path` by Save As: every other tab on that file, in any window,
/// goes, so one file is never two tabs. A tab with no unsaved edits closes at once; one with
/// edits asks first (`Document::ask_replaced`), and Cancel keeps it. An Untitled page whose draft
/// is that file moves to a fresh draft name (`Document::leave_draft`), so it never writes over the save.
pub(crate) fn drop_twin_tabs(keeper: &Entity<Document>, path: &Path, cx: &mut App) {
    let real = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let wanted = real(path);
    for (handle, workspace) in open_workspaces(cx) {
        for document in workspace.read(cx).documents(cx) {
            if document == *keeper {
                continue;
            }
            let (file_twin, draft_twin) = {
                let document = document.read(cx);
                (document.path.as_deref().map(real).as_ref() == Some(&wanted), document.draft_path().map(real).as_ref() == Some(&wanted))
            };
            if draft_twin {
                document.update(cx, |document, cx| document.leave_draft(cx));
                continue;
            }
            if !file_twin {
                continue;
            }
            if document.read(cx).shows_dot() {
                handle.update(cx, |_, window, cx| document.update(cx, |document, cx| document.ask_replaced(window, cx))).ok();
            } else {
                document.update(cx, |_, cx| cx.emit(crate::document::DropTab));
            }
        }
    }
}

/// Whether a file write is running anywhere: one counted app-wide (a closed tab's included) or an
/// open document still taking the result of its own.
fn writes_pending(cx: &App) -> bool {
    crate::document::writes_in_flight() > 0
        || open_workspaces(cx).iter().any(|(_, workspace)| workspace.read(cx).documents(cx).iter().any(|document| document.read(cx).is_saving()))
}

/// Resolves once no file write is running anywhere. Polls on a timer, so the main thread stays
/// free to draw.
async fn writes_landed(cx: &mut AsyncApp) {
    while cx.update(|cx| writes_pending(cx)) {
        cx.background_executor().timer(Duration::from_millis(50)).await;
    }
}

/// Quits once every save still writing, in any window or none, has landed: the process exiting
/// mid-write would leave the file cut short. The waiting can change what is unsaved (a tab was
/// typed in, a save failed and left its dot), so it looks again after: a file not in `covered`
/// (the ones a question already settled) asks as Cmd-Q does.
fn quit_when_saved(covered: Vec<Unsaved>, cx: &mut App) {
    cx.spawn(async move |cx| {
        writes_landed(cx).await;
        cx.update(|cx| {
            let uncovered = open_workspaces(cx).iter().any(|(_, workspace)| workspace.read(cx).unsaved_names(cx).iter().any(|name| !covered.contains(name)));
            if uncovered {
                quit(cx);
            } else {
                cx.quit();
            }
        });
    })
    .detach();
}

/// Quits once every file write still running has landed; for the last window closing, when
/// there is no tab left to ask about.
pub fn quit_after_writes(cx: &mut App) {
    cx.spawn(async move |cx| {
        writes_landed(cx).await;
        // A window opened during the wait stays: its own close waits again.
        cx.update(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        });
    })
    .detach();
}

/// Flush drafts before deciding whether system termination needs the save question.
/// A failed draft write must count as unsaved text, just as it does for Cmd-Q.
pub(crate) fn quit_needs_question(cx: &mut App) -> bool {
    let workspaces = open_workspaces(cx);
    for (_, workspace) in &workspaces {
        workspace.update(cx, |workspace, cx| workspace.flush_drafts(cx));
    }
    writes_pending(cx) || workspaces.iter().any(|(_, workspace)| !workspace.read(cx).unsaved_names(cx).is_empty())
}

/// Cmd-Q: quits at once when no window has unsaved files, otherwise asks,
/// naming them. Save all saves each (an Untitled file asks where) and quits
/// only if every save worked; Don’t Save quits; Cancel stays. If a window
/// with unsaved files showed up while the question was open, it asks again.
pub fn quit(cx: &mut App) {
    // Deferred: the action may run while a window is being updated.
    cx.defer(|cx| {
        // A save is writing: wait for it before asking what is unsaved, since it may settle that.
        if writes_pending(cx) {
            cx.spawn(async move |cx| {
                writes_landed(cx).await;
                cx.update(quit);
            })
            .detach();
            return;
        }
        let workspaces = open_workspaces(cx);
        for (_, workspace) in &workspaces {
            workspace.update(cx, |workspace, cx| workspace.flush_drafts(cx));
        }
        let names: Vec<Unsaved> = workspaces.iter().flat_map(|(_, workspace)| workspace.read(cx).unsaved_names(cx)).collect();
        let Some((asker, _)) = workspaces.iter().find(|(_, workspace)| !workspace.read(cx).unsaved_names(cx).is_empty()) else {
            quit_when_saved(Vec::new(), cx);
            return;
        };
        let Ok(answer) = asker.update(cx, |_, window, cx| ask_unsaved_tabs(&names, window, cx)) else {
            cx.cancel_quit();
            return;
        };
        cx.spawn(async move |cx| {
            let saved_all = match answer.await {
                Some(0) => {
                    let fresh = cx.update(|cx| open_workspaces(cx));
                    for (handle, workspace) in fresh {
                        let Ok(saved) = handle.update(cx, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.save_all(window, cx))) else {
                            continue;
                        };
                        if !saved.await {
                            cx.update(|cx| cx.cancel_quit());
                            return;
                        }
                    }
                    true
                }
                Some(1) => false,
                _ => {
                    // Also release AppKit's deferred logout or Dock Quit request.
                    cx.update(|cx| cx.cancel_quit());
                    return;
                }
            };
            cx.update(|cx| quit_when_saved(if saved_all { Vec::new() } else { names }, cx));
        })
        .detach();
    });
}

/// How many folder entries `through_link` reads before it gives up on a large tree.
const LINK_SEARCH_ENTRIES: usize = 5000;

/// The spelling of `file` (already resolved, as every opened document's path is) that the tree rooted at `root` holds
/// when a folder symlink inside the tree leads to it: `root/link/deep/c.txt` for a link `root/link` to the folder
/// holding `deep`. `None` when no link in the tree (read outside-in, real folders only, up to `LINK_SEARCH_ENTRIES`
/// entries) points at a folder that contains the file.
fn through_link(root: &Path, file: &Path) -> Option<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut seen = 0;
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for entry in entries.flatten() {
            seen += 1;
            if seen > LINK_SEARCH_ENTRIES {
                return None;
            }
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                // Build and dependency folders are thousands of entries that hold no folder links worth following.
                if !matches!(entry.file_name().to_str(), Some(".git" | "node_modules" | "target")) {
                    pending.push(entry.path());
                }
            } else if kind.is_symlink() {
                let link = entry.path();
                // A link to the root or a folder above it is a loop back into the tree, never a way to a file outside it.
                let Ok(target) = link.canonicalize() else { continue };
                if root.starts_with(&target) {
                    continue;
                }
                if let Some(rest) = file.strip_prefix(&target).ok().filter(|rest| !rest.as_os_str().is_empty()) {
                    return Some(link.join(rest));
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::{through_link, twins_to_close, Light, PaneId};
    use std::time::{Duration, Instant};

    /// Reveal for a file the tree holds through a folder link (finch#nj59): the link's spelling, not a re-root.
    #[test]
    fn a_file_under_a_linked_folder_is_held_at_the_links_spelling() {
        let base = std::env::temp_dir().join(format!("finch-nj59-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (root, real) = (base.join("root"), base.join("real"));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(real.join("deep")).unwrap();
        std::fs::write(real.join("deep/c.txt"), "q").unwrap();
        std::os::unix::fs::symlink(&real, root.join("sub/link")).unwrap();
        let base = base.canonicalize().unwrap();
        let (root, real) = (base.join("root"), base.join("real"));
        assert_eq!(through_link(&root, &real.join("deep/c.txt")), Some(root.join("sub/link/deep/c.txt")));
        // A file no link leads to, and the linked folder itself, have no spelling in the tree.
        assert_eq!(through_link(&root, &base.join("elsewhere/x.txt")), None);
        assert_eq!(through_link(&root, &real), None);
        let _ = std::fs::remove_dir_all(&base);
    }

    /// Merge all windows with one clean file open in the front window and two others: one copy stays, never none.
    #[test]
    fn merged_copies_of_a_clean_file_leave_one() {
        assert_eq!(twins_to_close(&[false, false, false]), vec![1, 2]);
        assert_eq!(twins_to_close(&[false]), Vec::<usize>::new());
        assert_eq!(twins_to_close(&[false, true, false]), vec![0, 2]);
        assert_eq!(twins_to_close(&[true, true]), Vec::<usize>::new());
    }

    fn at(t0: Instant, ms: u64) -> Instant {
        t0 + Duration::from_millis(ms)
    }

    #[test]
    fn closed_list_keeps_one_entry_per_file() {
        use super::push_closed;
        use crate::pane::ClosedTab;
        use std::path::PathBuf;
        let mut list = Vec::new();
        for round in 0..50 {
            for name in ["a", "b", "c"] {
                push_closed(&mut list, 0, ClosedTab { path: PathBuf::from(name), index: round, caret: None, pinned: false });
            }
        }
        let paths: Vec<_> = list.iter().map(|(_, closed)| closed.path.to_str().unwrap()).collect();
        assert_eq!(paths, ["a", "b", "c"]);
        assert_eq!(list[2].1.index, 49);
    }

    #[test]
    fn full_screen_room_follows_one_eased_level() {
        use super::full_screen_chrome;
        use crate::motion::Fade;
        use crate::tokens::COLUMN_SLIDE;
        let at_level = |level: f32| {
            let (clear, left) = full_screen_chrome(level);
            (f32::from(clear), f32::from(left))
        };
        assert_eq!(at_level(0.), (120., 90.));
        assert_eq!(at_level(1.), (42., 12.));
        // Windowed to full screen, turned around midway: the room never jumps, both numbers sit
        // at the same share of their way at every frame, and it lands where it was headed.
        let t0 = Instant::now();
        let mut chrome = Fade::with(0., COLUMN_SLIDE);
        chrome.aim(1., t0);
        let mut last = at_level(chrome.level(t0));
        assert_eq!(last, (120., 90.));
        for ms in (16..=320).step_by(16) {
            let now = at(t0, ms);
            if ms == 192 {
                chrome.aim(0., now);
            }
            let drawn = at_level(chrome.level(now));
            assert!((drawn.0 - last.0).abs() < 12. && (drawn.1 - last.1).abs() < 12., "{ms} ms: {last:?} to {drawn:?}");
            assert!(((120. - drawn.0) / 78. - (90. - drawn.1) / 78.).abs() < 1e-4);
            last = drawn;
        }
        assert!(!chrome.is_moving(at(t0, 192 + 400)));
        assert_eq!(at_level(chrome.level(at(t0, 1000))), (120., 90.));
    }

    fn level(light: &Light, pane: PaneId, now: Instant) -> f32 {
        light.levels(now).iter().find(|(id, _)| *id == pane).map_or(-1., |(_, lit)| *lit)
    }

    #[test]
    fn split_crossfades_and_lands() {
        let t0 = Instant::now();
        let mut light = Light::default();
        // One pane: lit at rest, nothing asks for frames, and the light shows no further.
        assert!(!light.step(&[1], &[1], 1, true, false, t0));
        assert_eq!(level(&light, 1, t0), 1.);
        // A split opens pane 2 and focuses it: pane 1 fades out, pane 2 fades in, the light opens out.
        assert!(light.step(&[1, 2], &[1, 2], 2, true, true, at(t0, 10)));
        let (one, two) = (level(&light, 1, at(t0, 40)), level(&light, 2, at(t0, 40)));
        assert!(one < 1. && one > 0. && two > 0. && two < 1., "{one} {two}");
        assert!(light.many.unwrap().level(at(t0, 40)) > 0.);
        // Landed: frames stop.
        assert!(!light.step(&[1, 2], &[1, 2], 2, true, true, at(t0, 400)));
        assert_eq!((level(&light, 1, at(t0, 400)), level(&light, 2, at(t0, 400))), (0., 1.));
    }

    #[test]
    fn interrupted_focus_turns_around_from_where_it_is_drawn() {
        let t0 = Instant::now();
        let mut light = Light::default();
        light.step(&[1, 2], &[1, 2], 1, true, true, t0);
        light.step(&[1, 2], &[1, 2], 2, true, true, at(t0, 10));
        let drawn = level(&light, 2, at(t0, 50));
        // Focus goes back before it lands: pane 2 heads down from the level it shows.
        light.step(&[1, 2], &[1, 2], 1, true, true, at(t0, 50));
        assert_eq!(level(&light, 2, at(t0, 50)), drawn);
        assert!(level(&light, 2, at(t0, 70)) < drawn);
    }

    #[test]
    fn close_to_one_pane_fades_the_light_out() {
        let t0 = Instant::now();
        let mut light = Light::default();
        light.step(&[1, 2], &[1, 2], 2, true, true, t0);
        // Pane 2 closes, pane 1 takes focus and the light draws less as one pane remains.
        assert!(light.step(&[1], &[1, 2], 1, true, false, at(t0, 10)));
        assert!(light.many.unwrap().level(at(t0, 40)) < 1.);
        assert!(level(&light, 1, at(t0, 40)) > 0.);
        assert!(!light.step(&[1], &[1], 1, true, false, at(t0, 400)));
        assert_eq!(light.many.unwrap().level(at(t0, 400)), 0.);
    }

    #[test]
    fn closing_pane_light_fades_from_the_level_it_is_drawn_at() {
        let t0 = Instant::now();
        let mut light = Light::default();
        light.step(&[1, 2], &[1, 2], 2, true, true, t0);
        // Pane 2 starts closing: on its first frame it is still drawn at full, then it eases down.
        assert!(light.step(&[1], &[1, 2], 1, true, true, at(t0, 10)));
        assert_eq!(level(&light, 2, at(t0, 10)), 1.);
        let drawn = level(&light, 2, at(t0, 40));
        assert!(drawn < 1. && drawn > 0., "{drawn}");
        // Gone with the pane.
        light.step(&[1], &[1], 1, true, true, at(t0, 400));
        assert_eq!(level(&light, 2, at(t0, 400)), -1.);
    }

    fn files(entries: &[(&str, &str)]) -> Vec<super::Unsaved> {
        entries.iter().map(|(name, path)| ((*name).to_string(), Some(std::path::PathBuf::from(path)))).collect()
    }

    #[test]
    fn unsaved_list_shows_six_names_and_counts_the_rest() {
        let list = files(&[
            ("a.md", "/x/a.md"),
            ("index.md", "/x/docs/index.md"),
            ("c.md", "/x/c.md"),
            ("d.md", "/x/d.md"),
            ("index.md", "/x/guide/index.md"),
            ("f.md", "/x/f.md"),
            ("g.md", "/x/g.md"),
            ("h.md", "/x/h.md"),
        ]);
        assert_eq!(
            super::unsaved_detail(&list),
            "\u{201C}a.md\u{201D}\n\u{201C}index.md\u{201D} in docs\n\u{201C}c.md\u{201D}\n\u{201C}d.md\u{201D}\n\u{201C}index.md\u{201D} in guide\n\u{201C}f.md\u{201D}\nand 2 more"
        );
        // Six or fewer list in full, and an Untitled page has no folder to name.
        let few = vec![("Untitled".to_string(), None), ("Untitled".to_string(), None)];
        assert_eq!(super::unsaved_detail(&few), "\u{201C}Untitled\u{201D}\n\u{201C}Untitled\u{201D}");
    }
}
