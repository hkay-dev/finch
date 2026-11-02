//! Cmd-click on a link in the editor (finch#9kz6). The editor finds the link and opens web
//! addresses itself; a path or `file:` URL comes here, resolved against the open file's folder
//! the way the preview resolves its own links. A compiler or stack-trace location
//! (`src/main.rs:42:7`, finch#r40n) is checked and opened here too: it must name an existing
//! file, found from the tree's root first, then the open file's folder (the working directory
//! for a page with no file).

use std::path::{Path, PathBuf};

use gpui_kit::{App, Window};

use crate::preview::render::{LinkTarget, OpenFile, beep, resolve_link};
use crate::start::Caret;
use crate::workspace::Workspace;

/// Hands the editor its way of opening a link that is not a web address. Call once at startup.
pub fn install(cx: &mut App) {
    gpui_kit::component::input::EditorState::set_link_opener(cx, open);
    gpui_kit::component::input::EditorState::set_location_handlers(cx, location_exists, open_location);
}

/// Opens `target`, a link written in the file of the clicked window's active tab: a file opens
/// as a tab, a web address in the system's browser or mail app, a folder or an app in Finder
/// (revealed, never launched), and a link that leads nowhere beeps. A `#heading` alone is
/// ignored: the editor has no headings to scroll to.
fn open(target: &str, window: &mut Window, cx: &mut App) {
    // The clicked window is on the stack: read it through `window`, never through its handle.
    let dir = window
        .root::<gpui_kit::base::Root>()
        .flatten()
        .and_then(|root| root.read(cx).view().clone().downcast::<Workspace>().ok())
        .and_then(|workspace| workspace.read(cx).active_pane().read(cx).active_document().cloned())
        .and_then(|document| document.read(cx).path.as_deref().and_then(|path| path.parent().map(|dir| dir.to_path_buf())));
    match resolve_link(target, dir.as_deref()) {
        LinkTarget::Web(url) => cx.open_url(&url),
        LinkTarget::Markdown(path, anchor) => window.dispatch_action(Box::new(OpenFile { path, anchor, caret: None }), cx),
        LinkTarget::Other(path) if path.is_file() => window.dispatch_action(Box::new(OpenFile { path, anchor: None, caret: None }), cx),
        LinkTarget::Other(path) => cx.reveal_path(&path),
        LinkTarget::Heading(_) => {}
        LinkTarget::Nothing => beep(),
    }
}

/// The folders a location in the clicked window's text is looked for in: the tree's root, and the
/// folder of the active tab's file.
fn bases(window: &Window, cx: &App) -> (Option<PathBuf>, Option<PathBuf>) {
    let workspace = window.root::<gpui_kit::base::Root>().flatten().and_then(|root| root.read(cx).view().clone().downcast::<Workspace>().ok());
    let Some(workspace) = workspace else { return (None, None) };
    let workspace = workspace.read(cx);
    let dir = workspace.active_pane().read(cx).active_document().and_then(|document| document.read(cx).path.as_deref()?.parent().map(Path::to_path_buf));
    (Some(workspace.root().to_path_buf()), dir)
}

/// Whether `location` (`path:line[:col]`) names a file that exists. The editor asks once per token
/// under the pointer.
fn location_exists(location: &str, window: &mut Window, cx: &mut App) -> bool {
    let (root, dir) = bases(window, cx);
    resolve_location(location, root.as_deref(), dir.as_deref()).is_some()
}

/// Opens the file of `location` as a tab with the caret there and its line washed. False when it
/// names no file.
fn open_location(location: &str, window: &mut Window, cx: &mut App) -> bool {
    let (root, dir) = bases(window, cx);
    let Some((path, caret)) = resolve_location(location, root.as_deref(), dir.as_deref()) else { return false };
    window.dispatch_action(Box::new(OpenFile { path, anchor: None, caret: Some(caret) }), cx);
    true
}

/// Opens the real file a patch row (Cmd-click in a `.diff`, finch#9s91) names, with the caret on
/// `line`: the first of `names` (the spellings a patch gives it, `b/src/x.rs` then `src/x.rs`) that
/// is a file under the tree's root or the patch's folder. False when none is.
pub fn open_patch_line(names: &[String], line: u32, window: &mut Window, cx: &mut App) -> bool {
    let (root, dir) = bases(window, cx);
    let Some((path, caret)) = names.iter().find_map(|name| resolve_location(&format!("{name}:{line}"), root.as_deref(), dir.as_deref())) else {
        return false;
    };
    window.dispatch_action(Box::new(OpenFile { path, anchor: None, caret: Some(caret) }), cx);
    true
}

/// `path:line[:col]` as its parts.
fn split_location(location: &str) -> Option<(&str, u32, Option<u32>)> {
    let (rest, last) = location.rsplit_once(':')?;
    let last: u32 = last.parse().ok()?;
    match rest.rsplit_once(':').and_then(|(path, line)| Some((path, line.parse::<u32>().ok()?))) {
        Some((path, line)) => Some((path, line, Some(last))),
        None => Some((rest, last, None)),
    }
}

/// The existing file `location` names and where its caret goes (column 1 when it has none). An
/// absolute path stands as it is; a relative one is joined to `root`, then to `dir` (the working
/// directory when there is none).
fn resolve_location(location: &str, root: Option<&Path>, dir: Option<&Path>) -> Option<(PathBuf, Caret)> {
    let (path, line, column) = split_location(location)?;
    let caret = Caret { line, column: column.unwrap_or(1) };
    let path = Path::new(path);
    if path.is_absolute() {
        return path.is_file().then(|| (path.to_path_buf(), caret));
    }
    let cwd;
    let dir = match dir {
        Some(dir) => Some(dir),
        None => {
            cwd = std::env::current_dir().ok();
            cwd.as_deref()
        }
    };
    [root, dir].into_iter().flatten().map(|base| base.join(path)).find(|found| found.is_file()).map(|found| (found, caret))
}

#[cfg(test)]
mod tests {
    use super::{Caret, resolve_location, split_location};

    #[test]
    fn splits_a_location() {
        assert_eq!(split_location("src/main.rs:42:7"), Some(("src/main.rs", 42, Some(7))));
        assert_eq!(split_location("src/main.rs:42"), Some(("src/main.rs", 42, None)));
        assert_eq!(split_location("src/main.rs"), None);
    }

    #[test]
    fn root_wins_then_the_folder_and_a_missing_file_is_none() {
        let base = std::env::temp_dir().join(format!("finch-r40n-{}", std::process::id()));
        let (root, dir) = (base.join("root"), base.join("root/sub"));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(root.join("src/a.rs"), "").unwrap();
        std::fs::write(dir.join("b.rs"), "").unwrap();
        std::fs::write(dir.join("a.rs"), "").unwrap();
        let at = |location: &str| resolve_location(location, Some(&root), Some(&dir));
        assert_eq!(at("src/a.rs:10:5"), Some((root.join("src/a.rs"), Caret { line: 10, column: 5 })));
        assert_eq!(at("src/a.rs:10"), Some((root.join("src/a.rs"), Caret { line: 10, column: 1 })));
        assert_eq!(at("b.rs:3"), Some((dir.join("b.rs"), Caret { line: 3, column: 1 })));
        assert_eq!(at("nope.rs:3"), None);
        let absolute = format!("{}:2:2", dir.join("a.rs").display());
        assert_eq!(at(&absolute), Some((dir.join("a.rs"), Caret { line: 2, column: 2 })));
        std::fs::remove_dir_all(&base).unwrap();
    }
}
