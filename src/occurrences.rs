//! Occurrence marks: rest the caret on a name and, after a beat, every other whole-word copy of it
//! in the window on screen gets a very faint wash. Nothing moves or selects; Control-Cmd-G
//! (Edit > Select all matches) selects them all.
//!
//! A layer on the shared mark driver (`marks.rs`), in the range-fill slot: a `RangeDecoration`
//! fill per copy, geometric, so text runs are untouched. The word comes from the primary caret
//! (or a selection that is exactly one word) with one caret only, 2 to 64 characters. The scan
//! reads only the rows the driver hands over (the screen and a screen each way, further capped
//! around the caret for a huge line), runs on the background executor after `WAIT`, and never
//! touches Find's `search_session` matcher, so Find's state, its marks and the menu flags stay
//! as they are. Any edit, or a caret move to another word, clears the marks at once and marks
//! again after the pause; moving inside the same word, or scrolling within the window, does
//! nothing. While Find is open nothing is marked.
//!
//! Case follows Select all matches (and Cmd-D): a word with a capital matches case, a lowercase
//! one matches any case.

use std::{cell::{Cell, RefCell}, ops::Range, rc::Rc, time::Duration};

use gpui_kit::component::input::{EditorState, RangeDecoration, RangeDecorationStyle, Rope};
use gpui_kit::*;

use crate::{
    marks::{self, Debounce, MarkLayer, RangeSet, View},
    theme,
};

/// How long the caret rests on a word before its copies are marked.
const WAIT: Duration = Duration::from_millis(400);

/// A word shorter or longer than this (in characters) is not marked.
const MIN_CHARS: usize = 2;
const MAX_CHARS: usize = 64;

/// The most copies marked.
const MAX_MARKS: usize = 2000;

/// How far from the caret a window is scanned, each way, so a single huge line costs a bounded scan.
const SCAN_BYTES: usize = 1 << 20;

/// The wash: the ink at this strength.
const WASH: f32 = 0.09;

/// What the layer last asked for: the word, where it sits, the text revision and the scanned bytes.
type Want = (String, Range<usize>, u64, Range<usize>);

/// The occurrence layer of one editor.
pub struct Layer {
    set: Rc<RefCell<RangeSet>>,
    job: Debounce,
    want: Option<Want>,
    /// Wakes the driver when Find opens or closes (the driver's key doesn't see that).
    _watch: Subscription,
}

impl Layer {
    pub fn new(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        let set = Rc::new(RefCell::new(RangeSet::new(editor, cx)));
        let seen = Cell::new(false);
        let _watch = cx.observe(editor, move |editor, cx| {
            let now = find_open(editor.read(cx));
            if seen.replace(now) != now {
                marks::with_layer(&editor, cx, |_: &mut Layer| ((), true));
            }
        });
        Self { set, job: Debounce::default(), want: None, _watch }
    }

    fn clear(&mut self, cx: &mut App) {
        self.job.cancel();
        self.want = None;
        self.set.borrow_mut().set_if_changed(Vec::new(), cx);
    }
}

fn find_open(state: &EditorState) -> bool {
    let session = state.search_session();
    session.open || session.is_active()
}

impl MarkLayer for Layer {
    fn active(&self) -> bool {
        true
    }

    fn release(&mut self) {
        // What the layer asked last and its copy of the marks go; the marks on screen stay, and the
        // next wake (the show, or a caret move) asks and scans again.
        self.want = None;
        self.set.borrow_mut().release();
    }

    fn held(&self) -> usize {
        self.set.borrow().held()
    }

    fn update(&mut self, view: &View, cx: &mut App) {
        let target = {
            let state = view.editor.read(cx);
            if find_open(state) || view.key.carets > 1 { None } else { word_near(view.text, view.key.caret, view.key.selection.clone()) }
        };
        let Some((at, word)) = target else {
            self.clear(cx);
            return;
        };
        let win = scan_window(view.text, view.bytes.clone(), view.key.caret);
        let want: Want = (word.clone(), at.clone(), view.key.rev, win.clone());
        if self.want.as_ref() == Some(&want) {
            return;
        }
        // Another word, another place or an edit empties the marks now; a scroll keeps them
        // until the new window is scanned.
        if self.want.as_ref().is_none_or(|old| old.0 != want.0 || old.1 != want.1 || old.2 != want.2) {
            self.set.borrow_mut().set_if_changed(Vec::new(), cx);
        }
        self.want = Some(want);
        let (set, editor) = (self.set.clone(), view.editor.clone());
        self.job.run(
            view,
            WAIT,
            move |text| {
                let base = win.start;
                let slice = text.slice(win).to_string();
                occurrences(&slice, base, &word, &at)
            },
            move |found, cx| {
                if find_open(editor.read(cx)) {
                    return;
                }
                let color = theme::ink(WASH, cx);
                let list = found.into_iter().map(|range| RangeDecoration::new(range).with_style(RangeDecorationStyle::Fill).with_color(color)).collect();
                set.borrow_mut().set_if_changed(list, cx);
            },
            cx,
        );
    }
}

/// `bytes` limited to `SCAN_BYTES` each side of `caret`, on character boundaries.
fn scan_window(text: &Rope, bytes: Range<usize>, caret: usize) -> Range<usize> {
    let mut start = bytes.start.max(caret.saturating_sub(SCAN_BYTES));
    let mut end = bytes.end.min(caret.saturating_add(SCAN_BYTES)).min(text.len());
    start = start.min(end);
    while start < end && !text.is_char_boundary(start) {
        start += 1;
    }
    while end > start && !text.is_char_boundary(end) {
        end -= 1;
    }
    start..end
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The word the caret rests on, or the selection when it is exactly one word: its byte range and text.
fn word_near(text: &Rope, caret: usize, selection: Range<usize>) -> Option<(Range<usize>, String)> {
    const NEAR: usize = 4 * MAX_CHARS;
    let len = text.len();
    let (a, b) = if selection.is_empty() { (caret, caret) } else { (selection.start, selection.end) };
    if a > b || b > len || b - a > NEAR {
        return None;
    }
    let mut lo = a.saturating_sub(NEAR);
    let mut hi = (b + NEAR).min(len);
    while lo < a && !text.is_char_boundary(lo) {
        lo += 1;
    }
    while hi > b && !text.is_char_boundary(hi) {
        hi -= 1;
    }
    let s = text.slice(lo..hi).to_string();
    let found = word_in(&s, a - lo, b - lo, lo > 0, hi < len)?;
    Some((lo + found.start..lo + found.end, s[found].to_string()))
}

/// The word in `s` at the caret `a == b` (the one it is in, or ends at), or exactly `a..b`. `None`
/// when the word runs off a cut edge, or isn't `MIN_CHARS` to `MAX_CHARS` characters.
fn word_in(s: &str, a: usize, b: usize, more_before: bool, more_after: bool) -> Option<Range<usize>> {
    let inside = if a == b {
        match (s[a..].chars().next(), s[..a].chars().next_back()) {
            (Some(c), _) if is_word_char(c) => a,
            (_, Some(c)) if is_word_char(c) => a - c.len_utf8(),
            _ => return None,
        }
    } else {
        a
    };
    let mut start = inside;
    while let Some(c) = s[..start].chars().next_back().filter(|c| is_word_char(*c)) {
        start -= c.len_utf8();
    }
    let mut end = inside;
    while let Some(c) = s[end..].chars().next().filter(|c| is_word_char(*c)) {
        end += c.len_utf8();
    }
    if start == end || (a != b && (start, end) != (a, b)) || (start == 0 && more_before) || (end == s.len() && more_after) {
        return None;
    }
    (MIN_CHARS..=MAX_CHARS).contains(&s[start..end].chars().count()).then_some(start..end)
}

/// Every whole-word copy of `word` in `s` (which starts at byte `base` of the text), except the one
/// at `skip`. A word with a capital matches case; a lowercase word matches any case.
fn occurrences(s: &str, base: usize, word: &str, skip: &Range<usize>) -> Vec<Range<usize>> {
    let exact = word.chars().any(char::is_uppercase);
    let count = word.chars().count();
    let same = |run: &str| {
        if exact {
            run == word
        } else {
            run.chars().flat_map(char::to_lowercase).eq(word.chars().flat_map(char::to_lowercase))
        }
    };
    let mut out = Vec::new();
    let mut run: Option<(usize, usize)> = None;
    let flush = |run: &mut Option<(usize, usize)>, end: usize, out: &mut Vec<Range<usize>>| {
        if let Some((start, chars)) = run.take() {
            let range = base + start..base + end;
            if chars == count && range != *skip && same(&s[start..end]) {
                out.push(range);
            }
        }
    };
    for (at, c) in s.char_indices() {
        if is_word_char(c) {
            run.get_or_insert((at, 0)).1 += 1;
        } else {
            flush(&mut run, at, &mut out);
            if out.len() >= MAX_MARKS {
                return out;
            }
        }
    }
    flush(&mut run, s.len(), &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::{occurrences, word_in};

    #[test]
    fn the_word_is_the_one_the_caret_is_in_or_ends() {
        let s = "let foo_bar = 1;";
        assert_eq!(word_in(s, 6, 6, false, false), Some(4..11));
        assert_eq!(word_in(s, 11, 11, false, false), Some(4..11));
        assert_eq!(word_in(s, 3, 3, false, false), Some(0..3));
        assert_eq!(word_in(s, 12, 12, false, false), None);
        // A one-letter word, or one cut by the edge of what was read, isn't marked.
        assert_eq!(word_in("a b", 0, 0, false, false), None);
        assert_eq!(word_in("abcdef", 2, 2, true, false), None);
    }

    #[test]
    fn a_selection_must_be_exactly_one_word() {
        let s = "foo bar foobar";
        assert_eq!(word_in(s, 0, 3, false, false), Some(0..3));
        assert_eq!(word_in(s, 0, 2, false, false), None);
        assert_eq!(word_in(s, 0, 7, false, false), None);
        assert_eq!(word_in(s, 4, 7, false, false), Some(4..7));
    }

    #[test]
    fn copies_are_whole_words_only_and_skip_the_caret_word() {
        let s = "foo foobar foo_ Foo (foo)";
        let found = occurrences(s, 100, "foo", &(100..103));
        // Lowercase matches any case; `foobar` and `foo_` are other words.
        assert_eq!(found, vec![116..119, 121..124]);
        // A capital matches case.
        assert_eq!(occurrences(s, 0, "Foo", &(16..19)).len(), 0);
        assert_eq!(occurrences(s, 0, "Foo", &(0..0)), vec![16..19]);
    }
}
