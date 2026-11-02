//! Finch: one window holding GPUI Kit's code editor, colored by file extension.
//!
//! `finch [path]` opens the file, with the folder tree on the left rooted
//! at its repo. A folder path opens the tree there and an empty buffer; no path
//! roots the tree at the working folder's repo, or at the last root when
//! launched from the Dock or Finder. A file that doesn't exist yet opens empty
//! and Cmd-S creates it. `path:42` or `path:42:7` puts the caret on that line (and column, from
//! 1); a path that exists as typed, colons and all, wins over that reading (`start::split_caret`).
//! `--help`/`-h` and `--version`/`-V` print and exit 0; another `-x` is an error (exit 2) and `--`
//! ends the options (`parse_args`).
//! Several paths open as tabs in order, the last one showing: one `open-files` request to a
//! running app, else this launch's window holds the first and `control::open_rest` hands it the
//! others. `--wait` waits for all of them.
//! `-` is the text piped in (`echo hi | finch -`): read to the end here (256 MiB at most),
//! decoded, left in a private temp file and handed over as `open-file --untitled <file>`, which the
//! app types into a blank or new Untitled page (a draft, so closing asks) and deletes. With a
//! terminal on stdin it says nothing was piped in and exits 1 (`control::stdin_entry`).
//!
//! With an app already running, `finch <path>` hands the path to it over the
//! control socket (`control::hand_over`, the `open-file` verb) and exits 0 when it answers; the
//! app opens a file as a tab and comes forward, or a folder in a new window. With none
//! accepting within 10 ms, or if writing the request fails, it starts as above. Once delivered,
//! it waits up to 5 s for the reply, then exits 1 rather than starting a second app.
//! With `--wait` (or `-w`) the hand-over is the `open-file-wait` verb and the command returns,
//! with 0, only when that tab (or the folder's window) closes; with no app running it starts
//! one detached
//! first (`control::wait_for_close`). A file opened from the Dock, Finder or
//! `open -a`, or by File > Open, is a tab too (`app::open_outside`): in the window whose
//! tree root holds it (the deepest root), else the front window. A folder from outside
//! always gets its own window.
//!
//! `GPUI_EDITOR_BACKGROUND=1` opens the window without taking focus, behind every other window
//! so a Space swipe never makes it the front app, and never
//! writes the settings file unless `GPUI_EDITOR_SETTINGS=<path>` names another one, which it
//! reads and writes.
//! `finch --append [--note <path>]` and `finch --new-note` send the text piped in to a note
//! without opening a window (`services::cli`; Services and Shortcuts, `docs/guide/services.md`).
//! `finch debug <verb>` drives a running app over its control socket
//! (`src/control.rs`).

mod app;
mod camera_text;
mod caret_layer;
mod change_bars;
mod compare;
mod compare_view;
mod config_check;
mod config_path;
mod control;
mod document;
mod draft_list;
mod drafts;
mod drag;
mod empty_state;
mod encoding;
mod export_html;
mod file_ops;
mod explorer;
mod indent_detect;
mod find_bar;
mod find_float;
mod focus;
mod float_field;
mod find_in_folder;
mod folder_search;
mod go_to_line;
mod go_to_symbol;
mod hotkey;
mod haptics;
mod heading_ticks;
mod note_links;
mod open_quickly;
mod outline;
mod hover;
mod language;
mod log_follow;
mod marks;
mod markdown_marks;
mod link_open;
mod menu;
mod motion;
mod native_menu;
mod occurrences;
mod shake;
mod pane;
mod panes;
mod parts_of_speech;
mod patch_view;
mod paste_image;
mod preview;
mod recent;
mod rename_field;
mod rich_copy;
mod scratch;
mod scrollbar;
mod services;
mod settings;
mod settings_controls;
mod settings_panes;
mod settings_watch;
mod settings_window;
mod snippet;
mod smart_punct;
mod spell;
mod space_efficient;
mod style_check;
mod speech;
mod start;
mod status_line;
mod sticky_scope;
mod swatches;
mod symbols;
mod tab_bar;
mod tab_stops;
mod templates;
mod text_drag;
mod text_style;
mod theme;
#[cfg(test)]
mod test_alloc;
mod tokens;
mod tooltip;
mod undo_slider;
mod versions;
mod versions_list;
mod window_look;
mod word_count;
mod workspace;
mod writing_goal;

use std::path::{Path, PathBuf};

fn main() {
    // `finch debug <verb>`: talks to a running app and exits, before any window code.
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|arg| arg == "debug") {
        control::client(&args[1..]);
    }
    let Cli { wait, paths, service, note } = parse_args(&args);
    // Choose or carry over the support folder before any settings, drafts or socket are read.
    settings::carry_over_support();
    if let Some(append) = service {
        services::cli(append, note.as_deref().map(Path::new));
    }
    // The start below replaces this process with itself once (`space_efficient::ensure`), so a
    // read of stdin must come after that: the second run would find it already read.
    let dashes = paths.iter().filter(|path| path.to_str() == Some("-")).count();
    if dashes > 1 {
        eprintln!("finch: - (the text piped in) can be given only once.");
        std::process::exit(2);
    }
    if dashes == 1 {
        space_efficient::ensure();
    }
    // Each path is made absolute (with its `:line[:col]` split off) for the hand-over; a `-` is
    // the text piped in, read now and left in a temp file for the app (`control::stdin_entry`).
    let entries: Vec<(PathBuf, Option<start::Caret>)> = paths
        .iter()
        .map(|path| {
            if path == "-" {
                return (control::stdin_entry(), None);
            }
            let (path, caret) = start::split_caret(path);
            (std::path::absolute(&path).unwrap_or(path), caret)
        })
        .collect();
    let background = std::env::var_os("GPUI_EDITOR_BACKGROUND").is_some();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    if !entries.is_empty() {
        control::hand_over(&paths[0], &entries, wait);
    } else if !background && cwd != Path::new("/") {
        // A bare `finch` in a terminal belongs to the running app too: its folder gets a window
        // there, since a second app would restore the same drafts and overwrite its text.
        let store = settings::Store::load(settings::Store::default_path(), false);
        let root = start::resolve(None, &cwd, &store.settings().explorer).root;
        control::hand_over(root.as_os_str(), &[(root.clone(), None)], wait);
    }
    control::note_launch(&entries);
    // This launch's window holds the first path; piped text instead fills the window's blank page once the app answers.
    let held = usize::from(entries.first().is_some_and(|(path, _)| !control::is_untitled(path)));
    let (arg, caret) = match entries.first().filter(|_| held == 1) {
        Some((path, caret)) => (Some(path.clone()), *caret),
        None => (None, None),
    };
    // This launch opens a window. Freed heap goes back to macOS at once (this may start the
    // binary again), and the window's symbols draw while the rest starts up.
    space_efficient::ensure();
    symbols::prewarm();

    // A background launch never writes the user's settings; one given its own file may.
    let writable = !background || std::env::var_os("GPUI_EDITOR_SETTINGS").is_some();
    let store = settings::Store::load(settings::Store::default_path(), writable);

    let mut start = start::resolve(arg.as_deref(), &cwd, &store.settings().explorer);
    start.caret = caret;
    let text = match app::read_text(start.file.as_deref()) {
        Ok(text) => text,
        // A path typed here is the caller's to hear about; a tab remembered from last time that
        // can't be read (permissions, now binary) must not stop the app from starting.
        Err(err) if arg.is_none() => {
            let path = start.file.take().unwrap_or_default();
            eprintln!("{}", file_ops::explain("open", &path, &err));
            start.caret = None;
            app::read_text(None).unwrap_or_else(|err| {
                eprintln!("finch: can't start: {err}");
                std::process::exit(1);
            })
        }
        Err(err) => {
            let path = start.file.as_deref().unwrap_or(Path::new(""));
            eprintln!("{}", file_ops::explain("open", path, &err));
            control::forget_untitled(&entries);
            std::process::exit(1);
        }
    };
    // Launched from the Dock or Finder: a document may be on its way.
    let defer = entries.is_empty() && cwd == Path::new("/");
    // Further files open as tabs once this app answers on its own socket.
    if entries.len() > held {
        control::open_rest(entries[held..].to_vec());
    }

    app::run(start, text, store, background, defer);
}

const USAGE: &str = "Usage: finch [--wait] [path[:line[:col]] | -]
       finch --append [--note <path>]
       finch --new-note
       finch debug <verb> [args]

  path[:line[:col]]  a file or folder to open, with the caret on a line and column (from 1)
  -                  open the text piped in as a new Untitled page
  -w, --wait         return only when the file's tab (or the folder's window) closes
  --append           add the text piped in to a note under a timestamp line: the --note,
                     else the last note appended to, else a new scrap draft
  --note <path>      the note --append adds to
  --new-note         make a new scrap draft from the text piped in
  debug <verb>       drive a running app over its control socket
  -h, --help         print this and exit
  -V, --version      print the version and exit
  --                 end of options, so a file named -x still opens";

/// What the command line asked for, once its options are out of the way.
struct Cli {
    /// `--wait` / `-w`: block until what was opened closes.
    wait: bool,
    /// Everything that isn't an option, in order.
    paths: Vec<std::ffi::OsString>,
    /// `--append` (`Some(true)`) or `--new-note` (`Some(false)`): send the text piped in to a note.
    service: Option<bool>,
    /// `--note <path>`: the note `--append` adds to.
    note: Option<std::ffi::OsString>,
}

/// Reads the options; `--help` and `--version` print and exit 0, any other `-x` exits 2.
fn parse_args(args: &[std::ffi::OsString]) -> Cli {
    let mut cli = Cli { wait: false, paths: Vec::new(), service: None, note: None };
    let mut options = true;
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        let text = arg.to_string_lossy();
        if !options || !text.starts_with('-') || text == "-" {
            cli.paths.push(arg.clone());
            continue;
        }
        match &*text {
            "--" => options = false,
            "--wait" | "-w" => cli.wait = true,
            "--append" => cli.service = Some(true),
            "--new-note" => cli.service = Some(false),
            "--note" => match args.next() {
                Some(note) => cli.note = Some(note.clone()),
                None => {
                    eprintln!("finch: --note needs a path.");
                    std::process::exit(2);
                }
            },
            "--help" | "-h" => {
                println!("{USAGE}");
                std::process::exit(0);
            }
            "--version" | "-V" => {
                println!("finch {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            _ => {
                eprintln!("finch: unknown option '{text}'\nTry 'finch --help'.");
                std::process::exit(2);
            }
        }
    }
    let bad = match cli.service {
        Some(true) => (!cli.paths.is_empty()).then_some("--append takes the text piped in, not a path."),
        Some(false) => (!cli.paths.is_empty() || cli.note.is_some() || cli.wait).then_some("--new-note takes only the text piped in."),
        None => cli.note.is_some().then_some("--note goes with --append."),
    };
    if let Some(why) = bad {
        eprintln!("finch: {why}\nTry 'finch --help'.");
        std::process::exit(2);
    }
    cli
}
