use std::{
    cell::RefCell,
    ops::Range,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use gpui::{HighlightStyle, SharedString, Task};
use gpui_base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit as BaseInputEdit, InputHighlighter,
    InputHighlighterFactory, SyntaxContext,
};
use ropey::Rope;
use gpui_base::input::RopeExt as _;
use tree_sitter::{InputEdit, ParseOptions, Point};

use crate::input::syntax_context::TreeSitterSyntaxContext;
use super::{LanguageRegistry, SyntaxHighlighter, highlighter::parse_input_bytes};

pub(crate) fn input_highlighter_factory() -> InputHighlighterFactory {
    Rc::new(|language| {
        LanguageRegistry::singleton().has_parser(language).then(|| {
            Box::new(TreeSitterInputHighlighter::new(language)) as Box<dyn InputHighlighter>
        })
    })
}

const SYNC_PARSE_TIMEOUT: Duration = Duration::from_millis(2);
const SYNC_PARSE_MAX_BYTES: usize = 256 * 1024;
const PARSE_DEBOUNCE: Duration = Duration::from_millis(150);

struct TreeSitterInputHighlighter {
    inner: Rc<RefCell<SyntaxHighlighter>>,
    parse_task: Rc<RefCell<Option<Task<()>>>>,
    /// Counts the syntax changes applied, a background parse or the injection
    /// layers, for [`InputHighlighter::syntax_generation`]: the editor reuses
    /// its visible styles until this or the text changes.
    parses: Rc<std::cell::Cell<u64>>,
    /// Whether the tree describes the text last handed to the highlighter:
    /// false from a synchronous parse that ran out of budget (or was skipped
    /// for a large text) until the background parse lands.
    tree_current: Rc<std::cell::Cell<bool>>,
    /// What each node kind means for a bracket, built from the first tree.
    bracket_kinds: RefCell<Vec<Option<SyntaxContext>>>,
    /// After a large-file timeout, retry sync parsing only below 75% of
    /// that size, rather than spending the budget on every keystroke.
    sync_parse_resume_bytes: Option<usize>,
    /// Whether a tree that isn't current has taken every edit since its parse,
    /// so it still answers for the text far from the edits.
    edits_tracked: std::cell::Cell<bool>,
    /// Whether an edit since that tree's parse touched a Markdown fence line
    /// (a fence char in what it replaced or inserted, or a fence line at the
    /// edit): the stale tree then can't say whether the caret is in a fence.
    fences_touched: std::cell::Cell<bool>,
}

impl TreeSitterInputHighlighter {
    fn new(language: &str) -> Self {
        Self {
            inner: Rc::new(RefCell::new(SyntaxHighlighter::new(language))),
            parse_task: Rc::new(RefCell::new(None)),
            parses: Rc::new(std::cell::Cell::new(0)),
            tree_current: Rc::new(std::cell::Cell::new(false)),
            bracket_kinds: RefCell::new(Vec::new()),
            sync_parse_resume_bytes: None,
            edits_tracked: std::cell::Cell::new(false),
            fences_touched: std::cell::Cell::new(false),
        }
    }

    /// A no-op edit while the tree is behind: the pending background parse
    /// still covers this text, and reparsing here would mark the stale tree
    /// current. The comparison runs only while stale.
    fn unchanged_while_stale(&self, text: &Rope) -> bool {
        !self.tree_current.get() && self.inner.borrow().text().eq(text)
    }

    /// Whether the tree can answer syntax questions about the current text:
    /// it is current, or it is the last parse with every edit since applied
    /// through `Tree::edit` (a timed-out parse on a big file leaves it so),
    /// which is right everywhere but near the edits. A change of unknown extent
    /// (an update without an edit) leaves it unusable until a parse lands.
    fn tree_usable(&self) -> bool {
        self.tree_current.get() || self.edits_tracked.get()
    }

    /// Re-parse the injection layers a synchronous update left stale. The
    /// main tree is already current; after a quiet `PARSE_DEBOUNCE` the layers
    /// are computed in the background and applied if the text is unchanged, so
    /// macro bodies and Markdown inline spans keep their old colours until then.
    fn refresh_injections(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<EditorState>) {
        let highlighter = self.inner.clone();
        let parses = self.parses.clone();
        let task = cx.spawn_in(window, async move |entity, cx| {
            cx.background_executor().timer(PARSE_DEBOUNCE).await;
            let Some((data, tree, text)) = highlighter.borrow().pending_injection_job() else {
                return;
            };
            let job_text = text.clone();
            let layers = cx
                .background_executor()
                .spawn(async move { SyntaxHighlighter::compute_injection_layers(data, &tree, &job_text) })
                .await;
            let applied = highlighter.borrow_mut().apply_injection_layers(&text, layers);
            if applied {
                parses.set(parses.get() + 1);
                let _ = entity.update(cx, |_, cx| cx.notify());
            }
        });
        self.parse_task.borrow_mut().replace(task);
    }

    /// Settle after the edits reached the tree: drop a pending background
    /// parse when the synchronous one `completed`, or schedule one for `text`.
    fn finish_update(
        &mut self,
        completed: bool,
        edits_known: bool,
        touched: bool,
        text: &Rope,
        folding: bool,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<EditorState>,
    ) {
        let was_usable = self.tree_usable();
        self.edits_tracked.set(completed || (edits_known && was_usable));
        let fresh = self.tree_current.get();
        self.fences_touched
            .set(!completed && ((!fresh && self.fences_touched.get()) || touched));
        self.tree_current.set(completed);
        if completed {
            self.parse_task.borrow_mut().take();
            if self.inner.borrow().injections_stale() {
                self.refresh_injections(window, cx);
            }
            return;
        }

        let highlighter = self.inner.clone();
        let parse_task = self.parse_task.clone();
        let parses = self.parses.clone();
        let tree_current = self.tree_current.clone();
        let language = highlighter.borrow().language().clone();
        let old_tree = highlighter.borrow().tree().cloned();
        let has_tree = old_tree.is_some();
        let injection_data = highlighter.borrow().injection_parse_data();
        let text = text.clone();
        let text_for_apply = text.clone();
        let cancel = Arc::new(AtomicBool::new(false));

        let task = cx.spawn_in(window, async move |entity, cx| {
            struct CancelOnDrop(Arc<AtomicBool>);
            impl Drop for CancelOnDrop {
                fn drop(&mut self) {
                    self.0.store(true, Ordering::Relaxed);
                }
            }
            let _cancel_guard = CancelOnDrop(cancel.clone());
            // A highlighter with no tree has nothing to show yet, so its
            // first parse starts at once; later edits wait for a quiet moment.
            if has_tree {
                cx.background_executor().timer(PARSE_DEBOUNCE).await;
            }

            let parse_cancel = cancel.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    let (mut parser, grammar) =
                        LanguageRegistry::singleton().parser(&language).ok()?;
                    parser.set_language(&grammar).ok()?;
                    let mut progress = |_: &tree_sitter::ParseState| {
                        if parse_cancel.load(Ordering::Relaxed) {
                            std::ops::ControlFlow::Break(())
                        } else {
                            std::ops::ControlFlow::Continue(())
                        }
                    };
                    let options = ParseOptions::new().progress_callback(&mut progress);
                    let tree = parser.parse_with_options(
                        &mut |offset, _| parse_input_bytes(&text, offset),
                        old_tree.as_ref(),
                        Some(options),
                    )?;
                    if parse_cancel.load(Ordering::Relaxed) {
                        return None;
                    }
                    let injections = injection_data.map_or_else(Default::default, |data| {
                        SyntaxHighlighter::compute_injection_layers(data, &tree, &text)
                    });
                    let folds = if !folding {
                        Vec::new()
                    } else if language.as_ref() == "diff" {
                        diff_fold_ranges(&text)
                    } else {
                        extract_fold_ranges(&tree)
                    };
                    Some((tree, injections, folds))
                })
                .await;

            if let Some((tree, injections, folds)) = result {
                highlighter
                    .borrow_mut()
                    .apply_background_tree(tree, &text_for_apply, injections);
                parses.set(parses.get() + 1);
                tree_current.set(true);
                let _ = entity.update(cx, |state, cx| {
                    state.apply_highlighter_fold_candidates(folds, cx);
                });
            }
        });
        parse_task.borrow_mut().replace(task);
    }
}

impl SyntaxHighlighter {
    pub(crate) fn update_input(
        &mut self,
        edit: Option<BaseInputEdit>,
        text: &Rope,
        timeout: Option<Duration>,
    ) -> bool {
        self.update(edit.map(to_tree_sitter_edit), text, timeout)
    }

    /// Keep first parses and large files that exhausted their budget async.
    fn update_input_edits(
        &mut self,
        edits: &[InputEdit],
        text: &Rope,
        sync_parse_resume_bytes: &mut Option<usize>,
    ) -> bool {
        if sync_parse_resume_bytes.is_some_and(|bytes| text.len() < bytes || text.len() <= SYNC_PARSE_MAX_BYTES) {
            *sync_parse_resume_bytes = None;
        }
        if (text.len() > SYNC_PARSE_MAX_BYTES && self.tree().is_none())
            || sync_parse_resume_bytes.is_some()
        {
            if edits.is_empty() {
                self.edit_tree(None, text);
            } else {
                for edit in edits {
                    self.edit_tree(Some(*edit), text);
                }
            }
            false
        } else {
            let completed =
                self.update_edits_deferring_injections(edits, text, Some(SYNC_PARSE_TIMEOUT));
            if !completed && text.len() > SYNC_PARSE_MAX_BYTES && self.tree().is_some() {
                *sync_parse_resume_bytes = Some(text.len() - text.len() / 4);
            }
            completed
        }
    }
}

impl InputHighlighter for TreeSitterInputHighlighter {
    fn language(&self) -> SharedString {
        self.inner.borrow().language().clone()
    }

    fn update(
        &mut self,
        edit: Option<BaseInputEdit>,
        text: &Rope,
        folding: bool,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<EditorState>,
    ) {
        if self.unchanged_while_stale(text) {
            return;
        }
        let touched = self.inner.borrow().language().as_ref() == "markdown"
            && edit.is_none_or(|edit| edit_touches_fence(self.inner.borrow().text(), text, edit));
        let edit = edit.map(to_tree_sitter_edit);
        let completed = self.inner.borrow_mut().update_input_edits(
            edit.as_slice(),
            text,
            &mut self.sync_parse_resume_bytes,
        );
        self.finish_update(completed, edit.is_some(), touched, text, folding, window, cx);
    }

    fn update_batch(
        &mut self,
        edits: &[BaseInputEdit],
        text: &Rope,
        folding: bool,
        window: &mut gpui::Window,
        cx: &mut gpui::Context<EditorState>,
    ) {
        if edits.is_empty() || self.unchanged_while_stale(text) {
            return;
        }
        let touched = self.inner.borrow().language().as_ref() == "markdown"
            && match edits {
                [edit] => edit_touches_fence(self.inner.borrow().text(), text, *edit),
                _ => true,
            };
        let edits: Vec<InputEdit> = edits.iter().map(|edit| to_tree_sitter_edit(*edit)).collect();
        let completed = self.inner.borrow_mut().update_input_edits(
            &edits,
            text,
            &mut self.sync_parse_resume_bytes,
        );
        self.finish_update(completed, true, touched, text, folding, window, cx);
    }

    fn styles(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        self.inner.borrow().styles(range, resolver)
    }
    fn fold_ranges(&self, text: &Rope) -> Vec<FoldRange> {
        if self.inner.borrow().language().as_ref() == "diff" {
            return diff_fold_ranges(text);
        }
        self.inner
            .borrow()
            .tree()
            .map(extract_fold_ranges)
            .unwrap_or_default()
    }

    fn fold_ranges_for_edit(&self, range: Range<usize>, text: &Rope) -> Vec<FoldRange> {
        if self.inner.borrow().language().as_ref() == "diff" {
            return diff_fold_ranges_for_edit(text, range);
        }
        self.inner
            .borrow()
            .tree()
            .map(|tree| extract_fold_ranges_in_range(tree, range))
            .unwrap_or_default()
    }

    fn syntax_contexts(&self, offsets: &[usize], out: &mut Vec<SyntaxContext>) -> bool {
        let highlighter = self.inner.borrow();
        let Some(tree) = highlighter.tree() else {
            return false;
        };
        let mut kinds = self.bracket_kinds.borrow_mut();
        if kinds.is_empty() {
            *kinds = TreeSitterSyntaxContext::kind_table(&tree.language());
        }
        TreeSitterSyntaxContext::char_contexts_in_tree(tree, &kinds, offsets, out);
        true
    }

    fn bracket_depths(&self, range: &Range<usize>, out: &mut Vec<(usize, u32)>) -> bool {
        // A Markdown document's brackets are prose and fence text, not code.
        let highlighter = self.inner.borrow();
        if highlighter.language().as_ref() == "markdown" {
            return false;
        }
        highlighter.bracket_depths(range, out)
    }

    fn styles_with_variables(
        &self,
        range: &Range<usize>,
        resolver: &dyn HighlightStyleResolver,
        variables: &mut Vec<(Range<usize>, u64)>,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        self.inner
            .borrow()
            .styles_collecting(range, resolver, Some(variables))
    }

    fn syntax_context_at(&self, text: &Rope, offset: usize) -> Option<SyntaxContext> {
        if !self.tree_usable() {
            return None;
        }
        let highlighter = self.inner.borrow();
        let tree = highlighter.tree()?;
        // The editor queries before it hands an edit to the highlighter in a
        // few places; a text of another length is not the one this tree parsed.
        if highlighter.text().len() != text.len() {
            return None;
        }
        Some(TreeSitterSyntaxContext::context_in_tree(tree, text, offset))
    }

    fn syntax_generation(&self) -> Option<u64> {
        self.inner.borrow().tree().map(|_| self.parses.get())
    }

    fn syntax_ancestor(&self, range: Range<usize>) -> Option<Option<Range<usize>>> {
        let highlighter = self.inner.borrow();
        let tree = highlighter.tree()?;
        let root = tree.root_node();
        let mut node = root.descendant_for_byte_range(range.start, range.end);
        // A cursor between two nodes: tree-sitter answers with the one to the
        // right. Take the named node ending at the cursor unless a named node
        // starts there, as Zed does.
        if range.is_empty() && range.start > 0 {
            let right_is_named =
                node.is_some_and(|n| n.start_byte() == range.start && n.is_named());
            let left = root
                .named_descendant_for_byte_range(range.start - 1, range.start)
                .filter(|n| n.end_byte() == range.start);
            if left.is_some() && !right_is_named {
                node = left;
            }
        }
        while let Some(n) = node {
            if n.is_named()
                && n.start_byte() <= range.start
                && range.end <= n.end_byte()
                && n.byte_range() != range
            {
                return Some(Some(n.byte_range()));
            }
            node = n.parent();
        }
        Some(None)
    }

    fn syntax_prose_at(&self, offset: usize) -> bool {
        let highlighter = self.inner.borrow();
        let Some(tree) = highlighter.tree() else {
            return false;
        };
        let mut node = tree
            .root_node()
            .named_descendant_for_byte_range(offset, offset);
        while let Some(n) = node {
            if n.kind() == "inline" {
                return true;
            }
            node = n.parent();
        }
        false
    }

    fn in_fenced_code(&self, text: &Rope, offset: usize) -> Option<bool> {
        // A fence typed or deleted since the last parse is not in the stale
        // tree, so a tree behind the text answers only while no edit since
        // then touched a fence line.
        if !self.tree_current.get() && (!self.edits_tracked.get() || self.fences_touched.get()) {
            return None;
        }
        let highlighter = self.inner.borrow();
        let tree = highlighter.tree()?;
        if highlighter.text().len() != text.len() {
            return None;
        }
        // The byte before `offset`, so a caret at the end of an unclosed block (where the
        // root answers for a collapsed range) still lands in it.
        let mut node = tree
            .root_node()
            .named_descendant_for_byte_range(offset.checked_sub(1)?, offset);
        while let Some(n) = node {
            if n.kind() == "fenced_code_block" {
                return Some(true);
            }
            node = n.parent();
        }
        Some(false)
    }

    fn inspect_current_tree(&self, text: &Rope, f: &mut dyn FnMut(&dyn std::any::Any)) -> bool {
        if !self.tree_current.get() {
            return false;
        }
        let highlighter = self.inner.borrow();
        let Some(tree) = highlighter.tree() else {
            return false;
        };
        if highlighter.text().len() != text.len() {
            return false;
        }
        f(tree);
        true
    }
}

/// Whether `edit`, which turned `old` into `new`, may have added, removed or
/// changed a Markdown code fence: a fence character in the replaced or the
/// inserted span, or a fence line where the edit starts or ends.
fn edit_touches_fence(old: &Rope, new: &Rope, edit: BaseInputEdit) -> bool {
    let is_fence_char = |c: char| c == '`' || c == '~';
    let old_end = edit.old_end_byte.min(old.len());
    let new_end = edit.new_end_byte.min(new.len());
    let starts_fence = |text: &Rope, offset: usize| {
        let line = text.slice_line(text.offset_to_point(offset.min(text.len())).row);
        line.chars().take_while(|c| *c == ' ').count() <= 3
            && line.chars().skip_while(|c| *c == ' ').next().is_some_and(is_fence_char)
    };
    old.slice(edit.start_byte.min(old_end)..old_end).chars().any(is_fence_char)
        || new.slice(edit.start_byte.min(new_end)..new_end).chars().any(is_fence_char)
        || starts_fence(old, edit.start_byte)
        || starts_fence(old, old_end)
        || starts_fence(new, new_end)
        || starts_fence(new, edit.start_byte)
}

fn to_tree_sitter_edit(edit: BaseInputEdit) -> InputEdit {
    InputEdit {
        start_byte: edit.start_byte,
        old_end_byte: edit.old_end_byte,
        new_end_byte: edit.new_end_byte,
        start_position: Point::new(edit.start_position.row, edit.start_position.column),
        old_end_position: Point::new(edit.old_end_position.row, edit.old_end_position.column),
        new_end_position: Point::new(edit.new_end_position.row, edit.new_end_position.column),
    }
}

fn extract_fold_ranges(tree: &tree_sitter::Tree) -> Vec<FoldRange> {
    extract_fold_ranges_in_range(tree, 0..usize::MAX)
}

/// A patch's fold candidates (finch#9s91): each file's section, from its header line to the next
/// file's header (a fold hides the lines between and keeps the last one), so a file's hunks fold under its header. A
/// file starts at a `diff ` line, an `Index: ` line, or a `--- ` line followed by `+++ ` outside
/// a header; the hunk lengths of `@@ -a,b +c,d @@` keep a body line that reads like a header
/// (`--- x` removing `-- x`) from starting one.
fn diff_fold_ranges(text: &Rope) -> Vec<FoldRange> {
    let lines = text.lines_len();
    let last = if lines > 1 && text.slice_line(lines - 1).len() == 0 { lines - 2 } else { lines.saturating_sub(1) };
    diff_fold_window(text, 0, last, true)
}

/// The candidates of the file sections an edit at `range` touched: from the nearest file header at
/// or above its first row to the next header below its last row, so a keystroke reads one section,
/// not the whole patch.
fn diff_fold_ranges_for_edit(text: &Rope, range: Range<usize>) -> Vec<FoldRange> {
    fn is_header(text: &Rope, row: usize) -> bool {
        let line = text.slice_line(row);
        ["diff ", "Index: "].iter().any(|prefix| line.len() >= prefix.len() && line.is_char_boundary(prefix.len()) && line.slice(..prefix.len()) == *prefix)
    }
    let lines = text.lines_len();
    let last = if lines > 1 && text.slice_line(lines - 1).len() == 0 { lines - 2 } else { lines.saturating_sub(1) };
    let len = text.len();
    let first_row = text.offset_to_point(range.start.min(len)).row.min(last);
    let last_row = text.offset_to_point(range.end.min(len)).row.min(last);
    let from = (0..=first_row).rev().find(|&row| is_header(text, row)).unwrap_or(0);
    match (last_row + 1..=last).find(|&row| is_header(text, row)) {
        Some(to) => diff_fold_window(text, from, to, false),
        None => diff_fold_window(text, from, last, true),
    }
}

/// The candidates of rows `from..=to` of a patch. `from` is a file header (or row 0). When
/// `to_end` the window runs to the end of the text; otherwise `to` is the next file's header, which
/// closes the section before it and starts none of its own here.
fn diff_fold_window(text: &Rope, from: usize, to: usize, to_end: bool) -> Vec<FoldRange> {
    fn has(line: ropey::RopeSlice, prefix: &str) -> bool {
        line.len() >= prefix.len() && line.is_char_boundary(prefix.len()) && line.slice(..prefix.len()) == prefix
    }
    /// `(old, new)` line counts of a hunk line (1 when a side's count is left out).
    fn counts(line: &str) -> (usize, usize) {
        let mut sides = line.split_whitespace().skip(1).take(2).map(|side| {
            let count = side.split_once(',').map_or("1", |(_, count)| count);
            count.parse().unwrap_or(1)
        });
        (sides.next().unwrap_or(1), sides.next().unwrap_or(1))
    }
    let lines = text.lines_len();
    let last = to;
    let mut ranges = Vec::new();
    let mut start: Option<usize> = None;
    let (mut in_header, mut old_left, mut new_left) = (false, 0usize, 0usize);
    for row in from..=last {
        let line = text.slice_line(row);
        if old_left > 0 || new_left > 0 {
            match line.chars().next() {
                Some('+') => new_left = new_left.saturating_sub(1),
                Some('-') => old_left = old_left.saturating_sub(1),
                Some('\\') => {}
                _ => {
                    old_left = old_left.saturating_sub(1);
                    new_left = new_left.saturating_sub(1);
                }
            }
            continue;
        }
        let begins = has(line, "diff ")
            || (!in_header && has(line, "Index: "))
            || (!in_header && has(line, "--- ") && row < last && has(text.slice_line(row + 1), "+++ "));
        if begins {
            // A fold keeps its last line shown, so a file's fold ends on the next file's header
            // and hides everything between the two headers.
            if let Some(from) = start.replace(row).filter(|from| row > from + 1) {
                ranges.push(FoldRange::new(from, row));
            }
            in_header = true;
            if !to_end && row == to {
                return ranges;
            }
        } else if has(line, "@@") {
            in_header = false;
            (old_left, new_left) = counts(&line.to_string());
        }
    }
    // The last file ends on the empty line after the final newline, when there is one.
    let end = if last + 1 < lines { last + 1 } else { last };
    if let Some(from) = start.filter(|from| end > from + 1) {
        ranges.push(FoldRange::new(from, end));
    }
    ranges
}

fn extract_fold_ranges_in_range(
    tree: &tree_sitter::Tree,
    byte_range: Range<usize>,
) -> Vec<FoldRange> {
    fn collect(node: tree_sitter::Node, bytes: &Range<usize>, ranges: &mut Vec<FoldRange>) {
        if node.end_byte() <= bytes.start || node.start_byte() >= bytes.end {
            return;
        }
        let start = node.start_position().row;
        let end = node.end_position().row;
        if end.saturating_sub(start) < 2 {
            return;
        }
        ranges.push(FoldRange::new(start, end));
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            collect(child, bytes, ranges);
        }
    }

    let root = tree.root_node();
    let mut ranges = Vec::new();
    let mut cursor = root.walk();
    for child in root.named_children(&mut cursor) {
        collect(child, &byte_range, &mut ranges);
    }
    ranges.sort_by_key(|range| range.start_line);
    ranges.dedup_by_key(|range| range.start_line);
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Bracket depth comes from the tree: right from the middle of the file,
    /// and brackets in strings and comments are left out.
    #[cfg(feature = "tree-sitter-rust")]
    #[test]
    fn bracket_depth_counts_pairs_from_the_tree() {
        let text = Rope::from("fn a(x: i32) { let v = [(1, 2)]; } // (\nlet s = \"(\";\n");
        let source = text.to_string();
        let highlighter = TreeSitterInputHighlighter::new("rust");
        highlighter.inner.borrow_mut().update_input(None, &text, None);
        let depths = |range: Range<usize>| {
            let mut out = Vec::new();
            assert!(highlighter.bracket_depths(&range, &mut out));
            out.into_iter()
                .map(|(offset, depth)| (&source[offset..offset + 1], depth))
                .collect::<Vec<_>>()
        };
        let expected = [("(", 0), (")", 0), ("{", 0), ("[", 1), ("(", 2), (")", 2), ("]", 1), ("}", 0)];
        assert_eq!(depths(0..source.len()), expected);
        let from = source.find("1, 2").unwrap();
        assert_eq!(depths(from..source.len()), expected[5..]);
    }

    /// Starting the walk anywhere gives the same depths as the whole file does
    /// there: attributes, index expressions and tuple patterns open brackets
    /// that are not their node's first child.
    #[cfg(feature = "tree-sitter-rust")]
    #[test]
    fn bracket_depth_is_the_same_wherever_the_range_starts() {
        let source = "#[derive(Debug)]\nstruct P(i32, i32);\nfn f(v: &[i32], p: P) -> i32 {\n    let P(a, b) = p;\n    let w = v[a as usize];\n    assert!(w.max(b) > (1 + 2));\n    [w, (b)][0]\n}\n";
        let text = Rope::from(source);
        let highlighter = TreeSitterInputHighlighter::new("rust");
        highlighter.inner.borrow_mut().update_input(None, &text, None);
        let mut whole = Vec::new();
        assert!(highlighter.bracket_depths(&(0..source.len()), &mut whole));
        assert_eq!(whole.len(), source.chars().filter(|c| "()[]{}".contains(*c)).count());
        for from in (0..source.len()).filter(|&i| source.is_char_boundary(i)) {
            let mut part = Vec::new();
            highlighter.bracket_depths(&(from..source.len()), &mut part);
            let want: Vec<_> = whole.iter().copied().filter(|(o, _)| *o >= from).collect();
            assert_eq!(part, want, "range from {from}");
        }
    }

    /// One `impl` with 5,000 methods: a range inside it looks at tens of nodes,
    /// not every method, and the depths are the ones stepping every sibling gives.
    #[cfg(feature = "tree-sitter-rust")]
    #[test]
    fn bracket_depths_skip_the_siblings_before_the_range() {
        use super::super::highlighter::BRACKET_NODES_VISITED;
        let mut source = String::from("impl T {\n");
        for i in 0..5000 {
            source.push_str(&format!("    fn m{i}(&self, a: [u8; 2]) -> u8 {{ a[0] }}\n"));
        }
        source.push_str("}\n");
        let text = Rope::from(source.as_str());
        let highlighter = TreeSitterInputHighlighter::new("rust");
        highlighter.inner.borrow_mut().update_input(None, &text, None);
        let inner = highlighter.inner.borrow();
        let visited = |f: &dyn Fn(&mut Vec<(usize, u32)>)| {
            BRACKET_NODES_VISITED.with(|n| n.set(0));
            let mut out = Vec::new();
            f(&mut out);
            (BRACKET_NODES_VISITED.with(|n| n.get()), out)
        };
        for at in [source.find("fn m2500(").unwrap(), source.find("fn m4990(").unwrap(), 10] {
            let range = at..at + 60;
            let (now, got) = visited(&|out| {
                inner.bracket_depths(&range, out);
            });
            let (before, want) = visited(&|out| {
                inner.bracket_depths_every_sibling(&range, out);
            });
            assert_eq!(got, want, "range at {at}");
            assert!(!got.is_empty());
            assert!(now < 100, "{now} nodes at {at}");
            if at > 1000 {
                assert!(before > 2000, "{before} nodes before at {at}");
            }
        }
    }

    /// Over many ranges of a real Rust file the depths equal the ones stepping
    /// every sibling gives.
    #[cfg(feature = "tree-sitter-rust")]
    #[test]
    fn bracket_depths_equal_stepping_every_sibling_on_a_real_file() {
        let source = include_str!("registry.rs");
        let text = Rope::from(source);
        let highlighter = TreeSitterInputHighlighter::new("rust");
        highlighter.inner.borrow_mut().update_input(None, &text, None);
        let inner = highlighter.inner.borrow();
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut next = |bound: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % bound as u64) as usize
        };
        let boundary = |mut i: usize| {
            while !source.is_char_boundary(i) {
                i += 1;
            }
            i
        };
        for n in 0..3000 {
            let start = boundary(next(source.len()));
            let len = [0, 1, 40, 400, 4000, source.len()][n % 6];
            let range = start..boundary((start + len).min(source.len()));
            let (mut got, mut want) = (Vec::new(), Vec::new());
            assert!(inner.bracket_depths(&range, &mut got));
            assert!(inner.bracket_depths_every_sibling(&range, &mut want));
            assert_eq!(got, want, "range {range:?}");
        }
    }

    /// The jump and stepping every sibling give the same depths for every start
    /// of `source` and several range lengths.
    #[allow(dead_code)]
    fn assert_jump_equals_walk(language: &str, source: &str) {
        let text = Rope::from(source);
        let highlighter = TreeSitterInputHighlighter::new(language);
        highlighter.inner.borrow_mut().update_input(None, &text, None);
        let inner = highlighter.inner.borrow();
        let mut brackets = 0;
        for start in (0..=source.len()).filter(|&i| source.is_char_boundary(i)) {
            for len in [0, 1, 30, 300, source.len()] {
                let mut end = (start + len).min(source.len());
                while !source.is_char_boundary(end) {
                    end += 1;
                }
                let range = start..end;
                let (mut got, mut want) = (Vec::new(), Vec::new());
                assert!(inner.bracket_depths(&range, &mut got));
                assert!(inner.bracket_depths_every_sibling(&range, &mut want));
                assert_eq!(got, want, "{language} range {range:?}");
                brackets += want.len();
            }
        }
        assert!(brackets > 0, "{language}: no brackets found");
    }

    #[allow(dead_code)]
    fn body(line: &str) -> String {
        (0..40).map(|_| format!("    {line}\n")).collect()
    }

    /// `for (...) { ... }` has its `)` as a direct child of the loop, not of a
    /// parenthesized node.
    #[cfg(feature = "tree-sitter-c")]
    #[test]
    fn jump_equals_walk_c_for_loop() {
        let src = format!(
            "void g(int n, int *a) {{\n  for (int i = 0; i < n; i++) {{\n{}  }}\n}}\n",
            body("f(a[i]);")
        );
        assert_jump_equals_walk("c", &src);
    }

    #[cfg(feature = "tree-sitter-cpp")]
    #[test]
    fn jump_equals_walk_cpp_for_loop() {
        let src = format!(
            "void g(int n, int *a) {{\n  for (int i = 0; i < n; i++) {{\n{}  }}\n}}\n",
            body("f(a[i]);")
        );
        assert_jump_equals_walk("cpp", &src);
    }

    #[cfg(feature = "tree-sitter-java")]
    #[test]
    fn jump_equals_walk_java_for_loop() {
        let src = format!(
            "class A {{ int[]\n[] b; void g(int n, int[] a) {{\n  for (int i = 0; i < n; i++) {{\n{}  }}\n}} }}\n",
            body("f(a[i]);")
        );
        assert_jump_equals_walk("java", &src);
    }

    #[cfg(feature = "tree-sitter-javascript")]
    #[test]
    fn jump_equals_walk_javascript_loops() {
        let src = format!(
            "function g(n, a) {{\n  for (let i = 0; i < n; i++) {{\n{}  }}\n  for (const x of xs) {{\n{}  }}\n}}\n",
            body("f(a[i]);"),
            body("f(x, [x]);")
        );
        assert_jump_equals_walk("javascript", &src);
    }

    #[cfg(feature = "tree-sitter-swift")]
    #[test]
    fn jump_equals_walk_swift_funcs() {
        let src = format!(
            "func f(a: Int) {{\n{}}}\npublic func g<T>(\n  a: [T],\n  b: (Int, Int)\n) {{\n{}}}\n",
            body("print(a)"),
            body("print(a[0])")
        );
        assert_jump_equals_walk("swift", &src);
    }

    #[cfg(feature = "tree-sitter-go")]
    #[test]
    fn jump_equals_walk_go_switches() {
        let src = format!(
            "package p\nfunc g(x any) {{\n  switch v := x.(type) {{\n  case int:\n{}  }}\n  switch x := f(); x {{\n  case 1:\n{}  }}\n}}\n",
            body("f(v)"),
            body("f([]int{x})")
        );
        assert_jump_equals_walk("go", &src);
    }

    #[cfg(feature = "tree-sitter-zig")]
    #[test]
    fn jump_equals_walk_zig_switch() {
        let mut cases = String::new();
        for _ in 0..40 {
            cases.push_str("        .a => f(y),\n");
        }
        let src = format!("fn g(x: E) void {{\n    switch (x) {{\n{cases}    }}\n}}\n");
        assert_jump_equals_walk("zig", &src);
    }

    /// A Markdown document's brackets are prose, not code: no rainbow.
    #[cfg(feature = "tree-sitter-markdown")]
    #[test]
    fn markdown_has_no_bracket_depths() {
        let text = Rope::from("some (prose) [here]\n\n```\nfn a() {}\n```\n");
        let highlighter = TreeSitterInputHighlighter::new("markdown");
        highlighter.inner.borrow_mut().update_input(None, &text, None);
        let mut out = Vec::new();
        assert!(!highlighter.bracket_depths(&(0..text.len()), &mut out));
        assert!(out.is_empty());
    }

    struct NoStyles;
    impl HighlightStyleResolver for NoStyles {
        fn style(&self, _: &str) -> Option<HighlightStyle> {
            None
        }
    }

    /// One name hashes to one value, another name to another; a function name
    /// is not a variable, also inside a macro body.
    #[cfg(feature = "tree-sitter-rust")]
    #[test]
    fn variable_hashes_are_stable_per_name() {
        let text = Rope::from(
            "fn f(a: i32, b: i32) -> i32 {\n    let c = a + b;\n    assert!(compute(a) > 0);\n    a + c\n}\n",
        );
        let source = text.to_string();
        let highlighter = TreeSitterInputHighlighter::new("rust");
        highlighter.inner.borrow_mut().update_input(None, &text, None);
        let mut out = Vec::new();
        highlighter.styles_with_variables(&(0..source.len()), &NoStyles, &mut out);
        let hashes = |name: &str| -> Vec<u64> {
            out.iter()
                .filter(|(r, _)| &source[r.clone()] == name)
                .map(|(_, h)| *h)
                .collect()
        };
        let a = hashes("a");
        assert!(a.len() >= 3 && a.iter().all(|h| *h == a[0]));
        let c = hashes("c");
        assert!(c.len() >= 2 && c.iter().all(|h| *h == c[0]));
        assert_ne!(a[0], hashes("b")[0]);
        // FNV-1a of "a".
        assert_eq!(a[0], 0xaf63dc4c8601ec8c);
        assert!(hashes("compute").is_empty(), "a function name is no variable");
    }

    fn large_code_text(bytes: usize) -> Rope {
        // Concatenate real Rust source, not a repeated trivial function: the
        // first parse and error recovery must see a representative grammar.
        let source = include_str!("highlighter.rs");
        Rope::from_str(&source.repeat(bytes.div_ceil(source.len())))
    }

    /// Indent and auto-close read the highlighter's tree: every offset answers
    /// as the language provider's own whole parse does, also after an edit.
    #[cfg(feature = "tree-sitter-rust")]
    #[test]
    fn highlighter_answers_insert_context_like_the_provider() {
        use crate::input::syntax_context::syntax_context_provider;

        let provider = syntax_context_provider("rust").unwrap();
        let highlighter = TreeSitterInputHighlighter::new("rust");
        let old = Rope::from("fn a() { let s = \"x(y\"; } // note\n/* b { */\n");
        highlighter.inner.borrow_mut().update_input(None, &old, None);
        highlighter.tree_current.set(true);
        let check = |highlighter: &TreeSitterInputHighlighter, text: &Rope| {
            for offset in 0..=text.len() {
                if !text.is_char_boundary(offset) {
                    continue;
                }
                assert_eq!(
                    highlighter.syntax_context_at(text, offset),
                    Some(provider.context_at(text, offset)),
                    "offset {offset} of {text:?}"
                );
            }
        };
        check(&highlighter, &old);

        // Typing a quote before the string, as an incremental edit.
        let at = "fn a() { let s = ".len();
        let mut new = old.clone();
        new.insert(at, "'");
        let edit = InputEdit {
            start_byte: at,
            old_end_byte: at,
            new_end_byte: at + 1,
            start_position: Point::new(0, at),
            old_end_position: Point::new(0, at),
            new_end_position: Point::new(0, at + 1),
        };
        assert!(highlighter.inner.borrow_mut().update(Some(edit), &new, None));
        check(&highlighter, &new);
    }

    /// A tree that may not describe the text answers nothing, so the caller
    /// asks the provider.
    #[cfg(feature = "tree-sitter-rust")]
    #[test]
    fn highlighter_declines_when_tree_is_stale_or_text_differs() {
        let highlighter = TreeSitterInputHighlighter::new("rust");
        let text = Rope::from("fn a() {}\n");
        assert_eq!(highlighter.syntax_context_at(&text, 3), None, "no tree");
        highlighter.inner.borrow_mut().update_input(None, &text, None);
        assert_eq!(highlighter.syntax_context_at(&text, 3), None, "not current");
        highlighter.tree_current.set(true);
        assert!(highlighter.syntax_context_at(&text, 3).is_some());
        assert_eq!(
            highlighter.syntax_context_at(&Rope::from("fn a() {}\n\n"), 3),
            None,
            "other text"
        );
    }

    /// A tree behind the text that took every edit through `Tree::edit` (a
    /// timed-out parse on a big file) still answers far from the edits, so a
    /// bracket or Return key never parses the text again; a change of unknown
    /// extent leaves it unusable.
    #[cfg(feature = "tree-sitter-rust")]
    #[test]
    fn stale_tree_that_took_the_edits_answers_context() {
        let highlighter = TreeSitterInputHighlighter::new("rust");
        let old = Rope::from("fn a() { let s = \"x(y\"; } // note\n/* b { */\n");
        highlighter.inner.borrow_mut().update_input(None, &old, None);
        assert!(!highlighter.tree_current.get());

        // Typing a space before the string, applied to the tree but not reparsed.
        let at = "fn a() { let s = ".len();
        let mut new = old.clone();
        new.insert(at, " ");
        let edit = InputEdit {
            start_byte: at,
            old_end_byte: at,
            new_end_byte: at + 1,
            start_position: Point::new(0, at),
            old_end_position: Point::new(0, at),
            new_end_position: Point::new(0, at + 1),
        };
        highlighter.inner.borrow_mut().edit_tree(Some(edit), &new);
        let in_string = new.to_string().find("x(y").unwrap() + 1;
        let in_comment = new.to_string().find("note").unwrap() + 1;
        let in_block_comment = new.to_string().find("b {").unwrap() + 1;
        assert_eq!(
            highlighter.syntax_context_at(&new, in_string),
            None,
            "edits not tracked"
        );

        highlighter.edits_tracked.set(true);
        assert!(!highlighter.tree_current.get());
        assert_eq!(
            highlighter.syntax_context_at(&new, in_string),
            Some(SyntaxContext::String)
        );
        assert_eq!(
            highlighter.syntax_context_at(&new, in_comment),
            Some(SyntaxContext::Comment)
        );
        assert_eq!(
            highlighter.syntax_context_at(&new, in_block_comment),
            Some(SyntaxContext::Comment)
        );
        assert_eq!(
            highlighter.syntax_context_at(&new, 3),
            Some(SyntaxContext::Code)
        );
        // The editor asked before handing the highlighter the text it holds.
        assert_eq!(
            highlighter.syntax_context_at(&Rope::from("fn a() {}\n"), 3),
            None,
            "other text"
        );
    }

    /// An update with the text the highlighter already holds must not reach
    /// the parser while the tree is behind, or it would count as current.
    #[cfg(feature = "tree-sitter-rust")]
    #[test]
    fn no_op_update_is_skipped_only_while_stale() {
        let highlighter = TreeSitterInputHighlighter::new("rust");
        let text = Rope::from("fn a() {}\n");
        highlighter.inner.borrow_mut().update_input(None, &text, None);
        assert!(highlighter.unchanged_while_stale(&text), "stale, same text");
        assert!(!highlighter.unchanged_while_stale(&Rope::from("fn b() {}\n")));
        highlighter.tree_current.set(true);
        assert!(!highlighter.unchanged_while_stale(&text), "current: parse as usual");
    }

    #[test]
    fn large_code_first_tree_stays_async() {
        let mut highlighter = SyntaxHighlighter::new("rust");
        let text = large_code_text(1024 * 1024);
        let mut resume_bytes = None;
        assert!(!highlighter.update_input_edits(&[], &text, &mut resume_bytes));
        assert_eq!(resume_bytes, None);
        assert!(highlighter.tree().is_none());
        assert_eq!(highlighter.text(), &text);
    }

    #[test]
    fn large_code_incremental_keys_and_batch_recolour_or_fall_back() {
        use std::time::Instant;

        for bytes in [1024 * 1024, 5 * 1024 * 1024] {
            let mut text = large_code_text(bytes);
            let mut current = SyntaxHighlighter::new("rust");
            assert!(current.update(None, &text, None));
            let mut baseline = SyntaxHighlighter::new("rust");
            assert!(baseline.update(None, &text, None));
            let mut before = Vec::new();
            let mut after = Vec::new();
            let mut timeouts = 0;
            let mut resume_bytes = None;
            let mut deferred = 0;
            let typed = "fn newly_coloured() {}\n";
            for (offset, byte) in typed.bytes().enumerate() {
                let edit = InputEdit {
                    start_byte: offset,
                    old_end_byte: offset,
                    new_end_byte: offset + 1,
                    start_position: Point::new(0, offset),
                    old_end_position: Point::new(0, offset),
                    new_end_position: if byte == b'\n' {
                        Point::new(1, 0)
                    } else {
                        Point::new(0, offset + 1)
                    },
                };
                text.insert(offset, &(byte as char).to_string());
                let start = Instant::now();
                baseline.edit_tree(Some(edit), &text);
                before.push(start.elapsed().as_secs_f64() * 1000.0);
                let was_deferred = resume_bytes.is_some();
                let start = Instant::now();
                let completed = current.update_input_edits(&[edit], &text, &mut resume_bytes);
                after.push(start.elapsed().as_secs_f64() * 1000.0);
                assert_eq!(current.text(), &text);
                assert!(current.tree().is_some());
                if completed {
                    if offset >= 2 {
                        let keyword = current.tree().unwrap().root_node()
                            .descendant_for_byte_range(0, 2).unwrap();
                        assert_eq!(keyword.kind(), "fn");
                    }
                } else if was_deferred {
                    deferred += 1;
                } else {
                    timeouts += 1;
                }
            }
            eprintln!(
                "large_code bytes={} before_ms={before:?} after_ms={after:?} timeouts={timeouts}/{} deferred={deferred}",
                text.len(), typed.len()
            );
            assert!(timeouts <= 1, "an oversized tree must not time out on every key");

            // Both adapter entry points use this same batch path. Two edits
            // must shift the old tree in order, then parse only the final text.
            let mut edits = Vec::new();
            for offset in 0..2 {
                text.insert(offset, " ");
                edits.push(InputEdit {
                    start_byte: offset,
                    old_end_byte: offset,
                    new_end_byte: offset + 1,
                    start_position: Point::new(0, offset),
                    old_end_position: Point::new(0, offset),
                    new_end_position: Point::new(0, offset + 1),
                });
            }
            if current.update_input_edits(&edits, &text, &mut resume_bytes) {
                let keyword = current.tree().unwrap().root_node()
                    .descendant_for_byte_range(2, 4).unwrap();
                assert_eq!(keyword.kind(), "fn");
            }
            assert_eq!(current.text(), &text);
            assert!(current.tree().is_some());
            // A substantial shrink permits sync parsing again, including a
            // whole-text replacement without a structural edit.
            let smaller = Rope::from_str("fn recovered() {}\n");
            assert!(current.update_input_edits(&[], &smaller, &mut resume_bytes));
            assert_eq!(resume_bytes, None);
            assert_eq!(current.tree().unwrap().root_node().end_byte(), smaller.len());
        }
    }

    fn chain(code: &str, start: usize, end: usize) -> Vec<String> {
        let mut highlighter = SyntaxHighlighter::new("rust");
        highlighter.update(None, &Rope::from_str(code), None);
        let adapter = TreeSitterInputHighlighter {
            inner: Rc::new(RefCell::new(highlighter)),
            parse_task: Rc::new(RefCell::new(None)),
            parses: Rc::new(std::cell::Cell::new(0)),
            tree_current: Rc::new(std::cell::Cell::new(false)),
            bracket_kinds: RefCell::new(Vec::new()),
            sync_parse_resume_bytes: None,
            edits_tracked: std::cell::Cell::new(false),
            fences_touched: std::cell::Cell::new(false),
        };
        let mut range = start..end;
        let mut steps = Vec::new();
        while let Some(Some(next)) = adapter.syntax_ancestor(range.clone()) {
            steps.push(code[next.clone()].to_string());
            range = next;
        }
        steps
    }

    #[test]
    fn syntax_ancestor_walks_from_a_word_in_a_string_out_to_the_file() {
        let code = "fn main() {\n    greet(\"hello world\");\n}\n";
        let at = code.find("world").unwrap() + 2;
        let steps = chain(code, at, at);
        assert_eq!(
            steps,
            [
                "hello world",
                "\"hello world\"",
                "(\"hello world\")",
                "greet(\"hello world\")",
                "greet(\"hello world\");",
                "{\n    greet(\"hello world\");\n}",
                "fn main() {\n    greet(\"hello world\");\n}",
                code,
            ]
        );
    }

    #[test]
    fn syntax_ancestor_without_a_tree_is_none_and_at_the_root_is_some_none() {
        let adapter = TreeSitterInputHighlighter {
            inner: Rc::new(RefCell::new(SyntaxHighlighter::new("rust"))),
            parse_task: Rc::new(RefCell::new(None)),
            parses: Rc::new(std::cell::Cell::new(0)),
            tree_current: Rc::new(std::cell::Cell::new(false)),
            bracket_kinds: RefCell::new(Vec::new()),
            sync_parse_resume_bytes: None,
            edits_tracked: std::cell::Cell::new(false),
            fences_touched: std::cell::Cell::new(false),
        };
        assert_eq!(adapter.syntax_ancestor(0..0), None);
        let code = "fn a() {}\n";
        let mut highlighter = SyntaxHighlighter::new("rust");
        highlighter.update(None, &Rope::from_str(code), None);
        let adapter = TreeSitterInputHighlighter {
            inner: Rc::new(RefCell::new(highlighter)),
            parse_task: Rc::new(RefCell::new(None)),
            parses: Rc::new(std::cell::Cell::new(0)),
            tree_current: Rc::new(std::cell::Cell::new(false)),
            bracket_kinds: RefCell::new(Vec::new()),
            sync_parse_resume_bytes: None,
            edits_tracked: std::cell::Cell::new(false),
            fences_touched: std::cell::Cell::new(false),
        };
        assert_eq!(adapter.syntax_ancestor(0..code.len()), Some(None));
    }

    #[test]
    fn syntax_ancestor_at_the_end_of_an_identifier_takes_the_identifier() {
        let code = "fn main() {\n    foo(bar);\n}\n";
        let at = code.find("foo").unwrap() + 3;
        assert_eq!(chain(code, at, at)[0], "foo");
        let at = code.find("bar").unwrap() + 3;
        assert_eq!(chain(code, at, at)[0], "bar");
    }

    #[test]
    fn syntax_prose_at_is_true_in_a_markdown_paragraph_only() {
        let code = "# Title\n\nhello world\n\n```rust\nfn a() {}\n```\n";
        let mut highlighter = SyntaxHighlighter::new("markdown");
        highlighter.update(None, &Rope::from_str(code), None);
        let adapter = TreeSitterInputHighlighter {
            inner: Rc::new(RefCell::new(highlighter)),
            parse_task: Rc::new(RefCell::new(None)),
            parses: Rc::new(std::cell::Cell::new(0)),
            tree_current: Rc::new(std::cell::Cell::new(false)),
            bracket_kinds: RefCell::new(Vec::new()),
            sync_parse_resume_bytes: None,
            edits_tracked: std::cell::Cell::new(false),
            fences_touched: std::cell::Cell::new(false),
        };
        assert!(adapter.syntax_prose_at(code.find("world").unwrap() + 2));
        assert!(!adapter.syntax_prose_at(code.find("fn a").unwrap() + 1));
    }

    #[test]
    fn in_fenced_code_is_true_inside_a_markdown_fence_only() {
        for (code, line, inside) in [
            ("- a\n```\n- b\n```\n- c\n", "- b", true),
            ("- a\n```\n- b\n```\n- c\n", "- c", false),
            ("- a\n```\n- b\n```\n- c\n", "- a", false),
            ("- a\n```\n- b\n", "- b", true),
            ("```\n- b\n\n- c", "- c", true),
            ("~~~\n- b\n~~~\n- c", "- c", false),
            ("1. x\n   ```\n   - b\n   ```\n- c", "- b", true),
        ] {
            let text = Rope::from_str(code);
            let mut highlighter = SyntaxHighlighter::new("markdown");
            highlighter.update(None, &text, None);
            let adapter = TreeSitterInputHighlighter {
                inner: Rc::new(RefCell::new(highlighter)),
                parse_task: Rc::new(RefCell::new(None)),
                parses: Rc::new(std::cell::Cell::new(0)),
                tree_current: Rc::new(std::cell::Cell::new(true)),
                bracket_kinds: RefCell::new(Vec::new()),
                sync_parse_resume_bytes: None,
                edits_tracked: std::cell::Cell::new(false),
                fences_touched: std::cell::Cell::new(false),
            };
            // The caret sits at the end of the line, as it does when Return continues a list.
            let at = code.find(line).unwrap() + line.len();
            assert_eq!(adapter.in_fenced_code(&text, at), Some(inside), "{code:?} at {line}");
        }
    }

    /// A fence typed since the last parse is not in the stale tree, so the
    /// tree answers again only for edits that left every fence line alone.
    #[test]
    fn stale_tree_declines_fence_answers_after_a_fence_edit() {
        let old = Rope::from_str("- a\n- c\n");
        let mut highlighter = SyntaxHighlighter::new("markdown");
        highlighter.update(None, &old, None);
        let adapter = TreeSitterInputHighlighter {
            inner: Rc::new(RefCell::new(highlighter)),
            parse_task: Rc::new(RefCell::new(None)),
            parses: Rc::new(std::cell::Cell::new(0)),
            tree_current: Rc::new(std::cell::Cell::new(false)),
            bracket_kinds: RefCell::new(Vec::new()),
            sync_parse_resume_bytes: None,
            edits_tracked: std::cell::Cell::new(true),
            fences_touched: std::cell::Cell::new(false),
        };
        let edit_at = |old: &Rope, at: usize, insert: &str| {
            let mut new = old.clone();
            new.insert(at, insert);
            let row = old.offset_to_point(at).row;
            let column = at - old.line_start_offset(row);
            let edit = BaseInputEdit {
                start_byte: at,
                old_end_byte: at,
                new_end_byte: at + insert.len(),
                start_position: gpui_base::input::Point::new(row, column),
                old_end_position: gpui_base::input::Point::new(row, column),
                new_end_position: gpui_base::input::Point::new(row, column + insert.len()),
            };
            let touched = edit_touches_fence(old, &new, edit);
            adapter
                .inner
                .borrow_mut()
                .edit_tree(Some(to_tree_sitter_edit(edit)), &new);
            (new, touched)
        };

        // Typing inside a list item leaves every fence alone.
        let (text, touched) = edit_at(&old, 3, "x");
        assert!(!touched);
        assert_eq!(adapter.in_fenced_code(&text, 4), Some(false));

        // Typing a fence and an item below it: the tree has no code block yet.
        let (text, touched) = edit_at(&text, 0, "```\n- b\n");
        assert!(touched);
        adapter.fences_touched.set(touched);
        let caret = text.to_string().find("- b").unwrap() + 3;
        assert_eq!(adapter.in_fenced_code(&text, caret), None);
    }
}
