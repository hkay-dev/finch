//! Ticking a task list box from the preview: finding the `[ ]` of a task item in the source and
//! flipping it through the editor, so the change is an edit like a typed one (it can be undone
//! and it makes the file unsaved).

use std::cell::Cell;
use std::ops::Range;
use std::time::{Duration, Instant};

use gpui_kit::component::input::{EditorState, RopeExt as _};
use gpui_kit::*;

/// How many bytes after an item's start `toggle_task` reads: a marker, its spaces and the box.
const WINDOW: usize = 48;

thread_local! {
    /// When `toggle_task` last edited; the edit's change event reaches `Preview::schedule` within the same dispatch.
    static TICKED: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// How long a tick's mark waits for its change event before it is stale.
const TICK_EVENT_WITHIN: Duration = Duration::from_millis(100);

/// How long the ticked line's wash fades: the paste wash's length (`INSERT_WASH`).
const TICK_WASH: f32 = 0.3;

/// Whether the edit being scheduled is a preview tick; the mark is used up.
pub fn take_ticked() -> bool {
    TICKED.with(|ticked| ticked.take()).is_some_and(|at| at.elapsed() < TICK_EVENT_WITHIN)
}

/// Where the box of the task item that starts at the beginning of `text` sits (the three bytes
/// `[ ]`, `[x]` or `[X]`, counted from the start of `text`) and what flips it. `None` when the
/// text doesn't open with a list marker (`-`, `*`, `+`, `1.` or `1)`), spaces, and a box that is
/// followed by a space or the end.
fn flip(text: &str) -> Option<(Range<usize>, &'static str)> {
    let bytes = text.as_bytes();
    // An item's span can start on the spaces before its marker.
    let lead = bytes.iter().take_while(|b| matches!(b, b' ' | b'\t')).count();
    let mut at = match bytes.get(lead)? {
        b'-' | b'*' | b'+' => lead + 1,
        b'0'..=b'9' => {
            let digits = bytes[lead..].iter().take_while(|b| b.is_ascii_digit()).count();
            if digits > 9 || !matches!(bytes.get(lead + digits), Some(b'.' | b')')) {
                return None;
            }
            lead + digits + 1
        }
        _ => return None,
    };
    let spaces = bytes[at..].iter().take_while(|b| matches!(b, b' ' | b'\t')).count();
    if spaces == 0 {
        return None;
    }
    at += spaces;
    let (open, mark, close) = (*bytes.get(at)?, *bytes.get(at + 1)?, *bytes.get(at + 2)?);
    if open != b'[' || close != b']' || !matches!(bytes.get(at + 3), None | Some(b' ' | b'\t' | b'\n' | b'\r')) {
        return None;
    }
    let flipped = match mark {
        b' ' => "[x]",
        b'x' | b'X' => "[ ]",
        _ => return None,
    };
    Some((at..at + 3, flipped))
}

/// Flips the box of the task item whose source spans `item` of the editor's text: `[ ]` becomes
/// `[x]` and `[x]` or `[X]` becomes `[ ]`. The bytes there are read again first, and if they are
/// no longer a task item whose box lies inside the span (the text moved on since the preview was
/// parsed) nothing changes, and neither does it when the text's length is not `parsed_len`, the
/// length the preview parsed; an edit that kept the length (a line moved with Option-Up/Down)
/// still passes. The three bytes are replaced through the editor by explicit range, so
/// a composition in progress isn't touched, undo works and the file turns unsaved; the caret and
/// selection stay where they were. Returns whether it changed anything.
pub fn toggle_task(editor: &Entity<EditorState>, item: Range<usize>, parsed_len: usize, window: &mut Window, cx: &mut App) -> bool {
    editor.update(cx, |state, cx| {
        let rope = state.text();
        let item_start = item.start;
        // Any edit that changed the length since the parse makes the click stale; an edit that
        // kept the length (Option-Up/Down moving a line) still passes.
        if is_stale(rope.len(), parsed_len, &item) {
            return false;
        }
        // The window may end inside a character, so shorten it until it doesn't.
        let reach = (item_start + WINDOW).min(item.end);
        let Some(head) = (0..4).find_map(|cut| rope.try_slice(item_start..reach.checked_sub(cut)?.max(item_start)).ok()) else {
            return false;
        };
        let head = head.to_string();
        let Some((range, flipped)) = flip(&head) else {
            return false;
        };
        let (selection, scroll) = (state.selected_range(), state.scroll_offset());
        TICKED.with(|ticked| ticked.set(Some(Instant::now())));
        // Three bytes for three bytes: the caret and selection keep their offsets.
        state.replace_range(item_start + range.start..item_start + range.end, flipped, selection.end, window, cx);
        // Putting the caret back scrolls to it, which would yank both panes; the click leaves
        // the editor where it was.
        state.set_selected_range(selection, cx);
        state.set_scroll_offset(scroll, cx);
        // The flip is three bytes the editor shows nowhere else: wash the item's first line once
        // so the change is seen, only when that line is on screen.
        let (row, line) = {
            let rope = state.text();
            let row = rope.offset_to_point(item_start + range.start).row;
            (row as usize, rope.line_start_offset(row)..rope.line_end_offset(row))
        };
        if state.visible_row_range().is_some_and(|rows| rows.contains(&row)) {
            state.wash_range(line, TICK_WASH, cx);
        }
        true
    })
}

/// Whether a click's item span no longer fits the text: the length changed since the parse, or the span is empty or past the end.
/// The click then changes nothing, and the window beeps the way a link that leads nowhere does.
fn is_stale(len: usize, parsed_len: usize, item: &Range<usize>) -> bool {
    len != parsed_len || item.start >= len || item.end > len || item.end <= item.start
}

#[cfg(test)]
mod tests {
    use super::{flip, is_stale};

    #[test]
    fn a_click_on_text_that_moved_on_is_stale() {
        assert!(!is_stale(100, 100, &(10..20)));
        assert!(is_stale(101, 100, &(10..20)));
        assert!(is_stale(100, 100, &(90..120)));
        assert!(is_stale(100, 100, &(100..100)));
    }

    #[test]
    fn open_box_gets_ticked_and_ticked_ones_open() {
        assert_eq!(flip("- [ ] one"), Some((2..5, "[x]")));
        assert_eq!(flip("- [x] one"), Some((2..5, "[ ]")));
        assert_eq!(flip("- [X] one"), Some((2..5, "[ ]")));
    }

    #[test]
    fn markers_and_spacing() {
        assert_eq!(flip("* [ ] a"), Some((2..5, "[x]")));
        assert_eq!(flip("+   [ ] a"), Some((4..7, "[x]")));
        assert_eq!(flip("-\t[ ] a"), Some((2..5, "[x]")));
        assert_eq!(flip("12. [x] a"), Some((4..7, "[ ]")));
        assert_eq!(flip("3) [ ] a"), Some((3..6, "[x]")));
    }

    #[test]
    fn an_item_span_may_start_on_its_indent() {
        assert_eq!(flip("  - [ ] b"), Some((4..7, "[x]")));
        assert_eq!(flip("    - [x] b"), Some((6..9, "[ ]")));
        assert_eq!(flip("\t- [ ] b"), Some((3..6, "[x]")));
        assert_eq!(flip("   1. [ ] b"), Some((6..9, "[x]")));
        assert_eq!(flip("   text"), None);
    }

    #[test]
    fn box_alone_on_its_line_counts() {
        assert_eq!(flip("- [ ]"), Some((2..5, "[x]")));
        assert_eq!(flip("- [ ]\n  more"), Some((2..5, "[x]")));
    }

    #[test]
    fn anything_else_is_left_alone() {
        for text in ["", "text", "- one", "- [y] one", "- [ ]one", "-[ ] one", "- [] one", "1 [ ] one", "- (x) one", "- [xx]"] {
            assert_eq!(flip(text), None, "{text:?}");
        }
    }
}
