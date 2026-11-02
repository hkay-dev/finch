//! Matching brackets: the pair next to the caret is highlighted and
//! `MoveToEnclosingBracket` jumps between its two halves.
//!
//! The partner is found by a depth-counting scan over the rope's bytes that
//! counts only brackets the highlighter's parse calls code, so a `)` inside a
//! string or comment never pairs with a `(` in code, and a pair of any width
//! matches (the scan is capped at [`MAX_SCAN`] bytes from the bracket). The
//! brackets of each [`WINDOW`] of the scan are classified in one call, and
//! the parse is the highlighter's own, never a new one. Documents over
//! [`SYNTAX_MAX_BYTES`], or with no parse, count every bracket as code. The
//! pairs are found when the selection or the text changes, not per frame:
//! [`InputBaseState::refresh_bracket_pairs`] compares a small key and returns
//! at once while nothing changed.

use gpui::{Context, Window};
use ropey::Rope;

use crate::input::{
    InputBaseState, InputModeKind, MoveToEnclosingBracket, RopeExt as _, SelectToEnclosingBracket, SyntaxContext, cursor::CursorSelection,
};

/// The farthest a partner is searched for, in bytes from the caret bracket.
const MAX_SCAN: usize = 1 << 20;

/// How many bytes of the scan have their brackets classified in one call.
const WINDOW: usize = 16 * 1024;

/// Above this many bytes the syntax is not consulted and every bracket counts:
/// classifying an outermost brace of dense JSON takes about 2.7 ms at this size
/// and grows in step with it.
const SYNTAX_MAX_BYTES: usize = 64 * 1024;

/// The bracket pairs next to the carets, with the state they were found for.
pub(super) struct BracketCache {
    revision: u64,
    len: usize,
    /// The parse generation the pairs were classified against, if any.
    parse: Option<u64>,
    /// `(start, end, cursor)` of each selection: a selection that collapses
    /// onto its own end keeps the cursor but not the pairs.
    carets: Vec<(usize, usize, usize)>,
    /// `(bracket, partner)` byte offsets of each bracket next to an empty caret.
    pairs: Vec<(usize, usize)>,
}

/// `(open, close, is_open)` for a bracket byte.
fn bracket_kind(byte: u8) -> Option<(u8, u8, bool)> {
    match byte {
        b'(' => Some((b'(', b')', true)),
        b')' => Some((b'(', b')', false)),
        b'[' => Some((b'[', b']', true)),
        b']' => Some((b'[', b']', false)),
        b'{' => Some((b'{', b'}', true)),
        b'}' => Some((b'{', b'}', false)),
        _ => None,
    }
}

/// Says, for each offset, whether it is code: appends one flag per offset.
type Classify<'a> = &'a mut dyn FnMut(&[usize], &mut Vec<bool>);

/// Scans up to `limit` bytes from `*at`, forward or backward, over the rope's
/// chunk slices, calling `found(offset, byte)` for each byte `wanted` accepts
/// (in scan order) and moving `*at` past what it scanned. Returns the number
/// of bytes scanned, less than `limit` only when the text ran out.
fn scan_window(
    text: &Rope,
    at: &mut usize,
    forward: bool,
    limit: usize,
    wanted: impl Fn(u8) -> bool,
    found: &mut dyn FnMut(usize, u8),
) -> usize {
    let mut left = limit;
    while left > 0 {
        if forward {
            if *at >= text.len() {
                break;
            }
            let (chunk, start) = text.chunk(*at);
            let bytes = &chunk.as_bytes()[*at - start..];
            let take = left.min(bytes.len());
            for (i, &b) in bytes[..take].iter().enumerate() {
                if wanted(b) {
                    found(*at + i, b);
                }
            }
            *at += take;
            left -= take;
        } else {
            if *at == 0 {
                break;
            }
            let (chunk, start) = text.chunk(*at - 1);
            let bytes = &chunk.as_bytes()[..*at - start];
            let take = left.min(bytes.len());
            let from = bytes.len() - take;
            for (i, &b) in bytes[from..].iter().enumerate().rev() {
                if wanted(b) {
                    found(*at - take + i, b);
                }
            }
            *at -= take;
            left -= take;
        }
    }
    limit - left
}

/// The offset of the bracket matching the one at byte `pos`, if there is one
/// within [`MAX_SCAN`] bytes. The brackets of one [`WINDOW`] of the scan are
/// classified together, so the syntax is looked up once per window.
fn partner_of(text: &Rope, pos: usize, byte: u8, classify: Classify) -> Option<usize> {
    let (open, close, is_open) = bracket_kind(byte)?;
    let mut flags = Vec::new();
    classify(&[pos], &mut flags);
    if !flags.first().copied().unwrap_or(true) {
        return None;
    }
    // Scanning forward, an `open` nests and a `close` ends; backward, the reverse.
    let (nest, end) = if is_open { (open, close) } else { (close, open) };
    let mut at = if is_open { pos + 1 } else { pos };
    let mut depth = 1usize;
    let mut scanned = 0usize;
    let mut exhausted = false;
    let mut offsets = Vec::new();
    let mut nests = Vec::new();
    while !exhausted && scanned < MAX_SCAN {
        offsets.clear();
        nests.clear();
        let limit = WINDOW.min(MAX_SCAN - scanned);
        let done = scan_window(text, &mut at, is_open, limit, |b| b == nest || b == end, &mut |offset, b| {
            offsets.push(offset);
            nests.push(b == nest);
        });
        exhausted = done < limit;
        scanned += done;
        if offsets.is_empty() {
            continue;
        }
        flags.clear();
        classify(&offsets, &mut flags);
        for (index, &offset) in offsets.iter().enumerate() {
            if !flags[index] {
                continue;
            }
            if nests[index] {
                depth += 1;
            } else {
                depth -= 1;
                if depth == 0 {
                    return Some(offset);
                }
            }
        }
    }
    None
}

/// The bracket next to `caret` (the one after it first, then the one before)
/// with its partner, as `(bracket, partner)`.
fn pair_at_caret(text: &Rope, caret: usize, classify: Classify) -> Option<(usize, usize)> {
    if caret > text.len() {
        return None;
    }
    if let Some(byte) = text.bytes_at(caret).next()
        && let Some(partner) = partner_of(text, caret, byte, classify)
    {
        return Some((caret, partner));
    }
    if caret > 0
        && let Some(byte) = text.bytes_at(caret).prev()
        && let Some(partner) = partner_of(text, caret - 1, byte, classify)
    {
        return Some((caret - 1, partner));
    }
    None
}

/// Where the caret lands to jump to the partner: the same side of the
/// partner as it was on the bracket, so a second jump comes back.
fn jump_target(caret: usize, bracket: usize, partner: usize) -> usize {
    if caret == bracket { partner + 1 } else { partner }
}

/// The innermost bracket pair around the text `start..end`, as
/// `(open, close)` offsets: scanning back from `start`, the first opening
/// bracket that nothing closes before `start` and whose partner is at or past
/// `end`. A pair that closes inside the selection is skipped, so a selection
/// that straddles one still finds the pair around it. Brackets that are not
/// code are ignored, and the scan is capped like [`partner_of`].
fn enclosing_pair(text: &Rope, start: usize, end: usize, classify: Classify) -> Option<(usize, usize)> {
    let mut at = start;
    let mut depth = 0usize;
    let mut scanned = 0usize;
    let mut exhausted = false;
    let mut found: Vec<(usize, u8)> = Vec::new();
    let mut offsets = Vec::new();
    let mut flags = Vec::new();
    while !exhausted && scanned < MAX_SCAN {
        found.clear();
        let limit = WINDOW.min(MAX_SCAN - scanned);
        let done = scan_window(text, &mut at, false, limit, |b| bracket_kind(b).is_some(), &mut |offset, b| {
            found.push((offset, b));
        });
        exhausted = done < limit;
        scanned += done;
        if found.is_empty() {
            continue;
        }
        offsets.clear();
        offsets.extend(found.iter().map(|&(offset, _)| offset));
        flags.clear();
        classify(&offsets, &mut flags);
        for (index, &(offset, byte)) in found.iter().enumerate() {
            let Some((_, _, is_open)) = bracket_kind(byte) else { continue };
            if !flags[index] {
                continue;
            }
            if !is_open {
                depth += 1;
            } else if depth > 0 {
                depth -= 1;
            } else if let Some(partner) = partner_of(text, offset, byte, classify)
                && partner >= end
            {
                return Some((offset, partner));
            }
        }
    }
    None
}

/// The bracket pair Remove surrounding takes: the selection's own first and
/// last characters when they are a pair (`partner_of` the first is the last),
/// else the innermost pair around it. Brackets that are not code are ignored.
pub(super) fn surrounding_pair(text: &Rope, start: usize, end: usize, classify: Classify) -> Option<(usize, usize)> {
    if end > start + 1
        && let Some(open) = text.char_at(start).filter(|c| c.is_ascii() && bracket_kind(*c as u8).is_some_and(|(_, _, is_open)| is_open))
        && partner_of(text, start, open as u8, classify) == Some(end - 1)
    {
        return Some((start, end - 1));
    }
    enclosing_pair(text, start, end, classify)
}

/// Where `select inside brackets` takes a selection, `None` when no pair
/// encloses it: the inside of the pair around it, or the pair with its
/// brackets when the selection already is that inside.
fn select_enclosing(text: &Rope, (start, end): (usize, usize), classify: Classify) -> Option<(usize, usize)> {
    let (open, close) = enclosing_pair(text, start, end, classify)?;
    if (start, end) == (open + 1, close) {
        Some((open, close + 1))
    } else {
        Some((open + 1, close))
    }
}

impl<M: InputModeKind> InputBaseState<M> {
    fn bracket_pair_at(&self, caret: usize) -> Option<(usize, usize)> {
        pair_at_caret(&self.text, caret, &mut |offsets, flags| {
            self.classify_code(offsets, flags)
        })
    }

    /// The bracket pair Remove surrounding takes around `start..end`, found with the syntax.
    pub(super) fn surrounding_bracket_pair(&self, start: usize, end: usize) -> Option<(usize, usize)> {
        surrounding_pair(&self.text, start, end, &mut |offsets, flags| self.classify_code(offsets, flags))
    }

    /// The generation of the parse brackets are classified against, or `None`
    /// when there is none or the document is over [`SYNTAX_MAX_BYTES`].
    fn parse_key(&self) -> Option<u64> {
        if self.text.len() > SYNTAX_MAX_BYTES {
            return None;
        }
        self.mode
            .highlighter()
            .and_then(|highlighter| highlighter.borrow().as_ref()?.syntax_generation())
    }

    /// Say for each of `offsets` whether it is code, from the highlighter's
    /// own parse (no new parse). Everything counts as code when there is no
    /// parse, or when the document is over [`SYNTAX_MAX_BYTES`].
    fn classify_code(&self, offsets: &[usize], flags: &mut Vec<bool>) {
        if self.text.len() <= SYNTAX_MAX_BYTES
            && let Some(highlighter) = self.mode.highlighter()
            && let Some(highlighter) = highlighter.borrow().as_ref()
        {
            let mut contexts = Vec::with_capacity(offsets.len());
            if highlighter.syntax_contexts(offsets, &mut contexts) {
                flags.extend(contexts.iter().map(|context| *context == SyntaxContext::Code));
                return;
            }
        }
        flags.extend(offsets.iter().map(|_| true));
    }

    /// Find the bracket pairs next to the carets again when the text or a
    /// caret moved since they were last found, or the parse arrived. Cheap
    /// when nothing changed.
    pub(crate) fn refresh_bracket_pairs(&mut self) {
        if !self.is_multi_line() {
            return;
        }
        let key = |sel: &CursorSelection| (sel.start, sel.end, sel.cursor_offset());
        let parse = self.parse_key();
        let fresh = self.bracket_cache.as_ref().is_some_and(|cache| {
            cache.revision == self.document_revision
                && cache.len == self.text.len()
                && cache.parse == parse
                && cache.carets.len() == self.selections.iter().count()
                && self
                    .selections
                    .iter()
                    .zip(&cache.carets)
                    .all(|(sel, caret)| key(sel) == *caret)
        });
        if fresh {
            return;
        }
        let carets: Vec<_> = self.selections.iter().map(key).collect();
        let pairs = self
            .selections
            .iter()
            .filter(|sel| sel.start == sel.end)
            .filter_map(|sel| self.bracket_pair_at(sel.cursor_offset()))
            .collect();
        self.bracket_cache = Some(Box::new(BracketCache {
            revision: self.document_revision,
            len: self.text.len(),
            parse,
            carets,
            pairs,
        }));
    }

    /// The `(bracket, partner)` byte offsets to highlight, as of the last
    /// [`Self::refresh_bracket_pairs`].
    pub(crate) fn bracket_pairs(&self) -> &[(usize, usize)] {
        self.bracket_cache.as_ref().map_or(&[], |cache| &cache.pairs)
    }

    pub(super) fn move_to_enclosing_bracket(
        &mut self,
        _: &MoveToEnclosingBracket,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_multi_line() {
            return;
        }
        self.move_all_cursors(
            |s, sel| {
                let caret = sel.cursor_offset();
                let target = s
                    .bracket_pair_at(caret)
                    .map_or(caret, |(bracket, partner)| jump_target(caret, bracket, partner));
                (target, s.preferred_column_for(target), false)
            },
            None,
            window,
            cx,
        );
    }

    /// Select the text inside the bracket pair around each selection; one that
    /// already is a pair's inside takes in the brackets, and one that is a
    /// pair with its brackets goes out to the inside of the next pair.
    pub(super) fn select_to_enclosing_bracket(
        &mut self,
        _: &SelectToEnclosingBracket,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.is_multi_line() {
            return;
        }
        let planned: Vec<Option<(usize, usize)>> = self
            .selections
            .iter()
            .map(|sel| {
                select_enclosing(&self.text, (sel.start, sel.end), &mut |offsets, flags| {
                    self.classify_code(offsets, flags)
                })
            })
            .collect();
        self.select_planned_ranges(|_, _| planned, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair_with(
        rope: &Rope,
        caret: usize,
        is_code: &dyn Fn(usize) -> bool,
    ) -> Option<(usize, usize)> {
        pair_at_caret(rope, caret, &mut |offsets, flags| {
            flags.extend(offsets.iter().map(|&offset| is_code(offset)));
        })
    }

    fn pair(text: &str, caret: usize) -> Option<(usize, usize)> {
        pair_with(&Rope::from_str(text), caret, &|_| true)
    }

    #[test]
    fn matches_nested_pairs_from_either_side() {
        let text = "f(a, (b), [c]) {}";
        assert_eq!(pair(text, 1), Some((1, 13)));
        assert_eq!(pair(text, 2), Some((1, 13)));
        assert_eq!(pair(text, 14), Some((13, 1)));
        assert_eq!(pair(text, 8), Some((7, 5)));
        assert_eq!(pair(text, 15), Some((15, 16)));
        assert_eq!(pair(text, 16), Some((16, 15)));
    }

    #[test]
    fn no_pair_for_unmatched_or_plain_text() {
        assert_eq!(pair("a ( b", 2), None);
        assert_eq!(pair("a ( b", 3), None);
        assert_eq!(pair("plain", 2), None);
        assert_eq!(pair("", 0), None);
        assert_eq!(pair("(x]", 0), None);
    }

    #[test]
    fn skips_brackets_the_context_calls_not_code() {
        let text = r#"{ "}" } "#;
        let rope = Rope::from_str(text);
        // Bytes 2..5 are the string "}".
        let is_code = |offset: usize| !(2..5).contains(&offset);
        assert_eq!(pair_with(&rope, 0, &is_code), Some((0, 6)));
        assert_eq!(pair_with(&rope, 7, &is_code), Some((6, 0)));
        // A bracket inside the string pairs with nothing.
        assert_eq!(pair_with(&rope, 3, &is_code), None);
    }

    #[test]
    fn looks_the_syntax_up_once_per_window_not_once_per_bracket() {
        let body = "(x)\n".repeat(100_000);
        let rope = Rope::from_str(&format!("{{\n{body}}}"));
        let mut calls = 0usize;
        let mut classify = |offsets: &[usize], flags: &mut Vec<bool>| {
            calls += 1;
            flags.extend(offsets.iter().map(|_| true));
        };
        let end = rope.len() - 1;
        assert_eq!(pair_at_caret(&rope, 0, &mut classify), Some((0, end)));
        assert!(calls <= rope.len() / WINDOW + 3, "{calls} lookups");
    }

    #[test]
    fn a_pair_wider_than_any_window_still_matches() {
        let body = "x\n".repeat(200_000);
        let text = format!("{{\n{body}}}");
        let end = text.len() - 1;
        assert_eq!(pair(&text, 0), Some((0, end)));
        assert_eq!(pair(&text, text.len()), Some((end, 0)));
    }

    /// The per-byte scan this module used before it read chunk slices, kept to
    /// check the new one against.
    fn old_partner_of(text: &Rope, pos: usize, byte: u8, is_code: &dyn Fn(usize) -> bool) -> Option<usize> {
        let (open, close, is_open) = bracket_kind(byte)?;
        if !is_code(pos) {
            return None;
        }
        let (nest, end) = if is_open { (open, close) } else { (close, open) };
        let mut at = if is_open { pos + 1 } else { pos };
        let mut bytes = text.bytes_at(at);
        let mut depth = 1usize;
        let mut scanned = 0usize;
        while scanned < MAX_SCAN {
            let Some(b) = (if is_open { bytes.next() } else { bytes.prev() }) else { break };
            let offset = if is_open {
                at += 1;
                at - 1
            } else {
                at -= 1;
                at
            };
            scanned += 1;
            if (b == nest || b == end) && is_code(offset) {
                if b == nest {
                    depth += 1;
                } else {
                    depth -= 1;
                    if depth == 0 {
                        return Some(offset);
                    }
                }
            }
        }
        None
    }

    fn new_partner_of(text: &Rope, pos: usize, byte: u8, is_code: &dyn Fn(usize) -> bool) -> Option<usize> {
        partner_of(text, pos, byte, &mut |offsets, flags| {
            flags.extend(offsets.iter().map(|&offset| is_code(offset)));
        })
    }

    /// A mix of brackets, multi-byte characters and newlines, the same every run.
    fn noisy_text(len: usize) -> String {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        let mut out = String::with_capacity(len + 4);
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.push(match state % 14 {
                0 => '(',
                1 => ')',
                2 => '[',
                3 => ']',
                4 => '{',
                5 => '}',
                6 => '\n',
                7 => '\u{e9}',
                8 => '\u{4e2d}',
                _ => 'x',
            });
        }
        out
    }

    #[test]
    fn chunk_scan_matches_the_per_byte_scan_over_many_chunks() {
        let text = noisy_text(300_000);
        let rope = Rope::from_str(&text);
        let is_code = |offset: usize| offset % 7 != 3;
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut checked = 0;
        for _ in 0..4_000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let pos = (state as usize) % text.len();
            let byte = text.as_bytes()[pos];
            if bracket_kind(byte).is_none() {
                continue;
            }
            checked += 1;
            assert_eq!(
                new_partner_of(&rope, pos, byte, &is_code),
                old_partner_of(&rope, pos, byte, &is_code),
                "bracket at {pos}"
            );
        }
        assert!(checked > 500, "{checked} brackets checked");
    }

    #[test]
    fn an_unmatched_close_after_two_megabytes_answers_as_before() {
        let text = format!("{}{}", "x".repeat(2 << 20), ")");
        let rope = Rope::from_str(&text);
        let last = text.len() - 1;
        let code = |_: usize| true;
        let now = std::time::Instant::now();
        let new = new_partner_of(&rope, last, b')', &code);
        let new_time = now.elapsed();
        let now = std::time::Instant::now();
        let old = old_partner_of(&rope, last, b')', &code);
        let old_time = now.elapsed();
        eprintln!("unmatched ) after 2 MiB: chunk scan {new_time:?}, per-byte scan {old_time:?}");
        assert_eq!(new, None);
        assert_eq!(old, None);
        // An opener just inside the cap still pairs; one past it does not.
        let mut near = String::from("(");
        near.push_str(&"x".repeat(MAX_SCAN - 1));
        near.push(')');
        let rope = Rope::from_str(&near);
        assert_eq!(new_partner_of(&rope, near.len() - 1, b')', &code), Some(0));
        let mut far = String::from("(");
        far.push_str(&"x".repeat(MAX_SCAN));
        far.push(')');
        let rope = Rope::from_str(&far);
        assert_eq!(new_partner_of(&rope, far.len() - 1, b')', &code), None);
        assert_eq!(old_partner_of(&rope, far.len() - 1, b')', &code), None);
    }

    #[test]
    fn unicode_and_crlf_do_not_shift_offsets() {
        let text = "(\u{e9}\r\n\u{4e2d})";
        let close = text.len() - 1;
        assert_eq!(pair(text, 0), Some((0, close)));
        assert_eq!(pair(text, text.len()), Some((close, 0)));
    }

    #[test]
    fn jumping_twice_comes_back() {
        let text = "{ a }";
        let (bracket, partner) = pair(text, 0).unwrap();
        let there = jump_target(0, bracket, partner);
        assert_eq!(there, 5);
        let (bracket, partner) = pair(text, there).unwrap();
        assert_eq!(jump_target(there, bracket, partner), 0);
        // Inside: after the open lands before the close, and back.
        let (bracket, partner) = pair(text, 1).unwrap();
        let there = jump_target(1, bracket, partner);
        assert_eq!(there, 4);
        let (bracket, partner) = pair(text, there).unwrap();
        assert_eq!(jump_target(there, bracket, partner), 1);
    }

    fn enclosing(text: &str, selection: (usize, usize), is_code: &dyn Fn(usize) -> bool) -> Option<(usize, usize)> {
        select_enclosing(&Rope::from_str(text), selection, &mut |offsets, flags| {
            flags.extend(offsets.iter().map(|&offset| is_code(offset)));
        })
    }

    #[test]
    fn select_inside_goes_out_one_pair_per_press() {
        let text = "x(a (b) c)";
        let code = |_: usize| true;
        // From a caret inside the inner pair: its inside, its brackets, then the outer pair.
        assert_eq!(enclosing(text, (5, 5), &code), Some((5, 6)));
        assert_eq!(enclosing(text, (5, 6), &code), Some((4, 7)));
        assert_eq!(enclosing(text, (4, 7), &code), Some((2, 9)));
        assert_eq!(enclosing(text, (2, 9), &code), Some((1, 10)));
        // Nothing encloses the outer pair.
        assert_eq!(enclosing(text, (1, 10), &code), None);
        // A caret beside the inner pair is inside the outer one.
        assert_eq!(enclosing(text, (8, 8), &code), Some((2, 9)));
        assert_eq!(enclosing(text, (7, 7), &code), Some((2, 9)));
    }

    #[test]
    fn select_inside_skips_a_pair_the_selection_straddles() {
        let text = "x(a (b) c)";
        let code = |_: usize| true;
        assert_eq!(enclosing(text, (3, 6), &code), Some((2, 9)));
        assert_eq!(enclosing(text, (5, 8), &code), Some((2, 9)));
    }

    #[test]
    fn select_inside_ignores_brackets_in_strings_and_finds_no_pair_in_plain_text() {
        let text = r#"{ "}" a }"#;
        let is_code = |offset: usize| !(2..5).contains(&offset);
        // From inside the string, and from beside it, the braces around it count.
        assert_eq!(enclosing(text, (3, 3), &is_code), Some((1, 8)));
        assert_eq!(enclosing(text, (6, 6), &is_code), Some((1, 8)));
        assert_eq!(enclosing("plain (text", (2, 2), &|_| true), None);
        assert_eq!(enclosing("a ( b", (4, 4), &|_| true), None);
        assert_eq!(enclosing("", (0, 0), &|_| true), None);
    }

    #[test]
    fn select_inside_mixes_bracket_kinds() {
        let text = "f({a: [1, 2]})";
        let code = |_: usize| true;
        // Caret after the 1: the square pair, then the braces, then the parens.
        assert_eq!(enclosing(text, (8, 8), &code), Some((7, 11)));
        assert_eq!(enclosing(text, (7, 11), &code), Some((6, 12)));
        assert_eq!(enclosing(text, (6, 12), &code), Some((3, 12)));
    }
}
