//! The caret as a layer of its own (gpui-editor#grwg).
//!
//! The text element records where the carets are in its prepaint (`record`)
//! and paints none. A small view drawn over the input paints them from that
//! record: the glide and the eased blink fade run there, the blink timer
//! notifies that view and not the input, and the frames a fade or glide asks
//! for are the layer's. A blink never rebuilds the input.
//!
//! The record is shared, not owned by the view, so the layer paints this
//! frame's geometry: the input's prepaint is before the layer's paint in the
//! same frame, whichever view asked for the frame.

use std::{cell::RefCell, rc::Rc};

use gpui::{
    App, Bounds, ContentMask, Context, Element, ElementId, Entity, EntityId, Font, GlobalElementId,
    Hsla, InspectorElementId, IntoElement, LayoutId, Pixels, Point, SharedString, Style,
    Subscription, TextAlign, TextRun, Window, point, relative,
};

use crate::input::system_look;

use super::{
    blink_cursor::{BlinkCursor, CaretGlide, Instant, ease_out},
    element::{CursorRenderInfo, paint_band, paint_carets, paint_hollow_carets},
};

/// The current-line band (gpui-editor#g7fw), recorded with the carets so the layer paints it on
/// the caret's clock. Points are window coordinates.
#[derive(Clone, PartialEq)]
pub(super) struct Band {
    pub(super) color: Hsla,
    /// The band's left edge (the gutter's) and its width, to the right edge of the input.
    pub(super) left: Pixels,
    pub(super) width: Pixels,
    /// The line's top in content space (the scroll's y taken off), and the buffer row it is,
    /// which tells a move to another line from a shift inside one.
    pub(super) top: Pixels,
    pub(super) row: usize,
    pub(super) number: Number,
    pub(super) height: Pixels,
    /// Where the band may show: the input's mask, gutter included.
    pub(super) clip: Bounds<Pixels>,
}

/// The current line's number in the gutter, bright (gpui-editor#dgar). The editor skips this
/// figure; the layer paints it once, its color moving from `dim` to `color` with the band's
/// glide.
#[derive(Clone, PartialEq)]
pub(super) struct Number {
    pub(super) text: SharedString,
    pub(super) font: Font,
    pub(super) color: Hsla,
    /// The figure's color when it isn't the current line's.
    pub(super) dim: Hsla,
    /// The column's right edge (window x), which the figures sit flush against.
    pub(super) right: Pixels,
    /// Down from the line's top to where the number's line is drawn.
    pub(super) dy: Pixels,
    pub(super) line_height: Pixels,
}

/// Where the bright number stands in its crossfade between lines, on the band's glide.
#[derive(Default)]
struct NumberFade {
    /// The line lit now: its buffer row.
    row: Option<usize>,
    /// How far the band had to go when this line was lit (points), the lit line's weight then,
    /// and how far the band has come since (0 to 1).
    reach: f32,
    base: f32,
    progress: f32,
    /// The lit line's weight now: 0 is the dim figure, 1 the bright one.
    weight: f32,
    /// The lines going dark, over the editor's dim copies: number, content top, and the alpha
    /// each had when the band was last at `progress` 0.
    out: Vec<(Number, Pixels, f32)>,
    /// The number lit last frame and its content top, which `out` takes when the row changes.
    last: Option<(Number, Pixels)>,
}

impl NumberFade {
    fn reset(&mut self) {
        *self = Self::default();
    }

    /// Notes the band's line, drawn at `at` (content y), moving or not. A row change in the
    /// middle of a glide keeps the numbers partly faded where they are: the old lit one joins
    /// `out` at its weight, those already going dark are scaled to what they showed, and a line
    /// lit again starts from what it showed.
    fn step(&mut self, band: &Band, at: Pixels, moving: bool) {
        let dist = f32::from((band.top - at).abs());
        if self.row != Some(band.row) {
            let mut base = 0.;
            if self.row.is_some() && moving && dist > 0.5 {
                let (weight, progress) = (self.weight, self.progress);
                for entry in &mut self.out {
                    entry.2 *= 1. - progress;
                }
                self.out.retain(|entry| entry.2 > 0.01);
                if let Some((number, top)) = self.last.take() {
                    self.out.push((number, top, weight));
                }
                if let Some(at) = self.out.iter().position(|entry| entry.1 == band.top) {
                    base = self.out.remove(at).2;
                }
            } else {
                self.out.clear();
            }
            self.row = Some(band.row);
            self.reach = dist;
            self.base = base;
        }
        if moving && self.reach > 0.5 {
            self.progress = (1. - dist / self.reach).clamp(0., 1.);
            self.weight = self.base + (1. - self.base) * self.progress;
        } else {
            self.out.clear();
            self.progress = 1.;
            self.weight = 1.;
        }
        self.last = Some((band.number.clone(), band.top));
    }
}

fn paint_number(
    number: &Number,
    top: Pixels,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    let runs = [TextRun {
        len: number.text.len(),
        font: number.font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }];
    let line = window.text_system().shape_line(
        number.text.clone(),
        system_look::GUTTER_SIZE,
        &runs,
        None,
    );
    _ = line.paint(
        point(number.right - line.width, top + number.dy),
        number.line_height,
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

struct Spot {
    /// The input is focused and shows its caret.
    visible: bool,
    /// The input is not focused, in a window that's in front: its caret shows
    /// as a still outline.
    hollow: bool,
    infos: Vec<CursorRenderInfo>,
    scroll: Point<Pixels>,
    color: Hsla,
    /// Where the carets may show: the input's mask, right of the gutter.
    clip: Bounds<Pixels>,
    /// The current-line band, painted under the caret from the record.
    band: Option<Band>,
    glide: CaretGlide,
    band_glide: CaretGlide,
    number_fade: NumberFade,
    dim: DimFade,
    /// The text revision last recorded, and whether the last record came with an edit.
    revision: u64,
    typing: bool,
    fade: FocusFade,
    /// The layer has been told to draw and hasn't yet, so a change to the
    /// record needs no frame of its own.
    pending: bool,
    layer: Option<EntityId>,
    /// The find bar's bounds, over the input.
    cover: Option<Bounds<Pixels>>,
    /// The carets outside `cover`, kept to paint without a fresh allocation each frame.
    shown: Vec<CursorRenderInfo>,
}

/// What an input and its caret layer share. Get it from
/// `InputBaseState::enable_caret_layer`.
#[derive(Clone)]
pub struct CaretLayerHandle {
    spot: Rc<RefCell<Spot>>,
    blink: Entity<BlinkCursor>,
}

impl CaretLayerHandle {
    pub(super) fn new(blink: Entity<BlinkCursor>) -> Self {
        Self {
            spot: Rc::new(RefCell::new(Spot {
                visible: false,
                hollow: false,
                infos: Vec::new(),
                scroll: Point::default(),
                color: Hsla::default(),
                clip: Bounds::default(),
                band: None,
                glide: CaretGlide::default(),
                band_glide: CaretGlide::default(),
                number_fade: NumberFade::default(),
                dim: DimFade { lit: 1., from: 1., began: None, to: 1., shown: Shown::Nothing },
                revision: 0,
                typing: false,
                fade: FocusFade { shown: Shown::Nothing, from: (0., 0.), drawn: (0., 0.), began: None },
                pending: false,
                layer: None,
                cover: None,
                shown: Vec::new(),
            })),
            blink,
        }
    }

    /// Makes `layer` the view this record wakes when the caret moves with no
    /// other notification to ride on.
    pub fn bind(&self, layer: EntityId) {
        let mut spot = self.spot.borrow_mut();
        spot.layer = Some(layer);
        spot.pending = false;
    }

    /// Lets go of `layer`, if it is still the one bound.
    pub fn unbind(&self, layer: EntityId) {
        let mut spot = self.spot.borrow_mut();
        if spot.layer == Some(layer) {
            spot.layer = None;
        }
    }

    /// Whether a layer view is bound to paint this record now.
    pub(super) fn bound(&self) -> bool {
        self.spot.borrow().layer.is_some()
    }

    /// A blur forgets the glide's spot, so the next focus lands instead of gliding.
    pub(super) fn forget(&self) {
        let mut spot = self.spot.borrow_mut();
        spot.glide.clear();
        spot.band_glide.clear();
        spot.number_fade.reset();
    }

    /// Calls `on_change` on each blink flip that could change a pixel, after noting the layer
    /// is to draw. Only a shown bar with a collapsed caret takes the blink (the band, the
    /// hollow box and the number don't), so a selection at rest, a hollow box and an input
    /// that isn't drawn wake nothing; a selection collapsing later notifies through the input.
    pub fn observe_blink<V: 'static>(
        &self,
        cx: &mut Context<V>,
        on_change: impl Fn(&mut V, &mut Context<V>) + 'static,
    ) -> Subscription {
        let spot = self.spot.clone();
        cx.observe(&self.blink.clone(), move |this, _, cx| {
            let mut record = spot.borrow_mut();
            if !(record.visible && record.infos.iter().any(|info| info.collapsed)) {
                return;
            }
            record.pending = true;
            drop(record);
            on_change(this, cx);
        })
    }

    /// Calls `on_change` each time `entity` (the input) notifies, after noting
    /// the layer is to draw: an edit, a scroll or a selection moves the caret
    /// in the frame the input rebuilds, and the layer draws in that frame.
    pub fn observe<V: 'static, T: 'static>(
        &self,
        entity: &Entity<T>,
        cx: &mut Context<V>,
        on_change: impl Fn(&mut V, &mut Context<V>) + 'static,
    ) -> Subscription {
        let spot = self.spot.clone();
        cx.observe(entity, move |this, _, cx| {
            spot.borrow_mut().pending = true;
            on_change(this, cx);
        })
    }

    /// The element that paints the carets, to fill the layer's bounds.
    pub fn element(&self) -> CaretElement {
        CaretElement(self.clone())
    }

    /// Where the find bar floats over the input, so carets under it stay unpainted; `None` when
    /// no bar draws.
    pub fn set_cover(&self, cover: Option<Bounds<Pixels>>) {
        self.spot.borrow_mut().cover = cover;
    }

    /// Where the find bar floats over the input, as last set by [`Self::set_cover`].
    pub fn cover(&self) -> Option<Bounds<Pixels>> {
        self.spot.borrow().cover
    }

    /// The input isn't drawn this frame (its row scrolled or folded out of a list), so it
    /// recorded nothing: the layer paints no caret until the input records again.
    pub fn hide(&self) {
        let mut spot = self.spot.borrow_mut();
        spot.visible = false;
        spot.hollow = false;
        spot.infos.clear();
    }

    /// Called from the input's prepaint with this frame's carets. A change
    /// that no notification brought the layer along for wakes it on the next
    /// frame. `springs` lets a keystroke's move spring; without it every move
    /// glides.
    pub(super) fn record(
        &self,
        visible: bool,
        hollow: bool,
        infos: &[CursorRenderInfo],
        scroll: Point<Pixels>,
        color: Hsla,
        clip: Bounds<Pixels>,
        band: Option<Band>,
        revision: u64,
        springs: bool,
        window: &Window,
    ) {
        let mut spot = self.spot.borrow_mut();
        // An edit moved the caret this frame: a keystroke, which springs.
        let typing = springs && revision != spot.revision;
        spot.revision = revision;
        spot.typing = typing;
        if spot.visible == visible
            && spot.hollow == hollow
            && spot.scroll == scroll
            && spot.color == color
            && spot.clip == clip
            && spot.band == band
            && spot.infos == infos
        {
            return;
        }
        spot.visible = visible;
        spot.hollow = hollow;
        spot.scroll = scroll;
        spot.color = color;
        spot.clip = clip;
        spot.infos.clear();
        spot.infos.extend_from_slice(infos);
        spot.band = band;
        if !spot.pending
            && let Some(layer) = spot.layer
        {
            spot.pending = true;
            window.on_next_frame(move |_, cx| cx.notify(layer));
        }
    }
}

/// How long the hollow box and the bar crossfade when a pane gains or loses the keyboard
/// (`FADE_QUICK`, motion.md 'Caret focus move'). The blink starts after it: its first half
/// is 0.53 s long.
const FOCUS_FADE: f32 = 0.15;

/// What the layer shows for the caret.
#[derive(Clone, Copy, PartialEq)]
enum Shown {
    /// The focused bar.
    Bar,
    /// The still box of an editor without the keyboard.
    Hollow,
    /// Nothing: the window isn't in front, or the input is off.
    Nothing,
}

/// The hollow box and the bar easing into each other over [`FOCUS_FADE`]. Going to or from
/// `Shown::Nothing` is at once.
struct FocusFade {
    shown: Shown,
    /// What was drawn, `(box, bar)`, when the running fade began and at the last frame.
    from: (f32, f32),
    drawn: (f32, f32),
    began: Option<Instant>,
}

impl FocusFade {
    /// The `(box, bar)` alphas for `shown` at `now`, the bar's before the blink's, and whether
    /// the fade is still running. `blink` is the blink's opacity, which only the bar at rest takes.
    fn step(&mut self, shown: Shown, blink: f32, now: Instant) -> (f32, f32, bool) {
        if shown != self.shown {
            let animate = shown != Shown::Nothing && self.shown != Shown::Nothing;
            self.began = animate.then_some(now);
            self.from = self.drawn;
            self.shown = shown;
        }
        let to = match shown {
            Shown::Bar => (0., 1.),
            Shown::Hollow => (1., 0.),
            Shown::Nothing => (0., 0.),
        };
        let mut running = false;
        let (box_alpha, mut bar_alpha) = match self.began {
            Some(began) => {
                let t = now.saturating_duration_since(began).as_secs_f32() / FOCUS_FADE;
                if t >= 1. {
                    self.began = None;
                    to
                } else {
                    running = true;
                    let e = ease_out(t);
                    (
                        self.from.0 + (to.0 - self.from.0) * e,
                        self.from.1 + (to.1 - self.from.1) * e,
                    )
                }
            }
            None => to,
        };
        if shown == Shown::Bar {
            bar_alpha *= blink;
        }
        self.drawn = (box_alpha, bar_alpha);
        (box_alpha, bar_alpha, running)
    }
}

/// The current-line band's strength in the pane that doesn't hold the keyboard, while the
/// window is in front (gpui-editor#q8k2); its bright number goes back to the plain figure.
const DIM_BAND: f32 = 0.5;

/// How long the unfocused pane takes to dim and to light again: the app's `HOVER_OUT` and
/// `HOVER_IN`, the fades every wash eases on.
const DIM_OUT: f32 = 0.2;
const DIM_IN: f32 = 0.12;

/// How lit the band and the bright number are: 1 in the focused pane and in a window that isn't
/// in front, 0 in an unfocused pane of the key window, easing between on a focus change.
/// Going to or from `Shown::Nothing` is at once. Asks for no frame at rest.
struct DimFade {
    lit: f32,
    from: f32,
    began: Option<Instant>,
    /// The target the running ease is heading for.
    to: f32,
    /// The `Shown` last stepped for.
    shown: Shown,
}

impl DimFade {
    /// The lit weight for `shown` at `now`, and whether the ease is still running.
    fn step(&mut self, shown: Shown, now: Instant) -> (f32, bool) {
        let to = if shown == Shown::Hollow { 0. } else { 1. };
        if shown != self.shown {
            // Like `FocusFade`: only a move between Bar and Hollow eases; from or to Nothing
            // (launch, a window coming back in front) it lands.
            let animate = shown != Shown::Nothing && self.shown != Shown::Nothing;
            self.shown = shown;
            self.from = self.lit;
            self.to = to;
            self.began = animate.then_some(now);
            if !animate {
                self.lit = to;
            }
        }
        let Some(began) = self.began else {
            self.lit = to;
            return (to, false);
        };
        let span = if to < self.from { DIM_OUT } else { DIM_IN };
        let t = now.saturating_duration_since(began).as_secs_f32() / span;
        if t >= 1. {
            self.began = None;
            self.lit = to;
            return (to, false);
        }
        self.lit = self.from + (to - self.from) * ease_out(t);
        (self.lit, true)
    }
}

/// Paints the recorded carets, clipped to where the input shows them.
pub struct CaretElement(CaretLayerHandle);

impl IntoElement for CaretElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for CaretElement {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let now = crate::motion::frame_time(window.window_handle().window_id());
        let blink = self.0.blink.read(cx).alpha(now);
        let mut spot = self.0.spot.borrow_mut();
        spot.pending = false;
        let spot = &mut *spot;
        // Carets under the find bar stay unpainted: the bar floats over the text, the layer over both.
        let infos: &[CursorRenderInfo] = match spot.cover {
            Some(cover) => {
                let scroll = spot.scroll;
                spot.shown.clear();
                spot.shown.extend(spot.infos.iter().filter(|info| {
                    // Where the bar and the box are painted at rest: `x` holds the scroll already.
                    let at = point(info.bounds.origin.x, info.bounds.origin.y + scroll.y);
                    !cover.intersects(&Bounds::new(at, info.bounds.size))
                }).cloned());
                &spot.shown
            }
            None => &spot.infos,
        };
        let shown = if spot.visible {
            Shown::Bar
        } else if spot.hollow {
            Shown::Hollow
        } else {
            Shown::Nothing
        };
        let (box_alpha, bar_alpha, crossfading) = spot.fade.step(shown, blink.0, now);
        if shown != Shown::Bar && (spot.glide.tracking() || spot.band_glide.tracking()) {
            // A blur forgets the spot, so the next focus lands instead of gliding.
            spot.glide.clear();
            spot.band_glide.clear();
            spot.number_fade.reset();
        }
        let (dim, dimming) = spot.dim.step(shown, now);
        let mut band_moving = false;
        if let Some(band) = &spot.band {
            // The band is on the caret's clock: its line glides or springs with the bar while
            // it is shown (a move inside one line only shifts it), and stands still otherwise.
            let collapsed = spot.infos.iter().any(|info| info.is_active && info.collapsed);
            let mut top = band.top;
            let mut gliding = false;
            if collapsed && shown == Shown::Bar {
                let (at, moving) = spot.band_glide.step(
                    point(-spot.scroll.x, band.top),
                    band.row,
                    spot.scroll,
                    now,
                    spot.typing,
                    window.scale_factor(),
                );
                top = at.y;
                gliding = moving;
            }
            band_moving = gliding;
            // The bright number rides the band's glide: the new line's comes up and the old
            // line's goes down as the band crosses between them.
            spot.number_fade.step(band, top, gliding);
            let fade = &spot.number_fade;
            let scroll_y = spot.scroll.y;
            window.with_content_mask(Some(ContentMask { bounds: band.clip }), |window| {
                if collapsed {
                    paint_band(band, top + scroll_y, DIM_BAND + (1. - DIM_BAND) * dim, window);
                }
                for (number, out_top, alpha) in &fade.out {
                    let alpha = alpha * (1. - fade.progress) * dim;
                    paint_number(number, *out_top + scroll_y, number.color.opacity(alpha), window, cx);
                }
                // The editor skips this figure: it is painted once, from dim to bright.
                let n = &band.number;
                let w = fade.weight * dim;
                let color = Hsla {
                    l: n.dim.l + (n.color.l - n.dim.l) * w,
                    a: n.dim.a + (n.color.a - n.dim.a) * w,
                    ..n.color
                };
                paint_number(n, band.top + scroll_y, color, window, cx);
            });
        } else {
            spot.number_fade.reset();
        }
        if shown == Shown::Nothing {
            return;
        }
        let more = window.with_content_mask(Some(ContentMask { bounds: spot.clip }), |window| {
            if box_alpha > 0. {
                paint_hollow_carets(infos, spot.scroll, spot.color, box_alpha, window);
            }
            if shown == Shown::Bar {
                paint_carets(
                    infos,
                    spot.scroll,
                    spot.color,
                    (bar_alpha, blink.1),
                    &mut spot.glide,
                    now,
                    spot.typing,
                    window,
                )
            } else if bar_alpha > 0. {
                // The bar fading out where it stood: no glide, no blink.
                paint_carets(
                    infos,
                    spot.scroll,
                    spot.color,
                    (bar_alpha, false),
                    &mut CaretGlide::default(),
                    now,
                    false,
                    window,
                )
            } else {
                false
            }
        });
        if more || crossfading || band_moving || dimming {
            window.request_animation_frame();
        }
    }
}
