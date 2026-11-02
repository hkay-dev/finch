//! A few lines of text as a small block of plain monospace, one line tinted: the tab peek's five
//! lines around a caret, and what Open quickly's peek shows of a match. The caller reads the lines
//! (a handful from a rope, never the whole text); this only draws them.

use gpui_kit::{prelude::FluentBuilder as _, *};

use crate::tokens::{RADIUS_TAB, SNIPPET_LINE, SNIPPET_PAD};

/// `lines` one under another in `family` at `size`, in `ink`, with row `caret_row` (0-based) washed in `tint`. A line wider than the
/// block is cut at its edge, not wrapped, so the block's height is its line count.
pub fn snippet(lines: &[SharedString], caret_row: usize, family: SharedString, size: Pixels, ink: Hsla, tint: Hsla) -> Div {
    let line = size * SNIPPET_LINE;
    div()
        .flex()
        .flex_col()
        .font_family(family)
        .text_size(size)
        .line_height(line)
        .text_color(ink)
        .children(lines.iter().enumerate().map(|(row, text)| {
            div()
                .h(line)
                .w_full()
                .flex_none()
                .px(px(SNIPPET_PAD))
                .rounded(RADIUS_TAB)
                .overflow_hidden()
                .whitespace_nowrap()
                .when(row == caret_row, |row| row.bg(tint))
                .child(text.clone())
        }))
}
