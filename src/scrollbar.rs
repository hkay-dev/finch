//! The Mac scrollbar and faded list edges, drawn by one overlay element per scrolling view.
//!
//! GPUI Kit's own `Scrollbar` shows on any offset change, so a reveal or a follow would show it,
//! and its knob is a square 6 pt with a 2 s hold. This one is hidden until the user scrolls the
//! view by hand: a wheel or trackpad event over the view (seen in the capture phase, before the
//! list handles and stops it) or a grab of the knob stamps `touched`. It holds `SCROLL_IDLE`,
//! fades out over `SCROLL_FADE_OUT` on smoothstep. The hold asks for no frames: one wake at its end
//! notifies the view, and frames run only for the fade and the ease between knob widths; hidden, it
//! draws nothing and asks for nothing. A touch while the knob is under full eases it up from the
//! level drawn over `HOVER_IN`, starting at the next painted frame. The pointer on a shown knob holds
//! it full (a hidden knob has no hitbox, so hover never reveals it), and leaving restarts the hold.
//!
//! The edge fade takes its strength from how far the list still scrolls that way, so it keeps no
//! state and runs no frames. The overlay paints it as a ramp of the ground the list sits on.
//!
//! The editor's scrollbar is drawn from a [`Feed`] by the caret layer's view (`editor_overlay`), not
//! inside the editor, so the knob's fade and ease frames rebuild that view alone (gpui-editor#1vmr).

use std::{cell::{Cell, RefCell}, ops::Range, rc::Rc, sync::{atomic::{AtomicU64, Ordering}, Arc}, time::{Duration, Instant}};

use gpui_kit::base::input::EditorScrollbarContext;
use gpui_kit::base::{ScrollbarAxis, ScrollbarHandle};
use gpui_kit::component::input::{BufferPoint, EditorState, Rope, RopeExt as _};
use gpui_kit::*;
use gpui_kit::component::ActiveTheme as _;
use crate::motion::{Fade, ScrollGlide};
use crate::tokens::{
    HAIRLINE_ALPHA, HOVER_IN, KNOB_ALPHA, KNOB_ALPHA_HOVER, LIST_EDGE_FADE, MARK_CARET, MARK_MATCH_MIN, MARK_RADIUS, TRACK_ALPHA, SCROLL_FADE_OUT, SCROLL_IDLE, SCROLL_INSET, SCROLL_KNOB, SCROLL_KNOB_HOVER, SCROLL_KNOB_MIN, SCROLL_PAGE_OVERLAP, SCROLL_TRACK};

/// A fade ramp is this many linear bands approximating smoothstep, so no band edge shows.
const RAMP_BANDS: usize = 8;

/// Ranges a scan walks between checks for a newer request.
const SCAN_CHUNK: usize = 4096;

fn smoothstep(t: f32) -> f32 {
    let t = t.clamp(0., 1.);
    t * t * (3. - 2. * t)
}

/// One scrolling view's scrollbar state: when the hand last scrolled it, the knob's hover level and
/// the grab while it is dragged. Keep one per view and clone it into the overlay.
#[derive(Clone, Default)]
pub struct Scroller(Rc<RefCell<Inner>>);

#[derive(Default)]
struct Inner {
    touched: Option<Instant>,
    /// The level the knob eases up from, set by a touch and started at the next painted frame.
    reveal_from: Option<f32>,
    /// The ease up to 1 after a touch.
    reveal: Fade,
    /// The vertical and the horizontal knob, by [`Dir::ix`].
    axes: [AxisState; 2],
    /// The `touched` stamp a wake is already scheduled for.
    wake: Option<Instant>,
    /// The pointer is in the heading ticks' column: it holds the knob as a hover on it does.
    held: bool,
}

#[derive(Default)]
struct AxisState {
    hover: Fade,
    /// The pointer was on the knob's track at the last paint.
    inside: bool,
    /// Pointer's distance from the knob's start while it is dragged.
    grab: Option<f32>,
    /// Where the knob sat when last painted: track start, knob length, travel and scroll extent.
    knob: Option<Geometry>,
    /// A page the track's click is easing the view through.
    glide: ScrollGlide,
    /// The offset the page last set, to tell another mover's.
    last: Option<f32>,
    /// A page holds the scroll estimate (`start_drag`) until it lands.
    paging: bool,
}

/// Which way a knob runs.
#[derive(Clone, Copy, PartialEq)]
enum Dir {
    Vertical,
    Horizontal,
}

impl Dir {
    const ALL: [Dir; 2] = [Dir::Vertical, Dir::Horizontal];

    fn ix(self) -> usize {
        self as usize
    }

    fn other(self) -> Dir {
        match self {
            Dir::Vertical => Dir::Horizontal,
            Dir::Horizontal => Dir::Vertical,
        }
    }

    /// The view's length this way.
    fn extent(self, bounds: Bounds<Pixels>) -> f32 {
        match self {
            Dir::Vertical => f32::from(bounds.size.height),
            Dir::Horizontal => f32::from(bounds.size.width),
        }
    }

    /// Where the view starts this way.
    fn start(self, bounds: Bounds<Pixels>) -> f32 {
        match self {
            Dir::Vertical => f32::from(bounds.origin.y),
            Dir::Horizontal => f32::from(bounds.origin.x),
        }
    }

    /// A point's place this way.
    fn along(self, point: Point<Pixels>) -> f32 {
        match self {
            Dir::Vertical => f32::from(point.y),
            Dir::Horizontal => f32::from(point.x),
        }
    }

    /// A strip of the view's far edge: `main_len` long from `main_start` this way, `cross_len` thick.
    fn oriented(self, bounds: Bounds<Pixels>, main_start: f32, main_len: f32, cross_len: f32) -> Bounds<Pixels> {
        self.oriented_snapped(bounds, main_start, main_len, cross_len, cross_len, &|v| v)
    }

    /// As [`Dir::oriented`], `cross_len` thick and `from_far` in from the far edge, snapped across.
    fn oriented_snapped(self, bounds: Bounds<Pixels>, main_start: f32, main_len: f32, from_far: f32, cross_len: f32, snap: &dyn Fn(f32) -> f32) -> Bounds<Pixels> {
        let main = self.start(bounds) + main_start;
        match self {
            Dir::Vertical => Bounds::new(point(px(snap(f32::from(bounds.right()) - from_far)), px(main)), size(px(cross_len), px(main_len))),
            Dir::Horizontal => Bounds::new(point(px(main), px(snap(f32::from(bounds.bottom()) - from_far))), size(px(main_len), px(cross_len))),
        }
    }
}

#[derive(Clone, Copy)]
struct Geometry {
    top: f32,
    length: f32,
    travel: f32,
    max: f32,
    inset: f32,
}

impl Inner {
    /// How visible the knob is at `now`: the ease up while a touch, a hover or a grab holds it, then
    /// smoothstep down to 0 once the hold ends.
    fn visibility(&self, now: Instant) -> f32 {
        if let Some(from) = self.reveal_from {
            return from;
        }
        let Some(touched) = self.touched else { return 0. };
        if self.held || self.axes.iter().any(|axis| axis.grab.is_some() || axis.inside || axis.hover.level(now) > 0.) {
            return self.reveal.level(now);
        }
        let age = now.saturating_duration_since(touched).as_secs_f32();
        if age < SCROLL_IDLE {
            self.reveal.level(now)
        } else {
            1. - smoothstep((age - SCROLL_IDLE) / SCROLL_FADE_OUT)
        }
    }

    /// Restarts the hold. A knob drawn under 1 eases up from that level (the ease starts at the next
    /// painted frame, see [`Inner::begin`]); a turn-around never jumps to the end.
    fn touch(&mut self, now: Instant) {
        let rising = self.reveal_from.is_some() || self.touched.is_some_and(|touched| now.saturating_duration_since(touched).as_secs_f32() < SCROLL_IDLE && self.reveal.is_moving(now));
        if !rising {
            let drawn = self.visibility(now);
            if drawn < 1. {
                self.reveal_from = Some(drawn);
            } else {
                self.reveal = Fade::with(1., HOVER_IN);
            }
        }
        self.touched = Some(now);
    }

    /// Starts a waiting ease at the frame about to be drawn.
    fn begin(&mut self, now: Instant) {
        if let Some(from) = self.reveal_from.take() {
            self.reveal = Fade::with(from, HOVER_IN);
            self.reveal.aim(1., now);
        }
    }
}

impl Scroller {
    fn visibility(&self, now: Instant) -> f32 {
        self.0.borrow().visibility(now)
    }

    /// Stamps a hand touch at the frame clock, the time glides are sampled at (see `motion::drawn_now`).
    pub(crate) fn touch(&self, window: &Window) {
        self.0.borrow_mut().touch(crate::motion::drawn_now(window.window_handle().window_id()));
    }

    /// The pointer is in (or left) the heading ticks' column.
    pub(crate) fn hold(&self, on: bool) {
        self.0.borrow_mut().held = on;
    }

    /// A knob is being dragged.
    pub(crate) fn grabbing(&self) -> bool {
        self.0.borrow().axes.iter().any(|axis| axis.grab.is_some())
    }

    /// While the knob is fully shown and nothing is grabbed, schedules one wake at the end of the
    /// hold that notifies `view`, so the fade starts without frames in between. A newer touch moves
    /// the end of the hold, so the older wake finds `touched` changed and does nothing.
    fn wake_at_fade(&self, visible: f32, shown: bool, view: EntityId, cx: &mut App) {
        let mut inner = self.0.borrow_mut();
        let Some(touched) = inner.touched else { return };
        let now = Instant::now();
        if !shown || visible < 1. || inner.held || inner.axes.iter().any(|axis| axis.grab.is_some() || axis.inside || axis.hover.level(now) > 0.) || inner.wake == Some(touched) {
            return;
        }
        inner.wake = Some(touched);
        drop(inner);
        let wait = (touched + Duration::from_secs_f32(SCROLL_IDLE)).saturating_duration_since(Instant::now());
        let scroller = self.clone();
        cx.spawn(async move |cx| {
            cx.background_executor().timer(wait).await;
            cx.update(|cx| {
                let mut inner = scroller.0.borrow_mut();
                inner.wake = None;
                if inner.touched == Some(touched) {
                    drop(inner);
                    cx.notify(view);
                }
            });
        })
        .detach();
    }
}

/// What the editor hands its scrollbar layer: the geometry and Find's colors the editor recorded
/// while it prepainted this frame (`EditorState`'s scrollbar renderer calls [`Feed::record`]).
/// The layer draws from it in its own view, so the knob's fade frames never rebuild the editor
/// (gpui-editor#1vmr). The scroll offset is read live from the editor's handle when the layer
/// prepaints, after the editor has, so the knob never trails the text by a frame.
#[derive(Clone)]
pub struct Feed(Rc<FeedInner>);

struct FeedInner {
    context: RefCell<Option<EditorScrollbarContext>>,
    ink: Cell<Option<MarkInk>>,
    /// Weak: the feed lives on the editor's renderer, so a strong handle would be a cycle.
    editor: WeakEntity<EditorState>,
    /// The view that draws the editor, woken when a drag scrolls it.
    owner: EntityId,
}

impl Feed {
    pub fn new(editor: WeakEntity<EditorState>, owner: EntityId) -> Self {
        Self(Rc::new(FeedInner { context: RefCell::new(None), ink: Cell::new(None), editor, owner }))
    }

    /// Keeps this frame's geometry and colors for the layer to draw.
    pub fn record(&self, context: EditorScrollbarContext, ink: MarkInk) {
        *self.0.context.borrow_mut() = Some(context);
        self.0.ink.set(Some(ink));
    }

    pub fn ptr_eq(&self, other: &Feed) -> bool {
        Rc::ptr_eq(&self.0, &other.0)
    }

    /// The editor's viewport in window coordinates; `None` until it has drawn.
    fn bounds(&self) -> Option<Bounds<Pixels>> {
        self.0.context.borrow().as_ref().map(|context| context.viewport_bounds())
    }

    /// Soft wrap off: the editor scrolls sideways too.
    fn sideways(&self) -> bool {
        self.0.context.borrow().as_ref().is_some_and(|context| context.axis() == ScrollbarAxis::Both)
    }
}

impl ScrollbarHandle for Feed {
    fn viewport_bounds(&self) -> Bounds<Pixels> {
        self.bounds().unwrap_or_default()
    }

    fn offset(&self) -> Point<Pixels> {
        self.0.context.borrow().as_ref().map_or_else(Point::default, |context| context.offset())
    }

    fn set_offset(&self, offset: Point<Pixels>) {
        if let Some(context) = self.0.context.borrow().as_ref() {
            context.set_offset(offset);
        }
    }

    fn content_size(&self) -> Size<Pixels> {
        self.0.context.borrow().as_ref().map_or_else(Size::default, |context| context.content_size())
    }
}

/// One editor's scrollbar: its hand-scroll clock and its feed. Keep one per editor the pane draws.
#[derive(Clone)]
pub struct Bar {
    pub scroller: Scroller,
    pub feed: Feed,
}

/// How far a list still scrolls each way, for fading what nears its edges.
#[derive(Clone, Copy)]
pub struct Edge {
    /// Points scrolled past the top.
    pub scrolled: f32,
    /// The most it scrolls.
    pub max: f32,
    /// The visible height.
    pub height: f32,
}

impl Edge {
    pub fn of(handle: &dyn ScrollbarHandle, height: f32) -> Self {
        Self::along(handle, Dir::Vertical, height)
    }

    fn along(handle: &dyn ScrollbarHandle, dir: Dir, extent: f32) -> Self {
        let (content, view, offset) = match dir {
            Dir::Vertical => (handle.content_size().height, handle.viewport_bounds().size.height, handle.offset().y),
            Dir::Horizontal => (handle.content_size().width, handle.viewport_bounds().size.width, handle.offset().x),
        };
        let max = (f32::from(content) - f32::from(view)).max(0.);
        let scrolled = (-f32::from(offset)).clamp(0., max);
        Self { scrolled, max, height: extent }
    }

    /// How much of the fade each edge shows: 1 when the list still scrolls a full fade that way.
    fn strengths(&self) -> (f32, f32) {
        let fade = f32::from(LIST_EDGE_FADE);
        ((self.scrolled / fade).clamp(0., 1.), ((self.max - self.scrolled) / fade).clamp(0., 1.))
    }
}

/// Where a view's pieces sit, for the overlay.
pub struct Overlay {
    pub handle: Rc<dyn ScrollbarHandle>,
    pub scroller: Scroller,
    /// The solid ground the view sits on, to fade its edges into; `None` when the caller fades the
    /// content itself.
    pub ground: Option<Hsla>,
}

/// The hand-scroll sensor: the first child of the same container as the overlay. A wheel event over
/// the container stamps the view as touched. It is first so that its capture listener runs before
/// the list under it handles the wheel and stops it.
pub fn watch(scroller: &Scroller) -> impl IntoElement {
    let scroller = scroller.clone();
    canvas(
        |bounds, _, _| bounds,
        move |_, bounds, window, _| touch_on_wheel(&scroller, bounds, window),
    )
    .absolute()
    .inset_0()
}

/// A wheel or trackpad event inside `bounds` stamps `scroller` and wakes the view painting now.
fn touch_on_wheel(scroller: &Scroller, bounds: Bounds<Pixels>, window: &mut Window) {
    let view = window.current_view();
    let scroller = scroller.clone();
    window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
        if phase == DispatchPhase::Capture && bounds.contains(&event.position) {
            // The hand takes over from a page still gliding.
            scroller.0.borrow_mut().axes.iter_mut().for_each(|axis| axis.glide.cancel());
            scroller.touch(window);
            cx.notify(view);
        }
    });
}

/// The overlay: the last child of a `relative` container around the scrolling view. It covers the
/// container, listens for hand scrolls and paints the edge fade and the scrollbar.
pub fn overlay(overlay: Overlay) -> impl IntoElement {
    build(overlay, || false, None)
}

/// [`overlay`] for a view that also scrolls sideways (`sideways`): a knob along the bottom shows when it does.
pub fn overlay_sideways(overlay: Overlay, sideways: bool) -> impl IntoElement {
    build(overlay, move || sideways, None)
}

/// The editor's scrollbar, for a view of its own over the editor (the caret layer's). It sits
/// where the editor recorded its viewport, so it needs no container of its own: a knob that
/// scrolls sideways when soft wrap is off, Find's ticks on the vertical track, the wheel sensor
/// and the drag. A drag wakes the editor's view too, which draws the text it moved.
pub fn editor_overlay(bar: &Bar, marks: &Marks) -> impl IntoElement {
    let overlay = Overlay { handle: Rc::new(bar.feed.clone()), scroller: bar.scroller.clone(), ground: None };
    let feed = bar.feed.clone();
    build(overlay, move || feed.sideways(), Some((bar.feed.clone(), marks.clone())))
}

fn build(overlay: Overlay, horizontal: impl Fn() -> bool + 'static, seat: Option<(Feed, Marks)>) -> impl IntoElement {
    let Overlay { handle, scroller, ground } = overlay;
    let (pre, pre_handle) = (scroller.clone(), handle.clone());
    let placed = seat.as_ref().map(|(feed, _)| feed.clone());
    let synced = seat.as_ref().map(|(_, marks)| marks.clone());
    canvas(
        move |bounds, window, cx| {
            // An editor's scrollbar draws where the editor recorded its viewport, and nothing before it has drawn.
            let (bounds, cover) = match &placed {
                Some(feed) => {
                    let editor = feed.0.editor.upgrade();
                    // The ticks' rows are mapped here, in the view a finished scan wakes, so they never trail the matches.
                    if let (Some(editor), Some(marks)) = (&editor, &synced) {
                        marks.sync(editor, window.current_view(), cx);
                        crate::log_follow::sync(editor, window.current_view(), cx);
                        crate::heading_ticks::sync(editor, window.current_view(), cx);
                    }
                    (feed.bounds()?, editor.and_then(|editor| editor.read(cx).caret_cover()))
                }
                None => (bounds, None),
            };
            let horizontal = horizontal();
            // A page the track's click started eases the view along before the knob is placed, so the two never part.
            let clock = crate::motion::drawn_now(window.window_handle().window_id());
            for dir in Dir::ALL {
                let (stepped, moving, released) = {
                    let mut inner = pre.0.borrow_mut();
                    let axis = &mut inner.axes[dir.ix()];
                    // Anything else that moved the view (Find, Go to line, a reveal) owns it now: the page lets go.
                    if let Some(last) = axis.last {
                        let kept = pre_handle.offset();
                        let at = -f32::from(match dir {
                            Dir::Vertical => kept.y,
                            Dir::Horizontal => kept.x,
                        });
                        if (at - last).abs() > 0.5 {
                            axis.glide.cancel();
                            axis.last = None;
                        }
                    }
                    let stepped = axis.glide.step(clock);
                    axis.last = if axis.glide.is_moving() { stepped } else { None };
                    // The frame after the landing (or a cancel) lets the estimate go.
                    let release = axis.paging && stepped.is_none() && !axis.glide.is_moving();
                    if release {
                        axis.paging = false;
                    }
                    (stepped, axis.glide.is_moving(), release)
                };
                if released {
                    pre_handle.end_drag();
                }
                if let Some(to) = stepped {
                    // Applied after the draw, as a drag's move is (an event), so the editor takes it into its next frame.
                    let (handle, view, owner) = (pre_handle.clone(), window.current_view(), placed.as_ref().map(|feed| feed.0.owner));
                    window.defer(cx, move |_, cx| {
                        let kept = handle.offset();
                        handle.set_offset(match dir {
                            Dir::Vertical => point(kept.x, px(-to)),
                            Dir::Horizontal => point(px(-to), kept.y),
                        });
                        cx.notify(view);
                        if let Some(owner) = owner {
                            cx.notify(owner);
                        }
                    });
                }
                if moving {
                    window.request_animation_frame();
                }
            }
            let scale = window.scale_factor();
            let edges = Dir::ALL.map(|dir| (dir == Dir::Vertical || horizontal).then(|| Edge::along(pre_handle.as_ref(), dir, dir.extent(bounds))));
            let now = crate::motion::drawn_now(window.window_handle().window_id());
            pre.0.borrow_mut().begin(now);
            let shown = pre.visibility(now) > 0.;
            // Only a shown knob takes the pointer; the wheel still reaches the list under it.
            let hitboxes = Dir::ALL.map(|dir| {
                let edge = edges[dir.ix()].filter(|edge| edge.max > 0.5 && shown)?;
                let mut track = dir.oriented(bounds, 0., edge.height - corner(&edges, dir), f32::from(SCROLL_TRACK));
                // The find bar floats above the track's top, and this layer sits over it: leave its pointer alone.
                if let Some(cover) = cover.filter(|cover| dir == Dir::Vertical && cover.intersects(&track)) {
                    let below = (cover.bottom() - track.origin.y).clamp(px(0.), track.size.height);
                    track.origin.y += below;
                    track.size.height -= below;
                }
                Some(window.insert_hitbox(track, HitboxBehavior::BlockMouseExceptScroll))
            });
            Some((bounds, edges, hitboxes, scale, now))
        },
        move |_, state, window, cx| {
            let Some((bounds, edges, hitboxes, scale, now)) = state else { return };
            if let (Some(ground), Some(edge)) = (ground, edges[Dir::Vertical.ix()]) {
                paint_fade(bounds, edge, ground, window);
            }
            let snap = |v: f32| (v * scale).round() / scale;
            let ink = cx.theme().foreground;
            let scrolls = edges.iter().flatten().any(|edge| edge.max > 0.5);
            let mut animating = false;
            let mut hovers = [0.; 2];
            let visible = {
                let mut inner = scroller.0.borrow_mut();
                let (mut entered, mut left, mut now_inside) = (false, false, [false; 2]);
                for dir in Dir::ALL {
                    let hovered = hitboxes[dir.ix()].as_ref().is_some_and(|hitbox| hitbox.is_hovered(window));
                    let axis = &mut inner.axes[dir.ix()];
                    let target = if hovered || axis.grab.is_some() { 1. } else { 0. };
                    axis.hover.aim(target, now);
                    animating |= axis.hover.is_moving(now);
                    axis.knob = None;
                    hovers[dir.ix()] = axis.hover.level(now);
                    entered |= hovered && !axis.inside;
                    left |= !hovered && axis.inside;
                    now_inside[dir.ix()] = hovered;
                }
                // A hover holds the knob; arriving counts as a touch (a fading knob eases back up from where it is drawn) and leaving restarts the hold.
                if entered || left {
                    inner.touch(now);
                }
                for dir in Dir::ALL {
                    inner.axes[dir.ix()].inside = now_inside[dir.ix()];
                }
                inner.begin(now);
                let visible = inner.visibility(now);
                // The ease up and the fade out move the knob; the hold is still, and one wake at its end starts the fade.
                animating |= scrolls && visible < 1. && (visible > 0. || inner.reveal.is_moving(now));
                visible
            };

            // The marks sit under the knob and show whenever Find is open, not only while the knob does.
            if let (Some((feed, marks)), Some(edge)) = (&seat, edges[Dir::Vertical.ix()]) {
                if let Some(ink) = feed.0.ink.get() {
                    paint_marks(marks, &ink, bounds, edge, edge.height - corner(&edges, Dir::Vertical), window, cx);
                }
                if let Some(editor) = feed.0.editor.upgrade() {
                    // A log's flagged lines, in the same column as Find's ticks.
                    let inset = f32::from(SCROLL_INSET);
                    let start = editor.read(cx).caret_cover().map_or(inset, |cover| (f32::from(cover.bottom() - bounds.origin.y) + inset).max(inset));
                    crate::log_follow::paint_ticks(editor.entity_id(), bounds, edge.max + edge.height, edge.height - corner(&edges, Dir::Vertical), start, window, cx);
                    // A Markdown note's headings, in a column beside the track, in the knob's own fade.
                    crate::heading_ticks::paint(editor.entity_id(), bounds, edge.scrolled, edge.max + edge.height, edge.height - corner(&edges, Dir::Vertical), start, edge.max > 0.5, visible, window, cx);
                }
            }

            for dir in Dir::ALL {
                let Some(edge) = edges[dir.ix()].filter(|edge| edge.max > 0.5 && visible > 0.) else { continue };
                let hover = hovers[dir.ix()];
                let inset = f32::from(SCROLL_INSET);
                let view = edge.height;
                let span = view - corner(&edges, dir);
                let track_length = span - 2. * inset;
                let content = edge.max + view;
                let length = snap((track_length * view / content).max(f32::from(SCROLL_KNOB_MIN)).min(track_length));
                let travel = track_length - length;
                let top = snap(inset + travel * edge.scrolled / edge.max);
                let width = snap(f32::from(SCROLL_KNOB) + (f32::from(SCROLL_KNOB_HOVER) - f32::from(SCROLL_KNOB)) * hover);
                let track_width = f32::from(SCROLL_TRACK);

                if hover > 0. {
                    let track = dir.oriented(bounds, 0., span, track_width);
                    window.paint_quad(fill(track, ink.opacity(TRACK_ALPHA * hover * visible)));
                    let hairline = dir.oriented_snapped(bounds, 0., span, track_width, 1. / scale, &snap);
                    window.paint_quad(fill(hairline, ink.opacity(HAIRLINE_ALPHA * hover * visible)));
                }
                let knob = dir.oriented_snapped(bounds, top, length, inset + width, width, &snap);
                let alpha = (KNOB_ALPHA + (KNOB_ALPHA_HOVER - KNOB_ALPHA) * hover) * visible;
                window.paint_quad(fill(knob, ink.opacity(alpha)).corner_radii(Corners::all(px(width / 2.))));
                scroller.0.borrow_mut().axes[dir.ix()].knob = Some(Geometry { top, length, travel, max: edge.max, inset });
            }
            if animating {
                window.request_animation_frame();
            }
            scroller.wake_at_fade(visible, scrolls, window.current_view(), cx);

            if let Some((feed, _)) = &seat {
                // The editor's own hand-scroll sensor can't sit under this layer: the wheel is read here.
                touch_on_wheel(&scroller, bounds, window);
                crate::heading_ticks::listen(feed.0.editor.entity_id(), &scroller, window);
                listen(bounds, hitboxes, &scroller, &handle, Some(feed.0.owner), Some(feed.0.editor.clone()), window);
            } else {
                listen(bounds, hitboxes, &scroller, &handle, None, None, window);
            }
        },
    )
    .absolute()
    .inset_0()
}

/// The colors of Find's marks, read from the theme each frame so a theme change shows at once.
#[derive(Clone, Copy)]
pub struct MarkInk {
    /// A match's tick.
    pub find: Hsla,
    /// The current match's tick, drawn over the rest.
    pub current: Hsla,
    /// The caret line's tick, shown while there's no current match.
    pub caret: Hsla,
}

/// Find's marks on an editor's vertical track: a tick for each row with a match, a stronger one
/// for the current match (or, with no match, one at the caret's line), all below the find bar.
/// Keep one per editor and clone it into the overlay and the sync that feeds it.
///
/// Work comes in three steps, each redone only when its input changes and none per frame while
/// Find is closed. A scan on the background executor turns the matches into buffer spans, keyed
/// by the matches' identity and the text length so a stale result is dropped. The display map
/// can't leave the main thread, so [`Marks::sync`] maps the spans to display rows (soft wrap and
/// folds) when the spans or the layout change. Painting turns rows to track pixels, merging ticks
/// that touch, when the track's size changes, then draws the cached quads.
#[derive(Clone, Default)]
pub struct Marks(Rc<RefCell<MarkState>>);

#[derive(Default)]
struct MarkState {
    /// Find is open and the marks are live.
    on: bool,
    /// The matches the last scan was asked for, and the text length then.
    source: Option<(Arc<Vec<Range<usize>>>, usize)>,
    /// The newest scan requested; a scan holding an older number stops, and its result is dropped.
    scan: Arc<AtomicU64>,
    /// Where matches begin and end in the buffer, one span per line with matches (two matches on a line share one).
    spans: Vec<Span>,
    spans_rev: u64,
    /// Count of display rows, spans revision and line height the rows were mapped for.
    layout: Option<(usize, u64, u32)>,
    /// Inclusive runs of adjacent display rows holding matches, in order.
    rows: Vec<(u32, u32)>,
    rows_rev: u64,
    caret_row: Option<usize>,
    /// The current match's first and last display rows.
    current: Option<(usize, usize)>,
    /// The editor, to read where the find bar is at paint time: the bar records its bounds after
    /// the scrollbar's prepaint, so only paint sees this frame's.
    editor: Option<WeakEntity<EditorState>>,
    line_height: f32,
    /// What the pixel cache was built for: rows revision, caret row, current match rows, track start and length, content height, device pixel.
    pixels: Option<(u64, Option<usize>, Option<(usize, usize)>, u32, u32, u32, u32)>,
    /// Match ticks as top and bottom from the view's top, merged.
    quads: Vec<(f32, f32)>,
    /// The current match's tick, top and bottom.
    current_quad: Option<(f32, f32)>,
    /// The caret tick's top.
    caret_top: Option<f32>,
}

/// A run of matching lines: where its first match starts and its last ends, as (line, column).
#[derive(Clone, Copy, Debug, PartialEq)]
struct Span {
    first: (u32, u32),
    last: (u32, u32),
}

impl Marks {
    /// The editor stopped showing: drop the match list the last scan was asked for and the spans
    /// and rows built from it. The ticks already drawn stay (bounded by the track's height), so
    /// a tab that shows again keeps them until its new scan lands.
    pub fn release(&self) {
        let mut marks = self.0.borrow_mut();
        if marks.on {
            marks.scan.fetch_add(1, Ordering::Relaxed);
            marks.source = None;
            marks.spans = Vec::new();
            marks.rows = Vec::new();
        }
    }

    /// Brings the marks up to date with the editor `entity`; call it every frame the editor draws
    /// its scrollbar. Closed Find costs one check. A new set of matches starts a background scan
    /// and wakes `view` when it lands.
    pub fn sync(&self, entity: &Entity<EditorState>, view: EntityId, cx: &mut App) {
        {
            let mut marks = self.0.borrow_mut();
            if marks.editor.is_none() {
                marks.editor = Some(entity.downgrade());
            }
        }
        let Some((scan_id, text, ranges)) = self.update(entity.read(cx)) else { return };
        let this = self.clone();
        let latest = self.0.borrow().scan.clone();
        cx.spawn(async move |cx| {
            // A newer request has replaced this one before it began: skip the copy and the scan.
            if latest.load(Ordering::Relaxed) != scan_id {
                return;
            }
            // The matches are an `Arc`, so the background scan shares them with the editor: no copy per key.
            let check = latest.clone();
            let Some(spans) = cx.background_executor().spawn(async move { scan(&text, &ranges, scan_id, &check) }).await else {
                return;
            };
            cx.update(|cx| {
                let mut marks = this.0.borrow_mut();
                if marks.scan.load(Ordering::Relaxed) != scan_id {
                    return;
                }
                marks.spans = spans;
                marks.spans_rev += 1;
                drop(marks);
                cx.notify(view);
            });
        })
        .detach();
    }

    /// The main-thread part of [`Marks::sync`]; returns the scan to start when the matches or the text changed.
    fn update(&self, state: &EditorState) -> Option<(u64, Rope, Arc<Vec<Range<usize>>>)> {
        let session = state.search_session();
        let mut marks = self.0.borrow_mut();
        if !session.is_active() {
            if marks.on {
                marks.scan.fetch_add(1, Ordering::Relaxed);
                *marks = MarkState { scan: marks.scan.clone(), ..MarkState::default() };
            }
            return None;
        }
        marks.on = true;
        // The matches were let go and are being scanned again: the ticks keep what they show.
        if session.matcher.is_released() {
            return None;
        }
        let matches = session.matcher.matched_ranges();
        let text = state.text();
        let fresh = marks.source.as_ref().is_some_and(|(seen, len)| Arc::ptr_eq(seen, &matches) && *len == text.len());
        let request = (!fresh).then(|| {
            marks.source = Some((matches.clone(), text.len()));
            (marks.scan.fetch_add(1, Ordering::Relaxed) + 1, text.clone(), matches.clone())
        });

        let Some(line_height) = state.line_height() else { return request };
        let map = state.display_map();
        let layout = (map.display_row_count(), marks.spans_rev, f32::from(line_height).to_bits());
        if marks.layout != Some(layout) {
            marks.layout = Some(layout);
            marks.line_height = f32::from(line_height);
            let MarkState { spans, rows, .. } = &mut *marks;
            rows.clear();
            for span in spans.iter() {
                let at = |(line, column): (u32, u32)| map.buffer_pos_to_display_pos(BufferPoint::new(line as usize, column as usize)).row as u32;
                let first = at(span.first);
                let last = at(span.last).max(first);
                match rows.last_mut() {
                    Some(run) if first <= run.1 + 1 => run.1 = run.1.max(last),
                    _ => rows.push((first, last)),
                }
            }
            marks.rows_rev += 1;
        }
        let caret = text.offset_to_point(state.cursor());
        marks.caret_row = Some(map.buffer_pos_to_display_pos(BufferPoint::new(caret.row, caret.column)).row);
        marks.current = session.matcher.current().and_then(|ix| matches.get(ix)).map(|range| {
            let (start, end) = (text.offset_to_point(range.start), text.offset_to_point(range.end));
            let first = map.buffer_pos_to_display_pos(BufferPoint::new(start.row, start.column)).row;
            (first, map.buffer_pos_to_display_pos(BufferPoint::new(end.row, end.column)).row.max(first))
        });
        request
    }
}

/// The buffer spans of `ranges`, in order; ranges on the same line, or one running into the next,
/// share a span. Lines and columns saturate at `u32::MAX`.
fn scan(text: &Rope, ranges: &[Range<usize>], scan_id: u64, latest: &AtomicU64) -> Option<Vec<Span>> {
    let mut spans: Vec<Span> = Vec::new();
    let at = |x: usize| u32::try_from(x).unwrap_or(u32::MAX);
    for (ix, range) in ranges.iter().enumerate() {
        if ix % SCAN_CHUNK == 0 && latest.load(Ordering::Relaxed) != scan_id {
            return None;
        }
        let (start, end) = (text.offset_to_point(range.start), text.offset_to_point(range.end));
        let first = (at(start.row), at(start.column));
        let last = (at(end.row), at(end.column));
        match spans.last_mut() {
            Some(prev) if first.0 <= prev.last.0 => prev.last = last,
            _ => spans.push(Span { first, last }),
        }
    }
    Some(spans)
}

impl MarkState {
    /// Turns rows to track pixels on a track `track` long from `start`: each run at least
    /// `MARK_MATCH_MIN` tall, centered on its rows and kept inside the track; ticks join only where
    /// they touch within `device`, one device pixel. The caret's tick is `MARK_CARET` tall.
    fn rebuild(&mut self, track: f32, content: f32, start: f32, device: f32) {
        let (min, end) = (f32::from(MARK_MATCH_MIN), start + track);
        let per_row = track * self.line_height / content;
        let run = |(first, last): (u32, u32)| {
            let (mut top, mut bottom) = (start + per_row * first as f32, start + per_row * (last + 1) as f32);
            if bottom - top < min {
                let grow = (min - (bottom - top)) / 2.;
                (top, bottom) = (top - grow, bottom + grow);
            }
            if top < start {
                (top, bottom) = (start, bottom + start - top);
            }
            if bottom > end {
                (top, bottom) = ((top - (bottom - end)).max(start), end);
            }
            (top, bottom)
        };
        self.quads.clear();
        for &rows in &self.rows {
            let (top, bottom) = run(rows);
            match self.quads.last_mut() {
                Some(prev) if top <= prev.1 + device => prev.1 = prev.1.max(bottom),
                _ => self.quads.push((top, bottom)),
            }
        }
        self.current_quad = self.current.map(|(first, last)| run((first as u32, last as u32)));
        let half = f32::from(MARK_CARET) / 2.;
        self.caret_top = self.caret_row.map(|row| (start + per_row * (row as f32 + 0.5) - half).clamp(start, (end - half * 2.).max(start)));
    }
}

/// Draws the cached ticks on the vertical track of `edge`'s view, `span` long, under the knob and
/// below the find bar.
fn paint_marks(marks: &Marks, ink: &MarkInk, bounds: Bounds<Pixels>, edge: Edge, span: f32, window: &mut Window, cx: &App) {
    let mut marks = marks.0.borrow_mut();
    if !marks.on || marks.line_height <= 0. {
        return;
    }
    let inset = f32::from(SCROLL_INSET);
    let cover = marks.editor.as_ref().and_then(|editor| editor.upgrade()).and_then(|editor| editor.read(cx).caret_cover());
    let start = cover.map_or(inset, |cover| (f32::from(cover.bottom() - bounds.origin.y) + inset).max(inset));
    let (track, content) = (span - start - inset, edge.max + edge.height);
    if track <= 0. || content <= 0. {
        return;
    }
    let device = 1. / window.scale_factor();
    let key = (marks.rows_rev, marks.caret_row, marks.current, start.to_bits(), track.to_bits(), content.to_bits(), device.to_bits());
    if marks.pixels != Some(key) {
        marks.pixels = Some(key);
        marks.rebuild(track, content, start, device);
    }
    let across = SCROLL_KNOB;
    let x = bounds.right() - px(inset) - across;
    let tick = |top: f32, bottom: f32, ink: Hsla| {
        let rect = Bounds::new(point(x, bounds.origin.y + px(top)), size(across, px(bottom - top)));
        fill(rect, ink).corner_radii(Corners::all(MARK_RADIUS.min(px((bottom - top) / 2.))))
    };
    for &(top, bottom) in &marks.quads {
        window.paint_quad(tick(top, bottom, ink.find));
    }
    if let Some((top, bottom)) = marks.current_quad {
        window.paint_quad(tick(top, bottom, ink.current));
    } else if let Some(top) = marks.caret_top {
        window.paint_quad(tick(top, top + f32::from(MARK_CARET), ink.caret));
    }
}

/// The room the other axis's track takes at this axis's far end, so the two tracks don't overlap.
fn corner(edges: &[Option<Edge>; 2], dir: Dir) -> f32 {
    let other = edges[dir.other().ix()];
    if other.is_some_and(|edge| edge.max > 0.5) { f32::from(SCROLL_TRACK) } else { 0. }
}

/// The fade ramps of `edge` in `ground`: each band is a linear gradient, eight of them trace smoothstep.
fn paint_fade(bounds: Bounds<Pixels>, edge: Edge, ground: Hsla, window: &mut Window) {
    let (top, bottom) = edge.strengths();
    paint_ramp(bounds, false, true, 0., top, ground, window);
    paint_ramp(bounds, false, false, 0., bottom, ground, window);
}

/// One end of a list's edge fade: `ground` solid at the edge (`at_start`: the top or left end, else
/// the bottom or right), clear `LIST_EDGE_FADE` in, all scaled by `strength`; nothing for 0. The
/// ramp starts `inset` points in from a start edge, under an opaque stack pinned there. `horizontal`
/// lays it along a strip of tabs, else down a column. Shared with the tab strip's fade.
pub fn paint_ramp(bounds: Bounds<Pixels>, horizontal: bool, at_start: bool, inset: f32, strength: f32, ground: Hsla, window: &mut Window) {
    if strength <= 0. {
        return;
    }
    let band = f32::from(LIST_EDGE_FADE) / RAMP_BANDS as f32;
    for i in 0..RAMP_BANDS {
        // Distance from the edge in bands: the ground is solid at the edge and clear at the fade's end.
        let near = 1. - smoothstep(i as f32 / RAMP_BANDS as f32);
        let far = 1. - smoothstep((i + 1) as f32 / RAMP_BANDS as f32);
        let (at, from, to) = if at_start { (inset + band * i as f32, near, far) } else { (-band * (i + 1) as f32, far, near) };
        let (rect, angle) = if horizontal {
            let x = if at_start { bounds.origin.x + px(at) } else { bounds.right() + px(at) };
            (Bounds::new(point(x, bounds.origin.y), size(px(band), bounds.size.height)), 90.)
        } else {
            let y = if at_start { bounds.origin.y + px(at) } else { bounds.bottom() + px(at) };
            (Bounds::new(point(bounds.origin.x, y), size(bounds.size.width, px(band))), 180.)
        };
        let background = linear_gradient(angle, linear_color_stop(ground.opacity(from * strength), 0.), linear_color_stop(ground.opacity(to * strength), 1.));
        window.paint_quad(fill(rect, background));
    }
}

/// How tall the pinned-header shadow is, and the scroll distance over which it comes in.
const HEADER_SHADOW: f32 = 6.;
const HEADER_SHADOW_SCROLL: f32 = 8.;
const HEADER_SHADOW_ALPHA: f32 = 0.18;
const HEADER_SHADOW_ALPHA_LIGHT_SCALE: f32 = 0.7;

/// The soft shadow of a header pinned above a scrolled view: a black gradient down from the view's
/// top edge whose strength is the scroll distance over eight points, read at paint, so it needs no
/// state or frames and reverses exactly. Nothing shows at scroll 0. Put it in the view's container.
pub fn header_shadow(handle: Rc<dyn ScrollbarHandle>, light: bool) -> impl IntoElement {
    canvas(
        move |_, _, _| Edge::of(handle.as_ref(), 0.).scrolled,
        move |bounds, scrolled, window, _| {
            let strength = (scrolled / HEADER_SHADOW_SCROLL).clamp(0., 1.);
            if strength <= 0. {
                return;
            }
            let alpha = HEADER_SHADOW_ALPHA * if light { HEADER_SHADOW_ALPHA_LIGHT_SCALE } else { 1. } * strength;
            let rect = Bounds::new(bounds.origin, size(bounds.size.width, px(HEADER_SHADOW)));
            let background = linear_gradient(180., linear_color_stop(hsla(0., 0., 0., alpha), 0.), linear_color_stop(hsla(0., 0., 0., 0.), 1.));
            window.paint_quad(fill(rect, background));
        },
    )
    .absolute()
    .inset_0()
}

/// The knobs can be grabbed and dragged. A drag also wakes `owner`, the view that draws what the knob scrolls, when it isn't this one.
fn listen(bounds: Bounds<Pixels>, hitboxes: [Option<Hitbox>; 2], scroller: &Scroller, handle: &Rc<dyn ScrollbarHandle>, owner: Option<EntityId>, followed: Option<WeakEntity<EditorState>>, window: &mut Window) {
    let view = window.current_view();
    let wake = move |cx: &mut App| {
        cx.notify(view);
        if let Some(owner) = owner {
            cx.notify(owner);
        }
    };

    let (watch, watched) = (hitboxes.clone(), scroller.clone());
    window.on_mouse_event(move |_: &MouseMoveEvent, phase, window, cx| {
        if phase != DispatchPhase::Capture {
            return;
        }
        // GPUI redraws no one when a hitbox's hover changes; the knob's hover and its hold need a frame.
        let inner = watched.0.borrow();
        if Dir::ALL.into_iter().any(|dir| watch[dir.ix()].as_ref().is_some_and(|hit| hit.is_hovered(window)) != inner.axes[dir.ix()].inside) {
            cx.notify(view);
        }
    });

    let (down, moved) = (scroller.clone(), handle.clone());
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
            return;
        }
        let Some(dir) = Dir::ALL.into_iter().find(|dir| hitboxes[dir.ix()].as_ref().is_some_and(|hit| hit.is_hovered(window))) else {
            return;
        };
        if matches!(dir, Dir::Vertical) {
            if let Some(editor) = followed.as_ref().and_then(WeakEntity::upgrade) {
                editor.update(cx, |state, cx| state.stop_following(cx));
            }
        }
        let mut inner = down.0.borrow_mut();
        let Some(knob) = inner.axes[dir.ix()].knob else { return };
        let at = dir.along(event.position) - dir.start(bounds);
        let on_knob = at >= knob.top && at <= knob.top + knob.length;
        let now = crate::motion::drawn_now(window.window_handle().window_id());
        if !on_knob && !click_jumps(default_jumps(), event.modifiers.alt) {
            // A page toward the click, on the scroll glide, as the Mac's default setting does.
            let kept = moved.offset();
            let shown = -f32::from(match dir {
                Dir::Vertical => kept.y,
                Dir::Horizontal => kept.x,
            });
            // A click while a page glides pages on from where that one lands, so fast clicks add whole pages.
            let scrolled = inner.axes[dir.ix()].glide.target().unwrap_or(shown);
            let to = page_target(scrolled, knob.max, dir.extent(bounds), at < knob.top);
            let axis = &mut inner.axes[dir.ix()];
            axis.last = None;
            axis.glide.aim(shown, to, now);
            if !std::mem::replace(&mut axis.paging, true) {
                // Held like a knob drag, so a preview's estimated height doesn't shift under the page.
                moved.start_drag();
            }
            inner.touch(now);
            drop(inner);
            wake(cx);
            cx.stop_propagation();
            return;
        }
        // On the knob, the grab holds where it was taken; on the track, the knob's middle comes to the pointer.
        let grab = if on_knob { at - knob.top } else { knob.length / 2. };
        inner.axes[dir.ix()].grab = Some(grab);
        inner.axes[dir.ix()].glide.cancel();
        inner.axes[dir.ix()].paging = false; // the drag's own start_drag below takes over the hold
        inner.touch(now);
        drop(inner);
        moved.start_drag();
        drag_to(&down, dir, &moved, bounds, event.position);
        wake(cx);
        cx.stop_propagation();
    });

    let (drag, handle_move) = (scroller.clone(), handle.clone());
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
        if phase != DispatchPhase::Capture {
            return;
        }
        let Some(dir) = Dir::ALL.into_iter().find(|dir| drag.0.borrow().axes[dir.ix()].grab.is_some()) else {
            return;
        };
        drag.touch(window);
        drag_to(&drag, dir, &handle_move, bounds, event.position);
        wake(cx);
        cx.stop_propagation();
    });

    let (up, handle_up) = (scroller.clone(), handle.clone());
    window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
        if phase != DispatchPhase::Capture {
            return;
        }
        let released = Dir::ALL.into_iter().fold(false, |any, dir| up.0.borrow_mut().axes[dir.ix()].grab.take().is_some() || any);
        if released {
            up.touch(window);
            handle_up.end_drag();
            wake(cx);
        }
    });
}

/// Scrolls `dir` so the knob's grabbed spot is under `pointer`.
fn drag_to(scroller: &Scroller, dir: Dir, handle: &Rc<dyn ScrollbarHandle>, bounds: Bounds<Pixels>, pointer: Point<Pixels>) {
    let inner = scroller.0.borrow();
    let axis = &inner.axes[dir.ix()];
    let (Some(knob), Some(grab)) = (axis.knob, axis.grab) else { return };
    if knob.travel <= 0. {
        return;
    }
    let at = dir.along(pointer) - dir.start(bounds) - grab;
    let offset = -((at - knob.inset) / knob.travel).clamp(0., 1.) * knob.max;
    let kept = handle.offset();
    handle.set_offset(match dir {
        Dir::Vertical => point(kept.x, px(offset)),
        Dir::Horizontal => point(px(offset), kept.y),
    });
}

/// Whether the Mac's setting (System Settings > Appearance > "Click in the scroll bar to") is
/// "Jump to the spot that's clicked". Read at each click, which is cheap, so a change in the
/// setting needs no watching; the key is absent until the setting is first changed (then paging).
fn default_jumps() -> bool {
    use objc2::{class, msg_send, runtime::{AnyObject, Bool}};
    // SAFETY: standard Foundation calls on the main thread with a valid, NUL-ended key.
    unsafe {
        let defaults: *mut AnyObject = msg_send![class!(NSUserDefaults), standardUserDefaults];
        let key: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: c"AppleScrollerPagingBehavior".as_ptr()];
        let on: Bool = msg_send![defaults, boolForKey: key];
        on.as_bool()
    }
}

/// A click in the track jumps the knob to the pointer when the setting says jump and Option is up,
/// or the setting says page and Option is down; otherwise it pages.
fn click_jumps(default_jumps: bool, option: bool) -> bool {
    default_jumps != option
}

/// Where a page toward the start (`backward`) or the end of a view `extent` long, scrolled `scrolled`
/// of `max`, lands: the view's length less `SCROLL_PAGE_OVERLAP`, so a line or two stays in sight.
fn page_target(scrolled: f32, max: f32, extent: f32, backward: bool) -> f32 {
    let page = (extent - f32::from(SCROLL_PAGE_OVERLAP)).max(extent / 2.);
    (if backward { scrolled - page } else { scrolled + page }).clamp(0., max)
}

#[cfg(test)]
mod tests {
    use super::{Span, click_jumps, page_target, scan};
    use gpui_kit::component::input::Rope;
    use std::ops::Range;
    use std::sync::atomic::AtomicU64;

    fn spans_of(text: &str, needle: &str) -> Vec<Span> {
        let ranges: Vec<Range<usize>> = text.match_indices(needle).map(|(at, hit)| at..at + hit.len()).collect();
        let latest = AtomicU64::new(1);
        scan(&Rope::from(text), &ranges, 1, &latest).unwrap()
    }

    #[test]
    fn matches_on_neighbouring_lines_keep_their_own_spans() {
        // Matches on lines 3, 4 and 5 (from 0: 2, 3, 4): soft wrap can put rows between them.
        let spans = spans_of("a\nb\nx\nx\nx\nb\n", "x");
        assert_eq!(spans.len(), 3);
    }

    #[test]
    fn the_setting_and_option_pick_page_or_jump() {
        // The default setting pages; Option jumps. Under "jump to the spot", it is the other way round.
        assert!(!click_jumps(false, false));
        assert!(click_jumps(false, true));
        assert!(click_jumps(true, false));
        assert!(!click_jumps(true, true));
    }

    #[test]
    fn a_page_is_the_view_less_the_overlap_and_stops_at_the_ends() {
        assert_eq!(page_target(100., 1000., 500., false), 560.);
        assert_eq!(page_target(100., 1000., 500., true), 0.);
        assert_eq!(page_target(900., 1000., 500., false), 1000.);
        assert_eq!(page_target(900., 1000., 500., true), 440.);
    }

    #[test]
    fn matches_on_one_line_share_a_span() {
        let spans = spans_of("xx x\nb\n", "x");
        assert_eq!(spans, vec![Span { first: (0, 0), last: (0, 4) }]);
    }
}
