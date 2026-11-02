//! Selected text dragged out of an editor. The editor starts the drag when a press inside its
//! selection moves (`on_text_drag`, set where the document builds it); the explorer's folder rows
//! and tree take the drop and open a new file's name field prefilled from the text (`file_name`).

use std::path::Path;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;

use crate::tokens::{RADIUS_FLOAT_CARD, ROW_GAP, ROW_HEIGHT, TEXT_BODY, TEXT_BODY_LINE, TOOLTIP_MAX_W};

/// The longest stem a dropped text names its file with, in characters.
const NAME_CHARS: usize = 40;
/// The longest line the drag card shows, in characters.
const CARD_CHARS: usize = 60;

/// What a drag of selected text carries.
#[derive(Clone, Debug)]
pub struct DraggedText {
    pub text: String,
    /// The extension of the file the text came from, without the dot; none for an unsaved page.
    pub ext: Option<String>,
}

/// Starts a drag of `text` taken from a file at `path` (none for an unsaved page).
pub fn start(text: String, path: Option<&Path>, window: &mut Window, cx: &mut App) {
    let ext = path.and_then(|path| path.extension()).map(|ext| ext.to_string_lossy().into_owned()).filter(|ext| !ext.is_empty());
    let label: SharedString = first_line(&text, CARD_CHARS).into();
    let (card, border) = crate::tooltip::float_fill(cx.theme().background, cx.theme().sidebar_foreground, crate::theme::is_light(cx));
    let (ink, light) = (crate::theme::ink(crate::tokens::ALPHA_PRIMARY, cx), crate::theme::is_light(cx));
    let view = cx.new(|_| Card { label, ink, card, border, light });
    let offset = Point::default();
    cx.start_active_drag(
        AnyDrag { view: view.into(), value: std::sync::Arc::new(DraggedText { text, ext }), cursor_offset: offset, cursor_style: None, external_payload_source: None },
        window,
    );
}

/// The first line with something on it, trimmed, whitespace runs folded to one space, cut to `max` characters.
fn first_line(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|line| !line.is_empty()).unwrap_or("");
    let folded = line.split_whitespace().collect::<Vec<_>>().join(" ");
    folded.chars().take(max).collect()
}

/// The name a dropped text gives its new file: its first line made safe for a name, cut to about
/// 40 characters, then the source's extension (else `txt`). A text with nothing to name it by is
/// `untitled`.
pub fn file_name(text: &str, ext: Option<&str>) -> String {
    let line = first_line(text, usize::MAX);
    let safe: String = line.chars().map(|c| if c == '/' || c == ':' || c == '\\' || c.is_control() { ' ' } else { c }).collect();
    let folded = safe.split_whitespace().collect::<Vec<_>>().join(" ");
    // No leading dot (a hidden file) and no trailing dot or space.
    let cut: String = folded.trim_start_matches('.').chars().take(NAME_CHARS).collect();
    let stem = cut.trim_end_matches(['.', ' ']);
    let stem = if stem.is_empty() { "untitled" } else { stem };
    let ext = ext.filter(|ext| !ext.is_empty() && !ext.contains(['/', ':', '\0'])).unwrap_or("txt");
    format!("{stem}.{ext}")
}

/// What follows the pointer: the text's first line on a small card.
struct Card {
    label: SharedString,
    ink: Hsla,
    card: Hsla,
    border: Hsla,
    light: bool,
}

impl Render for Card {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().pl(ROW_GAP).pt(ROW_GAP).child(
            crate::tooltip::float(RADIUS_FLOAT_CARD, self.card, self.border, self.light, true)
                .h(ROW_HEIGHT)
                .max_w(px(TOOLTIP_MAX_W))
                .px(ROW_GAP)
                .flex()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_color(self.ink)
                .text_size(TEXT_BODY)
                .line_height(TEXT_BODY_LINE)
                .child(self.label.clone()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::file_name;

    #[test]
    fn names_come_from_the_first_line_made_safe() {
        assert_eq!(file_name("  \n  Meeting notes: Q3 / plan\nmore", Some("md")), "Meeting notes Q3 plan.md");
        assert_eq!(file_name("", None), "untitled.txt");
        assert_eq!(file_name("...", Some("rs")), "untitled.rs");
        assert_eq!(file_name(".env file", None), "env file.txt");
        assert_eq!(file_name("tab\tseparated\t\tline", None), "tab separated line.txt");
        let long = file_name(&"abcdefghij".repeat(10), Some("py"));
        assert_eq!(long, format!("{}.py", "abcdefghij".repeat(4)));
    }
}
