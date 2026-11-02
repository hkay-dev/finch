//! The explorer: a folder tree of one project in a side panel.
//!
//! Folders load lazily, one level at a time, through the `ignore` crate, so
//! `.git` never shows and gitignored paths only with Show ignored files (dimmed). A folder is read on the background
//! executor the first time it is expanded; until then it carries one
//! placeholder child, because gpui-base draws an item with no children as a
//! file. Folders list first, then files, each in the window's sort (Finder's name order unless
//! View > Sort by picks Kind, Date modified or Size).

mod actions;
mod carets;
pub(crate) mod filter;
mod git;
pub(crate) mod icons;
pub(crate) mod row;
mod picked;
mod sticky;
mod watch;

use std::{
    cell::RefCell,
    cmp::Ordering,
    collections::{BTreeSet, HashMap, HashSet, VecDeque},
    iter::Peekable,
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
    time::{Duration, Instant},
};

use gpui_kit::base::{Tree, TreeEvent, TreeItem, TreeState, actions::Confirm};
use gpui_kit::component::input::{Copy as CopyAction, Escape, Paste as PasteAction, Redo, SelectAll as SelectAllAction, Undo};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::{
    drag::DraggedFiles,
    motion::{self, Fold, ScrollGlide},
    text_drag::DraggedText,
    native_menu::{self, COMMAND, CONTROL, Entry, Item, OPTION, SHIFT},
    hover::{self, FadeHover as _, icon_button},
    settings, symbols, theme,
    tokens::{
        ALPHA_DIM, ALPHA_PRIMARY, ALPHA_SECONDARY, ALPHA_STATUS_DIM, ALPHA_BUTTON_HOVER, ALPHA_BUTTON_OFF, ALPHA_BUTTON_PRESSED, ALPHA_DROP, HEADER_BUTTON_SYMBOL, HEADER_CHEVRON, HEADER_CHEVRON_GAP, HEADER_GAP,
        HEADER_HEIGHT, HEADER_INSET, ICON_BUTTON, LOADING_DELAY, RADIUS_BUTTON, RADIUS_ROW, REVEAL_INSET, ROW_HEIGHT, SETTLE, TEXT_BODY, TEXT_SUBHEADLINE, TOP_BAND, TREE_INSET, ARRIVED_MAX, FADE_QUICK, WARM_SHUT_ROWS,
    },
    tooltip,
};
use gpui_kit::prelude::FluentBuilder as _;
use ignore::{DirEntry, WalkBuilder};

const PLACEHOLDER_LOADING: &str = "\0loading";
const PLACEHOLDER_EMPTY: &str = "\0empty";
/// The labels of the placeholder (it keeps `PLACEHOLDER_EMPTY`'s id) of a folder that can't be read or isn't there.
const UNREADABLE: &str = "Can’t read this folder";
const MISSING: &str = "This folder isn’t there";
/// How long after a fold's spring settles its folder is shut for real, so the last frame has drawn.
const LAND_MARGIN: Duration = Duration::from_millis(60);
/// A pause this long in typing letters starts the type-to-select prefix over.
const TYPE_RESET: Duration = Duration::from_millis(1000);
/// The key of Open and Expand in a row's menu, with Command: the Down Arrow, as AppKit writes it.
const OPEN_KEY: &str = "\u{F701}";
/// How many shut folders keep their loaded children, so a quick reopen still slides with its rows.
/// A folder shut longer than the last this many lets go of them (and everything loaded under it);
/// opening it then reads it again as for a first open. Folders shut under a shut one count each.
/// The rows they keep are capped too (`WARM_SHUT_ROWS`), the longest shut going first.
const WARM_SHUT: usize = 12;
/// How many folders one Option-Right opens, so a huge tree is never read whole.
const DEEP_MAX: usize = 500;

/// A row a re-read took out of the tree, as the next sync finds it: what a ghost draws, and whether it was an open
/// folder (whose rows went with it).
struct Gone {
    id: SharedString,
    label: SharedString,
    is_folder: bool,
    open: bool,
    /// Drawn dimmed: a dot file, or one shown only because Show ignored files is on.
    dim: bool,
}

/// The removed rows that are on screen, read just before the tree gets its new items: each with its old index, its
/// depth and git status, and the ids of the old rows, to find the first one below it that survived.
struct Departing {
    rows: Vec<(usize, Gone, usize, Option<git::Status>)>,
    ids: Vec<SharedString>,
}

actions!(
    explorer,
    [EnclosingFolder, RevealCurrentFile, ToggleHiddenFiles, ToggleIgnoredFiles, CollapseAllFolders, FocusExplorer]
);

/// View > Sort by's item for one order: the key window's tree sorts each folder's rows that way.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = explorer, no_json)]
pub struct SortExplorer {
    pub sort: settings::ExplorerSort,
}

/// Whether the tree shows dot files.
fn show_hidden(cx: &App) -> bool {
    settings::get(cx).explorer.show_hidden
}

/// Whether the tree shows what `.gitignore` files leave out.
fn show_ignored(cx: &App) -> bool {
    settings::get(cx).explorer.show_ignored
}

/// What a folder read lists and in what order.
#[derive(Clone, Copy)]
struct Listing {
    hidden: bool,
    ignored: bool,
    sort: settings::ExplorerSort,
}

/// The longest a file or folder name runs in a menu title, in characters.
pub(crate) const MENU_NAME_MAX: usize = 28;

/// `name` with its middle cut to an ellipsis past `max` characters, keeping the start and the extension, so a
/// long name never widens a menu or the tab list.
pub(crate) fn cut_middle(name: &str, max: usize) -> String {
    let count = name.chars().count();
    if count <= max {
        return name.to_string();
    }
    // The extension is what follows the last dot, unless that is most of the name.
    let tail = match name.rfind('.') {
        Some(dot) if dot > 0 && name[dot..].chars().count() <= 10 => name[dot..].chars().count(),
        _ => 0,
    } + 6;
    let head = max - 1 - tail;
    let start: String = name.chars().take(head).collect();
    let end: String = name.chars().skip(count - tail).collect();
    format!("{start}\u{2026}{end}")
}

/// The letter a key press types for type-to-select: one printable character, with at most Shift
/// held. Space keeps Quick Look and a held Command, Control or Option key is a shortcut's.
fn typed_letter(keys: &gpui::Keystroke) -> Option<char> {
    let held = keys.modifiers;
    if held.platform || held.control || held.alt || held.function {
        return None;
    }
    let mut chars = keys.key_char.as_deref()?.chars();
    let letter = chars.next().filter(|letter| !letter.is_whitespace() && !letter.is_control())?;
    chars.next().is_none().then_some(letter)
}

/// The row type-to-select moves to: of `rows` (name, disabled), the first from the selection down,
/// wrapping, whose name starts with `prefix` (lowercase), case-insensitively. A first letter
/// starts after the selection so repeating it steps to the next name; a longer prefix starts at
/// the selection, which stays put while it still matches.
fn prefix_match(rows: &[(&str, bool)], selected: Option<usize>, prefix: &str) -> Option<usize> {
    let from = match selected {
        Some(ix) if prefix.chars().count() <= 1 => ix + 1,
        Some(ix) => ix,
        None => 0,
    };
    (0..rows.len())
        .map(|step| (from + step) % rows.len())
        .find(|&ix| !rows[ix].1 && rows[ix].0.to_lowercase().starts_with(prefix))
}

/// What the explorer tells its owner.
pub enum ExplorerEvent {
    /// A file was clicked or chosen with Return.
    Open(PathBuf),
    /// A file was single-clicked: it opens as the pane's preview tab.
    Preview(PathBuf),
    /// A file was asked to open in the pane right of the active one: Option-click or the menu's
    /// Open to the Side.
    OpenToSide(PathBuf),
    /// Git's answer about the files under the root changed: change bars read their HEAD text again.
    Git,
    /// The tree was rooted at another folder.
    Root(PathBuf),
    /// A file or folder was renamed (or moved back by an undo): the owner moves the open tabs.
    Renamed { from: PathBuf, to: PathBuf },
    /// A file or folder was trashed or removed by an undo: the owner closes the tabs of it.
    Gone(PathBuf),
    /// Escape in the tree with nothing to close: the keyboard goes back to the editor.
    Leave,
}

/// What a menu item or key asks the explorer to do.
#[derive(Clone)]
enum Command {
    /// Opens a file, or expands or collapses a folder; several open every file and expand every
    /// folder.
    Open(Vec<PathBuf>),
    /// Opens or shuts a folder and every folder under it (Option held on Expand and Collapse).
    ToggleSubtree(PathBuf),
    /// Opens the file (first path) with the app (second path).
    OpenWith(PathBuf, PathBuf),
    QuickLook(PathBuf),
    /// Reveals these in one Finder window.
    ShowInFinder(Vec<PathBuf>),
    /// Copies these paths, one to a line.
    CopyPath(Vec<PathBuf>),
    CopyRelativePath(Vec<PathBuf>),
    /// Roots the tree at this folder.
    Root(PathBuf),
    GoUp,
    CollapseAll,
    ToggleHidden,
    ToggleIgnored,
    Sort(settings::ExplorerSort),
    /// Makes a file in this folder (or the file's folder), its name open for typing.
    NewFile(PathBuf),
    NewFolder(PathBuf),
    Rename(PathBuf),
    Duplicate(Vec<PathBuf>),
    Trash(Vec<PathBuf>),
    /// Asks for a folder, then moves these there.
    MoveTo(Vec<PathBuf>),
    OpenToSide(PathBuf),
    /// Puts these rows' files on the pasteboard.
    CopyFile(Vec<PathBuf>),
    /// Copies the files on the pasteboard into this folder (the folder of a file).
    Paste(PathBuf),
    /// Moves the files on the pasteboard into this folder (the folder of a file), as a drag does.
    MoveHere(PathBuf),
}

pub use filter::FocusFilter;

pub struct Explorer {
    root: PathBuf,
    /// The whole loaded tree. The tree state flattens it into shallow rows and shares its
    /// subtrees, so this copy is the one that gets new children when a folder loads (the
    /// first change after a sync copies one level). Items share their expanded state with
    /// the tree's.
    items: Vec<TreeItem>,
    tree: Entity<TreeState>,
    /// Folders whose children are read.
    loaded: HashSet<PathBuf>,
    /// Which of them are stale and which were shut most recently.
    cooling: Cooling,
    /// Folders being read right now.
    loading: HashSet<PathBuf>,
    /// A row to reveal once the root's first level has been read (the file a window opens on, or
    /// the old root's row after Enclosing folder).
    reveal_after: Option<PathBuf>,
    /// The file the showing document took (Save As) whose row hasn't arrived: the next read of
    /// the tree selects it once it shows. Dropped by a tab switch or a click in the tree.
    follow_pending: Option<PathBuf>,
    /// Opened by Option-Right and not read yet: the folders in each open when its rows land.
    deep_pending: HashSet<PathBuf>,
    /// How many more folders the press under way may open.
    deep_left: usize,
    marked: Rc<HashSet<PathBuf>>,
    /// The rows shown only because Show ignored files is on, and everything loaded under them: they draw dimmed.
    ignored: Rc<HashSet<PathBuf>>,
    _tree_events: Subscription,
    /// The root's shared feed, which this explorer takes file changes and git statuses from.
    watching: Option<Watching>,
    /// The watcher's handle, which follows the symlinked folders the tree has loaded.
    targets: Option<watch::Targets>,
    /// The filter field at the foot and the narrowed tree it shows.
    filter: filter::State,
    /// The folders folding open or shut; the rows read it as they are built.
    folds: Rc<RefCell<row::Folds>>,
    /// The rows the merges removed since the last sync, which it turns into closing ghosts.
    gone: Vec<Gone>,
    /// The "No files" line while that view shows, so the frame that replaces it can keep it as a ghost.
    emptied: Option<String>,
    /// That view fading out under the first file: its line, and the first frame that drew it (`None` until then).
    leaving: Option<(String, Option<Instant>)>,
    /// The hover and selection shapes behind the rows, a view of their own.
    shapes: Entity<row::ShapesLayer>,
    /// The text fields' carets over the rows, a view of their own.
    carets: Entity<carets::FieldCarets>,
    /// When the hand last scrolled the tree, for its scrollbar.
    scroller: crate::scrollbar::Scroller,
    /// A reveal's glide to a row out of view.
    glide: Rc<RefCell<ScrollGlide>>,
    /// Each row's depth, for the rows' fade under the pinned folder names: the list builds a row
    /// while the tree is borrowed, so it can't read the entries itself.
    pins: Rc<RefCell<Vec<u16>>>,
    /// The window the explorer is in, for its frame clock.
    window_id: WindowId,
    /// 1 with the window key, 0 without, eased on the shared fade: the header's rest ink dims with it, as the tabs and the status line do; `None` before the first frame.
    key: Option<motion::Fade>,
    /// The root's pull-down is open: its name holds the pressed look until the menu closes.
    root_held: bool,
    /// Wakes the explorer when its window turns key or not, so `key` can ease; set on the first frame.
    _activation: Option<Subscription>,
    /// The name field open in a row, for a rename or a new item.
    editing: Option<actions::Editing>,
    /// The rows Command- and Shift-click picked, besides the tree's selected row.
    picked: picked::Picked,
    /// What Cmd-Z takes back and Shift-Cmd-Z does again, most recent last.
    made: actions::Steps,
    /// The folder (or the root) that files being dragged over the tree would drop into.
    drop_target: Option<PathBuf>,
    /// Opens the shut folder under dragged files once they rest there.
    drop_timer: Option<(PathBuf, Task<()>)>,
    /// Where a drag over the tree's top or bottom band is and how fast (points per second, negative
    /// up) it scrolls the tree.
    edge_pull: Option<(Point<Pixels>, f32)>,
    /// When the edge scroll last moved the list, for its next step.
    edge_at: Option<Instant>,
    /// The lowercase letters typed in the tree for type-to-select and when the last came.
    typed: (String, Option<Instant>),
    /// What git says about the files under the root.
    git: Rc<git::Statuses>,
    /// The `.gitignore` of each folder read so far that has one, to tell a batch of ignored
    /// paths from one that can change git's answer. The root's feed holds them for every window
    /// showing it; a root that can't be watched keeps its own.
    ignores: Rc<RefCell<watch::Ignores>>,
    /// How each folder's rows are ordered in this window (View > Sort by).
    sort: settings::ExplorerSort,
}

/// An explorer's hold on its root's feed. The feed (the stream and its thread, one `git status`
/// per change) ends when the last window showing the root lets go of it.
struct Watching {
    feed: Entity<watch::Feed>,
    /// Turns the feed's batches of file changes into tree updates.
    _batches: Subscription,
    /// Takes the feed's git statuses.
    _statuses: Subscription,
}

impl EventEmitter<ExplorerEvent> for Explorer {}

/// Files under the root that were created, changed, renamed or removed on
/// disk, as absolute paths, once per batch. The owner reloads what it has open.
pub struct FilesChanged(pub Arc<[PathBuf]>);

impl EventEmitter<FilesChanged> for Explorer {}

/// One entry of a folder, as read from disk.
struct Child {
    name: String,
    is_dir: bool,
    /// The resolved target of a folder symlink.
    target: Option<PathBuf>,
    /// Left out by a `.gitignore` or `.ignore` file: read only with Show ignored files on.
    ignored: bool,
}

impl Explorer {
    /// Roots the tree at `root` (already resolved) and reads its first level in the background, so
    /// the window shows before any folder is read. With `reveal`, the folders down to that file are
    /// read with it, and once they land the tree expands to it, selects it and scrolls it into view.
    pub fn new(
        root: PathBuf,
        reveal: Option<&Path>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // No rows until the first level lands; a read slower than `LOADING_DELAY` shows "Loading".
        let tree = cx.new(|cx| TreeState::new(cx).items(Vec::new()));
        let tree_events = cx.subscribe_in(&tree, window, |this, _, event: &TreeEvent, _, cx| {
            // A filtered tree already holds what it shows; nothing is read or folded for it.
            if this.filter.is_active() {
                return;
            }
            // A click in the tree is the user's own pick: a pending follow no longer applies.
            this.follow_pending = None;
            match event {
                TreeEvent::Expanded(id) => {
                    let dir = PathBuf::from(id.to_string());
                    // A folder shut since a change (or one inside it) reads again before its rows fold in.
                    if this.cooling.is_stale_in(&dir) {
                        this.sync(cx);
                    }
                    // A folder not read yet holds its fold shut until its rows arrive.
                    let held = !this.loaded.contains(&dir);
                    this.fold_to(id.clone(), true, held, false, cx);
                    this.load(dir, cx);
                }
                TreeEvent::Collapsed(id) => this.fold_closed(id.clone(), cx),
            }
        });
        // The window's hover and selection shapes go with it.
        let window_id = window.window_handle().window_id();
        let number = crate::window_look::number(window);
        cx.on_release(move |this, _| {
            this.release_loaded();
            row::forget(window_id);
            crate::hover::forget(window_id);
            crate::window_look::forget_lights(window_id);
            if let Some(number) = number {
                crate::window_look::forget(number);
            }
        })
        .detach();
        // The drop wash on a folder row is drawn from the system accent on every render, so a new accent only needs a render.
        cx.observe_global::<icons::AccentTick>(|this, cx| {
            this.tree.update(cx, |_, cx| cx.notify());
            cx.notify();
        })
        .detach();
        let (targets, watching, git, ignores) = Self::watching(&root, cx);
        let folds: Rc<RefCell<row::Folds>> = Rc::default();
        let scroll = tree.read(cx).scroll_handle().clone();
        let shapes = cx.new(|_| row::ShapesLayer::new(tree.clone(), scroll, folds.clone()));
        let filter = filter::State::new(window, cx);
        let explorer = cx.entity();
        let carets = cx.new(|cx| carets::FieldCarets::new(&explorer, filter.input(), cx));
        let mut this = Self {
            cooling: Cooling::default(),
            loaded: HashSet::new(),
            loading: HashSet::new(),
            reveal_after: reveal.map(Path::to_path_buf),
            follow_pending: None,
            marked: Rc::default(),
            ignored: Rc::default(),
            root,
            items: Vec::new(),
            tree,
            _tree_events: tree_events,
            watching,
            targets,
            filter,
            folds,
            gone: Vec::new(),
            emptied: None,
            leaving: None,
            shapes,
            carets,
            scroller: Default::default(),
            glide: Rc::default(),
            pins: Rc::default(),
            window_id,
            key: None,
            root_held: false,
            _activation: None,
            editing: None,
            picked: picked::Picked::default(),
            made: actions::Steps::default(),
            drop_target: None,
            drop_timer: None,
            edge_pull: None,
            edge_at: None,
            typed: (String::new(), None),
            git,
            ignores,
            deep_pending: HashSet::new(),
            deep_left: 0,
            sort: settings::get(cx).explorer.sort,
        };
        // An arrow key, a plain click or a reveal moved the selection: the picked rows go.
        cx.observe(&this.tree, |this, tree, cx| {
            let selected = tree.read(cx).selected_item().map(|item| item.id.clone());
            if !this.picked.rows.is_empty() && !this.picked.holds(selected.as_ref()) {
                this.set_picked(picked::Picked::default(), cx);
            }
        })
        .detach();
        this.read_ignores(cx);
        this.load_root(cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOADING_DELAY).await;
            this.update(cx, |this, cx| {
                // Rows land as a placeholder or the folder's own, so none means the read is still going.
                if this.items.is_empty() {
                    this.items = vec![placeholder(&this.root, PLACEHOLDER_LOADING, "Loading…")];
                    this.sync(cx);
                }
            })
            .ok();
        })
        .detach();
        this
    }

    /// Joins the root's feed (one stream and one git read per root, however many windows show it),
    /// turns its batches of file changes under `root` into tree updates and takes its git
    /// statuses. The handle comes back too: the tree adds the symlink targets it follows to it.
    fn watching(
        root: &Path,
        cx: &mut Context<Self>,
    ) -> (Option<watch::Targets>, Option<Watching>, Rc<git::Statuses>, Rc<RefCell<watch::Ignores>>) {
        let Some((feed, targets)) = watch::Feed::join(root, cx) else {
            // A root the system can't watch still shows git's colours, read once.
            let root = root.to_path_buf();
            cx.spawn(async move |this, cx| {
                let read_root = root.clone();
                let found = cx.background_executor().spawn(async move { git::read(&read_root) }).await;
                this.update(cx, |this, cx| {
                    if this.root == root {
                        this.take_git(Rc::new(found), cx);
                    }
                })
                .ok();
            })
            .detach();
            return (None, None, Rc::default(), Rc::default());
        };
        let batches = cx.subscribe(&feed, |this, _, batch: &watch::Batch, cx| this.changed(batch.clone(), cx));
        let statuses = cx.observe(&feed, |this, feed, cx| {
            this.take_git(feed.read(cx).git(), cx);
        });
        let git = feed.read(cx).git();
        let ignores = feed.read(cx).ignores();
        (Some(targets), Some(Watching { feed, _batches: batches, _statuses: statuses }), git, ignores)
    }

    /// Takes git's new answer. A row in the tree whose status differs from what the tree last drew eases its icon
    /// into the new tint; the first read of a root (nothing was drawn from git yet) snaps.
    fn take_git(&mut self, new: Rc<git::Statuses>, cx: &mut Context<Self>) {
        let old = std::mem::replace(&mut self.git, new);
        if old.git_dir().is_some() {
            let tree = self.tree.read(cx);
            let changes: Vec<_> = (0..)
                .map_while(|ix| tree.entry(ix))
                .filter_map(|entry| {
                    let path = Path::new(&*entry.item().id);
                    let (was, now) = (old.get(path), self.git.get(path));
                    (was != now).then(|| (path.to_path_buf(), was, now))
                })
                .collect();
            if !changes.is_empty() {
                row::git_changed(self.window_id, changes);
            }
        }
        // The open files' change bars read HEAD again when git's answer differs (a commit, a checkout).
        if old.git_dir().is_some() && *old != *self.git {
            cx.emit(ExplorerEvent::Git);
        }
        cx.notify();
    }

    /// Expands the folders above `path`, selects it and scrolls it into view. A folder on the way
    /// that isn't loaded yet is read here, one level, and only the ones on that path. A path
    /// outside the root is ignored.
    fn reveal(&mut self, path: &Path, cx: &mut Context<Self>) {
        if path == self.root || !path.starts_with(&self.root) {
            return;
        }
        let dirs = self.load_to(path, cx);
        self.select(&dirs, path, cx);
    }

    /// Reads the folders between the root and `path` that aren't loaded yet,
    /// and returns them top down.
    fn load_to(&mut self, path: &Path, cx: &mut Context<Self>) -> Vec<PathBuf> {
        let dirs = folders_to(&self.root, path);
        let how = self.listing(cx);
        for dir in &dirs {
            if !self.loaded.contains(dir) || self.cooling.is_stale(dir) {
                let kids = read_children(dir, how);
                self.merge_children(dir, kids);
            }
        }
        dirs
    }

    /// Files in this set get a small dot after their name (unsaved edits).
    /// Replaces the previous set.
    pub fn set_marked(&mut self, paths: HashSet<PathBuf>, cx: &mut Context<Self>) {
        if *self.marked == paths {
            return;
        }
        let old = std::mem::replace(&mut self.marked, Rc::new(paths));
        row::marks_changed(self.window_id, &old, &self.marked);
        self.tree.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Reads the root's first level in the background, with the folders on the way to the row
    /// waiting in `reveal_after`, then puts them in the tree and reveals that row. Until they land
    /// the tree keeps what it shows (a "Loading" row, or none at launch).
    fn load_root(&mut self, cx: &mut Context<Self>) {
        let root = self.root.clone();
        self.loading.insert(root.clone());
        let dirs = match &self.reveal_after {
            Some(path) if path != &root && path.starts_with(&root) => folders_to(&root, path),
            _ => Vec::new(),
        };
        let how = self.listing(cx);
        cx.spawn(async move |this, cx| {
            let read_root = root.clone();
            let reads = cx
                .background_executor()
                .spawn(async move {
                    std::iter::once(read_root)
                        .chain(dirs)
                        .map(|dir| {
                            let kids = read_children(&dir, how);
                            (dir, kids)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                // A read for a root the tree has left, or one a newer read of the same root beat, is dropped.
                if this.root != root || !this.loading.remove(&root) {
                    return;
                }
                for (dir, kids) in reads {
                    this.merge_children(&dir, kids);
                }
                this.sync(cx);
                if let Some(path) = this.reveal_after.take() {
                    this.reveal(&path, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Reads `dir` in the background and puts its children in the tree.
    fn load(&mut self, dir: PathBuf, cx: &mut Context<Self>) {
        if self.loaded.contains(&dir) || !self.loading.insert(dir.clone()) {
            return;
        }
        // A fast read never shows "Loading": its fold waits, hidden, for the rows, and starts the
        // moment they are in; only a slow one starts on the placeholder after `LOADING_DELAY`.
        let wait = dir.clone();
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(LOADING_DELAY).await;
            this.update(cx, |this, cx| this.unhold(&wait, cx)).ok();
        })
        .detach();
        let how = self.listing(cx);
        cx.spawn(async move |this, cx| {
            let kids = read_in_background(dir.clone(), how, cx).await;
            this.update(cx, |this, cx| {
                this.loading.remove(&dir);
                // A read that finished after the root changed belongs to the old tree.
                if dir.starts_with(&this.root) {
                    this.set_children(&dir, kids, cx);
                    this.unhold(&dir, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Merges `kids` into the tree's items and syncs once. When Option-Right waits on this
    /// folder, the folders in it open now.
    fn set_children(&mut self, dir: &Path, kids: Vec<Child>, cx: &mut Context<Self>) {
        self.merge_children(dir, kids);
        if self.deep_pending.remove(dir) {
            self.open_below(dir, cx);
        }
        self.sync(cx);
    }

    /// Puts `kids` among the items of `dir`, without handing the tree anything: the caller
    /// merges every folder it has, then syncs once. Returns the paths that are new in a folder
    /// the tree already had read (none for a first read), for the caller to say arrived.
    fn merge_children(&mut self, dir: &Path, kids: Vec<Child>) -> Vec<PathBuf> {
        if let Some(targets) = &mut self.targets {
            targets.learn(dir, kids.iter().filter_map(|kid| Some((dir.join(&kid.name), kid.target.clone()?))).collect());
        }
        // The rows that draw dimmed for being ignored: what a `.gitignore` leaves out, and all of an ignored folder's.
        let under = self.ignored.contains(dir);
        let dimmed: Vec<PathBuf> = kids.iter().filter(|kid| under || kid.ignored).map(|kid| dir.join(&kid.name)).collect();
        let fresh = child_items(dir, kids);
        let was_read = self.loaded.contains(dir);
        let ignored = self.ignored.clone();
        let Some(slot) = self.children_mut(dir) else {
            return Vec::new();
        };
        let arrived = if was_read {
            let held: HashSet<&SharedString> = slot.iter().map(|item| &item.id).collect();
            fresh.iter().filter(|item| !held.contains(&item.id)).map(|item| PathBuf::from(&*item.id)).collect()
        } else {
            Vec::new()
        };
        // A folder that is still there keeps its own items, so its
        // loaded children and expanded state survive a re-read.
        let mut old: HashMap<SharedString, TreeItem> =
            std::mem::take(slot).into_iter().map(|item| (item.id.clone(), item)).collect();
        *slot = fresh
            .into_iter()
            .map(|item| match old.get(&item.id) {
                Some(o) if o.is_folder() && item.is_folder() => {
                    old.remove(&item.id).unwrap_or(item)
                }
                _ => item,
            })
            .collect();
        // What a re-read of a folder the tree had took out (not a first read's "Loading" row, nor a folder left with
        // only its "Empty" row, which the placeholder fills in place) leaves the tree as a ghost.
        let kept: HashSet<&SharedString> = slot.iter().map(|item| &item.id).collect();
        let emptied = matches!(slot.as_slice(), [only] if only.id.ends_with(PLACEHOLDER_EMPTY));
        let removed: Vec<Gone> = if was_read && !emptied {
            old.values()
                .filter(|item| !kept.contains(&item.id) && !item.id.ends_with(PLACEHOLDER_LOADING) && !item.id.ends_with(PLACEHOLDER_EMPTY))
                .map(|item| Gone {
                    id: item.id.clone(),
                    label: item.label.clone(),
                    is_folder: item.is_folder(),
                    open: item.is_folder() && item.is_expanded(),
                    dim: item.label.starts_with('.') || ignored.contains(Path::new(&*item.id)),
                })
                .collect()
        } else {
            Vec::new()
        };
        // Every row this folder had or has now is judged again by this read.
        let judged: Vec<SharedString> =
            if dimmed.is_empty() && ignored.is_empty() { Vec::new() } else { slot.iter().map(|item| item.id.clone()).chain(old.keys().cloned()).collect() };
        drop(ignored);
        if !judged.is_empty() {
            let set = Rc::make_mut(&mut self.ignored);
            for id in &judged {
                set.remove(Path::new(&**id));
            }
            set.extend(dimmed);
        }
        self.gone.extend(removed);
        // Only a folder that has a slot counts as read: one deleted mid-read stays unread.
        self.cooling.read(dir);
        if self.loaded.insert(dir.to_path_buf()) {
            self.ignores.borrow_mut().hold(dir);
            if let Some(targets) = &mut self.targets {
                targets.follow(dir);
            }
        }
        // The folders left over are gone; so is what they had loaded.
        let gone: HashSet<&Path> =
            old.values().filter(|item| item.is_folder()).map(|item| Path::new(&*item.id)).collect();
        if !gone.is_empty() {
            self.forget_under(&gone);
        }
        arrived
    }

    /// The children of `dir`: the top level for the root, else its item's.
    fn children_mut(&mut self, dir: &Path) -> Option<&mut Vec<TreeItem>> {
        if dir == self.root {
            Some(&mut self.items)
        } else {
            find_mut(&mut self.items, dir).map(|item| item.kids_mut())
        }
    }

    /// A batch of changes from the watcher: tells the owner, and re-reads the
    /// loaded folders that have a changed direct child, and every loaded folder
    /// under a path the system asked to rescan or whose `.gitignore` or `.ignore`
    /// changed (what those hide may have changed all the way down).
    fn changed(&mut self, batch: watch::Batch, cx: &mut Context<Self>) {
        let watch::Batch { paths, rescans, git, seq } = batch;
        // Events from a followed symlink target come back as the link's paths; the stream is shared
        // with other windows, so the paths of a target only they follow are left out.
        let (paths, rescans) = match &self.targets {
            Some(targets) => (targets.map(&paths), targets.map(&rescans)),
            None => (paths, rescans),
        };
        // HEAD or the index moved: open files' change bars read HEAD again even when git's statuses come out equal.
        if git {
            cx.emit(ExplorerEvent::Git);
        }
        if paths.is_empty() && rescans.is_empty() {
            if git {
                self.refresh_git(seq, cx);
            }
            return;
        }
        // A changed `.gitignore` is read again before the next batch is judged by it.
        self.ignores.borrow_mut().changed(seq, &paths);
        self.read_ignores(cx);
        // The index or HEAD moved, or the working tree did in a repo: git's answer may differ.
        // A batch of only gitignored paths (a build under `target/`) can't change it.
        if git || (self.git.git_dir().is_some() && paths.iter().any(|path| !self.git_ignored(path, false))) {
            self.refresh_git(seq, cx);
        }
        let ignore_dirs: Vec<&Path> = paths
            .iter()
            .filter(|path| path.file_name().is_some_and(|name| name == ".gitignore" || name == ".ignore"))
            .filter_map(|path| path.parent())
            .collect();
        let mut dirs: BTreeSet<PathBuf> = paths
            .iter()
            .filter_map(|path| path.parent())
            .filter(|dir| self.loaded.contains(*dir))
            .map(Path::to_path_buf)
            .collect();
        for rescan in rescans.iter().map(PathBuf::as_path).chain(ignore_dirs.iter().copied()) {
            dirs.extend(self.loaded.iter().filter(|dir| dir.starts_with(rescan)).cloned());
        }
        if rescans.is_empty() && ignore_dirs.is_empty() {
            self.filter_files_changed(&paths, cx);
        } else {
            self.refilter(cx);
        }
        cx.emit(FilesChanged(paths));
        self.reread(dirs, true, cx);
    }

    /// Asks for the `.gitignore` of each loaded folder and of the folders above the root, once
    /// each (until it changes, and across every window on the root): the root's feed reads them in
    /// the background. `git_ignored` matches against them without touching disk.
    fn read_ignores(&mut self, cx: &mut Context<Self>) {
        let dirs = self
            .ignores
            .borrow_mut()
            .unasked(self.loaded.iter().map(PathBuf::as_path).chain(self.root.ancestors()));
        if dirs.is_empty() {
            return;
        }
        if let Some(watching) = &self.watching {
            watching.feed.update(cx, |feed, cx| feed.read_ignores(dirs, cx));
            return;
        }
        cx.spawn(async move |this, cx| {
            let read = cx.background_executor().spawn(async move { watch::read_ignores(dirs) }).await;
            this.update(cx, |this, _| this.ignores.borrow_mut().apply(read)).ok();
        })
        .detach();
    }

    /// Whether the `.gitignore` files read so far rule `path` out: the deepest file with an
    /// opinion decides. A path whose folder's file isn't read yet counts as not ignored.
    pub(crate) fn git_ignored(&self, path: &Path, is_dir: bool) -> bool {
        self.ignores.borrow().ignored(path, is_dir)
    }

    /// Asks the root's feed to read git's statuses again for batch `seq`. The feed reads once for
    /// every window showing the root, and settles for `GIT_SETTLE` after each read.
    fn refresh_git(&mut self, seq: u64, cx: &mut Context<Self>) {
        if let Some(watching) = &self.watching {
            watching.feed.update(cx, |feed, cx| feed.refresh_git(seq, cx));
        }
    }

    /// Re-reads the loaded folders `dirs` in the background and puts what it finds in the tree. A
    /// folder that is not open is not read: it is marked stale and read when it opens. With
    /// `wash`, the rows it finds new get the arrival wash (the watcher's changes, not a setting's).
    fn reread(&mut self, dirs: BTreeSet<PathBuf>, wash: bool, cx: &mut Context<Self>) {
        let dirs = self.cooling.defer(dirs, &open_dirs(&self.root, &self.items));
        if dirs.is_empty() {
            return;
        }
        let how = self.listing(cx);
        cx.spawn(async move |this, cx| {
            let reads = cx
                .background_executor()
                .spawn(async move {
                    dirs.into_iter()
                        .map(|dir| {
                            let kids = read_children(&dir, how);
                            (dir, kids)
                        })
                        .collect::<Vec<_>>()
                })
                .await;
            this.update(cx, |this, cx| {
                // Parents come first, so a folder removed above is skipped. One sync for all.
                let mut arrived = Vec::new();
                // The arrivals that add a row; a folder that held only its "Empty" row swaps it for the file.
                let mut grown = Vec::new();
                for (dir, kids) in reads {
                    if this.loaded.contains(&dir) && this.children_mut(&dir).is_some() {
                        let swaps = this
                            .children_mut(&dir)
                            .is_some_and(|slot| matches!(slot.as_slice(), [only] if only.id.ends_with(PLACEHOLDER_EMPTY)));
                        let found = this.merge_children(&dir, kids);
                        if !swaps {
                            grown.extend(found.iter().cloned());
                        }
                        arrived.extend(found);
                    }
                }
                if wash {
                    row::arrived(this.window_id, arrived);
                }
                this.sync(cx);
                if let Some(path) = this.follow_pending.take() {
                    this.follow_file(Some(&path), cx);
                }
                if wash {
                    this.open_gaps(grown, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Hands the tree state the current items, keeping the selection.
    fn sync(&mut self, cx: &mut Context<Self>) {
        // A filter shows its own rows; clearing it hands the tree these items.
        if self.filter.is_active() {
            self.gone.clear();
            return;
        }
        self.settle(cx);
        // The rows that left, while the tree still has them: they close their gaps instead of vanishing.
        let departing = self.departing(cx);
        let selected = self
            .tree
            .read(cx)
            .selected_item()
            .map(|item| item.id.clone());
        let items = self.items.clone();
        self.tree.update(cx, |tree, cx| {
            tree.set_items(items, cx);
            if let Some(id) = selected {
                let ix = tree.index_of(&id);
                tree.set_selected_index(ix, cx);
            }
        });
        self.place_runs(cx);
        if let Some(departing) = departing {
            self.open_ghosts(departing, cx);
        }
    }

    /// Before the tree gets its items: reads the stale folders that are open now (the rows are in
    /// before a fold starts, and no arrival wash runs for them), and lets go of the children of the
    /// folders that have been shut longer than `WARM_SHUT` others, or past `WARM_SHUT_ROWS` kept rows.
    fn settle(&mut self, cx: &App) {
        let open = open_dirs(&self.root, &self.items);
        let how = self.listing(cx);
        for dir in self.cooling.due(&open) {
            if self.loaded.contains(&dir) {
                let kids = read_children(&dir, how);
                self.merge_children(&dir, kids);
            }
            self.cooling.read(&dir);
        }
        let items = &self.items;
        let rows = |dir: &Path| find(items, dir).map_or(0, |item| item.kids().len());
        for dir in self.cooling.cool(&self.loaded, &self.root, &open, WARM_SHUT, &rows, WARM_SHUT_ROWS) {
            self.evict(&dir);
        }
    }

    /// Drops the children of the shut folder `dir` and of every folder loaded under it: it goes
    /// back to one "Loading" placeholder and counts as unread, so opening it reads it as for a
    /// first open.
    fn evict(&mut self, dir: &Path) {
        if !self.loaded.contains(dir) {
            return;
        }
        if let Some(item) = find(&self.items, dir) {
            item.clone().expanded(false);
        }
        if let Some(slot) = self.children_mut(dir) {
            *slot = vec![placeholder(dir, PLACEHOLDER_LOADING, "Loading…")];
        }
        self.forget_under(&HashSet::from([dir]));
    }

    /// Forgets what was read for the folders in `gone` and everything under them.
    fn forget_under(&mut self, gone: &HashSet<&Path>) {
        let (gone_dirs, kept): (HashSet<PathBuf>, HashSet<PathBuf>) =
            self.loaded.drain().partition(|path| path.ancestors().any(|above| gone.contains(above)));
        self.loaded = kept;
        if !self.ignored.is_empty() {
            Rc::make_mut(&mut self.ignored).retain(|path| !path.ancestors().skip(1).any(|above| gone.contains(above)));
        }
        // The root's other windows may show them still: the matcher of each goes with the last to let go.
        let mut ignores = self.ignores.borrow_mut();
        for dir in &gone_dirs {
            ignores.release(dir, &self.root);
        }
        drop(ignores);
        self.cooling.forget(|path| path.ancestors().any(|above| gone.contains(above)));
        if let Some(targets) = &mut self.targets {
            targets.retain(|link| self.loaded.contains(link));
        }
    }

    /// Lets go of every loaded folder's share of the root's `.gitignore` matchers and forgets them.
    fn release_loaded(&mut self) {
        let mut ignores = self.ignores.borrow_mut();
        for dir in self.loaded.drain() {
            ignores.release(&dir, &self.root);
        }
    }

    /// Expands `dirs` (already loaded), then selects `path` and centers it at once.
    fn select(&mut self, dirs: &[PathBuf], path: &Path, cx: &mut Context<Self>) {
        self.select_with(dirs, path, false, cx);
    }

    /// Expands `dirs` (already loaded), then selects `path` and brings it into view: by a glide of
    /// the scroll offset (or a flash of the selection when the row is in view already) with
    /// `glide`, else centered at once.
    fn select_with(&mut self, dirs: &[PathBuf], path: &Path, glide: bool, cx: &mut Context<Self>) {
        if glide {
            self.expand_folding(dirs, cx);
        } else {
            self.expand(dirs, cx);
        }
        let id = SharedString::from(path.to_string_lossy().into_owned());
        let ix = self.tree.update(cx, |tree, cx| {
            let ix = tree.index_of(&id)?;
            tree.set_selected_index(Some(ix), cx);
            Some(ix)
        });
        let Some(ix) = ix else { return };
        if !glide || !self.glide_to(ix, true, cx) {
            self.glide.borrow_mut().cancel();
            self.tree.update(cx, |tree, _| tree.scroll_to_item(ix, ScrollStrategy::Center));
        }
    }

    /// `select` for a row that is new here (a new, renamed, copied or put-back item): a row in view
    /// stays put, one out of view glides in as a reveal does, with no flash (the arrival has its
    /// own wash, a new row its name field). Centered at once only before the list is measured.
    fn select_settling(&mut self, dirs: &[PathBuf], path: &Path, cx: &mut Context<Self>) {
        self.expand(dirs, cx);
        let id = SharedString::from(path.to_string_lossy().into_owned());
        let ix = self.tree.update(cx, |tree, cx| {
            let ix = tree.index_of(&id)?;
            tree.set_selected_index(Some(ix), cx);
            Some(ix)
        });
        let Some(ix) = ix else { return };
        if !self.glide_to(ix, false, cx) {
            self.glide.borrow_mut().cancel();
            self.tree.update(cx, |tree, _| tree.scroll_to_item(ix, ScrollStrategy::Center));
        }
    }

    /// Brings row `ix` into view the way a jump should: a row fully in view stays put and the
    /// selection flashes; one out of view glides in to land `REVEAL_INSET` from the edge it comes in
    /// from. False when the list is not measured yet (before the first frame), which the caller
    /// answers by centering the row at once.
    fn glide_to(&mut self, ix: usize, flash: bool, cx: &mut Context<Self>) -> bool {
        let now = motion::drawn_now(self.window_id);
        let (view, offset) = {
            let scroll = self.tree.read(cx).scroll_handle().0.borrow();
            (f32::from(scroll.base_handle.bounds().size.height), scroll.base_handle.offset())
        };
        let view = view - self.folds.borrow().overdraw(now);
        if view <= 0. {
            return false;
        }
        let row = f32::from(ROW_HEIGHT);
        let (top, bottom, at) = (ix as f32 * row, (ix + 1) as f32 * row, -f32::from(offset.y));
        if top >= at && bottom <= at + view {
            self.glide.borrow_mut().cancel();
            if flash {
                row::flash(self.window_id, now);
                self.shapes.update(cx, |_, cx| cx.notify());
            }
            return true;
        }
        let tree = self.tree.read(cx);
        let rows = (0..).take_while(|ix| tree.entry(*ix).is_some()).count();
        let inset = f32::from(REVEAL_INSET);
        let to = if top < at { top - inset } else { bottom + inset - view };
        self.glide.borrow_mut().aim(at, to.clamp(0., (rows as f32 * row - view).max(0.)), now);
        cx.notify();
        true
    }

    /// Expands `dirs` (already loaded) and hands the tree the result. A fold under way in one of
    /// them is dropped: a reveal shows the end state.
    fn expand(&mut self, dirs: &[PathBuf], cx: &mut Context<Self>) {
        for dir in dirs {
            if let Some(item) = find(&self.items, dir) {
                item.clone().expanded(true);
            }
        }
        self.folds.borrow_mut().runs.retain(|run| !dirs.iter().any(|dir| Path::new(&*run.id) == dir));
        self.sync(cx);
    }

    /// `expand` for a reveal: the outermost shut folder of `dirs` folds open (the folders inside it
    /// open with it, in the same fold), so the rows slide apart instead of popping in. A folder of
    /// `dirs` that is folding shut turns around and opens from where it is drawn, as a second
    /// reveal mid-fold does.
    fn expand_folding(&mut self, dirs: &[PathBuf], cx: &mut Context<Self>) {
        let shut: Vec<SharedString> = dirs
            .iter()
            .filter_map(|dir| find(&self.items, dir))
            .filter(|item| !self.is_open(&item.id, item.is_expanded()))
            .map(|item| item.id.clone())
            .collect();
        for dir in dirs {
            if let Some(item) = find(&self.items, dir) {
                item.clone().expanded(true);
            }
        }
        self.sync(cx);
        for (n, id) in shut.into_iter().enumerate() {
            let folding = self.folds.borrow().runs.iter().any(|run| !run.lone && run.id == id);
            if n == 0 || folding {
                self.fold_to(id, true, false, false, cx);
            }
        }
    }

    /// Checks that `path` is below the root, for the control verbs.
    fn below_root(&self, path: &Path) -> Result<(), String> {
        if path == self.root || !path.starts_with(&self.root) {
            return Err(format!("not below the explorer root {}", self.root.display()));
        }
        Ok(())
    }

    /// Control verb `explorer select`: reveals `path` and selects its row.
    pub fn select_path(&mut self, path: &Path, cx: &mut Context<Self>) -> Result<(), String> {
        self.below_root(path)?;
        let dirs = self.load_to(path, cx);
        self.select(&dirs, path, cx);
        self.row_of(path, cx).map(|_| ())
    }

    /// Control verbs `explorer click` and `explorer double-click`: what a click on the file's row
    /// sends (a single click previews, a double-click keeps).
    pub fn click_path(&mut self, path: &Path, double: bool, cx: &mut Context<Self>) -> Result<(), String> {
        self.select_path(path, cx)?;
        if path.is_dir() {
            return Err(format!("{} is a folder", path.display()));
        }
        let path = path.to_path_buf();
        cx.emit(if double { ExplorerEvent::Open(path) } else { ExplorerEvent::Preview(path) });
        Ok(())
    }

    /// Control verb `explorer hover`: reveals `path` and puts the hover shape
    /// on its row, as if the pointer sat there.
    pub fn hover_path(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        self.below_root(path)?;
        let dirs = self.load_to(path, cx);
        self.expand(&dirs, cx);
        let ix = self.row_of(path, cx)?;
        self.tree.update(cx, |tree, _| tree.scroll_to_item(ix, ScrollStrategy::Center));
        row::set_hovered(window, SharedString::from(path.to_string_lossy().into_owned()));
        cx.notify();
        Ok(())
    }

    /// Control verb `explorer toggle`: expands or collapses the folder at `path`. A folder not
    /// read yet opens on its "Loading" placeholder and its rows land after the background read.
    /// Its rows fold, as for a click.
    pub fn toggle_path(&mut self, path: &Path, cx: &mut Context<Self>) -> Result<(), String> {
        self.below_root(path)?;
        let dirs = self.load_to(path, cx);
        self.expand(&dirs, cx);
        let Some((id, expanded)) = find(&self.items, path)
            .filter(|item| item.is_folder())
            .map(|item| (item.id.clone(), item.is_expanded()))
        else {
            return Err(format!("not a folder in the tree: {}", path.display()));
        };
        let open = !self.is_open(&id, expanded);
        // As for a click: a folder not read yet shows its placeholder and holds its fold shut
        // until the background read lands.
        let held = open && !self.loaded.contains(path);
        if open {
            if let Some(item) = find(&self.items, path) {
                item.clone().expanded(true);
            }
            self.sync(cx);
        }
        self.fold_to(id, open, held, false, cx);
        if open {
            self.load(path.to_path_buf(), cx);
        }
        Ok(())
    }

    /// Whether the folder `id` is open as far as the user sees it: a fold heading for shut keeps
    /// its rows listed (and the item expanded) until it lands.
    fn is_open(&self, id: &SharedString, expanded: bool) -> bool {
        self.folds.borrow().runs.iter().find(|run| !run.lone && run.id == *id).map_or(expanded, |run| run.fold.is_open())
    }

    /// Starts or turns around the fold of the folder `id`, heading for `open`. A closing fold keeps
    /// the folder's item expanded, and so its rows listed, until it lands; `held` keeps an opening
    /// one shut until `unhold` (its rows are still being read); `deep` also shuts the folders
    /// inside when it lands.
    fn fold_to(&mut self, id: SharedString, open: bool, held: bool, deep: bool, cx: &mut Context<Self>) {
        let now = motion::drawn_now(self.window_id);
        {
            let mut folds = self.folds.borrow_mut();
            match folds.runs.iter_mut().find(|run| !run.lone && run.id == id) {
                Some(run) => {
                    if run.fold.is_open() != open {
                        run.fold.reverse(now);
                    }
                    run.deep = deep && !open;
                }
                None => folds.runs.push(row::Run {
                    id,
                    fold: if open { Fold::opening(now, held) } else { Fold::closing(now) },
                    ix: 0,
                    n: 0,
                    deep,
                    lone: false,
                }),
            }
        }
        self.place_runs(cx);
        self.finish_later(cx);
        cx.notify();
    }

    /// A click, Left or Return shut a folder the tree has already marked collapsed. Its rows stay
    /// listed while they fold away, so the item is put back expanded and handed to the tree again;
    /// the same press on a folder that is already folding shut turns it back open.
    fn fold_closed(&mut self, id: SharedString, cx: &mut Context<Self>) {
        // A shut folder takes no more Option-Right reads under it.
        self.deep_pending.retain(|dir| !dir.starts_with(Path::new(&*id)));
        let Some(item) = find(&self.items, Path::new(&*id)) else {
            return;
        };
        item.clone().expanded(true);
        let open = !self.is_open(&id, true);
        self.sync(cx);
        self.fold_to(id, open, false, false, cx);
    }

    /// Starts a fold that waited for its folder's rows, if it still waits.
    fn unhold(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let id = SharedString::from(dir.to_string_lossy().into_owned());
        let now = motion::drawn_now(self.window_id);
        let started = {
            let mut folds = self.folds.borrow_mut();
            match folds.runs.iter_mut().find(|run| !run.lone && run.id == id && run.fold.is_held()) {
                Some(run) => {
                    run.fold.start(now);
                    true
                }
                None => false,
            }
        };
        if started {
            self.finish_later(cx);
            cx.notify();
        }
    }

    /// Where each fold's rows sit in the tree's list now, which the rows need to place themselves.
    fn place_runs(&self, cx: &App) {
        let tree = self.tree.read(cx);
        for run in self.folds.borrow_mut().runs.iter_mut() {
            (run.ix, run.n) = match tree.index_of(&run.id) {
                // An arrived row opens out of the gap under the row above it; the very first row has none.
                Some(ix) if run.lone => match ix.checked_sub(1) {
                    Some(above) => (above, 1),
                    None => (0, 0),
                },
                Some(ix) => {
                    let depth = tree.entry(ix).map_or(0, |entry| entry.depth());
                    let mut n = 0;
                    while tree.entry(ix + 1 + n).is_some_and(|entry| entry.depth() > depth) {
                        n += 1;
                    }
                    (ix, n)
                }
                None => (0, 0),
            };
        }
        // A ghost's gap follows the row below it when a sync moves the rows.
        for ghost in self.folds.borrow_mut().ghosts.iter_mut() {
            if let Some(ix) = ghost.anchor.as_ref().and_then(|id| tree.index_of(id)) {
                ghost.gap = ix;
            }
        }
    }

    /// Rows that arrived from outside and are on screen open out of a one-row gap, so the rows below slide down
    /// instead of jumping: a `lone` run each, at most `ARRIVED_MAX` under way like the wash. Not under a filter (it
    /// shows its own rows), not for rows off screen or before the list is measured, and not for the first row of
    /// the list (no row above it to open under). Call after the tree has the rows.
    fn open_gaps(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if self.filter.is_active() || paths.is_empty() {
            return;
        }
        let now = motion::drawn_now(self.window_id);
        let (view, offset) = {
            let scroll = self.tree.read(cx).scroll_handle().0.borrow();
            (f32::from(scroll.base_handle.bounds().size.height), scroll.base_handle.offset())
        };
        if view <= 0. {
            return;
        }
        let row = f32::from(ROW_HEIGHT);
        let at = -f32::from(offset.y);
        let mut room = ARRIVED_MAX.saturating_sub(self.folds.borrow().runs.iter().filter(|run| run.lone).count());
        let mut started = false;
        for path in paths {
            if room == 0 {
                break;
            }
            let id = SharedString::from(path.to_string_lossy().into_owned());
            let Some(ix) = self.tree.read(cx).index_of(&id) else {
                continue;
            };
            let (top, bottom) = (ix as f32 * row, (ix + 1) as f32 * row);
            if ix == 0 || bottom <= at || top >= at + view {
                continue;
            }
            let mut folds = self.folds.borrow_mut();
            if folds.runs.iter().any(|run| run.lone && run.id == id) {
                continue;
            }
            folds.runs.push(row::Run { id, fold: Fold::opening(now, false), ix: 0, n: 0, deep: false, lone: true });
            room -= 1;
            started = true;
        }
        if started {
            self.place_runs(cx);
            self.finish_later(cx);
            cx.notify();
        }
    }

    /// Before the tree gets its items: the rows the merges since the last sync removed that are on screen, with the
    /// place each had. A removed folder that was open is left out (its fold shows it going), and so is a sync that
    /// removes more than the ghosts have room for (a wholesale change, such as showing hidden files), under a filter,
    /// and before the list is measured.
    fn departing(&mut self, cx: &App) -> Option<Departing> {
        let gone = std::mem::take(&mut self.gone);
        if gone.is_empty() || self.filter.is_active() {
            return None;
        }
        let (view, offset) = {
            let scroll = self.tree.read(cx).scroll_handle().0.borrow();
            (f32::from(scroll.base_handle.bounds().size.height), scroll.base_handle.offset())
        };
        if view <= 0. {
            return None;
        }
        let row = f32::from(ROW_HEIGHT);
        let at = -f32::from(offset.y);
        let tree = self.tree.read(cx);
        let mut rows = Vec::new();
        for item in gone {
            let Some(ix) = tree.index_of(&item.id) else { continue };
            if item.open || (ix + 1) as f32 * row <= at || ix as f32 * row >= at + view {
                continue;
            }
            let depth = tree.entry(ix).map_or(0, |entry| entry.depth());
            let status = self.git.get(Path::new(&*item.id));
            rows.push((ix, item, depth, status));
        }
        let room = ARRIVED_MAX.saturating_sub(self.folds.borrow().ghosts.len());
        if rows.is_empty() || rows.len() > room {
            return None;
        }
        rows.sort_by_key(|(ix, ..)| *ix);
        let ids = tree.entries().iter().map(|entry| entry.item().id.clone()).collect();
        Some(Departing { rows, ids })
    }

    /// After the tree has its items: each row `leaving` found closes its gap, from one row tall. The rows below
    /// are already in their new places; the ghost holds them where they were and lets them slide up. Its gap is
    /// at the first row below it the tree still has.
    fn open_ghosts(&mut self, departing: Departing, cx: &mut Context<Self>) {
        let now = motion::drawn_now(self.window_id);
        let tree = self.tree.read(cx);
        let len = tree.entries().len();
        let mut folds = self.folds.borrow_mut();
        for (ix, item, depth, status) in departing.rows {
            let anchor = departing.ids[ix + 1..].iter().find(|id| tree.index_of(id).is_some()).cloned();
            let gap = anchor.as_ref().and_then(|id| tree.index_of(id)).unwrap_or(len);
            folds.ghosts.push(row::Ghost { fold: Fold::closing(now), gap, anchor, label: item.label, is_folder: item.is_folder, dim: item.dim, depth, status });
        }
        drop(folds);
        self.finish_later(cx);
        cx.notify();
    }

    /// Once a fold has had time to land, ends it: a folder that folded shut is collapsed for real
    /// and its rows leave the list.
    fn finish_later(&self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_secs_f32(SETTLE) + LAND_MARGIN).await;
            this.update(cx, |this, cx| this.finish_folds(false, cx)).ok();
        })
        .detach();
    }

    /// Ends the folds that have landed, or all of them with `all` (a filter shows its own rows, so
    /// nothing folds under it).
    fn finish_folds(&mut self, all: bool, cx: &mut Context<Self>) {
        let now = Instant::now();
        self.folds.borrow_mut().ghosts.retain(|ghost| !all && !ghost.fold.is_done(now));
        let landed: Vec<(SharedString, bool, bool)> = {
            let mut folds = self.folds.borrow_mut();
            let mut landed = Vec::new();
            folds.runs.retain(|run| {
                let ends = all || run.fold.is_done(now);
                if ends {
                    landed.push((run.id.clone(), run.fold.is_open(), run.deep));
                }
                !ends
            });
            landed
        };
        let mut shut = false;
        for (id, open, deep) in landed {
            if open {
                continue;
            }
            if let Some(item) = find(&self.items, Path::new(&*id)) {
                if deep {
                    collapse_below(item);
                }
                item.clone().expanded(false);
                shut = true;
            }
        }
        if shut {
            self.sync(cx);
        }
        cx.notify();
    }

    /// Collapse all: every open folder folds shut, the folders inside them too once they land.
    /// Folders hidden under a shut one are shut at once, since nothing shows.
    pub fn collapse_all(&mut self, cx: &mut Context<Self>) {
        self.deep_pending.clear();
        if self.filter.is_active() {
            return;
        }
        let mut open = Vec::new();
        for item in &mut self.items {
            if item.is_expanded() {
                open.push(item.id.clone());
            } else {
                collapse_below(item);
            }
        }
        for id in open {
            self.fold_to(id, false, false, true, cx);
        }
    }

    /// The selected folder while its fold is heading for shut (its rows still listed, its item
    /// still expanded), unless a field has the typing.
    fn folding_shut(&self, cx: &App) -> Option<(SharedString, PathBuf)> {
        if self.filter.focused() || self.is_editing() {
            return None;
        }
        self.selected_folder(cx).filter(|(id, _)| !self.is_open(id, true))
    }

    /// The selected row when it is a folder: its id and path.
    fn selected_folder(&self, cx: &App) -> Option<(SharedString, PathBuf)> {
        let entry = self.tree.read(cx).selected_entry()?;
        if !entry.is_folder() || entry.is_disabled() {
            return None;
        }
        let id = entry.item().id.clone();
        let path = PathBuf::from(&*id);
        Some((id, path))
    }

    /// Option-Right: opens the selected folder and every folder under it. A folder not read yet
    /// is read in the background and the folders in it open as its rows land, a level per read,
    /// so the window never waits on a big tree; one press opens at most `DEEP_MAX` folders.
    fn open_all(&mut self, cx: &mut Context<Self>) {
        if self.filter.is_active() {
            return;
        }
        let Some((id, path)) = self.selected_folder(cx) else {
            return;
        };
        let Some(expanded) = find(&self.items, &path).map(|item| item.is_expanded()) else {
            return;
        };
        // Reads still pending from an earlier press are let go, so this press owns its budget.
        self.deep_pending.clear();
        self.deep_left = DEEP_MAX;
        if !self.is_open(&id, expanded) {
            // As for a click: a folder not read yet holds its fold shut until its rows arrive.
            let held = !self.loaded.contains(&path);
            if let Some(item) = find(&self.items, &path) {
                item.clone().expanded(true);
            }
            self.sync(cx);
            self.fold_to(id, true, held, false, cx);
            self.load(path.clone(), cx);
        }
        if self.loaded.contains(&path) {
            self.open_below(&path, cx);
            self.sync(cx);
        } else {
            self.deep_pending.insert(path);
        }
    }

    /// Opens the folders under the read folder `dir`, and under those already read, and asks for
    /// the rest to be read (they open from `set_children`). Stops at the press's budget. The
    /// caller syncs.
    fn open_below(&mut self, dir: &Path, cx: &mut Context<Self>) {
        let mut ask = Vec::new();
        if let Some(item) = find(&self.items, dir) {
            let ignored = |path: &Path| self.git_ignored(path, true);
            let mut left = self.deep_left;
            open_folders(item, &self.loaded, &ignored, &mut left, &mut ask);
            self.deep_left = left;
        }
        for path in ask {
            self.deep_pending.insert(path.clone());
            self.load(path, cx);
        }
    }

    /// Option-Left: shuts the selected folder and every folder under it, the folders inside once
    /// the fold lands. Folders that were waiting to open under it stay shut.
    fn close_all(&mut self, cx: &mut Context<Self>) {
        if self.filter.is_active() {
            return;
        }
        let Some((id, path)) = self.selected_folder(cx) else {
            return;
        };
        self.deep_pending.retain(|dir| !dir.starts_with(&path));
        let Some(item) = find(&self.items, &path) else {
            return;
        };
        if self.is_open(&id, item.is_expanded()) {
            self.fold_to(id, false, false, true, cx);
        } else {
            collapse_below(item);
        }
    }

    /// Option-click on a folder's row: selects it, then opens it and every folder under it when
    /// it is shut (as Option-Right), or shuts it and every folder under it when it is open (as
    /// Option-Left). `click alt row:<path>` sends it.
    pub fn toggle_subtree(&mut self, path: &Path, cx: &mut Context<Self>) -> Result<(), String> {
        let ix = self.row_of(path, cx)?;
        self.unpick(cx);
        self.tree.update(cx, |tree, cx| tree.set_selected_index(Some(ix), cx));
        let (id, expanded) = {
            let (id, path) = self.selected_folder(cx).ok_or_else(|| format!("{} is not a folder", path.display()))?;
            let item = find(&self.items, &path).ok_or_else(|| format!("no row for {}", path.display()))?;
            (id, item.is_expanded())
        };
        if self.is_open(&id, expanded) {
            self.close_all(cx);
        } else {
            self.open_all(cx);
        }
        Ok(())
    }

    /// Control verb `explorer key <name>`: the tree takes the keyboard and gets that key, as a
    /// press does (`up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`,
    /// `alt-left`, `alt-right`, `shift-up`, `shift-down`, `open`, `show-in-finder`).
    pub fn press(&mut self, key: &str, window: &mut Window, cx: &mut Context<Self>) -> Result<(), String> {
        use gpui_kit::base::actions::{SelectDown, SelectFirst, SelectLast, SelectLeft, SelectPageDown, SelectPageUp, SelectRight, SelectUp};
        let action: Box<dyn Action> = match key {
            "up" => Box::new(SelectUp),
            "down" => Box::new(SelectDown),
            "left" => Box::new(SelectLeft),
            "right" => Box::new(SelectRight),
            "home" => Box::new(SelectFirst),
            "end" => Box::new(SelectLast),
            "pageup" => Box::new(SelectPageUp),
            "pagedown" => Box::new(SelectPageDown),
            "alt-right" => {
                self.open_all(cx);
                return Ok(());
            }
            "alt-left" => {
                self.close_all(cx);
                return Ok(());
            }
            "focus" => {
                // The key's own action, sent from the window's focus as the key sends it.
                // Deferred: the action updates this explorer, which the verb is updating now.
                window.defer(cx, |window, cx| window.dispatch_action(Box::new(FocusExplorer), cx));
                return Ok(());
            }
            "new-file" | "new-folder" | "copy-path" | "copy-relative-path" | "shift-up" | "shift-down" | "open" | "show-in-finder" => {
                self.tree.update(cx, |tree, cx| tree.focus(window, cx));
                let keystroke = match key {
                    "new-file" => "cmd-alt-n",
                    "new-folder" => "cmd-alt-shift-n",
                    "copy-path" => "cmd-ctrl-c",
                    "shift-up" => "shift-up",
                    "shift-down" => "shift-down",
                    "open" => "cmd-down",
                    "show-in-finder" => "cmd-alt-r",
                    _ => "cmd-ctrl-alt-c",
                };
                // Deferred: the key's handler updates this explorer, which the verb is updating now.
                window.defer(cx, move |window, cx| {
                    window.dispatch_keystroke(gpui::Keystroke::parse(keystroke).expect("a valid key"), cx);
                });
                return Ok(());
            }
            "escape" => {
                self.tree.update(cx, |tree, cx| tree.focus(window, cx));
                window.defer(cx, |window, cx| {
                    window.dispatch_keystroke(gpui::Keystroke::parse("escape").expect("a valid key"), cx);
                });
                return Ok(());
            }
            "type" => return Err("explorer key type needs the letters to type".into()),
            typed if typed.starts_with("type ") => {
                self.tree.update(cx, |tree, cx| tree.focus(window, cx));
                for letter in typed["type ".len()..].chars() {
                    self.type_select(letter, Instant::now(), cx);
                }
                return Ok(());
            }
            _ => return Err("explorer key needs up, down, left, right, home, end, pageup, pagedown, alt-left, alt-right, shift-up, shift-down, open, show-in-finder, focus, escape, new-file, new-folder, copy-path, copy-relative-path or type <letters>".into()),
        };
        self.tree.update(cx, |tree, cx| tree.focus(window, cx));
        window.dispatch_action(action, cx);
        Ok(())
    }

    /// Type-to-select: `letter` joins the prefix typed in the last `TYPE_RESET` (a pause starts
    /// over) and the selection moves to the row whose name starts with it, then glides into view.
    fn type_select(&mut self, letter: char, now: Instant, cx: &mut Context<Self>) {
        let (typed, at) = &mut self.typed;
        if at.is_none_or(|at| now.saturating_duration_since(at) > TYPE_RESET) {
            typed.clear();
        }
        typed.extend(letter.to_lowercase());
        *at = Some(now);
        let found = {
            let tree = self.tree.read(cx);
            let names: Vec<(&str, bool)> = tree.entries().iter().map(|entry| (&*entry.item().label, entry.is_disabled())).collect();
            prefix_match(&names, tree.selected_index(), &self.typed.0)
        };
        let Some(ix) = found else { return };
        self.tree.update(cx, |tree, cx| tree.set_selected_index(Some(ix), cx));
        if !self.glide_to(ix, true, cx) {
            self.glide.borrow_mut().cancel();
            self.tree.update(cx, |tree, _| tree.scroll_to_item(ix, ScrollStrategy::Center));
        }
    }

    /// The row index of `path` in the tree as it is now.
    fn row_of(&self, path: &Path, cx: &App) -> Result<usize, String> {
        let id = SharedString::from(path.to_string_lossy().into_owned());
        self.tree.read(cx).index_of(&id).ok_or_else(|| format!("no row for {}", path.display()))
    }

    /// Command-Down opens the selected file or toggles the selected folder; with several rows
    /// picked it opens all of them.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = self.picked_paths(cx);
        if picked.len() > 1 {
            self.open_paths(picked, cx);
            return;
        }
        let Some(entry) = self.tree.read(cx).selected_entry() else {
            return;
        };
        if entry.is_disabled() {
            return;
        }
        if entry.is_folder() {
            window.dispatch_action(Box::new(Confirm { secondary: false }), cx);
        } else {
            let path = PathBuf::from(entry.item().id.to_string());
            cx.emit(ExplorerEvent::Open(path));
        }
    }

    /// Opens `paths`: one file opens, one folder expands or collapses; several open every file as
    /// a tab and expand every shut folder (a folder is never opened as a file, and a folder
    /// already open stays open).
    fn open_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        if let [path] = paths.as_slice() {
            if path.is_dir() {
                self.toggle_path(path, cx).ok();
            } else {
                cx.emit(ExplorerEvent::Open(path.clone()));
            }
            return;
        }
        let (files, folders) = open_targets(paths);
        for folder in folders {
            let shut = find(&self.items, &folder).is_some_and(|item| !self.is_open(&item.id, item.is_expanded()));
            if shut {
                self.toggle_path(&folder, cx).ok();
            }
        }
        for file in files {
            cx.emit(ExplorerEvent::Open(file));
        }
    }

    /// Shift-Up (`down` false) and Shift-Down: the picked run grows or shrinks from its anchor by
    /// one row, and the row the keys move from follows the far end, into view.
    fn extend_pick(&mut self, down: bool, cx: &mut Context<Self>) {
        let next = {
            let tree = self.tree.read(cx);
            let id_at = |ix: usize| tree.entry(ix).filter(|entry| !entry.is_disabled()).map(|entry| entry.item().id.clone());
            self.picked.extend(tree.selected_index(), down, tree.entries().len(), id_at, |id| tree.index_of(id))
        };
        let Some((picked, ix)) = next else { return };
        self.tree.update(cx, |tree, cx| tree.set_selected_index(Some(ix), cx));
        self.set_picked(picked, cx);
        if !self.glide_to(ix, false, cx) {
            self.glide.borrow_mut().cancel();
            self.tree.update(cx, |tree, _| tree.scroll_to_item(ix, ScrollStrategy::Nearest));
        }
    }

    /// Command-A with the tree focused: picks every row showing (a folder's rows only while it is
    /// open). False when there is no row to pick.
    fn pick_all(&mut self, cx: &mut Context<Self>) -> bool {
        let (picked, ix, moved) = {
            let tree = self.tree.read(cx);
            let ids: Vec<SharedString> = tree.entries().iter().filter(|entry| !entry.is_disabled()).map(|entry| entry.item().id.clone()).collect();
            let selected = tree.selected_entry().map(|entry| entry.item().id.clone());
            let (picked, primary) = self.picked.all(selected.as_ref(), ids);
            let Some(primary) = primary else { return false };
            let ix = tree.index_of(&primary);
            (picked, ix, selected.as_ref() != Some(&primary))
        };
        self.tree.update(cx, |tree, cx| tree.set_selected_index(ix, cx));
        self.set_picked(picked, cx);
        if let Some(ix) = ix.filter(|_| moved) {
            if !self.glide_to(ix, false, cx) {
                self.glide.borrow_mut().cancel();
                self.tree.update(cx, |tree, _| tree.scroll_to_item(ix, ScrollStrategy::Nearest));
            }
        }
        true
    }

    /// Makes `root` the tree's root: starts watching it, reads its first level in the background (the
    /// tree shows a "Loading" row until it lands) and tells the owner. The tree starts at the top with
    /// nothing selected.
    pub fn set_root(&mut self, root: PathBuf, cx: &mut Context<Self>) {
        if root == self.root {
            return;
        }
        // Folds in flight belong to the old root's rows, and so do the washes.
        self.finish_folds(true, cx);
        row::reset_root(self.window_id);
        self.items = vec![placeholder(&root, PLACEHOLDER_LOADING, "Loading…")];
        self.release_loaded();
        self.cooling = Cooling::default();
        self.made.clear();
        self.ignored = Rc::default();
        self.loading.clear();
        self.deep_pending.clear();
        self.reveal_after = None;
        // The new root's feed is joined before the old one is let go, so a root shown twice keeps its stream.
        (self.targets, self.watching, self.git, self.ignores) = Self::watching(&root, cx);
        self.root = root.clone();
        self.read_ignores(cx);
        self.load_root(cx);
        self.sync(cx);
        self.tree.update(cx, |tree, _| tree.scroll_to_item(0, ScrollStrategy::Top));
        self.refilter(cx);
        cx.emit(ExplorerEvent::Root(root));
        cx.notify();
    }

    /// Roots the tree at the folder above the root, with the old root's row selected.
    pub fn go_up(&mut self, cx: &mut Context<Self>) {
        let Some(parent) = self.root.parent().map(Path::to_path_buf) else {
            return;
        };
        let old = self.root.clone();
        self.set_root(parent, cx);
        self.reveal_after = Some(old);
    }

    /// Opens the folders down to `path`, selects it and scrolls it into view (once the root's first
    /// level lands, while it's still being read). A path outside the root does nothing; says
    /// whether the tree showed it.
    pub fn reveal_file(&mut self, path: &Path, cx: &mut Context<Self>) -> bool {
        let inside = path != self.root && path.starts_with(&self.root);
        if inside && self.loading.contains(&self.root) {
            self.reveal_after = Some(path.to_path_buf());
        } else if inside {
            let dirs = self.load_to(path, cx);
            self.select_with(&dirs, path, true, cx);
        }
        inside
    }

    /// The tree follows the file a tab switch brought forward, or the one Save As gave the showing
    /// document: like `reveal_file`, but a row the tree already selects (the click that opened the
    /// file was in the tree) is left alone. A row in view stays put; one out of view glides in. A
    /// row that isn't there yet (Save As: the watcher reads the new file a moment later) is kept as
    /// a pending follow that the next read of its folder takes. Any call replaces a pending
    /// follow, so a tab without a file (`None`) drops it.
    pub fn follow_file(&mut self, path: Option<&Path>, cx: &mut Context<Self>) {
        self.follow_pending = None;
        let Some(path) = path else { return };
        if path == self.root || !path.starts_with(&self.root) || self.selected_path(cx).as_deref() == Some(path) {
            return;
        }
        if self.loading.contains(&self.root) {
            self.reveal_after = Some(path.to_path_buf());
            return;
        }
        let dirs = self.load_to(path, cx);
        if find(&self.items, path).is_some() {
            self.select_with(&dirs, path, true, cx);
        } else {
            self.follow_pending = Some(path.to_path_buf());
        }
    }

    /// Shows or hides dot files, and re-reads the folders already loaded.
    pub fn toggle_hidden(&mut self, cx: &mut Context<Self>) {
        settings::update(cx, |settings| settings.explorer.show_hidden = !settings.explorer.show_hidden);
        let dirs = self.loaded.iter().cloned().collect();
        self.reread(dirs, false, cx);
        self.refilter(cx);
        cx.notify();
    }

    /// Shows or hides what `.gitignore` and `.ignore` files leave out, and re-reads the folders already loaded.
    /// The filter keeps skipping them, as Open quickly and Find in folder do.
    pub fn toggle_ignored(&mut self, cx: &mut Context<Self>) {
        settings::update(cx, |settings| settings.explorer.show_ignored = !settings.explorer.show_ignored);
        let dirs = self.loaded.iter().cloned().collect();
        self.reread(dirs, false, cx);
        cx.notify();
    }

    /// How this window's tree orders each folder's rows.
    pub fn sort(&self) -> settings::ExplorerSort {
        self.sort
    }

    /// Orders each folder's rows by `sort` in this window, re-reading the folders already loaded, and
    /// saves it as the order the next window starts with. The filter's list keeps name order.
    pub fn set_sort(&mut self, sort: settings::ExplorerSort, cx: &mut Context<Self>) {
        if self.sort == sort {
            return;
        }
        self.sort = sort;
        settings::update(cx, |settings| settings.explorer.sort = sort);
        let dirs = self.loaded.iter().cloned().collect();
        self.reread(dirs, false, cx);
        cx.notify();
    }

    /// What a folder read lists and in what order, for this window.
    fn listing(&self, cx: &App) -> Listing {
        Listing { hidden: show_hidden(cx), ignored: show_ignored(cx), sort: self.sort }
    }

    /// The path of the selected row, if it is a real entry.
    fn selected_path(&self, cx: &App) -> Option<PathBuf> {
        let entry = self.tree.read(cx).selected_entry().filter(|entry| !entry.is_disabled())?;
        Some(PathBuf::from(entry.item().id.to_string()))
    }

    /// Option-Cmd-N and Shift-Option-Cmd-N: a new file or folder in the selected folder (the folder of a selected file), or
    /// in the tree's folder when nothing is selected.
    fn new_here(&mut self, folder: bool, window: &mut Window, cx: &mut Context<Self>) {
        let path = self.selected_path(cx).unwrap_or_else(|| self.root.clone());
        self.run(if folder { Command::NewFolder(path) } else { Command::NewFile(path) }, window, cx);
    }

    /// Runs `command` on the selected row's path (the keyboard's Space and Cmd-Option-R).
    fn run_on_selected(&mut self, command: fn(PathBuf) -> Command, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(path) = self.selected_path(cx) {
            self.run(command(path), window, cx);
        }
    }

    /// Runs `command` on the picked rows, else the selected one (Cmd-D and Cmd-Delete).
    fn run_on_picked(&mut self, command: fn(Vec<PathBuf>) -> Command, window: &mut Window, cx: &mut Context<Self>) {
        let paths = self.picked_paths(cx);
        if !paths.is_empty() {
            self.run(command(paths), window, cx);
        }
    }

    /// The rows the file actions take, in row order: the picked rows that still show, else the
    /// selected row.
    pub fn picked_paths(&self, cx: &App) -> Vec<PathBuf> {
        if self.picked.rows.is_empty() {
            return self.selected_path(cx).into_iter().collect();
        }
        let entries = self.tree.read(cx).entries().iter();
        entries.filter(|entry| self.picked.rows.contains(&entry.item().id)).map(|entry| PathBuf::from(entry.item().id.to_string())).collect()
    }

    /// Command-click (`run` false) or Shift-click on the row of `path`: Command picks the row as
    /// well, or takes it out when it is picked; Shift picks the run of rows from the last one
    /// clicked. Nothing opens. Also the control verbs `explorer pick` and `explorer pick-run`.
    pub fn pick(&mut self, path: &Path, run: bool, cx: &mut Context<Self>) -> Result<(), String> {
        let ix = self.row_of(path, cx)?;
        let (picked, select) = {
            let tree = self.tree.read(cx);
            let id_at = |ix: usize| tree.entry(ix).filter(|entry| !entry.is_disabled()).map(|entry| entry.item().id.clone());
            self.picked.pick(tree.selected_index(), ix, run, id_at, |id| tree.index_of(id))
        };
        self.tree.update(cx, |tree, cx| tree.set_selected_index(select, cx));
        self.set_picked(picked, cx);
        Ok(())
    }

    /// A plain click: the picked rows go, the clicked row is selected alone.
    fn unpick(&mut self, cx: &mut Context<Self>) {
        if !self.picked.rows.is_empty() {
            self.set_picked(picked::Picked::default(), cx);
        }
    }

    /// Takes `picked` as the picked rows and redraws the shapes and names that show them.
    fn set_picked(&mut self, picked: picked::Picked, cx: &mut Context<Self>) {
        let rows = picked.rows.clone();
        self.picked = picked;
        self.shapes.update(cx, |shapes, cx| {
            shapes.picked = rows;
            cx.notify();
        });
        self.tree.update(cx, |_, cx| cx.notify());
        cx.notify();
    }

    /// Does what a menu item or key asked for.
    fn run(&mut self, command: Command, window: &mut Window, cx: &mut Context<Self>) {
        match command {
            Command::Open(paths) => self.open_paths(paths, cx),
            Command::OpenWith(file, app) => native_menu::open_with(&file, &app),
            Command::QuickLook(path) => {
                native_menu::quick_look(&path);
            }
            Command::ShowInFinder(paths) => native_menu::show_all_in_finder(&paths),
            Command::CopyPath(paths) => cx.write_to_clipboard(ClipboardItem::new_string(joined_paths(&paths, None))),
            Command::CopyRelativePath(paths) => cx.write_to_clipboard(ClipboardItem::new_string(joined_paths(&paths, Some(&self.root)))),
            Command::Root(path) => self.set_root(path, cx),
            Command::GoUp => self.go_up(cx),
            Command::CollapseAll => self.collapse_all(cx),
            Command::ToggleSubtree(path) => {
                self.toggle_subtree(&path, cx).ok();
            }
            Command::ToggleHidden => self.toggle_hidden(cx),
            Command::ToggleIgnored => self.toggle_ignored(cx),
            Command::Sort(sort) => {
                self.set_sort(sort, cx);
                cx.defer(crate::menu::refresh);
            }
            Command::NewFile(path) => {
                self.new_item(&path, false, None, window, cx).ok();
            }
            Command::NewFolder(path) => {
                self.new_item(&path, true, None, window, cx).ok();
            }
            Command::Rename(path) => {
                self.rename_path(&path, None, window, cx).ok();
            }
            Command::Duplicate(paths) => {
                self.duplicate_paths(paths, window, cx).ok();
            }
            Command::Trash(paths) => {
                self.trash_paths(paths, window, cx).ok();
            }
            Command::MoveTo(paths) => self.move_to_picked_folder(paths, window, cx),
            Command::OpenToSide(path) => cx.emit(ExplorerEvent::OpenToSide(path)),
            Command::CopyFile(paths) => self.copy_files(paths, cx),
            Command::Paste(path) => {
                self.paste_files(Some(path), window, cx);
            }
            Command::MoveHere(path) => {
                self.move_files_here(Some(path), window, cx);
            }
        }
    }

    /// The root, the selected path, the picked paths and the visible rows' paths with their git
    /// status, for the control socket.
    pub fn snapshot(&self, cx: &App) -> (String, Option<String>, Vec<String>, Vec<(String, Option<&'static str>)>) {
        let tree = self.tree.read(cx);
        let rows = (0..)
            .map_while(|ix| tree.entry(ix))
            .map(|entry| {
                let id = entry.item().id.to_string();
                let status = self.git.get(Path::new(&id)).map(git::Status::name);
                (id, status)
            })
            .collect();
        let picked = if self.picked.rows.is_empty() { Vec::new() } else { self.picked_paths(cx).iter().map(|path| path.to_string_lossy().into_owned()).collect() };
        (self.root.to_string_lossy().into_owned(), self.selected_path(cx).map(|path| path.to_string_lossy().into_owned()), picked, rows)
    }

    /// The folder the tree starts from.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The pull-down under the root's name: the root and each folder above it, up to `/`.
    pub fn root_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut entries: Vec<Entry<Command>> = self
            .root
            .ancestors()
            .map(|dir| {
                let item = Item::new(native_menu::display_name(dir), Command::Root(dir.to_path_buf()));
                Entry::Item(item.icon(dir.to_path_buf()).checked(dir == self.root))
            })
            .collect();
        entries.push(Entry::Separator);
        entries.push(self.sort_menu());
        // Under the name's wash, where AppKit's own pull-downs open.
        let at = (f32::from(HEADER_INSET - HEADER_GAP), f32::from(TOP_BAND + (HEADER_HEIGHT + ICON_BUTTON) / 2.));
        self.show_menu(entries, Some(at), true, window, cx);
    }

    /// The Sort by submenu, its order checked.
    fn sort_menu(&self) -> Entry<Command> {
        let entries = settings::ExplorerSort::ALL.map(|sort| Entry::Item(Item::new(sort.title(), Command::Sort(sort)).checked(sort == self.sort))).into();
        Entry::Submenu { title: "Sort by".into(), image: Some(native_menu::Image::Symbol("arrow.up.arrow.down")), entries }
    }

    /// The right-click menu of the row at `path`, or of the empty space around the rows. A
    /// row is selected first, snapped there without the glide. A picked row keeps the others
    /// picked, and its menu acts on all of them.
    pub fn context_menu(&mut self, path: Option<PathBuf>, window: &mut Window, cx: &mut Context<Self>) {
        let entries = match path {
            Some(path) => {
                let Ok(ix) = self.row_of(&path, cx) else {
                    return;
                };
                let (is_folder, open, id) = self.tree.read(cx).entry(ix).map_or((false, false, None), |entry| (entry.is_folder(), entry.is_expanded(), Some(entry.item().id.clone())));
                match id.filter(|id| self.picked.rows.contains(id)) {
                    Some(id) => self.picked = self.picked.select(id),
                    None => self.unpick(cx),
                }
                self.tree.update(cx, |tree, cx| tree.set_selected_index(Some(ix), cx));
                row::snap_selection(window);
                if self.picked.rows.is_empty() {
                    self.row_menu(path, is_folder, open, actions::clipboard_files(cx).is_some())
                } else {
                    Self::picked_menu(self.picked_paths(cx))
                }
            }
            None => self.space_menu(cx),
        };
        self.show_menu(entries, None, false, window, cx);
    }

    /// Opens `entries` once the frame with the new selection has drawn: nothing draws while a
    /// native menu is open. Two frames on, since a callback runs at the start of the frame after
    /// the one it was queued in.
    fn show_menu(&mut self, entries: Vec<Entry<Command>>, at: Option<(f32, f32)>, held: bool, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.weak_entity();
        window.on_next_frame(move |window, cx| {
            // The root's name takes its held look in this frame, which draws before the menu blocks the thread.
            if held {
                this.update(cx, |this, cx| {
                    this.root_held = true;
                    cx.notify();
                })
                .ok();
            }
            window.on_next_frame(move |window, cx| {
                window
                    .spawn(cx, async move |cx| {
                        let chosen = native_menu::pop_up(&entries, at);
                        this.update_in(cx, |this, window, cx| {
                            if held {
                                this.root_held = false;
                                cx.notify();
                            }
                            if let Some(command) = chosen {
                                this.run(command, window, cx);
                            }
                        })
                        .ok();
                    })
                    .detach();
            });
        });
        cx.notify();
    }

    /// The menu of a row: open it, make, rename, copy and trash it, find it, and for a folder,
    /// make it the root.
    fn row_menu(&self, path: PathBuf, is_folder: bool, open: bool, can_paste: bool) -> Vec<Entry<Command>> {
        let mut entries = Vec::new();
        if is_folder {
            let (title, symbol) = if open { ("Collapse", "chevron.up") } else { ("Expand", "chevron.down") };
            entries.push(Entry::Item(Item::new(title, Command::Open(vec![path.clone()])).symbol(symbol).key(OPEN_KEY, COMMAND)));
            let all = if open { "Collapse all" } else { "Expand all" };
            entries.push(Entry::Item(Item::new(all, Command::ToggleSubtree(path.clone())).symbol(symbol).alternate()));
        } else {
            entries.push(Entry::Item(Item::new("Open", Command::Open(vec![path.clone()])).symbol("arrow.up.forward.square").key(OPEN_KEY, COMMAND)));
            entries.push(Entry::Item(Item::new("Open to the side", Command::OpenToSide(path.clone())).symbol("rectangle.split.2x1")));
            let apps = native_menu::apps_for(&path);
            if !apps.is_empty() {
                let mut list = Vec::new();
                let many = apps.len() > 1;
                for app in apps {
                    let title = if app.is_default { format!("{} (default)", app.name) } else { app.name.clone() };
                    let is_default = app.is_default;
                    list.push(Entry::Item(Item::new(title, Command::OpenWith(path.clone(), app.path.clone())).icon(app.path)));
                    if is_default && many {
                        list.push(Entry::Separator);
                    }
                }
                entries.push(Entry::Submenu { title: "Open with".into(), image: Some(native_menu::Image::Symbol("arrow.up.forward.app")), entries: list });
            }
        }
        let name = cut_middle(&path.file_name().map_or_else(String::new, |name| name.to_string_lossy().into_owned()), MENU_NAME_MAX);
        entries.push(Entry::Item(Item::new(format!("Quick Look \u{201C}{name}\u{201D}"), Command::QuickLook(path.clone())).symbol("eye").key(" ", 0)));
        entries.push(Entry::Separator);
        entries.extend(Self::new_items(&path, true));
        entries.push(Entry::Separator);
        entries.push(Entry::Item(Item::new("Rename", Command::Rename(path.clone())).symbol("pencil").key("\r", 0)));
        entries.push(Entry::Item(Item::new("Duplicate", Command::Duplicate(vec![path.clone()])).symbol("plus.square.on.square").key("d", COMMAND)));
        entries.push(Entry::Item(Item::new("Copy", Command::CopyFile(vec![path.clone()])).symbol("doc.on.doc").key("c", COMMAND)));
        entries.push(Entry::Item(Item::new("Paste", Command::Paste(path.clone())).symbol("doc.on.clipboard").key("v", COMMAND).disabled(!can_paste)));
        entries.push(Entry::Item(Item::new("Move item here", Command::MoveHere(path.clone())).symbol("doc.on.clipboard").alternate().key("v", COMMAND | OPTION).disabled(!can_paste)));
        entries.push(Entry::Item(Item::new("Move to\u{2026}", Command::MoveTo(vec![path.clone()])).symbol("folder")));
        entries.push(Entry::Item(Item::new("Move to Trash", Command::Trash(vec![path.clone()])).symbol("trash").key("\u{8}", COMMAND)));
        entries.push(Entry::Separator);
        entries.extend(Self::find_items(vec![path.clone()], true));
        if is_folder {
            entries.push(Entry::Separator);
            entries.push(Entry::Item(Item::new(format!("Show \u{201C}{name}\u{201D} as root"), Command::Root(path)).symbol("arrow.down.forward.square")));
        }
        entries
    }

    /// The menu of several picked rows: the items that act on all of them, with the same keys.
    fn picked_menu(paths: Vec<PathBuf>) -> Vec<Entry<Command>> {
        let mut entries = vec![
            Entry::Item(Item::new("Open", Command::Open(paths.clone())).symbol("arrow.up.forward.square").key(OPEN_KEY, COMMAND)),
            Entry::Separator,
            Entry::Item(Item::new("Duplicate", Command::Duplicate(paths.clone())).symbol("plus.square.on.square").key("d", COMMAND)),
            Entry::Item(Item::new("Copy", Command::CopyFile(paths.clone())).symbol("doc.on.doc").key("c", COMMAND)),
            Entry::Item(Item::new("Move to\u{2026}", Command::MoveTo(paths.clone())).symbol("folder")),
            Entry::Item(Item::new("Move to Trash", Command::Trash(paths.clone())).symbol("trash").key("\u{8}", COMMAND)),
            Entry::Separator,
        ];
        entries.extend(Self::find_items(paths, true));
        entries
    }

    /// The menu of the space around the rows: the root's own items, then the tree's.
    fn space_menu(&self, cx: &App) -> Vec<Entry<Command>> {
        let mut entries = Self::new_items(&self.root, false);
        entries.push(Entry::Separator);
        entries.push(Entry::Item(Item::new("Paste", Command::Paste(self.root.clone())).symbol("doc.on.clipboard").disabled(actions::clipboard_files(cx).is_none())));
        entries.push(Entry::Item(Item::new("Move item here", Command::MoveHere(self.root.clone())).symbol("doc.on.clipboard").alternate().disabled(actions::clipboard_files(cx).is_none())));
        entries.push(Entry::Separator);
        entries.extend(Self::find_items(vec![self.root.clone()], false));
        entries.push(Entry::Separator);
        if self.root.parent().is_some() {
            entries.push(Entry::Item(Item::new("Enclosing folder", Command::GoUp).symbol("arrow.up")));
        }
        entries.push(Entry::Item(Item::new("Collapse all folders", Command::CollapseAll).symbol("arrow.down.forward.and.arrow.up.backward")));
        entries.push(Entry::Item(Item::new("Show hidden files", Command::ToggleHidden).symbol("eye.slash").checked(show_hidden(cx))));
        entries.push(Entry::Item(Item::new("Show ignored files", Command::ToggleIgnored).symbol("circle.dashed").checked(show_ignored(cx))));
        entries
    }

    /// Show in Finder and the two ways to copy a path, which Option swaps. The row's menu writes
    /// their keys (`keyed`); the empty space has no row for them to act on. The alternate takes
    /// the key of the copy above it with Option added, which is how AppKit pairs them.
    fn find_items(paths: Vec<PathBuf>, keyed: bool) -> Vec<Entry<Command>> {
        let finder = Item::new("Show in Finder", Command::ShowInFinder(paths.clone())).symbol("finder");
        let copy = Item::new("Copy path", Command::CopyPath(paths.clone())).symbol("document.on.document");
        let relative = Item::new("Copy relative path", Command::CopyRelativePath(paths)).symbol("document.on.document").alternate();
        let (finder, copy, relative) = if keyed {
            (finder.key("r", COMMAND | OPTION), copy.key("c", COMMAND | CONTROL), relative.key("c", COMMAND | CONTROL | OPTION))
        } else {
            (finder, copy, relative)
        };
        vec![Entry::Item(finder), Entry::Item(copy), Entry::Item(relative)]
    }

    /// New File and New Folder, made in the folder at `path` or the folder of the file at it. The row's
    /// menu writes their keys (`keyed`); the empty space's items are for the tree's folder, not a selection.
    fn new_items(path: &Path, keyed: bool) -> Vec<Entry<Command>> {
        let file = Item::new("New file", Command::NewFile(path.to_path_buf())).symbol("doc.badge.plus");
        let folder = Item::new("New folder", Command::NewFolder(path.to_path_buf())).symbol("folder.badge.plus");
        let (file, folder) = if keyed {
            (file.key("n", COMMAND | OPTION), folder.key("n", COMMAND | OPTION | SHIFT))
        } else {
            (file, folder)
        };
        vec![Entry::Item(file), Entry::Item(folder)]
    }
}

/// A button of the explorer's header: an SF Symbol in a square that washes under the pointer.
struct HeaderButton {
    id: &'static str,
    symbol: &'static str,
    label: &'static str,
    /// The shortcut as the tooltip writes it.
    key: &'static str,
    action: Box<dyn Action>,
    /// Can't run now: dimmed, no wash, no click.
    off: bool,
    /// A toggle that is on: drawn in the accent while the window is key (the Mac's toolbar way), in the inactive ink like the others out of it.
    lit: bool,
}

/// How much of the accent a header button wears: a toggle that is on and can run, by how key the window is (1 key, 0 not).
fn accent_level(lit: bool, off: bool, key: f32) -> f32 {
    if lit && !off { key } else { 0. }
}

impl HeaderButton {
    fn render(self, ink: Hsla, text: Hsla, muted: Hsla, accent: Hsla, key_level: f32) -> impl IntoElement {
        let Self { id, symbol, label, key, action, off, lit } = self;
        let tint = accent_level(lit, off, key_level);
        let rest = if off { muted.opacity(ALPHA_BUTTON_OFF) } else { crate::tab_bar::mix(muted, accent, tint) };
        let hover = if tint > 0. { rest } else { text };
        // A button that can't run has no wash and keeps its dimmed glyph under the pointer. `occlude`
        // keeps a press on it from also starting a window move.
        let wash = if off { ink.opacity(0.) } else { ink };
        icon_button(id, symbol, ICON_BUTTON, HEADER_BUTTON_SYMBOL, hover::Ink { rest, lit: if off { rest } else { hover }, wash })
            .occlude()
            .when(!off, |button| button.on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)))
            .tooltip(tooltip::tip(tooltip::with_key(label, key)))
            .tooltip_show_delay(tooltip::DELAY)
            .children(crate::control::rects_enabled().then(|| {
                let name = format!("explorer-header:{}", id.strip_prefix("explorer-").unwrap_or(id));
                canvas(move |bounds, window, _| crate::control::record(window, name.clone(), bounds), |_, (), _, _| {}).absolute().size_full()
            }))
    }
}

impl Explorer {
    /// The hover and selection shapes, a view the workspace paints just under the explorer column
    /// (not inside the explorer, whose redraws would rebuild every row for each frame of a glide).
    pub fn shapes_layer(&self) -> AnyView {
        self.shapes.clone().into()
    }

    /// The text fields' carets, a view the workspace paints over the explorer column, so a blink
    /// redraws it and not the explorer's rows.
    pub fn caret_layer(&self) -> AnyView {
        self.carets.clone().into()
    }
}

impl Render for Explorer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A filter shows its own rows, so a fold under way ends where it was headed.
        if self.filter.is_active() && !self.folds.borrow().is_empty() {
            self.finish_folds(true, cx);
        }
        let window_id = self.window_id;
        let now = motion::drawn_now(window_id);
        // A reveal glides the list's scroll offset a frame at a time; the shapes and rows read it.
        if let Some(at) = self.glide.borrow_mut().step(now) {
            let base = self.tree.read(cx).scroll_handle().0.borrow().base_handle.clone();
            base.set_offset(point(base.offset().x, px(-at)));
        }
        if self.glide.borrow().is_moving() {
            window.request_animation_frame();
        }
        self.edge_scroll(window, cx);
        let overdraw = self.folds.borrow().overdraw(now);
        // The rows are built from the folds' levels, so the explorer redraws a frame at a time while any runs.
        if self.folds.borrow().moving(now) {
            window.request_animation_frame();
        }
        // A name's ink glides with the selection, a frame at a time while it moves.
        let selected = self.tree.read(cx).selected_item().map(|item| item.id.clone());
        if row::set_selected(window_id, selected, now) {
            window.request_animation_frame();
        }
        if self._activation.is_none() {
            self._activation = Some(cx.observe_window_activation(window, |_, _, cx| cx.notify()));
        }
        let to = f32::from(crate::control::window_key(window));
        let fade = self.key.get_or_insert(motion::Fade::at(to));
        fade.aim(to, now);
        let key = fade.level(now);
        if fade.is_moving(now) {
            window.request_animation_frame();
        }
        let folds = self.folds.clone();
        let theme = cx.theme();
        let foreground = theme.sidebar_foreground;
        let (text, muted, dim) = (theme::ink(ALPHA_PRIMARY, cx), theme::ink(ALPHA_SECONDARY, cx), theme::ink(ALPHA_DIM, cx));
        // The header's rest ink: `ALPHA_SECONDARY` in key, `ALPHA_STATUS_DIM` out of it.
        let rest = theme::ink(ALPHA_STATUS_DIM + (ALPHA_SECONDARY - ALPHA_STATUS_DIM) * key, cx);
        let light = theme::is_light(cx);
        let danger = theme.danger;
        let ink = foreground;
        let accent = icons::accent();
        let name = self.root.file_name().map_or_else(
            || self.root.to_string_lossy(),
            |name| name.to_string_lossy(),
        );
        let editor = cx.weak_entity();
        let marked = self.marked.clone();
        let ignored = self.ignored.clone();
        let git = self.git.clone();
        let hits = self.filter.hits();
        let footer = self.filter_footer(cx);
        // A root with nothing in it shows the centered state in place of its one "Empty" row.
        let lone = match self.items.as_slice() {
            [only] if !self.filter.is_active() && only.id.ends_with(PLACEHOLDER_EMPTY) => Some(only),
            _ => None,
        };
        let empty = lone.is_some();
        // A root that can't be read or is gone says so; it has no New file button, which would only fail.
        let readable = !lone.is_some_and(|only| &*only.label == UNREADABLE || &*only.label == MISSING);
        // The sentence is built only while the empty view shows.
        let empty_line = lone.map(|only| match &*only.label {
            UNREADABLE => format!("Finch can’t read “{name}”."),
            MISSING => format!("“{name}” isn’t there anymore."),
            _ => format!("Drop files here to add them to {name}."),
        });
        // The first file replaces it with one row, which covers none of it: it stays as a ghost for `FADE_QUICK`.
        // A filter being typed is not an arrival, so it forgets the view without a ghost.
        if let Some(line) = &empty_line {
            self.emptied = Some(line.clone());
            self.leaving = None;
        } else if let Some(line) = self.emptied.take()
            && !self.filter.is_active()
        {
            self.leaving = Some((line, None));
        }
        let ghost = match &mut self.leaving {
            Some((line, since)) => {
                let since = *since.get_or_insert(now);
                let t = now.saturating_duration_since(since).as_secs_f32() / FADE_QUICK;
                if t >= 1. {
                    self.leaving = None;
                    None
                } else {
                    window.request_animation_frame();
                    let level = 1. - motion::ease_in(t);
                    let view = crate::empty_state::leaving("folder", "No files", line.clone(), empty_actions(cx), level, cx);
                    Some(div().absolute().size_full().child(view))
                }
            }
            None => None,
        };
        let none = if let Some(line) = empty_line {
            let actions = if readable { empty_actions(cx) } else { Vec::new() };
            Some(crate::empty_state::view("explorer-empty", "folder", "No files", line, actions, cx))
        } else {
            self.filter.none().then(|| self.filter_none(cx))
        };
        let shows_hidden = show_hidden(cx);
        let edit = self.editing.as_ref().map(|edit| row::Edit { path: edit.path.clone(), input: edit.input.clone(), error: edit.error.clone(), danger });
        let drop_target = if cx.has_active_drag() { self.drop_target.clone() } else { None };
        let buttons = [
            HeaderButton {
                id: "explorer-up",
                symbol: "arrow.up",
                label: "Enclosing folder",
                key: "\u{2318}\u{2191}",
                action: Box::new(EnclosingFolder),
                off: self.root.parent().is_none(),
                lit: false,
            },
            HeaderButton {
                id: "explorer-reveal",
                symbol: "scope",
                label: "Reveal current file",
                key: "\u{21E7}\u{2318}J",
                action: Box::new(RevealCurrentFile),
                off: empty,
                lit: false,
            },
            HeaderButton {
                id: "explorer-hidden",
                symbol: if shows_hidden { "eye" } else { "eye.slash" },
                label: if shows_hidden { "Hidden files: shown" } else { "Hidden files: hidden" },
                key: "\u{21E7}\u{2318}.",
                action: Box::new(ToggleHiddenFiles),
                off: false,
                lit: shows_hidden,
            },
            HeaderButton {
                id: "explorer-collapse",
                symbol: "arrow.down.forward.and.arrow.up.backward",
                label: "Collapse all folders",
                key: "\u{2325}\u{2318}\u{2190}",
                action: Box::new(CollapseAllFolders),
                off: empty,
                lit: false,
            },
        ];

        div()
            .id("explorer")
            .size_full()
            .flex()
            .flex_col()
            // No ground: the window's sidebar material shows through.
            .text_color(text)
            .text_size(TEXT_BODY)
            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                // Escape in a row's name field keeps the name as it is on disk.
                if this.is_editing() {
                    this.cancel_edit(window, cx);
                    cx.stop_propagation();
                }
            }))
            // A folder folding shut still reads as open to the tree: Right opens it again and Left
            // goes to the folder holding it, as on a shut folder, instead of the tree's own handling.
            .capture_action(cx.listener(|this, _: &gpui_kit::base::actions::SelectRight, _, cx| {
                if let Some((id, _)) = this.folding_shut(cx) {
                    this.fold_to(id, true, false, false, cx);
                    cx.stop_propagation();
                }
            }))
            .capture_action(cx.listener(|this, _: &gpui_kit::base::actions::SelectLeft, _, cx| {
                if this.folding_shut(cx).is_some() {
                    this.tree.update(cx, |tree, cx| tree.select_parent(cx));
                    cx.stop_propagation();
                }
            }))
            .on_action(cx.listener(|this, _: &Undo, window, cx| {
                // Cmd-Z with the tree focused takes back the last file action; the fields and
                // the editor have their own.
                if !this.undo(window, cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &Redo, window, cx| {
                // Shift-Cmd-Z with the tree focused does the last taken-back file action again.
                if !this.redo(window, cx) {
                    cx.propagate();
                }
            }))
            // Cmd-C and Cmd-V with the tree focused copy and paste files; the editor's and the
            // fields' own copy and paste never get here (they handle theirs first), and a tree
            // with nothing to copy or paste lets the action go on.
            .on_action(cx.listener(|this, _: &CopyAction, _, cx| {
                if !this.copy_selected(cx) {
                    cx.propagate();
                }
            }))
            .on_action(cx.listener(|this, _: &PasteAction, window, cx| {
                if !this.paste_files(None, window, cx) {
                    cx.propagate();
                }
            }))
            // Cmd-A with the tree focused picks every row showing; a name or the filter being typed
            // in keeps its own, and the editor never gets here.
            .on_action(cx.listener(|this, _: &SelectAllAction, _, cx| {
                if this.is_editing() || !this.pick_all(cx) {
                    cx.propagate();
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                let keys = &event.keystroke;
                // The keyboard's own scrolling wins over a reveal's glide.
                this.glide.borrow_mut().cancel();
                // The filter field's and the name field's typing is their own.
                let typing = this.filter.focused() || this.is_editing();
                let plain = !keys.modifiers.modified();
                let command = keys.modifiers.platform && !keys.modifiers.alt && !keys.modifiers.shift && !keys.modifiers.control;
                let held = |platform, control, alt, shift| {
                    let m = &keys.modifiers;
                    (m.platform, m.control, m.alt, m.shift) == (platform, control, alt, shift)
                };
                let option = keys.modifiers.alt && !keys.modifiers.platform && !keys.modifiers.shift && !keys.modifiers.control;
                match keys.key.as_str() {
                    "escape" if plain && !typing => cx.emit(ExplorerEvent::Leave),
                    "enter" if plain && !typing => this.run_on_selected(Command::Rename, window, cx),
                    "down" if command && !typing => this.confirm(window, cx),
                    "up" if held(false, false, false, true) && !typing => this.extend_pick(false, cx),
                    "down" if held(false, false, false, true) && !typing => this.extend_pick(true, cx),
                    "right" if option && !typing => this.open_all(cx),
                    "left" if option && !typing => this.close_all(cx),
                    "d" if command && !typing => this.run_on_picked(Command::Duplicate, window, cx),
                    "backspace" | "delete" if command && !typing => this.run_on_picked(Command::Trash, window, cx),
                    "space" if plain && !typing => this.run_on_selected(Command::QuickLook, window, cx),
                    "r" if keys.modifiers.platform && keys.modifiers.alt && !typing => this.run_on_picked(Command::ShowInFinder, window, cx),
                    "n" if held(true, false, true, false) && !typing => this.new_here(false, window, cx),
                    "n" if held(true, false, true, true) && !typing => this.new_here(true, window, cx),
                    "c" if held(true, true, false, false) && !typing => this.run_on_picked(Command::CopyPath, window, cx),
                    "c" if held(true, true, true, false) && !typing => this.run_on_picked(Command::CopyRelativePath, window, cx),
                    _ if !typing => {
                        if let Some(letter) = typed_letter(keys) {
                            this.type_select(letter, Instant::now(), cx);
                        }
                    }
                    _ => {}
                }
            }))
            .children(crate::control::fresh("row:"))
            .children(crate::control::track("explorer"))
            .child(
                div()
                    .pt(TOP_BAND)
                    .h(TOP_BAND + HEADER_HEIGHT)
                    .px(HEADER_INSET) // design: `.px` is the horizontal padding method, the value is a token
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(HEADER_GAP)
                    .text_size(TEXT_SUBHEADLINE)
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rest)
                    .child(
                        // The root's name is a pull-down of the folders above it. Its wash starts
                        // a pad left of the name so the name lines up with the rows.
                        div()
                            .id("explorer-header")
                            .ml(-HEADER_GAP)
                            .px(HEADER_GAP) // design: `.px` is the horizontal padding method, the value is a token
                            .h(ICON_BUTTON)
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap(HEADER_CHEVRON_GAP)
                            .rounded(RADIUS_BUTTON)
                            .occlude()
                            .fade_hover("explorer-header", None, ink.opacity(ALPHA_BUTTON_HOVER))
                            .press_fill(ink.opacity(ALPHA_BUTTON_PRESSED))
                            .fade_ink(rest, text)
                            // Its menu is open: the pressed look stays until the menu closes, then fades back.
                            .held(self.root_held)
                            .on_click(cx.listener(|this, _, window, cx| this.root_menu(window, cx)))
                            .child(div().min_w_0().truncate().child(name.into_owned()))
                            .child(symbols::icon_weighted("chevron.down", HEADER_CHEVRON, symbols::SEMIBOLD))
                            .tooltip({
                                // The path with `~` for the home folder, as the rows' and tabs' tips have it; built when the tip shows.
                                let root = self.root.clone();
                                move |window, cx| tooltip::tip(crate::tab_bar::tip_path(&root, crate::pane::home_folder()))(window, cx)
                            })
                            .tooltip_show_delay(tooltip::DELAY)
                            .children(crate::control::track("explorer-header")),
                    )
                    .child(div().flex_1())
                    .child(div().flex().flex_none().items_center().children(buttons.map(|button| button.render(ink, text, rest, accent, key)))),
            )
            .child({
                let ink = row::Ink {
                    text: foreground,
                    primary: text,
                    muted,
                    dim,
                    light,
                    accent,
                    git: theme::git_inks(cx),
                };
                let scroll = self.tree.read(cx).scroll_handle().clone();
                let name_field = self.carets.read(cx).name_field().zip(self.editing.as_ref().map(|edit| SharedString::from(edit.path.to_string_lossy().into_owned())));
                let list_shown = none.is_none();
                self.shapes.update(cx, |shapes, _| (shapes.ink, shapes.shown) = (ink, list_shown));
                let picked = match self.picked.rows.is_empty() {
                    true => row::PickedRows::default(),
                    false => row::PickedRows { ids: self.picked.rows.clone(), paths: self.picked_paths(cx).into() },
                };
                let pins = self.pins.clone();
                {
                    let mut pins = pins.borrow_mut();
                    pins.clear();
                    pins.extend(self.tree.read(cx).entries().iter().map(|entry| entry.depth() as u16));
                }
                let (fade_scroll, fade_pins, fade_memo) = (scroll.clone(), pins.clone(), sticky::Memo::default());
                let tree = div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .px(TREE_INSET) // design: `.px` is the horizontal padding method, the value is a token
                    .rounded(RADIUS_ROW)
                    // The whole-tree drop wash eases in and out like a folder row's.
                    .child({
                        let (root, washed) = (self.root.clone(), drop_target.as_ref() == Some(&self.root));
                        canvas(
                            move |_, window, _| row::wash_level(window, &root, washed),
                            move |bounds, level, window, _| {
                                if level > 0. {
                                    window.paint_quad(gpui::fill(bounds, accent.opacity(ALPHA_DROP * level)).corner_radii(Corners::all(RADIUS_ROW)));
                                }
                            },
                        )
                        .absolute()
                        .inset(TREE_INSET)
                    })
                    // Files over the tree: a row under the pointer claims them later in the same
                    // pass; over none, they go to the root.
                    .on_drag_move::<DraggedFiles>(cx.listener(|this, event: &DragMoveEvent<DraggedFiles>, _, cx| {
                        let target = event.bounds.contains(&event.event.position).then(|| this.root.clone());
                        this.set_drop_target(target, cx);
                        this.set_edge_pull(event.event.position, event.bounds, cx);
                    }))
                    .on_drag_move::<ExternalPaths>(cx.listener(|this, event: &DragMoveEvent<ExternalPaths>, _, cx| {
                        let target = event.bounds.contains(&event.event.position).then(|| this.root.clone());
                        this.set_drop_target(target, cx);
                        this.set_edge_pull(event.event.position, event.bounds, cx);
                    }))
                    .on_drop::<DraggedFiles>(cx.listener(|this, dragged: &DraggedFiles, window, cx| {
                        let (root, copy) = (this.root.clone(), window.modifiers().alt);
                        this.drop_onto(dragged.0.to_vec(), root, copy, window, cx);
                    }))
                    .on_drag_move::<DraggedText>(cx.listener(|this, event: &DragMoveEvent<DraggedText>, _, cx| {
                        let target = event.bounds.contains(&event.event.position).then(|| this.root.clone());
                        this.set_drop_target(target, cx);
                        this.set_edge_pull(event.event.position, event.bounds, cx);
                    }))
                    .on_drop::<DraggedText>(cx.listener(|this, dropped: &DraggedText, window, cx| {
                        let root = this.root.clone();
                        this.drop_text(root, dropped.clone(), window, cx);
                    }))
                    .on_drop::<ExternalPaths>(cx.listener(|this, dropped: &ExternalPaths, window, cx| {
                        let root = this.root.clone();
                        this.drop_onto(dropped.paths().to_vec(), root, true, window, cx);
                    }))
                    // A right-click on a row stops at the row; this one is on the space around.
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(|this, _, window, cx| this.context_menu(None, window, cx)),
                    );
                // The "No files" view the first file just replaced fades out under it; it takes no input.
                let tree = tree.children(ghost);
                match none {
                    Some(none) => tree.child(none),
                    None => tree
                        .child(crate::scrollbar::watch(&self.scroller))
                        .child(hand_sensor(self.glide.clone()))
                        .child(
                            // The list reaches past the bottom while a fold slides rows in from there.
                            div()
                                .absolute()
                                .top_0()
                                .left(TREE_INSET)
                                .right(TREE_INSET)
                                .bottom(px(-overdraw)) // design: runtime geometry from the fold
                                .child(
                                    Tree::new(&self.tree)
                                        .pinned_rows(sticky::covered_rows)
                                        .decoration(sticky::Sticky::new(self.tree.downgrade(), scroll.clone(), editor.clone(), marked.clone(), ignored.clone(), git.clone(), ink, picked.clone(), name_field))
                                        .item({
                                            move |ix, entry, state, _, _| {
                                            let view = {
                                                let folds = folds.borrow();
                                                if folds.is_empty() {
                                                    row::FoldView::default()
                                                } else {
                                                    folds.view(ix, &entry.item().id, motion::drawn_now(window_id))
                                                }
                                            };
                                            let now = motion::drawn_now(window_id);
                                            let level = row::ink_level(window_id, &entry.item().id, state.is_selected(), now);
                                            let shift = view.shift();
                                            let row = row::row(ix, entry, &marked, &ignored, ink, &editor, hits.as_ref(), edit.as_ref(), drop_target.as_ref(), view, level, &git, &picked);
                                            // The edge fade and the fade under the pinned names are the rows' own alpha, so the material shows through,
                                            // read where a fold draws the row. Always wrapped, so a row's element state never moves when its alpha reaches 1.
                                            let alpha = {
                                                let pins = fade_pins.borrow();
                                                sticky::EdgeFade::read(&fade_memo, &fade_scroll, overdraw, &|at| usize::from(pins[at]), pins.len()).row(ix, shift)
                                            };
                                            div().w_full().h(ROW_HEIGHT).opacity(alpha).child(row).into_any_element()
                                            }
                                        })
                                        .list_style(StyleRefinement::default().flex_grow_1().size_full())
                                        .relative()
                                        .size_full(),
                                ),
                        )
                        .child(crate::scrollbar::overlay(crate::scrollbar::Overlay {
                            handle: Rc::new(scroll.clone()),
                            scroller: self.scroller.clone(),
                            // No ground to fade into: the rows fade by their own alpha (`sticky::EdgeFade`), so the overlay paints no ramp.
                            ground: None,
                        })),
                }
            })
            .child(footer)
    }
}

/// The hand-scroll sensor: a wheel or trackpad scroll over the tree stops a reveal's glide at once.
/// A capture listener, so it runs before the list under it handles the wheel.
fn hand_sensor(glide: Rc<RefCell<ScrollGlide>>) -> impl IntoElement {
    canvas(
        |bounds, _, _| bounds,
        move |_, bounds, window, _| {
            let glide = glide.clone();
            window.on_mouse_event(move |event: &ScrollEvent, phase, _, _| {
                if phase == DispatchPhase::Capture && bounds.contains(&event.position) {
                    glide.borrow_mut().cancel();
                }
            });
        },
    )
    .absolute()
    .inset_0()
}

/// Reads one level of `dir` on the background executor.
async fn read_in_background(dir: PathBuf, how: Listing, cx: &mut AsyncApp) -> Vec<Child> {
    cx.background_executor()
        .spawn(async move { read_children(&dir, how) })
        .await
}

/// The buttons of the "No files" view, which the live view and its leaving ghost both draw.
fn empty_actions(cx: &Context<Explorer>) -> Vec<crate::empty_state::Action> {
    vec![crate::empty_state::Action {
        id: "explorer-empty-new-file",
        label: "New file",
        key: None,
        run: Rc::new(cx.listener(|this, _: &ClickEvent, window, cx| {
            let root = this.root.clone();
            this.new_item(&root, false, None, window, cx).ok();
        })),
    }]
}

/// The visible entries of `dir`, folders first, each group in `how.sort`'s order.
/// `.git` and `.DS_Store` are left out, dot files too unless `how.hidden`, and what
/// `.gitignore` files (also those of parent folders), `.ignore` files and git's excludes
/// rule out unless `how.ignored`, which marks them.
fn read_children(dir: &Path, how: Listing) -> Vec<Child> {
    // With ignored files shown, the names a filtered read keeps tell the rest apart as ignored.
    let kept: Option<HashSet<std::ffi::OsString>> =
        how.ignored.then(|| level(dir, true).filter(|entry| entry.depth() == 1).map(|entry| entry.file_name().to_os_string()).collect());
    let mut kids: Vec<(Child, u128)> = level(dir, !how.ignored)
        .filter(|entry| entry.depth() == 1 && (how.hidden || !entry.file_name().to_string_lossy().starts_with('.')))
        .map(|entry| {
            let link = entry.path_is_symlink() && entry.path().is_dir();
            let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir()) || link;
            // Only Date modified and Size read an entry's metadata. A folder has no size of its own.
            let rank = match how.sort {
                settings::ExplorerSort::Date => entry
                    .metadata()
                    .ok()
                    .and_then(|meta| meta.modified().ok())
                    .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |since| since.as_nanos()),
                settings::ExplorerSort::Size if !is_dir => entry.metadata().map_or(0, |meta| u128::from(meta.len())),
                _ => 0,
            };
            let kid = Child {
                name: entry.file_name().to_string_lossy().into_owned(),
                is_dir,
                // What a folder symlink points at, resolved here so the main thread never touches the disk.
                target: if link { entry.path().canonicalize().ok() } else { None },
                ignored: kept.as_ref().is_some_and(|kept| !kept.contains(entry.file_name())),
            };
            (kid, rank)
        })
        .collect();
    kids.sort_by(|(a, a_rank), (b, b_rank)| sort_cmp(how.sort, (a.is_dir, &a.name, *a_rank), (b.is_dir, &b.name, *b_rank)));
    kids.into_iter().map(|(kid, _)| kid).collect()
}

/// `dir` and its entries as the `ignore` crate walks one level, `.git` and `.DS_Store` left out. `filtered` also leaves
/// out what `.gitignore` files (also those of parent folders), `.ignore` files and git's excludes rule out.
fn level(dir: &Path, filtered: bool) -> impl Iterator<Item = DirEntry> {
    WalkBuilder::new(dir)
        .max_depth(Some(1))
        .standard_filters(filtered)
        .hidden(false)
        .require_git(false)
        .filter_entry(|entry| !matches!(entry.file_name().to_str(), Some(".git" | ".DS_Store")))
        .build()
        .flatten()
}

/// The order of two entries as (is a folder, name, rank): folders first, then the newest or
/// largest (`rank`, 0 unless Date modified or Size), then for Kind a file's extension, then the name.
fn sort_cmp(sort: settings::ExplorerSort, (a_dir, a, a_rank): (bool, &str, u128), (b_dir, b, b_rank): (bool, &str, u128)) -> Ordering {
    fn extension(name: &str) -> &str {
        Path::new(name).extension().and_then(|ext| ext.to_str()).unwrap_or("")
    }
    b_dir
        .cmp(&a_dir)
        .then_with(|| b_rank.cmp(&a_rank))
        .then_with(|| if sort == settings::ExplorerSort::Kind && !a_dir { finder_cmp(extension(a), extension(b)) } else { Ordering::Equal })
        .then_with(|| finder_cmp(a, b))
}

/// Finder's name order: case- and accent-insensitive (`école` sorts with `e`),
/// with runs of digits compared by value, so `file2` comes before `file10`.
/// Names equal on that key fall back to the raw bytes, so the order is stable.
pub(crate) fn finder_cmp(a: &str, b: &str) -> Ordering {
    let (mut left, mut right) = (base_letters(a).peekable(), base_letters(b).peekable());
    loop {
        match (left.peek().copied(), right.peek().copied()) {
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (x, y) = (digits(&mut left), digits(&mut right));
                let (x, y) = (x.trim_start_matches('0'), y.trim_start_matches('0'));
                match x.len().cmp(&y.len()).then_with(|| x.cmp(y)) {
                    Ordering::Equal => {}
                    other => return other,
                }
            }
            (Some(x), Some(y)) => {
                match x.cmp(&y) {
                    Ordering::Equal => {}
                    other => return other,
                }
                left.next();
                right.next();
            }
        }
    }
}

/// A name as lowercase base letters: accents folded away and combining marks
/// (how a decomposed `e` + acute is stored) dropped. No allocation.
fn base_letters(name: &str) -> impl Iterator<Item = char> + '_ {
    name.chars().filter(|c| !('\u{300}'..='\u{36f}').contains(c)).map(|c| {
        let c = c.to_lowercase().next().unwrap_or(c);
        match c {
            '\u{e0}'..='\u{e5}' | '\u{100}'..='\u{105}' => 'a',
            '\u{e7}' | '\u{106}'..='\u{10d}' => 'c',
            '\u{10e}'..='\u{111}' => 'd',
            '\u{e8}'..='\u{eb}' | '\u{112}'..='\u{11b}' => 'e',
            '\u{11c}'..='\u{123}' => 'g',
            '\u{124}'..='\u{127}' => 'h',
            '\u{ec}'..='\u{ef}' | '\u{128}'..='\u{131}' => 'i',
            '\u{134}' | '\u{135}' => 'j',
            '\u{136}' | '\u{137}' => 'k',
            '\u{139}'..='\u{142}' => 'l',
            '\u{f1}' | '\u{143}'..='\u{148}' => 'n',
            '\u{f2}'..='\u{f6}' | '\u{f8}' | '\u{14c}'..='\u{151}' => 'o',
            '\u{154}'..='\u{159}' => 'r',
            '\u{15a}'..='\u{161}' => 's',
            '\u{162}'..='\u{167}' => 't',
            '\u{f9}'..='\u{fc}' | '\u{168}'..='\u{173}' => 'u',
            '\u{174}' | '\u{175}' => 'w',
            '\u{fd}' | '\u{ff}' | '\u{176}'..='\u{178}' => 'y',
            '\u{179}'..='\u{17e}' => 'z',
            other => other,
        }
    })
}

fn digits(chars: &mut Peekable<impl Iterator<Item = char>>) -> String {
    let mut run = String::new();
    while let Some(digit) = chars.next_if(char::is_ascii_digit) {
        run.push(digit);
    }
    run
}

/// The folders between `root` and `path` (which is below it), top down.
fn folders_to(root: &Path, path: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = path.ancestors().skip(1).take_while(|dir| *dir != root).map(Path::to_path_buf).collect();
    dirs.reverse();
    dirs
}

/// Tree items for the entries of `dir`. An unread folder carries one loading
/// placeholder; a folder with nothing in it carries an "Empty" one, since an
/// item with no children would draw as a file. A folder that can't be read at
/// all says so instead of "Empty" (`read_children` can't tell the two apart).
fn child_items(dir: &Path, kids: Vec<Child>) -> Vec<TreeItem> {
    if kids.is_empty() {
        let label = match std::fs::read_dir(dir) {
            Ok(_) => "Empty",
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => MISSING,
            Err(_) => UNREADABLE,
        };
        return vec![placeholder(dir, PLACEHOLDER_EMPTY, label)];
    }
    kids.into_iter()
        .map(|kid| {
            let path = dir.join(&kid.name);
            let item = TreeItem::new(path.to_string_lossy().into_owned(), kid.name);
            if kid.is_dir {
                item.child(placeholder(&path, PLACEHOLDER_LOADING, "Loading…"))
            } else {
                item
            }
        })
        .collect()
}

fn placeholder(dir: &Path, marker: &str, label: &str) -> TreeItem {
    TreeItem::new(format!("{}/{marker}", dir.display()), label.to_string()).disabled(true)
}

/// The item for `path`, found by walking down the folders above it.
fn find<'a>(items: &'a [TreeItem], path: &Path) -> Option<&'a TreeItem> {
    let item = items.iter().find(|item| path.starts_with(&*item.id))?;
    if Path::new(&*item.id) == path { Some(item) } else { find(item.kids(), path) }
}

/// `paths` as the text Copy path puts on the pasteboard: one to a line, relative to `root` when
/// given (the root itself is a dot).
fn joined_paths(paths: &[PathBuf], root: Option<&Path>) -> String {
    let line = |path: &PathBuf| match root {
        None => path.to_string_lossy().into_owned(),
        Some(root) => {
            let relative = path.strip_prefix(root).ok().filter(|rest| !rest.as_os_str().is_empty());
            relative.map_or_else(|| ".".to_string(), |rest| rest.to_string_lossy().into_owned())
        }
    };
    paths.iter().map(line).collect::<Vec<_>>().join("\n")
}

/// The files and the folders among `paths`, each in the order given.
fn open_targets(paths: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<PathBuf>) {
    paths.into_iter().partition(|path| !path.is_dir())
}

/// Like `find`, to change the item (and the children above it, one level each).
fn find_mut<'a>(items: &'a mut [TreeItem], path: &Path) -> Option<&'a mut TreeItem> {
    let item = items.iter_mut().find(|item| path.starts_with(&*item.id))?;
    if Path::new(&*item.id) == path {
        Some(item)
    } else {
        find_mut(item.kids_mut(), path)
    }
}

/// Shuts every folder below `item`, leaving `item` itself as it is.
fn collapse_below(item: &TreeItem) {
    for child in item.kids() {
        collapse_below(child);
        child.clone().expanded(false);
    }
}

/// Opens each folder under `item` that is a folder, and recurses into those already read; the ones
/// not read yet go to `ask`. Takes one from `left` per folder and stops when it runs out.
fn open_folders(item: &TreeItem, loaded: &HashSet<PathBuf>, ignored: &dyn Fn(&Path) -> bool, left: &mut usize, ask: &mut Vec<PathBuf>) {
    for child in item.kids().iter().filter(|child| child.is_folder() && !ignored(Path::new(&*child.id))) {
        if *left == 0 {
            return;
        }
        *left -= 1;
        child.clone().expanded(true);
        let path = PathBuf::from(&*child.id);
        if loaded.contains(&path) {
            open_folders(child, loaded, ignored, left, ask);
        } else {
            ask.push(path);
        }
    }
}

/// The folders whose rows show: the root and every expanded folder reached through expanded
/// ones. A closing fold keeps its folder expanded until it lands, so it counts as open.
fn open_dirs(root: &Path, items: &[TreeItem]) -> HashSet<PathBuf> {
    fn walk(items: &[TreeItem], open: &mut HashSet<PathBuf>) {
        for item in items.iter().filter(|item| item.is_folder() && item.is_expanded()) {
            open.insert(PathBuf::from(&*item.id));
            walk(item.kids(), open);
        }
    }
    let mut open = HashSet::from([root.to_path_buf()]);
    walk(items, &mut open);
    open
}

/// What the loaded folders that are shut have cost: which changed while shut (their children are
/// stale, read again when they open) and which were shut most recently (their children stay
/// warm, so a quick reopen slides with its rows).
#[derive(Default)]
struct Cooling {
    stale: HashSet<PathBuf>,
    /// Shut folders, the one shut longest first.
    warm: VecDeque<PathBuf>,
}

impl Cooling {
    /// Marks the folders of `dirs` that are not open stale, and returns the open ones: the only
    /// ones worth a read.
    fn defer(&mut self, dirs: BTreeSet<PathBuf>, open: &HashSet<PathBuf>) -> BTreeSet<PathBuf> {
        let (now, later): (BTreeSet<_>, BTreeSet<_>) = dirs.into_iter().partition(|dir| open.contains(dir));
        self.stale.extend(later);
        now
    }

    /// The stale folders that are open now, parents first.
    fn due(&self, open: &HashSet<PathBuf>) -> BTreeSet<PathBuf> {
        self.stale.iter().filter(|dir| open.contains(*dir)).cloned().collect()
    }

    fn is_stale(&self, dir: &Path) -> bool {
        self.stale.contains(dir)
    }

    fn is_stale_in(&self, dir: &Path) -> bool {
        self.stale.iter().any(|stale| stale.starts_with(dir))
    }

    /// `dir` was read just now.
    fn read(&mut self, dir: &Path) {
        self.stale.remove(dir);
    }

    fn forget(&mut self, gone: impl Fn(&Path) -> bool) {
        self.stale.retain(|dir| !gone(dir));
        self.warm.retain(|dir| !gone(dir));
    }

    /// Notes which of the `loaded` folders are shut (not in `open`) and returns the ones that
    /// have been shut longer than the `keep` most recent, or whose children (`rows` counts a
    /// folder's) the more recent ones leave past `budget` rows, for the caller to let go of.
    fn cool(&mut self, loaded: &HashSet<PathBuf>, root: &Path, open: &HashSet<PathBuf>, keep: usize, rows: &dyn Fn(&Path) -> usize, budget: usize) -> Vec<PathBuf> {
        let shut: BTreeSet<&PathBuf> = loaded.iter().filter(|dir| dir.as_path() != root && !open.contains(*dir)).collect();
        self.warm.retain(|dir| shut.contains(dir));
        let known: HashSet<PathBuf> = self.warm.iter().cloned().collect();
        self.warm.extend(shut.into_iter().rev().filter(|dir| !known.contains(*dir)).cloned());
        let over = self.warm.len().saturating_sub(keep);
        let mut dropped: Vec<PathBuf> = self.warm.drain(..over).collect();
        let mut kept: usize = self.warm.iter().map(|dir| rows(dir)).sum();
        while kept > budget {
            let Some(dir) = self.warm.pop_front() else { break };
            kept -= rows(&dir);
            dropped.push(dir);
        }
        dropped
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::{BTreeSet, HashSet},
        path::{Path, PathBuf},
    };

    use gpui_kit::base::TreeItem;

    use super::{Cooling, accent_level, open_dirs};

    /// A toggle that is on tints in the accent while the window is key, eases to the others' inactive ink out of key, and a toggle that is off never tints.
    #[test]
    fn an_on_toggle_takes_the_accent_only_in_key() {
        assert_eq!(accent_level(true, false, 1.), 1.);
        assert_eq!(accent_level(true, false, 0.), 0.);
        assert_eq!(accent_level(true, false, 0.5), 0.5);
        assert_eq!(accent_level(false, false, 1.), 0.);
        assert_eq!(accent_level(true, true, 1.), 0.);
    }

    fn paths(names: &[&str]) -> HashSet<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn several_paths_copy_one_to_a_line() {
        let both = [PathBuf::from("/r/a.txt"), PathBuf::from("/r/src")];
        assert_eq!(super::joined_paths(&both, None), "/r/a.txt\n/r/src");
        assert_eq!(super::joined_paths(&both, Some(Path::new("/r"))), "a.txt\nsrc");
        // One path is as before: no trailing newline, and the root itself is a dot.
        assert_eq!(super::joined_paths(&both[..1], None), "/r/a.txt");
        assert_eq!(super::joined_paths(&[PathBuf::from("/r")], Some(Path::new("/r"))), ".");
    }

    #[test]
    fn several_paths_open_files_as_tabs_and_folders_as_folders() {
        let dir = std::env::temp_dir().join(format!("finch-open-targets-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("a.txt"), "").unwrap();
        std::fs::write(dir.join("b.txt"), "").unwrap();
        let (files, folders) = super::open_targets(vec![dir.join("a.txt"), dir.join("src"), dir.join("b.txt")]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(files, [dir.join("a.txt"), dir.join("b.txt")]);
        assert_eq!(folders, [dir.join("src")]);
    }

    #[test]
    fn the_picked_menu_acts_on_all_rows_with_the_row_menus_words() {
        use crate::native_menu::Entry;
        let entries = super::Explorer::picked_menu(vec![PathBuf::from("/r/a"), PathBuf::from("/r/b")]);
        let titles: Vec<&str> = entries.iter().filter_map(|entry| if let Entry::Item(item) = entry { Some(item.title.as_str()) } else { None }).collect();
        assert_eq!(titles, ["Open", "Duplicate", "Copy", "Move to\u{2026}", "Move to Trash", "Show in Finder", "Copy path", "Copy relative path"]);
        let acts_on_both = |entry: &Entry<super::Command>| match entry {
            Entry::Item(item) => match &item.run {
                super::Command::Open(paths) | super::Command::ShowInFinder(paths) | super::Command::CopyPath(paths) | super::Command::CopyRelativePath(paths) => paths.len() == 2,
                _ => true,
            },
            _ => true,
        };
        assert!(entries.iter().all(acts_on_both));
    }

    #[test]
    fn a_change_in_a_shut_folder_waits_for_it_to_open() {
        let mut cooling = Cooling::default();
        let open = paths(&["/r", "/r/a"]);
        let dirs: BTreeSet<PathBuf> = ["/r/a", "/r/b"].map(PathBuf::from).into();
        // The open folder is read; the shut one is not, only marked.
        assert_eq!(cooling.defer(dirs, &open), BTreeSet::from([PathBuf::from("/r/a")]));
        assert!(cooling.is_stale_in(Path::new("/r/b")) && !cooling.is_stale_in(Path::new("/r/a")));
        assert!(cooling.due(&open).is_empty());
        // It opens: now it is due once, and reading it clears the mark.
        let open = paths(&["/r", "/r/a", "/r/b"]);
        assert_eq!(cooling.due(&open), BTreeSet::from([PathBuf::from("/r/b")]));
        cooling.read(Path::new("/r/b"));
        assert!(cooling.due(&open).is_empty() && !cooling.is_stale_in(Path::new("/r")));
    }

    #[test]
    fn a_parent_shut_with_its_children_stays_warm_longest() {
        let mut cooling = Cooling::default();
        let loaded = paths(&["/r", "/r/src", "/r/src/a", "/r/src/b"]);
        // All three shut in one pass, room for one: the subfolders go, the parent stays.
        let dropped = cooling.cool(&loaded, Path::new("/r"), &paths(&["/r"]), 1, &|_| 0, usize::MAX);
        assert_eq!(dropped.len(), 2);
        assert!(!dropped.contains(&PathBuf::from("/r/src")));
        assert_eq!(cooling.warm, [PathBuf::from("/r/src")]);
    }

    #[test]
    fn only_the_last_shut_folders_keep_their_children() {
        let mut cooling = Cooling::default();
        let root = Path::new("/r");
        let mut loaded = paths(&["/r", "/r/a", "/r/b", "/r/c"]);
        // Three shut in one pass, room for two: the last in name order goes (the caller forgets
        // it), and the next pass finds nothing more to drop.
        let dropped = cooling.cool(&loaded, root, &paths(&["/r"]), 2, &|_| 0, usize::MAX);
        assert_eq!(dropped, vec![PathBuf::from("/r/c")]);
        loaded.remove(Path::new("/r/c"));
        assert!(cooling.cool(&loaded, root, &paths(&["/r"]), 2, &|_| 0, usize::MAX).is_empty());
        // A folder that opens leaves the warm set; shutting it again puts it last.
        cooling.cool(&loaded, root, &paths(&["/r", "/r/a"]), 2, &|_| 0, usize::MAX);
        assert!(!cooling.warm.iter().any(|dir| dir == Path::new("/r/a")));
        assert!(cooling.cool(&loaded, root, &paths(&["/r"]), 2, &|_| 0, usize::MAX).is_empty());
        assert_eq!(cooling.warm, [PathBuf::from("/r/b"), PathBuf::from("/r/a")]);
    }

    #[test]
    fn shut_folders_keep_rows_up_to_the_budget_and_the_longest_shut_go_first() {
        let mut cooling = Cooling::default();
        let root = Path::new("/r");
        let mut loaded = paths(&["/r", "/r/a", "/r/b", "/r/c"]);
        // Each folder holds 5 rows; the budget is 10, so two stay warm. The last in name order is the longest shut.
        let dropped = cooling.cool(&loaded, root, &paths(&["/r"]), 12, &|_| 5, 10);
        assert_eq!(dropped, vec![PathBuf::from("/r/c")]);
        loaded.remove(Path::new("/r/c"));
        assert!(cooling.cool(&loaded, root, &paths(&["/r"]), 12, &|_| 5, 10).is_empty());
        assert_eq!(cooling.warm, [PathBuf::from("/r/b"), PathBuf::from("/r/a")]);
        // A folder alone over the budget is not kept warm: it is read again when it opens.
        let dropped = cooling.cool(&loaded, root, &paths(&["/r"]), 12, &|dir| if dir == Path::new("/r/a") { 50 } else { 5 }, 10);
        assert_eq!(dropped, vec![PathBuf::from("/r/b"), PathBuf::from("/r/a")]);
        assert!(cooling.warm.is_empty());
    }

    #[test]
    fn open_dirs_follow_expanded_folders_only() {
        let inner = TreeItem::new("/r/a/b", "b").child(TreeItem::new("/r/a/b/f", "f")).expanded(true);
        let a = TreeItem::new("/r/a", "a").child(inner).expanded(false);
        let c = TreeItem::new("/r/c", "c").child(TreeItem::new("/r/c/f", "f")).expanded(true);
        // `b` is expanded but sits in a shut folder, so it does not show.
        assert_eq!(open_dirs(Path::new("/r"), &[a, c]), paths(&["/r", "/r/c"]));
    }

    #[test]
    fn an_option_click_opens_every_folder_below_and_shuts_them_again() {
        use super::{collapse_below, open_folders};
        let leaf = |id: &str| TreeItem::new(id.to_string(), "f");
        let b = TreeItem::new("/r/a/b", "b").child(TreeItem::new("/r/a/b/d", "d").child(leaf("/r/a/b/d/f")));
        let c = TreeItem::new("/r/a/c", "c").child(leaf("/r/a/c/f"));
        let a = TreeItem::new("/r/a", "a").child(b).child(c).child(leaf("/r/a/file")).expanded(true);
        // `b` and `d` are read, `c` is not: it opens and is asked for, and the file stays alone.
        let loaded = paths(&["/r/a", "/r/a/b", "/r/a/b/d"]);
        let (mut left, mut ask) = (10, Vec::new());
        open_folders(&a, &loaded, &|_| false, &mut left, &mut ask);
        assert!(a.kids().iter().filter(|kid| kid.is_folder()).all(|kid| kid.is_expanded()));
        assert!(a.kids()[0].kids()[0].is_expanded());
        assert_eq!((left, ask), (7, vec![PathBuf::from("/r/a/c")]));
        // The cap stops the walk: one folder opens when only one is left.
        collapse_below(&a);
        let (mut left, mut ask) = (1, Vec::new());
        open_folders(&a, &loaded, &|_| false, &mut left, &mut ask);
        assert_eq!(left, 0);
        assert!(a.kids()[0].is_expanded() && !a.kids()[0].kids()[0].is_expanded() && !a.kids()[1].is_expanded());
        // Shutting takes every folder below shut and leaves the folder itself.
        collapse_below(&a);
        assert!(a.is_expanded() && !a.kids()[0].is_expanded() && !a.kids()[0].kids()[0].is_expanded());
    }

    #[test]
    fn typing_letters_picks_the_next_name_that_starts_with_them() {
        use super::prefix_match;
        let rows = [("docs", false), ("Readme.md", false), ("src", false), ("README.txt", false), ("Empty", true), ("target", false)];
        // From the top, "r" lands on the first match; "r" again steps to the next, then wraps.
        assert_eq!(prefix_match(&rows, None, "r"), Some(1));
        assert_eq!(prefix_match(&rows, Some(1), "r"), Some(3));
        assert_eq!(prefix_match(&rows, Some(3), "r"), Some(1));
        // A longer prefix stays on the selection while it matches, else moves down and wraps.
        assert_eq!(prefix_match(&rows, Some(1), "re"), Some(1));
        assert_eq!(prefix_match(&rows, Some(3), "readme.m"), Some(1));
        assert_eq!(prefix_match(&rows, Some(5), "re"), Some(1));
        // Placeholder rows never match; no match and no rows give nothing.
        assert_eq!(prefix_match(&rows, Some(0), "e"), None);
        assert_eq!(prefix_match(&rows, Some(0), "zz"), None);
        assert_eq!(prefix_match(&[], None, "a"), None);
    }

    #[test]
    fn a_long_menu_name_loses_its_middle_and_keeps_the_extension() {
        use super::{MENU_NAME_MAX, cut_middle};
        assert_eq!(cut_middle("README.md", MENU_NAME_MAX), "README.md");
        let long = format!("{}.swift", "a".repeat(60));
        let cut = cut_middle(&long, MENU_NAME_MAX);
        assert_eq!(cut.chars().count(), MENU_NAME_MAX);
        assert!(cut.ends_with("aaaaaa.swift") && cut.contains('\u{2026}'));
        assert_eq!(cut_middle(&"b".repeat(60), MENU_NAME_MAX).chars().count(), MENU_NAME_MAX);
    }

    #[test]
    fn accented_names_sort_with_their_base_letter() {
        use super::finder_cmp;
        let mut names = ["Zebra", "ecole10", "\u{e9}cole", "apple", "\u{c5}ngstr\u{f6}m", "ecole2", "Ecole", "ecole"];
        names.sort_by(|a, b| finder_cmp(a, b));
        // Ties on the folded name fall back to the raw one: E, e, then e-acute.
        assert_eq!(names, ["\u{c5}ngstr\u{f6}m", "apple", "Ecole", "ecole", "\u{e9}cole", "ecole2", "ecole10", "Zebra"]);
        // A decomposed name (e + combining acute, as HFS+ stores it) sorts the same as the composed one.
        assert!(finder_cmp("e\u{301}cole", "ecole2").is_lt() && finder_cmp("e\u{301}cole", "apple").is_gt());
    }
}
