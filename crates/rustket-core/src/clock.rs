//! Local time of other people (`utcOffset`, hours east of UTC) and the
//! offset the client publishes for its own user.

use chrono::{DateTime, FixedOffset, Timelike, Utc};

/// Offsets closer than this to one's own are not worth a hint.
const MIN_DIFFERENCE_SECS: i32 = 30 * 60;
/// Largest offset in use (UTC+14, UTC-12).
const MAX_OFFSET_SECS: i32 = 14 * 3600;

/// Someone's clock relative to ours.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerClock {
    pub offset_secs: i32,
    /// Their local time is between 22:00 and 07:59.
    pub night: bool,
}

fn offset_secs(hours: f64) -> Option<i32> {
    if !hours.is_finite() {
        return None;
    }
    let secs = (hours * 3600.0).round();
    (secs.abs() <= f64::from(MAX_OFFSET_SECS)).then_some(secs as i32)
}

/// The clock of someone at `offset_hours`; `None` when the offset is invalid
/// or within 30 minutes of `my_offset_secs`.
pub fn peer_clock(offset_hours: f64, my_offset_secs: i32, now: DateTime<Utc>) -> Option<PeerClock> {
    let secs = offset_secs(offset_hours)?;
    if (secs - my_offset_secs).abs() < MIN_DIFFERENCE_SECS {
        return None;
    }
    let tz = FixedOffset::east_opt(secs)?;
    let hour = now.with_timezone(&tz).hour();
    Some(PeerClock {
        offset_secs: secs,
        night: !(8..22).contains(&hour),
    })
}

/// The offset (hours) to publish when the server's `stored` value differs
/// from the local offset (or is missing); `None` when it is up to date.
pub fn offset_to_publish(stored: Option<f64>, local_offset_secs: i32) -> Option<f64> {
    if stored.and_then(offset_secs) == Some(local_offset_secs) {
        return None;
    }
    Some(f64::from(local_offset_secs) / 3600.0)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn utc(h: u32, m: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 15, h, m, 0).unwrap()
    }

    #[test]
    fn peer_clocks() {
        assert_eq!(peer_clock(0.0, 0, utc(12, 0)), None);
        // A quarter of an hour apart: no hint.
        assert_eq!(peer_clock(0.25, 0, utc(12, 0)), None);
        let c = peer_clock(3.0, 0, utc(12, 0)).unwrap();
        assert_eq!(c.offset_secs, 10_800);
        assert!(!c.night);
        // UTC+3: 21:59 day, 22:00 night, 07:59 night, 08:00 day.
        assert!(!peer_clock(3.0, 0, utc(18, 59)).unwrap().night);
        assert!(peer_clock(3.0, 0, utc(19, 0)).unwrap().night);
        assert!(peer_clock(3.0, 0, utc(4, 59)).unwrap().night);
        assert!(!peer_clock(3.0, 0, utc(5, 0)).unwrap().night);
        assert_eq!(peer_clock(5.75, 0, utc(0, 0)).unwrap().offset_secs, 20_700);
        assert_eq!(peer_clock(-3.5, 0, utc(0, 0)).unwrap().offset_secs, -12_600);
        assert_eq!(peer_clock(f64::NAN, 0, utc(0, 0)), None);
        assert_eq!(peer_clock(15.0, 0, utc(0, 0)), None);
        assert_eq!(peer_clock(-15.0, 0, utc(0, 0)), None);
    }

    #[test]
    fn publish_only_when_different() {
        assert_eq!(offset_to_publish(None, 25_200), Some(7.0));
        assert_eq!(offset_to_publish(Some(7.0), 25_200), None);
        assert_eq!(offset_to_publish(Some(5.5), 19_800), None);
        assert_eq!(offset_to_publish(Some(3.0), 19_800), Some(5.5));
        assert_eq!(offset_to_publish(Some(f64::NAN), 0), Some(0.0));
    }
}
