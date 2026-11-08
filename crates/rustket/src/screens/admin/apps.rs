//! Apps management (docs/admin.md §15): installed apps with enable/disable,
//! the app's settings form (labels from the app's own translations), its logs
//! and uninstall. Installing from the marketplace opens the browser.

use iced::widget::{button, column, container, pick_list, row, text};
use iced::{Alignment, Element, Length, Task};
use rocketchat::models::{AppInfo, AppLogs, AppLogsQuery, AppSetting, AppsCount, LanguageMap};
use rust_i18n::t;
use rustket_core::permissions::has_permission;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_MEDIUM, MONOSPACE_FONT, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_SMALL,
};
use rustket_ui::widget::{
    BannerKind, ChipKind, banner, chip, danger_button, primary_button, secondary_button,
};
use serde_json::Value;

use super::common::{
    Confirm, Cx, back_row, card, date, err_text, field, input, kv, muted, readonly, switch,
    title_row,
};
use super::{Message, V};
use crate::error::Res;

#[derive(Debug, Clone)]
pub enum Msg {
    Loaded(Res<Vec<AppInfo>>),
    Count(Res<AppsCount>),
    Reload,
    Toggle(String, bool),
    Toggled(Res<()>),
    Open(String),
    Settings(Res<Vec<AppSetting>>),
    Languages(Res<LanguageMap>),
    Logs(Res<AppLogs>),
    Back,
    Value(String, Value),
    Text(String, String),
    SaveSettings,
    SettingsSaved(Res<Vec<AppSetting>>),
    Uninstall,
    UninstallNow(String),
    Uninstalled(Res<()>),
    Marketplace,
    CloseDialog,
}

pub struct Detail {
    pub app: AppInfo,
    pub settings: Vec<AppSetting>,
    pub edits: Vec<(String, Value)>,
    /// Text of the input widgets per setting id.
    pub texts: std::collections::HashMap<String, String>,
    pub languages: LanguageMap,
    pub logs: Option<AppLogs>,
    pub busy: bool,
    pub error: Option<String>,
}

impl Detail {
    fn value<'a>(&'a self, s: &'a AppSetting) -> &'a Value {
        self.edits
            .iter()
            .find(|(id, _)| *id == s.id)
            .map(|(_, v)| v)
            .unwrap_or_else(|| s.effective_value())
    }

    /// The app's translation of `key` (UI language, then English), else the key.
    fn tr<'a>(&'a self, key: &'a str) -> &'a str {
        let lang = rust_i18n::locale().to_string();
        self.languages
            .get(&lang)
            .and_then(|m| m.get(key))
            .or_else(|| self.languages.get("en").and_then(|m| m.get(key)))
            .map(String::as_str)
            .unwrap_or(key)
    }

    fn changed(&self) -> Vec<AppSetting> {
        self.settings
            .iter()
            .filter_map(|s| {
                let (_, v) = self.edits.iter().find(|(id, _)| *id == s.id)?;
                (v != s.effective_value()).then(|| AppSetting {
                    value: Some(v.clone()),
                    ..s.clone()
                })
            })
            .collect()
    }
}

pub struct State {
    pub apps: Vec<AppInfo>,
    pub count: Option<AppsCount>,
    pub loaded: bool,
    pub error: Option<String>,
    pub detail: Option<Detail>,
}

impl Default for State {
    fn default() -> Self {
        Self::new()
    }
}

impl State {
    pub fn new() -> Self {
        Self {
            apps: Vec::new(),
            count: None,
            loaded: false,
            error: None,
            detail: None,
        }
    }

    pub fn load(&mut self, cx: &mut Cx<'_, '_>) -> Task<Message> {
        Task::batch([
            cx.ctx.rest_task(
                |r| async move { r.installed_apps().await },
                |r| Message::Apps(Msg::Loaded(r)),
            ),
            cx.ctx.rest_task(
                |r| async move { r.apps_count().await },
                |r| Message::Apps(Msg::Count(r)),
            ),
        ])
    }

    fn open(&mut self, cx: &mut Cx<'_, '_>, id: String) -> Task<Message> {
        let (a, b, c) = (id.clone(), id.clone(), id);
        let can_manage = has_permission(cx.ctx.workspace, "manage-apps", None);
        let mut tasks = vec![
            cx.ctx.rest_task(
                move |r| async move { r.app_settings(&a).await },
                |r| Message::Apps(Msg::Settings(r)),
            ),
            cx.ctx.rest_task(
                move |r| async move { r.app_languages(&b).await },
                |r| Message::Apps(Msg::Languages(r)),
            ),
        ];
        if can_manage {
            tasks.push(cx.ctx.rest_task(
                move |r| async move {
                    let q = AppLogsQuery {
                        count: Some(25),
                        ..AppLogsQuery::default()
                    };
                    r.app_logs(&c, &q).await
                },
                |r| Message::Apps(Msg::Logs(r)),
            ));
        }
        Task::batch(tasks)
    }

    pub fn update(&mut self, m: Msg, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let detail = self.detail.as_mut();
        match m {
            Msg::Loaded(r) => {
                self.loaded = true;
                match r {
                    Ok(mut apps) => {
                        apps.sort_by_key(|a| a.name.to_lowercase());
                        self.apps = apps;
                        self.error = None;
                    }
                    Err(f) => self.error = Some(err_text(&f, cx.i18n)),
                }
            }
            Msg::Count(r) => self.count = r.ok(),
            Msg::Reload => return self.load(cx),
            Msg::Toggle(id, on) => {
                return cx.ctx.rest_task(
                    move |r| async move { r.set_app_status(&id, on).await.map(|_| ()) },
                    |r| Message::Apps(Msg::Toggled(r)),
                );
            }
            Msg::Toggled(r) => {
                if cx.check(&r) {
                    return self.load(cx);
                }
            }
            Msg::Open(id) => {
                if let Some(app) = self.apps.iter().find(|a| a.id == id).cloned() {
                    self.detail = Some(Detail {
                        app,
                        settings: Vec::new(),
                        edits: Vec::new(),
                        texts: Default::default(),
                        languages: LanguageMap::new(),
                        logs: None,
                        busy: false,
                        error: None,
                    });
                    return self.open(cx, id);
                }
            }
            Msg::Settings(r) => {
                if let Some(d) = detail {
                    match r {
                        Ok(s) => {
                            d.texts = s
                                .iter()
                                .map(|x| {
                                    let t = match x.effective_value() {
                                        Value::String(s) => s.clone(),
                                        Value::Null => String::new(),
                                        v => v.to_string(),
                                    };
                                    (x.id.clone(), t)
                                })
                                .collect();
                            d.settings = s;
                        }
                        Err(f) => d.error = Some(err_text(&f, cx.i18n)),
                    }
                }
            }
            Msg::Languages(r) => {
                if let (Some(d), Ok(l)) = (detail, r) {
                    d.languages = l;
                }
            }
            Msg::Logs(r) => {
                if let Some(d) = detail {
                    d.logs = r.ok();
                }
            }
            Msg::Back | Msg::CloseDialog => self.detail = None,
            Msg::Text(id, text) => {
                if let Some(d) = detail {
                    let int = d
                        .settings
                        .iter()
                        .any(|s| s.id == id && matches!(s.kind.as_str(), "int" | "number"));
                    let v = match text.trim().parse::<i64>() {
                        Ok(n) if int => Value::from(n),
                        _ => Value::String(text.clone()),
                    };
                    d.texts.insert(id.clone(), text);
                    d.edits.retain(|(x, _)| *x != id);
                    d.edits.push((id, v));
                }
            }
            Msg::Value(id, v) => {
                if let Some(d) = detail {
                    d.edits.retain(|(x, _)| *x != id);
                    d.edits.push((id, v));
                }
            }
            Msg::SaveSettings => {
                if let Some(d) = detail {
                    let changed = d.changed();
                    if changed.is_empty() {
                        return Task::none();
                    }
                    d.busy = true;
                    let id = d.app.id.clone();
                    return cx.ctx.rest_task(
                        move |r| async move { r.set_app_settings(&id, &changed).await },
                        |r| Message::Apps(Msg::SettingsSaved(r)),
                    );
                }
            }
            Msg::SettingsSaved(r) => {
                if let Some(d) = detail {
                    d.busy = false;
                    match r {
                        Ok(_) => {
                            cx.ok(t!("admin.apps.saved").into_owned());
                            d.edits.clear();
                            let id = d.app.id.clone();
                            return self.open(cx, id);
                        }
                        Err(f) => d.error = Some(err_text(&f, cx.i18n)),
                    }
                }
            }
            Msg::Uninstall => {
                if let Some(d) = detail {
                    cx.ask(Confirm::danger(
                        t!("admin.apps.uninstall").into_owned(),
                        t!("admin.apps.uninstall_body", name = d.app.name.clone()).into_owned(),
                        Message::Apps(Msg::UninstallNow(d.app.id.clone())),
                    ));
                }
            }
            Msg::UninstallNow(id) => {
                return cx.ctx.rest_task(
                    move |r| async move { r.uninstall_app(&id).await.map(|_| ()) },
                    |r| Message::Apps(Msg::Uninstalled(r)),
                );
            }
            Msg::Uninstalled(r) => {
                if cx.check(&r) {
                    cx.ok(t!("admin.apps.uninstalled").into_owned());
                    self.detail = None;
                    return self.load(cx);
                }
            }
            Msg::Marketplace => {
                let url = format!(
                    "{}/marketplace/explore",
                    cx.ctx.server.as_str().trim_end_matches('/')
                );
                cx.effects.push(crate::component::Effect::OpenUrl(url));
            }
        }
        Task::none()
    }

    pub fn view<'a>(&'a self, v: &V<'a>) -> Element<'a, Message> {
        let m = Message::Apps;
        let can_manage = has_permission(v.ctx.workspace, "manage-apps", None);
        if let Some(d) = &self.detail {
            return detail_view(d, can_manage);
        }
        let mut col = column![title_row(
            t!("admin.section.apps").into_owned(),
            vec![
                secondary_button(t!("admin.refresh"), Some(m(Msg::Reload)))
                    .icon(Icon::RefreshCw)
                    .into(),
                primary_button(t!("admin.apps.marketplace"), Some(m(Msg::Marketplace)))
                    .icon(Icon::ExternalLink)
                    .into(),
            ]
        )]
        .spacing(SPACE_4);
        if let Some(c) = &self.count {
            col = col.push(muted(t!(
                "admin.apps.count",
                marketplace = c.total_marketplace_enabled,
                max_marketplace = c.max_marketplace_apps,
                private = c.total_private_enabled,
                max_private = c.max_private_apps
            )));
        }
        if let Some(e) = &self.error {
            col = col.push(banner(BannerKind::Error, e.clone(), None));
        }
        if self.loaded && self.apps.is_empty() {
            return col.push(muted(t!("admin.apps.empty"))).into();
        }
        let mut list = column![].spacing(0);
        for a in &self.apps {
            let enabled = a.status.is_enabled();
            let id = a.id.clone();
            list = list.push(
                row![
                    button(
                        column![
                            row![
                                text(a.name.clone()).size(TEXT_BODY).font(FONT_MEDIUM),
                                muted(a.version.clone()),
                                if a.private {
                                    chip(t!("admin.apps.private"), ChipKind::Neutral)
                                } else {
                                    container(text("")).into()
                                },
                            ]
                            .spacing(SPACE_2)
                            .align_y(Alignment::Center),
                            muted(a.description.clone()),
                            muted(format!("{} · {}", a.author.name, a.status.as_str())),
                        ]
                        .spacing(2),
                    )
                    .padding([SPACE_2, SPACE_3])
                    .width(Length::Fill)
                    .style(style::button_list_item(false))
                    .on_press(m(Msg::Open(a.id.clone()))),
                    container(switch(
                        String::new(),
                        enabled,
                        can_manage.then_some(move |b| Message::Apps(Msg::Toggle(id.clone(), b)))
                    ))
                    .width(64)
                    .padding([0.0, SPACE_3]),
                ]
                .align_y(Alignment::Center),
            );
        }
        col.push(
            container(list)
                .style(style::container_card)
                .width(Length::Fill),
        )
        .into()
    }

    pub fn overlay<'a>(&'a self, _v: &V<'a>) -> Option<Element<'a, Message>> {
        None
    }
}

fn detail_view<'a>(d: &'a Detail, can_manage: bool) -> Element<'a, Message> {
    let m = Message::Apps;
    let mut col = column![back_row(
        d.app.name.clone(),
        m(Msg::Back),
        vec![
            danger_button(
                t!("admin.apps.uninstall"),
                can_manage.then_some(m(Msg::Uninstall))
            )
            .icon(Icon::Trash2)
            .into()
        ]
    )]
    .spacing(SPACE_4);
    if let Some(e) = &d.error {
        col = col.push(banner(BannerKind::Error, e.clone(), None));
    }
    col = col.push(card(
        t!("admin.apps.details").into_owned(),
        column![
            kv(t!("admin.apps.version").into_owned(), d.app.version.clone()),
            kv(
                t!("admin.apps.author").into_owned(),
                d.app.author.name.clone()
            ),
            kv(
                t!("admin.apps.status").into_owned(),
                d.app.status.as_str().to_owned()
            ),
            kv("ID".into(), d.app.id.clone()),
            muted(d.app.description.clone()),
        ]
        .spacing(SPACE_2),
    ));
    let mut settings = column![].spacing(SPACE_3);
    for s in &d.settings {
        if s.hidden == Some(true) {
            continue;
        }
        let label = d.tr(&s.i18n_label).to_owned();
        let hint = s.i18n_description.as_deref().map(|k| d.tr(k).to_owned());
        let v = d.value(s);
        let id = s.id.clone();
        let on = move |val: Value| Message::Apps(Msg::Value(id.clone(), val));
        let w: Element<'a, Message> = match s.kind.as_str() {
            "boolean" => switch(
                label.clone(),
                v.as_bool().unwrap_or(false),
                can_manage.then_some(move |b| on(Value::Bool(b))),
            ),
            "select" => {
                let opts: Vec<Choice> = s
                    .values
                    .clone()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|o| Choice {
                        label: d.tr(&o.i18n_label).to_owned(),
                        key: o.key,
                    })
                    .collect();
                let sel = opts
                    .iter()
                    .find(|o| Some(o.key.as_str()) == v.as_str())
                    .cloned();
                field(
                    label.clone(),
                    hint.clone(),
                    pick_list(opts, sel, move |c: Choice| on(Value::String(c.key)))
                        .text_size(TEXT_BODY)
                        .width(Length::Fill)
                        .style(style::pick_list)
                        .menu_style(style::menu),
                )
            }
            _ => {
                let txt = d.texts.get(&s.id).map(String::as_str).unwrap_or_default();
                let secure = s.kind == "password";
                let id = s.id.clone();
                field(
                    label.clone(),
                    hint.clone(),
                    input(
                        txt,
                        can_manage
                            .then_some(move |x: String| Message::Apps(Msg::Text(id.clone(), x))),
                    )
                    .secure(secure)
                    .font(if s.kind == "code" {
                        MONOSPACE_FONT
                    } else {
                        rustket_ui::theme::DEFAULT_FONT
                    }),
                )
            }
        };
        settings = settings.push(w);
        if s.kind == "boolean"
            && let Some(h) = hint
        {
            settings = settings.push(muted(h));
        }
    }
    if d.settings.is_empty() {
        settings = settings.push(muted(t!("admin.apps.no_settings")));
    } else if can_manage {
        settings = settings.push(row![
            container(text("")).width(Length::Fill),
            primary_button(
                t!("admin.save"),
                (!d.busy && !d.changed().is_empty()).then_some(m(Msg::SaveSettings))
            )
            .loading(d.busy)
        ]);
    }
    col = col.push(card(t!("admin.apps.settings").into_owned(), settings));
    if let Some(logs) = &d.logs {
        let mut l = column![].spacing(SPACE_2);
        for item in &logs.logs {
            let entries: Vec<String> = item
                .entries
                .iter()
                .map(|e| {
                    format!(
                        "[{}] {} {}",
                        e.severity,
                        e.caller,
                        e.args
                            .iter()
                            .map(|a| match a {
                                Value::String(s) => s.clone(),
                                other => other.to_string(),
                            })
                            .collect::<Vec<_>>()
                            .join(" ")
                    )
                })
                .collect();
            l = l.push(
                column![
                    text(format!("{} · {}", date(item.created_at), item.method))
                        .size(TEXT_SMALL)
                        .font(FONT_MEDIUM),
                    readonly(entries.join("\n")),
                ]
                .spacing(SPACE_2),
            );
        }
        if logs.logs.is_empty() {
            l = l.push(muted(t!("admin.apps.no_logs")));
        }
        col = col.push(card(
            t!("admin.apps.logs", total = logs.total).into_owned(),
            l,
        ));
    }
    col.into()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    key: String,
    label: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}
