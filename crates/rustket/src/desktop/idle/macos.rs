//! macOS idle time: seconds since the last input event of any type in the
//! combined session state (`CGEventSourceSecondsSinceLastEventType`).

use std::time::Duration;

use objc2_core_graphics::{CGEventSource, CGEventSourceStateID, CGEventType};

/// `kCGAnyInputEventType`.
const ANY_INPUT: CGEventType = CGEventType(u32::MAX);

/// Never blocks: no thread or timeout needed.
pub fn query_idle() -> Option<Duration> {
    let secs = CGEventSource::seconds_since_last_event_type(
        CGEventSourceStateID::CombinedSessionState,
        ANY_INPUT,
    );
    Duration::try_from_secs_f64(secs).ok()
}
