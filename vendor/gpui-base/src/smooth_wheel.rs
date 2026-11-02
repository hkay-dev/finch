//! Wheel scrolling on the display's clock, ported from Harrier's
//! `smooth_wheel.rs` (harrier#84za) and shared by the editor, the tree and the
//! text view's scroll area (gpui-editor#ases).
//!
//! A mouse driver (BetterMouse's smoothing) posts wheel events every 6 to 14
//! ms, unevenly, and a scroller that jumps on each one moves half or double
//! the last frame's distance: jitter at any frame rate. Here the wheel moves a
//! target, and every frame eases the shown offset toward it with a time
//! constant of 16 ms. The shown offset rests on whole device pixels, and
//! nothing asks for frames once it has reached the target.
//!
//! [`SmoothWheel`] holds the state and no window: the scroller calls
//! [`SmoothWheel::nudge`] from its wheel handler and [`SmoothWheel::step`]
//! from its prepaint, and puts the offset `step` returns on its handle.
//! Offsets are the scroll handle's: 0 at the top, negative further down.
//! [`driver`] does both for a plain [`ScrollHandle`] (the tree's list).
//!
//! Stretch past the ends is not here: the editor and the text view keep
//! their own (`overscroll.rs`, `scroll_bounce.rs`), which run on `nudge`'s
//! return value, the part of a delta the target took.
//!
//! A finger dragging on the trackpad skips the ease (gpui-editor#yk38): the
//! scroller tells the glide each event's phase and when it came
//! ([`SmoothWheel::touch`]), and while a finger is down each target is a
//! sample of where the finger had taken the scroller. A trackpad's events
//! don't line up with the frames (6 to 14 ms apart against a 6 or 8 ms
//! refresh), so showing the latest target moves a frame by one event's worth
//! or two. Instead each frame shows the finger where it was about one event
//! gap before the frame ([`Path`], ported from Harrier's `scroll_stream.rs`,
//! harrier#51a7), so every frame moves by what the finger moved in a frame's
//! time (gpui-editor#dar7). The momentum that follows the lift plays on the
//! same path; wheels, which send no phase, still ease.

use std::{cell::RefCell, collections::VecDeque, rc::Rc, time::Instant};

use gpui::{
    DispatchPhase, Hitbox, HitboxBehavior, IntoElement, ScrollEvent, ScrollHandle, Styled,
    TouchPhase, Window, canvas, point, px,
};

/// How far the shown offset trails the target: the time constant of the ease,
/// about two thirds of the way there after this many milliseconds.
const TRAIL_MS: f32 = 16.;
/// Closer than this, the glide stops on the target.
const REST: f32 = 0.5;
/// A frame that took longer than this eases as if it took this long.
const MAX_FRAME_MS: f32 = 50.;

/// The least a finger's playback runs behind the frame, and the least a move
/// after a touch or a pause takes to come in: a trackpad's events are 6 to 14
/// ms apart.
const LAG_MIN: f64 = 0.008;
/// The most the playback runs behind the frame.
const LAG_MAX: f64 = 0.032;
/// The share of a frame's time by which the lag may change in that frame, so
/// the speed changes by at most that much from one frame to the next.
const CATCH_UP: f64 = 0.05;
/// The lag changes only when it's this far from the wanted one, so gaps that
/// wobble by a millisecond don't keep changing the speed.
const SLACK: f64 = 0.002;
/// The finger is drawn at its average over this long before the playhead, so
/// an event the main thread handled a little late doesn't bend its path.
const SMOOTH: f64 = 0.008;
/// The gaps between this many of the last events set the lag.
const GAPS: usize = 8;
const MAX_SAMPLES: usize = 64;

/// A scroller in motion: where the wheel wants it, where it was last put, and when.
#[derive(Clone, Copy, Debug)]
struct Glide {
    target: f32,
    shown: f32,
    /// The offset last handed out, on a device pixel. Another offset on the
    /// scroller means something else moved it.
    painted: f32,
    last: Instant,
}

/// What a frame puts on the scroller.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Step {
    /// The offset to set.
    pub offset: f32,
    /// Whether the glide has more to go: the next frame has to be asked for.
    pub more: bool,
}

/// The eased wheel state of one scroller.
#[derive(Debug, Default)]
pub struct SmoothWheel {
    glide: Option<Glide>,
    /// A finger is on the trackpad: the scroller follows it with no ease.
    finger: bool,
    /// The finger has touched but not moved since.
    resting: bool,
    /// The finger's targets, played back on the frame clock.
    path: Path,
    /// When the last event came, and the seconds since the one before it.
    event: Option<Instant>,
    gap: Option<f64>,
}

impl SmoothWheel {
    /// Whether a glide is in flight.
    pub fn is_moving(&self) -> bool {
        self.glide.is_some()
    }

    /// Where the wheel is taking the scroller: the offset the glide ends on.
    pub fn target(&self) -> Option<f32> {
        self.glide.map(|glide| glide.target)
    }

    /// Drops the glide: the scroller stays where it shows.
    pub fn clear(&mut self) {
        self.glide = None;
        self.path.clear();
    }

    /// Notes a wheel event's phase and `at`, when it came (the real time, not
    /// the frame's), so a dragging finger skips the ease. `moved` is whether
    /// the event moves on either axis. Call it for every event over the
    /// scroller, the ones that don't scroll it included, before `nudge`.
    pub fn touch(&mut self, phase: TouchPhase, moved: bool, at: Instant) {
        self.gap = self
            .event
            .map(|was| at.saturating_duration_since(was).as_secs_f64());
        self.event = Some(at);
        match phase {
            TouchPhase::Started => {
                self.finger = true;
                self.resting = !moved;
                // A finger that lands mid-glide catches it where it shows,
                // so its first move doesn't jump the rest of the way.
                if let Some(glide) = &mut self.glide {
                    glide.target = glide.shown;
                }
                self.path.clear();
            }
            TouchPhase::Ended | TouchPhase::Cancelled => {
                self.finger = false;
                self.resting = false;
            }
            TouchPhase::Moved if moved => self.resting = false,
            // A finger that touches and lifts without moving cancels its
            // gesture, and GPUI reports the cancel as a move of nothing.
            TouchPhase::Moved if self.resting => {
                self.finger = false;
                self.resting = false;
            }
            TouchPhase::Moved => {}
        }
    }

    /// Moves the target by `delta` (positive scrolls toward the top) and
    /// returns how much of it the target took: all of it, or less when the
    /// target ran into an end. `offset` is where the scroller shows now and
    /// `max` how far it can go (`None` when that isn't known: only the top
    /// stops it). A glide the scroller has left behind (something else moved
    /// it) starts over from `offset`.
    pub fn nudge(&mut self, delta: f32, offset: f32, max: Option<f32>, top: f32, now: Instant) -> f32 {
        let glide = match &mut self.glide {
            Some(glide) if (offset - glide.painted).abs() <= REST => glide,
            slot => {
                self.path.clear();
                slot.insert(Glide {
                    target: offset,
                    shown: offset,
                    painted: offset,
                    last: now,
                })
            }
        };
        // A glide at rest (a finger's, kept for its fraction) eases from now,
        // not from the frame it last painted.
        if glide.shown == glide.target {
            glide.last = now;
        }
        let before = glide.target;
        glide.target = clamp(before + delta, max, top);
        // A finger's move, or the momentum straight after it, goes on the
        // path the frames play back. A path that played out before the lift
        // (the finger held still, then lifted with no move) is done: what
        // comes after it, a wheel, eases.
        if !self.finger && self.path.caught_up() {
            self.path.clear();
        }
        if self.finger || !self.path.is_empty() {
            if let Some(at) = self.event {
                self.path.push(at, before, glide.target, self.gap);
            }
        }
        glide.target - before
    }

    /// Moves the shown offset for this frame: along the finger's path while
    /// one plays, else part of the way to the target. `offset` is where the
    /// scroller shows now, `scale` the window's scale factor, `now` the
    /// frame's time. `None`: no glide, or something else moved the scroller,
    /// which has the offset now.
    pub fn step(&mut self, offset: f32, max: Option<f32>, top: f32, scale: f32, now: Instant) -> Option<Step> {
        let glide = self.glide.as_mut()?;
        if (offset - glide.painted).abs() > REST {
            self.glide = None;
            self.path.clear();
            return None;
        }
        let elapsed = (now.saturating_duration_since(glide.last).as_secs_f32() * 1000.).min(MAX_FRAME_MS);
        glide.last = now;
        // The content may have shrunk since the wheel set the target.
        glide.target = clamp(glide.target, max, top);
        let more = match self.path.tick(now) {
            Some((at, playing)) => {
                glide.shown = clamp(at as f32, max, top);
                if !playing {
                    if self.finger {
                        // Caught up with a finger that holds still: on the target.
                        glide.shown = glide.target;
                    } else {
                        // Played out after the lift: the ease takes the rest.
                        self.path.clear();
                    }
                }
                playing || (glide.target - glide.shown).abs() > REST
            }
            None => {
                let left = glide.target - glide.shown;
                let settled = left.abs() <= REST;
                glide.shown = if settled {
                    glide.target
                } else {
                    glide.shown + left * (1. - (-elapsed / TRAIL_MS).exp())
                };
                !settled
            }
        };
        let painted = snap(glide.shown, max, top, scale);
        glide.painted = painted;
        // A finger's glide outlives the frame, so the next move starts from
        // the target's own fraction of a pixel, not the painted one.
        if !more && !self.finger {
            self.glide = None;
        }
        Some(Step {
            offset: painted,
            more,
        })
    }
}

/// A finger's drag played back on the frame clock (gpui-editor#dar7). Each
/// target the finger set is a sample of where it had taken the scroller and
/// when its event came. Each frame shows the finger where it was a little
/// before the frame: between two samples it moves at the speed the finger
/// moved between them, averaged over `SMOOTH`, so every frame moves by what
/// the finger moved in that frame's time however the events line up.
///
/// The playhead runs behind the frame by the longest recent gap between
/// events, so a sample is almost always there to move toward. When the gaps
/// change, the lag follows by at most `CATCH_UP` of each frame's time. Past
/// the last sample it holds, and the next move goes on from there.
#[derive(Debug, Default)]
struct Path {
    /// What times count from: the move's first event.
    base: Option<Instant>,
    /// Seconds from `base` and the target then, oldest first; the first is
    /// where the move began.
    samples: VecDeque<(f64, f64)>,
    /// Seconds the playhead runs behind the frame.
    lag: f64,
    /// The last frame's time and the playhead then, in seconds from `base`.
    frame: Option<(f64, f64)>,
}

impl Path {
    fn clear(&mut self) {
        self.base = None;
        self.samples.clear();
        self.frame = None;
    }

    fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    fn secs(&self, at: Instant) -> f64 {
        let Some(base) = self.base else { return 0. };
        match at.checked_duration_since(base) {
            Some(after) => after.as_secs_f64(),
            None => -base.duration_since(at).as_secs_f64(),
        }
    }

    /// The finger moved the target from `from` to `to` with an event that came
    /// `at`, `gap` seconds after the event before it.
    fn push(&mut self, at: Instant, from: f32, to: f32, gap: Option<f64>) {
        let t = self.secs(at);
        let playing = !self.caught_up();
        match self.samples.back_mut() {
            // Two events handled together: the later one moves the last sample.
            Some(last) if playing && t <= last.0 => {
                last.1 = f64::from(to);
                return;
            }
            Some(_) if playing => self.samples.push_back((t, f64::from(to))),
            _ => {
                // A first move, or one after the finger held still: it comes
                // in over the gap since the event before it, a trackpad's
                // usual gap when that was a pause.
                let first = gap.filter(|gap| *gap <= LAG_MAX).unwrap_or(LAG_MIN).max(LAG_MIN);
                self.base = Some(at);
                self.samples.clear();
                self.samples.extend([(-first, f64::from(from)), (0., f64::from(to))]);
                self.lag = first;
                self.frame = None;
                return;
            }
        }
        // Samples the playhead's average has passed go, keeping one before it
        // and the last `GAPS` gaps.
        let head = self.frame.map_or(f64::MIN, |(_, head)| head - SMOOTH);
        while self.samples.len() > MAX_SAMPLES
            || (self.samples.len() > GAPS + 1 && self.samples.get(1).is_some_and(|(t, _)| *t <= head))
        {
            self.samples.pop_front();
        }
    }

    /// Where the playhead stops: the path moves no more there until the next sample.
    fn end(&self) -> Option<f64> {
        self.samples.back().map(|(last, _)| last + SMOOTH)
    }

    /// The playhead has reached the end: the next move starts over.
    fn caught_up(&self) -> bool {
        match (self.frame, self.end()) {
            (Some((_, head)), Some(end)) => head >= end,
            (_, end) => end.is_none(),
        }
    }

    /// The longest gap between the last `GAPS` events.
    fn longest_gap(&self) -> f64 {
        let skip = self.samples.len().saturating_sub(GAPS + 1);
        let times = self.samples.iter().skip(skip);
        times.clone().zip(times.skip(1)).map(|(a, b)| b.0 - a.0).fold(0., f64::max)
    }

    /// Moves the playhead to the frame at `now` and returns the offset it shows
    /// and whether the playhead is short of the end. A frame at the same or an
    /// earlier time shows what the last one did. `None`: no path.
    fn tick(&mut self, now: Instant) -> Option<(f64, bool)> {
        let end = self.end()?;
        let t = self.secs(now);
        let head = match self.frame {
            Some((was, head)) if t <= was => head,
            last => {
                let head = match last {
                    Some((was, head)) => {
                        let off = self.longest_gap().clamp(LAG_MIN, LAG_MAX) - self.lag;
                        if off.abs() > SLACK {
                            let most = CATCH_UP * (t - was);
                            self.lag += off.clamp(-most, most);
                        }
                        (t - self.lag).max(head)
                    }
                    None => t - self.lag,
                };
                // Past the end the path holds, and the next move goes on from there.
                let head = head.min(end);
                self.lag = t - head;
                self.frame = Some((t, head));
                head
            }
        };
        let shown = (self.integral(head) - self.integral(head - SMOOTH)) / SMOOTH;
        Some((shown, head < end))
    }

    /// The offset summed over time from the first sample to `t`, linear
    /// between samples; before the first and after the last it holds.
    fn integral(&self, t: f64) -> f64 {
        let Some(&(first, at_first)) = self.samples.front() else { return 0. };
        let mut sum = at_first * (t.min(first) - first);
        for (&(a, va), &(b, vb)) in self.samples.iter().zip(self.samples.iter().skip(1)) {
            if t <= a {
                return sum;
            }
            let end = t.min(b);
            sum += (va + (va + (vb - va) * (end - a) / (b - a))) / 2. * (end - a);
        }
        let (last, at_last) = self.samples.back().copied().unwrap_or_default();
        sum + at_last * (t - last).max(0.)
    }
}

fn clamp(offset: f32, max: Option<f32>, top: f32) -> f32 {
    match max {
        Some(max) => offset.clamp(-max.max(0.), top),
        None => offset.min(top),
    }
}

/// `offset` on a whole device pixel, never past the ends (a row at rest on half a pixel
/// shimmers its glyphs). The glide keeps its fractional state; only what's painted snaps.
fn snap(offset: f32, max: Option<f32>, top: f32, scale: f32) -> f32 {
    let snapped = (offset * scale).round() / scale;
    match max {
        Some(max) if snapped < -max => (-max * scale).ceil() / scale,
        _ => snapped.min(top),
    }
}

/// The driver for the scroller that tracks `handle`, with its glide in `wheel`
/// (kept in the scroller's state so it outlives the frame). Add it as the
/// first child of the scroller or beside it. It takes the vertical wheel over
/// the scroller's viewport in the capture phase, ahead of the scroller's own
/// handler, so a scroller with this driver can't hold another scroller; a
/// popover drawn over it still gets its own wheel.
pub fn driver(wheel: &Rc<RefCell<SmoothWheel>>, handle: &ScrollHandle) -> impl IntoElement {
    let (step_wheel, step_handle) = (wheel.clone(), handle.clone());
    let (listen_wheel, listen_handle) = (wheel.clone(), handle.clone());
    canvas(
        move |_, window, _| {
            let max = f32::from(step_handle.max_offset().y);
            let offset = f32::from(step_handle.offset().y);
            let step = step_wheel
                .borrow_mut()
                .step(
                    offset,
                    Some(max),
                    0.,
                    window.scale_factor(),
                    crate::motion::frame_time(window.window_handle().window_id()),
                );
            if let Some(step) = step {
                step_handle.set_offset(point(step_handle.offset().x, px(step.offset)));
                if step.more {
                    window.request_animation_frame();
                }
            }
            // The scroller's viewport.
            window.insert_hitbox(step_handle.bounds(), HitboxBehavior::Normal)
        },
        move |_, hitbox, window, _| listen(listen_wheel, listen_handle, hitbox, window),
    )
    .absolute()
    .size_0()
}

/// Takes vertical wheel motion over the scroller as a new target.
fn listen(wheel: Rc<RefCell<SmoothWheel>>, handle: ScrollHandle, hitbox: Hitbox, window: &mut Window) {
    // The view that holds the scroller may be cached, so it's the one to redraw.
    let view = window.current_view();
    window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
        if phase != DispatchPhase::Capture || !hitbox.should_handle_scroll(window) {
            return;
        }
        let delta = event.delta.pixel_delta(window.line_height());
        wheel
            .borrow_mut()
            .touch(event.touch_phase, delta != point(px(0.), px(0.)), Instant::now());
        if delta.y.abs() < delta.x.abs() {
            return;
        }
        let delta = f32::from(delta.y);
        if delta == 0. {
            return;
        }
        let max = f32::from(handle.max_offset().y);
        if max <= 0. {
            return;
        }
        wheel
            .borrow_mut()
            .nudge(
                delta,
                f32::from(handle.offset().y),
                Some(max),
                0.,
                crate::motion::frame_time(window.window_handle().window_id()),
            );
        cx.stop_propagation();
        cx.notify(view);
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    const SCALE: f32 = 2.;
    const MAX: Option<f32> = Some(1e7);

    fn after(base: Instant, secs: f64) -> Instant {
        base + Duration::from_secs_f64(secs)
    }

    /// A finger dragging at `speed` px a second for `seconds`, its events
    /// `gaps` apart (cycled) and handled `late` after the finger moved
    /// (cycled), drawn on frames `refresh` apart. The finger lifts with the
    /// last event and nothing follows. Returns each frame's painted offset.
    fn drag(speed: f64, gaps: &[f64], late: &[f64], refresh: f64, seconds: f64) -> Vec<f32> {
        let base = Instant::now() + Duration::from_secs(1);
        let mut wheel = SmoothWheel::default();
        let (mut moved, mut next, mut event) = (0., 0., 0);
        let mut painted = 0.;
        let mut drawn = Vec::new();
        let mut frame = 1;
        loop {
            let draws = f64::from(frame) * refresh;
            while next < seconds && next + late[event % late.len()] <= draws {
                let delta = speed * next - moved;
                moved += delta;
                let phase = match event {
                    0 => TouchPhase::Started,
                    _ if next + gaps[(event + 1) % gaps.len()] >= seconds => TouchPhase::Ended,
                    _ => TouchPhase::Moved,
                };
                let at = after(base, next + late[event % late.len()]);
                wheel.touch(phase, true, at);
                wheel.nudge(-delta as f32, painted, MAX, 0., after(base, draws - refresh));
                event += 1;
                next += gaps[event % gaps.len()];
            }
            let Some(step) = wheel.step(painted, MAX, 0., SCALE, after(base, draws)) else {
                break;
            };
            painted = step.offset;
            drawn.push(painted);
            if !step.more && next >= seconds {
                break;
            }
            frame += 1;
        }
        drawn
    }

    fn steps(drawn: &[f32]) -> Vec<f32> {
        drawn.windows(2).map(|pair| pair[0] - pair[1]).collect()
    }

    /// How many steps fall outside half to double the median: the editor
    /// bench's measure of an uneven drag.
    fn uneven(steps: &[f32]) -> usize {
        let mut sorted = steps.to_vec();
        sorted.sort_by(f32::total_cmp);
        let median = sorted[sorted.len() / 2];
        steps.iter().filter(|step| **step < median / 2. || **step > median * 2.).count()
    }

    #[test]
    fn a_steady_drag_steps_evenly_however_events_meet_frames() {
        // The editor bench: events every 6 ms on a 120 Hz screen.
        let drawn = drag(5000., &[1. / 165.], &[0.], 1. / 120., 1.);
        let steady = steps(&drawn[20..drawn.len() - 20]);
        assert_eq!(uneven(&steady), 0, "steps {steady:?}");
        // A trackpad's uneven events, handled up to a millisecond late, on
        // either refresh.
        let gaps = [0.006, 0.011, 0.008, 0.014, 0.007, 0.012, 0.009, 0.013, 0.010];
        let late = [0.0002, 0.0011, 0.0004, 0.0008, 0.0001];
        for refresh in [1. / 120., 1. / 165.] {
            let drawn = drag(3000., &gaps, &late, refresh, 1.2);
            let all = steps(&drawn);
            assert_eq!(uneven(&all[1..all.len() - 1]), 0, "{refresh}: steps {all:?}");
            let steady = &all[all.len() / 3..all.len() * 2 / 3];
            let expected = (3000. * refresh) as f32;
            assert!(
                steady.iter().all(|step| (step - expected).abs() / expected < 0.1),
                "{refresh}: steps {steady:?}"
            );
        }
    }

    #[test]
    fn a_drag_never_steps_back_and_ends_where_the_finger_lifted() {
        let gaps = [0.006, 0.014, 0.009];
        let drawn = drag(2000., &gaps, &[0.0005], 1. / 165., 0.5);
        assert!(steps(&drawn).iter().all(|step| *step >= 0.), "drawn {drawn:?}");
        // The events' deltas add up to where the finger was at its last event.
        let mut sent = 0.;
        let mut next = 0.;
        let mut event = 0;
        while next < 0.5 {
            sent = 2000. * next;
            event += 1;
            next += gaps[event % gaps.len()];
        }
        assert!((drawn.last().unwrap() + sent as f32).abs() <= 0.5, "{drawn:?} vs {sent}");
    }

    #[test]
    fn momentum_straight_after_the_lift_keeps_playing_then_settles() {
        let base = Instant::now();
        let mut wheel = SmoothWheel::default();
        let mut painted = 0.;
        let mut drawn = Vec::new();
        // 8 ms events: a drag of 20 px each, the lift, then a coast that slows.
        let mut deltas = vec![(TouchPhase::Started, 20.)];
        deltas.extend(std::iter::repeat_n((TouchPhase::Moved, 20.), 20));
        deltas.push((TouchPhase::Ended, 20.));
        deltas.extend((1..30).map(|n| (TouchPhase::Moved, 20. * 0.9f32.powi(n))));
        let mut frame = 1;
        let mut event = 0;
        loop {
            let draws = f64::from(frame) / 165.;
            while event < deltas.len() && event as f64 * 0.008 <= draws {
                let (phase, delta) = deltas[event];
                wheel.touch(phase, true, after(base, event as f64 * 0.008));
                wheel.nudge(-delta, painted, MAX, 0., after(base, draws));
                event += 1;
            }
            let Some(step) = wheel.step(painted, MAX, 0., SCALE, after(base, draws)) else { break };
            painted = step.offset;
            drawn.push(painted);
            if !step.more && event == deltas.len() {
                break;
            }
            frame += 1;
        }
        let total: f32 = deltas.iter().map(|(_, delta)| delta).sum();
        assert!((painted + total).abs() <= 0.5, "{painted} vs {total}");
        assert!(!wheel.is_moving(), "a settled coast drops its glide");
        let all = steps(&drawn);
        assert!(all.iter().all(|step| *step >= 0.), "{all:?}");
        // No frame jumps or stalls across the lift into the coast.
        let drag = &all[5..all.len() / 2];
        for pair in drag.windows(2) {
            assert!(pair[1] <= pair[0] * 1.5 + 0.5 && pair[1] >= pair[0] * 0.5 - 0.5, "{all:?}");
        }
    }

    #[test]
    fn a_wheel_after_a_drag_that_held_still_eases() {
        let base = Instant::now();
        let frame = |n: u32| after(base, f64::from(n) / 120.);
        let mut wheel = SmoothWheel::default();
        wheel.touch(TouchPhase::Started, true, base);
        wheel.nudge(-40., 0., MAX, 0., base);
        let mut painted = 0.;
        for n in 1..10 {
            painted = wheel.step(painted, MAX, 0., SCALE, frame(n)).unwrap().offset;
        }
        assert_eq!(painted, -40., "the finger held still on its target");
        // The lift moves nothing, so no frame runs before the wheel turns.
        wheel.touch(TouchPhase::Ended, false, frame(10));
        wheel.touch(TouchPhase::Moved, true, frame(30));
        wheel.nudge(-100., painted, MAX, 0., frame(30));
        let first = wheel.step(painted, MAX, 0., SCALE, frame(31)).unwrap();
        let eased = 100. * (1. - (-1000. / 120. / TRAIL_MS).exp());
        assert!((first.offset - (painted - eased)).abs() <= 0.5, "{} vs {}", first.offset, painted - eased);
        assert!(first.more);
    }
}
