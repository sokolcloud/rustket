//! Notification preferences of the room (`rooms.saveNotification`), loaded
//! from the subscription.

use std::fmt;

use iced::widget::{column, container, pick_list, scrollable, text, toggler};
use iced::{Element, Length};
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION,
};
use rustket_ui::widget::primary_button;

use super::api::{self, Alert, CustomSound, NotificationPrefs};
use super::common;
use crate::error::Res;
use crate::room::{Action, UpdateCtx, ViewCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Desktop,
    Mobile,
    Email,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    TurnOn,
    MuteGroupMentions,
    ShowCounter,
    ShowMentions,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Sounds(Res<Vec<CustomSound>>),
    Alert(Channel, Alert),
    Sound(String),
    Flag(Flag, bool),
    Save,
    Saved(Res<()>),
}

#[derive(Default)]
pub struct Notify {
    form: Option<NotificationPrefs>,
    saved: Option<NotificationPrefs>,
    sounds: Vec<CustomSound>,
    saving: bool,
}

/// A pick list entry: value plus its label.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice<T> {
    value: T,
    label: String,
}

impl<T> fmt::Display for Choice<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

fn alert_label(a: Alert) -> String {
    match a {
        Alert::Default => t!("room_panel.alert_default"),
        Alert::All => t!("room_panel.alert_all"),
        Alert::Mentions => t!("room_panel.alert_mentions"),
        Alert::Nothing => t!("room_panel.alert_nothing"),
    }
    .into_owned()
}

impl Notify {
    pub fn activate(&mut self, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        let form = ctx
            .workspace
            .subscription(ctx.rid)
            .map(NotificationPrefs::from_subscription);
        self.saved = form.clone();
        self.form = form;
        Action::task(ctx.rest_task(api::custom_sounds, Msg::Sounds))
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut UpdateCtx<'_>) -> Action<Msg> {
        match msg {
            Msg::Sounds(Ok(s)) => {
                self.sounds = s;
                Action::none()
            }
            Msg::Sounds(Err(e)) => {
                tracing::debug!(error = %e, "custom-sounds.list failed");
                Action::none()
            }
            Msg::Alert(ch, a) => {
                if let Some(f) = &mut self.form {
                    match ch {
                        Channel::Desktop => f.desktop = a,
                        Channel::Mobile => f.mobile = a,
                        Channel::Email => f.email = a,
                    }
                }
                Action::none()
            }
            Msg::Sound(s) => {
                if let Some(f) = &mut self.form {
                    f.sound = s;
                }
                Action::none()
            }
            Msg::Flag(flag, v) => {
                if let Some(f) = &mut self.form {
                    match flag {
                        Flag::TurnOn => f.turn_on = v,
                        Flag::MuteGroupMentions => f.mute_group_mentions = v,
                        Flag::ShowCounter => f.show_counter = v,
                        Flag::ShowMentions => f.show_mentions = v,
                    }
                }
                Action::none()
            }
            Msg::Save => {
                let Some(form) = self.form.clone() else {
                    return Action::none();
                };
                if self.saving {
                    return Action::none();
                }
                self.saving = true;
                let rid = ctx.rid.to_owned();
                Action::task(ctx.rest_task(
                    move |rest| api::save_notifications(rest, rid, form),
                    Msg::Saved,
                ))
            }
            Msg::Saved(r) => {
                self.saving = false;
                match r {
                    Ok(()) => {
                        self.saved = self.form.clone();
                        Action::effect(common::ok_toast(t!("room_panel.prefs_saved").into_owned()))
                    }
                    Err(e) => {
                        Action::effect(common::error_toast(&t!("room_panel.save_failed"), &e))
                    }
                }
            }
        }
    }

    pub fn view<'a>(&'a self, _ctx: &ViewCtx<'a>) -> Element<'a, Msg> {
        let Some(f) = &self.form else {
            return common::empty(Icon::BellOff, t!("room_panel.not_member").into_owned());
        };
        let toggle =
            |label: String, hint: Option<String>, value: bool, flag: Flag| -> Element<'a, Msg> {
                let mut c = column![
                    toggler(value)
                        .label(label)
                        .text_size(TEXT_BODY)
                        .on_toggle(move |v| Msg::Flag(flag, v))
                        .style(style::toggler)
                        .width(Length::Fill),
                ]
                .spacing(2);
                if let Some(h) = hint {
                    c = c.push(text(h).size(TEXT_CAPTION).style(style::text_hint));
                }
                c.into()
            };
        let alerts: Vec<Choice<Alert>> = Alert::ALL
            .iter()
            .map(|a| Choice {
                value: *a,
                label: alert_label(*a),
            })
            .collect();
        let alert = |label: String, ch: Channel, value: Alert| -> Element<'a, Msg> {
            common::field(
                label,
                pick_list(
                    alerts.clone(),
                    Some(Choice {
                        value,
                        label: alert_label(value),
                    }),
                    move |c: Choice<Alert>| Msg::Alert(ch, c.value),
                )
                .text_size(TEXT_BODY)
                .width(Length::Fill)
                .style(style::pick_list)
                .menu_style(style::menu),
            )
        };
        let mut sounds = vec![
            Choice {
                value: "none".to_owned(),
                label: t!("room_panel.sound_none").into_owned(),
            },
            Choice {
                value: "default".to_owned(),
                label: t!("room_panel.sound_default").into_owned(),
            },
        ];
        sounds.extend(self.sounds.iter().map(|s| Choice {
            value: s.id.clone(),
            label: s.name.clone(),
        }));
        let selected_sound = sounds
            .iter()
            .find(|c| c.value == f.sound)
            .cloned()
            .or_else(|| sounds.get(1).cloned());
        let section = |s: String| {
            text(s)
                .size(TEXT_CAPTION)
                .font(FONT_SEMIBOLD)
                .style(style::text_hint)
        };

        let col = column![
            toggle(
                t!("room_panel.receive_notifications").into_owned(),
                Some(t!("room_panel.receive_notifications_hint").into_owned()),
                f.turn_on,
                Flag::TurnOn
            ),
            toggle(
                t!("room_panel.mute_group_mentions").into_owned(),
                None,
                f.mute_group_mentions,
                Flag::MuteGroupMentions
            ),
            toggle(
                t!("room_panel.show_counter").into_owned(),
                None,
                f.show_counter,
                Flag::ShowCounter
            ),
            toggle(
                t!("room_panel.show_mentions").into_owned(),
                None,
                f.show_mentions,
                Flag::ShowMentions
            ),
            section(t!("room_panel.section_desktop").into_owned()),
            alert(
                t!("room_panel.alert").into_owned(),
                Channel::Desktop,
                f.desktop
            ),
            common::field(
                t!("room_panel.sound").into_owned(),
                pick_list(sounds, selected_sound, |c: Choice<String>| Msg::Sound(
                    c.value
                ))
                .text_size(TEXT_BODY)
                .width(Length::Fill)
                .style(style::pick_list)
                .menu_style(style::menu),
            ),
            section(t!("room_panel.section_mobile").into_owned()),
            alert(
                t!("room_panel.alert").into_owned(),
                Channel::Mobile,
                f.mobile
            ),
            section(t!("room_panel.section_email").into_owned()),
            alert(t!("room_panel.alert").into_owned(), Channel::Email, f.email),
            container(
                primary_button(
                    t!("room_panel.save").into_owned(),
                    (self.form != self.saved).then_some(Msg::Save)
                )
                .loading(self.saving)
                .width(Length::Fill)
            )
            .padding([SPACE_2, 0.0]),
        ]
        .spacing(SPACE_3)
        .padding(SPACE_4);
        let _ = SPACE_1;
        scrollable(col)
            .height(Length::Fill)
            .style(style::scrollable)
            .into()
    }
}
