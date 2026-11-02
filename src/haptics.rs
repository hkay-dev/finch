//! The trackpad's ticks: a soft one for a snap (a divider at its rest width, a dragged tab over a new
//! drop zone, a pinch step) and a firmer one for landing on a usual value. AppKit does nothing without a Force Touch trackpad and follows the system setting.

use objc2::{msg_send, rc::Retained, runtime::{AnyClass, AnyObject}};

/// `NSHapticFeedbackPatternAlignment`.
const ALIGNMENT: isize = 1;
/// `NSHapticFeedbackPatternLevelChange`.
const LEVEL_CHANGE: isize = 2;
/// `NSHapticFeedbackPerformanceTimeNow`.
const NOW: usize = 1;

/// One alignment tick, now. Call it once per change, never per mouse move.
pub fn align() {
    perform(ALIGNMENT);
}

/// One firmer tick, now: a value that is the usual one (the pinch landing on Actual size).
pub fn level_change() {
    perform(LEVEL_CHANGE);
}

fn perform(pattern: isize) {
    let Some(class) = AnyClass::get(c"NSHapticFeedbackManager") else { return };
    // SAFETY: `defaultPerformer` returns the shared `NSHapticFeedbackPerformer` (a protocol object), and
    // `performFeedbackPattern:performanceTime:` takes an NSInteger and an NSUInteger and returns nothing.
    unsafe {
        let performer: Option<Retained<AnyObject>> = msg_send![class, defaultPerformer];
        if let Some(performer) = performer {
            let _: () = msg_send![&*performer, performFeedbackPattern: pattern, performanceTime: NOW];
        }
    }
}
