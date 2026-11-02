use std::{cell::Cell, rc::Rc, time::{Duration, Instant}};

use gpui::{AnyWindowHandle, Bounds, Context, Pixels, Point, WeakEntity, Window, point, px};

#[derive(Clone, Copy, Default)]
struct ScrollState {
    velocity: Option<Point<Pixels>>,
    generation: u64,
}

/// Frame-driven drag scrolling. Velocity is in pixels per second; positive
/// moves toward the bottom/right (callers adapt their scroll-handle signs).
#[derive(Default)]
pub struct AutoScroll {
    shared: Rc<Cell<ScrollState>>,
    /// Last drag position, available to the interaction that owns selection.
    pub last_drag_position: Option<Point<Pixels>>,
}

impl AutoScroll {
    fn axis_velocity(position: Pixels, start: Pixels, extent: Pixels) -> Pixels {
        let distance = if position < start {
            position - start
        } else if position > start + extent {
            position - start - extent
        } else {
            return px(0.);
        };
        // Two viewports/second at 60pt past the edge, capped at six.
        extent.max(px(0.)) * (f32::from(distance) / 30.).clamp(-6., 6.)
    }

    /// Vertical drag speed, zero at and inside the clipping viewport.
    pub fn vertical_velocity(y: Pixels, bounds: Bounds<Pixels>) -> Option<Pixels> {
        let velocity = Self::axis_velocity(y, bounds.top(), bounds.size.height);
        (velocity != px(0.)).then_some(velocity)
    }

    /// Two-axis speed. Wrapped content opts out of sideways scrolling.
    pub fn compute_velocity(
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        horizontal: bool,
    ) -> Option<Point<Pixels>> {
        let velocity = point(
            if horizontal {
                Self::axis_velocity(position.x, bounds.left(), bounds.size.width)
            } else {
                px(0.)
            },
            Self::axis_velocity(position.y, bounds.top(), bounds.size.height),
        );
        (velocity != point(px(0.), px(0.))).then_some(velocity)
    }

    fn frame_delta(velocity: Point<Pixels>, elapsed: Duration) -> Point<Pixels> {
        // Do not jump several screens after suspension or a stalled frame.
        velocity * elapsed.min(Duration::from_millis(50)).as_secs_f32()
    }

    /// Update speed and link one callback to each window frame while active.
    /// A deferred first registration avoids re-borrowing the event's window.
    pub fn set<T, F>(
        &mut self,
        velocity: Option<Point<Pixels>>,
        window: AnyWindowHandle,
        cx: &mut Context<T>,
        tick: F,
    ) where
        T: 'static,
        F: Fn(Point<Pixels>, &mut T, &mut Context<T>) + 'static,
    {
        let mut state = self.shared.get();
        let was_idle = state.velocity.is_none();
        state.velocity = velocity;
        if velocity.is_none() {
            state.generation = state.generation.wrapping_add(1);
        }
        self.shared.set(state);
        if was_idle && velocity.is_some() {
            let shared = self.shared.clone();
            let entity = cx.weak_entity();
            let generation = state.generation;
            cx.defer(move |cx| {
                _ = window.update(cx, |_, window, _| {
                    Self::next_frame(window, entity, shared, generation, Instant::now(), tick);
                });
            });
        }
    }

    fn next_frame<T, F>(
        window: &Window,
        entity: WeakEntity<T>,
        shared: Rc<Cell<ScrollState>>,
        generation: u64,
        previous: Instant,
        tick: F,
    ) where
        T: 'static,
        F: Fn(Point<Pixels>, &mut T, &mut Context<T>) + 'static,
    {
        window.on_next_frame(move |window, cx| {
            let state = shared.get();
            let Some(velocity) = state.velocity.filter(|_| state.generation == generation) else {
                return;
            };
            let now = Instant::now();
            let delta = Self::frame_delta(velocity, now.duration_since(previous));
            if entity.update(cx, |state, cx| tick(delta, state, cx)).is_err() {
                return;
            }
            let state = shared.get();
            if state.velocity.is_some() && state.generation == generation {
                Self::next_frame(window, entity, shared, generation, now, tick);
            }
        });
    }

    pub fn is_active(&self) -> bool {
        self.shared.get().velocity.is_some()
    }

    /// Invalidate queued callbacks, including across a stop/restart.
    pub fn stop(&mut self) {
        let generation = self.shared.get().generation.wrapping_add(1);
        self.shared.set(ScrollState { velocity: None, generation });
        self.last_drag_position = None;
    }
}

impl Drop for AutoScroll {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use gpui::size;
    use super::*;

    #[test]
    fn speed_is_proportional_symmetric_and_capped() {
        let bounds = Bounds::new(point(px(0.), px(100.)), size(px(200.), px(100.)));
        for y in [100., 150., 200.] {
            assert_eq!(AutoScroll::vertical_velocity(px(y), bounds), None);
        }
        assert_eq!(AutoScroll::vertical_velocity(px(230.), bounds), Some(px(100.)));
        assert_eq!(AutoScroll::vertical_velocity(px(260.), bounds), Some(px(200.)));
        assert_eq!(AutoScroll::vertical_velocity(px(40.), bounds), Some(px(-200.)));
        assert_eq!(AutoScroll::vertical_velocity(px(1000.), bounds), Some(px(600.)));
        assert_eq!(AutoScroll::compute_velocity(point(px(260.), px(150.)), bounds, false), None);
        assert_eq!(AutoScroll::compute_velocity(point(px(260.), px(150.)), bounds, true),
            Some(point(px(400.), px(0.))));
    }

    #[test]
    fn frame_distance_tracks_elapsed_time_not_refresh_rate() {
        let velocity = point(px(120.), px(240.));
        let fast = AutoScroll::frame_delta(velocity, Duration::from_secs_f32(1. / 120.));
        let slow = AutoScroll::frame_delta(velocity, Duration::from_secs_f32(1. / 60.));
        assert!((f32::from(fast.y * 2. - slow.y)).abs() < 0.001);
        assert_eq!(AutoScroll::frame_delta(velocity, Duration::from_secs(2)),
            point(px(6.), px(12.)));
    }

    #[test]
    fn stop_invalidates_callbacks_and_clears_drag_position() {
        let mut scroll = AutoScroll::default();
        scroll.shared.set(ScrollState { velocity: Some(point(px(1.), px(2.))), generation: 7 });
        scroll.last_drag_position = Some(point(px(1.), px(2.)));
        scroll.stop();
        assert!(!scroll.is_active());
        assert_eq!(scroll.shared.get().generation, 8);
        assert_eq!(scroll.last_drag_position, None);
    }
}
