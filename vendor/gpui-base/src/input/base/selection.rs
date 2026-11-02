use crate::input::InputModeKind;
use std::ops::Range;

use gpui::{Context, Window};
use ropey::Rope;
use sum_tree::Bias;

use super::line_ops::{real_rows, row_span, touched_rows};
use super::{InputBaseState, RopeExt as _};
use crate::input::SelectLine;
use crate::text_boundary::word_range_in_rope;

/// Unique identifier for a cursor/selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub(super) struct CursorId(usize);

impl CursorId {
    pub(super) fn new(id: usize) -> Self {
        Self(id)
    }
}

impl<M: InputModeKind> InputBaseState<M> {
    /// Select the word at the given offset on double-click.
    ///
    /// The offset is the UTF-8 offset.
    pub(super) fn select_word(&mut self, offset: usize, _: &mut Window, cx: &mut Context<Self>) {
        // A masked value renders as one unbroken run of mask characters, so it
        // has no word boundaries to select by. Take all of it instead, rather
        // than let the selection highlight reveal where the words are.
        let range = if self.masked {
            0..self.text.len()
        } else {
            let Some(range) = TextSelector::word_range(&self.text, offset) else {
                return;
            };
            range
        };

        self.undo_manager.break_transaction_coalescing();
        self.selections.remove_all_but_active();
        self.set_selection(range.start, range.end);
        self.selected_word_range = Some(*self.active_selection());
        cx.notify()
    }

    /// Select the line at the given offset on triple-click.
    ///
    /// The offset is the UTF-8 offset.
    pub(super) fn select_line(&mut self, offset: usize, _: &mut Window, cx: &mut Context<Self>) {
        let range = TextSelector::line_range(&self.text, offset);
        self.undo_manager.break_transaction_coalescing();
        self.selections.remove_all_but_active();
        self.set_selection(range.start, range.end);
        self.selected_word_range = Some(*self.active_selection());
        cx.notify()
    }

    /// Extend a mouse gesture by the unit selected at its first press.
    pub(super) fn select_mouse_to(
        &mut self,
        offset: usize,
        affinity: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(anchor) = self.selected_word_range else {
            self.select_to_with_affinity(offset, affinity, cx);
            return;
        };
        let range = match self.mouse_selection_click_count {
            2 if self.masked => 0..self.text.len(),
            2 => TextSelector::word_range(&self.text, offset).unwrap_or(offset..offset),
            3.. => TextSelector::line_range(&self.text, offset),
            _ => {
                self.select_to_with_affinity(offset, affinity, cx);
                return;
            }
        };
        let reversed = range.start < anchor.start;
        let start = anchor.start.min(range.start);
        let end = anchor.end.max(range.end);
        let range = self.normalize_token_range(start..end);
        self.set_selection(range.start, range.end);
        self.active_selection_mut().reversed = reversed;
        self.cursor_line_end_affinity = false;
        cx.notify();
    }

    /// Give each selection the range planned for it (a `None` stays as it is),
    /// and merge those that now overlap.
    pub(super) fn select_planned_ranges(
        &mut self,
        plan: impl FnOnce(&Rope, &[(usize, usize)]) -> Vec<Option<(usize, usize)>>,
        cx: &mut Context<Self>,
    ) {
        let before: Vec<_> = self.selections.iter().copied().collect();
        let ranges: Vec<(usize, usize)> = before.iter().map(|sel| (sel.start, sel.end)).collect();
        let planned = plan(&self.text, &ranges);
        if planned.iter().all(Option::is_none) {
            return;
        }
        let after: Vec<_> = before
            .iter()
            .zip(planned)
            .map(|(sel, range)| {
                let mut sel = *sel;
                if let Some((start, end)) = range {
                    sel.start = start;
                    sel.end = end;
                    sel.reversed = false;
                    sel.column_anchor = None;
                }
                sel
            })
            .collect();
        self.undo_manager.break_transaction_coalescing();
        self.pause_blink_cursor(cx);
        M::clear_inline_completion(self, cx);
        self.selections.replace_all(after);
        self.selections.merge_overlapping();
        self.selected_word_range = None;
        self.cursor_line_end_affinity = false;
        self.scroll_to(self.cursor(), None, cx);
        cx.notify()
    }

    pub(super) fn select_line_action(&mut self, _: &SelectLine, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_multi_line() {
            self.select_planned_ranges(plan_select_lines, cx);
        }
    }
}

/// Where each selection ends up when it is taken to whole lines, `None` for
/// one that stays. A selection takes the lines it touches with their breaks;
/// one that already is whole lines takes the next line as well, and stays when
/// no line follows. The empty row after a final newline is not a line.
pub(super) fn plan_select_lines(rope: &Rope, selections: &[(usize, usize)]) -> Vec<Option<(usize, usize)>> {
    let rows = real_rows(rope);
    selections
        .iter()
        .map(|sel| {
            let (first, last) = touched_rows(rope, rows, sel)?;
            let (start, end) = *sel;
            let head = row_span(rope, first).start;
            let tail = row_span(rope, last).end;
            if start == head && end > start && end == tail {
                return (last + 1 < rows).then(|| (head, row_span(rope, last + 1).end));
            }
            Some((head, tail))
        })
        .collect()
}

pub(crate) struct TextSelector;
impl TextSelector {
    /// Select a line in the given text at the specified offset.
    ///
    /// The offset is the UTF-8 offset.
    ///
    /// Returns the start and end offsets of the selected line.
    pub(crate) fn line_range(text: &Rope, offset: usize) -> Range<usize> {
        let offset = text.clip_offset(offset, Bias::Left);
        let row = text.offset_to_point(offset).row;
        let start = text.line_start_offset(row);
        let end = if row + 1 < text.lines_len() {
            text.line_start_offset(row + 1)
        } else {
            text.len()
        };

        start..end
    }

    /// Select a word in the given text at the specified offset.
    ///
    /// The offset is the UTF-8 offset.
    ///
    /// Returns the start and end offsets of the selected word.
    pub(crate) fn word_range(text: &Rope, offset: usize) -> Option<Range<usize>> {
        word_range_in_rope(text.slice(..), offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;

    #[test]
    fn test_unicode_word_range() {
        let text = "Ελληνικά עברית 한글 단어 ひらがな 👨‍👩‍👧";
        let rope = Rope::from(text);
        for word in text.split(' ') {
            let start = text.find(word).unwrap();
            let expected = start..start + word.len();
            for offset in expected.clone() {
                assert_eq!(TextSelector::word_range(&rope, offset), Some(expected.clone()));
            }
        }
    }

    #[test]
    fn test_word_range() {
        use indoc::indoc;

        let rope = Rope::from(indoc! {
            r#"
            test text:
            abcde 中文🎉 test
            hello[()]
            test_connector ____
            Rope
            rök
            grande île
            "#
        });

        let tests = vec![
            (0, 0, Some("test")),
            (0, 4, Some(" ")),
            (1, 0, Some("abcde")),
            (1, 4, Some("abcde")),
            (1, 5, Some(" ")),
            (1, 6, Some("中")),
            (1, 9, Some("文")),
            (1, 13, Some("🎉")),
            (1, 20, Some("test")),
            (2, 5, Some("[")),
            (2, 6, Some("(")),
            (2, 7, Some(")")),
            (2, 8, Some("]")),
            (3, 5, Some("test_connector")),
            (3, 14, Some(" ")),
            (3, 16, Some("____")),
            (4, 0, Some("Rope")),
            (5, 0, Some("rök")),
            (6, 8, Some("île")),
        ];

        for (line, column, expected) in tests {
            let line_start_offset = rope.line_start_offset(line);
            let offset = line_start_offset + column;
            let range = TextSelector::word_range(&rope, offset);

            let actual = range.map(|r| rope.slice(r).to_string());
            let expect = expected.map(|s| s.to_string());
            assert_eq!(actual, expect, "line {}, column {}", line, column);
        }
    }

    #[test]
    fn test_line_range() {
        let rope = Rope::from("first line\nsecond line\nthird");
        let tests = vec![
            (0, 0, "first line\n"),
            (0, 5, "first line\n"),
            (1, 3, "second line\n"),
            (2, 1, "third"),
        ];

        for (line, column, expected) in tests {
            let line_start_offset = rope.line_start_offset(line);
            let offset = line_start_offset + column;
            let range = TextSelector::line_range(&rope, offset);

            let actual = rope.slice(range).to_string();
            assert_eq!(actual, expected, "line {}, column {}", line, column);
        }
        let rope = Rope::from("first\r\n\r\nlast\n");
        for (offset, expected) in [(2, "first\r\n"), (7, "\r\n"), (10, "last\n"), (14, "")] {
            assert_eq!(rope.slice(TextSelector::line_range(&rope, offset)).to_string(), expected);
        }
    }

    fn selected(text: &str, selections: &[(usize, usize)]) -> Vec<(usize, usize)> {
        let rope = Rope::from(text);
        plan_select_lines(&rope, selections)
            .into_iter()
            .zip(selections)
            .map(|(range, &old)| range.unwrap_or(old))
            .collect()
    }

    #[test]
    fn select_line_takes_the_line_with_its_break() {
        let text = "one\ntwo\nthree";
        assert_eq!(selected(text, &[(1, 1)]), vec![(0, 4)]);
        assert_eq!(selected(text, &[(5, 6)]), vec![(4, 8)]);
        // The last line has no break to take.
        assert_eq!(selected(text, &[(10, 10)]), vec![(8, 13)]);
        // A selection over two lines takes both.
        assert_eq!(selected(text, &[(2, 5)]), vec![(0, 8)]);
        // CRLF stays whole.
        assert_eq!(selected("a\r\nb\r\n", &[(0, 0)]), vec![(0, 3)]);
    }

    #[test]
    fn select_line_again_adds_the_next_line() {
        let text = "one\ntwo\nthree\n";
        assert_eq!(selected(text, &[(0, 4)]), vec![(0, 8)]);
        assert_eq!(selected(text, &[(0, 8)]), vec![(0, 14)]);
        // Nothing follows the last line: it stays selected.
        assert_eq!(selected(text, &[(0, 14)]), vec![(0, 14)]);
        assert_eq!(selected("one\ntwo", &[(4, 7)]), vec![(4, 7)]);
        // A selection that is not whole lines is taken to whole lines first.
        assert_eq!(selected(text, &[(1, 4)]), vec![(0, 4)]);
    }

    #[test]
    fn select_line_works_at_every_caret() {
        let text = "one\ntwo\nthree\nfour";
        assert_eq!(selected(text, &[(1, 1), (9, 9)]), vec![(0, 4), (8, 14)]);
        assert_eq!(selected(text, &[(0, 4), (8, 14)]), vec![(0, 8), (8, 18)]);
    }
}

