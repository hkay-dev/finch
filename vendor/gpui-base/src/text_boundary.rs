use std::ops::Range;
use ropey::{LineType, RopeSlice};
use sum_tree::Bias;
use unicode_properties::{GeneralCategoryGroup, UnicodeEmoji, UnicodeGeneralCategory};
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

/// Clip a UTF-8 character boundary to a grapheme boundary without copying text.
/// Limit context to the current logical line, including its CRLF terminator.
pub(crate) fn grapheme_boundary(text: RopeSlice<'_>, offset: usize, bias: Bias) -> usize {
    let row = text.byte_to_line_idx(offset, LineType::LF);
    let start = text.line_to_byte_idx(row, LineType::LF);
    let line = text.line(row, LineType::LF);
    start + grapheme_boundary_in_line(line, offset - start, bias)
}

fn grapheme_boundary_in_line(line: RopeSlice<'_>, offset: usize, bias: Bias) -> usize {
    // Step from the adjacent scalar so an existing boundary remains unchanged.
    let query = if bias == Bias::Left {
        let Some(next) = line.chars_at(offset).next() else {
            return offset;
        };
        offset + next.len_utf8()
    } else {
        let Some(previous) = line.chars_at(offset).reversed().next() else {
            return offset;
        };
        offset - previous.len_utf8()
    };
    let mut cursor = GraphemeCursor::new(query, line.len(), true);
    let (mut chunk, mut chunk_start) = line.chunk(query);
    loop {
        let result = if bias == Bias::Left {
            cursor.prev_boundary(chunk, chunk_start)
        } else {
            cursor.next_boundary(chunk, chunk_start)
        };
        match result {
            Ok(Some(boundary)) => return boundary,
            Ok(None) => return cursor.cur_cursor(),
            Err(GraphemeIncomplete::PreContext(end)) => {
                let (context, context_start) = line.chunk(end - 1);
                cursor.provide_context(&context[..end - context_start], context_start);
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                (chunk, chunk_start) = line.chunk(chunk_start - 1);
            }
            Err(GraphemeIncomplete::NextChunk) => {
                (chunk, chunk_start) = line.chunk(chunk_start + chunk.len());
            }
            Err(GraphemeIncomplete::InvalidOffset) => {
                unreachable!("rope chunk must contain the grapheme cursor");
            }
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CharacterKind {
    Word,
    Whitespace,
    Newline,
    Other,
}

impl From<char> for CharacterKind {
    fn from(character: char) -> Self {
        // Keep Han ideographs individually selectable rather than joining an
        // entire sentence. Dictionary segmentation is deliberately not used.
        if matches!(character,
            '\u{3007}' | '\u{3400}'..='\u{4dbf}' | '\u{4e00}'..='\u{9fff}'
            | '\u{f900}'..='\u{faff}' | '\u{20000}'..='\u{2ffff}'
            | '\u{30000}'..='\u{3ffff}')
        {
            Self::Other
        } else if character == '_'
            || character.is_alphanumeric()
            || character.general_category_group() == GeneralCategoryGroup::Mark
        {
            Self::Word
        } else if matches!(character, '\n' | '\r') {
            Self::Newline
        } else if character.is_whitespace() {
            Self::Whitespace
        } else {
            Self::Other
        }
    }
}

impl CharacterKind {
    fn connects(self, next: Self) -> bool {
        self == next && matches!(self, Self::Word | Self::Whitespace)
    }
}

pub(crate) fn word_range_from_chars(
    offset: usize,
    character: char,
    previous: impl Iterator<Item = char>,
    following: impl Iterator<Item = char>,
) -> Range<usize> {
    let kind = CharacterKind::from(character);
    let connects = |next| kind.connects(CharacterKind::from(next));
    let start = previous
        .take(128)
        .take_while(|character| connects(*character))
        .fold(offset, |offset, character| offset - character.len_utf8());
    let end = following
        .take(128)
        .take_while(|character| connects(*character))
        .fold(offset + character.len_utf8(), |offset, character| {
            offset + character.len_utf8()
        });
    start..end
}

pub(crate) fn word_range_at(text: &str, offset: usize) -> Option<Range<usize>> {
    word_range_in_rope(RopeSlice::from(text), offset)
}

/// Find a word without copying rope text or splitting a grapheme cluster.
pub fn word_range_in_rope(text: RopeSlice<'_>, offset: usize) -> Option<Range<usize>> {
    let offset = text.floor_char_boundary(offset.min(text.len()));
    if offset == text.len() {
        return None;
    }
    let row = text.byte_to_line_idx(offset, LineType::LF);
    let line_start = text.line_to_byte_idx(row, LineType::LF);
    let line = text.line(row, LineType::LF);
    let mut start = grapheme_boundary_in_line(line, offset - line_start, Bias::Left);
    let mut end = grapheme_boundary_in_line(
        line, start + line.chars_at(start).next()?.len_utf8(), Bias::Right,
    );
    let cluster_kind = |range: Range<usize>| {
        let cluster = line.slice(range);
        let first = cluster.chars().next().unwrap();
        // Some emoji are also alphabetic. Digits remain words unless they
        // form a keycap; the other emoji select as standalone clusters.
        if first.is_emoji_char()
            && (!matches!(first, '0'..='9' | '#' | '*')
                || cluster.chars().any(|character| character == '\u{20e3}'))
        {
            CharacterKind::Other
        } else {
            CharacterKind::from(first)
        }
    };
    let kind = cluster_kind(start..end);
    if matches!(kind, CharacterKind::Word | CharacterKind::Whitespace) {
        for _ in 0..128 {
            if start == 0 {
                break;
            }
            let previous = grapheme_boundary_in_line(
                line, line.floor_char_boundary(start - 1), Bias::Left,
            );
            if !kind.connects(cluster_kind(previous..start)) {
                break;
            }
            start = previous;
        }
        for _ in 0..128 {
            let Some(next) = line.chars_at(end).next() else {
                break;
            };
            let following = grapheme_boundary_in_line(line, end + next.len_utf8(), Bias::Right);
            if !kind.connects(cluster_kind(end..following)) {
                break;
            }
            end = following;
        }
    }
    Some(line_start + start..line_start + end)
}

pub(crate) fn line_range_at(text: &str, offset: usize) -> Range<usize> {
    let offset = clip_offset_left(text, offset);
    let start = text[..offset].rfind('\n').map_or(0, |newline| newline + 1);
    let end = text[offset..]
        .find('\n')
        .map_or(text.len(), |newline| offset + newline);
    start..end
}

fn clip_offset_left(text: &str, offset: usize) -> usize {
    let mut offset = offset.min(text.len());
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

#[cfg(test)]
mod tests {
    use super::*;
    use ropey::Rope;
    use unicode_segmentation::UnicodeSegmentation as _;

    #[test]
    fn unicode_word_ranges_in_strings_and_rope_chunks() {
        for word in [
            "Ελληνικά", "עברית", "العَرَبِيَّة", "ภาษาไทย", "한글", "단어",
            "ひらがな", "カタカナ", "नमस्ते", "e\u{301}", "a\u{1ab0}b",
            "snake_case", "123", "abc123", "rök", "Русский",
        ] {
            for padding in [0, 985, 1000, 1015] {
                let prefix = format!("{}(", " ".repeat(padding));
                let text = format!("{prefix}{word})");
                let rope = Rope::from(text.as_str());
                let expected = prefix.len()..prefix.len() + word.len();
                for offset in expected.clone() {
                    assert_eq!(word_range_at(&text, offset), Some(expected.clone()), "{word}");
                    assert_eq!(
                        word_range_in_rope(rope.slice(..), offset),
                        Some(expected.clone()), "{word}",
                    );
                }
            }
        }
        for cluster in [
            "👨‍👩‍👧", "🇯🇵", "👍🏽", "1\u{fe0f}\u{20e3}", "ℹ️", "Ⓜ️", "🅰️",
            "\r\n", "中", "𠀀",
        ] {
            let text = format!("a{cluster}b");
            let rope = Rope::from(text.as_str());
            let end = 1 + cluster.len();
            for offset in 1..end {
                assert_eq!(word_range_at(&text, offset), Some(1..end), "{cluster}");
                assert_eq!(word_range_in_rope(rope.slice(..), offset), Some(1..end));
            }
            assert_eq!(word_range_at(&text, 0), Some(0..1));
            assert_eq!(word_range_at(&text, end), Some(end..end + 1));
        }
        assert_eq!(word_range_at("kebab-case", 1), Some(0..5));
        assert_eq!(word_range_at("kebab-case", 5), Some(5..6));
        assert_eq!(word_range_at("kebab-case", 7), Some(6..10));
        assert_eq!(word_range_at("123.45", 4), Some(4..6));
        assert_eq!(word_range_at("中文", 3), Some(3..6));
        assert_eq!(word_range_at("", 0), None);
        assert_eq!(word_range_at("end", 3), None);
    }

    #[test]
    fn grapheme_boundaries_across_rope_chunks() {
        for padding in [0, 985, 1000, 1015] {
            let text = format!(
                "{}e{}👨‍👩‍👧🇯🇵🇺🇸👍🏽1\u{fe0f}\u{20e3}\r\nz",
                "a".repeat(padding), "\u{301}".repeat(1200),
            );
            let rope = Rope::from(text.as_str());
            for (start, cluster) in text.grapheme_indices(true) {
                let end = start + cluster.len();
                for (inside, _) in cluster.char_indices() {
                    let offset = start + inside;
                    assert_eq!(grapheme_boundary(rope.slice(..), offset, Bias::Left), start);
                    assert_eq!(
                        grapheme_boundary(rope.slice(..), offset, Bias::Right),
                        if inside == 0 { start } else { end },
                    );
                }
            }
            assert_eq!(grapheme_boundary(rope.slice(..), text.len(), Bias::Left), text.len());
        }
    }
}
