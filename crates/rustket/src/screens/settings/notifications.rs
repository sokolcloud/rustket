//! Notifications tab: desktop notifications, preview, sound and a test.

use iced::Element;
use iced::widget::row;
use rust_i18n::t;
use rustket_ui::widget::secondary_button;

use super::widgets::{section, switch};
use super::{Message, SettingsView};

pub(super) fn view<'a>(v: &SettingsView<'a>) -> Vec<Element<'a, Message>> {
    let ui = &v.config.ui;
    let notify_on = ui.desktop_notifications;
    vec![section(
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
    )]
}
