//! Notifications tab: desktop notifications, preview, sound and a test;
//! message reminders; pausing (busy while paused, working hours).

use chrono::Weekday;
use iced::widget::{checkbox, column, container, pick_list, row, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::config::REMINDER_MORNING_HOURS;
use rustket_core::quiet::DayHours;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_3, TEXT_BODY};
use rustket_ui::widget::secondary_button;

use super::widgets::{Choice, action_row, caption, field, section, switch};
use super::{Message, SettingsView, Tab};

/// "09:00" for hour 9.
fn hour_label(h: u8) -> String {
    format!("{h:02}:00")
}

/// "09:30" for 570 minutes.
fn minutes_label(m: u16) -> String {
    format!("{:02}:{:02}", m / 60, m % 60)
}

/// Half-hour steps of a day, plus `current` when it is between them.
fn time_choices(current: u16) -> Vec<Choice<u16>> {
    let mut v: Vec<u16> = (0..48).map(|i| i * 30).collect();
    if !v.contains(&current) {
        v.push(current);
        v.sort_unstable();
    }
    v.into_iter()
        .map(|m| Choice {
            value: m,
            label: minutes_label(m),
        })
        .collect()
}

fn weekday_label(wd: Weekday) -> String {
    match wd {
        Weekday::Mon => t!("quiet.mon"),
        Weekday::Tue => t!("quiet.tue"),
        Weekday::Wed => t!("quiet.wed"),
        Weekday::Thu => t!("quiet.thu"),
        Weekday::Fri => t!("quiet.fri"),
        Weekday::Sat => t!("quiet.sat"),
        Weekday::Sun => t!("quiet.sun"),
    }
    .into_owned()
}

/// One weekday: on/off, start and end.
fn day_row<'a>(wd: Weekday, d: DayHours, enabled: bool) -> Element<'a, Message> {
    let i = wd.num_days_from_monday() as u8;
    let time = |value: u16, set: fn(DayHours, u16) -> DayHours| {
        let pick = pick_list(
            time_choices(value),
            Some(Choice {
                value,
                label: minutes_label(value),
            }),
            move |c: Choice<u16>| Message::SetWorkDay(i, set(d, c.value)),
        )
        .text_size(TEXT_BODY)
        .width(Length::Fixed(96.0))
        .style(style::pick_list)
        .menu_style(style::menu);
        if enabled && d.on {
            pick.into()
        } else {
            // Disabled: shown without a handler.
            Element::from(
                container(
                    text(minutes_label(value))
                        .size(TEXT_BODY)
                        .style(style::text_hint),
                )
                .padding([0.0, SPACE_2 + 2.0])
                .width(Length::Fixed(96.0)),
            )
        }
    };
    let mut on = checkbox(d.on)
        .label(weekday_label(wd))
        .text_size(TEXT_BODY)
        .width(Length::Fixed(150.0));
    if enabled {
        on = on.on_toggle(move |b| Message::SetWorkDay(i, DayHours { on: b, ..d }));
    }
    let mut r = row![
        on,
        time(d.start, |d, m| DayHours { start: m, ..d }),
        text("–").size(TEXT_BODY),
        time(d.end, |d, m| DayHours { end: m, ..d }),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    if d.on && d.overnight() && d.start != d.end {
        r = r.push(caption(t!("quiet.next_day").into_owned()));
    }
    r.into()
}

/// "Play a sound": independent of desktop notifications (on alone, the
/// sound plays without a notification). The help says what silences it on
/// this platform (`desktop::sound::route`).
fn sound_switch<'a>(ui: &rustket_core::config::UiPrefs) -> Element<'a, Message> {
    let dnd = crate::desktop::sound::Platform::current().dnd_text();
    switch(
        t!("settings.notifications_sound").into_owned(),
        Some(t!("settings.notifications_sound_help", dnd = dnd).into_owned()),
        ui.sound,
        Some(Message::SetSound),
    )
}

/// "Send test notification": with the sound alone it plays the sound
/// without a toast.
fn test_button<'a>(ui: &rustket_core::config::UiPrefs) -> Element<'a, Message> {
    row![secondary_button(
        t!("settings.notifications_test"),
        (ui.desktop_notifications || ui.sound).then_some(Message::TestNotification)
    )]
    .into()
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
                    // Reminder notifications show the message too.
                    (notify_on || reminders_on).then_some(Message::SetPreview),
                ),
                sound_switch(ui),
                test_button(ui),
                action_row(
                    t!("help.link_title").into_owned(),
                    caption(t!("help.link_caption").into_owned()).into(),
                    t!("help.open").into_owned(),
                    Some(Message::Tab(Tab::Help)),
                ),
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
                    reminders_on.then_some(Message::SetReminderSound),
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
        section(t!("quiet.settings_title").into_owned(), {
            let wh = &ui.working_hours;
            let mut days = column![].spacing(SPACE_3);
            for wd in [
                Weekday::Mon,
                Weekday::Tue,
                Weekday::Wed,
                Weekday::Thu,
                Weekday::Fri,
                Weekday::Sat,
                Weekday::Sun,
            ] {
                days = days.push(day_row(wd, wh.day(wd), wh.enabled));
            }
            vec![
                switch(
                    t!("quiet.set_busy").into_owned(),
                    Some(t!("quiet.set_busy_help").into_owned()),
                    ui.pause_sets_busy,
                    Some(Message::SetPauseBusy),
                ),
                switch(
                    t!("quiet.working_hours").into_owned(),
                    Some(t!("quiet.working_hours_help").into_owned()),
                    wh.enabled,
                    Some(Message::SetWorkingHours),
                ),
                days.into(),
            ]
        }),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use iced::advanced::widget::operation::Focusable;
    use iced::advanced::widget::{Id, Operation};
    use iced::{Rectangle, Size, mouse};
    use iced_runtime::user_interface::{Cache, UserInterface};

    /// The messages a click on each focusable widget of `e` sends.
    fn clicks(e: Element<'_, Message>) -> Vec<Vec<Message>> {
        struct Stops(Vec<Rectangle>);
        impl Operation for Stops {
            fn traverse(&mut self, operate: &mut dyn FnMut(&mut dyn Operation)) {
                operate(self);
            }
            fn focusable(&mut self, _: Option<&Id>, bounds: Rectangle, _: &mut dyn Focusable) {
                self.0.push(bounds);
            }
        }
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            rustket_ui::theme::font_regular(),
            iced::Pixels(TEXT_BODY),
        ));
        let mut ui =
            UserInterface::build(e, Size::new(600.0, 400.0), Cache::default(), &mut renderer);
        let mut stops = Stops(Vec::new());
        ui.operate(&renderer, &mut stops);
        let mut out_all = Vec::new();
        for b in stops.0 {
            let at = b.center();
            let mut out = Vec::new();
            let _ = ui.update(
                &[
                    iced::Event::Mouse(mouse::Event::CursorMoved { position: at }),
                    iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                    iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
                ],
                mouse::Cursor::Available(at),
                &mut renderer,
                &mut iced::advanced::clipboard::Null,
                &mut out,
            );
            out_all.push(out);
        }
        out_all
    }

    /// Item 10: with desktop notifications off, "Play a sound" stays
    /// interactive and the test button plays the sound alone.
    #[test]
    fn sound_works_without_notifications() {
        let ui = rustket_core::config::UiPrefs {
            desktop_notifications: false,
            sound: true,
            ..Default::default()
        };
        let sent: Vec<Message> =
            clicks(iced::widget::column![sound_switch(&ui), test_button(&ui)].into())
                .into_iter()
                .flatten()
                .collect();
        assert!(
            sent.iter().any(|m| matches!(m, Message::SetSound(false))),
            "{sent:?}"
        );
        assert!(
            sent.iter().any(|m| matches!(m, Message::TestNotification)),
            "{sent:?}"
        );
        // Neither notifications nor sound: nothing to test.
        let off = rustket_core::config::UiPrefs { sound: false, ..ui };
        let sent: Vec<Message> = clicks(test_button(&off)).into_iter().flatten().collect();
        assert!(sent.is_empty(), "{sent:?}");
    }
}
