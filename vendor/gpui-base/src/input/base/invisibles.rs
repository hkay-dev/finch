// Invisibility in a Unicode context is not well defined, so we have to guess.
//
// We highlight all ASCII control codes, and unicode whitespace because they are likely
// confused with an ASCII space in a programming context (U+0020).
//
// We also highlight the handful of blank non-space characters:
//   U+2800 BRAILLE PATTERN BLANK - Category: So
//   U+115F HANGUL CHOSEONG FILLER - Category: Lo
//   U+1160 HANGUL CHOSEONG FILLER - Category: Lo
//   U+3164 HANGUL FILLER - Category: Lo
//   U+FFA0 HALFWIDTH HANGUL FILLER - Category: Lo
//   U+FFFC OBJECT REPLACEMENT CHARACTER - Category: So
//
// For the rest of Unicode, invisibility happens for two reasons:
// * A Format character (like a byte order mark or right-to-left override)
// * An invisible Nonspacing Mark character (like U+034F, or variation selectors)
//
// We don't consider unassigned codepoints invisible as the font renderer already shows
// a replacement character in that case (and there are a *lot* of them)
//
// Control characters are mostly fine to highlight; except:
// * U+E0020..=U+E007F are used in emoji flags, and are four bytes long anyway (see `scan`).
// * U+200D is used to join characters. Zed highlights it but doesn't replace it; `scan` skips
//   it, because a highlight would split the run inside an emoji sequence and break the join.
//
// Nonspacing marks that `replacement` preserves (U+034F, the Khmer and Mongolian ones) are
// skipped the same way.
//
// ref: https://invisible-characters.com
// ref: https://www.compart.com/en/unicode/category/Cf
// ref: https://gist.github.com/ConradIrwin/f759e1fc29267143c4c7895aa495dca5?h=1
// ref: https://unicode.org/Public/emoji/13.0/emoji-test.txt
// https://github.com/bits/UTF-8-Unicode-Test-Documents/blob/master/UTF-8_sequence_separated/utf8_sequence_0-0x10ffff_assigned_including-unprintable-asis.txt
//
// Ported from Zed's `crates/editor/src/display_map/invisibles.rs` (gpui-editor#ytm8).
// The shaped string keeps the byte length of the buffer's, so every index, column
// and caret position stays right; see `stand_in` and `splice_symbols`.

use gpui::{
    Font, LineLayout, Pixels, ShapedGlyph, ShapedLine, ShapedRun, SharedString, TextRun,
    Window,
};
use smallvec::SmallVec;
use std::sync::Arc;

fn is_invisible(c: char) -> bool {
    if c <= '\u{1f}' {
        c != '\t' && c != '\n' && c != '\r'
    } else if c >= '\u{7f}' {
        c <= '\u{9f}'
            || (c.is_whitespace() && c != IDEOGRAPHIC_SPACE)
            || is_format_or_other_invisible_character(c)
    } else {
        false
    }
}

// ASCII control characters have fancy unicode glyphs, everything else
// is replaced by a space - unless it is used in combining characters in
// which case we need to leave it in the string.
fn replacement(c: char) -> Option<char> {
    if c <= '\x1f' {
        C0_SYMBOLS.get(c as usize).copied()
    } else if c == '\x7f' {
        Some(DEL)
    } else if should_preserve_invisible_character(c) {
        None
    } else {
        Some(FIXED_WIDTH_SPACE)
    }
}

pub(super) fn is_stand_in(c: char) -> bool {
    is_invisible(c) && replacement(c).is_some()
}

#[inline(always)]
fn is_format_or_other_invisible_character(c: char) -> bool {
    match c {
        // Generated using: ucd-generate general-category --include Format --chars ucd-16.0.0
        '\u{ad}'
        | '\u{600}'..='\u{605}'
        | '\u{61c}'
        | '\u{6dd}'
        | '\u{70f}'
        | '\u{890}'..='\u{891}'
        | '\u{8e2}'
        | '\u{180e}'
        | '\u{200b}'..='\u{200f}'
        | '\u{202a}'..='\u{202e}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206f}'
        | '\u{feff}'
        | '\u{fff9}'..='\u{fffb}'
        | '\u{110bd}'
        | '\u{110cd}'
        | '\u{13430}'..='\u{1343f}'
        | '\u{1bca0}'..='\u{1bca3}'
        | '\u{1d173}'..='\u{1d17a}'
        | '\u{e0001}'
        | '\u{e0020}'..='\u{e007f}'
        // Hand-made based on https://invisible-characters.com, excluding Cf.
        | '\u{034f}'
        | '\u{115f}'..='\u{1160}'
        | '\u{17b4}'..='\u{17b5}'
        | '\u{180b}'..='\u{180d}'
        | '\u{2800}'
        | '\u{3164}'
        | '\u{fe00}'..='\u{fe0d}'
        | '\u{ffa0}'
        | '\u{fffc}'
        | '\u{e0100}'..='\u{e01ef}' => true,
        _ => false,
    }
}

#[inline(always)]
fn should_preserve_invisible_character(c: char) -> bool {
    match c {
        '\u{034f}'
        | '\u{200d}'
        | '\u{17b4}'..='\u{17b5}'
        | '\u{180b}'..='\u{180d}'
        | '\u{e0061}'..='\u{e007a}'
        | '\u{e007f}' => true,
        _ => false,
    }
}

const FIXED_WIDTH_SPACE: char = '\u{2007}';

// IDEOGRAPHIC SPACE is common alongside Chinese and other wide character sets.
// We don't highlight this for now (as it already shows up wide in the editor),
// but could if we tracked state in the classifier.
const IDEOGRAPHIC_SPACE: char = '\u{3000}';

const C0_SYMBOLS: &[char] = &[
    '␀', '␁', '␂', '␃', '␄', '␅', '␆', '␇', '␈', '␉', '␊', '␋', '␌', '␍', '␎', '␏', '␐', '␑', '␒',
    '␓', '␔', '␕', '␖', '␗', '␘', '␙', '␚', '␛', '␜', '␝', '␞', '␟',
];
const DEL: char = '␡';

/// An invisible character in a line: its byte index and the character.
pub(super) type Found = SmallVec<[(usize, char); 2]>;

/// The invisible characters of `text` that are shown, in order. Empty, after
/// one pass over the bytes, for the plain lines nearly every file has.
///
/// Four-byte characters (tag characters, a few musical and Egyptian format
/// controls) are left alone: nothing of that length stands in for them, and
/// the shaped text must keep the buffer's byte length.
pub(super) fn scan(text: &str) -> Found {
    let mut found = Found::new();
    let plain = |b: &u8| (*b < 0x20 && !matches!(*b, b'\t' | b'\n' | b'\r')) || *b >= 0x7f;
    if !text.as_bytes().iter().any(plain) {
        return found;
    }
    for (index, c) in text.char_indices() {
        if c.len_utf8() < 4 && is_stand_in(c) {
            found.push((index, c));
        }
    }
    found
}

/// `text` with each found character swapped for one of the same byte length:
/// the shaper never sees a direction control (U+202E would reorder the text
/// after it), and a hidden character gets a cell of its own. Control
/// characters become a space, under the symbol `splice_symbols` puts there.
pub(super) fn stand_in(text: &str, found: &Found) -> SharedString {
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for &(index, c) in found {
        out.push_str(&text[copied..index]);
        out.push(match c.len_utf8() {
            1 => ' ',
            2 => '\u{a0}',
            _ => FIXED_WIDTH_SPACE,
        });
        copied = index + c.len_utf8();
    }
    out.push_str(&text[copied..]);
    out.into()
}

fn font_at(runs: &[TextRun], index: usize) -> Option<&Font> {
    let mut start = 0;
    runs.iter().find_map(|run| {
        let end = start + run.len;
        let hit = index < end;
        start = end;
        hit.then_some(&run.font)
    })
}

/// Puts each control character's symbol glyph (U+2400 and on) over the space
/// `stand_in` shaped for it, at the same index, so columns and the caret are
/// the space's. `runs` are those the line was shaped with.
pub(super) fn splice_symbols(
    line: &mut ShapedLine,
    found: &Found,
    runs: &[TextRun],
    font_size: Pixels,
    window: &mut Window,
) {
    let mut symbols: SmallVec<[(usize, ShapedRun); 2]> = SmallVec::new();
    for &(index, c) in found {
        let Some(symbol) = replacement(c).filter(|_| c.len_utf8() == 1) else {
            continue;
        };
        let Some(font) = font_at(runs, index) else {
            continue;
        };
        let shaped = window.text_system().shape_line(
            symbol.to_string().into(),
            font_size,
            &[TextRun {
                len: symbol.len_utf8(),
                font: font.clone(),
                color: gpui::black(),
                background_color: None,
                underline: None,
                strikethrough: None,
            }],
            None,
        );
        if let Some(run) = shaped.runs.first().filter(|run| !run.glyphs.is_empty()) {
            symbols.push((index, run.clone()));
        }
    }
    if symbols.is_empty() {
        return;
    }

    let mut spliced = Vec::with_capacity(line.runs.len() + symbols.len() * 2);
    for run in &line.runs {
        let mut glyphs = Vec::with_capacity(run.glyphs.len());
        for glyph in &run.glyphs {
            let Some((_, symbol)) = symbols.iter().find(|(index, _)| *index == glyph.index) else {
                glyphs.push(glyph.clone());
                continue;
            };
            if !glyphs.is_empty() {
                spliced.push(ShapedRun {
                    font_id: run.font_id,
                    glyphs: std::mem::take(&mut glyphs),
                });
            }
            spliced.push(ShapedRun {
                font_id: symbol.font_id,
                glyphs: vec![ShapedGlyph {
                    id: symbol.glyphs[0].id,
                    position: glyph.position,
                    index: glyph.index,
                    is_emoji: false,
                }],
            });
        }
        if !glyphs.is_empty() {
            spliced.push(ShapedRun {
                font_id: run.font_id,
                glyphs,
            });
        }
    }
    **line = Arc::new(LineLayout {
        font_size: line.font_size,
        width: line.width,
        ascent: line.ascent,
        descent: line.descent,
        runs: spliced,
        len: line.len,
    });
}
