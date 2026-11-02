//! The shake a refusal gives its owner, next to the beep: a short damped sideways swing, like the
//! Mac login field's. One curve for every owner (the Go to line and Rename fields, the open
//! quickly field, a pane's tab bar). The owner keeps a `Shake` from the moment it refused and asks
//! for frames only while `offset` still returns a value.

use std::time::Instant;

use gpui_kit::{Pixels, px};

use crate::tokens::{SHAKE_PEAK, SHAKE_SECS, SHAKE_SWINGS};

/// A shake that began at `start` (the time the refusal is drawn at, `motion::drawn_now`).
#[derive(Clone, Copy)]
pub struct Shake {
    start: Instant,
}

impl Shake {
    pub fn new(start: Instant) -> Self {
        Self { start }
    }

    /// How far to the right of rest the owner draws at `now`, or `None` once it has landed, so the
    /// caller stops asking for frames. `SHAKE_SWINGS` full swings over `SHAKE_SECS`, the peak
    /// `SHAKE_PEAK` falling away linearly to 0 as it ends.
    pub fn offset(&self, now: Instant) -> Option<Pixels> {
        let t = now.saturating_duration_since(self.start).as_secs_f32() / SHAKE_SECS;
        (t < 1.).then(|| px(offset_at(t)))
    }
}

/// The offset in points at `t`, 0 to 1 through the shake.
fn offset_at(t: f32) -> f32 {
    SHAKE_PEAK * (1. - t) * (std::f32::consts::TAU * SHAKE_SWINGS * t).sin()
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Shake, offset_at};
    use crate::tokens::{SHAKE_PEAK, SHAKE_SECS};

    #[test]
    fn starts_and_lands_on_zero_and_ends() {
        assert_eq!(offset_at(0.), 0.);
        assert!(offset_at(0.999).abs() < 0.05);
        let start = Instant::now();
        let shake = Shake::new(start);
        assert!(shake.offset(start + Duration::from_millis(40)).is_some());
        assert!(shake.offset(start + Duration::from_secs_f32(SHAKE_SECS + 0.01)).is_none());
    }

    #[test]
    fn never_passes_the_peak_and_decays() {
        let samples: Vec<f32> = (0..=350).map(|i| offset_at(i as f32 / 350.)).collect();
        assert!(samples.iter().all(|v| v.abs() <= SHAKE_PEAK));
        let early = samples[..175].iter().fold(0f32, |m, v| m.max(v.abs()));
        let late = samples[175..].iter().fold(0f32, |m, v| m.max(v.abs()));
        assert!(late < early);
    }
}
