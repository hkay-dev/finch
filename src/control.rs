//! The control socket: a Unix socket a running app answers on, and the
//! `finch debug <verb> [args]` client that talks to it. Both are
//! developer tools: they let a background launch (`GPUI_EDITOR_BACKGROUND=1`)
//! be driven and captured with no key presses and no focus.
//!
//! The socket is `~/Library/Application Support/Finch/control.sock`, or
//! `GPUI_EDITOR_CONTROL` when set. One request is one line, `<verb> [args]`;
//! the reply is one line: `ok`, `err <why>`, or for `rects` a JSON object.
//! Verbs act on the most recently active editor window (the last one to open
//! or to become the key window). The verbs are the table of command names near the end of this file.
//!
//! A background thread accepts connections and reads lines; each line crosses
//! a channel to a task on the app's foreground executor, which runs the verb
//! and sends the reply back. Nothing here runs unless a client connects.

use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap},
    ffi::{OsStr, OsString},
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::{net::{UnixListener, UnixStream}, process::CommandExt as _},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, LazyLock,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use futures_channel::{
    mpsc::{UnboundedSender, unbounded},
    oneshot,
};
use gpui_kit::*;

use crate::{
    app,
    drag::Zone,
    explorer,
    settings::{self, ThemeSetting},
    start::Caret,
    theme,
    window_look,
    workspace::{self, Workspace},
};

/// How long a client waits for a reply before giving up.
const REPLY_WAIT: Duration = Duration::from_secs(10);

/// The socket's path.
pub fn socket_path() -> PathBuf {
    if let Some(path) = std::env::var_os("GPUI_EDITOR_CONTROL") {
        return path.into();
    }
    settings::support_dir().join("control.sock")
}

/// One line from a client and where its reply goes.
struct Request {
    line: String,
    reply: mpsc::Sender<String>,
    /// Set by the connection's thread once its client is gone (an `open-file-wait` that was killed).
    gone: Arc<AtomicBool>,
}

/// Starts answering on the socket. A stale socket file (nobody listening) is
/// removed first; one with a live app behind it is left alone and not served.
pub fn start(background: bool, inbox: &[PathBuf], cx: &mut App) {
    listen_for_washes();
    let path = socket_path();
    // A test launch without its own socket must not take (or answer for) the real one.
    if background && std::env::var_os("GPUI_EDITOR_CONTROL").is_none() {
        eprintln!("finch: background launch without GPUI_EDITOR_CONTROL; control socket off");
        return;
    }
    if UnixStream::connect(&path).is_ok() {
        if !background {
            // A second full app would restore the same drafts and overwrite the first one's text,
            // so a launch that doesn't own the socket stops here, after handing on what it was asked to open.
            let mut entries = LAUNCH.get().cloned().unwrap_or_default();
            entries.extend(inbox.iter().map(|path| (path.clone(), None)));
            if !entries.is_empty() {
                hand_over(OsStr::new(""), &entries, false);
                eprintln!("Finch isn't responding.");
                forget_untitled(&entries);
                std::process::exit(1);
            }
            eprintln!("finch: another app answers on {}; this launch stops", path.display());
            std::process::exit(0);
        }
        eprintln!("finch: another app answers on {}; control socket off", path.display());
        return;
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).ok();
    }
    std::fs::remove_file(&path).ok();
    let listener = match UnixListener::bind(&path) {
        Ok(listener) => listener,
        Err(err) => {
            eprintln!("finch: can't listen on {}: {err}", path.display());
            return;
        }
    };
    // Quit unlinks only this file: another launch may have replaced it since.
    let bound = socket_identity(&path);
    BOUND.store(true, Ordering::Relaxed);
    let (requests, mut inbox) = unbounded::<Request>();
    let spawned = thread::Builder::new().name("control-socket".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let requests = requests.clone();
            thread::spawn(move || serve(stream, requests));
        }
    });
    if spawned.is_err() {
        return;
    }
    cx.spawn(async move |cx| {
        while let Ok(request) = inbox.recv().await {
            cx.update(|cx| handle(request, background, cx));
        }
    })
    .detach();
    cx.on_app_quit(move |_| {
        if bound.is_some() && socket_identity(&path) == bound {
            std::fs::remove_file(&path).ok();
        }
        async {}
    })
    .detach();
}

/// The device and inode of the file at `path`, which tell this process's socket from one a later launch bound there.
fn socket_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::symlink_metadata(path).ok().map(|meta| (meta.dev(), meta.ino()))
}

/// One connection: reads a line, waits for the reply, writes it. An `open-file-wait` has no
/// time limit; instead the wait checks every `PING` that the client is still there, and stops
/// (telling the app, which drops what it watched) when it isn't.
fn serve(stream: UnixStream, requests: UnboundedSender<Request>) {
    let mut line = String::new();
    if BufReader::new(&stream).read_line(&mut line).unwrap_or(0) == 0 {
        return;
    }
    let waits = matches!(split_word(line.trim()).0, "open-file-wait" | "open-files-wait");
    let (reply, answer) = mpsc::channel();
    let gone = Arc::new(AtomicBool::new(false));
    let text = if requests.unbounded_send(Request { line, reply, gone: gone.clone() }).is_err() {
        "err app is quitting".to_string()
    } else if waits {
        loop {
            match answer.recv_timeout(PING) {
                Ok(text) => break text,
                Err(mpsc::RecvTimeoutError::Timeout) if client_gone(&stream) => {
                    gone.store(true, Ordering::Relaxed);
                    return;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => break "err app is quitting".to_string(),
            }
        }
    } else {
        answer.recv_timeout(REPLY_WAIT).unwrap_or_else(|_| "err timed out".to_string())
    };
    // A failed write means the client went away, which is fine.
    let mut stream = &stream;
    writeln!(stream, "{text}").ok();
}

/// How often an `open-file-wait` checks that its client is still connected.
const PING: Duration = Duration::from_secs(1);

/// Whether the client closed its end: a read that returns nothing at once, rather than blocking.
/// A client sends one line and then only listens, so anything else it sent is ignored.
fn client_gone(stream: &UnixStream) -> bool {
    if stream.set_nonblocking(true).is_err() {
        return true;
    }
    let mut byte = [0u8; 1];
    let gone = match (&*stream).read(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(err) => err.kind() != io::ErrorKind::WouldBlock,
    };
    stream.set_nonblocking(false).ok();
    gone
}

fn handle(request: Request, background: bool, cx: &mut App) {
    let Request { line, reply, gone } = request;
    let (verb, rest) = split_word(line.trim());
    if verb == "snapshot" {
        snapshot_png(rest, reply, cx);
        return;
    }
    if verb == "frame" {
        frame(reply, cx);
        return;
    }
    if verb == "action" && matches!(rest, "move-tab-left" | "move-tab-right" | "move-tab-up" | "move-tab-down") {
        move_tab_action(rest, reply, cx);
        return;
    }
    if verb == "resize" {
        resize(rest, reply, cx);
        return;
    }
    if verb == "drag-frames" && background {
        drag_frames(rest, reply, cx);
        return;
    }
    if verb == "open-file-wait" || verb == "open-files-wait" {
        open_wait(verb, rest, background, reply, gone, cx);
        return;
    }
    if verb == "services" {
        services(rest, background, reply, cx);
        return;
    }
    if verb == "rich-copy" {
        rich_copy(background, reply, cx);
        return;
    }
    if verb == "camera-text" {
        crate::camera_text::from_file(rest, reply, cx);
        return;
    }
    // `type` keeps its text as sent: trailing spaces count, so it is taken from the untrimmed line.
    let typed = line.strip_suffix('\n').map(|line| line.strip_suffix('\r').unwrap_or(line)).unwrap_or(&line);
    let typed = typed.trim_start().strip_prefix("type").map(|text| text.strip_prefix(' ').unwrap_or(text)).unwrap_or("");
    let result = match verb {
        "launch" => launch(),
        "open" => open(rest, background, cx),
        "open-file" => open_file(rest, background, cx).map(|_| String::new()),
        "open-files" => open_files(rest, background, cx).map(|_| String::new()),
        "explorer" => explorer(rest, cx),
        "paste-files" => paste_files(rest, cx),
        "action" => action(rest, background, cx),
        "wheel" => wheel(rest, cx),
        "pinch" => pinch(rest, cx),
        "scroll" => scroll(cx),
        "preview-scroll" => preview_scroll(cx),
        "diagram-stats" => {
            let stats = crate::preview::diagrams::stats();
            Ok(format!("{{\"diagrams\":{},\"formulas\":{},\"pele_loaded\":{}}}", stats.diagrams, stats.formulas, stats.pele_loaded))
        }
        "click" => click(rest, cx),
        "text-click" => text_click(rest, cx),
        "text-hover" => text_hover(rest, cx),
        "right-click" => right_click(rest, cx),
        "press" => press(rest, cx),
        "release" => release(rest, cx),
        "tooltip" => tooltip_text(cx),
        "ime" if background => ime(rest, cx),
        "mouse" if background => mouse(rest, cx),
        "hover" => hover(rest, cx),
        "answer" => answer(rest, cx),
        "theme" => set_theme(rest, cx),
        "appearance" => appearance(rest, cx),
        "open-tab" => open_tab(rest, cx),
        "tab" => tab(rest, cx),
        "close-tab" => close_tab(cx),
        "close-window" => close_window(rest, cx),
        "close-others" | "close-right" | "pin" | "unpin" | "reveal-in-explorer" | "tab-menu" => tab_menu_verb(verb, rest, cx),
        "tab-list" => tab_list_verb(cx),
        "reopen-tab" => reopen_tab(cx),
        "recent-tab" => recent_tab(rest, cx),
        "release-control" => release_control(cx),
        "tabs" => tabs(cx),
        "tab-state" => tab_state(cx),
        "new-page" => new_page(cx),
        "new-markdown" => new_markdown(rest, cx),
        "welcome-back" => welcome_back(cx),
        "scratch" => scratch(rest, background, cx),
        // `settings [pane]`: the Settings window on a pane (the one shown last when none is named); it never activates in a background launch.
        "settings" => crate::settings_window::show(rest, background, cx),
        // `settings-search <text>`: types into the Settings window's search field; `--escape` presses Escape there.
        "settings-search" => crate::settings_window::search(rest, cx),
        "new-from-template" => new_from_template(rest, cx),
        "split" => split(rest, cx),
        "split-share" => split_share(rest, cx),
        "focus-pane" => focus_pane(rest, cx),
        "drop" => drop_on(rest, cx),
        "drag-hold" => drag_hold(rest, cx),
        "windows" => windows_verb(cx),
        "tab-to-window" => tab_to_window(rest, cx),
        "drag-release" => drag_release(cx),
        "draw-as-key" => draw_as_key(rest, background, cx),
        "disk-state" => disk_state(rest, cx),
        "panes" => panes(cx),
        "type" => type_text(typed, cx),
        "key" => key(rest, cx),
        "quit" => quit(cx),
        "focus" => focus(cx),
        "find" => find(rest, cx),
        "field" => field(rest, cx),
        "selection" => selection(cx),
        "folds" => folds(cx),
        "editor-menu" => editor_menu(cx),
        "menu-bar" => { crate::menu::dump_native(); Ok(String::new()) },
        "find-next" => find_step(false, cx),
        "find-prev" => find_step(true, cx),
        "goto" => goto(rest, cx),
        "go-to-line" => go_to_line(rest, cx),
        "goal" => goal(rest, cx),
        "words-menu" => words_menu(cx),
        "open-quickly" => open_quickly(rest, cx),
        "find-in-folder" => find_in_folder(rest, cx),
        "draft-list" => draft_list(rest, cx),
        "versions-list" => versions_list(rest, cx),
        "undo-history" => undo_history(rest, cx),
        "compare" => compare(rest, cx),
        "file-rename" => file_rename(rest, cx),
        "file-duplicate" => file_duplicate(cx),
        "file-move" => file_move(rest, cx),
        "save-as" => save_as(rest, cx),
        "export-html" => export_html(rest, cx),
        "print-pdf" => print_pdf(rest, cx),
        "text" => text(rest, cx),
        "rects" => rects(cx),
        "service-append" | "service-new" | "service-call" => crate::services::verb(verb, rest, background, cx),
        _ => Err("unknown verb".to_string()),
    };
    let text = match result {
        Ok(body) if body.is_empty() => "ok".to_string(),
        Ok(body) => body,
        Err(why) => format!("err {why}"),
    };
    reply.send(text).ok();
}

/// `launch`: `{"exec_to_first_frame_ms": f64}`, an error until a frame has drawn.
fn launch() -> Result<String, String> {
    let ms = window_look::exec_to_first_frame_ms().ok_or("no frame yet")?;
    Ok(format!("{{\"exec_to_first_frame_ms\": {ms:.1}}}"))
}

/// The first word of `text` and the rest, trimmed.
fn split_word(text: &str) -> (&str, &str) {
    match text.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None => (text, ""),
    }
}

thread_local! {
    /// The window that opened or became key last.
    static LAST: Cell<Option<WindowId>> = const { Cell::new(None) };
}

/// Marks `window` as the one verbs act on.
pub fn note_active(window: &Window) {
    LAST.set(Some(window.window_handle().window_id()));
}

/// The window verbs act on: the last one noted, else the newest open.
pub fn target(cx: &App) -> Result<(AnyWindowHandle, Entity<Workspace>), String> {
    let all = workspace::open_workspaces(cx);
    let last = LAST.get();
    all.iter()
        .find(|(handle, _)| Some(handle.window_id()) == last)
        .or(all.last())
        .cloned()
        .ok_or_else(|| "no editor window".to_string())
}

/// `open <path>`: always a new window on the file or folder (`open-file` makes a tab). `open --tab <path>`: the file as a tab
/// of the target window (what `open-tab` does), a folder still a new window.
fn open(path: &str, background: bool, cx: &mut App) -> Result<String, String> {
    if let Some(tab) = path.strip_prefix("--tab").filter(|rest| rest.is_empty() || rest.starts_with(' ')) {
        let tab = tab.trim_start();
        return if Path::new(tab).is_dir() { open(tab, background, cx) } else { open_tab(tab, cx) };
    }
    if path.is_empty() {
        return Err("open needs a path".into());
    }
    let path = Path::new(path);
    app::open_path(path, None, background, cx)
        .map(|_| String::new())
        .map_err(|err| crate::file_ops::explain("open", path, &err))
}

/// `open-file [<line>:<column> ]<absolute path>`: what `finch <path>[:line[:column]]` in a
/// terminal sends. A file opens as a tab (see `app::open_outside`) with the caret on the line and
/// column (from 1) when given, and the app comes forward; a folder gets a new window. Returns what
/// `open-file-wait` watches, or the named reason it could not open.
fn open_file(rest: &str, background: bool, cx: &mut App) -> Result<app::Closing, String> {
    let (caret, path) = caret_prefix(rest);
    if path.is_empty() {
        return Err("open-file needs a path".into());
    }
    let path = decode_path(path);
    if let Some(file) = path.to_str().and_then(|path| path.strip_prefix(UNTITLED)) {
        return open_untitled(Path::new(file), background, cx);
    }
    app::open_outside(&path, caret, background, cx).map_err(|err| crate::file_ops::explain("open", &path, &err))
}

/// `open-file --untitled <temp file>`: what `finch -` sends. The text of the file `stdin_entry`
/// made is typed into a page of the target window as typed characters are (so it is a draft and
/// unsaved: closing asks), and the file is deleted. The showing page takes it when it is blank,
/// else a new tab does. Only a plain file (one link) in the socket's own folder is read or deleted.
fn open_untitled(file: &Path, background: bool, cx: &mut App) -> Result<app::Closing, String> {
    let ours = file.parent() == socket_path().parent() && std::fs::symlink_metadata(file).is_ok_and(|meta| meta.is_file() && std::os::unix::fs::MetadataExt::nlink(&meta) == 1);
    if !ours {
        return Err("open-file --untitled takes a file made by finch -".into());
    }
    let text = std::fs::read_to_string(file).map_err(|err| crate::file_ops::explain("open", file, &err))?;
    // With every window closed (resident), the text still has a place to go: a new window first.
    let (handle, workspace) = match target(cx) {
        Ok(found) => found,
        Err(_) => {
            app::new_window(background, cx);
            target(cx)?
        }
    };
    let document = cx
        .update_window(handle, |_, window, cx| {
            let document = workspace.update(cx, |workspace, cx| workspace.new_page_with(&text, window, cx));
            if !background {
                window.activate_window();
            }
            document
        })
        .map_err(|err| err.to_string())?;
    if !background {
        cx.activate(true);
    }
    std::fs::remove_file(file).ok();
    Ok(app::Closing::Tab(document))
}

/// `open-files <entry>[<TAB><entry>...]`, each entry what `open-file` takes: what `finch a b`
/// sends. Each opens in order, so the last one shows. Returns what `open-files-wait` watches.
fn open_files(rest: &str, background: bool, cx: &mut App) -> Result<Vec<app::Closing>, String> {
    let mut closings = Vec::new();
    let mut first_error = None;
    let mut entries = rest.split('\t').map(str::trim).filter(|entry| !entry.is_empty()).peekable();
    if entries.peek().is_none() {
        return Err("open-files needs a path".into());
    }
    for entry in entries {
        match open_file(entry, background, cx) {
            Ok(closing) => closings.push(closing),
            Err(err) => {
                if first_error.is_none() {
                    first_error = Some(err);
                }
            }
        }
    }
    if closings.is_empty() {
        if let Some(err) = first_error {
            return Err(err);
        }
    }
    Ok(closings)
}

/// `open-file-wait [<line>:<column> ]<absolute path>` and `open-files-wait`: `open-file` and
/// `open-files`, but the reply comes only when the tab it opened (or the window, for a folder or
/// a file that didn't exist) is closed, for several when all of them are, however long that takes.
/// `serve` sets `gone` when the client has gone away, and the next check here stops watching.
fn open_wait(verb: &str, rest: &str, background: bool, reply: mpsc::Sender<String>, gone: Arc<AtomicBool>, cx: &mut App) {
    let opened = if verb == "open-file-wait" { open_file(rest, background, cx).map(|closing| vec![closing]) } else { open_files(rest, background, cx) };
    let closings = match opened {
        Ok(closings) if !closings.is_empty() => closings,
        Ok(_) => return drop(reply.send("err Couldn’t open it. The app says why.".to_string())),
        Err(why) => return drop(reply.send(format!("err {why}"))),
    };
    let closed = Rc::new(Cell::new(false));
    let left = Rc::new(Cell::new(closings.len()));
    let watches: Vec<Subscription> = closings
        .into_iter()
        .map(|closing| {
            let (closed, left, reply) = (closed.clone(), left.clone(), reply.clone());
            closing.on_close(cx, move || {
                left.set(left.get() - 1);
                if left.get() == 0 {
                    closed.set(true);
                    reply.send("ok".to_string()).ok();
                }
            })
        })
        .collect();
    cx.spawn(async move |cx| {
        let _watches = watches;
        while !closed.get() && !gone.load(Ordering::Relaxed) {
            cx.background_executor().timer(PING).await;
        }
    })
    .detach();
}

/// The `<line>:<column> ` that may lead an `open-file` argument, and the path after it. A path
/// is absolute, so it never reads as a position.
fn caret_prefix(rest: &str) -> (Option<Caret>, &str) {
    let (word, path) = split_word(rest);
    let caret = word.split_once(':').and_then(|(line, column)| Some(Caret { line: line.parse().ok()?, column: column.parse().ok()? }));
    match caret {
        Some(caret) if !path.is_empty() => (Some(caret), path),
        _ => (None, rest),
    }
}

/// `paste-files [--into <folder><TAB>]<path>[<TAB><path>...]`: puts those files on the launch's
/// pasteboard (a test launch's own, never the real app's) as one Finder copy of several files, then
/// pastes them into the tree as Cmd-V with the tree focused does: into the folder, else the
/// selected row's folder, else the root. Paths are tab separated and `encode_path` encoded, as
/// `open-files` takes them. The copies land in the background, so `explorer rows` shows them a
/// moment later. `err` when the folder lies outside the tree.
fn paste_files(rest: &str, cx: &mut App) -> Result<String, String> {
    let mut entries = rest.split('\t').map(str::trim).filter(|entry| !entry.is_empty()).peekable();
    let into = entries.next_if(|entry| entry.starts_with("--into ")).map(|entry| {
        let folder = decode_path(entry["--into ".len()..].trim());
        std::fs::canonicalize(&folder).unwrap_or(folder)
    });
    let paths: Vec<PathBuf> = entries.map(decode_path).collect();
    if paths.is_empty() {
        return Err("paste-files needs one or more paths".into());
    }
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        cx.write_to_clipboard(ClipboardItem { entries: vec![ClipboardEntry::ExternalPaths(ExternalPaths(paths.into()))] });
        let explorer = workspace.read(cx).explorer().clone();
        if explorer.update(cx, |explorer, cx| explorer.paste_files(into, window, cx)) {
            Ok(String::new())
        } else {
            Err("the folder is outside the tree".to_string())
        }
    })
    .map_err(|err| err.to_string())?
}

/// `explorer select|hover|toggle <path>`, `explorer menu [path]` (a row's right-click menu, or the
/// empty space's), `explorer header` (the root pull-down), `explorer root <path>` (what picking a
/// folder in the pull-down does) and `explorer rows` (the root, the selection, the picked rows and
/// the visible rows as JSON). A menu opens a frame or two later; with `GPUI_EDITOR_MENU_DUMP` set
/// it is written to that file instead. The file actions: `explorer new-file|new-folder|rename
/// <path> [-> <name>]` (the name field opens, or with a name is filled and confirmed), `explorer
/// duplicate|trash [path]` (no path: the picked rows, as Cmd-D and Cmd-Delete), `explorer move-to
/// <folder>` (the picked rows, as Move to… with that folder picked) and `explorer undo` (Cmd-Z).
/// Picking: `explorer pick <path>` is a Command-click on the row, `explorer pick-run <path>` a
/// Shift-click. Dragging: `explorer move <from> <folder> [copy]` drops a row on a folder (`copy`
/// is Option held), `explorer drop-external <path> <folder>` drops a file from Finder there (a
/// copy); paths hold no spaces. `explorer key <name>` focuses the tree and sends it that key
/// (`up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `alt-left`, `alt-right`).
/// `explorer sort name|kind|date|size` orders the tree as View > Sort by does.
fn explorer(rest: &str, cx: &mut App) -> Result<String, String> {
    let (action, path) = split_word(rest);
    if action == "filter" {
        let (handle, workspace) = target(cx)?;
        return cx
            .update_window(handle, |_, window, cx| {
                let explorer = workspace.read(cx).explorer().clone();
                explorer.update(cx, |explorer, cx| explorer.type_filter(path, window, cx));
            })
            .map(|()| String::new())
            .map_err(|err| err.to_string());
    }
    if action == "sort" {
        let sort = match path {
            "name" => settings::ExplorerSort::Name,
            "kind" => settings::ExplorerSort::Kind,
            "date" => settings::ExplorerSort::Date,
            "size" => settings::ExplorerSort::Size,
            _ => return Err("usage: explorer sort name|kind|date|size".into()),
        };
        let (_, workspace) = target(cx)?;
        let explorer = workspace.read(cx).explorer().clone();
        explorer.update(cx, |explorer, cx| explorer.set_sort(sort, cx));
        cx.defer(crate::menu::refresh);
        return Ok(String::new());
    }
    if action == "key" {
        let (handle, workspace) = target(cx)?;
        return cx
            .update_window(handle, |_, window, cx| {
                let explorer = workspace.read(cx).explorer().clone();
                explorer.update(cx, |explorer, cx| explorer.press(path, window, cx))
            })
            .map_err(|err| err.to_string())?
            .map(|()| String::new());
    }
    let action = if action == "focus-rename" { "rename" } else { action };
    if matches!(action, "move" | "drop-external") {
        let words: Vec<&str> = path.split_whitespace().collect();
        let (from, folder, copy) = match (action, words.as_slice()) {
            ("move", [from, folder]) => (from, folder, false),
            ("move", [from, folder, "copy"]) => (from, folder, true),
            ("drop-external", [from, folder]) => (from, folder, true),
            _ => return Err(format!("usage: explorer {action} <path> <folder>{}", if action == "move" { " [copy]" } else { "" })),
        };
        let canonical = |path: &str| std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
        let (from, folder) = (canonical(from), canonical(folder));
        let (handle, workspace) = target(cx)?;
        return cx
            .update_window(handle, |_, window, cx| {
                let explorer = workspace.read(cx).explorer().clone();
                explorer.update(cx, |explorer, cx| explorer.drop_files(vec![from], folder, copy, window, cx)).map(|()| String::new()).map_err(|(_, why)| why)
            })
            .map_err(|err| err.to_string())?;
    }
    if path.is_empty() && !matches!(action, "menu" | "header" | "rows" | "undo" | "redo" | "duplicate" | "trash") {
        return Err("explorer needs select, hover, toggle or root, then a path; or menu, header or rows".into());
    }
    // `new-file`, `new-folder` and `rename` take the name typed in the row's field after ` -> `.
    let (path, name) = match path.split_once(" -> ") {
        Some((path, name)) => (path.trim(), Some(name.trim())),
        None => (path, None),
    };
    // The explorer's root is canonical (`/private/tmp`, not `/tmp`), so match it.
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let path = path.as_path();
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        let explorer = workspace.read(cx).explorer().clone();
        explorer.update(cx, |explorer, cx| match action {
            "select" => explorer.select_path(path, cx).map(|()| String::new()),
            "hover" => explorer.hover_path(path, window, cx).map(|()| String::new()),
            "toggle" => explorer.toggle_path(path, cx).map(|()| String::new()),
            "click" => explorer.click_path(path, false, cx).map(|()| String::new()),
            "double-click" => explorer.click_path(path, true, cx).map(|()| String::new()),
            "root" => {
                explorer.set_root(path.to_path_buf(), cx);
                Ok(String::new())
            }
            "menu" => {
                explorer.context_menu((!path.as_os_str().is_empty()).then(|| path.to_path_buf()), window, cx);
                Ok(String::new())
            }
            "header" => {
                explorer.root_menu(window, cx);
                Ok(String::new())
            }
            "rows" => {
                let (root, selected, picked, rows) = explorer.snapshot(cx);
                let rows: Vec<String> = rows
                    .iter()
                    .map(|(path, git)| format!("{{\"path\":{},\"git\":{}}}", json_string(path), git.map_or_else(|| "null".to_string(), json_string)))
                    .collect();
                let selected = selected.map_or_else(|| "null".to_string(), |path| json_string(&path));
                let picked: Vec<String> = picked.iter().map(|path| json_string(path)).collect();
                Ok(format!(
                    "{{\"root\":{},\"hidden\":{},\"ignored\":{},\"sort\":\"{}\",\"selected\":{selected},\"picked\":[{}],\"rows\":[{}]}}",
                    json_string(&root),
                    settings::get(cx).explorer.show_hidden,
                    settings::get(cx).explorer.show_ignored,
                    format!("{:?}", explorer.sort()).to_lowercase(),
                    picked.join(","),
                    rows.join(",")
                ))
            }
            "pick" => explorer.pick(path, false, cx).map(|()| String::new()),
            "pick-run" => explorer.pick(path, true, cx).map(|()| String::new()),
            "new-file" => explorer.new_item(path, false, name, window, cx).map(|()| String::new()),
            "new-folder" => explorer.new_item(path, true, name, window, cx).map(|()| String::new()),
            "rename" => explorer.rename_path(path, name, window, cx).map(|()| String::new()),
            "duplicate" | "trash" => {
                let paths = if path.as_os_str().is_empty() { explorer.picked_paths(cx) } else { vec![path.to_path_buf()] };
                if paths.is_empty() {
                    return Err("nothing is selected".to_string());
                }
                match action {
                    "duplicate" => explorer.duplicate_paths(paths, window, cx),
                    _ => explorer.trash_paths(paths, window, cx),
                }
                .map(|()| String::new())
            }
            "move-to" => {
                let paths = explorer.picked_paths(cx);
                explorer.move_paths(paths, path.to_path_buf(), window, cx).map(|()| String::new()).map_err(|(_, why)| why)
            }
            "undo" => Ok(String::new()).inspect(|_| {
                explorer.undo(window, cx);
            }),
            "redo" => Ok(String::new()).inspect(|_| {
                explorer.redo(window, cx);
            }),
            _ => Err("explorer needs select, hover, toggle, root, menu, header, rows, pick, pick-run, new-file, new-folder, rename, duplicate, trash, move-to, undo or redo".to_string()),
        })
    })
    .map_err(|err| err.to_string())?
}

/// `action <name>`: the command of that name (the names are the `ACTIONS` table; an unknown one lists them), sent as the action its key
/// and menu item send, to the window's root. `save` and `reopen-<encoding label>` are built here.
fn action(name: &str, background: bool, cx: &mut App) -> Result<String, String> {
    // Show in Finder activates Finder, which would take the front from a background launch: it answers ok and opens nothing.
    if name == "show-in-finder" && background {
        let (_, workspace) = target(cx)?;
        return if workspace.read(cx).has_file_tab(cx) { Ok(String::new()) } else { Err("show-in-finder needs a tab with a file".into()) };
    }
    if name == "save" {
        let (handle, workspace) = target(cx)?;
        let document = workspace.read(cx).document(cx).ok_or("save needs an open tab")?;
        if document.read(cx).path.is_none() {
            return Err("save needs a tab with a path".into());
        }
        cx.update_window(handle, |_, window, cx| window.dispatch_action(Box::new(app::Save), cx)).map_err(|err| err.to_string())?;
        return Ok(String::new());
    }
    let action: Box<dyn Action> = match ACTIONS.iter().find(|(known, _)| *known == name) {
        Some((_, make)) => make(),
        None => match name {
            name if name.starts_with("reopen-") => {
                let label = &name["reopen-".len()..];
                let encoding = encoding_rs::Encoding::for_label(label.as_bytes())
                    .filter(|encoding| crate::encoding::REOPEN_CHOICES.contains(encoding) || crate::encoding::MORE_CHOICES.contains(encoding))
                    .ok_or_else(|| format!("reopen-<label> needs an encoding label from the Reopen with encoding menu, such as utf-8, windows-1252 or shift_jis, not {label}"))?;
                Box::new(app::ReopenWithEncoding { encoding })
            }
            _ => return Err(format!("action needs {}", action_names())),
        },
    };
    let (handle, _) = target(cx)?;
    cx.update_window(handle, |_, window, cx| window.dispatch_action(action, cx)).map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// Makes the action a name stands for.
type MakeAction = fn() -> Box<dyn Action>;

/// The `action` verb's names and what each sends: the one table the verb and its error read.
static ACTIONS: &[(&str, MakeAction)] = &[
    ("new-window", || Box::new(app::NewWindow)),
    ("show-in-finder", || Box::new(workspace::ShowFileInFinder)),
    ("copy-path", || Box::new(workspace::CopyFilePath)),
    ("enclosing", || Box::new(explorer::EnclosingFolder)),
    ("reveal", || Box::new(explorer::RevealCurrentFile)),
    ("hidden", || Box::new(explorer::ToggleHiddenFiles)),
    ("ignored", || Box::new(explorer::ToggleIgnoredFiles)),
    ("collapse", || Box::new(explorer::CollapseAllFolders)),
    ("split-right", || Box::new(app::SplitRight)),
    ("split-down", || Box::new(app::SplitDown)),
    ("focus-left", || Box::new(app::FocusPaneLeft)),
    ("focus-right", || Box::new(app::FocusPaneRight)),
    ("focus-up", || Box::new(app::FocusPaneUp)),
    ("focus-down", || Box::new(app::FocusPaneDown)),
    ("focus-next", || Box::new(app::FocusNextPane)),
    ("focus-previous", || Box::new(app::FocusPreviousPane)),
    ("even-out-panes", || Box::new(app::EvenOutPanes)),
    ("move-tab-earlier", || Box::new(app::MoveTabEarlier)),
    ("move-tab-later", || Box::new(app::MoveTabLater)),
    ("merge-panes", || Box::new(app::MergePanes)),
    ("merge-all-windows", || Box::new(app::MergeAllWindows)),
    ("toggle-explorer", || Box::new(app::ToggleExplorer)),
    ("full-screen", || Box::new(app::ToggleFullScreen)),
    ("minimal", || Box::new(app::ToggleMinimal)),
    ("look-up", || Box::new(app::LookUp)),
    ("toggle-preview", || Box::new(app::TogglePreview)),
    ("help", || Box::new(app::ShowHelp)),
    ("undo", || Box::new(gpui_kit::component::input::Undo)),
    ("redo", || Box::new(gpui_kit::component::input::Redo)),
    ("select-line", || Box::new(gpui_kit::base::input::SelectLine)),
    ("select-inside-brackets", || Box::new(gpui_kit::base::input::SelectToEnclosingBracket)),
    ("expand-selection", || Box::new(gpui_kit::component::input::SelectLargerSyntaxNode)),
    ("shrink-selection", || Box::new(gpui_kit::component::input::SelectSmallerSyntaxNode)),
    ("duplicate-line", || Box::new(gpui_kit::component::input::DuplicateLine)),
    ("move-line-up", || Box::new(gpui_kit::component::input::MoveLineUp)),
    ("move-line-down", || Box::new(gpui_kit::component::input::MoveLineDown)),
    ("delete-line", || Box::new(gpui_kit::base::input::DeleteLine)),
    ("join-lines", || Box::new(gpui_kit::base::input::JoinLines)),
    ("transpose", || Box::new(gpui_kit::base::input::Transpose)),
    ("insert-line-below", || Box::new(gpui_kit::base::input::InsertLineBelow)),
    ("insert-line-above", || Box::new(gpui_kit::base::input::InsertLineAbove)),
    ("toggle-comment", || Box::new(gpui_kit::component::input::ToggleComments)),
    ("indent", || Box::new(gpui_kit::component::input::Indent)),
    ("outdent", || Box::new(gpui_kit::component::input::Outdent)),
    ("reindent", || Box::new(gpui_kit::base::input::Reindent)),
    ("add-cursor-above", || Box::new(gpui_kit::base::input::AddCursorAbove)),
    ("add-cursor-below", || Box::new(gpui_kit::base::input::AddCursorBelow)),
    ("make-upper-case", || Box::new(gpui_kit::base::input::MakeUpperCase)),
    ("make-lower-case", || Box::new(gpui_kit::base::input::MakeLowerCase)),
    ("capitalize", || Box::new(gpui_kit::base::input::Capitalize)),
    ("title-case", || Box::new(gpui_kit::base::input::ConvertToTitleCase)),
    ("snake-case", || Box::new(gpui_kit::base::input::ConvertToSnakeCase)),
    ("camel-case", || Box::new(gpui_kit::base::input::ConvertToCamelCase)),
    ("pascal-case", || Box::new(gpui_kit::base::input::ConvertToPascalCase)),
    ("kebab-case", || Box::new(gpui_kit::base::input::ConvertToKebabCase)),
    ("surround-parentheses", || Box::new(gpui_kit::base::input::SurroundWithParentheses)),
    ("surround-brackets", || Box::new(gpui_kit::base::input::SurroundWithBrackets)),
    ("surround-braces", || Box::new(gpui_kit::base::input::SurroundWithBraces)),
    ("surround-double-quotes", || Box::new(gpui_kit::base::input::SurroundWithDoubleQuotes)),
    ("surround-single-quotes", || Box::new(gpui_kit::base::input::SurroundWithSingleQuotes)),
    ("surround-backticks", || Box::new(gpui_kit::base::input::SurroundWithBackticks)),
    ("remove-surrounding", || Box::new(gpui_kit::base::input::RemoveSurrounding)),
    ("sort-lines", || Box::new(gpui_kit::base::input::SortLines)),
    ("sort-lines-descending", || Box::new(gpui_kit::base::input::SortLinesDescending)),
    ("sort-lines-ignoring-case", || Box::new(gpui_kit::base::input::SortLinesIgnoringCase)),
    ("reverse-lines", || Box::new(gpui_kit::base::input::ReverseLines)),
    ("remove-duplicate-lines", || Box::new(gpui_kit::base::input::RemoveDuplicateLines)),
    ("remove-blank-lines", || Box::new(gpui_kit::base::input::RemoveBlankLines)),
    ("format-json", || Box::new(crate::config_check::FormatJson)),
    ("trim-trailing-spaces", || Box::new(gpui_kit::base::input::TrimTrailingWhitespace)),
    ("hard-wrap", || Box::new(gpui_kit::base::input::HardWrap)),
    ("increment", || Box::new(gpui_kit::base::input::Increment)),
    ("decrement", || Box::new(gpui_kit::base::input::Decrement)),
    ("jump-to-bracket", || Box::new(gpui_kit::base::input::MoveToEnclosingBracket)),
    ("move-left", || Box::new(gpui_kit::component::input::MoveLeft)),
    ("move-right", || Box::new(gpui_kit::component::input::MoveRight)),
    ("select-left", || Box::new(gpui_kit::base::actions::SelectLeft)),
    ("select-right", || Box::new(gpui_kit::base::actions::SelectRight)),
    ("backspace", || Box::new(gpui_kit::component::input::Backspace)),
    ("delete", || Box::new(gpui_kit::component::input::Delete)),
    ("copy", || Box::new(gpui_kit::component::input::Copy)),
    ("cut", || Box::new(gpui_kit::component::input::Cut)),
    ("paste", || Box::new(gpui_kit::component::input::Paste)),
    ("select-all", || Box::new(gpui_kit::component::input::SelectAll)),
    ("find-next", || Box::new(app::FindNext)),
    ("find-previous", || Box::new(app::FindPrevious)),
    ("use-selection-for-find", || Box::new(workspace::UseSelectionForFind)),
    ("select-all-search-matches", || Box::new(app::SelectAllSearchMatches)),
    ("select-next-occurrence", || Box::new(app::SelectNextOccurrence)),
    ("skip-next-occurrence", || Box::new(app::SkipNextOccurrence)),
    ("match-case", || Box::new(app::ToggleMatchCase)),
    ("whole-word", || Box::new(app::ToggleWholeWord)),
    ("ignore-diacritics", || Box::new(app::ToggleIgnoreDiacritics)),
    ("find-in-selection", || Box::new(app::ToggleFindInSelection)),
    ("jump-to-selection", || Box::new(app::JumpToSelection)),
    ("go-to-line", || Box::new(app::ShowGoToLine)),
    ("open-quickly", || Box::new(app::ShowOpenQuickly)),
    ("find-in-folder", || Box::new(app::ShowFindInFolder)),
    ("open-draft", || Box::new(workspace::ShowDraftList)),
    ("toggle-scratch", || Box::new(app::ToggleScratch)),
    ("browse-versions", || Box::new(workspace::BrowseVersions)),
    ("undo-history", || Box::new(workspace::ShowUndoHistory)),
    ("rename-file", || Box::new(workspace::RenameFile)),
    ("duplicate-file", || Box::new(workspace::DuplicateFile)),
    ("move-file", || Box::new(workspace::MoveFile)),
    ("new-file-in-explorer", || Box::new(workspace::NewFileInExplorer)),
    ("new-folder-in-explorer", || Box::new(workspace::NewFolderInExplorer)),
    ("invisibles", || Box::new(app::ToggleInvisibles)),
    ("line-numbers", || Box::new(app::ToggleLineNumbers)),
    ("reading-width", || Box::new(app::ToggleReadingWidth)),
    ("soft-wrap", || Box::new(app::ToggleSoftWrap)),
    ("wrap-width", || Box::new(app::CycleWrapWidth)),
    ("smart-punctuation", || Box::new(app::ToggleSmartPunctuation)),
    ("revert", || Box::new(app::RevertToSaved)),
    ("save-all", || Box::new(app::SaveAll)),
    ("close-other-tabs", || Box::new(app::CloseOtherTabs)),
    ("move-tab-left", || Box::new(app::MoveTabLeft)),
    ("move-tab-right", || Box::new(app::MoveTabRight)),
    ("move-tab-up", || Box::new(app::MoveTabUp)),
    ("move-tab-down", || Box::new(app::MoveTabDown)),
    ("close-tabs-right", || Box::new(app::CloseTabsToTheRight)),
    ("close-tabs-left", || Box::new(app::CloseTabsToTheLeft)),
    ("move-tab-to-new-window", || Box::new(app::MoveTabToNewWindow)),
    ("toggle-pin-tab", || Box::new(app::TogglePinTab)),
    ("show-first-tab", || Box::new(app::ShowTab1)),
    ("show-last-tab", || Box::new(app::ShowTab9)),
    ("reopen-closed-tab", || Box::new(app::ReopenClosedTab)),
    ("indent-spaces", || Box::new(gpui_kit::base::input::ConvertIndentationToSpaces)),
    ("indent-tabs", || Box::new(gpui_kit::base::input::ConvertIndentationToTabs)),
    ("line-endings-lf", || Box::new(app::SetLineEndings { crlf: false })),
    ("line-endings-crlf", || Box::new(app::SetLineEndings { crlf: true })),
];

/// What `action` accepts, for its error: every table name, then the two kinds the table can't hold.
fn action_names() -> String {
    let names: Vec<&str> = ACTIONS.iter().map(|(name, _)| *name).collect();
    format!("{}, reopen-<encoding label> or save", names.join(", "))
}

/// `scratch [open|close|toggle]`: the scratch window without the shortcut (toggle when no word is
/// given). Replies `open` or `closed`; closing is asked for and takes a moment (the slide, a draft written).
fn scratch(word: &str, background: bool, cx: &mut App) -> Result<String, String> {
    let open = crate::scratch::is_open(cx);
    let now = match word {
        "" | "toggle" => crate::scratch::toggle(background, cx),
        "open" => open || crate::scratch::toggle(background, cx),
        "close" => {
            crate::scratch::dismiss(cx);
            false
        }
        _ => return Err("scratch takes open, close or toggle".to_owned()),
    };
    Ok(if now { "open" } else { "closed" }.to_owned())
}

/// `new-page`: File > New (Cmd-N), a new Untitled page in the active pane.
fn new_page(cx: &mut App) -> Result<String, String> {
    let (handle, _) = target(cx)?;
    cx.update_window(handle, |_, window, cx| window.dispatch_action(Box::new(app::NewTab), cx)).map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `new-markdown [<text>]`: a new page in Markdown mode with `text` (`\n` is a line break, `\\` a
/// backslash) and the preview showing, for checking the preview and anything else Markdown gets.
/// An Untitled page is Markdown only while its first line or its template says so, and note links
/// and pasted pictures need a folder, so the page is a file called `Untitled.md` in its own folder
/// under the temp folder, opened as a tab: it is not a draft. Replies with the path.
fn new_markdown(text: &str, cx: &mut App) -> Result<String, String> {
    static MADE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match (c, chars.as_str().chars().next()) {
            ('\\', Some('n')) => {
                unescaped.push('\n');
                chars.next();
            }
            ('\\', Some('\\')) => {
                unescaped.push('\\');
                chars.next();
            }
            _ => unescaped.push(c),
        }
    }
    let folder = std::env::temp_dir().join(format!("finch-markdown-{}-{}", std::process::id(), MADE.fetch_add(1, Ordering::Relaxed)));
    std::fs::create_dir_all(&folder).map_err(|err| err.to_string())?;
    let path = std::fs::canonicalize(&folder).map_err(|err| err.to_string())?.join("Untitled.md");
    std::fs::write(&path, unescaped).map_err(|err| err.to_string())?;
    let (handle, workspace) = target(cx)?;
    if !settings::get(cx).preview.visible {
        settings::update_later(cx, |settings| settings.preview.visible = true);
    }
    let shown = path.clone();
    cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.open_file(shown, window, cx)))
        .map_err(|err| err.to_string())?;
    Ok(path.display().to_string())
}

/// `new-from-template <name>`: File > New from template's item with that title (a built-in one or
/// a file name in the templates folder, any case), sent as the menu sends it.
fn new_from_template(name: &str, cx: &mut App) -> Result<String, String> {
    let template = crate::templates::find(name).ok_or_else(|| format!("new-from-template needs one of: {}", crate::templates::titles().join(", ")))?;
    let (handle, _) = target(cx)?;
    let action = Box::new(crate::templates::NewFromTemplate { template });
    cx.update_window(handle, |_, window, cx| window.dispatch_action(action, cx)).map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `welcome-back`: the wash a return after a long time away gives, on the active tab's caret line now (for tests; the window's activation can't be driven from here).
fn welcome_back(cx: &mut App) -> Result<String, String> {
    let (_, workspace) = target(cx)?;
    let document = workspace.read(cx).document(cx).ok_or("welcome-back needs an open tab")?;
    let editor = document.read(cx).built_editor().cloned().ok_or("welcome-back needs a built editor")?;
    editor.update(cx, |state, cx| state.wash_caret_line(crate::tokens::WELCOME_BACK_WASH, cx));
    Ok(String::new())
}

/// `scroll`: the editor's scroll place as `{"y":<n>}`, where a larger y is further down the document.
fn scroll(cx: &mut App) -> Result<String, String> {
    let (_, workspace) = target(cx)?;
    let document = workspace.read(cx).document(cx).ok_or("scroll needs an open tab")?;
    let editor = document.read(cx).built_editor().ok_or("scroll needs a built editor")?;
    let offset = editor.read(cx).scroll_offset();
    Ok(format!("{{\"y\":{}}}", -f32::from(offset.y)))
}

/// `preview-scroll`: the preview's scroll place as `{"item":<block>,"offset":<points into it>,"items":<blocks>,"source":<top byte, or null>}`.
/// The pair `item`, `offset` moves with a wheel over the preview; `source` is the Markdown byte at its top.
fn preview_scroll(cx: &mut App) -> Result<String, String> {
    let (_, workspace) = target(cx)?;
    let document = workspace.read(cx).document(cx).ok_or("preview-scroll needs an open tab")?;
    let document = document.read(cx);
    let preview = document.preview.as_ref().ok_or("preview-scroll needs a Markdown tab with a preview")?;
    let view = preview.state.read(cx);
    let list = view.list_state();
    let top = list.logical_scroll_top();
    let source = view.source_at_top().map_or("null".to_string(), |byte| byte.to_string());
    Ok(format!(
        "{{\"item\":{},\"offset\":{},\"items\":{},\"source\":{source}}}",
        top.item_ix,
        f32::from(top.offset_in_item),
        list.item_count()
    ))
}

/// `open-tab <path>`: opens the file in a tab of the target window, or brings its tab forward.
fn open_tab(path: &str, cx: &mut App) -> Result<String, String> {
    if path.is_empty() {
        return Err("open-tab needs a path".into());
    }
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| PathBuf::from(path));
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.open_file(path, window, cx)))
        .map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `tab <n>`: shows tab `n` of the target window, counting from 1.
fn tab(n: &str, cx: &mut App) -> Result<String, String> {
    let n: usize = n.parse().map_err(|_| "tab needs a number from 1".to_string())?;
    let (handle, workspace) = target(cx)?;
    let count = workspace.read(cx).tab_summary(cx).0.len();
    if n == 0 || n > count {
        return Err(format!("no tab {n}, the window has {count}"));
    }
    cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.activate_tab(n - 1, window, cx)))
        .map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `recent-tab [-1]`: one Ctrl-Tab (or, with -1, Ctrl-Shift-Tab) step through the recent tabs; the
/// history reorders only at `release-control`, as when Control is let go.
fn recent_tab(step: &str, cx: &mut App) -> Result<String, String> {
    let step: isize = if step.is_empty() { 1 } else { step.parse().map_err(|_| "recent-tab takes 1 or -1".to_string())? };
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.step_recent(step.signum(), window, cx)))
        .map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `release-control`: Control let go; a Ctrl-Tab walk confirms the tab it shows.
fn release_control(cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, _, cx| workspace.update(cx, |workspace, cx| workspace.end_switch(cx))).map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `close-tab`: closes the target window's showing tab, as Cmd-W does (an unsaved one asks).
fn close_tab(cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    let closed = cx
        .update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.close_active_tab(window, cx)))
        .map_err(|err| err.to_string())?;
    if closed { Ok(String::new()) } else { Err("no tab to close".into()) }
}

/// `close-window [n]`: closes window `n` (from 1, in open order; the one verbs act on by default)
/// as its close button does: Cmd-Shift-W's action, so unsaved tabs ask first (a background window
/// can't answer the sheet). The window goes a frame or two later; for memory proofs.
fn close_window(n: &str, cx: &mut App) -> Result<String, String> {
    let handle = if n.is_empty() {
        target(cx)?.0
    } else {
        let n: usize = n.parse().map_err(|_| "close-window takes a window number from 1".to_string())?;
        workspace::open_workspaces(cx).get(n.wrapping_sub(1)).ok_or("no such window")?.0
    };
    cx.update_window(handle, |_, window, cx| window.dispatch_action(Box::new(app::CloseWindow), cx)).map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `close-others [n]`, `close-right [n]` and `tab-menu [n]`: what the tab's right-click menu does
/// for tab `n` of the active pane (counting from 1; the showing tab without a number): close the
/// others, close the ones to its right, or open the menu (written to `GPUI_EDITOR_MENU_DUMP`
/// when set). `tab-menu <n> <row>` picks the row of that name (`Pin tab`, `Close tabs to the left`,
/// case ignored) as a click on it would, from the menu's own rows; an error names a missing or
/// greyed row. Unsaved tabs ask as they do from the menu.
fn tab_menu_verb(verb: &str, n: &str, cx: &mut App) -> Result<String, String> {
    let (n, row) = match n.split_once(char::is_whitespace) {
        Some((n, row)) if verb == "tab-menu" => (n, row.trim()),
        _ => (n, ""),
    };
    let n: Option<usize> = if n.is_empty() { None } else { Some(n.parse().map_err(|_| format!("{verb} takes a tab number from 1"))?) };
    let (handle, workspace) = target(cx)?;
    let found = cx
        .update_window(handle, |_, window, cx| {
            let pane = workspace.read(cx).active_pane().clone();
            pane.update(cx, |pane, cx| {
                let Some(id) = pane.id_at(n.map_or(pane.active_index(), |n| n.wrapping_sub(1))) else { return Ok::<bool, String>(false) };
                if !row.is_empty() {
                    pane.pick_tab_menu_row(id, row, window, cx)?;
                    return Ok(true);
                }
                match verb {
                    "close-others" => pane.close_others(id, window, cx),
                    "close-right" => pane.close_right(id, window, cx),
                    "pin" => pane.pin_id(id, cx),
                    "unpin" => pane.unpin_id(id, cx),
                    "reveal-in-explorer" => pane.reveal_in_explorer(id, cx),
                    _ => pane.show_tab_menu(id, window, cx),
                }
                Ok(true)
            })
        })
        .map_err(|err| err.to_string())??;
    if found { Ok(String::new()) } else { Err("no such tab".into()) }
}

/// `tab-list`: opens the tab list, the chevron at the strip's end, for the active pane (written to
/// `GPUI_EDITOR_MENU_DUMP` when set). An error while the strip fits and the chevron is hidden.
fn tab_list_verb(cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    let shown = cx
        .update_window(handle, |_, window, cx| {
            let pane = workspace.read(cx).active_pane().clone();
            pane.update(cx, |pane, cx| pane.show_tab_list(window, cx))
        })
        .map_err(|err| err.to_string())?;
    if shown { Ok(String::new()) } else { Err("the strip fits its tabs: no tab list".into()) }
}

/// `reopen-tab`: Cmd-Shift-T, reopens the tab closed last (its file, slot and caret).
fn reopen_tab(cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    let back = cx
        .update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.reopen_closed_tab(window, cx)))
        .map_err(|err| err.to_string())?;
    if back { Ok(String::new()) } else { Err("no closed tab to reopen".into()) }
}

/// `tab-state`: how many entries the active pane's bookkeeping holds, as JSON: `{"recent":..,"dropped":..,"untitled":..,"history":..,"known":..,"fitted":..,"marks":..,"queue":..,"status_line":<owners>,"books":<entries of the mark, log, patch and heading books>,"mark_words":<words and marks the mark layers of all editors keep>,"hidden_mark_words":<the part hidden tabs keep>,"wrap_width":<points or null>}`. Flat counts across rounds of new pages and closes mean nothing leaks.
fn tab_state(cx: &App) -> Result<String, String> {
    let (_, workspace) = target(cx)?;
    let ([recent, dropped, untitled, history], [known, fitted, marks, queue], wrap) = workspace.read(cx).state_sizes(cx);
    let wrap = wrap.map_or("null".to_string(), |width| width.to_string());
    let (marks_book, mark_words, hidden_mark_words) = crate::marks::state_sizes(cx);
    let books = marks_book + crate::log_follow::book_size(cx) + crate::patch_view::book_size(cx) + crate::heading_ticks::book_size(cx);
    Ok(format!(
        "{{\"recent\":{recent},\"dropped\":{dropped},\"untitled\":{untitled},\"history\":{history},\"known\":{known},\"fitted\":{fitted},\"marks\":{marks},\"queue\":{queue},\"status_line\":{},\"books\":{books},\"mark_words\":{mark_words},\"hidden_mark_words\":{hidden_mark_words},\"wrap_width\":{wrap}}}",
        crate::status_line::owner_count(cx)
    ))
}

/// `tabs`: the target window's active pane's tabs as JSON: `{"active":<index from 0>,"tabs":[{"title":..,"dirty":..,"preview":..,"pinned":..}]}`.
fn tabs(cx: &App) -> Result<String, String> {
    let (_, workspace) = target(cx)?;
    let (tabs, active) = workspace.read(cx).tab_summary(cx);
    Ok(tabs_json(&tabs, active))
}

fn tabs_json(tabs: &[(String, bool, bool, bool)], active: usize) -> String {
    let tabs: Vec<String> = tabs
        .iter()
        .map(|(title, dirty, preview, pinned)| format!("{{\"title\":{},\"dirty\":{dirty},\"preview\":{preview},\"pinned\":{pinned}}}", json_string(title)))
        .collect();
    format!("{{\"active\":{active},\"tabs\":[{}]}}", tabs.join(","))
}

/// `split right|down`: moves the active pane's showing tab into a new pane on its right or below,
/// as Cmd-\ and Cmd-Shift-\ do. A pane with one tab refuses.
fn split(side: &str, cx: &mut App) -> Result<String, String> {
    let axis = match side {
        "right" => settings::SplitAxis::Right,
        "down" => settings::SplitAxis::Down,
        _ => return Err("split needs right or down".into()),
    };
    let (handle, workspace) = target(cx)?;
    let split = cx
        .update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.split(axis, window, cx)))
        .map_err(|err| err.to_string())?;
    if split { Ok(String::new()) } else { Err("the pane needs two tabs to split".into()) }
}

/// `focus-pane <n>`: the keyboard goes to pane `n` of the target window, counting from 1 in
/// reading order (a split's first pane, then its second).
fn focus_pane(n: &str, cx: &mut App) -> Result<String, String> {
    let n: usize = n.parse().map_err(|_| "focus-pane needs a number from 1".to_string())?;
    let (handle, workspace) = target(cx)?;
    let focused = cx
        .update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.focus_pane(n, window, cx)))
        .map_err(|err| err.to_string())?;
    if focused { Ok(String::new()) } else { Err(format!("no pane {n}")) }
}

/// `split-share <pane> <share>`: the first side's share (0 to 1) of the split whose divider is nearest
/// pane `<pane>` (counting from 1, as `focus-pane` does), set the way a divider drag sets it, so the
/// pane minimums hold and a share past them stretches and springs back. Replies with the share kept.
fn split_share(rest: &str, cx: &mut App) -> Result<String, String> {
    let usage = || "split-share needs a pane from 1 and a share from 0 to 1".to_string();
    let (pane, share) = rest.split_once(char::is_whitespace).ok_or_else(usage)?;
    let pane: usize = pane.parse().map_err(|_| usage())?;
    let share: f32 = share.trim().parse().ok().filter(|share| (0.0..=1.0).contains(share)).ok_or_else(usage)?;
    let (handle, workspace) = target(cx)?;
    let kept = cx
        .update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.set_share(pane, share, window, cx)))
        .map_err(|err| err.to_string())?;
    kept.map(|kept| kept.to_string()).ok_or(format!("pane {pane} has no split to move (or isn't drawn yet)"))
}

/// `drop <path or tab:<pane>:<tab>> <pane> bar|left|right|top|bottom|middle`: what dropping a file
/// (from the explorer or Finder) or a tab (from another pane's bar) on pane `<pane>` does, run
/// through the same handler as the real drop. Panes and tabs count from 1; a bar drop lands after
/// the last tab.
fn drop_on(rest: &str, cx: &mut App) -> Result<String, String> {
    let (source, pane, zone) = drop_args("drop", rest)?;
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.drop_by_name(source, pane, zone, window, cx)))
        .map_err(|err| err.to_string())??;
    Ok(String::new())
}

/// The `<source> <pane> <zone>` of `drop` and `drag-hold`.
fn drop_args<'a>(verb: &str, rest: &'a str) -> Result<(&'a str, usize, Zone), String> {
    let words: Vec<&str> = rest.rsplitn(3, char::is_whitespace).collect();
    let [zone, pane, source] = words[..] else { return Err(format!("{verb} needs a path (or tab:<pane>:<tab>), a pane and bar, left, right, top, bottom or middle")) };
    let zone = match zone {
        "bar" => Zone::Bar(usize::MAX),
        "left" => Zone::Left,
        "right" => Zone::Right,
        "top" => Zone::Top,
        "bottom" => Zone::Bottom,
        "middle" => Zone::Middle,
        _ => return Err(format!("{verb} needs bar, left, right, top, bottom or middle")),
    };
    let pane: usize = pane.parse().map_err(|_| format!("{verb} needs a pane number from 1"))?;
    Ok((source.trim(), pane, zone))
}

/// `drag-hold <path or tab:<pane>:<tab>> <pane> bar|left|right|top|bottom|middle`: holds a drag of
/// that source over the pane's zone, so the ghost, the zone's preview and (over a bar) the lifted
/// tab draw on the next frame; `drag-release` ends it with no drop.
fn drag_hold(rest: &str, cx: &mut App) -> Result<String, String> {
    let (source, pane, zone) = drop_args("drag-hold", rest)?;
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, _, cx| workspace.update(cx, |workspace, cx| workspace.hold_drag(source, pane, zone, cx)))
        .map_err(|err| err.to_string())??;
    Ok(String::new())
}

/// `drag-release`: ends the held drag with no drop.
fn drag_release(cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, _, cx| workspace.update(cx, |workspace, cx| workspace.release_drag(cx))).map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `windows`: every window's tabs as `tabs` gives them, in the order `tab-to-window` counts windows from 1.
fn windows_verb(cx: &App) -> Result<String, String> {
    let windows: Vec<String> = workspace::open_workspaces(cx)
        .iter()
        .map(|(_, workspace)| {
            let (tabs, active) = workspace.read(cx).tab_summary(cx);
            tabs_json(&tabs, active)
        })
        .collect();
    Ok(format!("[{}]", windows.join(",")))
}

/// `tab-to-window <window> <pane> <tab> new|<window>`: what dragging a tab out of its window does
/// (a new window opens, or the tab joins another window's first pane at its end): the document
/// entity moves with its text, caret, scroll and unsaved edits, and the window left with no tab closes.
/// Windows come from `windows`; panes and tabs count from 1.
fn tab_to_window(rest: &str, cx: &mut App) -> Result<String, String> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let [from, pane, tab, to] = words[..] else { return Err("tab-to-window needs a window, a pane, a tab and new or a window".into()) };
    let number = |word: &str| word.parse::<usize>().ok().filter(|n| *n > 0).ok_or(format!("tab-to-window needs numbers from 1, not {word}"));
    let windows = workspace::open_workspaces(cx);
    let (source, workspace) = windows.get(number(from)? - 1).cloned().ok_or("no such window")?;
    let id = {
        let workspace = workspace.read(cx);
        let holder = workspace.pane_at(number(pane)?).ok_or("no such pane")?;
        workspace.tab_id(holder, number(tab)?, cx).ok_or("no such tab")?
    };
    let landing = if to == "new" {
        workspace::Landing::New(None)
    } else {
        let (handle, target) = windows.get(number(to)? - 1).cloned().ok_or("no such window")?;
        let pane = target.read(cx).pane_at(1).ok_or("that window has no pane")?;
        workspace::Landing::Window { handle, pane, at: usize::MAX }
    };
    workspace::move_tab_to(source, id, landing, cx)?;
    Ok(String::new())
}

/// `draw-as-key on|off`: the editor, the panes' focus light, the tree selection and the fields
/// draw as if the window were key, and GPUI reports the window active (animated images step).
/// Background launches only, so it is never on in a real app.
fn draw_as_key(arg: &str, background: bool, cx: &mut App) -> Result<String, String> {
    if !background {
        return Err("draw-as-key needs GPUI_EDITOR_BACKGROUND=1".into());
    }
    let on = match arg {
        "on" => true,
        "off" => false,
        _ => return Err("draw-as-key needs on or off".into()),
    };
    gpui_kit::base::input::set_draw_as_key(on);
    Window::set_force_window_active(on);
    cx.refresh_windows();
    Ok(String::new())
}

/// Whether `window` draws as the key window: it is, or `draw-as-key on` says so.
pub fn window_key(window: &Window) -> bool {
    gpui_kit::base::input::window_key(window)
}

/// `disk-state <path> conflict|gone`: marks the open file's document as changed on disk under
/// unsaved edits (`conflict`) or deleted (`gone`), as the watcher does.
fn disk_state(rest: &str, cx: &mut App) -> Result<String, String> {
    let (path, state) = rest.rsplit_once(char::is_whitespace).ok_or("disk-state needs a path and conflict or gone")?;
    let gone = match state {
        "conflict" => false,
        "gone" => true,
        _ => return Err("disk-state needs conflict or gone".into()),
    };
    let path = std::fs::canonicalize(path.trim()).unwrap_or_else(|_| PathBuf::from(path.trim()));
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.disk_state(&path, gone, window, cx)))
        .map_err(|err| err.to_string())??;
    Ok(String::new())
}

/// `panes`: the target window's panes as JSON, in reading order: `{"active":<pane from 1>,"panes":[<what `tabs` prints>..]}`.
fn panes(cx: &App) -> Result<String, String> {
    let (_, workspace) = target(cx)?;
    let (panes, active) = workspace.read(cx).pane_summary(cx);
    let panes: Vec<String> = panes.iter().map(|(tabs, active)| tabs_json(tabs, *active)).collect();
    Ok(format!("{{\"active\":{},\"panes\":[{}]}}", active + 1, panes.join(",")))
}

/// `type <text>`: types `text` into the field that holds the keyboard (find, replace, go to line, rename
/// field), else into the target window's showing editor at its cursor, as typed characters would (an
/// empty tab gets a draft file with the first one). Spaces are kept, even trailing; a literal `\n` is
/// Return (through the keymap, so auto-indent runs) and `\\` a backslash.
fn type_text(text: &str, cx: &mut App) -> Result<String, String> {
    if text.is_empty() {
        return Err("type needs some text".into());
    }
    let (handle, workspace) = target(cx)?;
    let typed = cx
        .update_window(handle, |_, window, cx| {
            // A named field with the keyboard takes the characters as key presses, which the platform hands to its input handler.
            let into_field = window
                .focused(cx)
                .is_some_and(|focused| FIELDS.with_borrow(|fields| fields.iter().any(|(_, weak)| weak.upgrade().is_some_and(|handle| handle == focused))));
            let editor = if into_field {
                None
            } else {
                let Some(document) = workspace.read(cx).document(cx) else { return false };
                Some(document.update(cx, |document, cx| document.editor(window, cx)))
            };
            let mut piece = String::new();
            let mut chars = text.chars().peekable();
            let flush = |piece: &mut String, window: &mut Window, cx: &mut App| {
                if !piece.is_empty() {
                    // The input handler's replace is what typing runs: every caret gets the text, a lone caret takes it as before.
                    match &editor {
                        Some(editor) => {
                            editor.update(cx, |state, cx| gpui_kit::EntityInputHandler::replace_text_in_range(state, None, piece, window, cx));
                        }
                        None => {
                            for ch in piece.chars() {
                                let key = if ch == ' ' { "space".to_string() } else { ch.to_lowercase().to_string() };
                                let keystroke = Keystroke { modifiers: Modifiers::default(), key, key_char: Some(ch.to_string()) };
                                window.dispatch_keystroke(keystroke, cx);
                            }
                        }
                    }
                    piece.clear();
                }
            };
            while let Some(ch) = chars.next() {
                match (ch, chars.peek()) {
                    ('\\', Some('n')) => {
                        chars.next();
                        flush(&mut piece, window, cx);
                        if let Some(editor) = editor.as_ref().filter(|editor| !editor.read(cx).focus_handle(cx).is_focused(window)) {
                            editor.update(cx, |state, cx| state.focus(window, cx));
                        }
                        window.dispatch_keystroke(Keystroke::parse("enter").expect("enter parses"), cx);
                    }
                    ('\\', Some('\\')) => {
                        chars.next();
                        piece.push('\\');
                    }
                    _ => piece.push(ch),
                }
            }
            flush(&mut piece, window, cx);
            true
        })
        .map_err(|err| err.to_string())?;
    if typed { Ok(String::new()) } else { Err("no tab to type in".into()) }
}

/// `key <chord>`: presses the chord (`shift-cmd-f`, `cmd-g`, `escape`) in the window as a key press
/// would, from what holds the keyboard now, and replies with the action its keymap binds there,
/// or `err no binding` (then nothing is pressed, so no letter is typed).
fn key(chord: &str, cx: &mut App) -> Result<String, String> {
    if chord.is_empty() {
        return Err("key needs a chord, like shift-cmd-f".into());
    }
    let keystroke = Keystroke::parse_with_os(chord, OperatingSystem::Mac).map_err(|err| err.to_string())?;
    let (handle, _) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        let stack = window.context_stack();
        let (bindings, _) = cx.key_bindings().borrow().bindings_for_input(std::slice::from_ref(&keystroke), &stack);
        let name = bindings.first().ok_or("no binding")?.action().name();
        window.dispatch_keystroke(keystroke, cx);
        Ok(name.to_string())
    })
    .map_err(|err| err.to_string())?
}

/// `quit`: Cmd-Q's path: quits at once with nothing unsaved, else asks (`answer <button>` reaches the sheet).
fn quit(cx: &mut App) -> Result<String, String> {
    workspace::quit(cx);
    Ok(String::new())
}

thread_local! {
    /// The text fields that name themselves for `focus`: their label and a weak handle on the field's focus.
    static FIELDS: RefCell<Vec<(&'static str, WeakFocusHandle)>> = const { RefCell::new(Vec::new()) };
}

/// Called by a field when it is made, so `focus` can name it.
pub fn name_field(label: &'static str, handle: FocusHandle) {
    FIELDS.with_borrow_mut(|fields| {
        fields.retain(|(_, weak)| weak.upgrade().is_some());
        if fields.iter().any(|(_, weak)| weak.upgrade().is_some_and(|known| known == handle)) {
            return;
        }
        fields.push((label, handle.downgrade()));
    });
}

thread_local! {
    /// The find bar's two text inputs by label (`find`, `replace`), newest last: the replace text lives in the bar's own input, which nothing else reads.
    static INPUTS: RefCell<Vec<(&'static str, WeakEntity<gpui_kit::component::input::InputState>)>> = const { RefCell::new(Vec::new()) };
}

/// Called by the find bar when it draws, so `field` can read what its fields hold.
pub fn name_input(label: &'static str, input: &Entity<gpui_kit::component::input::InputState>) {
    INPUTS.with_borrow_mut(|inputs| {
        inputs.retain(|(_, weak)| weak.upgrade().is_some());
        if !inputs.iter().any(|(_, weak)| weak.entity_id() == input.entity_id()) {
            inputs.push((label, input.downgrade()));
        }
    });
}

/// `field find|replace`: the text in the key editor's find bar's Find or Replace field, or `closed`
/// while its bar isn't open. `field replace` is `err` while the bar shows no replace row.
fn field(rest: &str, cx: &mut App) -> Result<String, String> {
    let which = match rest.trim() {
        which @ ("find" | "replace") => which,
        _ => return Err("field takes find or replace".into()),
    };
    let editor = app::key_editor(cx).ok_or("no editor")?;
    let session = editor.read(cx).search_session();
    if !session.open {
        return Ok("closed".into());
    }
    if which == "find" {
        return Ok(session.query.clone());
    }
    if !session.replace_mode {
        return Err("the replace row is not showing".into());
    }
    let input = INPUTS.with_borrow(|inputs| inputs.iter().rev().find(|(label, weak)| *label == "replace" && weak.upgrade().is_some()).and_then(|(_, weak)| weak.upgrade()));
    let input = input.ok_or("the replace field has not drawn")?;
    Ok(input.read(cx).value().to_string())
}

/// `focus`: what holds the keyboard: `editor`, `find`, `replace`, `go to line`, `open quickly`, `rename field`,
/// `other` (something that doesn't name itself: the tree or the filter) or `none`.
fn focus(cx: &mut App) -> Result<String, String> {
    let (handle, _) = target(cx)?;
    let editor = app::key_editor(cx);
    cx.update_window(handle, |_, window, cx| {
        let Some(focused) = window.focused(cx) else { return "none".to_string() };
        if editor.is_some_and(|editor| editor.read(cx).focus_handle(cx) == focused) {
            return "editor".to_string();
        }
        let named = FIELDS.with_borrow(|fields| fields.iter().find(|(_, weak)| weak.upgrade().is_some_and(|handle| handle == focused)).map(|(label, _)| *label));
        named.unwrap_or("other").to_string()
    })
    .map_err(|err| err.to_string())
}

/// `find [query]`: starts a search for `query` (smart case, as the bar) in the target window's first open
/// editor and highlights its matches; no query ends the search. `find --open [query]` opens the
/// find bar on the active tab's editor instead (as Cmd-F does), filled with `query` when given.
/// Replies with the match count. `find --scope` changes nothing and replies `on` or `off`: whether
/// the active editor's open search is limited to the selection (Find in selection); `err` while no
/// search is open.
fn find(rest: &str, cx: &mut App) -> Result<String, String> {
    let (flag, after) = split_word(rest);
    if flag == "--scope" {
        let (handle, workspace) = target(cx)?;
        return cx
            .update_window(handle, |_, _, cx| {
                let editor = workspace.read(cx).document(cx).and_then(|document| document.read(cx).built_editor().cloned()).ok_or("no editor to search")?;
                let session = editor.read(cx).search_session();
                if !session.open {
                    return Err("no search is open".to_string());
                }
                Ok(if session.in_selection() { "on" } else { "off" }.to_string())
            })
            .map_err(|err| err.to_string())?;
    }
    let (open, query) = if flag == "--open" { (true, after) } else { (false, rest) };
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, _, cx| {
        let editor = if open {
            workspace.read(cx).document(cx).and_then(|document| document.read(cx).built_editor().cloned()).ok_or("no editor to search")?
        } else {
            workspace.read(cx).documents(cx).iter().find_map(|document| document.read(cx).built_editor().cloned()).ok_or("no editor to search")?
        };
        Ok(editor.update(cx, |state, cx| {
            if open {
                state.open_search(false, cx);
            }
            if query.is_empty() {
                if open {
                    return state.search_session().matcher.matched_ranges().len().to_string();
                }
                state.close_search(cx);
                return String::new();
            }
            if !open {
                state.open_search(false, cx);
            }
            state.set_search_query(query, !query.chars().any(char::is_uppercase), cx);
            state.search_session().matcher.matched_ranges().len().to_string()
        }))
    })
    .map_err(|err| err.to_string())?
}

/// `text [max]`: the active editor's text as `{"text": <JSON string>, "cut": bool}`, at most `max`
/// bytes (default 64 KB, cut back to a character boundary) with `"cut":true` when it was longer.
fn text(rest: &str, cx: &mut App) -> Result<String, String> {
    let max = match rest {
        "" => 64 * 1024,
        _ => rest.parse::<usize>().map_err(|_| "text takes a byte count".to_string())?,
    };
    let editor = app::key_editor(cx).ok_or("no editor")?;
    let all = editor.read(cx).text().to_string();
    let mut end = all.len().min(max);
    while !all.is_char_boundary(end) {
        end -= 1;
    }
    Ok(format!("{{\"text\":{},\"cut\":{}}}", json_string(&all[..end]), end < all.len()))
}

/// `resize <w> <h>`: sets the target window's content size in points on its own `NSWindow`
/// (`setFrame:display:`), keeping the frame's top-left, so the app sees a live resize as a drag
/// makes. Never activates, orders or posts an event. Replies with the content size set. The
/// `setFrame:` runs on a later turn of the foreground executor, outside the app and window borrows,
/// since AppKit's resize callback updates the window synchronously.
fn resize(rest: &str, reply: mpsc::Sender<String>, cx: &mut App) {
    let native = (|| {
        use objc2::{msg_send, rc::Retained, runtime::AnyObject};
        let (w, h) = rest.split_once(char::is_whitespace).ok_or("resize needs <w> <h>")?;
        let (w, h) = match (w.trim().parse::<f64>(), h.trim().parse::<f64>()) {
            (Ok(w), Ok(h)) if w >= 1.0 && h >= 1.0 => (w, h),
            _ => return Err("resize takes a width and a height in points".to_string()),
        };
        let (handle, _) = target(cx)?;
        let native = cx
            .update_window(handle, |_, window, _| {
                if window.is_fullscreen() {
                    return Err("window is full screen".to_string());
                }
                let raw_window_handle::RawWindowHandle::AppKit(native) =
                    raw_window_handle::HasWindowHandle::window_handle(window).map_err(|err| err.to_string())?.as_raw()
                else {
                    return Err("no native window".to_string());
                };
                // SAFETY: NSView's `window`, on the main thread, where verbs run.
                let native: Option<Retained<AnyObject>> = unsafe { msg_send![native.ns_view.as_ptr().cast::<AnyObject>(), window] };
                native.ok_or_else(|| "no native window".to_string())
            })
            .map_err(|err| err.to_string())??;
        Ok((native, w, h))
    })();
    match native {
        Ok((native, w, h)) => {
            cx.foreground_executor().spawn(async move {
                use crate::window_look::{Point, Rect, Size};
                use objc2::msg_send;
                // SAFETY: NSWindow's frame getters and `setFrame:display:` on the main thread.
                let set = unsafe {
                    let frame: Rect = msg_send![&*native, frame];
                    let content: Rect = msg_send![&*native, contentRectForFrameRect: frame];
                    let grow = Size { width: w - content.size.width, height: h - content.size.height };
                    let next = Rect {
                        // AppKit's origin is the bottom-left, so the top-left stays by moving it down as the height grows.
                        origin: Point { x: frame.origin.x, y: frame.origin.y - grow.height },
                        size: Size { width: frame.size.width + grow.width, height: frame.size.height + grow.height },
                    };
                    let _: () = msg_send![&*native, setFrame: next, display: true];
                    let after: Rect = msg_send![&*native, frame];
                    let set: Rect = msg_send![&*native, contentRectForFrameRect: after];
                    set
                };
                reply.send(format!("{{\"w\":{},\"h\":{}}}", set.size.width, set.size.height)).ok();
            }).detach();
        }
        Err(err) => { reply.send(format!("err {err}")).ok(); }
    }
}

/// `selection`: the active editor's selection as `<start>..<end> "<text>"` in byte offsets, to read
/// back where Find next, Find previous or a click left the caret.
fn selection(cx: &mut App) -> Result<String, String> {
    let editor = app::key_editor(cx).ok_or("no editor")?;
    Ok(editor.update(cx, |state, _| {
        let range = state.selected_range();
        format!("{}..{} {:?}", range.start, range.end, state.selected_text().to_string())
    }))
}

/// `folds`: how the active editor's folding stands, as `{"rows":N,"candidates":K}`: the display rows it shows
/// (a fold hides its body's rows, so a fold at the caret lowers it) and the lines that can fold.
fn folds(cx: &mut App) -> Result<String, String> {
    let editor = app::key_editor(cx).ok_or("no editor")?;
    Ok(editor.update(cx, |state, _| format!("{{\"rows\":{},\"candidates\":{}}}", state.display_row_count(), state.fold_candidates().len())))
}

/// `editor-menu`: the editor's right-click menu, as a right-click makes it. With
/// `GPUI_EDITOR_MENU_DUMP` set (a background launch) the items are written there; send `frame`
/// after it, since the menu opens two frames on.
fn editor_menu(cx: &mut App) -> Result<String, String> {
    let (handle, _) = target(cx)?;
    let editor = app::key_editor(cx).ok_or("no editor")?;
    cx.update_window(handle, |_, window, cx| crate::panes::show_editor_menu(&editor, window, cx)).map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `find-next` and `find-prev`: jump to the next or previous match of the running search in the
/// window's first editor, as Find Next and Find Previous do. Replies with the match's byte range.
fn find_step(back: bool, cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, _, cx| {
        let editor = workspace.read(cx).documents(cx).iter().find_map(|document| document.read(cx).built_editor().cloned()).ok_or("no editor to search")?;
        editor.update(cx, |state, cx| {
            let range = if back { state.previous_search_match(cx) } else { state.next_search_match(cx) };
            range.map(|range| format!("{}..{}", range.start, range.end)).ok_or_else(|| "no match".to_string())
        })
    })
    .map_err(|err| err.to_string())?
}

/// `goto <line>[:<column>]`: moves the active tab's caret (the target window's active pane) to line `line` (from 1) and column `column` (from 1, the
/// line's start without one), as Go to Line does, and scrolls to show it sideways too.
fn goto(place: &str, cx: &mut App) -> Result<String, String> {
    let usage = || "goto needs a line number from 1, or line:column".to_string();
    let (line, column) = place.trim().split_once(':').map_or((place.trim(), "1"), |(line, column)| (line.trim(), column.trim()));
    let line: u32 = line.parse().map_err(|_| usage())?;
    let column: u32 = column.parse().map_err(|_| usage())?;
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        let document = workspace.read(cx).document(cx).ok_or("no document to move in")?;
        document.update(cx, |document, cx| document.go_to(crate::start::Caret { line, column }, window, cx));
        Ok(String::new())
    })
    .map_err(|err| err.to_string())?
}

/// `go-to-line <text>`: types `text` (`120` or `120:8`) into the open Go to line field (open it with
/// `action go-to-line`) and presses Return; `err` when the field is shut or the text isn't a place.
fn go_to_line(text: &str, cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.go_to_line_submit(text, window, cx)))
        .map_err(|err| err.to_string())??;
    Ok(String::new())
}

/// `goal <words>` or `goal off`: sets or clears the showing tab's session writing goal, as Format > Writing goal… does (a goal in
/// the file's front matter shows again once the session's is cleared). `err` for anything but a whole number in range or `off`.
fn goal(text: &str, cx: &mut App) -> Result<String, String> {
    let goal = match text.trim() {
        "off" => None,
        text => match crate::writing_goal::parse(text) {
            Some(Some(goal)) => Some(goal),
            _ => return Err(format!("goal needs a whole number from 1 to {} or off", crate::word_count::MAX_GOAL)),
        },
    };
    let (_, workspace) = target(cx)?;
    let document = workspace.read(cx).document(cx).ok_or("goal needs an open tab")?;
    let editor = document.read(cx).built_editor().cloned().ok_or("goal needs a built editor")?;
    crate::writing_goal::set(&document, goal, cx);
    // The status line reads the goal when its editor notifies.
    editor.update(cx, |_, cx| cx.notify());
    Ok(String::new())
}

/// `words-menu`: opens the word count pop-up in the status line, as a click on the count does (needs `GPUI_EDITOR_RECTS=1` and a
/// frame drawn, to find the count; `err` when the file shows no count). With `GPUI_EDITOR_MENU_DUMP` set the rows are written there.
fn words_menu(cx: &mut App) -> Result<String, String> {
    let position = rect_centre("status-words", cx).map_err(|why| format!("words-menu: {why}"))?;
    click_at(Modifiers::default(), position, cx)
}

/// `open-quickly <text>`: types `text` into the open Open Quickly float (open it with `action
/// open-quickly`) and presses Return; replies with the file it opened, `err` when the float is shut
/// or no file matches.
fn open_quickly(text: &str, cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    let path = cx
        .update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.open_quickly_submit(text, window, cx)))
        .map_err(|err| err.to_string())??;
    Ok(path.display().to_string())
}

/// `find-in-folder <text>`: types `text` into the open Find in folder float (open it with `action
/// find-in-folder`), searches at once and presses Return; replies with the `path:line` it opened, `err`
/// when the float is shut or nothing matches.
fn find_in_folder(text: &str, cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.find_in_folder_submit(text, window, cx))).map_err(|err| err.to_string())?
}

/// `versions-list [up|down|home|end|pageup|pagedown|return|escape]`: the open version picker. Bare, it replies
/// `{"selected":N,"total":N}` (the pick's 0-based row). The keymap doesn't bind these keys, so `key` can't press them: up, down, return
/// and escape call the picker's steps; home, end, pageup and pagedown send the real key to the window, through the picker's key handler.
fn versions_list(rest: &str, cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        let float = workspace.read(cx).versions_list_float().ok_or("the version picker isn't open")?;
        let word = rest.trim();
        match word {
            "" => {
                let (selected, total) = float.read(cx).selected();
                return Ok(format!("{{\"selected\":{selected},\"total\":{total}}}"));
            }
            "home" | "end" | "pageup" | "pagedown" => {
                let key = ["home", "end", "pageup", "pagedown"].into_iter().find(|key| *key == word).expect("one of the four");
                window.dispatch_keystroke(Keystroke::parse(key).expect("a valid key"), cx);
            }
            _ => float.update(cx, |float, cx| match word {
                "up" => Ok(float.step(false, cx)),
                "down" => Ok(float.step(true, cx)),
                "return" => Ok(float.commit(cx)),
                "escape" => Ok(float.close(cx)),
                _ => Err("versions-list takes up, down, home, end, pageup, pagedown, return or escape".to_string()),
            })?,
        }
        Ok(String::new())
    })
    .map_err(|err| err.to_string())?
}

/// `undo-history [to <n>|escape|close]`: the open Undo history slider (open it with `action
/// undo-history`). Bare, it replies `{"position":N,"total":N,"origin":N,"label":".."}`; `to` drags to
/// `n` steps applied and lets go (0 is before the first edit); `escape` puts the text back as it was
/// and closes; `close` closes keeping the text. `err` when the slider is shut.
fn undo_history(rest: &str, cx: &mut App) -> Result<String, String> {
    use crate::undo_slider::Close;
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        let slider = workspace.read(cx).undo_slider().ok_or("the Undo history slider isn't open")?;
        let (verb, arg) = rest.split_once(' ').map_or((rest, ""), |(verb, arg)| (verb, arg.trim()));
        match verb {
            "" => {}
            "to" => {
                let position = arg.parse().map_err(|_| "undo-history to needs a step number".to_string())?;
                slider.update(cx, |slider, cx| slider.to(position, window, cx));
            }
            "escape" | "close" => {
                let how = if verb == "escape" { Close::Restore } else { Close::Keep };
                slider.update(cx, |slider, cx| slider.shut(how, window, cx, true));
                return Ok(String::new());
            }
            _ => return Err("undo-history takes nothing, to <n>, escape or close".to_string()),
        }
        Ok(slider.read(cx).snapshot())
    })
    .map_err(|err| err.to_string())?
}

/// `draft-list [filter <text>|select <n>|return|trash]`: the open Open draft list (open it with
/// `action open-draft`). Bare, it replies `{"loaded":bool,"selected":N,"rows":[{title,empty,date,
/// words,open,path}]}` (`loaded` is false while the folder is still being read, ask again); `filter`
/// types `text` into the field and replies the same; `select` picks row `n` (from 1); `return` opens
/// the pick and replies its path; `trash` presses Cmd-Delete and replies the path it asked about (a
/// draft open in a tab isn't moved: its row says so). `err` when the list is shut.
fn draft_list(rest: &str, cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        let float = workspace.read(cx).draft_list_float(cx)?;
        let (verb, text) = rest.split_once(' ').map_or((rest, ""), |(verb, text)| (verb, text));
        match verb {
            "" => {}
            "filter" => float.update(cx, |float, cx| float.filter(text, window, cx)),
            "select" => {
                let number = text.trim().parse().map_err(|_| "draft-list select needs a row number from 1".to_string())?;
                float.update(cx, |float, cx| float.select(number, window, cx))?;
            }
            "return" => return float.update(cx, |float, cx| float.submit(window, cx)).map(|path| path.display().to_string()),
            "trash" => return float.update(cx, |float, cx| float.trash(cx)).map(|path| path.display().to_string()),
            _ => return Err("draft-list needs nothing, filter <text>, select <n>, return or trash".into()),
        }
        Ok(float.read(cx).snapshot())
    })
    .map_err(|err| err.to_string())?
}

/// `compare [use|cancel|save-as|next|prev|pick <change> yours|disk|pick-all yours|disk|copy <change> yours|disk]`: the active tab's Compare view (open it
/// with Compare… in the changed-on-disk alert). Bare, it replies `{"changes":N,"current":K,"picks":["yours",..]}` (`current` is the 1-based change at the top, 0 above the first);
/// the words press Use These, Cancel and Save as…, step to the next or previous change (an error at the last or first), take a side for change `<change>` (from 1), take it for every change,
/// or run the right-click menu's Copy for a side of a change, replying with the private pasteboard's text read back.
fn compare(rest: &str, cx: &mut App) -> Result<String, String> {
    use crate::compare::Side;
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, _, cx| {
        let view = workspace.read(cx).active_compare(cx).ok_or("no Compare view is open")?;
        let (word, args) = split_word(rest);
        view.update(cx, |view, cx| match word {
            "" => {
                let picks: Vec<&str> = view.picks().iter().map(|side| if *side == Side::Yours { "\"yours\"" } else { "\"disk\"" }).collect();
                Ok(format!("{{\"changes\":{},\"current\":{},\"picks\":[{}]}}", view.changes(), view.current().unwrap_or(0), picks.join(",")))
            }
            "use" | "cancel" | "save-as" => {
                match word {
                    "use" => view.use_these(cx),
                    "cancel" => view.cancel(cx),
                    _ => view.save_as(cx),
                }
                Ok(String::new())
            }
            "next" | "prev" => {
                if view.step(word == "next", cx) {
                    Ok(String::new())
                } else {
                    Err(format!("no {} change", if word == "next" { "next" } else { "previous" }))
                }
            }
            "pick" => {
                let (change, side) = split_word(args);
                let change = change.parse::<usize>().ok().filter(|n| (1..=view.changes()).contains(n)).ok_or("pick takes a change number from 1")?;
                let side = match side {
                    "yours" => Side::Yours,
                    "disk" => Side::Disk,
                    _ => return Err("pick takes yours or disk".to_string()),
                };
                view.pick(change - 1, side, cx);
                Ok(String::new())
            }
            "pick-all" => {
                let side = match args.trim() {
                    "yours" => Side::Yours,
                    "disk" => Side::Disk,
                    _ => return Err("pick-all takes yours or disk".to_string()),
                };
                view.pick_all(side, cx);
                Ok(String::new())
            }
            "copy" => {
                let (change, side) = split_word(args);
                let change = change.parse::<usize>().ok().filter(|n| (1..=view.changes()).contains(n)).ok_or("copy takes a change number from 1")?;
                let side = match side {
                    "yours" => Side::Yours,
                    "disk" => Side::Disk,
                    _ => return Err("copy takes yours or disk".to_string()),
                };
                let copied = view.copy(change - 1, side, cx).ok_or("that side has no lines in the change")?;
                let on_clipboard = cx.read_from_clipboard().and_then(|item| item.text()).unwrap_or_default();
                Ok(format!("{{\"copied\":{},\"clipboard\":{}}}", copied.len(), json_string(&on_clipboard)))
            }
            _ => Err("compare takes use, cancel, save-as, next, prev, pick, pick-all or copy".to_string()),
        })
    })
    .map_err(|err| err.to_string())?
}

/// `save-as <absolute path>`: Save As on the target window's showing tab with the path the panel
/// would have returned (a native panel can't be driven offscreen). Replies at once; the write and
/// the closing of other tabs on that file follow.
fn save_as(path: &str, cx: &mut App) -> Result<String, String> {
    if !Path::new(path).is_absolute() {
        return Err("save-as needs an absolute path".into());
    }
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        let document = workspace.read(cx).document(cx).ok_or("no document")?;
        document.update(cx, |document, cx| document.save_to(PathBuf::from(path), window, cx)).detach();
        Ok(String::new())
    })
    .map_err(|err| err.to_string())?
}

/// `export-html <absolute path>`: File > Export as HTML on the showing Markdown tab with the path the
/// panel would have returned (a native panel can't be driven offscreen). Replies at once; the write follows.
fn export_html(path: &str, cx: &mut App) -> Result<String, String> {
    if !Path::new(path).is_absolute() {
        return Err("export-html needs an absolute path".into());
    }
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        workspace.update(cx, |workspace, cx| workspace.export_html_to(PathBuf::from(path), window, cx));
    })
    .map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `print-pdf <out.pdf>`: File > Print on the showing tab, saved to the PDF with no print panel.
fn print_pdf(path: &str, cx: &mut App) -> Result<String, String> {
    if !path.starts_with('/') {
        return Err("print-pdf needs an absolute path".into());
    }
    app::print_showing(Some(std::path::Path::new(path)), cx)?;
    Ok(String::new())
}

/// `file-rename <name>`: File > Rename on the showing tab's file, as Return in the Rename field does
/// (the field closes if open); `err` carries the refusal for a taken, empty or invalid name.
fn file_rename(name: &str, cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.file_rename(name, window, cx)))
        .map_err(|err| err.to_string())??;
    Ok(String::new())
}

/// `file-duplicate`: File > Duplicate on the showing tab's file; the copy opens beside it.
fn file_duplicate(cx: &mut App) -> Result<String, String> {
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.file_duplicate(window, cx)))
        .map_err(|err| err.to_string())??;
    Ok(String::new())
}

/// `file-move <folder>`: File > Move to on the showing tab's file, into `folder` (no picker).
fn file_move(folder: &str, cx: &mut App) -> Result<String, String> {
    if folder.is_empty() {
        return Err("file-move needs a folder".into());
    }
    let folder = std::fs::canonicalize(folder).unwrap_or_else(|_| PathBuf::from(folder));
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| workspace.update(cx, |workspace, cx| workspace.file_move(&folder, window, cx)))
        .map_err(|err| err.to_string())??;
    Ok(String::new())
}

/// `wheel <dy> [x y] [dx=<n>]`: scrolls the pane under the point (window points,
/// default the editor's center) by `dy` points, as a mouse wheel would, and sideways by `dx`.
fn wheel(rest: &str, cx: &mut App) -> Result<String, String> {
    let mut dx = 0.;
    let mut numbers = Vec::new();
    for word in rest.split_whitespace() {
        if let Some(value) = word.strip_prefix("dx=") {
            dx = value.parse::<f32>().map_err(|_| format!("not a number: {word}"))?;
        } else {
            numbers.push(word.parse::<f32>().map_err(|_| format!("not a number: {word}"))?);
        }
    }
    let (dy, at) = match numbers[..] {
        [dy] => (dy, None),
        [dy, x, y] => (dy, Some(point(px(x), px(y)))),
        _ => return Err("wheel needs dy, or dy x y, then an optional dx=<n>".into()),
    };
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        let position = at.unwrap_or_else(|| workspace.read(cx).editor_center(window, cx));
        window.dispatch_event(
            PlatformInput::Scroll(ScrollEvent {
                position,
                delta: ScrollDelta::Pixels(point(px(dx), px(dy))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
                source: ScrollSource::Pointer,
            }),
            cx,
        );
    })
    .map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `pinch <delta> started|moved|ended|cancelled [x y]`: one pinch event (0.1 is 10% bigger) at a
/// window point (default the editor's center), through the real hit test, so a point over the
/// preview or the explorer does nothing. A background launch can't make the gesture itself.
fn pinch(rest: &str, cx: &mut App) -> Result<String, String> {
    let words: Vec<&str> = rest.split_whitespace().collect();
    let delta = words.first().and_then(|word| word.parse::<f32>().ok()).ok_or("pinch needs a delta, then a phase")?;
    let phase = match words.get(1).copied() {
        Some("started") => TouchPhase::Started,
        Some("moved") => TouchPhase::Moved,
        Some("ended") => TouchPhase::Ended,
        Some("cancelled") => TouchPhase::Cancelled,
        _ => return Err("pinch phase is started, moved, ended or cancelled".into()),
    };
    let at = match words[2..] {
        [] => None,
        [x, y] => match (x.parse::<f32>(), y.parse::<f32>()) {
            (Ok(x), Ok(y)) => Some(point(px(x), px(y))),
            _ => return Err("pinch x y must be numbers".into()),
        },
        _ => return Err("pinch needs delta phase, or delta phase x y".into()),
    };
    let (handle, workspace) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        let position = at.unwrap_or_else(|| workspace.read(cx).editor_center(window, cx));
        window.dispatch_event(
            PlatformInput::Pinch(PinchEvent { position, delta, modifiers: Modifiers::default(), phase }),
            cx,
        );
    })
    .map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// A leading modifier word of `click` and `hover`: `cmd`, `shift`, `alt` or `ctrl`, joined with
/// `+` for several (`cmd+shift`). Returns the modifiers held and the rest of the line.
fn split_modifiers(rest: &str) -> (Modifiers, &str) {
    let (word, tail) = split_word(rest);
    let mut modifiers = Modifiers::default();
    for name in word.split('+') {
        match name {
            "cmd" | "command" => modifiers.platform = true,
            "shift" => modifiers.shift = true,
            "alt" | "option" => modifiers.alt = true,
            "ctrl" | "control" => modifiers.control = true,
            _ => return (Modifiers::default(), rest),
        }
    }
    (modifiers, tail)
}

/// The centre of the rect `id` from `rects`, in window points.
fn rect_centre(id: &str, cx: &mut App) -> Result<Point<Pixels>, String> {
    if !rects_enabled() {
        return Err("rects are off; launch with GPUI_EDITOR_RECTS=1".into());
    }
    let [x, y, w, h] = snapshot(cx)
        .ok()
        .and_then(|(_, _, _, _, elements)| elements.get(id).copied())
        .ok_or_else(|| format!("no rect {id}; send frame first"))?;
    Ok(point(px(x + w / 2.0), px(y + h / 2.0)))
}

/// Tells the window which modifiers are held, as pressing or releasing a key does, so a handler
/// that reads `window.modifiers()` (Cmd over a link) sees them before the pointer events.
fn hold_modifiers(modifiers: Modifiers, window: &mut Window, cx: &mut App) {
    if window.modifiers() != modifiers {
        window.dispatch_event(PlatformInput::ModifiersChanged(ModifiersChangedEvent { modifiers, capslock: Capslock::default() }), cx);
    }
}

/// `click [cmd|shift|alt|ctrl[+...]] x y` or `click [mods] <rect id>`: a left click at window point
/// (x, y) or the centre of a rect from `rects`, as the mouse would make it: the modifiers go
/// down, then a move, a press and a release, then the modifiers come up. Reaches whatever is
/// under the point, a preview link or checkbox included; `click cmd x y` is a Cmd-click.
fn click(rest: &str, cx: &mut App) -> Result<String, String> {
    let (modifiers, rest) = split_modifiers(rest);
    let words: Vec<&str> = rest.split_whitespace().collect();
    let position = match words[..] {
        [id] if id.parse::<f32>().is_err() => rect_centre(id, cx)?,
        _ => {
            let numbers: Vec<f32> = words.iter().map(|word| word.parse::<f32>().map_err(|_| format!("not a number: {word}"))).collect::<Result<_, _>>()?;
            let [x, y] = numbers[..] else {
                return Err("click needs x y or a rect id, after any modifiers".into());
            };
            point(px(x), px(y))
        }
    };
    click_at(modifiers, position, cx)
}

/// `text-click [mods] <line> <column>` and `text-hover [mods] <line> <column>`: the same click or
/// move as `click` and `hover`, at the character at that place (both from 1, as `goto` takes them)
/// in the target window's showing editor, so a Cmd-click follows a link and a Cmd-hover shows the
/// link pill without working out window points by hand. `err` when that place isn't laid out
/// (scrolled out of view, folded, no frame drawn yet): send `frame` or `goto` first.
fn text_position(rest: &str, cx: &mut App) -> Result<(Modifiers, Point<Pixels>), String> {
    let (modifiers, rest) = split_modifiers(rest);
    let numbers: Vec<u32> = rest.split_whitespace().map(|word| word.parse::<u32>().map_err(|_| format!("not a number: {word}"))).collect::<Result<_, _>>()?;
    let [line, column] = numbers[..] else {
        return Err("needs a line and a column, from 1, after any modifiers".into());
    };
    let (_, workspace) = target(cx)?;
    let document = workspace.read(cx).document(cx).ok_or("no open tab")?;
    let editor = document.read(cx).built_editor().cloned().ok_or("no built editor")?;
    use gpui_kit::component::input::{Position, RopeExt as _};
    let state = editor.read(cx);
    let rope = state.text();
    let line = line.saturating_sub(1).min(rope.lines_len().saturating_sub(1) as u32);
    let offset = rope.position_to_offset(&Position::new(line, column.saturating_sub(1)));
    let bounds = state.range_to_bounds(&(offset..offset)).ok_or("that place isn't laid out; send frame, or goto it first")?;
    // A hair right of the character's left edge, half way down its line, so the hit lands on it.
    Ok((modifiers, point(bounds.left() + px(1.0), bounds.top() + bounds.size.height / 2.0)))
}

fn click_at(modifiers: Modifiers, position: Point<Pixels>, cx: &mut App) -> Result<String, String> {
    let (handle, _) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        hold_modifiers(modifiers, window, cx);
        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }), cx);
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position, modifiers, click_count: 1, first_mouse: false }),
            cx,
        );
        window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position, modifiers, click_count: 1 }), cx);
        hold_modifiers(Modifiers::default(), window, cx);
    })
    .map_err(|err| err.to_string())?;
    Ok(String::new())
}

fn text_click(rest: &str, cx: &mut App) -> Result<String, String> {
    let (modifiers, position) = text_position(rest, cx).map_err(|why| format!("text-click {why}"))?;
    click_at(modifiers, position, cx)
}

fn text_hover(rest: &str, cx: &mut App) -> Result<String, String> {
    if split_modifiers(rest).1.trim() == "off" {
        return hover(rest, cx);
    }
    let (modifiers, position) = text_position(rest, cx).map_err(|why| format!("text-hover {why}"))?;
    hover_at(modifiers, position, cx)
}

fn hover_at(modifiers: Modifiers, position: Point<Pixels>, cx: &mut App) -> Result<String, String> {
    let (handle, _) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        hold_modifiers(modifiers, window, cx);
        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }), cx);
    })
    .map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `right-click x y`: a right click at window point (x, y), as the mouse would make it: a move, then
/// a right press and release through the window's own event path (never a real pointer event). On
/// the preview or the editor it opens the right-click menu a frame or two on; with
/// `GPUI_EDITOR_MENU_DUMP` set the menu is written there, so send `frame` after it.
fn right_click(rest: &str, cx: &mut App) -> Result<String, String> {
    let numbers: Vec<f32> = rest
        .split_whitespace()
        .map(|word| word.parse::<f32>().map_err(|_| format!("not a number: {word}")))
        .collect::<Result<_, _>>()?;
    let [x, y] = numbers[..] else {
        return Err("right-click needs x y".into());
    };
    let position = point(px(x), px(y));
    let (handle, _) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Right,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Right, position, modifiers: Modifiers::default(), click_count: 1 }),
            cx,
        );
    })
    .map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `ime <start> <end> <text>`: deliver marked text and its relative UTF-16
/// selection through the editor's platform input handler, in background only.
fn ime(rest: &str, cx: &mut App) -> Result<String, String> {
    let (start, rest) = split_word(rest);
    let (end, text) = split_word(rest);
    let start = start.parse::<usize>().map_err(|_| "ime needs start end text")?;
    let end = end.parse::<usize>().map_err(|_| "ime needs start end text")?;
    if start > end || end > text.encode_utf16().count() {
        return Err("ime selection is outside its text".into());
    }
    let (handle, _) = target(cx)?;
    let editor = app::key_editor(cx).ok_or("no editor")?;
    cx.update_window(handle, |_, window, cx| {
        editor.update(cx, |state, cx| {
            state.focus(window, cx);
            gpui_kit::EntityInputHandler::replace_and_mark_text_in_range(
                state, None, text, Some(start..end), window, cx,
            );
        });
    }).map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `mouse down|move|up <x> <y> [clicks]`: synthetic held selection gesture,
/// background only. No real mouse input is posted.
fn mouse(rest: &str, cx: &mut App) -> Result<String, String> {
    let (phase, rest) = split_word(rest);
    let mut words = rest.split_whitespace();
    let mut coordinate = || words.next().ok_or("mouse needs phase x y")
        .and_then(|value| value.parse::<f32>().map_err(|_| "mouse needs phase x y"));
    let position = point(px(coordinate()?), px(coordinate()?));
    let clicks = words.next().unwrap_or("1").parse::<usize>()
        .map_err(|_| "mouse click count must be a number")?;
    let event = match phase {
        "down" => PlatformInput::MouseDown(MouseDownEvent {
            button: MouseButton::Left, position, modifiers: Modifiers::default(),
            click_count: clicks, first_mouse: false,
        }),
        "move" => PlatformInput::MouseMove(MouseMoveEvent {
            position, pressed_button: Some(MouseButton::Left), modifiers: Modifiers::default(),
        }),
        "up" => PlatformInput::MouseUp(MouseUpEvent {
            button: MouseButton::Left, position, modifiers: Modifiers::default(), click_count: clicks,
        }),
        _ => return Err("mouse phase must be down, move or up".into()),
    };
    let (handle, _) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        window.dispatch_event(event, cx);
    }).map_err(|err| err.to_string())?;
    Ok(String::new())
}

thread_local! {
    /// Where `press` holds the left button, until `release`.
    static PRESSED: Cell<Option<Point<Pixels>>> = const { Cell::new(None) };
}

/// `press <x> <y>` or `press <rect id>`: a mouse move to the window point (or the centre of that rect from `rects`), then a left
/// press that stays down, handed to the window through `window.dispatch_event` (never a real pointer event), so a pressed look can be
/// read frame by frame. `release [x y]` lets it go where it was pressed (or at that point, after a move with the button down); `err`
/// when no press is held. A second `press` while one is held is an `err`.
fn press(rest: &str, cx: &mut App) -> Result<String, String> {
    if PRESSED.get().is_some() {
        return Err("a press is already held; send release".into());
    }
    let words: Vec<&str> = rest.split_whitespace().collect();
    let position = match words[..] {
        [id] if id.parse::<f32>().is_err() => rect_centre(id, cx)?,
        [x, y] => point(px(x.parse::<f32>().map_err(|_| format!("not a number: {x}"))?), px(y.parse::<f32>().map_err(|_| format!("not a number: {y}"))?)),
        _ => return Err("press needs x y or a rect id".into()),
    };
    let (handle, _) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers: Modifiers::default() }), cx);
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position, modifiers: Modifiers::default(), click_count: 1, first_mouse: false }),
            cx,
        );
    })
    .map_err(|err| err.to_string())?;
    PRESSED.set(Some(position));
    Ok(String::new())
}

/// `release [x y]`: the left press `press` holds comes up (see `press`).
fn release(rest: &str, cx: &mut App) -> Result<String, String> {
    let held = PRESSED.get().ok_or("no press is held")?;
    let words: Vec<&str> = rest.split_whitespace().collect();
    let position = match words[..] {
        [] => held,
        [x, y] => point(px(x.parse::<f32>().map_err(|_| format!("not a number: {x}"))?), px(y.parse::<f32>().map_err(|_| format!("not a number: {y}"))?)),
        _ => return Err("release takes x y or nothing".into()),
    };
    let (handle, _) = target(cx)?;
    cx.update_window(handle, |_, window, cx| {
        if position != held {
            window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: Some(MouseButton::Left), modifiers: Modifiers::default() }), cx);
        }
        window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position, modifiers: Modifiers::default(), click_count: 1 }), cx);
    })
    .map_err(|err| err.to_string())?;
    PRESSED.set(None);
    Ok(String::new())
}

/// `tooltip`: the text of the tip showing in the target window, or empty (`tooltip::showing_text`).
fn tooltip_text(cx: &mut App) -> Result<String, String> {
    let (handle, _) = target(cx)?;
    Ok(crate::tooltip::showing_text(handle.window_id()).map(|text| text.to_string()).unwrap_or_default())
}

/// `hover [cmd|shift|alt|ctrl[+...]] <rect id> [x y]`, `hover [mods] x y` or `hover off`: rests the
/// pointer over the centre (or the window point `x y`) of a rect from `rects`, as a synthetic
/// mouse move handed to the window through GPUI's own event path (never a real pointer event), so
/// hover fades and tooltips start their waits. The modifiers stay held until the next `hover` or
/// `click` without them, so `hover cmd <link>` shows what Cmd over a link does. `off` moves the
/// pointer out of the window, as leaving it does, and lets the modifiers go.
fn hover(rest: &str, cx: &mut App) -> Result<String, String> {
    let (handle, _) = target(cx)?;
    let (modifiers, rest) = split_modifiers(rest);
    let mut words = rest.split_whitespace().peekable();
    let first = *words.peek().ok_or("hover needs a rect id, a point, or off")?;
    let by_point = first.parse::<f32>().is_ok();
    let id = if by_point { "" } else { words.next().unwrap_or_default() };
    let numbers: Vec<f32> = words.map(|word| word.parse::<f32>().map_err(|_| format!("not a number: {word}"))).collect::<Result<_, _>>()?;
    let position = if id == "off" {
        point(px(-10.0), px(-10.0))
    } else {
        match numbers[..] {
            [] if !by_point => rect_centre(id, cx)?,
            [x, y] => point(px(x), px(y)),
            _ => return Err("hover takes a rect id and optionally x y, or x y".into()),
        }
    };
    let modifiers = if id == "off" { Modifiers::default() } else { modifiers };
    cx.update_window(handle, |_, window, cx| {
        hold_modifiers(modifiers, window, cx);
        window.dispatch_event(PlatformInput::MouseMove(MouseMoveEvent { position, pressed_button: None, modifiers }), cx);
        if id == "off" {
            window.dispatch_event(PlatformInput::MouseExited(MouseExitEvent { position, pressed_button: None, modifiers }), cx);
        }
    })
    .map_err(|err| err.to_string())?;
    Ok(String::new())
}

/// `rich-copy`: background launches only. Runs the preview menu's Copy as Rich Text on the target
/// window's Markdown preview (the selection, else the whole file) into the launch's private
/// pasteboard, then replies with `copied` and the pasteboard's `public.html`, `public.rtf` and
/// `public.utf8-plain-text` entries (size and first bytes). The general pasteboard is untouched.
fn rich_copy(background: bool, reply: mpsc::Sender<String>, cx: &mut App) {
    let task = (|| {
        if !background {
            return Err("rich-copy is only available in a background launch".to_string());
        }
        let (_, workspace) = target(cx)?;
        let document = workspace.read(cx).document(cx).ok_or("rich-copy needs an open tab")?;
        let document = document.read(cx);
        let preview = document.preview.as_ref().ok_or("rich-copy needs a Markdown tab with a preview")?;
        let dir = document.path.as_deref().and_then(Path::parent).map(Path::to_path_buf);
        let state = preview.state.clone();
        Ok(crate::preview::render::copy_rich(&state, dir, cx))
    })();
    match task {
        Ok(task) => {
            cx.foreground_executor()
                .spawn(async move {
                    let copied = task.await;
                    reply.send(format!("copied={copied}; {}", crate::rich_copy::read_back())).ok();
                })
                .detach();
        }
        Err(err) => {
            reply.send(format!("err {err}")).ok();
        }
    }
}

/// Exercises native Services with a private pasteboard, outside any App/window borrow.
fn services(replacement: &str, background: bool, reply: mpsc::Sender<String>, cx: &mut App) {
    use objc2::{rc::Retained, runtime::AnyObject};
    let view = (|| {
        if !background {
            return Err("services is only available in a background launch".to_string());
        }
        let (handle, _) = target(cx)?;
        cx.update_window(handle, |_, window, _| {
            let raw_window_handle::RawWindowHandle::AppKit(native) =
                raw_window_handle::HasWindowHandle::window_handle(window)
                    .map_err(|err| err.to_string())?.as_raw()
            else {
                return Err("no native window".to_string());
            };
            unsafe { Retained::retain(native.ns_view.as_ptr().cast::<AnyObject>()) }
                .ok_or_else(|| "no native view".to_string())
        })
        .map_err(|err| err.to_string())?
    })();
    match view {
        Ok(view) => {
            let replacement = (!replacement.is_empty()).then(|| replacement.to_owned());
            cx.foreground_executor().spawn(async move {
                let result = crate::native_menu::probe_services(&view, replacement.as_deref());
                reply.send(result).ok();
            }).detach();
        }
        Err(err) => { reply.send(format!("err {err}")).ok(); }
    }
}

/// `answer <button title>`: presses the named button of the native sheet the window has open
/// (Revert to saved's, Discard edits', the changed-on-disk alert's), as a click does: the
/// button's own action ends the sheet and the completion handler `window.prompt` gave the
/// receiver resolves with that button's index, so the document runs its reply path unchanged.
/// Titles match without regard to case. A sheet shows a frame or two after the verb that asks.
fn answer(title: &str, cx: &mut App) -> Result<String, String> {
    use std::ffi::{CStr, c_char};
    use objc2::{class, msg_send, runtime::{AnyObject, Bool}};

    if title.is_empty() {
        return Err("answer needs a button title".into());
    }
    /// The title of an `NSButton`.
    unsafe fn button_title(button: *mut AnyObject) -> String {
        unsafe {
            let title: *mut AnyObject = msg_send![button, title];
            let chars: *const c_char = msg_send![title, UTF8String];
            if chars.is_null() { String::new() } else { CStr::from_ptr(chars).to_string_lossy().into_owned() }
        }
    }
    /// Every `NSButton` under `view`, depth first.
    unsafe fn buttons(view: *mut AnyObject, out: &mut Vec<*mut AnyObject>) {
        unsafe {
            let is_button: Bool = msg_send![view, isKindOfClass: class!(NSButton)];
            if is_button.as_bool() {
                out.push(view);
            }
            let subviews: *mut AnyObject = msg_send![view, subviews];
            let count: usize = msg_send![subviews, count];
            for ix in 0..count {
                let child: *mut AnyObject = msg_send![subviews, objectAtIndex: ix];
                buttons(child, out);
            }
        }
    }

    let (handle, _) = target(cx)?;
    cx.update_window(handle, |_, window, _| {
        let raw_window_handle::RawWindowHandle::AppKit(native) = raw_window_handle::HasWindowHandle::window_handle(window).map_err(|err| err.to_string())?.as_raw() else {
            return Err("no native window".to_string());
        };
        let view = native.ns_view.as_ptr().cast::<AnyObject>();
        unsafe {
            let ns_window: *mut AnyObject = msg_send![view, window];
            if ns_window.is_null() {
                return Err("no native window".to_string());
            }
            let sheet: *mut AnyObject = msg_send![ns_window, attachedSheet];
            if sheet.is_null() {
                return Err("no sheet is open".to_string());
            }
            let content: *mut AnyObject = msg_send![sheet, contentView];
            let mut found = Vec::new();
            buttons(content, &mut found);
            let titles: Vec<String> = found.iter().map(|button| button_title(*button)).collect();
            let Some(ix) = titles.iter().position(|name| name.eq_ignore_ascii_case(title)) else {
                return Err(format!("no button {title}; the sheet has {}", titles.join(", ")));
            };
            let _: () = msg_send![found[ix], performClick: std::ptr::null_mut::<AnyObject>()];
        }
        Ok(String::new())
    })
    .map_err(|err| err.to_string())?
}

/// `theme system|dark|light`.
fn set_theme(name: &str, cx: &mut App) -> Result<String, String> {
    let setting = match name {
        "system" => ThemeSetting::System,
        "dark" => ThemeSetting::Dark,
        "light" => ThemeSetting::Light,
        _ => return Err("theme needs system, dark or light".into()),
    };
    settings::update(cx, |settings| settings.window.theme = setting);
    theme::apply_setting(cx);
    cx.refresh_windows();
    Ok(String::new())
}

/// `appearance dark|light`: gives every window that appearance, as if the Mac had switched, so
/// the follow-the-Mac path (`theme system`) can be tried without touching the Mac's setting.
fn appearance(name: &str, cx: &mut App) -> Result<String, String> {
    let light = match name {
        "dark" => false,
        "light" => true,
        _ => return Err("appearance needs dark or light".into()),
    };
    for handle in cx.windows() {
        cx.update_window(handle, |_, window, _| window_look::force_appearance(window, light)).ok();
    }
    Ok(String::new())
}

/// `frame`: draws every window once, replying when it has. A window draws a
/// frame, then the callback queued behind it runs at the start of the next one.
fn frame(reply: mpsc::Sender<String>, cx: &mut App) {
    let drawn: Vec<_> = cx.windows().into_iter().filter_map(|handle| two_frames(handle, cx)).collect();
    cx.spawn(async move |_| {
        // A window closed before it drew drops its sender; that counts as done.
        for rx in drawn {
            rx.await.ok();
        }
        reply.send("ok".to_string()).ok();
    })
    .detach();
}

/// Refreshes `handle`'s window and resolves once it has drawn a frame and the next one has begun;
/// `None` when the window is gone.
fn two_frames(handle: AnyWindowHandle, cx: &mut App) -> Option<oneshot::Receiver<()>> {
    let (tx, rx) = oneshot::channel::<()>();
    cx.update_window(handle, |_, window, _| {
        window.refresh();
        window.on_next_frame(move |window, _| {
            window.on_next_frame(move |_, _| {
                tx.send(()).ok();
            });
        });
    })
    .ok()?;
    Some(rx)
}

/// The sidebar column and the top band are see-through (alpha 0) so the window's material shows
/// behind them, and a PNG of them would read as white or black with the viewer's backdrop. A flat
/// stand-in for the material goes behind them, light or dark by the window's ground near its bottom
/// right (not the middle, where an empty state's card sits); text and icons there keep their pixels. The render's alpha is premultiplied.
fn flatten_clear(image: &mut image::RgbaImage) {
    let (w, h) = image.dimensions();
    let [r, g, b, _] = image.get_pixel(w * 19 / 20, h * 19 / 20).0;
    let dark = (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000 < 128;
    let fill: [u32; 3] = if dark { [38, 38, 38] } else { [236, 236, 236] };
    for pixel in image.pixels_mut() {
        let behind = 255 - u32::from(pixel[3]);
        if behind > 0 {
            for (channel, fill) in pixel.0.iter_mut().zip(fill) {
                *channel = (u32::from(*channel) + fill * behind / 255).min(255) as u8;
            }
            pixel[3] = 255;
        }
    }
}

/// `snapshot <path> [settings]`: draws the target window once (as `frame` does), renders its last
/// scene offscreen at device scale with Metal and writes it to `path` as a PNG (finch#7nbd). No
/// screen capture, so it works where `screencapture` is refused and never asks for Screen Recording.
/// `settings` targets the Settings window instead. It leaves out what the compositor adds: the
/// sidebar material behind see-through areas (`flatten_clear` fills them with a flat stand-in),
/// the traffic lights and title bar, native layers (the caret layer) and the window shadow.
/// Replies `{"w","h"}` in pixels.
fn snapshot_png(rest: &str, reply: mpsc::Sender<String>, cx: &mut App) {
    let (path, settings) = match rest.strip_suffix(" settings") {
        Some(path) => (path.trim(), true),
        None => (rest.trim(), false),
    };
    let found = if path.is_empty() {
        Err("snapshot needs a path".to_string())
    } else if !path.starts_with('/') {
        Err("snapshot needs an absolute path".to_string())
    } else if settings {
        crate::settings_window::handle(cx).ok_or_else(|| "no Settings window".to_string())
    } else {
        target(cx).map(|(handle, _)| handle)
    };
    let drawn = found.and_then(|handle| two_frames(handle, cx).map(|rx| (handle, rx)).ok_or_else(|| "window is gone".to_string()));
    let (handle, drawn) = match drawn {
        Ok(found) => found,
        Err(why) => {
            reply.send(format!("err {why}")).ok();
            return;
        }
    };
    let path = PathBuf::from(path);
    cx.spawn(async move |cx| {
        drawn.await.ok();
        let image = cx.update(|cx| handle.update(cx, |_, window, _| window.render_to_image())).and_then(|image| image);
        let text = match image {
            Ok(image) => {
                let (w, h) = image.dimensions();
                let saved = cx
                    .background_executor()
                    .spawn(async move {
                        let mut image = image;
                        flatten_clear(&mut image);
                        image.save_with_format(&path, image::ImageFormat::Png)
                    })
                    .await;
                match saved {
                    Ok(()) => format!("{{\"w\":{w},\"h\":{h}}}"),
                    Err(err) => format!("err could not write the picture: {err}"),
                }
            }
            Err(err) => format!("err {err}"),
        };
        reply.send(text).ok();
    })
    .detach();
}

/// `action move-tab-left|right|up|down`: the Window menu's move, run once the window has drawn a
/// frame (a pane made a moment ago has no place to be a neighbour until then, and the key would
/// beep). Replies `err` when no tab changed pane, since the action itself never says.
fn move_tab_action(name: &str, reply: mpsc::Sender<String>, cx: &mut App) {
    let (action, side): (Box<dyn Action>, &str) = match name {
        "move-tab-left" => (Box::new(app::MoveTabLeft), "left"),
        "move-tab-right" => (Box::new(app::MoveTabRight), "right"),
        "move-tab-up" => (Box::new(app::MoveTabUp), "upper"),
        _ => (Box::new(app::MoveTabDown), "lower"),
    };
    let (handle, workspace) = match target(cx) {
        Ok(found) => found,
        Err(err) => {
            reply.send(format!("err {err}")).ok();
            return;
        }
    };
    let Some(drawn) = two_frames(handle, cx) else {
        reply.send("err the window is gone".into()).ok();
        return;
    };
    cx.spawn(async move |cx| {
        drawn.await.ok();
        // A pane split a moment ago has no neighbours until its opening glide has laid it out, so a move that changed nothing is sent again for up to half a second before it is called refused.
        let mut text = format!("err the tab did not move: no {side} pane beside the active one");
        for attempt in 0..6 {
            if attempt > 0 {
                cx.background_executor().timer(std::time::Duration::from_millis(100)).await;
            }
            // The window runs the action a turn after `dispatch_action` returns, so the tabs are read again after the next frames.
            let sent = cx.update(|cx| {
                let before = workspace.read(cx).pane_summary(cx);
                cx.update_window(handle, |_, window, cx| window.dispatch_action(action.boxed_clone(), cx)).map_err(|err| err.to_string())?;
                Ok::<_, String>((before, two_frames(handle, cx)))
            });
            let (before, settled) = match sent {
                Ok(sent) => sent,
                Err(err) => {
                    text = format!("err {err}");
                    break;
                }
            };
            if let Some(settled) = settled {
                settled.await.ok();
            }
            if cx.update(|cx| workspace.read(cx).pane_summary(cx)) != before {
                text = "ok".to_string();
                break;
            }
        }
        reply.send(text).ok();
    })
    .detach();
}

/// `drag-frames <count>`: sample editor scroll offsets at consecutive window
/// frames. Returns elapsed milliseconds and x/y offsets as JSON, background only.
fn drag_frames(rest: &str, reply: mpsc::Sender<String>, cx: &mut App) {
    let count = match rest.parse::<usize>() {
        Ok(count @ 1..=120) => count,
        _ => {
            reply.send("err drag-frames needs a count from 1 to 120".into()).ok();
            return;
        }
    };
    let (handle, _) = match target(cx) {
        Ok(target) => target,
        Err(err) => { reply.send(format!("err {err}")).ok(); return; }
    };
    let Some(editor) = app::key_editor(cx) else {
        reply.send("err no editor".into()).ok();
        return;
    };
    let failed = reply.clone();
    if let Err(err) = cx.update_window(handle, |_, window, _| {
        sample_drag_frame(count, std::time::Instant::now(), Vec::with_capacity(count), reply, editor, window);
    }) {
        failed.send(format!("err {err}")).ok();
    }
}

fn sample_drag_frame(
    remaining: usize,
    started: std::time::Instant,
    mut samples: Vec<String>,
    reply: mpsc::Sender<String>,
    editor: gpui_kit::Entity<gpui_kit::component::input::EditorState>,
    window: &gpui_kit::Window,
) {
    window.on_next_frame(move |window, cx| {
        let offset = editor.read(cx).scroll_offset();
        samples.push(format!(
            "{{\"elapsed_ms\":{:.3},\"x\":{},\"y\":{}}}",
            started.elapsed().as_secs_f64() * 1000.,
            -f32::from(offset.x), -f32::from(offset.y),
        ));
        if remaining == 1 {
            reply.send(format!("[{}]", samples.join(","))).ok();
        } else {
            sample_drag_frame(remaining - 1, started, samples, reply, editor, window);
        }
    });
}

/// `rects`: the last frame's element rects in the target window, as JSON.
fn rects(cx: &mut App) -> Result<String, String> {
    if !rects_enabled() {
        return Err("rects are off; launch with GPUI_EDITOR_RECTS=1".into());
    }
    let (handle, number, scale, tracked, mut elements) = snapshot(cx)?;
    let scrolls = RECTS.with_borrow(|all| all.get(&handle.window_id()).map(|frame| frame.scrolls.clone()).unwrap_or_default());
    // The washes the editors painted in this very frame; one an editor stopped painting is over.
    let mut alphas: BTreeMap<String, f32> = BTreeMap::new();
    RECTS.with_borrow(|all| {
        let Some(frame) = all.get(&handle.window_id()) else { return };
        let rows = frame.washes.values().filter(|(_, _, seen)| *seen == frame.number).flat_map(|(rects, alpha, _)| rects.iter().map(move |place| (*place, *alpha)));
        for (n, (place, alpha)) in rows.enumerate() {
            elements.insert(format!("wash:{}", n + 1), place);
            alphas.insert(format!("wash:{}", n + 1), alpha);
        }
    });
    let elements: Vec<String> = elements
        .iter()
        .map(|(id, place)| {
            let [x, y, w, h] = *place;
            // `editor` is the active pane's rect, so it takes the scroll of the `editor:<pane>` drawn in the same place.
            let scroll = scrolls.get(id.as_str()).or_else(|| {
                if id != "editor" {
                    return None;
                }
                let (other, _) = elements.iter().find(|(other, at)| other.starts_with("editor:") && *at == place)?;
                scrolls.get(other.as_str())
            });
            let scroll = scroll.map_or(String::new(), |[sx, sy]| format!(",\"scroll\":{{\"x\":{sx},\"y\":{sy}}}"));
            let alpha = alphas.get(id.as_str()).map_or(String::new(), |alpha| format!(",\"alpha\":{alpha}"));
            format!("{}:{{\"x\":{x},\"y\":{y},\"w\":{w},\"h\":{h}{scroll}{alpha}}}", json_string(id))
        })
        .collect();
    Ok(format!(
        "{{\"version\":1,\"window_number\":{},\"frame\":{},\"scale\":{},\"tracked_windows\":{},\"elements\":{{{}}}}}",
        handle.window_id().as_u64(),
        number,
        scale,
        tracked,
        elements.join(",")
    ))
}

fn json_string(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for c in text.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Whether element rects are recorded: `GPUI_EDITOR_RECTS=1` at launch.
/// Off, every tracker below is absent and the rows' record call is one branch.
pub fn rects_enabled() -> bool {
    static ON: LazyLock<bool> = LazyLock::new(|| std::env::var_os("GPUI_EDITOR_RECTS").is_some_and(|value| value == "1"));
    *ON
}

/// Starts listening for the editors' line washes when rects are on, at launch.
pub fn listen_for_washes() {
    if rects_enabled() {
        gpui_kit::base::input::set_wash_probe(wash_probe);
    }
}

/// One window's last frame of rects, in window points.
struct Frame {
    number: u64,
    scale: f32,
    elements: BTreeMap<String, [f32; 4]>,
    /// Each editor pane's rect and the frame it was last drawn in, by the pane view's id.
    panes: BTreeMap<u64, ([f32; 4], u64)>,
    /// The scroll place `track_scrolled` read for each element (`editor:<pane>`, `preview`), x then y, down and right positive.
    scrolls: BTreeMap<String, [f32; 2]>,
    /// The line washes each editor pane (by its view's id) painted last: the rects, the alpha they drew at and the frame it was in.
    washes: BTreeMap<u64, (Vec<[f32; 4]>, f32, u64)>,
}

thread_local! {
    static RECTS: RefCell<HashMap<WindowId, Frame>> = RefCell::new(HashMap::new());
}

/// Drops every rect under `prefix` from `window`'s last frame, for a view that records them all again as it paints.
pub fn forget_prefix(window: WindowId, prefix: &str) {
    RECTS.with_borrow_mut(|all| {
        if let Some(frame) = all.get_mut(&window) {
            frame.elements.retain(|id, _| !id.starts_with(prefix));
        }
    });
}

/// Drops one element's rect from `window`'s last frame, for a floating thing that has gone and
/// left no view to clear it (a tooltip).
pub fn unrecord(window: WindowId, id: &str) {
    RECTS.with_borrow_mut(|all| {
        if let Some(frame) = all.get_mut(&window) {
            frame.elements.remove(id);
        }
    });
}

/// Drops a closed window's rects (`row::forget`, which runs when its explorer is released).
pub fn forget(window: WindowId) {
    RECTS.with_borrow_mut(|all| {
        all.remove(&window);
    });
}

/// Records `bounds` under `id` for the frame being laid out.
pub fn record(window: &Window, id: impl Into<String>, bounds: Bounds<Pixels>) {
    RECTS.with_borrow_mut(|all| {
        if let Some(frame) = all.get_mut(&window.window_handle().window_id()) {
            frame.elements.insert(
                id.into(),
                [
                    f32::from(bounds.origin.x),
                    f32::from(bounds.origin.y),
                    f32::from(bounds.size.width),
                    f32::from(bounds.size.height),
                ],
            );
        }
    });
}

/// What the editor tells of a line wash it paints (`set_wash_probe`): kept for `rects` as `wash:<n>` with its alpha.
fn wash_probe(window: &Window, drawn: &[Bounds<Pixels>], alpha: f32) {
    RECTS.with_borrow_mut(|all| {
        if let Some(frame) = all.get_mut(&window.window_handle().window_id()) {
            let places = drawn.iter().map(|bounds| [f32::from(bounds.origin.x), f32::from(bounds.origin.y), f32::from(bounds.size.width), f32::from(bounds.size.height)]).collect();
            frame.washes.insert(window.current_view().as_u64(), (places, alpha, frame.number));
        }
    });
}

/// The first child of the window's root: counts a frame and keeps the window's size. It clears
/// nothing, since a cached view replays without recording and its rects must stay; a view whose
/// elements come and go clears its own with `fresh`. `None` unless rects are on.
pub fn frame_marker() -> Option<impl IntoElement> {
    rects_enabled().then(|| {
        canvas(
            |bounds, window, _| {
                let scale = window.scale_factor();
                RECTS.with_borrow_mut(|all| {
                    let frame = all.entry(window.window_handle().window_id()).or_insert(Frame {
                        number: 0,
                        scale,
                        elements: BTreeMap::new(),
                        panes: BTreeMap::new(),
                        scrolls: BTreeMap::new(),
                        washes: BTreeMap::new(),
                    });
                    frame.number += 1;
                    frame.scale = scale;
                });
                record(window, "window", bounds);
            },
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0()
    })
}

/// The first child of a view whose elements come and go: forgets the rects under `prefix`
/// each time the view builds, so rows gone from its tree drop out. `None` unless rects are on.
pub fn fresh(prefix: &'static str) -> Option<impl IntoElement> {
    rects_enabled().then(|| {
        canvas(
            move |_, window, _| {
                RECTS.with_borrow_mut(|all| {
                    if let Some(frame) = all.get_mut(&window.window_handle().window_id()) {
                        frame.elements.retain(|id, _| !id.starts_with(prefix));
                    }
                });
            },
            |_, (), _, _| {},
        )
        .absolute()
        .size_0()
    })
}

/// Records the bounds of the element it sits in (as its last child, filling
/// it) under `id`. `None` unless rects are on.
pub fn track(id: &'static str) -> Option<impl IntoElement> {
    rects_enabled().then(|| {
        canvas(
            move |bounds, window, _| {
                // A split's panes each track "editor": keep one per pane (`panes`), and let `snapshot` name them.
                if id == "editor" {
                    record_pane(window, bounds);
                } else {
                    record(window, id, bounds);
                }
            },
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0()
    })
}

/// `track` for an element that scrolls (an editor pane, the preview): also keeps `scroll()` as the
/// element's scroll place in `rects`, read at the same prepaint. `scroll` gives the content's
/// offset as the widgets keep it (negative once scrolled down); `rects` reports its negation.
pub fn track_scrolled(id: &'static str, scroll: impl Fn(&App) -> Point<Pixels> + 'static) -> Option<impl IntoElement> {
    rects_enabled().then(|| {
        canvas(
            move |bounds, window, cx| {
                let offset = scroll(cx);
                let key = if id == "editor" {
                    record_pane(window, bounds);
                    format!("editor:{}", window.current_view().as_u64())
                } else {
                    record(window, id, bounds);
                    id.to_string()
                };
                RECTS.with_borrow_mut(|all| {
                    if let Some(frame) = all.get_mut(&window.window_handle().window_id()) {
                        let flip = |at: Pixels| if f32::from(at) == 0. { 0. } else { -f32::from(at) };
                        frame.scrolls.insert(key, [flip(offset.x), flip(offset.y)]);
                    }
                });
            },
            |_, (), _, _| {},
        )
        .absolute()
        .inset_0()
    })
}

/// Records the editor pane being laid out (the view `window` is building) with the frame it was seen in.
fn record_pane(window: &Window, bounds: Bounds<Pixels>) {
    let pane = window.current_view().as_u64();
    RECTS.with_borrow_mut(|all| {
        if let Some(frame) = all.get_mut(&window.window_handle().window_id()) {
            let place = [
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                f32::from(bounds.size.width),
                f32::from(bounds.size.height),
            ];
            frame.panes.insert(pane, (place, frame.number));
        }
    });
}

/// A frame's elements with its editor panes named in: `editor:<pane>` for each live pane, and
/// `editor` for the one holding `focus` (the active pane's middle), else the last one drawn.
/// A pane that closed leaves its rect behind (its view is gone, so nothing clears it); it
/// overlaps the pane that took its room, which was drawn later, so the older of two overlapping
/// panes is dropped.
fn with_panes(frame: &Frame, focus: Option<(f32, f32)>) -> BTreeMap<String, [f32; 4]> {
    let mut elements = frame.elements.clone();
    let mut newest: Vec<(&u64, &([f32; 4], u64))> = frame.panes.iter().collect();
    newest.sort_by_key(|(_, (_, seen))| std::cmp::Reverse(*seen));
    let mut live: Vec<(u64, [f32; 4])> = Vec::new();
    for (pane, (place, _)) in newest {
        let apart = live.iter().all(|(_, other)| {
            place[0] + place[2] <= other[0] || other[0] + other[2] <= place[0] || place[1] + place[3] <= other[1] || other[1] + other[3] <= place[1]
        });
        if apart {
            live.push((*pane, *place));
        }
    }
    let inside = |place: &[f32; 4], (x, y): (f32, f32)| x >= place[0] && x < place[0] + place[2] && y >= place[1] && y < place[1] + place[3];
    let focused = focus.and_then(|at| live.iter().find(|(_, place)| inside(place, at))).or(live.first());
    if let Some((_, place)) = focused {
        elements.insert("editor".to_string(), *place);
    }
    for (pane, place) in &live {
        elements.insert(format!("editor:{pane}"), *place);
    }
    elements
}

/// An explorer that left the tree (hidden, or minimal) is no longer drawn, so nothing rewrites its rects: the last one stays, where the
/// column's slide ended, left of the window (its right edge at the 0.5 pt hairline). Its rects (`explorer*`, `row:*`, `filter-clear`) go
/// once no more than that hairline is in the window. A column sliding still has more in it, so its series stays whole.
fn without_gone_explorer(mut elements: BTreeMap<String, [f32; 4]>) -> BTreeMap<String, [f32; 4]> {
    let Some(left) = elements.get("window").map(|window| window[0]) else { return elements };
    elements.retain(|id, [x, _, w, _]| !(id.starts_with("explorer") || id.starts_with("row:") || id == "filter-clear") || *x + *w > left + 1.);
    elements
}

/// The target window's rects with its panes named in (`with_panes`), for `rects` and `hover`.
fn snapshot(cx: &mut App) -> Result<(AnyWindowHandle, u64, f32, usize, BTreeMap<String, [f32; 4]>), String> {
    let (handle, workspace) = target(cx)?;
    let focus = cx
        .update_window(handle, |_, window, cx| {
            let at = workspace.read(cx).editor_center(window, cx);
            (f32::from(at.x), f32::from(at.y))
        })
        .ok();
    RECTS.with_borrow(|all| {
        let frame = all.get(&handle.window_id()).ok_or("no frame recorded yet")?;
        Ok((handle, frame.number, frame.scale, all.len(), without_gone_explorer(with_panes(frame, focus))))
    })
}

/// `finch debug <verb> [args]`: sends the verb to the running app,
/// prints the reply and exits (nonzero on an error reply or no app).
pub fn client(args: &[OsString]) -> ! {
    let Some(verb) = args.first().and_then(|verb| verb.to_str()) else {
        eprintln!("usage: finch debug <verb> [args]");
        std::process::exit(2);
    };
    let mut words: Vec<String> = args.iter().map(|arg| arg.to_string_lossy().into_owned()).collect();
    // Paths are resolved here: the app's working folder isn't the caller's.
    let path_at = match verb {
        "open" if words.get(1).is_some_and(|flag| flag == "--tab") => Some(2),
        "open" => Some(1),
        "file-move" | "print-pdf" | "snapshot" => Some(1),
        "explorer" if words.get(1).is_none_or(|action| !matches!(action.as_str(), "filter" | "key" | "sort")) => Some(2),
        _ => None,
    };
    // `snapshot <path> settings`: the trailing word names the window, not the path.
    let settings_word = (verb == "snapshot" && words.len() > 2 && words.last().is_some_and(|word| word == "settings")).then(|| words.pop()).flatten();
    if let Some(at) = path_at.filter(|at| *at < words.len()) {
        let path = words[at..].join(" ");
        if let Ok(path) = std::path::absolute(&path) {
            words.truncate(at);
            words.push(path.to_string_lossy().into_owned());
        }
    }
    words.extend(settings_word);
    let path = socket_path();
    let outcome = UnixStream::connect(&path).map_err(|err| format!("no app on {}: {err}", path.display())).and_then(|mut stream| {
        stream.set_read_timeout(Some(REPLY_WAIT + Duration::from_secs(5))).ok();
        writeln!(stream, "{}", words.join(" ")).map_err(|err| err.to_string())?;
        let mut reply = String::new();
        BufReader::new(stream).read_line(&mut reply).map_err(|err| err.to_string())?;
        Ok(reply.trim_end().to_string())
    });
    match outcome {
        Ok(reply) if reply.starts_with("err") || reply.is_empty() => {
            eprintln!("{reply}");
            std::process::exit(1);
        }
        Ok(reply) => {
            println!("{reply}");
            std::process::exit(0);
        }
        Err(why) => {
            eprintln!("finch: {why}");
            std::process::exit(1);
        }
    }
}

/// How long `hand_over` waits for the socket to accept before the caller starts its own copy.
const CONNECT_WAIT: Duration = Duration::from_millis(10);

/// How long `hand_over` waits for the reply of an app that accepted the connection.
const HANDOVER_WAIT: Duration = Duration::from_secs(5);

/// How long `request` keeps trying a socket file that didn't accept at once.
const LISTENER_WAIT: Duration = Duration::from_millis(500);

/// Connects to the socket, giving up after `CONNECT_WAIT`: a stale file refuses at once, but an
/// app that never accepts must not hold the caller up. The connect runs on a thread because
/// std has no connect timeout for a Unix socket.
fn connect_quick() -> Option<UnixStream> {
    connect_within(CONNECT_WAIT)
}

/// `connect_quick` with its own limit.
fn connect_within(wait: Duration) -> Option<UnixStream> {
    let path = socket_path();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        sender.send(UnixStream::connect(path)).ok();
    });
    receiver.recv_timeout(wait).ok()?.ok()
}

/// `finch [--wait] <path[:line[:column]]>...`: hands the (absolute) paths to a running app
/// with the `open-file` verb, or `open-files` for several, which open as tabs in order so the
/// last one shows. Returns only when connecting or writing fails, so the caller starts as usual.
/// Once delivered, the request belongs to the running app: exits 0 on its reply, or 1 if it
/// refuses or does not answer within `HANDOVER_WAIT`. With `wait` it never returns: see
/// `wait_for_close`. `arg` is the first path as typed, which `wait_for_close` passes on when
/// it has to start the app.
pub fn hand_over(arg: &OsStr, entries: &[(PathBuf, Option<Caret>)], wait: bool) {
    if wait {
        wait_for_close(arg, entries);
    }
    // Piped text is read and typed before the app replies, so a big one outlasts the short wait.
    let piped = entries.iter().any(|(entry, _)| is_untitled(entry));
    let limit = if piped { REPLY_WAIT } else { HANDOVER_WAIT };
    let patient = || socket_path().exists().then(|| connect_within(LISTENER_WAIT)).flatten();
    let Some(mut stream) = connect_quick().or_else(patient) else { return };
    stream.set_read_timeout(Some(limit)).ok();
    stream.set_write_timeout(Some(limit)).ok();
    if writeln!(stream, "{}", open_request(entries, false)).is_err() {
        eprintln!("Finch isn't responding.");
        forget_untitled(entries);
        std::process::exit(1);
    }
    let mut reply = String::new();
    if BufReader::new(stream).read_line(&mut reply).is_ok() {
        if reply.trim_end() == "ok" {
            std::process::exit(0);
        }
        if let Some(why) = reply.trim_end().strip_prefix("err ") {
            eprintln!("{why}");
            // The app refused the piped text, so nothing will read its file.
            forget_untitled(entries);
            std::process::exit(1);
        }
    }
    // No answer in time: the app may still read a piped file, so it is left for it.
    eprintln!("Finch isn't responding.");
    std::process::exit(1);
}

/// This launch's own app has started: once it answers on the socket, hands it the files after the
/// first (which its window already holds), so they open as tabs. A thread, since `app::run` takes
/// the main thread and the socket only answers once it is running.
pub fn open_rest(entries: Vec<(PathBuf, Option<Caret>)>) {
    thread::spawn(move || {
        let deadline = Instant::now() + START_WAIT;
        while Instant::now() < deadline {
            // Only this app's own socket: with another app's behind the path, `start` forwards every entry itself and stops.
            if BOUND.load(Ordering::Relaxed) {
                if let Some(mut stream) = connect_quick() {
                    stream.set_read_timeout(Some(REPLY_WAIT)).ok();
                    if writeln!(stream, "{}", open_request(&entries, false)).is_ok() {
                        BufReader::new(stream).read_line(&mut String::new()).ok();
                    }
                    return;
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
    });
}

/// Set once `start` has bound the socket: this process is the app that answers on it.
static BOUND: AtomicBool = AtomicBool::new(false);

/// What this launch's command line asked to open, kept for `start` to forward when another app
/// turns out to own the socket.
static LAUNCH: std::sync::OnceLock<Vec<(PathBuf, Option<Caret>)>> = std::sync::OnceLock::new();

pub fn note_launch(entries: &[(PathBuf, Option<Caret>)]) {
    LAUNCH.set(entries.to_vec()).ok();
}

/// The request line for `entries`: `open-file <entry>` for one, `open-files <entry>\t<entry>...`
/// for several, each with `-wait` after the verb when `wait`. A path goes as `encode_path` makes
/// it, so a name with a trailing space, a tab, a newline or bytes that aren't UTF-8 survives the
/// one-line protocol.
fn open_request(entries: &[(PathBuf, Option<Caret>)], wait: bool) -> String {
    let verb = if entries.len() == 1 { "open-file" } else { "open-files" };
    let entries: Vec<String> = entries.iter().map(|(path, caret)| format!("{}{}", caret_word(*caret), encode_path(path))).collect();
    format!("{verb}{} {}", if wait { "-wait" } else { "" }, entries.join("\t"))
}

/// `path` as a request carries it: printable ASCII as it is, every other byte (and `%`, and the
/// space, which the line's trimming would eat at the end of a name) as `%XX`.
pub(crate) fn encode_path(path: &Path) -> String {
    use std::os::unix::ffi::OsStrExt as _;
    let mut text = String::new();
    for &byte in path.as_os_str().as_bytes() {
        if (0x21..=0x7e).contains(&byte) && byte != b'%' {
            text.push(char::from(byte));
        } else {
            text.push_str(&format!("%{byte:02X}"));
        }
    }
    text
}

/// The path `encode_path` made `text` from. A `%` not followed by two hex digits stays a `%`, so a
/// path typed by hand into `debug open-file` mostly works; it is decoded too, so a literal `%`
/// there must be typed as `%25`.
pub(crate) fn decode_path(text: &str) -> PathBuf {
    use std::os::unix::ffi::OsStringExt as _;
    let bytes = text.as_bytes();
    let hex = |byte: u8| char::from(byte).to_digit(16);
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let escaped = match bytes[at..] {
            [b'%', high, low, ..] => hex(high).zip(hex(low)).map(|(high, low)| (high * 16 + low) as u8),
            _ => None,
        };
        match escaped {
            Some(byte) => {
                out.push(byte);
                at += 3;
            }
            None => {
                out.push(bytes[at]);
                at += 1;
            }
        }
    }
    PathBuf::from(OsString::from_vec(out))
}

/// `--wait`: sends `open-file-wait` (`open-files-wait` for several) and blocks until the app
/// replies, which it does when the tab (or the window, for a folder) closes, or for several when
/// every one has, then exits 0. With no app running it first starts one (`start_app`) and then
/// hands over to it, so the app outlives the tabs being waited on. A reply of `err` exits 1; the
/// app going away (a quit) ends the wait with 0, since nothing is left open.
fn wait_for_close(arg: &OsStr, entries: &[(PathBuf, Option<Caret>)]) -> ! {
    let fail = |why: &str| -> ! {
        eprintln!("finch: {why}");
        forget_untitled(entries);
        std::process::exit(1);
    };
    let Some(mut stream) = connect_quick().or_else(|| start_app(arg, &entries[0].0)) else {
        fail("--wait found no app and couldn’t start one");
    };
    stream.set_read_timeout(None).ok();
    if writeln!(stream, "{}", open_request(entries, true)).is_err() {
        fail("couldn’t hand the file to the app.");
    }
    let mut reply = String::new();
    match BufReader::new(stream).read_line(&mut reply) {
        Ok(_) if reply.starts_with("err") => fail(reply.trim_end().strip_prefix("err").unwrap_or_default().trim_start()),
        Ok(_) => std::process::exit(0),
        Err(_) => fail("lost touch with the app before the file closed."),
    }
}

/// How long `start_app` waits for a new app to answer on the socket.
const START_WAIT: Duration = Duration::from_secs(30);

/// Starts this program as a detached app (its own process group, no terminal streams) and
/// returns a connection once it answers. A file opens in it as the command line says; a folder
/// starts it with no path, since `wait_for_close` then hands the folder over for a window of
/// its own. `None` when it exits or doesn't answer in `START_WAIT`.
fn start_app(arg: &OsStr, path: &Path) -> Option<UnixStream> {
    let mut command = std::process::Command::new(std::env::current_exe().ok()?);
    // Piped text isn't an argument: the app starts with an empty page and the hand-over fills it.
    if !path.is_dir() && !is_untitled(path) {
        command.arg(arg);
    }
    let mut child = command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .process_group(0)
        .spawn()
        .ok()?;
    let deadline = Instant::now() + START_WAIT;
    while Instant::now() < deadline {
        if let Some(stream) = connect_quick() {
            return Some(stream);
        }
        if child.try_wait().ok()?.is_some() {
            return None;
        }
        thread::sleep(Duration::from_millis(50));
    }
    None
}

/// The `<line>:<column> ` word `caret_prefix` reads, or nothing.
fn caret_word(caret: Option<Caret>) -> String {
    caret.map_or_else(String::new, |caret| format!("{}:{} ", caret.line, caret.column))
}

/// The word that leads an `open-file` entry whose path is a private temp file holding text to open
/// as a new Untitled page (`finch -`). A real path is absolute, so it never starts with it.
const UNTITLED: &str = "--untitled ";
/// How the names of those temp files start; they sit in the socket's own folder.
const STDIN_FILE: &str = "finch-stdin-";
/// The most `finch -` reads from stdin: what a file may be to open without asking.
const STDIN_MAX: u64 = crate::encoding::OPEN_LIMIT;

/// Whether `entry` is the one `stdin_entry` made, not a path.
pub fn is_untitled(entry: &Path) -> bool {
    entry.to_str().is_some_and(|entry| entry.starts_with(UNTITLED))
}

/// `finch -`: reads what was piped in, decodes it and leaves it in a private (0600) temp file
/// the app reads and deletes, so the socket protocol stays one line. Returns the entry that stands
/// for it. With a terminal on stdin, or text that can't be read or isn't text, it says so and
/// exits 1 at once.
pub fn stdin_entry() -> PathBuf {
    use std::io::IsTerminal as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    fn stop(why: &str) -> ! {
        eprintln!("finch: {why}");
        std::process::exit(1);
    }
    if io::stdin().is_terminal() {
        stop("nothing was piped in. Pipe some text into it, like: echo hi | finch -");
    }
    let mut bytes = Vec::new();
    if io::stdin().lock().take(STDIN_MAX + 1).read_to_end(&mut bytes).is_err() {
        stop("couldn’t read what was piped in.");
    }
    if bytes.len() as u64 > STDIN_MAX {
        stop("what was piped in is too large to open (over 256 MB).");
    }
    let Ok(decoded) = crate::encoding::decode(bytes) else { stop("what was piped in isn’t text.") };
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_nanos());
    // Next to the socket: the app takes a file only from there, so it never reads or deletes one elsewhere.
    let dir = socket_path().parent().map(Path::to_path_buf).unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&dir).ok();
    let file = dir.join(format!("{STDIN_FILE}{}-{nanos}", std::process::id()));
    let written = std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(&file).and_then(|mut out| out.write_all(decoded.text.as_bytes()));
    if written.is_err() {
        std::fs::remove_file(&file).ok();
        stop("couldn’t keep what was piped in for the app to read.");
    }
    PathBuf::from(format!("{UNTITLED}{}", file.display()))
}

/// Deletes the temp files of the `stdin_entry` entries, for a command that ends without the app having taken them.
pub fn forget_untitled(entries: &[(PathBuf, Option<Caret>)]) {
    for (entry, _) in entries {
        if let Some(file) = entry.to_str().and_then(|entry| entry.strip_prefix(UNTITLED)) {
            std::fs::remove_file(file).ok();
        }
    }
}

// ---- Text sent from other apps (`src/services.rs`) ----

/// What `finch --append` and `finch --new-note` send: the text piped in, read and decoded as
/// `finch -` does, left in a private temp file next to the socket. Returns the file; with a terminal
/// on stdin or text that can't be read it says so and exits 1.
pub fn piped_file() -> PathBuf {
    let entry = stdin_entry();
    PathBuf::from(entry.to_str().and_then(|entry| entry.strip_prefix(UNTITLED)).unwrap_or_default())
}

/// The text of a file `piped_file` made, or why it can't be taken: only a plain file (one link)
/// in the socket's own folder is read.
pub fn take_piped_file(file: &Path) -> Result<String, String> {
    let ours = file.parent() == socket_path().parent() && std::fs::symlink_metadata(file).is_ok_and(|meta| meta.is_file() && std::os::unix::fs::MetadataExt::nlink(&meta) == 1);
    if !ours {
        return Err("the text file isn't one finch made".into());
    }
    std::fs::read_to_string(file).map_err(|err| crate::file_ops::explain("read", file, &err))
}

/// The reply `request` makes up when the app took the connection but sent nothing back in time.
pub const NO_REPLY: &str = "Finch isn’t responding.";

/// Sends one request line to a running app and returns its reply, or `None` when nothing
/// listens. A socket file that doesn't accept in `CONNECT_WAIT` is tried again for `LISTENER_WAIT`
/// (a busy app still has a listener; the caller must not act as if there were none). An app that
/// doesn't answer in `HANDOVER_WAIT` gives an `err` reply of `NO_REPLY`.
pub fn request(line: &str) -> Option<String> {
    let mut stream = connect_within(CONNECT_WAIT).or_else(|| socket_path().exists().then(|| connect_within(LISTENER_WAIT)).flatten())?;
    stream.set_read_timeout(Some(HANDOVER_WAIT)).ok();
    stream.set_write_timeout(Some(HANDOVER_WAIT)).ok();
    let mut reply = String::new();
    let answered = writeln!(stream, "{line}").is_ok() && BufReader::new(stream).read_line(&mut reply).is_ok_and(|read| read > 0);
    Some(if answered { reply.trim_end().to_string() } else { format!("err {NO_REPLY}") })
}

#[cfg(test)]
mod tests {
    // Named imports: `super::*` would bring GPUI Kit's `test` attribute over the standard one.
    use super::{Caret, caret_prefix, caret_word};

    #[test]
    fn action_error_lists_every_table_name_once() {
        use super::{ACTIONS, action_names};
        let listed = action_names();
        let words: Vec<&str> = listed.split(", ").collect();
        for (name, _) in ACTIONS {
            assert_eq!(words.iter().filter(|word| *word == name).count(), 1, "{name}");
        }
        assert!(listed.ends_with("reopen-<encoding label> or save"));
        assert!(ACTIONS.iter().any(|(name, _)| *name == "line-endings-crlf"));
    }

    #[test]
    fn open_file_argument_splits_its_position() {
        assert_eq!(caret_prefix("/a/b.rs"), (None, "/a/b.rs"));
        assert_eq!(caret_prefix("/a/b c.rs"), (None, "/a/b c.rs"));
        assert_eq!(caret_prefix("/a/b:3"), (None, "/a/b:3"));
        assert_eq!(caret_prefix("42:7 /a/b c.rs"), (Some(Caret { line: 42, column: 7 }), "/a/b c.rs"));
        assert_eq!(caret_prefix(&format!("{}/a/b", caret_word(Some(Caret { line: 3, column: 1 })))), (Some(Caret { line: 3, column: 1 }), "/a/b"));
    }

    #[test]
    fn odd_file_names_survive_the_request_line() {
        use super::{decode_path, encode_path, open_request};
        use std::{ffi::OsString, os::unix::ffi::OsStringExt as _, path::PathBuf};
        for name in [&b"/t/a b "[..], b"/t/tab\there", b"/t/nl\nhere", b"/t/100%25", b"/t/caf\xc3\xa9", b"/t/\xff\xfe.txt"] {
            let path = PathBuf::from(OsString::from_vec(name.to_vec()));
            let line = open_request(&[(path.clone(), None)], false);
            assert!(!line.contains(['\n', '\t']), "{line:?}");
            assert_eq!(line.trim_end(), line);
            assert_eq!(decode_path(&encode_path(&path)), path);
        }
        assert_eq!(decode_path("/t/a b%"), PathBuf::from("/t/a b%"));
        assert_eq!(decode_path("/t/%zz"), PathBuf::from("/t/%zz"));
    }
}
