//! `timespan` settings: integer milliseconds edited as a number plus a unit
//! (docs/admin.md §14.5, web `client/lib/convertTimeUnit.ts`).

const MINUTE: u64 = 60_000;
const HOUR: u64 = 60 * MINUTE;
const DAY: u64 = 24 * HOUR;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimeUnit {
    Days,
    Hours,
    Minutes,
}

impl TimeUnit {
    pub const ALL: [Self; 3] = [Self::Days, Self::Hours, Self::Minutes];

    pub fn millis(self) -> u64 {
        match self {
            Self::Days => DAY,
            Self::Hours => HOUR,
            Self::Minutes => MINUTE,
        }
    }
}

/// Initial unit for `ms`: minutes if not a whole number of hours, hours if not a
/// whole number of days, else days (`getHighestTimeUnit`).
pub fn highest_unit(ms: u64) -> TimeUnit {
    if !ms.is_multiple_of(HOUR) {
        TimeUnit::Minutes
    } else if !ms.is_multiple_of(DAY) {
        TimeUnit::Hours
    } else {
        TimeUnit::Days
    }
}

/// `ms` expressed in `unit` (may be fractional for minutes when `ms` is not a whole
/// number of minutes).
pub fn from_ms(ms: u64, unit: TimeUnit) -> f64 {
    ms as f64 / unit.millis() as f64
}

/// `n × unit` in milliseconds; negative or non-finite input is clamped to 0 and
/// fractions are truncated like the web input.
pub fn to_ms(n: f64, unit: TimeUnit) -> u64 {
    if !n.is_finite() || n <= 0.0 {
        return 0;
    }
    (n.trunc() as u64).saturating_mul(unit.millis())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units() {
        assert_eq!(highest_unit(0), TimeUnit::Days);
        assert_eq!(highest_unit(2 * DAY), TimeUnit::Days);
        assert_eq!(highest_unit(30 * HOUR), TimeUnit::Hours);
        assert_eq!(highest_unit(90 * MINUTE), TimeUnit::Minutes);
        assert_eq!(highest_unit(1500), TimeUnit::Minutes);
        assert_eq!(from_ms(30 * HOUR, TimeUnit::Hours), 30.0);
        assert_eq!(from_ms(2 * DAY, TimeUnit::Days), 2.0);
        assert_eq!(to_ms(3.0, TimeUnit::Days), 3 * DAY);
        assert_eq!(to_ms(2.7, TimeUnit::Minutes), 2 * MINUTE);
        assert_eq!(to_ms(-1.0, TimeUnit::Hours), 0);
        assert_eq!(to_ms(f64::NAN, TimeUnit::Hours), 0);
    }
}
