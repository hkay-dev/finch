//! The find bar, drawn by the app over the editor's text. GPUI Kit's search panel keeps the engine
//! (its matcher, the query and replace fields, smart case, Enter and Escape); this is only its view,
//! which the editor asks for through `FindBarRenderer`.
//!
//! It is a floating plane like the tooltip (`tooltip::float`): a capsule in the float fill with its
//! rim, a lit top edge in the dark, a soft shadow. The field is the explorer filter's capsule, the controls
//! are the app's icon buttons with SF symbols, and each has a tooltip. It sits in the editor's top
//! right under the tab band and is drawn over the text, so opening it never moves a line. It
//! rises into place over `OVERLAY_ENTER` on the expo-out curve and fades out over `OVERLAY_EXIT` on
//! the ease-in; closing and reopening mid-flight turns around from where the bar is drawn.

use std::{rc::Rc, time::{Duration, Instant}};

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::component::input::{FindBar, FindBarRenderer, FindControl, InputState, recent_find_queries};
use gpui_kit::*;

use crate::{
    float_field::{Lens, float_field_named},
    hover::{FadeHover as _, Ink, icon_button},
    native_menu::{self, Entry, Item},
    theme, tooltip,
    tokens::{
        ALPHA_BUTTON_HOVER, ALPHA_BUTTON_OFF, ALPHA_BUTTON_PRESSED, FADE_QUICK,
        ALPHA_OPTION_ON, ALPHA_OPTION_REST, ALPHA_PRIMARY, ALPHA_SECONDARY, FILTER_PAD_X,
        FIND_FIELD_HEIGHT, FIND_FIELD_WIDTH, FIND_HEIGHT, FIND_INSET_RIGHT, FIND_INSET_TOP, FIND_PAD, FIND_SYMBOL,
        ICON_BUTTON, HOVER_IN, HOVER_OUT, OVERLAY_ENTER, OVERLAY_ENTER_RISE, OVERLAY_EXIT,
        RADIUS_BUTTON, RADIUS_FLOAT_CAPSULE, TEXT_SUBHEADLINE,
        TEXT_SUBHEADLINE_LINE, tabular,
    },
};

/// Hands the editor this bar to draw its search with, once.
pub fn ensure(cx: &mut App) {
    if !cx.has_global::<FindBarRenderer>() {
        cx.set_global(FindBarRenderer { render, exit: Duration::from_secs_f32(OVERLAY_EXIT) });
    }
}

/// Where the bar is between gone (0) and shown (1), which sets its opacity and its rise.
struct Reveal {
    /// The level when the bar last turned toward `open`.
    from: f32,
    open: bool,
    since: Instant,
}

impl Reveal {
    /// The drawn level at `now`: expo-out over `OVERLAY_ENTER` toward 1, ease-in over `OVERLAY_EXIT` toward 0.
    fn level(&self, now: Instant) -> (f32, bool) {
        let secs = now.saturating_duration_since(self.since).as_secs_f32();
        if self.open {
            let t = secs / OVERLAY_ENTER;
            if t >= 1. { (1., false) } else { (self.from + (1. - self.from) * crate::motion::expo_out(t), true) }
        } else {
            let t = secs / OVERLAY_EXIT;
            if t >= 1. { (0., false) } else { (self.from * (1. - crate::motion::ease_in(t)), true) }
        }
    }
}

/// The bar's level now. It turns toward the new direction from the level it was drawn at, and
/// asks for frames only while it moves. The state lives while the bar is drawn, so a bar that
/// opens fresh starts at 0.
fn reveal(key: usize, leaving: bool, window: &mut Window, cx: &mut App) -> f32 {
    let now = crate::motion::drawn_now(window.window_handle().window_id());
    let state = window.use_keyed_state(("find-reveal", key), cx, |_, _| Reveal { from: if leaving { 1. } else { 0. }, open: !leaving, since: now });
    let turned = state.update(cx, |reveal, _| {
        if reveal.open == leaving {
            reveal.from = reveal.level(now).0;
            reveal.open = !leaving;
            reveal.since = now;
        }
        reveal.level(now)
    });
    if turned.1 {
        window.request_animation_frame();
    }
    turned.0
}

/// How far the no-match state shows, 0 (matches) to 1 (none): the dimmed controls and the count's
/// colour. It eases over `FADE_QUICK` only when the state flips, turning around from where it is
/// drawn, and asks for frames only while it moves, so typing a longer no-match query asks for none.
fn fade_none(key: usize, none: bool, window: &mut Window, cx: &mut App) -> f32 {
    let now = crate::motion::drawn_now(window.window_handle().window_id());
    let to = if none { 1. } else { 0. };
    let state = window.use_keyed_state(("find-none", key), cx, |_, _| crate::motion::Fade::with(to, FADE_QUICK));
    let (level, moving) = state.update(cx, |fade, _| {
        fade.aim(to, now);
        (fade.level(now), fade.is_moving(now))
    });
    if moving {
        window.request_animation_frame();
    }
    level
}

/// The wrap pulse on a chevron: the ink rises toward the primary over `HOVER_IN` and falls back over
/// `HOVER_OUT`, both on the one ease-out. A wrap that comes while it runs joins it: it never restarts
/// or stacks. It asks for frames only while it runs.
struct Pulse {
    seen: u32,
    since: Option<Instant>,
    forward: bool,
}

impl Pulse {
    fn level(&self, now: Instant) -> Option<f32> {
        let secs = now.saturating_duration_since(self.since?).as_secs_f32();
        if secs < HOVER_IN {
            Some(crate::motion::ease_out(secs / HOVER_IN))
        } else if secs < HOVER_IN + HOVER_OUT {
            Some(1. - crate::motion::ease_out((secs - HOVER_IN) / HOVER_OUT))
        } else {
            None
        }
    }
}

/// The pulse's levels now, `(previous, next)`, 0 at rest. `wraps` is the panel's count of wraps: a
/// change starts a pulse at the first frame that draws it, on the chevron the last wrap left from,
/// and a bar drawn fresh starts at the count it finds.
fn pulse(key: usize, wraps: u32, forward: bool, window: &mut Window, cx: &mut App) -> (f32, f32) {
    let now = crate::motion::drawn_now(window.window_handle().window_id());
    let state = window.use_keyed_state(("find-pulse", key), cx, |_, _| Pulse { seen: wraps, forward, since: None });
    let (level, forward) = state.update(cx, |pulse, _| {
        let running = pulse.level(now).is_some();
        if pulse.seen != wraps {
            pulse.seen = wraps;
            if !running {
                pulse.since = Some(now);
                pulse.forward = forward;
            }
        }
        let level = pulse.level(now);
        if level.is_none() {
            pulse.since = None;
        }
        (level, pulse.forward)
    });
    if level.is_some() {
        window.request_animation_frame();
    }
    let level = level.unwrap_or(0.);
    if forward { (0., level) } else { (level, 0.) }
}

/// The miss pulse on the count: a Find next or previous that finds nothing lifts the count's ink to
/// the full ink over `HOVER_IN` and back over `HOVER_OUT`, on the one ease-out. A miss that comes while
/// it runs turns it around: it rises again from the level drawn. It asks for frames only while it runs.
struct Miss {
    seen: u32,
    from: f32,
    since: Option<Instant>,
}

impl Miss {
    fn level(&self, now: Instant) -> Option<f32> {
        let secs = now.saturating_duration_since(self.since?).as_secs_f32();
        if secs < HOVER_IN {
            Some(self.from + (1. - self.from) * crate::motion::ease_out(secs / HOVER_IN))
        } else if secs < HOVER_IN + HOVER_OUT {
            Some(1. - crate::motion::ease_out((secs - HOVER_IN) / HOVER_OUT))
        } else {
            None
        }
    }
}

/// The miss pulse's level now, 0 at rest. `misses` is the panel's count of misses: a change starts
/// the pulse at the first frame that draws it, and a bar drawn fresh starts at the count it finds.
fn miss_pulse(key: usize, misses: u32, window: &mut Window, cx: &mut App) -> f32 {
    let now = crate::motion::drawn_now(window.window_handle().window_id());
    let state = window.use_keyed_state(("find-miss", key), cx, |_, _| Miss { seen: misses, from: 0., since: None });
    let level = state.update(cx, |miss, _| {
        if miss.seen != misses {
            miss.seen = misses;
            miss.from = miss.level(now).unwrap_or(0.);
            miss.since = Some(now);
        }
        let level = miss.level(now);
        if level.is_none() {
            miss.since = None;
        }
        level
    });
    if level.is_some() {
        window.request_animation_frame();
    }
    level.unwrap_or(0.)
}

/// What the count says: the kind picks its ink and decides when the count crossfades.
#[derive(Clone, Copy, PartialEq)]
enum CountKind {
    Empty,
    Matches,
    None,
    Replaced,
}

/// The count's two layers: the string now shown and, while the kind just flipped, the string it
/// replaced, fading out over `FADE_QUICK` as the new one fades in.
struct Count {
    kind: CountKind,
    text: SharedString,
    fade_in: crate::motion::Fade,
    out: Option<(CountKind, SharedString, crate::motion::Fade)>,
}

/// The count's layers drawn now: the shown string with its level, and the outgoing one with its
/// level while it fades. A flip mid-fade sends the string shown from the level it is drawn at, so
/// nothing pops; a number changing inside one kind swaps at once. Frames only while it runs.
fn count_layers(
    key: usize,
    kind: CountKind,
    text: &SharedString,
    window: &mut Window,
    cx: &mut App,
) -> (f32, Option<(CountKind, SharedString, f32)>) {
    let now = crate::motion::drawn_now(window.window_handle().window_id());
    let state = window.use_keyed_state(("find-count", key), cx, |_, _| Count {
        kind,
        text: text.clone(),
        fade_in: crate::motion::Fade::with(1., FADE_QUICK),
        out: None,
    });
    let (level, out, moving) = state.update(cx, |count, _| {
        if count.kind != kind {
            // Flipping back to the kind still fading out turns that fade around from where it is drawn.
            let back = match count.out.take() {
                Some((was, _, mut gone)) if was == kind => {
                    gone.aim(1., now);
                    Some(gone)
                }
                _ => None,
            };
            let mut gone = crate::motion::Fade::with(count.fade_in.level(now), FADE_QUICK);
            gone.aim(0., now);
            count.out = Some((count.kind, std::mem::take(&mut count.text), gone));
            count.kind = kind;
            count.fade_in = back.unwrap_or_else(|| {
                let mut fade = crate::motion::Fade::with(0., FADE_QUICK);
                fade.aim(1., now);
                fade
            });
        }
        count.text = text.clone();
        if count.out.as_ref().is_some_and(|(_, _, gone)| !gone.is_moving(now)) {
            count.out = None;
        }
        let out = count.out.as_ref().map(|(kind, text, gone)| (*kind, text.clone(), gone.level(now)));
        (count.fade_in.level(now), out, count.fade_in.is_moving(now) || count.out.is_some())
    });
    if moving {
        window.request_animation_frame();
    }
    (level, out)
}

/// `n` with a comma between thousands ("12,345"), as the count reads in a big file.
pub(crate) fn grouped(n: usize) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (at, digit) in digits.chars().enumerate() {
        if at > 0 && (digits.len() - at) % 3 == 0 {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

thread_local! {
    /// The count box's width and the font family it was shaped in.
    static COUNT_WIDTH: std::cell::RefCell<Option<(SharedString, Pixels)>> = const { std::cell::RefCell::new(None) };
}

/// The count box's width: the widest fixed text ("No results", "Bad pattern", "No selection", a 4-digit "8,888 of 8,888") shaped as the count is drawn, and its padding. Shaped once, and again only when the font family changes.
fn count_width(family: &SharedString, window: &Window) -> Pixels {
    if let Some(width) = COUNT_WIDTH.with_borrow(|seen| seen.as_ref().filter(|(seen, _)| seen == family).map(|(_, width)| *width)) {
        return width;
    }
    let mut style = window.text_style();
    style.font_family = family.clone();
    style.font_features = tabular();
    let width = ["No results", "Bad pattern", "No selection", "8,888 of 8,888"]
        .into_iter()
        .map(|text| window.text_system().layout_line(text, TEXT_SUBHEADLINE, &[style.to_run(text.len())], None).width)
        .fold(px(0.), Pixels::max)
        + FIND_PAD * 2.;
    COUNT_WIDTH.with_borrow_mut(|seen| *seen = Some((family.clone(), width)));
    width
}

/// Which of the five option buttons (Match case, Whole word, Regular expression, Find in selection,
/// Ignore diacritics, in that order) a bar `avail` wide shows, with its field at its least. A narrow bar
/// drops them from the right, so the end's Replace and Close stay inside it; an option that is on
/// (`on`, in the same order) never drops, so the next one that is off from the right goes instead, and
/// when only on options remain they all stay. At least one always stays. The options a bar drops stay in
/// Edit > Find.
fn options_that_fit(avail: Pixels, count: Pixels, can_replace: bool, on: [bool; 5]) -> [bool; 5] {
    // The field and its spacer, the count, previous, next, close and maybe replace, and the option buttons, with the row's gap between each.
    let fixed = |options: usize| {
        let items = 3 + options + 3 + usize::from(can_replace);
        let buttons = options + 3 + usize::from(can_replace);
        FIND_PAD * 2. + FIND_FIELD_HEIGHT * 3. + FIND_PAD / 2. + count + ICON_BUTTON * buttons as f32 + FIND_PAD / 2. * (items - 1) as f32
    };
    let mut shown = [true; 5];
    let mut options = 5;
    while options > 1 && fixed(options) > avail {
        let Some(drop) = (0..5).rev().find(|&at| shown[at] && !on[at]) else { break };
        shown[drop] = false;
        options -= 1;
    }
    shown
}

/// What the bar's margins and rim take from the editor's width besides `FIND_INSET_RIGHT`, as measured
/// at the 480 pt window (editor 359 wide, bar 337).
const BAR_EDGES: Pixels = px(2.);

/// The longest a search runs in the Recent Searches menu, in characters; the rest is cut to an ellipsis.
const RECENT_TITLE_MAX: usize = 40;

/// The magnifier's menu: "Recent Searches" as a header, the finds Up in the field walks (newest first), then
/// Clear Recent Searches, grey with nothing to clear. What a row hands back is the control the panel runs.
fn recent_entries(recent: &[String]) -> Vec<Entry<FindControl>> {
    let title = |query: &str| -> String {
        if query.chars().count() <= RECENT_TITLE_MAX {
            query.to_string()
        } else {
            query.chars().take(RECENT_TITLE_MAX).chain(std::iter::once('\u{2026}')).collect()
        }
    };
    let mut entries = vec![Entry::Header("Recent Searches".into())];
    entries.extend(recent.iter().enumerate().map(|(ix, query)| Entry::Item(Item::new(title(query), FindControl::Recent(ix)))));
    entries.push(Entry::Separator);
    entries.push(Entry::Item(Item::new("Clear Recent Searches", FindControl::ClearRecent).disabled(recent.is_empty())));
    entries
}

/// Opens the Recent Searches menu once the frame has drawn (nothing draws while a native menu is open),
/// two frames on as the status line's menus do, and runs the row picked through the bar's `act`.
fn recent_menu(act: Rc<dyn Fn(FindControl, &mut Window, &mut App)>, window: &mut Window, cx: &mut App) {
    let entries = recent_entries(&recent_find_queries(cx));
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |window, cx| {
            window
                .spawn(cx, async move |cx| {
                    let Some(control) = native_menu::pop_up(&entries, None) else { return };
                    cx.update(|window, cx| act(control, window, cx)).ok();
                })
                .detach();
        });
    });
}

/// One of the bar's option toggles (Match case, Whole word, Regular expression) drawn on its own: the app's icon button with
/// the bar's wash while it is on, `tip` as its tooltip and `press` as its click. Find in folder's float shows the same three.
pub fn option_button(id: &'static str, key: usize, symbol: &'static str, tip: String, on: bool, press: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static, cx: &App) -> Div {
    let (primary, muted, wash) = (theme::ink(ALPHA_PRIMARY, cx), theme::ink(ALPHA_SECONDARY, cx), theme::ink(1., cx));
    let button = icon_button((id, key), symbol, ICON_BUTTON, FIND_SYMBOL, Ink { rest: if on { primary } else { muted }, lit: primary, wash })
        .occlude()
        .on_click(press)
        .tooltip(tooltip::tip(tip))
        .tooltip_show_delay(tooltip::DELAY);
    let ground = if on { crate::explorer::icons::accent().opacity(ALPHA_OPTION_ON) } else { wash.opacity(ALPHA_OPTION_REST) };
    div().flex_none().rounded(RADIUS_BUTTON).bg(ground).child(button)
}

/// The replace button's tip, by whether the replace row is open. Open, one Cmd-F goes back to plain Find from
/// either field (the panel's `Search` handler is not tied to a field), so it is the one key shown; Shift-Cmd-F
/// shows the row.
fn replace_tip(open: bool) -> String {
    if open { tooltip::with_key("Hide replace", "\u{2318}F") } else { tooltip::with_key("Show replace", "\u{21e7}\u{2318}F") }
}

fn render(bar: FindBar, window: &mut Window, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let ink = theme.sidebar_foreground;
    let light = theme::is_light(cx);
    let (fill, border) = tooltip::float_fill(theme.background, ink, light);
    let family = theme.font_family.clone();
    let (primary, muted) = (theme::ink(ALPHA_PRIMARY, cx), theme::ink(ALPHA_SECONDARY, cx));
    let wash = theme::ink(1., cx);

    let field = |input: &Entity<InputState>, symbol: &'static str, name: &'static str, lens: Option<Lens>, cx: &App| {
        float_field_named(input, Some(symbol), 0., Some(name), lens, cx).flex_shrink(1.).min_w(FIND_FIELD_HEIGHT * 3.).w(FIND_FIELD_WIDTH)
    };

    // Ids carry the search entity, so two panes' bars never share a hover fade.
    let key = bar.search.entity_id().as_u64() as usize;
    crate::control::name_field("find", bar.search.focus_handle(cx));
    if let Some(replace) = &bar.replace {
        crate::control::name_field("replace", replace.focus_handle(cx));
        crate::control::name_input("replace", replace);
    }
    let act = bar.act.clone();
    // The magnifier opens Recent Searches, the list Up in the field walks.
    let lens = Lens {
        key,
        tip: "Recent searches",
        press: {
            let act = bar.act.clone();
            Rc::new(move |window, cx| recent_menu(act.clone(), window, cx))
        },
    };
    let press = move |control: FindControl| {
        let act = act.clone();
        move |_: &ClickEvent, window: &mut Window, cx: &mut App| act(control, window, cx)
    };
    let none = bar.total == 0;
    let dim = fade_none(key, none, window, cx);
    let (pulse_prev, pulse_next) = pulse(key, bar.wraps, bar.wrap_forward, window, cx);
    let miss = miss_pulse(key, bar.misses, window, cx);
    // The Find menu's checks follow these buttons: a click, or smartcase turning on for a capital,
    // changes them without an action, so the bar sets the menu again when they change. Its replace
    // rows follow whether the replace row is open (and the bar not leaving).
    let options = (bar.case_sensitive, bar.whole_word, bar.regex, bar.ignore_diacritics, !bar.leaving && bar.replace.is_some(), !bar.leaving && bar.in_selection);
    let seen = window.use_keyed_state(("find-options", key), cx, |_, _| options);
    if seen.update(cx, |seen, _| std::mem::replace(seen, options)) != options {
        cx.defer(crate::menu::refresh);
    }
    let accent = (bar.case_sensitive || bar.whole_word || bar.regex || bar.ignore_diacritics || bar.in_selection).then(crate::explorer::icons::accent);
    // A control: its tooltip, its click, and a wash behind it while it is on.
    let button = |id: &'static str, symbol: &'static str, label: String, control: FindControl, on: Option<bool>, option: bool, disabled: bool, dimmable: bool, pulse: f32| {
        let base = crate::tab_bar::mix(muted, primary, pulse);
        let glyph = if dimmable { base.opacity(1. - dim * (1. - ALPHA_BUTTON_OFF)) } else if on == Some(true) { primary } else { muted };
        // A control that can't run has no wash and keeps its dimmed glyph under the pointer, and no
        // click (as the explorer's header buttons). `occlude` keeps a press on it from moving the window.
        let control_button = icon_button(
            (id, key),
            symbol,
            ICON_BUTTON,
            FIND_SYMBOL,
            Ink { rest: glyph, lit: if disabled { glyph } else { primary }, wash: if disabled { wash.opacity(0.) } else { wash } },
        )
        .occlude()
        .when(!disabled, |button| button.on_click(press(control)))
        .tooltip(tooltip::tip(label))
        .tooltip_show_delay(tooltip::DELAY);
        div()
            .flex_none()
            .rounded(RADIUS_BUTTON)
            .bg(match (on == Some(true), option) {
                (true, true) => accent.unwrap_or_else(crate::explorer::icons::accent).opacity(ALPHA_OPTION_ON),
                (true, false) => wash.opacity(ALPHA_BUTTON_PRESSED),
                (false, true) => wash.opacity(ALPHA_OPTION_REST),
                (false, false) => gpui_kit::transparent_black(),
            })
            .child(control_button)
            // The drawn rect for `debug rects` and `click`, under the button's own id.
            .children(crate::control::track(id))
    };

    // The tip belongs to the "Bad pattern" count only (the chain below), not to a count another state shows.
    let why = bar.bad_pattern.filter(|_| bar.replaced.is_none() && !bar.query_empty && !bar.scope_empty);
    let (kind, count): (CountKind, SharedString) = if let Some(replaced) = bar.replaced {
        (CountKind::Replaced, format!("Replaced {}", grouped(replaced)).into())
    } else if bar.query_empty {
        (CountKind::Empty, SharedString::default())
    } else if bar.scope_empty {
        (CountKind::None, "No selection".into())
    } else if bar.bad_pattern.is_some() {
        (CountKind::None, "Bad pattern".into())
    } else if bar.total == 0 {
        (CountKind::None, "No results".into())
    } else {
        (CountKind::Matches, format!("{} of {}", grouped(bar.current), grouped(bar.total)).into())
    };
    let (count_level, count_out) = count_layers(key, kind, &count, window, cx);
    // A layer's ink follows its own kind; the miss pulse lifts the no-results ink to the full ink.
    let layer_ink = |kind: CountKind, miss: f32| if kind == CountKind::None { crate::tab_bar::mix(primary, wash, miss) } else { muted };
    let layer = |ink: Hsla, level: f32, text: SharedString| {
        div().px(FIND_PAD).text_right().whitespace_nowrap().text_color(ink).opacity(level).child(text)
    };

    // The count's box is as wide as the widest fixed text ("No selection", a 4-digit "8,888 of 8,888") and its padding, shaped as it is drawn, so the buttons beside it stay put as the count changes words.
    let count_width = count_width(&family, window);
    let on = [bar.case_sensitive, bar.whole_word, bar.regex, bar.in_selection, bar.ignore_diacritics];
    let shown = options_that_fit(bar.width - FIND_INSET_RIGHT - BAR_EDGES, count_width, bar.can_replace, on);
    let first = div()
        .flex()
        .min_w_0()
        .items_center()
        .h(FIND_HEIGHT - FIND_PAD * 2.)
        .gap(FIND_PAD / 2.)
        .child(field(&bar.search, "magnifyingglass", "find", Some(lens), cx))
        .child(div().w(FIND_PAD / 2.))
        .when(shown[0], |row| row.child(button("find-case", "textformat", tooltip::with_key("Match case", "\u{2325}\u{2318}C"), FindControl::CaseSensitive, Some(bar.case_sensitive), true, false, false, 0.)))
        .when(shown[1], |row| row.child(button("find-word", "textformat.abc.dottedunderline", tooltip::with_key("Whole word", "\u{2325}\u{2318}W"), FindControl::WholeWord, Some(bar.whole_word), true, false, false, 0.)))
        .when(shown[2], |row| row.child(button("find-regex", "ellipsis.curlybraces", tooltip::with_key("Regular expression", "\u{2325}\u{2318}R"), FindControl::Regex, Some(bar.regex), true, false, false, 0.)))
        .when(shown[3], |row| row.child(button("find-selection", "text.viewfinder", tooltip::with_key("Find in selection", "\u{2325}\u{2318}L"), FindControl::InSelection, Some(bar.in_selection), true, false, false, 0.)))
        .when(shown[4], |row| row.child(button("find-diacritics", "character.phonetic", "Ignore diacritics".to_string(), FindControl::IgnoreDiacritics, Some(bar.ignore_diacritics), true, false, false, 0.)))
        .child(
            div()
                .id(("find-count", key))
                .relative()
                .flex_none()
                .min_w(count_width)
                .text_size(TEXT_SUBHEADLINE)
                .line_height(TEXT_SUBHEADLINE_LINE)
                .font_features(tabular())
                .child(layer(layer_ink(kind, miss), count_level, count))
                .children(count_out.map(|(kind, text, level)| layer(layer_ink(kind, 0.), level, text).absolute().top_0().right_0()))
                // A pattern that does not compile says what is wrong with it.
                .when_some(why, |count, why| count.tooltip(tooltip::tip(why)).tooltip_show_delay(tooltip::DELAY)),
        )
        .child(button("find-prev", "chevron.up", tooltip::with_key("Previous match", "\u{21e7}\u{21a9}"), FindControl::Previous, None, false, none, true, pulse_prev))
        .child(button("find-next", "chevron.down", tooltip::with_key("Next match", "\u{21a9}"), FindControl::Next, None, false, none, true, pulse_next))
        .children(bar.can_replace.then(|| {
            let tip = replace_tip(bar.replace.is_some());
            button(
                "find-replace",
                "arrow.left.arrow.right",
                tip,
                FindControl::ToggleReplace,
                Some(bar.replace.is_some()),
                false,
                false,
                false,
                0.,
            )
        }))
        .child(button("find-close", "xmark", tooltip::with_key("Close", "\u{238b}"), FindControl::Close, None, false, false, false, 0.));

    let second = bar.replace.as_ref().map(|replace| {
        let text_button = |id: &'static str, label: &'static str, tip: String, control: FindControl| {
            let (hover, pressed) = if none { (0., 0.) } else { (ALPHA_BUTTON_HOVER, ALPHA_BUTTON_PRESSED) };
            div()
                .id((id, key))
                .flex_none()
                .h(FIND_FIELD_HEIGHT)
                .px(FILTER_PAD_X) // design: `.px` is the horizontal padding method, the value is a token
                .flex()
                .items_center()
                .rounded(RADIUS_BUTTON)
                .fade_hover((id, key), None, wash.opacity(hover))
                .press_fill(wash.opacity(pressed))
                .text_size(TEXT_SUBHEADLINE)
                .line_height(TEXT_SUBHEADLINE_LINE)
                .text_color(primary.opacity(1. - dim * (1. - ALPHA_BUTTON_OFF)))
                .when(!none, |button| button.on_click(press(control)))
                .tooltip(tooltip::tip(tip))
                .tooltip_show_delay(tooltip::DELAY)
                .child(label)
        };
        div()
            .flex()
            .min_w_0()
            .items_center()
            .h(FIND_HEIGHT - FIND_PAD * 2.)
            .gap(FIND_PAD / 2.)
            .child(field(replace, "arrow.triangle.2.circlepath", "replace", None, cx))
            // The button moves on to the next match, as Return in the Replace field does, so its tip
            // names the menu's Replace and find.
            .child(text_button("find-replace-one", "Replace", tooltip::with_key("Replace and find", "\u{2325}\u{2318}="), FindControl::ReplaceOne))
            .child(text_button("find-replace-all", "Replace all", tooltip::with_key("Replace all", "\u{2325}\u{2318}\u{21a9}"), FindControl::ReplaceAll))
    });

    let body = tooltip::float(RADIUS_FLOAT_CAPSULE, fill, border, light, false)
        .id("find-bar")
        .occlude()
        .flex()
        .flex_col()
        .gap(FIND_PAD)
        .p(FIND_PAD)
        .font_family(family)
        .text_color(primary)
        .child(div().absolute().inset_0().child({
            let cover = bar.cover.clone();
            canvas(move |bounds, _, cx| cover(Some(bounds), cx), |_, (), _, _| ()).size_full()
        }))
        .child(first)
        .children(second)
        // The drawn rect, rise included, for `debug rects`; the bar draws only while open or leaving.
        .children(crate::control::track("find"));

    // The bar's place is a margin, so its rise and fade move only the margin and the paint. The
    // panel around it spans the editor and pushes it right, so a narrow pane shrinks the bar.
    let level = reveal(key, bar.leaving, window, cx);
    let place = div().min_w_0().mt(FIND_INSET_TOP + px(OVERLAY_ENTER_RISE * (1. - level))).mr(FIND_INSET_RIGHT).opacity(level).child(body);
    if bar.leaving {
        // A leaving bar takes no clicks and no focus: a cover over it eats them until it is gone.
        place.child(div().id("find-bar-cover").absolute().inset_0().occlude()).into_any_element()
    } else {
        place.into_any_element()
    }
}

#[cfg(test)]
mod tests {
    // Not `super::*`: it carries GPUI Kit's own `test` attribute over the standard one.
    use super::{BAR_EDGES, Entry, FindControl, RECENT_TITLE_MAX, options_that_fit, recent_entries, replace_tip};
    use gpui_kit::px;

    /// The rows as the menu reads: a header, each find newest first, a part, Clear Recent Searches.
    #[test]
    fn recent_menu_lists_the_finds_newest_first_then_clear() {
        let entries = recent_entries(&["beta".to_string(), "alpha".to_string()]);
        assert_eq!(entries.len(), 5);
        assert!(matches!(&entries[0], Entry::Header(title) if title == "Recent Searches"));
        assert!(matches!(&entries[1], Entry::Item(item) if item.title == "beta" && item.run == FindControl::Recent(0) && !item.disabled));
        assert!(matches!(&entries[2], Entry::Item(item) if item.title == "alpha" && item.run == FindControl::Recent(1)));
        assert!(matches!(entries[3], Entry::Separator));
        assert!(matches!(&entries[4], Entry::Item(item) if item.title == "Clear Recent Searches" && item.run == FindControl::ClearRecent && !item.disabled));
    }

    #[test]
    fn clear_is_grey_with_nothing_to_clear_and_a_long_find_is_cut() {
        let entries = recent_entries(&[]);
        assert!(matches!(entries.last(), Some(Entry::Item(item)) if item.disabled));
        let long = "x".repeat(RECENT_TITLE_MAX + 10);
        let entries = recent_entries(&[long]);
        assert!(matches!(&entries[1], Entry::Item(item) if item.title.chars().count() == RECENT_TITLE_MAX + 1 && item.title.ends_with('\u{2026}')));
    }

    /// A narrow bar drops the options that are off, from the right, one by one, and always keeps one;
    /// a wide one keeps all five, and an option that is on is never dropped.
    #[test]
    fn a_narrow_bar_drops_options_that_are_off_from_the_right() {
        let fit = |width: f32, replace, on| options_that_fit(px(width) - crate::tokens::FIND_INSET_RIGHT - BAR_EDGES, px(90.), replace, on);
        let count = |shown: [bool; 5]| shown.iter().filter(|shown| **shown).count();
        let off = [false; 5];
        assert_eq!(fit(10_000., true, off), [true; 5]);
        let mut last = 5;
        for width in (0..900).rev() {
            let shown = fit(width as f32, true, off);
            let now = count(shown);
            assert!(now <= last && last - now <= 1, "{width}: {last} then {now}");
            assert!(shown.iter().take(now).all(|shown| *shown), "{width}: {shown:?} drops from the right");
            last = now;
        }
        assert_eq!(last, 1);
        // Without the replace button there is room for one more option at the same width.
        let width = (0..900).find(|&width| count(fit(width as f32, true, off)) == 4).unwrap() as f32;
        assert_eq!(count(fit(width, false, off)), 5);
        // An option that is on stays where the bar is too narrow for it: the next off one from the right goes instead.
        let narrow = (0..900).find(|&width| count(fit(width as f32, true, off)) == 2).unwrap() as f32;
        assert_eq!(fit(narrow, true, [false, false, false, true, false]), [true, false, false, true, false]);
        assert_eq!(fit(narrow, true, [false, false, true, false, true]), [false, false, true, false, true]);
        // With every option on, nothing goes, however narrow.
        assert_eq!(fit(0., true, [true; 5]), [true; 5]);
    }

    #[test]
    fn hide_replace_tip_names_cmd_f() {
        let sep = '\u{2002}';
        assert_eq!(replace_tip(false), format!("Show replace{sep}\u{21e7}\u{2318}F"));
        assert_eq!(replace_tip(true), format!("Hide replace{sep}\u{2318}F"));
    }
}
