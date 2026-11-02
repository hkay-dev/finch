//! The version store: a copy of a file's text from each save, so an old version can be browsed
//! in the Compare view (`versions_list`, `compare_view`).
//!
//! The store is `~/Library/Application Support/Finch/Versions`, or `GPUI_EDITOR_VERSIONS=<folder>`
//! (tests set it). A background launch (`GPUI_EDITOR_BACKGROUND`) with no override uses a
//! `finch-versions-<pid>` folder in the temp dir, so it never writes or prunes the real one. Each file has a
//! folder named for its id: a hash of the file's canonical path. A rename or move starts a new
//! history; a file replaced by rename (git checkout, other editors) or deleted and recreated
//! keeps its history, and another file never inherits it. A snapshot is
//! `<unix milliseconds>.snap` in it.
//!
//! `record` runs on the background executor after a save has succeeded, and just before a save
//! overwrites a file, for the text it replaces (that save waits for it, so a first save can pause); every failure in it is
//! logged and swallowed, never handed back to the save. It writes a hidden temporary file, syncs
//! it and renames it into place, so a crash leaves whole snapshots only (a leftover temporary file
//! is never listed, is named for its process, and a record sweeps it once it is a minute old). A text identical to the newest snapshot is
//! skipped, and so is a file over `MAX_FILE_BYTES`. The text is written from the rope's chunks
//! through a buffer, never copied whole. Each record then thins that file's snapshots
//! (everything from the last hour, one per hour for a day, one per day for 30 days, nothing
//! older) and drops the oldest while the file is over its byte cap or the store is over its own.
//! The store's size is a count: one walk of the folder seeds it (the first record of a run), then
//! each record adds what it wrote and takes off what it pruned, so a record reads only its own
//! file's folder. Only a count over the cap walks the store again, for the real sizes.

use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use gpui_kit::component::input::Rope;

/// A file longer than this is never snapshotted.
pub const MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;
/// The most one file's snapshots hold together.
pub const MAX_BYTES_PER_FILE: u64 = 20 * 1024 * 1024;
/// The most the whole store holds.
pub const MAX_BYTES_TOTAL: u64 = 200 * 1024 * 1024;

const HOUR: u64 = 3600;
const DAY: u64 = 24 * HOUR;
/// Snapshots newer than this are all kept.
const KEEP_ALL: u64 = HOUR;
/// Up to this age one snapshot per hour is kept; up to `KEEP_DAILY` one per day.
const KEEP_HOURLY: u64 = DAY;
const KEEP_DAILY: u64 = 30 * DAY;

const EXTENSION: &str = "snap";

/// One saved version of a file, as `list` gives it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Version {
    pub time: SystemTime,
    /// The text's length in bytes.
    pub len: u64,
    /// Names the version for `read`.
    pub id: String,
}

/// The byte caps a store holds to; tests shrink them.
#[derive(Clone, Copy, Debug)]
struct Limits {
    file: u64,
    per_file: u64,
    total: u64,
}

const LIMITS: Limits = Limits { file: MAX_FILE_BYTES, per_file: MAX_BYTES_PER_FILE, total: MAX_BYTES_TOTAL };

/// The folder versions live in: `GPUI_EDITOR_VERSIONS`, else `Versions` in Application Support.
pub fn folder() -> PathBuf {
    if let Some(folder) = std::env::var_os("GPUI_EDITOR_VERSIONS") {
        return folder.into();
    }
    if std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some() {
        return std::env::temp_dir().join(format!("finch-versions-{}", std::process::id()));
    }
    crate::settings::support_dir().join("Versions")
}

/// Serialises records, so two saves of one file never thin or cap over each other.
static RECORDING: Mutex<()> = Mutex::new(());

/// The bytes each store folder holds as this process last counted them (one entry per store root,
/// which is one in the app): seeded by one walk, then kept by `record`.
static TOTALS: Mutex<Vec<(PathBuf, u64)>> = Mutex::new(Vec::new());

/// Keeps `text` as a version of the file at `path`, saved at `now`. For the background executor,
/// after a save succeeded: a failure is logged and goes no further.
pub fn record(path: &Path, text: &Rope, now: SystemTime) {
    let _turn = RECORDING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Err(err) = Store::new(folder(), LIMITS).record(path, text, now) {
        eprintln!("finch: can't keep a version of {}: {err}", path.display());
    }
}

/// The versions kept for the file at `path`, newest first. Empty when there are none or the store can't be read.
pub fn list(path: &Path) -> Vec<Version> {
    Store::new(folder(), LIMITS).list(path)
}

/// The text of version `id` of the file at `path`.
pub fn read(path: &Path, id: &str) -> io::Result<String> {
    Store::new(folder(), LIMITS).read(path, id)
}

struct Store {
    root: PathBuf,
    limits: Limits,
}

/// FNV-1a over `bytes`: a name for a path that is the same on every run and toolchain.
fn fnv(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
}

/// The folder name of the file at `path`: the hash of its canonical path (the path as given
/// when the file can't be resolved).
fn file_id(path: &Path) -> String {
    let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    format!("p-{:016x}", fnv(std::os::unix::ffi::OsStrExt::as_bytes(path.as_os_str())))
}

fn millis(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64)
}

/// A snapshot on disk, found by listing a file's folder.
struct Found {
    millis: u64,
    len: u64,
    path: PathBuf,
}

impl Found {
    fn seconds(&self) -> u64 {
        self.millis / 1000
    }
}

/// `fs::read_dir`, counted in tests (per thread) so a test can say how many directory reads a record makes.
fn list_dir(dir: &Path) -> io::Result<fs::ReadDir> {
    #[cfg(test)]
    DIR_READS.with(|reads| reads.set(reads.get() + 1));
    fs::read_dir(dir)
}

#[cfg(test)]
thread_local! {
    static DIR_READS: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// The snapshots in `dir`, newest first. A temporary or foreign file is not one.
fn snapshots(dir: &Path) -> Vec<Found> {
    let Ok(entries) = list_dir(dir) else { return Vec::new() };
    let mut found: Vec<Found> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension()? != EXTENSION {
                return None;
            }
            let millis = path.file_stem()?.to_str()?.parse().ok()?;
            Some(Found { millis, len: entry.metadata().ok()?.len(), path })
        })
        .collect();
    found.sort_by(|a, b| b.millis.cmp(&a.millis));
    found
}

/// The snapshots in `dir` like `snapshots`, from the one read that also sweeps its leftover
/// temporary files. Only an old temporary file is a leftover: a young one is another Finch's
/// record in flight.
fn scan(dir: &Path) -> Vec<Found> {
    let Ok(entries) = list_dir(dir) else { return Vec::new() };
    let mut found: Vec<Found> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        match path.extension() {
            Some(ext) if ext == "tmp" => {
                if older_than_a_minute(&path) {
                    let _ = fs::remove_file(&path);
                }
            }
            Some(ext) if ext == EXTENSION => {
                let Some(millis) = path.file_stem().and_then(|stem| stem.to_str()).and_then(|stem| stem.parse().ok()) else { continue };
                let Some(len) = entry.metadata().ok().map(|meta| meta.len()) else { continue };
                found.push(Found { millis, len, path });
            }
            _ => {}
        }
    }
    found.sort_by(|a, b| b.millis.cmp(&a.millis));
    found
}

/// Whether the snapshot at `path` holds exactly `text`, compared chunk by chunk (the caller has
/// checked the lengths match).
fn same_text(path: &Path, text: &Rope) -> bool {
    fs::read(path).is_ok_and(|bytes| {
        let mut at = 0;
        text.chunks().all(|chunk| {
            let end = at + chunk.len();
            let same = bytes.get(at..end) == Some(chunk.as_bytes());
            at = end;
            same
        })
    })
}

/// Whether `path` was last changed over a minute ago: a temporary file or an empty folder that
/// old belongs to no record still running, in this process or another.
fn older_than_a_minute(path: &Path) -> bool {
    fs::metadata(path).and_then(|meta| meta.modified()).ok().and_then(|time| time.elapsed().ok()).is_some_and(|age| age >= Duration::from_secs(60))
}

impl Store {
    fn new(root: PathBuf, limits: Limits) -> Self {
        Self { root, limits }
    }

    fn dir(&self, path: &Path) -> PathBuf {
        self.root.join(file_id(path))
    }

    fn list(&self, path: &Path) -> Vec<Version> {
        snapshots(&self.dir(path))
            .into_iter()
            .map(|found| Version { time: UNIX_EPOCH + Duration::from_millis(found.millis), len: found.len, id: found.millis.to_string() })
            .collect()
    }

    fn read(&self, path: &Path, id: &str) -> io::Result<String> {
        // An id is the digits of a snapshot's name; anything else never reaches the file system.
        if id.is_empty() || !id.bytes().all(|b| b.is_ascii_digit()) {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a version"));
        }
        let bytes = fs::read(self.dir(path).join(format!("{id}.{EXTENSION}")))?;
        String::from_utf8(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "the version isn’t text"))
    }

    fn record(&self, path: &Path, text: &Rope, now: SystemTime) -> io::Result<()> {
        let len = text.len() as u64;
        if len > self.limits.file {
            return Ok(());
        }
        let dir = self.dir(path);
        fs::create_dir_all(&dir)?;
        // Seeded before this snapshot exists, so the count below adds it exactly once.
        self.seed(&dir);
        let mut existing = scan(&dir);
        if let Some(newest) = existing.first()
            && newest.len == len
            && same_text(&newest.path, text)
        {
            return Ok(());
        }
        // Snapshots in one millisecond, or a clock that went back, get the next free name.
        let mut stamp = millis(now);
        while existing.iter().any(|found| found.millis == stamp) {
            stamp += 1;
        }
        let target = dir.join(format!("{stamp}.{EXTENSION}"));
        let temp = dir.join(format!(".{stamp}.{}.tmp", std::process::id()));
        let stage = || {
            File::create(&temp).and_then(|file| {
                // Chunk by chunk, so no copy of the whole text is made.
                let mut buffered = io::BufWriter::new(file);
                text.write_to(&mut buffered)?;
                buffered.into_inner().map_err(io::IntoInnerError::into_error)?.sync_all()
            })
        };
        // Another Finch's sweep may have removed the folder, empty, since it was made: make it again, once.
        let staged = match stage() {
            Err(err) if err.kind() == io::ErrorKind::NotFound => fs::create_dir_all(&dir).and_then(|()| stage()),
            other => other,
        };
        if let Err(err) = staged.and_then(|()| fs::rename(&temp, &target)) {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
        if let Ok(dir) = File::open(&dir) {
            let _ = dir.sync_all();
        }
        existing.push(Found { millis: stamp, len, path: target.clone() });
        existing.sort_by(|a, b| b.millis.cmp(&a.millis));
        let removed = self.tidy(existing, millis(now), &target);
        self.change_total(|total| total.saturating_add(len).saturating_sub(removed));
        self.cap_total(&dir, &target);
        Ok(())
    }

    /// Thins a file's snapshots (`found`, newest first, the one just written among them) as of
    /// `now` and holds them to the file's byte cap; `keep` (the snapshot just written) is never
    /// dropped. Best effort: a snapshot that won't delete stays. Gives the bytes removed.
    fn tidy(&self, found: Vec<Found>, now: u64, keep: &Path) -> u64 {
        let now = now / 1000;
        let mut removed = 0;
        let mut kept: Vec<Found> = Vec::new();
        let mut bucket = None;
        for found in found {
            let age = now.saturating_sub(found.seconds());
            let slot = match age {
                0..=KEEP_ALL => None,
                ..=KEEP_HOURLY => Some((0, found.seconds() / HOUR)),
                ..=KEEP_DAILY => Some((1, found.seconds() / DAY)),
                _ => {
                    if found.path != keep {
                        if fs::remove_file(&found.path).is_ok() {
                            removed += found.len;
                        }
                        continue;
                    }
                    None
                }
            };
            // Newest first: the first of a bucket stays, the rest of it go.
            if slot.is_some() && slot == bucket && found.path != keep {
                if fs::remove_file(&found.path).is_ok() {
                    removed += found.len;
                }
                continue;
            }
            if slot.is_some() {
                bucket = slot;
            }
            kept.push(found);
        }
        // Over the file's cap: drop from the oldest end.
        let mut size: u64 = kept.iter().map(|found| found.len).sum();
        while size > self.limits.per_file {
            let Some(at) = kept.iter().rposition(|found| found.path != keep) else { break };
            let gone = kept.remove(at);
            if fs::remove_file(&gone.path).is_ok() {
                size -= gone.len;
                removed += gone.len;
            }
        }
        removed
    }

    /// Every snapshot of every file, and removes the folders left empty (an old empty one). A
    /// folder a second Finch just made has no snapshot yet: only an old empty one goes, and never
    /// `protect`, the folder a record is writing into. `remove_dir` refuses a folder that holds
    /// anything, a temporary file included.
    fn walk(&self, protect: &Path) -> Vec<Found> {
        let Ok(dirs) = list_dir(&self.root) else { return Vec::new() };
        let mut all: Vec<Found> = Vec::new();
        for dir in dirs.filter_map(Result::ok) {
            let path = dir.path();
            if !path.is_dir() {
                continue;
            }
            let found = snapshots(&path);
            if found.is_empty() && path != protect && older_than_a_minute(&path) {
                let _ = fs::remove_dir(&path);
                continue;
            }
            all.extend(found);
        }
        all
    }

    /// Counts the store's bytes once per process (and root), by walking it; every record after
    /// that keeps the count with what it writes and prunes. Another Finch's records aren't seen
    /// until `cap_total` finds the count over the cap and walks again.
    fn seed(&self, protect: &Path) {
        if self.total().is_none() {
            let size = self.walk(protect).iter().map(|found| found.len).sum();
            self.change_total(|_| size);
        }
    }

    fn total(&self) -> Option<u64> {
        TOTALS.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).iter().find(|(root, _)| *root == self.root).map(|(_, total)| *total)
    }

    fn change_total(&self, change: impl FnOnce(u64) -> u64) {
        let mut totals = TOTALS.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        match totals.iter_mut().find(|(root, _)| *root == self.root) {
            Some((_, total)) => *total = change(*total),
            None => totals.push((self.root.clone(), change(0))),
        }
    }

    /// While the store's count is over its cap, walks it for the real sizes and drops the
    /// oldest snapshots of any file, but not `keep`, and removes the folders that leaves empty.
    /// Under the cap it reads nothing.
    fn cap_total(&self, protect: &Path, keep: &Path) {
        if self.total().is_none_or(|total| total <= self.limits.total) {
            return;
        }
        let mut all = self.walk(protect);
        let mut size: u64 = all.iter().map(|found| found.len).sum();
        all.sort_by_key(|found| found.millis);
        let mut touched: Vec<PathBuf> = Vec::new();
        for found in all {
            if size <= self.limits.total {
                break;
            }
            if found.path != keep && fs::remove_file(&found.path).is_ok() {
                size -= found.len;
                if let Some(dir) = found.path.parent().filter(|dir| !touched.iter().any(|seen| seen == dir)) {
                    touched.push(dir.to_path_buf());
                }
            }
        }
        for dir in touched {
            if dir != protect {
                let _ = fs::remove_dir(&dir);
            }
        }
        self.change_total(|_| size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    impl Store {
        fn put(&self, path: &Path, text: &str, now: SystemTime) -> io::Result<()> {
            self.record(path, &Rope::from(text), now)
        }
    }

    fn store(name: &str, limits: Limits) -> (Store, PathBuf) {
        let root = std::env::temp_dir().join(format!("finch-versions-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let file = root.join("note.txt");
        fs::write(&file, "x").unwrap();
        (Store::new(root.join("store"), limits), file)
    }

    fn at(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }

    fn cleanup(store: &Store) {
        let _ = fs::remove_dir_all(store.root.parent().unwrap());
    }

    const NOW: u64 = 100 * DAY;

    #[test]
    fn lists_newest_first_and_reads_back() {
        let (store, file) = store("list", LIMITS);
        store.put(&file, "one", at(NOW)).unwrap();
        store.put(&file, "two!", at(NOW + 5)).unwrap();
        let versions = store.list(&file);
        assert_eq!(versions.iter().map(|v| v.len).collect::<Vec<_>>(), [4, 3]);
        assert_eq!(versions[0].time, at(NOW + 5));
        assert_eq!(store.read(&file, &versions[1].id).unwrap(), "one");
        assert!(store.read(&file, "../x").is_err());
        cleanup(&store);
    }

    #[test]
    fn history_follows_the_path_not_the_inode() {
        let (store, file) = store("path", LIMITS);
        store.put(&file, "first", at(NOW)).unwrap();
        // Replaced by rename (new inode), the file keeps its history.
        let staged = file.with_file_name("staged");
        fs::write(&staged, "second").unwrap();
        fs::rename(&staged, &file).unwrap();
        store.put(&file, "second", at(NOW + 5)).unwrap();
        assert_eq!(store.list(&file).len(), 2);
        // A moved file starts over; another file never shows this one's versions.
        let moved = file.with_file_name("moved.txt");
        fs::rename(&file, &moved).unwrap();
        assert!(store.list(&moved).is_empty());
        cleanup(&store);
    }

    #[test]
    fn an_identical_text_is_skipped_but_an_old_text_again_is_kept() {
        let (store, file) = store("dedupe", LIMITS);
        store.put(&file, "same", at(NOW)).unwrap();
        store.put(&file, "same", at(NOW + 5)).unwrap();
        assert_eq!(store.list(&file).len(), 1);
        store.put(&file, "other", at(NOW + 10)).unwrap();
        store.put(&file, "same", at(NOW + 15)).unwrap();
        assert_eq!(store.list(&file).len(), 3);
        cleanup(&store);
    }

    #[test]
    fn a_file_over_the_size_cap_is_not_kept() {
        let (store, file) = store("big", Limits { file: 10, ..LIMITS });
        store.put(&file, "0123456789", at(NOW)).unwrap();
        store.put(&file, "0123456789a", at(NOW + 1)).unwrap();
        assert_eq!(store.list(&file).len(), 1);
        cleanup(&store);
    }

    #[test]
    fn old_snapshots_thin_by_the_hour_then_the_day_then_go() {
        let (store, file) = store("thin", LIMITS);
        let put = |seconds: u64, text: &str| store.put(&file, text, at(seconds)).unwrap();
        // 40 days ago: past the month. 10 days ago, twice in a day. 5 hours ago, twice in an hour.
        put(NOW - 40 * DAY, "ancient");
        put(NOW - 10 * DAY, "ten a");
        put(NOW - 10 * DAY + 600, "ten b");
        put(NOW - 5 * HOUR, "five a");
        put(NOW - 5 * HOUR + 60, "five b");
        // Within the last hour: all kept.
        put(NOW - 600, "recent a");
        put(NOW - 300, "recent b");
        put(NOW, "now");
        let texts: Vec<String> = store.list(&file).iter().map(|v| store.read(&file, &v.id).unwrap()).collect();
        assert_eq!(texts, ["now", "recent b", "recent a", "five b", "ten b"]);
        cleanup(&store);
    }

    #[test]
    fn a_file_keeps_to_its_byte_cap_dropping_the_oldest() {
        let (store, file) = store("cap", Limits { per_file: 25, ..LIMITS });
        for n in 0..5 {
            store.put(&file, &format!("{n}").repeat(10), at(NOW + n)).unwrap();
        }
        let versions = store.list(&file);
        assert_eq!(versions.len(), 2);
        assert_eq!(store.read(&file, &versions[0].id).unwrap(), "4444444444");
        cleanup(&store);
    }

    #[test]
    fn the_store_keeps_to_its_total_cap() {
        let (store, a) = store("total", Limits { total: 25, ..LIMITS });
        let b = a.with_file_name("other.txt");
        fs::write(&b, "y").unwrap();
        store.put(&a, &"a".repeat(10), at(NOW)).unwrap();
        store.put(&b, &"b".repeat(10), at(NOW + 1)).unwrap();
        store.put(&a, &"c".repeat(10), at(NOW + 2)).unwrap();
        assert_eq!(store.list(&a).len(), 1);
        assert_eq!(store.list(&b).len(), 1);
        assert_eq!(store.read(&a, &store.list(&a)[0].id).unwrap(), "cccccccccc");
        cleanup(&store);
    }

    #[test]
    fn a_folder_the_total_cap_emptied_is_removed() {
        let (store, a) = store("emptied", Limits { total: 15, ..LIMITS });
        let b = a.with_file_name("other.txt");
        fs::write(&b, "y").unwrap();
        store.put(&a, &"a".repeat(10), at(NOW)).unwrap();
        store.put(&b, &"b".repeat(10), at(NOW + 1)).unwrap();
        assert!(store.list(&a).is_empty());
        assert!(!store.dir(&a).exists());
        assert_eq!(store.list(&b).len(), 1);
        cleanup(&store);
    }

    #[test]
    fn a_failed_write_is_an_error_here_and_never_a_panic() {
        let (store, file) = store("fail", LIMITS);
        // The store's folder is a plain file, so nothing can be made inside it.
        fs::write(&store.root, "in the way").unwrap();
        assert!(store.put(&file, "text", at(NOW)).is_err());
        assert!(store.list(&file).is_empty());
        assert!(store.read(&file, "1").is_err());
        let broken = Store::new(store.root.join("nested"), LIMITS);
        assert!(broken.put(&file, "text", at(NOW)).is_err());
        cleanup(&store);
    }

    #[test]
    fn a_hundred_saves_of_a_megabyte_stay_under_the_file_cap() {
        let (store, file) = store("hundred", LIMITS);
        let mut text = "0123456789abcdef".repeat(64 * 1024);
        for n in 0..100u64 {
            // Each save changes the text, a minute apart (all inside the hour for the first 59).
            text.replace_range(..8, &format!("{n:08}"));
            store.put(&file, &text, at(NOW + n * 60)).unwrap();
        }
        let size: u64 = store.list(&file).iter().map(|v| v.len).sum();
        println!("100 saves of {} bytes: {} versions, {} bytes", text.len(), store.list(&file).len(), size);
        assert!(size <= MAX_BYTES_PER_FILE);
        assert!(!store.list(&file).is_empty());
        cleanup(&store);
    }

    fn dir_reads() -> u32 {
        DIR_READS.with(std::cell::Cell::get)
    }

    #[test]
    fn a_save_reads_one_folder_however_many_files_the_store_holds() {
        let (store, file) = store("reads", LIMITS);
        // Forty other files' folders already in the store.
        for n in 0..40 {
            let other = file.with_file_name(format!("other-{n}.txt"));
            fs::write(&other, "y").unwrap();
            store.put(&other, "elsewhere", at(NOW + n)).unwrap();
        }
        // The first record of the run walks the store to count it; every later one does not.
        store.put(&file, "one", at(NOW + 100)).unwrap();
        let before = dir_reads();
        for n in 1..=5u64 {
            store.put(&file, &format!("save {n}"), at(NOW + 100 + n)).unwrap();
        }
        // One listing of the file's own folder per save: before, that was 3 plus one per folder (41).
        assert_eq!(dir_reads() - before, 5);
        cleanup(&store);
    }

    #[test]
    fn the_count_follows_what_is_written_and_pruned() {
        let (store, file) = store("count", Limits { per_file: 25, ..LIMITS });
        let real = |store: &Store| -> u64 { store.walk(Path::new("")).iter().map(|found| found.len).sum() };
        for n in 0..6 {
            store.put(&file, &format!("{n}").repeat(10), at(NOW + n)).unwrap();
            assert_eq!(store.total(), Some(real(&store)));
        }
        // Thinned by age too: an old snapshot goes with the hour bucket's others.
        store.put(&file, "a", at(NOW + 10 * DAY)).unwrap();
        assert_eq!(store.total(), Some(real(&store)));
        cleanup(&store);
    }

    #[test]
    fn a_text_in_many_chunks_is_written_whole_and_found_again() {
        let (store, file) = store("chunks", LIMITS);
        let text = "héllo wörld\r\n".repeat(20_000);
        let rope = Rope::from(text.as_str());
        assert!(rope.chunks().count() > 1);
        store.record(&file, &rope, at(NOW)).unwrap();
        assert_eq!(store.read(&file, &store.list(&file)[0].id).unwrap(), text);
        // The same rope again is the newest snapshot: skipped.
        store.record(&file, &rope, at(NOW + 1)).unwrap();
        assert_eq!(store.list(&file).len(), 1);
        // One byte different, same length: kept.
        store.record(&file, &Rope::from(text.replacen('h', "j", 1).as_str()), at(NOW + 2)).unwrap();
        assert_eq!(store.list(&file).len(), 2);
        cleanup(&store);
    }
}
