//! Text sent to Finch from other apps: the Services menu (Append to Finch Note, New Finch Note)
//! and the command line a Shortcuts "Run Shell Script" action calls (`finch --append`,
//! `finch --new-note`).
//!
//! Both ways end in the same two writes: `append` adds the text under a timestamp line to a note,
//! and `new_note` makes a new scrap draft holding it. A note that a tab has open gets the block
//! at the end of that tab's text instead (`append_open`, unsaved like any edit; a draft saves
//! itself), so nothing on disk can be overwritten by the tab. Otherwise the write is made on disk
//! first so a crash never loses the text, and a running app shows the note as a tab without
//! coming forward (`app::open_outside` in background mode). A failed Services append shows
//! nothing (it is only printed to stderr); the command line says why and exits 1.
//!
//! Which note an append goes to: the one `--note` names, else the last one appended to (the path
//! in the `service-note` file next to the control socket, which the user may edit to choose
//! another), else a new scrap draft, which becomes that last note.
//!
//! The Services side is an `NSObject` set as `NSApp.servicesProvider` at launch. Its methods
//! read the string from the pasteboard and send it over a channel to a task on the app's
//! foreground executor, so nothing runs inside AppKit's call.

use std::{
    cell::RefCell,
    ffi::{CString, c_char},
    fs,
    io::{self, Write as _},
    os::unix::ffi::{OsStrExt as _, OsStringExt as _},
    path::{Path, PathBuf},
};

use futures_channel::mpsc::{UnboundedSender, unbounded};
use gpui_kit::*;
use objc2::{
    ClassType, class, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, NSObject},
};

use crate::{app, control, drafts, workspace};

/// What the provider or a control verb asks for.
pub enum Job {
    /// Add the text to `note`, else to the last note appended to, else to a new scrap draft.
    Append { text: String, note: Option<PathBuf> },
    /// A new scrap draft holding the text.
    New(String),
}

thread_local! {
    /// Where the provider's methods send their jobs; set by `install`, read on the main thread.
    static JOBS: RefCell<Option<UnboundedSender<Job>>> = const { RefCell::new(None) };
    /// The provider, kept alive for as long as the app runs.
    static PROVIDER: RefCell<Option<Retained<Provider>>> = const { RefCell::new(None) };
}

define_class!(
    // SAFETY: `NSObject` has no subclassing requirements and the class has no ivars.
    #[unsafe(super(NSObject))]
    #[name = "FinchServicesProvider"]
    struct Provider;

    impl Provider {
        #[unsafe(method(appendToFinchNote:userData:error:))]
        fn append_to_note(&self, pasteboard: &AnyObject, _user_data: Option<&AnyObject>, _error: *mut *mut AnyObject) {
            if let Some(text) = pasteboard_text(pasteboard) {
                send(Job::Append { text, note: None });
            }
        }

        #[unsafe(method(newFinchNote:userData:error:))]
        fn new_note(&self, pasteboard: &AnyObject, _user_data: Option<&AnyObject>, _error: *mut *mut AnyObject) {
            if let Some(text) = pasteboard_text(pasteboard) {
                send(Job::New(text));
            }
        }
    }
);

/// Registers the provider with the app and starts the task that carries out what it sends.
/// Cheap: one object and one idle task.
pub fn install(cx: &mut App) {
    let (sender, mut inbox) = unbounded::<Job>();
    JOBS.with_borrow_mut(|jobs| *jobs = Some(sender));
    let provider: Retained<Provider> = unsafe { msg_send![Provider::class(), new] };
    unsafe {
        let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![&*app, setServicesProvider: &*provider];
    }
    PROVIDER.with_borrow_mut(|kept| *kept = Some(provider));
    cx.spawn(async move |cx| {
        while let Ok(job) = inbox.recv().await {
            cx.update(|cx| {
                if let Err(why) = deliver(job, cx) {
                    eprintln!("finch: Services: {why}");
                }
            });
        }
    })
    .detach();
}

fn send(job: Job) {
    JOBS.with_borrow(|jobs| {
        if let Some(jobs) = jobs {
            jobs.unbounded_send(job).ok();
        }
    });
}

/// The plain text on a pasteboard a service was given.
fn pasteboard_text(pasteboard: &AnyObject) -> Option<String> {
    ["public.utf8-plain-text", "NSStringPboardType"].into_iter().find_map(|kind| {
        let string: Option<Retained<AnyObject>> = unsafe { msg_send![pasteboard, stringForType: &*ns_string(kind)] };
        string.map(|string| ns_text(&string))
    })
}

fn ns_string(text: &str) -> Retained<AnyObject> {
    let text = CString::new(text.replace('\0', "")).unwrap_or_default();
    unsafe { msg_send![class!(NSString), stringWithUTF8String: text.as_ptr()] }
}

fn ns_text(string: &AnyObject) -> String {
    let chars: *const c_char = unsafe { msg_send![string, UTF8String] };
    if chars.is_null() {
        return String::new();
    }
    unsafe { std::ffi::CStr::from_ptr(chars) }.to_string_lossy().into_owned()
}

/// The draft files the open documents keep their text in.
fn held_drafts(cx: &App) -> Vec<PathBuf> {
    workspace::open_workspaces(cx).into_iter().flat_map(|(_, workspace)| workspace.read(cx).documents(cx)).filter_map(|document| document.read(cx).draft_path().map(Path::to_path_buf)).collect()
}

/// Writes the text (`append` or `new_note`) and shows the note as a tab without activating the
/// app. An append to a note that a tab has open goes into that tab's buffer, so a draft's own
/// autosave or the tab's unsaved edits can't undo it. Returns the note's path.
fn deliver(job: Job, cx: &mut App) -> Result<PathBuf, String> {
    let path = match job {
        Job::Append { text, note } => {
            let target = note.or_else(last_note);
            if target.as_deref().is_some_and(|target| !text.trim().is_empty() && append_open(target, &text, cx)) {
                // Its tab already shows it (a draft's tab has no file path, so opening it again would make a second tab).
                let open = target.unwrap_or_default();
                remember(&open);
                return Ok(open.canonicalize().unwrap_or(open));
            }
            let held = held_drafts(cx);
            append(target.as_deref(), &text, &held)
        }
        Job::New(text) => new_note(&text, &held_drafts(cx)),
    }
    .map_err(|err| err.to_string())?;
    // A failed open has already told the user why; the text is safe on disk either way. With no
    // editor window (resident, or only the scratch window) the note stays on disk: a window made
    // here would be a background one, hidden under everything.
    if workspace::open_workspaces(cx).iter().any(|(_, workspace)| !workspace.read(cx).is_scratch()) {
        app::open_outside(&path, None, true, cx).ok();
    }
    Ok(path)
}

/// Adds the block at the end of the buffer of the tab that has `note` open, in any window, as one
/// edit that leaves the caret, the selection and the scroll where they were; the tab's own save
/// or draft write puts it on disk. `false` when no tab has the note open.
fn append_open(note: &Path, text: &str, cx: &mut App) -> bool {
    let Ok(want) = note.canonicalize() else { return false };
    for (handle, workspace) in workspace::open_workspaces(cx) {
        let documents = workspace.read(cx).documents(cx);
        let found = documents.into_iter().find(|document| {
            let document = document.read(cx);
            [document.path.as_deref(), document.draft_path()].into_iter().flatten().any(|path| path.canonicalize().ok().as_deref() == Some(want.as_path()))
        });
        let Some(document) = found else { continue };
        let text = text.to_string();
        let done = handle.update(cx, |_, window, cx| {
            let editor = document.update(cx, |document, cx| document.editor(window, cx));
            // The editor holds text, not bytes: only a UTF-8 note can take the block as it is.
            if document.read(cx).encoding() != encoding_rs::UTF_8 {
                return false;
            }
            editor.update(cx, |state, cx| {
                let selection = state.selected_range();
                let existing = state.text().to_string();
                let at = existing.len();
                state.replace_range(at..at, &block(&existing, &text), selection.end, window, cx);
                state.set_selected_range(selection, cx);
            });
            true
        });
        return done.unwrap_or(false);
    }
    false
}

/// The file holding the path of the last note appended to.
fn state_file() -> PathBuf {
    control::socket_path().parent().map_or_else(std::env::temp_dir, Path::to_path_buf).join("service-note")
}

fn last_note() -> Option<PathBuf> {
    let bytes = fs::read(state_file()).ok()?;
    let bytes = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes.to_vec()));
    (!bytes.is_empty() && path.is_file()).then_some(path)
}

fn remember(note: &Path) {
    let written = state_file().parent().map_or(Ok(()), fs::create_dir_all).and_then(|()| fs::write(state_file(), note.as_os_str().as_bytes()));
    if let Err(err) = written {
        eprintln!("finch: can't remember the note appended to: {err}");
    }
}

fn nothing_to_add() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "there is no text to add")
}

/// The local time as `2026-10-30 14:05`, the line an append puts above its text.
fn timestamp() -> String {
    let seconds = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_secs() as i64);
    let now = drafts::local(seconds);
    format!("{:04}-{:02}-{:02} {:02}:{:02}", now.year, now.month, now.day, now.hour, now.minute)
}

/// Adds `text` to the end of `note`, else the last note appended to, else a new scrap draft: a
/// blank line, a timestamp line, then the text. A note that doesn't exist yet is made. A note that
/// isn't UTF-8 text is left alone. Returns the note's path.
pub fn append(note: Option<&Path>, text: &str, held: &[PathBuf]) -> io::Result<PathBuf> {
    let text = text.trim_end_matches(['\n', '\r']);
    if text.trim().is_empty() {
        return Err(nothing_to_add());
    }
    let Some(path) = note.map(Path::to_path_buf).or_else(last_note) else {
        let path = new_note(text, held)?;
        remember(&path);
        return Ok(path);
    };
    append_to(&path, text)?;
    remember(&path);
    path.canonicalize().or(Ok(path))
}

/// Adds the text block to the end of the note at `path`.
fn append_to(path: &Path, text: &str) -> io::Result<()> {
    let text = text.trim_end_matches(['\n', '\r']);
    let existing = match fs::read(path) {
        Ok(bytes) => Some(String::from_utf8(bytes).map_err(|_| io::Error::new(io::ErrorKind::InvalidData, format!("{} isn’t UTF-8 text", path.display())))?),
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => return Err(err),
    };
    let added = block(existing.as_deref().unwrap_or(""), text);
    // Appending (not rewriting) leaves the note's own text untouched even if this fails midway.
    fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut file| file.write_all(added.as_bytes()))
}

/// What an append adds after `existing`: a blank line (none for an empty note), the timestamp line,
/// then the text and a line end.
fn block(existing: &str, text: &str) -> String {
    let text = text.trim_end_matches(['\n', '\r']);
    let mut added = String::new();
    if !existing.is_empty() {
        if !existing.ends_with('\n') {
            added.push('\n');
        }
        added.push('\n');
    }
    added.push_str(&format!("{}\n{text}\n", timestamp()));
    added
}

/// Makes a new scrap draft holding `text`, under a name no file and none of the `held` drafts (the
/// files open documents keep their text in) has. Returns its path.
pub fn new_note(text: &str, held: &[PathBuf]) -> io::Result<PathBuf> {
    if text.trim().is_empty() {
        return Err(nothing_to_add());
    }
    let folder = drafts::folder();
    fs::create_dir_all(&folder)?;
    let stamp = drafts::stamp_now();
    let path = (1..)
        .map(|n| drafts::new_path(&folder, &if n == 1 { stamp.clone() } else { format!("{stamp} {n}") }))
        .find(|path| !held.contains(path))
        .expect("an unused draft name");
    let text = format!("{}\n", text.trim_end_matches(['\n', '\r']));
    fs::OpenOptions::new().write(true).create_new(true).open(&path).and_then(|mut file| file.write_all(text.as_bytes()))?;
    Ok(path)
}

/// The `service-append`, `service-new` and `service-call` control verbs. `service-append <temp
/// file>[<TAB><note>]` and `service-new <temp file>` take text from a private file the command line
/// left (as `finch -` does) and reply with the note's path; `service-call append|new <text>` (a
/// background launch only) runs the provider's own methods on a private pasteboard, `\n` in the text
/// a line break, and replies once they have been sent: the write follows a moment later.
pub fn verb(verb: &str, rest: &str, background: bool, cx: &mut App) -> Result<String, String> {
    let from_file = |file: &str| {
        let file = control::decode_path(file);
        control::take_piped_file(&file).map(|text| (file, text))
    };
    let delivered = match verb {
        "service-append" => {
            let (file, note) = rest.split_once('\t').unwrap_or((rest, ""));
            let (file, text) = from_file(file)?;
            let note = (!note.is_empty()).then(|| control::decode_path(note));
            deliver(Job::Append { text, note }, cx).inspect(|_| {
                fs::remove_file(&file).ok();
            })
        }
        "service-new" => {
            let (file, text) = from_file(rest)?;
            deliver(Job::New(text), cx).inspect(|_| {
                fs::remove_file(&file).ok();
            })
        }
        _ => return call(rest, background),
    };
    delivered.map(|path| path.display().to_string())
}

fn call(rest: &str, background: bool) -> Result<String, String> {
    if !background {
        return Err("service-call is only available in a background launch".into());
    }
    let (which, text) = rest.split_once(' ').unwrap_or((rest, ""));
    let selector = match which {
        "append" => "append",
        "new" => "new",
        _ => return Err("service-call takes append or new, then the text".into()),
    };
    let text = text.replace("\\n", "\n");
    let provider = PROVIDER.with_borrow(Clone::clone).ok_or("the Services provider isn't installed")?;
    unsafe {
        let app: Retained<AnyObject> = msg_send![class!(NSApplication), sharedApplication];
        let set: Option<Retained<AnyObject>> = msg_send![&*app, servicesProvider];
        if set.is_none_or(|set| !std::ptr::eq(&*set, Retained::as_ptr(&provider).cast::<AnyObject>())) {
            return Err("the Services provider isn't set on the app".into());
        }
        let pasteboard: Retained<AnyObject> = msg_send![class!(NSPasteboard), pasteboardWithUniqueName];
        let _: isize = msg_send![&*pasteboard, clearContents];
        let _: bool = msg_send![&*pasteboard, setString: &*ns_string(&text), forType: &*ns_string("public.utf8-plain-text")];
        let user_data = ns_string("");
        let error = std::ptr::null_mut::<*mut AnyObject>();
        if selector == "append" {
            let _: () = msg_send![&*provider, appendToFinchNote: &*pasteboard, userData: &*user_data, error: error];
        } else {
            let _: () = msg_send![&*provider, newFinchNote: &*pasteboard, userData: &*user_data, error: error];
        }
        let _: () = msg_send![&*pasteboard, releaseGlobally];
    }
    Ok(String::new())
}

/// `finch --append [--note <path>]` and `finch --new-note`: sends the text piped in to a running
/// app (which writes it and shows the note without coming forward), or, with no app running,
/// writes it here and starts nothing. Prints the note's path and exits 0, or says why and exits 1.
pub fn cli(append_text: bool, note: Option<&Path>) -> ! {
    let file = control::piped_file();
    let note = note.map(|note| std::path::absolute(note).unwrap_or_else(|_| note.to_path_buf()));
    let line = if append_text {
        let note = note.as_deref().map(control::encode_path).unwrap_or_default();
        format!("service-append {}\t{note}", control::encode_path(&file))
    } else {
        format!("service-new {}", control::encode_path(&file))
    };
    let outcome = match control::request(&line) {
        Some(reply) => match reply.strip_prefix("err ") {
            // The app may still take the request, so its temp file stays and the result is unknown.
            Some(why) if why == control::NO_REPLY => {
                eprintln!("finch: Finch didn’t answer in time. The text may or may not have been added, so check the note before running this again.");
                std::process::exit(1);
            }
            Some(why) => Err(why.to_string()),
            None => Ok(reply),
        },
        None => fs::read_to_string(&file)
            .map_err(|err| err.to_string())
            .and_then(|text| if append_text { append(note.as_deref(), &text, &[]) } else { new_note(&text, &[]) }.map_err(|err| err.to_string()))
            .map(|path| path.display().to_string()),
    };
    fs::remove_file(&file).ok();
    match outcome {
        Ok(path) => {
            println!("{path}");
            std::process::exit(0);
        }
        Err(why) => {
            eprintln!("finch: {why}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::append_to;

    #[test]
    fn an_append_adds_a_timestamped_block_to_the_end_of_a_note() {
        let dir = std::env::temp_dir().join(format!("finch-services-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let note = dir.join("note.txt");
        std::fs::write(&note, "first").unwrap();
        append_to(&note, "hello\nthere\n").unwrap();
        let text = std::fs::read_to_string(&note).unwrap();
        let mut lines = text.lines();
        assert_eq!(lines.next(), Some("first"));
        assert_eq!(lines.next(), Some(""));
        let stamp = lines.next().unwrap();
        assert!(stamp.len() == 16 && stamp.as_bytes()[10] == b' ' && stamp.as_bytes()[13] == b':', "{stamp:?}");
        assert_eq!((lines.next(), lines.next(), lines.next()), (Some("hello"), Some("there"), None));
                        std::fs::remove_dir_all(&dir).ok();
    }
}
