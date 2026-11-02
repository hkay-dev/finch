//! Edit > Re-indent (Xcode's Control-I): every line a selection touches takes
//! its indent from the syntax tree.
//!
//! A line inside brackets sits one indent deeper than the line that opened
//! them (brackets opened on one line count once, so `foo(a, {` is one level),
//! and a line that starts with closing brackets lines up with the line that
//! opened them. A line that continues a construct begun on an earlier line
//! inside the same brackets (a method chain, a `where` clause, JSX children)
//! keeps the depth it had under that line. Lines that start inside a string or
//! a block comment are left as they are.

use gpui::{Context, Window};
use ropey::Rope;

use super::line_ops::{LinePlan, merge_rows, real_rows, row_span, touched_rows};
use crate::input::{InputBaseState, InputModeKind, Reindent, RopeExt as _, SyntaxContext, TabSize};

/// A bracket of code: its byte offset and whether it opens.
type Bracket = (usize, bool);

/// A bracket open at the current line, or the file itself at the bottom.
struct Open {
    /// Where it opened; `None` for the file.
    offset: Option<usize>,
    row: usize,
    /// The indent of the line it opened on, and of lines inside it.
    outer: usize,
    inner: usize,
}

/// The width in columns of the spaces and tabs from `start`, and the offset of
/// the first other character (or `end`).
fn leading(rope: &Rope, start: usize, end: usize, tab: usize) -> (usize, usize) {
    let (mut width, mut at) = (0, start);
    for c in rope.slice(start..end).chars() {
        match c {
            ' ' => width += 1,
            '\t' => width += tab - width % tab,
            _ => break,
        }
        at += 1;
    }
    (width, at)
}

fn indent_for(width: usize, tab: TabSize) -> String {
    let size = tab.tab_size.max(1);
    if tab.hard_tabs {
        format!("{}{}", "\t".repeat(width / size), " ".repeat(width % size))
    } else {
        " ".repeat(width)
    }
}

/// Plan the re-indent of every row the selections touch. `brackets` are the
/// code brackets from the start of the text through the last touched row, in
/// order. `in_text(row start)` says whether a row starts inside a string or
/// comment that began on an earlier row. `anchor(first character, row start,
/// innermost open bracket)` is the start of the construct the row continues: a
/// syntax node holding the row's first character that starts on an earlier row,
/// after that bracket, below the file's root; `None` when the row starts a
/// construct of its own. `None` when nothing changes.
pub(super) fn plan_reindent(
    rope: &Rope,
    selections: &[(usize, usize)],
    tab: TabSize,
    brackets: &[Bracket],
    in_text: impl Fn(usize) -> bool,
    anchor: impl Fn(usize, usize, Option<usize>) -> Option<usize>,
) -> Option<LinePlan> {
    let size = tab.tab_size.max(1);
    let rows = real_rows(rope);
    let blocks = merge_rows(selections.iter().filter_map(|sel| touched_rows(rope, rows, sel)).collect(), false);
    let last = blocks.last()?.last;

    let mut stack = vec![Open {
        offset: None,
        row: usize::MAX,
        outer: 0,
        inner: 0,
    }];
    // The old indent of each row, then its new one once it's planned.
    let mut widths: Vec<usize> = Vec::with_capacity(last + 1);
    let mut next = 0;
    let mut block = 0;
    let mut edits: Vec<(std::ops::Range<usize>, String)> = Vec::new();
    for row in 0..=last {
        let span = row_span(rope, row);
        while blocks[block].last < row {
            block += 1;
        }
        let (old, content) = leading(rope, span.start, span.content_end, size);
        widths.push(old);
        let blank = content == span.content_end;

        if blocks[block].first <= row && !in_text(span.start) {
            // Closing brackets that start the line close before it.
            let mut closers = 0;
            let mut at = content;
            for c in rope.slice(content..span.content_end).chars() {
                match c {
                    ' ' | '\t' => {}
                    ')' | ']' | '}' if brackets.get(next + closers) == Some(&(at, false)) => closers += 1,
                    _ => break,
                }
                at += c.len_utf8();
            }
            let top = &stack[stack.len() - 1];
            let closed = closers.min(stack.len() - 1);
            let new = if blank {
                0
            } else if closed > 0 {
                stack[stack.len() - closed].outer
            } else {
                match anchor(content, span.start, top.offset) {
                    Some(start) => {
                        let from = rope.offset_to_point(start).row;
                        let (was, _) = leading(rope, row_span(rope, from).start, start, size);
                        widths[from] + old.saturating_sub(was)
                    }
                    None => top.inner,
                }
            };
            widths[row] = new;
            let new = indent_for(new, tab);
            if rope.slice(span.start..content) != new.as_str() {
                edits.push((span.start..content, new));
            }
        }

        while let Some(&(offset, opens)) = brackets.get(next) {
            if offset >= span.end {
                break;
            }
            next += 1;
            if opens {
                let top = &stack[stack.len() - 1];
                let (outer, inner) = if top.row == row {
                    (top.outer, top.inner)
                } else {
                    (widths[row], widths[row] + size)
                };
                stack.push(Open {
                    offset: Some(offset),
                    row,
                    outer,
                    inner,
                });
            } else if stack.len() > 1 {
                stack.pop();
            }
        }
    }
    if edits.is_empty() {
        return None;
    }

    // A position after an edit moves by its length change; one inside an old
    // indent keeps its column, up to the new indent's end. The start of a
    // selection stays before an indent inserted where it starts.
    let moved = |position: usize, left: bool| -> usize {
        let mut delta = 0isize;
        for (range, new) in &edits {
            if range.end < position || (range.end == position && !(left && range.is_empty())) {
                delta += new.len() as isize - range.len() as isize;
            } else if range.start < position {
                let start = (range.start as isize + delta) as usize;
                return start + (position - range.start).min(new.len());
            } else {
                break;
            }
        }
        (position as isize + delta) as usize
    };
    let selections = selections
        .iter()
        .map(|&(start, end)| (moved(start, start != end), moved(end, false)))
        .collect();
    Some(LinePlan { edits, selections })
}

/// The brackets in `depths` (offset and depth from the highlighter), each closer
/// kept only when it shares its depth with the last opener still open. That drops
/// the closer of a two-character opener the parse doesn't report, such as the `}`
/// of a `${` substitution. `None` when an offset isn't a bracket: a parse of
/// another text.
fn paired(depths: &[(usize, u32)], char_at: impl Fn(usize) -> Option<char>) -> Option<Vec<Bracket>> {
    let mut open: Vec<u32> = Vec::new();
    let mut brackets = Vec::with_capacity(depths.len());
    for &(offset, depth) in depths {
        match char_at(offset)? {
            '(' | '[' | '{' => {
                open.push(depth);
                brackets.push((offset, true));
            }
            ')' | ']' | '}' => {
                if open.last() == Some(&depth) {
                    open.pop();
                    brackets.push((offset, false));
                }
            }
            _ => return None,
        }
    }
    Some(brackets)
}

impl<M: InputModeKind> InputBaseState<M> {
    /// Re-indent the rows every selection touches as one undo step, from the
    /// highlighter's parse. Does nothing without a current parse, or in a
    /// language where indentation is syntax or text (the same ones paste leaves alone).
    pub(super) fn reindent(&mut self, _: &Reindent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_editable() || !self.paste_reindents() {
            return;
        }
        let ranges: Vec<(usize, usize)> = self.selections.iter().map(|sel| (sel.start, sel.end)).collect();
        let rows = real_rows(&self.text);
        let Some(end) = ranges
            .iter()
            .filter_map(|sel| touched_rows(&self.text, rows, sel))
            .map(|(_, last)| row_span(&self.text, last).end)
            .max()
        else {
            return;
        };
        let plan = {
            let Some(highlighter) = self.mode.highlighter() else {
                return;
            };
            let highlighter = highlighter.borrow();
            let Some(highlighter) = highlighter.as_ref() else {
                return;
            };
            // `None` when the tree doesn't describe this text.
            if highlighter.syntax_context_at(&self.text, 0).is_none() {
                return;
            }
            let mut depths = Vec::new();
            if !highlighter.bracket_depths(&(0..end), &mut depths) {
                return;
            }
            let Some(brackets) = paired(&depths, |offset| self.text.char_at(offset)) else {
                return;
            };
            let text = &self.text;
            let in_text = |start: usize| match highlighter.syntax_context_at(text, start) {
                // Open across the line break, not one that starts here.
                Some(SyntaxContext::String) => start > 0 && highlighter.syntax_context_at(text, start - 1) == Some(SyntaxContext::String),
                // A comment that begins at this row's first column starts here;
                // one that began on an earlier row holds it.
                Some(SyntaxContext::Comment) => highlighter
                    .syntax_ancestor(start..start)
                    .flatten()
                    .is_none_or(|node| node.start < start),
                _ => false,
            };
            let anchor = |content: usize, row_start: usize, open: Option<usize>| {
                let mut range = content..content + text.char_at(content).map_or(1, char::len_utf8);
                loop {
                    let node = highlighter.syntax_ancestor(range).flatten()?;
                    if open.is_some_and(|open| node.start <= open) {
                        return None;
                    }
                    if node.start < row_start {
                        // The file's root holds every line without one continuing another.
                        return highlighter.syntax_ancestor(node.clone()).flatten().map(|_| node.start);
                    }
                    range = node;
                }
            };
            plan_reindent(text, &ranges, self.mode.tab_size(), &brackets, in_text, anchor)
        };
        self.transform_selection(move |_, _| plan, window, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOFT4: TabSize = TabSize {
        tab_size: 4,
        hard_tabs: false,
    };

    /// Every bracket character is code: the tests use no strings or comments.
    /// A row starting with `.` continues the row above.
    fn reindented(text: &str, selections: &[(usize, usize)], tab: TabSize) -> Option<(String, Vec<(usize, usize)>)> {
        let brackets: Vec<Bracket> = text
            .char_indices()
            .filter_map(|(offset, c)| match c {
                '(' | '[' | '{' => Some((offset, true)),
                ')' | ']' | '}' => Some((offset, false)),
                _ => None,
            })
            .collect();
        let anchor = |content: usize, row_start: usize, _| {
            text[content..].starts_with('.').then(|| {
                let above = text[..row_start - 1].rfind('\n').map_or(0, |at| at + 1);
                above + text[above..].len() - text[above..].trim_start_matches([' ', '\t']).len()
            })
        };
        let plan = plan_reindent(&Rope::from(text), selections, tab, &brackets, |_| false, anchor)?;
        let mut out = text.to_string();
        for (range, new) in plan.edits.iter().rev() {
            out.replace_range(range.clone(), new);
        }
        Some((out, plan.selections))
    }

    fn all(text: &str) -> String {
        reindented(text, &[(0, text.len())], SOFT4).map_or_else(|| text.to_string(), |(out, _)| out)
    }

    #[test]
    fn flush_left_code_takes_its_nesting() {
        assert_eq!(
            all("fn a() {\nif x {\ny();\n} else {\nz();\n}\n}\n"),
            "fn a() {\n    if x {\n        y();\n    } else {\n        z();\n    }\n}\n"
        );
    }

    #[test]
    fn brackets_on_one_row_open_one_level() {
        assert_eq!(all("foo(a, {\nx\n})\n"), "foo(a, {\n    x\n})\n");
        assert_eq!(all("foo(\n      a,\n  b,\n)\n"), "foo(\n    a,\n    b,\n)\n");
    }

    #[test]
    fn a_continued_line_keeps_its_depth_under_its_start() {
        // The block's base moves from 8 to 4; the chain keeps its extra 4.
        assert_eq!(
            all("fn a() {\n        let v = xs\n            .iter();\n        let w = 2;\n}"),
            "fn a() {\n    let v = xs\n        .iter();\n    let w = 2;\n}"
        );
        // A statement deeper than the one above it is not a continuation.
        assert_eq!(all("fn a() {\nlet s = 1;\n        let v = 2;\n}"), "fn a() {\n    let s = 1;\n    let v = 2;\n}");
        // Brackets opened on a continued line nest under it.
        assert_eq!(all("{\nxs\n    .map(|x| {\nx\n})\n}"), "{\n    xs\n        .map(|x| {\n            x\n        })\n}");
    }

    #[test]
    fn lines_inside_an_unselected_block_follow_its_opening_line() {
        let text = "  if x {\na\n  }";
        let (out, _) = reindented(text, &[(9, 9)], SOFT4).unwrap();
        assert_eq!(out, "  if x {\n      a\n  }");
    }

    #[test]
    fn blank_lines_empty_and_strings_stay() {
        let text = "{\n  \na\n}";
        assert_eq!(all(text), "{\n\n    a\n}");
        // A row that starts inside a string keeps its whitespace.
        let brackets = [(0, true), (7, false)];
        let plan = plan_reindent(&Rope::from(text), &[(0, text.len())], SOFT4, &brackets, |start| start == 2, |_, _, _| None);
        assert_eq!(plan.unwrap().edits, vec![(5..5, "    ".to_string())]);
    }

    #[test]
    fn hard_tabs_and_a_caret_line() {
        let text = "{\n  a\n  b\n}";
        let hard = TabSize {
            tab_size: 4,
            hard_tabs: true,
        };
        let (out, selections) = reindented(text, &[(5, 5)], hard).unwrap();
        assert_eq!(out, "{\n\ta\n  b\n}");
        // The caret after `a` stays after it.
        assert_eq!(selections, vec![(4, 4)]);
    }

    #[test]
    fn a_whole_line_selection_keeps_covering_its_lines() {
        let text = "{\na\nb\n}\n";
        let (out, selections) = reindented(text, &[(2, 6)], SOFT4).unwrap();
        assert_eq!(out, "{\n    a\n    b\n}\n");
        assert_eq!(selections, vec![(2, 14)]);
    }

    #[test]
    fn a_substitution_closer_does_not_close_the_block() {
        // `{ `${x}` }`: the parse reports no `${`, only its `}` one level in.
        let text = "{ `${x}` }";
        let depths = [(0, 0), (6, 1), (9, 0)];
        let brackets = paired(&depths, |offset| text[offset..].chars().next()).unwrap();
        assert_eq!(brackets, vec![(0, true), (9, false)]);
        assert_eq!(paired(&[(2, 0)], |offset| text[offset..].chars().next()), None);
    }
}
