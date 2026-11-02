//! File actions of the explorer: new file or folder, rename, duplicate, move to Trash.
//!
//! The disk side is plain functions here, so they can be tested without a window. The tab side
//! is an `impl Workspace` block: a rename moves the open documents of the file (or of the files
//! under a folder) to the new path, keeping their text, undo history and unsaved state; a
//! removal closes the tabs of what is gone. Neither reads or writes a document's text, so
//! edits that are not saved yet are never touched.

use std::{
    collections::HashSet,
    ffi::{CStr, CString, c_char},
    fs, io,
    os::unix::{ffi::OsStrExt as _, fs::MetadataExt as _},
    path::{Path, PathBuf},
    ptr,
};

use gpui_kit::base::Root;
use gpui_kit::*;
use objc2::{
    class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool},
};

use crate::{
    document::Document,
    workspace::{Workspace, open_workspaces_except},
};

/// The name a new file starts with, before the user types one.
pub const NEW_FILE_NAME: &str = "untitled.txt";
/// The name a new folder starts with.
pub const NEW_FOLDER_NAME: &str = "untitled folder";

/// Why `name` can't name a file or folder in one folder, or `Ok`.
pub fn check_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("The name can’t be empty.".into());
    }
    if name == "." || name == ".." {
        return Err(format!("\u{201C}{name}\u{201D} can’t be a name."));
    }
    if name.contains('/') || name.contains(':') || name.contains('\0') {
        return Err("No \u{201C}/\u{201D} or \u{201C}:\u{201D} in a name.".into());
    }
    if name.len() > 255 {
        return Err("The name is too long.".into());
    }
    Ok(())
}

/// Splits `name` at its extension: `("notes", ".md")`. A dot first in the name, or a folder, has none.
fn split_extension(name: &str, is_dir: bool) -> (&str, &str) {
    match name.rfind('.').filter(|&dot| dot > 0 && !is_dir) {
        Some(dot) => name.split_at(dot),
        None => (name, ""),
    }
}

/// A path in `dir` that nothing uses yet: `name`, else `stem 2.ext`, `stem 3.ext`.
pub fn free_name(dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, extension) = split_extension(name, is_dir);
    (2..).map(|n| dir.join(format!("{stem} {n}{extension}"))).find(|path| !path.exists()).unwrap_or(first)
}

/// Where a copy of `path` goes, as Finder names it: `name copy.ext`, then `name copy 2.ext`.
pub fn copy_path(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?.to_string_lossy();
    Some(copy_name(path.parent()?, &name, path.is_dir()))
}

/// The free path in `dir` for a copy of `name`: `stem copy.ext`, then `stem copy 2.ext`.
fn copy_name(dir: &Path, name: &str, is_dir: bool) -> PathBuf {
    let (stem, extension) = split_extension(name, is_dir);
    let first = dir.join(format!("{stem} copy{extension}"));
    if !first.exists() {
        return first;
    }
    (2..).map(|n| dir.join(format!("{stem} copy {n}{extension}"))).find(|path| !path.exists()).unwrap_or(first)
}

/// Why `name` can't be typed for a new item, or `Ok`: each part of it passes `check_name` (so none
/// is empty, `.` or `..`), but `/` splits it into folders (`notes/2026/idea.md`) and one trailing
/// `/` makes a folder.
pub fn check_new_name(name: &str) -> Result<(), String> {
    let name = name.strip_suffix('/').unwrap_or(name);
    name.split('/').try_for_each(check_name)
}

/// Splits a checked new name into the folders on the way and the last part, and whether a
/// trailing `/` asked for a folder.
fn split_new_name(name: &str) -> (Vec<&str>, &str, bool) {
    let (name, trailing) = match name.strip_suffix('/') {
        Some(name) => (name, true),
        None => (name, false),
    };
    let mut parts: Vec<&str> = name.split('/').collect();
    let last = parts.pop().unwrap_or(name);
    (parts, last, trailing)
}

/// The words for a file action that failed with `err`: what happened to `path` and what to try.
/// `doing` is the verb phrase for what was being done ("save", "create", "move"). Nothing here
/// shows the error number or the system's own wording.
pub fn explain(doing: &str, path: &Path, err: &io::Error) -> String {
    let name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_else(|| path.display().to_string());
    // macOS errno: EPERM 1, EACCES 13, EEXIST 17, EISDIR 21, EFBIG 27, ENOSPC 28, EROFS 30, EDQUOT 69.
    let raw = err.raw_os_error();
    if raw == Some(30) {
        return format!("The disk is read-only, so \u{201C}{name}\u{201D} can’t be changed there. Choose a folder on another disk.");
    }
    if matches!(raw, Some(28 | 69)) {
        return format!("The disk is full. Free up some space, then try to {doing} \u{201C}{name}\u{201D} again.");
    }
    if raw == Some(27) {
        return format!("\u{201C}{name}\u{201D} is too large to {doing} here. Choose another place with more room.");
    }
    if doing == "save" && raw == Some(21) {
        return "A folder now has that name. Use Save as… to save it somewhere else.".to_owned();
    }
    match err.kind() {
        io::ErrorKind::PermissionDenied if matches!(doing, "open" | "read" | "unlock") => {
            format!("You don’t have permission to {doing} \u{201C}{name}\u{201D}. Check its permissions in Finder (Get Info).")
        }
        io::ErrorKind::PermissionDenied => {
            format!("You don’t have permission to {doing} \u{201C}{name}\u{201D} in this folder. Choose another place or change its permissions in Finder.")
        }
        io::ErrorKind::NotFound => format!("\u{201C}{name}\u{201D} or its folder isn’t there anymore. Check that it wasn’t moved or deleted, then try again."),
        io::ErrorKind::AlreadyExists => taken(path),
        io::ErrorKind::InvalidInput if doing == "open" || doing == "read" => format!("\u{201C}{name}\u{201D} isn’t a file Finch can open."),
        io::ErrorKind::InvalidData => format!("\u{201C}{name}\u{201D} isn’t a text file, so it can’t be shown here."),
        _ => format!("Couldn’t {doing} \u{201C}{name}\u{201D}. Check that it’s still there and try again."),
    }
}

/// The words for a folder an item was to be made in that is no longer there.
fn gone_folder(dir: &Path) -> String {
    let name = dir.file_name().map_or_else(|| dir.display().to_string(), |name| name.to_string_lossy().into_owned());
    format!("The folder \u{201C}{name}\u{201D} isn’t there anymore.")
}

/// The title of the alert for a failed `doing` ("move", "copy", "duplicate") of `paths`: it names
/// the file in curly quotes, or counts the items when there are several.
pub fn failed_title(doing: &str, paths: &[PathBuf]) -> String {
    match paths {
        [one] => {
            let name = one.file_name().map_or_else(|| one.display().to_string(), |name| name.to_string_lossy().into_owned());
            format!("Couldn’t {doing} \u{201C}{name}\u{201D}")
        }
        _ => format!("Couldn’t {doing} {} items", paths.len()),
    }
}

/// Makes an empty file or an empty folder `name` in `dir`, which must be a folder that is there
/// (a gone one is refused, not recreated) and must not hold `name` yet. `name` may be
/// a path (`notes/2026/idea.md`): the folders on the way are made, and a name ending in `/` makes
/// a folder. Returns the path made.
pub fn create(dir: &Path, name: &str, is_dir: bool) -> Result<PathBuf, String> {
    check_new_name(name)?;
    // Only the folders typed in the name are made: a base that is gone is never recreated.
    if !dir.is_dir() {
        return Err(gone_folder(dir));
    }
    let (folders, last, trailing) = split_new_name(name);
    let is_dir = is_dir || trailing;
    let parent = folders.iter().fold(dir.to_path_buf(), |path, part| path.join(part));
    fs::create_dir_all(&parent).map_err(|err| explain("create", &parent, &err))?;
    let path = free_name(&parent, last, is_dir);
    make_empty(&path, is_dir)?;
    Ok(path)
}

/// Makes an empty file or folder at `path`, refusing a name that is taken (nothing is overwritten).
pub fn make_empty(path: &Path, is_dir: bool) -> Result<(), String> {
    let made = if is_dir { fs::create_dir(path) } else { fs::OpenOptions::new().write(true).create_new(true).open(path).map(drop) };
    made.map_err(|err| explain("create", path, &err))
}

/// The last part of a new file's typed name as it is made. The field opens as `untitled.txt` with
/// the stem selected, so a typed `idea.md` arrives as `idea.md.txt`: the opening `.txt` goes when
/// the stem holds a dot. Any other name (`idea.md`, `Makefile`, `.env`) stays as typed: a name with
/// no dot means the field's `.txt` was cleared on purpose.
fn new_file_name(last: &str) -> String {
    match last.strip_suffix(".txt") {
        Some(stem) if stem.contains('.') && check_name(stem).is_ok() => stem.to_owned(),
        _ => last.to_owned(),
    }
}

/// Names the new item `from` (made empty a moment ago) as typed: a plain name renames it; a path
/// (`notes/2026/idea.md`) makes the folders on the way and moves it there; a name ending in `/`
/// makes it a folder. A taken name is refused and nothing is overwritten. Returns the new path.
pub fn name_new(from: &Path, name: &str) -> Result<PathBuf, String> {
    check_new_name(name)?;
    let (folders, last, trailing) = split_new_name(name);
    let typed;
    let last = if trailing || from.is_dir() {
        last
    } else {
        typed = new_file_name(last);
        &typed
    };
    if folders.is_empty() && !trailing {
        return if from.file_name().is_some_and(|current| current == last) { Ok(from.to_path_buf()) } else { rename(from, last) };
    }
    let base = from.parent().ok_or_else(|| "There is no folder to make it in.".to_string())?;
    if !base.is_dir() {
        return Err(gone_folder(base));
    }
    let parent = folders.iter().fold(base.to_path_buf(), |path, part| path.join(part));
    let to = parent.join(last);
    let was_dir = from.is_dir();
    if to == from {
        return Ok(to);
    }
    if fs::symlink_metadata(&to).is_ok() {
        return Err(taken(&to));
    }
    if trailing && !was_dir && fs::metadata(from).is_ok_and(|meta| meta.len() > 0) {
        return Err("That file has text in it, so it can’t become a folder.".into());
    }
    fs::create_dir_all(&parent).map_err(|err| explain("create", &parent, &err))?;
    if trailing && !was_dir {
        fs::remove_file(from).map_err(|err| explain("rename", from, &err))?;
        fs::create_dir(&to).map_err(|err| explain("create", &to, &err))?;
    } else {
        move_to(from, &to)?;
    }
    Ok(to)
}

/// Renames `from` to `name` in the same folder and returns the new path. A name that is taken,
/// empty or invalid is an error with the words to show; nothing is overwritten. A change of
/// letter case alone is allowed (the volume sees one file there).
pub fn rename(from: &Path, name: &str) -> Result<PathBuf, String> {
    check_name(name)?;
    let to = from.with_file_name(name);
    move_to(from, &to)?;
    Ok(to)
}

unsafe extern "C" {
    /// macOS `renamex_np(2)`: a rename that can refuse to replace, decided by the volume in one step.
    fn renamex_np(from: *const c_char, to: *const c_char, flags: u32) -> std::ffi::c_int;
}

/// `RENAME_EXCL`: fail with `EEXIST` when the target exists.
const RENAME_EXCL: u32 = 4;

/// The refusal shown when `to` is taken, and what `explain` says for an `AlreadyExists`. It is short like the
/// other name refusals, so the refused-name card holds it in a line or two; a long name is cut in the
/// middle (finch#5frc). A path with no name says it without one.
fn taken(to: &Path) -> String {
    match to.file_name() {
        Some(name) => format!("\u{201C}{}\u{201D} is already taken.", crate::explorer::cut_middle(&name.to_string_lossy(), 22)),
        None => "That name is already taken.".to_owned(),
    }
}

/// Moves `from` to `to`, refusing to replace anything.
pub fn move_to(from: &Path, to: &Path) -> Result<(), String> {
    // A change of letter case alone is fine when both names are the one file (a case-insensitive
    // volume); on a case-sensitive one a file with the other name is a different file.
    let same_file = || match (fs::symlink_metadata(from), fs::symlink_metadata(to)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
        _ => false,
    };
    let case_only = from.parent() == to.parent()
        && from.file_name().zip(to.file_name()).is_some_and(|(a, b)| a != b && a.to_string_lossy().to_lowercase() == b.to_string_lossy().to_lowercase());
    if case_only && same_file() {
        return fs::rename(from, to).map_err(|err| explain("rename", from, &err));
    }
    // The volume refuses a taken name inside the rename itself, so a file made a moment ago is
    // never replaced. Only a volume without `RENAME_EXCL` falls back to check, then rename.
    if let (Ok(src), Ok(dst)) = (CString::new(from.as_os_str().as_bytes()), CString::new(to.as_os_str().as_bytes())) {
        if unsafe { renamex_np(src.as_ptr(), dst.as_ptr(), RENAME_EXCL) } == 0 {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        match err.raw_os_error() {
            // macOS errno: EEXIST 17; ENOTSUP 45, EINVAL 22, ENOSYS 78 mean the volume can't do it.
            Some(17) => return Err(taken(to)),
            Some(45 | 22 | 78) => {}
            _ => return Err(explain("move", from, &err)),
        }
    }
    if fs::symlink_metadata(to).is_ok() {
        return Err(taken(to));
    }
    fs::rename(from, to).map_err(|err| explain("move", from, &err))
}

/// Moves `from` into the folder `dir` under its own name and returns the new path. Refused, with
/// the words to show: into itself or a folder of its own, into the folder it is already in, and
/// onto a name that is taken there (nothing is overwritten).
pub fn move_into(from: &Path, dir: &Path) -> Result<PathBuf, String> {
    let name = from.file_name().ok_or_else(|| "Nothing to move.".to_string())?;
    let shown = name.to_string_lossy();
    if dir.starts_with(from) {
        return Err(format!("\u{201C}{shown}\u{201D} can’t be moved into itself."));
    }
    if from.parent() == Some(dir) {
        return Err(format!("\u{201C}{shown}\u{201D} is already in that folder."));
    }
    let to = dir.join(name);
    move_to(from, &to)?;
    Ok(to)
}

/// Refuses, with the words to show, copying `from` into itself or a folder of its own.
pub fn check_copy_into(from: &Path, dir: &Path) -> Result<(), String> {
    let name = from.file_name().ok_or_else(|| "Nothing to copy.".to_string())?;
    let inside = match (fs::canonicalize(dir), fs::canonicalize(from)) {
        (Ok(dir), Ok(from)) => dir.starts_with(from),
        _ => dir.starts_with(from),
    };
    if inside {
        return Err(format!("\u{201C}{}\u{201D} can’t be copied into itself.", name.to_string_lossy()));
    }
    Ok(())
}

/// Copies `from` into the folder `dir` and returns the copy's path: under its own name, else
/// `name 2`, so nothing is overwritten. A folder can't be copied into itself or a folder of its own.
pub fn copy_into(from: &Path, dir: &Path) -> Result<PathBuf, String> {
    check_copy_into(from, dir)?;
    let name = from.file_name().ok_or_else(|| "Nothing to copy.".to_string())?;
    copy_to(from, free_name(dir, &name.to_string_lossy(), from.is_dir()))
}

/// Pastes `from` into the folder `dir` and returns the copy's path: under its own name, else as
/// Finder names a paste, `name copy`, `name copy 2`, so nothing is overwritten. When `dir` is `from`
/// itself or inside it, the paste goes beside `from` instead, as Finder does.
pub fn paste_into(from: &Path, dir: &Path) -> Result<PathBuf, String> {
    let name = from.file_name().ok_or_else(|| "Nothing to copy.".to_string())?;
    let dir = match check_copy_into(from, dir) {
        Ok(()) => dir,
        Err(_) => from.parent().ok_or_else(|| "Nothing to copy.".to_string())?,
    };
    let name = name.to_string_lossy();
    let first = dir.join(&*name);
    let to = if first.exists() { copy_name(dir, &name, from.is_dir()) } else { first };
    copy_to(from, to)
}

/// Copies `from` to the free path `to`. A copy that stopped partway leaves no half folder behind
/// (a name taken meanwhile is someone else's, and stays).
fn copy_to(from: &Path, to: PathBuf) -> Result<PathBuf, String> {
    let existed = fs::symlink_metadata(&to).is_ok();
    if let Err(err) = copy_tree(from, &to) {
        if !existed && err.kind() != io::ErrorKind::AlreadyExists && fs::symlink_metadata(&to).is_ok() {
            let _ = trash(&to);
        }
        return Err(if err.kind() == io::ErrorKind::AlreadyExists { taken(&to) } else { explain("copy", from, &err) });
    }
    Ok(to)
}

/// Copies the file or folder `path` next to itself under its copy name and returns that path.
pub fn duplicate(path: &Path) -> Result<PathBuf, String> {
    let to = copy_path(path).ok_or_else(|| "Nothing to duplicate.".to_string())?;
    let existed = fs::symlink_metadata(&to).is_ok();
    if let Err(err) = copy_tree(path, &to) {
        if !existed && err.kind() != io::ErrorKind::AlreadyExists && fs::symlink_metadata(&to).is_ok() {
            let _ = trash(&to);
        }
        return Err(if err.kind() == io::ErrorKind::AlreadyExists { taken(&to) } else { explain("copy", path, &err) });
    }
    Ok(to)
}

/// Makes the copy of the file `path` as `duplicate` names it, but fills it with what `write` puts
/// there (the text of a tab with unsaved edits) instead of the bytes on disk. The copy starts as a
/// new empty file, never over an existing one (a name taken meanwhile is picked past); a failed `write` takes the half-made
/// copy away again. `path` itself is never touched.
pub fn duplicate_text(path: &Path, write: impl FnOnce(&Path) -> Result<(), String>) -> Result<PathBuf, String> {
    // Two quick duplicates can pick the same name: the one that loses the create picks again. A
    // name `copy_path` sees as free but can't create (a dangling link) would come back every time,
    // so the tries are bounded.
    let mut tries = 0;
    let to = loop {
        let to = copy_path(path).ok_or_else(|| "Nothing to duplicate.".to_string())?;
        match fs::OpenOptions::new().write(true).create_new(true).open(&to) {
            Ok(_) => break to,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists && tries < 8 => tries += 1,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Err(taken(&to)),
            Err(err) => return Err(explain("copy", path, &err)),
        }
    };
    if let Err(why) = write(&to) {
        let _ = fs::remove_file(&to);
        return Err(why);
    }
    Ok(to)
}

unsafe extern "C" {
    /// macOS `clonefile(2)`: one call copy-on-write clones a file or a whole folder tree on APFS.
    fn clonefile(src: *const std::ffi::c_char, dst: *const std::ffi::c_char, flags: u32) -> std::ffi::c_int;
}

/// `CLONE_NOFOLLOW`: a symlink is cloned as a link, not followed.
const CLONE_NOFOLLOW: u32 = 1;

/// Clones `from` to `to` in one call (APFS). `Ok(false)` when the volume can't (or the clone
/// failed with nothing created), so the caller copies by hand; a name already taken is an error.
fn clone_tree(from: &Path, to: &Path) -> io::Result<bool> {
    let (Ok(src), Ok(dst)) = (CString::new(from.as_os_str().as_bytes()), CString::new(to.as_os_str().as_bytes())) else {
        return Ok(false);
    };
    if unsafe { clonefile(src.as_ptr(), dst.as_ptr(), CLONE_NOFOLLOW) } == 0 {
        return Ok(true);
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::AlreadyExists || fs::symlink_metadata(to).is_ok() {
        return Err(err);
    }
    Ok(false)
}

fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    if clone_tree(from, to)? {
        return Ok(());
    }
    copy_tree_by_hand(from, to)
}

fn copy_tree_by_hand(from: &Path, to: &Path) -> io::Result<()> {
    let kind = fs::symlink_metadata(from)?.file_type();
    if kind.is_symlink() {
        std::os::unix::fs::symlink(fs::read_link(from)?, to)
    } else if kind.is_dir() {
        fs::create_dir(to)?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_tree_by_hand(&entry.path(), &to.join(entry.file_name()))?;
        }
        Ok(())
    } else {
        fs::copy(from, to).map(drop)
    }
}

/// Moves `path` to the system Trash (so Finder's Put Back works) and returns where Trash put it.
pub fn trash(path: &Path) -> Result<PathBuf, String> {
    let bytes = CString::new(path.as_os_str().as_bytes()).map_err(|_| "The path has a null byte.".to_string())?;
    unsafe {
        let string: Retained<AnyObject> = msg_send![class!(NSString), stringWithUTF8String: bytes.as_ptr()];
        let url: Retained<AnyObject> = msg_send![class!(NSURL), fileURLWithPath: &*string];
        let manager: Retained<AnyObject> = msg_send![class!(NSFileManager), defaultManager];
        let mut result: *mut AnyObject = ptr::null_mut();
        let mut error: *mut AnyObject = ptr::null_mut();
        let done: Bool = msg_send![&*manager, trashItemAtURL: &*url, resultingItemURL: &mut result, error: &mut error];
        if !done.as_bool() {
            let reason = match error.as_ref() {
                Some(error) => {
                    let text: Retained<AnyObject> = msg_send![error, localizedDescription];
                    ns_string(&text)
                }
                None => format!("\u{201C}{}\u{201D} couldn’t be moved to the Trash.", path.file_name().unwrap_or_default().to_string_lossy()),
            };
            return Err(reason);
        }
        let url = result.as_ref().ok_or_else(|| "The Trash didn’t say where it put the item.".to_string())?;
        let at: Retained<AnyObject> = msg_send![url, path];
        Ok(PathBuf::from(ns_string(&at)))
    }
}

unsafe fn ns_string(string: &AnyObject) -> String {
    let bytes: *const c_char = unsafe { msg_send![string, UTF8String] };
    if bytes.is_null() { String::new() } else { unsafe { CStr::from_ptr(bytes) }.to_string_lossy().into_owned() }
}

/// Takes away what an undo of a new item or a copy leaves: an empty file or folder is removed
/// outright, anything with content goes to the Trash, so nothing typed into it is lost for good.
pub fn discard(path: &Path) -> Result<Option<PathBuf>, String> {
    let empty = match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => fs::read_dir(path).map(|mut entries| entries.next().is_none()).unwrap_or(false),
        Ok(meta) => meta.is_file() && meta.len() == 0,
        Err(err) => return Err(explain("remove", path, &err)),
    };
    if empty {
        let gone = if path.is_dir() { fs::remove_dir(path) } else { fs::remove_file(path) };
        gone.map(|()| None).map_err(|err| explain("remove", path, &err))
    } else {
        trash(path).map(Some)
    }
}

/// Where `path` stands once `from` is moved to `to`: the new path for `from` itself and for what
/// is under it, `None` for any other path.
pub fn moved_path(path: &Path, from: &Path, to: &Path) -> Option<PathBuf> {
    let rest = path.strip_prefix(from).ok()?;
    Some(if rest.as_os_str().is_empty() { to.to_path_buf() } else { to.join(rest) })
}

/// The paths of `paths` that are `target` or under it.
pub fn under(paths: &HashSet<PathBuf>, target: &Path) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = paths.iter().filter(|path| path.starts_with(target)).cloned().collect();
    found.sort();
    found
}

impl Workspace {
    /// A file or folder was renamed: every open document at `from`, or under it, moves to the
    /// same place under `to`. Text, undo history and unsaved edits stay as they were.
    pub(crate) fn retarget_tabs(&mut self, from: &Path, to: &Path, cx: &mut Context<Self>) {
        for document in self.documents(cx) {
            document.update(cx, |document: &mut Document, cx| document.retarget(from, to, cx));
        }
    }

    /// A file or folder is gone (trashed, or taken away by an undo): its tabs close, a draft's
    /// too (its file is forgotten, not rewritten). What they held unsaved was already asked
    /// about, so they close without asking again.
    pub(crate) fn close_tabs_under(&mut self, path: &Path, window: &mut Window, cx: &mut Context<Self>) {
        for document in self.documents(cx) {
            if document.read(cx).tab_file().is_some_and(|open| open.starts_with(path)) {
                document.update(cx, |document, _| document.discard_edits());
                self.close_document(document.entity_id(), window, cx);
            }
        }
    }
}

/// The other windows' workspaces, which can be read while `window` is being updated.
fn others(window: AnyWindowHandle, cx: &App) -> Vec<(AnyWindowHandle, gpui_kit::Entity<Workspace>)> {
    open_workspaces_except(Some(window), cx)
}

/// Renames reach the tabs of every other window too (the window that did it moved its own).
pub(crate) fn retarget_elsewhere(window: AnyWindowHandle, from: &Path, to: &Path, cx: &mut App) {
    for (_, workspace) in others(window, cx) {
        workspace.update(cx, |workspace, cx| workspace.retarget_tabs(from, to, cx));
    }
}

/// Closes the tabs under `path` in every other window.
pub(crate) fn close_elsewhere(window: AnyWindowHandle, path: &Path, cx: &mut App) {
    for (handle, workspace) in others(window, cx) {
        cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.close_tabs_under(path, window, cx))).ok();
    }
}

/// Files at or under `target` with unsaved edits in the other windows.
pub fn unsaved_elsewhere(window: AnyWindowHandle, target: &Path, cx: &App) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = others(window, cx)
        .into_iter()
        .flat_map(|(_, workspace)| workspace.read(cx).documents(cx))
        .filter_map(|document| {
            let document = document.read(cx);
            document.path.clone().filter(|path| document.shows_dot() && path.starts_with(target))
        })
        .collect();
    found.sort();
    found
}

/// Every document open in any window. The window being updated can't be read by handle, so it
/// is reached through its root.
fn open_documents(window: &Window, cx: &App) -> Vec<Entity<Document>> {
    let own = window.root::<Root>().flatten().and_then(|root| root.read(cx).view().clone().downcast::<Workspace>().ok());
    let others = open_workspaces_except(Some(window.window_handle()), cx).into_iter().map(|(_, workspace)| workspace);
    own.into_iter().chain(others).flat_map(|workspace| workspace.read(cx).documents(cx)).collect()
}

/// Why a document with `path` (saving or not) and `draft` file stops `targets` being renamed,
/// moved or trashed, or `None`. A file with a save writing would come back at its old name when
/// the write lands, so it is refused until the save is done. With `draft_files`, an open
/// Untitled page whose draft file is at or under a target is refused too: removing the file
/// closes the page and loses what it held since its last write.
fn busy_reason(path: Option<&Path>, saving: bool, draft: Option<&Path>, targets: &[PathBuf], draft_files: bool) -> Option<String> {
    let name = |path: &Path| path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    targets.iter().find_map(|target| {
        if let Some(path) = path.filter(|path| saving && path.starts_with(target)) {
            return Some(format!("\u{201C}{}\u{201D} is still being saved. Try again in a moment.", name(path)));
        }
        let draft = draft.filter(|draft| draft_files && draft.starts_with(target))?;
        Some(format!("\u{201C}{}\u{201D} is where an open Untitled page keeps its text. Close the page first, then try again.", name(draft)))
    })
}

fn first_busy(documents: impl IntoIterator<Item = Entity<Document>>, targets: &[PathBuf], draft_files: bool, cx: &App) -> Result<(), String> {
    for document in documents {
        let document = document.read(cx);
        if let Some(why) = busy_reason(document.path.as_deref(), document.is_saving(), document.draft_path(), targets, draft_files) {
            return Err(why);
        }
    }
    Ok(())
}

/// Why `targets` (or what is under them) can't be renamed, moved or trashed right now, or `Ok`,
/// by the documents open in every window (see `busy_reason`). `window` is the one being updated.
pub fn refuse_busy(window: &Window, targets: &[PathBuf], draft_files: bool, cx: &App) -> Result<(), String> {
    first_busy(open_documents(window, cx), targets, draft_files, cx)
}

impl Workspace {
    /// `refuse_busy` for a rename or move of the showing tab's file, called while this workspace
    /// is being updated in `window`: its own documents are read through `self`, the other
    /// windows' by handle (the window being updated can't be read by handle).
    pub(crate) fn refuse_busy_file(&self, target: &Path, window: &Window, cx: &App) -> Result<(), String> {
        let others = open_workspaces_except(Some(window.window_handle()), cx).into_iter().flat_map(|(_, workspace)| workspace.read(cx).documents(cx));
        first_busy(self.documents(cx).into_iter().chain(others), std::slice::from_ref(&target.to_path_buf()), false, cx)
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{NEW_FILE_NAME, NEW_FOLDER_NAME, copy_into, create, discard, duplicate, duplicate_text, explain, failed_title, move_into, move_to, moved_path, name_new, paste_into, rename, trash};

    #[test]
    fn explain_says_what_happened_and_what_to_try() {
        let path = std::path::Path::new("/tmp/notes/a.md");
        let said = |err: std::io::Error| explain("save", path, &err);
        assert_eq!(
            said(std::io::Error::from_raw_os_error(13)),
            "You don’t have permission to save “a.md” in this folder. Choose another place or change its permissions in Finder."
        );
        for doing in ["open", "read", "unlock"] {
            assert_eq!(
                explain(doing, path, &std::io::Error::from_raw_os_error(13)),
                format!("You don’t have permission to {doing} “a.md”. Check its permissions in Finder (Get Info).")
            );
        }
        for doing in ["create", "move", "copy", "export"] {
            assert_eq!(
                explain(doing, path, &std::io::Error::from_raw_os_error(1)),
                format!("You don’t have permission to {doing} “a.md” in this folder. Choose another place or change its permissions in Finder.")
            );
        }
        assert_eq!(
            said(std::io::Error::from_raw_os_error(2)),
            "“a.md” or its folder isn’t there anymore. Check that it wasn’t moved or deleted, then try again."
        );
        assert_eq!(said(std::io::Error::from_raw_os_error(28)), "The disk is full. Free up some space, then try to save “a.md” again.");
        assert_eq!(
            said(std::io::Error::from_raw_os_error(30)),
            "The disk is read-only, so “a.md” can’t be changed there. Choose a folder on another disk."
        );
        assert_eq!(
            said(std::io::Error::new(std::io::ErrorKind::InvalidData, "stream did not contain valid UTF-8")),
            "“a.md” isn’t a text file, so it can’t be shown here."
        );
        assert_eq!(said(std::io::Error::from_raw_os_error(17)), "“a.md” is already taken.");
        assert_eq!(
            explain("create", std::path::Path::new("/tmp/notes/a-very-long-name-for-a-note.md"), &std::io::Error::from_raw_os_error(17)),
            "“a-very-long-…a-note.md” is already taken."
        );
        assert_eq!(explain("create", std::path::Path::new("/"), &std::io::Error::from_raw_os_error(17)), "That name is already taken.");
        assert_eq!(
            said(std::io::Error::from_raw_os_error(27)),
            "“a.md” is too large to save here. Choose another place with more room."
        );
        assert_eq!(said(std::io::Error::from_raw_os_error(21)), "A folder now has that name. Use Save as… to save it somewhere else.");
        for doing in ["open", "read", "move", "rename", "copy", "remove"] {
            assert_eq!(
                explain(doing, path, &std::io::Error::from_raw_os_error(21)),
                format!("Couldn’t {doing} “a.md”. Check that it’s still there and try again.")
            );
        }
        let other = said(std::io::Error::other("boom"));
        assert_eq!(other, "Couldn’t save “a.md”. Check that it’s still there and try again.");
        assert!(!other.contains("os error") && !other.contains("boom"));
    }

    #[test]
    fn failed_title_names_the_file() {
        let one = [PathBuf::from("/tmp/notes/a.md")];
        assert_eq!(failed_title("duplicate", &one), "Couldn’t duplicate “a.md”");
        assert_eq!(format!("{} to the Trash", failed_title("move", &one)), "Couldn’t move “a.md” to the Trash");
        let two = [PathBuf::from("/tmp/a.md"), PathBuf::from("/tmp/b.md")];
        assert_eq!(failed_title("copy", &two), "Couldn’t copy 2 items");
    }

    #[test]
    fn a_paste_keeps_its_name_then_takes_copy_and_copy_2() {
        let dir = scratch("paste");
        let other = scratch("paste-from");
        let source = other.join("note.txt");
        fs::write(&source, "one").unwrap();
        let first = paste_into(&source, &dir).unwrap();
        assert_eq!(first, dir.join("note.txt"));
        let second = paste_into(&source, &dir).unwrap();
        assert_eq!(second, dir.join("note copy.txt"));
        let third = paste_into(&source, &dir).unwrap();
        assert_eq!(third, dir.join("note copy 2.txt"));
        // Pasting a file into its own folder is a copy beside it, and nothing is overwritten.
        assert_eq!(paste_into(&first, &dir).unwrap(), dir.join("note copy 3.txt"));
        assert_eq!(fs::read_to_string(dir.join("note.txt")).unwrap(), "one");
        // A folder is named the same way, and cannot go inside itself.
        let folder = other.join("pics");
        fs::create_dir(&folder).unwrap();
        assert_eq!(paste_into(&folder, &dir).unwrap(), dir.join("pics"));
        assert_eq!(paste_into(&folder, &dir).unwrap(), dir.join("pics copy"));
        // A target that is the item or inside it pastes beside the item, as Finder does.
        assert_eq!(paste_into(&folder, &folder).unwrap(), other.join("pics copy"));
        fs::create_dir(folder.join("deep")).unwrap();
        assert_eq!(paste_into(&folder, &folder.join("deep")).unwrap(), other.join("pics copy 2"));
        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&other);
    }

    /// A fresh empty folder under the system temp dir.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("finch-file-ops-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Opening a file in a tab keeps its text in the editor; this stands in for that buffer, since
    /// a `Document` needs a window. The document side of a rename is `moved_path` (what
    /// `Document::retarget` applies to its path) and nothing else: no write, no reload.
    struct OpenTab {
        path: PathBuf,
        text: String,
        unsaved: bool,
    }

    #[test]
    fn rename_with_an_open_unsaved_tab_moves_the_file_and_keeps_the_edits() {
        let dir = scratch("rename-open");
        let file = dir.join("notes.txt");
        fs::write(&file, "on disk").unwrap();
        let mut tab = OpenTab { path: file.clone(), text: "on disk, plus edits".into(), unsaved: true };

        let to = rename(&file, "renamed.txt").unwrap();
        tab.path = moved_path(&tab.path, &file, &to).unwrap();

        assert_eq!(tab.path, dir.join("renamed.txt"));
        assert_eq!(tab.text, "on disk, plus edits");
        assert!(tab.unsaved);
        assert!(!file.exists());
        // Nothing was written over the file: it holds what it held.
        assert_eq!(fs::read_to_string(&to).unwrap(), "on disk");
    }

    #[test]
    fn renaming_a_folder_moves_the_tabs_under_it() {
        let dir = scratch("rename-folder");
        fs::create_dir_all(dir.join("src/inner")).unwrap();
        fs::write(dir.join("src/inner/a.rs"), "a").unwrap();
        let to = rename(&dir.join("src"), "code").unwrap();
        assert_eq!(moved_path(&dir.join("src/inner/a.rs"), &dir.join("src"), &to), Some(dir.join("code/inner/a.rs")));
        assert_eq!(moved_path(&dir.join("srcs/a.rs"), &dir.join("src"), &to), None);
        assert_eq!(fs::read_to_string(dir.join("code/inner/a.rs")).unwrap(), "a");
    }

    #[test]
    fn rename_refuses_a_taken_empty_or_invalid_name() {
        let dir = scratch("rename-refused");
        fs::write(dir.join("a.txt"), "a").unwrap();
        fs::write(dir.join("b.txt"), "b").unwrap();
        assert!(rename(&dir.join("a.txt"), "b.txt").is_err());
        assert!(rename(&dir.join("a.txt"), "").is_err());
        assert!(rename(&dir.join("a.txt"), "x/y").is_err());
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "a");
        assert_eq!(fs::read_to_string(dir.join("b.txt")).unwrap(), "b");
        // The same name, or only its letter case, is fine.
        assert!(rename(&dir.join("a.txt"), "A.txt").is_ok());
    }

    #[test]
    fn trash_of_an_open_file_moves_it_to_the_trash_and_keeps_the_text() {
        let dir = scratch("trash-open");
        let file = dir.join("trash me.txt");
        fs::write(&file, "keep this").unwrap();
        let trashed = trash(&file).unwrap();
        assert!(!file.exists());
        assert!(trashed.exists(), "{} should exist", trashed.display());
        assert!(trashed.to_string_lossy().contains(".Trash"));
        assert_eq!(fs::read_to_string(&trashed).unwrap(), "keep this");
        // Put it back, as undo does.
        move_to(&trashed, &file).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "keep this");
    }

    #[test]
    fn copies_are_named_as_finder_names_them() {
        let dir = scratch("copies");
        fs::write(dir.join("name.ext"), "x").unwrap();
        let first = duplicate(&dir.join("name.ext")).unwrap();
        assert_eq!(first, dir.join("name copy.ext"));
        assert_eq!(duplicate(&dir.join("name.ext")).unwrap(), dir.join("name copy 2.ext"));
        assert_eq!(fs::read_to_string(first).unwrap(), "x");
        fs::create_dir(dir.join("pics")).unwrap();
        fs::write(dir.join("pics/a.png"), "p").unwrap();
        let folder = duplicate(&dir.join("pics")).unwrap();
        assert_eq!(folder, dir.join("pics copy"));
        assert_eq!(fs::read_to_string(folder.join("a.png")).unwrap(), "p");
    }

    #[test]
    fn a_text_copy_gets_the_written_text_and_leaves_the_original_alone() {
        let dir = scratch("text-copy");
        let file = dir.join("name.ext");
        fs::write(&file, "disk").unwrap();
        let copy = duplicate_text(&file, |to| fs::write(to, "edits").map_err(|err| err.to_string())).unwrap();
        assert_eq!(copy, dir.join("name copy.ext"));
        assert_eq!(fs::read_to_string(&copy).unwrap(), "edits");
        assert_eq!(fs::read_to_string(&file).unwrap(), "disk");
        // A write that fails leaves no copy behind, and says why.
        let said = duplicate_text(&file, |_| Err("no room".to_string())).unwrap_err();
        assert_eq!(said, "no room");
        assert!(!dir.join("name copy 2.ext").exists());
    }

    #[test]
    fn new_items_take_a_free_name_and_discard_removes_empty_ones_only() {
        let dir = scratch("new-items");
        let first = create(&dir, NEW_FILE_NAME, false).unwrap();
        let second = create(&dir, NEW_FILE_NAME, false).unwrap();
        assert_eq!(first, dir.join("untitled.txt"));
        assert_eq!(second, dir.join("untitled 2.txt"));
        let folder = create(&dir, NEW_FOLDER_NAME, true).unwrap();
        assert_eq!(folder, dir.join("untitled folder"));
        assert_eq!(discard(&first).unwrap(), None);
        assert!(!first.exists());
        fs::write(&second, "typed").unwrap();
        let trashed = discard(&second).unwrap().expect("content goes to the Trash");
        assert!(!second.exists());
        assert_eq!(fs::read_to_string(&trashed).unwrap(), "typed");
        // Leave the real Trash as it was.
        fs::remove_file(trashed).unwrap();
    }

    #[test]
    fn a_new_file_keeps_its_own_extension_and_drops_only_the_fields_opening_txt() {
        let dir = scratch("new-extension");
        for (typed, made) in [
            ("idea.md.txt", "idea.md"),
            ("page.html", "page.html"),
            ("Makefile.am.txt", "Makefile.am"),
            ("notes.txt", "notes.txt"),
            ("todo", "todo"),
            (".env", ".env"),
            (".rc.txt", ".rc"),
            ("sub/deep.rs.txt", "sub/deep.rs"),
            ("..txt", "..txt"),
            ("sub/plain", "sub/plain"),
        ] {
            let from = create(&dir, NEW_FILE_NAME, false).unwrap();
            assert_eq!(name_new(&from, typed).unwrap(), dir.join(made), "{typed}");
            assert!(dir.join(made).is_file());
        }
        // A folder keeps its name, and a trailing slash makes one without `.txt`.
        let folder = create(&dir, NEW_FOLDER_NAME, true).unwrap();
        assert_eq!(name_new(&folder, "assets").unwrap(), dir.join("assets"));
        let from = create(&dir, NEW_FILE_NAME, false).unwrap();
        assert_eq!(name_new(&from, "docs/").unwrap(), dir.join("docs"));
        assert!(dir.join("docs").is_dir());
    }

    #[test]
    fn a_case_change_never_replaces_a_different_file() {
        let dir = scratch("case");
        fs::write(dir.join("a.txt"), "lower").unwrap();
        fs::write(dir.join("A.txt"), "upper").unwrap();
        // On a case-insensitive volume the two names are one file; nothing to test then.
        if fs::read_to_string(dir.join("a.txt")).unwrap() != "lower" {
            return;
        }
        assert!(rename(&dir.join("a.txt"), "A.txt").is_err());
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "lower");
        assert_eq!(fs::read_to_string(dir.join("A.txt")).unwrap(), "upper");
    }

    #[test]
    fn moving_an_open_unsaved_file_into_a_folder_keeps_the_edits_and_overwrites_nothing() {
        let dir = scratch("move-open");
        fs::create_dir(dir.join("inbox")).unwrap();
        let file = dir.join("notes.txt");
        fs::write(&file, "on disk").unwrap();
        let mut tab = OpenTab { path: file.clone(), text: "on disk, plus edits".into(), unsaved: true };

        let to = move_into(&file, &dir.join("inbox")).unwrap();
        tab.path = moved_path(&tab.path, &file, &to).unwrap();

        assert_eq!(tab.path, dir.join("inbox/notes.txt"));
        assert_eq!(tab.text, "on disk, plus edits");
        assert!(tab.unsaved);
        assert!(!file.exists());
        assert_eq!(fs::read_to_string(&to).unwrap(), "on disk");
        // Undo moves it back the same way.
        move_to(&to, &file).unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "on disk");
    }

    #[test]
    fn a_move_onto_a_taken_name_is_refused_and_both_files_stay() {
        let dir = scratch("move-taken");
        fs::create_dir(dir.join("inbox")).unwrap();
        fs::write(dir.join("a.txt"), "mine").unwrap();
        fs::write(dir.join("inbox/a.txt"), "theirs").unwrap();
        assert!(move_into(&dir.join("a.txt"), &dir.join("inbox")).is_err());
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "mine");
        assert_eq!(fs::read_to_string(dir.join("inbox/a.txt")).unwrap(), "theirs");
    }

    #[test]
    fn a_new_item_can_be_named_with_a_path_but_a_rename_cannot() {
        let dir = scratch("new-path");
        let made = create(&dir, "notes/2026/idea.md", false).unwrap();
        assert_eq!(made, dir.join("notes/2026/idea.md"));
        assert!(made.is_file());
        let folder = create(&dir, "newdir/", false).unwrap();
        assert!(folder.is_dir() && folder == dir.join("newdir"));
        for bad in ["../x", "a/../x", "a//b", "/abs", "./x"] {
            assert!(create(&dir, bad, false).is_err(), "{bad}");
        }
        assert!(!dir.parent().unwrap().join("x").exists());
        // The placeholder of a new item is named with a path the same way.
        let placeholder = create(&dir, NEW_FILE_NAME, false).unwrap();
        let named = name_new(&placeholder, "docs/a/b.txt").unwrap();
        assert_eq!(named, dir.join("docs/a/b.txt"));
        assert!(named.is_file() && !placeholder.exists());
        let placeholder = create(&dir, NEW_FILE_NAME, false).unwrap();
        let named = name_new(&placeholder, "made/").unwrap();
        assert!(named.is_dir() && !placeholder.exists());
        // A rename still takes no slash.
        fs::write(dir.join("a.txt"), "a").unwrap();
        assert!(rename(&dir.join("a.txt"), "x/b.txt").is_err());
        assert!(dir.join("a.txt").exists());
    }

    #[test]
    fn a_new_item_in_a_folder_that_is_gone_is_refused_and_the_folder_stays_gone() {
        let dir = scratch("new-gone").join("project");
        let err = create(&dir, NEW_FILE_NAME, false).unwrap_err();
        assert_eq!(err, "The folder \u{201C}project\u{201D} isn’t there anymore.");
        assert!(create(&dir, "notes/idea.md", false).is_err());
        assert!(!dir.exists());
        // A new item made before its folder was deleted is not named into a recreated folder.
        assert_eq!(name_new(&dir.join("untitled folder"), "a/b").unwrap_err(), "The folder “project” isn’t there anymore.");
        assert!(!dir.exists());
    }

    #[test]
    fn a_folder_is_not_moved_or_copied_into_itself_or_its_own_child() {
        let dir = scratch("move-self");
        fs::create_dir_all(dir.join("top/child")).unwrap();
        fs::write(dir.join("top/child/a.txt"), "a").unwrap();
        assert!(move_into(&dir.join("top"), &dir.join("top")).is_err());
        assert!(move_into(&dir.join("top"), &dir.join("top/child")).is_err());
        assert!(copy_into(&dir.join("top"), &dir.join("top/child")).is_err());
        assert_eq!(fs::read_to_string(dir.join("top/child/a.txt")).unwrap(), "a");
        // Into the folder it is already in moves nothing.
        assert!(move_into(&dir.join("top/child"), &dir.join("top")).is_err());
    }

    #[test]
    fn copying_into_a_folder_keeps_the_original_and_never_overwrites() {
        let dir = scratch("copy-into");
        fs::create_dir(dir.join("inbox")).unwrap();
        fs::write(dir.join("a.txt"), "mine").unwrap();
        fs::write(dir.join("inbox/a.txt"), "theirs").unwrap();
        let copy = copy_into(&dir.join("a.txt"), &dir.join("inbox")).unwrap();
        assert_eq!(copy, dir.join("inbox/a 2.txt"));
        assert_eq!(fs::read_to_string(&copy).unwrap(), "mine");
        assert_eq!(fs::read_to_string(dir.join("a.txt")).unwrap(), "mine");
        assert_eq!(fs::read_to_string(dir.join("inbox/a.txt")).unwrap(), "theirs");
        // A folder copies with what is in it.
        fs::create_dir_all(dir.join("pics/deep")).unwrap();
        fs::write(dir.join("pics/deep/p.png"), "p").unwrap();
        let folder = copy_into(&dir.join("pics"), &dir.join("inbox")).unwrap();
        assert_eq!(folder, dir.join("inbox/pics"));
        assert_eq!(fs::read_to_string(folder.join("deep/p.png")).unwrap(), "p");
        assert!(dir.join("pics/deep/p.png").exists());
    }

    #[test]
    fn a_save_in_flight_or_an_open_draft_under_the_target_refuses() {
        use std::path::Path;

        use super::busy_reason;
        let folder = [PathBuf::from("/notes")];
        let file = Some(Path::new("/notes/a.md"));
        // A file whose save is writing, directly or inside a folder, is refused; an idle one is not.
        assert!(busy_reason(file, true, None, &folder, false).unwrap().contains("still being saved"));
        assert!(busy_reason(file, true, None, &[PathBuf::from("/notes/a.md")], false).is_some());
        assert!(busy_reason(file, false, None, &folder, false).is_none());
        assert!(busy_reason(file, true, None, &[PathBuf::from("/other")], false).is_none());
        // An open draft's file is refused only when the action would remove it (`draft_files`).
        let draft = Some(Path::new("/notes/Scrap Drafts/Untitled.txt"));
        assert!(busy_reason(None, false, draft, &folder, true).unwrap().contains("Untitled page"));
        assert!(busy_reason(None, false, draft, &folder, false).is_none());
        assert!(busy_reason(None, false, draft, &[PathBuf::from("/other")], true).is_none());
    }
}
