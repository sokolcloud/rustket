//! Native administration panel (docs/admin.md §22.3): the section list gated
//! by permissions and license (`rustket_core::admin::visible_sections`), one
//! page per section, the generic settings editor, the 2FA prompt with replay
//! of the pending call, confirmations, and the "Open in browser" fallback.
//! Owner: admin agent. Strings: `locales/admin.yml` (`admin.*`); server-defined
//! metadata (setting labels, permission names, server errors) goes through
//! `ServerI18n`, fetched per server and cached in memory.

pub mod apps;
pub mod common;
pub mod emoji;
pub mod guard;
pub mod integrations;
pub mod invites;
pub mod mailer;
pub mod moderation;
pub mod oauth;
pub mod permissions;
pub mod rooms;
pub mod settings;
pub mod sounds;
pub mod statuses;
pub mod users;
pub mod workspace;

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length, Subscription, Task};
use rocketchat::models::admin::LicenseInfo;
use rocketchat::session::SessionEvent;
use rust_i18n::t;
use rustket_core::admin::server_i18n::{ServerI18n, ServerI18nCache};
use rustket_core::admin::{AdminSection, VisibleSection, visible_sections};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_BODY, TEXT_CAPTION,
    TEXT_SMALL, TEXT_TITLE,
};
use rustket_ui::widget::{
    ChipKind, chip, danger_button, dialog, divider, empty_state, icon_button, modal_keyed,
    primary_button, secondary_button,
};

use crate::component::{Action, Ctx, Effect, ViewCtx};
use crate::error::Res;
use common::{Confirm, Cx};
use guard::{Done, Op, Outcome};

/// Parsed server translation files per server version and language.
static I18N_CACHE: LazyLock<Mutex<ServerI18nCache>> = LazyLock::new(Mutex::default);

/// UI language plus its server translation file and the English one.
pub type I18nFiles = (
    String,
    Arc<HashMap<String, String>>,
    Arc<HashMap<String, String>>,
);

/// Read-only data for page views.
pub struct V<'a> {
    pub ctx: ViewCtx<'a>,
    pub i18n: &'a ServerI18n,
    pub license: Option<&'a LicenseInfo>,
}

pub enum Page {
    Home,
    Workspace(Box<workspace::State>),
    Users(Box<users::State>),
    Rooms(Box<rooms::State>),
    Invites(Box<invites::State>),
    Emoji(Box<emoji::State>),
    Sounds(Box<sounds::State>),
    Statuses(Box<statuses::State>),
    Integrations(Box<integrations::State>),
    OAuth(Box<oauth::State>),
    Mailer(Box<mailer::State>),
    Permissions(Box<permissions::State>),
    Inboxes(Box<mailer::Inboxes>),
    Moderation(Box<moderation::State>),
    Apps(Box<apps::State>),
    Settings(Box<settings::State>),
    /// EE section without its license module.
    Premium(AdminSection),
    /// Not native: "Open in browser".
    Browser(AdminSection),
}

#[derive(Debug, Clone)]
pub enum Message {
    Select(Option<AdminSection>),
    OpenInBrowser(AdminSection),
    License(Res<LicenseInfo>),
    I18n(Res<I18nFiles>),
    Close,
    Workspace(workspace::Msg),
    Users(users::Msg),
    Rooms(rooms::Msg),
    Invites(invites::Msg),
    Emoji(emoji::Msg),
    Sounds(sounds::Msg),
    Statuses(statuses::Msg),
    Integrations(integrations::Msg),
    OAuth(oauth::Msg),
    Mailer(mailer::Msg),
    Inboxes(mailer::InboxMsg),
    Permissions(permissions::Msg),
    Moderation(moderation::Msg),
    Apps(apps::Msg),
    Settings(settings::Msg),
    Guard(guard::Msg),
    /// A 2FA-capable call finished.
    Guarded(Op, Res<Done>),
    /// Confirmation dialog answered.
    Confirmed(bool),
    Noop,
}

pub struct Admin {
    pub page: Page,
    pub section: Option<AdminSection>,
    pub license: Option<LicenseInfo>,
    pub i18n: ServerI18n,
    pub i18n_loaded: bool,
    pub guard: Option<guard::Prompt>,
    pub confirm: Option<Confirm>,
}

impl Default for Admin {
    fn default() -> Self {
        Self::new()
    }
}

impl Admin {
    pub fn new() -> Self {
        Self {
            page: Page::Home,
            section: None,
            license: None,
            i18n: ServerI18n::default(),
            i18n_loaded: false,
            guard: None,
            confirm: None,
        }
    }

    /// Loads the license (EE gating) and the server's translations.
    pub fn open(&mut self, ctx: &mut Ctx<'_>) -> Task<Message> {
        let license = ctx.rest_task(
            |r| async move { r.admin_licenses_info(false).await },
            Message::License,
        );
        let version = ctx
            .workspace
            .server_info()
            .and_then(|i| i.version.clone())
            .unwrap_or_default();
        let lang = rust_i18n::locale().to_string();
        let lang = if lang.starts_with("ru") {
            "ru".to_owned()
        } else {
            "en".to_owned()
        };
        let i18n = ctx.rest_task(
            move |r| async move {
                let cached = |l: &str| I18N_CACHE.lock().ok().and_then(|c| c.get(&version, l));
                let en = match cached("en") {
                    Some(f) => f,
                    None => {
                        let f = r.server_translations("en").await?;
                        I18N_CACHE
                            .lock()
                            .map(|mut c| c.insert(&version, "en", f.clone()))
                            .unwrap_or_else(|_| Arc::new(f))
                    }
                };
                let local = if lang == "en" {
                    en.clone()
                } else {
                    match cached(&lang) {
                        Some(f) => f,
                        // Missing languages fall back to English per key.
                        None => match r.server_translations(&lang).await {
                            Ok(f) => I18N_CACHE
                                .lock()
                                .map(|mut c| c.insert(&version, &lang, f.clone()))
                                .unwrap_or_else(|_| Arc::new(f)),
                            Err(_) => Arc::new(HashMap::new()),
                        },
                    }
                };
                Ok((lang, local, en))
            },
            Message::I18n,
        );
        Task::batch([license, i18n])
    }

    pub fn sections(&self, ws: &rustket_core::Workspace) -> Vec<VisibleSection> {
        visible_sections(ws, self.license.as_ref())
    }

    /// Opens `section` (`None`: the overview).
    pub fn select(&mut self, section: Option<AdminSection>, ctx: &mut Ctx<'_>) -> Action<Message> {
        self.section = section;
        self.confirm = None;
        let Some(s) = section else {
            self.page = Page::Home;
            return Action::none();
        };
        let locked = self
            .sections(ctx.workspace)
            .iter()
            .find(|v| v.section == s)
            .is_some_and(|v| v.premium_locked);
        let mut cx = Cx {
            ctx,
            i18n: &self.i18n,
            license: self.license.as_ref(),
            effects: Vec::new(),
            confirm: None,
        };
        let (page, task) = match s {
            _ if locked => (Page::Premium(s), Task::none()),
            AdminSection::Workspace | AdminSection::Reports => {
                let mut p = workspace::State::new(s == AdminSection::Reports);
                let t = p.load(&mut cx);
                (Page::Workspace(Box::new(p)), t)
            }
            AdminSection::Users => {
                let mut p = users::State::new();
                let t = p.load(&mut cx);
                (Page::Users(Box::new(p)), t)
            }
            AdminSection::Rooms => {
                let mut p = rooms::State::new();
                let t = p.load(&mut cx);
                (Page::Rooms(Box::new(p)), t)
            }
            AdminSection::Invites => {
                let mut p = invites::State::new();
                let t = p.load(&mut cx);
                (Page::Invites(Box::new(p)), t)
            }
            AdminSection::Emoji => {
                let mut p = emoji::State::new();
                let t = p.load(&mut cx);
                (Page::Emoji(Box::new(p)), t)
            }
            AdminSection::Sounds => {
                let mut p = sounds::State::new();
                let t = p.load(&mut cx);
                (Page::Sounds(Box::new(p)), t)
            }
            AdminSection::UserStatus => {
                let mut p = statuses::State::new();
                let t = p.load(&mut cx);
                (Page::Statuses(Box::new(p)), t)
            }
            AdminSection::Integrations => {
                let mut p = integrations::State::new();
                let t = p.load(&mut cx);
                (Page::Integrations(Box::new(p)), t)
            }
            AdminSection::ThirdPartyLogin => {
                let mut p = oauth::State::new();
                let t = p.load(&mut cx);
                (Page::OAuth(Box::new(p)), t)
            }
            AdminSection::Mailer => (Page::Mailer(Box::default()), Task::none()),
            AdminSection::EmailInboxes => {
                let mut p = mailer::Inboxes::new();
                let t = p.load(&mut cx);
                (Page::Inboxes(Box::new(p)), t)
            }
            AdminSection::Permissions => {
                let mut p = permissions::State::new();
                let t = p.load(&mut cx);
                (Page::Permissions(Box::new(p)), t)
            }
            AdminSection::Moderation => {
                let mut p = moderation::State::new();
                let t = p.load(&mut cx);
                (Page::Moderation(Box::new(p)), t)
            }
            AdminSection::Apps => {
                let mut p = apps::State::new();
                let t = p.load(&mut cx);
                (Page::Apps(Box::new(p)), t)
            }
            AdminSection::Settings => {
                let mut p = settings::State::new();
                let t = p.load(&mut cx);
                (Page::Settings(Box::new(p)), t)
            }
            _ => (Page::Browser(s), Task::none()),
        };
        let effects = std::mem::take(&mut cx.effects);
        self.page = page;
        Action { task, effects }
    }

    pub fn update(&mut self, msg: Message, ctx: &mut Ctx<'_>) -> Action<Message> {
        match msg {
            Message::Select(s) => {
                if self.dirty_settings(ctx) && s != self.section {
                    self.confirm = Some(Confirm::ask(
                        t!("admin.unsaved_title").into_owned(),
                        t!("admin.unsaved_body").into_owned(),
                        t!("admin.discard").into_owned(),
                        Message::Settings(settings::Msg::DiscardAndGo(s)),
                    ));
                    return Action::none();
                }
                self.select(s, ctx)
            }
            Message::OpenInBrowser(s) => {
                let url = format!(
                    "{}/{}",
                    ctx.server.as_str().trim_end_matches('/'),
                    s.web_path()
                );
                Action::effect(Effect::OpenUrl(url))
            }
            Message::License(r) => {
                match r {
                    Ok(l) => self.license = Some(l),
                    Err(e) => tracing::debug!(error = %e, "licenses.info failed"),
                }
                Action::none()
            }
            Message::I18n(r) => {
                self.i18n_loaded = true;
                match r {
                    Ok((lang, local, en)) => {
                        let custom = ctx
                            .workspace
                            .settings()
                            .str("Custom_Translations")
                            .unwrap_or_default()
                            .to_owned();
                        self.i18n =
                            ServerI18n::new(&lang, local, en).with_custom_translations(&custom);
                    }
                    Err(e) => tracing::debug!(error = %e, "server translations unavailable"),
                }
                Action::none()
            }
            Message::Close => {
                if self.dirty_settings(ctx) {
                    self.confirm = Some(Confirm::ask(
                        t!("admin.unsaved_title").into_owned(),
                        t!("admin.unsaved_body").into_owned(),
                        t!("admin.discard").into_owned(),
                        Message::Settings(settings::Msg::DiscardAndClose),
                    ));
                    return Action::none();
                }
                Action::effect(Effect::Close)
            }
            Message::Guard(m) => self.on_guard(m, ctx),
            Message::Guarded(op, r) => match guard::classify(&op, self.guard.as_ref(), r) {
                Outcome::Prompt(p) => {
                    self.guard = Some(*p);
                    Action::task(iced::widget::operation::focus(guard::INPUT_ID))
                }
                Outcome::Invalid => {
                    if let Some(p) = &mut self.guard {
                        p.busy = false;
                        p.code.clear();
                        p.error = Some(t!("admin.tfa_invalid").into_owned());
                    }
                    Action::none()
                }
                Outcome::Done(r) => {
                    if self.guard.as_ref().is_some_and(|p| p.op == op) {
                        self.guard = None;
                    }
                    self.with_page(ctx, |page, cx| page.on_guarded(&op, &r, cx))
                }
            },
            Message::Confirmed(yes) => {
                let Some(c) = self.confirm.take() else {
                    return Action::none();
                };
                if yes {
                    self.update(*c.yes, ctx)
                } else {
                    Action::none()
                }
            }
            Message::Settings(settings::Msg::DiscardAndGo(s)) => {
                if let Page::Settings(p) = &mut self.page {
                    p.discard();
                }
                self.select(s, ctx)
            }
            Message::Settings(settings::Msg::DiscardAndClose) => Action::effect(Effect::Close),
            Message::Noop => Action::none(),
            m => self.with_page(ctx, move |page, cx| page.update(m, cx)),
        }
    }

    fn dirty_settings(&self, ctx: &Ctx<'_>) -> bool {
        let _ = ctx;
        matches!(&self.page, Page::Settings(p) if p.is_dirty())
    }

    fn on_guard(&mut self, m: guard::Msg, ctx: &mut Ctx<'_>) -> Action<Message> {
        let Some(p) = &mut self.guard else {
            return Action::none();
        };
        match m {
            guard::Msg::Code(c) => {
                p.code = c;
                p.error = None;
                Action::none()
            }
            guard::Msg::Submit => {
                if p.code.trim().is_empty() || p.busy {
                    return Action::none();
                }
                p.busy = true;
                let code = p.code();
                // The secret is not kept beyond this retry.
                p.code.clear();
                Action::task(guard::run(ctx, p.op.clone(), Some(code)))
            }
            guard::Msg::Cancel => {
                let op = p.op.clone();
                self.guard = None;
                self.with_page(ctx, |page, cx| page.on_guard_cancelled(&op, cx))
            }
        }
    }

    /// Runs `f` on the current page with a [`Cx`], collecting effects and a
    /// confirmation request.
    fn with_page(
        &mut self,
        ctx: &mut Ctx<'_>,
        f: impl FnOnce(&mut Page, &mut Cx<'_, '_>) -> Task<Message>,
    ) -> Action<Message> {
        let mut cx = Cx {
            ctx,
            i18n: &self.i18n,
            license: self.license.as_ref(),
            effects: Vec::new(),
            confirm: None,
        };
        let task = f(&mut self.page, &mut cx);
        let effects = std::mem::take(&mut cx.effects);
        if let Some(c) = cx.confirm.take() {
            self.confirm = Some(c);
        }
        Action { task, effects }
    }

    /// Realtime events the open page cares about (settings and permission
    /// changes). Called by the app for every event of the server.
    pub fn on_event(&mut self, ev: &SessionEvent) {
        match (&mut self.page, ev) {
            (
                Page::Settings(p),
                SessionEvent::SettingChanged {
                    kind,
                    setting,
                    public: false,
                },
            ) => {
                p.on_setting_event(*kind, setting);
            }
            (Page::Permissions(p), SessionEvent::PermissionChanged { kind, permission }) => {
                p.on_permission_event(*kind, permission);
            }
            _ => {}
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        match &self.page {
            Page::Settings(p) => p.subscription().map(Message::Settings),
            _ => Subscription::none(),
        }
    }

    pub fn view<'a>(&'a self, ctx: ViewCtx<'a>) -> Element<'a, Message> {
        let sections = self.sections(ctx.workspace);
        let v = V {
            ctx,
            i18n: &self.i18n,
            license: self.license.as_ref(),
        };
        let nav = self.nav(&sections);
        let body: Element<'a, Message> = match &self.page {
            Page::Home => self.home(&sections),
            Page::Workspace(p) => p.view(&v),
            Page::Users(p) => p.view(&v),
            Page::Rooms(p) => p.view(&v),
            Page::Invites(p) => p.view(&v),
            Page::Emoji(p) => p.view(&v),
            Page::Sounds(p) => p.view(&v),
            Page::Statuses(p) => p.view(&v),
            Page::Integrations(p) => p.view(&v),
            Page::OAuth(p) => p.view(&v),
            Page::Mailer(p) => p.view(&v),
            Page::Inboxes(p) => p.view(&v),
            Page::Permissions(p) => p.view(&v),
            Page::Moderation(p) => p.view(&v),
            Page::Apps(p) => p.view(&v),
            Page::Settings(p) => p.view(&v),
            Page::Premium(s) => fallback(*s, true),
            Page::Browser(s) => fallback(*s, false),
        };
        // The settings page keeps its save bar outside the scrolled area.
        let footer = match &self.page {
            Page::Settings(p) => p.footer(&v),
            _ => None,
        };
        let mut content = column![
            scrollable(container(body).padding(SPACE_6).width(Length::Fill))
                .height(Length::Fill)
                .style(style::scrollable)
        ];
        if let Some(f) = footer {
            content = content.push(divider()).push(f);
        }
        let base = container(
            column![
                row![
                    icon::sized(Icon::Shield, 20.0),
                    text(t!("admin.title"))
                        .size(TEXT_TITLE)
                        .font(FONT_SEMIBOLD)
                        .width(Length::Fill),
                    icon_button(Icon::X, Some(Message::Close), t!("admin.close")),
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center)
                .padding([SPACE_4, SPACE_6]),
                divider(),
                row![nav, content.width(Length::Fill)].height(Length::Fill),
            ]
            .height(Length::Fill),
        )
        .width(Length::Fill)
        .height(Length::Fill)
        .style(style::container_room);
        let overlay = self.overlay(&v);
        modal_keyed(base, overlay, Some(self.dismiss()))
    }

    fn dismiss(&self) -> Message {
        if self.guard.is_some() {
            Message::Guard(guard::Msg::Cancel)
        } else if self.confirm.is_some() {
            Message::Confirmed(false)
        } else {
            match &self.page {
                Page::Users(_) => Message::Users(users::Msg::CloseDialog),
                Page::Rooms(_) => Message::Rooms(rooms::Msg::CloseDialog),
                Page::Emoji(_) => Message::Emoji(emoji::Msg::CloseDialog),
                Page::Sounds(_) => Message::Sounds(sounds::Msg::CloseDialog),
                Page::Statuses(_) => Message::Statuses(statuses::Msg::CloseDialog),
                Page::OAuth(_) => Message::OAuth(oauth::Msg::CloseDialog),
                Page::Inboxes(_) => Message::Inboxes(mailer::InboxMsg::CloseDialog),
                Page::Permissions(_) => Message::Permissions(permissions::Msg::CloseDialog),
                Page::Workspace(_) => Message::Workspace(workspace::Msg::CloseDialog),
                Page::Settings(_) => Message::Settings(settings::Msg::CloseDialog),
                Page::Apps(_) => Message::Apps(apps::Msg::CloseDialog),
                Page::Moderation(_) => Message::Moderation(moderation::Msg::CloseDialog),
                _ => Message::Noop,
            }
        }
    }

    fn overlay<'a>(&'a self, v: &V<'a>) -> Option<(u8, Element<'a, Message>)> {
        if let Some(p) = &self.guard {
            return Some((1, guard::view(p)));
        }
        if let Some(c) = &self.confirm {
            let yes = if c.danger {
                danger_button(c.yes_label.clone(), Some(Message::Confirmed(true)))
            } else {
                primary_button(c.yes_label.clone(), Some(Message::Confirmed(true)))
            };
            return Some((
                2,
                dialog(
                    c.title.clone(),
                    text(c.body.clone()).size(TEXT_BODY),
                    vec![
                        secondary_button(t!("admin.cancel"), Some(Message::Confirmed(false)))
                            .into(),
                        yes.into(),
                    ],
                    Some(Message::Confirmed(false)),
                ),
            ));
        }
        let e = match &self.page {
            Page::Users(p) => p.overlay(v),
            Page::Rooms(p) => p.overlay(v),
            Page::Emoji(p) => p.overlay(v),
            Page::Sounds(p) => p.overlay(v),
            Page::Statuses(p) => p.overlay(v),
            Page::OAuth(p) => p.overlay(v),
            Page::Inboxes(p) => p.overlay(v),
            Page::Permissions(p) => p.overlay(v),
            Page::Workspace(p) => p.overlay(v),
            Page::Settings(p) => p.overlay(v),
            Page::Apps(p) => p.overlay(v),
            Page::Moderation(p) => p.overlay(v),
            _ => None,
        };
        e.map(|e| (3, e))
    }

    fn nav<'a>(&'a self, sections: &[VisibleSection]) -> Element<'a, Message> {
        let mut col = column![nav_item(
            Icon::LayoutGrid,
            t!("admin.section.home").into_owned(),
            false,
            self.section.is_none(),
            Message::Select(None),
        )]
        .spacing(SPACE_1)
        .padding(SPACE_2);
        for v in sections {
            col = col.push(nav_item(
                section_icon(v.section),
                section_title(v.section),
                v.premium_locked,
                self.section == Some(v.section),
                Message::Select(Some(v.section)),
            ));
        }
        container(
            scrollable(col)
                .height(Length::Fill)
                .style(style::scrollable),
        )
        .width(232)
        .height(Length::Fill)
        .style(style::container_sidebar)
        .into()
    }

    fn home<'a>(&'a self, sections: &[VisibleSection]) -> Element<'a, Message> {
        let mut col =
            column![common::title_row(t!("admin.overview").into_owned(), vec![])].spacing(SPACE_4);
        if sections.is_empty() {
            return container(empty_state(
                Icon::ShieldOff,
                t!("admin.no_sections").into_owned(),
                String::new(),
            ))
            .height(320)
            .into();
        }
        let mut grid = column![].spacing(SPACE_3);
        for chunk in sections.chunks(3) {
            let mut r = row![].spacing(SPACE_3);
            for v in chunk {
                let mut head = row![
                    icon::sized(section_icon(v.section), 18.0),
                    text(section_title(v.section))
                        .size(TEXT_BODY)
                        .font(FONT_SEMIBOLD)
                        .width(Length::Fill)
                ]
                .spacing(SPACE_2)
                .align_y(Alignment::Center);
                if v.premium_locked {
                    head = head.push(chip(t!("admin.premium"), ChipKind::Warning));
                } else if !v.section.is_native() {
                    head = head
                        .push(icon::sized(Icon::ExternalLink, 14.0).style(style::text_secondary));
                }
                let card = button(
                    column![
                        head,
                        text(t!(format!("admin.section_hint.{}", v.section.key())))
                            .size(TEXT_SMALL)
                            .style(style::text_secondary)
                    ]
                    .spacing(SPACE_2),
                )
                .padding(SPACE_4)
                .width(Length::FillPortion(1))
                .style(style::button_secondary)
                .on_press(Message::Select(Some(v.section)));
                r = r.push(card);
            }
            for _ in chunk.len()..3 {
                r = r.push(container(text("")).width(Length::FillPortion(1)));
            }
            grid = grid.push(r);
        }
        col = col.push(grid);
        col.into()
    }
}

impl Page {
    fn update(&mut self, m: Message, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match (self, m) {
            (Page::Workspace(p), Message::Workspace(m)) => p.update(m, cx),
            (Page::Users(p), Message::Users(m)) => p.update(m, cx),
            (Page::Rooms(p), Message::Rooms(m)) => p.update(m, cx),
            (Page::Invites(p), Message::Invites(m)) => p.update(m, cx),
            (Page::Emoji(p), Message::Emoji(m)) => p.update(m, cx),
            (Page::Sounds(p), Message::Sounds(m)) => p.update(m, cx),
            (Page::Statuses(p), Message::Statuses(m)) => p.update(m, cx),
            (Page::Integrations(p), Message::Integrations(m)) => p.update(m, cx),
            (Page::OAuth(p), Message::OAuth(m)) => p.update(m, cx),
            (Page::Mailer(p), Message::Mailer(m)) => p.update(m, cx),
            (Page::Inboxes(p), Message::Inboxes(m)) => p.update(m, cx),
            (Page::Permissions(p), Message::Permissions(m)) => p.update(m, cx),
            (Page::Moderation(p), Message::Moderation(m)) => p.update(m, cx),
            (Page::Apps(p), Message::Apps(m)) => p.update(m, cx),
            (Page::Settings(p), Message::Settings(m)) => p.update(m, cx),
            // A result for a page that is no longer open.
            _ => Task::none(),
        }
    }

    fn on_guarded(&mut self, op: &Op, r: &Res<Done>, cx: &mut Cx<'_, '_>) -> Task<Message> {
        match self {
            Page::Users(p) => p.on_guarded(op, r, cx),
            Page::Settings(p) => p.on_guarded(op, r, cx),
            _ => {
                cx.check(r);
                Task::none()
            }
        }
    }

    fn on_guard_cancelled(&mut self, op: &Op, cx: &mut Cx<'_, '_>) -> Task<Message> {
        let _ = cx;
        match self {
            Page::Users(p) => p.on_guard_cancelled(op),
            Page::Settings(p) => p.on_guard_cancelled(op),
            _ => {}
        }
        Task::none()
    }
}

/// The admin screen view (the app passes its per-server view context).
pub fn view_in<'a>(admin: &'a Admin, ctx: ViewCtx<'a>) -> Element<'a, Message> {
    admin.view(ctx)
}

pub fn section_title(s: AdminSection) -> String {
    t!(format!("admin.section.{}", s.key())).into_owned()
}

pub fn section_icon(s: AdminSection) -> Icon {
    match s {
        AdminSection::Workspace => Icon::Info,
        AdminSection::Subscription => Icon::CreditCard,
        AdminSection::Engagement => Icon::ChartLine,
        AdminSection::Moderation => Icon::Flag,
        AdminSection::Rooms => Icon::Hash,
        AdminSection::Users => Icon::Users,
        AdminSection::AiCenter => Icon::Sparkles,
        AdminSection::Invites => Icon::Link,
        AdminSection::UserStatus => Icon::CircleDot,
        AdminSection::Permissions => Icon::Lock,
        AdminSection::DeviceManagement => Icon::Smartphone,
        AdminSection::EmailInboxes => Icon::Inbox,
        AdminSection::Mailer => Icon::Mail,
        AdminSection::ThirdPartyLogin => Icon::KeyRound,
        AdminSection::Integrations => Icon::Webhook,
        AdminSection::Import => Icon::Import,
        AdminSection::Reports => Icon::FileChartColumn,
        AdminSection::Sounds => Icon::Music,
        AdminSection::Emoji => Icon::Smile,
        AdminSection::Settings => Icon::Settings,
        AdminSection::Apps => Icon::Blocks,
    }
}

fn nav_item<'a>(
    ic: Icon,
    label: String,
    premium: bool,
    selected: bool,
    on_press: Message,
) -> Element<'a, Message> {
    let mut r = row![
        icon::sized(ic, 16.0),
        text(label).size(TEXT_BODY).width(Length::Fill)
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center);
    if premium {
        r = r.push(
            text(t!("admin.premium"))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
        );
    }
    button(r)
        .padding([6, 10])
        .width(Length::Fill)
        .style(style::button_list_item(selected))
        .on_press(on_press)
        .into()
}

/// EE-only or not native: explanation and "Open in browser".
fn fallback<'a>(s: AdminSection, premium: bool) -> Element<'a, Message> {
    let (title, body) = if premium {
        (
            t!("admin.premium_title", section = section_title(s)).into_owned(),
            t!("admin.premium_body").into_owned(),
        )
    } else {
        (section_title(s), t!("admin.browser_body").into_owned())
    };
    column![
        common::title_row(title, vec![]),
        text(body).size(TEXT_BODY).style(style::text_secondary),
        primary_button(t!("admin.open_in_browser"), Some(Message::OpenInBrowser(s)))
            .icon(Icon::ExternalLink),
    ]
    .spacing(SPACE_4)
    .into()
}
