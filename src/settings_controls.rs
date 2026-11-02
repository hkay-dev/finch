//! The controls the Settings window's panes are made of, in the look of a SwiftUI Settings form (Harrier's
//! `settings/controls.rs` is the model): a section is a header over a grouped box of rows, each row a label on
//! the left and its control on the right, with a quiet note under the box.
//!
//! A control reads its setting when the pane is built and writes through `write` (`settings::update_later`,
//! then every window redraws and the menu's checks follow), so the observers that already apply a setting
//! everywhere (line numbers, invisibles, code colors, spelling, highlights) do their work as they do for the
//! View menu. Motion lives in a few thread-local springs keyed by window and control (`spring`): a switch's knob
//! glides, a segmented control's pill slides, a dimmed row fades. At rest nothing asks for a frame.
//!
//! Every size, alpha and length is a `tokens::SETTINGS_*` value; `Look` turns the alphas into this frame's
//! colors for the dark or the light theme, once per render.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    ops::Range,
    rc::Rc,
};

use gpui_kit::component::{ActiveTheme as _, input::InputState};
use gpui_kit::*;

use crate::{
    control,
    float_field::{field_focus, field_input, paint_focus_ring},
    hover::{FadeFill, FadeHover as _, Ink, by_hover, icon_button},
    motion::{self, Glide},
    native_menu::{self, Entry},
    settings,
    settings_window::SettingsWindow,
    symbols,
    tab_bar::mix,
    theme, tokens,
    tokens::{SETTINGS_CONTROL_HEIGHT as CONTROL, TEXT_SUBHEADLINE},
};

/// This frame's colors, from the tokens' alphas in the dark or the light theme. The window's render sets them
/// before any pane is built (`set_look`), and every control reads them.
#[derive(Clone, Copy)]
pub struct Look {
    pub light: bool,
    pub text: Hsla,
    pub muted: Hsla,
    pub dim: Hsla,
    pub ground: Hsla,
    pub group: Hsla,
    pub edge: Hsla,
    pub rule: Hsla,
    pub fill: Hsla,
    pub fill_hover: Hsla,
    pub fill_press: Hsla,
    pub track: Hsla,
    pub field: Hsla,
    pub field_edge: Hsla,
    pub knob: Hsla,
    pub accent: Hsla,
    pub danger: Hsla,
}

impl Look {
    pub fn of(window: &Window, cx: &App) -> Self {
        let light = theme::is_light(cx);
        let ink = cx.theme().sidebar_foreground;
        // The wash that lifts a control off the box: white on the dark theme, black on the light.
        let wash = |alpha: f32| hsla(0., 0., if light { 0. } else { 1. }, alpha);
        let by = |dark: f32, bright: f32| wash(if light { bright } else { dark });
        let accent = crate::explorer::icons::accent();
        Self {
            light,
            text: tokens::ink(light, ink, tokens::ALPHA_PRIMARY),
            muted: tokens::ink(light, ink, tokens::ALPHA_SECONDARY),
            dim: tokens::ink(light, ink, tokens::ALPHA_DIM),
            ground: rgb(if light { tokens::SETTINGS_GROUND_LIGHT } else { tokens::SETTINGS_GROUND_DARK }).into(),
            group: hsla(0., 0., 1., if light { tokens::SETTINGS_GROUP_ALPHA_LIGHT } else { tokens::SETTINGS_GROUP_ALPHA }),
            edge: if light { hsla(0., 0., 0., tokens::SETTINGS_EDGE_ALPHA_LIGHT) } else { hsla(0., 0., 1., tokens::SETTINGS_EDGE_ALPHA) },
            rule: wash(tokens::SETTINGS_RULE_ALPHA),
            fill: by(tokens::SETTINGS_FILL_ALPHA, tokens::SETTINGS_FILL_ALPHA_LIGHT),
            fill_hover: by(tokens::SETTINGS_FILL_HOVER_ALPHA, tokens::SETTINGS_FILL_HOVER_ALPHA_LIGHT),
            fill_press: by(tokens::SETTINGS_FILL_PRESS_ALPHA, tokens::SETTINGS_FILL_PRESS_ALPHA_LIGHT),
            track: by(tokens::SETTINGS_TRACK_ALPHA, tokens::SETTINGS_TRACK_ALPHA_LIGHT),
            field: hsla(0., 0., 0., if light { tokens::SETTINGS_FIELD_ALPHA_LIGHT } else { tokens::SETTINGS_FIELD_ALPHA }),
            field_edge: wash(tokens::SETTINGS_FIELD_EDGE_ALPHA),
            knob: hsla(0., 0., tokens::SETTINGS_KNOB_LIGHTNESS, 1.),
            // Out of key the accent keeps its lightness and loses its hue, as AppKit's switch and slider do.
            accent: if control::window_key(window) { accent } else { Hsla { s: 0., ..accent } },
            danger: cx.theme().danger,
        }
    }
}

thread_local! {
    static LOOK: Cell<Option<Look>> = const { Cell::new(None) };
    /// The switch row the pointer holds down now, by its key: its knob widens while held.
    static HELD: Cell<Option<u64>> = const { Cell::new(None) };
    /// The slider whose thumb is being dragged, by its key.
    static DRAG: Cell<Option<u64>> = const { Cell::new(None) };
    /// Each pop-up face's box as last drawn, by its key, so its menu opens under it.
    static FACES: RefCell<HashMap<u64, Bounds<Pixels>>> = RefCell::new(HashMap::new());
    static SPRINGS: RefCell<HashMap<(WindowId, u64), Spring>> = RefCell::new(HashMap::new());
}

pub fn set_look(look: Look) {
    LOOK.set(Some(look));
}

pub fn look() -> Look {
    LOOK.get().expect("the Settings window sets its look before building a pane")
}

/// A control's key: the hash of what names it.
pub fn key_of(name: impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    name.hash(&mut hasher);
    hasher.finish()
}

/// A level moving toward a target on a critically damped spring, from where it is drawn and at the speed it has.
struct Spring {
    to: f32,
    glide: Glide,
    /// The target changed since the control was first drawn: until it lands, frames are wanted.
    live: bool,
}

/// The level of control `key` aimed at `target` in this frame, and whether it still moves (the caller asks for the
/// next frame). A control seen for the first time sits at its target; a new target turns the level around from where it
/// is drawn.
pub fn spring(window: &Window, key: u64, target: f32) -> (f32, bool) {
    let id = window.window_handle().window_id();
    let now = motion::drawn_now(id);
    SPRINGS.with_borrow_mut(|all| {
        let spring = all.entry((id, key)).or_insert_with(|| Spring { to: target, glide: Glide::over(target, target, tokens::SETTINGS_SPRING, now), live: false });
        if spring.to != target {
            let (from, speed) = (spring.glide.level(now), spring.glide.velocity(now));
            *spring = Spring { to: target, glide: Glide::over(from, target, tokens::SETTINGS_SPRING, now).with_speed(speed), live: true };
        }
        (spring.glide.level(now), spring.live && !spring.glide.is_done(now))
    })
}

/// Puts control `key` at `value` with no glide (a thumb under the pointer).
pub fn settle(window: &Window, key: u64, value: f32) {
    let id = window.window_handle().window_id();
    let now = motion::drawn_now(id);
    SPRINGS.with_borrow_mut(|all| {
        all.insert((id, key), Spring { to: value, glide: Glide::over(value, value, tokens::SETTINGS_SPRING, now), live: false });
    });
}

/// Drops a closed window's springs.
pub fn forget(window: WindowId) {
    SPRINGS.with_borrow_mut(|all| all.retain(|(id, _), _| *id != window));
}

/// Changes a setting the way the View menu does: in memory now, in the file after the quiet, then every window
/// redraws (the editors read the font and the styles as they draw) and the menu's checks follow. The observers
/// that already watch the settings apply the rest.
pub fn write(cx: &mut App, change: impl FnOnce(&mut settings::Settings)) {
    settings::update_later(cx, change);
    cx.defer(|cx| {
        cx.refresh_windows();
        crate::menu::refresh(cx);
    });
}

// ---- Search ----

/// The weight of a label's matched letters while a search is typed (the rest of the label dims to `ALPHA_NAME_DIM`).
const MATCH_WEIGHT: FontWeight = FontWeight::SEMIBOLD;

thread_local! {
    /// The words of the search typed in the sidebar's field, lowercased; empty when nothing is typed.
    static TERMS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    /// Which of the search's words each row built since its section was last closed holds, newest last: a section takes the last of its own rows'.
    static TAGS: RefCell<Vec<Vec<bool>>> = const { RefCell::new(Vec::new()) };
    /// How many rows the pane being built keeps.
    static KEPT: Cell<usize> = const { Cell::new(0) };
}

/// Sets the search the next panes are built under: its whitespace-separated words, each of which a row must hold.
pub fn set_query(query: &str) {
    TERMS.with_borrow_mut(|terms| {
        terms.clear();
        terms.extend(query.split_whitespace().map(str::to_lowercase));
    });
}

/// Whether a search is typed.
pub fn filtering() -> bool {
    TERMS.with_borrow(|terms| !terms.is_empty())
}

/// Starts a pane: no row seen, none kept.
pub fn begin_pane() {
    TAGS.with_borrow_mut(Vec::clear);
    KEPT.set(0);
}

/// How many rows the pane built since `begin_pane` keeps under the search (all of them when none is typed).
pub fn kept() -> usize {
    KEPT.get()
}

/// The byte ranges of `text` where `term` (already lowercased) sits, ignoring case.
fn ranges_of(text: &str, term: &str) -> Vec<Range<usize>> {
    let fold = |c: char| c.to_lowercase().next().unwrap_or(c);
    let want: Vec<char> = term.chars().map(fold).collect();
    let chars: Vec<(usize, char)> = text.char_indices().map(|(at, c)| (at, fold(c))).collect();
    let mut found = Vec::new();
    let mut at = 0;
    while !want.is_empty() && at + want.len() <= chars.len() {
        if chars[at..at + want.len()].iter().map(|(_, c)| *c).eq(want.iter().copied()) {
            found.push(chars[at].0..chars.get(at + want.len()).map_or(text.len(), |(next, _)| *next));
            at += want.len();
        } else {
            at += 1;
        }
    }
    found
}

/// The ranges of `label` the search lights: where any of its words sits, overlaps joined, in order.
fn lit_ranges(label: &str) -> Vec<Range<usize>> {
    let mut all: Vec<Range<usize>> = TERMS.with_borrow(|terms| terms.iter().flat_map(|term| ranges_of(label, term)).collect());
    all.sort_by_key(|range| range.start);
    let mut joined: Vec<Range<usize>> = Vec::with_capacity(all.len());
    for range in all {
        match joined.last_mut() {
            Some(last) if range.start <= last.end => last.end = last.end.max(range.end),
            _ => joined.push(range),
        }
    }
    joined
}

/// Notes a row built with `label` and `note`: which of the search's words sit in its label or its note, kept for the section
/// that closes over it (its header can hold the rest).
pub fn tag(label: &str, note: Option<&str>) {
    let hits = TERMS.with_borrow(|terms| terms.iter().map(|term| !ranges_of(label, term).is_empty() || note.is_some_and(|note| !ranges_of(note, term).is_empty())).collect());
    TAGS.with_borrow_mut(|tags| tags.push(hits));
}

/// `rows` less those that miss the search, by the tags the rows left as they were built: a row stays when each word is in its
/// label, its note or its section's `header`.
fn rows_shown(rows: Vec<AnyElement>, header: Option<&str>) -> Vec<AnyElement> {
    let tags: Vec<Vec<bool>> = TAGS.with_borrow_mut(|tags| tags.split_off(tags.len().saturating_sub(rows.len())));
    let under: Vec<bool> = TERMS.with_borrow(|terms| terms.iter().map(|term| header.is_some_and(|header| !ranges_of(header, term).is_empty())).collect());
    let keeps = |at: usize| tags.get(at).is_none_or(|hits| hits.iter().zip(&under).all(|(hit, under)| *hit || *under));
    let kept: Vec<AnyElement> = rows.into_iter().enumerate().filter(|(at, _)| keeps(*at)).map(|(_, row)| row).collect();
    KEPT.set(KEPT.get() + kept.len());
    kept
}

/// A row's label, with the letters the search matches in the full ink and semibold and the rest dimmed.
pub fn lit_label(label: SharedString) -> Div {
    let look = look();
    let marks = lit_ranges(&label);
    let base = div().text_color(look.text);
    if marks.is_empty() {
        return base.child(label);
    }
    let style = HighlightStyle { font_weight: Some(MATCH_WEIGHT), color: Some(look.text), ..Default::default() };
    base.text_color(look.text.opacity(tokens::ALPHA_NAME_DIM)).child(StyledText::new(label).with_highlights(marks.into_iter().map(|range| (range, style))))
}

// ---- Layout: sections and rows ----

/// A header over a grouped box holding `rows`, with an optional footnote under the box.
pub fn section(header: Option<SharedString>, rows: Vec<AnyElement>, footer: Option<SharedString>) -> AnyElement {
    let look = look();
    let rows = rows_shown(rows, header.as_deref());
    // While a search is typed, a section none of whose rows match leaves the page, and the gap with it.
    if filtering() && rows.is_empty() {
        return div().hidden().into_any_element();
    }
    let inset = px(tokens::SETTINGS_GROUP_INSET + 1.);
    let mut ruled = Vec::with_capacity(rows.len() * 2);
    for (at, row) in rows.into_iter().enumerate() {
        if at > 0 {
            ruled.push(theme::hairline(look.rule, false).w_full().h(px(tokens::SETTINGS_RULE_HEIGHT)).flex_none().into_any_element());
        }
        ruled.push(row);
    }
    let boxed = div()
        .flex()
        .flex_col()
        .rounded(px(tokens::SETTINGS_GROUP_RADIUS))
        .bg(look.group)
        .border_1()
        .border_color(look.edge)
        .px(px(tokens::SETTINGS_GROUP_INSET))
        .children(ruled);
    div()
        .flex()
        .flex_col()
        .gap(px(tokens::SETTINGS_HEADER_GAP))
        .children(header.map(|header| {
            div().h(px(tokens::SETTINGS_HEADER_HEIGHT)).px(inset).flex().items_center().font_weight(FontWeight::SEMIBOLD).child(lit_label(header))
        }))
        .child(boxed)
        .children(footer.map(|footer| {
            div().px(inset).pr(px(tokens::SETTINGS_NOTE_TRAIL)).text_size(TEXT_SUBHEADLINE).line_height(px(tokens::SETTINGS_NOTE_LINE)).text_color(look.muted).child(footer)
        }))
        .into_any_element()
}

/// A line's label with the note under it, as the left side of a row.
fn caption(label: SharedString, note: Option<SharedString>) -> Div {
    let look = look();
    div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .child(lit_label(label))
        .children(note.map(|note| div().text_size(TEXT_SUBHEADLINE).line_height(px(tokens::SETTINGS_NOTE_LINE)).text_color(look.muted).child(note)))
}

/// One row: the label (and a note under it) on the left, `control` on the right.
pub fn row(label: impl Into<SharedString>, note: Option<SharedString>, control: impl IntoElement) -> Div {
    let label = label.into();
    tag(&label, note.as_deref());
    div()
        .min_h(px(tokens::SETTINGS_ROW_HEIGHT))
        .py(px(tokens::SETTINGS_ROW_PAD))
        .flex()
        .items_center()
        .justify_between()
        .gap(px(tokens::SETTINGS_ROW_GAP))
        .child(caption(label, note))
        .child(div().flex_none().flex().items_center().child(control))
}

/// How dead a row is, 0 live to 1 dead, easing over the spring as the switch above it flips; frames only while it moves.
pub(crate) fn dead(window: &Window, name: &'static str, off: bool) -> f32 {
    let (level, moving) = spring(window, key_of((name, "dead")), if off { 1. } else { 0. });
    if moving {
        window.request_animation_frame();
    }
    level
}

/// The opacity of a row `dead` (0 to 1) dims to: `ALPHA_DIM` when fully dead.
pub(crate) fn fade(dead: f32) -> f32 {
    1. - (1. - tokens::ALPHA_DIM) * dead
}

// ---- Switch ----

/// A switch of `tokens::SETTINGS_SWITCH_*` sizes: the track's color crossfades and the knob slides on a spring when
/// `on` changes (a switch first drawn already sits at its value), and the knob stretches toward where a flip takes it
/// at the middle of a slide and while its row is held.
fn switch_painted(on: bool, held: u64, name: &'static str) -> Div {
    let (track, knob, inset) = (tokens::SETTINGS_SWITCH_TRACK, tokens::SETTINGS_SWITCH_KNOB, tokens::SETTINGS_SWITCH_INSET);
    let travel = track.0 - knob.0 - 2. * inset;
    let look = look();
    div().w(px(track.0)).h(px(track.1)).flex_none().child(
        canvas(
            |_, _, _| {},
            move |bounds, (), window, _| {
                let (level, sliding) = spring(window, key_of((name, "switch")), if on { 1. } else { 0. });
                let (press, pressing) = spring(window, key_of((name, "held")), if HELD.get() == Some(held) { 1. } else { 0. });
                window.paint_quad(fill(bounds, mix(look.track, look.accent, level)).corner_radii(Corners::all(bounds.size.height / 2.)));
                // The stretch is 0 at both ends of a slide and full halfway, so a knob at rest is its own size.
                let stretch = (tokens::SETTINGS_KNOB_STRETCH * 4. * level * (1. - level)).max(tokens::SETTINGS_KNOB_STRETCH * press);
                let thumb = Bounds::new(
                    point(bounds.left() + px(inset + travel * level - stretch * level), bounds.top() + px((track.1 - knob.1) / 2.)),
                    size(px(knob.0 + stretch), px(knob.1)),
                );
                let shadow = Bounds::new(thumb.origin + point(px(0.), px(tokens::SETTINGS_KNOB_SHADOW_DROP)), thumb.size);
                let corners = Corners::all(px(knob.1 / 2.));
                window.paint_quad(fill(shadow, hsla(0., 0., 0., tokens::SETTINGS_KNOB_SHADOW_ALPHA)).corner_radii(corners));
                window.paint_quad(fill(thumb, look.knob).corner_radii(corners));
                if sliding || pressing {
                    window.request_animation_frame();
                }
            },
        )
        .size_full(),
    )
}

/// A row with a switch on its right; a click anywhere on the row flips it through `flip(new value)`. While `enabled` is
/// false the row is dimmed and takes no clicks.
pub fn switch_row(
    name: &'static str,
    label: &'static str,
    note: Option<&'static str>,
    on: bool,
    enabled: bool,
    window: &Window,
    cx: &mut Context<SettingsWindow>,
    flip: impl Fn(bool, &mut App) + 'static,
) -> AnyElement {
    let held = key_of(name);
    let off = dead(window, name, !enabled);
    let row = row(label, note.map(SharedString::from), switch_painted(on, held, name)).id(name).opacity(fade(off));
    if !enabled {
        return row.into_any_element();
    }
    row.on_mouse_down(MouseButton::Left, cx.listener(move |_, _, _, cx| {
        HELD.set(Some(held));
        cx.notify();
    }))
    .on_mouse_up(MouseButton::Left, cx.listener(move |_, _, _, cx| {
        if HELD.replace(None) == Some(held) {
            cx.notify();
        }
    }))
    .on_mouse_up_out(MouseButton::Left, cx.listener(move |_, _, _, cx| {
        if HELD.replace(None) == Some(held) {
            cx.notify();
        }
    }))
    .on_click(move |_, _, cx| flip(!on, cx))
    .into_any_element()
}

// ---- Pop-up ----

/// Pops a native menu once the frame has drawn (nothing draws while it is open), two frames on as the editor's
/// right-click menu does, at `at` (window points from the top left), and hands the chosen row to `then`.
fn pop_up<T: Clone + 'static>(entries: Vec<Entry<T>>, at: Option<(f32, f32)>, window: &mut Window, then: impl FnOnce(T, &mut App) + 'static) {
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |window, cx| {
            window
                .spawn(cx, async move |cx| {
                    let Some(choice) = native_menu::pop_up(&entries, at) else { return };
                    cx.update(|_, cx| then(choice, cx)).ok();
                })
                .detach();
        });
    });
}

/// A pop-up's face: the chosen title (in the muted ink when `placeholder`, a prompt rather than a choice) and a round chip with up and down chevrons, which brightens while the pointer is over
/// the row and drops back while it is pressed. Records its box, so the menu opens under it.
fn popup_face(name: &'static str, title: SharedString, placeholder: bool) -> Div {
    let look = look();
    let key = key_of(name);
    div()
        .flex()
        .items_center()
        .gap(px(tokens::SETTINGS_POPUP_GAP))
        .child(div().text_color(if placeholder { look.muted } else { look.text }).child(title))
        .child(by_hover(("settings-popup", name), move |level| {
            div()
                .size(px(tokens::SETTINGS_POPUP_CHIP))
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(mix(look.fill, look.fill_hover, level))
                .text_color(look.text)
                .child(symbols::icon_weighted("chevron.up.chevron.down", tokens::SETTINGS_CHEVRON, symbols::SEMIBOLD))
                .child(canvas(move |bounds, _, _| FACES.with_borrow_mut(|all| all.insert(key, bounds)), |_, _, _, _| {}).absolute().size_full())
        }))
}

/// A row with a pop-up on its right whose whole row opens the native menu `entries` builds when it is clicked; the chosen item goes to `pick`. A `placeholder` title is drawn muted.
pub fn popup_row<T: Clone + 'static>(
    name: &'static str,
    label: &'static str,
    note: Option<&'static str>,
    title: impl Into<SharedString>,
    placeholder: bool,
    entries: impl Fn() -> Vec<Entry<T>> + 'static,
    cx: &mut Context<SettingsWindow>,
    pick: impl Fn(T, &mut App) + 'static,
) -> AnyElement {
    let key = key_of(name);
    let pick = Rc::new(pick);
    let row = row(label, note.map(SharedString::from), popup_face(name, title.into(), placeholder));
    div()
        .id(name)
        .fade_hover(("settings-popup", name), None, hsla(0., 0., 0., 0.))
        .child(row)
        .on_click(cx.listener(move |_, _, window, _| {
            // Under the face, its left edge on the face's.
            let at = FACES.with_borrow(|all| all.get(&key).map(|face| (f32::from(face.left()), f32::from(face.bottom()) + tokens::SETTINGS_SEGMENT_PAD)));
            let pick = pick.clone();
            pop_up(entries(), at, window, move |chosen, cx| pick(chosen, cx));
        }))
        .into_any_element()
}

// ---- Segmented control ----

/// `titles` in a fill, the chosen one under a pill that glides to a new choice. A click on segment `at` calls `pick`.
pub fn segmented(name: &'static str, label: &'static str, note: Option<&'static str>, titles: &[&'static str], chosen: usize, pick: impl Fn(usize, &mut App) + 'static) -> AnyElement {
    let look = look();
    let pick = Rc::new(pick);
    let spans: Rc<RefCell<Vec<Bounds<Pixels>>>> = Rc::new(RefCell::new(vec![Bounds::default(); titles.len()]));
    let radius = tokens::RADIUS_FIELD;
    let pill = {
        let spans = spans.clone();
        canvas(
            |_, _, _| {},
            move |group, (), window, _| {
                let Some(span) = spans.borrow().get(chosen).copied().filter(|span| span.size.width > px(0.)) else { return };
                let (x, sliding) = spring(window, key_of((name, "x")), f32::from(span.left() - group.left()));
                let (width, growing) = spring(window, key_of((name, "w")), f32::from(span.size.width));
                let pill = Bounds::new(point(group.left() + px(x), span.top()), size(px(width), span.size.height));
                let shadow = Bounds::new(pill.origin + point(px(0.), px(tokens::SETTINGS_KNOB_SHADOW_DROP)), pill.size);
                window.paint_quad(fill(shadow, hsla(0., 0., 0., tokens::SETTINGS_KNOB_SHADOW_ALPHA)).corner_radii(Corners::all(radius)));
                // The chosen pill is the knob's light capsule on the dark theme and white on the light.
                window.paint_quad(fill(pill, if look.light { hsla(0., 0., 1., 1.) } else { hsla(0., 0., 1., 0.28) }).corner_radii(Corners::all(radius)));
                if sliding || growing {
                    window.request_animation_frame();
                }
            },
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    };
    let mut group = div()
        .relative()
        .flex()
        .h(px(CONTROL))
        .rounded(radius + px(tokens::SETTINGS_SEGMENT_PAD))
        .bg(look.fill)
        .p(px(tokens::SETTINGS_SEGMENT_PAD))
        .gap(px(tokens::SETTINGS_SEGMENT_PAD))
        .child(pill);
    for (at, title) in titles.iter().enumerate() {
        let measure = {
            let spans = spans.clone();
            canvas(move |bounds, _, _| spans.borrow_mut()[at] = bounds, |_, (), _, _| {}).absolute().top_0().left_0().size_full()
        };
        let pick = pick.clone();
        let segment = div()
            .id((name, at))
            .relative()
            .h_full()
            .flex()
            .items_center()
            .px(px(tokens::SETTINGS_BUTTON_PAD))
            .rounded(radius)
            .text_color(look.text)
            .child(measure)
            .child(*title)
            .on_click(move |_, _, cx| pick(at, cx));
        // A segment that isn't chosen fades a quiet fill in under the pointer and takes the stronger one while pressed.
        group = group.child(if at != chosen { segment.fade_hover((name, at, "hover"), None, look.fill_hover).press_fill(look.fill_press).into_any_element() } else { segment.into_any_element() });
    }
    row(label, note.map(SharedString::from), group).into_any_element()
}

// ---- Stepper ----

/// A number with an up and a down half beside it (NSStepper): `shown` is the value as words, `up` and `down` the values the halves
/// go to (`None`: at the end of the range, the half dims and goes dead), `set` takes the new value. While `enabled` is false the row is dimmed.
pub fn stepper_row<V: Copy + 'static>(
    name: &'static str,
    label: &'static str,
    note: Option<&'static str>,
    shown: String,
    (up, down): (Option<V>, Option<V>),
    enabled: bool,
    window: &Window,
    set: impl Fn(V, &mut App) + 'static,
) -> AnyElement {
    let look = look();
    let off = dead(window, name, !enabled);
    let set = Rc::new(set);
    let half = |part: usize, symbol: &'static str, target: Option<V>| {
        let live = enabled && target.is_some();
        let half = div()
            .id((name, part))
            .w(px(tokens::SETTINGS_STEPPER_WIDTH))
            .h(px((CONTROL - tokens::SETTINGS_RULE_HEIGHT) / 2.))
            .flex()
            .items_center()
            .justify_center()
            .text_color(if live { look.text } else { look.dim })
            .child(symbols::icon_weighted(symbol, tokens::SETTINGS_CHEVRON, symbols::SEMIBOLD));
        match target {
            Some(target) if live => {
                let set = set.clone();
                half.fade_hover((name, part, "hover"), None, look.fill_hover).press_fill(look.fill_press).on_click(move |_, _, cx| set(target, cx)).into_any_element()
            }
            _ => half.into_any_element(),
        }
    };
    let control = div()
        .flex()
        .items_center()
        .gap(px(tokens::SETTINGS_POPUP_GAP))
        .child(div().text_color(look.text).font_features(tokens::tabular()).child(shown))
        .child(
            div()
                .flex()
                .flex_col()
                .overflow_hidden()
                .rounded(tokens::RADIUS_ROW)
                .bg(look.fill)
                .child(half(0, "chevron.up", up))
                .child(theme::hairline(look.rule, false).w_full().h(px(tokens::SETTINGS_RULE_HEIGHT)))
                .child(half(1, "chevron.down", down)),
        );
    row(label, note.map(SharedString::from), control).opacity(fade(off)).into_any_element()
}

// ---- Slider ----

/// What a slider row shows and does.
pub struct Slider {
    pub name: &'static str,
    pub label: &'static str,
    pub value: f64,
    pub range: (f64, f64),
    pub step: f64,
    pub shown: String,
    pub set: Rc<dyn Fn(f64, &mut App)>,
}

/// A slider row: the label, a rail with a thumb that follows the pointer while it is held (a click glides it to the pointer),
/// and the value in tabular digits.
pub fn slider_row(slider: Slider, window: &Window) -> AnyElement {
    let look = look();
    let Slider { name, label, value, range: (min, max), step, shown, set } = slider;
    let key = key_of(name);
    let target = ((value - min) / (max - min)).clamp(0., 1.) as f32;
    let dragging = DRAG.get() == Some(key);
    // A thumb under the pointer is at the pointer; a click elsewhere or a change made by hand glides there.
    let (fraction, gliding) = if dragging {
        settle(window, key_of((name, "at")), target);
        (target, false)
    } else {
        spring(window, key_of((name, "at")), target)
    };
    let (press, pressing) = spring(window, key_of((name, "press")), if dragging { 1. } else { 0. });
    if gliding || pressing {
        window.request_animation_frame();
    }
    let (knob_w, knob_h) = tokens::SETTINGS_SLIDER_KNOB;
    let track = canvas(
        |_, _, _| {},
        move |bounds, (), window, _| {
            let rail_h = px(tokens::SETTINGS_SLIDER_RAIL);
            let rail = Bounds::new(point(bounds.left(), bounds.center().y - rail_h / 2.), size(bounds.size.width, rail_h));
            let corners = Corners::all(rail_h / 2.);
            window.paint_quad(fill(rail, look.track).corner_radii(corners));
            let room = f32::from(bounds.size.width) - knob_w;
            let grown = tokens::SETTINGS_KNOB_STRETCH * press;
            let thumb = Bounds::new(
                point(bounds.left() + px(room * fraction - grown / 2.), bounds.center().y - px(knob_h / 2.)),
                size(px(knob_w + grown), px(knob_h)),
            );
            let lit = Bounds::new(rail.origin, size(px(room * fraction + knob_w / 2.), rail_h));
            window.paint_quad(fill(lit, look.accent).corner_radii(corners));
            let round = Corners::all(px(knob_h / 2.));
            window.paint_quad(fill(Bounds::new(thumb.origin + point(px(0.), px(tokens::SETTINGS_KNOB_SHADOW_DROP)), thumb.size), hsla(0., 0., 0., tokens::SETTINGS_KNOB_SHADOW_ALPHA)).corner_radii(round));
            window.paint_quad(fill(thumb, look.knob).corner_radii(round));
            // The pointer holds the thumb from the press until it lets go, wherever it travels.
            let view = window.current_view();
            let value_at = {
                let set = set.clone();
                move |x: Pixels, cx: &mut App| {
                    let across = ((x - bounds.left() - px(knob_w / 2.)) / px(room.max(1.))).clamp(0., 1.);
                    let raw = min + f64::from(across) * (max - min);
                    let snapped = (min + ((raw - min) / step).round() * step).clamp(min, max);
                    set((snapped * 1000.).round() / 1000., cx);
                }
            };
            let aim = value_at.clone();
            window.on_mouse_event(move |event: &MouseDownEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left && bounds.contains(&event.position) {
                    DRAG.set(Some(key));
                    aim(event.position.x, cx);
                    cx.notify(view);
                }
            });
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && DRAG.get() == Some(key) && event.pressed_button == Some(MouseButton::Left) {
                    value_at(event.position.x, cx);
                    cx.notify(view);
                }
            });
            window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left && DRAG.get() == Some(key) {
                    DRAG.set(None);
                    cx.notify(view);
                }
            });
        },
    )
    .h(px(tokens::SETTINGS_SLIDER_HEIGHT))
    .flex_1();
    tag(label, None);
    div()
        .min_h(px(tokens::SETTINGS_ROW_HEIGHT))
        .flex()
        .items_center()
        .gap(px(tokens::SETTINGS_ROW_GAP))
        .child(lit_label(label.into()).w(px(tokens::SETTINGS_SLIDER_LABEL)).flex_none())
        .child(track)
        .child(div().w(px(tokens::SETTINGS_SLIDER_READOUT)).flex_none().flex().justify_end().font_features(tokens::tabular()).text_color(look.muted).child(shown))
        .into_any_element()
}

// ---- Buttons, fields and small parts ----

/// The push button: `label` on the control fill, which fades up under the pointer and on while pressed.
pub fn button(id: impl Into<ElementId>, label: impl Into<SharedString>) -> FadeFill {
    let look = look();
    let id: ElementId = id.into();
    div()
        .id(id.clone())
        .h(px(CONTROL))
        .px(px(tokens::SETTINGS_BUTTON_PAD))
        .flex()
        .flex_none()
        .items_center()
        .rounded(tokens::RADIUS_FIELD)
        .bg(look.fill)
        .text_color(look.text)
        .fade_hover(id, Some(look.fill), look.fill_hover)
        .press_fill(look.fill_press)
        .child(label.into())
}

/// A single-line text field on a bezel: sunk into the box, an edge, and the accent ring closing in while it has the keys. `bad` lays the
/// danger color over it (text that isn't a color or a shortcut yet). `hint` sits at the field's trailing edge in the muted ink, out of the
/// pointer's way (a shortcut's glyphs beside the words typed).
pub fn field(input: &Entity<InputState>, width: f32, bad: bool, hint: Option<String>, cx: &App) -> AnyElement {
    let look = look();
    let handle = input.read(cx).focus_handle(cx);
    let key = input.entity_id();
    let radius = tokens::RADIUS_FIELD;
    let bezel = canvas(
        move |bounds, window, _| {
            let focused = control::window_key(window) && handle.is_focused(window);
            (bounds, field_focus(("settings-field", key), focused, window))
        },
        move |_, (bounds, level), window, _| {
            window.paint_quad(gpui::quad(bounds, Corners::all(radius), look.field, Edges::all(px(1.)), look.field_edge, BorderStyle::Solid));
            if bad {
                window.paint_quad(fill(bounds, look.danger.opacity(tokens::SETTINGS_FIELD_BAD_ALPHA)).corner_radii(Corners::all(radius)));
            }
            paint_focus_ring(window, bounds, radius, level, look.accent);
        },
    )
    .absolute()
    .inset_0();
    div()
        .relative()
        .w(px(width))
        .h(px(CONTROL))
        .flex_none()
        .child(bezel)
        .child(div().relative().size_full().px(px(tokens::SETTINGS_FIELD_PAD)).flex().items_center().text_color(look.text).child(field_input(input, false)))
        .children(hint.map(|hint| {
            div().absolute().inset_0().px(px(tokens::SETTINGS_FIELD_PAD)).flex().items_center().justify_end().text_size(TEXT_SUBHEADLINE).text_color(look.muted).child(hint)
        }))
        .into_any_element()
}

/// A color's swatch: a rounded square of `color` under a hairline edge.
pub fn swatch(color: Hsla) -> Div {
    let look = look();
    div().size(px(tokens::SETTINGS_SWATCH)).flex_none().rounded(px(tokens::SETTINGS_SWATCH_RADIUS)).bg(color).border_1().border_color(look.edge)
}

/// A borderless round button with `symbol`, for a row's remove: 20 points, the least a button takes.
pub fn glyph_button(id: impl Into<ElementId>, symbol: &'static str) -> FadeFill {
    let look = look();
    icon_button(id, symbol, px(tokens::SETTINGS_TILE), tokens::SETTINGS_TILE_SYMBOL, Ink { rest: look.muted, lit: look.text, wash: look.text })
}

#[cfg(test)]
mod tests {
    use super::{lit_ranges, ranges_of, set_query};

    #[test]
    fn terms_light_every_place_they_sit_ignoring_case() {
        assert_eq!(ranges_of("Show hidden files", "hid"), vec![5..8]);
        assert_eq!(ranges_of("Font and FONT", "font"), vec![0..4, 9..13]);
        assert!(ranges_of("Size", "").is_empty());
        set_query("fi hid");
        assert_eq!(lit_ranges("Show hidden files"), vec![5..8, 12..14]);
        set_query("");
    }
}
