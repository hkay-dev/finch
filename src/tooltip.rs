//! The system tooltip's look and delay for GPUI's `.tooltip()` (Harrier's `tooltip.rs` is the
//! model): AppKit's `.help()` text is 11 pt on a panel with a small radius and a soft shadow, sits
//! just below the pointer's arrow, and appears half a second after the pointer settles, settling
//! 2 pt as it fades in. A tip asked for within `TOOLTIP_WARM` of the last one hiding shows at once
//! (AppKit's warm tooltips); a press or the pointer leaving the window makes the next one cold.
//! GPUI fixes its show delay before it knows which, so `DELAY` is zero and the tip keeps the wait.
//!
//! ```ignore
//! button.tooltip(tooltip::tip("Show preview\u{2002}⇧⌘V")).tooltip_show_delay(tooltip::DELAY)
//! ```
//!
//! GPUI places a tip 1 pt off the pointer, under the arrow, so the tip pads itself down and right
//! by the arrow's size. Near the window's bottom or right edge GPUI flips it above or left of the
//! pointer; the tip sees that from where it last landed and moves its padding to the far side, so a
//! flipped tip sits just above the pointer or ends at it. The padding's total never changes, so the
//! flip can't flicker. It settles toward the pointer by a relative offset, which leaves its laid-out
//! size alone.
//!
//! GPUI drops a tip the moment it hides, so a tip that goes leaves its look and place behind
//! (`Drop`), and the window draws that over everything for `TOOLTIP_FADE_OUT` as it fades (`Layer`,
//! a small view among the root's children). A hand-over leaves no ghost: the next tip is already up.
//!
//! Two of AppKit's manners GPUI lacks, kept from the same layer. GPUI tracks the pointer only while
//! it moves over the window, so a tip whose pointer leaves the window stays up; the layer hears the
//! window's mouse-exit and the tip fades out. And GPUI asks for a tip again after a click on its
//! control; the layer notes each press, and a tip asked for under one (while it waited, or on the
//! control whose tip was showing) stays away until the pointer leaves that control.

use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::LazyLock,
    time::{Duration, Instant},
};

use gpui_kit::{
    AnyElement, AnyView, App, Bounds, Context, Div, EntityId, Hsla, IntoElement, MouseDownEvent, MouseExitEvent, Pixels, Point, Render,
    SharedString, Window, WindowId, anchored, canvas, deferred, div, point, prelude::*, px, BoxShadow,
};
use gpui_kit::component::ActiveTheme as _;

use crate::theme;
use crate::tokens::{
    ALPHA_BORDER, ALPHA_BORDER_LIGHT, ALPHA_HIGHLIGHT, ALPHA_PRIMARY, ALPHA_SECONDARY, OVERLAY_ENTER, LAYER_TOOLTIP, PANEL_LIFT,
    PANEL_LIFT_LIGHT, RADIUS_TOOLTIP, SHADOW_FLOAT_AMBIENT, SHADOW_FLOAT_AMBIENT_LIGHT, SHADOW_FLOAT_CONTACT,
    SHADOW_FLOAT_CONTACT_LIGHT, SHADOW_FLOAT_LIFT, SHADOW_FLOAT_LIFT_LIGHT, TEXT_SUBHEADLINE,
    TEXT_SUBHEADLINE_LINE, TOOLTIP_FADE_OUT, TOOLTIP_GAP_ABOVE, TOOLTIP_MAX_W, TOOLTIP_OFFSET_X, TOOLTIP_OFFSET_Y, TOOLTIP_PAD_X,
    TOOLTIP_PAD_Y, TOOLTIP_PRESS_REACH, TOOLTIP_RISE, TOOLTIP_DELAY, TOOLTIP_WARM,
};
/// GPUI's own show delay for a tip: none. `tip` waits `TOOLTIP_DELAY` itself, which lets it skip the
/// wait when warm (GPUI fixes its delay before it knows).
pub const DELAY: Duration = Duration::ZERO;

/// Between a tip's words and its key: an en space, so the text reads right anywhere else it lands,
/// and the tip finds the key after it.
const KEY_SEPARATOR: char = '\u{2002}';

/// `text` with `key` after it: "Hide preview ⇧⌘V".
pub fn with_key(text: &str, key: &str) -> String {
    format!("{text}{KEY_SEPARATOR}{key}")
}

/// What a tip draws: its words, its key (split off once, so a fading tip allocates nothing per
/// frame), and inks.
#[derive(Clone)]
struct Look {
    words: SharedString,
    key: Option<SharedString>,
    fg: Hsla,
    key_fg: Hsla,
    bg: Hsla,
    border: Hsla,
    light: bool,
}

impl Look {
    fn new(text: &SharedString, cx: &App) -> Self {
        let theme = cx.theme();
        let ink = theme.sidebar_foreground;
        let light = theme::is_light(cx);
        let (words, key) = match text.rsplit_once(KEY_SEPARATOR) {
            Some((words, key)) => (SharedString::from(words.to_owned()), Some(SharedString::from(key.to_owned()))),
            None => (text.clone(), None),
        };
        let (bg, border) = float_fill(theme.background, ink, light);
        Look {
            words,
            key,
            fg: theme::ink(ALPHA_PRIMARY, cx),
            key_fg: theme::ink(ALPHA_SECONDARY, cx),
            bg,
            border,
            light,
        }
    }
}

/// Where a tip was last drawn, in which window, how much of it showed, and its side of the pointer.
type Placed = Rc<Cell<Option<(Bounds<Pixels>, WindowId, f32, Side)>>>;

/// Which side of the pointer GPUI put a tip on: below or flipped above, right or flipped left.
#[derive(Clone, Copy)]
struct Side {
    below: bool,
    right: bool,
}

impl Side {
    const DEFAULT: Side = Side { below: true, right: true };
}

struct Tip {
    look: Look,
    /// The text it was asked for and its window, to know it again in `SHOWING`.
    text: SharedString,
    window: WindowId,
    /// When it started to show (set again as it goes live), on the frame clock.
    shown: Cell<Instant>,
    /// When it was asked for, and when its wait ends (at once for a warm tip).
    created: Instant,
    ready: Instant,
    /// The wait is over and it shows; `since` is when it did, for the presses after it.
    live: Cell<bool>,
    since: Cell<Instant>,
    placed: Placed,
    /// The pointer when the tip was asked for, where GPUI anchors it.
    pointer: Point<Pixels>,
    /// Asked for under a press, or a press fell while it waited: it draws nothing, and going (the
    /// pointer leaving its control) lets the next tip show.
    quiet: Cell<bool>,
}

/// The last tip to go, fading where it stood.
struct Ghost {
    look: Look,
    bounds: Bounds<Pixels>,
    side: Side,
    window: WindowId,
    from: f32,
    left: Instant,
}

/// The last mouse press in a window, when, where, and the text of the tip showing then, if one was.
struct Press {
    window: WindowId,
    at: Instant,
    pointer: Point<Pixels>,
    under_tip: Option<SharedString>,
}

thread_local! {
    /// Tips on screen now, in any window: while one is, no ghost draws (a hand-over).
    static LIVE: Cell<usize> = const { Cell::new(0) };
    static GHOST: RefCell<Option<Ghost>> = const { RefCell::new(None) };
    /// The tip showing, by window, text and entity, for a press to name and the pointer leaving to wake.
    static SHOWING: RefCell<Option<(WindowId, SharedString, EntityId)>> = const { RefCell::new(None) };
    static PRESS: RefCell<Option<Press>> = const { RefCell::new(None) };
    /// When the pointer last left a window.
    static LEFT: Cell<Option<(WindowId, Instant)>> = const { Cell::new(None) };
    /// The window and time the last tip hid, while the next tip can still show warm; a press or the
    /// pointer leaving the window clears it.
    static WARM: Cell<Option<(WindowId, Instant)>> = const { Cell::new(None) };
}

/// The float plane's shadows: a contact shadow and an ambient one, in black. A card held by the
/// pointer passes `held` and swaps the ambient for the deeper lift the dragged tab has, since it is
/// one plane above the rest.
pub(crate) fn shadows(light: bool, held: bool) -> Vec<BoxShadow> {
    let contact = if light { SHADOW_FLOAT_CONTACT_LIGHT } else { SHADOW_FLOAT_CONTACT };
    let ambient = match (light, held) {
        (false, false) => SHADOW_FLOAT_AMBIENT,
        (true, false) => SHADOW_FLOAT_AMBIENT_LIGHT,
        (false, true) => SHADOW_FLOAT_LIFT,
        (true, true) => SHADOW_FLOAT_LIFT_LIGHT,
    };
    [contact, ambient]
        .map(|[alpha, y, blur]| BoxShadow {
            color: gpui_kit::black().opacity(alpha),
            offset: point(px(0.), px(y)),
            blur_radius: px(blur),
            spread_radius: px(0.),
            inset: false,
        })
        .to_vec()
}

/// A float's fill and rim: the window's ground `PANEL_LIFT` of the way toward the ink (white on a
/// light theme, `PANEL_LIFT_LIGHT`), opaque, and the ink at `ALPHA_BORDER` (`ALPHA_BORDER_LIGHT` on
/// light) for the rim.
pub(crate) fn float_fill(ground: Hsla, ink: Hsla, light: bool) -> (Hsla, Hsla) {
    let toward = if light { Hsla { h: 0., s: 0., l: 1., a: PANEL_LIFT_LIGHT } } else { Hsla { a: PANEL_LIFT, ..ink } };
    (ground.blend(toward), Hsla { a: if light { ALPHA_BORDER_LIGHT } else { ALPHA_BORDER }, ..ink })
}

/// The one float plane every tooltip, the find bar and a drag card stand on: `fill` and `border`
/// from `float_fill`, a rounded rim, both shadows and, on a dark theme, the lit top edge.
pub(crate) fn float(radius: f32, fill: Hsla, border: Hsla, light: bool, held: bool) -> Div {
    div()
        .relative()
        .rounded(px(radius))
        .border_1()
        .border_color(border)
        .bg(fill)
        .shadow(shadows(light, held))
        .children((!light).then(|| top_highlight(radius)))
}

/// The float plane's lit top edge on a dark theme: white along the top of a rounded rim, clear
/// toward the sides. A light float is white already, so it has none.
pub(crate) fn top_highlight(radius: f32) -> Div {
    let white = gpui_kit::white().opacity(ALPHA_HIGHLIGHT);
    div().absolute().inset_0().child(
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let width = 1. / window.scale_factor().max(1.);
                let inset = width / 2.;
                let (left, top) = (bounds.left() + px(inset), bounds.top() + px(inset));
                let (right, bottom) = (bounds.right() - px(inset), bounds.bottom() - px(inset));
                let r = px((radius - inset).max(0.));
                let k = r * 0.552_284_8;
                let mut path = gpui_kit::PathBuilder::stroke(px(width));
                path.move_to(point(left + r, top));
                path.line_to(point(right - r, top));
                path.cubic_bezier_to(point(right, top + r), point(right - r + k, top), point(right, top + r - k));
                path.line_to(point(right, bottom - r));
                path.cubic_bezier_to(point(right - r, bottom), point(right, bottom - r + k), point(right - r + k, bottom));
                path.line_to(point(left + r, bottom));
                path.cubic_bezier_to(point(left, bottom - r), point(left + r - k, bottom), point(left, bottom - r + k));
                path.line_to(point(left, top + r));
                path.cubic_bezier_to(point(left + r, top), point(left, top + r - k), point(left + r - k, top));
                path.close();
                if let Ok(path) = path.build() {
                    window.paint_path(path, gpui_kit::linear_gradient(
                        180.,
                        gpui_kit::linear_color_stop(white, 0.),
                        gpui_kit::linear_color_stop(white.opacity(0.), 0.35),
                    ));
                }
            },
        )
        .size_full(),
    )
}

/// The tip itself, `amount` of the way in: the words, then the key a step quieter, padded off the
/// pointer's arrow on `side`. It carries the float plane's lit top edge, as every floating panel does.
fn frame(look: &Look, amount: f32, side: Side) -> Div {
    let body = float(RADIUS_TOOLTIP, look.bg, look.border, look.light, false)
        .flex()
        .items_baseline()
        .gap(px(TOOLTIP_PAD_X))
        .px(px(TOOLTIP_PAD_X))
        .py(px(TOOLTIP_PAD_Y))
        .max_w(px(TOOLTIP_MAX_W))
        // The whole style, not only the size: GPUI lays a live tip out as a root of its own, on its
        // default line, while the fading ghost sits in the window's text, so a bare size drew the
        // tip taller than its ghost and the words jumped as it let go.
        .text_size(TEXT_SUBHEADLINE)
        .line_height(TEXT_SUBHEADLINE_LINE)
        .font_weight(gpui_kit::FontWeight::NORMAL)
        // Even-width figures, so a count in a tip (a goal's progress) lines up as the status line does.
        .font_features(crate::tokens::tabular())
        .text_color(look.fg)
        .opacity(amount)
        // Settling toward the pointer moves it without changing its size, which GPUI's flip reads.
        .top(px(TOOLTIP_RISE * (1. - amount) * if side.below { 1. } else { -1. }))
        .child(div().min_w_0().child(look.words.clone()))
        .children(look.key.clone().map(|key| div().flex_none().text_color(look.key_fg).child(key)))
        .children(crate::control::track("tooltip"));
    // The arrow's padding goes on the far side of a flipped tip, the same total either way.
    let (above, under) = if side.below {
        (TOOLTIP_OFFSET_Y, 0.)
    } else {
        (TOOLTIP_OFFSET_Y - TOOLTIP_GAP_ABOVE, TOOLTIP_GAP_ABOVE)
    };
    let (before, after) = if side.right { (TOOLTIP_OFFSET_X, 0.) } else { (0., TOOLTIP_OFFSET_X) };
    div().relative().pl(px(before)).pr(px(after)).pt(px(above)).pb(px(under)).child(body)
}

impl Render for Tip {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.live.get() && !self.quiet.get() {
            if Instant::now() < self.ready {
                return div().into_any_element();
            }
            if pressed_since(self.window, self.created) {
                // A press fell while it waited: it stays away until the pointer leaves the control.
                self.quiet.set(true);
            } else {
                self.go_live(cx.entity_id());
            }
        }
        // A tip whose pointer left the window since it showed fades out where it stands.
        let out = match LEFT.get().filter(|(left, at)| *left == self.window && *at > self.shown.get()) {
            None => 1.,
            Some((_, at)) => {
                let t = crate::motion::drawn_now(self.window).saturating_duration_since(at).as_secs_f32() / TOOLTIP_FADE_OUT;
                if t < 1. {
                    window.request_animation_frame();
                }
                1. - crate::motion::ease_in(t.min(1.))
            }
        };
        if self.quiet.get() || out <= 0. {
            // Nothing to leave behind when GPUI drops it.
            self.placed.set(None);
            return div().into_any_element();
        }
        let amount = arrival(self.shown.get(), window);
        // GPUI flips a tip that would cross the window's edge; where it landed last frame says which way.
        let side = self
            .placed
            .get()
            .map_or(Side::DEFAULT, |(bounds, ..)| Side { below: bounds.origin.y >= self.pointer.y, right: bounds.origin.x >= self.pointer.x });
        let placed = self.placed.clone();
        let shown = amount * out;
        frame(&self.look, amount, side)
            .opacity(out)
            .child(
                canvas(move |bounds, window, _| placed.set(Some((bounds, window.window_handle().window_id(), shown, side))), |_, _, _, _| ())
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
            )
            .into_any_element()
    }
}

/// How far a tip that showed at `shown` has come in, on the expo-out curve over `OVERLAY_ENTER`, 0 to 1. Asks for frames while it moves.
fn arrival(shown: Instant, window: &Window) -> f32 {
    let t = crate::motion::drawn_now(window.window_handle().window_id()).saturating_duration_since(shown).as_secs_f32() / OVERLAY_ENTER;
    if t >= 1. {
        return 1.;
    }
    window.request_animation_frame();
    crate::motion::expo_out(t)
}

impl Tip {
    /// The wait is over: count the tip, name it for presses and the pointer leaving, and start its fade in.
    fn go_live(&self, id: EntityId) {
        self.live.set(true);
        self.shown.set(crate::motion::drawn_now(self.window));
        self.since.set(Instant::now());
        LIVE.set(LIVE.get() + 1);
        PRESS.set(None);
        SHOWING.set(Some((self.window, self.text.clone(), id)));
    }
}

impl Drop for Tip {
    fn drop(&mut self) {
        if self.quiet.get() {
            // The pointer left the control it pressed: its tip may show again.
            PRESS.set(None);
            return;
        }
        if !self.live.get() {
            // Never showed: nothing to leave behind, nothing warm.
            return;
        }
        LIVE.set(LIVE.get().saturating_sub(1));
        SHOWING.with_borrow_mut(|showing| {
            if showing.as_ref().is_some_and(|(window, text, _)| *window == self.window && *text == self.text) {
                *showing = None;
            }
        });
        // A press or the pointer leaving the window since it showed leaves the next tip cold.
        let left = LEFT.get().is_some_and(|(window, at)| window == self.window && at > self.shown.get());
        if !left && !pressed_since(self.window, self.since.get()) {
            WARM.set(Some((self.window, Instant::now())));
        }
        if let Some((bounds, window, from, side)) = self.placed.get().filter(|(.., from, _)| *from > 0.) {
            GHOST.set(Some(Ghost { look: self.look.clone(), bounds, side, window, from, left: crate::motion::drawn_now(window) }));
        } else if LIVE.get() == 0 {
            crate::control::unrecord(self.window, "tooltip");
        }
    }
}

/// Whether a tip for `text` asked for at `pointer` in `window` comes under the last press: the tip
/// showing then was this control's (its text, or the pointer still near where it pressed, for a
/// control whose tip changes with the click).
fn pressed(window: WindowId, text: &SharedString, pointer: Point<Pixels>) -> bool {
    PRESS.with_borrow(|press| {
        press.as_ref().is_some_and(|press| {
            let near = || {
                let moved = press.pointer - pointer;
                f32::from(moved.x).hypot(f32::from(moved.y)) < TOOLTIP_PRESS_REACH
            };
            press.window == window && press.under_tip.as_ref().is_some_and(|under| under == text || near())
        })
    })
}

/// Whether `window` saw a press after `since`: a tip that waited through one stays away.
fn pressed_since(window: WindowId, since: Instant) -> bool {
    PRESS.with_borrow(|press| press.as_ref().is_some_and(|press| press.window == window && press.at > since))
}

/// Whether a tip asked for now in `window` shows at once: another is up there (a hand-over), or one
/// hid within `TOOLTIP_WARM` and no press or exit since.
fn warm(window: WindowId) -> bool {
    WARM.get().is_some_and(|(warm, at)| warm == window && at.elapsed() < TOOLTIP_WARM)
        || SHOWING.with_borrow(|showing| showing.as_ref().is_some_and(|(showing, ..)| *showing == window))
}

/// The text of the tip showing in `window` now, with its key after an en space as `with_key` gave it (`debug tooltip`).
pub fn showing_text(window: WindowId) -> Option<SharedString> {
    SHOWING.with_borrow(|showing| showing.as_ref().filter(|(showing, ..)| *showing == window).map(|(_, text, _)| text.clone()))
}

/// The builder `.tooltip()` takes, for `text`. Its words and key are split apart once, as the tip
/// shows, not on every frame it draws. GPUI's own delay is zero (`DELAY`): the tip waits
/// `TOOLTIP_DELAY` itself unless it is warm, so it knows which.
pub fn tip(text: impl Into<SharedString>) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let text = text.into();
    move |window, cx| {
        let here = window.window_handle().window_id();
        let pointer = window.mouse_position();
        let quiet = pressed(here, &text, pointer);
        let warm = warm(here);
        let created = Instant::now();
        let wait = if warm { Duration::ZERO } else { TOOLTIP_DELAY };
        let tip = Tip {
            look: Look::new(&text, cx),
            text: text.clone(),
            window: here,
            shown: Cell::new(crate::motion::drawn_now(here)),
            created,
            ready: created + wait,
            live: Cell::new(false),
            since: Cell::new(created),
            placed: Placed::default(),
            pointer,
            quiet: Cell::new(quiet),
        };
        let tip = cx.new(|_| tip);
        if !quiet && !warm {
            let weak = tip.downgrade();
            cx.spawn(async move |cx| {
                cx.background_executor().timer(wait).await;
                weak.update(cx, |_, cx| cx.notify()).ok();
            })
            .detach();
        }
        tip.into()
    }
}

/// For a background capture, where nothing hovers: `GPUI_EDITOR_SHOW_TOOLTIP=<name>` makes the
/// control that asks with that `name` draw its tip in place, in the tip's own look, as if the
/// pointer rested on its middle. `None` for every other control.
pub fn forced(name: &str, text: &str, middle: Point<Pixels>, cx: &App) -> Option<AnyElement> {
    static NAMED: LazyLock<Option<String>> = LazyLock::new(|| std::env::var("GPUI_EDITOR_SHOW_TOOLTIP").ok());
    if NAMED.as_deref() != Some(name) {
        return None;
    }
    let look = Look::new(&SharedString::from(text.to_owned()), cx);
    Some(deferred(anchored().offset(middle).child(frame(&look, 1., Side::DEFAULT))).with_priority(LAYER_TOOLTIP).into_any_element())
}

/// Notes each press and the pointer leaving `window`, for `pressed` and a tip's fade out.
fn watch(window: &mut Window) {
    let here = window.window_handle().window_id();
    window.on_mouse_event(move |event: &MouseDownEvent, phase, _, _| {
        if phase.capture() {
            let under_tip = SHOWING.with_borrow(|showing| showing.as_ref().filter(|(window, ..)| *window == here).map(|(_, text, _)| text.clone()));
            PRESS.set(Some(Press { window: here, at: Instant::now(), pointer: event.position, under_tip }));
            WARM.set(None);
        }
    });
    window.on_mouse_event(move |_: &MouseExitEvent, phase, _, cx| {
        if phase.capture() {
            LEFT.set(Some((here, crate::motion::drawn_now(here))));
            PRESS.set(None);
            WARM.set(None);
            // Its fade out starts at the next render: wake the tip showing in this window.
            let showing = SHOWING.with_borrow(|showing| showing.as_ref().filter(|(window, ..)| *window == here).map(|(.., id)| *id));
            if let Some(id) = showing {
                cx.notify(id);
            }
        }
    });
}

/// The window's tooltip layer: it watches presses and the pointer leaving (`watch`), and draws the
/// tip that just went, fading out where it stood, over everything in the window; no ghost once it
/// has gone or while another tip is up. Asks for frames only while a ghost fades. A view of its own
/// among the root's children, so the ghost's frames redraw it and the root and leave every cached
/// view as it was.
pub struct Layer;

impl Render for Layer {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let watcher = canvas(|_, _, _| (), |_, (), window, _| watch(window)).size_0();
        div().size_0().child(watcher).children(ghost(window))
    }
}

fn ghost(window: &mut Window) -> Option<impl IntoElement> {
    if LIVE.get() > 0 {
        GHOST.set(None);
        return None;
    }
    let here = window.window_handle().window_id();
    let (look, bounds, side, amount) = GHOST.with_borrow_mut(|ghost| {
        let gone = ghost.as_ref()?;
        if gone.window != here {
            return None;
        }
        let t = crate::motion::drawn_now(here).saturating_duration_since(gone.left).as_secs_f32() / TOOLTIP_FADE_OUT;
        if t >= 1. {
            crate::control::unrecord(here, "tooltip");
            *ghost = None;
            return None;
        }
        Some((gone.look.clone(), gone.bounds, gone.side, gone.from * (1. - crate::motion::ease_in(t))))
    })?;
    window.request_animation_frame();
    Some(deferred(anchored().position(bounds.origin).child(frame(&look, 1., side).w(bounds.size.width).opacity(amount))).with_priority(LAYER_TOOLTIP))
}
