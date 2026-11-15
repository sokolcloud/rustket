//! The time picker of reminders and due dates: preset buttons with the time
//! they resolve to, and a custom date and time. Used by the message menu's
//! "Remind me…" dialog (`room::timeline`) and the Later view's dialogs.

use chrono::{DateTime, Datelike, Local, NaiveDate};
use iced::widget::{button, column, row, text, text_input};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::reminders::{self, Preset};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{danger_button, primary_button, secondary_button};

/// Hour the due-date presets use.
pub const DUE_HOUR: u32 = 18;

/// What is picked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Reminder,
    Due,
}

/// A due-date preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuePreset {
    Today,
    Tomorrow,
    NextWeek,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Preset(Preset),
    Due(DuePreset),
    /// Show the custom date and time fields.
    Custom,
    Date(String),
    Time(String),
    SetCustom,
    /// Remove the reminder or the due date.
    Clear,
}

/// The user's choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Picked {
    At(i64),
    Clear,
}

/// Why a custom time is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CustomError {
    Invalid,
    Past,
}

#[derive(Debug, Clone, PartialEq)]
pub struct WhenPicker {
    pub kind: Kind,
    /// The current reminder or due date (offers "Remove").
    pub current: Option<i64>,
    custom: bool,
    date: String,
    time: String,
    error: Option<CustomError>,
}

pub const DATE_INPUT_ID: &str = "when-picker-date";

impl WhenPicker {
    pub fn new(kind: Kind, current: Option<i64>) -> Self {
        Self {
            kind,
            current,
            custom: false,
            date: String::new(),
            time: String::new(),
            error: None,
        }
    }

    /// Today, tomorrow or next week's Friday (the end of the next work
    /// week) at [`DUE_HOUR`].
    fn due_at(p: DuePreset, now: &DateTime<Local>) -> DateTime<Local> {
        match p {
            DuePreset::Today => reminders::day_at(now, 0, DUE_HOUR),
            DuePreset::Tomorrow => reminders::day_at(now, 1, DUE_HOUR),
            DuePreset::NextWeek => {
                let from_monday = i64::from(now.weekday().num_days_from_monday());
                reminders::day_at(now, 7 - from_monday + 4, DUE_HOUR)
            }
        }
    }

    /// Applies `msg`; `Some` when the user picked.
    pub fn update(&mut self, msg: Msg, now: DateTime<Local>, morning: u32) -> Option<Picked> {
        match msg {
            Msg::Preset(p) => Some(Picked::At(
                reminders::preset_at(p, &now, morning).timestamp_millis(),
            )),
            Msg::Due(p) => Some(Picked::At(Self::due_at(p, &now).timestamp_millis())),
            Msg::Custom => {
                self.custom = true;
                if self.date.is_empty() {
                    let base = match self.current {
                        Some(at) => chrono::TimeZone::timestamp_millis_opt(&Local, at)
                            .single()
                            .unwrap_or(now),
                        None => reminders::preset_at(Preset::Tomorrow, &now, morning),
                    };
                    self.date = base.format("%Y-%m-%d").to_string();
                    self.time = base.format("%H:%M").to_string();
                }
                None
            }
            Msg::Date(s) => {
                self.date = s;
                self.error = None;
                None
            }
            Msg::Time(s) => {
                self.time = s;
                self.error = None;
                None
            }
            Msg::SetCustom => match self.custom_at(&now) {
                Ok(at) => Some(Picked::At(at)),
                Err(e) => {
                    self.error = Some(e);
                    None
                }
            },
            Msg::Clear => Some(Picked::Clear),
        }
    }

    fn custom_at(&self, now: &DateTime<Local>) -> Result<i64, CustomError> {
        let date = crate::uikit::elements::parse_date(&self.date)
            .and_then(|d| NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
            .ok_or(CustomError::Invalid)?;
        let (h, m) = crate::uikit::elements::parse_time(&self.time).ok_or(CustomError::Invalid)?;
        let at = reminders::custom_at(date, h, m, &Local).ok_or(CustomError::Invalid)?;
        if self.kind == Kind::Reminder && at <= *now {
            return Err(CustomError::Past);
        }
        Ok(at.timestamp_millis())
    }

    /// The picker's content (the owner puts it into its dialog).
    pub fn view<'a>(
        &'a self,
        now: DateTime<Local>,
        morning: u32,
        clock_24h: bool,
    ) -> Element<'a, Msg> {
        let when =
            |at: DateTime<Local>| crate::format::moment(at.timestamp_millis(), now, clock_24h);
        let option = |label: String, at: String, on: Msg| -> Element<'a, Msg> {
            button(
                row![
                    text(label).size(TEXT_BODY).width(Length::Fill),
                    text(at).size(TEXT_SMALL).style(style::text_secondary),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
            )
            .width(Length::Fill)
            .padding([6.0, SPACE_2])
            .style(style::button_list_item(false))
            .on_press(on)
            .into()
        };
        let mut col = column![].spacing(SPACE_1);
        if let Some(cur) = self.current {
            let key = match self.kind {
                Kind::Reminder => "room_timeline.remind_set",
                Kind::Due => "later.due_on",
            };
            let line = t!(key, time = crate::format::moment(cur, now, clock_24h)).into_owned();
            let remove = match self.kind {
                Kind::Reminder => t!("later.act_remove_reminder"),
                Kind::Due => t!("later.due_none"),
            };
            col = col.push(
                row![
                    text(line)
                        .size(TEXT_SMALL)
                        .style(style::text_secondary)
                        .width(Length::Fill),
                    danger_button(remove, Some(Msg::Clear)),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center),
            );
        }
        match self.kind {
            Kind::Reminder => {
                for p in Preset::ALL {
                    let key = match p {
                        Preset::In20Min => "later.preset_20m",
                        Preset::In1Hour => "later.preset_1h",
                        Preset::In3Hours => "later.preset_3h",
                        Preset::Tomorrow => "later.preset_tomorrow",
                        Preset::NextWeek => "later.preset_next_week",
                    };
                    let at = reminders::preset_at(p, &now, morning);
                    col = col.push(option(t!(key).into_owned(), when(at), Msg::Preset(p)));
                }
            }
            Kind::Due => {
                for p in [DuePreset::Today, DuePreset::Tomorrow, DuePreset::NextWeek] {
                    let at = Self::due_at(p, &now);
                    if at <= now {
                        continue;
                    }
                    let key = match p {
                        DuePreset::Today => "later.preset_today",
                        DuePreset::Tomorrow => "later.preset_tomorrow",
                        DuePreset::NextWeek => "later.preset_next_week",
                    };
                    col = col.push(option(t!(key).into_owned(), when(at), Msg::Due(p)));
                }
            }
        }
        if !self.custom {
            col = col.push(option(
                t!("later.preset_custom").into_owned(),
                String::new(),
                Msg::Custom,
            ));
            return col.into();
        }
        let date = text_input(&t!("later.custom_date"), &self.date)
            .id(DATE_INPUT_ID)
            .on_input(Msg::Date)
            .on_submit(Msg::SetCustom)
            .padding([SPACE_1, SPACE_2])
            .style(style::text_input);
        let time = text_input(&t!("later.custom_time"), &self.time)
            .on_input(Msg::Time)
            .on_submit(Msg::SetCustom)
            .padding([SPACE_1, SPACE_2])
            .width(Length::Fixed(90.0))
            .style(style::text_input);
        col = col.push(
            row![
                date,
                time,
                primary_button(t!("later.custom_set"), Some(Msg::SetCustom))
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        );
        if let Some(e) = self.error {
            let key = match e {
                CustomError::Invalid => "later.custom_invalid",
                CustomError::Past => "later.custom_past",
            };
            col = col.push(
                text(t!(key).into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_danger),
            );
        }
        column![col].spacing(SPACE_3).into()
    }
}

/// A secondary "Cancel" button for the dialogs around the picker.
pub fn cancel<'a, M: Clone + 'a>(on: M) -> Element<'a, M> {
    secondary_button(t!("app.cancel"), Some(on)).into()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use chrono::TimeZone;

    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 11, 16, 14, 0, 0).unwrap()
    }

    #[test]
    fn presets_and_custom_times() {
        let mut p = WhenPicker::new(Kind::Reminder, None);
        let n = now();
        assert_eq!(
            p.update(Msg::Preset(Preset::In1Hour), n, 9),
            Some(Picked::At(
                (n + chrono::Duration::hours(1)).timestamp_millis()
            ))
        );
        assert_eq!(p.update(Msg::Custom, n, 9), None);
        // Prefilled with tomorrow morning.
        assert_eq!(p.date, "2026-11-17");
        assert_eq!(p.time, "09:00");
        p.update(Msg::Date("2026-11-16".into()), n, 9);
        p.update(Msg::Time("13:00".into()), n, 9);
        assert_eq!(p.update(Msg::SetCustom, n, 9), None);
        assert_eq!(p.error, Some(CustomError::Past));
        p.update(Msg::Time("25:00".into()), n, 9);
        assert_eq!(p.update(Msg::SetCustom, n, 9), None);
        assert_eq!(p.error, Some(CustomError::Invalid));
        p.update(Msg::Time("15:30".into()), n, 9);
        let at = Local.with_ymd_and_hms(2026, 11, 16, 15, 30, 0).unwrap();
        assert_eq!(
            p.update(Msg::SetCustom, n, 9),
            Some(Picked::At(at.timestamp_millis()))
        );
        assert_eq!(p.update(Msg::Clear, n, 9), Some(Picked::Clear));
        // Due dates may lie in the past.
        let mut d = WhenPicker::new(Kind::Due, None);
        d.update(Msg::Custom, n, 9);
        d.update(Msg::Date("2026-11-01".into()), n, 9);
        assert!(matches!(
            d.update(Msg::SetCustom, n, 9),
            Some(Picked::At(_))
        ));
        let today = Local.with_ymd_and_hms(2026, 11, 16, 18, 0, 0).unwrap();
        assert_eq!(
            d.update(Msg::Due(DuePreset::Today), n, 9),
            Some(Picked::At(today.timestamp_millis()))
        );
        // Next week: its Friday evening (2026-11-16 is a Monday), from any
        // day of this week.
        let friday = Local.with_ymd_and_hms(2026, 11, 27, 18, 0, 0).unwrap();
        for day in [16, 20, 22] {
            let from = Local.with_ymd_and_hms(2026, 11, day, 14, 0, 0).unwrap();
            assert_eq!(
                d.update(Msg::Due(DuePreset::NextWeek), from, 9),
                Some(Picked::At(friday.timestamp_millis())),
                "from Nov {day}"
            );
        }
    }
}
