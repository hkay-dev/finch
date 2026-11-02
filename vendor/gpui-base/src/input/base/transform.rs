//! Edit > Transformations: make upper case, make lower case, capitalize, the
//! Convert case styles (Title, snake, camel, Pascal, kebab), sort lines, hard
//! wrap and the other line rewrites.
//!
//! Like `line_ops`, the planning functions work on the rope and plain offsets
//! so they are tested without a window, and `InputBaseState` applies a plan as
//! one undo step with every selection kept over the text it changed.

use std::ops::Range;

use unicode_properties::{GeneralCategoryGroup, UnicodeGeneralCategory};

use gpui::{Context, Window};
use ropey::Rope;

use super::line_ops::{LinePlan, merge_rows, real_rows, row_span, touched_rows};
use super::mode::LayoutMode;
use sum_tree::Bias;
use crate::input::{
    Capitalize, ConvertIndentationToSpaces, ConvertIndentationToTabs, ConvertToCamelCase, ConvertToKebabCase, ConvertToPascalCase,
    ConvertToSnakeCase, ConvertToTitleCase, Decrement, DecrementByTen, HardWrap, Increment, IncrementByTen, InputBaseState, InputModeKind,
    MakeLowerCase, MakeUpperCase, RemoveBlankLines, RemoveDuplicateLines, RemoveSurrounding, ReverseLines, RopeExt as _, SortLines,
    SortLinesDescending, SortLinesIgnoringCase, SurroundWithBackticks, SurroundWithBraces, SurroundWithBrackets, SurroundWithDoubleQuotes,
    SurroundWithParentheses, SurroundWithSingleQuotes, TrimTrailingWhitespace,
};

#[derive(Clone, Copy)]
pub(super) enum CaseKind {
    Upper,
    Lower,
    Capitalize,
    Title,
    Snake,
    Camel,
    Pascal,
    Kebab,
}

/// A character that belongs inside a word. An apostrophe joins two letters, so
/// "don't" is one word.
fn in_word(chars: &[char], index: usize) -> bool {
    let c = chars[index];
    if c.is_alphanumeric() || c == '_' {
        return true;
    }
    matches!(c, '\'' | '\u{2019}')
        && index > 0
        && chars[index - 1].is_alphanumeric()
        && chars.get(index + 1).is_some_and(|next| next.is_alphanumeric())
}

/// One piece of a line: a word (letters and digits, apostrophes inside it
/// kept) or the text between words.
enum Token {
    Word(String),
    Other(String),
}

/// Split a line into words and the text between them. A word ends at any
/// character that isn't a letter or digit, and at a case boundary: `fooBar`
/// is `foo` and `Bar`, `HTTPServer` is `HTTP` and `Server`, `v2Beta` is `v2`
/// and `Beta`. A single `_` or `-` between two words is a separator and
/// leaves no token, so `foo_bar` and `fooBar` give the same two words.
fn tokenize(line: &str) -> Vec<Token> {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens: Vec<Token> = Vec::new();
    let mut word = String::new();
    let mut other = String::new();
    let mut previous: Option<char> = None;
    for (index, &c) in chars.iter().enumerate() {
        // A combining mark belongs to the letter before it.
        if !word.is_empty() && c.general_category_group() == GeneralCategoryGroup::Mark {
            word.push(c);
            continue;
        }
        let joins = c.is_alphanumeric() || (is_apostrophe(c) && !word.is_empty() && in_word(&chars, index));
        if !joins {
            if !word.is_empty() {
                tokens.push(Token::Word(std::mem::take(&mut word)));
            }
            let between_words = matches!(c, '_' | '-')
                && matches!(tokens.last(), Some(Token::Word(_)))
                && other.is_empty()
                && chars.get(index + 1).is_some_and(|next| next.is_alphanumeric());
            if !between_words {
                other.push(c);
            }
            previous = None;
            continue;
        }
        if !other.is_empty() {
            tokens.push(Token::Other(std::mem::take(&mut other)));
        }
        if let Some(before) = previous.filter(|_| c.is_uppercase()) {
            let after_lower = before.is_lowercase() || before.is_numeric();
            let acronym_end = before.is_uppercase() && chars.get(index + 1).is_some_and(|next| next.is_lowercase());
            if (after_lower || acronym_end) && !word.is_empty() {
                tokens.push(Token::Word(std::mem::take(&mut word)));
            }
        }
        word.push(c);
        previous = (!is_apostrophe(c)).then_some(c);
    }
    if !word.is_empty() {
        tokens.push(Token::Word(word));
    }
    if !other.is_empty() {
        tokens.push(Token::Other(other));
    }
    tokens
}

/// The words of a line, apostrophes dropped, in lower case.
fn lower_words(line: &str) -> Vec<String> {
    tokenize(line)
        .into_iter()
        .filter_map(|token| match token {
            Token::Word(word) => Some(word.chars().filter(|&c| !is_apostrophe(c)).collect::<String>().to_lowercase()),
            Token::Other(_) => None,
        })
        .collect()
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    let mut out = String::with_capacity(word.len());
    if let Some(first) = chars.next() {
        out.extend(first.to_uppercase());
    }
    out.push_str(&chars.as_str().to_lowercase());
    out
}

/// Words a title keeps in lower case unless they come first or last.
const SMALL_WORDS: [&str; 17] = ["a", "an", "and", "as", "at", "but", "by", "for", "in", "nor", "of", "on", "or", "the", "to", "up", "vs"];

/// Title Case for one line: every word capitalized except the small words in
/// the middle, `_`, `-` and case boundaries between words become spaces, and
/// all other text stays.
fn title_line(line: &str) -> String {
    let tokens = tokenize(line);
    let count = tokens.iter().filter(|token| matches!(token, Token::Word(_))).count();
    let mut out = String::with_capacity(line.len() + 4);
    let mut seen = 0;
    let mut after_word = false;
    for token in &tokens {
        match token {
            Token::Word(word) => {
                if after_word {
                    out.push(' ');
                }
                let lower = word.to_lowercase();
                if seen > 0 && seen + 1 < count && SMALL_WORDS.contains(&lower.as_str()) {
                    out.push_str(&lower);
                } else {
                    out.push_str(&capitalized(word));
                }
                seen += 1;
                after_word = true;
            }
            Token::Other(text) => {
                out.push_str(text);
                after_word = false;
            }
        }
    }
    out
}

/// snake, camel, Pascal or kebab case for one line. The indentation, trailing
/// spaces and any `_` at either end stay (`__init__` stays `__init__`); a line
/// with no words stays as it is.
fn identifier_line(line: &str, kind: CaseKind) -> String {
    let core = line.trim_matches(|c: char| c.is_whitespace());
    let lead_ws = &line[..line.len() - line.trim_start().len()];
    let trail_ws = &line[line.trim_end().len()..];
    let lead = core.len() - core.trim_start_matches('_').len();
    let trail = core.len() - core.trim_end_matches('_').len();
    let (lead_marks, trail_marks) = if matches!(kind, CaseKind::Kebab) || lead == core.len() { ("", "") } else { (&core[..lead], &core[core.len() - trail..]) };
    let inner = &core[lead_marks.len()..core.len() - trail_marks.len()];
    let words = lower_words(inner);
    if words.is_empty() {
        return line.to_owned();
    }
    let body = match kind {
        CaseKind::Snake => words.join("_"),
        CaseKind::Kebab => words.join("-"),
        CaseKind::Pascal => words.iter().map(|word| capitalized(word)).collect(),
        _ => words.iter().enumerate().map(|(index, word)| if index == 0 { word.clone() } else { capitalized(word) }).collect(),
    };
    format!("{lead_ws}{lead_marks}{body}{trail_marks}{trail_ws}")
}

/// Apply `style` to each line of `text`, keeping the line endings.
fn per_line(text: &str, style: impl Fn(&str) -> String) -> String {
    let mut out = String::with_capacity(text.len());
    for (index, line) in text.split('\n').enumerate() {
        if index > 0 {
            out.push('\n');
        }
        match line.strip_suffix('\r') {
            Some(line) => {
                out.push_str(&style(line));
                out.push('\r');
            }
            None => out.push_str(&style(line)),
        }
    }
    out
}

fn convert(text: &str, kind: CaseKind) -> String {
    match kind {
        CaseKind::Upper => text.to_uppercase(),
        CaseKind::Lower => text.to_lowercase(),
        CaseKind::Title => per_line(text, title_line),
        CaseKind::Snake | CaseKind::Camel | CaseKind::Pascal | CaseKind::Kebab => per_line(text, |line| identifier_line(line, kind)),
        CaseKind::Capitalize => {
            let chars: Vec<char> = text.chars().collect();
            let mut out = String::with_capacity(text.len());
            let mut word_start = true;
            for (index, &c) in chars.iter().enumerate() {
                if !in_word(&chars, index) {
                    word_start = true;
                    out.push(c);
                } else if word_start {
                    word_start = false;
                    out.extend(c.to_uppercase());
                } else {
                    out.extend(c.to_lowercase());
                }
            }
            out
        }
    }
}

fn is_apostrophe(c: char) -> bool {
    matches!(c, '\'' | '\u{2019}')
}

/// Widen a word across apostrophes with a letter or digit on both sides, so
/// the caret's target is the word `convert` sees.
fn widen(rope: &Rope, mut word: Range<usize>) -> Range<usize> {
    loop {
        let mut grown = false;
        if let Some(apostrophe) = rope.chars_at(word.start).prev().filter(|&c| is_apostrophe(c)) {
            let before = word.start - apostrophe.len_utf8();
            if rope.chars_at(before).prev().is_some_and(|c| c.is_alphanumeric()) {
                if let Some(left) = rope.word_range(before) {
                    word.start = left.start;
                    grown = true;
                }
            }
        }
        if let Some(apostrophe) = rope.char_at(word.end).filter(|&c| is_apostrophe(c)) {
            let after = word.end + apostrophe.len_utf8();
            if rope.char_at(after).is_some_and(|c| c.is_alphanumeric()) {
                if let Some(right) = rope.word_range(after) {
                    word.end = right.end;
                    grown = true;
                }
            }
        }
        if !grown {
            return word;
        }
    }
}

/// The range a selection transforms: the selection, or the word at the caret.
fn target(rope: &Rope, &(start, end): &(usize, usize)) -> Option<Range<usize>> {
    if start != end {
        return Some(start..end);
    }
    if let Some(word) = rope.word_range(start) {
        return Some(widen(rope, word));
    }
    // A caret at the very end of the text sits just after its word.
    let before = rope.chars_at(start).prev()?;
    let word = rope.word_range(start - before.len_utf8())?;
    (word.end == start).then(|| widen(rope, word))
}

/// Change the case of every selection, or of the word at an empty caret.
/// Returns `None` when no text changes. Selections stay over the new text; a
/// caret keeps its place in its word.
pub(super) fn plan_case(rope: &Rope, selections: &[(usize, usize)], kind: CaseKind) -> Option<LinePlan> {
    let targets: Vec<Option<Range<usize>>> = selections.iter().map(|sel| target(rope, sel)).collect();
    let mut order: Vec<usize> = (0..selections.len()).filter(|&index| targets[index].is_some()).collect();
    order.sort_by_key(|&index| targets[index].as_ref().map(|range| (range.start, range.end)));

    // Each edit keeps its original text so a caret can find its place.
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut originals: Vec<String> = Vec::new();
    let mut edit_of: Vec<Option<usize>> = vec![None; selections.len()];
    for index in order {
        let range = targets[index].clone().expect("filtered to targets");
        if let Some((last, _)) = edits.last() {
            if *last == range {
                edit_of[index] = Some(edits.len() - 1);
                continue;
            }
            if last.end > range.start {
                continue;
            }
        }
        let original = rope.slice(range.clone()).to_string();
        let converted = convert(&original, kind);
        if converted == original {
            continue;
        }
        edit_of[index] = Some(edits.len());
        edits.push((range, converted));
        originals.push(original);
    }
    if edits.is_empty() {
        return None;
    }

    // Offset change from the edits that end at or before a position.
    let shift = |position: usize| -> isize {
        edits
            .iter()
            .filter(|(range, _)| range.end <= position)
            .map(|(range, text)| text.len() as isize - range.len() as isize)
            .sum()
    };
    let resulting = selections
        .iter()
        .enumerate()
        .map(|(index, &(start, end))| {
            let Some(edit) = edit_of[index] else {
                // A position inside another cursor's changed word moves to the
                // end of its new text; any other moves with the edits before it.
                let moved = |position: usize| {
                    if let Some((range, text)) = edits.iter().find(|(range, _)| range.start < position && position < range.end) {
                        return (range.start as isize + shift(range.start)) as usize + text.len();
                    }
                    (position as isize + shift(position)) as usize
                };
                return (moved(start), moved(end));
            };
            let (range, text) = &edits[edit];
            let new_start = (range.start as isize + shift(range.start)) as usize;
            if start != end {
                return (new_start, new_start + text.len());
            }
            let prefix = &originals[edit][..start - range.start];
            let mut at = convert(prefix, kind).len().min(text.len());
            while !text.is_char_boundary(at) {
                at -= 1;
            }
            let caret = new_start + at;
            (caret, caret)
        })
        .collect();
    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

/// Put `open` before and `close` after every selection, keeping the inner text
/// selected. A caret gets an empty pair with the caret between. Selections
/// are taken in text order, so each edit's shift is the sum of the pairs
/// before it.
pub(super) fn plan_surround(rope: &Rope, selections: &[(usize, usize)], open: &str, close: &str) -> Option<LinePlan> {
    let mut order: Vec<usize> = (0..selections.len()).collect();
    order.sort_by_key(|&index| selections[index]);
    let mut edits: Vec<(Range<usize>, String)> = Vec::with_capacity(selections.len());
    let mut resulting = vec![(0, 0); selections.len()];
    let mut shift = 0;
    for index in order {
        let (start, end) = selections[index];
        let inner = rope.slice(start..end).to_string();
        edits.push((start..end, format!("{open}{inner}{close}")));
        let new_start = start + shift + open.len();
        resulting[index] = (new_start, new_start + inner.len());
        shift += open.len() + close.len();
    }
    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

/// The nearest pair of one kind of quote around `start..end` on its line.
/// Quotes on the line pair up in order; one behind a backslash, and an
/// apostrophe between two letters, are not quotes. A selection that leaves
/// its line has none.
fn enclosing_quotes(rope: &Rope, start: usize, end: usize) -> Option<(usize, usize)> {
    let span = row_span(rope, rope.offset_to_point(start).row);
    if end > span.content_end {
        return None;
    }
    let line = rope.slice(span.start..span.content_end).to_string();
    let bytes = line.as_bytes();
    let (start, end) = (start - span.start, end - span.start);
    let mut best: Option<(usize, usize)> = None;
    for quote in [b'"', b'\'', b'`'] {
        let mut marks: Vec<usize> = Vec::new();
        for (position, _) in bytes.iter().enumerate().filter(|&(_, &byte)| byte == quote) {
            let slashes = bytes[..position].iter().rev().take_while(|&&byte| byte == b'\\').count();
            let word_apostrophe = quote == b'\''
                && position > 0
                && bytes[position - 1].is_ascii_alphanumeric()
                && bytes.get(position + 1).is_some_and(|next| next.is_ascii_alphanumeric());
            if slashes % 2 == 0 && !word_apostrophe {
                marks.push(position);
            }
        }
        for pair in marks.chunks_exact(2) {
            let (open, close) = (pair[0], pair[1]);
            let around = (open == start && close + 1 == end) || (open < start && close >= end);
            if around && best.is_none_or(|(best_open, _)| open > best_open) {
                best = Some((open, close));
            }
        }
    }
    best.map(|(open, close)| (open + span.start, close + span.start))
}

/// Delete the nearest bracket pair or quote pair that encloses each selection
/// (or is the selection's own first and last character), keeping the inner
/// text selected. `brackets` holds each selection's bracket pair, found with
/// the syntax (brackets in strings and comments are skipped); the quote pair
/// comes from the selection's line. A selection with no pair stays as it is;
/// `None` when no selection has one.
pub(super) fn plan_unsurround(rope: &Rope, selections: &[(usize, usize)], brackets: &[Option<(usize, usize)>]) -> Option<LinePlan> {
    let mut removed: Vec<usize> = Vec::new();
    for (&(start, end), &brackets) in selections.iter().zip(brackets) {
        let quotes = enclosing_quotes(rope, start, end);
        let pair = match (brackets, quotes) {
            (Some(a), Some(b)) => Some(if a.0 >= b.0 { a } else { b }),
            (a, b) => a.or(b),
        };
        if let Some((open, close)) = pair {
            removed.extend([open, close]);
        }
    }
    if removed.is_empty() {
        return None;
    }
    removed.sort_unstable();
    removed.dedup();
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for &position in &removed {
        match edits.last_mut() {
            Some((range, _)) if range.end == position => range.end += 1,
            _ => edits.push((position..position + 1, String::new())),
        }
    }
    let moved = |offset: usize| offset - removed.partition_point(|&position| position < offset);
    let resulting = selections.iter().map(|&(start, end)| (moved(start), moved(end))).collect();
    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

/// Reorder the whole lines each selection touches with `reorder`, which
/// permutes the lines' text in place. Line endings stay where they are, so no
/// offset moves. A selection of one line, or lines left in the same order,
/// changes nothing; returns `None` when no text changes.
fn plan_reorder_lines(rope: &Rope, selections: &[(usize, usize)], reorder: impl Fn(&mut Vec<String>)) -> Option<LinePlan> {
    let rows = real_rows(rope);
    let touched: Vec<Option<(usize, usize)>> = selections.iter().map(|sel| touched_rows(rope, rows, sel)).collect();
    let blocks = merge_rows(touched.iter().flatten().copied().collect(), false);

    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    // Per block: its content range when it was reordered.
    let mut sorted: Vec<Option<Range<usize>>> = Vec::with_capacity(blocks.len());
    for block in &blocks {
        if block.first == block.last {
            sorted.push(None);
            continue;
        }
        let spans: Vec<_> = (block.first..=block.last).map(|row| row_span(rope, row)).collect();
        let contents: Vec<String> = spans
            .iter()
            .map(|span| rope.slice(span.start..span.content_end).to_string())
            .collect();
        let mut ordered = contents.clone();
        reorder(&mut ordered);
        if ordered == contents {
            sorted.push(None);
            continue;
        }
        let range = spans[0].start..spans[spans.len() - 1].content_end;
        let mut text = String::with_capacity(range.len());
        for (index, line) in ordered.iter().enumerate() {
            text.push_str(line);
            if let Some(span) = spans.get(index + 1) {
                // The terminator that follows this row stays with the row.
                text.push_str(&rope.slice(spans[index].content_end..span.start).to_string());
            }
        }
        edits.push((range.clone(), text));
        sorted.push(Some(range));
    }
    if edits.is_empty() {
        return None;
    }

    let resulting = selections
        .iter()
        .zip(&touched)
        .map(|(&(start, end), touched)| {
            let Some((first, _)) = touched else {
                return (start, end);
            };
            let block = blocks.iter().position(|block| block.first <= *first && *first <= block.last);
            match block.and_then(|block| sorted[block].as_ref()) {
                Some(range) => (range.start, range.end),
                None => (start, end),
            }
        })
        .collect();
    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

/// How Sort lines orders the lines it touches.
#[derive(Clone, Copy)]
pub(super) enum SortMode {
    /// Unicode order of the text, so capitals come before lower case.
    Ascending,
    /// The reverse of `Ascending`.
    Descending,
    /// Unicode order of the lower-cased text. Lines that differ only in case
    /// keep the order they had, so a second sort changes nothing.
    IgnoringCase,
}

/// Sort the whole lines each selection touches, in the order `mode` names.
pub(super) fn plan_sort_lines(rope: &Rope, selections: &[(usize, usize)], mode: SortMode) -> Option<LinePlan> {
    plan_reorder_lines(rope, selections, |lines| match mode {
        SortMode::Ascending => lines.sort(),
        SortMode::Descending => lines.sort_by(|a, b| b.cmp(a)),
        SortMode::IgnoringCase => lines.sort_by_cached_key(|line| line.to_lowercase()),
    })
}

/// Turn the whole lines each selection touches upside down: the last becomes
/// the first. Each line keeps its text; the line endings stay in place.
pub(super) fn plan_reverse_lines(rope: &Rope, selections: &[(usize, usize)]) -> Option<LinePlan> {
    plan_reorder_lines(rope, selections, |lines| lines.reverse())
}

/// Strip the spaces and tabs at the end of each line a selection touches, or
/// of every line when nothing is selected. Only lines that change get an edit,
/// and line endings (`\n` or `\r\n`) stay as they are. Returns `None` when no
/// line has trailing whitespace.
pub(super) fn plan_trim_trailing(rope: &Rope, selections: &[(usize, usize)]) -> Option<LinePlan> {
    let rows = real_rows(rope);
    let touched: Vec<(usize, usize)> = if selections.iter().all(|&(start, end)| start == end) {
        vec![(0, rows - 1)]
    } else {
        selections
            .iter()
            .filter(|&&(start, end)| start != end)
            .filter_map(|sel| touched_rows(rope, rows, sel))
            .collect()
    };

    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for block in merge_rows(touched, false) {
        for row in block.first..=block.last {
            let span = row_span(rope, row);
            let mut keep = span.content_end;
            while keep > span.start && rope.chars_at(keep).prev().is_some_and(|c| c == ' ' || c == '\t') {
                keep -= 1;
            }
            if keep < span.content_end {
                edits.push((keep..span.content_end, String::new()));
            }
        }
    }
    if edits.is_empty() {
        return None;
    }

    // A position after a cut moves back by it; one inside a cut lands where it starts.
    let moved = |position: usize| -> usize {
        let before = edits.partition_point(|(range, _)| range.end <= position);
        let removed: usize = edits[..before].iter().map(|(range, _)| range.len()).sum();
        let inside = edits.get(before).filter(|(range, _)| range.start < position).map_or(0, |(range, _)| position - range.start);
        position - removed - inside
    };
    let resulting = selections.iter().map(|&(start, end)| (moved(start), moved(end))).collect();
    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

/// Delete whole lines from the lines each selection touches, or from every
/// line when nothing is selected. `pick` gets the text of one run of touched
/// lines and says which of them go. A deleted line takes its terminator with
/// it; a last line without one takes the terminator before it instead, so the
/// file still ends the way it did. Returns `None` when no line goes.
fn plan_remove_rows(rope: &Rope, selections: &[(usize, usize)], pick: impl Fn(&[String]) -> Vec<bool>) -> Option<LinePlan> {
    let rows = real_rows(rope);
    let touched: Vec<(usize, usize)> = if selections.iter().all(|&(start, end)| start == end) {
        vec![(0, rows - 1)]
    } else {
        selections
            .iter()
            .filter(|&&(start, end)| start != end)
            .filter_map(|sel| touched_rows(rope, rows, sel))
            .collect()
    };

    let mut gone: Vec<usize> = Vec::new();
    for block in merge_rows(touched, false) {
        let contents: Vec<String> = (block.first..=block.last)
            .map(|row| {
                let span = row_span(rope, row);
                rope.slice(span.start..span.content_end).to_string()
            })
            .collect();
        gone.extend(pick(&contents).into_iter().enumerate().filter(|&(_, go)| go).map(|(index, _)| block.first + index));
    }

    // One edit per run of neighbouring deleted rows.
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut index = 0;
    while index < gone.len() {
        let first = gone[index];
        let mut last = first;
        while gone.get(index + 1) == Some(&(last + 1)) {
            index += 1;
            last += 1;
        }
        index += 1;
        let tail = row_span(rope, last);
        let range = if tail.end == tail.content_end && first > 0 {
            row_span(rope, first - 1).content_end..tail.content_end
        } else {
            row_span(rope, first).start..tail.end
        };
        if !range.is_empty() {
            edits.push((range, String::new()));
        }
    }
    if edits.is_empty() {
        return None;
    }

    // A position after a cut moves back by it; one inside a cut lands where it starts.
    let moved = |position: usize| -> usize {
        let before = edits.partition_point(|(range, _)| range.end <= position);
        let removed: usize = edits[..before].iter().map(|(range, _)| range.len()).sum();
        let inside = edits.get(before).filter(|(range, _)| range.start < position).map_or(0, |(range, _)| position - range.start);
        position - removed - inside
    };
    let resulting = selections.iter().map(|&(start, end)| (moved(start), moved(end))).collect();
    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

/// Keep the first of each line that appears more than once in a run of touched
/// lines (or in the whole file when nothing is selected) and delete the later
/// copies, in the same order and with the same line endings. Lines match when
/// their text is the same, spaces included. Returns `None` when nothing repeats.
pub(super) fn plan_remove_duplicate_lines(rope: &Rope, selections: &[(usize, usize)]) -> Option<LinePlan> {
    plan_remove_rows(rope, selections, |lines| {
        let mut seen = std::collections::HashSet::new();
        lines.iter().map(|line| !seen.insert(line.as_str())).collect()
    })
}

/// Delete the empty and whitespace-only lines among the lines each selection
/// touches, or in the whole file when nothing is selected. The other lines,
/// their endings and the file's last line without a newline stay as they are.
/// Returns `None` when no line is blank.
pub(super) fn plan_remove_blank_lines(rope: &Rope, selections: &[(usize, usize)]) -> Option<LinePlan> {
    plan_remove_rows(rope, selections, |lines| lines.iter().map(|line| line.chars().all(char::is_whitespace)).collect())
}

/// The column Hard wrap fills lines to.
pub(super) const HARD_WRAP_COLUMN: usize = 80;

/// What a line repeats on every line of its paragraph: the indent, then a
/// comment or quote mark (`//`, `///`, `//!`, `--`, a run of `#` or of `>`)
/// and the blanks after it. A mark other than `>` counts only when a blank or
/// the line's end follows it, so `#include` and `--flag` are text. Returns the
/// bytes of the whole prefix and of the indent and mark alone.
fn wrap_prefix(line: &str) -> (usize, usize) {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = &line[indent..];
    let mark = if rest.starts_with("///") || rest.starts_with("//!") {
        3
    } else if rest.starts_with("//") || rest.starts_with("--") {
        2
    } else if rest.starts_with('#') {
        rest.len() - rest.trim_start_matches('#').len()
    } else {
        // Nested quotes are written `> > `, so the mark takes the spaces between.
        let run = rest.len() - rest.trim_start_matches(['>', ' ', '\t']).len();
        rest[..run].trim_end_matches([' ', '\t']).len()
    };
    let after = &rest[mark..];
    if mark > 0 && !rest.starts_with('>') && !(after.is_empty() || after.starts_with([' ', '\t'])) {
        return (indent, indent);
    }
    let blanks = after.len() - after.trim_start_matches([' ', '\t']).len();
    (indent + mark + blanks, indent + mark)
}

/// The indent and mark of a line, `None` when the line holds no text after them.
fn wrap_key(line: &str) -> Option<&str> {
    let (full, key) = wrap_prefix(line);
    (!line[full..].trim().is_empty()).then(|| &line[..key])
}

/// The columns `text` covers, a tab reaching the next multiple of `tab`.
fn columns(text: &str, tab: usize) -> usize {
    text.chars().fold(0, |col, c| if c == '\t' { col + tab - col % tab } else { col + 1 })
}

/// The line break after `row`, or before it on a last line without one.
fn line_break(rope: &Rope, row: usize) -> String {
    for row in [row, row.saturating_sub(1)] {
        let span = row_span(rope, row);
        if span.end > span.content_end {
            return rope.slice(span.content_end..span.end).to_string();
        }
    }
    "\n".to_string()
}

/// The words of a paragraph's lines refilled to `column`, each line starting
/// with `prefix`. A word longer than the room gets a line of its own.
fn fill(lines: &[String], prefix: &str, column: usize, tab: usize, newline: &str) -> String {
    let start = columns(prefix, tab);
    let mut out = String::new();
    let mut width = 0;
    // Only spaces and tabs split words, so a no-break space keeps its words together.
    let words = lines.iter().flat_map(|line| line[wrap_prefix(line).0..].split([' ', '\t']).filter(|word| !word.is_empty()));
    for word in words {
        let len = word.chars().count();
        if width > 0 && width + 1 + len <= column {
            out.push(' ');
            width += 1 + len;
        } else {
            if width > 0 {
                out.push_str(newline);
            }
            out.push_str(prefix);
            width = start + len;
        }
        out.push_str(word);
    }
    out
}

/// Refill the paragraphs among the lines each selection touches so no line
/// passes `column`, or the paragraph at a caret. A paragraph is a run of lines
/// with text that share an indent and comment or quote mark; each new line
/// starts with its first line's prefix. Blank lines, and blank comment lines
/// such as `//`, stay and split paragraphs. A selection ends over the
/// paragraphs it touched, a caret at their end. Returns `None` when no line
/// changes.
pub(super) fn plan_hard_wrap(rope: &Rope, selections: &[(usize, usize)], column: usize, tab: usize) -> Option<LinePlan> {
    let tab = tab.max(1);
    let rows = real_rows(rope);
    let line = |row: usize| {
        let span = row_span(rope, row);
        rope.slice(span.start..span.content_end).to_string()
    };
    let touched: Vec<Option<(usize, usize)>> = selections
        .iter()
        .map(|sel| {
            let (first, last) = touched_rows(rope, rows, sel)?;
            if sel.0 != sel.1 {
                return Some((first, last));
            }
            let text = line(first);
            let key = wrap_key(&text)?;
            let (mut top, mut bottom) = (first, first);
            while top > 0 && wrap_key(&line(top - 1)) == Some(key) {
                top -= 1;
            }
            while bottom + 1 < rows && wrap_key(&line(bottom + 1)) == Some(key) {
                bottom += 1;
            }
            Some((top, bottom))
        })
        .collect();
    let blocks = merge_rows(touched.iter().flatten().copied().collect(), false);

    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    let mut changed = vec![false; blocks.len()];
    for (index, block) in blocks.iter().enumerate() {
        let mut row = block.first;
        while row <= block.last {
            let mut lines = vec![line(row)];
            let Some(key) = wrap_key(&lines[0]).map(str::to_string) else {
                row += 1;
                continue;
            };
            while row + lines.len() <= block.last {
                let next = line(row + lines.len());
                if wrap_key(&next) != Some(key.as_str()) {
                    break;
                }
                lines.push(next);
            }
            let (first, last) = (row, row + lines.len() - 1);
            row = last + 1;
            let prefix = lines[0][..wrap_prefix(&lines[0]).0].to_string();
            let new = fill(&lines, &prefix, column, tab, &line_break(rope, first));
            let range = row_span(rope, first).start..row_span(rope, last).content_end;
            if rope.slice(range.clone()).to_string() != new {
                edits.push((range, new));
                changed[index] = true;
            }
        }
    }
    if edits.is_empty() {
        return None;
    }

    // Offset change from the edits that end at or before a position.
    let shift = |position: usize| -> usize {
        let delta: isize = edits
            .iter()
            .filter(|(range, _)| range.end <= position)
            .map(|(range, text)| text.len() as isize - range.len() as isize)
            .sum();
        (position as isize + delta) as usize
    };
    let resulting = selections
        .iter()
        .zip(&touched)
        .map(|(&(start, end), touched)| {
            let block = touched.and_then(|(first, _)| blocks.iter().position(|block| block.first <= first && first <= block.last));
            match block.filter(|&block| changed[block]).map(|block| &blocks[block]) {
                Some(block) => {
                    let end_at = shift(row_span(rope, block.last).content_end);
                    if start == end {
                        (end_at, end_at)
                    } else {
                        (shift(row_span(rope, block.first).start), end_at)
                    }
                }
                None => (shift(start), shift(end)),
            }
        })
        .collect();
    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

/// A leading run of `width` columns written with spaces, or with as many tabs
/// as fit plus spaces for the rest.
fn indent_for(width: usize, tab: usize, to_tabs: bool) -> String {
    if to_tabs {
        format!("{}{}", "\t".repeat(width / tab), " ".repeat(width % tab))
    } else {
        " ".repeat(width)
    }
}

/// Rewrite the spaces and tabs each line starts with, for the lines a
/// selection touches or every line when nothing is selected. A run keeps its
/// width in columns (a tab reaches the next multiple of `tab`), so a tab
/// followed by spaces converts by what it draws. Spaces after the first
/// character of code stay as they are. Returns `None` when no line changes.
pub(super) fn plan_convert_indent(rope: &Rope, selections: &[(usize, usize)], tab: usize, to_tabs: bool) -> Option<LinePlan> {
    let tab = tab.max(1);
    let rows = real_rows(rope);
    let touched: Vec<(usize, usize)> = if selections.iter().all(|&(start, end)| start == end) {
        vec![(0, rows - 1)]
    } else {
        selections
            .iter()
            .filter(|&&(start, end)| start != end)
            .filter_map(|sel| touched_rows(rope, rows, sel))
            .collect()
    };

    // Each edit with the columns its old run drew.
    let mut edits: Vec<(Range<usize>, String)> = Vec::new();
    for block in merge_rows(touched, false) {
        for row in block.first..=block.last {
            let span = row_span(rope, row);
            let (mut width, mut end) = (0usize, span.start);
            for c in rope.slice(span.start..span.content_end).chars() {
                match c {
                    ' ' => width += 1,
                    '\t' => width += tab - width % tab,
                    _ => break,
                }
                end += 1;
            }
            let new = indent_for(width, tab, to_tabs);
            if end > span.start && rope.slice(span.start..end).to_string() != new {
                edits.push((span.start..end, new));
            }
        }
    }
    if edits.is_empty() {
        return None;
    }

    // A position after an edit shifts by its length change; one inside lands
    // after the new characters that draw the columns before it.
    let moved = |position: usize| -> usize {
        let before = edits.partition_point(|(range, _)| range.end <= position);
        let delta: isize = edits[..before].iter().map(|(range, new)| new.len() as isize - range.len() as isize).sum();
        let base = (position as isize + delta) as usize;
        match edits.get(before).filter(|(range, _)| range.start < position) {
            Some((range, new)) => {
                let mut width = 0usize;
                for c in rope.slice(range.start..position).chars() {
                    width += if c == '\t' { tab - width % tab } else { 1 };
                }
                let shifted = (range.start as isize + delta) as usize;
                // Walk the new run until it draws as many columns as the old text before the caret.
                let (mut col, mut at) = (0usize, 0usize);
                for c in new.chars() {
                    if col >= width {
                        break;
                    }
                    col += if c == '\t' { tab - col % tab } else { 1 };
                    at += 1;
                }
                shifted + at
            }
            None => base,
        }
    };
    let resulting = selections.iter().map(|&(start, end)| (moved(start), moved(end))).collect();
    Some(LinePlan {
        edits,
        selections: resulting,
    })
}

impl<M: InputModeKind> InputBaseState<M> {
    /// Lift the selection fill once, the way a Copy does, for a command that changed the text
    /// under a selection and left it selected. A caret alone, or a window that isn't key, gets
    /// none. The lift ends at the next edit, so it takes the revision the command left.
    pub(crate) fn lift_selection(&mut self, window: &Window, cx: &mut Context<Self>) {
        if !crate::input::window_key(window) || self.selections.iter().all(|sel| sel.is_empty()) {
            return;
        }
        let selections = self.selections.iter().map(|sel| (sel.start, sel.end)).collect();
        self.lift = Some(super::element::Lift::new(selections, self.document_revision));
        cx.notify();
    }

    /// Apply a text transformation as one undo step and, when it changed the text, lift the
    /// selection it left.
    pub(super) fn transform_selection(
        &mut self,
        plan: impl FnOnce(&Rope, &[(usize, usize)]) -> Option<LinePlan>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let revision = self.document_revision;
        self.apply_line_plan(false, plan, window, cx);
        if self.document_revision != revision {
            self.lift_selection(window, cx);
        }
    }

    pub(super) fn make_upper_case(&mut self, _: &MakeUpperCase, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_case(text, ranges, CaseKind::Upper), window, cx);
    }

    pub(super) fn make_lower_case(&mut self, _: &MakeLowerCase, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_case(text, ranges, CaseKind::Lower), window, cx);
    }

    pub(super) fn capitalize(&mut self, _: &Capitalize, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_case(text, ranges, CaseKind::Capitalize), window, cx);
    }

    pub(super) fn convert_to_title_case(&mut self, _: &ConvertToTitleCase, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_case(text, ranges, CaseKind::Title), window, cx);
    }

    pub(super) fn convert_to_snake_case(&mut self, _: &ConvertToSnakeCase, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_case(text, ranges, CaseKind::Snake), window, cx);
    }

    pub(super) fn convert_to_camel_case(&mut self, _: &ConvertToCamelCase, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_case(text, ranges, CaseKind::Camel), window, cx);
    }

    pub(super) fn convert_to_pascal_case(&mut self, _: &ConvertToPascalCase, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_case(text, ranges, CaseKind::Pascal), window, cx);
    }

    pub(super) fn convert_to_kebab_case(&mut self, _: &ConvertToKebabCase, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_case(text, ranges, CaseKind::Kebab), window, cx);
    }

    pub(super) fn surround_with_parentheses(&mut self, _: &SurroundWithParentheses, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_surround(text, ranges, "(", ")"), window, cx);
    }

    pub(super) fn surround_with_brackets(&mut self, _: &SurroundWithBrackets, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_surround(text, ranges, "[", "]"), window, cx);
    }

    pub(super) fn surround_with_braces(&mut self, _: &SurroundWithBraces, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_surround(text, ranges, "{", "}"), window, cx);
    }

    pub(super) fn surround_with_double_quotes(&mut self, _: &SurroundWithDoubleQuotes, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_surround(text, ranges, "\"", "\""), window, cx);
    }

    pub(super) fn surround_with_single_quotes(&mut self, _: &SurroundWithSingleQuotes, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_surround(text, ranges, "'", "'"), window, cx);
    }

    pub(super) fn surround_with_backticks(&mut self, _: &SurroundWithBackticks, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_surround(text, ranges, "`", "`"), window, cx);
    }

    pub(super) fn remove_surrounding(&mut self, _: &RemoveSurrounding, window: &mut Window, cx: &mut Context<Self>) {
        let pairs: Vec<Option<(usize, usize)>> = self.selections.iter().map(|sel| self.surrounding_bracket_pair(sel.start, sel.end)).collect();
        self.transform_selection(|text, ranges| plan_unsurround(text, ranges, &pairs), window, cx);
    }

    pub(super) fn sort_lines(&mut self, _: &SortLines, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_sort_lines(text, ranges, SortMode::Ascending), window, cx);
    }

    pub(super) fn sort_lines_descending(&mut self, _: &SortLinesDescending, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_sort_lines(text, ranges, SortMode::Descending), window, cx);
    }

    pub(super) fn sort_lines_ignoring_case(&mut self, _: &SortLinesIgnoringCase, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(|text, ranges| plan_sort_lines(text, ranges, SortMode::IgnoringCase), window, cx);
    }

    pub(super) fn reverse_lines(&mut self, _: &ReverseLines, window: &mut Window, cx: &mut Context<Self>) {
        self.transform_selection(plan_reverse_lines, window, cx);
    }

    pub(super) fn remove_duplicate_lines(&mut self, _: &RemoveDuplicateLines, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_washed_line_plan(plan_remove_duplicate_lines, window, cx);
    }

    pub(super) fn remove_blank_lines(&mut self, _: &RemoveBlankLines, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_washed_line_plan(plan_remove_blank_lines, window, cx);
    }

    pub(super) fn trim_trailing_whitespace(&mut self, _: &TrimTrailingWhitespace, window: &mut Window, cx: &mut Context<Self>) {
        self.apply_washed_line_plan(plan_trim_trailing, window, cx);
    }

    pub(super) fn hard_wrap(&mut self, _: &HardWrap, window: &mut Window, cx: &mut Context<Self>) {
        let tab = self.mode.tab_size().tab_size;
        self.transform_selection(|text, ranges| plan_hard_wrap(text, ranges, HARD_WRAP_COLUMN, tab), window, cx);
    }

    pub(super) fn convert_indentation_to_spaces(&mut self, _: &ConvertIndentationToSpaces, window: &mut Window, cx: &mut Context<Self>) {
        self.convert_indentation(false, window, cx);
    }

    pub(super) fn convert_indentation_to_tabs(&mut self, _: &ConvertIndentationToTabs, window: &mut Window, cx: &mut Context<Self>) {
        self.convert_indentation(true, window, cx);
    }

    /// Rewrite the indentation as one undo step, then make the document's
    /// indent style the one converted to, so the next Tab key matches.
    fn convert_indentation(&mut self, to_tabs: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.is_multi_line() || !self.is_editable() {
            return;
        }
        let tab = self.mode.tab_size();
        let before = self.text.clone();
        self.apply_washed_line_plan(|text, ranges| plan_convert_indent(text, ranges, tab.tab_size, to_tabs), window, cx);
        if self.text != before {
            match &mut self.mode {
                LayoutMode::PlainText { tab: value, .. } | LayoutMode::CodeEditor { tab: value, .. } => value.hard_tabs = to_tabs,
                _ => {}
            }
        }
        cx.notify();
    }

    fn step_selection(&mut self, delta: i64, window: &mut Window, cx: &mut Context<Self>) {
        let carets: Vec<usize> = self.selections.iter().map(|sel| sel.cursor_offset()).collect();
        self.apply_washed_line_plan(|text, ranges| plan_step(text, ranges, &carets, delta), window, cx);
    }

    pub(super) fn increment(&mut self, _: &Increment, window: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(1, window, cx);
    }

    pub(super) fn decrement(&mut self, _: &Decrement, window: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(-1, window, cx);
    }

    pub(super) fn increment_by_ten(&mut self, _: &IncrementByTen, window: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(10, window, cx);
    }

    pub(super) fn decrement_by_ten(&mut self, _: &DecrementByTen, window: &mut Window, cx: &mut Context<Self>) {
        self.step_selection(-10, window, cx);
    }
}

/// Replacement for a hex literal such as `0x0f`: same digit count, the case of
/// the digits (upper when any digit is, or the prefix is `0X`), floor of zero.
/// `None` when `word` is not a hex literal.
fn step_hex(word: &str, delta: i64) -> Option<String> {
    let prefix = word.get(..2)?;
    if !prefix.eq_ignore_ascii_case("0x") {
        return None;
    }
    let digits = &word[2..];
    if digits.is_empty() || digits.len() > 32 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let value = u128::from_str_radix(digits, 16).ok()?;
    let value = if delta >= 0 { value.checked_add(delta as u128)? } else { value.saturating_sub(delta.unsigned_abs() as u128) };
    let upper = digits.bytes().any(|b| b.is_ascii_uppercase()) || prefix == "0X" && !digits.bytes().any(|b| b.is_ascii_lowercase());
    let body = if upper { format!("{value:0width$X}", width = digits.len()) } else { format!("{value:0width$x}", width = digits.len()) };
    Some(format!("{prefix}{body}"))
}

/// The other word of `true`/`false`, `yes`/`no` or `on`/`off`, in the case of `word`
/// (`TRUE`, `True`, `true`). `None` for any other word.
fn flip_word(word: &str) -> Option<String> {
    let lower = word.to_ascii_lowercase();
    let other = match lower.as_str() {
        "true" => "false",
        "false" => "true",
        "yes" => "no",
        "no" => "yes",
        "on" => "off",
        "off" => "on",
        _ => return None,
    };
    let mut letters = word.chars();
    let first_upper = letters.next().is_some_and(|c| c.is_ascii_uppercase());
    let rest_upper = word.len() > 1 && letters.all(|c| c.is_ascii_uppercase());
    Some(if first_upper && rest_upper {
        other.to_ascii_uppercase()
    } else if first_upper {
        let mut text = other.to_string();
        text[..1].make_ascii_uppercase();
        text
    } else {
        other.to_string()
    })
}

/// Add `delta` to a decimal number without a sign: `digits` or `digits.digits`.
/// The leading zeros of the whole part stay as width and the decimals stay as
/// many; the sign follows the result (`-1` + 1 is `0`).
fn step_number(unsigned: &str, negative: bool, delta: i64) -> Option<String> {
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let width = if whole.len() > 1 && whole.starts_with('0') { whole.len() } else { 0 };
    let mut value: i128 = format!("{whole}{fraction}").parse().ok()?;
    if negative {
        value = -value;
    }
    let scale = 10i128.checked_pow(fraction.len() as u32)?;
    value = value.checked_add(i128::from(delta).checked_mul(scale)?)?;
    let mut digits = value.unsigned_abs().to_string();
    while digits.len() <= fraction.len() {
        digits.insert(0, '0');
    }
    let (whole, fraction) = digits.split_at(digits.len() - fraction.len());
    let sign = if value < 0 { "-" } else { "" };
    let dot = if fraction.is_empty() { "" } else { "." };
    Some(format!("{sign}{whole:0>width$}{dot}{fraction}"))
}

/// How far each side of the caret a line is read, so a minified line stays cheap.
const STEP_WINDOW: usize = 256;

/// The number, hex literal or on/off word the caret is on or just after, and
/// what it becomes after adding `delta`. The range is in bytes of `line`.
fn step_in_line(line: &str, caret: usize, delta: i64) -> Option<(Range<usize>, String)> {
    let mut from = caret.saturating_sub(STEP_WINDOW);
    while !line.is_char_boundary(from) {
        from -= 1;
    }
    let mut to = (caret + STEP_WINDOW).min(line.len());
    while !line.is_char_boundary(to) {
        to += 1;
    }
    let text = &line[from..to];
    let bytes = text.as_bytes();
    let at = caret - from;
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80;

    let mut tokens: Vec<(Range<usize>, String)> = Vec::new();
    let mut hex_words: Vec<Range<usize>> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if !is_word(bytes[index]) {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && is_word(bytes[index]) {
            index += 1;
        }
        let word = &text[start..index];
        if let Some(new) = step_hex(word, delta) {
            hex_words.push(start..index);
            tokens.push((start..index, new));
        } else if let Some(new) = flip_word(word) {
            tokens.push((start..index, new));
        }
    }
    let mut index = 0;
    while index < bytes.len() {
        if !bytes[index].is_ascii_digit() {
            index += 1;
            continue;
        }
        let start = index;
        while index < bytes.len() && bytes[index].is_ascii_digit() {
            index += 1;
        }
        if bytes.get(index) == Some(&b'.') && bytes.get(index + 1).is_some_and(u8::is_ascii_digit) {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
        }
        if hex_words.iter().any(|word| word.contains(&start)) {
            continue;
        }
        let negative = start > 0
            && bytes[start - 1] == b'-'
            && (start == 1 || !(is_word(bytes[start - 2]) || matches!(bytes[start - 2], b'.' | b')' | b']')));
        if let Some(new) = step_number(&text[start..index], negative, delta) {
            tokens.push((start - usize::from(negative)..index, new));
        }
    }
    let (range, new) = tokens.into_iter().filter(|(range, _)| range.start <= at && at <= range.end).min_by_key(|(range, _)| range.start)?;
    (new != text[range.clone()]).then(|| (range.start + from..range.end + from, new))
}

/// The token at a caret, as an absolute range and its replacement.
fn step_target(rope: &Rope, caret: usize, delta: i64) -> Option<(Range<usize>, String)> {
    let span = row_span(rope, rope.offset_to_point(caret).row);
    let from = rope.clip_offset(span.start.max(caret.saturating_sub(STEP_WINDOW)), Bias::Left);
    let to = rope.clip_offset(span.content_end.min(caret + STEP_WINDOW), Bias::Right);
    let line = rope.slice(from..to).to_string();
    let (range, new) = step_in_line(&line, caret.checked_sub(from)?.min(line.len()), delta)?;
    Some((range.start + from..range.end + from, new))
}

/// Add `delta` to the number, or flip the on/off word, under every caret (the
/// end of each selection). Carets with nothing under them are left alone;
/// returns `None` when no text changes. Two carets on one token change it once.
pub(super) fn plan_step(rope: &Rope, selections: &[(usize, usize)], carets: &[usize], delta: i64) -> Option<LinePlan> {
    let mut edits: Vec<(Range<usize>, String)> = carets.iter().filter_map(|&caret| step_target(rope, caret, delta)).collect();
    edits.sort_by_key(|(range, _)| (range.start, range.end));
    edits.dedup_by(|next, kept| next.0 == kept.0);
    let mut last_end = 0;
    edits.retain(|(range, _)| {
        let keep = range.start >= last_end;
        if keep {
            last_end = range.end;
        }
        keep
    });
    if edits.is_empty() {
        return None;
    }
    let shift = |position: usize| -> isize {
        edits
            .iter()
            .filter(|(range, _)| range.end <= position)
            .map(|(range, text)| text.len() as isize - range.len() as isize)
            .sum()
    };
    let moved = |position: usize| -> usize {
        if let Some((range, text)) = edits.iter().find(|(range, _)| range.start < position && position < range.end) {
            return (range.start as isize + shift(range.start)) as usize + (position - range.start).min(text.len());
        }
        (position as isize + shift(position)) as usize
    };
    let selections = selections.iter().map(|&(start, end)| (moved(start), moved(end))).collect();
    Some(LinePlan { edits, selections })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{EntityInputHandler as _, ParentElement as _, Styled as _};

    fn apply(text: &str, plan: &LinePlan) -> String {
        let mut out = text.to_string();
        let mut edits: Vec<_> = plan.edits.iter().collect();
        edits.sort_by_key(|(range, _)| std::cmp::Reverse(range.start));
        for (range, new_text) in edits {
            out.replace_range(range.clone(), new_text);
        }
        out
    }

    fn wrapped(text: &str, selections: &[(usize, usize)], column: usize) -> Option<(String, Vec<(usize, usize)>)> {
        let plan = plan_hard_wrap(&Rope::from(text), selections, column, 4)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    #[test]
    fn hard_wrap_fills_a_paragraph_to_the_column() {
        let text = "one two three four five six\nseven\n\nnext para here\n";
        let (out, selections) = wrapped(text, &[(0, text.len())], 14).unwrap();
        assert_eq!(out, "one two three\nfour five six\nseven\n\nnext para here\n");
        assert_eq!(selections, vec![(0, out.len() - 1)]);
        // Short lines join up to the column.
        let (out, _) = wrapped("a\nb\nc", &[(0, 5)], 80).unwrap();
        assert_eq!(out, "a b c");
    }

    #[test]
    fn hard_wrap_keeps_indent_and_comment_marks() {
        let text = "    // alpha beta gamma delta\n    // epsilon\n    //\n    // zeta eta\n";
        let (out, _) = wrapped(text, &[(0, text.len())], 22).unwrap();
        assert_eq!(out, "    // alpha beta\n    // gamma delta\n    // epsilon\n    //\n    // zeta eta\n");
        let (out, _) = wrapped("# a b c d\n> e f g h\n> i", &[(0, 23)], 6).unwrap();
        assert_eq!(out, "# a b\n# c d\n> e f\n> g h\n> i");
        // A tab indent counts to its tab stop.
        let (out, _) = wrapped("\t/// aa bb", &[(0, 10)], 10).unwrap();
        assert_eq!(out, "\t/// aa\n\t/// bb");
        // `#include` is text, not a mark.
        assert_eq!(wrap_prefix("#include <a>"), (0, 0));
    }

    #[test]
    fn hard_wrap_keeps_nested_quotes_and_no_break_spaces() {
        let (out, _) = wrapped("> > a b\n> > c", &[(0, 13)], 80).unwrap();
        assert_eq!(out, "> > a b c");
        let (out, _) = wrapped("a 10\u{a0}km", &[(0, 8)], 5).unwrap();
        assert_eq!(out, "a\n10\u{a0}km");
    }

    #[test]
    fn hard_wrap_at_a_caret_takes_its_paragraph() {
        let text = "intro\n\n// a b\n// c d\ncode();\n";
        let caret = text.find("c d").unwrap();
        let (out, selections) = wrapped(text, &[(caret, caret)], 80).unwrap();
        assert_eq!(out, "intro\n\n// a b c d\ncode();\n");
        let end = out.find("\ncode").unwrap();
        assert_eq!(selections, vec![(end, end)]);
        // A caret on a blank line does nothing.
        assert!(wrapped(text, &[(6, 6)], 80).is_none());
    }

    #[test]
    fn hard_wrap_keeps_crlf_and_moves_later_selections() {
        let (out, _) = wrapped("a b c\r\nd", &[(0, 8)], 3).unwrap();
        assert_eq!(out, "a b\r\nc d");
        // Runs of spaces become one, so a later selection moves back.
        let text = "a  b  c\n\nx\n";
        let (out, selections) = wrapped(text, &[(0, 1), (9, 10)], 3).unwrap();
        assert_eq!(out, "a b\nc\n\nx\n");
        assert_eq!(selections, vec![(0, 5), (7, 8)]);
        assert!(wrapped("a b\nc", &[(0, 5)], 3).is_none());
        // A word longer than the room keeps its own line.
        let (out, _) = wrapped("abcdef g", &[(0, 8)], 3).unwrap();
        assert_eq!(out, "abcdef\ng");
    }

    fn cased(text: &str, selections: &[(usize, usize)], kind: CaseKind) -> Option<(String, Vec<(usize, usize)>)> {
        let plan = plan_case(&Rope::from(text), selections, kind)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    fn styled(text: &str, kind: CaseKind) -> String {
        convert(text, kind)
    }

    #[test]
    fn the_splitter_breaks_on_separators_and_case_boundaries() {
        let split = |text: &str| lower_words(text);
        assert_eq!(split("foo bar_baz-qux"), ["foo", "bar", "baz", "qux"]);
        assert_eq!(split("fooBar"), ["foo", "bar"]);
        assert_eq!(split("FooBar"), ["foo", "bar"]);
        assert_eq!(split("HTTPServer"), ["http", "server"]);
        assert_eq!(split("parseHTTP"), ["parse", "http"]);
        assert_eq!(split("v2Beta"), ["v2", "beta"]);
        assert_eq!(split("abc123def"), ["abc123def"]);
        assert_eq!(split("don't stop"), ["dont", "stop"]);
        assert_eq!(split("  "), Vec::<String>::new());
    }

    #[test]
    fn a_combining_mark_stays_with_its_letter() {
        assert_eq!(lower_words("nai\u{308}ve test"), ["nai\u{308}ve", "test"]);
        assert_eq!(styled("nai\u{308}ve test", CaseKind::Snake), "nai\u{308}ve_test");
        assert_eq!(styled("e\u{301}Cole", CaseKind::Snake), "e\u{301}_cole");
    }

    #[test]
    fn the_caret_never_lands_inside_a_multi_byte_char() {
        let (text, selections) = cased("x_\u{e9}bc", &[(2, 2)], CaseKind::Camel).unwrap();
        assert_eq!(text, "x\u{c9}bc");
        assert!(text.is_char_boundary(selections[0].0));
        assert_eq!(selections, vec![(1, 1)]);
    }

    #[test]
    fn each_style_joins_the_words() {
        for text in ["hello big_world", "helloBig-World", "HelloBigWorld", "hello  big   world"] {
            assert_eq!(styled(text, CaseKind::Snake), "hello_big_world", "{text}");
            assert_eq!(styled(text, CaseKind::Kebab), "hello-big-world", "{text}");
            assert_eq!(styled(text, CaseKind::Camel), "helloBigWorld", "{text}");
            assert_eq!(styled(text, CaseKind::Pascal), "HelloBigWorld", "{text}");
        }
        assert_eq!(styled("HTTPServer v2Beta", CaseKind::Snake), "http_server_v2_beta");
        assert_eq!(styled("HTTPServer v2Beta", CaseKind::Camel), "httpServerV2Beta");
    }

    #[test]
    fn title_case_capitalizes_and_leaves_small_words() {
        assert_eq!(styled("the lord of the rings", CaseKind::Title), "The Lord of the Rings");
        assert_eq!(styled("snake_case_name", CaseKind::Title), "Snake Case Name");
        assert_eq!(styled("fooBar and HTTPServer", CaseKind::Title), "Foo Bar and Http Server");
        assert_eq!(styled("a tale to remember, or not", CaseKind::Title), "A Tale to Remember, or Not");
        assert_eq!(styled("what to do in", CaseKind::Title), "What to Do In");
    }

    #[test]
    fn styles_keep_indentation_line_endings_and_edge_underscores() {
        assert_eq!(styled("  foo bar\r\n\tbaz qux  \n", CaseKind::Snake), "  foo_bar\r\n\tbaz_qux  \n");
        assert_eq!(styled("__init__ main", CaseKind::Snake), "__init_main");
        assert_eq!(styled("__initThing__", CaseKind::Snake), "__init_thing__");
        assert_eq!(styled("__initThing__", CaseKind::Kebab), "init-thing");
        assert_eq!(styled("***", CaseKind::Camel), "***");
        assert_eq!(styled("", CaseKind::Pascal), "");
    }

    #[test]
    fn convert_case_acts_on_each_selection_and_keeps_them() {
        let (text, selections) = cased("fooBar and bazQux", &[(0, 6), (11, 17)], CaseKind::Snake).unwrap();
        assert_eq!(text, "foo_bar and baz_qux");
        assert_eq!(selections, vec![(0, 7), (12, 19)]);
        // A caret with no selection converts the word it is in and stays in it.
        let (text, selections) = cased("say fooBar now", &[(8, 8)], CaseKind::Kebab).unwrap();
        assert_eq!(text, "say foo-bar now");
        assert_eq!(selections, vec![(9, 9)]);
        // Text already in the style changes nothing.
        assert!(cased("foo_bar", &[(0, 7)], CaseKind::Snake).is_none());
    }

    fn surrounded(text: &str, selections: &[(usize, usize)], open: &str, close: &str) -> (String, Vec<(usize, usize)>) {
        let plan = plan_surround(&Rope::from(text), selections, open, close).unwrap();
        (apply(text, &plan), plan.selections.clone())
    }

    fn unsurrounded(text: &str, selections: &[(usize, usize)]) -> Option<(String, Vec<(usize, usize)>)> {
        unsurrounded_where(text, selections, |_| true)
    }

    /// `is_code` says which offsets are code, standing in for the highlighter's parse.
    fn unsurrounded_where(text: &str, selections: &[(usize, usize)], is_code: impl Fn(usize) -> bool) -> Option<(String, Vec<(usize, usize)>)> {
        let rope = Rope::from(text);
        let pairs: Vec<_> = selections
            .iter()
            .map(|&(start, end)| {
                super::super::brackets::surrounding_pair(&rope, start, end, &mut |offsets, flags| flags.extend(offsets.iter().map(|&offset| is_code(offset))))
            })
            .collect();
        let plan = plan_unsurround(&rope, selections, &pairs)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    #[test]
    fn remove_surrounding_skips_brackets_in_strings_and_comments() {
        // `f("(", x)`: the quoted `(` is not code, so the call's own parentheses go.
        let (text, selections) = unsurrounded_where("f(\"(\", x)", &[(7, 7)], |offset| !(2..=4).contains(&offset)).unwrap();
        assert_eq!(text, "f\"(\", x");
        assert_eq!(selections, vec![(6, 6)]);
        // An unclosed `(` in a comment is skipped too.
        let (text, _) = unsurrounded_where("(a // (\nb)", &[(1, 1)], |offset| offset < 5 || offset > 7).unwrap();
        assert_eq!(text, "a // (\nb");
        // Counted as code, the quoted `(` would have been taken.
        let (text, _) = unsurrounded("f(\"(\", x)", &[(7, 7)]).unwrap();
        assert_ne!(text, "f\"(\", x");
    }

    #[test]
    fn surround_wraps_each_selection_and_keeps_the_inner_text_selected() {
        let (text, selections) = surrounded("one two three", &[(0, 3), (8, 13)], "(", ")");
        assert_eq!(text, "(one) two (three)");
        assert_eq!(selections, vec![(1, 4), (11, 16)]);
        let (text, selections) = surrounded("one two", &[(4, 7)], "\"", "\"");
        assert_eq!(text, "one \"two\"");
        assert_eq!(selections, vec![(5, 8)]);
        // A caret gets the empty pair and stays between.
        let (text, selections) = surrounded("ab", &[(1, 1)], "{", "}");
        assert_eq!(text, "a{}b");
        assert_eq!(selections, vec![(2, 2)]);
        // Selections keep their order when given out of text order.
        let (text, selections) = surrounded("ab cd", &[(3, 5), (0, 2)], "[", "]");
        assert_eq!(text, "[ab] [cd]");
        assert_eq!(selections, vec![(6, 8), (1, 3)]);
    }

    #[test]
    fn remove_surrounding_takes_the_nearest_balanced_pair() {
        // Around a selection, an inner pair before an outer one.
        let (text, selections) = unsurrounded("a (b [c] d) e", &[(6, 7)]).unwrap();
        assert_eq!(text, "a (b c d) e");
        assert_eq!(selections, vec![(5, 6)]);
        // From a caret, past a pair that closes before it.
        let (text, selections) = unsurrounded("f(a, (b), c)", &[(3, 3)]).unwrap();
        assert_eq!(text, "fa, (b), c");
        assert_eq!(selections, vec![(2, 2)]);
        let (text, _) = unsurrounded("f(a, (b), c)", &[(7, 7)]).unwrap();
        assert_eq!(text, "f(a, b, c)");
        // The selection's own first and last characters.
        let (text, selections) = unsurrounded("x (yz) w", &[(2, 6)]).unwrap();
        assert_eq!(text, "x yz w");
        assert_eq!(selections, vec![(2, 4)]);
    }

    #[test]
    fn remove_surrounding_needs_a_balanced_pair() {
        assert!(unsurrounded("(a", &[(1, 1)]).is_none());
        assert!(unsurrounded("(a]", &[(1, 1)]).is_none());
        assert!(unsurrounded("a b", &[(1, 1)]).is_none());
        // A selection that reaches past the closer is not inside the pair.
        assert!(unsurrounded("(a) b", &[(1, 5)]).is_none());
    }

    #[test]
    fn remove_surrounding_matches_quotes_on_the_line() {
        let (text, selections) = unsurrounded("say \"hi there\" now", &[(7, 7)]).unwrap();
        assert_eq!(text, "say hi there now");
        assert_eq!(selections, vec![(6, 6)]);
        // An apostrophe inside a word is not a quote, and a quote on another line doesn't pair.
        let (text, _) = unsurrounded("don't say 'hi'", &[(12, 12)]).unwrap();
        assert_eq!(text, "don't say hi");
        assert!(unsurrounded("don't", &[(2, 2)]).is_none());
        assert!(unsurrounded("\"a\nb\"", &[(1, 1)]).is_none());
        assert!(unsurrounded("\"a\nb\"", &[(4, 4)]).is_none());
        // The innermost pair wins whether it is a quote or a bracket.
        let (text, _) = unsurrounded("(\"a\")", &[(2, 2)]).unwrap();
        assert_eq!(text, "(a)");
        let (text, _) = unsurrounded("\"(a)\"", &[(2, 2)]).unwrap();
        assert_eq!(text, "\"a\"");
    }

    #[test]
    fn remove_surrounding_shares_a_pair_between_selections() {
        let (text, selections) = unsurrounded("(a b)", &[(1, 2), (3, 4)]).unwrap();
        assert_eq!(text, "a b");
        assert_eq!(selections, vec![(0, 1), (2, 3)]);
        // An empty pair goes whole.
        let (text, selections) = unsurrounded("x()y", &[(2, 2)]).unwrap();
        assert_eq!(text, "xy");
        assert_eq!(selections, vec![(1, 1)]);
    }

    fn sorted(text: &str, selections: &[(usize, usize)]) -> Option<(String, Vec<(usize, usize)>)> {
        let plan = plan_sort_lines(&Rope::from(text), selections, SortMode::Ascending)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    #[test]
    fn upper_case_a_selection_and_keep_it_selected() {
        let (text, selections) = cased("hello big world", &[(6, 9)], CaseKind::Upper).unwrap();
        assert_eq!(text, "hello BIG world");
        assert_eq!(selections, vec![(6, 9)]);
    }

    #[test]
    fn empty_selection_changes_the_word_at_the_caret() {
        let (text, selections) = cased("hello big world", &[(8, 8)], CaseKind::Upper).unwrap();
        assert_eq!(text, "hello BIG world");
        assert_eq!(selections, vec![(8, 8)]);
        // A caret at a word's end still counts as in it.
        let (text, _) = cased("hello world", &[(11, 11)], CaseKind::Upper).unwrap();
        assert_eq!(text, "hello WORLD");
        let (text, _) = cased("hello world", &[(5, 5)], CaseKind::Upper).unwrap();
        assert_eq!(text, "HELLO world");
    }

    #[test]
    fn caret_between_spaces_changes_nothing() {
        assert!(cased("a  b", &[(2, 2)], CaseKind::Upper).is_none());
        assert!(cased("ABC", &[(0, 3)], CaseKind::Upper).is_none());
    }

    #[test]
    fn lower_case_and_changed_length() {
        let (text, _) = cased("HELLO World", &[(0, 11)], CaseKind::Lower).unwrap();
        assert_eq!(text, "hello world");
        // The fi ligature shrinks from three bytes to two; later selections shift.
        let (text, selections) = cased("\u{fb01} ab", &[(0, 3), (4, 6)], CaseKind::Upper).unwrap();
        assert_eq!(text, "FI AB");
        assert_eq!(selections, vec![(0, 2), (3, 5)]);
    }

    #[test]
    fn capitalize_each_word_and_lower_the_rest() {
        let (text, selections) = cased("hELLO wORLD it's_ok", &[(0, 19)], CaseKind::Capitalize).unwrap();
        assert_eq!(text, "Hello World It's_ok");
        assert_eq!(selections, vec![(0, 19)]);
        let (text, _) = cased("say don't stop", &[(4, 9)], CaseKind::Capitalize).unwrap();
        assert_eq!(text, "say Don't stop");
    }

    #[test]
    fn a_selection_inside_another_cursors_word_lands_at_its_new_end() {
        // Each fi ligature is three bytes and becomes FI (two).
        let (text, selections) = cased("\u{fb01}\u{fb01}\u{fb01}", &[(3, 3), (6, 9)], CaseKind::Upper).unwrap();
        assert_eq!(text, "FIFIFI");
        assert_eq!(selections, vec![(2, 2), (6, 6)]);
        // Capital dotted I (two bytes) lowers to i plus a combining dot (three).
        let (text, selections) = cased("\u{130}\u{130}", &[(0, 0), (2, 4)], CaseKind::Lower).unwrap();
        assert_eq!(text, "i\u{307}i\u{307}");
        assert_eq!(selections, vec![(0, 0), (6, 6)]);
    }

    #[test]
    fn caret_after_an_apostrophe_capitalizes_the_whole_word() {
        let (text, selections) = cased("don't", &[(4, 4)], CaseKind::Capitalize).unwrap();
        assert_eq!(text, "Don't");
        assert_eq!(selections, vec![(4, 4)]);
        let (text, _) = cased("say don't stop", &[(7, 7)], CaseKind::Capitalize).unwrap();
        assert_eq!(text, "say Don't stop");
        let (text, _) = cased("DON'T", &[(0, 0)], CaseKind::Lower).unwrap();
        assert_eq!(text, "don't");
    }

    #[test]
    fn several_cursors_each_change_their_word() {
        let (text, selections) = cased("one two three", &[(1, 1), (5, 5), (9, 9)], CaseKind::Upper).unwrap();
        assert_eq!(text, "ONE TWO THREE");
        assert_eq!(selections, vec![(1, 1), (5, 5), (9, 9)]);
        // Two cursors in one word share one edit.
        let (text, _) = cased("one two", &[(1, 1), (2, 2)], CaseKind::Upper).unwrap();
        assert_eq!(text, "ONE two");
    }

    #[test]
    fn sort_the_selected_lines_by_unicode_order() {
        let (text, selections) = sorted("d\nb\nB\nc\na", &[(2, 7)]).unwrap();
        assert_eq!(text, "d\nB\nb\nc\na");
        assert_eq!(selections, vec![(2, 7)]);
        let (text, _) = sorted("pear\nApple\napple\nzebra\n", &[(0, 22)]).unwrap();
        assert_eq!(text, "Apple\napple\npear\nzebra\n");
    }

    #[test]
    fn sort_keeps_line_endings_and_the_last_line_without_one() {
        let (text, _) = sorted("c\r\nb\r\na", &[(0, 7)]).unwrap();
        assert_eq!(text, "a\r\nb\r\nc");
    }

    #[test]
    fn sort_touches_whole_lines_from_a_partial_selection() {
        // The selection starts and ends mid-line.
        let (text, selections) = sorted("c\nb\na\nz", &[(1, 5)]).unwrap();
        assert_eq!(text, "a\nb\nc\nz");
        assert_eq!(selections, vec![(0, 5)]);
        // Ending at the start of a row leaves that row alone.
        let (text, _) = sorted("c\nb\na\nz", &[(0, 4)]).unwrap();
        assert_eq!(text, "b\nc\na\nz");
    }

    #[test]
    fn sort_does_nothing_for_one_line_or_sorted_lines() {
        assert!(sorted("b\na", &[(0, 0)]).is_none());
        assert!(sorted("a\nb\nc", &[(0, 5)]).is_none());
    }

    #[test]
    fn separate_selections_sort_separately() {
        let (text, _) = sorted("b\na\nz\ny\nx", &[(0, 3), (6, 9)]).unwrap();
        assert_eq!(text, "a\nb\nz\nx\ny");
    }

    fn sorted_by(text: &str, selections: &[(usize, usize)], mode: SortMode) -> Option<(String, Vec<(usize, usize)>)> {
        let plan = plan_sort_lines(&Rope::from(text), selections, mode)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    #[test]
    fn sort_descending_puts_the_touched_lines_in_reverse_unicode_order() {
        let (text, selections) = sorted_by("d\nb\nB\nc\na", &[(2, 7)], SortMode::Descending).unwrap();
        assert_eq!(text, "d\nc\nb\nB\na");
        assert_eq!(selections, vec![(2, 7)]);
        let (text, _) = sorted_by("a\r\nb\r\nc", &[(0, 7)], SortMode::Descending).unwrap();
        assert_eq!(text, "c\r\nb\r\na");
        let (text, _) = sorted_by("a\nb\nm\nx\ny", &[(0, 3), (6, 9)], SortMode::Descending).unwrap();
        assert_eq!(text, "b\na\nm\ny\nx");
        assert!(sorted_by("b\na", &[(0, 0)], SortMode::Descending).is_none());
        assert!(sorted_by("c\nb\na", &[(0, 5)], SortMode::Descending).is_none());
    }

    #[test]
    fn sort_ignoring_case_keeps_the_order_of_lines_that_differ_only_in_case() {
        let (text, _) = sorted_by("B\nb\nA\na\nc", &[(0, 9)], SortMode::IgnoringCase).unwrap();
        assert_eq!(text, "A\na\nB\nb\nc");
        let (text, _) = sorted_by("c\r\nB\r\na", &[(0, 7)], SortMode::IgnoringCase).unwrap();
        assert_eq!(text, "a\r\nB\r\nc");
        // Already in order ignoring case, though plain order would move "a" below "A".
        assert!(sorted_by("a\nA\nb", &[(0, 5)], SortMode::IgnoringCase).is_none());
        assert_eq!(sorted_by("a\nA\nb", &[(0, 5)], SortMode::Ascending).unwrap().0, "A\na\nb");
        let (text, _) = sorted_by("b\nA\nz\nY\nx", &[(0, 3), (6, 9)], SortMode::IgnoringCase).unwrap();
        assert_eq!(text, "A\nb\nz\nx\nY");
    }

    fn reversed(text: &str, selections: &[(usize, usize)]) -> Option<(String, Vec<(usize, usize)>)> {
        let plan = plan_reverse_lines(&Rope::from(text), selections)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    #[test]
    fn reverse_the_selected_lines() {
        let (text, selections) = reversed("a\nb\nc\nd\ne", &[(2, 7)]).unwrap();
        assert_eq!(text, "a\nd\nc\nb\ne");
        assert_eq!(selections, vec![(2, 7)]);
        // A selection ending at the start of a row leaves that row alone.
        let (text, _) = reversed("a\nb\nc\n", &[(0, 4)]).unwrap();
        assert_eq!(text, "b\na\nc\n");
    }

    #[test]
    fn reverse_keeps_crlf_and_the_last_line_without_a_newline() {
        let (text, _) = reversed("a\r\nb\r\nc", &[(0, 7)]).unwrap();
        assert_eq!(text, "c\r\nb\r\na");
        let (text, _) = reversed("a\nb\nc\n", &[(0, 6)]).unwrap();
        assert_eq!(text, "c\nb\na\n");
    }

    #[test]
    fn separate_selections_reverse_separately() {
        let (text, _) = reversed("a\nb\nc\nd\ne", &[(0, 3), (6, 9)]).unwrap();
        assert_eq!(text, "b\na\nc\ne\nd");
    }

    #[test]
    fn reverse_does_nothing_for_one_line_or_a_palindrome() {
        assert!(reversed("b\na", &[(0, 0)]).is_none());
        assert!(reversed("a\nb\na", &[(0, 5)]).is_none());
        assert!(reversed("a\na", &[(0, 3)]).is_none());
    }

    fn deduped(text: &str, selections: &[(usize, usize)]) -> Option<(String, Vec<(usize, usize)>)> {
        let plan = plan_remove_duplicate_lines(&Rope::from(text), selections)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    #[test]
    fn duplicates_go_from_the_whole_file_with_no_selection_and_the_first_stays() {
        let (text, selections) = deduped("a\nb\na\nc\nb\n", &[(0, 0)]).unwrap();
        assert_eq!(text, "a\nb\nc\n");
        assert_eq!(selections, vec![(0, 0)]);
    }

    #[test]
    fn duplicates_go_from_the_touched_lines_only() {
        let (text, selections) = deduped("a\na\nb\nb", &[(0, 3)]).unwrap();
        assert_eq!(text, "a\nb\nb");
        assert_eq!(selections, vec![(0, 2)]);
        // A copy outside the selection is not a repeat of one inside it.
        assert!(deduped("a\nx\na", &[(0, 1), (4, 5)]).is_none());
        let (text, _) = deduped("a\na\nx\nb\nb", &[(0, 3), (6, 9)]).unwrap();
        assert_eq!(text, "a\nx\nb");
    }

    #[test]
    fn dedupe_keeps_crlf_and_the_last_line_without_a_newline() {
        let (text, _) = deduped("a\r\nb\r\na\r\nc", &[(0, 0)]).unwrap();
        assert_eq!(text, "a\r\nb\r\nc");
        let (text, _) = deduped("a\r\nb\r\na", &[(0, 0)]).unwrap();
        assert_eq!(text, "a\r\nb");
        let (text, _) = deduped("a\nb\nb\nb", &[(0, 0)]).unwrap();
        assert_eq!(text, "a\nb");
    }

    #[test]
    fn dedupe_matches_text_exactly_and_does_nothing_without_repeats() {
        assert!(deduped("a\nA\na \n\n", &[(0, 0)]).is_none());
        assert!(deduped("a\nb\nc", &[(0, 5)]).is_none());
        // Two empty lines are a repeat.
        let (text, _) = deduped("a\n\nb\n\n", &[(0, 0)]).unwrap();
        assert_eq!(text, "a\n\nb\n");
    }

    fn unblanked(text: &str, selections: &[(usize, usize)]) -> Option<(String, Vec<(usize, usize)>)> {
        let plan = plan_remove_blank_lines(&Rope::from(text), selections)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    #[test]
    fn blank_lines_go_from_the_whole_file_with_no_selection() {
        let (text, selections) = unblanked("a\n\n  \nb\n\t\n", &[(0, 0)]).unwrap();
        assert_eq!(text, "a\nb\n");
        assert_eq!(selections, vec![(0, 0)]);
        // Every line blank: nothing is left.
        assert_eq!(unblanked("\n\n", &[(0, 0)]).unwrap().0, "");
        assert_eq!(unblanked("  ", &[(0, 0)]).unwrap().0, "");
    }

    #[test]
    fn blank_lines_go_from_the_touched_lines_only() {
        let (text, selections) = unblanked("a\n\nb\n\nc", &[(0, 3)]).unwrap();
        assert_eq!(text, "a\nb\n\nc");
        assert_eq!(selections, vec![(0, 2)]);
        let (text, _) = unblanked("a\n\nb\nc\n\nd", &[(0, 3), (7, 9)]).unwrap();
        assert_eq!(text, "a\nb\nc\nd");
    }

    #[test]
    fn blank_removal_keeps_crlf_and_the_last_line_without_a_newline() {
        let (text, _) = unblanked("a\r\n\r\nb", &[(0, 0)]).unwrap();
        assert_eq!(text, "a\r\nb");
        let (text, _) = unblanked("a\r\nb\r\n \t", &[(0, 0)]).unwrap();
        assert_eq!(text, "a\r\nb");
        let (text, _) = unblanked("a\n\n", &[(0, 0)]).unwrap();
        assert_eq!(text, "a\n");
    }

    #[test]
    fn blank_removal_does_nothing_without_blank_lines_in_reach() {
        assert!(unblanked("a\nb", &[(0, 0)]).is_none());
        assert!(unblanked("", &[(0, 0)]).is_none());
        assert!(unblanked("a\n\nb", &[(0, 1)]).is_none());
    }

    fn trimmed(text: &str, selections: &[(usize, usize)]) -> Option<(String, Vec<(usize, usize)>)> {
        let plan = plan_trim_trailing(&Rope::from(text), selections)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    #[test]
    fn trim_a_selection_inside_one_line() {
        // The selection sits mid-line, so the whole line is trimmed; the caret-free
        // lines around it are left alone.
        let (text, selections) = trimmed("a  \nb \t \nc  ", &[(5, 6)]).unwrap();
        assert_eq!(text, "a  \nb\nc  ");
        assert_eq!(selections, vec![(5, 5)]);
    }

    #[test]
    fn trim_every_line_with_no_selection() {
        let (text, selections) = trimmed("a  \n\t\nb\nc \t", &[(1, 1)]).unwrap();
        assert_eq!(text, "a\n\nb\nc");
        assert_eq!(selections, vec![(1, 1)]);
        // A caret past the cut lands where the cut began.
        let (_, selections) = trimmed("ab   \ncd", &[(4, 4), (8, 8)]).unwrap();
        assert_eq!(selections, vec![(2, 2), (5, 5)]);
    }

    #[test]
    fn trim_keeps_crlf() {
        let (text, _) = trimmed("a \r\nb\t\r\nc", &[(0, 0)]).unwrap();
        assert_eq!(text, "a\r\nb\r\nc");
    }

    #[test]
    fn trim_only_edits_changed_lines_and_nothing_when_clean() {
        let plan = plan_trim_trailing(&Rope::from("a\nb  \nc\n"), &[(0, 0)]).unwrap();
        assert_eq!(plan.edits, vec![(3..5, String::new())]);
        assert!(trimmed("a\nb\n\nc", &[(0, 0)]).is_none());
        assert!(trimmed("a\nb  \nc", &[(0, 1)]).is_none());
    }

    fn converted(text: &str, selections: &[(usize, usize)], to_tabs: bool) -> Option<(String, Vec<(usize, usize)>)> {
        let plan = plan_convert_indent(&Rope::from(text), selections, 4, to_tabs)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    #[test]
    fn spaces_become_tabs_with_the_remainder_kept() {
        let (text, selections) = converted("fn a() {\n    b();\n        c();\n      d();\n}\n", &[(0, 0)], true).unwrap();
        assert_eq!(text, "fn a() {\n\tb();\n\t\tc();\n\t  d();\n}\n");
        assert_eq!(selections, vec![(0, 0)]);
    }

    #[test]
    fn tabs_become_spaces() {
        let (text, _) = converted("a\n\tb\n\t\tc\n", &[(0, 0)], false).unwrap();
        assert_eq!(text, "a\n    b\n        c\n");
    }

    #[test]
    fn mixed_runs_convert_by_visual_column() {
        // A tab, then two spaces is six columns; two spaces then a tab is four.
        let (text, _) = converted("\t  a\n  \tb\n", &[(0, 0)], false).unwrap();
        assert_eq!(text, "      a\n    b\n");
        let (text, _) = converted("\t  a\n  \tb\n", &[(0, 0)], true).unwrap();
        assert_eq!(text, "\t  a\n\tb\n");
    }

    #[test]
    fn alignment_spaces_after_code_stay() {
        let (text, _) = converted("    let x = 1;   // note\n    foo(a,\n        b)\n", &[(0, 0)], true).unwrap();
        assert_eq!(text, "\tlet x = 1;   // note\n\tfoo(a,\n\t\tb)\n");
        let (text, _) = converted("\tx   y\n", &[(0, 0)], false).unwrap();
        assert_eq!(text, "    x   y\n");
    }

    #[test]
    fn convert_keeps_crlf_and_does_nothing_when_already_converted() {
        let (text, _) = converted("    a\r\n    b\r\n", &[(0, 0)], true).unwrap();
        assert_eq!(text, "\ta\r\n\tb\r\n");
        assert!(converted("\ta\n\tb\n", &[(0, 0)], true).is_none());
        assert!(converted("a\n b", &[(0, 0)], false).is_none());
    }

    #[test]
    fn convert_a_selection_touches_only_its_lines_and_carets_follow() {
        let text = "    a\n    b\n    c\n";
        let (out, selections) = converted(text, &[(6, 11)], true).unwrap();
        assert_eq!(out, "    a\n\tb\n    c\n");
        assert_eq!(selections, vec![(6, 8)]);
        // A caret inside the run lands after the characters drawing its columns.
        let (_, selections) = converted("      a\n", &[(2, 2)], true).unwrap();
        assert_eq!(selections, vec![(1, 1)]);
        let (_, selections) = converted("\t\ta\n", &[(1, 1)], false).unwrap();
        assert_eq!(selections, vec![(4, 4)]);
    }

    struct Harness(gpui::Entity<crate::input::EditorState>);

    impl gpui::Render for Harness {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl gpui::IntoElement {
            gpui::div().size_full().child(self.0.clone())
        }
    }

    /// A key test window holding a Rust editor with `text` and the given selection.
    fn lift_editor(
        cx: &mut gpui::TestAppContext,
        text: &str,
        selection: std::ops::Range<usize>,
    ) -> (gpui::Entity<crate::input::EditorState>, gpui::AnyWindowHandle) {
        use gpui::AppContext as _;
        cx.update(crate::init);
        let mut editor = None;
        let window = cx.open_window(gpui::size(gpui::px(240.), gpui::px(140.)), |window, cx| {
            let state = cx.new(|cx| crate::input::EditorState::new(window, cx).default_value(text));
            editor = Some(state.clone());
            Harness(state)
        });
        let editor = editor.unwrap();
        let window: gpui::AnyWindowHandle = window.into();
        window
            .update(cx, |_, window, cx| {
                // A test window is not the active one; the lift needs a key window.
                crate::input::set_draw_as_key(true);
                assert!(crate::input::window_key(window));
                editor.update(cx, |state, cx| {
                    state.set_highlighter("rust", cx);
                    state.set_selected_range(selection, cx);
                });
            })
            .unwrap();
        (editor, window)
    }

    /// Run `command`, then say whether the lift is set, and when set, that it plays out: it
    /// steps to a level at once and to `None` after its length.
    fn lifted(
        cx: &mut gpui::TestAppContext,
        text: &str,
        selection: std::ops::Range<usize>,
        command: impl FnOnce(&mut crate::input::EditorState, &mut Window, &mut Context<crate::input::EditorState>),
    ) -> bool {
        use std::time::{Duration, Instant};
        let (editor, window) = lift_editor(cx, text, selection);
        window.update(cx, |_, window, cx| editor.update(cx, |state, cx| command(state, window, cx))).unwrap();
        window
            .update(cx, |_, _, cx| {
                editor.update(cx, |state, _| {
                    if state.lift.is_none() {
                        return false;
                    }
                    let now = Instant::now();
                    assert!(state.step_lift(now).is_some());
                    assert!(state.step_lift(now + Duration::from_millis(100)).is_some());
                    assert!(state.step_lift(now + Duration::from_millis(300)).is_none());
                    assert!(state.lift.is_none());
                    true
                })
            })
            .unwrap()
    }

    #[gpui::test]
    fn transformations_over_a_selection_lift_it_once(cx: &mut gpui::TestAppContext) {
        let text = "pear\napple\nfig\n";
        assert!(lifted(cx, text, 0..14, |s, w, cx| s.sort_lines(&SortLines, w, cx)));
        assert!(lifted(cx, text, 0..4, |s, w, cx| s.make_upper_case(&MakeUpperCase, w, cx)));
        assert!(lifted(cx, "PEAR\n", 0..4, |s, w, cx| s.make_lower_case(&MakeLowerCase, w, cx)));
        assert!(lifted(cx, "pear\n", 0..4, |s, w, cx| s.capitalize(&Capitalize, w, cx)));
    }

    #[gpui::test]
    fn a_transformation_that_changes_nothing_or_a_caret_sets_none(cx: &mut gpui::TestAppContext) {
        assert!(!lifted(cx, "a\nb\nc\n", 0..5, |s, w, cx| s.sort_lines(&SortLines, w, cx)));
        assert!(!lifted(cx, "PEAR\n", 0..4, |s, w, cx| s.make_upper_case(&MakeUpperCase, w, cx)));
        assert!(!lifted(cx, "pear\n", 0..0, |s, w, cx| s.make_upper_case(&MakeUpperCase, w, cx)));
    }

    #[gpui::test]
    fn the_lift_ends_at_the_next_edit(cx: &mut gpui::TestAppContext) {
        let (editor, window) = lift_editor(cx, "pear\napple\n", 0..10);
        window
            .update(cx, |_, window, cx| {
                editor.update(cx, |state, cx| {
                    state.sort_lines(&SortLines, window, cx);
                    assert!(state.lift.is_some());
                    state.replace_text_in_range(Some(0..0), "x", window, cx);
                    assert!(state.step_lift(std::time::Instant::now()).is_none());
                });
            })
            .unwrap();
    }

    #[gpui::test]
    fn toggle_comment_and_indent_lift_over_several_rows_only(cx: &mut gpui::TestAppContext) {
        let text = "a\nb\nc\n";
        assert!(lifted(cx, text, 0..6, |s, w, cx| s.toggle_comments(&crate::input::ToggleComments, w, cx)));
        assert!(!lifted(cx, text, 0..1, |s, w, cx| s.toggle_comments(&crate::input::ToggleComments, w, cx)));
        assert!(lifted(cx, text, 0..6, |s, w, cx| s.indent(true, w, cx)));
        assert!(lifted(cx, "  a\n  b\n  c\n", 0..12, |s, w, cx| s.outdent(true, w, cx)));
        assert!(!lifted(cx, text, 0..1, |s, w, cx| s.indent(true, w, cx)));
        assert!(!lifted(cx, text, 0..0, |s, w, cx| s.indent(false, w, cx)));
    }

    fn stepped(text: &str, selections: &[(usize, usize)], delta: i64) -> Option<(String, Vec<(usize, usize)>)> {
        let carets: Vec<usize> = selections.iter().map(|&(_, end)| end).collect();
        let plan = plan_step(&Rope::from(text), selections, &carets, delta)?;
        Some((apply(text, &plan), plan.selections.clone()))
    }

    /// Step the token at the end of `text` (caret there) and return the new text.
    fn step_end(text: &str, delta: i64) -> Option<String> {
        stepped(text, &[(text.len(), text.len())], delta).map(|(text, _)| text)
    }

    #[test]
    fn numbers_keep_width_and_leading_zeros() {
        assert_eq!(step_end("007", 1).unwrap(), "008");
        assert_eq!(step_end("009", 1).unwrap(), "010");
        assert_eq!(step_end("099", 1).unwrap(), "100");
        assert_eq!(step_end("010", -1).unwrap(), "009");
        assert_eq!(step_end("9", 1).unwrap(), "10");
        assert_eq!(step_end("7", 10).unwrap(), "17");
    }

    #[test]
    fn numbers_cross_zero() {
        assert_eq!(step_end("-1", 1).unwrap(), "0");
        assert_eq!(step_end("0", 1).unwrap(), "1");
        assert_eq!(step_end("0", -1).unwrap(), "-1");
        assert_eq!(step_end("x = -5", 10).unwrap(), "x = 5");
        assert_eq!(step_end("a-1", 1).unwrap(), "a-2");
    }

    #[test]
    fn decimals_keep_their_decimals() {
        assert_eq!(step_end("1.50", 1).unwrap(), "2.50");
        assert_eq!(step_end("0.99", -1).unwrap(), "-0.01");
        assert_eq!(step_end("-1.5", 1).unwrap(), "-0.5");
        // A caret in the decimals steps the whole part.
        assert_eq!(stepped("1.50", &[(3, 3)], 1).unwrap().0, "2.50");
    }

    #[test]
    fn hex_keeps_width_and_case() {
        assert_eq!(step_end("0x0f", 1).unwrap(), "0x10");
        assert_eq!(step_end("0xFF", 1).unwrap(), "0x100");
        assert_eq!(step_end("0xAb", -1).unwrap(), "0xAA");
        assert_eq!(step_end("0x10", -1).unwrap(), "0x0f");
        assert_eq!(step_end("0x00", -1), None);
    }

    #[test]
    fn words_flip_in_their_case() {
        assert_eq!(step_end("true", 1).unwrap(), "false");
        assert_eq!(step_end("True", -1).unwrap(), "False");
        assert_eq!(step_end("TRUE", 1).unwrap(), "FALSE");
        assert_eq!(step_end("Napoléon", 1), None);
        assert_eq!(step_end("yes", 1).unwrap(), "no");
        assert_eq!(step_end("No", 1).unwrap(), "Yes");
        assert_eq!(step_end("off", -1).unwrap(), "on");
        assert_eq!(step_end("ON", 1).unwrap(), "OFF");
        assert_eq!(step_end("true_value", 1), None);
    }

    #[test]
    fn caret_on_or_after_the_token_only() {
        assert_eq!(stepped("a 12 b", &[(2, 2)], 1).unwrap().0, "a 13 b");
        assert_eq!(stepped("a 12 b", &[(3, 3)], 1).unwrap().0, "a 13 b");
        assert_eq!(stepped("a 12 b", &[(4, 4)], 1).unwrap().0, "a 13 b");
        assert!(stepped("a 12 b", &[(0, 0)], 1).is_none());
        assert!(stepped("a 12 b", &[(6, 6)], 1).is_none());
        assert!(stepped("no number here", &[(3, 3)], 1).is_none());
    }

    #[test]
    fn every_caret_steps_and_a_shared_token_once() {
        let (text, selections) = stepped("9 99\nx 5", &[(1, 1), (4, 4), (8, 8)], 1).unwrap();
        assert_eq!(text, "10 100\nx 6");
        assert_eq!(selections, vec![(2, 2), (6, 6), (10, 10)]);
        let (text, _) = stepped("12", &[(0, 0), (2, 2)], 1).unwrap();
        assert_eq!(text, "13");
    }
}
