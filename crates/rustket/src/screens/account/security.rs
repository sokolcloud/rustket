//! Security tab: TOTP two-factor authentication (`2fa:enable`,
//! `2fa:validateTempToken`, `2fa:disable`, `2fa:regenerateCodes`) and e-mail
//! 2FA (`users.2fa.enableEmail` / `disableEmail`).

use iced::widget::{column, container, qr_code, row, text};
use iced::{Alignment, Color, Element, Length};
use rocketchat::models::Enabled;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, MONOSPACE_FONT, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION,
    TEXT_SMALL,
};
use rustket_ui::widget::{
    BannerKind, ChipKind, banner, chip, danger_button, dialog, ghost_button, primary_button,
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
    TotpStarted(Res<TotpSetup>),
    SetupCode(String),
    VerifySetup,
    Verified(Res<Option<Vec<String>>>),
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
}

/// TOTP being set up: the secret, its QR code and the confirmation code.
pub struct Pending {
    pub setup: TotpSetup,
    pub qr: Option<qr_code::Data>,
    pub code: String,
    pub error: Option<String>,
    pub busy: bool,
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
    pub fn refresh(&mut self, ctx: &Ctx<'_>) -> Action<Message> {
        if !ctx.workspace.me().is_some_and(|m| m.totp_enabled()) {
            return Action::none();
        }
        Action::task(ctx.rest_task(
            |rest| async move { api::totp_codes_remaining(&rest).await },
            |r| Message::Security(Msg::Remaining(r)),
        ))
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut Ctx<'_>) -> Action<Message> {
        let s = |m| Message::Security(m);
        match msg {
            Msg::EnableTotp => {
                if self.busy.is_some() {
                    return Action::none();
                }
                self.busy = Some(Busy::Starting);
                self.error = None;
                self.notice = None;
                Action::task(ctx.rest_task(
                    |rest| async move { api::totp_enable(&rest).await },
                    move |r| s(Msg::TotpStarted(r)),
                ))
            }
            Msg::TotpStarted(r) => {
                self.busy = None;
                match r {
                    Ok(setup) => {
                        let qr = qr_code::Data::new(setup.url.as_bytes()).ok();
                        self.pending = Some(Pending {
                            setup,
                            qr,
                            code: String::new(),
                            error: None,
                            busy: false,
                        });
                        Action::task(iced::widget::operation::focus(SETUP_INPUT_ID))
                    }
                    Err(e) => {
                        self.error = Some(e.user_text());
                        Action::none()
                    }
                }
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
                Action::task(ctx.rest_task(
                    move |rest| async move { api::totp_validate(&rest, &code).await },
                    move |r| s(Msg::Verified(r)),
                ))
            }
            Msg::Verified(r) => {
                let Some(p) = self.pending.as_mut() else {
                    return Action::none();
                };
                p.busy = false;
                match r {
                    Ok(Some(codes)) => {
                        self.pending = None;
                        self.remaining = Some(codes.len() as u64);
                        self.codes = Some(codes);
                        self.notice = Some(t!("account.totp_enabled_notice").into_owned());
                        set_factor(ctx.workspace, Some(true), None);
                    }
                    Ok(None) => {
                        p.error = Some(t!("account.tfa_invalid").into_owned());
                        p.code.clear();
                    }
                    Err(e) => p.error = Some(e.user_text()),
                }
                Action::none()
            }
            Msg::CancelSetup => {
                self.pending = None;
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
        }
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
        let mut body = column![].spacing(SPACE_4).max_width(640);
        if let Some(e) = &self.error {
            body = body.push(banner(BannerKind::Error, e.clone(), None));
        }
        if let Some(n) = &self.notice {
            body = body.push(banner(BannerKind::Success, n.clone(), None));
        }
        if !settings.bool_or("Accounts_TwoFactorAuthentication_Enabled", true) {
            body = body.push(banner(
                BannerKind::Info,
                t!("account.tfa_server_disabled"),
                None,
            ));
            return body.into();
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
        body.into()
    }

    /// The backup codes dialog (shown once).
    pub fn codes_dialog(&self) -> Option<Element<'_, Message>> {
        let codes = self.codes.as_ref()?;
        let s = |m| Message::Security(m);
        let grid = column(codes.chunks(2).map(|pair| {
            row(pair.iter().map(|c| {
                text(c.as_str())
                    .font(MONOSPACE_FONT)
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
            Some(s(Msg::CloseCodes)),
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
            .font(FONT_SEMIBOLD)
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
                .font(MONOSPACE_FONT)
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
            secondary_button(t!("account.cancel"), Some(s(Msg::CancelSetup))),
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
    fn secret_grouping_and_codes() {
        assert_eq!(group_secret("ABCDEFGHIJ"), "ABCD EFGH IJ");
        assert_eq!(clean(" 123 456 "), "123456");
        assert!(qr_code::Data::new(b"otpauth://totp/Rocket.Chat:x?secret=ABC").is_ok());
    }
}
