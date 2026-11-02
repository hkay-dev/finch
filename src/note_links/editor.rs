//! The editor side of `[[`: a completion provider on Markdown documents only. It watches the
//! edits (`is_completion_trigger`, no text read) for `[[` and the letters typed after it, and
//! answers from the note index (`note_links::find`, off the main thread) with one menu row per
//! note: its file name, its first heading under it. Return swaps `[[query` (and the `]]` the
//! editor closed it with) for `[Title](relative/path.md)` as one edit, so one Undo takes it back.
//! With no match, one row `Create “name.md”` links a note that is made, beside the current
//! file, when the row is taken (`completion_accepted`).

use std::{
    cell::Cell,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
};

use gpui_kit::component::input::{
    Command, CompletionContext, CompletionItem, CompletionItemKind, CompletionProvider, CompletionResponse, CompletionTextEdit, EditorState, LspRange, Rope, RopeExt, TextEdit, highlight_contains,
};
use gpui_kit::*;

use super::{Ask, Note, find, is_markdown, link, scan, title_of};
use crate::{file_ops, settings, workspace::Workspace};

/// Rows the menu lists.
const ROWS: usize = 8;
/// The command a `Create` row carries; its title is the path to make.
const CREATE: &str = "finch.create-note";

/// Gives `editor` the `[[` list, or takes it away, to match the document being Markdown.
pub(crate) fn attach(editor: &Entity<EditorState>, markdown: bool, cx: &mut App) {
    editor.update(cx, |state, _| {
        if state.lsp().completion_provider.is_some() != markdown {
            let lsp = state.lsp_mut();
            lsp.completion_provider = markdown.then(|| Rc::new(NoteLinks::default()) as Rc<dyn CompletionProvider>);
            // The menu's query still holds the second `[` it was opened at.
            lsp.set_completion_highlighter(Rc::new(|query, item| highlight_contains(query.trim_start_matches('['), item)));
        }
    });
}

#[derive(Default)]
struct NoteLinks {
    /// Bumped by each request; matching under an older number gives up.
    latest: Arc<AtomicU64>,
    /// Where a lone `[` just typed ends: a second `[` right there makes `[[`.
    lone: Cell<Option<usize>>,
    /// Where the next character of a `[[query` being typed goes.
    active: Cell<Option<usize>>,
    /// Where the query starts, just after the `[[`; Backspace back to it keeps the list.
    open_end: Cell<Option<usize>>,
    /// The `[[` (its offset) already found to be outside any fenced block, so the keys after it
    /// don't read the lines above again.
    outside_fence: Cell<Option<usize>>,
}

impl CompletionProvider for NoteLinks {
    fn is_completion_trigger(&self, offset: usize, new_text: &str, _: &mut App) -> bool {
        let end = offset + new_text.len();
        if new_text.is_empty()
            && let (Some(open), Some(active)) = (self.open_end.get(), self.active.get())
            && open <= offset
            && offset < active
        {
            self.active.set(Some(offset));
            return true;
        }
        if self.active.get() == Some(offset) {
            if new_text.contains(['[', ']', '|', '\n']) {
                self.active.set(None);
                return false;
            }
            self.active.set(Some(end));
            return true;
        }
        self.active.set(None);
        let begins = match new_text {
            "[" if self.lone.get() == Some(offset) => true,
            "[[" => true,
            "[" => {
                self.lone.set(Some(end));
                return false;
            }
            _ => false,
        };
        self.lone.set(None);
        if begins {
            self.active.set(Some(end));
            self.open_end.set(Some(end));
            self.outside_fence.set(None);
        }
        begins
    }

    fn provides_inline_completion(&self) -> bool {
        false
    }

    fn washes_accepted_text(&self) -> bool {
        true
    }

    fn completions(&self, text: &Rope, offset: usize, _: CompletionContext, window: &mut Window, cx: &mut App) -> Task<Result<CompletionResponse>> {
        let nothing = || Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        let number = self.latest.fetch_add(1, Relaxed) + 1;
        let row = text.offset_to_position(offset).line as usize;
        if text.line_len(row) > scan::MAX_LINE {
            return nothing();
        }
        let line_start = text.line_start_offset(row);
        let line = text.slice_line(row).to_string();
        let caret = offset - line_start;
        let Some(open) = scan::open_link(&line, caret) else { return nothing() };
        let start = line_start + open.start;
        if self.outside_fence.get() != Some(start) {
            if scan::in_fence(text.iter_lines().take(row).map(|line| line.chars())) {
                return nothing();
            }
            self.outside_fence.set(Some(start));
        }
        let Some(place) = Place::read(window, cx) else { return nothing() };
        let Some(dir) = place.current.parent().map(Path::to_path_buf) else { return nothing() };
        let end = offset + if line[caret..].starts_with("]]") { 2 } else { 0 };
        let range = LspRange { start: text.offset_to_position(start), end: text.offset_to_position(end) };
        let query = open.query.trim().to_owned();
        let ask = Ask { root: place.root, show_hidden: place.show_hidden, window: place.window, explorer: place.explorer, query: query.clone(), current: place.current };
        let found = find(ask, ROWS, self.latest.clone(), number, cx);
        cx.spawn(async move |_| {
            let Some(notes) = found.await else { return Ok(CompletionResponse::Array(Vec::new())) };
            let mut items: Vec<CompletionItem> = notes.iter().map(|note| note_item(note, &dir, &range)).collect();
            if items.is_empty() && let Some(item) = create_item(&query, &dir, &range) {
                items.push(item);
            }
            Ok(CompletionResponse::Array(items))
        })
    }

    fn completion_accepted(&self, item: &CompletionItem, _: &mut Window, _: &mut App) {
        let Some(command) = item.command.as_ref().filter(|command| command.command == CREATE) else { return };
        let path = Path::new(&command.title);
        let made = path.parent().zip(path.file_name()).map(|(dir, name)| file_ops::create(dir, &name.to_string_lossy(), false));
        if !matches!(made, Some(Ok(_))) {
            crate::preview::render::beep();
        }
    }
}

/// What the window says about where the caret's document is.
struct Place {
    root: PathBuf,
    show_hidden: bool,
    window: WindowId,
    explorer: Entity<crate::explorer::Explorer>,
    /// The Markdown file being edited.
    current: PathBuf,
}

impl Place {
    /// Read through `window`, the way a click on a link finds its file: the window is on the
    /// stack, so it is never read through its handle. `None` for a document with no file or
    /// one that isn't Markdown by name (a rename can leave the provider behind).
    fn read(window: &Window, cx: &App) -> Option<Self> {
        let workspace = window.root::<gpui_kit::base::Root>().flatten()?.read(cx).view().clone().downcast::<Workspace>().ok()?;
        let workspace = workspace.read(cx);
        let current = workspace.active_pane().read(cx).active_document()?.read(cx).path.clone()?;
        if !current.file_name().is_some_and(|name| is_markdown(&name.to_string_lossy())) {
            return None;
        }
        let explorer = workspace.explorer().clone();
        let root = explorer.read(cx).root().to_path_buf();
        Some(Self { root, show_hidden: settings::get(cx).explorer.show_hidden, window: window.window_handle().window_id(), explorer, current })
    }
}

/// A note's row: its name, its first heading under it (else its folder), and the link as the edit.
fn note_item(note: &Note, dir: &Path, range: &LspRange) -> CompletionItem {
    let detail = note.heading.clone().or_else(|| (!note.folder.is_empty()).then(|| note.folder.clone()));
    let title = title_of(note.heading.as_deref(), &note.path);
    CompletionItem {
        label: note.name.clone(),
        kind: Some(CompletionItemKind::FILE),
        detail,
        text_edit: Some(CompletionTextEdit::Edit(TextEdit { range: *range, new_text: link(&title, dir, &note.path) })),
        ..Default::default()
    }
}

/// The `Create “name.md”` row for a query nothing matched; `None` when the query can't name a file.
fn create_item(query: &str, dir: &Path, range: &LspRange) -> Option<CompletionItem> {
    let name = note_name(query)?;
    let path = file_ops::free_name(dir, &name, false);
    let made = path.file_name()?.to_string_lossy().into_owned();
    let title = title_of(None, &path);
    Some(CompletionItem {
        label: format!("Create \u{201C}{made}\u{201D}"),
        kind: Some(CompletionItemKind::FILE),
        text_edit: Some(CompletionTextEdit::Edit(TextEdit { range: *range, new_text: link(&title, dir, &path) })),
        command: Some(Command { title: path.to_string_lossy().into_owned(), command: CREATE.into(), arguments: None }),
        ..Default::default()
    })
}

/// The file name a typed query makes: the query, with `.md` unless it already ends like a
/// Markdown name; `None` when it isn't a name a file can have.
fn note_name(query: &str) -> Option<String> {
    let query = query.trim();
    let name = if is_markdown(query) { query.to_owned() } else { format!("{query}.md") };
    (!query.is_empty() && file_ops::check_name(&name).is_ok()).then_some(name)
}

#[cfg(test)]
mod tests {
    use super::note_name;

    #[test]
    fn a_typed_name_becomes_a_markdown_file() {
        assert_eq!(note_name("ideas"), Some("ideas.md".into()));
        assert_eq!(note_name(" my idea "), Some("my idea.md".into()));
        assert_eq!(note_name("log.markdown"), Some("log.markdown".into()));
        assert_eq!(note_name("v2.0"), Some("v2.0.md".into()));
        assert_eq!(note_name(""), None);
        assert_eq!(note_name("a/b"), None);
        assert_eq!(note_name("a:b"), None);
    }
}
