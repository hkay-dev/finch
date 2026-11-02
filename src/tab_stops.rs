//! Template tab stops. A template's text can hold `{{date}}` (today, `2026-11-01`), `{{title}}`
//! and `{{cursor}}`. File > New from template fills in the dates, takes the other two markers out
//! and keeps their places: the caret goes to the first `{{title}}` spot, Tab hops to the next one
//! (more `{{title}}`s, then `{{cursor}}`s), and the move is the editor's own caret glide.
//!
//! The stops live in a short-lived record per editor (`Hops`): Escape, a click in the editor or a
//! Tab past the last stop ends it, and the editor's closing does too. While it lasts, each text
//! change shifts the later stops by what was typed (the change is found by comparing the text
//! before and after, which is why a long text never hops). Nothing here runs per frame.

use std::collections::HashMap;

use gpui_kit::component::input::{EditorState, InputEvent};
use gpui_kit::*;

/// The longest text that hops: each change reads the whole text back, so a template stays small.
const MOST_BYTES: usize = 64 * 1024;

/// A template's text with its dates filled in and its markers taken out, and where the markers
/// were (byte offsets into `text`) in the order Tab visits them.
#[derive(Debug, PartialEq)]
pub struct Expanded {
    pub text: String,
    pub stops: Vec<usize>,
}

/// Fills in `{{date}}` with `date` and lifts `{{title}}` and `{{cursor}}` out of `template`. The
/// stops are every title spot in text order, then every cursor spot. A marker is the name between
/// two braces each side, spaces around the name allowed; any other `{{...}}` stays as typed.
pub fn expand(template: &str, date: &str) -> Expanded {
    let mut text = String::with_capacity(template.len());
    let (mut titles, mut cursors) = (Vec::new(), Vec::new());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        let Some(len) = rest[open + 2..].find("}}") else { break };
        let name = rest[open + 2..open + 2 + len].trim();
        let end = open + 2 + len + 2;
        match name {
            "date" => {
                text.push_str(&rest[..open]);
                text.push_str(date);
            }
            "title" => {
                text.push_str(&rest[..open]);
                titles.push(text.len());
            }
            "cursor" => {
                text.push_str(&rest[..open]);
                cursors.push(text.len());
            }
            // Not ours: keep the braces and look again from just after them.
            _ => {
                text.push_str(&rest[..open + 2]);
                rest = &rest[open + 2..];
                continue;
            }
        }
        rest = &rest[end..];
    }
    text.push_str(rest);
    titles.extend(cursors);
    Expanded { text, stops: titles }
}

/// Moves `stops` for the change from `old` to `new`: the changed part is what lies between their
/// common start and common end. A stop after it moves by how much longer the text got; one inside
/// it goes to the end of what replaced it; one before it stays.
pub fn shift(stops: &mut [usize], old: &str, new: &str) {
    let (old, new) = (old.as_bytes(), new.as_bytes());
    let start = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let tail = old[start..].iter().rev().zip(new[start..].iter().rev()).take_while(|(a, b)| a == b).count();
    let (cut_end, put_end) = (old.len() - tail, new.len() - tail);
    for stop in stops {
        if *stop >= cut_end {
            *stop = *stop + put_end - cut_end;
        } else if *stop > start {
            *stop = put_end;
        }
    }
}

/// Today's local date as `2026-11-01`.
pub fn today() -> String {
    crate::drafts::stamp_now().chars().take(10).collect()
}

/// One editor's stops still to visit.
struct Hop {
    stops: Vec<usize>,
    /// The stop Tab goes to next.
    next: usize,
    /// The text as the last change left it, to find what the next change did.
    text: String,
    _changes: Subscription,
    _release: Subscription,
}

#[derive(Default)]
struct Hops(HashMap<EntityId, Hop>);

impl Global for Hops {}

/// Puts the caret on the first stop of `expanded` and, when more follow, starts hopping in
/// `editor`. The editor must hold exactly `expanded.text`.
pub fn begin(editor: &Entity<EditorState>, expanded: Expanded, window: &mut Window, cx: &mut App) {
    let Some(&first) = expanded.stops.first() else { return };
    if editor.read(cx).text().to_string() != expanded.text {
        return;
    }
    let editor_now = editor.clone();
    place(editor, first, cx);
    // A page that hasn't drawn has no layout to scroll by, so the move is asked again two frames on (as `Document::go_to` does).
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |_, cx| {
            if editor_now.read(cx).cursor() == first {
                place(&editor_now, first, cx);
            }
        });
    });
    if expanded.stops.len() < 2 || expanded.text.len() > MOST_BYTES {
        return;
    }
    let id = editor.entity_id();
    let changes = cx.subscribe(editor, move |editor, event: &InputEvent, cx| {
        if matches!(event, InputEvent::Change) {
            changed(&editor, cx);
        }
    });
    let release = cx.observe_release(editor, move |_, cx| {
        if cx.has_global::<Hops>() {
            cx.global_mut::<Hops>().0.remove(&id);
        }
    });
    if !cx.has_global::<Hops>() {
        cx.set_global(Hops::default());
    }
    cx.global_mut::<Hops>().0.insert(id, Hop { stops: expanded.stops, next: 1, text: expanded.text, _changes: changes, _release: release });
}

/// Puts the caret on the stop and marks the line it lands on with the editor's landing wash, so a hop reads as a move.
fn place(editor: &Entity<EditorState>, at: usize, cx: &mut App) {
    editor.update(cx, |state, cx| {
        state.set_selected_range(at..at, cx);
        state.wash_landing(cx);
    });
}

/// The editor's text changed: shift the stops left, or stop hopping when the text grew too long.
fn changed(editor: &Entity<EditorState>, cx: &mut App) {
    let id = editor.entity_id();
    if !cx.has_global::<Hops>() || !cx.global::<Hops>().0.contains_key(&id) {
        return;
    }
    let new = editor.read(cx).text().to_string();
    let hops = &mut cx.global_mut::<Hops>().0;
    if new.len() > MOST_BYTES {
        hops.remove(&id);
        return;
    }
    if let Some(hop) = hops.get_mut(&id) {
        shift(&mut hop.stops, &hop.text, &new);
        hop.text = new;
    }
}

/// Tab in `editor`: goes to the next stop and says so (`true`), or ends the hopping and leaves
/// the Tab to the editor (`false`) when none is left or none was started.
pub fn next(editor: &Entity<EditorState>, cx: &mut App) -> bool {
    if !cx.has_global::<Hops>() {
        return false;
    }
    let id = editor.entity_id();
    let hops = &mut cx.global_mut::<Hops>().0;
    let Some(hop) = hops.get_mut(&id) else { return false };
    let Some(&at) = hop.stops.get(hop.next) else {
        hops.remove(&id);
        return false;
    };
    hop.next += 1;
    place(editor, at, cx);
    true
}

/// Ends the hopping in `editor`; `true` when there was some.
pub fn end(editor: &Entity<EditorState>, cx: &mut App) -> bool {
    cx.has_global::<Hops>() && cx.global_mut::<Hops>().0.remove(&editor.entity_id()).is_some()
}

#[cfg(test)]
mod tests {
    use super::{Expanded, expand, shift};

    #[test]
    fn dates_are_filled_and_markers_lifted() {
        let got = expand("# {{date}}\n\n{{cursor}}", "2026-11-01");
        assert_eq!(got, Expanded { text: "# 2026-11-01\n\n".into(), stops: vec![14] });
    }

    #[test]
    fn titles_come_before_cursors_in_text_order() {
        let got = expand("{{cursor}}a{{title}}b{{title}}c", "d");
        assert_eq!(got.text, "abc");
        assert_eq!(got.stops, vec![1, 2, 0]);
    }

    #[test]
    fn a_text_without_markers_is_unchanged() {
        let text = "plain {{ other }} {{ and {x} }} {{unclosed";
        assert_eq!(expand(text, "d"), Expanded { text: text.into(), stops: vec![] });
    }

    #[test]
    fn spaces_inside_the_braces_are_allowed_and_dates_repeat() {
        let got = expand("{{ date }}/{{date}}{{ title }}", "D");
        assert_eq!(got, Expanded { text: "D/D".into(), stops: vec![3] });
    }

    #[test]
    fn multibyte_text_gives_byte_offsets() {
        assert_eq!(expand("é{{title}}", "d").stops, vec![2]);
    }

    #[test]
    fn typing_at_a_stop_moves_the_later_stops() {
        let mut stops = vec![2, 2, 5];
        shift(&mut stops, "ab\ncd", "abXYZ\ncd");
        assert_eq!(stops, vec![5, 5, 8]);
    }

    #[test]
    fn an_edit_after_a_stop_leaves_it() {
        let mut stops = vec![1, 3];
        shift(&mut stops, "abcd", "abcdef");
        assert_eq!(stops, vec![1, 3]);
    }

    #[test]
    fn deleting_across_a_stop_pulls_it_to_the_edit() {
        let mut stops = vec![3, 6];
        shift(&mut stops, "abcdefg", "abg");
        assert_eq!(stops, vec![2, 2]);
    }
}
