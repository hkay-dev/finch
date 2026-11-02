//! Hover and press fades: GPUI's `.hover()` and `.active()` switch in one frame, so a fill that
//! should fade is tracked here instead (Harrier's `FadeFill` is the model, ported by hand).
//!
//! A fade is a level from 0 (rest) to 1 (pointer over it) to 2 (pressed), eased out over `HOVER_IN`
//! going up and `HOVER_OUT` going down, and it turns around from where it is drawn. The level is a
//! function of when it last changed course and the frame's time alone, kept by element key and
//! window, so it survives the view redrawing. An element asks for frames only while its level
//! moves; at rest at 0 its entry is dropped. `icon_button` is the one look of a small glyph button.

use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    hash::{DefaultHasher, Hash, Hasher},
    time::Instant,
};

use gpui_kit::*;

use crate::{
    motion::{drawn_now, ease_out},
    symbols,
    tab_bar::mix,
    tokens::{
        ALPHA_BUTTON_HOVER, ALPHA_BUTTON_PRESSED, ALPHA_HOVER, ALPHA_HOVER_LIGHT, ALPHA_SELECTED, ALPHA_SELECTED_LIGHT, HOVER_IN, HOVER_OUT, RADIUS_BUTTON, RADIUS_ROW,
        ROW_WASH_INSET_Y,
    },
};

/// An element's fade is found by its window and the hash of its key.
type Key = (WindowId, u64);

/// Where one element's fade is: its pointer state, and the level and time it last changed course at.
#[derive(Clone, Copy)]
struct Fill {
    hovered: bool,
    pressed: bool,
    from: f32,
    since: Instant,
}

impl Fill {
    /// Where the level is headed: 0 rest, 1 under the pointer, 2 pressed under it.
    fn to(&self) -> f32 {
        match (self.hovered, self.pressed) {
            (false, _) => 0.,
            (true, false) => 1.,
            (true, true) => 2.,
        }
    }

    /// The level at `now`, and whether it still moves: a fade already at its target (a fresh one at
    /// rest, or one turned around where it is) has nowhere to go and asks for no frames.
    fn level(&self, now: Instant) -> (f32, bool) {
        let to = self.to();
        let length = if to > self.from { HOVER_IN } else { HOVER_OUT };
        let u = (now.saturating_duration_since(self.since).as_secs_f32() / length).min(1.);
        (self.from + (to - self.from) * ease_out(u), u < 1. && to != self.from)
    }
}

thread_local! {
    static FILLS: RefCell<HashMap<Key, Fill>> = RefCell::new(HashMap::new());
    /// The view that last drew each fill, so a pointer change redraws that view alone.
    static VIEWS: RefCell<HashMap<Key, EntityId>> = RefCell::new(HashMap::new());
    /// The fills held lit by an open menu (`FadeFill::held`).
    static HELD: RefCell<HashSet<Key>> = RefCell::new(HashSet::new());
    /// Where each fill that is being pointed at last drew.
    static BOUNDS: RefCell<HashMap<Key, Bounds<Pixels>>> = RefCell::new(HashMap::new());
}

/// How many views are remembered before those of elements nobody is fading are let go; a tab that
/// closed leaves its entry, and the next hover of one that still draws costs one full refresh.
const VIEWS_KEPT: usize = 128;

/// Remembers the view drawing the fill `key`; call it where the fill lays out.
fn drew_in_view(key: Key, window: &Window) {
    let view = window.current_view();
    VIEWS.with_borrow_mut(|views| {
        if views.get(&key) != Some(&view) {
            if views.len() >= VIEWS_KEPT {
                FILLS.with_borrow(|fills| views.retain(|key, _| fills.contains_key(key)));
            }
            views.insert(key, view);
        }
    });
}

/// Redraws the view that drew the fill `key`. A refresh draws every cached view, so it is only the
/// fallback before the fill has drawn.
fn redraw(key: Key, window: &mut Window, cx: &mut App) {
    match VIEWS.with_borrow(|views| views.get(&key).copied()) {
        Some(view) => cx.notify(view),
        None => window.refresh(),
    }
}

/// Sets the pointer state of the fill `key` (`None` keeps it), turning the level around from where
/// it is drawn; true when the state changed.
fn point_at(key: Key, hovered: Option<bool>, pressed: Option<bool>) -> bool {
    let now = drawn_now(key.0);
    FILLS.with_borrow_mut(|fills| {
        let old = fills.get(&key).copied().unwrap_or(Fill { hovered: false, pressed: false, from: 0., since: now });
        let (hovered, pressed) = (hovered.unwrap_or(old.hovered), pressed.unwrap_or(old.pressed));
        if (hovered, pressed) == (old.hovered, old.pressed) {
            return false;
        }
        fills.insert(key, Fill { hovered, pressed, from: old.level(now).0, since: now });
        true
    })
}

/// The eased level of the fill `key` in this frame, asking for the next frame while it moves; 0
/// for an element nobody has pointed at.
fn level(key: Key, window: &mut Window) -> f32 {
    let Some(fill) = FILLS.with_borrow(|fills| fills.get(&key).copied()) else {
        return 0.;
    };
    // An element that stopped drawing never hears the pointer leave (a button that opened a tab and
    // came back when the tab closed), so a hover whose last drawn bounds no longer hold the pointer
    // is dropped when the element draws again.
    let outlived = fill.hovered && !HELD.with_borrow(|held| held.contains(&key)) && BOUNDS.with_borrow(|all| all.get(&key).is_some_and(|at| !at.contains(&window.mouse_position())));
    if outlived {
        forget_fill(key);
        return 0.;
    }
    let (level, moving) = fill.level(drawn_now(key.0));
    if moving {
        window.request_animation_frame();
    } else if fill.to() == 0. {
        // At rest with the pointer away: no entry reads the same as this one.
        forget_fill(key);
    }
    level
}

/// Drops everything kept for the fill `key`.
fn forget_fill(key: Key) {
    FILLS.with_borrow_mut(|fills| fills.remove(&key));
    VIEWS.with_borrow_mut(|views| views.remove(&key));
    BOUNDS.with_borrow_mut(|all| all.remove(&key));
}

/// Drops a closed window's fades and the views that drew them.
pub fn forget(window: WindowId) {
    FILLS.with_borrow_mut(|fills| fills.retain(|(id, _), _| *id != window));
    VIEWS.with_borrow_mut(|views| views.retain(|(id, _), _| *id != window));
    BOUNDS.with_borrow_mut(|all| all.retain(|(id, _), _| *id != window));
    HELD.with_borrow_mut(|held| held.retain(|(id, _)| *id != window));
}

/// The eased level, 0 to 1, of an element that is `lit` while the pointer is over or holds it, for
/// code that is told the pointer state each frame instead of hearing its events (the kit's resize
/// handles). The level is kept under the element being laid out, turns around from where it is
/// drawn, and asks for frames only while it moves.
pub fn lit_level(lit: bool, window: &mut Window, cx: &mut App) -> f32 {
    let now = drawn_now(window.window_handle().window_id());
    let state = window.use_keyed_state("lit-level", cx, |_, _| Fill { hovered: false, pressed: false, from: 0., since: now });
    let mut fill = *state.read(cx);
    if fill.hovered != lit {
        fill = Fill { hovered: lit, pressed: false, from: fill.level(now).0, since: now };
        state.update(cx, |state, _| *state = fill);
    }
    let (level, moving) = fill.level(now);
    if moving {
        window.request_animation_frame();
    }
    level
}

/// An element whose fill fades between `rest` and `hover` as the pointer enters and leaves, and on
/// to `press` while it is held. It stands in for the div where `.hover()` and `.active()` were, and
/// takes the rest of the chain.
pub struct FadeFill {
    inner: Option<Stateful<Div>>,
    key: u64,
    rest: Hsla,
    hover: Hsla,
    press: Hsla,
    /// The text color at rest and under the pointer, fading with the fill.
    ink: Option<(Hsla, Hsla)>,
    /// Another fill covers this element (a chosen row's pill): the level falls to rest, from where it is drawn.
    covered: bool,
    /// A menu from this element is open: the press stays lit (`held`).
    held: bool,
    /// The fill is painted as the explorer's row pill (inset, rounded) under the element's content instead of as its background.
    pill: bool,
    /// The pill's color this frame, set at layout and painted under the content.
    wash: Option<Hsla>,
}

pub trait FadeHover {
    /// Fades a fill to `hover` under the pointer. `rest` is the fill it leaves, `None` is clear.
    /// `key` names the element across frames.
    fn fade_hover(self, key: impl Hash, rest: Option<Hsla>, hover: Hsla) -> FadeFill;
    /// A list row's wash: the explorer's hover wash fading in and out under the pointer and the selected wash on
    /// the press, as a pill inset `ROW_WASH_INSET_Y`. `selected` rows keep the selected wash only (selected plus hover is selected).
    fn fade_row(self, key: impl Hash, ink: Hsla, light: bool, selected: bool) -> FadeFill;
}

impl FadeHover for Stateful<Div> {
    fn fade_hover(self, key: impl Hash, rest: Option<Hsla>, hover: Hsla) -> FadeFill {
        let mut hasher = DefaultHasher::new();
        key.hash(&mut hasher);
        let key = hasher.finish();
        let at = move |window: &Window| (window.window_handle().window_id(), key);
        let inner = self
            .on_hover(move |hovered, window, cx| {
                let key = at(window);
                if point_at(key, Some(*hovered), None) {
                    redraw(key, window, cx);
                }
            })
            .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                let key = at(window);
                if point_at(key, None, Some(true)) {
                    redraw(key, window, cx);
                }
            })
            // Let go in the capture phase, so a child that stops the click (a picking Command-click
            // on an explorer row) can't leave the press lit.
            .capture_any_mouse_up(move |event, window, cx| {
                let key = at(window);
                if event.button == MouseButton::Left && point_at(key, None, Some(false)) {
                    redraw(key, window, cx);
                }
            })
            .on_mouse_up_out(MouseButton::Left, move |_, window, cx| {
                let key = at(window);
                if point_at(key, None, Some(false)) {
                    redraw(key, window, cx);
                }
            });
        // With no rest fill, the hover color at nothing, so the fade starts from the same hue.
        FadeFill { inner: Some(inner), key, rest: rest.unwrap_or(hover.opacity(0.)), hover, press: hover, ink: None, covered: false, held: false, pill: false, wash: None }
    }

    fn fade_row(self, key: impl Hash, ink: Hsla, light: bool, selected: bool) -> FadeFill {
        let (hover, press) = if light { (ALPHA_HOVER_LIGHT, ALPHA_SELECTED_LIGHT) } else { (ALPHA_HOVER, ALPHA_SELECTED) };
        let mut row = self.relative().fade_hover(key, None, ink.opacity(hover)).press_fill(ink.opacity(press)).covered(selected);
        row.pill = true;
        row
    }
}

impl FadeFill {
    /// The fill a press holds, where it differs from the hover's.
    pub fn press_fill(mut self, press: Hsla) -> Self {
        self.press = press;
        self
    }

    /// While `covered`, the fade lets go of the pointer and falls to rest over `HOVER_OUT`, and stays there under it.
    pub fn covered(mut self, covered: bool) -> Self {
        self.covered = covered;
        self
    }

    /// While `held` (a menu that came from this button is open), the pressed look stays from the first frame that draws it; on `false` it fades to where the pointer is.
    pub fn held(mut self, held: bool) -> Self {
        self.held = held;
        self
    }

    /// The text color fades from `rest` to `lit` with the pointer.
    pub fn fade_ink(mut self, rest: Hsla, lit: Hsla) -> Self {
        self.ink = Some((rest, lit));
        self
    }
}

impl Styled for FadeFill {
    fn style(&mut self) -> &mut StyleRefinement {
        self.inner.as_mut().expect("built once").style()
    }
}

impl InteractiveElement for FadeFill {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.inner.as_mut().expect("built once").interactivity()
    }
}

impl StatefulInteractiveElement for FadeFill {}

impl ParentElement for FadeFill {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.inner.as_mut().expect("built once").extend(elements)
    }
}

impl IntoElement for FadeFill {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for FadeFill {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, AnyElement) {
        let key = (window.window_handle().window_id(), self.key);
        drew_in_view(key, window);
        let mut inner = self.inner.take().expect("laid out once");
        if self.covered {
            point_at(key, Some(false), Some(false));
        }
        match (self.held, HELD.with_borrow(|held| held.contains(&key))) {
            (true, false) => {
                // The press stays lit from this frame: a menu that blocks the main thread would freeze a fade half way.
                HELD.with_borrow_mut(|held| held.insert(key));
                FILLS.with_borrow_mut(|fills| fills.insert(key, Fill { hovered: true, pressed: true, from: 2., since: drawn_now(key.0) }));
            }
            (false, true) => {
                // Let go: fades to where the pointer is (over it, or away), from the pressed look.
                HELD.with_borrow_mut(|held| held.remove(&key));
                let over = BOUNDS.with_borrow(|all| all.get(&key).is_some_and(|at| at.contains(&window.mouse_position())));
                point_at(key, Some(over), Some(false));
            }
            // A leave heard while the menu is open must not let go of the press.
            (true, true) => {
                point_at(key, Some(true), Some(true));
            }
            (false, false) => {}
        }
        let level = level(key, window);
        self.wash = None;
        if level > 0. {
            let fill = if level <= 1. { mix(self.rest, self.hover, level) } else { mix(self.hover, self.press, level - 1.) };
            if self.pill {
                // Painted under the row's content in `paint`, like the explorer's wash.
                self.wash = Some(fill);
            } else {
                inner = inner.bg(fill);
            }
        }
        if let Some((rest, lit)) = self.ink {
            inner = inner.text_color(mix(rest, lit, level.min(1.)));
        }
        let mut element = inner.into_any_element();
        (element.request_layout(window, cx), element)
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, element: &mut AnyElement, window: &mut Window, cx: &mut App) {
        element.prepaint(window, cx);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, bounds: Bounds<Pixels>, element: &mut AnyElement, _: &mut (), window: &mut Window, cx: &mut App) {
        let key = (window.window_handle().window_id(), self.key);
        // Only a fill that is being pointed at keeps its place, so `level` can tell a hover that outlived the element.
        if FILLS.with_borrow(|fills| fills.contains_key(&key)) {
            BOUNDS.with_borrow_mut(|all| all.insert(key, bounds));
        }
        if let Some(fill) = self.wash {
            let inset = ROW_WASH_INSET_Y;
            let pill = Bounds::new(point(bounds.origin.x, bounds.origin.y + inset), size(bounds.size.width, bounds.size.height - inset * 2.));
            window.paint_quad(gpui::fill(pill, fill).corner_radii(Corners::all(RADIUS_ROW)));
        }
        element.paint(window, cx);
    }
}

/// An element built each frame from how far the pointer is over the element tracked under `key`
/// (by `fade_hover`): 0 at rest to 1 under it. It lays out as what `build` returns.
pub struct ByHover {
    key: u64,
    /// Set by `by_lit`: the owner says each frame whether the element is lit, instead of `fade_hover` hearing the pointer.
    lit: Option<bool>,
    build: Option<Box<dyn FnOnce(f32) -> AnyElement>>,
}

pub fn by_hover<E: IntoElement>(key: impl Hash, build: impl FnOnce(f32) -> E + 'static) -> ByHover {
    let mut hasher = DefaultHasher::new();
    key.hash(&mut hasher);
    ByHover { key: hasher.finish(), lit: None, build: Some(Box::new(move |level| build(level).into_any_element())) }
}

/// Like `by_hover`, for an element whose owner knows whether it is `lit` (under the pointer or held):
/// the level eases up over `HOVER_IN` and down over `HOVER_OUT`, from wherever it is drawn.
pub fn by_lit<E: IntoElement>(key: impl Hash, lit: bool, build: impl FnOnce(f32) -> E + 'static) -> ByHover {
    ByHover { lit: Some(lit), ..by_hover(key, build) }
}

impl IntoElement for ByHover {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for ByHover {
    type RequestLayoutState = AnyElement;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, AnyElement) {
        let key = (window.window_handle().window_id(), self.key);
        drew_in_view(key, window);
        if let Some(lit) = self.lit {
            point_at(key, Some(lit), None);
        }
        let level = level(key, window).min(1.);
        let mut element = (self.build.take().expect("laid out once"))(level);
        (element.request_layout(window, cx), element)
    }

    fn prepaint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, element: &mut AnyElement, window: &mut Window, cx: &mut App) {
        element.prepaint(window, cx);
    }

    fn paint(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, _: Bounds<Pixels>, element: &mut AnyElement, _: &mut (), window: &mut Window, cx: &mut App) {
        element.paint(window, cx);
    }
}

/// The colors of an icon button: its glyph at rest and under the pointer, and the ink its wash is
/// drawn in (at `ALPHA_BUTTON_HOVER` under the pointer, `ALPHA_BUTTON_PRESSED` held).
#[derive(Clone, Copy)]
pub struct Ink {
    pub rest: Hsla,
    pub lit: Hsla,
    pub wash: Hsla,
}

/// The one icon button: the symbol `symbol` at `glyph` points in a `slot` square, its wash and
/// glyph fading in and out with the pointer and on to the press. The caller adds the click, the
/// tooltip and, where it sits over a window-move hitbox, `occlude`.
pub fn icon_button(id: impl Into<ElementId>, symbol: &'static str, slot: Pixels, glyph: f32, ink: Ink) -> FadeFill {
    icon_button_weighted(id, symbol, slot, glyph, symbols::REGULAR, ink)
}

/// [`icon_button`] with the symbol drawn at an `NSFont` weight ([`symbols::SEMIBOLD`]).
pub fn icon_button_weighted(id: impl Into<ElementId>, symbol: &'static str, slot: Pixels, glyph: f32, weight: f64, ink: Ink) -> FadeFill {
    let id: ElementId = id.into();
    div()
        .id(id.clone())
        .size(slot)
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(RADIUS_BUTTON)
        .fade_hover(id, None, ink.wash.opacity(ALPHA_BUTTON_HOVER))
        .press_fill(ink.wash.opacity(ALPHA_BUTTON_PRESSED))
        .fade_ink(ink.rest, ink.lit)
        .child(symbols::icon_weighted(symbol, glyph, weight))
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use gpui_kit::{EntityId, WindowId};

    use super::{FILLS, Fill, VIEWS, forget};

    #[test]
    fn forget_drops_only_that_windows_fades_and_views() {
        let (gone, kept) = (WindowId::from(1), WindowId::from(2));
        let fill = Fill { hovered: true, pressed: false, from: 0., since: Instant::now() };
        FILLS.with_borrow_mut(|all| {
            all.clear();
            all.extend([((gone, 7), fill), ((kept, 7), fill)]);
        });
        VIEWS.with_borrow_mut(|all| {
            all.clear();
            all.extend([((gone, 7), EntityId::from(1u64)), ((kept, 7), EntityId::from(1u64))]);
        });
        forget(gone);
        FILLS.with_borrow(|all| assert_eq!(all.keys().copied().collect::<Vec<_>>(), [(kept, 7)]));
        VIEWS.with_borrow(|all| assert_eq!(all.keys().copied().collect::<Vec<_>>(), [(kept, 7)]));
    }
}
