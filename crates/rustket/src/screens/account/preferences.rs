//! Preferences tab: server-side user preferences (`users.setPreferences`).

use std::fmt;

use iced::widget::{column, pick_list, row, text, toggler};
use iced::{Alignment, Element, Length};
use rocketchat::models::User;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_3, SPACE_4, TEXT_BODY};
use rustket_ui::widget::{BannerKind, banner, primary_button, secondary_button, text_field};
use serde_json::{Map, Value, json};

use super::{Message, api, section};
use crate::component::{Action, Ctx, ViewCtx};
use crate::error::Res;

/// Kind of a preference control.
#[derive(Debug, Clone, Copy)]
pub enum Kind {
    Toggle,
    /// `(value, i18n key)`.
    Choice(&'static [(&'static str, &'static str)]),
    /// Like `Choice` with numeric values.
    Number(&'static [(i64, &'static str)]),
    /// Seconds, typed.
    Seconds,
    /// Comma-separated words, stored as an array.
    Words,
}

pub struct Pref {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: Kind,
    pub default: fn() -> Value,
}

const NOTIFY: &[(&str, &str)] = &[
    ("default", "account.opt_default"),
    ("all", "account.opt_all"),
    ("mentions", "account.opt_mentions"),
    ("nothing", "account.opt_nothing"),
];
const EMAIL_MODE: &[(&str, &str)] = &[
    ("nothing", "account.opt_email_nothing"),
    ("mentions", "account.opt_email_mentions"),
];
const THREAD: &[(&str, &str)] = &[
    ("default", "account.opt_default"),
    ("always", "account.opt_always"),
    ("never", "account.opt_never"),
];
const SOUNDS: &[(&str, &str)] = &[
    ("none", "account.opt_sound_none"),
    ("chime", "account.opt_sound_chime"),
    ("door", "account.opt_sound_door"),
    ("beep", "account.opt_sound_beep"),
    ("chelle", "account.opt_sound_chelle"),
    ("ding", "account.opt_sound_ding"),
    ("droplet", "account.opt_sound_droplet"),
    ("highbell", "account.opt_sound_highbell"),
    ("seasons", "account.opt_sound_seasons"),
];
const CLOCK: &[(i64, &str)] = &[
    (0, "account.opt_default"),
    (1, "account.opt_clock_12"),
    (2, "account.opt_clock_24"),
];

fn t_() -> Value {
    Value::Bool(true)
}
fn f_() -> Value {
    Value::Bool(false)
}

pub const MESSAGES: &[Pref] = &[
    Pref {
        key: "alsoSendThreadToChannel",
        label: "account.pref_also_send_thread",
        kind: Kind::Choice(THREAD),
        default: || json!("default"),
    },
    Pref {
        key: "useEmojis",
        label: "account.pref_use_emojis",
        kind: Kind::Toggle,
        default: t_,
    },
    Pref {
        key: "convertAsciiEmoji",
        label: "account.pref_convert_ascii",
        kind: Kind::Toggle,
        default: t_,
    },
    Pref {
        key: "hideUsernames",
        label: "account.pref_hide_usernames",
        kind: Kind::Toggle,
        default: f_,
    },
    Pref {
        key: "hideRoles",
        label: "account.pref_hide_roles",
        kind: Kind::Toggle,
        default: f_,
    },
    Pref {
        key: "displayAvatars",
        label: "account.pref_display_avatars",
        kind: Kind::Toggle,
        default: t_,
    },
    Pref {
        key: "clockMode",
        label: "account.pref_clock",
        kind: Kind::Number(CLOCK),
        default: || json!(0),
    },
];

pub const NOTIFICATIONS: &[Pref] = &[
    Pref {
        key: "desktopNotifications",
        label: "account.pref_desktop",
        kind: Kind::Choice(NOTIFY),
        default: || json!("default"),
    },
    Pref {
        key: "pushNotifications",
        label: "account.pref_push",
        kind: Kind::Choice(NOTIFY),
        default: || json!("default"),
    },
    Pref {
        key: "emailNotificationMode",
        label: "account.pref_email",
        kind: Kind::Choice(EMAIL_MODE),
        default: || json!("mentions"),
    },
    Pref {
        key: "newMessageNotification",
        label: "account.pref_sound_message",
        kind: Kind::Choice(SOUNDS),
        default: || json!("chime"),
    },
    Pref {
        key: "newRoomNotification",
        label: "account.pref_sound_room",
        kind: Kind::Choice(SOUNDS),
        default: || json!("door"),
    },
    Pref {
        key: "muteFocusedConversations",
        label: "account.pref_mute_focused",
        kind: Kind::Toggle,
        default: t_,
    },
];

pub const PRESENCE: &[Pref] = &[
    Pref {
        key: "enableAutoAway",
        label: "account.pref_auto_away",
        kind: Kind::Toggle,
        default: t_,
    },
    Pref {
        key: "idleTimeLimit",
        label: "account.pref_idle_limit",
        kind: Kind::Seconds,
        default: || json!(300),
    },
];

pub const HIGHLIGHTS: &[Pref] = &[Pref {
    key: "highlights",
    label: "account.pref_highlights",
    kind: Kind::Words,
    default: || json!([]),
}];

/// Interface languages offered (native names; `""` = server default).
pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("ru", "Русский"),
    ("de", "Deutsch"),
    ("es", "Español"),
    ("fr", "Français"),
    ("it", "Italiano"),
    ("pt-BR", "Português (Brasil)"),
    ("pl", "Polski"),
    ("uk", "Українська"),
    ("tr", "Türkçe"),
    ("nl", "Nederlands"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("zh", "简体中文"),
];

fn all() -> impl Iterator<Item = &'static Pref> {
    MESSAGES
        .iter()
        .chain(NOTIFICATIONS)
        .chain(PRESENCE)
        .chain(HIGHLIGHTS)
}

/// A pick-list entry.
#[derive(Debug, Clone, PartialEq)]
pub struct Choice {
    pub value: Value,
    pub label: String,
}

impl fmt::Display for Choice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.label)
    }
}

#[derive(Debug, Clone)]
pub enum Msg {
    Set(&'static str, Value),
    Text(&'static str, String),
    Language(Choice),
    Save,
    Revert,
    Saved(Value, Res<()>),
}

#[derive(Debug, Default)]
pub struct Preferences {
    pub loaded_for: Option<String>,
    /// Current form values by preference key (plus `language`).
    pub values: Map<String, Value>,
    pub original: Map<String, Value>,
    /// Text of `Seconds` / `Words` inputs.
    pub texts: std::collections::HashMap<&'static str, String>,
    pub saving: bool,
    pub error: Option<String>,
    pub saved: bool,
}

fn text_of(kind: Kind, v: &Value) -> String {
    match kind {
        Kind::Words => v
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default(),
        _ => v.as_i64().map(|n| n.to_string()).unwrap_or_default(),
    }
}

/// Parses a `Words` input: trimmed, non-empty, deduplicated.
/// The server refuses an `idleTimeLimit` below this
/// (`invalid-idle-time-limit-value`).
pub const MIN_IDLE_SECONDS: u32 = 60;

pub fn parse_seconds(s: &str) -> Option<u32> {
    s.trim()
        .parse::<u32>()
        .ok()
        .filter(|n| *n >= MIN_IDLE_SECONDS)
}

pub fn parse_words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for w in s.split(',').map(str::trim).filter(|w| !w.is_empty()) {
        if !out.iter().any(|o| o == w) {
            out.push(w.to_owned());
        }
    }
    out
}

impl Preferences {
    pub fn sync(&mut self, ws: &Workspace) {
        let Some(me) = ws.me() else {
            return;
        };
        if self.loaded_for.as_deref() == Some(me.id.as_str()) {
            return;
        }
        self.loaded_for = Some(me.id.clone());
        self.load(me);
    }

    fn load(&mut self, me: &User) {
        let prefs = me.preferences();
        self.values.clear();
        self.texts.clear();
        for p in all() {
            let v = prefs
                .and_then(|m| m.get(p.key))
                .cloned()
                .unwrap_or_else(p.default);
            if matches!(p.kind, Kind::Seconds | Kind::Words) {
                self.texts.insert(p.key, text_of(p.kind, &v));
            }
            self.values.insert(p.key.to_owned(), v);
        }
        self.values.insert(
            "language".to_owned(),
            Value::String(me.language.clone().unwrap_or_default()),
        );
        self.original = self.values.clone();
    }

    /// Changed preferences as `setPreferences` `data`.
    pub fn changes(&self) -> Map<String, Value> {
        self.values
            .iter()
            .filter(|(k, v)| self.original.get(*k) != Some(*v))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect()
    }

    /// The idle limit text is not a number of at least [`MIN_IDLE_SECONDS`].
    pub fn invalid(&self) -> bool {
        self.texts
            .get("idleTimeLimit")
            .is_some_and(|s| parse_seconds(s).is_none())
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut Ctx<'_>) -> Action<Message> {
        match msg {
            Msg::Set(k, v) => {
                self.values.insert(k.to_owned(), v);
                self.saved = false;
                Action::none()
            }
            Msg::Text(k, s) => {
                let kind = all().find(|p| p.key == k).map(|p| p.kind);
                match kind {
                    Some(Kind::Words) => {
                        self.values.insert(k.to_owned(), json!(parse_words(&s)));
                    }
                    Some(Kind::Seconds) => {
                        if let Some(n) = parse_seconds(&s) {
                            self.values.insert(k.to_owned(), json!(n));
                        }
                    }
                    _ => {}
                }
                self.texts.insert(k, s);
                self.saved = false;
                Action::none()
            }
            Msg::Language(c) => {
                self.values.insert("language".to_owned(), c.value);
                self.saved = false;
                Action::none()
            }
            Msg::Revert => {
                self.values = self.original.clone();
                for p in all() {
                    if matches!(p.kind, Kind::Seconds | Kind::Words)
                        && let Some(v) = self.values.get(p.key)
                    {
                        self.texts.insert(p.key, text_of(p.kind, v));
                    }
                }
                self.error = None;
                Action::none()
            }
            Msg::Save => {
                let changes = self.changes();
                if self.saving || changes.is_empty() || self.invalid() {
                    return Action::none();
                }
                self.saving = true;
                self.error = None;
                let data = Value::Object(changes);
                let d2 = data.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { api::set_preferences(&rest, data).await },
                    move |r| Message::Prefs(Msg::Saved(d2, r)),
                ))
            }
            Msg::Saved(data, r) => {
                self.saving = false;
                match r {
                    Ok(()) => {
                        self.saved = true;
                        if let Value::Object(m) = &data {
                            for (k, v) in m {
                                self.original.insert(k.clone(), v.clone());
                            }
                            apply_to_me(ctx.workspace, m);
                        }
                    }
                    Err(e) => self.error = Some(e.user_text()),
                }
                Action::none()
            }
        }
    }

    pub fn view<'a>(&'a self, _ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let lang_value = self
            .values
            .get("language")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let mut langs = vec![Choice {
            value: json!(""),
            label: t!("account.opt_default").into_owned(),
        }];
        langs.extend(LANGUAGES.iter().map(|(code, name)| Choice {
            value: json!(code),
            label: (*name).to_owned(),
        }));
        let selected = langs.iter().find(|c| c.value == json!(lang_value)).cloned();
        let language = control_row(
            t!("account.pref_language").into_owned(),
            pick_list(langs, selected, |c| Message::Prefs(Msg::Language(c)))
                .text_size(TEXT_BODY)
                .style(style::pick_list)
                .width(Length::Fill)
                .into(),
        );

        let changes = self.changes();
        let mut actions = column![].spacing(SPACE_3);
        if let Some(e) = &self.error {
            actions = actions.push(banner(BannerKind::Error, e.clone(), None));
        }
        if self.saved {
            actions = actions.push(banner(BannerKind::Success, t!("account.saved"), None));
        }
        let p = |m| Message::Prefs(m);
        actions = actions.push(
            row![
                secondary_button(
                    t!("account.revert"),
                    (!changes.is_empty() && !self.saving).then_some(p(Msg::Revert))
                ),
                primary_button(
                    t!("account.save"),
                    (!changes.is_empty() && !self.saving && !self.invalid())
                        .then_some(p(Msg::Save))
                )
                .loading(self.saving),
            ]
            .spacing(SPACE_2),
        );

        let left = column![
            section(t!("account.sec_localization").into_owned(), language),
            section(
                t!("account.sec_messages").into_owned(),
                self.controls(MESSAGES)
            ),
        ]
        .spacing(SPACE_4)
        .width(Length::Fill);
        let right = column![
            section(
                t!("account.sec_notifications").into_owned(),
                self.controls(NOTIFICATIONS)
            ),
            section(
                t!("account.sec_presence").into_owned(),
                self.controls(PRESENCE)
            ),
            section(
                t!("account.sec_highlights").into_owned(),
                self.controls(HIGHLIGHTS)
            ),
        ]
        .spacing(SPACE_4)
        .width(Length::Fill);
        column![row![left, right].spacing(SPACE_4), actions]
            .spacing(SPACE_4)
            .max_width(1100)
            .into()
    }

    fn controls<'a>(&'a self, prefs: &'static [Pref]) -> Element<'a, Message> {
        let mut col = column![].spacing(SPACE_3);
        for pref in prefs {
            let value = self
                .values
                .get(pref.key)
                .cloned()
                .unwrap_or_else(pref.default);
            let label = t!(pref.label).into_owned();
            let key = pref.key;
            let el: Element<'a, Message> = match pref.kind {
                Kind::Toggle => toggler(value.as_bool().unwrap_or(false))
                    .label(label)
                    .text_size(TEXT_BODY)
                    .on_toggle(move |b| Message::Prefs(Msg::Set(key, Value::Bool(b))))
                    .style(style::toggler)
                    .width(Length::Fill)
                    .into(),
                Kind::Choice(opts) => {
                    let choices: Vec<Choice> = opts
                        .iter()
                        .map(|(v, k)| Choice {
                            value: json!(v),
                            label: t!(*k).into_owned(),
                        })
                        .collect();
                    let sel = choices.iter().find(|c| c.value == value).cloned();
                    control_row(
                        label,
                        pick_list(choices, sel, move |c: Choice| {
                            Message::Prefs(Msg::Set(key, c.value))
                        })
                        .text_size(TEXT_BODY)
                        .style(style::pick_list)
                        .width(Length::Fill)
                        .into(),
                    )
                }
                Kind::Number(opts) => {
                    let choices: Vec<Choice> = opts
                        .iter()
                        .map(|(v, k)| Choice {
                            value: json!(v),
                            label: t!(*k).into_owned(),
                        })
                        .collect();
                    let sel = choices.iter().find(|c| c.value == value).cloned();
                    control_row(
                        label,
                        pick_list(choices, sel, move |c: Choice| {
                            Message::Prefs(Msg::Set(key, c.value))
                        })
                        .text_size(TEXT_BODY)
                        .style(style::pick_list)
                        .width(Length::Fill)
                        .into(),
                    )
                }
                Kind::Seconds => {
                    let s = self.texts.get(key).map(String::as_str).unwrap_or_default();
                    let err = self
                        .invalid()
                        .then(|| t!("account.err_idle_min", min = MIN_IDLE_SECONDS).into_owned());
                    text_field(label, s, move |s| Message::Prefs(Msg::Text(key, s)))
                        .help(t!("account.pref_idle_help"))
                        .error(err)
                        .width(240)
                        .into()
                }
                Kind::Words => {
                    let s = self.texts.get(key).map(String::as_str).unwrap_or_default();
                    text_field(label, s, move |s| Message::Prefs(Msg::Text(key, s)))
                        .placeholder(t!("account.pref_highlights_placeholder"))
                        .help(t!("account.pref_highlights_help"))
                        .into()
                }
            };
            col = col.push(el);
        }
        col.into()
    }
}

fn control_row<'a>(label: String, control: Element<'a, Message>) -> Element<'a, Message> {
    row![
        text(label).size(TEXT_BODY).width(Length::Fill),
        iced::widget::container(control).width(300)
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center)
    .into()
}

/// Keeps the workspace user's preferences in line with what was saved.
fn apply_to_me(ws: &mut Workspace, data: &Map<String, Value>) {
    let Some(mut me) = ws.me().cloned() else {
        return;
    };
    let settings = me.settings.get_or_insert_with(Default::default);
    let prefs = settings.preferences.get_or_insert_with(Default::default);
    for (k, v) in data {
        if k == "language" {
            continue;
        }
        prefs.insert(k.clone(), v.clone());
    }
    if let Some(l) = data.get("language").and_then(Value::as_str) {
        me.language = (!l.is_empty()).then(|| l.to_owned());
    }
    ws.apply_me(me);
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::UserSettings;

    fn ws_with_prefs(prefs: Value) -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(User {
            id: "u1".into(),
            language: Some("ru".into()),
            settings: Some(UserSettings {
                profile: None,
                preferences: prefs.as_object().cloned(),
            }),
            ..Default::default()
        });
        ws
    }

    #[test]
    fn loads_prefs_with_defaults_and_diffs() {
        let ws = ws_with_prefs(
            json!({"hideRoles": true, "highlights": ["a", "b"], "idleTimeLimit": 600}),
        );
        let mut p = Preferences::default();
        p.sync(&ws);
        assert_eq!(p.values["hideRoles"], json!(true));
        assert_eq!(p.values["useEmojis"], json!(true));
        assert_eq!(p.values["language"], json!("ru"));
        assert_eq!(p.texts["highlights"], "a, b");
        assert_eq!(p.texts["idleTimeLimit"], "600");
        assert!(p.changes().is_empty());
        p.values.insert("clockMode".into(), json!(2));
        assert_eq!(Value::Object(p.changes()), json!({"clockMode": 2}));
    }

    #[test]
    fn words_and_seconds() {
        assert_eq!(parse_words(" foo, bar ,,foo "), vec!["foo", "bar"]);
        let mut p = Preferences::default();
        p.sync(&ws_with_prefs(json!({})));
        p.texts.insert("idleTimeLimit", "abc".into());
        assert!(p.invalid());
        p.texts.insert("idleTimeLimit", "30".into());
        assert!(p.invalid());
        p.texts.insert("idleTimeLimit", "120".into());
        assert!(!p.invalid());
    }

    #[test]
    fn saved_prefs_reach_the_workspace_user() {
        let mut ws = ws_with_prefs(json!({}));
        let mut data = Map::new();
        data.insert("hideRoles".into(), json!(true));
        data.insert("language".into(), json!("de"));
        apply_to_me(&mut ws, &data);
        let me = ws.me().unwrap();
        assert_eq!(me.preferences().unwrap()["hideRoles"], json!(true));
        assert_eq!(me.language.as_deref(), Some("de"));
    }
}
