//! Scrap drafts: every Untitled tab is backed by a file in `~/Documents/Scrap Drafts/`, so what
//! was typed in it is never lost.
//!
//! A draft is named for when it started (`Untitled 2026-10-30 14.05.txt`, with ` 2`, ` 3` on a
//! clash). `GPUI_EDITOR_DRAFTS=<folder>` replaces the folder; tests and background launches use
//! it so they never touch the real Documents folder (macOS asks before an app first reads or
//! writes there). A write goes to a hidden temporary file in the same folder, is synced, then
//! renamed over the draft, so a crash leaves the last full copy; a temporary file left behind by
//! an interrupted write is overwritten by the next write and never read as a draft.

use gpui_kit::component::input::Rope;
use std::{
    ffi::{OsStr, OsString},
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex, MutexGuard, PoisonError,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

/// The folder drafts live in: `GPUI_EDITOR_DRAFTS`, else (a background launch) a temporary folder
/// of its own, else `~/Documents/Scrap Drafts`.
pub fn folder() -> PathBuf {
    if let Some(folder) = std::env::var_os("GPUI_EDITOR_DRAFTS") {
        return folder.into();
    }
    if std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some() {
        return std::env::temp_dir().join(format!("finch-drafts-{}", std::process::id()));
    }
    std::env::home_dir().unwrap_or_default().join("Documents/Scrap Drafts")
}

/// Whether `path` is a file directly in the drafts folder.
pub fn is_draft(path: &Path) -> bool {
    let (Some(parent), folder) = (path.parent(), folder()) else { return false };
    parent == folder || matches!((parent.canonicalize(), folder.canonicalize()), (Ok(parent), Ok(folder)) if parent == folder)
}

/// A draft's text. Drafts are written as UTF-8, so that is read as it is; bytes that aren't (a
/// file touched by something else) are decoded as any file is, but only when every byte has a
/// character: a decode that replaces some would be written back over the original on the first
/// key. That and a binary file are an `InvalidData` error: the file stays where it is, and the
/// caller says so.
pub fn read(path: &Path) -> io::Result<String> {
    let bytes = fs::read(path)?;
    match String::from_utf8(bytes) {
        Ok(text) => Ok(text),
        Err(err) => match crate::encoding::decode(err.into_bytes()) {
            Ok(decoded) if !decoded.replaced => Ok(decoded.text),
            _ => Err(io::Error::new(io::ErrorKind::InvalidData, "it isn’t a text file")),
        },
    }
}

/// A draft's path in `dir` for a draft that started at `stamp` (`2026-10-30 14.05`): no file
/// of that name exists yet and no open page has claimed it (`claim`).
pub fn new_path(dir: &Path, stamp: &str) -> PathBuf {
    let free = |path: &Path| !path.exists() && !lock_claims().iter().any(|claimed| claimed == path);
    let first = dir.join(format!("Untitled {stamp}.txt"));
    if free(&first) {
        return first;
    }
    (2..)
        .map(|n| dir.join(format!("Untitled {stamp} {n}.txt")))
        .find(|path| free(path))
        .expect("an unused draft name")
}

/// Names a page took that may not have a file yet: its first write is in flight or failed.
static CLAIMED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

fn lock_claims() -> MutexGuard<'static, Vec<PathBuf>> {
    CLAIMED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A page took `path` as its new draft: `new_path` hands it out to no one else until `release`.
pub fn claim(path: &Path) {
    lock_claims().push(path.to_path_buf());
}

/// The page's first write landed (the file's own existence keeps the name now) or the page let the
/// draft go.
pub fn release(path: &Path) {
    lock_claims().retain(|claimed| claimed != path);
}

/// A local date and time, as `localtime_r` gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Local {
    pub year: i32,
    /// 1 to 12.
    pub month: i32,
    pub day: i32,
    pub hour: i32,
    pub minute: i32,
}

/// `seconds` since the Unix epoch as local time.
pub fn local(seconds: i64) -> Local {
    #[repr(C)]
    struct Tm {
        sec: i32,
        min: i32,
        hour: i32,
        mday: i32,
        mon: i32,
        year: i32,
        wday: i32,
        yday: i32,
        isdst: i32,
        gmtoff: i64,
        zone: *const std::ffi::c_char,
    }
    unsafe extern "C" {
        fn localtime_r(time: *const i64, out: *mut Tm) -> *mut Tm;
    }
    let mut tm = Tm {
        sec: 0,
        min: 0,
        hour: 0,
        mday: 0,
        mon: 0,
        year: 0,
        wday: 0,
        yday: 0,
        isdst: 0,
        gmtoff: 0,
        zone: std::ptr::null(),
    };
    // SAFETY: `Tm` matches the C `struct tm` on macOS, and both pointers are valid for the call.
    unsafe { localtime_r(&seconds, &mut tm) };
    Local { year: tm.year + 1900, month: tm.mon + 1, day: tm.mday, hour: tm.hour, minute: tm.min }
}

/// The local date and time as `2026-10-30 14.05`, for a draft's name.
pub fn stamp_now() -> String {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
    let now = local(seconds);
    format!("{:04}-{:02}-{:02} {:02}.{:02}", now.year, now.month, now.day, now.hour, now.minute)
}

/// How much of each draft `list` reads: enough for a first line and the words filtering looks in.
pub const HEAD_BYTES: usize = 8 * 1024;

/// One draft as the Open draft list shows it: read-only, from the file's first `HEAD_BYTES`.
#[derive(Clone, Debug)]
pub struct Listed {
    pub path: PathBuf,
    /// The folder's own spelling of `path`, resolved, so an open tab's draft is recognised through a symlinked folder.
    pub real: PathBuf,
    pub modified: SystemTime,
    /// The first `HEAD_BYTES` of the text.
    pub head: String,
    /// The words in `head`.
    pub words: usize,
    /// The file is longer than `head`, so `words` is a floor.
    pub more: bool,
}

/// The words in `text`: runs of non-whitespace.
pub fn count_words(text: &str) -> usize {
    text.split_whitespace().count()
}

/// The drafts in `dir`, newest first (the file's modified time; a clash by name, descending). Hidden
/// files (a write's temporary copy) and files that aren't text (a NUL in the head) are left out; a
/// folder that can't be read lists nothing. Nothing is written.
pub fn list(dir: &Path) -> Vec<Listed> {
    use std::io::Read as _;
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<Listed> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if entry.file_name().to_string_lossy().starts_with('.') {
                return None;
            }
            let path = entry.path();
            let meta = fs::metadata(&path).ok().filter(fs::Metadata::is_file)?;
            let mut bytes = Vec::with_capacity(HEAD_BYTES.min(meta.len() as usize));
            File::open(&path).ok()?.take(HEAD_BYTES as u64).read_to_end(&mut bytes).ok()?;
            if bytes.contains(&0) {
                return None;
            }
            // The one buffer becomes the text as it is (valid UTF-8, nearly always); only a damaged head is decoded into a copy.
            let head = String::from_utf8(bytes).unwrap_or_else(|err| String::from_utf8_lossy(err.as_bytes()).into_owned());
            let words = count_words(&head);
            let real = path.canonicalize().unwrap_or_else(|_| path.clone());
            Some(Listed { path, real, modified: meta.modified().unwrap_or(UNIX_EPOCH), head, words, more: meta.len() > HEAD_BYTES as u64 })
        })
        .collect();
    found.sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| b.path.cmp(&a.path)));
    found
}

/// Where `write` stages `draft`'s next copy.
fn temp_path(draft: &Path) -> PathBuf {
    let mut name = OsString::from(".");
    name.push(draft.file_name().unwrap_or(OsStr::new("draft")));
    name.push(".tmp");
    draft.with_file_name(name)
}

/// Writes `text` as the whole draft, chunk by chunk so no whole-text copy is made: to a
/// temporary file in the same folder, synced, then renamed over the draft. The folder must
/// exist (`Slot::write` makes it).
pub fn write(draft: &Path, text: &Rope) -> io::Result<()> {
    let temp = temp_path(draft);
    let staged = File::create(&temp).and_then(|file| {
        let mut buffered = io::BufWriter::new(file);
        text.write_to(&mut buffered)?;
        buffered.into_inner().map_err(io::IntoInnerError::into_error)?.sync_all()
    });
    if let Err(err) = staged.and_then(|()| fs::rename(&temp, draft)) {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }
    // The rename itself reaches the disk once the folder does; best effort.
    if let Some(dir) = draft.parent().and_then(|dir| File::open(dir).ok()) {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Deletes the draft; a draft already gone is fine.
pub fn delete(draft: &Path) -> io::Result<()> {
    match fs::remove_file(draft) {
        Err(err) if err.kind() != io::ErrorKind::NotFound => Err(err),
        _ => Ok(()),
    }
}

/// Whether both paths reach one file on disk (same device and inode).
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    matches!((fs::symlink_metadata(a), fs::symlink_metadata(b)), (Ok(a), Ok(b)) if (a.dev(), a.ino()) == (b.dev(), b.ino()))
}

/// One draft's file as the document and its background writes share it. Writes and the
/// delete take the lock one at a time, each write is tagged with the document's edit count
/// when its text was taken, and a write no newer than what already landed is skipped, so an
/// older copy never lands over a newer one however the threads interleave.
pub struct Slot {
    path: PathBuf,
    /// The tag of the last write that landed at `path`.
    written: Option<u64>,
    /// Counts every `write` call, so the page can tell which result is the latest.
    attempts: u64,
    folder_made: bool,
    /// The draft was deleted or let go: nothing writes to it any more.
    closed: Arc<AtomicBool>,
}

/// A slot and its closed flag, which `close` sets without waiting for a write in flight.
#[derive(Clone)]
pub struct Shared {
    slot: Arc<Mutex<Slot>>,
    closed: Arc<AtomicBool>,
}

impl Shared {
    /// Lets the draft go without touching its file; later writes are skipped.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    pub fn same(&self, other: &Shared) -> bool {
        Arc::ptr_eq(&self.slot, &other.slot)
    }
}

impl Slot {
    pub fn shared(path: PathBuf) -> Shared {
        let closed = Arc::new(AtomicBool::new(false));
        let slot = Self { path, written: None, attempts: 0, folder_made: false, closed: closed.clone() };
        Shared { slot: Arc::new(Mutex::new(slot)), closed }
    }

    /// The draft's file was renamed to `path`. A write that held the lock while the rename
    /// happened may have made a copy at the old path: it goes, and nothing counts as written
    /// at the new one, so the next write puts the current text there. A rename that changed
    /// only the case of the name leaves one file that both paths reach (the Mac's default volume
    /// ignores case): that is the draft itself and stays.
    pub fn set_path(&mut self, path: PathBuf) {
        if path == self.path {
            return;
        }
        if !same_file(&self.path, &path) {
            if let Err(err) = delete(&self.path) {
                eprintln!("finch: can't delete the draft {}: {err}", self.path.display());
            }
        }
        self.path = path;
        self.written = None;
        self.folder_made = false;
    }

    /// Writes the text tagged `tag` unless a write at least as new has landed (or the slot is
    /// closed); `Ok` then too, since the file already holds that text or newer. The folder is
    /// made on the first write only, again after a failed one. Returns the try's number with
    /// the result: a later number is a later try.
    pub fn write(&mut self, tag: u64, text: &Rope) -> (u64, io::Result<()>) {
        self.attempts += 1;
        (self.attempts, self.put(tag, text))
    }

    fn put(&mut self, tag: u64, text: &Rope) -> io::Result<()> {
        if self.closed.load(Ordering::SeqCst) || self.written.is_some_and(|written| written >= tag) {
            return Ok(());
        }
        if !self.folder_made {
            if let Some(dir) = self.path.parent() {
                fs::create_dir_all(dir)?;
            }
            self.folder_made = true;
        }
        write(&self.path, text).inspect_err(|_| self.folder_made = false)?;
        self.written = Some(tag);
        Ok(())
    }

    /// Deletes the draft's file and closes the slot.
    pub fn delete(&mut self) -> io::Result<()> {
        self.closed.store(true, Ordering::SeqCst);
        delete(&self.path)
    }
}

/// Locks a slot; a write that panicked leaves nothing worse than a stale tag.
pub fn lock(slot: &Shared) -> MutexGuard<'_, Slot> {
    slot.slot.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh empty folder under the system temp dir.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("finch-drafts-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_draft_that_would_lose_bytes_is_unreadable_and_stays() {
        let dir = scratch("lossy");
        let good = dir.join("good.txt");
        fs::write(&good, "caf\u{e9}\n").unwrap();
        assert_eq!(read(&good).unwrap(), "caf\u{e9}\n");
        let bad = dir.join("bad.txt");
        let bytes = b"hi \xff\xfe\x80\x81 there\n";
        fs::write(&bad, bytes).unwrap();
        // Whatever the guess does with the odd bytes, the answer is never text that lost some.
        match read(&bad) {
            Ok(text) => assert!(!text.contains('\u{fffd}'), "{text:?}"),
            Err(err) => assert_eq!(err.kind(), io::ErrorKind::InvalidData),
        }
        let binary = dir.join("binary.txt");
        fs::write(&binary, b"a\0\0\xff\xfe\0b").unwrap();
        assert_eq!(read(&binary).unwrap_err().kind(), io::ErrorKind::InvalidData);
        assert_eq!(fs::read(&bad).unwrap(), bytes);
        let _ = fs::remove_dir_all(&dir);
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    }

    fn put(slot: &Shared, tag: u64, text: &str) -> io::Result<()> {
        lock(slot).write(tag, &Rope::from(text)).1
    }

    #[test]
    fn drafts_chunked_write_equals_the_string_write_byte_for_byte() {
        let dir = scratch("chunks");
        // Multi-byte text and CRLF, long enough to span several rope chunks.
        let text = "caf\u{e9} \u{1f600} \u{4e2d}\u{6587}\r\nsecond\r\nbare lf\n".repeat(4000);
        let rope = Rope::from(text.as_str());
        let draft = dir.join("Untitled 2026-10-30 14.05.txt");
        write(&draft, &rope).unwrap();
        let old = dir.join("old.txt");
        fs::write(&old, text.as_bytes()).unwrap();
        assert_eq!(fs::read(&draft).unwrap(), fs::read(&old).unwrap());
        assert_eq!(fs::read(&draft).unwrap(), text.as_bytes());
    }

    #[test]
    fn drafts_write_makes_the_folder_and_the_file() {
        let dir = scratch("write").join("Scrap Drafts");
        let draft = dir.join("Untitled 2026-10-30 14.05.txt");
        let slot = Slot::shared(draft.clone());
        put(&slot, 1, "hello\nworld").unwrap();
        assert_eq!(fs::read_to_string(&draft).unwrap(), "hello\nworld");
    }

    #[test]
    fn drafts_an_older_write_never_lands_over_a_newer_one() {
        let dir = scratch("order");
        let draft = dir.join("Untitled 2026-10-30 14.05.txt");
        let slot = Slot::shared(draft.clone());
        put(&slot, 3, "newest").unwrap();
        put(&slot, 2, "older").unwrap();
        put(&slot, 3, "same tag again").unwrap();
        assert_eq!(fs::read_to_string(&draft).unwrap(), "newest");
        put(&slot, 4, "next").unwrap();
        assert_eq!(fs::read_to_string(&draft).unwrap(), "next");
    }

    #[test]
    fn drafts_each_try_counts_so_a_late_result_can_be_told_from_a_newer_one() {
        let dir = scratch("attempts");
        let slot = Slot::shared(dir.join("Untitled 2026-10-30 14.05.txt"));
        let first = lock(&slot).write(1, &Rope::from("a")).0;
        let again = lock(&slot).write(1, &Rope::from("a")).0;
        let skipped = lock(&slot).write(0, &Rope::from("old")).0;
        assert!(first < again && again < skipped);
    }

    #[test]
    fn drafts_a_closed_slot_writes_nothing_and_a_failed_write_can_retry() {
        let dir = scratch("closed");
        let draft = dir.join("sub").join("Untitled 2026-10-30 14.05.txt");
        let slot = Slot::shared(draft.clone());
        put(&slot, 1, "a").unwrap();
        // The folder vanishes under the app: the write fails, the next one makes it again.
        fs::remove_dir_all(dir.join("sub")).unwrap();
        assert!(put(&slot, 2, "b").is_err());
        put(&slot, 2, "b").unwrap();
        assert_eq!(fs::read_to_string(&draft).unwrap(), "b");
        lock(&slot).delete().unwrap();
        put(&slot, 9, "late").unwrap();
        assert!(!draft.exists());

        let other = dir.join("kept.txt");
        let slot = Slot::shared(other.clone());
        put(&slot, 1, "kept").unwrap();
        slot.close();
        put(&slot, 2, "dropped").unwrap();
        assert_eq!(fs::read_to_string(&other).unwrap(), "kept");
    }

    #[test]
    fn drafts_a_renamed_slot_drops_a_copy_a_late_write_left_at_the_old_path() {
        let dir = scratch("renamed");
        let old = dir.join("Untitled 2026-10-30 14.05.txt");
        let new = dir.join("Notes.txt");
        let slot = Slot::shared(old.clone());
        // A write that held the lock while the folder renamed the file lands at the old path.
        put(&slot, 5, "newest").unwrap();
        lock(&slot).set_path(new.clone());
        assert!(!old.exists());
        // Nothing counts as written at the new path, so the same text writes there again.
        put(&slot, 5, "newest").unwrap();
        assert_eq!(fs::read_to_string(&new).unwrap(), "newest");
    }

    #[test]
    fn drafts_a_rename_to_the_same_file_keeps_it() {
        let dir = scratch("same-file");
        let old = dir.join("Untitled 2026-10-30 14.05.txt");
        let new = dir.join("Alias 2026-10-30 14.05.txt");
        let slot = Slot::shared(old.clone());
        put(&slot, 5, "newest").unwrap();
        // On a volume that ignores case a case-only new name reaches the same file; a hard link
        // stands in for it on any volume.
        fs::hard_link(&old, &new).unwrap();
        lock(&slot).set_path(new.clone());
        assert_eq!(fs::read_to_string(&old).unwrap(), "newest");
        assert_eq!(fs::read_to_string(&new).unwrap(), "newest");
    }

    #[test]
    fn drafts_rename_leaves_no_temp_file_and_the_full_text() {
        let dir = scratch("rename");
        let draft = dir.join("Untitled 2026-10-30 14.05.txt");
        write(&draft, &Rope::from("first copy, longer than the second")).unwrap();
        write(&draft, &Rope::from("second")).unwrap();
        assert_eq!(fs::read_to_string(&draft).unwrap(), "second");
        assert_eq!(names(&dir), ["Untitled 2026-10-30 14.05.txt"]);
    }

    #[test]
    fn drafts_delete_on_save_as_removes_the_file_and_forgives_a_missing_one() {
        let dir = scratch("delete");
        let draft = dir.join("Untitled 2026-10-30 14.05.txt");
        write(&draft, &Rope::from("moved to a real file")).unwrap();
        delete(&draft).unwrap();
        assert!(names(&dir).is_empty());
        delete(&draft).unwrap();
    }

    #[test]
    fn drafts_survive_a_kill_between_writes() {
        let dir = scratch("kill");
        let draft = dir.join("Untitled 2026-10-30 14.05.txt");
        write(&draft, &Rope::from("last full copy")).unwrap();
        // A write killed half way: the temp file holds a torn copy, the rename never ran.
        fs::write(temp_path(&draft), "last fu").unwrap();
        assert_eq!(fs::read_to_string(&draft).unwrap(), "last full copy");
        // The next write replaces the leftover and the draft in one piece.
        write(&draft, &Rope::from("last full copy and more")).unwrap();
        assert_eq!(fs::read_to_string(&draft).unwrap(), "last full copy and more");
        assert_eq!(names(&dir), ["Untitled 2026-10-30 14.05.txt"]);
    }

    #[test]
    fn drafts_a_clash_adds_a_number() {
        let dir = scratch("clash");
        let stamp = "2026-10-30 14.05";
        let first = new_path(&dir, stamp);
        assert_eq!(first, dir.join("Untitled 2026-10-30 14.05.txt"));
        write(&first, &Rope::from("a")).unwrap();
        let second = new_path(&dir, stamp);
        assert_eq!(second, dir.join("Untitled 2026-10-30 14.05 2.txt"));
        write(&second, &Rope::from("b")).unwrap();
        assert_eq!(new_path(&dir, stamp), dir.join("Untitled 2026-10-30 14.05 3.txt"));
    }

    #[test]
    fn drafts_a_claimed_name_is_skipped_until_released() {
        let dir = scratch("claimed");
        let stamp = "2026-10-30 14.06";
        // The first page's write hasn't landed (or failed): the second must not take its name.
        let first = new_path(&dir, stamp);
        claim(&first);
        let second = new_path(&dir, stamp);
        assert_eq!(second, dir.join("Untitled 2026-10-30 14.06 2.txt"));
        release(&first);
        assert_eq!(new_path(&dir, stamp), first);
    }

    #[test]
    fn drafts_list_is_newest_first_skips_hidden_and_binary_and_reads_only_a_head() {
        let dir = scratch("list");
        let older = dir.join("Untitled 2026-10-30 14.05.txt");
        let newer = dir.join("Untitled 2026-10-31 09.00.txt");
        fs::write(&older, "first one\nsecond line").unwrap();
        fs::write(&newer, "x ".repeat(HEAD_BYTES)).unwrap();
        fs::write(dir.join(".Untitled 2026-10-31 09.00.txt.tmp"), "torn").unwrap();
        fs::write(dir.join("binary.txt"), b"a\0b").unwrap();
        fs::create_dir(dir.join("folder")).unwrap();
        let past = SystemTime::now() - std::time::Duration::from_secs(3600);
        File::options().write(true).open(&older).unwrap().set_modified(past).unwrap();
        let listed = list(&dir);
        assert_eq!(listed.iter().map(|draft| draft.path.clone()).collect::<Vec<_>>(), [newer, older]);
        assert_eq!(listed[0].head.len(), HEAD_BYTES);
        assert!(listed[0].more && !listed[1].more);
        assert_eq!((listed[1].words, listed[1].head.as_str()), (4, "first one\nsecond line"));
        assert!(list(&dir.join("missing")).is_empty());
    }

    /// `list` reads a head into one buffer and makes it the text: before, the valid UTF-8 head was copied into a second `String`.
    #[test]
    fn listing_a_draft_makes_no_second_copy_of_its_head() {
        let dir = scratch("list-bytes");
        for i in 0..50 {
            fs::write(dir.join(format!("Untitled {i}.txt")), "word ".repeat(3 * HEAD_BYTES / 5)).unwrap();
        }
        let (listed, asked) = crate::test_alloc::bytes(|| list(&dir));
        assert_eq!(listed.len(), 50);
        assert!(listed.iter().all(|draft| draft.head.len() == HEAD_BYTES && draft.more));
        eprintln!("list of 50 drafts asked {asked} bytes ({} per draft, the head is {HEAD_BYTES})", asked / 50);
        assert!(asked < 50 * (HEAD_BYTES + 2048), "{asked}");
    }
}
