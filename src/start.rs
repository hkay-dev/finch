//! Where a window starts: the folder tree's root and the file to open.

use std::{
    ffi::OsStr,
    path::{Path, PathBuf},
};

use crate::settings::ExplorerSettings;

/// A line and column to put the caret on, both counted from 1 (what `file:42:7` names).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Caret {
    pub line: u32,
    pub column: u32,
}

/// The folder tree's root and the file open in the editor, if any.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Start {
    pub root: PathBuf,
    pub file: Option<PathBuf>,
    /// Where the caret goes in `file`, when the command line said (`file:42`).
    pub caret: Option<Caret>,
}

/// Splits a command-line path from a `:line` or `:line:column` suffix (counted from 1). A path that
/// exists as typed wins over the suffix reading, so a file really named `a:3` opens as that file.
pub fn split_caret(arg: &OsStr) -> (PathBuf, Option<Caret>) {
    let literal = PathBuf::from(arg);
    let Some(text) = arg.to_str() else { return (literal, None) };
    if literal.exists() {
        return (literal, None);
    }
    match suffix(text) {
        Some((name, caret)) => (PathBuf::from(name), Some(caret)),
        None => (literal, None),
    }
}

/// `name:line` or `name:line:column` as its parts; `None` when `text` ends in no such numbers or
/// the name is empty.
fn suffix(text: &str) -> Option<(&str, Caret)> {
    let number = |digits: &str| {
        if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
            return None;
        }
        digits.parse::<u32>().ok()
    };
    let (rest, last) = text.rsplit_once(':')?;
    let last = number(last)?;
    let (name, caret) = match rest.rsplit_once(':').and_then(|(name, line)| Some((name, number(line)?))) {
        Some((name, line)) => (name, Caret { line, column: last }),
        None => (rest, Caret { line: last, column: 1 }),
    };
    (!name.is_empty()).then_some((name, Caret { line: caret.line.max(1), column: caret.column.max(1) }))
}

/// Works out the start from the command-line path, the working folder and the
/// saved `[explorer]` settings. A folder roots the tree there; a file roots it
/// at the nearest folder above with a `.git` (else its own folder); no path
/// roots it at the working folder's git root (else the working folder), or,
/// when launched from the Dock or Finder (working folder `/`), at the last root.
pub fn resolve(arg: Option<&Path>, cwd: &Path, last: &ExplorerSettings) -> Start {
    if let Some(arg) = arg {
        let full = canonical(&cwd.join(arg));
        if full.is_dir() {
            return Start { root: full, file: None, caret: None };
        }
        let parent = full.parent().map_or_else(|| cwd.to_path_buf(), Path::to_path_buf);
        let root = git_root(&parent).unwrap_or(parent);
        return Start { root, file: Some(full), caret: None };
    }

    if cwd != Path::new("/") {
        let cwd = canonical(cwd);
        let root = git_root(&cwd).unwrap_or(cwd);
        return Start { root, file: None, caret: None };
    }

    let root = last
        .last_root
        .as_deref()
        .map(canonical)
        .filter(|root| root.is_dir())
        .or_else(|| std::env::home_dir().map(|home| canonical(&home)))
        .unwrap_or_else(|| PathBuf::from("/"));
    let (files, active) = last.saved_tabs();
    let file = files
        .get(active)
        .map(|file| canonical(file))
        .filter(|file| file.is_file() && file.starts_with(&root) && !crate::drafts::is_draft(file));
    Start { root, file, caret: None }
}

/// The nearest folder at or above `dir` holding a `.git` file or folder.
fn git_root(dir: &Path) -> Option<PathBuf> {
    dir.ancestors().find(|folder| folder.join(".git").exists()).map(Path::to_path_buf)
}

/// `path` with symlinks resolved. A path that doesn't exist yet keeps its file
/// name and has its folder resolved; one with no existing folder stays as is.
fn canonical(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    match (path.parent().and_then(|dir| dir.canonicalize().ok()), path.file_name()) {
        (Some(dir), Some(name)) => dir.join(name),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(arg: &str) -> (PathBuf, Option<Caret>) {
        split_caret(OsStr::new(arg))
    }

    fn at(line: u32, column: u32) -> Option<Caret> {
        Some(Caret { line, column })
    }

    #[test]
    fn line_and_column_suffixes() {
        assert_eq!(split("/nope/src/main.rs:42"), (PathBuf::from("/nope/src/main.rs"), at(42, 1)));
        assert_eq!(split("/nope/src/main.rs:42:7"), (PathBuf::from("/nope/src/main.rs"), at(42, 7)));
        assert_eq!(split("/nope/main.rs:0:0"), (PathBuf::from("/nope/main.rs"), at(1, 1)));
    }

    #[test]
    fn text_that_is_not_a_position_stays_in_the_name() {
        for name in ["/nope/main.rs", "/nope/a:b", "/nope/a:", "/nope/a:+3", "/nope/a:3:", ":3", "/nope/a:99999999999"] {
            assert_eq!(split(name), (PathBuf::from(name), None), "{name}");
        }
    }

    #[test]
    fn a_path_that_exists_as_typed_wins() {
        let dir = std::env::temp_dir().join(format!("finch-caret-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let literal = dir.join("a:3");
        std::fs::write(&literal, "x").unwrap();
        let arg = literal.to_str().unwrap();
        assert_eq!(split(arg), (literal.clone(), None));
        std::fs::remove_file(&literal).unwrap();
        assert_eq!(split(arg), (dir.join("a"), at(3, 1)));
        std::fs::remove_dir(&dir).unwrap();
    }
}
