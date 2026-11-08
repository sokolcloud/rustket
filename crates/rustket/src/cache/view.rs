//! The offline connection strip and the Settings → Storage row.

use chrono::{Local, TimeZone};
use iced::widget::{column, row, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::md::TimestampValue;
use rust_i18n::t;
use rustket_core::workspace::ConnectionState;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_3, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{BannerKind, secondary_button, spinner, status_strip};

use crate::workspace::ServerState;

/// "14:05" today, otherwise the localized long date and time.
fn saved_label(saved_at: Option<i64>, clock_24h: bool) -> Option<String> {
    let ms = saved_at?;
    let at = Local.timestamp_millis_opt(ms).single()?;
    let today = at.date_naive() == Local::now().date_naive();
    let v = TimestampValue {
        timestamp: (ms / 1000).to_string(),
        format: Some(if today { "t" } else { "f" }.into()),
    };
    Some(crate::room::render::format_timestamp(&v, clock_24h))
}

/// The connection strip while the workspace shows cached data (the saved
/// state before the first sync, or cached messages of the shown room while
/// the server is unreachable). It replaces the plain connection strip
/// (`None`: that one is shown instead).
pub fn strip<'a, M: Clone + 'a>(
    s: &'a ServerState,
    clock_24h: bool,
    retry: M,
) -> Option<Element<'a, M>> {
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
    let updating = || match &when {
        Some(w) => t!("cache.updating_from", time = w),
        None => t!("cache.updating"),
    };
    let offline = || match &when {
        Some(w) => t!("cache.offline_from", time = w),
        None => t!("cache.offline"),
    };
    let retry = Some((t!("connection.retry").into_owned(), retry));
    match ws.connection() {
        // A failed sync or start keeps its own strip (with its error).
        ConnectionState::Connected if s.sync_error.is_some() => None,
        ConnectionState::Disconnected if s.start_error.is_some() => None,
        ConnectionState::LoggedOut => None,
        ConnectionState::Connected | ConnectionState::Connecting => Some(status_strip(
            BannerKind::Info,
            updating(),
            Some(spinner(12.0)),
            None,
        )),
        ConnectionState::Reconnecting { .. } => Some(status_strip(
            BannerKind::Warning,
            offline(),
            Some(spinner(12.0)),
            retry,
        )),
        ConnectionState::Disconnected => {
            Some(status_strip(BannerKind::Warning, offline(), None, retry))
        }
    }
}

/// Settings → Storage rows with the offline cache row first (when it is
/// enabled: `Some((bytes on disk, clearing))`).
pub fn with_storage_row<'a, M: Clone + 'a>(
    cache: Option<(Option<u64>, bool)>,
    on_clear: M,
    mut rows: Vec<Element<'a, M>>,
) -> Vec<Element<'a, M>> {
    if let Some((size, clearing)) = cache {
        rows.insert(0, storage_row(size, clearing, on_clear));
    }
    rows
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
        let then = now - 400 * 86_400_000;
        let old = saved_label(Some(then), true).unwrap_or_default();
        let year = Local
            .timestamp_millis_opt(then)
            .single()
            .map(|d| d.format("%Y").to_string())
            .unwrap_or_default();
        assert!(old.contains(&year) && old.contains(':'), "{old}");
        assert_eq!(saved_label(None, true), None);
    }
}
