//! Styled Markdown's caret line: with View > Styled Markdown on, the marks of every line are faded
//! (the `markup.*` roles in `theme.rs`), and the line being edited shows its marks back at full
//! strength, so nothing is hidden while you type.
//!
//! A layer on the shared mark driver (`marks.rs`): one text-style collection holding a color
//! decoration over each mark on the caret's buffer line, in the color that mark has with the
//! option off (`theme::markdown_mark_inks`). The marks of that one line come from a small scan of
//! the line's text (`marks_in`), not from the highlighter: `#` of a heading, runs of `*` and `_`, `~~`
//! around struck text, backtick runs of a code span, and the brackets, parentheses and address of an inline link or
//! image. A line over `LONG_LINE` bytes keeps its faded look. Nothing is set unless the line or its
//! text changed (`TextSet::set_if_changed`); turning the option off, or a file that isn't Markdown,
//! clears the one list.
//!
//! The option reaches each editor's layer through an observer on the settings store, so the
//! driver is woken for that editor when the option flips.

use std::ops::Range;

use gpui_kit::component::input::{EditorState, RopeExt as _, TextDecoration};
use gpui_kit::*;

use crate::{
    marks::{self, MarkLayer, TextSet, View},
    settings, theme,
};

/// A line longer than this keeps its faded marks (the scan stays cheap on a huge line).
const LONG_LINE: usize = 16 * 1024;

/// Which off-state ink a mark has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ink {
    /// Heading markers and emphasis marks.
    Quiet,
    /// Backticks, as the code span's text.
    Code,
    /// Link and image brackets, parentheses and the `!`, which have no style of their own: the plain text ink.
    Plain,
    /// Link addresses.
    Link,
}

/// The Styled Markdown layer of one editor.
pub struct Layer {
    on: bool,
    /// The `[code_colors]` the caret line's inks were taken from.
    colors: settings::CodeColors,
    set: TextSet,
    /// The collection holds marks that have to be cleared.
    holds: bool,
    _watch: Subscription,
}

impl Layer {
    pub fn new(editor: &Entity<EditorState>, cx: &mut App) -> Self {
        let weak = editor.downgrade();
        let watch = cx.observe_global::<settings::Store>(move |cx| {
            let on = settings::get(cx).editor.styled_markdown;
            let colors = settings::get(cx).code_colors.clone();
            let weak = weak.clone();
            // After this settings change: the driver wakes the editor's layers.
            cx.defer(move |cx| {
                if let Some(editor) = weak.upgrade() {
                    // A scheme, vivid or override change recolors the marks the caret line shows, so it counts as a change too.
                    marks::with_layer(&editor, cx, |layer: &mut Layer| {
                        let toggled = std::mem::replace(&mut layer.on, on) != on;
                        let recolored = !layer.colors.same_palette(&colors);
                        layer.colors = colors;
                        ((), toggled || recolored)
                    });
                }
            });
        });
        Self { on: settings::get(cx).editor.styled_markdown, colors: settings::get(cx).code_colors.clone(), set: TextSet::new(editor, cx), holds: false, _watch: watch }
    }
}

impl MarkLayer for Layer {
    fn active(&self) -> bool {
        self.on || self.holds
    }

    fn update(&mut self, view: &View, cx: &mut App) {
        let mut list = Vec::new();
        if self.on && view.lang == "markdown" {
            let text = view.text;
            let row = text.offset_to_point(view.key.caret).row;
            let (start, end) = (text.line_start_offset(row), text.line_end_offset(row));
            if end - start <= LONG_LINE {
                let (quiet, code, plain, link) = theme::markdown_mark_inks(cx);
                let line = text.slice(start..end).to_string();
                list = marks_in(&line)
                    .into_iter()
                    .map(|(range, ink)| {
                        let color = match ink {
                            Ink::Quiet => quiet,
                            Ink::Code => code,
                            Ink::Plain => plain,
                            Ink::Link => link,
                        };
                        TextDecoration::new(start + range.start..start + range.end, HighlightStyle { color: Some(color), ..Default::default() })
                    })
                    .collect();
            }
        }
        self.holds = !list.is_empty();
        self.set.set_if_changed(list, cx);
    }
}

/// The marks of one line of Markdown, as byte ranges in `line` with the ink each has when
/// Styled Markdown is off, in order and not overlapping.
fn marks_in(line: &str) -> Vec<(Range<usize>, Ink)> {
    let bytes = line.as_bytes();
    let mut marks: Vec<(Range<usize>, Ink)> = Vec::new();
    let mut i = 0;
    // A heading: up to three spaces, one to six `#`, then a space or the end.
    let indent = bytes.iter().take(3).take_while(|&&b| b == b' ').count();
    let hashes = bytes[indent..].iter().take_while(|&&b| b == b'#').count();
    if (1..=6).contains(&hashes) && bytes.get(indent + hashes).is_none_or(|&b| b == b' ' || b == b'\t') {
        marks.push((indent..indent + hashes, Ink::Quiet));
        i = indent + hashes;
    }
    // Address ranges of links already marked, which the scan jumps over.
    let mut skips: Vec<Range<usize>> = Vec::new();
    while i < bytes.len() {
        if let Some(skip) = skips.iter().find(|skip| skip.contains(&i)) {
            i = skip.end;
            continue;
        }
        match bytes[i] {
            b'\\' => i += 2,
            b'`' => {
                let run = bytes[i..].iter().take_while(|&&b| b == b'`').count();
                match closing_run(bytes, i + run, run) {
                    Some(close) => {
                        marks.push((i..i + run, Ink::Code));
                        marks.push((close..close + run, Ink::Code));
                        i = close + run;
                    }
                    None => i += run,
                }
            }
            c @ (b'*' | b'_') => {
                let run = bytes[i..].iter().take_while(|&&b| b == c).count();
                let before = line[..i].chars().next_back();
                let after = line[i + run..].chars().next();
                let space = |ch: Option<char>| ch.is_none_or(char::is_whitespace);
                let word = |ch: Option<char>| ch.is_some_and(char::is_alphanumeric);
                // `a * b` and `snake_case` are text, not marks.
                if !(space(before) && space(after)) && !(c == b'_' && word(before) && word(after)) {
                    marks.push((i..i + run, Ink::Quiet));
                }
                i += run;
            }
            b'~' => {
                // Strikethrough: a run of exactly two, with the same word-edge test as `*`.
                let run = bytes[i..].iter().take_while(|&&b| b == b'~').count();
                let before = line[..i].chars().next_back();
                let after = line[i + run..].chars().next();
                let space = |ch: Option<char>| ch.is_none_or(char::is_whitespace);
                if run == 2 && !(space(before) && space(after)) {
                    marks.push((i..i + run, Ink::Quiet));
                }
                i += run;
            }
            b'[' => {
                if let Some((close, open, end)) = link_at(bytes, i) {
                    let image = i > 0 && bytes[i - 1] == b'!';
                    if image {
                        marks.push((i - 1..i, Ink::Plain));
                    }
                    marks.extend([(i..i + 1, Ink::Plain), (close..close + 1, Ink::Plain), (open..open + 1, Ink::Plain), (open + 1..end, Ink::Link), (end..end + 1, Ink::Plain)]);
                    skips.push(close + 1..end + 1);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    marks.retain(|(range, _)| !range.is_empty());
    marks.sort_by_key(|(range, _)| range.start);
    marks
}

/// Where a run of exactly `run` backticks starts at or after `from`.
fn closing_run(bytes: &[u8], from: usize, run: usize) -> Option<usize> {
    let mut i = from;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            let found = bytes[i..].iter().take_while(|&&b| b == b'`').count();
            if found == run {
                return Some(i);
            }
            i += found;
        } else {
            i += 1;
        }
    }
    None
}

/// For a `[` at `at` that starts an inline link `[text](address)`: the `]`, the `(` and the `)`.
fn link_at(bytes: &[u8], at: usize) -> Option<(usize, usize, usize)> {
    let mut depth = 0;
    let mut i = at;
    let close = loop {
        match *bytes.get(i)? {
            b'\\' => i += 1,
            b'[' => depth += 1,
            b']' => {
                depth -= 1;
                if depth == 0 {
                    break i;
                }
            }
            _ => {}
        }
        i += 1;
    };
    let open = close + 1;
    if bytes.get(open) != Some(&b'(') {
        return None;
    }
    let mut parens = 0;
    let mut i = open;
    while let Some(&b) = bytes.get(i) {
        match b {
            b'\\' => i += 1,
            b'(' => parens += 1,
            b')' => {
                parens -= 1;
                if parens == 0 {
                    return Some((close, open, i));
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    // Named, not `super::*`: the glob would bring gpui's `test` attribute over the standard one.
    use super::{Ink, marks_in};

    fn found(line: &str) -> Vec<(&str, Ink)> {
        marks_in(line).into_iter().map(|(range, ink)| (&line[range], ink)).collect()
    }

    #[test]
    fn heading_emphasis_and_code() {
        assert_eq!(found("## Title"), [("##", Ink::Quiet)]);
        assert_eq!(found("#hashtag"), []);
        assert_eq!(found("a **b** _c_"), [("**", Ink::Quiet), ("**", Ink::Quiet), ("_", Ink::Quiet), ("_", Ink::Quiet)]);
        assert_eq!(found("snake_case and 2 * 3"), []);
        assert_eq!(found("`a*b`"), [("`", Ink::Code), ("`", Ink::Code)]);
    }

    #[test]
    fn links_and_images() {
        let line = "see [a *b*](http://x.y/_z_) ok";
        let got = found(line);
        assert_eq!(got.iter().map(|(s, _)| *s).collect::<Vec<_>>(), ["[", "*", "*", "]", "(", "http://x.y/_z_", ")"]);
        assert_eq!(found("![alt](u)").iter().map(|(s, _)| *s).collect::<Vec<_>>(), ["!", "[", "]", "(", "u", ")"]);
        assert_eq!(found("[a][b]"), []);
    }

    #[test]
    fn strikethrough_pairs() {
        assert_eq!(found("a ~~b~~ c"), [("~~", Ink::Quiet), ("~~", Ink::Quiet)]);
        assert_eq!(found("a ~~ b"), []);
        assert_eq!(found("~~~x~~~"), []);
        assert_eq!(found("a ~b~ c"), []);
    }
}
