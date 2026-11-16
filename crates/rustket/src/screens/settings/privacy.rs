//! Privacy & storage tab: where login tokens and E2E keys are kept (and
//! whether they may go to a file), what the profile shares, the offline
//! cache and the image cache.

use iced::widget::{container, text};
use iced::{Element, Length};
use rust_i18n::t;
use rustket_core::secrets::SecretBackend;
use rustket_ui::theme::TEXT_BODY;
use rustket_ui::widget::{BannerKind, banner};

use super::widgets::{action_row, caption, field, section, segmented, switch};
use super::{IMAGE_CACHE_CHOICES, Message, SECRET_STORE_CARD, STORAGE_CARD, SettingsView};

pub(super) fn view<'a>(v: &SettingsView<'a>) -> Vec<Element<'a, Message>> {
    vec![secret_store_card(v), profile_card(v), storage_card(v)]
}

/// What Rustket publishes to the user's profile (the local time zone).
fn profile_card<'a>(v: &SettingsView<'a>) -> Element<'a, Message> {
    section(
        t!("settings.profile_sharing").into_owned(),
        vec![switch(
            t!("settings.share_utc_offset").into_owned(),
            Some(t!("settings.share_utc_offset_help").into_owned()),
            v.config.ui.share_utc_offset,
            Some(Message::SetShareUtcOffset as fn(bool) -> Message),
        )],
    )
}

/// The secret store in use (the file and the no-directory fallbacks warn)
/// and the "never store secrets in a file" switch.
fn secret_store_card<'a>(v: &SettingsView<'a>) -> Element<'a, Message> {
    let line = |s: std::borrow::Cow<'_, str>| -> Element<'a, Message> {
        text(s.into_owned())
            .size(TEXT_BODY)
            .width(Length::Fill)
            .into()
    };
    let status: Element<'a, Message> = match v.secret_backend {
        Some(SecretBackend::Keyring) => line(t!("settings.secret_store_keyring")),
        Some(SecretBackend::File) => banner(BannerKind::Warning, t!("app.keyring_file"), None),
        Some(SecretBackend::MemoryByPolicy) => banner(
            BannerKind::Info,
            t!("settings.secret_store_memory_policy"),
            None,
        ),
        Some(SecretBackend::Memory) if v.secrets_system => {
            banner(BannerKind::Warning, t!("settings.secrets_memory"), None)
        }
        Some(SecretBackend::Memory) => line(t!("settings.secret_store_session")),
        None => text(t!("settings.secret_store_loading"))
            .size(TEXT_BODY)
            .style(rustket_ui::style::text_secondary)
            .width(Length::Fill)
            .into(),
    };
    let ready = v.secret_backend.is_some() && !v.secrets_switching;
    let toggle = switch(
        t!("settings.secrets_memory_only").into_owned(),
        Some(t!("settings.secrets_memory_only_help").into_owned()),
        v.config.ui.secrets_memory_only,
        ready.then_some(Message::SetSecretsMemoryOnly as fn(bool) -> Message),
    );
    container(section(
        t!("settings.secret_store").into_owned(),
        vec![status, toggle],
    ))
    .id(SECRET_STORE_CARD)
    .into()
}

fn storage_card<'a>(v: &SettingsView<'a>) -> Element<'a, Message> {
    let ui = &v.config.ui;
    let rows = crate::cache::view::with_storage_row(
        v.offline_cache,
        Message::ClearOfflineCache,
        vec![
            action_row(
                t!("settings.image_cache").into_owned(),
                caption(t!("settings.image_cache_help").into_owned()).into(),
                t!("settings.clear_cache").into_owned(),
                Some(Message::ClearImageCache),
            ),
            field(
                t!("settings.cache_limit").into_owned(),
                Some(cache_limit_help(ui.image_cache_mb)),
                segmented(
                    &ui.image_cache_mb,
                    cache_choices(ui.image_cache_mb),
                    Message::SetImageCacheMb,
                ),
            ),
        ],
    );
    container(section(t!("settings.storage").into_owned(), rows))
        .id(STORAGE_CARD)
        .into()
}

/// "100 MB", "1 GB" … (decimal units, like the cap: [`crate::images::MB`]).
pub fn size_label(mb: u64) -> String {
    if mb >= 1000 && mb.is_multiple_of(1000) {
        t!("settings.size_gb", n = mb / 1000).into_owned()
    } else {
        t!("settings.size_mb", n = mb).into_owned()
    }
}

/// The cap choices, with a configured value outside them kept as its own.
pub(super) fn cache_choices(current: u32) -> Vec<(u32, String)> {
    let mut v: Vec<u32> = IMAGE_CACHE_CHOICES.to_vec();
    if !v.contains(&current) {
        v.push(current);
        v.sort_unstable();
    }
    v.into_iter()
        .map(|mb| (mb, size_label(u64::from(mb))))
        .collect()
}

fn cache_limit_help(cap_mb: u32) -> String {
    let help = t!("settings.cache_limit_help").into_owned();
    match crate::images::disk_usage() {
        Some(bytes) => format!(
            "{help}\n{}",
            t!(
                "settings.cache_used",
                used = size_label(bytes.div_ceil(crate::images::MB)),
                limit = size_label(u64::from(cap_mb))
            )
        ),
        None => help,
    }
}
