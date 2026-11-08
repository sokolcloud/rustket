//! Formatting helpers for times, dates, counts and one-line text previews.

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
        localized(&local, "app.date_short_fmt")
    } else {
        full_date(&local)
    }
}

/// Date with the year in the UI locale (`Nov 8, 2026` / `08.11.2026`).
pub fn full_date(local: &DateTime<Local>) -> String {
    localized(local, "app.date_full_fmt")
}

/// `local` formatted with the chrono format stored under `key`.
fn localized(local: &DateTime<Local>, key: &str) -> String {
    localized_in(local, key, &rust_i18n::locale())
}

fn localized_in(local: &DateTime<Local>, key: &str, locale: &str) -> String {
    use std::fmt::Write as _;
    let fmt = t!(key, locale = locale);
    let mut out = String::new();
    // A broken translation must not panic in `to_string`.
    if write!(out, "{}", local.format(&fmt)).is_err() {
        out = local.format("%Y-%m-%d").to_string();
    }
    out
}

/// One line of plain text from a markdown text (room topic or
/// announcement): markup removed, spoilers masked, `<t:…>` timestamps in
/// local time, line breaks collapsed.
pub fn md_one_line(text: &str, clock_24h: bool) -> String {
    use rocketchat::models::md::{Block, Inline};
    fn walk(i: &Inline, out: &mut String, clock_24h: bool) {
        match i {
            Inline::Spoiler { .. } => out.push_str("•••"),
            Inline::Bold { value } | Inline::Italic { value } | Inline::Strike { value } => {
                value.iter().for_each(|i| walk(i, out, clock_24h));
            }
            Inline::Timestamp { value, .. } => {
                out.push_str(&crate::room::render::format_timestamp(value, clock_24h));
            }
            other => other.plain_text(out),
        }
    }
    let inline = |i: &Inline, out: &mut String| walk(i, out, clock_24h);
    let mut out = String::new();
    for b in crate::e2e::markdown::parse(text) {
        let start = out.len();
        if !out.is_empty() {
            out.push(' ');
        }
        match &b {
            Block::Paragraph { value } | Block::Heading { value, .. } => {
                value.iter().for_each(|i| inline(i, &mut out));
            }
            Block::Quote { value } => value.iter().for_each(|b| {
                if let Block::Paragraph { value } = b {
                    value.iter().for_each(|i| inline(i, &mut out));
                    out.push(' ');
                }
            }),
            Block::LineBreak => {}
            other => other.plain_text(&mut out),
        }
        if out[start..].trim().is_empty() {
            out.truncate(start);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Clamp a counter for badges.
pub fn count_u32(n: u64) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn at(y: i32, m: u32, d: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, 12, 0, 0).single().unwrap()
    }

    /// Older sidebar dates follow the locale: `08.11` is read as August 11
    /// in English.
    #[test]
    fn dates_follow_the_locale() {
        // The global locale is shared with parallel tests: pass it explicitly.
        let d = at(2025, 11, 8);
        let both = |key: &str| (localized_in(&d, key, "en"), localized_in(&d, key, "ru"));
        assert_eq!(both("app.date_short_fmt"), ("Nov 8".into(), "08.11".into()));
        assert_eq!(
            both("app.date_full_fmt"),
            ("Nov 8, 2025".into(), "08.11.2025".into())
        );
        let now = at(2026, 11, 20);
        let old = RcDate::from_millis(d.timestamp_millis()).unwrap();
        let shown = short_time(old, now, true);
        assert!(shown == "Nov 8, 2025" || shown == "08.11.2025", "{shown}");
    }

    #[test]
    fn markdown_one_liners() {
        assert_eq!(
            md_one_line(
                "Release **0.2** is _frozen_\n\nsee ||secret|| [notes](https://x.org)",
                true
            ),
            "Release 0.2 is frozen see ••• notes"
        );
        assert_eq!(md_one_line("# Title\n> quoted", true), "Title quoted");
        assert_eq!(md_one_line("  ", true), "");
        let line = md_one_line("Freeze at <t:1767225600:f>", true);
        assert!(
            !line.contains("1767225600") && line.contains(" 202"),
            "{line}"
        );
    }
}
