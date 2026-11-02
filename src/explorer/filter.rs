//! Filter by name: a field at the foot of the explorer narrows the tree to the
//! names that match, with their folders open.
//!
//! The first key walks the whole root on the background executor, with the
//! same ignore rules as the tree (`.git`, `.DS_Store`, `.gitignore` and
//! `.ignore` files), and keeps the result as a flat index until the field
//! clears. Each key then matches the query against that index, also in the
//! background; a newer key makes the older search stop at its next check. The
//! tree's own items are never touched while a filter shows, so clearing it
//! brings the open folders and the selection back as they were.
//!
//! Matching is fuzzy: the query's letters must appear in the name in order,
//! case-insensitively, and a run of the same letters is preferred (a name that
//! holds the query whole matches there, at a word start when it can).

use std::{
    collections::{BTreeSet, HashMap},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering::Relaxed},
    },
};

use gpui_kit::base::TreeItem;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::input::{Copy, Escape, InputEvent, InputState, SelectAll};
use gpui_kit::*;
use ignore::{WalkBuilder, WalkState};
use gpui_kit::prelude::FluentBuilder as _;

use super::{Explorer, ExplorerEvent, PLACEHOLDER_EMPTY, finder_cmp, placeholder};
// The focus fade and ring are the floats' too (`float_field`); the explorer's row.rs reaches them through here.
pub(super) use crate::float_field::{field_focus, forget, paint_inset_ring};
use crate::{
    float_field::{field_input, mix, paint_focus_ring},
    hover::{self, by_lit, icon_button},
    symbols, theme, tooltip,
    tokens::{
        ALPHA_SECONDARY, ALPHA_FILTER, ALPHA_FILTER_FOCUSED, ALPHA_FILTER_FOCUSED_LIGHT, ALPHA_FILTER_LIGHT,
        ALPHA_PRIMARY, FIELD_SYMBOL_GAP, FILTER_HEIGHT, FILTER_MATCH_CAP, INDEX_ENTRY_CAP, TAB_CLOSE_HIT, TAB_CLOSE_SLOT,
        FILTER_INSET, FILTER_PAD_X, FILTER_SYMBOL, SIDEBAR_EDGE, TEXT_BODY, TEXT_BODY_LINE,
    },
};

actions!(explorer, [
    /// Moves the keyboard to the explorer's filter field.
    FocusFilter
]);


/// The note row's words when the cap cut the matches: how many show of how many matched.
fn capped_note(shown: usize, total: usize) -> String {
    format!("{} of {} shown", crate::find_bar::grouped(shown), crate::find_bar::grouped(total))
}

/// The words when the index stopped at `INDEX_ENTRY_CAP`: Open Quickly's last row and the filter's.
pub(crate) fn index_cut_note() -> String {
    format!("Showing the first {} files. Open a smaller folder for the rest.", crate::find_bar::grouped(INDEX_ENTRY_CAP))
}

/// Where each matching name's letters are, by item id, in characters. Present
/// only while a filter shows; every name then draws dimmer, and the letters in
/// here draw bold at full ink.
pub type Hits = Rc<HashMap<SharedString, Vec<usize>>>;

/// What the explorer keeps for its filter field.
pub struct State {
    input: Entity<InputState>,
    /// Set while a query narrows the tree.
    active: Option<Active>,
    /// Every visible entry under the root, read while the field has the
    /// keyboard or a filter shows.
    index: Option<Arc<Index>>,
    /// The read of the index, while it goes.
    loading: Option<Task<()>>,
    /// The read's number; a newer one (or a reset) makes it stop.
    reading: Arc<AtomicU64>,
    /// Folders the watcher reported while the read went, judged once it lands.
    touched: BTreeSet<PathBuf>,
    task: Option<Task<()>>,
    /// The newest search's number; an older one sees it changed and stops.
    latest: Arc<AtomicU64>,
    focused: bool,
    _subscriptions: Vec<Subscription>,
}

struct Active {
    /// The tree's selection before the first key, put back on clear.
    selection: Option<SharedString>,
    hits: Hits,
    /// The first file in the filtered tree, which Return opens.
    first_file: Option<PathBuf>,
    /// The search came back with nothing.
    none: bool,
}

impl State {
    pub fn new(window: &mut Window, cx: &mut Context<Explorer>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Filter"));
        let handle = input.focus_handle(cx);
        let subscriptions = vec![
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, _, cx| match event {
                InputEvent::Change => this.filter_changed(cx),
                InputEvent::PressEnter { .. } => this.open_first_match(cx),
                InputEvent::Focus | InputEvent::Blur => {}
            }),
            // The field draws inside this view, which is cached; its caret blinks in the explorer's caret layer.
            cx.observe(&input, |_, _, cx| cx.notify()),
            cx.on_focus_in(&handle, window, |this, _, cx| this.filter_focus(true, cx)),
            cx.on_focus_out(&handle, window, |this, _, _, cx| this.filter_focus(false, cx)),
        ];
        Self {
            input,
            active: None,
            index: None,
            loading: None,
            reading: Arc::default(),
            touched: BTreeSet::new(),
            task: None,
            latest: Arc::default(),
            focused: false,
            _subscriptions: subscriptions,
        }
    }

    /// Drops the index, and the read of one if it is going; a read that was
    /// started sees the number change and stops.
    fn reset_index(&mut self) {
        self.index = None;
        self.loading = None;
        self.reading.fetch_add(1, Relaxed);
        self.touched.clear();
    }

    /// The colors of the names: `Some` while a filter shows.
    pub fn hits(&self) -> Option<Hits> {
        self.active.as_ref().map(|active| active.hits.clone())
    }

    /// The field itself, for the explorer's caret layer.
    pub fn input(&self) -> &Entity<InputState> {
        &self.input
    }

    /// Whether the field has the keyboard.
    pub fn focused(&self) -> bool {
        self.focused
    }

    /// A filter shows and found nothing.
    pub fn none(&self) -> bool {
        self.active.as_ref().is_some_and(|active| active.none)
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }
}

impl Explorer {
    /// Puts the keyboard in the filter field, selecting what is typed there.
    pub fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        cx.notify();
    }

    /// For the control socket: types `query` into the filter field and runs it; an empty one clears it.
    pub fn type_filter(&mut self, query: &str, window: &mut Window, cx: &mut Context<Self>) {
        if !query.is_empty() {
            self.focus_filter(window, cx);
        }
        // Setting a value sends no change, so the filter is run here.
        self.filter.input.update(cx, |input, cx| input.set_value(query.to_string(), window, cx));
        self.filter_changed(cx);
    }

    fn filter_focus(&mut self, focused: bool, cx: &mut Context<Self>) {
        if self.filter.focused != focused {
            self.filter.focused = focused;
            // The ring and fill ease from `field_focus`, which reads the field's focus as it draws.
            if focused {
                self.start_index(cx);
            } else {
                // The results on screen stay; the index (the memory) goes, and the next focus or key reads it again.
                self.filter.reset_index();
            }
            cx.notify();
        }
    }

    fn filter_changed(&mut self, cx: &mut Context<Self>) {
        let query = self.filter.input.read(cx).value().trim().to_string();
        if query.is_empty() {
            self.clear_filter(cx);
            return;
        }
        if self.filter.active.is_none() {
            let selection = self.tree.read(cx).selected_item().map(|item| item.id.clone());
            self.filter.active = Some(Active { selection, hits: Rc::default(), first_file: None, none: false });
        }
        self.search(query, cx);
    }

    /// Matches `query` in the background against the index. With no index yet
    /// the read is started (or already going) and runs this search when it lands.
    fn search(&mut self, query: String, cx: &mut Context<Self>) {
        let number = self.filter.latest.fetch_add(1, Relaxed) + 1;
        let Some(index) = self.filter.index.clone() else {
            self.filter.task = None;
            self.start_index(cx);
            return;
        };
        let latest = self.filter.latest.clone();
        let root = self.root.clone();
        self.filter.task = Some(cx.spawn(async move |this, cx| {
            let found = cx
                .background_executor()
                .spawn(async move { index.search(&root, &query, FILTER_MATCH_CAP, &|| latest.load(Relaxed) != number) })
                .await;
            let Some(matches) = found else { return };
            this.update(cx, |this, cx| {
                if this.filter.latest.load(Relaxed) != number {
                    return;
                }
                this.show_matches(matches, cx);
            })
            .ok();
        }));
    }

    /// Replaces the tree's rows with the matches, every folder open. Past the cap a disabled note row
    /// at the end says how many of the matches show.
    fn show_matches(&mut self, matches: Matches, cx: &mut Context<Self>) {
        let Some(active) = self.filter.active.as_mut() else {
            return;
        };
        let mut hits = HashMap::new();
        let mut first_file = None;
        let mut items = node_items(matches.nodes, &mut hits, &mut first_file);
        active.none = items.is_empty();
        // One row: a second placeholder would share its id.
        let mut notes = Vec::new();
        if matches.total > matches.shown {
            notes.push(capped_note(matches.shown, matches.total));
        }
        if self.filter.index.as_ref().is_some_and(|index| index.capped()) {
            notes.push(index_cut_note());
        }
        if !notes.is_empty() {
            items.push(placeholder(&self.root, PLACEHOLDER_EMPTY, &notes.join(". ")));
        }
        active.hits = Rc::new(hits);
        active.first_file = first_file;
        self.tree.update(cx, |tree, cx| tree.set_items(items, cx));
        cx.notify();
    }

    /// Stops any search and brings the whole tree back, with the selection it had.
    fn clear_filter(&mut self, cx: &mut Context<Self>) {
        self.filter.latest.fetch_add(1, Relaxed);
        self.filter.task = None;
        // A field that still has the keyboard keeps its index for the next key.
        if !self.filter.focused {
            self.filter.reset_index();
        }
        let Some(active) = self.filter.active.take() else {
            return;
        };
        let items = self.items.clone();
        self.tree.update(cx, |tree, cx| {
            tree.set_items(items, cx);
            let ix = active.selection.and_then(|id| tree.index_of(&id));
            tree.set_selected_index(ix, cx);
            if let Some(ix) = ix {
                tree.scroll_to_item(ix, ScrollStrategy::Nearest);
            }
        });
        cx.notify();
    }

    /// The clear button's work, and the empty state's Clear filter: empties the field, brings the tree back and keeps the keyboard in the field.
    fn clear_field(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter.input.update(cx, |input, cx| input.set_value("", window, cx));
        self.clear_filter(cx);
        self.focus_filter(window, cx);
    }

    /// Escape: clears the field and gives the keyboard to the tree.
    pub(super) fn escape_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.filter.input.read(cx).value().is_empty() {
            // Setting a value sends no change, so the tree is brought back here.
            self.filter.input.update(cx, |input, cx| input.set_value("", window, cx));
        }
        self.clear_filter(cx);
        self.focus_tree(window, cx);
    }

    /// Puts the keyboard in the tree (Shift-Cmd-E, or Escape in the filter field).
    pub fn focus_tree(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tree.update(cx, |tree, cx| tree.focus(window, cx));
    }

    /// Return in the field opens the first file the filter shows.
    fn open_first_match(&mut self, cx: &mut Context<Self>) {
        if let Some(path) = self.filter.active.as_ref().and_then(|active| active.first_file.clone()) {
            cx.emit(ExplorerEvent::Open(path));
        }
    }

    /// The files changed on disk. The index goes stale only when a change sits
    /// in a folder it holds, so a build writing into an ignored `target/`
    /// leaves it be; a read still going keeps the folders touched meanwhile
    /// and judges them against what it finds.
    pub(super) fn filter_files_changed(&mut self, paths: &[PathBuf], cx: &mut Context<Self>) {
        let mut touched = paths.iter().map(|path| touched_dir(path, &self.root));
        if let Some(index) = &self.filter.index {
            if touched.any(|dir| index.holds_dir(&self.root, dir)) {
                self.refilter(cx);
            }
        } else if self.filter.loading.is_some() {
            for dir in touched {
                if !self.filter.touched.contains(dir) {
                    self.filter.touched.insert(dir.to_path_buf());
                }
            }
        }
    }

    /// The root or the files under it changed in a way the index can't follow:
    /// reads it again, and the open filter searches the new one.
    pub(super) fn refilter(&mut self, cx: &mut Context<Self>) {
        self.filter.reset_index();
        if self.filter.active.is_some() {
            self.filter_changed(cx);
        } else if self.filter.focused {
            self.start_index(cx);
        }
    }

    /// Reads the index in the background unless it is there or on its way. The
    /// field starts it when it takes the keyboard, so it is ready by the first key.
    fn start_index(&mut self, cx: &mut Context<Self>) {
        if self.filter.index.is_some() || self.filter.loading.is_some() {
            return;
        }
        let reading = self.filter.reading.clone();
        let number = reading.load(Relaxed);
        let root = self.root.clone();
        let show_hidden = crate::settings::get(cx).explorer.show_hidden;
        self.filter.loading = Some(cx.spawn(async move |this, cx| {
            let read = cx
                .background_executor()
                .spawn({
                    let reading = reading.clone();
                    async move { Index::read(&root, show_hidden, &|| reading.load(Relaxed) != number) }
                })
                .await;
            let Some(index) = read else { return };
            this.update(cx, |this, cx| {
                if this.filter.reading.load(Relaxed) != number {
                    return;
                }
                this.filter.loading = None;
                let touched = std::mem::take(&mut this.filter.touched);
                if touched.iter().any(|dir| index.holds_dir(&this.root, dir)) {
                    this.start_index(cx);
                    return;
                }
                this.filter.index = Some(Arc::new(index));
                if this.filter.active.is_some() {
                    this.filter_changed(cx);
                }
            })
            .ok();
        }));
    }

    /// The capsule at the foot of the explorer: a filter symbol and the field,
    /// its fill and focus ring easing in and out together on focus changes.
    pub(super) fn filter_footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let ink = theme.sidebar_foreground;
        let (rest, lit) = if theme::is_light(cx) {
            (ALPHA_FILTER_LIGHT, ALPHA_FILTER_FOCUSED_LIGHT)
        } else {
            (ALPHA_FILTER, ALPHA_FILTER_FOCUSED)
        };
        let ring = theme.ring;
        let muted = theme::ink(ALPHA_SECONDARY, cx);
        let focused = self.filter.focused;
        // The fill and the ring ease together on `field_focus`, drawn in one pass over the capsule.
        let capsule = canvas(
            move |bounds, window, _| (bounds, field_focus("filter-capsule", focused, window)),
            move |_, (bounds, level), window, _| {
                let radius = bounds.size.height / 2.;
                window.paint_quad(gpui::fill(bounds, ink.opacity(mix(rest, lit, level))).corner_radii(Corners::all(radius)));
                paint_focus_ring(window, bounds, radius, level, ring);
            },
        )
        .absolute()
        .inset_0();
        let input = self.filter.input.clone();
        let has_text = !input.read(cx).value().is_empty();
        let clear_ink = hover::Ink { rest: muted, lit: theme::ink(ALPHA_PRIMARY, cx), wash: ink };
        // Clears the field and keeps the keyboard in it.
        let on_clear = cx.listener(|this, _: &ClickEvent, window, cx| this.clear_field(window, cx));
        div()
            .flex_none()
            .px(SIDEBAR_EDGE) // design: `.px` is the horizontal padding method, the value is a token
            .py(FILTER_INSET)
            .child(
                div()
                    .relative()
                    .h(FILTER_HEIGHT)
                    .w_full()
                    .child(capsule)
                    .child(
                        div()
                            .relative()
                            .size_full()
                            .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
                            .flex()
                            .items_center()
                            .gap(FIELD_SYMBOL_GAP)
                            .text_size(TEXT_BODY)
                            .line_height(TEXT_BODY_LINE)
                            .child(symbols::icon("line.3.horizontal.decrease", FILTER_SYMBOL).text_color(muted).flex_none())
                            .child(div().flex_1().min_w_0().child(field_input(&input, true)))
                            // The clear button fades in with the text and out with it; the slot stays so the field never shifts.
                            .child(div().relative().flex_none().size(TAB_CLOSE_SLOT).child(by_lit("filter-clear-shown", has_text, move |level| {
                                div().size_full().opacity(level).when(has_text || level > 0., |slot| {
                                    // The hit is `TAB_CLOSE_HIT` square, laid out over the slot and out each side of it, as the tab's close button is.
                                    let out = (TAB_CLOSE_HIT - TAB_CLOSE_SLOT) / 2.;
                                    slot.child(
                                        div()
                                            .absolute()
                                            .top(-out)
                                            .left(-out)
                                            .size(TAB_CLOSE_HIT)
                                            .child(
                                                icon_button("filter-clear", "xmark.circle.fill", TAB_CLOSE_HIT, FILTER_SYMBOL, clear_ink)
                                                    .occlude()
                                                    .on_click(on_clear)
                                                    .tooltip(tooltip::tip("Clear filter"))
                                                    .tooltip_show_delay(tooltip::DELAY),
                                            )
                                            .children(crate::control::rects_enabled().then(|| {
                                                canvas(|bounds, window, _| crate::control::record(window, "filter-clear", bounds), |_, (), _, _| {})
                                                    .absolute()
                                                    .top_0()
                                                    .left_0()
                                                    .size_full()
                                            })),
                                    )
                                })
                            })))
                            // The field answers these itself, ahead of the window's editor.
                            .capture_action(cx.listener(|this, _: &Escape, window, cx| {
                                this.escape_filter(window, cx);
                                cx.stop_propagation();
                            }))
                            .capture_action(cx.listener(|this, _: &SelectAll, window, cx| {
                                this.filter.input.update(cx, |input, cx| input.select_all(window, cx));
                                cx.stop_propagation();
                            }))
                            .capture_action(cx.listener(|this, _: &Copy, _, cx| {
                                let selected = this.filter.input.read(cx).selected_value();
                                if !selected.is_empty() {
                                    cx.write_to_clipboard(ClipboardItem::new_string(selected.to_string()));
                                }
                                cx.stop_propagation();
                            }))
                            // Down moves into the filtered rows.
                            .capture_action(cx.listener(|this, _: &gpui_kit::component::input::MoveDown, window, cx| {
                                if this.filter.is_active() && !this.filter.none() {
                                    this.focus_tree(window, cx);
                                    this.tree.update(cx, |tree, cx| tree.set_selected_index(Some(0), cx));
                                    cx.stop_propagation();
                                }
                            })),
                    ),
            )
            .into_any_element()
    }

    /// What shows in place of the tree when nothing matches.
    pub(super) fn filter_none(&self, cx: &mut Context<Self>) -> AnyElement {
        let folder = self.root.file_name().map_or_else(
            || self.root.to_string_lossy(),
            |name| name.to_string_lossy(),
        );
        let query = self.filter.input.read(cx).value();
        crate::empty_state::view(
            "explorer-no-matches",
            "line.3.horizontal.decrease.circle",
            "No matches",
            format!("\u{201c}{}\u{201d} in {folder}", query.trim()),
            vec![crate::empty_state::Action {
                id: "explorer-clear-filter",
                label: "Clear filter",
                key: Some("\u{238b}"),
                run: Rc::new(cx.listener(|this, _: &ClickEvent, window, cx| this.clear_field(window, cx))),
            }],
            cx,
        )
    }
}

/// The items for a filtered tree: every folder open, the matching letters
/// recorded by id. A folder that holds matches below it is context: its own letters are marked only
/// when the query sits in its name whole, never as scattered letters.
fn node_items(nodes: Vec<Node>, hits: &mut HashMap<SharedString, Vec<usize>>, first_file: &mut Option<PathBuf>) -> Vec<TreeItem> {
    nodes
        .into_iter()
        .map(|node| {
            let id = node.path.to_string_lossy().into_owned();
            let context = node.is_dir && !node.kids.is_empty() && scattered(&node.hits);
            if !node.hits.is_empty() && !context {
                hits.insert(SharedString::from(id.clone()), node.hits);
            }
            let item = TreeItem::new(id, node.name);
            if !node.is_dir {
                first_file.get_or_insert(node.path);
                return item;
            }
            if node.kids.is_empty() {
                item.child(placeholder(&node.path, PLACEHOLDER_EMPTY, "No matches inside")).expanded(false)
            } else {
                item.children(node_items(node.kids, hits, first_file)).expanded(true)
            }
        })
        .collect()
}

/// Whether the matched letters are not one run: the query was found as letters spread through the name.
fn scattered(hits: &[usize]) -> bool {
    hits.windows(2).any(|pair| pair[1] != pair[0] + 1)
}

const NONE: u32 = u32::MAX;

struct Entry {
    parent: u32,
    is_dir: bool,
    name: Box<str>,
}

/// Every visible entry under the root, parents before children. Open Quickly reads one of its own.
pub(crate) struct Index {
    entries: Vec<Entry>,
    /// Where each folder's subtree ends in `entries`, as (folder, end) sorted
    /// by folder; finds a folder's children and skips what is inside the rest.
    spans: Vec<(u32, u32)>,
    /// The walk stopped at `INDEX_ENTRY_CAP`, so files past it aren't listed.
    capped: bool,
}

/// One path found by a walker thread: where its text sits below the root in the batch's arena.
struct Row {
    start: u32,
    len: u32,
    depth: u32,
    is_dir: bool,
}

/// What one walker thread has found, as paths below the root packed into one
/// string; handed over when the thread is done.
#[derive(Default)]
struct Found {
    arena: String,
    rows: Vec<Row>,
}

struct Batch<'a> {
    found: Found,
    root: &'a Path,
    into: &'a Mutex<Vec<Found>>,
}

impl Drop for Batch<'_> {
    fn drop(&mut self) {
        if let Ok(mut all) = self.into.lock() {
            all.push(std::mem::take(&mut self.found));
        }
    }
}

/// One file Open Quickly lists: where it is, its name with the matched characters, and its folder
/// below the root ("" for the root itself).
pub(crate) struct Pick {
    pub path: PathBuf,
    pub name: SharedString,
    pub folder: SharedString,
    pub hits: Vec<usize>,
}

/// What a search found: the tree of the entries it shows, how many matched entries that is, and how many matched in all.
struct Matches {
    nodes: Vec<Node>,
    shown: usize,
    total: usize,
}

/// A match and the folders above it, as the filtered tree shows them.
struct Node {
    path: PathBuf,
    name: String,
    is_dir: bool,
    /// The matched characters of the name; empty for a folder that only holds matches.
    hits: Vec<usize>,
    kids: Vec<Node>,
}

/// The walk the tree's folders and the filter share, and Find in folder: `.gitignore` files (also
/// those of parent folders) and `.ignore` files rule entries out, `.git` and `.DS_Store` are left
/// out, and dot files too unless `show_hidden`. The root itself is never ruled out.
pub(crate) fn walker(root: &Path, show_hidden: bool) -> WalkBuilder {
    let mut walk = WalkBuilder::new(root);
    walk.parents(true).hidden(false).git_ignore(true).require_git(false).filter_entry(move |entry| {
        let name = entry.file_name().to_string_lossy();
        entry.depth() == 0 || (!matches!(&*name, ".git" | ".DS_Store") && (show_hidden || !name.starts_with('.')))
    });
    walk
}

impl Index {
    /// Walks the root on every core with the tree's ignore rules; `None` when
    /// `stale` says the read was given up.
    pub(crate) fn read(root: &Path, show_hidden: bool, stale: &(dyn Fn() -> bool + Sync)) -> Option<Self> {
        Self::read_capped(root, show_hidden, INDEX_ENTRY_CAP, stale)
    }

    /// `read`, stopping after `cap` entries (`capped` says it did). Entries come in walk order, so which ones a capped read
    /// leaves out depends on the order the walker threads reach them; the entries it keeps always hold their parent folders.
    fn read_capped(root: &Path, show_hidden: bool, cap: usize, stale: &(dyn Fn() -> bool + Sync)) -> Option<Self> {
        let walk = walker(root, show_hidden).build_parallel();
        let batches: Mutex<Vec<Found>> = Mutex::new(Vec::new());
        let (quit, full, taken) = (AtomicBool::new(false), AtomicBool::new(false), AtomicUsize::new(0));
        walk.run(|| {
            let mut mine = Batch { found: Found::default(), root, into: &batches };
            let (quit, full, taken) = (&quit, &full, &taken);
            Box::new(move |entry| {
                if quit.load(Relaxed) || full.load(Relaxed) {
                    return WalkState::Quit;
                }
                let Ok(entry) = entry else {
                    return WalkState::Continue;
                };
                if entry.depth() == 0 {
                    return WalkState::Continue;
                }
                if mine.found.rows.len() % 512 == 0 && stale() {
                    quit.store(true, Relaxed);
                    return WalkState::Quit;
                }
                let is_dir = entry.file_type().is_some_and(|kind| kind.is_dir())
                    || (entry.path_is_symlink() && entry.path().is_dir());
                let depth = entry.depth() as u32;
                if let Ok(rel) = entry.path().strip_prefix(mine.root) {
                    // One slot per kept path, so the index holds exactly `cap`; every folder is taken before what it holds.
                    if taken.fetch_add(1, Relaxed) >= cap {
                        full.store(true, Relaxed);
                        return WalkState::Quit;
                    }
                    let rel = rel.to_string_lossy();
                    let start = mine.found.arena.len() as u32;
                    mine.found.arena.push_str(&rel);
                    mine.found.rows.push(Row { start, len: rel.len() as u32, depth, is_dir });
                }
                WalkState::Continue
            })
        });
        if quit.load(Relaxed) || stale() {
            return None;
        }
        let full = full.load(Relaxed);
        let batches = batches.into_inner().ok()?;
        let mut order: Vec<(u32, u32)> = Vec::with_capacity(batches.iter().map(|found| found.rows.len()).sum());
        for (batch, found) in batches.iter().enumerate() {
            order.extend((0..found.rows.len() as u32).map(|row| (batch as u32, row)));
        }
        let row = |&(batch, row): &(u32, u32)| {
            let found = &batches[batch as usize];
            let row = &found.rows[row as usize];
            (row, &found.arena[row.start as usize..(row.start + row.len) as usize])
        };
        // Path order puts every folder before what it holds, its whole subtree right after it.
        order.sort_unstable_by(|a, b| Path::new(row(a).1).cmp(Path::new(row(b).1)));
        let mut entries: Vec<Entry> = Vec::with_capacity(order.len());
        // The folders above the next row, by entry and depth, and each folder's subtree end.
        let mut open: Vec<(u32, u32)> = Vec::new();
        let mut spans: Vec<(u32, u32)> = Vec::new();
        for (at, key) in order.iter().enumerate() {
            let (found, rel) = row(key);
            while let Some(&(dir, depth)) = open.last() {
                if depth < found.depth {
                    break;
                }
                spans.push((dir, at as u32));
                open.pop();
            }
            let parent = open.last().map_or(NONE, |&(dir, _)| dir);
            let name = Path::new(rel).file_name().map_or_else(Default::default, |name| name.to_string_lossy().into());
            if found.is_dir {
                open.push((at as u32, found.depth));
            }
            entries.push(Entry { parent, is_dir: found.is_dir, name });
        }
        spans.extend(open.into_iter().map(|(dir, _)| (dir, entries.len() as u32)));
        spans.sort_unstable();
        Some(Self { entries, spans, capped: full })
    }

    /// The end of the entries held by folder `ix`: its subtree is `ix + 1..end`.
    fn subtree_end(&self, ix: u32) -> u32 {
        self.spans.binary_search_by_key(&ix, |span| span.0).map_or(ix + 1, |at| self.spans[at].1)
    }

    /// Whether `dir` is the root or a folder in the index, found by walking its
    /// names down from the root.
    fn holds_dir(&self, root: &Path, dir: &Path) -> bool {
        let Ok(below) = dir.strip_prefix(root) else {
            return false;
        };
        let (mut from, mut end) = (0, self.entries.len() as u32);
        for part in below.components() {
            let part = part.as_os_str().to_string_lossy();
            let mut ix = from;
            let found = loop {
                if ix >= end {
                    return false;
                }
                let entry = &self.entries[ix as usize];
                if !entry.is_dir {
                    ix += 1;
                } else if *entry.name == *part {
                    break ix;
                } else {
                    ix = self.subtree_end(ix);
                }
            };
            from = found + 1;
            end = self.subtree_end(found);
        }
        true
    }

    /// Whether the index holds an entry at `path` (a file or a folder), found by walking its
    /// names down from the root.
    pub(crate) fn has_entry(&self, root: &Path, path: &Path) -> bool {
        let Ok(below) = path.strip_prefix(root) else {
            return false;
        };
        let parts: Vec<_> = below.components().collect();
        let (mut from, mut end) = (0, self.entries.len() as u32);
        for (at, part) in parts.iter().enumerate() {
            let part = part.as_os_str().to_string_lossy();
            let last = at + 1 == parts.len();
            let mut ix = from;
            let found = loop {
                if ix >= end {
                    return false;
                }
                let entry = &self.entries[ix as usize];
                if *entry.name == *part && (last || entry.is_dir) {
                    break ix;
                }
                ix = if entry.is_dir { self.subtree_end(ix) } else { ix + 1 };
            };
            from = found + 1;
            end = self.subtree_end(found);
        }
        !parts.is_empty()
    }

    /// The tree of matches for `query`, folders first, each group in Finder's order, from the first
    /// `cap` matched entries in index order (with the folders above them); `total` counts them all.
    /// `None` when `stale` says a newer key came.
    fn search(&self, root: &Path, query: &str, cap: usize, stale: &dyn Fn() -> bool) -> Option<Matches> {
        let needle: Vec<char> = query.chars().filter(|ch| !ch.is_whitespace()).map(fold).collect();
        let mut keep = vec![false; self.entries.len()];
        let mut hits: HashMap<u32, Vec<usize>> = HashMap::new();
        let mut folded = Vec::new();
        let mut total = 0;
        for (ix, entry) in self.entries.iter().enumerate() {
            if ix % 512 == 0 && stale() {
                return None;
            }
            let Some(at) = positions(&needle, &entry.name, &mut folded) else {
                continue;
            };
            total += 1;
            if hits.len() >= cap {
                continue;
            }
            hits.insert(ix as u32, at);
            let mut up = ix;
            while !keep[up] {
                keep[up] = true;
                match self.entries[up].parent {
                    NONE => break,
                    parent => up = parent as usize,
                }
            }
        }
        let shown = hits.len();
        let mut kids: HashMap<u32, Vec<u32>> = HashMap::new();
        for (ix, entry) in self.entries.iter().enumerate() {
            if keep[ix] {
                kids.entry(entry.parent).or_default().push(ix as u32);
            }
        }
        Some(Matches { nodes: self.nodes(NONE, root, &mut kids, &mut hits), shown, total })
    }

    fn nodes(&self, parent: u32, dir: &Path, kids: &mut HashMap<u32, Vec<u32>>, hits: &mut HashMap<u32, Vec<usize>>) -> Vec<Node> {
        let mut list = kids.remove(&parent).unwrap_or_default();
        list.sort_by(|a, b| {
            let (a, b) = (&self.entries[*a as usize], &self.entries[*b as usize]);
            b.is_dir.cmp(&a.is_dir).then_with(|| finder_cmp(&a.name, &b.name))
        });
        list.into_iter()
            .map(|ix| {
                let entry = &self.entries[ix as usize];
                let path = dir.join(&*entry.name);
                Node {
                    kids: self.nodes(ix, &path, kids, hits),
                    hits: hits.remove(&ix).unwrap_or_default(),
                    name: entry.name.to_string(),
                    is_dir: entry.is_dir,
                    path,
                }
            })
            .collect()
    }

    /// The files whose names match `query` the way the filter matches, best first and at most
    /// `limit` of them: a name that starts with the query, then one that holds it whole at a word
    /// start, then anywhere, then its letters spread out; within each, the shorter name, then the
    /// shallower file, then path order. `None` when `stale` says a newer key came.
    pub(crate) fn files(&self, root: &Path, query: &str, limit: usize, stale: &dyn Fn() -> bool) -> Option<Vec<Pick>> {
        self.files_where(root, query, limit, stale, &|_| true, false)
    }

    /// `files`, limited to the names `keep` accepts. With `list_all`, an empty query lists every
    /// kept file (shortest name first) instead of none.
    pub(crate) fn files_where(&self, root: &Path, query: &str, limit: usize, stale: &dyn Fn() -> bool, keep: &dyn Fn(&str) -> bool, list_all: bool) -> Option<Vec<Pick>> {
        let needle: Vec<char> = query.chars().filter(|ch| !ch.is_whitespace()).map(fold).collect();
        let mut folded = Vec::new();
        let mut found: Vec<((u8, u32, u32, u32), Vec<usize>)> = Vec::new();
        for (ix, entry) in self.entries.iter().enumerate() {
            if ix % 512 == 0 && stale() {
                return None;
            }
            if entry.is_dir || !keep(&entry.name) {
                continue;
            }
            let at = if needle.is_empty() && list_all {
                folded.clear();
                folded.extend(entry.name.chars().map(fold));
                Vec::new()
            } else {
                let Some(at) = positions(&needle, &entry.name, &mut folded) else {
                    continue;
                };
                at
            };
            let tier = tier(&at, &folded);
            found.push(((tier, folded.len() as u32, self.depth(ix as u32), ix as u32), at));
        }
        if found.len() > limit {
            found.select_nth_unstable_by_key(limit, |(key, _)| *key);
            found.truncate(limit);
        }
        found.sort_unstable_by_key(|(key, _)| *key);
        Some(
            found
                .into_iter()
                .map(|((.., ix), hits)| {
                    let below = self.below(ix);
                    let folder = below.parent().map_or_else(String::new, |folder| folder.to_string_lossy().into_owned());
                    Pick { path: root.join(&below), name: self.entries[ix as usize].name.to_string().into(), folder: folder.into(), hits }
                })
                .collect(),
        )
    }

    /// How many files and folders the index holds.
    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the read stopped at `INDEX_ENTRY_CAP`: files past it are not in the index.
    pub(crate) fn capped(&self) -> bool {
        self.capped
    }

    /// How many folders hold entry `ix`.
    fn depth(&self, ix: u32) -> u32 {
        let (mut depth, mut up) = (0, self.entries[ix as usize].parent);
        while up != NONE {
            depth += 1;
            up = self.entries[up as usize].parent;
        }
        depth
    }

    /// Entry `ix`'s path below the root.
    fn below(&self, ix: u32) -> PathBuf {
        let mut names = vec![&*self.entries[ix as usize].name];
        let mut up = self.entries[ix as usize].parent;
        while up != NONE {
            names.push(&self.entries[up as usize].name);
            up = self.entries[up as usize].parent;
        }
        names.iter().rev().collect()
    }
}

/// The folder a change at `path` belongs to: the one it sits in, or the root
/// for the root itself.
fn touched_dir<'a>(path: &'a Path, root: &'a Path) -> &'a Path {
    if path == root { root } else { path.parent().unwrap_or(path) }
}

/// One character in lower case, keeping positions one to one.
fn fold(ch: char) -> char {
    ch.to_lowercase().next().unwrap_or(ch)
}

/// The characters of `name` (by index) that `needle` (already folded) matches,
/// or `None`. The query whole inside the name wins, at a word start when there
/// is one; else the tightest in-order run that ends at the first possible spot.
fn positions(needle: &[char], name: &str, folded: &mut Vec<char>) -> Option<Vec<usize>> {
    folded.clear();
    folded.extend(name.chars().map(fold));
    if needle.is_empty() || needle.len() > folded.len() {
        return None;
    }
    let mut whole = None;
    for start in 0..=folded.len() - needle.len() {
        if folded[start..start + needle.len()] == *needle {
            if start == 0 || !folded[start - 1].is_alphanumeric() {
                whole = Some(start);
                break;
            }
            whole.get_or_insert(start);
        }
    }
    if let Some(start) = whole {
        return Some((start..start + needle.len()).collect());
    }
    let (mut next, mut end) = (0, None);
    for (ix, ch) in folded.iter().enumerate() {
        if *ch == needle[next] {
            next += 1;
            if next == needle.len() {
                end = Some(ix);
                break;
            }
        }
    }
    let mut left = needle.len();
    let mut at = Vec::with_capacity(left);
    for ix in (0..=end?).rev() {
        if left > 0 && folded[ix] == needle[left - 1] {
            at.push(ix);
            left -= 1;
        }
    }
    at.reverse();
    Some(at)
}

/// How well `at` (the matched characters `positions` found in `folded`) ranks: 0 a name that starts with the query (or any
/// name for an empty one), 1 the query whole at a word start, 2 whole anywhere, 3 its letters spread out.
fn tier(at: &[usize], folded: &[char]) -> u8 {
    match at.first() {
        None => 0,
        _ if scattered(at) => 3,
        Some(0) => 0,
        Some(&start) if !folded[start - 1].is_alphanumeric() => 1,
        Some(_) => 2,
    }
}

/// The query folded the way names are, spaces dropped: what `rank` takes.
pub(crate) fn needle(query: &str) -> Vec<char> {
    query.chars().filter(|ch| !ch.is_whitespace()).map(fold).collect()
}

/// Open Quickly's ranking for one `name` (any text, not only a file): its tier (lower is better, see `tier`), its length in
/// characters and the matched characters; `None` when `needle` doesn't match.
pub(crate) fn rank(needle: &[char], name: &str, folded: &mut Vec<char>) -> Option<(u8, u32, Vec<usize>)> {
    let at = positions(needle, name, folded)?;
    Some((tier(&at, folded), folded.len() as u32, at))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Index, Node, touched_dir};

    fn tree(name: &str, files: &[&str]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("finch-filter-{name}-{}", std::process::id()));
        std::fs::remove_dir_all(&root).ok();
        for file in files {
            let path = root.join(file);
            if file.ends_with('/') {
                std::fs::create_dir_all(&path).unwrap();
            } else {
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "x").unwrap();
            }
        }
        root
    }

    fn read(root: &Path) -> Index {
        Index::read(root, false, &|| false).unwrap()
    }

    fn names(nodes: &[Node], out: &mut Vec<String>) {
        for node in nodes {
            out.push(node.path.to_string_lossy().into_owned());
            names(&node.kids, out);
        }
    }

    #[test]
    fn a_change_inside_an_indexed_folder_is_seen() {
        let root = tree("holds", &["a/b/c.txt", "a.b/x.txt", "a/d/", "e.txt", ".git/config", "z/deep/er/f.rs"]);
        let index = read(&root);
        let touched = |path: &str| index.holds_dir(&root, touched_dir(&root.join(path), &root));
        // A file's change belongs to its folder; the root's own change to the root.
        assert!(index.holds_dir(&root, touched_dir(&root, &root)));
        assert!(touched("e.txt"));
        assert!(touched("a/b/c.txt"));
        assert!(touched("a/b/new.txt"));
        assert!(touched("a/d/new.txt"));
        assert!(touched("a.b/y.txt"));
        assert!(touched("z/deep/er/g.rs"));
        // Folders the index doesn't hold: ignored, missing, or below a file.
        assert!(!touched(".git/HEAD"));
        assert!(!touched("a/nope/f.txt"));
        assert!(!touched("a/b/c.txt/under"));
        assert!(!touched("z/deep/other/f.rs"));
        assert!(!index.holds_dir(&root, Path::new("/elsewhere")));
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn the_index_keeps_every_parent_and_the_search_finds_it() {
        let root = tree("search", &["a/b/needle.txt", "a.b/other.txt", "a/d/", "needle/inner.txt"]);
        let index = read(&root);
        assert_eq!(index.entries.len(), 8);
        let found = index.search(&root, "needle", usize::MAX, &|| false).unwrap();
        assert_eq!((found.shown, found.total), (2, 2));
        let mut listed = Vec::new();
        names(&found.nodes, &mut listed);
        let below = |path: &str| root.join(path).to_string_lossy().into_owned();
        assert_eq!(listed, [below("a"), below("a/b"), below("a/b/needle.txt"), below("needle")]);
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_search_stops_building_at_the_cap_and_counts_every_match() {
        let root = tree("cap", &["a/e1.txt", "a/e2.txt", "b/e3.txt", "e4.txt", "e5.txt", "zzz.md"]);
        let index = read(&root);
        let found = index.search(&root, "e", 2, &|| false).unwrap();
        // Two entries show (with the folders above them), all matches count: five files and no folder.
        assert_eq!((found.shown, found.total), (2, 5));
        let mut listed = Vec::new();
        names(&found.nodes, &mut listed);
        assert_eq!(listed.iter().filter(|path| path.ends_with(".txt")).count(), 2);
        assert_eq!(super::capped_note(1_500, 80_214), "1,500 of 80,214 shown");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn only_a_run_is_not_scattered() {
        assert!(!super::scattered(&[]));
        assert!(!super::scattered(&[4, 5, 6]));
        assert!(super::scattered(&[0, 7, 12]));
    }

    #[test]
    fn the_walk_stops_at_the_cap_and_builds_only_what_it_keeps() {
        let files: Vec<String> = (0..8).flat_map(|dir| (0..50).map(move |file| format!("d{dir}/f{file}.txt"))).collect();
        let names: Vec<&str> = files.iter().map(String::as_str).collect();
        let root = tree("walkcap", &names);
        // 400 files and 8 folders under the root.
        let (whole, whole_allocs) = crate::test_alloc::allocations(|| Index::read_capped(&root, false, usize::MAX, &|| false).unwrap());
        assert_eq!((whole.len(), whole.capped()), (408, false));
        let (cut, cut_allocs) = crate::test_alloc::allocations(|| Index::read_capped(&root, false, 100, &|| false).unwrap());
        assert_eq!((cut.len(), cut.capped()), (100, true));
        // The thread that sorts and builds the entries allocates for what it keeps (a name each, about 160 more for the walk's
        // own setup), not for the whole tree: 273 against 622 when written.
        assert!(cut_allocs < 100 + 250 && cut_allocs + 250 < whole_allocs, "cut {cut_allocs} allocations, whole {whole_allocs}");
        // Exactly as many entries as the cap is not a cut.
        assert!(!Index::read_capped(&root, false, 408, &|| false).unwrap().capped());
        // Open Quickly lists from what is kept, and every file it lists has its folder in the index.
        let picks = cut.files(&root, "f1", 10, &|| false).unwrap();
        assert!(!picks.is_empty() && picks.iter().all(|pick| cut.has_entry(&root, &pick.path)));
        assert_eq!(super::index_cut_note(), "Showing the first 200,000 files. Open a smaller folder for the rest.");
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn open_quickly_lists_files_best_match_first() {
        let root = tree("files", &["deep/er/main.rs", "main.rs", "src/domain.rs", "my_main.txt", "m_a_i_n.md", "main/", "notes.txt"]);
        let index = read(&root);
        let picks = index.files(&root, "main", 10, &|| false).unwrap();
        let listed: Vec<(&str, &str)> = picks.iter().map(|pick| (pick.name.as_ref(), pick.folder.as_ref())).collect();
        // Folders never list; a name starting with the query first (shallower first), then at a word start, then inside a word, then spread out.
        assert_eq!(listed, [("main.rs", ""), ("main.rs", "deep/er"), ("my_main.txt", ""), ("domain.rs", "src"), ("m_a_i_n.md", "")]);
        assert_eq!(picks[1].path, root.join("deep/er/main.rs"));
        assert_eq!(index.files(&root, "main", 2, &|| false).unwrap().len(), 2);
        std::fs::remove_dir_all(&root).ok();
    }
}
