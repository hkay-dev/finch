//! What git says about the files under the explorer's root: one status per changed path, and
//! for each folder the strongest status inside it (Zed's folder summaries). It is read with
//! `git status --porcelain=v1 --untracked-files=normal --no-renames -z` on the background executor,
//! never in a draw. Not a repo, or no `git` binary: no statuses and nothing said. An untracked
//! folder is one entry (git lists it once), and everything under it reads as added.

use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Mutex,
};

/// A path's state against git. The order is the strength of a status: a folder takes the
/// greatest one inside it, and a row shows conflicted, then deleted, modified, added.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Status {
    /// New: untracked or added to the index (Zed's "created").
    Added,
    Modified,
    Deleted,
    Conflicted,
}

impl Status {
    /// The word `debug explorer rows` prints.
    pub fn name(self) -> &'static str {
        match self {
            Status::Added => "added",
            Status::Modified => "modified",
            Status::Deleted => "deleted",
            Status::Conflicted => "conflicted",
        }
    }

    /// The status of a porcelain `XY` pair, in Zed's order: conflicted, deleted, modified, added.
    fn of(x: u8, y: u8) -> Option<Status> {
        if x == b'U' || y == b'U' || (x, y) == (b'A', b'A') || (x, y) == (b'D', b'D') {
            Some(Status::Conflicted)
        } else if x == b'D' || y == b'D' {
            Some(Status::Deleted)
        } else if matches!(x, b'M' | b'T') || matches!(y, b'M' | b'T') {
            Some(Status::Modified)
        } else if x == b'A' || (x, y) == (b'?', b'?') {
            Some(Status::Added)
        } else {
            None
        }
    }
}

/// The statuses under a root, as absolute paths in terms of that root.
#[derive(Default, PartialEq, Eq)]
pub struct Statuses {
    /// Files with a status, and every folder between them and the root with the strongest one
    /// inside. The root itself isn't in it.
    by_path: HashMap<PathBuf, Status>,
    /// The untracked folders git listed whole. Their contents have no entry of their own.
    untracked_dirs: HashSet<PathBuf>,
    /// The root the paths are in terms of, where a walk up from a path stops.
    root: PathBuf,
    /// The repo's git dir, when the root is inside a repo.
    git_dir: Option<PathBuf>,
}

impl Statuses {
    /// The status of the file or folder at `path`: its own entry, else added when an untracked
    /// folder holds it. No allocation.
    pub fn get(&self, path: &Path) -> Option<Status> {
        if let Some(status) = self.by_path.get(path) {
            return Some(*status);
        }
        if self.untracked_dirs.is_empty() {
            return None;
        }
        for dir in path.ancestors().skip(1) {
            if dir == self.root || !dir.starts_with(&self.root) {
                break;
            }
            if self.untracked_dirs.contains(dir) {
                return Some(Status::Added);
            }
        }
        None
    }

    /// The git dir of the repo holding the root, `None` outside a repo.
    pub fn git_dir(&self) -> Option<&Path> {
        self.git_dir.as_deref()
    }

    /// Records an untracked folder as one entry, marking the folders above it as `set` does.
    fn set_untracked_dir(&mut self, root: &Path, path: PathBuf) {
        self.untracked_dirs.insert(path.clone());
        self.set(root, path, Status::Added);
    }

    fn set(&mut self, root: &Path, path: PathBuf, status: Status) {
        for dir in path.ancestors() {
            if dir == root || !dir.starts_with(root) {
                break;
            }
            match self.by_path.get_mut(dir) {
                Some(have) if *have >= status => break,
                Some(have) => *have = status,
                None => {
                    self.by_path.insert(dir.to_path_buf(), status);
                }
            }
        }
    }
}

/// Runs git in `root`. `None` when it can't (no binary, not a repo).
fn git(root: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .args(args)
        .current_dir(root)
        // A read must not take the index lock: it would race the user's own git commands and
        // rewrite the index, which the watcher reports as a change.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

/// What `rev-parse` said for each root that is in a repo: the top and the git dir. Neither moves
/// while the repo stays, so a run asks once per root; a failed status or a git dir that has gone
/// forgets the root, and a root that wasn't in a repo is asked again (it may have been `git init`ed).
/// The last window on a root forgets it too (`watch::Feed`'s drop), so a root nobody shows holds nothing.
static TOPS: Mutex<Option<HashMap<PathBuf, (PathBuf, PathBuf)>>> = Mutex::new(None);

pub fn forget(root: &Path) {
    if let Some(tops) = TOPS.lock().unwrap().as_mut() {
        tops.remove(root);
    }
}

/// The top and git dir of the repo holding `root`, asked of git only the first time.
fn tops(root: &Path) -> Option<(PathBuf, PathBuf)> {
    let known = TOPS.lock().unwrap().as_ref().and_then(|tops| tops.get(root).cloned());
    if let Some(known) = known.filter(|(_, git_dir)| git_dir.exists()) {
        return Some(known);
    }
    let out = git(root, &["rev-parse", "--show-toplevel", "--absolute-git-dir"])?;
    let mut lines = out.split(|&byte| byte == b'\n');
    let (Some(top), Some(git_dir)) = (lines.next(), lines.next()) else { return None };
    let found = (PathBuf::from(OsStr::from_bytes(top)), PathBuf::from(OsStr::from_bytes(git_dir)));
    TOPS.lock().unwrap().get_or_insert_default().insert(root.to_path_buf(), found.clone());
    Some(found)
}

/// Reads the statuses under `root`. Blocks on a git process (two the first time): call it off the main thread.
pub fn read(root: &Path) -> Statuses {
    let mut statuses = Statuses::default();
    let Some((top, git_dir)) = tops(root) else { return statuses };
    // The root as git sees it, to find where it sits below the top (`root` may go through a link).
    let Ok(real) = root.canonicalize() else { return statuses };
    let top = top.canonicalize().unwrap_or(top);
    let Ok(inside) = real.strip_prefix(&top) else { return statuses };
    statuses.git_dir = Some(git_dir);
    statuses.root = root.to_path_buf();
    let Some(out) = git(root, &["status", "--porcelain=v1", "--untracked-files=normal", "--no-renames", "-z", "--", "."]) else {
        forget(root);
        return statuses;
    };
    // Entries are `XY path` separated by NUL, with paths relative to the top.
    for entry in out.split(|&byte| byte == 0) {
        let [x, y, b' ', path @ ..] = entry else { continue };
        let Some(status) = Status::of(*x, *y) else { continue };
        // Git lists an untracked folder once, with a trailing slash.
        let is_dir = path.last() == Some(&b'/');
        let path = Path::new(OsStr::from_bytes(path));
        if let Ok(rest) = path.strip_prefix(inside) {
            if is_dir {
                statuses.set_untracked_dir(root, root.join(rest));
            } else {
                statuses.set(root, root.join(rest), status);
            }
        }
    }
    statuses
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pairs_take_zeds_order() {
        assert_eq!(Status::of(b'U', b'U'), Some(Status::Conflicted));
        assert_eq!(Status::of(b'A', b'A'), Some(Status::Conflicted));
        assert_eq!(Status::of(b'A', b'D'), Some(Status::Deleted));
        assert_eq!(Status::of(b'A', b'M'), Some(Status::Modified));
        assert_eq!(Status::of(b' ', b'M'), Some(Status::Modified));
        assert_eq!(Status::of(b'A', b' '), Some(Status::Added));
        assert_eq!(Status::of(b'?', b'?'), Some(Status::Added));
        assert_eq!(Status::of(b' ', b' '), None);
    }

    #[test]
    fn folders_take_the_strongest_inside() {
        let root = Path::new("/r");
        let mut statuses = Statuses::default();
        statuses.set(root, PathBuf::from("/r/a/b/new.txt"), Status::Added);
        statuses.set(root, PathBuf::from("/r/a/old.txt"), Status::Deleted);
        statuses.set(root, PathBuf::from("/r/a/b/m.txt"), Status::Modified);
        assert_eq!(statuses.get(Path::new("/r/a/b")), Some(Status::Modified));
        assert_eq!(statuses.get(Path::new("/r/a")), Some(Status::Deleted));
        assert_eq!(statuses.get(Path::new("/r")), None);
    }

    #[test]
    fn an_untracked_folder_is_one_entry_and_holds_everything_under_it() {
        let root = Path::new("/r");
        let mut statuses = Statuses { root: root.to_path_buf(), ..Statuses::default() };
        statuses.set_untracked_dir(root, PathBuf::from("/r/a/new"));
        statuses.set(root, PathBuf::from("/r/a/m.txt"), Status::Modified);
        assert_eq!(statuses.get(Path::new("/r/a/new")), Some(Status::Added));
        assert_eq!(statuses.get(Path::new("/r/a/new/f.txt")), Some(Status::Added));
        assert_eq!(statuses.get(Path::new("/r/a/new/deep/g.txt")), Some(Status::Added));
        assert_eq!(statuses.get(Path::new("/r/a")), Some(Status::Modified));
        assert_eq!(statuses.get(Path::new("/r/a/other.txt")), None);
        assert_eq!(statuses.get(Path::new("/r/a/newer/f.txt")), None);
        // The folder, the file and the folder above both: nothing per file under the new folder.
        assert_eq!(statuses.by_path.len(), 3);
    }

    /// Each root read adds one entry to `TOPS`; forgetting a root takes its entry and no other.
    #[test]
    fn forgetting_a_root_takes_its_entry_and_only_its_entry() {
        let base = std::env::temp_dir().join(format!("finch-tops-{}", std::process::id()));
        let roots: Vec<PathBuf> = (0..3).map(|n| base.join(format!("repo{n}"))).collect();
        for root in &roots {
            std::fs::create_dir_all(root).unwrap();
            assert!(git(root, &["init", "-q"]).is_some());
            read(root);
        }
        let held = |root: &Path| TOPS.lock().unwrap().as_ref().is_some_and(|tops| tops.contains_key(root));
        assert!(roots.iter().all(|root| held(root)), "three roots read, three entries");
        forget(&roots[1]);
        assert!(held(&roots[0]) && !held(&roots[1]) && held(&roots[2]));
        forget(&roots[1]);
        roots.iter().for_each(|root| forget(root));
        assert!(roots.iter().all(|root| !held(root)));
        std::fs::remove_dir_all(&base).ok();
    }
}
