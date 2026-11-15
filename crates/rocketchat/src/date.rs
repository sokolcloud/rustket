//! Dates as sent by Rocket.Chat: ISO-8601 strings over REST, `{"$date": ms}` over DDP.

use std::fmt;

use chrono::{DateTime, NaiveDate, Offset, SecondsFormat, TimeZone, Utc};
use serde::de::{self, MapAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A UTC timestamp that deserialises from both REST and DDP representations.
///
/// Serialises as an ISO-8601 string with milliseconds (`2026-11-07T15:55:01.324Z`);
/// use [`RcDate::ddp`] or the [`ddp_date`] serde module for `{"$date": ms}`.
///
/// Parsed values are limited to years 0001..=9999 ([`MIN_MILLIS`]..=[`MAX_MILLIS`]):
/// chrono's own range ends where adding a UTC offset or a margin panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RcDate(pub DateTime<Utc>);

/// `0001-01-01T00:00:00.000Z`.
pub const MIN_MILLIS: i64 = -62_135_596_800_000;
/// `9999-12-31T23:59:59.999Z`.
pub const MAX_MILLIS: i64 = 253_402_300_799_999;

/// Calendar date of `d` in its own timezone; `None` where chrono's
/// `date_naive()` would panic (local time outside chrono's range).
pub fn local_date<Tz: TimeZone>(d: &DateTime<Tz>) -> Option<NaiveDate> {
    d.naive_utc()
        .checked_add_offset(d.offset().fix())
        .map(|n| n.date())
}

impl RcDate {
    pub fn now() -> Self {
        Self(Utc::now())
    }

    /// `None` outside [`MIN_MILLIS`]..=[`MAX_MILLIS`].
    pub fn from_millis(ms: i64) -> Option<Self> {
        if !(MIN_MILLIS..=MAX_MILLIS).contains(&ms) {
            return None;
        }
        Utc.timestamp_millis_opt(ms).single().map(Self)
    }

    pub fn timestamp_millis(&self) -> i64 {
        self.0.timestamp_millis()
    }

    pub fn to_iso(&self) -> String {
        self.0.to_rfc3339_opts(SecondsFormat::Millis, true)
    }

    pub fn parse_iso(s: &str) -> Option<Self> {
        let d = DateTime::parse_from_rfc3339(s).ok()?;
        // An offset can move a four-digit year out of the accepted range.
        Self::from_millis(d.timestamp_millis())
    }

    /// Wrapper that serialises as `{"$date": ms}` (EJSON, for DDP method params).
    pub fn ddp(self) -> DdpDate {
        DdpDate(self)
    }
}

impl From<DateTime<Utc>> for RcDate {
    fn from(d: DateTime<Utc>) -> Self {
        Self(d)
    }
}

impl From<RcDate> for DateTime<Utc> {
    fn from(d: RcDate) -> Self {
        d.0
    }
}

impl fmt::Display for RcDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_iso())
    }
}

impl Serialize for RcDate {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_iso())
    }
}

impl<'de> Deserialize<'de> for RcDate {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        d.deserialize_any(RcDateVisitor)
    }
}

struct RcDateVisitor;

/// Bare numbers below 10^11 are taken as seconds (10^11 ms is March 1973, 10^11 s
/// is year 5138); `{"$date": n}` is always milliseconds.
fn guess_millis(v: i64) -> i64 {
    if v.unsigned_abs() < 100_000_000_000 {
        v.saturating_mul(1000)
    } else {
        v
    }
}

impl<'de> Visitor<'de> for RcDateVisitor {
    type Value = RcDate;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("an ISO-8601 date string, {\"$date\": ms} or a millisecond timestamp")
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<RcDate, E> {
        if let Some(d) = RcDate::parse_iso(v) {
            return Ok(d);
        }
        // Legacy integrations store numeric strings (seconds or milliseconds).
        let t = v.trim();
        if let Ok(n) = t.parse::<i64>() {
            return self.visit_i64(n);
        }
        if let Ok(f) = t.parse::<f64>() {
            return self.visit_f64(f);
        }
        Err(E::custom(format!("invalid date: {v}")))
    }

    fn visit_i64<E: de::Error>(self, v: i64) -> Result<RcDate, E> {
        RcDate::from_millis(guess_millis(v)).ok_or_else(|| E::custom("timestamp out of range"))
    }

    fn visit_u64<E: de::Error>(self, v: u64) -> Result<RcDate, E> {
        let v = i64::try_from(v).map_err(|_| E::custom("timestamp out of range"))?;
        self.visit_i64(v)
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<RcDate, E> {
        if !v.is_finite() {
            return Err(E::custom("timestamp is not finite"));
        }
        #[allow(clippy::cast_possible_truncation)]
        self.visit_i64(v.round() as i64)
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<RcDate, A::Error> {
        let mut result = None;
        while let Some(key) = map.next_key::<String>()? {
            if key == "$date" {
                let ms = map.next_value::<f64>()?;
                if !ms.is_finite() {
                    return Err(de::Error::custom("timestamp is not finite"));
                }
                #[allow(clippy::cast_possible_truncation)]
                let ms = ms.round() as i64;
                result = Some(
                    RcDate::from_millis(ms)
                        .ok_or_else(|| de::Error::custom("timestamp out of range"))?,
                );
            } else {
                map.next_value::<de::IgnoredAny>()?;
            }
        }
        result.ok_or_else(|| de::Error::missing_field("$date"))
    }
}

/// An [`RcDate`] that serialises as `{"$date": ms}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize)]
#[serde(transparent)]
pub struct DdpDate(pub RcDate);

impl Serialize for DdpDate {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(1))?;
        m.serialize_entry("$date", &self.0.timestamp_millis())?;
        m.end()
    }
}

/// `#[serde(with = "rocketchat::date::ddp_date")]` for `RcDate` fields sent over DDP.
pub mod ddp_date {
    use super::{DdpDate, RcDate};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer>(d: &RcDate, s: S) -> Result<S::Ok, S::Error> {
        DdpDate(*d).serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<RcDate, D::Error> {
        RcDate::deserialize(d)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn parses_iso_and_ddp() {
        let a: RcDate = serde_json::from_str("\"2026-11-07T15:55:01.324Z\"").unwrap();
        let b: RcDate = serde_json::from_str("{\"$date\": 1794067677488}").unwrap();
        let c: RcDate = serde_json::from_str("1794067677488").unwrap();
        let d: RcDate = serde_json::from_str("{\"$date\": 1794067677488.0}").unwrap();
        assert_eq!(a.timestamp_millis(), 1_794_066_901_324);
        assert_eq!(b, c);
        assert_eq!(b, d);
        assert_eq!(b.to_iso(), "2026-11-07T16:07:57.488Z");
    }

    #[test]
    fn serialises_both_forms() {
        let d = RcDate::from_millis(1_794_067_677_488).unwrap();
        assert_eq!(
            serde_json::to_string(&d).unwrap(),
            "\"2026-11-07T16:07:57.488Z\""
        );
        assert_eq!(
            serde_json::to_string(&d.ddp()).unwrap(),
            "{\"$date\":1794067677488}"
        );
        #[derive(Serialize)]
        struct P {
            #[serde(with = "ddp_date")]
            ts: RcDate,
        }
        assert_eq!(
            serde_json::to_string(&P { ts: d }).unwrap(),
            "{\"ts\":{\"$date\":1794067677488}}"
        );
    }

    #[test]
    fn numeric_strings_and_seconds() {
        let s: RcDate = serde_json::from_str("\"1481302386\"").unwrap();
        assert_eq!(s.timestamp_millis(), 1_481_302_386_000);
        let ms: RcDate = serde_json::from_str("\"1481302386123\"").unwrap();
        assert_eq!(ms.timestamp_millis(), 1_481_302_386_123);
        let n: RcDate = serde_json::from_str("1481302386").unwrap();
        assert_eq!(n, s);
        let small: RcDate = serde_json::from_str("{\"$date\": 1000}").unwrap();
        assert_eq!(small.timestamp_millis(), 1000);
    }

    #[test]
    fn extreme_values_do_not_panic() {
        for j in [
            "-9223372036854775808",
            "9223372036854775807",
            "\"-9223372036854775808\"",
            "-1e19",
            "1e300",
            "{\"$date\": -1e19}",
            "{\"$date\": -9223372036854775808}",
        ] {
            let r = std::panic::catch_unwind(|| serde_json::from_str::<RcDate>(j));
            assert!(r.is_ok(), "panicked on {j}");
        }
    }

    /// One second inside chrono's limits: a local offset used to overflow
    /// `date_naive()` and panic the UI thread.
    #[test]
    fn rejects_dates_outside_the_supported_range() {
        for ms in [8_210_266_876_799_000_i64, -8_334_601_228_800_000] {
            assert!(RcDate::from_millis(ms).is_none(), "{ms}");
            for j in [
                format!("{ms}"),
                format!("\"{ms}\""),
                format!("{ms}.0"),
                format!("{{\"$date\": {ms}}}"),
                format!("{{\"$date\": {ms}.0}}"),
            ] {
                assert!(serde_json::from_str::<RcDate>(&j).is_err(), "{j}");
            }
        }
        assert!(RcDate::from_millis(MIN_MILLIS - 1).is_none());
        assert!(RcDate::from_millis(MAX_MILLIS + 1).is_none());
        let min = RcDate::from_millis(MIN_MILLIS).unwrap();
        let max = RcDate::from_millis(MAX_MILLIS).unwrap();
        assert_eq!(min.to_iso(), "0001-01-01T00:00:00.000Z");
        assert_eq!(max.to_iso(), "9999-12-31T23:59:59.999Z");
        assert_eq!(RcDate::parse_iso(&min.to_iso()), Some(min));
        assert_eq!(RcDate::parse_iso(&max.to_iso()), Some(max));
        // The offset moves these past either end.
        assert!(RcDate::parse_iso("9999-12-31T23:59:59-01:00").is_none());
        assert!(RcDate::parse_iso("0001-01-01T00:00:00+01:00").is_none());
        assert!(serde_json::from_str::<RcDate>("\"9999-12-31T23:59:59-01:00\"").is_err());
    }

    #[test]
    fn local_date_does_not_panic_at_chrono_limits() {
        use chrono::FixedOffset;
        let east = FixedOffset::east_opt(3 * 3600).unwrap();
        let west = FixedOffset::west_opt(5 * 3600).unwrap();
        let late = DateTime::from_timestamp_millis(8_210_266_876_799_000).unwrap();
        let early = DateTime::from_timestamp_millis(-8_334_601_228_800_000).unwrap();
        assert_eq!(local_date(&late.with_timezone(&east)), None);
        assert_eq!(local_date(&early.with_timezone(&west)), None);
        assert_eq!(local_date(&late), Some(late.date_naive()));
        assert_eq!(
            local_date(&early.with_timezone(&east)),
            Some(early.date_naive())
        );
        // Every accepted date has a local date in any offset.
        for ms in [MIN_MILLIS, MAX_MILLIS] {
            let d = RcDate::from_millis(ms).unwrap().0;
            for tz in [east, west] {
                let l = d.with_timezone(&tz);
                assert_eq!(local_date(&l), Some(l.date_naive()));
            }
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(serde_json::from_str::<RcDate>("\"yesterday\"").is_err());
        assert!(serde_json::from_str::<RcDate>("{\"x\": 1}").is_err());
        assert!(serde_json::from_str::<RcDate>("true").is_err());
    }

    #[test]
    fn non_utc_offset() {
        let d = RcDate::parse_iso("2026-11-07T18:00:00+03:00").unwrap();
        assert_eq!(d.to_iso(), "2026-11-07T15:00:00.000Z");
    }
}
