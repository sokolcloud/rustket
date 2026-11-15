//! Notifications tab: desktop notifications, preview, sound and a test;
//! message reminders.

use iced::widget::{pick_list, row};
use iced::{Element, Length};
use rust_i18n::t;
use rustket_core::config::REMINDER_MORNING_HOURS;
use rustket_ui::style;
use rustket_ui::theme::TEXT_BODY;
use rustket_ui::widget::secondary_button;

use super::widgets::{Choice, field, section, switch};
use super::{Message, SettingsView};

/// "09:00" for hour 9.
fn hour_label(h: u8) -> String {
    format!("{h:02}:00")
}

pub(super) fn view<'a>(v: &SettingsView<'a>) -> Vec<Element<'a, Message>> {
    let ui = &v.config.ui;
    let notify_on = ui.desktop_notifications;
    let reminders_on = ui.reminder_notifications;
    let hour = ui.morning_hour() as u8;
    let hours: Vec<Choice<u8>> = REMINDER_MORNING_HOURS
        .map(|h| Choice {
            value: h,
            label: hour_label(h),
        })
        .collect();
    vec![
        section(
            t!("settings.notifications").into_owned(),
            vec![
                switch(
                    t!("settings.notifications_enable").into_owned(),
                    Some(t!("settings.notifications_help").into_owned()),
                    notify_on,
                    Some(Message::SetNotifications),
                ),
                switch(
                    t!("settings.notifications_preview").into_owned(),
                    None,
                    ui.notification_preview,
                    notify_on.then_some(Message::SetPreview),
                ),
                switch(
                    t!("settings.notifications_sound").into_owned(),
                    None,
                    ui.sound,
                    notify_on.then_some(Message::SetSound),
                ),
                row![secondary_button(
                    t!("settings.notifications_test"),
                    notify_on.then_some(Message::TestNotification)
                )]
                .into(),
            ],
        ),
        section(
            t!("settings.reminders").into_owned(),
            vec![
                switch(
                    t!("settings.reminders_notify").into_owned(),
                    Some(t!("settings.reminders_notify_help").into_owned()),
                    reminders_on,
                    Some(Message::SetReminderNotifications),
                ),
                switch(
                    t!("settings.reminders_sound").into_owned(),
                    None,
                    ui.reminder_sound,
                    (reminders_on && ui.sound).then_some(Message::SetReminderSound),
                ),
                field(
                    t!("settings.reminders_morning").into_owned(),
                    None,
                    pick_list(
                        hours,
                        Some(Choice {
                            value: hour,
                            label: hour_label(hour),
                        }),
                        |c: Choice<u8>| Message::SetReminderMorningHour(c.value),
                    )
                    .text_size(TEXT_BODY)
                    .width(Length::Fixed(120.0))
                    .style(style::pick_list)
                    .menu_style(style::menu)
                    .into(),
                ),
            ],
        ),
    ]
}
