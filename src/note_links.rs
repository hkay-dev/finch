//! The note index behind typing `[[` in a Markdown document (finch#z5z0): the Markdown files under
//! the window's tree, ranked the way Open Quickly ranks names, each with its first heading.
//!
//! Nothing runs until the first `[[` or the first Open Quickly (finch#0qtf). That one asks
//! `Hub::index` for the tree's files: a walk on the background executor through
//! `filter::Index::read`, shared by every `[[` and every Open Quickly that comes while it runs
//! and kept as one `Arc<Index>` per root (at most `MAX_ROOTS`).
//! The Hub drops it all after `IDLE` without a `[[` or an Open Quickly (`IDLE_BIG` for a huge
//! tree), drops a root its window left, and drops a root when the explorer reports a change that
//! could add, remove or rename a note (`FilesChanged`). The next ask walks again, once.
//!
//! Matching and the heading reads run on the background executor too. A heading is the first few
//! KB of the file (`HEAD_BYTES`), read only for the rows that are shown and cached by path and
//! modified time. The editor side (the trigger, the menu rows, the insert) is in `editor`, the
//! reading of the text around the caret in `scan`.

pub(crate) mod editor;
mod scan;

use std::{
    collections::HashMap,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, SystemTime},
};

use futures_channel::oneshot;
use gpui_kit::*;

use crate::explorer::{Explorer, FilesChanged, filter::Index};

/// Roots whose index is held at once; the oldest one goes when another comes.
const MAX_ROOTS: usize = 2;
/// How long the indexes are held without a `[[`.
const IDLE: Duration = Duration::from_secs(90);
/// The same for a tree of more than `BIG_ENTRIES` files and folders, whose index is tens of MB.
const IDLE_BIG: Duration = Duration::from_secs(15);
const BIG_ENTRIES: usize = 200_000;
/// How much of a file is read to find its first heading.
const HEAD_BYTES: u64 = 4096;
/// Headings held by path; the whole cache goes when it is full.
const HEADINGS_CAP: usize = 2000;
/// A heading is cut here, so one odd line never fills the menu.
const TITLE_CHARS: usize = 160;

/// One note the menu offers.
pub(crate) struct Note {
    pub path: PathBuf,
    /// The file's name, with its extension.
    pub name: String,
    /// The folder below the tree's root; empty for the root itself.
    pub folder: String,
    /// The first heading, when the file has one.
    pub heading: Option<String>,
}

/// What one `[[` asks for.
pub(crate) struct Ask {
    pub root: PathBuf,
    pub show_hidden: bool,
    pub window: WindowId,
    pub explorer: Entity<Explorer>,
    pub query: String,
    /// The file being edited, which is never offered to itself.
    pub current: PathBuf,
}

/// Whether `name` is a Markdown file, by the editor's own name rule.
pub(crate) fn is_markdown(name: &str) -> bool {
    crate::language::for_path(Some(Path::new(name))) == "markdown"
}

/// The best `rows` notes for `ask.query` (an empty query lists the shortest names), with their
/// headings. Starts the walk if no index is held. `latest` says a newer request came when it
/// no longer holds `number`. `None` when there is no answer: the walk failed or a newer request
/// took over.
pub(crate) fn find(ask: Ask, rows: usize, latest: Arc<AtomicU64>, number: u64, cx: &mut App) -> Task<Option<Vec<Note>>> {
    let (index, headings) = index(&ask, cx);
    let Ask { root, query, current, .. } = ask;
    cx.spawn(async move |cx| {
        let index = index.await.ok().flatten()?;
        cx.background_executor()
            .spawn(async move {
                let stale = || latest.load(Relaxed) != number;
                let picks = index.files_where(&root, &query, rows + 1, &stale, &is_markdown, true)?;
                let mut notes = Vec::with_capacity(rows);
                for pick in picks.into_iter().filter(|pick| pick.path != current).take(rows) {
                    if stale() {
                        return None;
                    }
                    let heading = heading_of(&pick.path, &headings);
                    notes.push(Note { name: pick.name.to_string(), folder: pick.folder.to_string(), path: pick.path, heading });
                }
                Some(notes)
            })
            .await
    })
}

/// The index for the window's tree for Open Quickly: the one the Hub holds for that root, or
/// the walk it starts (which then stays held for the next ask). `None` when `window` has no
/// workspace on `root`, so the caller reads for itself.
pub(crate) fn share(root: &Path, show_hidden: bool, window: &Window, cx: &mut App) -> Option<oneshot::Receiver<Option<Arc<Index>>>> {
    let workspace = window.root::<gpui_kit::base::Root>().flatten()?.read(cx).view().clone().downcast::<crate::workspace::Workspace>().ok()?;
    let explorer = workspace.read(cx).explorer().clone();
    if explorer.read(cx).root() != root {
        return None;
    }
    let ask = Ask { root: root.to_path_buf(), show_hidden, window: window.window_handle().window_id(), explorer, query: String::new(), current: PathBuf::new() };
    Some(index(&ask, cx).0)
}

type Headings = Arc<Mutex<HashMap<PathBuf, (Option<SystemTime>, Option<String>)>>>;

/// The first heading of the file at `path`: from the cache while its modified time holds, else
/// from the first `HEAD_BYTES` of the file.
fn heading_of(path: &Path, cache: &Headings) -> Option<String> {
    let modified = std::fs::metadata(path).and_then(|meta| meta.modified()).ok();
    if let Ok(cache) = cache.lock()
        && let Some((seen, heading)) = cache.get(path)
        && *seen == modified
    {
        return heading.clone();
    }
    let heading = read_head(path).and_then(|(text, cut)| first_heading(&text, cut));
    if let Ok(mut cache) = cache.lock() {
        if cache.len() >= HEADINGS_CAP {
            cache.clear();
        }
        cache.insert(path.to_path_buf(), (modified, heading.clone()));
    }
    heading
}

/// The start of the file as text, and whether the file goes on past it.
fn read_head(path: &Path) -> Option<(String, bool)> {
    let mut bytes = Vec::with_capacity(HEAD_BYTES as usize);
    File::open(path).ok()?.take(HEAD_BYTES + 1).read_to_end(&mut bytes).ok()?;
    let cut = bytes.len() as u64 > HEAD_BYTES;
    bytes.truncate(HEAD_BYTES as usize);
    // A character cut in half at the end is dropped by the lossy read's replacement mark below.
    Some((String::from_utf8_lossy(&bytes).into_owned(), cut))
}

/// The text of the first heading in `text`, the start of a Markdown file: an `# ATX` line or a
/// line over `===` or `---`, outside front matter and fenced code. `cut` says the text ends
/// mid-file, so its last line may be half of one.
pub(crate) fn first_heading(text: &str, cut: bool) -> Option<String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut lines: Vec<&str> = text.lines().collect();
    if cut && !text.ends_with('\n') {
        lines.pop();
    }
    let mut lines = lines.into_iter();
    let mut paragraph: Option<&str> = None;
    let mut fence: Option<(char, usize)> = None;
    let mut first = true;
    while let Some(line) = lines.next() {
        if first && line.trim_end() == "---" {
            // Front matter: nothing counts until its closing line.
            first = false;
            if !lines.by_ref().any(|line| matches!(line.trim_end(), "---" | "...")) {
                return None;
            }
            continue;
        }
        first = false;
        let indent = line.len() - line.trim_start_matches(' ').len();
        let body = line.trim_start_matches(' ').trim_end();
        if let Some((mark, run)) = fence {
            let closing = body.chars().take_while(|&ch| ch == mark).count();
            if indent < 4 && closing >= run && body[closing..].trim().is_empty() {
                fence = None;
            }
            continue;
        }
        if indent < 4 {
            for mark in ['`', '~'] {
                let run = body.chars().take_while(|&ch| ch == mark).count();
                if run >= 3 && !(mark == '`' && body[run..].contains('`')) {
                    fence = Some((mark, run));
                }
            }
            if fence.is_some() {
                paragraph = None;
                continue;
            }
        }
        if body.is_empty() {
            paragraph = None;
            continue;
        }
        if indent < 4 {
            let hashes = body.chars().take_while(|&ch| ch == '#').count();
            if (1..=6).contains(&hashes) && (body.len() == hashes || body[hashes..].starts_with([' ', '\t'])) {
                let title = atx_title(&body[hashes..]);
                if !title.is_empty() {
                    return Some(clip(title));
                }
                paragraph = None;
                continue;
            }
            if body.chars().all(|ch| ch == '=') || body.chars().all(|ch| ch == '-') {
                match paragraph {
                    Some(title) => return Some(clip(title.trim())),
                    None => continue,
                }
            }
        }
        paragraph = Some(body);
    }
    None
}

/// An ATX heading's text: trimmed, without its closing `###`.
fn atx_title(rest: &str) -> &str {
    let rest = rest.trim();
    let bare = rest.trim_end_matches('#');
    if bare.len() == rest.len() {
        rest
    } else if bare.is_empty() || bare.ends_with([' ', '\t']) {
        bare.trim_end()
    } else {
        rest
    }
}

fn clip(title: &str) -> String {
    title.chars().take(TITLE_CHARS).collect()
}

/// `to` as a path from the folder `from`: `../notes/idea.md`, with any ASCII character that
/// would end or split a link target as `%XX` (the preview decodes it back), so a space is `%20`.
pub(crate) fn relative_link(from: &Path, to: &Path) -> String {
    let (from, to): (Vec<Component>, Vec<Component>) = (from.components().collect(), to.components().collect());
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = from[common..].iter().map(|_| "..".to_owned()).collect();
    parts.extend(to[common..].iter().map(|part| encode(&part.as_os_str().to_string_lossy())));
    parts.join("/")
}

/// A path segment with every ASCII character but letters, digits and `-_.~` as `%XX`.
fn encode(part: &str) -> String {
    let mut out = String::with_capacity(part.len());
    for ch in part.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') || !ch.is_ascii() {
            out.push(ch);
        } else {
            out.push_str(&format!("%{:02X}", ch as u8));
        }
    }
    out
}

/// A Markdown link to `to` from a file in `from`, titled `title`, which may hold brackets.
pub(crate) fn link(title: &str, from: &Path, to: &Path) -> String {
    let title: String = title.chars().flat_map(|ch| if matches!(ch, '[' | ']') { vec!['\\', ch] } else { vec![ch] }).collect();
    format!("[{title}]({})", relative_link(from, to))
}

/// The words a link to the note at `path` shows: its first heading, else the file's name
/// without its extension.
pub(crate) fn title_of(heading: Option<&str>, path: &Path) -> String {
    heading.map(str::to_owned).unwrap_or_else(|| path.file_stem().map_or_else(String::new, |stem| stem.to_string_lossy().into_owned()))
}

// The Hub: what is held between one `[[` and the next.

enum State {
    Cold,
    /// The walk is running; these `[[` wait for it.
    Reading { waiters: Vec<oneshot::Sender<Option<Arc<Index>>>>, _walk: Task<()> },
    Ready(Arc<Index>),
}

struct Slot {
    root: PathBuf,
    show_hidden: bool,
    /// Bumped by each walk start and by each invalidation; a walk under an older number quits.
    generation: Arc<AtomicU64>,
    state: State,
    big: bool,
    /// The explorers whose `FilesChanged` drops this index.
    watching: Vec<(EntityId, Subscription)>,
}

impl Drop for Slot {
    /// A walk still running for a dropped slot quits.
    fn drop(&mut self) {
        self.generation.fetch_add(1, Relaxed);
    }
}

#[derive(Default)]
struct Hub {
    /// Oldest first.
    slots: Vec<Slot>,
    /// The root each window asked with last.
    owners: Vec<(WindowId, PathBuf)>,
    /// Per window in `owners`: forgets it when its explorer (the window's) is released.
    closing: Vec<(WindowId, Subscription)>,
    headings: Headings,
    idle: Option<Task<()>>,
}

impl Global for Hub {}

/// The index for `ask`'s root, on its way or ready, and the heading cache.
fn index(ask: &Ask, cx: &mut App) -> (oneshot::Receiver<Option<Arc<Index>>>, Headings) {
    if !cx.has_global::<Hub>() {
        cx.set_global(Hub::default());
    }
    cx.update_global::<Hub, _>(|hub, cx| {
        let (sender, receiver) = oneshot::channel();
        let new_owner = hub.leave_old_root(ask);
        if new_owner {
            let window = ask.window;
            let release = cx.observe_release(&ask.explorer, move |_, cx| forget(window, cx));
            hub.closing.push((window, release));
        }
        let at = hub.slot(ask, cx);
        let waiting = match &mut hub.slots[at].state {
            State::Ready(index) => {
                sender.send(Some(index.clone())).ok();
                None
            }
            State::Reading { waiters, .. } => {
                waiters.push(sender);
                None
            }
            State::Cold => Some(sender),
        };
        if let Some(sender) = waiting {
            hub.read(at, vec![sender], cx);
        }
        hub.arm_idle(cx);
        (receiver, hub.headings.clone())
    })
}

impl Hub {
    /// Lets go of the root `ask`'s window used before, when it moved to another and no other
    /// window is on it. True when the window was not an owner yet.
    fn leave_old_root(&mut self, ask: &Ask) -> bool {
        let previous = match self.owners.iter_mut().find(|(window, _)| *window == ask.window) {
            Some((_, root)) => std::mem::replace(root, ask.root.clone()),
            None => {
                self.owners.push((ask.window, ask.root.clone()));
                return true;
            }
        };
        if previous != ask.root && !self.owners.iter().any(|(_, root)| *root == previous) {
            self.slots.retain(|slot| slot.root != previous);
        }
        false
    }

    /// A window closed: its owner entry goes, and so does every slot no other window is on.
    /// The heading cache goes with the last window.
    fn forget(&mut self, window: WindowId) {
        self.closing.retain(|(closing, _)| *closing != window);
        let Some(at) = self.owners.iter().position(|(owner, _)| *owner == window) else { return };
        let (_, root) = self.owners.remove(at);
        if !self.owners.iter().any(|(_, other)| *other == root) {
            self.slots.retain(|slot| slot.root != root);
        }
        if self.owners.is_empty() {
            self.headings.lock().map(|mut headings| headings.clear()).ok();
        }
    }

    /// The slot for `ask`'s root, made if there is none, and watching `ask`'s explorer.
    fn slot(&mut self, ask: &Ask, cx: &mut App) -> usize {
        let at = match self.slots.iter().position(|slot| slot.root == ask.root && slot.show_hidden == ask.show_hidden) {
            Some(at) => at,
            None => {
                if self.slots.len() >= MAX_ROOTS {
                    self.slots.remove(0);
                }
                self.slots.push(Slot {
                    root: ask.root.clone(),
                    show_hidden: ask.show_hidden,
                    generation: Arc::default(),
                    state: State::Cold,
                    big: false,
                    watching: Vec::new(),
                });
                self.slots.len() - 1
            }
        };
        let slot = &mut self.slots[at];
        let id = ask.explorer.entity_id();
        if !slot.watching.iter().any(|(watched, _)| *watched == id) {
            let (root, show_hidden) = (ask.root.clone(), ask.show_hidden);
            let subscription = cx.subscribe(&ask.explorer, move |explorer, FilesChanged(paths): &FilesChanged, cx: &mut App| {
                cx.update_global::<Hub, _>(|hub, cx| {
                    let ignored = |path: &Path, is_dir: bool| explorer.read(cx).git_ignored(path, is_dir);
                    if paths.iter().any(|path| listable(&root, show_hidden, path, &ignored) && hub.changes_notes(&root, show_hidden, path)) {
                        hub.invalidate(&root, show_hidden, cx);
                    }
                });
            });
            slot.watching.push((id, subscription));
        }
        at
    }

    /// Walks slot `at`'s root in the background; `waiters` get the index when it is read.
    fn read(&mut self, at: usize, waiters: Vec<oneshot::Sender<Option<Arc<Index>>>>, cx: &mut App) {
        let slot = &mut self.slots[at];
        let number = slot.generation.fetch_add(1, Relaxed) + 1;
        let (root, show_hidden, generation) = (slot.root.clone(), slot.show_hidden, slot.generation.clone());
        let task = cx.spawn(async move |cx| {
            let (walked_root, quit) = (root.clone(), generation.clone());
            let read = cx.background_executor().spawn(async move { Index::read(&walked_root, show_hidden, &|| quit.load(Relaxed) != number) }).await;
            cx.update(|cx| {
                cx.update_global::<Hub, _>(|hub, cx| hub.finished(&root, show_hidden, number, read.map(Arc::new), cx));
            });
        });
        slot.state = State::Reading { waiters, _walk: task };
    }

    fn finished(&mut self, root: &Path, show_hidden: bool, number: u64, read: Option<Arc<Index>>, cx: &mut App) {
        let Some(at) = self.slots.iter().position(|slot| slot.root == root && slot.show_hidden == show_hidden) else { return };
        let slot = &mut self.slots[at];
        if slot.generation.load(Relaxed) != number {
            return;
        }
        let State::Reading { waiters, .. } = std::mem::replace(&mut slot.state, State::Cold) else { return };
        if let Some(index) = read {
            slot.big = index.capped() || index.len() > BIG_ENTRIES;
            slot.state = State::Ready(index.clone());
            for waiter in waiters {
                waiter.send(Some(index.clone())).ok();
            }
            self.arm_idle(cx);
        } else {
            for waiter in waiters {
                waiter.send(None).ok();
            }
        }
    }

    /// Whether a change at `path` makes the held index wrong. With an index held, a file only
    /// counts when it now exists differently from what the index says (a save of a file that is
    /// already listed changes nothing). Any kind of file counts: Open Quickly lists them all.
    /// A capped index never lists the files past its cap, so one that exists unlisted is no change.
    fn changes_notes(&self, root: &Path, show_hidden: bool, path: &Path) -> bool {
        let index = self.slots.iter().find(|slot| slot.root == root && slot.show_hidden == show_hidden).and_then(|slot| match &slot.state {
            State::Ready(index) => Some(index),
            _ => None,
        });
        match (index, path.file_name()) {
            (Some(index), Some(_)) if path.exists() => !index.capped() && !index.has_entry(root, path),
            (Some(index), Some(_)) => index.has_entry(root, path),
            _ => true,
        }
    }

    /// A change under `root` that could add, remove or rename a note: the held index is out of
    /// date. A walk under way starts over, still for the `[[` waiting on it.
    fn invalidate(&mut self, root: &Path, show_hidden: bool, cx: &mut App) {
        let Some(at) = self.slots.iter().position(|slot| slot.root == root && slot.show_hidden == show_hidden) else { return };
        let slot = &mut self.slots[at];
        slot.generation.fetch_add(1, Relaxed);
        match std::mem::replace(&mut slot.state, State::Cold) {
            State::Reading { waiters, .. } => self.read(at, waiters, cx),
            State::Ready(_) | State::Cold => {}
        }
    }

    /// Drops everything held `IDLE` after the last `[[`.
    fn arm_idle(&mut self, cx: &mut App) {
        let wait = if self.slots.iter().any(|slot| slot.big) { IDLE_BIG } else { IDLE };
        self.idle = Some(cx.spawn(async move |cx| {
            cx.background_executor().timer(wait).await;
            cx.update(|cx| {
                cx.update_global::<Hub, _>(|hub, _| {
                    hub.slots.clear();
                    hub.owners.clear();
                    hub.closing.clear();
                    hub.headings.lock().map(|mut headings| headings.clear()).ok();
                    if let Some(done) = hub.idle.take() {
                        done.detach();
                    }
                });
            });
        }));
    }
}

/// Lets the index go of what a closed window held (the slot for its root, unless another
/// window is on that root) instead of waiting for the idle timer.
pub(crate) fn forget(window: WindowId, cx: &mut App) {
    if cx.has_global::<Hub>() {
        cx.update_global::<Hub, _>(|hub, _| hub.forget(window));
    }
}

/// Whether a change at `path` is one the index could list: under `root`, with no dot component
/// unless `show_hidden`, and not ruled out by a `.gitignore` (`ignored` says). Build output in
/// `target/` or `node_modules/` is never listed, so it never makes the index stale.
fn listable(root: &Path, show_hidden: bool, path: &Path, ignored: &dyn Fn(&Path, bool) -> bool) -> bool {
    let Ok(below) = path.strip_prefix(root) else { return false };
    if !show_hidden && below.components().any(|part| part.as_os_str().to_string_lossy().starts_with('.')) {
        return false;
    }
    // A file rule and a folder rule both rule a path out; only a folder rule needs to know it is one, so a build batch costs no stat calls.
    !ignored(path, false) && !(ignored(path, true) && path.is_dir())
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        sync::Arc,
    };

    use gpui_kit::WindowId;

    use crate::explorer::filter::Index;

    use super::{Hub, Slot, State, first_heading, link, listable, relative_link, title_of};

    fn slot(root: &str) -> Slot {
        Slot { root: PathBuf::from(root), show_hidden: false, generation: Default::default(), state: State::Cold, big: false, watching: Vec::new() }
    }

    #[test]
    fn a_closed_window_keeps_the_index_another_window_uses() {
        let (first, second, third) = (WindowId::from(1), WindowId::from(2), WindowId::from(3));
        let mut hub = Hub::default();
        hub.slots = vec![slot("/w/a"), slot("/w/b")];
        hub.owners = vec![(first, "/w/a".into()), (second, "/w/a".into()), (third, "/w/b".into())];
        hub.headings.lock().unwrap().insert("/w/a/x.md".into(), (None, Some("X".into())));
        let roots = |hub: &Hub| hub.slots.iter().map(|slot| slot.root.clone()).collect::<Vec<_>>();

        hub.forget(first);
        assert_eq!(roots(&hub), [PathBuf::from("/w/a"), PathBuf::from("/w/b")], "the second window still types [[ on /w/a");
        assert_eq!(hub.owners.len(), 2);

        hub.forget(second);
        assert_eq!(roots(&hub), [PathBuf::from("/w/b")], "no window is on /w/a now");
        assert!(!hub.headings.lock().unwrap().is_empty(), "a window is still open");

        hub.forget(third);
        assert!(hub.slots.is_empty() && hub.owners.is_empty());
        assert!(hub.headings.lock().unwrap().is_empty());
        hub.forget(third);
    }

    fn head(text: &str) -> Option<String> {
        first_heading(text, false)
    }

    #[test]
    fn finds_the_first_atx_heading() {
        assert_eq!(head("# Plans ##\n\ntext\n# Later"), Some("Plans".into()));
        assert_eq!(head("intro line\n\n### Third level"), Some("Third level".into()));
        assert_eq!(head("#hashtag\n# Real"), Some("Real".into()));
        assert_eq!(head("# C#"), Some("C#".into()));
    }

    #[test]
    fn finds_a_setext_heading() {
        assert_eq!(head("Big idea\n========\n"), Some("Big idea".into()));
        assert_eq!(head("Small idea\n---\n"), Some("Small idea".into()));
        assert_eq!(head("---\n\n----\n"), None);
    }

    #[test]
    fn skips_front_matter_and_code() {
        assert_eq!(head("---\ntitle: x\n# not this\n---\n# Real\n"), Some("Real".into()));
        assert_eq!(head("---\ntitle: x\n"), None);
        assert_eq!(head("```\n# comment\n```\n# After\n"), Some("After".into()));
        assert_eq!(head("~~~md\n# no\n"), None);
        assert_eq!(head("    # indented code\n# Yes"), Some("Yes".into()));
    }

    #[test]
    fn a_cut_line_is_not_a_heading() {
        assert_eq!(first_heading("intro\n# A long tit", true), None);
        assert_eq!(first_heading("# Whole\n# A long tit", true), Some("Whole".into()));
        assert_eq!(first_heading("# Whole\n", true), Some("Whole".into()));
    }

    #[test]
    fn relative_paths_climb_and_encode() {
        let (from, to) = (Path::new("/w/docs/guide"), Path::new("/w/docs/my notes/a(1).md"));
        assert_eq!(relative_link(from, to), "../my%20notes/a%281%29.md");
        assert_eq!(relative_link(Path::new("/w"), Path::new("/w/b.md")), "b.md");
        assert_eq!(relative_link(Path::new("/w/a/b"), Path::new("/w/c.md")), "../../c.md");
        assert_eq!(relative_link(Path::new("/w"), Path::new("/w/caf\u{e9}.md")), "caf\u{e9}.md");
    }

    #[test]
    fn links_escape_brackets_and_fall_back_to_the_stem() {
        assert_eq!(link("Notes [draft]", Path::new("/w"), Path::new("/w/n.md")), "[Notes \\[draft\\]](n.md)");
        assert_eq!(title_of(None, Path::new("/w/my idea.md")), "my idea");
        assert_eq!(title_of(Some("Heading"), Path::new("/w/x.md")), "Heading");
    }

    #[test]
    fn only_changes_the_index_does_not_know_drop_it() {
        let dir = std::env::temp_dir().join(format!("finch-0qtf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.md"), "x").unwrap();
        std::fs::write(dir.join("main.rs"), "x").unwrap();
        let index = Index::read(&dir, false, &|| false).unwrap();
        let mut hub = Hub::default();
        hub.slots = vec![slot(dir.to_str().unwrap())];
        hub.slots[0].state = State::Ready(Arc::new(index));
        let changes = |hub: &Hub, name: &str| hub.changes_notes(&dir, false, &dir.join(name));
        assert!(!changes(&hub, "a.md"), "a save of a listed note");
        assert!(!changes(&hub, "main.rs"), "a save of a listed file of any kind");
        std::fs::write(dir.join("new.rs"), "x").unwrap();
        assert!(changes(&hub, "new.rs"), "a file made after the read is one Open Quickly must find");
        std::fs::remove_file(dir.join("main.rs")).unwrap();
        assert!(changes(&hub, "main.rs"), "a listed file removed");
        hub.slots[0].state = State::Cold;
        assert!(changes(&hub, "a.md"), "with no index held, any change counts");
        std::fs::create_dir_all(dir.join("target")).unwrap();
        std::fs::write(dir.join("target/x.o"), "x").unwrap();
        let mut builder = ignore::gitignore::GitignoreBuilder::new(&dir);
        builder.add_line(None, "target/").unwrap();
        let rules = builder.build().unwrap();
        let ignored = |path: &Path, is_dir: bool| rules.matched_path_or_any_parents(path, is_dir).is_ignore();
        let listed = |name: &str, show_hidden: bool| listable(&dir, show_hidden, &dir.join(name), &ignored);
        assert!(hub.changes_notes(&dir, false, &dir.join("target/x.o")) && !listed("target/x.o", false), "build output the index never lists drops nothing");
        assert!(listed("new.rs", false), "a plain file is listable");
        assert!(!listed(".cache/y.txt", false) && listed(".cache/y.txt", true), "a hidden path is listable only with hidden files shown");
        assert!(!listed("target", false), "the ignored folder itself is not listable");
        std::fs::remove_dir_all(&dir).ok();
    }
}
