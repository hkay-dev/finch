//! Tells the settings when the file is changed by another program. FSEvents through the `notify`
//! crate, as the explorer's watcher does, with the same 100 ms window per burst.
//!
//! The folder is watched, not the file: editors save by writing a temp file and renaming it over
//! the original, which a watch on the file itself would lose. A settings file that is a symlink
//! (chezmoi) is watched at both ends: the folder the link sits in and the folder it points into.
//! Events for other files in those folders are dropped; the caller compares content to tell its
//! own writes from the others.

use std::{
    collections::BTreeSet,
    ffi::OsString,
    path::{Path, PathBuf},
    sync::mpsc,
    time::{Duration, Instant},
};

use futures_channel::mpsc::{UnboundedReceiver, unbounded};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// How long a burst of events collects, counted from its first one.
const WINDOW: Duration = Duration::from_millis(100);

/// The watcher; dropping it stops the events and ends the thread.
pub struct Watch {
    _watcher: RecommendedWatcher,
}

/// Follows the file at `path`. A unit comes from the receiver for each burst of changes to it.
/// `None` when no folder around it can be watched.
pub fn start(path: &Path) -> Option<(Watch, UnboundedReceiver<()>)> {
    let mut names: BTreeSet<OsString> = BTreeSet::new();
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut note = |file: &Path| {
        if let (Some(name), Some(dir)) = (file.file_name(), file.parent()) {
            if let Ok(dir) = dir.canonicalize() {
                names.insert(name.to_os_string());
                dirs.insert(dir);
            }
        }
    };
    note(path);
    if let Ok(real) = path.canonicalize() {
        note(&real);
    }

    let (raw_tx, raw_rx) = mpsc::channel::<()>();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event
            && !matches!(event.kind, notify::EventKind::Access(_))
            && (event.need_rescan() || event.paths.iter().any(|path| path.file_name().is_some_and(|name| names.contains(name))))
        {
            let _ = raw_tx.send(());
        }
    })
    .ok()?;
    let mut any = false;
    for dir in &dirs {
        any |= watcher.watch(dir, RecursiveMode::NonRecursive).is_ok();
    }
    if !any {
        return None;
    }

    let (tx, rx) = unbounded();
    std::thread::Builder::new()
        .name("settings-watch".into())
        .spawn(move || {
            while raw_rx.recv().is_ok() {
                let deadline = Instant::now() + WINDOW;
                while raw_rx.recv_timeout(deadline.saturating_duration_since(Instant::now())).is_ok() {}
                if tx.unbounded_send(()).is_err() {
                    return;
                }
            }
        })
        .ok()?;
    Some((Watch { _watcher: watcher }, rx))
}
