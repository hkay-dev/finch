//! Expand and shrink the selection by syntax.
//!
//! `SelectLargerSyntaxNode` grows every selection to the first named syntax
//! node strictly larger than it (the word first when inside a string or a
//! comment). `SelectSmallerSyntaxNode` walks back through the selections the
//! expansions replaced. Without a syntax tree, expanding goes word, line, then
//! the whole text.

use std::ops::Range;

use gpui::{Context, Window};
use sum_tree::Bias;

use super::{
    InputBaseState, RopeExt as _,
    cursor::CursorSelection,
    kind::InputModeKind,
    state::{SelectLargerSyntaxNode, SelectSmallerSyntaxNode},
};
use crate::input::SyntaxContext;
use crate::text_boundary::word_range_from_chars;

/// The selections each expansion replaced, so shrinking restores them exactly.
///
/// It is valid only while the text and the selections are as the last
/// expansion or shrink left them; any other selection change or edit makes
/// the next action start it afresh.
#[derive(Default)]
pub(super) struct SyntaxSelectHistory {
    /// `InputBaseState::document_revision` the stack belongs to.
    revision: u64,
    /// The selections the last expansion or shrink left, as (start, end, reversed).
    current: Vec<(usize, usize, bool)>,
    /// Selections before each expansion, the latest last.
    before: Vec<Vec<CursorSelection>>,
}

impl<M: InputModeKind> InputBaseState<M> {
    fn snapshot_selections(&self) -> Vec<(usize, usize, bool)> {
        self.selections
            .iter()
            .map(|s| (s.start, s.end, s.reversed))
            .collect()
    }

    /// Drops the history unless nothing changed since the last action of ours.
    fn validate_syntax_select_history(&mut self) {
        let history = &self.syntax_select_history;
        if history.before.is_empty() {
            return;
        }
        if history.revision != self.document_revision
            || history.current != self.snapshot_selections()
        {
            self.syntax_select_history.before.clear();
        }
    }

    fn record_syntax_select(&mut self) {
        self.syntax_select_history.revision = self.document_revision;
        self.syntax_select_history.current = self.snapshot_selections();
    }

    /// The word around `offset`, if it holds a letter or digit; at a word's
    /// end, the word before it.
    fn word_at_for_expand(&self, offset: usize) -> Option<Range<usize>> {
        let offset = self.text.clip_offset(offset, Bias::Left);
        let word_at = |offset: usize| {
            let char = self.text.char_at(offset)?;
            let end = offset + char.len_utf8();
            let prev_chars = self.text.chars_at(offset).reversed().take(128);
            let next_chars = self.text.chars_at(end).take(128);
            let word = word_range_from_chars(offset, char, prev_chars, next_chars);
            self.text
                .slice(word.clone())
                .chars()
                .any(|c| c.is_alphanumeric())
                .then_some(word)
        };
        word_at(offset).or_else(|| {
            let prev = self.text.chars_at(offset).reversed().next()?;
            word_at(offset - prev.len_utf8())
        })
    }

    /// The range `range` grows to, or `None` when there is nothing larger.
    fn expanded_range(
        &self,
        range: &Range<usize>,
        tree: Option<Option<Range<usize>>>,
        prose: bool,
    ) -> Option<Range<usize>> {
        let strictly_larger = |candidate: &Range<usize>| {
            candidate.start <= range.start && range.end <= candidate.end && candidate != range
        };
        let in_text = prose
            || self.mode.syntax_context_at(&self.text, range.start) != SyntaxContext::Code;
        let word = if self.masked || (tree.is_some() && !in_text) {
            None
        } else {
            self.word_at_for_expand(range.start)
                .filter(|word| strictly_larger(word))
        };
        match tree {
            Some(ancestor) => word.or(ancestor),
            None => {
                if word.is_some() {
                    return word;
                }
                let start_row = self.text.offset_to_point(range.start).row;
                let end_row = self.text.offset_to_point(range.end).row;
                let line = self.text.line_start_offset(start_row)..self.text.line_end_offset(end_row);
                if strictly_larger(&line) {
                    return Some(line);
                }
                let all = 0..self.text.len();
                strictly_larger(&all).then_some(all)
            }
        }
    }

    pub(super) fn select_larger_syntax_node(
        &mut self,
        _: &SelectLargerSyntaxNode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        self.validate_syntax_select_history();

        let ranges: Vec<Option<Range<usize>>> = {
            let highlighter = self.mode.highlighter().map(|h| h.borrow());
            let highlighter = highlighter.as_ref().and_then(|h| h.as_ref());
            self.selections
                .iter()
                .map(|sel| {
                    let range = sel.start..sel.end;
                    let tree = highlighter.and_then(|h| h.syntax_ancestor(range.clone()));
                    let prose = highlighter.is_some_and(|h| h.syntax_prose_at(range.start));
                    self.expanded_range(&range, tree, prose)
                })
                .collect()
        };
        if ranges.iter().all(Option::is_none) {
            return;
        }

        let before: Vec<CursorSelection> = self.selections.iter().copied().collect();
        let after: Vec<CursorSelection> = before
            .iter()
            .zip(ranges)
            .map(|(sel, range)| {
                let mut sel = *sel;
                if let Some(range) = range {
                    let range = self.normalize_token_range(range);
                    sel.start = range.start;
                    sel.end = range.end;
                    sel.reversed = false;
                    sel.column_anchor = None;
                }
                sel
            })
            .collect();

        self.pause_blink_cursor(cx);
        M::clear_inline_completion(self, cx);
        self.selections.replace_all(after);
        self.selections.merge_overlapping();
        self.selected_word_range = None;
        self.cursor_line_end_affinity = false;
        self.syntax_select_history.before.push(before);
        self.record_syntax_select();
        self.scroll_to(self.cursor(), None, cx);
        cx.notify()
    }

    pub(super) fn select_smaller_syntax_node(
        &mut self,
        _: &SelectSmallerSyntaxNode,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.undo_manager.break_transaction_coalescing();
        self.validate_syntax_select_history();
        let Some(before) = self.syntax_select_history.before.pop() else {
            return;
        };

        self.pause_blink_cursor(cx);
        M::clear_inline_completion(self, cx);
        self.selections.replace_all(before);
        self.selected_word_range = None;
        self.cursor_line_end_affinity = false;
        self.record_syntax_select();
        self.scroll_to(self.cursor(), None, cx);
        cx.notify()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{TextareaMode, TextareaState};
    use crate::theme::Theme;
    use gpui::{
        AppContext as _, EntityInputHandler as _, Context, Entity, IntoElement, Render, TestAppContext, VisualTestContext,
        div, prelude::*,
    };

    struct Root(Entity<InputBaseState<TextareaMode>>);

    impl Render for Root {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size_full().child(self.0.clone())
        }
    }

    fn open(
        cx: &mut TestAppContext,
        text: &'static str,
    ) -> (Entity<InputBaseState<TextareaMode>>, VisualTestContext) {
        let mut input = None;
        let window = cx.update(|cx| {
            cx.open_window(Default::default(), |window, cx| {
                cx.set_global(Theme::default());
                super::super::super::init(cx);
                let entity = cx.new(|cx| TextareaState::new(window, cx).default_value(text));
                input = Some(entity.clone());
                cx.new(|_| Root(entity))
            })
            .unwrap()
        });
        (input.unwrap(), VisualTestContext::from_window(window.into(), cx))
    }

    fn expand(input: &Entity<InputBaseState<TextareaMode>>, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            input.update(cx, |s, cx| s.select_larger_syntax_node(&SelectLargerSyntaxNode, window, cx))
        });
    }

    fn shrink(input: &Entity<InputBaseState<TextareaMode>>, cx: &mut VisualTestContext) {
        cx.update(|window, cx| {
            input.update(cx, |s, cx| s.select_smaller_syntax_node(&SelectSmallerSyntaxNode, window, cx))
        });
    }

    fn selected(input: &Entity<InputBaseState<TextareaMode>>, cx: &mut VisualTestContext) -> Vec<String> {
        cx.update(|_, cx| {
            let s = input.read(cx);
            s.selections
                .iter()
                .map(|sel| s.text.slice(sel.start..sel.end).to_string())
                .collect()
        })
    }

    #[gpui::test]
    fn plain_text_grows_word_line_then_text_and_shrinks_back(cx: &mut TestAppContext) {
        let (input, mut cx) = open(cx, "one two\nthree four\nfive");
        cx.update(|_, cx| input.update(cx, |s, cx| s.set_selected_range(10..10, cx)));

        expand(&input, &mut cx);
        assert_eq!(selected(&input, &mut cx), ["three"]);
        expand(&input, &mut cx);
        assert_eq!(selected(&input, &mut cx), ["three four"]);
        expand(&input, &mut cx);
        assert_eq!(selected(&input, &mut cx), ["one two\nthree four\nfive"]);
        expand(&input, &mut cx);
        assert_eq!(selected(&input, &mut cx), ["one two\nthree four\nfive"]);

        shrink(&input, &mut cx);
        assert_eq!(selected(&input, &mut cx), ["three four"]);
        shrink(&input, &mut cx);
        assert_eq!(selected(&input, &mut cx), ["three"]);
        shrink(&input, &mut cx);
        cx.update(|_, cx| assert_eq!(input.read(cx).selected_range(), 10..10));
        shrink(&input, &mut cx);
        cx.update(|_, cx| assert_eq!(input.read(cx).selected_range(), 10..10));
    }

    #[gpui::test]
    fn every_cursor_grows_and_any_other_change_clears_the_history(cx: &mut TestAppContext) {
        let (input, mut cx) = open(cx, "ab cd\nef gh");
        cx.update(|_, cx| {
            input.update(cx, |s, cx| {
                s.set_selected_range(1..1, cx);
                s.add_selection(9..9, cx);
            })
        });

        expand(&input, &mut cx);
        assert_eq!(selected(&input, &mut cx), ["ab", "gh"]);

        // A selection change of its own: shrinking has nothing to restore.
        cx.update(|_, cx| input.update(cx, |s, cx| s.set_selected_range(0..2, cx)));
        shrink(&input, &mut cx);
        cx.update(|_, cx| assert_eq!(input.read(cx).selected_range(), 0..2));

        // An edit clears it too.
        cx.update(|_, cx| input.update(cx, |s, cx| s.set_selected_range(4..4, cx)));
        expand(&input, &mut cx);
        cx.update(|window, cx| {
            input.update(cx, |s, cx| s.replace_text_in_range(None, "x", window, cx))
        });
        shrink(&input, &mut cx);
        assert_eq!(selected(&input, &mut cx), [""]);
    }

    #[gpui::test]
    fn a_cursor_at_the_end_of_a_word_takes_that_word_first(cx: &mut TestAppContext) {
        let (input, mut cx) = open(cx, "one two\nthree");
        cx.update(|_, cx| input.update(cx, |s, cx| s.set_selected_range(7..7, cx)));
        expand(&input, &mut cx);
        assert_eq!(selected(&input, &mut cx), ["two"]);
        expand(&input, &mut cx);
        assert_eq!(selected(&input, &mut cx), ["one two"]);
    }
}
