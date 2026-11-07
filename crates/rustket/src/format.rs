//! Formatting helpers for times and counts.

use chrono::{DateTime, Datelike, Local, TimeZone};
use rocketchat::RcDate;
use rust_i18n::t;

/// Sidebar time: `14:05` today, `Yesterday`, weekday within a week, else date.
pub fn short_time(ts: RcDate, now: DateTime<Local>, clock_24h: bool) -> String {
    let Some(local) = Local.timestamp_millis_opt(ts.timestamp_millis()).single() else {
        return String::new();
    };
    let days = now
        .date_naive()
        .signed_duration_since(local.date_naive())
        .num_days();
    if days <= 0 {
        if clock_24h {
            local.format("%H:%M").to_string()
        } else {
            local.format("%-I:%M %p").to_string()
        }
    } else if days == 1 {
        t!("app.yesterday").into_owned()
    } else if days < 7 {
        t!(format!(
            "app.weekday_{}",
            local.weekday().num_days_from_monday()
        ))
        .into_owned()
    } else if local.year() == now.year() {
        local.format("%d.%m").to_string()
    } else {
        local.format("%d.%m.%Y").to_string()
    }
}

/// Clamp a counter for badges.
pub fn count_u32(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}
