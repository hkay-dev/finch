//! Toggle comments on the selected lines (Cmd-/), after Zed's `toggle_comments`.
//!
//! Line-comment languages add or remove `prefix + space` at the smallest indent
//! of the non-blank selected rows. Languages without a line comment (html,
//! css, markdown) wrap the selected rows in their block comment. Plain text
//! has no comment syntax, so nothing happens.

use std::ops::Range;

use gpui::{Context, Window};
use ropey::Rope;

use crate::input::{
    InputBaseState, InputModeKind, RopeExt, ToggleComments, cursor::CursorSelection,
    mode::LayoutMode,
};

/// How a language writes comments.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CommentStyle {
    Line(&'static str),
    Block(&'static str, &'static str),
}

/// The comment syntax of an editor language name (as `src/language.rs` returns
/// it). `None` for plain text and unknown names.
fn comment_style(language: &str) -> Option<CommentStyle> {
    match language {
        "rust" | "swift" | "typescript" | "tsx" | "javascript" | "json" | "c" | "cpp" | "csharp"
        | "go" | "java" | "kotlin" | "scala" | "zig" | "proto" | "php" => {
            Some(CommentStyle::Line("//"))
        }
        "python" | "bash" | "toml" | "yaml" | "ruby" | "elixir" | "cmake" | "make" | "graphql" => {
            Some(CommentStyle::Line("#"))
        }
        "lua" => Some(CommentStyle::Line("--")),
        "markdown" | "html" | "astro" | "svelte" => Some(CommentStyle::Block("<!--", "-->")),
        "erb" | "ejs" => Some(CommentStyle::Block("<%#", "%>")),
        "css" => Some(CommentStyle::Block("/*", "*/")),
        _ => None,
    }
}

type Edit = (Range<usize>, String);

/// The rows every selection touches, as sorted row ranges where overlapping
/// ranges are merged. A selection that ends at column 0 of a later row leaves
/// that row out.
fn selected_row_ranges(text: &Rope, selections: &[(usize, usize)]) -> Vec<Range<usize>> {
    let mut ranges: Vec<Range<usize>> = selections
        .iter()
        .map(|&(start, end)| {
            let (start, end) = (start.min(end), start.max(end));
            let start_row = text.offset_to_point(start).row;
            let end_point = text.offset_to_point(end);
            let end_row = if end > start && end_point.column == 0 && end_point.row > start_row {
                end_point.row - 1
            } else {
                end_point.row
            };
            start_row..end_row + 1
        })
        .collect();
    ranges.sort_by_key(|range| range.start);
    let mut merged: Vec<Range<usize>> = Vec::with_capacity(ranges.len());
    for range in ranges {
        match merged.last_mut() {
            Some(last) if range.start < last.end => last.end = last.end.max(range.end),
            _ => merged.push(range),
        }
    }
    merged
}

/// One row's text, without its line break.
fn row_text(text: &Rope, row: usize) -> String {
    let mut line = text.slice_line(row).to_string();
    while line.ends_with(['\r', '\n']) {
        line.pop();
    }
    line
}

/// Bytes of leading blanks in a line.
fn indent_len(line: &str) -> usize {
    line.len() - line.trim_start_matches([' ', '\t']).len()
}

/// The edits that toggle comments on the rows the selections touch. Disjoint
/// and sorted by start; empty when there is nothing to do.
fn toggle_edits(text: &Rope, selections: &[(usize, usize)], style: CommentStyle) -> Vec<Edit> {
    let row_ranges = selected_row_ranges(text, selections);
    match style {
        CommentStyle::Line(prefix) => line_edits(text, &row_ranges, prefix),
        CommentStyle::Block(open, close) => {
            let mut edits = Vec::new();
            for rows in row_ranges {
                block_edits(text, rows, open, close, &mut edits);
            }
            edits
        }
    }
}

fn line_edits(text: &Rope, row_ranges: &[Range<usize>], prefix: &str) -> Vec<Edit> {
    // (row start offset, line text) of every non-blank row.
    let mut rows: Vec<(usize, String)> = Vec::new();
    for rows_range in row_ranges {
        for row in rows_range.clone() {
            let line = row_text(text, row);
            if !line.trim().is_empty() {
                rows.push((text.line_start_offset(row), line));
            }
        }
    }
    if rows.is_empty() {
        return Vec::new();
    }

    let all_commented = rows
        .iter()
        .all(|(_, line)| line[indent_len(line)..].starts_with(prefix));
    if all_commented {
        return rows
            .iter()
            .map(|(start, line)| {
                let from = start + indent_len(line);
                let mut len = prefix.len();
                if line[indent_len(line) + len..].starts_with(' ') {
                    len += 1;
                }
                (from..from + len, String::new())
            })
            .collect();
    }

    let column = rows
        .iter()
        .map(|(_, line)| indent_len(line))
        .min()
        .unwrap_or(0);
    let insert = format!("{prefix} ");
    rows.iter()
        .map(|(start, _)| (start + column..start + column, insert.clone()))
        .collect()
}

/// Wrap the rows in the block comment, or unwrap them when the first
/// non-blank row opens it and the last closes it.
fn block_edits(text: &Rope, rows: Range<usize>, open: &str, close: &str, edits: &mut Vec<Edit>) {
    let non_blank = |row: &usize| !row_text(text, *row).trim().is_empty();
    let Some(first) = rows.clone().find(non_blank) else {
        return;
    };
    let last = rows.clone().rev().find(non_blank).unwrap_or(first);
    let first_line = row_text(text, first);
    let last_line = row_text(text, last);
    let first_start = text.line_start_offset(first) + indent_len(&first_line);
    let last_trimmed = last_line.trim_end();
    let last_end = text.line_start_offset(last) + last_trimmed.len();

    let body = &first_line[indent_len(&first_line)..];
    let wrapped = body.starts_with(open)
        && last_trimmed.ends_with(close)
        && first_start + open.len() <= last_end - close.len();
    if wrapped {
        let indent = indent_len(&first_line);
        let mut open_len = open.len();
        if body[open_len..].starts_with(' ') {
            open_len += 1;
        }
        let before_close = &last_trimmed[..last_trimmed.len() - close.len()];
        let mut close_len = close.len();
        let open_end = if first == last { indent + open_len } else { 0 };
        if before_close.ends_with(' ') && before_close.len() > open_end {
            close_len += 1;
        }
        edits.push((first_start..first_start + open_len, String::new()));
        edits.push((last_end - close_len..last_end, String::new()));
    } else {
        edits.push((first_start..first_start, format!("{open} ")));
        edits.push((last_end..last_end, format!(" {close}")));
    }
}

/// Where an offset lands once the edits (sorted, disjoint, in pre-edit
/// coordinates) are applied: after an insertion at or before it, except a
/// block-comment close (` */`, ` -->`) inserted at it, which stays after the
/// offset as Zed keeps selections before an inserted suffix; at the start of
/// a removal that covers it.
fn map_offset(edits: &[Edit], offset: usize) -> usize {
    let mut mapped = offset;
    for (range, text) in edits {
        let suffix_at_offset = range.is_empty() && range.start == offset && text.starts_with(' ');
        if range.end <= offset && !suffix_at_offset {
            mapped = mapped + text.len() - range.len();
        } else if range.start < offset {
            mapped -= offset - range.start;
            break;
        } else {
            break;
        }
    }
    mapped
}

impl<M: InputModeKind> InputBaseState<M> {
    pub(super) fn toggle_comments(
        &mut self,
        _: &ToggleComments,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_editable() || !self.is_multi_line() {
            cx.propagate();
            return;
        }
        // A refused edit would leave the shifted selections past the end of the text.
        if self.edit_blocked(window, cx) {
            return;
        }
        let style = match &self.mode {
            LayoutMode::CodeEditor { language, .. } => comment_style(&language.name()),
            _ => None,
        };
        let Some(style) = style else {
            // No comment mark for this language: say so the way other refused commands do.
            window.play_system_bell();
            return;
        };

        let before: Vec<CursorSelection> = self.selections.iter().copied().collect();
        let ranges: Vec<(usize, usize)> = before.iter().map(|sel| (sel.start, sel.end)).collect();
        let edits = toggle_edits(&self.text, &ranges, style);
        if edits.is_empty() {
            return;
        }
        let new_selections: Vec<CursorSelection> = before
            .iter()
            .map(|sel| {
                let mut selection = *sel;
                selection.start = map_offset(&edits, sel.start);
                selection.end = map_offset(&edits, sel.end);
                selection.column_anchor = None;
                selection
            })
            .collect();

        let rows: usize = selected_row_ranges(&self.text, &ranges).iter().map(|rows| rows.len()).sum();
        self.undo_manager.begin_transaction();
        self.replace_text_in_ranges(&edits, window, cx);
        self.selections.replace_all(new_selections);
        let after: Vec<CursorSelection> = self.selections.iter().copied().collect();
        self.undo_manager.record_selections(before, after);
        self.undo_manager.commit_transaction();

        self.scroll_to(self.cursor(), None, cx);
        cx.notify();
        // Two or more rows moved as a block lift once; one row is its own sign.
        if rows >= 2 {
            self.lift_selection(window, cx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RUST: CommentStyle = CommentStyle::Line("//");

    /// Toggle over the given (start, end) selections and return the new text
    /// with the mapped selections.
    fn toggle(
        text: &str,
        selections: &[(usize, usize)],
        style: CommentStyle,
    ) -> (String, Vec<(usize, usize)>) {
        let mut rope = Rope::from(text);
        let edits = toggle_edits(&rope, selections, style);
        let mapped = selections
            .iter()
            .map(|&(s, e)| (map_offset(&edits, s), map_offset(&edits, e)))
            .collect();
        for (range, new) in edits.iter().rev() {
            rope.replace(range.clone(), new);
        }
        (rope.to_string(), mapped)
    }

    fn all(text: &str) -> Vec<(usize, usize)> {
        vec![(0, text.len())]
    }

    #[test]
    fn mixed_rows_comment_every_row() {
        let text = "// a\nb\n// c\n";
        let (out, _) = toggle(text, &[(0, text.len() - 1)], RUST);
        assert_eq!(out, "// // a\n// b\n// // c\n");
    }

    #[test]
    fn all_commented_rows_uncomment_with_one_space() {
        let text = "// a\n//b\n//  c\n";
        let (out, _) = toggle(text, &all(text), RUST);
        assert_eq!(out, "a\nb\n c\n");
    }

    #[test]
    fn blank_rows_are_skipped_both_ways() {
        let text = "a\n\n   \nb\n";
        let (out, _) = toggle(text, &all(text), RUST);
        assert_eq!(out, "// a\n\n   \n// b\n");
        let (back, _) = toggle(&out, &all(&out), RUST);
        assert_eq!(back, text);
        assert_eq!(toggle("\n  \n", &[(0, 4)], RUST).0, "\n  \n");
    }

    #[test]
    fn indented_block_comments_at_the_smallest_indent() {
        let text = "fn f() {\n    a;\n      b;\n    c;\n}";
        let start = text.find("    a").unwrap();
        let end = text.find("\n}").unwrap();
        let (out, _) = toggle(text, &[(start, end)], RUST);
        assert_eq!(out, "fn f() {\n    // a;\n    //   b;\n    // c;\n}");
        let start = out.find("    //").unwrap();
        let end = out.find("\n}").unwrap();
        let (back, _) = toggle(&out, &[(start, end)], RUST);
        assert_eq!(back, text);
    }

    #[test]
    fn selection_ending_at_line_start_leaves_that_row() {
        let (out, _) = toggle("a\nb\nc", &[(0, 4)], RUST);
        assert_eq!(out, "// a\n// b\nc");
    }

    #[test]
    fn multi_cursor_rows_merge_and_toggle_twice_restores() {
        let text = "a\nb\nc\nd\ne\n";
        // Cursors on rows 0, 1 (same row twice over), 3 and a selection over 1..2.
        let selections = [(0, 0), (2, 2), (3, 5), (6, 6)];
        let (out, mapped) = toggle(text, &selections, RUST);
        assert_eq!(out, "// a\n// b\n// c\n// d\ne\n");
        assert_eq!(mapped[0], (3, 3));
        let (back, _) = toggle(&out, &mapped, RUST);
        assert_eq!(back, text);
    }

    #[test]
    fn selections_keep_their_place_on_the_text() {
        let text = "ab\ncd";
        let (out, mapped) = toggle(text, &[(1, 4)], RUST);
        assert_eq!(out, "// ab\n// cd");
        assert_eq!(&out[mapped[0].0..mapped[0].1], "b\n// c");
        let (back, back_sel) = toggle(&out, &mapped, RUST);
        assert_eq!(back, text);
        assert_eq!(back_sel[0], (1, 4));
    }

    #[test]
    fn crlf_rows_comment_and_restore() {
        let text = "a\r\n\r\nb\r\n";
        let (out, _) = toggle(text, &all(text), RUST);
        assert_eq!(out, "// a\r\n\r\n// b\r\n");
        assert_eq!(toggle(&out, &all(&out), RUST).0, text);
    }

    #[test]
    fn unicode_and_last_row_without_newline() {
        let text = "héllo\n  wörld";
        let (out, _) = toggle(text, &all(text), CommentStyle::Line("#"));
        assert_eq!(out, "# héllo\n#   wörld");
        assert_eq!(toggle(&out, &all(&out), CommentStyle::Line("#")).0, text);
    }

    #[test]
    fn block_comment_wraps_and_unwraps_the_rows() {
        let html = CommentStyle::Block("<!--", "-->");
        let text = "  <p>a</p>\n  <p>b</p>\n";
        let (out, _) = toggle(text, &[(0, text.len() - 1)], html);
        assert_eq!(out, "  <!-- <p>a</p>\n  <p>b</p> -->\n");
        assert_eq!(toggle(&out, &[(0, out.len() - 1)], html).0, text);

        let css = CommentStyle::Block("/*", "*/");
        let (out, _) = toggle("a {}", &[(0, 0)], css);
        assert_eq!(out, "/* a {} */");
        assert_eq!(toggle(&out, &[(0, 0)], css).0, "a {}");
        assert_eq!(toggle("a {}", &[(4, 4)], css).1, vec![(7, 7)]);
        assert_eq!(toggle("a {}", &[(0, 4)], css).1, vec![(3, 7)]);
    }

    #[test]
    fn style_table_covers_the_language_names() {
        for name in [
            "rust", "swift", "typescript", "tsx", "javascript", "json", "c", "cpp", "csharp", "go",
            "java", "kotlin", "scala", "zig", "proto", "php",
        ] {
            assert_eq!(comment_style(name), Some(CommentStyle::Line("//")));
        }
        for name in ["python", "bash", "toml", "yaml", "ruby", "elixir", "cmake", "make", "graphql"]
        {
            assert_eq!(comment_style(name), Some(CommentStyle::Line("#")));
        }
        assert_eq!(comment_style("lua"), Some(CommentStyle::Line("--")));
        for name in ["astro", "svelte", "markdown"] {
            assert_eq!(comment_style(name), Some(CommentStyle::Block("<!--", "-->")));
        }
        assert_eq!(comment_style("diff"), None);
        assert!(matches!(comment_style("html"), Some(CommentStyle::Block(..))));
        assert!(matches!(comment_style("css"), Some(CommentStyle::Block(..))));
        assert_eq!(comment_style("text"), None);
    }
}
