//! A patch file read like a review (finch#9s91): a `.diff` or `.patch` file, or a patch piped in
//! (`git diff | finch -`), shows its file headers as bars with the file's icon, its added and removed
//! lines washed green and red across the row, and the old and new line numbers side by side in the
//! gutter, and Cmd-click on a line opens the real file at that line. Each file's hunks fold under its
//! header (the Diff highlighter's fold candidates, in `gpui-component`'s `input_adapter.rs`).
//!
//! This is the driver. The editor paints the look from a row map (`EditorState::set_patch_rows`);
//! this module works the map out. `parse` reads the text into one `(kind, old, new)` per line from the
//! `@@ -a,b +c,d @@` headers, on the background executor after a pause (300 ms; the first map at once),
//! keyed by `text_revision()`; a map that lands for an older text is dropped. The editor paints the
//! visible rows only and ignores a map that isn't its text's own. Nothing runs for a file that isn't
//! a patch or is over `language::MAX_HIGHLIGHT_BYTES`, and a scroll or caret move costs one revision
//! compare.

use std::{cell::{Cell, RefCell}, collections::HashMap, path::Path, rc::Rc, sync::Arc, time::Duration};

use gpui_kit::{
    AnyElement, App, Entity, EntityId, Hsla, Pixels, Subscription, Task, WeakEntity,
    base::input::{PATCH_ADDED, PATCH_FILE, PATCH_HEADER, PATCH_HUNK, PATCH_PLAIN, PATCH_REMOVED},
    component::input::EditorState,
};

use crate::{explorer::icons, language, link_open};

/// How long the text stays unchanged before the map is worked out again.
const PAUSE: Duration = Duration::from_millis(300);

/// The icons asked for at once when a map lands, so the common and uncommon file types of a big
/// patch don't all draw at the same time.
const ICONS_WARMED: usize = 48;

/// One file of the patch.
#[derive(Debug, Default, PartialEq)]
struct File {
    /// The name on the `---` line (or the `diff` line), without a tab-separated date.
    old: Option<String>,
    new: Option<String>,
}

impl File {
    /// The spellings the real file may go by, best first: the new name and the old, each as written and
    /// without its first folder (`b/src/x.rs` is `src/x.rs` under `-p1`).
    fn names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for name in self.new.iter().chain(self.old.iter()) {
            for spelling in [Some(name.as_str()), name.split_once('/').map(|(_, rest)| rest)].into_iter().flatten() {
                if !names.iter().any(|known| known == spelling) {
                    names.push(spelling.to_string());
                }
            }
        }
        names
    }

    /// The name the icon is picked by: the new file's, else the old.
    fn icon_name(&self) -> &str {
        let name = self.new.as_deref().or(self.old.as_deref()).unwrap_or("");
        Path::new(name).file_name().and_then(|name| name.to_str()).unwrap_or(name)
    }
}

/// What a patch's text works out to.
#[derive(Debug, Default, PartialEq)]
struct Map {
    /// `(kind, old, new)` per line: `kind` as the editor's `PATCH_*`, `old` and `new` the line numbers
    /// of the two sides (0 for none).
    rows: Vec<(u8, u32, u32)>,
    /// The file each row belongs to (an index into `files`), `u32::MAX` before the first file.
    file_of: Vec<u32>,
    files: Vec<File>,
    /// The width of the widest line number, at least two digits.
    digits: usize,
}

/// A line is the start of a file's header: a `diff ` line, an `Index: ` line, or (outside a header) a
/// `--- ` line followed by a `+++ ` one.
fn starts_file(line: &str, next: Option<&str>, in_header: bool) -> bool {
    line.starts_with("diff ") || (!in_header && (line.starts_with("Index: ") || (line.starts_with("--- ") && next.is_some_and(|next| next.starts_with("+++ ")))))
}

/// `(start, count)` of one side of an `@@` line, `-12,3` or `+12`.
fn side(text: &str) -> (u32, u32) {
    let text = text.get(1..).unwrap_or("");
    match text.split_once(',') {
        Some((start, count)) => (start.parse().unwrap_or(0), count.parse().unwrap_or(1)),
        None => (text.parse().unwrap_or(0), 1),
    }
}

/// The names on a `--- path` or `+++ path` line: the path before a tab, `None` for `/dev/null`.
fn name_on(line: &str) -> Option<String> {
    let name = line[4..].split('\t').next().unwrap_or("").trim();
    (!name.is_empty() && name != "/dev/null").then(|| name.trim_matches('"').to_string())
}

/// The names on a `diff --git a/x b/x` line (a path with a space in it is read at the first ` b/`).
fn names_on_git_line(line: &str) -> Option<File> {
    let rest = line.strip_prefix("diff --git ")?;
    let (old, new) = rest.split_once(" b/").map(|(old, new)| (old, format!("b/{new}")))?;
    Some(File { old: Some(old.to_string()), new: Some(new) })
}

/// Reads a patch: the kind of every line, the old and new line numbers worked out from the hunk
/// headers, and the files. Text outside a header and a hunk (a mail's message, a signature) is plain
/// and numberless.
fn parse(text: &str) -> Map {
    let lines: Vec<&str> = text.lines().collect();
    let mut map = Map { digits: 2, ..Map::default() };
    let (mut in_header, mut old_left, mut new_left, mut old_no, mut new_no) = (false, 0u32, 0u32, 0u32, 0u32);
    for (at, &line) in lines.iter().enumerate() {
        let (mut kind, mut old, mut new) = (PATCH_PLAIN, 0, 0);
        if old_left > 0 || new_left > 0 {
            match line.as_bytes().first() {
                Some(b'+') => {
                    (kind, new) = (PATCH_ADDED, new_no);
                    new_no = new_no.saturating_add(1);
                    new_left = new_left.saturating_sub(1);
                }
                Some(b'-') => {
                    (kind, old) = (PATCH_REMOVED, old_no);
                    old_no = old_no.saturating_add(1);
                    old_left = old_left.saturating_sub(1);
                }
                Some(b'\\') => {}
                _ => {
                    (old, new) = (old_no, new_no);
                    old_no = old_no.saturating_add(1);
                    new_no = new_no.saturating_add(1);
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
            }
        } else if starts_file(line, lines.get(at + 1).copied(), in_header) {
            kind = PATCH_FILE;
            in_header = true;
            let file = if line.starts_with("diff --git ") {
                names_on_git_line(line).unwrap_or_default()
            } else if let Some(path) = line.strip_prefix("Index: ") {
                File { old: Some(path.trim().to_string()), new: Some(path.trim().to_string()) }
            } else if line.starts_with("--- ") {
                File { old: name_on(line), new: None }
            } else {
                File::default()
            };
            map.files.push(file);
        } else if line.starts_with("@@") && line[2..].trim_start().starts_with('-') {
            kind = PATCH_HUNK;
            in_header = false;
            let mut sides = line[2..].split_whitespace();
            let (old_side, new_side) = (sides.next().unwrap_or("-0"), sides.next().unwrap_or("+0"));
            let (old_start, old_count) = side(old_side);
            let (new_start, new_count) = side(new_side);
            (old_no, new_no, old_left, new_left) = (old_start, new_start, old_count, new_count);
        } else if in_header {
            kind = PATCH_HEADER;
            if let Some(file) = map.files.last_mut() {
                if line.starts_with("--- ") {
                    file.old = name_on(line);
                } else if line.starts_with("+++ ") {
                    file.new = name_on(line);
                } else if let Some(name) = line.strip_prefix("rename to ").or_else(|| line.strip_prefix("copy to ")) {
                    file.new.get_or_insert_with(|| name.trim().to_string());
                }
            }
        }
        map.digits = map.digits.max(old.max(new).max(1).ilog10() as usize + 1).min(7);
        map.rows.push((kind, old, new));
        map.file_of.push(if map.files.is_empty() { u32::MAX } else { map.files.len() as u32 - 1 });
    }
    map
}

/// Where the driver keeps each patch editor's state, by editor.
#[derive(Default)]
struct Book(HashMap<EntityId, Rc<Patch>>);

impl gpui_kit::Global for Book {}

/// One editor's patch state.
struct Patch {
    editor: WeakEntity<EditorState>,
    /// The editor's language is Diff now.
    on: Cell<bool>,
    /// The `text_revision()` whose map the editor has, and the one being worked out.
    done: Cell<Option<u64>>,
    pending: Cell<Option<u64>>,
    /// The map the editor shows, for Cmd-click.
    map: RefCell<Option<Arc<Map>>>,
    task: RefCell<Option<Task<()>>>,
    watch: RefCell<Option<Subscription>>,
}

/// An editor was built for a file of language `lang`: it gets the patch look while that is "diff".
pub fn attach(editor: &Entity<EditorState>, lang: &'static str, cx: &mut App) {
    let book = cx.default_global::<Book>();
    book.0.retain(|_, patch| patch.editor.upgrade().is_some());
    let patch = Rc::new(Patch {
        editor: editor.downgrade(),
        on: Cell::new(false),
        done: Cell::new(None),
        pending: Cell::new(None),
        map: RefCell::new(None),
        task: RefCell::new(None),
        watch: RefCell::new(None),
    });
    book.0.insert(editor.entity_id(), patch);
    let id = editor.entity_id();
    cx.observe_release(editor, move |_, cx| {
        cx.default_global::<Book>().0.remove(&id);
    })
    .detach();
    set_lang(editor, lang, cx);
}

/// How many editors the book holds, for `tab-state`.
pub fn book_size(cx: &App) -> usize {
    cx.try_global::<Book>().map_or(0, |book| book.0.len())
}

/// The editor's language changed (a rename, a save under another name): the look follows it.
pub fn set_lang(editor: &Entity<EditorState>, lang: &'static str, cx: &mut App) {
    let Some(patch) = cx.try_global::<Book>().and_then(|book| book.0.get(&editor.entity_id()).cloned()) else { return };
    let diff = lang == "diff";
    if diff == patch.on.get() {
        return;
    }
    patch.on.set(diff);
    if diff {
        let opener = patch.clone();
        editor.update(cx, |state, _| {
            state.set_patch_open(Some(Rc::new(move |row, window, cx| open(&opener, row, window, cx))));
        });
        let watched = patch.clone();
        let subscription = cx.observe(editor, move |editor, cx| sync(&watched, &editor, cx));
        *patch.watch.borrow_mut() = Some(subscription);
        sync(&patch, editor, cx);
    } else {
        patch.watch.borrow_mut().take();
        patch.task.borrow_mut().take();
        patch.done.set(None);
        patch.pending.set(None);
        patch.map.borrow_mut().take();
        editor.update(cx, |state, cx| {
            state.set_patch_open(None);
            state.clear_patch_rows(cx);
        });
    }
}

/// Starts working out the map when the text is one the editor has no map for: now for the first, after
/// a pause for later ones (a newer text drops the task waiting). A file over the highlight limit has
/// none. Called on every notify of the editor, so it does nothing but compare unless the text changed.
fn sync(patch: &Rc<Patch>, editor: &Entity<EditorState>, cx: &mut App) {
    if !patch.on.get() {
        return;
    }
    let (revision, bytes) = {
        let state = editor.read(cx);
        (state.text_revision(), state.text().len())
    };
    if patch.done.get() == Some(revision) || patch.pending.get() == Some(revision) {
        return;
    }
    if bytes > language::MAX_HIGHLIGHT_BYTES {
        patch.task.borrow_mut().take();
        patch.pending.set(None);
        patch.map.borrow_mut().take();
        if patch.done.replace(Some(revision)).is_some() {
            editor.update(cx, |state, cx| state.clear_patch_rows(cx));
        }
        return;
    }
    let first = patch.done.get().is_none() && patch.map.borrow().is_none();
    patch.pending.set(Some(revision));
    let (weak, kept) = (editor.downgrade(), patch.clone());
    let task = cx.spawn(async move |cx| {
        if !first {
            cx.background_executor().timer(PAUSE).await;
        }
        let Ok(text) = weak.read_with(cx, |state, _| (state.text_revision() == revision).then(|| state.text().clone())) else { return };
        let Some(text) = text else { return };
        let map = Arc::new(cx.background_executor().spawn(async move { parse(&text.to_string()) }).await);
        let rows = map.rows.clone();
        let digits = map.digits;
        let shown = map.clone();
        weak.update(cx, |state, cx| {
            if state.text_revision() != revision {
                return;
            }
            // Ask for the icons of the files now, so they are drawn by the time the bars are.
            let mut asked = std::collections::HashSet::new();
            for file in &shown.files {
                if asked.len() >= ICONS_WARMED {
                    break;
                }
                if asked.insert(file.icon_name().rsplit('.').next().unwrap_or("").to_string()) {
                    drop(icons::file(file.icon_name(), 14., Hsla::default()));
                }
            }
            let drawn = shown.clone();
            let icon: Rc<dyn Fn(usize, Pixels, Hsla) -> Option<AnyElement>> = Rc::new(move |row, size, ink| {
                let file = drawn.files.get(*drawn.file_of.get(row)? as usize)?;
                Some(icons::file(file.icon_name(), f32::from(size), ink))
            });
            kept.map.borrow_mut().replace(shown);
            kept.done.set(Some(revision));
            kept.pending.set(None);
            state.set_patch_rows(revision, rows, digits, icon, cx);
        })
        .ok();
    });
    patch.task.borrow_mut().replace(task);
}

/// Cmd-click on buffer line `row`: the real file at the line the row stands for (the new side for an
/// added or a context line, the old for a removed one).
fn open(patch: &Rc<Patch>, row: usize, window: &mut gpui_kit::Window, cx: &mut App) -> bool {
    let Some(map) = patch.map.borrow().clone() else { return false };
    let Some(&(kind, old, new)) = map.rows.get(row) else { return false };
    let line = if kind == PATCH_REMOVED { old } else { new };
    if line == 0 {
        return false;
    }
    let Some(file) = map.file_of.get(row).and_then(|&file| map.files.get(file as usize)) else { return false };
    link_open::open_patch_line(&file.names(), line, window, cx)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
From 0123456789abcdef0123456789abcdef01234567 Mon Sep 17 00:00:00 2001
Subject: x

---
 a.txt | 2 +-
diff --git a/src/a.rs b/src/a.rs
index 1..2 100644
--- a/src/a.rs
+++ b/src/a.rs
@@ -3,3 +3,4 @@ fn x
 keep
-old
+new
+more
 tail
\\ No newline at end of file
diff --git a/b.md b/b.md
new file mode 100644
--- /dev/null
+++ b/b.md
@@ -0,0 +1,2 @@
+# t
+u
";

    #[test]
    fn numbers_come_from_the_hunk_headers() {
        let map = parse(SAMPLE);
        let kinds: Vec<u8> = map.rows.iter().map(|row| row.0).collect();
        assert_eq!(
            kinds,
            [
                PATCH_PLAIN, PATCH_PLAIN, PATCH_PLAIN, PATCH_PLAIN, PATCH_PLAIN, PATCH_FILE, PATCH_HEADER, PATCH_HEADER, PATCH_HEADER, PATCH_HUNK,
                PATCH_PLAIN, PATCH_REMOVED, PATCH_ADDED, PATCH_ADDED, PATCH_PLAIN, PATCH_PLAIN, PATCH_FILE, PATCH_HEADER, PATCH_HEADER, PATCH_HEADER,
                PATCH_HUNK, PATCH_ADDED, PATCH_ADDED
            ]
        );
        assert_eq!(map.rows[10], (PATCH_PLAIN, 3, 3));
        assert_eq!(map.rows[11], (PATCH_REMOVED, 4, 0));
        assert_eq!(map.rows[12], (PATCH_ADDED, 0, 4));
        assert_eq!(map.rows[13], (PATCH_ADDED, 0, 5));
        assert_eq!(map.rows[14], (PATCH_PLAIN, 5, 6));
        assert_eq!(map.rows[15], (PATCH_PLAIN, 0, 0));
        assert_eq!(map.rows[21], (PATCH_ADDED, 0, 1));
        assert_eq!(map.rows[22], (PATCH_ADDED, 0, 2));
        assert_eq!(map.digits, 2);
        assert_eq!(map.files.len(), 2);
        assert_eq!(map.files[0].new.as_deref(), Some("b/src/a.rs"));
        assert_eq!(map.files[1].new.as_deref(), Some("b/b.md"));
        assert_eq!(map.file_of[0], u32::MAX);
        assert_eq!(map.file_of[10], 0);
        assert_eq!(map.file_of[21], 1);
    }

    #[test]
    fn a_plain_unified_diff_starts_at_its_dashes() {
        let map = parse("--- old/x.c\t2020\n+++ new/x.c\t2021\n@@ -1 +1 @@\n--- gone\n+++ here\n");
        let kinds: Vec<u8> = map.rows.iter().map(|row| row.0).collect();
        assert_eq!(kinds, [PATCH_FILE, PATCH_HEADER, PATCH_HUNK, PATCH_REMOVED, PATCH_ADDED]);
        assert_eq!(map.files.len(), 1);
        assert_eq!(map.files[0].names(), ["new/x.c", "x.c", "old/x.c"]);
        assert_eq!(map.files[0].icon_name(), "x.c");
    }

    #[test]
    fn a_deleted_file_names_its_old_side() {
        let map = parse("diff --git a/gone.rs b/gone.rs\n--- a/gone.rs\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n");
        assert_eq!(map.files[0].new, None);
        assert_eq!(map.files[0].names(), ["a/gone.rs", "gone.rs"]);
        assert_eq!(map.rows[4], (PATCH_REMOVED, 1, 0));
    }
}
