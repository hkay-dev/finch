//! Paste or drop an image into a saved Markdown file: the image lands in an `assets/` folder next
//! to the file and `![](assets/name.png)` goes in at the caret.
//!
//! The name is a slug of the heading the caret is under, else `image-YYYY-MM-DD`, with `-2`, `-3`
//! on a clash; a file is created with `create_new`, so one that exists is never overwritten, even
//! by two pastes racing. The write runs on the background executor and the link goes in when it
//! lands, as one atomic edit, so one Undo takes the link out; the file stays, so Redo works.
//! GPUI's clipboard carries the pasteboard's images (PNG, TIFF and the rest), so there is no
//! pasteboard code here; only a TIFF (a screenshot's usual form) or another bitmap is re-encoded as
//! PNG, through AppKit's `NSBitmapImageRep`, on that same background thread.

use std::{
    cell::RefCell,
    ffi::c_void,
    fs::{self, File, OpenOptions},
    io::{self, Write as _},
    path::{Path, PathBuf},
    rc::Rc,
    time::{SystemTime, UNIX_EPOCH},
};

use gpui_kit::component::input::{EditorState, RopeExt as _};
use gpui_kit::{App, ClipboardEntry, ClipboardItem, Entity, ImageFormat, PromptLevel, Window, WeakEntity};
use objc2::{
    class, msg_send,
    rc::{Retained, autoreleasepool},
    runtime::AnyObject,
};

use crate::{file_ops, language};

/// The path of the file an editor shows, shared with the closure that handles its paste: `None`
/// for an Untitled page. The pane keeps it current as tabs and renames change it.
pub type NotePath = Rc<RefCell<Option<PathBuf>>>;

/// The folder next to the note that the images go in.
const ASSETS: &str = "assets";
/// How long the wash on an inserted link lasts, in seconds: the length of the editor's own paste wash (`WASH_LENGTH` in the vendored element).
const INSERT_WASH: f32 = 0.3;

/// What one image is made of before it is written.
enum Source {
    /// Image bytes off the pasteboard, in the format they arrived in.
    Bytes(Vec<u8>, ImageFormat),
    /// A file dropped on the editor.
    File(PathBuf),
}

/// A saved Markdown file takes pasted images; an Untitled page and any other file don't.
fn is_note(path: &Path) -> bool {
    language::for_path(Some(path)) == "markdown"
}

/// File extensions a drop copies in rather than opens.
fn is_image_file(path: &Path) -> bool {
    let ext = path.extension().and_then(|ext| ext.to_str()).map(str::to_ascii_lowercase);
    matches!(ext.as_deref(), Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "tif" | "tiff" | "bmp" | "svg" | "heic"))
}

/// Whether a drop of `paths` on a pane showing `note` is images for the note: every one an image
/// file and the note a saved Markdown file.
pub fn takes_drop(note: Option<&Path>, paths: &[PathBuf]) -> bool {
    note.is_some_and(is_note) && !paths.is_empty() && paths.iter().all(|path| is_image_file(path) && path.is_file())
}

/// The picture files a paste of `entries` into `note` adds like a drop: files copied in Finder
/// arrive as `ExternalPaths` plus a string of their paths, so this looks before any text check.
/// Empty unless the note is a saved Markdown file and every copied path is an existing image file.
fn copied_pictures(note: Option<&Path>, entries: &[ClipboardEntry]) -> Vec<PathBuf> {
    let Some(paths) = entries.iter().find_map(|entry| match entry {
        ClipboardEntry::ExternalPaths(paths) => Some(paths.paths()),
        _ => None,
    }) else {
        return Vec::new();
    };
    if takes_drop(note, paths) { paths.to_vec() } else { Vec::new() }
}

/// Cmd-V in the editor of `note` with `item` on the pasteboard. True when it took the paste (picture
/// files copied in Finder, or an image and no text, in a saved Markdown file that can be edited);
/// false lets the text paste as before.
pub fn paste(note: &NotePath, editor: &WeakEntity<EditorState>, item: &ClipboardItem, window: &mut Window, cx: &mut App) -> bool {
    let Some(path) = note.borrow().clone().filter(|path| is_note(path)) else { return false };
    let files = copied_pictures(Some(&path), item.entries());
    if !files.is_empty() {
        let Some(editor) = editor.upgrade().filter(|editor| editor.read(cx).is_editable()) else { return false };
        place(editor, path, files.into_iter().map(Source::File).collect(), window, cx);
        return true;
    }
    if item.entries().iter().any(|entry| matches!(entry, ClipboardEntry::String(_))) {
        return false;
    }
    let Some(image) = item.entries().iter().find_map(|entry| match entry {
        ClipboardEntry::Image(image) => Some(image),
        _ => None,
    }) else {
        return false;
    };
    let Some(editor) = editor.upgrade().filter(|editor| editor.read(cx).is_editable()) else { return false };
    place(editor, path, vec![Source::Bytes(image.bytes.clone(), image.format)], window, cx);
    true
}

/// Image files dropped on the editor of the saved Markdown file `note` (`takes_drop` said yes):
/// each is copied into the assets folder, keeping its extension, and its link goes in.
pub fn drop_files(editor: Entity<EditorState>, note: PathBuf, files: Vec<PathBuf>, window: &mut Window, cx: &mut App) {
    if !editor.read(cx).is_editable() {
        return;
    }
    place(editor, note, files.into_iter().map(Source::File).collect(), window, cx);
}

/// Writes `sources` beside `note` off the main thread, then puts their links at the caret.
fn place(editor: Entity<EditorState>, note: PathBuf, sources: Vec<Source>, window: &mut Window, cx: &mut App) {
    let Some(folder) = note.parent().map(|parent| parent.join(ASSETS)) else { return };
    let stem = {
        let state = editor.read(cx);
        heading_slug(state.text().lines_len(), state.cursor_position().line as usize, |row| state.text().slice_line(row).to_string()).unwrap_or_else(today)
    };
    let weak = editor.downgrade();
    window
        .spawn(cx, async move |cx| {
            let written = cx.background_executor().spawn(async move { store(&folder, &stem, sources) }).await;
            match written {
                Ok(names) => {
                    let links: Vec<String> = names.iter().map(|name| format!("![]({ASSETS}/{name})")).collect();
                    weak.update_in(cx, |state, window, cx| {
                        let start = state.cursor();
                        state.insert(links.join("\n"), window, cx);
                        // The line arrives in one frame: wash what went in, once, as a paste's text does.
                        state.wash_range(start..state.cursor(), INSERT_WASH, cx);
                    })
                    .ok();
                }
                Err((path, err)) => {
                    let detail = file_ops::explain("save", &path, &err);
                    cx.update(|window, cx| {
                        let _ = window.prompt(PromptLevel::Critical, &file_ops::failed_title("save", &[path.clone()]), Some(&detail), &["OK"], cx);
                    })
                    .ok();
                }
            }
        })
        .detach();
}

/// Writes each source into `folder` as `stem.ext`, `stem-2.ext`, ...: the first name nothing has.
/// Gives the names written. A failure gives the path it failed on and why; the files written
/// before it stay.
fn store(folder: &Path, stem: &str, sources: Vec<Source>) -> Result<Vec<String>, (PathBuf, io::Error)> {
    fs::create_dir_all(folder).map_err(|err| (folder.to_path_buf(), err))?;
    let mut names = Vec::new();
    for source in sources {
        let (ext, data) = match source {
            Source::Bytes(bytes, format) => match format {
                ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::Gif | ImageFormat::Webp | ImageFormat::Svg => (format_extension(format).to_owned(), Data::Bytes(bytes)),
                _ => match to_png(&bytes) {
                    Some(png) => ("png".to_owned(), Data::Bytes(png)),
                    None => return Err((folder.to_path_buf(), io::Error::new(io::ErrorKind::InvalidData, "not an image"))),
                },
            },
            Source::File(path) => {
                let ext = path.extension().and_then(|ext| ext.to_str()).map_or_else(|| "png".to_owned(), str::to_ascii_lowercase);
                (ext, Data::File(path))
            }
        };
        let mut n = 1;
        let (name, path, mut file) = loop {
            let name = if n == 1 { format!("{stem}.{ext}") } else { format!("{stem}-{n}.{ext}") };
            let path = folder.join(&name);
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(file) => break (name, path, file),
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => n += 1,
                Err(err) => return Err((path, err)),
            }
        };
        let wrote = match data {
            Data::Bytes(bytes) => file.write_all(&bytes),
            Data::File(from) => File::open(&from).and_then(|mut from| io::copy(&mut from, &mut file)).map(|_| ()),
        };
        if let Err(err) = wrote.and_then(|()| file.flush()) {
            drop(file);
            let _ = fs::remove_file(&path);
            return Err((path, err));
        }
        names.push(name);
    }
    Ok(names)
}

enum Data {
    Bytes(Vec<u8>),
    File(PathBuf),
}

fn format_extension(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Gif => "gif",
        ImageFormat::Webp => "webp",
        ImageFormat::Svg => "svg",
        _ => "png",
    }
}

/// `bytes` of any image AppKit reads, as PNG; `None` when it isn't one.
fn to_png(bytes: &[u8]) -> Option<Vec<u8>> {
    autoreleasepool(|_| unsafe {
        let data: Retained<AnyObject> = msg_send![class!(NSData), dataWithBytes: bytes.as_ptr() as *const c_void, length: bytes.len()];
        let rep: Option<Retained<AnyObject>> = msg_send![class!(NSBitmapImageRep), imageRepWithData: &*data];
        let rep = rep?;
        let properties: Retained<AnyObject> = msg_send![class!(NSDictionary), dictionary];
        // NSBitmapImageFileTypePNG is 4.
        let png: Option<Retained<AnyObject>> = msg_send![&*rep, representationUsingType: 4usize, properties: &*properties];
        let png = png?;
        let ptr: *const c_void = msg_send![&*png, bytes];
        let length: usize = msg_send![&*png, length];
        if ptr.is_null() {
            return None;
        }
        Some(std::slice::from_raw_parts(ptr as *const u8, length).to_vec())
    })
}

/// The slug of the heading the caret (on row `caret`) is under: the last heading at or above it,
/// leaving out lines inside a fenced code block. `None` when there is none or it has no letters.
fn heading_slug(rows: usize, caret: usize, line: impl Fn(usize) -> String) -> Option<String> {
    let mut found = None;
    let mut fence: Option<char> = None;
    for row in 0..rows.min(caret + 1) {
        let text = line(row);
        let trimmed = text.trim_start();
        if let Some(mark) = ["```", "~~~"].iter().find(|mark| trimmed.starts_with(**mark)) {
            let ch = mark.chars().next();
            match fence {
                None => fence = ch,
                Some(open) if Some(open) == ch => fence = None,
                Some(_) => {}
            }
            continue;
        }
        if fence.is_none() && text.len() - trimmed.len() <= 3 {
            let hashes = trimmed.chars().take_while(|ch| *ch == '#').count();
            if (1..=6).contains(&hashes) {
                let rest = &trimmed[hashes..];
                if rest.is_empty() || rest.starts_with(' ') || rest.starts_with('\t') {
                    found = Some(rest.trim().trim_end_matches('#').to_owned());
                }
            }
        }
    }
    found.map(|heading| slug(&heading)).filter(|slug| !slug.is_empty())
}

/// Lowercase letters and digits joined by single dashes, at most `SLUG_CHARS` long.
fn slug(text: &str) -> String {
    const SLUG_CHARS: usize = 60;
    let mut out = String::new();
    let mut dash = false;
    for ch in text.chars() {
        if ch.is_alphanumeric() {
            if dash && !out.is_empty() {
                out.push('-');
            }
            dash = false;
            out.extend(ch.to_lowercase());
        } else {
            dash = true;
        }
        if out.chars().count() >= SLUG_CHARS {
            break;
        }
    }
    out
}

/// `image-YYYY-MM-DD` for today in the local time zone.
fn today() -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
    let offset = autoreleasepool(|_| unsafe {
        let zone: Retained<AnyObject> = msg_send![class!(NSTimeZone), localTimeZone];
        let seconds: isize = msg_send![&*zone, secondsFromGMT];
        seconds as i64
    });
    let (year, month, day) = civil(now + offset);
    format!("image-{year:04}-{month:02}-{day:02}")
}

/// The calendar date of `seconds` since 1970 (days since the epoch, Hinnant's civil_from_days).
fn civil(seconds: i64) -> (i64, i64, i64) {
    let z = seconds.div_euclid(86_400) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn under(text: &str, caret: usize) -> Option<String> {
        let lines: Vec<String> = text.lines().map(str::to_owned).collect();
        heading_slug(lines.len(), caret, |row| lines[row].clone())
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Getting  Started: the \"Basics\"!"), "getting-started-the-basics");
        assert_eq!(slug("  ---  "), "");
        assert_eq!(slug("Über Café"), "über-café");
    }

    #[test]
    fn nearest_heading_above_the_caret() {
        let text = "# One\n\n## Two ##\ntext\n\n# Three";
        assert_eq!(under(text, 1).as_deref(), Some("one"));
        assert_eq!(under(text, 3).as_deref(), Some("two"));
        assert_eq!(under(text, 5).as_deref(), Some("three"));
        assert_eq!(under("text only", 0), None);
        assert_eq!(under("#nospace\n#", 1), None);
    }

    #[test]
    fn fenced_hashes_are_not_headings() {
        let text = "# Real\n```\n# comment\n```\nafter";
        assert_eq!(under(text, 4).as_deref(), Some("real"));
    }

    #[test]
    fn dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(1_793_577_600), (2026, 11, 2));
        assert_eq!(civil(951_782_400), (2000, 2, 29));
    }

    #[test]
    fn a_taken_name_gets_a_number_and_nothing_is_overwritten() {
        let dir = std::env::temp_dir().join(format!("finch-paste-image-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let folder = dir.join(ASSETS);
        assert_eq!(store(&folder, "shot", vec![Source::Bytes(b"one".to_vec(), ImageFormat::Png)]).unwrap(), ["shot.png"]);
        assert_eq!(store(&folder, "shot", vec![Source::Bytes(b"two".to_vec(), ImageFormat::Png)]).unwrap(), ["shot-2.png"]);
        assert_eq!(fs::read(folder.join("shot.png")).unwrap(), b"one");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn copied_pictures_paste_like_a_drop() {
        let dir = std::env::temp_dir().join(format!("finch-copied-pictures-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let (one, two, text) = (dir.join("one.png"), dir.join("two.PNG"), dir.join("notes.txt"));
        for path in [&one, &two, &text] {
            fs::write(path, b"x").unwrap();
        }
        let note = dir.join("note.md");
        let finder = |paths: &[&PathBuf]| {
            let joined = paths.iter().map(|path| path.display().to_string()).collect::<Vec<_>>().join("\n");
            vec![
                ClipboardEntry::ExternalPaths(gpui_kit::ExternalPaths(paths.iter().map(|path| (*path).clone()).collect())),
                ClipboardEntry::String(gpui_kit::ClipboardString::new(joined)),
            ]
        };
        assert_eq!(copied_pictures(Some(&note), &finder(&[&one, &two])), [one.clone(), two.clone()]);
        assert!(copied_pictures(Some(&note), &finder(&[&text])).is_empty());
        assert!(copied_pictures(Some(&note), &finder(&[&one, &text])).is_empty());
        assert!(copied_pictures(Some(&dir.join("plain.txt")), &finder(&[&one])).is_empty());
        assert!(copied_pictures(None, &finder(&[&one])).is_empty());
        assert!(copied_pictures(Some(&note), &finder(&[&dir.join("gone.png")])).is_empty());
        assert!(copied_pictures(Some(&note), &[ClipboardEntry::String(gpui_kit::ClipboardString::new("hello".into()))]).is_empty());
        let image = ClipboardEntry::Image(gpui_kit::Image::from_bytes(ImageFormat::Png, b"x".to_vec()));
        assert!(copied_pictures(Some(&note), &[image]).is_empty());
        let _ = fs::remove_dir_all(&dir);
    }
}
