//! The text field capsule the floats share (the find bar's find and replace fields, Go to line) and
//! the focus fade and ring it shares with the explorer's filter: the fill eases from its rest alpha
//! to its focused alpha and a ring closes in around it, both over `FADE_QUICK` on the one ease-out,
//! softened out of key like the filter's. Go to line and Rename also tint it toward the danger colour
//! for a bad entry (`tint`, 0 to 1, which the caller fades). `Reveal` is the float's own arrival.

use std::{
    cell::RefCell,
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    rc::Rc,
    time::Instant,
};

use gpui_kit::component::{
    ActiveTheme as _,
    input::{Input, InputState},
};
use gpui_kit::*;

use crate::{
    hover::FadeHover as _,
    motion::{Fade, Glide, drawn_now, ease_out},
    symbols, theme,
    tokens::{
        ALPHA_BUTTON_PRESSED, ALPHA_SELECTED, ALPHA_SELECTED_LIGHT, ALPHA_SELECTED_UNFOCUSED, ALPHA_SELECTED_UNFOCUSED_LIGHT,
        ALPHA_FILTER, ALPHA_FILTER_FOCUSED, ALPHA_FILTER_FOCUSED_LIGHT, ALPHA_FILTER_LIGHT, ALPHA_RING, ALPHA_SECONDARY, ALPHA_TERTIARY,
        FADE_QUICK, FIELD_LENS_PAD, FIELD_SYMBOL_GAP, FILTER_PAD_X, FILTER_RING, FILTER_RING_FROM, FILTER_SYMBOL, FIND_FIELD_HEIGHT, FIND_PAD, OVERLAY_ENTER, OVERLAY_EXIT, RADIUS_BUTTON, ROW_FIELD_RING, ROW_HEIGHT,
        TEXT_BODY, TEXT_BODY_LINE,
    },
};

/// A capsule `FIND_FIELD_HEIGHT` tall holding `input` after an optional leading symbol. Its fill and
/// focus ring ease on the field's focus (keyed by the input), and `tint` (0 to 1) lays the danger
/// colour over the fill at `ALPHA_TERTIARY`. The caller sets its width and shrink.
pub(crate) fn float_field(input: &Entity<InputState>, symbol: Option<&'static str>, tint: f32, cx: &App) -> Div {
    float_field_named(input, symbol, tint, None, None, cx)
}

/// A field's leading symbol that does something when pressed (the find field's magnifier opens Recent
/// Searches): `key` keeps its fade apart from the same field in another pane, `tip` is its tooltip.
pub(crate) struct Lens {
    pub key: usize,
    pub tip: &'static str,
    pub press: Rc<dyn Fn(&mut Window, &mut App)>,
}

/// [`float_field`] that, with `rects` on, records the box its text sits in as `<name>-text`, so
/// two fields' text starts can be compared.
pub(crate) fn float_field_named(input: &Entity<InputState>, symbol: Option<&'static str>, tint: f32, name: Option<&'static str>, lens: Option<Lens>, cx: &App) -> Div {
    let theme = cx.theme();
    let ink = theme.sidebar_foreground;
    let (ring, danger) = (theme.ring, theme.danger);
    let light = theme::is_light(cx);
    let (rest, lit) = if light { (ALPHA_FILTER_LIGHT, ALPHA_FILTER_FOCUSED_LIGHT) } else { (ALPHA_FILTER, ALPHA_FILTER_FOCUSED) };
    let muted = theme::ink(ALPHA_SECONDARY, cx);
    let handle = input.read(cx).focus_handle(cx);
    let key = input.entity_id();
    // The fill and the ring ease together on `field_focus`, drawn in one pass under the field (`float_focus`).
    let capsule = canvas(
        move |bounds, window, cx| {
            // Out of key the field softens to its rest fill and no ring, like the explorer filter's.
            let focused = crate::control::window_key(window) && handle.is_focused(window);
            (bounds, float_focus(key, focused, window, cx))
        },
        move |_, (bounds, level), window, _| {
            let radius = bounds.size.height / 2.;
            window.paint_quad(gpui::fill(bounds, ink.opacity(mix(rest, lit, level))).corner_radii(Corners::all(radius)));
            if tint > 0. {
                window.paint_quad(gpui::fill(bounds, danger.opacity(ALPHA_TERTIARY * tint)).corner_radii(Corners::all(radius)));
            }
            paint_focus_ring(window, bounds, radius, level, ring);
        },
    )
    .absolute()
    .inset_0();
    div()
        .relative()
        .h(FIND_FIELD_HEIGHT)
        .child(capsule)
        .child(
            div()
                .relative()
                .size_full()
                .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
                .flex()
                .items_center()
                .gap(FIELD_SYMBOL_GAP)
                .text_size(TEXT_BODY)
                .line_height(TEXT_BODY_LINE)
                .children(symbol.map(|glyph| {
                    let icon = symbols::icon(glyph, FILTER_SYMBOL).text_color(muted).flex_none();
                    let Some(lens) = lens else { return icon.into_any_element() };
                    // The arrow, not the I-beam of the text beside it; the press lights the wash at once.
                    let id = ("field-lens", lens.key);
                    let press = lens.press;
                    div()
                        .id(id)
                        .relative()
                        .flex_none()
                        .p(FIELD_LENS_PAD)
                        .m(-FIELD_LENS_PAD)
                        .cursor_default()
                        .rounded(RADIUS_BUTTON)
                        .fade_hover(id, None, ink.opacity(0.))
                        .press_fill(ink.opacity(ALPHA_BUTTON_PRESSED))
                        .on_click(move |_, window, cx| press(window, cx))
                        .tooltip(crate::tooltip::tip(lens.tip))
                        .tooltip_show_delay(crate::tooltip::DELAY)
                        .children(name.and_then(|name| rect_as(name, "magnifier")))
                        .child(icon)
                        .into_any_element()
                }))
                .child(div().relative().flex_1().min_w_0().child(field_input(input, symbol.is_some())).children(name.and_then(text_rect)))
        )
}

/// Records the box of the element it sits in as `<name>-text` when rects are on.
fn text_rect(name: &'static str) -> Option<impl IntoElement> {
    rect_as(name, "text")
}

/// Records the box of the element it sits in as `<name>-<suffix>` when rects are on; the id is built only then.
fn rect_as(name: &'static str, suffix: &'static str) -> Option<impl IntoElement> {
    crate::control::rects_enabled().then(|| {
        let id = format!("{name}-{suffix}");
        canvas(move |bounds, window, _| crate::control::record(window, id.clone(), bounds), |_, (), _, _| {}).absolute().size_full()
    })
}

/// The text input a capsule field holds, its text at the rows' `TEXT_BODY` (the input's own default
/// is a step above, and the placeholder takes the size too), so every field matches the filter's.
/// After a leading symbol its own left padding is dropped, so the gap to the symbol is
/// `FIELD_SYMBOL_GAP` alone, in every field that has one.
pub(crate) fn field_input(input: &Entity<InputState>, after_symbol: bool) -> Input {
    let field = Input::new(input).appearance(false).bordered(false).focus_bordered(false).w_full().text_size(TEXT_BODY).line_height(TEXT_BODY_LINE);
    if after_symbol { field.pl_0() } else { field }
}

pub(crate) fn mix(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// The picked row's wash in a list float (Open quickly, Go to symbol, the draft list): the selected alpha while the window is key,
/// easing to the unfocused alpha out of key like the explorer's, kept in `slot` (the float's own, `None` until first drawn). The
/// float redraws when its window gains or loses key, and asks for frames only while the fade moves.
pub(crate) fn selected_wash<T: 'static>(slot: &mut Option<Fade>, ink: Hsla, light: bool, window: &mut Window, cx: &mut Context<T>) -> Hsla {
    let now = drawn_now(window.window_handle().window_id());
    let to = f32::from(crate::control::window_key(window));
    if slot.is_none() {
        cx.observe_window_activation(window, |_, _, cx| cx.notify()).detach();
    }
    let fade = slot.get_or_insert(Fade::at(to));
    fade.aim(to, now);
    if fade.is_moving(now) {
        window.request_animation_frame();
    }
    let (key, unfocused) = if light { (ALPHA_SELECTED_LIGHT, ALPHA_SELECTED_UNFOCUSED_LIGHT) } else { (ALPHA_SELECTED, ALPHA_SELECTED_UNFOCUSED) };
    ink.opacity(mix(unfocused, key, fade.level(now)))
}

/// The first list a float shows when its index landed after it opened: the rows fade in over `FADE_QUICK` while the list's
/// height grows from nothing on a `SPLIT` spring, so the float eases to its height instead of popping. Made at the first frame
/// the list is drawn, so both start painted. Any later answer (typing, filtering) drops it and lands at once.
pub(crate) struct Arrival {
    /// The list's height in rows, rising from 0 to `rows`.
    rise: Glide,
    fade: Fade,
    rows: f32,
}

impl Arrival {
    pub(crate) fn start(rows: usize, now: Instant) -> Self {
        let rows = rows as f32;
        let mut fade = Fade::with(0., FADE_QUICK);
        fade.aim(1., now);
        Self { rise: Glide::new(0., rows, now), fade, rows }
    }

    /// It has reached its end at `now`, so the list is drawn bare and no more frames are wanted.
    pub(crate) fn is_done(&self, now: Instant) -> bool {
        self.rise.is_done(now) && !self.fade.is_moving(now)
    }

    /// `list` clipped to the height drawn at `now` and faded in. The gap above it grows with the height, so nothing pops at the start.
    pub(crate) fn wrap(&self, list: AnyElement, now: Instant) -> AnyElement {
        let level = self.rise.level(now);
        let (alpha, share) = (self.fade.level(now), if self.rows > 0. { level / self.rows } else { 1. });
        div().h(ROW_HEIGHT * level).mt(FIND_PAD * (share - 1.)).overflow_hidden().opacity(alpha).child(list).into_any_element()
    }
}

/// Open quickly's list when it goes from no rows to rows (its first letter, or its index landing under typed text): the rows
/// fade in over `FADE_QUICK` while the list's height grows from nothing on a `SPLIT` spring, so the float eases to its height
/// instead of popping. Made at the first frame the list is drawn, so both start painted. Typing that only changes the rows
/// lands at once; emptying the field mid-growth aims it back to nothing (`aim`) from where it is drawn, and a letter typed
/// again turns it round again.
pub(crate) struct Growth {
    /// The share of the list's height shown, 0 to 1.
    rise: Glide,
    fade: Fade,
    /// Where `rise` is heading.
    to: f32,
}

impl Growth {
    pub(crate) fn start(now: Instant) -> Self {
        let mut fade = Fade::with(0., FADE_QUICK);
        fade.aim(1., now);
        Self { rise: Glide::new(0., 1., now), fade, to: 1. }
    }

    /// Heads for `to` (1 shown, 0 gone) from where the list is drawn at `now`, carrying its speed; a list already heading there carries on.
    pub(crate) fn aim(&mut self, to: f32, now: Instant) {
        if to == self.to {
            return;
        }
        let speed = self.rise.velocity(now);
        self.rise = Glide::new(self.rise.level(now), to, now).with_speed(speed);
        self.fade.aim(to, now);
        self.to = to;
    }

    /// It has reached its end at `now`, so the list is drawn bare (or dropped) and no more frames are wanted.
    pub(crate) fn is_done(&self, now: Instant) -> bool {
        self.rise.is_done(now) && !self.fade.is_moving(now)
    }

    /// Whether it is heading away from the list.
    pub(crate) fn leaving(&self) -> bool {
        self.to <= 0.
    }

    /// `list`, `rows` tall when whole (a row count mid-glide is fine), clipped to the height drawn at `now` and faded. The gap above it grows with the height, so nothing pops at the start.
    pub(crate) fn wrap(&self, list: AnyElement, rows: f32, now: Instant) -> AnyElement {
        let (share, alpha) = (self.rise.level(now).clamp(0., 1.), self.fade.level(now));
        div().h(ROW_HEIGHT * rows * share).mt(FIND_PAD * (share - 1.)).overflow_hidden().opacity(alpha).child(list).into_any_element()
    }
}

/// The height of a float's list while typing narrows or widens it (10 rows to 1, a line to 10): it eases to the new row count on
/// the `SPLIT` spring and the card grows or shrinks from its top edge, while the rows are laid out at the final height and only
/// the clip moves, so the text and rows land at once. A key mid-glide aims from the height drawn, carrying its speed; it never
/// restarts. Frames only while it moves. A list that comes from nothing (`None`) starts at its height; `Arrival` and `Growth`
/// own that edge.
pub(crate) struct Height {
    /// The glide to `to`; `None` once it rests there.
    glide: Option<Glide>,
    /// The row count the list is heading for.
    to: usize,
}

impl Height {
    pub(crate) fn new(rows: usize) -> Self {
        Self { glide: None, to: rows }
    }

    /// Sets the height to `rows` at once (an arrival or growth is running its own).
    pub(crate) fn snap(&mut self, rows: usize) {
        *self = Self::new(rows);
    }

    /// Heads for `rows` from where the list is drawn at `now`, carrying its speed (a count already heading there carries on);
    /// returns the height drawn, in rows. `is_moving` says whether more frames are wanted.
    pub(crate) fn follow(&mut self, rows: usize, now: Instant) -> f32 {
        if rows != self.to {
            let (from, speed) = self.glide.map_or((self.to as f32, 0.), |glide| (glide.level(now), glide.velocity(now)));
            self.glide = Some(Glide::new(from, rows as f32, now).with_speed(speed));
            self.to = rows;
        }
        if self.glide.is_some_and(|glide| glide.is_done(now)) {
            self.glide = None;
        }
        self.glide.map_or(self.to as f32, |glide| glide.level(now).max(0.))
    }

    /// It is between two row counts, so the list wants frames.
    pub(crate) fn is_moving(&self) -> bool {
        self.glide.is_some()
    }

    /// `list`, laid out whole, clipped to `drawn` rows from its top edge.
    pub(crate) fn clip(list: AnyElement, drawn: f32) -> AnyElement {
        div().h(ROW_HEIGHT * drawn).overflow_hidden().child(list).into_any_element()
    }
}

/// Where a float is between gone (0) and shown (1): its opacity and its rise. Go to line, Rename and
/// the closed-bar find count share it. It arrives on expo-out over `OVERLAY_ENTER` and leaves on
/// ease-in over `OVERLAY_EXIT`; turning it mid-flight goes on from the level drawn, never from 0.
pub(crate) struct Reveal {
    /// The level when it last turned toward `open`.
    from: f32,
    pub(crate) open: bool,
    since: Instant,
}

impl Reveal {
    /// Shut and at rest, as of `now`.
    pub(crate) fn shut(now: Instant) -> Self {
        Self { from: 0., open: false, since: now }
    }

    /// The drawn level at `now` and whether it is still moving.
    pub(crate) fn level(&self, now: Instant) -> (f32, bool) {
        let secs = now.saturating_duration_since(self.since).as_secs_f32();
        if self.open {
            let t = secs / OVERLAY_ENTER;
            if t >= 1. { (1., false) } else { (self.from + (1. - self.from) * crate::motion::expo_out(t), true) }
        } else {
            let t = secs / OVERLAY_EXIT;
            if t >= 1. { (0., false) } else { (self.from * (1. - crate::motion::ease_in(t)), true) }
        }
    }

    /// Turns toward `open` from the level drawn at `now`.
    pub(crate) fn turn(&mut self, open: bool, now: Instant) {
        if self.open != open {
            self.from = self.level(now).0;
            self.open = open;
            self.since = now;
        }
    }
}

/// How a text field's focus is on its way: toward `to` from `from` since `since`.
struct Focus {
    to: bool,
    from: f32,
    since: Instant,
}

impl Focus {
    fn progress(&self, now: Instant) -> f32 {
        (now.saturating_duration_since(self.since).as_secs_f32() / FADE_QUICK).min(1.)
    }

    fn level(&self, now: Instant) -> f32 {
        let to = if self.to { 1. } else { 0. };
        mix(self.from, to, ease_out(self.progress(now)))
    }
}

thread_local! {
    /// Each text field's focus fade, by window and key. An entry goes once its field is unfocused and at rest.
    static FOCUS: RefCell<HashMap<(WindowId, u64), Focus>> = RefCell::new(HashMap::new());
}

/// `field_focus` for a float's field, kept in the element's own keyed state so it goes with the
/// element: a find bar closed mid-fade leaves nothing behind. Same fade, same turn-around, and no
/// frames for a field that is unfocused and at rest.
fn float_focus(key: EntityId, focused: bool, window: &mut Window, cx: &mut App) -> f32 {
    let now = crate::motion::drawn_now(window.window_handle().window_id());
    let state = window.use_keyed_state(("float-field-focus", key.as_u64() as usize), cx, |_, _| None::<Focus>);
    let (level, moving) = state.update(cx, |slot, _| {
        if slot.is_none() && !focused {
            return (0., false);
        }
        let focus = slot.get_or_insert(Focus { to: false, from: 0., since: now });
        if focus.to != focused {
            *focus = Focus { to: focused, from: focus.level(now), since: now };
        }
        let (level, moving) = (focus.level(now), focus.progress(now) < 1.);
        if !moving && !focused {
            *slot = None;
        }
        (level, moving)
    });
    if moving {
        window.request_animation_frame();
    }
    level
}

/// How lit the focus of the field `key` is this frame, 0 to 1: it eases over `FADE_QUICK` with the
/// shared ease-out and turns around from where it is drawn, so a quick focus then blur reverses
/// instead of restarting. A field first seen unfocused rests at 0; one first seen focused fades in.
/// Asks for frames only while it moves. Call it from a prepaint, where the field draws.
pub(crate) fn field_focus(key: impl Hash, focused: bool, window: &mut Window) -> f32 {
    let window_id = window.window_handle().window_id();
    let now = crate::motion::drawn_now(window_id);
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    let key = (window_id, hasher.finish());
    let (level, moving) = FOCUS.with_borrow_mut(|all| {
        // An unfocused field with no entry is at rest at 0: inserting one would run a no-op fade
        // that asks frames, and an entry is dropped again once at rest, so it would repeat (gpui-editor#c1q7).
        if !focused && !all.contains_key(&key) {
            return (0., false);
        }
        let focus = all.entry(key).or_insert(Focus { to: false, from: 0., since: now });
        if focus.to != focused {
            *focus = Focus { to: focused, from: focus.level(now), since: now };
        }
        let (level, moving) = (focus.level(now), focus.progress(now) < 1.);
        if !moving && !focused {
            all.remove(&key);
        }
        (level, moving)
    });
    if moving {
        window.request_animation_frame();
    }
    level
}

/// Drops a closed window's focus fades.
pub fn forget(window: WindowId) {
    FOCUS.with_borrow_mut(|all| all.retain(|(id, _), _| *id != window));
}

/// The focus ring: a band `FILTER_RING` wide outside `field` that closes in from `FILTER_RING_FROM`
/// farther out as `level` rises, and fades in. `radius` is the field's own corner radius. It is
/// painted where it is called, so a caller inside a list draws it in a `deferred` to leave the list's clip.
pub(crate) fn paint_focus_ring(window: &mut Window, field: Bounds<Pixels>, radius: Pixels, level: f32, color: Hsla) {
    if level <= 0. {
        return;
    }
    let out = FILTER_RING + FILTER_RING_FROM * (1. - level);
    window.paint_quad(gpui::quad(
        field.dilate(out),
        Corners::all(radius + out),
        gpui::transparent_black(),
        Edges::all(FILTER_RING),
        color.opacity(ALPHA_RING * level),
        gpui::BorderStyle::Solid,
    ));
}

/// The focus ring of a field that sits in a list row: a band `ROW_FIELD_RING` wide inside `field`'s
/// own edge, fading in with `level`. A row leaves only `FIELD_INSET` around its field, so the
/// outside ring would spill over the row's neighbours.
pub(crate) fn paint_inset_ring(window: &mut Window, field: Bounds<Pixels>, radius: Pixels, level: f32, color: Hsla) {
    if level <= 0. {
        return;
    }
    window.paint_quad(gpui::quad(
        field,
        Corners::all(radius),
        gpui::transparent_black(),
        Edges::all(ROW_FIELD_RING),
        color.opacity(ALPHA_RING * level),
        gpui::BorderStyle::Solid,
    ));
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use gpui_kit::WindowId;

    use super::{FOCUS, Focus, Height, forget};

    #[test]
    fn height_eases_and_turns_round_from_where_it_is_drawn() {
        use std::time::Duration;
        let t0 = Instant::now();
        let mut height = Height::new(10);
        assert_eq!(height.follow(10, t0), 10.);
        assert!(!height.is_moving());
        assert_eq!(height.follow(1, t0), 10.);
        let mid = t0 + Duration::from_millis(80);
        let drawn = height.follow(1, mid);
        assert!(drawn < 10. && drawn > 1., "easing, drawn {drawn}");
        // A key mid-glide aims at 6 from the drawn height, not from 10 or 1.
        let turned = height.follow(6, mid);
        assert!((turned - drawn).abs() < 1e-3, "turned from {drawn} to {turned}");
        assert_eq!(height.follow(6, mid + Duration::from_secs(5)), 6.);
        assert!(!height.is_moving());
    }

    #[test]
    fn forget_drops_only_that_windows_fades() {
        let (gone, kept) = (WindowId::from(1), WindowId::from(2));
        FOCUS.with_borrow_mut(|all| {
            all.clear();
            for window in [gone, kept] {
                all.insert((window, 7), Focus { to: true, from: 0., since: Instant::now() });
            }
        });
        forget(gone);
        FOCUS.with_borrow(|all| {
            assert_eq!(all.len(), 1);
            assert!(all.contains_key(&(kept, 7)));
        });
        forget(kept);
        FOCUS.with_borrow(|all| assert!(all.is_empty()));
    }
}
