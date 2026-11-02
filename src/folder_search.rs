//! Find in folder, the engine: every match of a query in the text files under a root folder, with
//! no UI. It runs on a background thread (the caller's job), reads each file a line at a time,
//! and hands each hit to `on_hit` in the tree's order.
//!
//! Which files it reads is what the tree shows (`explorer::filter::walker`: `.gitignore` and
//! `.ignore` files, `.git`, `.DS_Store`, dot files unless `show_hidden`). How a line matches is
//! how in-file Find matches: the same `SearchMatcher` reads each line, so Match case, Whole word
//! and Regular expression mean the same thing in both. A pattern that names a line break (`\n`)
//! can't match here, since a hit lives on one line.

use std::{
    cmp::Ordering,
    fs::File,
    io::{BufRead, BufReader, Read as _},
    ops::Range,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering::Relaxed},
};

use gpui_kit::base::input::{Rope, SearchMatcher};

use crate::explorer::{filter::walker, finder_cmp};

/// How many hits a search lists before it stops.
pub const MAX_HITS: usize = 500;

/// The characters of the line a preview keeps before the match, and in all.
const PREVIEW_BEFORE: usize = 32;
const PREVIEW_CHARS: usize = 140;

/// The longest line read; a file with a longer one (minified code, a data blob) is skipped.
const MAX_LINE_BYTES: usize = 1 << 20;

/// What the folder search is asked for: the in-file Find's text and its three options, and
/// whether dot files are read (the explorer's Show hidden files).
#[derive(Clone, Debug, Default)]
pub struct FolderQuery {
    pub text: String,
    pub match_case: bool,
    pub whole_word: bool,
    pub regex: bool,
    pub show_hidden: bool,
}

/// One match: where it is, and the line it is on, cut to a short window around it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hit {
    pub path: PathBuf,
    /// The line, from 0.
    pub line: usize,
    /// The match, in bytes within the line.
    pub byte_range: Range<usize>,
    /// The line trimmed to a short window around the match, tabs and other control characters
    /// turned into spaces.
    pub preview: String,
    /// The match, in bytes within `preview`.
    pub preview_range: Range<usize>,
}

/// How a search ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// Every file was read.
    Done,
    /// There are more than `MAX_HITS` hits; the first `MAX_HITS` were given.
    Capped,
    Cancelled,
    /// The regular expression doesn't compile; the string says what is wrong.
    BadPattern(String),
}

/// Searches the text files under `root` for `query`, giving each hit to `on_hit`, files in the
/// tree's order and hits in line order. Checks `cancel` between lines and while it lists the files.
pub fn search(root: &Path, query: &FolderQuery, cancel: &AtomicBool, on_hit: &mut dyn FnMut(Hit)) -> End {
    let mut matcher = SearchMatcher::new();
    matcher.update_pattern(&query.text, !query.match_case, query.whole_word, query.regex);
    if let Some(problem) = matcher.problem() {
        return End::BadPattern(problem.to_string());
    }
    if query.text.is_empty() {
        return End::Done;
    }
    let Some(files) = list(root, query.show_hidden, cancel) else {
        return End::Cancelled;
    };
    let mut given = 0;
    for path in files {
        let mut found = Vec::new();
        if matches!(read(&path, &mut matcher, cancel, &mut found, MAX_HITS + 1 - given), Read::Cancelled) {
            return End::Cancelled;
        }
        let over = given + found.len() > MAX_HITS;
        found.truncate(MAX_HITS - given);
        given += found.len();
        found.into_iter().for_each(&mut *on_hit);
        if over {
            return End::Capped;
        }
    }
    End::Done
}

/// The files to read, in the tree's order: folders before files, each in Finder's name order.
/// `None` when cancelled.
fn list(root: &Path, show_hidden: bool, cancel: &AtomicBool) -> Option<Vec<PathBuf>> {
    let mut files: Vec<(Vec<String>, PathBuf)> = Vec::new();
    for entry in walker(root, show_hidden).build().flatten() {
        if cancel.load(Relaxed) {
            return None;
        }
        // A link to a file is a file; a link to a folder is not entered, as the tree's walk doesn't.
        let file = entry.file_type().is_some_and(|kind| kind.is_file()) || (entry.path_is_symlink() && entry.path().is_file());
        if !file {
            continue;
        }
        let Ok(below) = entry.path().strip_prefix(root) else {
            continue;
        };
        let names = below.components().map(|part| part.as_os_str().to_string_lossy().into_owned()).collect();
        files.push((names, entry.into_path()));
    }
    files.sort_by(|a, b| tree_order(&a.0, &b.0));
    Some(files.into_iter().map(|(_, path)| path).collect())
}

/// The tree's order of two paths, as their names below the root: at the first folder that
/// differs, a folder comes before a file, and two of a kind go by Finder's name order.
fn tree_order(a: &[String], b: &[String]) -> Ordering {
    for (at, (x, y)) in a.iter().zip(b).enumerate() {
        if x == y {
            continue;
        }
        // The last name of a path is its file; any name before it is a folder.
        let (x_dir, y_dir) = (at + 1 < a.len(), at + 1 < b.len());
        return y_dir.cmp(&x_dir).then_with(|| finder_cmp(x, y));
    }
    a.len().cmp(&b.len())
}

enum Read {
    Read,
    /// Binary, not UTF-8, a line too long, or not readable.
    Skipped,
    Cancelled,
}

/// Reads `path` line by line and pushes its hits to `found`, stopping once it holds `room` of
/// them. A file that turns out unreadable on the way gives none.
fn read(path: &Path, matcher: &mut SearchMatcher, cancel: &AtomicBool, found: &mut Vec<Hit>, room: usize) -> Read {
    let Ok(file) = File::open(path) else {
        return Read::Skipped;
    };
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    match reader.fill_buf() {
        Ok(first) if first.contains(&0) => return Read::Skipped,
        Ok(_) => {}
        Err(_) => return Read::Skipped,
    }
    let mut bytes = Vec::new();
    for line in 0.. {
        if cancel.load(Relaxed) {
            return Read::Cancelled;
        }
        bytes.clear();
        // One byte past the limit tells a line that is too long from one that ends there.
        match (&mut reader).take(MAX_LINE_BYTES as u64 + 1).read_until(b'\n', &mut bytes) {
            Ok(0) => break,
            Ok(_) => {}
            Err(_) => {
                found.clear();
                return Read::Skipped;
            }
        }
        if bytes.len() > MAX_LINE_BYTES {
            found.clear();
            return Read::Skipped;
        }
        if bytes.ends_with(b"\n") {
            bytes.pop();
        }
        if bytes.ends_with(b"\r") {
            bytes.pop();
        }
        let Ok(text) = std::str::from_utf8(&bytes) else {
            found.clear();
            return Read::Skipped;
        };
        let text = if line == 0 { text.strip_prefix('\u{feff}').unwrap_or(text) } else { text };
        matcher.forget_text();
        matcher.update(&Rope::from(text));
        for range in matcher.matched_ranges().iter() {
            let (preview, preview_range) = preview(text, range);
            found.push(Hit { path: path.to_path_buf(), line, byte_range: range.clone(), preview, preview_range });
            if found.len() >= room {
                return Read::Read;
            }
        }
    }
    Read::Read
}

/// `line` cut to a window of `PREVIEW_CHARS` characters that starts `PREVIEW_BEFORE` before the
/// match (at the line's first word when that is nearer), with each control character a space,
/// and the match's bytes in the result. A match that runs past the window is cut at its end.
fn preview(line: &str, found: &Range<usize>) -> (String, Range<usize>) {
    let mut start = line[..found.start].char_indices().rev().nth(PREVIEW_BEFORE - 1).map_or(0, |(at, _)| at);
    if start == 0 {
        start = found.start - line[..found.start].trim_start().len();
    }
    let end = line[start..].char_indices().nth(PREVIEW_CHARS).map_or(line.len(), |(at, _)| start + at);
    let found_end = found.end.min(end);
    let mut out = String::with_capacity(end - start);
    let (mut from, mut to) = (0, 0);
    for (at, c) in line[start..end].char_indices() {
        if start + at == found.start {
            from = out.len();
        }
        if start + at == found_end {
            to = out.len();
        }
        out.push(if c.is_control() { ' ' } else { c });
    }
    if found_end == end {
        to = out.len();
    }
    (out, from..to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("finch-folder-search-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    fn put(root: &Path, name: &str, text: impl AsRef<[u8]>) {
        let path = root.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn query(text: &str) -> FolderQuery {
        FolderQuery { text: text.into(), ..FolderQuery::default() }
    }

    fn run(root: &Path, query: &FolderQuery) -> (Vec<Hit>, End) {
        let mut hits = Vec::new();
        let end = search(root, query, &AtomicBool::new(false), &mut |hit| hits.push(hit));
        (hits, end)
    }

    fn places(root: &Path, hits: &[Hit]) -> Vec<String> {
        hits.iter().map(|hit| format!("{}:{}", hit.path.strip_prefix(root).unwrap().display(), hit.line)).collect()
    }

    #[test]
    fn match_case_decides_whether_case_counts() {
        let root = tree("case");
        put(&root, "a.txt", "Foo\nfoo\nFOO\n");
        let (hits, end) = run(&root, &query("foo"));
        assert_eq!(places(&root, &hits), ["a.txt:0", "a.txt:1", "a.txt:2"]);
        assert_eq!(end, End::Done);
        let (hits, _) = run(&root, &FolderQuery { match_case: true, ..query("foo") });
        assert_eq!(places(&root, &hits), ["a.txt:1"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn whole_word_skips_longer_words() {
        let root = tree("word");
        put(&root, "a.txt", "foobar\nfoo bar\nbar_foo\n(foo)\n");
        let (hits, _) = run(&root, &FolderQuery { whole_word: true, ..query("foo") });
        assert_eq!(places(&root, &hits), ["a.txt:1", "a.txt:3"]);
        assert_eq!(hits[0].byte_range, 0..3);
        assert_eq!(hits[1].byte_range, 1..4);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn regex_reads_the_text_as_a_pattern_and_plain_text_does_not() {
        let root = tree("regex");
        put(&root, "a.txt", "a1b\na.b\nacb\n");
        let (hits, _) = run(&root, &FolderQuery { regex: true, ..query(r"a\db") });
        assert_eq!(places(&root, &hits), ["a.txt:0"]);
        let (hits, _) = run(&root, &query("a.b"));
        assert_eq!(places(&root, &hits), ["a.txt:1"]);
        let (hits, _) = run(&root, &FolderQuery { regex: true, whole_word: true, ..query("int|integer") });
        assert!(hits.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_pattern_that_does_not_compile_ends_the_search_without_hits() {
        let root = tree("bad");
        put(&root, "a.txt", "(\n");
        let (hits, end) = run(&root, &FolderQuery { regex: true, ..query("(") });
        assert!(hits.is_empty());
        assert!(matches!(end, End::BadPattern(problem) if !problem.is_empty()));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn it_stops_after_the_cap_and_says_so_only_when_more_were_left() {
        let root = tree("cap");
        put(&root, "a.txt", "x\n".repeat(MAX_HITS));
        put(&root, "b.txt", "x\n");
        let (hits, end) = run(&root, &query("x"));
        assert_eq!((hits.len(), end), (MAX_HITS, End::Capped));
        assert_eq!(hits.last().unwrap().line, MAX_HITS - 1);
        std::fs::remove_file(root.join("b.txt")).unwrap();
        let (hits, end) = run(&root, &query("x"));
        assert_eq!((hits.len(), end), (MAX_HITS, End::Done));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn hidden_ignored_and_binary_files_are_skipped_like_the_tree() {
        let root = tree("skip");
        put(&root, "seen.txt", "needle\n");
        put(&root, ".hidden/in.txt", "needle\n");
        put(&root, ".dot.txt", "needle\n");
        put(&root, "out/in.txt", "needle\n");
        put(&root, ".gitignore", "out/\n");
        put(&root, ".git/config", "needle\n");
        put(&root, "image.bin", b"needle\0needle");
        put(&root, "latin.txt", b"needle\n\xe9\n");
        let (hits, end) = run(&root, &query("needle"));
        assert_eq!(places(&root, &hits), ["seen.txt:0"]);
        assert_eq!(end, End::Done);
        let (hits, _) = run(&root, &FolderQuery { show_hidden: true, ..query("needle") });
        assert_eq!(places(&root, &hits), [".hidden/in.txt:0", ".dot.txt:0", "seen.txt:0"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_cancelled_search_ends_cancelled() {
        let root = tree("cancel");
        put(&root, "a.txt", "x\nx\nx\n");
        let cancel = AtomicBool::new(true);
        let mut hits = 0;
        let end = search(&root, &query("x"), &cancel, &mut |_| hits += 1);
        assert_eq!((hits, end), (0, End::Cancelled));
        // Cancelled from a hit: the next file is not read.
        put(&root, "b.txt", "x\n");
        let cancel = AtomicBool::new(false);
        let mut hits = 0;
        let end = search(&root, &query("x"), &cancel, &mut |_| {
            hits += 1;
            cancel.store(true, Relaxed);
        });
        assert_eq!((hits, end), (3, End::Cancelled));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn files_come_in_the_trees_order_and_hits_in_line_order() {
        let root = tree("order");
        for name in ["file10.txt", "file2.txt", "Zeta/a.txt", "alpha/b.txt", "alpha/sub/c.txt"] {
            put(&root, name, "x\nx\n");
        }
        let (hits, _) = run(&root, &query("x"));
        let order: Vec<String> = places(&root, &hits).into_iter().filter(|place| place.ends_with(":0")).collect();
        assert_eq!(order, ["alpha/sub/c.txt:0", "alpha/b.txt:0", "Zeta/a.txt:0", "file2.txt:0", "file10.txt:0"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_preview_is_a_short_clean_window_around_the_match() {
        let root = tree("preview");
        let line = format!("\t\tlet a = 1;\t// {} needle {}", "w".repeat(100), "z".repeat(300));
        put(&root, "a.txt", format!("{line}\r\n"));
        let (hits, _) = run(&root, &query("needle"));
        let hit = &hits[0];
        assert_eq!(&line[hit.byte_range.clone()], "needle");
        assert_eq!(&hit.preview[hit.preview_range.clone()], "needle");
        assert!(hit.preview.chars().count() <= PREVIEW_CHARS);
        assert!(!hit.preview.contains(['\t', '\r', '\n']));
        let (short, range) = preview("\t  let x = 1;", &(7..8));
        assert_eq!((short.as_str(), &short[range]), ("let x = 1;", "x"));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
