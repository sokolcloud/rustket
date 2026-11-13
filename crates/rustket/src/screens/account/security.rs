//! Security tab: TOTP two-factor authentication (`users.enableTotp`,
//! `users.validateTotp`, `users.disableTotp`, `users.regenerateTotpCodes`;
//! the `2fa:*` methods on older servers) and e-mail 2FA
//! (`users.2fa.enableEmail` / `disableEmail`); active sessions / devices
//! (`sessions/list`, `sessions/logout.me`: the `device-management` license
//! module, so Community servers answer "enterprise feature" and the section
//! says it is not available).

use iced::widget::{column, container, qr_code, row, text};
use iced::{Alignment, Color, Element, Length};
use rocketchat::models::Enabled;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL, font_mono,
    font_semibold,
};
use rustket_ui::widget::{
    BannerKind, ChipKind, above, banner, chip, danger_button, dialog, ghost_button, primary_button,
    secondary_button, text_field,
};

use super::api::{self, TotpSetup};
use super::two_factor::{self, Op};
use super::{Message, section};
use crate::component::{Action, Ctx, ViewCtx};
use crate::error::Res;

pub const SETUP_INPUT_ID: &str = "account-totp-setup";

#[derive(Debug, Clone)]
pub enum Msg {
    EnableTotp,
    SetupCode(String),
    VerifySetup,
    CancelSetup,
    DisableCode(String),
    DisableTotp,
    Disabled(Res<bool>),
    RegenCode(String),
    Regenerate,
    Regenerated(Res<Option<Vec<String>>>),
    Remaining(Res<u64>),
    CloseCodes,
    EnableEmail,
    EmailEnabled(Res<()>),
    DisableEmail,
    LoadSessions,
    Sessions(Res<Vec<DeviceSession>>),
    /// Ask before logging out session `sessionId`.
    LogoutSession(String),
    ConfirmLogoutSession,
    CancelLogoutSession,
    SessionLoggedOut(String, Res<()>),
    /// End-to-end encryption section.
    E2e(super::e2e::Msg),
}

/// One login session of the user (`sessions/list`).
#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct DeviceSession {
    pub session_id: String,
    pub device: Option<SessionDevice>,
    pub host: Option<String>,
    pub ip: Option<String>,
    pub login_at: Option<rocketchat::RcDate>,
    /// The session of this client's token.
    pub current: bool,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct SessionDevice {
    pub name: Option<String>,
    #[serde(rename = "type")]
    pub kind: Option<String>,
    pub version: Option<String>,
    pub os: Option<SessionOs>,
}

#[derive(Debug, Clone, Default, PartialEq, serde::Deserialize)]
#[serde(default)]
pub struct SessionOs {
    pub name: Option<String>,
    pub version: Option<String>,
}

fn join_words(parts: [Option<String>; 2]) -> String {
    parts
        .into_iter()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

impl DeviceSession {
    /// "Chrome 120 · Linux". The server keeps no name for clients it does not
    /// know (Rustket included): this client's own session shows as
    /// "Rustket <version>", other unnamed ones as "Unknown app".
    pub fn title(&self) -> String {
        let d = self.device.as_ref();
        let mut client = join_words([
            d.and_then(|d| d.name.clone()),
            d.and_then(|d| d.version.clone()),
        ]);
        if d.and_then(|d| d.name.as_deref())
            .is_none_or(|n| n.trim().is_empty())
        {
            client = if self.current {
                concat!("Rustket ", env!("CARGO_PKG_VERSION")).to_owned()
            } else {
                t!("account.session_unknown_app").into_owned()
            };
        }
        let os = d
            .and_then(|d| d.os.as_ref())
            .map(|o| join_words([o.name.clone(), o.version.clone()]))
            .unwrap_or_default();
        if os.is_empty() {
            client
        } else {
            format!("{client} · {os}")
        }
    }
}

/// Sessions of the user (`sessions/list`, newest login first).
pub async fn sessions_list(
    rest: &rocketchat::RestClient,
) -> rocketchat::Result<Vec<DeviceSession>> {
    let v: serde_json::Value = rest
        .get(
            "sessions/list",
            &rocketchat::rest::Query::new().push("count", "50"),
        )
        .await?;
    let list = v.get("sessions").cloned().unwrap_or_default();
    Ok(serde_json::from_value(list)?)
}

/// Logs out one of the user's sessions (`sessions/logout.me`).
pub async fn sessions_logout(
    rest: &rocketchat::RestClient,
    session_id: &str,
) -> rocketchat::Result<()> {
    rest.post_ok(
        "sessions/logout.me",
        &serde_json::json!({ "sessionId": session_id }),
    )
    .await
}

/// The server has no device management: Community Edition ("enterprise
/// feature", `error-action-not-allowed`), forbidden or unknown endpoint.
pub fn sessions_unavailable(e: &rocketchat::Error) -> bool {
    use rocketchat::Error;
    match e {
        Error::Status { status, .. } => matches!(status, 403 | 404),
        Error::Api {
            status,
            error,
            error_type,
            ..
        } => {
            matches!(status, 403 | 404)
                || error_type.as_deref() == Some("error-action-not-allowed")
                || error
                    .as_deref()
                    .is_some_and(|m| m.contains("enterprise feature"))
        }
        _ => false,
    }
}

/// State of the sessions section.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum SessionsState {
    #[default]
    Idle,
    Loading,
    Loaded(Vec<DeviceSession>),
    /// Device management is not available on this server.
    Unavailable,
    Failed(String),
}

/// TOTP being set up: the secret, its QR code and the confirmation code.
pub struct Pending {
    pub setup: TotpSetup,
    pub qr: Option<qr_code::Data>,
    pub code: String,
    pub error: Option<String>,
    pub busy: bool,
    /// The password given to start the setup, sent again to confirm it.
    pub second: Option<rocketchat::auth::TwoFactorCode>,
}

impl std::fmt::Debug for Pending {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pending")
            .field("qr", &self.qr.is_some())
            .field("busy", &self.busy)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Busy {
    Starting,
    Disabling,
    Regenerating,
    Email,
}

#[derive(Debug, Default)]
pub struct Security {
    pub pending: Option<Pending>,
    pub disable_code: String,
    pub regen_code: String,
    pub busy: Option<Busy>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub remaining: Option<u64>,
    /// Backup codes to show once (after enabling or regenerating).
    pub codes: Option<Vec<String>>,
    pub sessions: SessionsState,
    /// Session waiting for the logout confirmation.
    pub confirm_logout: Option<String>,
    /// Session being logged out.
    pub logging_out: Option<String>,
    /// End-to-end encryption section.
    pub e2e: super::e2e::E2eSection,
}

/// Flags of the second factors on the workspace user. `services.*` changes
/// are not streamed (the session strips them), so they are patched here.
pub fn set_factor(ws: &mut Workspace, totp: Option<bool>, email: Option<bool>) {
    let Some(mut me) = ws.me().cloned() else {
        return;
    };
    let services = me.services.get_or_insert_with(Default::default);
    if let Some(on) = totp {
        services.totp = Some(Enabled { enabled: on });
    }
    if let Some(on) = email {
        services.email2fa = Some(Enabled { enabled: on });
    }
    ws.apply_me(me);
}

impl Security {
    /// Loads the number of remaining backup codes when TOTP is on.
    /// Also (re)loads the sessions.
    pub fn refresh(&mut self, ctx: &Ctx<'_>) -> Action<Message> {
        let sessions = self.load_sessions(ctx);
        if !ctx.workspace.me().is_some_and(|m| m.totp_enabled()) {
            return Action::task(sessions);
        }
        Action::task(iced::Task::batch([
            sessions,
            ctx.rest_task(
                |rest| async move { api::totp_codes_remaining(&rest).await },
                |r| Message::Security(Msg::Remaining(r)),
            ),
        ]))
    }

    fn load_sessions(&mut self, ctx: &Ctx<'_>) -> iced::Task<Message> {
        if self.sessions == SessionsState::Loading {
            return iced::Task::none();
        }
        self.sessions = SessionsState::Loading;
        ctx.rest_task(
            |rest| async move { sessions_list(&rest).await },
            |r| Message::Security(Msg::Sessions(r)),
        )
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut Ctx<'_>) -> Action<Message> {
        let s = |m| Message::Security(m);
        match msg {
            Msg::E2e(m) => self.e2e.update(m, ctx),
            Msg::EnableTotp => {
                if self.busy.is_some() {
                    return Action::none();
                }
                self.busy = Some(Busy::Starting);
                self.error = None;
                self.notice = None;
                Action::task(two_factor::run(ctx, Op::EnableTotp, None))
            }
            Msg::SetupCode(c) => {
                if let Some(p) = self.pending.as_mut() {
                    p.code = c;
                }
                Action::none()
            }
            Msg::VerifySetup => {
                let Some(p) = self.pending.as_mut() else {
                    return Action::none();
                };
                let code: String = p.code.chars().filter(|c| !c.is_whitespace()).collect();
                if p.busy || code.is_empty() {
                    return Action::none();
                }
                p.busy = true;
                p.error = None;
                Action::task(two_factor::run(
                    ctx,
                    Op::ValidateTotp { code },
                    p.second.clone(),
                ))
            }
            Msg::CancelSetup => {
                // Not while the confirmation is in flight: its backup codes
                // would be lost.
                if !self.pending.as_ref().is_some_and(|p| p.busy) {
                    self.pending = None;
                }
                Action::none()
            }
            Msg::DisableCode(c) => {
                self.disable_code = c;
                Action::none()
            }
            Msg::DisableTotp => {
                let code = clean(&self.disable_code);
                if self.busy.is_some() || code.is_empty() {
                    return Action::none();
                }
                self.busy = Some(Busy::Disabling);
                self.error = None;
                self.notice = None;
                Action::task(ctx.rest_task(
                    move |rest| async move { api::totp_disable(&rest, &code).await },
                    move |r| s(Msg::Disabled(r)),
                ))
            }
            Msg::Disabled(r) => {
                self.busy = None;
                match r {
                    Ok(true) => {
                        self.disable_code.clear();
                        self.remaining = None;
                        self.notice = Some(t!("account.totp_disabled_notice").into_owned());
                        set_factor(ctx.workspace, Some(false), None);
                    }
                    Ok(false) => self.error = Some(t!("account.tfa_invalid").into_owned()),
                    Err(e) => self.error = Some(e.user_text()),
                }
                Action::none()
            }
            Msg::RegenCode(c) => {
                self.regen_code = c;
                Action::none()
            }
            Msg::Regenerate => {
                let code = clean(&self.regen_code);
                if self.busy.is_some() || code.is_empty() {
                    return Action::none();
                }
                self.busy = Some(Busy::Regenerating);
                self.error = None;
                self.notice = None;
                Action::task(ctx.rest_task(
                    move |rest| async move { api::totp_regenerate_codes(&rest, &code).await },
                    move |r| s(Msg::Regenerated(r)),
                ))
            }
            Msg::Regenerated(r) => {
                self.busy = None;
                match r {
                    Ok(Some(codes)) => {
                        self.regen_code.clear();
                        self.remaining = Some(codes.len() as u64);
                        self.codes = Some(codes);
                    }
                    Ok(None) => self.error = Some(t!("account.tfa_invalid").into_owned()),
                    Err(e) => self.error = Some(e.user_text()),
                }
                Action::none()
            }
            Msg::Remaining(r) => {
                if let Ok(n) = r {
                    self.remaining = Some(n);
                }
                Action::none()
            }
            Msg::CloseCodes => {
                self.codes = None;
                Action::none()
            }
            Msg::EnableEmail => {
                if self.busy.is_some() {
                    return Action::none();
                }
                self.busy = Some(Busy::Email);
                self.error = None;
                self.notice = None;
                Action::task(ctx.rest_task(
                    |rest| async move { api::email_2fa_enable(&rest).await },
                    move |r| s(Msg::EmailEnabled(r)),
                ))
            }
            Msg::EmailEnabled(r) => {
                self.busy = None;
                match r {
                    Ok(()) => {
                        self.notice = Some(t!("account.email2fa_enabled_notice").into_owned());
                        set_factor(ctx.workspace, None, Some(true));
                    }
                    Err(e) => self.error = Some(e.user_text()),
                }
                Action::none()
            }
            Msg::DisableEmail => {
                if self.busy.is_some() {
                    return Action::none();
                }
                self.busy = Some(Busy::Email);
                self.error = None;
                self.notice = None;
                Action::task(two_factor::run(ctx, Op::DisableEmail2fa, None))
            }
            Msg::LoadSessions => Action::task(self.load_sessions(ctx)),
            Msg::Sessions(r) => {
                self.sessions = match r {
                    Ok(list) => SessionsState::Loaded(list),
                    Err(e) if sessions_unavailable(e.error()) => SessionsState::Unavailable,
                    Err(e) => SessionsState::Failed(e.user_text()),
                };
                Action::none()
            }
            Msg::LogoutSession(id) => {
                if self.logging_out.is_none() {
                    self.confirm_logout = Some(id);
                }
                Action::none()
            }
            Msg::CancelLogoutSession => {
                self.confirm_logout = None;
                Action::none()
            }
            Msg::ConfirmLogoutSession => {
                let Some(id) = self.confirm_logout.take() else {
                    return Action::none();
                };
                self.logging_out = Some(id.clone());
                let call = id.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { sessions_logout(&rest, &call).await },
                    move |r| s(Msg::SessionLoggedOut(id.clone(), r)),
                ))
            }
            Msg::SessionLoggedOut(id, r) => {
                self.logging_out = None;
                match r {
                    Ok(()) => {
                        if let SessionsState::Loaded(list) = &mut self.sessions {
                            list.retain(|x| x.session_id != id);
                        }
                        self.notice = Some(t!("account.session_logged_out").into_owned());
                    }
                    Err(e) if sessions_unavailable(e.error()) => {
                        self.sessions = SessionsState::Unavailable;
                    }
                    Err(e) => self.error = Some(e.user_text()),
                }
                Action::none()
            }
        }
    }

    /// The guarded `users.enableTotp` finished (`None`: prompt cancelled).
    pub fn totp_started(
        &mut self,
        r: Option<Result<TotpSetup, String>>,
        second: Option<rocketchat::auth::TwoFactorCode>,
    ) -> Action<Message> {
        self.busy = None;
        match r {
            Some(Ok(setup)) => {
                let qr = qr_code::Data::new(setup.url.as_bytes()).ok();
                self.pending = Some(Pending {
                    setup,
                    qr,
                    code: String::new(),
                    error: None,
                    busy: false,
                    second,
                });
                Action::task(iced::widget::operation::focus(SETUP_INPUT_ID))
            }
            Some(Err(e)) => {
                self.error = Some(e);
                Action::none()
            }
            None => Action::none(),
        }
    }

    /// The guarded `users.validateTotp` finished (`None`: prompt cancelled).
    pub fn verified(
        &mut self,
        r: Option<Result<Option<Vec<String>>, String>>,
        ws: &mut Workspace,
    ) -> Action<Message> {
        if let Some(Ok(Some(codes))) = r {
            // Even when the setup view is gone: TOTP is on now.
            self.pending = None;
            self.remaining = Some(codes.len() as u64);
            self.codes = Some(codes);
            self.notice = Some(t!("account.totp_enabled_notice").into_owned());
            set_factor(ws, Some(true), None);
            return Action::none();
        }
        let Some(p) = self.pending.as_mut() else {
            return Action::none();
        };
        p.busy = false;
        match r {
            Some(Ok(None)) => {
                p.error = Some(t!("account.tfa_invalid").into_owned());
                p.code.clear();
            }
            Some(Err(e)) => p.error = Some(e),
            _ => {}
        }
        Action::none()
    }

    /// The guarded e-mail 2FA disable finished (or was cancelled: `ok = None`).
    pub fn email_disabled(&mut self, ok: Option<Result<(), String>>, ws: &mut Workspace) {
        self.busy = None;
        match ok {
            Some(Ok(())) => {
                self.notice = Some(t!("account.email2fa_disabled_notice").into_owned());
                set_factor(ws, None, Some(false));
            }
            Some(Err(e)) => self.error = Some(e),
            None => {}
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let s = |m| Message::Security(m);
        let ws = ctx.workspace;
        let settings = ws.settings();
        let me = ws.me();
        let mut body = column![].spacing(SPACE_4);
        // Error and notice share one slot: the code fields below keep their
        // place (and focus) when a banner appears.
        let mut notices = column![].spacing(SPACE_4);
        let mut any = false;
        if let Some(e) = &self.error {
            notices = notices.push(banner(BannerKind::Error, e.clone(), None));
            any = true;
        }
        if let Some(n) = &self.notice {
            notices = notices.push(banner(BannerKind::Success, n.clone(), None));
            any = true;
        }
        let notices = any.then(|| notices.into());
        if !settings.bool_or("Accounts_TwoFactorAuthentication_Enabled", true) {
            body = body.push(banner(
                BannerKind::Info,
                t!("account.tfa_server_disabled"),
                None,
            ));
            if let Some(e2e) = self.e2e.view(ctx) {
                body = body.push(e2e);
            }
            body = body.push(self.sessions_view());
            return above(notices, SPACE_4, body).max_width(640).into();
        }

        if settings.bool_or("Accounts_TwoFactorAuthentication_By_TOTP_Enabled", true) {
            let totp_on = me.is_some_and(|m| m.totp_enabled());
            let mut c = column![status_line(t!("account.totp_title").into_owned(), totp_on)]
                .spacing(SPACE_3);
            c = c.push(
                text(t!("account.totp_help"))
                    .size(TEXT_SMALL)
                    .style(style::text_secondary),
            );
            if totp_on {
                if let Some(n) = self.remaining {
                    c = c.push(
                        text(t!("account.totp_codes_remaining", n = n))
                            .size(TEXT_SMALL)
                            .style(style::text_secondary),
                    );
                }
                c = c.push(code_action(
                    t!("account.totp_disable_label").into_owned(),
                    &self.disable_code,
                    |v| Message::Security(Msg::DisableCode(v)),
                    danger_button(
                        t!("account.totp_disable"),
                        (self.busy.is_none() && !self.disable_code.trim().is_empty())
                            .then_some(s(Msg::DisableTotp)),
                    )
                    .loading(self.busy == Some(Busy::Disabling))
                    .into(),
                ));
                c = c.push(code_action(
                    t!("account.totp_regen_label").into_owned(),
                    &self.regen_code,
                    |v| Message::Security(Msg::RegenCode(v)),
                    secondary_button(
                        t!("account.totp_regenerate"),
                        (self.busy.is_none() && !self.regen_code.trim().is_empty())
                            .then_some(s(Msg::Regenerate)),
                    )
                    .loading(self.busy == Some(Busy::Regenerating))
                    .into(),
                ));
            } else if let Some(p) = &self.pending {
                c = c.push(setup_view(p));
            } else {
                c = c.push(
                    row![
                        primary_button(
                            t!("account.totp_enable"),
                            self.busy.is_none().then_some(s(Msg::EnableTotp))
                        )
                        .icon(Icon::Smartphone)
                        .loading(self.busy == Some(Busy::Starting))
                    ]
                    .spacing(SPACE_2),
                );
                c = c.push(
                    text(t!("account.totp_logout_hint"))
                        .size(TEXT_CAPTION)
                        .style(style::text_hint),
                );
            }
            body = body.push(section(t!("account.sec_totp").into_owned(), c.into()));
        }

        if settings.bool_or("Accounts_TwoFactorAuthentication_By_Email_Enabled", true) {
            let on = me.is_some_and(|m| m.email_2fa_enabled());
            let verified = me
                .and_then(|m| m.emails.as_ref())
                .and_then(|e| e.first())
                .is_some_and(|e| e.verified);
            let mut c = column![status_line(t!("account.email2fa_title").into_owned(), on)]
                .spacing(SPACE_3);
            c = c.push(
                text(t!("account.email2fa_help"))
                    .size(TEXT_SMALL)
                    .style(style::text_secondary),
            );
            let busy = self.busy == Some(Busy::Email);
            if on {
                c = c.push(
                    row![
                        danger_button(
                            t!("account.email2fa_disable"),
                            self.busy.is_none().then_some(s(Msg::DisableEmail))
                        )
                        .loading(busy)
                    ]
                    .spacing(SPACE_2),
                );
            } else {
                if !verified {
                    c = c.push(
                        text(t!("account.email2fa_needs_verified"))
                            .size(TEXT_SMALL)
                            .style(style::text_danger),
                    );
                }
                c = c.push(
                    row![
                        primary_button(
                            t!("account.email2fa_enable"),
                            (self.busy.is_none() && verified).then_some(s(Msg::EnableEmail))
                        )
                        .icon(Icon::Mail)
                        .loading(busy)
                    ]
                    .spacing(SPACE_2),
                );
                c = c.push(
                    text(t!("account.email2fa_logout_hint"))
                        .size(TEXT_CAPTION)
                        .style(style::text_hint),
                );
            }
            body = body.push(section(t!("account.sec_email2fa").into_owned(), c.into()));
        }
        // End-to-end encryption comes after two-factor authentication (web).
        if let Some(e2e) = self.e2e.view(ctx) {
            body = body.push(e2e);
        }
        body = body.push(self.sessions_view());
        above(notices, SPACE_4, body).max_width(640).into()
    }

    /// Active sessions / devices.
    fn sessions_view(&self) -> Element<'_, Message> {
        let s = |m| Message::Security(m);
        let mut c = column![
            text(t!("account.sessions_help"))
                .size(TEXT_SMALL)
                .style(style::text_secondary)
        ]
        .spacing(SPACE_3);
        match &self.sessions {
            SessionsState::Idle | SessionsState::Loading => {
                c = c.push(
                    text(t!("account.sessions_loading"))
                        .size(TEXT_SMALL)
                        .style(style::text_hint),
                );
            }
            SessionsState::Unavailable => {
                c = c.push(banner(
                    BannerKind::Info,
                    t!("account.sessions_unavailable"),
                    None,
                ));
            }
            SessionsState::Failed(e) => {
                c = c.push(banner(BannerKind::Error, e.clone(), None));
                c = c.push(
                    row![secondary_button(
                        t!("account.sessions_retry"),
                        Some(s(Msg::LoadSessions))
                    )]
                    .spacing(SPACE_2),
                );
            }
            SessionsState::Loaded(list) if list.is_empty() => {
                c = c.push(
                    text(t!("account.sessions_empty"))
                        .size(TEXT_SMALL)
                        .style(style::text_hint),
                );
            }
            SessionsState::Loaded(list) => {
                for x in list {
                    c = c.push(self.session_row(x));
                }
            }
        }
        section(t!("account.sec_sessions").into_owned(), c.into())
    }

    fn session_row<'a>(&'a self, x: &'a DeviceSession) -> Element<'a, Message> {
        let s = |m| Message::Security(m);
        let mut details: Vec<String> = Vec::new();
        if let Some(at) = x.login_at {
            let at =
                at.0.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M")
                    .to_string();
            details.push(t!("account.session_login_at", at = at).into_owned());
        }
        if let Some(ip) = x.ip.as_deref().filter(|s| !s.is_empty()) {
            details.push(ip.to_owned());
        }
        if let Some(h) = x.host.as_deref().filter(|s| !s.is_empty()) {
            details.push(h.to_owned());
        }
        let mut title = row![
            icon::sized(Icon::Smartphone, 14.0),
            text(x.title()).size(TEXT_BODY).font(font_semibold())
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
        if x.current {
            title = title.push(chip(
                t!("account.session_current").into_owned(),
                ChipKind::Success,
            ));
        }
        let info = column![
            title,
            text(details.join(" · "))
                .size(TEXT_CAPTION)
                .style(style::text_secondary)
        ]
        .spacing(2)
        .width(Length::Fill);
        let action: Element<'a, Message> =
            if self.confirm_logout.as_deref() == Some(x.session_id.as_str()) {
                row![
                    ghost_button(t!("account.cancel"), Some(s(Msg::CancelLogoutSession))),
                    danger_button(
                        t!("account.session_logout_confirm"),
                        Some(s(Msg::ConfirmLogoutSession))
                    ),
                ]
                .spacing(SPACE_1)
                .into()
            } else if x.current {
                // This client: "Log out" in the status menu.
                row![].into()
            } else {
                secondary_button(
                    t!("account.session_logout"),
                    self.logging_out
                        .is_none()
                        .then(|| s(Msg::LogoutSession(x.session_id.clone()))),
                )
                .loading(self.logging_out.as_deref() == Some(x.session_id.as_str()))
                .into()
            };
        row![info, action]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into()
    }

    /// The backup codes dialog (shown once).
    pub fn codes_dialog(&self) -> Option<Element<'_, Message>> {
        let codes = self.codes.as_ref()?;
        let s = |m| Message::Security(m);
        let grid = column(codes.chunks(2).map(|pair| {
            row(pair.iter().map(|c| {
                text(c.as_str())
                    .font(font_mono())
                    .size(TEXT_BODY + 1.0)
                    .width(Length::Fill)
                    .into()
            }))
            .spacing(SPACE_4)
            .into()
        }))
        .spacing(SPACE_1);
        let body = column![
            text(t!("account.backup_codes_help"))
                .size(TEXT_BODY)
                .style(style::text_secondary),
            container(grid)
                .padding(SPACE_3)
                .width(Length::Fill)
                .style(style::container_code_block),
        ]
        .spacing(SPACE_3);
        Some(dialog(
            t!("account.backup_codes_title"),
            body,
            vec![
                ghost_button(
                    t!("account.copy_all"),
                    Some(Message::Copy(codes.join("\n"))),
                )
                .icon(Icon::Copy)
                .into(),
                primary_button(t!("account.backup_codes_done"), Some(s(Msg::CloseCodes))).into(),
            ],
            // Shown once: only "Done" closes it (no backdrop/Escape/close button).
            None,
        ))
    }
}

fn clean(code: &str) -> String {
    code.chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect()
}

fn status_line<'a>(title: String, on: bool) -> Element<'a, Message> {
    row![
        text(title)
            .size(TEXT_BODY)
            .font(font_semibold())
            .width(Length::Fill),
        chip(
            if on {
                t!("account.state_on").into_owned()
            } else {
                t!("account.state_off").into_owned()
            },
            if on {
                ChipKind::Success
            } else {
                ChipKind::Neutral
            }
        ),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center)
    .into()
}

fn code_action<'a>(
    label: String,
    value: &'a str,
    on: fn(String) -> Message,
    button: Element<'a, Message>,
) -> Element<'a, Message> {
    row![
        text_field(label, value, on)
            .placeholder("123456")
            .width(Length::Fixed(220.0)),
        container(button).padding(iced::Padding {
            top: 22.0,
            ..Default::default()
        }),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Start)
    .into()
}

fn setup_view(p: &Pending) -> Element<'_, Message> {
    let s = |m| Message::Security(m);
    let qr: Element<'_, Message> = match &p.qr {
        // Black on white whatever the theme, so authenticator apps can read it.
        Some(data) => container(qr_code(data).cell_size(4).style(|_| qr_code::Style {
            cell: Color::BLACK,
            background: Color::WHITE,
        }))
        .padding(SPACE_2)
        .style(|_: &iced::Theme| container::Style {
            background: Some(Color::WHITE.into()),
            border: iced::border::rounded(6),
            ..Default::default()
        })
        .into(),
        None => text(t!("account.totp_qr_failed")).size(TEXT_SMALL).into(),
    };
    let secret = column![
        text(t!("account.totp_manual"))
            .size(TEXT_CAPTION)
            .style(style::text_secondary),
        row![
            text(group_secret(&p.setup.secret))
                .font(font_mono())
                .size(TEXT_SMALL)
                .width(Length::Fill),
            ghost_button(
                t!("account.copy"),
                Some(Message::Copy(p.setup.secret.clone()))
            )
            .icon(Icon::Copy),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center),
    ]
    .spacing(SPACE_1);
    let steps = column![
        row![
            icon::sized(Icon::ScanQrCode, 16.0),
            text(t!("account.totp_scan")).size(TEXT_SMALL)
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center),
        secret,
        text_field(t!("account.tfa_code"), &p.code, move |v| s(Msg::SetupCode(
            v
        )))
        .placeholder("123456")
        .id(SETUP_INPUT_ID)
        .error(p.error.clone())
        .on_submit(s(Msg::VerifySetup))
        .width(Length::Fixed(220.0)),
        row![
            secondary_button(
                t!("account.cancel"),
                (!p.busy).then_some(s(Msg::CancelSetup))
            ),
            primary_button(
                t!("account.tfa_verify"),
                (!p.busy && !p.code.trim().is_empty()).then_some(s(Msg::VerifySetup))
            )
            .loading(p.busy),
        ]
        .spacing(SPACE_2),
    ]
    .spacing(SPACE_3)
    .width(Length::Fill);
    row![qr, steps]
        .spacing(SPACE_4)
        .align_y(Alignment::Start)
        .into()
}

/// `ABCDEFGH…` → `ABCD EFGH …` for reading aloud/typing.
pub fn group_secret(s: &str) -> String {
    s.chars()
        .collect::<Vec<_>>()
        .chunks(4)
        .map(|c| c.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::User;

    #[test]
    fn factor_flags_are_patched() {
        let mut ws = Workspace::new();
        ws.apply_me(User {
            id: "u".into(),
            ..Default::default()
        });
        set_factor(&mut ws, Some(true), None);
        assert!(ws.me().unwrap().totp_enabled());
        assert!(!ws.me().unwrap().email_2fa_enabled());
        set_factor(&mut ws, Some(false), Some(true));
        assert!(!ws.me().unwrap().totp_enabled());
        assert!(ws.me().unwrap().email_2fa_enabled());
    }

    #[test]
    fn late_verification_still_shows_the_codes() {
        let mut ws = Workspace::new();
        ws.apply_me(User {
            id: "u".into(),
            ..Default::default()
        });
        let mut sec = Security::default();
        let _ = sec.totp_started(
            Some(Ok(TotpSetup {
                secret: "ABC".into(),
                url: "otpauth://totp/x?secret=ABC".into(),
            })),
            None,
        );
        sec.pending.as_mut().unwrap().busy = true;
        // Cancel is ignored while the confirmation is in flight.
        let p = sec.pending.as_ref().unwrap();
        assert!(p.busy);
        sec.pending = None; // e.g. the screen state was reset meanwhile
        let _ = sec.verified(Some(Ok(Some(vec!["a".into(), "b".into()]))), &mut ws);
        assert_eq!(sec.codes.as_ref().unwrap().len(), 2);
        assert!(ws.me().unwrap().totp_enabled());
        assert_eq!(sec.remaining, Some(2));
    }

    const CE_ANSWER: &[u8] = br#"{"success":false,"error":"This is an enterprise feature [error-action-not-allowed]","errorType":"error-action-not-allowed"}"#;

    #[test]
    fn sessions_decode_and_title() {
        let v = serde_json::json!({"sessions": [
            {"_id": "a", "sessionId": "a", "device": {"name": "Chrome", "version": "120", "type": "browser",
             "os": {"name": "Linux"}}, "ip": "1.2.3.4", "loginAt": "2026-01-02T03:04:05.000Z", "current": true},
            {"sessionId": "b"}
        ], "count": 2, "offset": 0, "total": 2});
        let list: Vec<DeviceSession> = serde_json::from_value(v["sessions"].clone()).unwrap();
        assert_eq!(list[0].title(), "Chrome 120 · Linux");
        assert!(list[0].current && list[0].login_at.is_some());
        assert_eq!(list[1].title(), "Unknown app");
        let rustket = |current| DeviceSession {
            session_id: "c".into(),
            device: Some(SessionDevice {
                kind: Some("other".into()),
                os: Some(SessionOs {
                    name: Some("Linux".into()),
                    version: None,
                }),
                ..Default::default()
            }),
            current,
            ..Default::default()
        };
        assert_eq!(
            rustket(true).title(),
            concat!("Rustket ", env!("CARGO_PKG_VERSION"), " · Linux")
        );
        assert_eq!(rustket(false).title(), "Unknown app · Linux");
    }

    #[test]
    fn community_servers_have_no_device_management() {
        // What 8.9 CE answers (captured from the dev server).
        let e = rocketchat::Error::from_response(400, None, CE_ANSWER);
        assert!(sessions_unavailable(&e));
        let e = rocketchat::Error::Status {
            status: 404,
            body: String::new(),
        };
        assert!(sessions_unavailable(&e));
        let e = rocketchat::Error::from_response(
            400,
            None,
            br#"{"success":false,"error":"Session not found"}"#,
        );
        assert!(!sessions_unavailable(&e));
    }

    #[test]
    fn session_logout_flow() {
        let mut sec = Security {
            sessions: SessionsState::Loaded(vec![
                DeviceSession {
                    session_id: "a".into(),
                    ..Default::default()
                },
                DeviceSession {
                    session_id: "b".into(),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        };
        let mut ws = Workspace::new();
        let sid = rustket_core::ServerId("s".into());
        let url = rocketchat::ServerUrl::parse("http://localhost:3000").unwrap();
        let images = crate::images::Images::new(None);
        let config = rustket_core::config::AppConfig::default();
        let mut ctx = Ctx {
            server_id: &sid,
            server: &url,
            workspace: &mut ws,
            session: None,
            images: &images,
            config: &config,
        };
        let _ = sec.update(Msg::LogoutSession("b".into()), &mut ctx);
        assert_eq!(sec.confirm_logout.as_deref(), Some("b"));
        let _ = sec.update(Msg::ConfirmLogoutSession, &mut ctx);
        assert_eq!(sec.logging_out.as_deref(), Some("b"));
        let _ = sec.update(Msg::SessionLoggedOut("b".into(), Ok(())), &mut ctx);
        assert!(sec.logging_out.is_none());
        match &sec.sessions {
            SessionsState::Loaded(l) => assert_eq!(l.len(), 1),
            other => panic!("{other:?}"),
        }
        let e = crate::error::Failure::from(rocketchat::Error::from_response(400, None, CE_ANSWER));
        let _ = sec.update(Msg::Sessions(Err(e)), &mut ctx);
        assert_eq!(sec.sessions, SessionsState::Unavailable);
    }

    #[test]
    fn secret_grouping_and_codes() {
        assert_eq!(group_secret("ABCDEFGHIJ"), "ABCD EFGH IJ");
        assert_eq!(clean(" 123 456 "), "123456");
        assert!(qr_code::Data::new(b"otpauth://totp/Rocket.Chat:x?secret=ABC").is_ok());
    }
}
