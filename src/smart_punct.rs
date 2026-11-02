//! Smart punctuation as you type, in Markdown and plain text: a straight quote becomes a curly one
//! that opens or closes by what comes before it, `...` becomes an ellipsis and `--` an en dash.
//!
//! Pure rules over the text of one line, so they are cheap per key and testable without an editor.
//! `document.rs` calls [`replacement`] after a keystroke with the caret's line up to the caret, and
//! [`in_front_matter_or_fence`] with the lines above it (only when a key could change something).
//! Code spans, fenced code, front matter, link targets, URLs and a rule or table line of dashes are
//! left as typed.

use std::ops::Range;
use std::path::Path;

/// The kind of document the rules run in. Code files have none.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Markdown: also leaves code, link targets and the like alone.
    Markdown,
    /// Plain text: every character is prose.
    Plain,
}

/// A change to make: the bytes `range` of the line (counted from its start) become `text`, and the
/// caret ends `caret` bytes from the line's start.
#[derive(Debug, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: &'static str,
    pub caret: usize,
}

impl Edit {
    /// `range` becomes `text` with the caret after it.
    fn put(range: Range<usize>, text: &'static str) -> Self {
        let caret = range.start + text.len();
        Self { range, text, caret }
    }
}

/// How many lines above the caret one check reads at most. A caret further down is never changed.
pub const LINES_ABOVE_MAX: usize = 5000;

/// The kind for a document: Markdown by its language (an Untitled page's too, `language::for_untitled`),
/// plain text for an untitled page or a `.txt` file, nothing for anything else (code, config, files of unknown kind).
pub fn kind(path: Option<&Path>, lang: &str) -> Option<Kind> {
    if lang == "markdown" {
        return Some(Kind::Markdown);
    }
    match path {
        None => (lang == "text").then_some(Kind::Plain),
        Some(path) => path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("txt") || ext.eq_ignore_ascii_case("text"))
            .then_some(Kind::Plain),
    }
}

/// Whether a key can change anything: only a quote, a dot or a dash can.
pub fn is_trigger(ch: char) -> bool {
    matches!(ch, '"' | '\'' | '.' | '-')
}

/// The change for the character just typed at the end of `line` (the caret's line up to and
/// including it). `after` is the character right after the caret. `paired` says the editor closed
/// the quote itself (`"` typed became `""`, the caret between): the closer is `after`, and the edit
/// covers it too. `None` leaves the text as typed.
pub fn replacement(kind: Kind, line: &str, after: Option<char>, paired: bool) -> Option<Edit> {
    let typed = line.chars().next_back()?;
    if !is_trigger(typed) {
        return None;
    }
    if in_url(line) || (kind == Kind::Markdown && in_markup(line)) {
        return None;
    }
    let end = line.len();
    let head = &line[..end - typed.len_utf8()];
    let prev = head.chars().next_back();
    if paired && (!matches!(typed, '"' | '\'') || after != Some(typed)) {
        return None;
    }
    match typed {
        '"' | '\'' => {
            let double = typed == '"';
            // An apostrophe inside a word closes; so does a quote after a closing quote or a mark.
            let opening = opens(prev) && !(!double && prev.is_some_and(char::is_alphanumeric));
            let (open, close, pair) = if double { ("\u{201C}", "\u{201D}", "\u{201C}\u{201D}") } else { ("\u{2018}", "\u{2019}", "\u{2018}\u{2019}") };
            if paired {
                // The editor added a closer: an opener keeps the pair, a closer swallows it.
                return Some(if opening {
                    Edit { range: end - 1..end + 1, text: pair, caret: end - 1 + open.len() }
                } else {
                    Edit::put(end - 1..end + 1, close)
                });
            }
            // A closer typed over the curly closer the pair left: step over it.
            if after == close.chars().next() {
                return Some(Edit::put(end - 1..end + close.len(), close));
            }
            Some(Edit::put(end - 1..end, if opening { open } else { close }))
        }
        '.' => {
            let dots = head.strip_suffix("..")?;
            if dots.ends_with('.') || after == Some('.') {
                return None;
            }
            Some(Edit::put(end - 3..end, "\u{2026}"))
        }
        _ => {
            if after == Some('-') {
                return None;
            }
            let before = head.strip_suffix('-')?;
            if before.ends_with('-') || before.ends_with("<!") {
                return None;
            }
            // A line of only dashes (a rule, a list marker and dashes) or of table marks stays.
            if before.chars().all(|c| matches!(c, '-' | ' ' | '\t' | '|' | ':')) {
                return None;
            }
            Some(Edit::put(before.len()..end, "\u{2013}"))
        }
    }
}

/// Whether a quote after `prev` opens: at the start, after a space, an opening bracket or dash, or
/// another opening quote.
fn opens(prev: Option<char>) -> bool {
    match prev {
        None => true,
        Some(c) => c.is_whitespace() || matches!(c, '(' | '[' | '{' | '<' | '\u{2013}' | '\u{2014}' | '\u{201C}' | '\u{2018}'),
    }
}

/// Whether the word being typed is a URL or an autolink.
fn in_url(line: &str) -> bool {
    let word = line.rsplit(char::is_whitespace).next().unwrap_or("");
    word.contains("://") || word.starts_with("www.") || word.starts_with("mailto:")
}

/// Whether the end of `line` sits where markup, not prose, is: in a code span, a link target, a
/// URL, an HTML tag or comment, a fence's info line or a reference definition.
fn in_markup(line: &str) -> bool {
    let trimmed = line.trim_start();
    if line.len() - trimmed.len() <= 3 && (trimmed.starts_with("```") || trimmed.starts_with("~~~")) {
        return true;
    }
    if in_code_span(line) {
        return true;
    }
    // `[label]: target` and `](target`.
    if let Some(rest) = trimmed.strip_prefix('[') {
        if rest.find("]:").is_some() && line.len() - trimmed.len() <= 3 {
            return true;
        }
    }
    if let Some(at) = line.rfind("](") {
        if !line[at..].contains(')') {
            return true;
        }
    }
    // An open tag (`<a href="`) or comment.
    if let Some(at) = line.rfind('<') {
        let tail = &line[at + 1..];
        let tag = tail.starts_with(|c: char| c.is_ascii_alphabetic() || c == '/' || c == '!');
        if tag && !tail.contains('>') {
            return true;
        }
    }
    line.contains("<!--") && !line.contains("-->")
}

/// Whether the end of `line` is inside a code span: a run of backticks opened it and no run of the
/// same length has closed it.
fn in_code_span(line: &str) -> bool {
    let mut open: Option<usize> = None;
    let mut run = 0usize;
    let settle = |run: usize, open: &mut Option<usize>| {
        if run == 0 {
            return;
        }
        match *open {
            None => *open = Some(run),
            Some(len) if len == run => *open = None,
            Some(_) => {}
        }
    };
    for ch in line.chars() {
        if ch == '`' {
            run += 1;
        } else {
            settle(run, &mut open);
            run = 0;
        }
    }
    settle(run, &mut open);
    open.is_some()
}

/// How far a line is indented, a tab counting to the next multiple of four.
fn indent_of(line: &str) -> usize {
    let mut width = 0;
    for ch in line.chars() {
        match ch {
            ' ' => width += 1,
            '\t' => width += 4 - width % 4,
            _ => break,
        }
    }
    width
}

/// Whether `line` opens a list item: a bullet or a number, then a space.
fn starts_item(line: &str) -> bool {
    let rest = line.trim_start();
    let digits = rest.chars().take_while(char::is_ascii_digit).count();
    let (marker, after) = if digits > 0 { (rest[digits..].chars().next(), &rest[digits..]) } else { (rest.chars().next(), rest) };
    let ok = if digits > 0 { matches!(marker, Some('.' | ')')) } else { matches!(marker, Some('-' | '*' | '+')) };
    ok && after[1..].starts_with([' ', '\t'])
}

/// How many characters of a line above the caret one check reads.
const PREFIX_CHARS: usize = 64;

/// The first characters of a line, kept on the stack so a check over thousands of lines above
/// the caret allocates nothing.
pub struct LinePrefix {
    bytes: [u8; PREFIX_CHARS * 4],
    len: usize,
}

impl LinePrefix {
    /// The first 64 characters of a text of `len` bytes that `chunk(at)` serves piece by piece
    /// (the piece that starts at byte `at`). Walking chunks instead of `chars()` keeps a rope
    /// line from allocating a cursor.
    pub fn of<'a>(len: usize, chunk: impl Fn(usize) -> &'a str) -> Self {
        let mut prefix = Self { bytes: [0; PREFIX_CHARS * 4], len: 0 };
        let (mut at, mut chars) = (0, 0);
        while at < len && chars < PREFIX_CHARS {
            let piece = chunk(at);
            if piece.is_empty() {
                break;
            }
            at += piece.len();
            for ch in piece.chars().take(PREFIX_CHARS - chars) {
                prefix.len += ch.encode_utf8(&mut prefix.bytes[prefix.len..]).len();
                chars += 1;
            }
        }
        prefix
    }
}

impl AsRef<str> for LinePrefix {
    fn as_ref(&self) -> &str {
        // Only whole encoded characters were written.
        std::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }
}

/// Whether the line being typed (`line`, up to the caret) in a `kind` document, after `above`
/// (every line above it, in order, each cut to a few characters is enough), sits in front matter,
/// fenced code or, in Markdown, an indented code block: a line indented four spaces or a tab that
/// follows a blank line (or continues such a block) outside a list item.
pub fn in_front_matter_or_fence<S: AsRef<str>>(kind: Kind, above: impl IntoIterator<Item = S>, line: &str) -> bool {
    let mut front: Option<bool> = None; // Some(true) while inside the opening block
    let mut fence: Option<(char, usize)> = None;
    let (mut blank, mut list, mut code) = (true, false, false);
    for (row, text) in above.into_iter().enumerate() {
        let text = text.as_ref().trim_end();
        if row == 0 && text == "---" {
            front = Some(true);
            continue;
        }
        if front == Some(true) {
            if text == "---" || text == "..." {
                front = Some(false);
            }
            continue;
        }
        let trimmed = text.trim_start();
        let mark = trimmed.chars().next().filter(|c| matches!(c, '`' | '~'));
        let len = mark.map_or(0, |mark| trimmed.chars().take_while(|&c| c == mark).count());
        if let (Some(mark), true) = (mark, len >= 3) {
            match fence {
                None => fence = Some((mark, len)),
                Some((open, min)) if open == mark && len >= min && trimmed.chars().all(|c| c == mark) => fence = None,
                Some(_) => {}
            }
            (blank, list, code) = (false, false, false);
            continue;
        }
        if fence.is_some() {
            continue;
        }
        if trimmed.is_empty() {
            blank = true;
            continue;
        }
        let indent = indent_of(text);
        if indent >= 4 {
            code = code || (blank && !list);
        } else {
            code = false;
            if starts_item(text) {
                list = true;
            } else if blank && !(list && indent >= 2) {
                list = false;
            }
        }
        blank = false;
    }
    let indented_code = kind == Kind::Markdown && indent_of(line) >= 4 && (code || (blank && !list));
    front == Some(true) || fence.is_some() || indented_code
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Types `typed` one character at a time into `kind`, applying each replacement, as the editor does.
    fn type_in(kind: Kind, typed: &str) -> String {
        let mut text = String::new();
        for ch in typed.chars() {
            text.push(ch);
            if let Some(edit) = replacement(kind, &text, None, false) {
                text.replace_range(edit.range, edit.text);
            }
        }
        text
    }

    #[test]
    fn the_brief_example() {
        assert_eq!(type_in(Kind::Markdown, "\"Hi\" -- it's..."), "\u{201C}Hi\u{201D} \u{2013} it\u{2019}s\u{2026}");
    }

    #[test]
    fn quotes_open_and_close_by_context() {
        assert_eq!(type_in(Kind::Plain, "say 'hi' and (\"yes\")"), "say \u{2018}hi\u{2019} and (\u{201C}yes\u{201D})");
        assert_eq!(type_in(Kind::Plain, "don't"), "don\u{2019}t");
        assert_eq!(type_in(Kind::Plain, "\"'a'\""), "\u{201C}\u{2018}a\u{2019}\u{201D}");
    }

    #[test]
    fn dashes_stay_in_rules_and_tables() {
        assert_eq!(type_in(Kind::Markdown, "---"), "---");
        assert_eq!(type_in(Kind::Markdown, "- --"), "- --");
        assert_eq!(type_in(Kind::Markdown, "|--|--|"), "|--|--|");
        assert_eq!(type_in(Kind::Markdown, "a --"), "a \u{2013}");
        assert_eq!(type_in(Kind::Markdown, "a -- b"), "a \u{2013} b");
        assert_eq!(replacement(Kind::Plain, "a --", Some('-'), false), None);
    }

    #[test]
    fn quotes_the_editor_closed_for_you() {
        // `"` typed at the end of a line: the editor adds a closer, the rules keep an opener and its closer.
        let open = replacement(Kind::Markdown, "say \"", Some('"'), true).unwrap();
        assert_eq!((open.range, open.text, open.caret), (4..6, "\u{201C}\u{201D}", 7));
        // After a word the same key is a closer and takes the editor's extra one with it.
        let close = replacement(Kind::Markdown, "say \"hi\"", Some('"'), true).unwrap();
        assert_eq!((close.range, close.text, close.caret), (7..9, "\u{201D}", 10));
        // Typing the closer in front of the curly one steps over it.
        let over = replacement(Kind::Markdown, "\u{201C}hi\"", Some('\u{201D}'), false).unwrap();
        assert_eq!((over.range, over.text, over.caret), (5..9, "\u{201D}", 8));
        // A pair with no matching key after it is not one.
        assert_eq!(replacement(Kind::Markdown, "say \"", None, true), None);
        assert_eq!(replacement(Kind::Markdown, "say (", Some(')'), true), None);
    }

    #[test]
    fn four_dots_stay() {
        assert_eq!(type_in(Kind::Plain, "...."), "\u{2026}.");
        assert_eq!(replacement(Kind::Plain, "a....", None, false), None);
    }

    #[test]
    fn markdown_markup_is_left_alone() {
        assert_eq!(type_in(Kind::Markdown, "`it's..."), "`it's...");
        assert_eq!(type_in(Kind::Markdown, "`a` it's"), "`a` it\u{2019}s");
        assert_eq!(type_in(Kind::Markdown, "[a](http://x.com/it's"), "[a](http://x.com/it's");
        assert_eq!(type_in(Kind::Markdown, "see https://x.com/a--b"), "see https://x.com/a--b");
        assert_eq!(type_in(Kind::Markdown, "<a href=\"x"), "<a href=\"x");
        assert_eq!(type_in(Kind::Markdown, "```rust \"x"), "```rust \"x");
        assert_eq!(type_in(Kind::Markdown, "[a]: http://x \"t"), "[a]: http://x \"t");
        assert_eq!(type_in(Kind::Plain, "`it's`"), "`it\u{2019}s`");
    }

    #[test]
    fn fences_and_front_matter() {
        let md = Kind::Markdown;
        assert!(in_front_matter_or_fence(md, ["---", "title: x"], "a"));
        assert!(!in_front_matter_or_fence(md, ["---", "title: x", "---", "text"], "a"));
        assert!(in_front_matter_or_fence(md, ["text", "```rust", "let a = 1;"], "a"));
        assert!(!in_front_matter_or_fence(md, ["```", "x", "```", "after"], "a"));
        assert!(in_front_matter_or_fence(md, ["~~~~", "```", "x"], "a"));
        assert!(!in_front_matter_or_fence(md, ["a", "---", "b"], "a"));
        assert!(in_front_matter_or_fence(md, ["- item", "    ```rust", "    let a;"], "a"));
        assert_eq!(type_in(Kind::Plain, "see www.x.com/it's"), "see www.x.com/it's");
    }

    #[test]
    fn indented_code_blocks_stay_as_typed() {
        let md = Kind::Markdown;
        let none: [&str; 0] = [];
        // After a blank line, four spaces or a tab is code; so is the next line of the block.
        assert!(in_front_matter_or_fence(md, ["text", ""], "    say \"hi\""));
        assert!(in_front_matter_or_fence(md, ["text", ""], "\tsay \"hi\""));
        assert!(in_front_matter_or_fence(md, ["", "    a", ""], "    b -- c"));
        assert!(in_front_matter_or_fence(md, none, "    a"));
        // Not after a paragraph line (a continuation), in a list item, or in plain text.
        assert!(!in_front_matter_or_fence(md, ["text"], "    say \"hi\""));
        assert!(!in_front_matter_or_fence(md, ["- item", ""], "    more \"text\""));
        assert!(!in_front_matter_or_fence(md, ["1. item", "", "    para", ""], "    more"));
        assert!(!in_front_matter_or_fence(md, ["text", ""], "  two spaces"));
        assert!(!in_front_matter_or_fence(Kind::Plain, ["text", ""], "    say \"hi\""));
        // A list that ended with a flush paragraph is over; code can follow.
        assert!(in_front_matter_or_fence(md, ["- item", "", "plain", ""], "    code"));
        // A flush line ends a code block; the next indented line continues that paragraph.
        assert!(!in_front_matter_or_fence(md, ["", "    a", "flush"], "    b"));
    }

    #[test]
    fn front_matter_and_fences_at_the_edges() {
        let md = Kind::Markdown;
        let none: [&str; 0] = [];
        // Front matter closes on `---` or `...`, trailing blanks allowed, and only from row 0.
        assert!(in_front_matter_or_fence(md, ["---"], "a"));
        assert!(!in_front_matter_or_fence(md, ["---", "---"], "a"));
        assert!(!in_front_matter_or_fence(md, ["---  ", "k: v", "...  "], "a"));
        assert!(!in_front_matter_or_fence(md, ["", "---", "k: v"], "a"));
        assert!(!in_front_matter_or_fence(md, none, "a"));
        // A fence needs a matching closer that is at least as long and has nothing after it.
        assert!(in_front_matter_or_fence(md, ["````", "```"], "a"));
        assert!(in_front_matter_or_fence(md, ["```", "``` x"], "a"));
        assert!(in_front_matter_or_fence(md, ["```", "~~~"], "a"));
        assert!(!in_front_matter_or_fence(md, ["````", "`````"], "a"));
        assert!(!in_front_matter_or_fence(md, ["``", "x"], "a"));
        // A fence inside front matter is only front matter.
        assert!(!in_front_matter_or_fence(md, ["---", "```", "---"], "a"));
    }

    #[test]
    fn the_rope_feeds_the_check_without_allocating() {
        use gpui_kit::component::input::{Rope, RopeExt as _};
        let mut text = String::from("---\ntitle: x\n---\n");
        for row in 3..4000 {
            text.push_str(&format!("line {row} \u{e9}{}\n", "word ".repeat(30)));
        }
        text.push_str("```rust\nlet a = 1;\n");
        let rope = Rope::from(text.as_str());
        let row = 4001;
        let above = || rope.iter_lines().take(row).map(|line| LinePrefix::of(line.len(), |at| line.chunk(at).0));
        let strings: Vec<String> = (0..row).map(|r| rope.slice_line(r).chars().take(64).collect()).collect();
        assert!(above().zip(&strings).all(|(prefix, text)| prefix.as_ref() == text.as_str()));
        // Same answers as owned strings, with the fence open.
        assert_eq!(in_front_matter_or_fence(Kind::Markdown, above(), "a"), in_front_matter_or_fence(Kind::Markdown, &strings, "a"));
        assert!(in_front_matter_or_fence(Kind::Markdown, above(), "a"));
        let (_, counted) = crate::test_alloc::allocations(|| {
            for _ in 0..200 {
                assert!(in_front_matter_or_fence(Kind::Markdown, above(), "a"));
            }
        });
        assert_eq!(counted, 0);
    }

    #[test]
    fn which_documents_get_it() {
        assert_eq!(kind(Some(Path::new("a.md")), "markdown"), Some(Kind::Markdown));
        assert_eq!(kind(Some(Path::new("a.txt")), "text"), Some(Kind::Plain));
        assert_eq!(kind(None, "text"), Some(Kind::Plain));
        assert_eq!(kind(Some(Path::new("a.rs")), "rust"), None);
        assert_eq!(kind(Some(Path::new("a.conf")), "text"), None);
        assert_eq!(kind(None, "rust"), None);
    }
}
