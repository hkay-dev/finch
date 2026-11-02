//! The empty state every pane and list uses: one symbol, a title, a line and text buttons,
//! centered a little above the middle. It fades in over `FADE_QUICK` each time it is added to
//! the tree. It leaves at once where the content that replaces it covers it; where the content is
//! small (the explorer's first file), `leaving` draws it fading out under the new content.

use std::{rc::Rc, time::Duration};

use gpui_kit::*;

use crate::{
    hover::FadeHover as _,
    motion::ease_out,
    symbols, theme,
    tokens::{
        ALPHA_BUTTON_HOVER, ALPHA_BUTTON_PRESSED, ALPHA_HOVER, ALPHA_PRIMARY, ALPHA_SECONDARY, EMPTY_ACTION_GAP, EMPTY_DETAIL_WIDTH,
        EMPTY_LIFT, EMPTY_SYMBOL, FADE_QUICK, FILTER_GAP, FILTER_INSET, FILTER_PAD_X, FIND_FIELD_HEIGHT, RADIUS_BUTTON, TEXT_BODY,
        TEXT_BODY_LINE, TEXT_DETAIL, TEXT_DETAIL_LINE, TEXT_SUBHEADLINE, TEXT_SUBHEADLINE_LINE,
    },
};

/// One text button under the line: its label, the key that does the same, and what a click runs.
pub struct Action {
    pub id: &'static str,
    pub label: &'static str,
    pub key: Option<&'static str>,
    pub run: Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>,
}

/// The state as an element filling its parent. `id` names it across frames, so its fade restarts
/// when it comes back after being gone for a frame.
pub fn view(
    id: impl Into<ElementId>,
    symbol: &'static str,
    title: &'static str,
    line: String,
    actions: Vec<Action>,
    cx: &App,
) -> AnyElement {
    fitted(id, symbol, title, line, actions, false, cx)
}

/// `view` for a parent that may be too short for the whole column: `compact` drops the symbol and
/// the line, so the title and the buttons stay and nothing clips.
pub fn fitted(
    id: impl Into<ElementId>,
    symbol: &'static str,
    title: &'static str,
    line: String,
    actions: Vec<Action>,
    compact: bool,
    cx: &App,
) -> AnyElement {
    fitted_at(id, symbol, title, line, actions, if compact { 0. } else { 1. }, cx)
}

/// `fitted` for a caller that drives the change: `level` (1 whole column, 0 title and buttons only) is the
/// symbol's and the line's drawn level. Between the two they fade and give up their room as they go, so
/// the title and the buttons glide to their compact place and nothing jumps when they leave the layout.
pub fn fitted_at(
    id: impl Into<ElementId>,
    symbol: &'static str,
    title: &'static str,
    line: String,
    actions: Vec<Action>,
    level: f32,
    cx: &App,
) -> AnyElement {
    let id: ElementId = id.into();
    column(symbol, title, line, actions, level, true, cx)
        .with_animation(id, Animation::new(Duration::from_secs_f32(FADE_QUICK)), |state, t| state.opacity(ease_out(t)))
        .into_any_element()
}

/// The state as it was, fading out at `level` (1 to 0) over whatever replaced it: the same column,
/// its buttons drawn but taking no hover and no click. The caller drives `level` on `ease_in` over
/// `FADE_QUICK` and drops it at 0; nothing here animates, so the exit never restarts.
pub fn leaving(symbol: &'static str, title: &'static str, line: String, actions: Vec<Action>, level: f32, cx: &App) -> AnyElement {
    column(symbol, title, line, actions, 1., false, cx).opacity(level).into_any_element()
}

/// The column itself. A `live` one has working buttons; a ghost's are plain fills.
fn column(
    symbol: &'static str,
    title: &'static str,
    line: String,
    actions: Vec<Action>,
    level: f32,
    live: bool,
    cx: &App,
) -> Div {
    let has_buttons = !actions.is_empty();
    let (primary, muted) = (theme::ink(ALPHA_PRIMARY, cx), theme::ink(ALPHA_SECONDARY, cx));
    let wash = theme::ink(1., cx);
    let buttons = actions.into_iter().map(|action| {
        let run = action.run;
        let rest = wash.opacity(ALPHA_HOVER);
        let label = div()
            .flex_none()
            .h(FIND_FIELD_HEIGHT)
            .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
            .flex()
            .items_center()
            .gap(FILTER_GAP)
            .rounded(RADIUS_BUTTON)
            // The row hover wash at rest, so the text reads as a button before the pointer arrives. `fade_hover` paints only
            // from the first hover frame, so the same fill is set here for the frames before it.
            .bg(rest)
            .text_size(TEXT_SUBHEADLINE)
            .line_height(TEXT_SUBHEADLINE_LINE)
            .text_color(primary)
            .child(action.label)
            .children(action.key.map(|key| div().text_color(muted).child(key)));
        if !live {
            return label.into_any_element();
        }
        label
            .id((action.id, 0usize))
            .fade_hover((action.id, 0usize), Some(rest), wash.opacity(ALPHA_BUTTON_HOVER))
            .press_fill(wash.opacity(ALPHA_BUTTON_PRESSED))
            .on_click(move |event, window, cx| run(event, window, cx))
            .into_any_element()
    });
    div()
        .size_full()
        .pb(EMPTY_LIFT * 2.) // design: padding of twice the lift moves the centered column up by the lift
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        // The gaps belong to the symbol's and the line's boxes, so they shrink with them; a whole column is laid out as before.
        .children((level > 0.).then(|| {
            let room = div().flex_none().w_full().flex().flex_col().items_center().overflow_hidden().opacity(level);
            let room = if level < 1. { room.h(px(EMPTY_SYMBOL + f32::from(FILTER_GAP)) * level) } else { room };
            room.child(div().flex_none().pb(FILTER_GAP).child(symbols::icon(symbol, EMPTY_SYMBOL).text_color(muted)))
        }))
        .child(div().text_size(TEXT_BODY).line_height(TEXT_BODY_LINE).font_weight(FontWeight::SEMIBOLD).child(title))
        .children((level > 0.).then(|| {
            let room = div().flex_none().w_full().flex().flex_col().items_center().overflow_hidden().opacity(level);
            let room = if level < 1. { room.h((TEXT_DETAIL_LINE + FILTER_GAP) * level) } else { room };
            room.child(
                div()
                    .max_w(EMPTY_DETAIL_WIDTH)
                    .px(FILTER_INSET) // design: `.px` is the horizontal padding method, the value is a token
                    .pt(FILTER_GAP)
                    .text_center()
                    .text_size(TEXT_DETAIL)
                    .line_height(TEXT_DETAIL_LINE)
                    .text_color(muted)
                    .child(line),
            )
        }))
        // In a pane too narrow for both buttons side by side, the second stacks under the first.
        .children(has_buttons.then(|| {
            div().flex().flex_wrap().justify_center().max_w_full().gap(FILTER_GAP).mt(EMPTY_ACTION_GAP + FILTER_GAP).children(buttons)
        }))
}
