//! Tidying the text just before a file is written: strip trailing spaces and
//! tabs from every line, and end the text with a newline. Both go through the
//! same plan as Edit > Transformations > Remove trailing spaces, so a save
//! is one undo step and every caret stays on the text it was on.

use std::ops::Range;

use gpui::{Context, Window};
use ropey::Rope;

use super::line_ops::{LinePlan, row_span};
use super::transform::plan_trim_trailing;
use crate::input::{InputBaseState, InputModeKind, RopeExt as _};

/// The edits that trim every line (`trim`) and end the text with one newline
/// (`final_newline`, in the text's own line ending; an empty text and a text
/// that already ends with one are left as they are). `markdown` keeps a run of
/// two or more spaces after text, which is a hard line break there. `None` when
/// nothing changes.
fn plan_save(rope: &Rope, selections: &[(usize, usize)], trim: bool, final_newline: bool, markdown: bool) -> Option<LinePlan> {
    let mut edits: Vec<(Range<usize>, String)> = if trim {
        // Nothing selected: every line.
        let mut edits = plan_trim_trailing(rope, &[(0, 0)]).map(|plan| plan.edits).unwrap_or_default();
        if markdown {
            edits.retain(|(range, _)| {
                let after_text = rope.chars_at(range.start).prev().is_some_and(|c| c != '\n' && c != '\r');
                let spaces = range.len() >= 2 && rope.slice(range.clone()).to_string().chars().all(|c| c == ' ');
                !(after_text && spaces)
            });
        }
        edits
    } else {
        Vec::new()
    };
    let len = rope.len();
    let after_trim = edits.last().filter(|(range, _)| range.end == len).map_or(len, |(range, _)| range.start);
    if final_newline && after_trim > 0 && rope.char_at(after_trim - 1) != Some('\n') {
        let crlf = rope.lines_len() > 1 && {
            let first = row_span(rope, 0);
            first.end - first.content_end == 2
        };
        let newline = if crlf { "\r\n" } else { "\n" };
        match edits.last_mut() {
            Some((range, with)) if range.end == len => with.push_str(newline),
            _ => edits.push((len..len, newline.to_owned())),
        }
    }
    if edits.is_empty() {
        return None;
    }
    // A position after a cut moves back by it; one inside a cut lands where it starts. Only cuts
    // count: the newline goes in at the very end, so a caret there stays before it.
    let moved = |position: usize| -> usize {
        let before = edits.partition_point(|(range, _)| range.end <= position);
        let removed: usize = edits[..before].iter().map(|(range, _)| range.len()).sum();
        let inside = edits.get(before).filter(|(range, _)| range.start < position).map_or(0, |(range, _)| position - range.start);
        position - removed - inside
    };
    let selections = selections.iter().map(|&(start, end)| (moved(start), moved(end))).collect();
    Some(LinePlan { edits, selections })
}

/// The rows of `after` (the text with `edits` applied, as planned against the text before) that
/// the edits touched, ascending and without repeats. A cut leaves its row; an added final newline
/// counts the row it ends.
fn changed_rows(after: &Rope, edits: &[(Range<usize>, String)]) -> Vec<usize> {
    let len = after.len();
    let mut rows: Vec<usize> = Vec::new();
    let mut shift = 0isize;
    for (range, with) in edits {
        let start = ((range.start as isize + shift) as usize).min(len);
        let mut end = (start + with.len()).min(len);
        shift += with.len() as isize - range.len() as isize;
        if end > start && after.char_at(end - 1) == Some('\n') {
            end -= 1;
        }
        let (first, last) = (after.offset_to_point(start).row, after.offset_to_point(end).row);
        rows.extend(first..=last);
    }
    rows.dedup();
    rows
}

impl<M: InputModeKind> InputBaseState<M> {
    /// Before a save: trims trailing spaces and tabs (`trim`) and makes the text end with a
    /// newline (`final_newline`), as one undo step with the carets kept in place. The rows it
    /// changed wash once, like Remove trailing spaces does; a save that changes nothing washes
    /// nothing. Returns those rows (empty when nothing changed).
    pub fn normalize_for_save(&mut self, trim: bool, final_newline: bool, markdown: bool, window: &mut Window, cx: &mut Context<Self>) -> Vec<usize> {
        if !trim && !final_newline {
            return Vec::new();
        }
        let mut planned = Vec::new();
        self.apply_washed_line_plan_in(
            true,
            |rope, selections| {
                let plan = plan_save(rope, selections, trim, final_newline, markdown)?;
                planned = plan.edits.clone();
                Some(plan)
            },
            window,
            cx,
        );
        changed_rows(self.text(), &planned)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(text: &str, trim: bool, final_newline: bool) -> String {
        apply_in(text, trim, final_newline, false)
    }

    fn apply_in(text: &str, trim: bool, final_newline: bool, markdown: bool) -> String {
        let rope = Rope::from(text);
        let Some(plan) = plan_save(&rope, &[(0, 0)], trim, final_newline, markdown) else { return text.to_owned() };
        let mut out = text.to_owned();
        for (range, with) in plan.edits.iter().rev() {
            out.replace_range(range.clone(), with);
        }
        out
    }

    #[test]
    fn markdown_keeps_hard_line_breaks() {
        assert_eq!(apply_in("a  \nb \n   \nc\t\n", true, false, true), "a  \nb\n\nc\n");
        assert_eq!(apply_in("a  \r\nb  ", true, true, true), "a  \r\nb  \r\n");
        assert_eq!(apply_in("é  \nb", true, false, true), "é  \nb");
    }

    #[test]
    fn off_changes_nothing() {
        assert_eq!(apply("a  \nb", false, false), "a  \nb");
    }

    #[test]
    fn trim_strips_spaces_and_tabs_only() {
        assert_eq!(apply("a \t\n  b  \n\n c", true, false), "a\n  b\n\n c");
    }

    #[test]
    fn final_newline_adds_one_never_two() {
        assert_eq!(apply("a\nb", false, true), "a\nb\n");
        assert_eq!(apply("a\nb\n", false, true), "a\nb\n");
        assert_eq!(apply("", false, true), "");
    }

    #[test]
    fn both_together_and_crlf() {
        assert_eq!(apply("a  \nb  ", true, true), "a\nb\n");
        assert_eq!(apply("a  \r\nb  ", true, true), "a\r\nb\r\n");
        assert_eq!(apply("a \r\nb\r\n", true, true), "a\r\nb\r\n");
    }

    #[test]
    fn a_caret_stays_on_its_text() {
        let rope = Rope::from("ab  \ncd");
        let plan = plan_save(&rope, &[(1, 1), (7, 7)], true, true, false).unwrap();
        assert_eq!(plan.selections, vec![(1, 1), (5, 5)]);
    }

    fn rows(text: &str, trim: bool, final_newline: bool) -> Vec<usize> {
        let rope = Rope::from(text);
        let Some(plan) = plan_save(&rope, &[(0, 0)], trim, final_newline, false) else { return Vec::new() };
        changed_rows(&Rope::from(apply(text, trim, final_newline)), &plan.edits)
    }

    #[test]
    fn changed_rows_name_trims_and_the_added_newline() {
        assert_eq!(rows("a  \nb\nc \t\nd", true, false), vec![0, 2]);
        assert_eq!(rows("a\nb", false, true), vec![1]);
        assert_eq!(rows("a  \r\nb  ", true, true), vec![0, 1]);
        assert_eq!(rows("a\n   \nb\n", true, true), vec![1]);
    }

    #[test]
    fn a_clean_save_changes_no_rows() {
        assert!(rows("a\nb\n", true, true).is_empty());
        assert!(rows("a  \nb", false, false).is_empty());
        assert!(rows("", true, true).is_empty());
    }
}
