use gpui::{Context, Pixels, Point, Task, px};
use instant::Duration;
#[cfg(not(target_family = "wasm"))]
pub(super) use std::time::Instant;
#[cfg(target_family = "wasm")]
pub(super) use web_time::Instant;

use crate::animation::cubic_bezier;

/// AppKit's insertion point is on for 0.53 s, then off for 0.53 s.
const HALF: Duration = Duration::from_millis(530);
/// How long typing holds the caret solid before the blink resumes.
const PAUSE_DELAY: Duration = HALF;
/// How long a half's fade takes (`CARET_FADE`, motion.md): the Sonoma
/// insertion point eases in and out, it never blinks hard.
pub(super) const CARET_FADE: Duration = Duration::from_millis(140);
/// How long the caret takes to glide to a new spot (`CARET_GLIDE`, motion.md).
pub(super) const CARET_GLIDE: Duration = Duration::from_millis(90);
/// How long after the last key or focus the caret keeps blinking before it
/// settles solid. Every fade repaints the window, and a window that keeps
/// drawing keeps the renderer's first-frame memory resident, so an idle caret
/// stops blinking, and no fade runs past this time.
const BLINK_WINDOW: Duration = Duration::from_secs(5);

pub(super) const CURSOR_WIDTH: Pixels = px(1.5);

/// Ease-out, SwiftUI's bezier (0, 0, 0.58, 1) from the duration ladder.
pub(super) fn ease_out(t: f32) -> f32 {
    cubic_bezier(0., 0., 0.58, 1.)(t)
}

/// To manage the Input cursor blinking.
///
/// It blinks in halves of 0.53 s. Each flip notifies the view once, and the
/// painter reads [`Self::alpha`] for the fade: it asks for a frame only while
/// the fade runs, so the time between flips draws nothing.
///
/// The input painter will check [`Self::alpha`], then it will draw the cursor.
pub(crate) struct BlinkCursor {
    /// What the current half is heading to.
    visible: bool,
    paused: bool,
    epoch: usize,
    /// When the blink window opened: the last start or pause.
    anchor: Option<Instant>,
    /// Where a running fade began and the alpha it left; `None` once settled.
    fade: Option<(Instant, f32)>,

    _task: Task<()>,
}

impl BlinkCursor {
    pub(crate) fn new() -> Self {
        Self {
            visible: false,
            paused: false,
            epoch: 0,
            anchor: None,
            fade: None,
            _task: Task::ready(()),
        }
    }

    /// Start the blinking from a visible cursor.
    ///
    /// A single focus can start the cursor more than once: `InputState::focus`
    /// starts it and then the focus observer starts it again, and window
    /// activation starts it too. Resuming the running cycle would toggle the
    /// cursor that was just shown off again, so every start restarts the cycle.
    pub(crate) fn start(&mut self, cx: &mut Context<Self>) {
        self.paused = false;
        self.visible = true;
        self.fade = None;
        self.anchor = Some(cx.background_executor().now());
        cx.notify();
        let epoch = self.next_epoch();
        self.schedule(epoch, HALF, cx);
    }

    /// Stop the blinking and clear the blink state, so the next [`Self::start`]
    /// begins from a visible cursor instead of resuming a stale pause.
    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        self.epoch = 0;
        self.paused = false;
        self.anchor = None;
        self.fade = None;
        self.visible = false;
        self._task = Task::ready(());
        cx.notify();
    }

    fn next_epoch(&mut self) -> usize {
        self.epoch += 1;
        self.epoch
    }

    /// Flip after `delay`, unless something restarted the cycle meanwhile.
    fn schedule(&mut self, epoch: usize, delay: Duration, cx: &mut Context<Self>) {
        self._task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| this.flip(epoch, cx));
            }
        });
    }

    fn flip(&mut self, epoch: usize, cx: &mut Context<Self>) {
        // A task from an earlier blink lifecycle must not mutate the current state.
        if epoch != self.epoch {
            return;
        }

        if self.paused {
            self.visible = true;
            return;
        }

        let now = cx.background_executor().now();
        let next = !self.visible;
        // A half that goes off needs the on half after it to finish inside the
        // window, and a half that comes on needs its own fade. Otherwise the
        // blink ends on a solid caret and schedules nothing more.
        let needs = if next { CARET_FADE } else { HALF + CARET_FADE };
        let deadline = self.anchor.map_or(now, |anchor| anchor + BLINK_WINDOW);
        if now + needs > deadline {
            if !self.visible {
                self.visible = true;
                self.fade = None;
                cx.notify();
            }
            return;
        }

        let from = self.alpha(now).0;
        self.visible = next;
        self.fade = Some((now, from));
        cx.notify();

        let epoch = self.next_epoch();
        self.schedule(epoch, HALF, cx);
    }

    /// The caret's opacity at `now`, and whether a fade is still running, which
    /// is when the painter must ask for another frame.
    pub(crate) fn alpha(&self, now: Instant) -> (f32, bool) {
        let to = if self.visible { 1. } else { 0. };
        let Some((began, from)) = self.fade else {
            return (to, false);
        };
        let t = now.saturating_duration_since(began).as_secs_f32() / CARET_FADE.as_secs_f32();
        if t >= 1. {
            (to, false)
        } else {
            (from + (to - from) * ease_out(t), true)
        }
    }

    pub(crate) fn visible(&self) -> bool {
        // Keep showing the cursor if paused
        self.paused || self.visible
    }

    /// Show the cursor immediately and restart the idle delay before blinking resumes.
    ///
    /// This is a no-op on a cursor that is not blinking. `epoch` is zero only
    /// before the first [`Self::start`] and after [`Self::stop`], so a zero
    /// epoch means the input is not focused, and there is no cursor on screen
    /// to keep visible. Pausing it anyway would start a blink loop that no blur
    /// is left to stop, and every blink repaints the view that the input is in.
    pub(crate) fn pause(&mut self, cx: &mut Context<Self>) {
        if self.epoch == 0 {
            return;
        }

        self.paused = true;
        self.visible = true;
        self.fade = None;
        self.anchor = Some(cx.background_executor().now());
        cx.notify();

        // Every pause replaces the pending timer, keeping repeated input visible.
        let epoch = self.next_epoch();
        self._task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(PAUSE_DELAY).await;

            if let Some(this) = this.upgrade() {
                this.update(cx, |this, cx| {
                    if epoch != this.epoch {
                        return;
                    }

                    this.paused = false;
                    this.flip(epoch, cx);
                });
            }
        });
    }
}

/// The caret's spring while you type, in radians a second: critically damped, about 100 ms to
/// 99 percent of the way (`HEAD_RATE`, Harrier's caret head spring, motion.md 'Caret typing').
const HEAD_RATE: f32 = 62.;
/// A move farther than this many points glides instead of springing: a jump, not a keystroke.
const SPRING_REACH: f32 = 200.;
/// A spring lands within this many device pixels of its target...
const REST_GAP: f32 = 0.5;
/// ...and slower than this many device pixels a second.
const REST_SPEED: f32 = 1.;

/// A critically damped spring on both axes as it was at `since`, heading for the target its
/// owner holds. Between targets it is read in closed form, so a frame never depends on the one
/// before. Positions are points, speeds points a second.
#[derive(Clone, Copy)]
struct Spring {
    at: (f32, f32),
    speed: (f32, f32),
    since: Instant,
}

impl Spring {
    /// Where it is and how fast it moves at `now`, heading for `to`.
    fn read(&self, to: (f32, f32), now: Instant) -> ((f32, f32), (f32, f32)) {
        let t = now.saturating_duration_since(self.since).as_secs_f32();
        let (w, decay) = (HEAD_RATE, (-HEAD_RATE * t).exp());
        let axis = |x: f32, v: f32, to: f32| {
            let e = x - to;
            let j = v + w * e;
            (to + (e + j * t) * decay, (v - w * j * t) * decay)
        };
        let ((x, vx), (y, vy)) =
            (axis(self.at.0, self.speed.0, to.0), axis(self.at.1, self.speed.1, to.1));
        ((x, y), (vx, vy))
    }
}

/// Where the active caret is drawn. A click or a jump glides from the spot it stood at to the
/// new one over [`CARET_GLIDE`] with ease-out. A keystroke moves the target of a spring that
/// keeps its position and speed, so a run of keys never brakes the caret to a stop between
/// them. Points are in content space, so scrolling moves the caret with the text and never
/// glides it.
#[derive(Default)]
pub(super) struct CaretGlide {
    from: Point<Pixels>,
    to: Point<Pixels>,
    /// When the running glide began; `None` once it landed.
    began: Option<Instant>,
    /// The running spring; `None` once it landed.
    spring: Option<Spring>,
    tracking: bool,
    /// The text offset the caret was last at, to tell a caret move from a layout move.
    offset: usize,
    /// The scroll offset last seen, to start a glide from the old on-screen spot.
    scroll: Point<Pixels>,
    /// When each extra caret first showed, so a new one fades in.
    pub(super) births: CaretBirths,
}

/// When each extra caret (a multi-cursor's, not the active one the glide follows) first showed:
/// a new one fades in over [`CARET_FADE`] on the caret's own clock. Only a pass with more
/// carets than the last fades the unknown ones in: an edit or an arrow key moves every caret
/// and none of them is new.
#[derive(Default)]
pub(super) struct CaretBirths {
    /// Set after the first pass: carets there from the start (a first paint, a fade-out) don't fade.
    primed: bool,
    /// The carets in the last pass, and whether this one has more.
    count: usize,
    grew: bool,
    born: Vec<(usize, Instant)>,
    last: Vec<(usize, Instant)>,
}

impl CaretBirths {
    /// Starts a paint pass over `count` selections (all of them, in view or not): the carets
    /// of the last pass become the ones to look up.
    pub(super) fn begin(&mut self, count: usize) {
        std::mem::swap(&mut self.born, &mut self.last);
        self.born.clear();
        self.last.sort_unstable_by_key(|&(at, _)| at);
        self.grew = self.primed && count > self.count;
        self.count = count;
    }

    /// Ends the pass.
    pub(super) fn end(&mut self) {
        self.primed = true;
    }

    /// How much of the caret at `offset` shows as of `now`, and whether it is still fading in.
    pub(super) fn weight(&mut self, offset: usize, now: Instant) -> (f32, bool) {
        let settled = now.checked_sub(CARET_FADE).unwrap_or(now);
        let born = match self.last.binary_search_by_key(&offset, |&(at, _)| at) {
            Ok(at) => self.last[at].1,
            Err(_) if self.grew => now,
            Err(_) => settled,
        };
        self.born.push((offset, born));
        let t = now.saturating_duration_since(born).as_secs_f32() / CARET_FADE.as_secs_f32();
        if t >= 1. { (1., false) } else { (ease_out(t), true) }
    }
 }

impl CaretGlide {
    /// Whether a spot is being followed, so a blur knows to forget it.
    pub(super) fn tracking(&self) -> bool {
        self.tracking
    }

    /// Forget the spot: the next one is landed on, not glided to.
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Moves toward `target` as of `now`: the spot to draw, and whether it is still moving
    /// (the painter asks for another frame). `typing` is a keystroke's edit moving the caret:
    /// a short move then springs. `scale` is the window's scale factor, for the landing rule.
    pub(super) fn step(
        &mut self,
        target: Point<Pixels>,
        offset: usize,
        scroll: Point<Pixels>,
        now: Instant,
        typing: bool,
        scale: f32,
    ) -> (Point<Pixels>, bool) {
        if !self.tracking {
            *self = Self {
                from: target,
                to: target,
                tracking: true,
                offset,
                scroll,
                ..Self::default()
            };
            return (target, false);
        }
        if (target.x - self.to.x).abs() > px(0.01) || (target.y - self.to.y).abs() > px(0.01) {
            if offset == self.offset {
                // The text moved under a caret that stayed put (the panel
                // resized, the gutter grew, the wrap changed): shift the glide.
                let (dx, dy) = (target.x - self.to.x, target.y - self.to.y);
                self.from = Point { x: self.from.x + dx, y: self.from.y + dy };
                if let Some(spring) = &mut self.spring {
                    spring.at = (spring.at.0 + f32::from(dx), spring.at.1 + f32::from(dy));
                }
            } else {
                // The caret moved: start from where it stood on screen, which
                // is not where the content spot is when the move also scrolled.
                let speed = self
                    .spring
                    .map_or((0., 0.), |spring| spring.read(self.to.into_pair(), now).1);
                let here = self.at(now, scale).0;
                let here = Point {
                    x: here.x + self.scroll.x - scroll.x,
                    y: here.y + self.scroll.y - scroll.y,
                };
                let reach = (f32::from(target.x - here.x)).hypot(f32::from(target.y - here.y));
                if typing && reach <= SPRING_REACH {
                    // The spring keeps the speed it had: the new key only moves its target.
                    self.spring = Some(Spring { at: here.into_pair(), speed, since: now });
                    self.began = None;
                } else {
                    self.from = here;
                    self.began = Some(now);
                    self.spring = None;
                }
            }
            self.to = target;
        }
        self.offset = offset;
        self.scroll = scroll;
        self.at(now, scale)
    }

    fn at(&mut self, now: Instant, scale: f32) -> (Point<Pixels>, bool) {
        if let Some(spring) = self.spring {
            let to = self.to.into_pair();
            let (at, speed) = spring.read(to, now);
            let gap = (at.0 - to.0).hypot(at.1 - to.1) * scale;
            if gap < REST_GAP && speed.0.hypot(speed.1) * scale < REST_SPEED {
                self.spring = None;
                return (self.to, false);
            }
            return (Point { x: px(at.0), y: px(at.1) }, true);
        }
        let Some(began) = self.began else {
            return (self.to, false);
        };
        let t = now.saturating_duration_since(began).as_secs_f32() / CARET_GLIDE.as_secs_f32();
        if t >= 1. {
            self.began = None;
            return (self.to, false);
        }
        let e = ease_out(t);
        (
            Point {
                x: self.from.x + (self.to.x - self.from.x) * e,
                y: self.from.y + (self.to.y - self.from.y) * e,
            },
            true,
        )
    }
}

trait IntoPair {
    fn into_pair(self) -> (f32, f32);
}

impl IntoPair for Point<Pixels> {
    fn into_pair(self) -> (f32, f32) {
        (f32::from(self.x), f32::from(self.y))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};
    use std::{cell::Cell, rc::Rc};

    #[gpui::test]
    fn repeated_pauses_keep_cursor_visible_until_idle(cx: &mut TestAppContext) {
        let cursor = cx.new(|_| BlinkCursor::new());
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
        // Only a focused input blinks, and only a blinking cursor pauses.
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        cx.run_until_parked();
        for _ in 0..5 {
            cursor.update(cx, |cursor, cx| cursor.pause(cx));
            cx.run_until_parked();
            cx.executor().advance_clock(Duration::from_millis(200));
            cx.run_until_parked();
            assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
        }
        cx.executor()
            .advance_clock(PAUSE_DELAY - Duration::from_millis(200));
        cx.run_until_parked();
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
        cx.executor().advance_clock(HALF);
        cx.run_until_parked();
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
    }

    #[gpui::test]
    fn starting_a_blinking_cursor_keeps_it_visible(cx: &mut TestAppContext) {
        let cursor = cx.new(|_| BlinkCursor::new());

        // Clicking into an input starts the cursor from `focus` and again from
        // the focus observer; the second start must not hide the new cursor.
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        cx.run_until_parked();
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));

        // The restart drops the first cycle's timer, so the cursor stays
        // visible for a full interval before it blinks off.
        cx.executor()
            .advance_clock(HALF - Duration::from_millis(1));
        cx.run_until_parked();
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
        cx.executor().advance_clock(Duration::from_millis(1));
        cx.run_until_parked();
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
    }

    #[gpui::test]
    fn pausing_a_cursor_that_is_not_blinking_does_not_start_it(cx: &mut TestAppContext) {
        // Never focused, so `start` was never called: the epoch is still zero.
        let cursor = cx.new(|_| BlinkCursor::new());

        // What a programmatic `set_value` on an unfocused input does.
        cursor.update(cx, |cursor, cx| cursor.pause(cx));
        cx.run_until_parked();
        cx.executor().advance_clock(PAUSE_DELAY);
        cx.run_until_parked();
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));

        let notifies = Rc::new(Cell::new(0usize));
        let counter = notifies.clone();
        let _observer =
            cx.update(|cx| cx.observe(&cursor, move |_, _| counter.set(counter.get() + 1)));
        cx.run_until_parked();

        cx.executor().advance_clock(HALF * 6);
        cx.run_until_parked();

        assert_eq!(
            notifies.get(),
            0,
            "a cursor that was never started is blinking, and every blink repaints the view"
        );
    }

    #[gpui::test]
    fn blurring_a_paused_cursor_leaves_the_next_focus_blinking(cx: &mut TestAppContext) {
        let cursor = cx.new(|_| BlinkCursor::new());
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        cx.run_until_parked();

        // Typing pauses the blink, then the input is blurred before the pause
        // elapses: tabbing away right after a keystroke does exactly this.
        cursor.update(cx, |cursor, cx| cursor.pause(cx));
        cx.run_until_parked();
        cursor.update(cx, |cursor, cx| cursor.stop(cx));
        cx.run_until_parked();
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));

        // Focusing again shows the cursor and blinks it, rather than leaving a
        // stale pause to swallow the start.
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        cx.run_until_parked();
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
        cx.executor().advance_clock(HALF);
        cx.run_until_parked();
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
    }

    #[gpui::test]
    fn stopping_a_paused_cursor_ends_the_blink_loop(cx: &mut TestAppContext) {
        let cursor = cx.new(|_| BlinkCursor::new());
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        cx.run_until_parked();
        cursor.update(cx, |cursor, cx| cursor.pause(cx));
        cx.run_until_parked();
        cursor.update(cx, |cursor, cx| cursor.stop(cx));
        cx.run_until_parked();

        let notifications = Rc::new(Cell::new(0usize));
        let count = notifications.clone();
        let _observer = cx.update(|cx| cx.observe(&cursor, move |_, _| count.set(count.get() + 1)));
        cx.run_until_parked();

        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();

        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
        assert_eq!(notifications.get(), 0, "a stopped cursor kept blinking");

        cursor.update(cx, |cursor, cx| cursor.start(cx));
        cx.run_until_parked();
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
    }

    #[gpui::test]
    fn stopping_a_blinking_cursor_ends_the_blink_loop(cx: &mut TestAppContext) {
        let cursor = cx.new(|_| BlinkCursor::new());
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        cx.run_until_parked();
        cursor.update(cx, |cursor, cx| cursor.stop(cx));
        cx.run_until_parked();

        let notifications = Rc::new(Cell::new(0usize));
        let count = notifications.clone();
        let _observer = cx.update(|cx| cx.observe(&cursor, move |_, _| count.set(count.get() + 1)));
        cx.run_until_parked();

        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();

        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
        assert_eq!(notifications.get(), 0, "a stopped cursor kept blinking");
    }

    #[gpui::test]
    fn blink_stops_after_the_cap_and_a_pause_restarts_it(cx: &mut TestAppContext) {
        let cursor = cx.new(|_| BlinkCursor::new());
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        cx.run_until_parked();

        let notifications = Rc::new(Cell::new(0usize));
        let count = notifications.clone();
        let _observer = cx.update(|cx| cx.observe(&cursor, move |_, _| count.set(count.get() + 1)));

        // The blink ends on a solid caret inside the 5 s window: the last off
        // half starts at 3.71 s, the last on half at 4.24 s, and no later half
        // fits, so nothing is scheduled after it.
        cx.executor().advance_clock(HALF * 10);
        cx.run_until_parked();
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));

        let settled = notifications.get();
        cx.executor().advance_clock(Duration::from_secs(10));
        cx.run_until_parked();
        assert!(cursor.read_with(cx, |cursor, _| cursor.visible()));
        assert_eq!(notifications.get(), settled, "an idle cursor kept drawing");

        // A key press restarts the blink.
        cursor.update(cx, |cursor, cx| cursor.pause(cx));
        cx.run_until_parked();
        cx.executor().advance_clock(PAUSE_DELAY);
        cx.run_until_parked();
        assert!(!cursor.read_with(cx, |cursor, _| cursor.visible()));
    }

    #[gpui::test]
    fn a_flip_fades_and_asks_for_no_frames_between_halves(cx: &mut TestAppContext) {
        let cursor = cx.new(|_| BlinkCursor::new());
        cursor.update(cx, |cursor, cx| cursor.start(cx));
        cx.run_until_parked();
        let alpha = |cx: &mut TestAppContext| {
            let now = cx.executor().now();
            cursor.read_with(cx, |cursor, _| cursor.alpha(now))
        };
        assert_eq!(alpha(cx), (1., false), "a solid caret needs no frames");

        cx.executor().advance_clock(HALF);
        cx.run_until_parked();
        let (start, fading) = alpha(cx);
        assert!(fading && start > 0.99, "the fade begins from where it stood");

        cx.executor().advance_clock(CARET_FADE / 2);
        let (middle, fading) = alpha(cx);
        assert!(fading && middle > 0. && middle < 1.);
        // Ease-out: past 0.68 of the way after half the time.
        assert!(middle < 0.35, "{middle}");

        cx.executor().advance_clock(CARET_FADE);
        assert_eq!(alpha(cx), (0., false), "between halves nothing moves");
    }

    const SCALE: f32 = 2.;

    fn at(x: f32, y: f32) -> Point<Pixels> {
        Point { x: px(x), y: px(y) }
    }

    #[gpui::test]
    fn a_click_glides_to_each_new_spot_and_lands(cx: &mut TestAppContext) {
        let now = cx.executor().now();
        let mut glide = CaretGlide::default();
        let none = at(0., 0.);
        let (a, b) = (at(10., 0.), at(20., 0.));
        assert_eq!(
            glide.step(a, 1, none, now, false, SCALE),
            (a, false),
            "the first spot is landed on"
        );

        let (at_start, moving) = glide.step(b, 2, none, now, false, SCALE);
        assert!(moving && at_start.x == px(10.));
        assert!(glide.spring.is_none(), "a click is a glide, not a spring");
        let (half, moving) = glide.step(b, 2, none, now + CARET_GLIDE / 2, false, SCALE);
        assert!(moving && half.x > px(16.) && half.x < px(18.), "{:?}", half.x);
        // A new click moves the target mid-glide: it carries on from where it is.
        let c = at(30., 0.);
        let (carried, moving) = glide.step(c, 3, none, now + CARET_GLIDE / 2, false, SCALE);
        assert!(moving && carried.x == half.x);
        assert_eq!(glide.step(c, 3, none, now + CARET_GLIDE * 2, false, SCALE), (c, false));

        glide.clear();
        assert_eq!(glide.step(a, 1, none, now, false, SCALE), (a, false), "a blur forgets the spot");
    }

    #[gpui::test]
    fn a_far_move_glides_even_while_typing(cx: &mut TestAppContext) {
        let now = cx.executor().now();
        let mut glide = CaretGlide::default();
        let none = at(0., 0.);
        glide.step(at(10., 0.), 1, none, now, true, SCALE);
        // Enter at the end of a long line: the caret jumps back across the window.
        let (start, moving) = glide.step(at(10., 400.), 2, none, now, true, SCALE);
        assert!(moving && start == at(10., 0.));
        assert!(glide.spring.is_none() && glide.began.is_some());
    }

    #[gpui::test]
    fn a_keystroke_moves_the_springs_target_and_keeps_its_speed(cx: &mut TestAppContext) {
        let now = cx.executor().now();
        let mut glide = CaretGlide::default();
        let none = at(0., 0.);
        glide.step(at(10., 0.), 1, none, now, true, SCALE);

        // The first key springs from rest.
        let (start, moving) = glide.step(at(18., 0.), 2, none, now, true, SCALE);
        assert!(moving && start == at(10., 0.));
        assert_eq!(glide.spring.unwrap().speed, (0., 0.));

        // The next key lands 40 ms later, mid-flight: the position carries on and so does the
        // speed, instead of braking to zero for the new target.
        let later = now + Duration::from_millis(40);
        let (mid, moving) = glide.step(at(18., 0.), 2, none, later, false, SCALE);
        assert!(moving && mid.x > px(10.) && mid.x < px(18.));
        let moving_at = glide.spring.unwrap().read((18., 0.), later).1 .0;
        assert!(moving_at > 20., "{moving_at}");
        let (here, moving) = glide.step(at(26., 0.), 3, none, later, true, SCALE);
        assert!(moving && (here.x - mid.x).abs() < px(0.001), "{here:?} vs {mid:?}");
        assert!((glide.spring.unwrap().speed.0 - moving_at).abs() < 0.01);
        assert!(glide.began.is_none(), "a key never starts the 0.09 s glide");

        // It keeps going toward the new target, faster than a spring started from rest would.
        let (next, _) = glide.step(at(26., 0.), 3, none, later + Duration::from_millis(10), false, SCALE);
        let rest = Spring { at: (f32::from(here.x), 0.), speed: (0., 0.), since: later };
        let from_rest = rest.read((26., 0.), later + Duration::from_millis(10)).0 .0;
        assert!(f32::from(next.x) > from_rest + 0.1, "{next:?} vs {from_rest}");
    }

    #[gpui::test]
    fn a_spring_lands_within_half_a_device_pixel_and_under_one_pixel_a_second(
        cx: &mut TestAppContext,
    ) {
        let now = cx.executor().now();
        let mut glide = CaretGlide::default();
        let none = at(0., 0.);
        glide.step(at(10., 0.), 1, none, now, true, SCALE);
        glide.step(at(18., 0.), 2, none, now, true, SCALE);

        // Close but still fast: not landed.
        glide.spring = Some(Spring { at: (17.9, 0.), speed: (100., 0.), since: now });
        let (_, moving) = glide.step(at(18., 0.), 2, none, now, false, SCALE);
        assert!(moving, "0.2 device px away but at 200 px/s is still moving");
        // Slow but still far: not landed (0.3 pt is 0.6 device px at 2x).
        glide.spring = Some(Spring { at: (17.7, 0.), speed: (0., 0.), since: now });
        let (_, moving) = glide.step(at(18., 0.), 2, none, now, false, SCALE);
        assert!(moving, "0.6 device px away is not landed");
        // Close and slow: lands on the target, and asks for no more frames.
        glide.spring = Some(Spring { at: (17.9, 0.), speed: (0.2, 0.), since: now });
        assert_eq!(glide.step(at(18., 0.), 2, none, now, false, SCALE), (at(18., 0.), false));
        assert!(glide.spring.is_none());

        // Left alone it settles: a second after the last key it is at rest on the target.
        glide.step(at(30., 0.), 3, none, now, true, SCALE);
        let end = now + Duration::from_secs(1);
        assert_eq!(glide.step(at(30., 0.), 3, none, end, false, SCALE), (at(30., 0.), false));
    }

    #[gpui::test]
    fn a_layout_move_shifts_the_glide_and_a_scroll_keeps_the_old_screen_spot(
        cx: &mut TestAppContext,
    ) {
        let now = cx.executor().now();
        let mut glide = CaretGlide::default();
        let none = at(0., 0.);
        glide.step(at(10., 0.), 1, none, now, false, SCALE);
        // The text moved 40 px under a caret that stayed at offset 1: no glide.
        assert_eq!(glide.step(at(50., 0.), 1, none, now, false, SCALE), (at(50., 0.), false));

        // A move that scrolls the view 100 px: it starts from where the caret
        // stood on screen (50), which is -50 in the new content space.
        let scrolled = at(0., -100.);
        let (start, moving) = glide.step(at(60., 200.), 2, scrolled, now, false, SCALE);
        assert!(moving && start == at(50., 100.), "{start:?}");
    }
}
