//! What the text around the caret says about `[[`: the open link before the caret, and whether
//! it sits in a code span or a fenced block. Pure text, so the rules are tested here.

/// The longest line a `[[` is looked for in; a longer one (minified text) never offers the list.
pub(super) const MAX_LINE: usize = 2000;
/// The longest name typed after `[[`.
const MAX_QUERY: usize = 120;

/// An unfinished `[[query` before the caret.
#[derive(Debug, PartialEq)]
pub(super) struct Open {
    /// Where the `[[` starts in the line.
    pub start: usize,
    /// What was typed after it.
    pub query: String,
}

/// The `[[query` that ends at `caret` in `line` (the line's text, `caret` a byte offset in it):
/// the last `[[` before the caret with nothing after it that closes or splits a link (`]`, `[`
/// or `|`, which starts an alias the user types by hand). `None` for a `[[` inside a code span.
pub(super) fn open_link(line: &str, caret: usize) -> Option<Open> {
    if line.len() > MAX_LINE || !line.is_char_boundary(caret) {
        return None;
    }
    let before = &line[..caret];
    let start = before.rfind("[[")?;
    let query = &before[start + 2..];
    if query.chars().count() > MAX_QUERY || query.contains([']', '[', '|']) {
        return None;
    }
    if in_code_span(line, start) {
        return None;
    }
    Some(Open { start, query: query.to_owned() })
}

/// Whether byte `at` of `line` is inside a code span: between a run of backticks and the next
/// run of the same length. A backslash outside a span makes the next character plain.
fn in_code_span(line: &str, at: usize) -> bool {
    let bytes = line.as_bytes();
    let mut open: Option<(usize, usize)> = None;
    let mut ix = 0;
    while ix < bytes.len() {
        match bytes[ix] {
            b'\\' if open.is_none() => ix += 2,
            b'`' => {
                let run = bytes[ix..].iter().take_while(|&&byte| byte == b'`').count();
                match open {
                    Some((from, len)) if len == run => {
                        if from < at && at < ix + run {
                            return true;
                        }
                        open = None;
                    }
                    None => open = Some((ix, run)),
                    Some(_) => {}
                }
                ix += run;
            }
            _ => ix += 1,
        }
    }
    false
}

/// Whether the line after `lines` (every line above it, in order) is inside a fenced code block:
/// an opening run of three or more backticks or tildes, up to three spaces in, and no closing
/// run of at least that length yet.
pub(super) fn in_fence<L: IntoIterator<Item = char>>(lines: impl Iterator<Item = L>) -> bool {
    let mut open: Option<(char, usize)> = None;
    for line in lines {
        let mut chars = line.into_iter().peekable();
        let mut indent = 0;
        while indent < 4 && chars.next_if_eq(&' ').is_some() {
            indent += 1;
        }
        if indent > 3 {
            continue;
        }
        let Some(&mark) = chars.peek().filter(|&&ch| ch == '`' || ch == '~') else { continue };
        let mut run = 0;
        while chars.next_if_eq(&mark).is_some() {
            run += 1;
        }
        if run < 3 {
            continue;
        }
        let rest: Vec<char> = chars.collect();
        match open {
            Some((fence, len)) => {
                if mark == fence && run >= len && rest.iter().all(|ch| ch.is_whitespace()) {
                    open = None;
                }
            }
            None => {
                // A backtick fence's info text can't hold a backtick: that line is a code span.
                if !(mark == '`' && rest.contains(&'`')) {
                    open = Some((mark, run));
                }
            }
        }
    }
    open.is_some()
}

#[cfg(test)]
mod tests {
    use super::{Open, in_fence, in_code_span, open_link};

    fn open(line: &str) -> Option<Open> {
        open_link(line, line.len())
    }

    fn fenced(text: &str) -> bool {
        in_fence(text.lines().map(str::chars))
    }

    #[test]
    fn reads_the_query_after_the_last_brackets() {
        assert_eq!(open("see [[pla"), Some(Open { start: 4, query: "pla".into() }));
        assert_eq!(open("[["), Some(Open { start: 0, query: String::new() }));
        assert_eq!(open("[[a b"), Some(Open { start: 0, query: "a b".into() }));
        assert_eq!(open("[[one]] and [[tw"), Some(Open { start: 12, query: "tw".into() }));
    }

    #[test]
    fn a_closed_or_split_link_offers_nothing() {
        assert_eq!(open("[[one]]"), None);
        assert_eq!(open("[[one|alias"), None);
        assert_eq!(open("[[a[b"), None);
        assert_eq!(open("no brackets"), None);
        assert_eq!(open("[ [x"), None);
    }

    #[test]
    fn the_caret_can_sit_before_the_rest_of_the_line() {
        assert_eq!(open_link("[[ab]] tail", 4), Some(Open { start: 0, query: "ab".into() }));
        assert_eq!(open_link("é[[ab", 2), None);
    }

    #[test]
    fn code_spans_are_left_alone() {
        assert_eq!(open("use `[[x"), Some(Open { start: 5, query: "x".into() }));
        assert_eq!(open_link("use `[[x` now", 8), None);
        assert!(in_code_span("a ``x [[ y`` b", 7));
        assert!(!in_code_span("a `x` [[ y", 8));
        assert!(!in_code_span("a \\`x [[ y`", 6));
        assert!(!in_code_span("`a` `b` [[", 8));
    }

    #[test]
    fn fences_open_and_close() {
        assert!(fenced("text\n```rust\ncode"));
        assert!(!fenced("```\ncode\n```\nafter"));
        assert!(fenced("~~~\ncode\n```\nstill"));
        assert!(fenced("````\n```\nstill"));
        assert!(!fenced("````\n```\n````\nout"));
        assert!(!fenced("    ```\nindented code, not a fence"));
        assert!(!fenced("``` not `a fence`\ntext"));
        assert!(!fenced("plain\ntext"));
    }
}
