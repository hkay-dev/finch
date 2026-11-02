//! Edit > Markdown: Bold, Italic, Strikethrough, Inline code and Link on the editor's selection.
//!
//! `plan` is the whole rule set over plain text; `apply_markdown_format` runs one plan on an
//! editor as a single undo step. A selection is wrapped (trimmed of edge spaces), a caret in a
//! word wraps that word, and a caret elsewhere gets the empty pair with the caret inside. A
//! span already wrapped, from outside or with the marks in the selection, is unwrapped.
//! Link wraps as `[text]()` with the caret in the parentheses and never reads the clipboard.

use std::ops::Range;

use gpui::{Context, Window};
use unicode_segmentation::UnicodeSegmentation as _;

use crate::input::{EditorState, cursor::CursorSelection};

/// How far either side of the selection the editor's text is read to find the word and marks.
const REACH: usize = 4096;

/// One Markdown command of Edit > Markdown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkdownFormat {
    Bold,
    Italic,
    Code,
    Strike,
    Link,
}

/// One replacement and where the selection goes after it (offsets in the new text).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FormatEdit {
    pub(crate) range: Range<usize>,
    pub(crate) text: String,
    pub(crate) select: Range<usize>,
}

/// Runs `format` on every selection of the editor as one undo step.
pub fn apply_markdown_format(state: &mut EditorState, format: MarkdownFormat, window: &mut Window, cx: &mut Context<EditorState>) {
    if !state.is_editable() {
        return;
    }
    // A refused edit would leave the shifted selections past the end of the text.
    if state.edit_blocked(window, cx) {
        return;
    }
    state.selections.merge_overlapping();
    let before: Vec<CursorSelection> = state.selections.iter().copied().collect();
    let mut sorted = before.clone();
    sorted.sort_by_key(|sel| sel.start);
    let len = state.text().len();
    let mut edits: Vec<(Range<usize>, String)> = Vec::with_capacity(sorted.len());
    let mut after: Vec<CursorSelection> = Vec::with_capacity(sorted.len());
    let (mut shift, mut last_end) = (0isize, 0usize);
    for sel in sorted {
        let mut lo = sel.start.saturating_sub(REACH);
        while !state.text().is_char_boundary(lo) {
            lo += 1;
        }
        let mut hi = (sel.end + REACH).min(len);
        while !state.text().is_char_boundary(hi) {
            hi -= 1;
        }
        let near = state.text().slice(lo..hi).to_string();
        let edit = plan(&near, sel.start - lo..sel.end - lo, format);
        let range = edit.range.start + lo..edit.range.end + lo;
        // Two carets in one word make one edit.
        if range.start < last_end {
            continue;
        }
        let mut next = sel;
        next.start = (edit.select.start + lo).saturating_add_signed(shift);
        next.end = (edit.select.end + lo).saturating_add_signed(shift);
        next.column_anchor = None;
        shift += edit.text.len() as isize - range.len() as isize;
        last_end = range.end;
        edits.push((range, edit.text));
        after.push(next);
    }
    state.pause_blink_cursor(cx);
    state.undo_manager.begin_transaction();
    state.undo_manager.record_selections(before.clone(), before.clone());
    state.replace_text_in_ranges(&edits, window, cx);
    state.selections.replace_all(after);
    let now = state.selections.iter().copied().collect();
    state.undo_manager.record_selections(before, now);
    state.undo_manager.commit_transaction();
    cx.notify();
}

/// The edit `format` makes on `text` for the selection `selection`.
pub(crate) fn plan(text: &str, selection: Range<usize>, format: MarkdownFormat) -> FormatEdit {
    let (mut start, mut end) = (selection.start, selection.end);
    if start == end {
        // A caret in a word takes the word, a grapheme at a time so a combining mark stays with its letter.
        let is_word = |g: &str| g.chars().next().is_some_and(char::is_alphanumeric);
        for g in text[..start].graphemes(true).rev() {
            if !is_word(g) {
                break;
            }
            start -= g.len();
        }
        for g in text[end..].graphemes(true) {
            if !is_word(g) {
                break;
            }
            end += g.len();
        }
        if start == end {
            (start, end) = (selection.start, selection.end);
        }
    } else {
        // Marks hug the words: the spaces at the edges stay outside.
        while let Some(c) = text[start..end].chars().next().filter(|c| c.is_whitespace()) {
            start += c.len_utf8();
        }
        while let Some(c) = text[start..end].chars().next_back().filter(|c| c.is_whitespace()) {
            end -= c.len_utf8();
        }
        if start == end {
            (start, end) = (selection.end, selection.end);
        }
    }

    if let Some((outer, inner)) = wrapped(text, start, end, format) {
        let kept = text[inner.clone()].to_string();
        let select = outer.start..outer.start + kept.len();
        return FormatEdit { range: outer, text: kept, select };
    }
    let inner = &text[start..end];
    match format {
        MarkdownFormat::Link => {
            let caret = if inner.is_empty() { start + 1 } else { start + inner.len() + 3 };
            FormatEdit { range: start..end, text: format!("[{inner}]()"), select: caret..caret }
        }
        MarkdownFormat::Code => {
            // One more backtick than the longest run inside, and a space where the text touches one.
            let longest = inner.split(|c| c != '`').map(str::len).max().unwrap_or(0);
            let fence = "`".repeat(longest + 1);
            let pad = if inner.starts_with('`') || inner.ends_with('`') { " " } else { "" };
            let from = start + fence.len() + pad.len();
            FormatEdit { range: start..end, text: format!("{fence}{pad}{inner}{pad}{fence}"), select: from..from + inner.len() }
        }
        _ => {
            let mark = mark(format);
            FormatEdit {
                range: start..end,
                text: format!("{mark}{inner}{mark}"),
                select: start + mark.len()..start + mark.len() + inner.len(),
            }
        }
    }
}

fn mark(format: MarkdownFormat) -> &'static str {
    match format {
        MarkdownFormat::Bold => "**",
        MarkdownFormat::Strike => "~~",
        MarkdownFormat::Italic => "*",
        MarkdownFormat::Code => "`",
        MarkdownFormat::Link => "",
    }
}

/// When `start..end` already carries `format`: the range to replace and the range of `text` to keep.
fn wrapped(text: &str, start: usize, end: usize, format: MarkdownFormat) -> Option<(Range<usize>, Range<usize>)> {
    let span = &text[start..end];
    match format {
        MarkdownFormat::Link => {
            if text[..start].ends_with('[') && text[end..].starts_with("](") {
                let close = end + text[end..].find(')')?;
                return Some((start - 1..close + 1, start..end));
            }
            // The caret in the parentheses `Link` just left it in: the link comes off, keeping its text.
            if start == end && text[..start].ends_with("](") && text[end..].starts_with(')') {
                let open = text[..start - 2].rfind('[').filter(|&open| !text[open..start].contains('\n'))?;
                return Some((open..end + 1, open + 1..start - 2));
            }
            if span.starts_with('[') && span.ends_with(')') {
                let split = span.rfind("](")?;
                return Some((start..end, start + 1..start + split));
            }
            None
        }
        MarkdownFormat::Code => code_span(text, start, end),
        MarkdownFormat::Strike => {
            let left = text[..start].chars().rev().take_while(|&c| c == '~').count();
            let right = text[end..].chars().take_while(|&c| c == '~').count();
            if left.min(right) >= 2 {
                return Some((start - 2..end + 2, start..end));
            }
            let lead = span.chars().take_while(|&c| c == '~').count();
            let trail = span.chars().rev().take_while(|&c| c == '~').count();
            (lead.min(trail) >= 2 && span.len() > 4).then_some((start..end, start + 2..end - 2))
        }
        MarkdownFormat::Bold | MarkdownFormat::Italic => {
            // Bold is a run of two or more marks each side; italic is an odd run (`***x***` is both).
            let drop = |run: usize| match format {
                MarkdownFormat::Bold if run >= 2 => 2,
                MarkdownFormat::Italic if run % 2 == 1 => 1,
                _ => 0,
            };
            for mark in ['*', '_'] {
                let left = text[..start].chars().rev().take_while(|&c| c == mark).count();
                let right = text[end..].chars().take_while(|&c| c == mark).count();
                // `foo_bar_baz` is a name, not emphasis: underscores inside a word never come off.
                let glued = mark == '_'
                    && (text[..start - left].chars().next_back().is_some_and(char::is_alphanumeric)
                        || text[end + right..].chars().next().is_some_and(char::is_alphanumeric));
                let k = if glued { 0 } else { drop(left.min(right)) };
                if k > 0 {
                    return Some((start - k..end + k, start..end));
                }
                let lead = span.chars().take_while(|&c| c == mark).count();
                let trail = span.chars().rev().take_while(|&c| c == mark).count();
                let k = drop(lead.min(trail));
                if k > 0 && span.len() > 2 * k {
                    return Some((start..end, start + k..end - k));
                }
            }
            None
        }
    }
}

/// The backtick span (CommonMark: an opening run closed by a run of the same length, on one line)
/// that holds `start..end`: the range to replace and the text to keep, less the one space of padding.
fn code_span(text: &str, start: usize, end: usize) -> Option<(Range<usize>, Range<usize>)> {
    // The empty pair `` ` ` `` has no closing run of its own length.
    if start == end && text[..start].ends_with('`') && text[end..].starts_with('`') {
        let lone = !text[..start - 1].ends_with('`') && !text[end + 1..].starts_with('`');
        if lone {
            return Some((start - 1..end + 1, start..start));
        }
    }
    let first = text[..start].rfind('\n').map_or(0, |at| at + 1);
    let last = end + text[end..].find('\n').unwrap_or(text.len() - end);
    let bytes = &text.as_bytes()[first..last];
    let run = |at: usize| bytes[at..].iter().take_while(|&&b| b == b'`').count();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'`' {
            i += 1;
            continue;
        }
        let n = run(i);
        let mut j = i + n;
        let mut close = None;
        while j < bytes.len() {
            if bytes[j] == b'`' {
                let m = run(j);
                if m == n {
                    close = Some(j);
                    break;
                }
                j += m;
            } else {
                j += 1;
            }
        }
        let Some(close) = close else {
            i += n;
            continue;
        };
        let outer = first + i..first + close + n;
        let inside = outer.start <= start && end <= outer.end && !(start == end && (start == outer.start || start == outer.end));
        if inside {
            let content = &text[outer.start + n..outer.end - n];
            let padded = content.len() >= 2 && content.starts_with(' ') && content.ends_with(' ') && !content.trim().is_empty();
            let pad = usize::from(padded);
            return Some((outer.clone(), outer.start + n + pad..outer.end - n - pad));
        }
        i = close + n;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::MarkdownFormat::*;
    use super::*;

    /// Applies `format` to `text` with `|` marking the caret or `<` and `>` the selection;
    /// returns the result in the same notation.
    fn run(marked: &str, format: MarkdownFormat) -> String {
        let (text, selection) = match (marked.find('<'), marked.find('>')) {
            (Some(a), Some(b)) => (marked.replace(['<', '>'], ""), a..b - 1),
            _ => {
                let at = marked.find('|').unwrap();
                (marked.replace('|', ""), at..at)
            }
        };
        let edit = plan(&text, selection, format);
        let mut out = text;
        out.replace_range(edit.range, &edit.text);
        if edit.select.is_empty() {
            out.insert(edit.select.start, '|');
        } else {
            out.insert(edit.select.end, '>');
            out.insert(edit.select.start, '<');
        }
        out
    }

    #[test]
    fn a_selection_wraps_and_stays_selected() {
        assert_eq!(run("a <word> b", Bold), "a **<word>** b");
        assert_eq!(run("a <word> b", Italic), "a *<word>* b");
        assert_eq!(run("a <word> b", Code), "a `<word>` b");
        assert_eq!(run("a <two words> b", Bold), "a **<two words>** b");
    }

    #[test]
    fn edge_spaces_stay_outside_the_marks() {
        assert_eq!(run("a< word > b", Bold), "a **<word>**  b");
    }

    #[test]
    fn a_caret_in_a_word_wraps_the_word() {
        assert_eq!(run("a wo|rd b", Italic), "a *<word>* b");
        assert_eq!(run("a word| b", Bold), "a **<word>** b");
        assert_eq!(run("caf\u{e9}|", Code), "`<caf\u{e9}>`");
    }

    #[test]
    fn a_caret_outside_a_word_gets_the_empty_pair() {
        assert_eq!(run("a | b", Bold), "a **|** b");
        assert_eq!(run("a | b", Italic), "a *|* b");
        assert_eq!(run("a | b", Code), "a `|` b");
        assert_eq!(run("a | b", Link), "a [|]() b");
        assert_eq!(run("a <  > b", Bold), "a   **|** b");
    }

    #[test]
    fn a_wrapped_span_unwraps_from_outside() {
        assert_eq!(run("**<word>**", Bold), "<word>");
        assert_eq!(run("a **wo|rd** b", Bold), "a <word> b");
        assert_eq!(run("*<word>*", Italic), "<word>");
        assert_eq!(run("_<word>_", Italic), "<word>");
        assert_eq!(run("`<word>`", Code), "<word>");
    }

    #[test]
    fn a_selection_that_holds_the_marks_unwraps() {
        assert_eq!(run("<**word**>", Bold), "<word>");
        assert_eq!(run("<*word*>", Italic), "<word>");
        assert_eq!(run("<`word`>", Code), "<word>");
    }

    #[test]
    fn strikethrough_wraps_a_selection_or_the_word_at_the_caret() {
        assert_eq!(run("a <word> b", Strike), "a ~~<word>~~ b");
        assert_eq!(run("a <two words> b", Strike), "a ~~<two words>~~ b");
        assert_eq!(run("a wo|rd b", Strike), "a ~~<word>~~ b");
        assert_eq!(run("a | b", Strike), "a ~~|~~ b");
        assert_eq!(run("a< word > b", Strike), "a ~~<word>~~  b");
    }

    #[test]
    fn strikethrough_toggles_off_like_bold() {
        assert_eq!(run("~~<word>~~", Strike), "<word>");
        assert_eq!(run("a ~~wo|rd~~ b", Strike), "a <word> b");
        assert_eq!(run("<~~word~~>", Strike), "<word>");
        assert_eq!(run("~~|~~", Strike), "|");
    }

    #[test]
    fn strikethrough_stacks_with_the_other_marks() {
        assert_eq!(run("**<word>**", Strike), "**~~<word>~~**");
        assert_eq!(run("~~<word>~~", Bold), "~~**<word>**~~");
        assert_eq!(run("**~~<word>~~**", Strike), "**<word>**");
    }

    #[test]
    fn an_empty_pair_unwraps() {
        assert_eq!(run("**|**", Bold), "|");
        assert_eq!(run("*|*", Italic), "|");
        assert_eq!(run("`|`", Code), "|");
        assert_eq!(run("[|]()", Link), "|");
    }

    #[test]
    fn bold_and_italic_stack_and_come_off_one_at_a_time() {
        assert_eq!(run("**<word>**", Italic), "***<word>***");
        assert_eq!(run("*<word>*", Bold), "***<word>***");
        assert_eq!(run("***<word>***", Bold), "*<word>*");
        assert_eq!(run("***<word>***", Italic), "**<word>**");
    }

    #[test]
    fn a_link_puts_the_caret_in_the_parentheses() {
        assert_eq!(run("a <word> b", Link), "a [word](|) b");
        assert_eq!(run("a wo|rd b", Link), "a [word](|) b");
    }

    #[test]
    fn a_link_unwraps_and_drops_its_address() {
        assert_eq!(run("[<word>](https://a.b)", Link), "<word>");
        assert_eq!(run("[word](|)", Link), "<word>");
        assert_eq!(run("<[word](https://a.b)>", Link), "<word>");
        assert_eq!(run("x [wo|rd](u) y", Link), "x <word> y");
    }

    #[test]
    fn a_word_next_to_other_text_keeps_its_neighbours() {
        assert_eq!(run("# T<itle>", Bold), "# T**<itle>**");
        assert_eq!(run("- it|em\n", Code), "- `<item>`\n");
    }

    #[test]
    fn a_combining_mark_stays_in_the_word() {
        assert_eq!(run("ca|fe\u{301} x", Italic), "*<cafe\u{301}>* x");
        assert_eq!(run("x \u{915}\u{94d}\u{937}|", Bold), "x **<\u{915}\u{94d}\u{937}>**");
    }

    #[test]
    fn underscores_inside_a_name_are_not_emphasis() {
        assert_eq!(run("foo_ba|r_baz", Italic), "foo_*<bar>*_baz");
        assert_eq!(run("_wo|rd_", Italic), "<word>");
        assert_eq!(run("a _wo|rd_ b", Italic), "a <word> b");
    }

    #[test]
    fn code_around_a_backtick_takes_a_longer_fence() {
        assert_eq!(run("a <x`y> b", Code), "a ``<x`y>`` b");
        assert_eq!(run("a <`x> b", Code), "a `` <`x> `` b");
        assert_eq!(run("a <``> b", Code), "a ``` <``> ``` b");
    }

    #[test]
    fn a_longer_fence_comes_off() {
        assert_eq!(run("``<a`b>``", Code), "<a`b>");
        assert_eq!(run("a `` `|x `` b", Code), "a <`x> b");
        assert_eq!(run("a ``x`|y`` b", Code), "a <x`y> b");
        assert_eq!(run("`a` b `c|d`", Code), "`a` b <cd>");
    }
}
