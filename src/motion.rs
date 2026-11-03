//! Motion helpers shared by the views: the frame clock that glides read, and the fold of rows
//! opening and closing on one critically damped spring (Harrier's `src/motion.rs` is the model).
//!
//! A frame's value is a function of when the motion started and the frame's time, never of the
//! frame before, so a motion can turn around mid-way from where it is drawn.

use std::{
    cell::RefCell,
    collections::HashMap,
    time::{Duration, Instant},
};

use gpui_kit::WindowId;

use crate::tokens::{ARRIVE, COLUMN_FADE_LEAD, COLUMN_LAND, COLUMN_SLIDE, EQUALIZE_DAMPING, HOVER_IN, HOVER_OUT, OVERSHOOT, SCROLL_GLIDE, SETTLE, SETTLE_RATE, SOFT_SETTLE_RATE, SPLIT};

/// The time frames are sampled at. A frame's paint starts a varying while after its vsync (layout
/// and the other views come first), so sampling `Instant::now()` there makes a glide advance by
/// uneven steps even when the frames are evenly spaced. This clock follows the frames' rhythm
/// instead: each frame advances it by the learned frame period, nudged toward the real elapsed time,
/// so it keeps pace with the display without its jitter.
#[derive(Default, Clone, Copy)]
pub struct FrameClock {
    /// The clock's last reading and the real time it was asked at.
    last: Option<(Instant, Instant)>,
    /// The frame period learned so far, in seconds.
    period: f32,
    /// How many frames in a row came over 1.5 periods apart (but inside `MAX_GAP`), and their
    /// gaps added up: a display that got slower, once there are `SLOWER_AFTER` of them.
    slow: (u8, f32),
    /// The same for frames under 1.5 periods' worth of a period too long (a period learned from
    /// late frames), so a wrong long period doesn't wait out the slow `LEARN`.
    fast: (u8, f32),
}

impl FrameClock {
    /// Frames this far apart or less are one display's consecutive frames; a longer gap restarts the clock.
    const MAX_GAP: f32 = 0.05;
    /// The share of each real period that updates the learned one.
    const LEARN: f32 = 0.1;
    /// The share of the phase error taken in each frame.
    const PHASE: f32 = 0.25;
    /// A gap shorter than this is a frame kicked just before the link's own (2 to 5 ms apart),
    /// not a display's period: it teaches nothing.
    const MIN_FRAME: f32 = 0.004;
    /// Frames in a row over 1.5 periods apart before the period rises to them: one late frame
    /// never teaches it, a window moved to a slower display is caught in three frames.
    const SLOWER_AFTER: u8 = 3;

    pub fn at(&mut self, now: Instant) -> Instant {
        let Some((shown, asked)) = self.last else {
            self.last = Some((now, now));
            return now;
        };
        let real = now.saturating_duration_since(asked).as_secs_f32();
        if real > Self::MAX_GAP {
            self.slow = (0, 0.);
            self.fast = (0, 0.);
            self.last = Some((now, now));
            return now;
        }
        if real < Self::MIN_FRAME {
            // Not a display frame: leave the learning where it is.
        } else if self.period <= 0. {
            self.period = real;
        } else if real < 1.5 * self.period {
            self.period += (real - self.period) * Self::LEARN;
            self.slow = (0, 0.);
            if real * 1.5 < self.period {
                self.fast = (self.fast.0 + 1, self.fast.1 + real);
                if self.fast.0 >= Self::SLOWER_AFTER {
                    self.period = self.fast.1 / f32::from(self.fast.0);
                    self.fast = (0, 0.);
                }
            } else {
                self.fast = (0, 0.);
            }
        } else {
            self.fast = (0, 0.);
            self.slow = (self.slow.0 + 1, self.slow.1 + real);
            if self.slow.0 >= Self::SLOWER_AFTER {
                self.period = self.slow.1 / f32::from(self.slow.0);
                self.slow = (0, 0.);
            }
        }
        // How far the clock has fallen behind (or run ahead of) the real time since the last frame.
        let behind = now.saturating_duration_since(shown).as_secs_f32() - self.period;
        let advance = (self.period + behind * Self::PHASE).max(0.);
        // Never reads later than the real time, so a glide never starts ahead of its own key press.
        let next = (shown + Duration::from_secs_f32(advance)).min(now);
        self.last = Some((next, now));
        next
    }
}

#[derive(Default)]
struct Shared {
    clock: FrameClock,
    /// The real time of the last tick and the reading it gave.
    read: Option<(Instant, Instant)>,
}

thread_local! {
    static SHARED: RefCell<HashMap<WindowId, Shared>> = RefCell::new(HashMap::new());
}

/// A frame's time in `window`, advancing its clock: call it once per frame, at the top of the
/// workspace's render (the root draws every frame), and let everything else that frame draws read
/// `drawn_now`.
pub fn frame_now(window: WindowId) -> Instant {
    let real = Instant::now();
    SHARED.with_borrow_mut(|all| {
        let shared = all.entry(window).or_default();
        let read = shared.clock.at(real);
        shared.read = Some((real, read));
        read
    })
}

/// The time a motion that starts or turns around now is drawn at: the last frame's reading while
/// frames are running, else the real time (the next frame restarts the clock there).
pub fn drawn_now(window: WindowId) -> Instant {
    let real = Instant::now();
    SHARED.with_borrow(|all| match all.get(&window).and_then(|shared| shared.read) {
        Some((asked, read)) if real.saturating_duration_since(asked).as_secs_f32() <= FrameClock::MAX_GAP => read,
        _ => real,
    })
}

/// The reading the frame about to draw in `window` will take (`frame_now` at the top of its
/// render), without advancing the clock: for a next-frame callback that decides before the draw.
pub fn next_now(window: WindowId) -> Instant {
    let real = Instant::now();
    SHARED.with_borrow(|all| all.get(&window).map_or(real, |shared| { let mut clock = shared.clock; clock.at(real) }))
}

/// Drops a closed window's clock.
pub fn forget(window: WindowId) {
    SHARED.with_borrow_mut(|all| {
        all.remove(&window);
    });
    gpui_kit::base::motion::forget_frame_time(window);
}

/// The y of the cubic bezier (0, 0), (x1, y1), (x2, y2), (1, 1) at x = `u`: the CSS `cubic-bezier`
/// timing function, solved by bisection on its parameter. `x1` and `x2` lie in 0 to 1, so x rises
/// with the parameter. No allocation; a few multiplications per step.
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, u: f32) -> f32 {
    fn at(s: f32, p1: f32, p2: f32) -> f32 {
        let r = 1. - s;
        3. * r * r * s * p1 + 3. * r * s * s * p2 + s * s * s
    }
    let u = u.clamp(0., 1.);
    let (mut lo, mut hi) = (0f32, 1f32);
    for _ in 0..16 {
        let s = (lo + hi) / 2.;
        if at(s, x1, x2) < u {
            lo = s;
        } else {
            hi = s;
        }
    }
    at((lo + hi) / 2., y1, y2)
}

/// The one fade curve: SwiftUI's ease-out, the bezier (0, 0, 0.58, 1), at `u` of its length.
pub fn ease_out(u: f32) -> f32 {
    cubic_bezier(0., 0., 0.58, 1., u)
}

/// The overshoot curve (`OVERSHOOT`): runs a touch past 1 and settles back to it, for a mark that lands with a settle.
pub fn overshoot(u: f32) -> f32 {
    let [x1, y1, x2, y2] = OVERSHOOT;
    cubic_bezier(x1, y1, x2, y2, u)
}

/// The leaving curve: the bezier (0.42, 0, 1, 1), slow at first and quickest at the end, for what
/// fades out (a tooltip, the drop preview).
pub fn ease_in(u: f32) -> f32 {
    cubic_bezier(0.42, 0., 1., 1., u)
}

/// The arriving curve of floating cards: the bezier (0.16, 1, 0.3, 1), nearly all of the way in
/// the first frames and then a long settle.
pub fn expo_out(u: f32) -> f32 {
    cubic_bezier(0.16, 1., 0.3, 1., u)
}

/// A level fading to a target: `from + (to - from) * ease_out(t / length)`, where `from` is the
/// level drawn when the target last changed, so a fade reversed midway turns around from where it
/// is. The length is `HOVER_IN` going up and `HOVER_OUT` going down, or one length both ways from
/// `Fade::with`. A function of the start and the frame's time alone, so it is the same at any
/// frame rate.
#[derive(Clone, Copy, Debug)]
pub struct Fade {
    from: f32,
    to: f32,
    /// When the target last changed; `None` for a fade that has never moved.
    since: Option<Instant>,
    length: f32,
    /// The lengths going up and going down.
    lengths: [f32; 2],
}

impl Default for Fade {
    fn default() -> Self {
        Self::at(0.)
    }
}

impl Fade {
    /// A fade at rest on `level`.
    pub fn at(level: f32) -> Self {
        Self { from: level, to: level, since: None, length: HOVER_IN, lengths: [HOVER_IN, HOVER_OUT] }
    }

    /// A fade at rest on `level` that takes `length` seconds in both directions.
    pub fn with(level: f32, length: f32) -> Self {
        Self { from: level, to: level, since: None, length, lengths: [length; 2] }
    }

    /// Heads for `to` from the level drawn at `now`; a fade already heading there carries on.
    pub fn aim(&mut self, to: f32, now: Instant) {
        if to == self.to {
            return;
        }
        let from = self.level(now);
        let length = self.lengths[usize::from(to <= from)];
        *self = Self { from, to, since: Some(now), length, lengths: self.lengths };
    }

    /// The level drawn at `now`.
    pub fn level(&self, now: Instant) -> f32 {
        let Some(since) = self.since else { return self.to };
        let u = now.saturating_duration_since(since).as_secs_f32() / self.length;
        if u >= 1. {
            return self.to;
        }
        self.from + (self.to - self.from) * ease_out(u)
    }

    /// It has not reached its target at `now`, so the next frame is wanted.
    pub fn is_moving(&self, now: Instant) -> bool {
        self.since.is_some_and(|since| now.saturating_duration_since(since).as_secs_f32() < self.length)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    fn feed(clock: &mut FrameClock, from: Instant, gap_ms: f32, frames: usize) -> Instant {
        let mut at = from;
        for _ in 0..frames {
            at += Duration::from_secs_f32(gap_ms / 1000.);
            clock.at(at);
        }
        at
    }

    #[test]
    fn frame_clock_period_rises_when_the_display_gets_slower() {
        let mut clock = FrameClock::default();
        let t = feed(&mut clock, Instant::now(), 8.33, 30);
        assert!((clock.period - 0.00833).abs() < 0.0002);
        // Moved to a 60 Hz display: three frames and the period is up.
        let t = feed(&mut clock, t, 16.7, 3);
        assert!((clock.period - 0.0167).abs() < 0.0002, "{}", clock.period);
        // And the clock keeps up: after a few more frames it reads within a frame of real time.
        let t = feed(&mut clock, t, 16.7, 10);
        let read = clock.at(t + Duration::from_secs_f32(0.0167));
        let lag = (t + Duration::from_secs_f32(0.0167)).saturating_duration_since(read);
        assert!(lag < Duration::from_millis(2), "{lag:?}");
    }

    #[test]
    fn frame_clock_period_ignores_one_late_frame_or_two() {
        let mut clock = FrameClock::default();
        let mut t = feed(&mut clock, Instant::now(), 8.33, 30);
        let before = clock.period;
        for late in [25., 16.7] {
            t = feed(&mut clock, t, late, 1);
            t = feed(&mut clock, t, 8.33, 2);
        }
        t = feed(&mut clock, t, 16.7, 2);
        feed(&mut clock, t, 8.33, 1);
        assert!((clock.period - before).abs() < 0.0003, "{} vs {before}", clock.period);
    }

    #[test]
    fn frame_clock_late_gaps_from_separate_bursts_do_not_add_up() {
        let mut clock = FrameClock::default();
        let mut t = feed(&mut clock, Instant::now(), 8.33, 30);
        let before = clock.period;
        for _ in 0..3 {
            t = feed(&mut clock, t, 14., 1);
            t = feed(&mut clock, t, 80., 1);
        }
        assert_eq!(clock.period, before);
    }

    #[test]
    fn frame_clock_period_comes_back_fast_from_a_wrong_long_one() {
        let mut clock = FrameClock::default();
        let t = feed(&mut clock, Instant::now(), 8.33, 30);
        let t = feed(&mut clock, t, 25., 3);
        assert!(clock.period > 0.02, "{}", clock.period);
        feed(&mut clock, t, 8.33, 3);
        assert!((clock.period - 0.00833).abs() < 0.0003, "{}", clock.period);
    }

    #[test]
    fn frame_clock_is_not_seeded_by_a_kicked_frame() {
        let mut clock = FrameClock::default();
        let t = feed(&mut clock, Instant::now(), 0.0, 1);
        let t = feed(&mut clock, t, 2.5, 1);
        assert_eq!(clock.period, 0.);
        feed(&mut clock, t, 8.33, 5);
        assert!((clock.period - 0.00833).abs() < 0.0002, "{}", clock.period);
    }

    #[test]
    fn fade_with_a_length_lands_at_it_and_turns_around_from_the_drawn_level() {
        let t0 = Instant::now();
        let mut fade = Fade::with(0., 0.2);
        fade.aim(1., t0);
        assert!(fade.is_moving(t0 + Duration::from_millis(199)));
        assert!(!fade.is_moving(t0 + Duration::from_millis(200)));
        assert_eq!(fade.level(t0 + Duration::from_millis(200)), 1.);

        let mid = t0 + Duration::from_millis(80);
        let drawn = fade.level(mid);
        assert!(drawn > 0. && drawn < 1.);
        fade.aim(0., mid);
        assert!((fade.level(mid) - drawn).abs() < 1e-6);
        assert!(fade.is_moving(mid + Duration::from_millis(199)));
        assert!(!fade.is_moving(mid + Duration::from_millis(200)));
        assert_eq!(fade.level(mid + Duration::from_millis(200)), 0.);
    }

    #[test]
    fn fade_at_keeps_the_hover_lengths() {
        let t0 = Instant::now();
        let mut fade = Fade::at(0.);
        fade.aim(1., t0);
        let up = Duration::from_secs_f32(HOVER_IN);
        assert!(fade.is_moving(t0 + up - Duration::from_micros(10)));
        assert!(!fade.is_moving(t0 + up + Duration::from_micros(10)));
        fade.aim(0., t0 + up + Duration::from_millis(1));
        let down = Duration::from_secs_f32(HOVER_OUT);
        let t1 = t0 + up + Duration::from_millis(1);
        assert!(fade.is_moving(t1 + down - Duration::from_micros(10)));
        assert!(!fade.is_moving(t1 + down + Duration::from_micros(10)));
    }

    #[test]
    fn glide_velocity_is_the_slope_of_its_level() {
        let t0 = Instant::now();
        for glide in [Glide::new(0., 1., t0).with_speed(2.), Glide::equalize(0.2, 0.8, t0).with_speed(-1.)] {
            for ms in [0, 40, 120, 220] {
                let at = t0 + Duration::from_millis(ms);
                let step = Duration::from_micros(20);
                let slope = (glide.level(at + step) - glide.level(at)) / step.as_secs_f32();
                assert!((glide.velocity(at) - slope).abs() < 0.05 * slope.abs().max(1.), "{ms} ms: {} vs {slope}", glide.velocity(at));
            }
            assert_eq!(glide.velocity(t0 + Duration::from_secs(2)), 0.);
        }
    }

    #[test]
    fn glide_range_follows_its_speed() {
        let t0 = Instant::now();
        let at_rest = Glide::equalize(0.2, 0.8, t0);
        for glide in [at_rest.with_speed(3.), at_rest.with_speed(-3.), Glide::new(0., 1., t0).with_speed(2.)] {
            assert_eq!(glide.range(), glide.measure(), "no stale range after with_speed");
        }
        assert_ne!(at_rest.range(), at_rest.with_speed(3.).range(), "a seeded speed moves the range");
    }

    #[test]
    fn scroll_glide_turned_around_keeps_the_speed_it_was_drawn_at() {
        let t0 = Instant::now();
        let mut scroll = ScrollGlide::default();
        scroll.aim(0., 1000., t0);
        let mid = t0 + Duration::from_millis(60);
        let drawn = scroll.step(mid).unwrap();
        let speed = scroll.glide.unwrap().velocity(mid);
        scroll.aim(drawn, 0., mid);
        assert!((scroll.glide.unwrap().velocity(mid) - speed).abs() < 1e-3);
        let later = scroll.step(mid + Duration::from_millis(5)).unwrap();
        assert!(later > drawn, "it carries on before it comes back");
    }
}

/// How far a critically damped spring has come `t` seconds after it began: `SETTLE` is its 99.9
/// percent time, and past it the spring is at rest (1).
fn spring(t: f32) -> f32 {
    if t >= SETTLE {
        return 1.;
    }
    let u = SETTLE_RATE / SETTLE * t.max(0.);
    1. - (1. + u) * (-u).exp()
}

/// The time at which the spring has come `progress` of the way, by bisection.
fn time_at(progress: f32) -> f32 {
    if progress >= spring(SETTLE - 1e-4) {
        return SETTLE;
    }
    let (mut low, mut high) = (0., SETTLE);
    for _ in 0..24 {
        let mid = (low + high) / 2.;
        if spring(mid) < progress {
            low = mid;
        } else {
            high = mid;
        }
    }
    (low + high) / 2.
}

/// One folder's rows opening or closing: how open they are drawn, 0 shut to 1 open, on the `SETTLE`
/// spring. A turn-around starts the new direction from the drawn level, so nothing queues.
#[derive(Clone, Copy, Debug)]
pub struct Fold {
    /// Where the fold is headed.
    open: bool,
    /// When the spring began; `None` while it waits (the folder's rows are still being read).
    since: Option<Instant>,
}

impl Fold {
    /// A fold opening from shut, running from `now` or held shut until `start`.
    pub fn opening(now: Instant, held: bool) -> Self {
        Self { open: true, since: (!held).then_some(now) }
    }

    /// A fold closing from fully open.
    pub fn closing(now: Instant) -> Self {
        Self { open: false, since: Some(now) }
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Whether the fold is waiting to start.
    pub fn is_held(&self) -> bool {
        self.since.is_none()
    }

    /// Starts a held fold at `now`.
    pub fn start(&mut self, now: Instant) {
        self.since.get_or_insert(now);
    }

    fn progress(&self, now: Instant) -> f32 {
        self.since.map_or(0., |since| spring(now.saturating_duration_since(since).as_secs_f32()))
    }

    /// How open the rows are drawn at `now`.
    pub fn level(&self, now: Instant) -> f32 {
        let progress = self.progress(now);
        match (self.open, self.since) {
            (_, None) => 0.,
            (true, _) => progress,
            (false, _) => 1. - progress,
        }
    }

    /// Turns the fold around from where it is drawn at `now`.
    pub fn reverse(&mut self, now: Instant) {
        let level = self.level(now);
        self.open = !self.open;
        let progress = if self.open { level } else { 1. - level };
        let back = Duration::from_secs_f32(time_at(progress));
        self.since = Some(now.checked_sub(back).unwrap_or(now));
    }

    /// The fold has landed (a held one has not).
    pub fn is_done(&self, now: Instant) -> bool {
        self.since.is_some_and(|since| now.saturating_duration_since(since).as_secs_f32() >= SETTLE)
    }

    /// The fold asks for frames: it runs and has not landed.
    pub fn is_moving(&self, now: Instant) -> bool {
        self.since.is_some() && !self.is_done(now)
    }
}

/// A value moving from one level to another on a spring that settles in `span` (`SPLIT` for a
/// split's share while a pane opens or closes). Critically damped unless made softer with
/// `equalize`; `with_speed` seeds it with a speed. The level is a function of the start and the
/// frame's time alone.
#[derive(Clone, Copy, Debug)]
pub struct Glide {
    from: f32,
    to: f32,
    since: Instant,
    span: f32,
    /// 1 is critical; under 1 the level passes the target once and comes back.
    damping: f32,
    /// The speed the level starts with, in levels per second (signed, not relative to the target).
    speed: f32,
    /// The lowest and highest level between the start and landing; see `range`.
    reach: (f32, f32),
}

impl Glide {
    pub fn new(from: f32, to: f32, now: Instant) -> Self {
        Self::over(from, to, SPLIT, now)
    }

    /// A glide that settles in `span` seconds.
    pub fn over(from: f32, to: f32, span: f32, now: Instant) -> Self {
        Self::reached(Self { from, to, since: now, span, damping: 1., speed: 0., reach: (0., 0.) })
    }

    /// A divider's double-click: settles in `SETTLE` on a `EQUALIZE_DAMPING` spring, so it passes
    /// even by about 3% and comes back.
    pub fn equalize(from: f32, to: f32, now: Instant) -> Self {
        Self::reached(Self { damping: EQUALIZE_DAMPING, ..Self::over(from, to, SETTLE, now) })
    }

    /// The glide starts moving at `speed` levels per second (signed): a drag's release carries the
    /// pointer's speed into the spring.
    pub fn with_speed(self, speed: f32) -> Self {
        Self::reached(Self { speed, ..self })
    }

    /// The glide with its `reach` worked out from its other fields.
    fn reached(self) -> Self {
        Self { reach: self.measure(), ..self }
    }

    /// How far the value is from `to` after `t` seconds.
    fn away(&self, t: f32) -> f32 {
        let gap = self.from - self.to;
        let start = self.speed;
        if self.damping >= 1. {
            let rate = SETTLE_RATE / self.span;
            (gap + (start + rate * gap) * t) * (-rate * t).exp()
        } else {
            let rate = SOFT_SETTLE_RATE / self.span;
            let turn = rate * (1. - self.damping * self.damping).sqrt() / self.damping;
            (-rate * t).exp() * (gap * (turn * t).cos() + (start + rate * gap) / turn * (turn * t).sin())
        }
    }

    /// Where the value is drawn at `now`.
    pub fn level(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.since).as_secs_f32();
        if t >= self.span {
            return self.to;
        }
        self.to + self.away(t)
    }

    /// How fast the value is drawn moving at `now`, in levels per second (signed); 0 once landed.
    pub fn velocity(&self, now: Instant) -> f32 {
        let t = now.saturating_duration_since(self.since).as_secs_f32();
        if t >= self.span {
            return 0.;
        }
        let gap = self.from - self.to;
        let carry = self.speed + if self.damping >= 1. { SETTLE_RATE } else { SOFT_SETTLE_RATE } / self.span * gap;
        if self.damping >= 1. {
            let rate = SETTLE_RATE / self.span;
            (carry - rate * (gap + carry * t)) * (-rate * t).exp()
        } else {
            let rate = SOFT_SETTLE_RATE / self.span;
            let turn = rate * (1. - self.damping * self.damping).sqrt() / self.damping;
            let (a, b) = (gap, carry / turn);
            (-rate * t).exp() * ((-rate * a + turn * b) * (turn * t).cos() - (rate * b + turn * a) * (turn * t).sin())
        }
    }

    /// The lowest and highest the value gets between now and landing: a pane held to the widest
    /// of them never shows bare ground, whatever the spring overshoots. Worked out when the glide
    /// is made or given a speed, so a frame only reads it.
    pub fn range(&self) -> (f32, f32) {
        self.reach
    }

    /// `range`, evaluated from the glide's inputs.
    fn measure(&self) -> (f32, f32) {
        const STEPS: usize = 32;
        (0..=STEPS).fold((self.from.min(self.to), self.from.max(self.to)), |(low, high), step| {
            let level = self.to + self.away(self.span * step as f32 / STEPS as f32);
            (low.min(level), high.max(level))
        })
    }

    pub fn is_done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since).as_secs_f32() >= self.span
    }
}

/// A rectangle that glides between places on `SPLIT` springs and fades in on `ARRIVE`, then out over
/// two thirds of it on an ease-in curve: the drop preview. `aim` it each frame with where it belongs
/// (`None` for nowhere); the first place after it was hidden is taken at once and only fades in,
/// and every later place glides from where the rectangle is drawn. `rect` and `opacity` read it,
/// `is_moving` says whether frames are needed.
#[derive(Clone, Copy, Debug)]
pub struct Morph {
    /// The place aimed at, as `[x, y, width, height]`, and the glides toward it.
    to: [f32; 4],
    glides: [Glide; 4],
    fade: Glide,
    /// The fade is heading to shown.
    up: bool,
    /// While leaving: the opacity it left from and when.
    leave: Option<(f32, Instant)>,
}

impl Morph {
    /// The fade-out's length: two thirds of the entrance.
    const LEAVE: f32 = ARRIVE * 2. / 3.;

    pub fn new(now: Instant) -> Self {
        Self { to: [0.; 4], glides: [Glide::new(0., 0., now); 4], fade: Glide::over(0., 0., ARRIVE, now), up: false, leave: None }
    }

    pub fn aim(&mut self, to: Option<[f32; 4]>, now: Instant) {
        match to {
            Some(to) => {
                if !self.up {
                    let hidden = self.opacity(now) <= 0.;
                    self.fade = Glide::over(self.opacity(now), 1., ARRIVE, now);
                    self.leave = None;
                    self.up = true;
                    if hidden {
                        self.to = to;
                        self.glides = to.map(|value| Glide::new(value, value, now));
                        return;
                    }
                }
                if self.to != to {
                    let from = self.rect(now);
                    self.to = to;
                    self.glides = std::array::from_fn(|ix| Glide::new(from[ix], to[ix], now));
                }
            }
            None => {
                if self.up {
                    self.leave = Some((self.opacity(now), now));
                    self.up = false;
                }
            }
        }
    }

    /// Where the rectangle is drawn at `now`, as `[x, y, width, height]`.
    pub fn rect(&self, now: Instant) -> [f32; 4] {
        self.glides.map(|glide| glide.level(now))
    }

    /// How visible it is at `now`, 0 to 1.
    pub fn opacity(&self, now: Instant) -> f32 {
        match self.leave {
            Some((from, since)) => from * (1. - ease_in(now.saturating_duration_since(since).as_secs_f32() / Self::LEAVE)),
            None => self.fade.level(now),
        }
    }

    /// It is sliding or fading at `now`.
    pub fn is_moving(&self, now: Instant) -> bool {
        let fading = match self.leave {
            Some((_, since)) => now.saturating_duration_since(since).as_secs_f32() < Self::LEAVE,
            None => !self.fade.is_done(now),
        };
        fading || (self.opacity(now) > 0. && self.glides.iter().any(|glide| !glide.is_done(now)))
    }
}

/// A scroll offset easing toward a target over `SCROLL_GLIDE`, for a jump that would otherwise
/// snap. `aim` it from where the view is drawn, `step` it once per frame and set the offset it
/// returns, and ask for another frame only while `is_moving`. `cancel` it when the hand scrolls.
#[derive(Default)]
pub struct ScrollGlide {
    glide: Option<Glide>,
    /// A new target is pending: the glide runs on until `aim` or `settle`.
    coasting: bool,
}

impl ScrollGlide {
    /// Heads for `to` from the offset `from` the view shows now; a glide already heading for `to`
    /// carries on, and one heading elsewhere hands its speed to the new one, so it turns around
    /// from where it is drawn instead of restarting from rest.
    pub fn aim(&mut self, from: f32, to: f32, now: Instant) {
        self.coasting = false;
        if self.glide.is_some_and(|glide| glide.to == to) {
            return;
        }
        let speed = self.glide.map_or(0., |glide| glide.velocity(now));
        self.glide = Some(Glide::over(from, to, SCROLL_GLIDE, now).with_speed(speed));
    }

    /// The offset to draw at `now`, or `None` when no glide runs. The frame it lands in returns
    /// the target and leaves the glide idle.
    pub fn step(&mut self, now: Instant) -> Option<f32> {
        let glide = self.glide?;
        if glide.is_done(now) {
            self.glide = None;
        }
        Some(glide.level(now))
    }

    /// A glide is running, so the next frame is wanted.
    pub fn is_moving(&self) -> bool {
        self.glide.is_some()
    }

    /// Where the running glide is heading.
    pub fn target(&self) -> Option<f32> {
        self.glide.map(|glide| glide.to)
    }

    /// Stops where the view is drawn, at once.
    pub fn cancel(&mut self) {
        self.glide = None;
        self.coasting = false;
    }

    /// A new target is coming with the next frame: the glide keeps its pace until `aim` hands its
    /// speed on, or `settle` stops it where the new target needs no move.
    pub fn coast(&mut self) {
        self.coasting = self.glide.is_some();
    }

    /// The coast ended with nowhere new to go: stops where the view is drawn. A glide not coasting
    /// is left alone.
    pub fn settle(&mut self) {
        if self.coasting {
            self.cancel();
        }
    }
}

/// A side column opening or closing: how far it is drawn open, 0 shut to 1 open, on a critically
/// damped spring that settles in `COLUMN_SLIDE`, and how visible its content is. Both are functions
/// of when the slide started and the frame's time alone, and a turn-around starts from the drawn
/// level, so nothing jumps. The slide lands once the edge is within `COLUMN_LAND` points of its
/// end, so the spring's tail never crawls.
#[derive(Clone, Copy, Debug)]
pub struct Slide {
    from: f32,
    to: f32,
    since: Instant,
    /// How visible the content was drawn when this slide began: an opening slide holds at least
    /// this, a closing one at most.
    held: f32,
    /// The column's full width in points, which turns the remaining level into a distance. Zero
    /// until `shape` says, and then the slide runs its whole `COLUMN_SLIDE`.
    travel: f32,
    /// The content is drawn whole from the first frame of an opening slide (a column whose
    /// content is ready) and not held back for `COLUMN_FADE_LEAD`. Closing fades either way.
    whole_on_open: bool,
}

impl Slide {
    /// A column at rest, open or shut.
    pub fn settled(open: bool) -> Self {
        let level = if open { 1. } else { 0. };
        Self { from: level, to: level, since: Instant::now(), held: level, travel: 0., whole_on_open: false }
    }

    /// Tells the slide the column's full width in points and whether its content rides in whole.
    pub fn shape(&mut self, travel: f32, whole_on_open: bool) {
        (self.travel, self.whole_on_open) = (travel, whole_on_open);
    }

    /// Heads for `open` from where the column is drawn at `now`; a column already heading there
    /// carries on.
    pub fn aim(&mut self, open: bool, now: Instant) {
        let to = if open { 1. } else { 0. };
        if self.to == to {
            return;
        }
        *self = Self { from: self.level(now), to, since: now, held: self.fade(now), ..*self };
    }

    /// Where the slide started, 0 shut to 1 open.
    pub fn started_at(&self) -> f32 {
        self.from
    }

    /// How much of the way is left at `now` as a share of the whole trip, or `None` once the slide
    /// has landed: its time is up, or the edge is within `COLUMN_LAND` points of its end.
    fn remaining(&self, now: Instant) -> Option<f32> {
        let t = now.saturating_duration_since(self.since).as_secs_f32();
        if self.from == self.to || t >= COLUMN_SLIDE {
            return None;
        }
        let u = SETTLE_RATE / COLUMN_SLIDE * t;
        let left = (1. + u) * (-u).exp();
        (left * (self.to - self.from).abs() * self.travel >= COLUMN_LAND).then_some(left)
    }

    /// How open the column is drawn at `now`.
    pub fn level(&self, now: Instant) -> f32 {
        match self.remaining(now) {
            Some(left) => self.from + (self.to - self.from) * (1. - left),
            None => self.to,
        }
    }

    /// The column is between open and shut at `now`: frames are needed.
    pub fn moving(&self, now: Instant) -> bool {
        self.remaining(now).is_some()
    }

    /// How visible the content is at `now`, 0 to 1. Opening, it waits hidden for `COLUMN_FADE_LEAD`
    /// of the way open and then eases in on a smoothstep (or is whole at once for a column that
    /// asked for it); closing, it stays whole and fades over the last `COLUMN_FADE_LEAD` of the
    /// way shut.
    pub fn fade(&self, now: Instant) -> f32 {
        let shown = self.level(now);
        if self.to >= 1. {
            if self.whole_on_open {
                1.
            } else {
                smoothstep((shown - COLUMN_FADE_LEAD) / (1. - COLUMN_FADE_LEAD)).max(self.held)
            }
        } else {
            smoothstep(shown / COLUMN_FADE_LEAD).min(self.held)
        }
    }
}

/// `x` clamped to 0 to 1, eased in and out.
fn smoothstep(x: f32) -> f32 {
    let x = x.clamp(0., 1.);
    x * x * (3. - 2. * x)
}
