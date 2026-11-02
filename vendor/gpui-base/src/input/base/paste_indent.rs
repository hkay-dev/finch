//! Paste indentation: a block of code pasted into a code editor takes the
//! indentation of the line it lands on, as in Zed (`crates/language/src/buffer.rs`,
//! `compute_autoindents`), instead of keeping the indent of the file it was copied from.

use std::borrow::Cow;
use std::ops::Range;

use super::mode::LayoutMode;
use crate::input::{InputBaseState, InputModeKind, RopeExt as _, TabSize};

/// Languages whose leading whitespace is content or structure (prose, Makefile recipe tabs,
/// diff line markers, Python and YAML nesting), so a paste keeps its lines as they are.
fn is_prose_language(name: &str) -> bool {
    ["", "text", "plain", "plaintext", "markdown", "md", "make", "diff", "python", "yaml", "yml"]
        .iter()
        .any(|prose| name.eq_ignore_ascii_case(prose))
}

/// The column after `ch`, with tabs running to the next multiple of `tab_size`.
fn advance(col: usize, ch: char, tab_size: usize) -> usize {
    if ch == '\t' {
        col + tab_size - col % tab_size
    } else {
        col + 1
    }
}

/// Columns of leading spaces and tabs in `line`.
fn indent_width(line: &str, tab_size: usize) -> usize {
    line.chars()
        .take_while(|ch| matches!(ch, ' ' | '\t'))
        .fold(0, |col, ch| advance(col, ch, tab_size))
}

fn is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// Shifts every line of `text` after the first so the block's own indent (the
/// least indent of its non-blank lines after the first) becomes `target`
/// columns. Blank lines lose their whitespace. Indentation is written with
/// tabs when the editor uses hard tabs, else spaces.
///
/// When the first line starts with whitespace and the caret has only
/// whitespace before it (`caret_in_indent`), whole lines were copied: the
/// block's indent is the least over all its lines including the first, and the
/// first line keeps only what it has beyond that (the caret column already
/// supplies the indent). With text before the caret the first line is left
/// alone and the rule above applies.
fn reindent(text: &str, target: usize, tab: TabSize, caret_in_indent: bool) -> Cow<'_, str> {
    let tab_size = tab.tab_size.max(1);
    if !text.contains('\n') {
        return Cow::Borrowed(text);
    }
    let mut lines = text.split('\n');
    let first = lines.next().unwrap_or("");
    let whole_lines = caret_in_indent && first.starts_with([' ', '\t']) && !is_blank(first);
    let rest_min = lines
        .clone()
        .filter(|line| !is_blank(line))
        .map(|line| indent_width(line, tab_size))
        .min();
    let first_width = indent_width(first, tab_size);
    let original = match (whole_lines, rest_min) {
        (true, Some(rest)) => rest.min(first_width),
        (true, None) => first_width,
        (false, Some(rest)) => rest,
        (false, None) => return Cow::Borrowed(text),
    };
    if !whole_lines && original == target {
        return Cow::Borrowed(text);
    }

    let push_indent = |out: &mut String, width: usize| {
        if tab.hard_tabs {
            out.extend(std::iter::repeat_n('\t', width / tab_size));
            out.extend(std::iter::repeat_n(' ', width % tab_size));
        } else {
            out.extend(std::iter::repeat_n(' ', width));
        }
    };
    let mut out = String::with_capacity(text.len() + 16);
    if whole_lines {
        push_indent(&mut out, first_width - original);
        out.push_str(first.trim_start_matches([' ', '\t']));
    } else {
        out.push_str(first);
    }
    for line in lines {
        out.push('\n');
        if is_blank(line) {
            if line.ends_with('\r') {
                out.push('\r');
            }
            continue;
        }
        push_indent(
            &mut out,
            (indent_width(line, tab_size) + target).saturating_sub(original),
        );
        out.push_str(line.trim_start_matches([' ', '\t']));
    }
    Cow::Owned(out)
}

impl<M: InputModeKind> InputBaseState<M> {
    /// Whether a paste here re-indents, and Edit > Re-indent works: only in a code
    /// editor whose language is not text, markdown, make, diff, Python or YAML.
    pub(super) fn paste_reindents(&self) -> bool {
        match &self.mode {
            LayoutMode::CodeEditor { language, .. } => !is_prose_language(&language.name()),
            _ => false,
        }
    }

    /// The column pasted lines should start at for a caret at `offset`: the
    /// indent of the caret's line, or the caret column when the caret sits
    /// inside that indent.
    fn paste_indent_at(&self, offset: usize) -> (usize, bool) {
        let tab_size = self.mode.tab_size().tab_size.max(1);
        let line_start = self.text.line_start_offset(self.text.offset_to_point(offset).row);
        let mut col = 0;
        // Spaces and tabs are one byte each, so the byte position is the char count.
        for (pos, ch) in (line_start..).zip(self.text.slice(line_start..).chars()) {
            if !matches!(ch, ' ' | '\t') || pos >= offset {
                break;
            }
            col = advance(col, ch, tab_size);
        }
        let in_indent = self
            .text
            .slice(line_start..offset)
            .chars()
            .all(|ch| matches!(ch, ' ' | '\t'));
        (col, in_indent)
    }

    /// The edits a paste of `text` makes, one per selection in document order,
    /// each block shifted to its own caret line. `None` when the paste keeps
    /// the text as it is: a single line, or a language that is not code.
    pub(super) fn paste_reindent_edits(&self, text: &str) -> Option<Vec<(Range<usize>, String)>> {
        if !text.contains('\n') || !self.paste_reindents() {
            return None;
        }
        let tab = self.mode.tab_size();
        let mut selections: Vec<_> = self.selections.iter().copied().collect();
        selections.sort_by_key(|sel| sel.start);
        Some(
            selections
                .iter()
                .map(|sel| {
                    let (target, in_indent) = self.paste_indent_at(sel.start);
                    let text = reindent(text, target, tab, in_indent).into_owned();
                    (sel.start..sel.end, text)
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOFT4: TabSize = TabSize {
        tab_size: 4,
        hard_tabs: false,
    };
    const HARD4: TabSize = TabSize {
        tab_size: 4,
        hard_tabs: true,
    };

    #[test]
    fn indentation_is_syntax_in_python_and_yaml() {
        for name in ["python", "Python", "yaml", "yml", "make", "markdown"] {
            assert!(is_prose_language(name), "{name} keeps its pasted lines");
        }
        assert!(!is_prose_language("rust"));
    }

    #[test]
    fn block_from_eight_pasted_at_four() {
        let text = "if x {\n        a();\n            b();\n        c();";
        // Lines after the first keep their shape relative to the block's 8.
        assert_eq!(
            reindent(text, 4, SOFT4, false),
            "if x {\n    a();\n        b();\n    c();"
        );
    }

    #[test]
    fn block_from_eight_pasted_at_column_zero() {
        let text = "fn a() {\n        x();\n\n        y();\n        z();";
        assert_eq!(
            reindent(text, 0, SOFT4, false),
            "fn a() {\nx();\n\ny();\nz();"
        );
    }

    #[test]
    fn blank_lines_stay_blank() {
        let text = "a\n        b\n        \n\n        c";
        assert_eq!(reindent(text, 4, SOFT4, false), "a\n    b\n\n\n    c");
        assert_eq!(reindent("a\r\n    b\r\n  \r\n    c", 0, SOFT4, false), "a\r\nb\r\n\r\nc");
    }

    #[test]
    fn single_line_is_unchanged() {
        assert_eq!(reindent("    foo()", 8, SOFT4, true), "    foo()");
        assert_eq!(reindent("foo()\n", 8, SOFT4, false), "foo()\n");
    }

    #[test]
    fn same_indent_is_unchanged() {
        let text = "a\n    b  \n    \n    c";
        assert_eq!(reindent(text, 4, SOFT4, false), text);
    }

    #[test]
    fn hard_tabs_follow_the_editor_setting() {
        let text = "a\n        b\n            c";
        assert_eq!(reindent(text, 4, HARD4, false), "a\n\tb\n\t\tc");
        assert_eq!(reindent("a\n\t\tb\n\t\t\tc", 4, SOFT4, false), "a\n    b\n        c");
        assert_eq!(reindent(text, 6, HARD4, false), "a\n\t  b\n\t\t  c");
        assert_eq!(
            reindent("\t\ta\n\t\t\tb\n", 4, HARD4, true),
            "a\n\t\tb\n"
        );
    }

    #[test]
    fn whole_lines_pasted_at_a_caret_in_an_indent() {
        // 8-space whole lines onto an empty line with the caret at column 4:
        // the caret column is the indent, the first line adds nothing.
        let text = "        if x {\n            a();\n        }\n";
        assert_eq!(
            reindent(text, 4, SOFT4, true),
            "if x {\n        a();\n    }\n"
        );
        // A first line deeper than the rest keeps the difference.
        assert_eq!(reindent("    a\n  b", 0, SOFT4, true), "  a\nb");
        // One whole line with its newline.
        assert_eq!(reindent("        foo()\n", 4, SOFT4, true), "foo()\n");
    }

    #[test]
    fn whole_lines_pasted_after_text_keep_the_first_line() {
        // Text before the caret: the first line is untouched, the rest shift
        // by the caret line's indent minus their own minimum.
        let text = "        a\n        b\n            c";
        assert_eq!(reindent(text, 4, SOFT4, false), "        a\n    b\n        c");
    }

    #[test]
    fn prose_languages_are_not_code() {
        for name in ["", "text", "Markdown", "md", "make", "diff"] {
            assert!(is_prose_language(name), "{name}");
        }
        assert!(!is_prose_language("rust"));
    }
}
