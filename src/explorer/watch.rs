//! Tells the explorer when files change under its root. FSEvents through the
//! `notify` crate; a thread of its own gathers the events of a burst (a save,
//! a checkout) into one batch: the window opens at the first event and shuts
//! `WINDOW` later, so a burst that never goes quiet still shows as it goes.
//!
//! Windows on one folder share one stream, one thread, one `git status` per change and one read of
//! each `.gitignore`: the `Feed` of a root is an entity each explorer holds, and it ends with the last of them.
//!
//! Folders reached through a symlink that points outside the root aren't under
//! the root stream, so `Targets` adds a watch for each such target while the
//! tree has the link loaded, and maps the events back to the link's path.

use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    path::{Component, Path, PathBuf},
    rc::Rc,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Task, WeakEntity};
use ignore::{Match, gitignore::Gitignore};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

use super::git;

/// How long a batch collects events, counted from its first one (Zed's 100 ms).
const WINDOW: Duration = Duration::from_millis(100);
/// How long a git read settles before the next may start, so a stream of file events (a save
/// loop, a build) costs one run per this long, not one per event.
const GIT_SETTLE: Duration = Duration::from_millis(400);

/// What happened in one window. The lists are shared (`Arc`), so every window showing the root
/// takes the batch without copying its paths: a build writing under `target/` sends thousands.
#[derive(Clone)]
pub struct Batch {
    /// The changed paths, sorted and without repeats.
    pub paths: Arc<[PathBuf]>,
    /// Paths where the system says events were dropped, so everything under
    /// them may have changed. Sorted; one inside another is left out.
    pub rescans: Arc<[PathBuf]>,
    /// A git dir's index or HEAD changed: a commit, checkout or stage. It is not in `paths`.
    pub git: bool,
    /// Its number among the root's batches, from 1 (the thread leaves 0 for the feed to fill).
    pub seq: u64,
}

/// A raw event: its paths, the paths it asks to rescan, and whether it touched git's index or HEAD.
type Raw = (Vec<PathBuf>, Vec<PathBuf>, bool);

/// The stream under one root, shared by every explorer showing it. Dropping this drops the
/// watcher, which closes the raw channel and ends the thread.
struct Core {
    /// The root's stream. Never restarted: a symlink target or git dir has a watcher of its own,
    /// because notify stops and restarts a stream on every watch or unwatch and loses events between.
    _watcher: RecommendedWatcher,
    /// Where the watchers of the targets and the git dir send their events.
    raw_tx: mpsc::Sender<Raw>,
    /// The root as the explorers name it.
    given: PathBuf,
    /// The root, resolved: the system reports events in these terms.
    root: PathBuf,
    /// Each symlink target some explorer follows, and how many links point at it.
    wanted: BTreeMap<PathBuf, usize>,
    /// The targets with a watcher of their own.
    watched: BTreeMap<PathBuf, RecommendedWatcher>,
    /// The git dir that holds the repo's index and HEAD, watched on its own when it lies outside the root.
    git_dir: Option<(PathBuf, RecommendedWatcher)>,
}

impl Core {
    /// Watches `dir`, the git dir of the repo holding the root, when it is outside the root (a
    /// root below the repo's top, or a linked worktree): its `index` and `HEAD` then report
    /// as git changes. Not recursive, so the objects below it stay quiet. `None` stops it.
    fn follow_git(&mut self, dir: Option<&Path>) {
        let dir = dir.filter(|dir| !dir.starts_with(&self.root));
        if self.git_dir.as_ref().map(|(held, _)| held.as_path()) == dir {
            return;
        }
        self.git_dir = None;
        if let Some(dir) = dir
            && let Some(mut watcher) = make_watcher(self.raw_tx.clone(), dir.to_path_buf())
            && watcher.watch(dir, RecursiveMode::NonRecursive).is_ok()
        {
            self.git_dir = Some((dir.to_path_buf(), watcher));
        }
    }

    /// One more link points at `target`.
    fn want(&mut self, target: &Path) {
        *self.wanted.entry(target.to_path_buf()).or_default() += 1;
        self.reconcile();
    }

    /// One link less points at `target`.
    fn release(&mut self, target: &Path) {
        if let Some(count) = self.wanted.get_mut(target) {
            *count -= 1;
            if *count == 0 {
                self.wanted.remove(target);
            }
        }
        self.reconcile();
    }

    /// Watches each target no watch covers yet, and drops the watches no link needs.
    fn reconcile(&mut self) {
        self.watched.retain(|target, _| self.wanted.contains_key(target));
        let missing: Vec<PathBuf> = self
            .wanted
            .keys()
            .filter(|target| !self.watched.keys().any(|watched| target.starts_with(watched)))
            .cloned()
            .collect();
        for target in missing {
            // A target that can't be watched is left out; its link just doesn't refresh.
            let Some(mut watcher) = make_watcher(self.raw_tx.clone(), target.clone()) else { continue };
            if watcher.watch(&target, RecursiveMode::Recursive).is_ok() {
                self.watched.insert(target, watcher);
            }
        }
    }
}

/// One explorer's share of a root's stream: the symlink targets it follows besides the root,
/// whose watches the stream keeps while any explorer follows them. Dropping it lets them go.
pub struct Targets {
    core: Rc<RefCell<Core>>,
    /// Each followed link in the tree and what it points at, resolved.
    links: BTreeMap<PathBuf, PathBuf>,
    /// The folder symlinks the tree's reads have seen, each with its resolved target.
    known: BTreeMap<PathBuf, PathBuf>,
}

impl Targets {
    /// Starts following `link`, a folder the tree just loaded, when it is a
    /// symlink to somewhere the root stream doesn't cover: not into the root,
    /// and not around it (a link up the tree would flood). What a folder links to
    /// is learned by the background read of its parent (`learn`), so this never touches the disk.
    pub fn follow(&mut self, link: &Path) {
        if self.links.contains_key(link) {
            return;
        }
        let Some(target) = self.known.get(link).cloned() else { return };
        {
            let mut core = self.core.borrow_mut();
            if target.starts_with(&core.root) || core.root.starts_with(&target) {
                return;
            }
            core.want(&target);
        }
        self.links.insert(link.to_path_buf(), target);
    }

    /// What the read of `dir` found: the resolved target of each of its children that is a
    /// folder symlink. Replaces what an earlier read of `dir` found.
    pub fn learn(&mut self, dir: &Path, found: Vec<(PathBuf, PathBuf)>) {
        self.known.retain(|link, _| link.parent() != Some(dir));
        self.known.extend(found);
    }

    /// Stops following the links `keep` refuses (the folder was unloaded or is gone).
    pub fn retain(&mut self, keep: impl Fn(&Path) -> bool) {
        let mut core = self.core.borrow_mut();
        self.links.retain(|link, target| {
            let kept = keep(link);
            if !kept {
                core.release(target);
            }
            kept
        });
    }

    /// `paths` as this explorer sees them: the ones inside a followed target given as the link's
    /// path (one per link that points there), the ones under the root as they are, and nothing
    /// else (the stream is shared, so the paths of another explorer's targets arrive too).
    /// When every path is under a root the system names as the explorers do, they are all kept
    /// as they are and the list comes back shared, not copied.
    pub fn map(&self, paths: &Arc<[PathBuf]>) -> Arc<[PathBuf]> {
        let core = self.core.borrow();
        if core.root == core.given && paths.iter().all(|path| path.starts_with(&core.root)) {
            return paths.clone();
        }
        let mut out = BTreeSet::new();
        for path in paths.iter() {
            // The system reports real paths; the tree and its tabs use the root as given.
            if let Ok(rest) = path.strip_prefix(&core.root) {
                if core.root != core.given {
                    // A tab held by its real path still matches.
                    out.insert(path.to_path_buf());
                }
                out.insert(if rest.as_os_str().is_empty() { core.given.clone() } else { core.given.join(rest) });
                continue;
            }
            if path.starts_with(&core.given) {
                out.insert(path.to_path_buf());
                continue;
            }
            for (link, target) in &self.links {
                if let Ok(rest) = path.strip_prefix(target) {
                    out.insert(if rest.as_os_str().is_empty() { link.clone() } else { link.join(rest) });
                }
            }
        }
        out.into_iter().collect()
    }
}

impl Drop for Targets {
    fn drop(&mut self) {
        let mut core = self.core.borrow_mut();
        for target in self.links.values() {
            core.release(target);
        }
    }
}

thread_local! {
    /// The feed of each root some explorer shows, weakly: the last explorer to let go ends it.
    static FEEDS: RefCell<HashMap<PathBuf, WeakEntity<Feed>>> = RefCell::new(HashMap::new());
}

/// The `.gitignore` of each folder some window of a root has loaded (and of the folders above the
/// root), read once for all of them. They tell a batch of ignored paths from one that can change
/// git's answer, and the tree's deep open skip ignored folders.
#[derive(Default)]
pub struct Ignores {
    /// The `.gitignore` of each folder read so far that has one.
    found: BTreeMap<PathBuf, Gitignore>,
    /// The folders whose `.gitignore` is read or being read.
    asked: HashSet<PathBuf>,
    /// The last batch that changed a `.gitignore`, so one batch seen by many windows costs one read.
    changed_by: u64,
    /// How many windows have each folder loaded.
    held: HashMap<PathBuf, usize>,
}

impl Ignores {
    /// The folders of `dirs` not asked for yet, sorted, now counted as asked: the caller reads them.
    pub fn unasked<'a>(&mut self, dirs: impl Iterator<Item = &'a Path>) -> Vec<PathBuf> {
        let new: BTreeSet<PathBuf> = dirs.filter(|dir| !self.asked.contains(*dir)).map(Path::to_path_buf).collect();
        self.asked.extend(new.iter().cloned());
        new.into_iter().collect()
    }

    /// Marks the folders of the `.gitignore` files among `paths` unread, so the next `unasked`
    /// returns them. Batch `seq` counts once, however many windows pass it.
    pub fn changed(&mut self, seq: u64, paths: &[PathBuf]) {
        let dirs: Vec<&Path> = paths
            .iter()
            .filter(|path| path.file_name().is_some_and(|name| name == ".gitignore"))
            .filter_map(|path| path.parent())
            .collect();
        if dirs.is_empty() || (seq != 0 && self.changed_by == seq) {
            return;
        }
        self.changed_by = seq;
        for dir in dirs {
            self.asked.remove(dir);
        }
    }

    /// Takes what a read found. A folder without a `.gitignore` (or an empty one) holds nothing.
    pub fn apply(&mut self, read: Vec<(PathBuf, Gitignore)>) {
        for (dir, matcher) in read {
            if matcher.is_empty() {
                self.found.remove(&dir);
            } else {
                self.found.insert(dir, matcher);
            }
        }
    }

    /// One more window has `dir` loaded.
    pub fn hold(&mut self, dir: &Path) {
        *self.held.entry(dir.to_path_buf()).or_default() += 1;
    }

    /// A window let go of the loaded `dir` (shut and evicted, deleted, or the window ended). The
    /// last one to let go takes its matcher with it; the folders above `root` stay, as they are
    /// asked for on purpose.
    pub fn release(&mut self, dir: &Path, root: &Path) {
        if let Some(count) = self.held.get_mut(dir) {
            *count -= 1;
            if *count > 0 {
                return;
            }
            self.held.remove(dir);
        }
        if !root.ancestors().any(|above| above == dir) {
            self.found.remove(dir);
            self.asked.remove(dir);
        }
    }

    /// Whether the `.gitignore` files read so far rule `path` out: the deepest file with an
    /// opinion decides. A path whose folder's file isn't read yet counts as not ignored.
    pub fn ignored(&self, path: &Path, is_dir: bool) -> bool {
        for dir in path.ancestors().skip(1) {
            if let Some(matcher) = self.found.get(dir) {
                match matcher.matched_path_or_any_parents(path, is_dir) {
                    Match::Ignore(_) => return true,
                    Match::Whitelist(_) => return false,
                    Match::None => {}
                }
            }
        }
        false
    }
}

/// Reads the `.gitignore` of each of `dirs` (blocking: run it in the background).
pub fn read_ignores(dirs: Vec<PathBuf>) -> Vec<(PathBuf, Gitignore)> {
    dirs.into_iter().map(|dir| (dir.clone(), Gitignore::new(dir.join(".gitignore")).0)).collect()
}

/// What the explorers showing one root share: the watcher and its batches, and git's statuses
/// for the root, read once per change however many windows show it. Each explorer holds the
/// entity, takes the batches as events and the statuses on notify; the stream and its thread
/// end with the last one.
pub struct Feed {
    core: Rc<RefCell<Core>>,
    root: PathBuf,
    /// What git says about the files under the root.
    git: Rc<git::Statuses>,
    /// The `.gitignore` files of the root's folders, read once for every window showing it.
    ignores: Rc<RefCell<Ignores>>,
    /// A `git status` is running in the background.
    git_busy: bool,
    /// Something changed while it ran, so one more run follows it.
    git_again: bool,
    /// The last batch some explorer asked a read for, so one batch seen by many explorers costs one run.
    asked: u64,
    /// The number of the batch last sent.
    seq: u64,
    /// Turns the watcher's batches into events. Dropping it closes the channel, which ends the thread.
    _pump: Task<()>,
}

impl EventEmitter<Batch> for Feed {}

impl Drop for Feed {
    /// The last explorer on the root let go: its weak entry and git's remembered top go with it,
    /// unless a new feed for the same root has already taken the entry.
    fn drop(&mut self) {
        let live = FEEDS
            .try_with(|feeds| {
                let mut feeds = feeds.borrow_mut();
                let live = feeds.get(&self.root).is_some_and(|feed| feed.upgrade().is_some());
                if !live {
                    feeds.remove(&self.root);
                }
                live
            })
            .unwrap_or(true);
        if !live {
            git::forget(&self.root);
        }
    }
}

impl Feed {
    /// The feed for `root`, started if no explorer shows it yet, and this explorer's `Targets`.
    /// `None` when the stream can't start.
    pub fn join(root: &Path, cx: &mut App) -> Option<(Entity<Feed>, Targets)> {
        let live = FEEDS.with(|feeds| feeds.borrow().get(root).and_then(WeakEntity::upgrade));
        let feed = match live {
            Some(feed) => feed,
            None => {
                let (core, batches) = start(root)?;
                let feed = cx.new(|cx| Feed::new(core, root, batches, cx));
                FEEDS.with(|feeds| {
                    let mut feeds = feeds.borrow_mut();
                    feeds.retain(|_, feed| feed.upgrade().is_some());
                    feeds.insert(root.to_path_buf(), feed.downgrade());
                });
                feed
            }
        };
        let targets = Targets { core: feed.read(cx).core.clone(), links: BTreeMap::new(), known: BTreeMap::new() };
        Some((feed, targets))
    }

    fn new(core: Core, root: &Path, mut batches: UnboundedReceiver<Batch>, cx: &mut Context<Self>) -> Self {
        let pump = cx.spawn(async move |this, cx| {
            while let Ok(mut batch) = batches.recv().await {
                let sent = this.update(cx, |this, cx| {
                    this.seq += 1;
                    batch.seq = this.seq;
                    cx.emit(batch);
                });
                if sent.is_err() {
                    break;
                }
            }
        });
        let mut this = Self {
            core: Rc::new(RefCell::new(core)),
            root: root.to_path_buf(),
            git: Rc::default(),
            ignores: Rc::default(),
            git_busy: false,
            git_again: false,
            asked: 0,
            seq: 0,
            _pump: pump,
        };
        this.refresh_git(0, cx);
        this
    }

    /// What git says about the files under the root.
    pub fn git(&self) -> Rc<git::Statuses> {
        self.git.clone()
    }

    /// The root's `.gitignore` matchers, shared by its windows.
    pub fn ignores(&self) -> Rc<RefCell<Ignores>> {
        self.ignores.clone()
    }

    /// Reads the `.gitignore` of `dirs` (each handed out once by `Ignores::unasked`) in the background.
    pub fn read_ignores(&mut self, dirs: Vec<PathBuf>, cx: &mut Context<Self>) {
        if dirs.is_empty() {
            return;
        }
        cx.spawn(async move |this, cx| {
            let read = cx.background_executor().spawn(async move { read_ignores(dirs) }).await;
            this.update(cx, |this, _| this.ignores.borrow_mut().apply(read)).ok();
        })
        .detach();
    }

    /// Reads git's statuses in the background and holds them for the rows. One read runs at a
    /// time, and a read settles for `GIT_SETTLE` after it ends; a request made meanwhile queues
    /// one more run after that, which reads the state after all of them. A burst of file events
    /// so costs a run now and one when it quiets, not one per event. `batch` is the number of
    /// the batch that asks (0: nothing asks, the feed itself does): a second ask for the same
    /// batch, from another window, changes nothing.
    pub fn refresh_git(&mut self, batch: u64, cx: &mut Context<Self>) {
        if batch != 0 {
            if self.asked == batch {
                return;
            }
            self.asked = batch;
        }
        if self.git_busy {
            self.git_again = true;
            return;
        }
        self.git_busy = true;
        let root = self.root.clone();
        cx.spawn(async move |this, cx| {
            let found = cx.background_executor().spawn(async move { git::read(&root) }).await;
            let alive = this
                .update(cx, |this, cx| {
                    this.core.borrow_mut().follow_git(found.git_dir());
                    if *this.git != found {
                        this.git = Rc::new(found);
                        cx.notify();
                    }
                })
                .is_ok();
            if alive {
                cx.background_executor().timer(GIT_SETTLE).await;
            }
            this.update(cx, |this, cx| {
                this.git_busy = false;
                if std::mem::take(&mut this.git_again) {
                    this.refresh_git(0, cx);
                }
            })
            .ok();
        })
        .detach();
    }
}

/// Starts the stream on `root`: the watcher and the thread that batches its events. `None`
/// when the stream can't start.
fn start(root: &Path) -> Option<(Core, UnboundedReceiver<Batch>)> {
    let (raw_tx, raw_rx) = mpsc::channel::<Raw>();
    let mut watcher = make_watcher(raw_tx.clone(), root.to_path_buf())?;
    watcher.watch(root, RecursiveMode::Recursive).ok()?;
    let (tx, rx) = unbounded();
    std::thread::Builder::new()
        .name("explorer-watch".into())
        .spawn(move || batch(&raw_rx, &tx))
        .ok()?;
    let resolved = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let core = Core {
        _watcher: watcher,
        raw_tx,
        given: root.to_path_buf(),
        root: resolved,
        wanted: BTreeMap::new(),
        watched: BTreeMap::new(),
        git_dir: None,
    };
    Some((core, rx))
}

/// A watcher feeding `raw_tx`; `overflow` is what an overflow with no path asks to rescan (the
/// folder the watcher watches).
fn make_watcher(raw_tx: mpsc::Sender<Raw>, overflow: PathBuf) -> Option<RecommendedWatcher> {
    notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !matches!(event.kind, notify::EventKind::Access(_))
        {
            let rescan = event.need_rescan();
            let pathless = event.paths.is_empty();
            let git = event.paths.iter().any(|path| git_signal(path));
            let paths: Vec<PathBuf> = event
                .paths
                .into_iter()
                .filter(|path| !ignored(path))
                .collect();
            // An overflow with no path means anything under the watched folder may have changed.
            let (paths, rescans) = match (rescan, pathless) {
                (true, true) => (vec![overflow.clone()], vec![overflow.clone()]),
                (true, false) => (paths.clone(), paths),
                (false, _) => (paths, Vec::new()),
            };
            if !paths.is_empty() || git {
                let _ = raw_tx.send((paths, rescans, git));
            }
        }
    })
    .ok()
}

/// Gathers raw events for `WINDOW` from the first of a batch, then sends it.
/// Blocks while idle; ends when the watcher is gone or nobody listens.
fn batch(raw: &mpsc::Receiver<Raw>, out: &UnboundedSender<Batch>) {
    while let Ok((first, first_rescans, first_git)) = raw.recv() {
        let mut changed: BTreeSet<PathBuf> = first.into_iter().collect();
        let mut rescans: BTreeSet<PathBuf> = first_rescans.into_iter().collect();
        let mut git = first_git;
        let deadline = Instant::now() + WINDOW;
        // The window ends at its deadline even while events are still queued.
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let Ok((more, more_rescans, more_git)) = raw.recv_timeout(left) else { break };
            changed.extend(more);
            rescans.extend(more_rescans);
            git |= more_git;
        }
        let outermost = outermost(rescans);
        let batch = Batch { paths: changed.into_iter().collect(), rescans: outermost.into(), git, seq: 0 };
        if out.unbounded_send(batch).is_err() {
            return;
        }
    }
}

/// The paths of `sorted` with those inside another one left out. A parent
/// sorts before its children, so one pass keeps the outermost.
fn outermost(sorted: BTreeSet<PathBuf>) -> Vec<PathBuf> {
    let mut kept: Vec<PathBuf> = Vec::new();
    for path in sorted {
        if !kept.last().is_some_and(|last| path.starts_with(last)) {
            kept.push(path);
        }
    }
    kept
}

/// Paths the tree never shows: inside `.git`, and `.DS_Store`.
fn ignored(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == ".DS_Store")
        || path
            .components()
            .any(|part| matches!(part, Component::Normal(name) if name == ".git"))
}

/// The two files whose change means git's answer changed without a working-tree change: `index`
/// and `HEAD` of a `.git` dir or of a linked worktree's dir inside it (`.git/worktrees/<name>`).
/// The rest of `.git`'s churn (objects, logs, lock files) is never reported.
fn git_signal(path: &Path) -> bool {
    let named = |path: Option<&Path>, want: &str| path.and_then(Path::file_name).is_some_and(|name| name == want);
    if !path.file_name().is_some_and(|name| name == "index" || name == "HEAD") {
        return false;
    }
    let dir = path.parent();
    named(dir, ".git") || (dir.and_then(Path::parent).is_some_and(|above| named(Some(above), "worktrees")) && named(dir.and_then(Path::parent).and_then(Path::parent), ".git"))
}

#[cfg(test)]
mod tests {
    use ignore::gitignore::GitignoreBuilder;

    use super::*;

    fn matcher(dir: &Path) -> Gitignore {
        let mut builder = GitignoreBuilder::new(dir);
        builder.add_line(None, "target").unwrap();
        builder.build().unwrap()
    }

    /// One window: a folder opened then shut and evicted gives its matcher back. Two windows:
    /// the matcher stays until both have let go. The folder above the root stays.
    #[test]
    fn matcher_goes_with_the_last_window_that_has_its_folder() {
        let root = Path::new("/r/app");
        let (above, dir) = (Path::new("/r"), Path::new("/r/app/sub"));
        let mut ignores = Ignores::default();
        ignores.apply(vec![(above.to_path_buf(), matcher(above)), (dir.to_path_buf(), matcher(dir))]);
        assert_eq!(ignores.found.len(), 2);

        ignores.hold(dir);
        ignores.release(dir, root);
        assert_eq!(ignores.found.len(), 1);
        assert!(!ignores.asked.contains(dir));

        ignores.apply(vec![(dir.to_path_buf(), matcher(dir))]);
        ignores.hold(dir);
        ignores.hold(dir);
        ignores.release(dir, root);
        assert_eq!(ignores.found.len(), 2);
        ignores.release(dir, root);
        assert_eq!(ignores.found.len(), 1);

        ignores.release(above, root);
        assert_eq!(ignores.found.len(), 1);
    }
}

#[cfg(test)]
mod batch_tests {
    use super::*;

    /// Events that keep coming never hold a batch back past its window.
    #[test]
    fn batch_ends_at_its_deadline_during_a_burst() {
        let (raw_tx, raw_rx) = mpsc::channel::<Raw>();
        let (tx, mut rx) = unbounded();
        let pump = std::thread::spawn(move || batch(&raw_rx, &tx));
        let started = Instant::now();
        let feeder = std::thread::spawn(move || {
            while started.elapsed() < WINDOW * 4 {
                let _ = raw_tx.send((vec![PathBuf::from("/burst/file")], Vec::new(), false));
                std::thread::sleep(Duration::from_micros(200));
            }
        });
        let first = loop {
            if let Ok(batch) = rx.try_recv() {
                break batch;
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        let took = started.elapsed();
        assert_eq!(&*first.paths, [PathBuf::from("/burst/file")]);
        assert!(took < WINDOW * 3, "first batch took {took:?} of a {:?} burst", WINDOW * 4);
        feeder.join().unwrap();
        drop(rx);
        pump.join().unwrap();
    }

    /// A build writing under the root sends thousands of paths in a batch; each window showing
    /// the root takes it (the clone the subscription makes, then `Targets::map`) without copying one.
    #[test]
    fn a_window_takes_a_batch_without_copying_its_paths() {
        let dir = std::env::temp_dir().join(format!("finch-batch-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.canonicalize().unwrap();
        let (core, _batches) = start(&root).unwrap();
        let targets = Targets { core: Rc::new(RefCell::new(core)), links: BTreeMap::new(), known: BTreeMap::new() };

        let paths: Vec<PathBuf> = (0..2000).map(|n| root.join(format!("target/debug/deps/obj{n:04}.o"))).collect();
        let batch = Batch { paths: paths.clone().into(), rescans: Arc::from([]), git: false, seq: 1 };

        // What a window cost before: the batch cloned with its Vec, then mapped into a new list.
        let (_, old) = crate::test_alloc::allocations(|| paths.clone());
        assert!(old > 2000, "a Vec of 2000 paths cloned {old} allocations");

        let (mapped, new) = crate::test_alloc::allocations(|| {
            let taken = batch.clone();
            (targets.map(&taken.paths), targets.map(&taken.rescans))
        });
        eprintln!("allocations per window for a 2000-path batch: {new} now, {old} for the old clone alone");
        assert_eq!(new, 0, "a window took the batch with {new} allocations (was {old} for the clone alone)");
        assert!(Arc::ptr_eq(&mapped.0, &batch.paths));

        // Paths outside the root (another window's link target) still come out filtered.
        let mixed: Arc<[PathBuf]> = Arc::from([root.join("a"), PathBuf::from("/elsewhere/b")]);
        assert_eq!(&*targets.map(&mixed), [root.join("a")]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
