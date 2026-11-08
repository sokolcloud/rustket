//! The offline indicator above the room and the Settings → Storage row.

use chrono::{Local, TimeZone};
use iced::widget::{column, row, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::workspace::ConnectionState;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_3, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{BannerKind, secondary_button, status_strip};

use crate::workspace::ServerState;

/// "from 14:05" / "from 3 Nov, 14:05".
fn saved_label(saved_at: Option<i64>, clock_24h: bool) -> Option<String> {
    let at = Local.timestamp_millis_opt(saved_at?).single()?;
    let now = Local::now();
    let time = if clock_24h {
        at.format("%H:%M").to_string()
    } else {
        at.format("%-I:%M %p").to_string()
    };
    Some(if at.date_naive() == now.date_naive() {
        time
    } else {
        format!("{}, {time}", at.format("%-d %b"))
    })
}

/// The strip shown while the workspace shows cached data: the saved state
/// before the first sync, or cached messages of the shown room while the
/// server is unreachable.
pub fn strip<'a, M: Clone + 'a>(s: &'a ServerState, clock_24h: bool) -> Option<Element<'a, M>> {
    let ws = &s.workspace;
    let connected = *ws.connection() == ConnectionState::Connected;
    let room_cached = s
        .active_room
        .as_deref()
        .and_then(|r| ws.timeline(r))
        .is_some_and(|t| t.has_seeded());
    if !ws.from_cache() && (connected || !room_cached) {
        return None;
    }
    let when = saved_label(s.cache.saved_at, clock_24h);
    let (kind, message) = match ws.connection() {
        ConnectionState::Connected | ConnectionState::Connecting => (
            BannerKind::Info,
            match when {
                Some(w) => t!("cache.updating_from", time = w),
                None => t!("cache.updating"),
            },
        ),
        _ => (
            BannerKind::Warning,
            match when {
                Some(w) => t!("cache.offline_from", time = w),
                None => t!("cache.offline"),
            },
        ),
    };
    Some(status_strip(kind, message, None, None))
}

/// Settings → Storage: size of the offline cache and its "Clear" button.
pub fn storage_row<'a, M: Clone + 'a>(
    size: Option<u64>,
    clearing: bool,
    on_clear: M,
) -> Element<'a, M> {
    let help = match size {
        Some(n) => t!(
            "cache.settings_help_size",
            size = crate::room::upload::human_size(n)
        ),
        None => t!("cache.settings_help"),
    };
    row![
        column![
            text(t!("cache.settings_title")).size(TEXT_BODY),
            text(help).size(TEXT_CAPTION).style(style::text_secondary),
        ]
        .spacing(SPACE_1)
        .width(Length::Fill),
        secondary_button(t!("cache.settings_clear"), (!clearing).then_some(on_clear)),
    ]
    .spacing(SPACE_3)
    .align_y(Alignment::Center)
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_label_formats() {
        let now = Local::now().timestamp_millis();
        let today = saved_label(Some(now), true).unwrap_or_default();
        assert_eq!(today.len(), 5, "{today}");
        let old = saved_label(Some(now - 10 * 86_400_000), true).unwrap_or_default();
        assert!(old.contains(", "), "{old}");
        assert_eq!(saved_label(None, true), None);
    }
}
