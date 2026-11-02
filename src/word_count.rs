//! What the status line says about text: the word count (finch#1cmj) and the words of the
//! selection, with the characters, lines and reading time its pop-up gives, the front matter's
//! writing goal (finch#1ayn) and the sum and average of the numbers in a selection (finch#rwcd).
//!
//! Counting is pure and takes the text as a stream of characters, so a rope is counted without
//! being copied. The status line asks for it on the background executor, about `DEBOUNCE` after an
//! edit, and drops an answer whose `text_revision()` has moved on.

use std::time::Duration;

use crate::language::MAX_HIGHLIGHT_BYTES;

/// How long after an edit the count waits, so typing doesn't count per key.
pub const DEBOUNCE: Duration = Duration::from_millis(300);

/// A selection of at most this many characters is counted in place; a longer one in the background.
pub const DIRECT_CHARS: usize = 20_000;

/// The reading speed the reading time is worked out at.
const WORDS_PER_MINUTE: usize = 230;

/// A file of this many bytes or fewer is counted; a bigger one shows no count, like the highlighting.
pub const MAX_BYTES: usize = MAX_HIGHLIGHT_BYTES;

/// Whether the status line counts words for a file of `language` (`language::for_text`): Markdown and plain text.
pub fn counts_words(language: &str) -> bool {
    matches!(language, "markdown" | "text")
}

/// What a count found.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub words: usize,
    pub chars: usize,
    /// As the editor numbers them: a text ending in a line break has an empty last line.
    pub lines: usize,
}

/// The running state of a count, fed a character at a time or a chunk at a time.
struct Counter {
    stats: Stats,
    /// Whether the word being read already counted.
    counted: bool,
}

impl Counter {
    fn new() -> Self {
        Self { stats: Stats { lines: 1, ..Stats::default() }, counted: false }
    }

    fn push(&mut self, ch: char) {
        self.stats.chars += 1;
        if ch == '\n' {
            self.stats.lines += 1;
        }
        if ch.is_whitespace() {
            self.counted = false;
        } else if !self.counted && ch.is_alphanumeric() {
            self.counted = true;
            self.stats.words += 1;
        }
    }

    /// A chunk of ASCII goes by bytes (a character is a byte, whitespace is `char::is_whitespace`'s ASCII set); any other goes by characters.
    fn push_str(&mut self, chunk: &str) {
        if !chunk.is_ascii() {
            chunk.chars().for_each(|ch| self.push(ch));
            return;
        }
        self.stats.chars += chunk.len();
        let (mut lines, mut words, mut counted) = (0, 0, self.counted);
        for &byte in chunk.as_bytes() {
            lines += usize::from(byte == b'\n');
            if matches!(byte, 9..=13 | 32) {
                counted = false;
            } else if !counted && byte.is_ascii_alphanumeric() {
                counted = true;
                words += 1;
            }
        }
        self.stats.lines += lines;
        self.stats.words += words;
        self.counted = counted;
    }
}

/// Counts `text`. A word is a run of characters between whitespace that holds a letter or a digit,
/// so a lone dash or a row of asterisks isn't one.
#[cfg(test)]
fn count(text: impl IntoIterator<Item = char>) -> Stats {
    let mut counter = Counter::new();
    text.into_iter().for_each(|ch| counter.push(ch));
    counter.stats
}

/// `count` over the chunks of a rope (or any split of the text): the same answer, with a word that runs across a chunk's end counted once.
pub fn count_chunks<'a>(chunks: impl IntoIterator<Item = &'a str>) -> Stats {
    let mut counter = Counter::new();
    chunks.into_iter().for_each(|chunk| counter.push_str(chunk));
    counter.stats
}

/// The front matter goal of a text given as chunks. The head is only gathered (up to `FRONT_MATTER_CHARS`) when the text opens with `---`, so a file without front matter costs one look at its first chunk.
pub fn front_matter_goal_of<'a>(chunks: impl IntoIterator<Item = &'a str>) -> Option<usize> {
    let mut chunks = chunks.into_iter().filter(|chunk| !chunk.is_empty()).peekable();
    let first = chunks.peek()?;
    if !first.strip_prefix('\u{feff}').unwrap_or(first).starts_with("---") {
        return None;
    }
    let mut head = String::new();
    let mut room = FRONT_MATTER_CHARS;
    for chunk in chunks {
        let take = chunk.char_indices().nth(room).map_or(chunk.len(), |(at, _)| at);
        head.push_str(&chunk[..take]);
        room -= chunk[..take].chars().count();
        if room == 0 {
            break;
        }
    }
    front_matter_goal(&head)
}

/// `n` with a comma between thousands: `1204` reads `1,204`.
pub fn group(n: usize) -> String {
    group_digits(&n.to_string())
}

/// `digits` (ASCII digits only) with a comma between thousands.
fn group_digits(digits: &str) -> String {
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

/// `1,204 words`, or `38 of 1,204 words` with a selection.
pub fn label(selected: Option<usize>, total: usize) -> String {
    let unit = if total == 1 { "word" } else { "words" };
    match selected {
        Some(selected) => format!("{} of {} {unit}", group(selected), group(total)),
        None => format!("{} {unit}", group(total)),
    }
}

/// The reading time of `words` at about 230 a minute, rounded up to whole minutes.
pub fn reading_time(words: usize) -> String {
    if words == 0 {
        return "Nothing to read".into();
    }
    let minutes = words.div_ceil(WORDS_PER_MINUTE);
    if words < WORDS_PER_MINUTE / 2 {
        "Under a minute to read".into()
    } else {
        format!("About {minutes} min to read")
    }
}

/// How far `words` is toward `goal`: `412 of 500 words, 82%`, or `Goal reached`. The percentage rounds down, so it reads 100 only at the goal.
pub fn progress(words: usize, goal: usize) -> String {
    if words >= goal {
        return "Goal reached".into();
    }
    format!("{} of {} words, {}%", group(words), group(goal), words * 100 / goal)
}

/// The Words field's tip: the goal's progress while one is in force and counted, else what its pop-up holds.
pub fn tip(words: Option<usize>, goal: Option<usize>) -> String {
    match words.zip(goal) {
        Some((words, goal)) => progress(words, goal),
        None => "Characters, lines and reading time".into(),
    }
}

/// The rows of the word count's pop-up: the file's characters, lines and reading time, then the goal's progress when one is set.
pub fn details(stats: &Stats, goal: Option<usize>) -> Vec<String> {
    let plural = |n: usize, one: &str, many: &str| format!("{} {}", group(n), if n == 1 { one } else { many });
    let mut rows = vec![plural(stats.chars, "character", "characters"), plural(stats.lines, "line", "lines"), reading_time(stats.words)];
    rows.extend(goal.map(|goal| progress(stats.words, goal)));
    rows
}

/// How much of the start of a file is read for a front matter `goal:`.
pub const FRONT_MATTER_CHARS: usize = 2048;

/// A goal this large is a typo, not a plan.
pub const MAX_GOAL: usize = 10_000_000;

/// The `goal: 500` of a YAML front matter block at the top of `head` (a file's first `FRONT_MATTER_CHARS`
/// characters): the block opens with a `---` line on the first line and ends at the next `---` or `...`.
/// A whole number from 1, quotes, thousands commas and a trailing ` # comment` allowed; anything else is no goal.
pub fn front_matter_goal(head: &str) -> Option<usize> {
    let head = head.strip_prefix('\u{feff}').unwrap_or(head);
    let mut lines = head.lines();
    if lines.next()?.trim_end() != "---" {
        return None;
    }
    for line in lines {
        let line = line.trim_end();
        if line == "---" || line == "..." {
            return None;
        }
        if let Some(value) = line.strip_prefix("goal:") {
            let value = value.split(" #").next().unwrap_or(value).trim().trim_matches(['"', '\'']);
            let digits: String = value.chars().filter(|ch| *ch != ',').collect();
            return digits.parse::<usize>().ok().filter(|goal| (1..=MAX_GOAL).contains(goal));
        }
    }
    None
}

/// A selection of more than this many bytes shows no sum: it is not scanned.
pub const NUMBER_SCAN_MAX: usize = 1_000_000;

/// The numbers found in a selection: how many, and their sum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tally {
    pub count: usize,
    pub sum: f64,
}

impl Tally {
    /// `Sum 1,284 · avg 160.5 · 8 numbers`.
    pub fn label(&self) -> String {
        format!("Sum {} \u{b7} avg {} \u{b7} {} numbers", format_number(self.sum, 6, true), format_number(self.sum / self.count as f64, 2, true), group(self.count))
    }

    /// The sum as a spreadsheet takes it: no thousands commas.
    pub fn plain_sum(&self) -> String {
        format_number(self.sum, 6, false)
    }
}

/// `value` to at most `decimals` places with no trailing zeros, with thousands commas when `grouped`.
fn format_number(value: f64, decimals: usize, grouped: bool) -> String {
    let text = format!("{:.*}", decimals, value.abs());
    let (whole, fraction) = text.split_once('.').unwrap_or((&text, ""));
    let fraction = fraction.trim_end_matches('0');
    let negative = value < 0. && (whole.bytes().any(|digit| digit != b'0') || !fraction.is_empty());
    let mut out = String::with_capacity(text.len() + 4);
    if negative {
        out.push('-');
    }
    if grouped {
        out.push_str(&group_digits(whole));
    } else {
        out.push_str(whole);
    }
    if !fraction.is_empty() {
        out.push('.');
        out.push_str(fraction);
    }
    out
}

/// The numbers in `text`: whole tokens such as `42`, `-3.5`, `1,284`, `$9.99`, `50%`. A number glued
/// into a word (`v2`, `x86_64`, `3rd`), a range or date (`10-20`, `2024-05-01`) and a version
/// (`1.2.3`) is not one. A comma between digits is a thousands separator only before exactly three
/// digits (`1,284`); anywhere else (`5,10,15`) it separates the values. `None` with fewer than two numbers.
pub fn tally(text: &str) -> Option<Tally> {
    let mut tally = Tally { count: 0, sum: 0. };
    let mut add = |value: f64| {
        tally.count += 1;
        tally.sum += value;
    };
    for token in text.split(|ch: char| !(ch.is_alphanumeric() || matches!(ch, '_' | '.' | ',' | '-' | '+' | '\u{2212}' | '$' | '\u{20ac}' | '\u{a3}' | '\u{a5}' | '%'))) {
        if token.is_empty() {
            continue;
        }
        match number(token) {
            Some(value) => add(value),
            None if token.contains(',') => token.split(',').filter_map(number).for_each(&mut add),
            None => {}
        }
    }
    (tally.count >= 2).then_some(tally)
}

/// One token read as a number, or `None`.
fn number(token: &str) -> Option<f64> {
    let mut rest = token.trim_end_matches(['.', ',', '%']);
    let (mut negative, mut signed, mut currency) = (false, false, false);
    // A sign and a currency mark, in either order, before the digits.
    while let Some(first) = rest.chars().next() {
        match first {
            '-' | '\u{2212}' | '+' if !signed => (signed, negative) = (true, first != '+'),
            '$' | '\u{20ac}' | '\u{a3}' | '\u{a5}' if !currency => currency = true,
            _ => break,
        }
        rest = &rest[first.len_utf8()..];
    }
    let (whole, fraction) = match rest.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (rest, None),
    };
    if fraction.is_some_and(|fraction| fraction.is_empty() || !fraction.bytes().all(|digit| digit.is_ascii_digit())) || !grouped(whole) {
        return None;
    }
    let mut digits = whole.replace(',', "");
    if let Some(fraction) = fraction {
        digits.push('.');
        digits.push_str(fraction);
    }
    let value: f64 = digits.parse().ok().filter(|value: &f64| value.is_finite())?;
    Some(if negative { -value } else { value })
}

/// Whether `whole` is digits, or digits grouped by commas in threes (`1,284,000`) after a first group of one to three.
fn grouped(whole: &str) -> bool {
    let digits = |part: &str| part.bytes().all(|digit| digit.is_ascii_digit());
    if !whole.contains(',') {
        return !whole.is_empty() && digits(whole);
    }
    let mut groups = whole.split(',');
    let first = groups.next().unwrap_or("");
    (1..=3).contains(&first.len()) && digits(first) && groups.all(|group| group.len() == 3 && digits(group))
}

#[cfg(test)]
mod tests {
    use super::{Stats, count, details, group, label, progress, reading_time, tip};

    fn words(text: &str) -> usize {
        count(text.chars()).words
    }

    #[test]
    fn progress_rounds_down_and_ends_at_the_goal() {
        assert_eq!(progress(412, 500), "412 of 500 words, 82%");
        assert_eq!(progress(1999, 2000), "1,999 of 2,000 words, 99%");
        assert_eq!(progress(500, 500), "Goal reached");
        assert_eq!(progress(900, 500), "Goal reached");
        assert_eq!(tip(Some(3), Some(10)), "3 of 10 words, 30%");
        assert_eq!(tip(Some(3), None), "Characters, lines and reading time");
        let stats = Stats { words: 3, chars: 20, lines: 1 };
        assert_eq!(details(&stats, None).len(), 3);
        assert_eq!(details(&stats, Some(10)).last().map(String::as_str), Some("3 of 10 words, 30%"));
    }

    #[test]
    fn counts_words_between_whitespace() {
        assert_eq!(words(""), 0);
        assert_eq!(words("   \n\t "), 0);
        assert_eq!(words("one"), 1);
        assert_eq!(words("one two\nthree\tfour  five"), 5);
        assert_eq!(words("well-known e.g. don't 3.14"), 4);
    }

    #[test]
    fn marks_and_rules_are_not_words() {
        assert_eq!(words("# Title\n\n---\n\n- item *  **\n\u{2014}"), 2);
        assert_eq!(words("caf\u{e9} \u{65e5}\u{672c}\u{8a9e}"), 2);
    }

    #[test]
    fn counts_characters_and_lines_as_the_editor_does() {
        assert_eq!(count("".chars()), Stats { words: 0, chars: 0, lines: 1 });
        assert_eq!(count("ab\ncd".chars()), Stats { words: 2, chars: 5, lines: 2 });
        assert_eq!(count("ab\ncd\n".chars()).lines, 3);
    }

    #[test]
    fn reads_the_goal_from_the_front_matter() {
        use super::front_matter_goal as goal;
        assert_eq!(goal("---\ntitle: A\ngoal: 500\n---\nbody"), Some(500));
        assert_eq!(goal("\u{feff}---\ngoal: \"1,500\" # a day\n---\n"), Some(1500));
        assert_eq!(goal("---\r\ngoal: 300\r\n---\r\n"), Some(300));
        assert_eq!(goal("---\ngoal: 7\n..."), Some(7));
    }

    #[test]
    fn no_goal_outside_the_front_matter_or_when_it_is_not_a_number() {
        use super::front_matter_goal as goal;
        assert_eq!(goal("goal: 500\n"), None);
        assert_eq!(goal("# Title\n---\ngoal: 500\n---\n"), None);
        assert_eq!(goal("---\ntitle: A\n---\ngoal: 500\n"), None);
        assert_eq!(goal("---\n  goal: 500\n---\n"), None);
        for value in ["0", "-3", "lots", "5.5", "", "99999999999999999999"] {
            assert_eq!(goal(&format!("---\ngoal: {value}\n---\n")), None, "{value:?}");
        }
        assert_eq!(goal(""), None);
    }

    #[test]
    fn reads_with_thousands_and_a_selection() {
        assert_eq!(group(0), "0");
        assert_eq!(group(999), "999");
        assert_eq!(group(1204), "1,204");
        assert_eq!(group(1_234_567), "1,234,567");
        assert_eq!(label(None, 1204), "1,204 words");
        assert_eq!(label(None, 1), "1 word");
        assert_eq!(label(Some(38), 1204), "38 of 1,204 words");
    }

    #[test]
    fn reading_time_rounds_up_at_about_230_a_minute() {
        assert_eq!(reading_time(0), "Nothing to read");
        assert_eq!(reading_time(80), "Under a minute to read");
        assert_eq!(reading_time(230), "About 1 min to read");
        assert_eq!(reading_time(231), "About 2 min to read");
        assert_eq!(reading_time(1150), "About 5 min to read");
    }
}

#[cfg(test)]
mod number_tests {
    use super::{Tally, format_number, tally};

    fn sum(text: &str) -> Option<(usize, f64)> {
        tally(text).map(|tally| (tally.count, (tally.sum * 1e6).round() / 1e6))
    }

    #[test]
    fn sums_a_column_with_negatives_decimals_and_thousands() {
        assert_eq!(sum("3\n12\n-4\n1,284\n0.5"), Some((5, 1295.5)));
        assert_eq!(sum("$9.99 and 50% of +1"), Some((3, 60.99)));
        assert_eq!(sum("\u{2212}2 2"), Some((2, 0.)));
        assert_eq!(sum("a: 1, b: 2."), Some((2, 3.)));
    }

    #[test]
    fn commas_group_only_before_three_digits() {
        assert_eq!(sum("1,284,000 2"), Some((2, 1_284_002.)));
        assert_eq!(sum("5,10,15"), Some((3, 30.)));
        assert_eq!(sum("1,28 7"), Some((3, 36.)));
        assert_eq!(sum("apple,3,1.50"), Some((2, 4.5)));
    }

    #[test]
    fn numbers_glued_into_words_ranges_and_versions_are_ignored() {
        assert_eq!(sum("v2 x86 x86_64 3rd 4 5"), Some((2, 9.)));
        assert_eq!(sum("10-20 2024-05-01 1.2.3 1e5 0x1F 7 8"), Some((2, 15.)));
        assert_eq!(sum("--5 $$5 1. 2"), Some((2, 3.)));
    }

    #[test]
    fn needs_two_numbers() {
        assert_eq!(sum(""), None);
        assert_eq!(sum("just some words"), None);
        assert_eq!(sum("one 42 only"), None);
    }

    #[test]
    fn reads_as_the_status_line_shows_it() {
        let label = |count, sum| Tally { count, sum }.label();
        assert_eq!(label(8, 1284.), "Sum 1,284 \u{b7} avg 160.5 \u{b7} 8 numbers");
        assert_eq!(label(2, 0.1 + 0.2), "Sum 0.3 \u{b7} avg 0.15 \u{b7} 2 numbers");
        assert_eq!(label(3, -1234.5), "Sum -1,234.5 \u{b7} avg -411.5 \u{b7} 3 numbers");
        assert_eq!(label(1000, 1.), "Sum 1 \u{b7} avg 0 \u{b7} 1,000 numbers");
        assert_eq!(format_number(-0.0000001, 6, true), "0");
        assert_eq!(Tally { count: 2, sum: 1_234_567.25 }.plain_sum(), "1234567.25");
    }
}

#[cfg(test)]
mod chunk_tests {
    use super::{count, count_chunks, front_matter_goal_of};
    use gpui_kit::component::input::Rope;

    /// A word that runs over a chunk's end counts once, wherever the text is cut, for ASCII, accents, CJK and the whitespace `char::is_whitespace` knows beyond ASCII.
    #[test]
    fn a_word_across_a_chunk_boundary_counts_once() {
        for text in ["one two\nthree\tfour  five\r\nsix\u{b}seven\u{c}eight", "caf\u{e9} na\u{ef}ve \u{65e5}\u{672c}\u{8a9e} x", "a\u{a0}b\u{85}c\u{2003}d --- ** 3.14 e.g.", "", " ", "word"] {
            let whole = count(text.chars());
            for cut in (0..=text.len()).filter(|&at| text.is_char_boundary(at)) {
                let (a, b) = text.split_at(cut);
                assert_eq!(count_chunks([a, b]), whole, "{text:?} cut at {cut}");
                assert_eq!(count_chunks([a, "", b]), whole);
            }
            assert_eq!(count_chunks(text.split_inclusive(' ')), whole);
        }
    }

    #[test]
    fn a_rope_counts_as_its_characters() {
        let text = "word \u{e9}tude\n".repeat(5_000);
        let rope = Rope::from(text.as_str());
        assert!(rope.chunks().count() > 1);
        assert_eq!(count_chunks(rope.chunks()), count(text.chars()));
        let ascii = "word, another one\n".repeat(5_000);
        assert_eq!(count_chunks(Rope::from(ascii.as_str()).chunks()), count(ascii.chars()));
    }

    #[test]
    fn the_goal_is_read_from_the_first_chunks() {
        assert_eq!(front_matter_goal_of(["---\ngoal: ", "500\n---\nbody"]), Some(500));
        assert_eq!(front_matter_goal_of(["", "\u{feff}---\ngoal: 7\n---\n"]), Some(7));
        assert_eq!(front_matter_goal_of(["# T\n---\ngoal: 5\n---\n"]), None);
        assert_eq!(front_matter_goal_of([""; 0]), None);
        let long = format!("---\ntitle: {}\ngoal: 300\n---\n", "x".repeat(900));
        let rope = Rope::from(long.as_str());
        assert_eq!(front_matter_goal_of(rope.chunks()), super::front_matter_goal(&long.chars().take(super::FRONT_MATTER_CHARS).collect::<String>()));
        assert_eq!(front_matter_goal_of(rope.chunks()), Some(300));
    }

    /// A 1 MB prose file: the old walk by character against the walk by chunk.
    #[test]
    fn a_megabyte_counts_by_chunk_faster_than_by_character() {
        for (name, line) in [("ascii", "The quick brown fox jumps over the lazy dog, again.\n"), ("accents", "Th\u{e9} quick brown fox jumps \u{f6}ver the lazy dog, again.\n")] {
            let rope = Rope::from(line.repeat(1_000_000 / line.len()).as_str());
            let time = |run: &dyn Fn() -> super::Stats| {
                let _ = run();
                let start = std::time::Instant::now();
                let found = (0..10).map(|_| run()).last().unwrap();
                (start.elapsed() / 10, found)
            };
            let (before, old) = time(&|| count(rope.chars()));
            let (after, new) = time(&|| count_chunks(rope.chunks()));
            let head_before = std::time::Instant::now();
            let _: String = rope.chars().take(super::FRONT_MATTER_CHARS).collect();
            let head_before = head_before.elapsed();
            let head_after = std::time::Instant::now();
            let _ = front_matter_goal_of(rope.chunks());
            let head_after = head_after.elapsed();
            eprintln!("1 MB count ({name}): {before:?} by character, {after:?} by chunk; head read {head_before:?} before, {head_after:?} after");
            assert_eq!(new, old);
        }
    }
}
