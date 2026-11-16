//! Pausing notifications, working hours and statuses that clear themselves
//! (`rustket_core::quiet`): requests from the status menu, the tray and the
//! custom pause dialog, and their labels. The app wiring is in
//! `app/quiet.rs`. Strings: `locales/quiet.yml` (`quiet.*`).

use chrono::{DateTime, Local};
use iced::Element;
use iced::widget::{checkbox, column, text};
use rocketchat::models::UserStatus;
use rust_i18n::t;
use rustket_core::quiet::{ClearAfter, PausePreset, Quiet, StatusExpiry, StatusHold};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_3, TEXT_SMALL};
use rustket_ui::widget::dialog;

use crate::error::Res;
use crate::later::when::{self, Kind, WhenPicker};

/// What a component asks of the app.
#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Pause notifications until this time (UTC ms).
    Pause(i64),
    Resume,
    /// The custom pause dialog.
    PauseCustom,
    /// The expiry of the status just set on the component's server (`None`:
    /// it does not clear itself).
    Expiry(Option<StatusExpiry>),
    /// The user chose a status on the component's server: a pause's busy
    /// hold no longer restores anything.
    ReleaseHold,
}

/// A status call the app made on a server.
#[derive(Debug, Clone, PartialEq)]
pub enum StatusCall {
    /// Busy for a pause; the hold is remembered once it is set.
    Busy(StatusHold),
    /// The pause ended: the status before it.
    Restore(UserStatus),
    /// A status expired: the text is cleared (and this status set).
    Expire(Option<UserStatus>),
    /// A status expired during a busy pause: the text is cleared and this
    /// status is restored when the pause ends.
    ExpireHold(UserStatus),
}

#[derive(Debug, Clone)]
pub enum Msg {
    Request(Request),
    Preset(PausePreset),
    /// The custom pause dialog's picker.
    Picker(when::Msg),
    /// "Set my status to Busy while paused".
    SetBusy(bool),
    /// A status call ended: server, session epoch, call.
    StatusDone(rustket_core::ServerId, u64, StatusCall, Res<()>),
    /// The timer armed for this time fired.
    Due(std::time::Instant),
}

/// The label of a pause preset ("For 30 minutes", "Until tomorrow").
pub fn preset_label(p: PausePreset) -> String {
    match p {
        PausePreset::Min30 => t!("quiet.for_30m"),
        PausePreset::Hour1 => t!("quiet.for_1h"),
        PausePreset::Hour2 => t!("quiet.for_2h"),
        PausePreset::Tomorrow => t!("quiet.until_tomorrow"),
    }
    .into_owned()
}

/// The short label of a pause preset (the status menu's chips).
pub fn chip_label(p: PausePreset) -> String {
    match p {
        PausePreset::Min30 => t!("quiet.chip_30m"),
        PausePreset::Hour1 => t!("quiet.chip_1h"),
        PausePreset::Hour2 => t!("quiet.chip_2h"),
        PausePreset::Tomorrow => t!("quiet.chip_tomorrow"),
    }
    .into_owned()
}

/// The pause line ("Notifications paused until 14:30").
pub fn paused_line(q: &Quiet, now: DateTime<Local>, clock_24h: bool) -> String {
    match q.until {
        Some(at) => t!(
            "quiet.paused_until",
            time = crate::format::moment(at, now, clock_24h)
        )
        .into_owned(),
        None => t!("quiet.paused").into_owned(),
    }
}

/// The label of a "Clear after" choice.
pub fn clear_label(c: ClearAfter) -> String {
    match c {
        ClearAfter::Never => t!("quiet.clear_never"),
        ClearAfter::Min30 => t!("quiet.clear_30m"),
        ClearAfter::Hour1 => t!("quiet.clear_1h"),
        ClearAfter::Hour4 => t!("quiet.clear_4h"),
        ClearAfter::Today => t!("quiet.clear_today"),
        ClearAfter::ThisWeek => t!("quiet.clear_week"),
    }
    .into_owned()
}

/// The custom pause dialog: the picker and the busy switch.
pub fn custom_dialog<'a>(
    picker: &'a WhenPicker,
    busy: bool,
    now: DateTime<Local>,
    morning: u32,
    clock_24h: bool,
    close: crate::message::Message,
    wrap: impl Fn(Msg) -> crate::message::Message + Copy + 'a,
) -> Element<'a, crate::message::Message> {
    let content = column![
        text(t!("quiet.custom_body").into_owned())
            .size(TEXT_SMALL)
            .style(style::text_secondary),
        picker
            .view(now, morning, clock_24h)
            .map(move |m| wrap(Msg::Picker(m))),
        checkbox(busy)
            .label(t!("quiet.set_busy").into_owned())
            .on_toggle(move |b| wrap(Msg::SetBusy(b)))
            .text_size(TEXT_SMALL),
    ]
    .spacing(SPACE_3);
    dialog(
        t!("quiet.pause_title").into_owned(),
        content,
        vec![when::cancel(close.clone())],
        Some(close),
    )
}

/// A fresh custom pause picker.
pub fn custom_picker() -> WhenPicker {
    WhenPicker::new(Kind::Pause, None)
}
