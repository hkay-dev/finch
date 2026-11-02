//! Focus highlighting: View > Focus dims everything but the sentence, the line or the paragraph
//! at the caret, so the eye stays where the writing is.
//!
//! It is a layer on the shared mark driver (`marks.rs`): a text-style collection of exactly two dim
//! ranges, the text before the unit and the text after it, each with `HighlightStyle::fade_out`
//! (the multiplicative dim, which leaves syntax color, find fills, the selection and the caret
//! alone and never splits a run by color). The unit comes from the primary caret only, with
//! many carets. The scan is bounded to the rows the driver hands over (the screen and a screen
//! each way), and a unit never reaches past `SCAN_BYTES` of the caret, so a 5 MB line costs the same
//! as a short one.
//!
//! Typing inside the unit keeps it steady: the editor moves the two ranges with the edit, the unit
//! comes out the same, and the layer sets nothing. When the caret crosses a boundary the light
//! moves with a short cross-fade (the caret's glide length and curve): a timer sets the dim levels
//! a few times over `EASE` and then stops, so the editor asks for no frame at rest. Changing the
//! mode lands at once.
//!
//! Sentences come from the Mac's own sentence rules (`CFStringTokenizer` by sentence, the ICU rules
//! behind `NSString`), so "e.g." and "Dr." don't split. In code only Line and Paragraph apply;
//! Sentence acts as Line. The mode is remembered per window (`Workspace::focus`), not saved.

use std::{cell::RefCell, ffi::c_void, ops::Range, rc::Rc, time::{Duration, Instant}};

use gpui_kit::component::input::{EditorState, Rope, RopeExt as _, TextDecoration};
use gpui_kit::*;

use crate::{
    marks::{self, MarkLayer, TextSet, View},
    motion, workspace,
};

/// What the dim leaves of the ink: it fades out this much of it, so about a third is left.
const DIM: f32 = 0.67;

/// How long the light takes to move to the new unit: the caret's glide (`CARET_GLIDE`, 90 ms),
/// on the same ease-out.
const EASE: f32 = 0.09;

/// How often the ease sets a step.
const STEP: Duration = Duration::from_millis(16);

/// How far from the caret a unit is looked for, each way.
const SCAN_BYTES: usize = 32 * 1024;

/// The most rows a paragraph is followed up or down from the caret.
const SCAN_ROWS: usize = 2000;

/// What View > Focus lights.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    #[default]
    Off,
    Sentence,
    Line,
    Paragraph,
}

/// View > Focus' item for one mode.
#[derive(Action, Clone, PartialEq)]
#[action(namespace = gpui_editor, no_json)]
pub struct SetFocus {
    pub mode: Mode,
}

/// Wires the View > Focus items to the key window.
pub fn install(cx: &mut App) {
    cx.on_action(|action: &SetFocus, cx| {
        let mode = action.mode;
        // After this dispatch: the window can't be updated from inside it.
        cx.defer(move |cx| {
            workspace::set_key_focus(mode, cx);
            crate::menu::refresh(cx);
        });
    });
}

/// Brings `editor` to `mode` (a window's mode reaches each of its editors as they show).
pub fn apply(editor: &Entity<EditorState>, mode: Mode, cx: &mut App) {
    marks::with_layer(editor, cx, |layer: &mut Layer| ((), layer.set_mode(mode)));
}

/// Makes the unit follow `at` instead of the caret (the word being read aloud), or the caret again with `None`.
pub fn follow(editor: &Entity<EditorState>, at: Option<usize>, cx: &mut App) {
    marks::with_layer(editor, cx, |layer: &mut Layer| {
        let changed = layer.follow != at;
        layer.follow = at;
        ((), changed)
    });
}

/// The focus layer of one editor.
pub struct Layer {
    mode: Mode,
    set: Rc<RefCell<TextSet>>,
    /// The first list after a mode change lands at once; later ones ease.
    landed: bool,
    /// What the running ease heads for, and the timer driving it.
    ease: Option<(Vec<(Range<usize>, f32)>, Task<()>)>,
    /// The collection holds a dim that has to be cleared when the mode goes off.
    holds: bool,
    /// While text is read aloud (`speech.rs`): the offset the unit follows instead of the caret.
    follow: Option<usize>,
}

impl Layer {
    pub fn new(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        Self { mode: Mode::Off, set: Rc::new(RefCell::new(TextSet::new(editor, cx))), landed: false, ease: None, holds: false, follow: None }
    }

    /// Whether the mode changed.
    fn set_mode(&mut self, mode: Mode) -> bool {
        if self.mode == mode {
            return false;
        }
        self.mode = mode;
        self.landed = false;
        self.ease = None;
        true
    }
}

impl MarkLayer for Layer {
    fn active(&self) -> bool {
        self.mode != Mode::Off || self.holds
    }

    fn update(&mut self, view: &View, cx: &mut App) {
        if self.mode == Mode::Off {
            self.ease = None;
            self.set.borrow_mut().set_if_changed(Vec::new(), cx);
            self.holds = false;
            return;
        }
        let at = self.follow.filter(|&at| at <= view.text.len() && view.text.is_char_boundary(at)).unwrap_or(view.key.caret);
        let unit = unit_at(view.text, at, self.mode, view.prose(), view.key.rows.clone());
        let to = dims(&unit, view.text.len());
        // The editor dropped a dim an edit emptied: `held` then says it doesn't know the list.
        let (known, from) = {
            let mut set = self.set.borrow_mut();
            let held = set.held(cx);
            let known = held.iter().all(|dim| dim.range.start != usize::MAX);
            (known, held.iter().filter(|dim| !dim.range.is_empty()).map(|dim| (dim.range.clone(), dim.style.fade_out.unwrap_or(0.))).collect::<Vec<(Range<usize>, f32)>>())
        };
        if known && from == to {
            self.ease = None;
            return;
        }
        // A new key while the ease already heads here (a selection moved) lets it run on.
        if self.ease.as_ref().is_some_and(|(heading, _)| *heading == to) {
            return;
        }
        self.holds = true;
        if from.is_empty() || !self.landed {
            self.ease = None;
            self.landed = true;
            self.set.borrow_mut().set_if_changed(list(&to), cx);
            return;
        }
        let (set, since, target) = (self.set.clone(), Instant::now(), to.clone());
        let task = cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(STEP).await;
                let done = cx.update(|cx| {
                    let u = since.elapsed().as_secs_f32() / EASE;
                    let steps = if u >= 1. { list(&target) } else { list(&blend(&from, &target, motion::ease_out(u))) };
                    set.borrow_mut().set_if_changed(steps, cx);
                    u >= 1.
                });
                if done {
                    break;
                }
            }
        });
        self.ease = Some((to, task));
    }
}

/// The two dim ranges: before the unit and after it, to the ends of the text.
fn dims(unit: &Range<usize>, len: usize) -> Vec<(Range<usize>, f32)> {
    [0..unit.start, unit.end..len].into_iter().filter(|range| !range.is_empty()).map(|range| (range, DIM)).collect()
}

fn list(dims: &[(Range<usize>, f32)]) -> Vec<TextDecoration> {
    dims.iter().map(|(range, fade)| TextDecoration::new(range.clone(), HighlightStyle { fade_out: Some(*fade), ..Default::default() })).collect()
}

/// The dim levels `p` of the way (0 to 1) from `from` to `to`: each stretch of text goes from the
/// level it had to the level it will have, so the old unit dims as the new one lights. Ranges of
/// each list are apart and sorted; neighbours of one level join.
fn blend(from: &[(Range<usize>, f32)], to: &[(Range<usize>, f32)], p: f32) -> Vec<(Range<usize>, f32)> {
    let mut cuts: Vec<usize> = from.iter().chain(to).flat_map(|(range, _)| [range.start, range.end]).collect();
    cuts.sort_unstable();
    cuts.dedup();
    let level = |dims: &[(Range<usize>, f32)], at: usize| dims.iter().find(|(range, _)| range.contains(&at)).map_or(0., |(_, fade)| *fade);
    let mut out: Vec<(Range<usize>, f32)> = Vec::new();
    for pair in cuts.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (was, will) = (level(from, a), level(to, a));
        let now = was + (will - was) * p;
        if now < 0.005 {
            continue;
        }
        match out.last_mut() {
            Some((range, fade)) if range.end == a && (*fade - now).abs() < 1e-4 => range.end = b,
            _ => out.push((a..b, now)),
        }
    }
    out
}

/// The byte range of the unit at `caret`: the buffer line, the paragraph (lines up to the next
/// blank one, followed within `rows`) or the sentence (inside its paragraph). Sentence is Line in
/// code.
pub fn unit_at(text: &Rope, caret: usize, mode: Mode, prose: bool, rows: Range<usize>) -> Range<usize> {
    let caret = caret.min(text.len());
    let row = text.offset_to_point(caret).row;
    let line = text.line_start_offset(row)..text.line_end_offset(row);
    let mode = if !prose && mode == Mode::Sentence { Mode::Line } else { mode };
    match mode {
        Mode::Off | Mode::Line => line,
        Mode::Paragraph => paragraph(text, row, rows).unwrap_or(line),
        Mode::Sentence => {
            let Some(paragraph) = paragraph(text, row, rows) else { return line };
            let mut from = paragraph.start.max(caret.saturating_sub(SCAN_BYTES));
            let mut to = paragraph.end.min(caret + SCAN_BYTES);
            while from < caret && !text.is_char_boundary(from) {
                from += 1;
            }
            while to > caret && !text.is_char_boundary(to) {
                to -= 1;
            }
            let found = sentence_at(&text.slice(from..to).to_string(), caret - from);
            from + found.start..from + found.end
        }
    }
}

/// The lines from `row` up and down to the blank ones around it, within `rows` and `SCAN_ROWS` of
/// it; `None` when `row` itself is blank.
fn paragraph(text: &Rope, row: usize, rows: Range<usize>) -> Option<Range<usize>> {
    let blank = |row: usize| text.slice_line(row).chars().all(char::is_whitespace);
    if blank(row) {
        return None;
    }
    let (low, high) = (rows.start.min(row).max(row.saturating_sub(SCAN_ROWS)), rows.end.max(row + 1).min(text.lines_len()).min(row + SCAN_ROWS + 1));
    let (mut top, mut bottom) = (row, row);
    while top > low && !blank(top - 1) {
        top -= 1;
    }
    while bottom + 1 < high && !blank(bottom + 1) {
        bottom += 1;
    }
    Some(text.line_start_offset(top)..text.line_end_offset(bottom))
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CFRange {
    location: isize,
    length: isize,
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(alloc: *const c_void, bytes: *const u8, count: isize, encoding: u32, external: u8) -> *const c_void;
    fn CFStringGetLength(string: *const c_void) -> isize;
    fn CFStringTokenizerCreate(alloc: *const c_void, string: *const c_void, range: CFRange, options: usize, locale: *const c_void) -> *mut c_void;
    fn CFStringTokenizerAdvanceToNextToken(tokenizer: *mut c_void) -> usize;
    fn CFStringTokenizerGetCurrentTokenRange(tokenizer: *mut c_void) -> CFRange;
    fn CFRelease(object: *const c_void);
}

const UTF8: u32 = 0x0800_0100;
const UNIT_SENTENCE: usize = 1;

/// The sentence of `text` that holds byte `at`, by the Mac's sentence rules: the one starting at
/// `at` when a boundary falls there, the last one when `at` is the end. The whole text if the
/// system can't split it.
pub(crate) fn sentence_at(text: &str, at: usize) -> Range<usize> {
    let at = at.min(text.len());
    if text.is_empty() {
        return 0..0;
    }
    let target = text[..at].encode_utf16().count() as isize;
    // SAFETY: a CFString made from `text`'s bytes and a tokenizer over all of it, both released
    // before returning; the token ranges are read while the tokenizer is alive.
    let found = unsafe {
        let string = CFStringCreateWithBytes(std::ptr::null(), text.as_ptr(), text.len() as isize, UTF8, 0);
        if string.is_null() {
            return 0..text.len();
        }
        let range = CFRange { location: 0, length: CFStringGetLength(string) };
        let tokenizer = CFStringTokenizerCreate(std::ptr::null(), string, range, UNIT_SENTENCE, std::ptr::null());
        let mut found = None;
        if !tokenizer.is_null() {
            while CFStringTokenizerAdvanceToNextToken(tokenizer) != 0 {
                let token = CFStringTokenizerGetCurrentTokenRange(tokenizer);
                found = Some(token);
                if target < token.location + token.length {
                    break;
                }
            }
            CFRelease(tokenizer);
        }
        CFRelease(string);
        found
    };
    let Some(token) = found else { return 0..text.len() };
    let byte_of = |units: isize| {
        let mut seen = 0;
        for (at, c) in text.char_indices() {
            if seen >= units {
                return at;
            }
            seen += c.len_utf16() as isize;
        }
        text.len()
    };
    byte_of(token.location)..byte_of(token.location + token.length)
}

#[cfg(test)]
mod tests {
    // Named, not `super::*`: the glob would bring gpui's `test` attribute over the standard one.
    use super::{DIM, Mode, Rope, blend, dims, sentence_at, unit_at};
    use gpui_kit::component::input::RopeExt as _;

    fn rope(text: &str) -> Rope {
        Rope::from(text)
    }

    #[test]
    fn sentence_keeps_abbreviations_whole_and_splits_at_the_next_one() {
        let text = "We met on a bus, e.g. the 9 at noon. Then we left. Done";
        let first = sentence_at(text, text.find("bus").unwrap());
        assert_eq!(&text[first.clone()], "We met on a bus, e.g. the 9 at noon. ");
        let second = sentence_at(text, text.find("Then").unwrap());
        assert_eq!(&text[second], "Then we left. ");
        let last = sentence_at(text, text.len());
        assert_eq!(&text[last], "Done");
    }

    #[test]
    fn sentence_offsets_are_bytes_not_utf16_units() {
        let text = "Caf\u{e9} \u{1f600} is nice. Next one here.";
        let at = text.find("Next").unwrap() + 2;
        assert_eq!(&text[sentence_at(text, at)], "Next one here.");
    }

    #[test]
    fn paragraph_stops_at_blank_lines_and_the_window() {
        let text = rope("one\ntwo\n\nthree\nfour\nfive\n\nsix");
        let at = text.line_start_offset(4) + 1;
        let unit = unit_at(&text, at, Mode::Paragraph, true, 0..8);
        assert_eq!(text.slice(unit).to_string(), "three\nfour\nfive");
        // A window that ends mid-paragraph bounds the scan there.
        let unit = unit_at(&text, at, Mode::Paragraph, true, 3..5);
        assert_eq!(text.slice(unit).to_string(), "three\nfour");
    }

    #[test]
    fn sentence_is_a_line_in_code_and_a_blank_line_is_its_own_unit() {
        let text = rope("let a = 1; let b = 2;\n\nfn main() {}");
        let unit = unit_at(&text, 3, Mode::Sentence, false, 0..3);
        assert_eq!(text.slice(unit).to_string(), "let a = 1; let b = 2;");
        let blank = text.line_start_offset(1);
        assert_eq!(unit_at(&text, blank, Mode::Paragraph, true, 0..3), blank..blank);
    }

    #[test]
    fn blend_moves_the_dim_from_the_old_unit_to_the_new_one() {
        let from = dims(&(10..20), 100);
        let to = dims(&(40..50), 100);
        assert_eq!(blend(&from, &to, 0.), from);
        assert_eq!(blend(&from, &to, 1.), to);
        let mid = blend(&from, &to, 0.5);
        // The old unit is half dim, the new one half lit, the rest fully dim.
        assert!(mid.contains(&(10..20, DIM * 0.5)) && mid.contains(&(40..50, DIM * 0.5)));
        assert_eq!(mid.iter().map(|(range, _)| range.len()).sum::<usize>(), 100);
    }
}
