//! Guesses a file's indent from its first lines, so guides, Tab and paste-indent use its width.

use gpui_kit::component::input::TabSize;

/// Lines looked at; the start of a file shows its style, and a huge file need not be walked.
const SCAN_LINES: usize = 1_000;
/// Width used when the text has no indent to learn from, and the width a tab draws.
pub const DEFAULT_WIDTH: usize = 4;
/// Widest step counted; larger jumps are dedents or continuation lines, not one level.
const MAX_STEP: usize = 8;

/// The indent of `text`: hard tabs when most indented lines start with a tab, else the most
/// common step between the leading-space widths of consecutive non-blank lines (ties go to the
/// narrower step). No signal gives four spaces. A tab always draws `DEFAULT_WIDTH` columns.
pub fn detect(text: &str) -> TabSize {
    let (mut tabbed, mut spaced) = (0usize, 0usize);
    let mut steps = [0usize; MAX_STEP + 1];
    let mut previous = 0usize;
    for line in text.lines().take(SCAN_LINES) {
        if line.trim().is_empty() {
            continue;
        }
        match line.as_bytes()[0] {
            b'\t' => {
                tabbed += 1;
                continue;
            }
            b' ' => spaced += 1,
            _ => {}
        }
        let width = line.bytes().take_while(|&b| b == b' ').count();
        let step = width.abs_diff(previous);
        if (2..=MAX_STEP).contains(&step) {
            steps[step] += 1;
        }
        previous = width;
    }
    if tabbed > spaced {
        return TabSize { tab_size: DEFAULT_WIDTH, hard_tabs: true };
    }
    let mut best = (0, DEFAULT_WIDTH);
    for (step, &count) in steps.iter().enumerate().skip(2) {
        if count > best.0 {
            best = (count, step);
        }
    }
    TabSize { tab_size: best.1, hard_tabs: false }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_space_file() {
        let tab = detect("fn a() {\n  if x {\n    y();\n  }\n}\n");
        assert_eq!((tab.tab_size, tab.hard_tabs), (2, false));
    }

    #[test]
    fn four_space_file() {
        let tab = detect("fn a() {\n    if x {\n        y();\n    }\n}\n");
        assert_eq!((tab.tab_size, tab.hard_tabs), (4, false));
    }

    #[test]
    fn tab_file() {
        let tab = detect("fn a() {\n\tif x {\n\t\ty();\n\t}\n}\n");
        assert_eq!((tab.tab_size, tab.hard_tabs), (4, true));
    }

    #[test]
    fn no_signal_is_four_spaces() {
        for text in ["", "one\ntwo\n", "\n\n   \n"] {
            let tab = detect(text);
            assert_eq!((tab.tab_size, tab.hard_tabs), (4, false));
        }
    }

    #[test]
    fn mixed_picks_the_most_common() {
        let tab = detect("a\n  b\n    c\n      d\n        e\na\n    f\n");
        assert_eq!((tab.tab_size, tab.hard_tabs), (2, false));
        let tab = detect("a\n    b\n        c\n  d\n    e\n        f\n");
        assert_eq!((tab.tab_size, tab.hard_tabs), (4, false));
    }
}
