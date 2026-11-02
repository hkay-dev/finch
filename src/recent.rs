//! The recent files and folders, kept by macOS itself (`NSDocumentController`): the Dock icon's
//! menu lists them, and so does File > Open Recent (`menu.rs`), so the app stores no list of its
//! own. Written against the Objective-C runtime without AppKit's typed bindings, as `menu.rs` does.

use std::{
    ffi::{CStr, c_char},
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use objc2::{class, msg_send, rc::Retained, runtime::AnyObject};

/// How many recent items File > Open Recent shows until `list` has read the Mac's setting.
const DEFAULT_CAP: usize = 10;

/// The cap `list` last read; `keep_existing` runs off the main thread, where the controller is not
/// asked.
static CAP: AtomicUsize = AtomicUsize::new(DEFAULT_CAP);

fn controller() -> Retained<AnyObject> {
    unsafe { msg_send![class!(NSDocumentController), sharedDocumentController] }
}

/// The recent files and folders macOS lists, newest first, as it gives them: not checked to exist
/// (a stat can stall on a sleeping network volume), so `keep_existing` runs off the main thread.
pub fn list() -> Vec<PathBuf> {
    let limit: usize = unsafe { msg_send![&*controller(), maximumRecentDocumentCount] };
    CAP.store(limit, Ordering::Relaxed);
    let urls: Option<Retained<AnyObject>> = unsafe { msg_send![&*controller(), recentDocumentURLs] };
    let Some(urls) = urls else { return Vec::new() };
    let count: usize = unsafe { msg_send![&*urls, count] };
    let mut paths = Vec::new();
    for index in 0..count {
        let url: Option<Retained<AnyObject>> = unsafe { msg_send![&*urls, objectAtIndex: index] };
        let path: Option<Retained<AnyObject>> = url.and_then(|url| unsafe { msg_send![&*url, path] });
        let Some(path) = path else { continue };
        let chars: *const c_char = unsafe { msg_send![&*path, UTF8String] };
        if chars.is_null() {
            continue;
        }
        paths.push(PathBuf::from(unsafe { CStr::from_ptr(chars) }.to_string_lossy().into_owned()));
    }
    paths
}

/// The first `cap` of `paths` that still exist, in order.
fn first_existing(paths: Vec<PathBuf>, cap: usize) -> Vec<PathBuf> {
    paths.into_iter().filter(|path| path.exists()).take(cap).collect()
}

/// The first of `paths` that still exist, as many as `list` last read the Mac's Recent items
/// setting to allow.
pub fn keep_existing(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    first_existing(paths, CAP.load(Ordering::Relaxed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cap_limits_the_listing() {
        let dir = std::env::temp_dir().join(format!("finch-recent-cap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let files: Vec<PathBuf> = (0..5).map(|n| dir.join(format!("{n}.txt"))).collect();
        for file in &files {
            std::fs::write(file, "").unwrap();
        }
        assert_eq!(first_existing(files.clone(), 3), files[..3].to_vec());
        assert_eq!(first_existing(files.clone(), 20), files);
        assert!(first_existing(files, 0).is_empty());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_deleted_recent_drops_out() {
        let dir = std::env::temp_dir().join(format!("finch-recent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (kept, gone) = (dir.join("kept.txt"), dir.join("gone.txt"));
        std::fs::write(&kept, "").unwrap();
        std::fs::write(&gone, "").unwrap();
        let listed = vec![gone.clone(), kept.clone()];
        assert_eq!(first_existing(listed.clone(), 10), listed);
        std::fs::remove_file(&gone).unwrap();
        assert_eq!(first_existing(listed, 10), vec![kept]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

/// Forgets every recent item, in the menu and in the Dock.
pub fn clear() {
    let none: *mut AnyObject = std::ptr::null_mut();
    let _: () = unsafe { msg_send![&*controller(), clearRecentDocuments: none] };
}
