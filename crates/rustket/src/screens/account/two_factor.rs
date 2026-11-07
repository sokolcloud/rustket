//! Second-factor prompt for protected account calls (the login 2FA dialog
//! pattern): an operation fails with `totp-required`, the user enters a TOTP /
//! e-mail code or the password, and the same operation is repeated with the
//! `x-2fa-code` / `x-2fa-method` headers.

use iced::widget::{column, row, text};
use iced::{Alignment, Element, Task};
use rocketchat::auth::TwoFactorCode;
use rocketchat::models::User;
use rocketchat::{Error as RcError, TwoFactorChallenge, TwoFactorMethod};
use rust_i18n::t;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION};
use rustket_ui::widget::{dialog, ghost_button, primary_button, secondary_button, text_field};
use serde_json::Value;

use super::{Message, api};
use crate::component::Ctx;
use crate::error::{Failure, Res};

pub const INPUT_ID: &str = "account-2fa";

/// A call that may ask for a second factor.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    SaveProfile { data: Value },
    GenerateToken { name: String, bypass: bool },
    RegenerateToken(String),
    RemoveToken(String),
    DisableEmail2fa,
}

/// Result of a successful [`Op`].
#[derive(Debug, Clone)]
pub enum Done {
    Profile(Box<User>),
    Token { name: String, token: String },
    Removed(String),
    EmailDisabled,
}

/// Runs `op` (with `code` when the user already gave one).
pub fn run(ctx: &Ctx<'_>, op: Op, code: Option<TwoFactorCode>) -> Task<Message> {
    let tag = op.clone();
    ctx.rest_task(
        move |rest| async move {
            match op {
                Op::SaveProfile { data } => api::update_own_basic_info(&rest, data, code)
                    .await
                    .map(|u| Done::Profile(Box::new(u))),
                Op::GenerateToken { name, bypass } => {
                    api::generate_token(&rest, &name, bypass, code)
                        .await
                        .map(|token| Done::Token { name, token })
                }
                Op::RegenerateToken(name) => api::regenerate_token(&rest, &name, code)
                    .await
                    .map(|token| Done::Token { name, token }),
                Op::RemoveToken(name) => api::remove_token(&rest, &name, code)
                    .await
                    .map(|()| Done::Removed(name)),
                Op::DisableEmail2fa => api::email_2fa_disable(&rest, code)
                    .await
                    .map(|()| Done::EmailDisabled),
            }
        },
        move |r| Message::Guarded(tag, r),
    )
}

#[derive(Debug, Clone)]
pub enum Msg {
    Code(String),
    Submit,
    Cancel,
    Resend,
    Resent(Res<()>),
}

/// The open prompt.
#[derive(Debug, Clone)]
pub struct Prompt {
    pub op: Op,
    pub method: TwoFactorMethod,
    pub email_or_username: Option<String>,
    pub code: String,
    pub error: Option<String>,
    pub busy: bool,
    pub resent: bool,
}

impl Prompt {
    pub fn new(op: Op, ch: &TwoFactorChallenge) -> Self {
        let method = match ch.method {
            TwoFactorMethod::Unknown => TwoFactorMethod::Totp,
            ref m => m.clone(),
        };
        Self {
            op,
            method,
            email_or_username: ch.email_or_username.clone(),
            code: String::new(),
            error: None,
            busy: false,
            resent: false,
        }
    }

    pub fn code(&self) -> TwoFactorCode {
        match self.method {
            TwoFactorMethod::Password => TwoFactorCode::password(&self.code),
            ref m => TwoFactorCode::new(&self.code, m.clone()),
        }
    }
}

/// What the account screen does with a failed guarded call.
#[derive(Debug)]
pub enum Outcome {
    /// Ask for (another) code.
    Prompt(Prompt),
    /// The prompt stays open with an error.
    Invalid,
    /// A real failure (prompt closed).
    Failed(Failure),
}

/// Classifies a failure of `op` given the currently open prompt.
pub fn on_failure(op: &Op, prompt: Option<&Prompt>, f: Failure) -> Outcome {
    let open = prompt.filter(|p| &p.op == op);
    match f.error() {
        RcError::TwoFactorInvalid(_) if open.is_some() => Outcome::Invalid,
        RcError::TwoFactorRequired(ch) | RcError::TwoFactorInvalid(ch) => {
            let mut p = Prompt::new(op.clone(), ch);
            if matches!(f.error(), RcError::TwoFactorInvalid(_)) {
                p.error = Some(t!("account.tfa_invalid").into_owned());
            }
            Outcome::Prompt(p)
        }
        _ => Outcome::Failed(f),
    }
}

pub fn view(p: &Prompt) -> Element<'_, Message> {
    let tf = |m| Message::Tfa(m);
    let (help, label) = match p.method {
        TwoFactorMethod::Email => (
            t!(
                "account.tfa_email_help",
                who = p.email_or_username.clone().unwrap_or_default()
            ),
            t!("account.tfa_code"),
        ),
        TwoFactorMethod::Password => (
            t!("account.tfa_password_help"),
            t!("account.current_password"),
        ),
        _ => (t!("account.tfa_totp_help"), t!("account.tfa_code")),
    };
    let password = p.method == TwoFactorMethod::Password;
    let mut body =
        column![text(help).size(TEXT_BODY).style(style::text_secondary)].spacing(SPACE_3);
    body = body.push(
        text_field(label, &p.code, move |s| tf(Msg::Code(s)))
            .password(password)
            .placeholder(if password { "" } else { "123456" })
            .id(INPUT_ID)
            .error(p.error.clone())
            .on_submit(tf(Msg::Submit)),
    );
    if p.method == TwoFactorMethod::Email {
        body = body.push(
            row![
                ghost_button(t!("account.tfa_resend"), Some(tf(Msg::Resend))).icon(Icon::RefreshCw),
                text(if p.resent {
                    t!("account.tfa_resent").into_owned()
                } else {
                    String::new()
                })
                .size(TEXT_CAPTION)
                .style(style::text_success),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        );
    }
    dialog(
        t!("account.tfa_title"),
        body,
        vec![
            secondary_button(t!("account.cancel"), Some(tf(Msg::Cancel))).into(),
            primary_button(
                t!("account.tfa_verify"),
                (!p.code.trim().is_empty() && !p.busy).then_some(tf(Msg::Submit)),
            )
            .loading(p.busy)
            .into(),
        ],
        Some(tf(Msg::Cancel)),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn challenge(method: &str) -> TwoFactorChallenge {
        TwoFactorChallenge::from_details(Some(&serde_json::json!({
            "method": method, "emailOrUsername": "bob", "availableMethods": [method]
        })))
    }

    #[test]
    fn required_opens_a_prompt_and_invalid_keeps_it() {
        let op = Op::RemoveToken("t1".into());
        let f = Failure::from(RcError::TwoFactorRequired(challenge("email")));
        let Outcome::Prompt(p) = on_failure(&op, None, f) else {
            panic!("prompt expected");
        };
        assert_eq!(p.method, TwoFactorMethod::Email);
        assert_eq!(p.email_or_username.as_deref(), Some("bob"));
        assert!(p.error.is_none());

        let f = Failure::from(RcError::TwoFactorInvalid(challenge("email")));
        assert!(matches!(on_failure(&op, Some(&p), f), Outcome::Invalid));

        // An invalid inline password without a prompt opens one with an error.
        let f = Failure::from(RcError::TwoFactorInvalid(challenge("password")));
        let Outcome::Prompt(p2) = on_failure(&op, None, f) else {
            panic!("prompt expected");
        };
        assert!(p2.error.is_some());

        let f = Failure::from(RcError::Protocol("boom".into()));
        assert!(matches!(on_failure(&op, Some(&p), f), Outcome::Failed(_)));
    }

    #[test]
    fn codes_per_method() {
        let op = Op::DisableEmail2fa;
        let mut p = Prompt::new(op.clone(), &challenge("password"));
        p.code = "secret".into();
        let c = p.code();
        assert_eq!(c.method, TwoFactorMethod::Password);
        assert_eq!(c.code, rocketchat::auth::sha256_hex("secret"));
        let mut p = Prompt::new(op.clone(), &challenge("email"));
        p.code = "435-478".into();
        assert_eq!(p.code().code, "435478");
        let p = Prompt::new(op, &TwoFactorChallenge::from_details(None));
        assert_eq!(p.method, TwoFactorMethod::Totp);
    }
}
