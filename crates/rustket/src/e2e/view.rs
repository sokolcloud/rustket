//! Account banner and modals (§10), and the message placeholders used by
//! the shared renderer.

use iced::widget::{column, container, row, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::Message as RcMessage;
use rust_i18n::t;
use rustket_core::e2e::AccountE2eState;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL, font_mono, font_semibold,
};
use rustket_ui::widget::{
    BannerKind, banner as info_banner, danger_button, dialog, ghost_button, primary_button,
    secondary_button, status_strip, text_field,
};

use super::shown::{self, State};
use super::{Message, Modal, outgoing};
use crate::workspace::ServerState;

pub const PASSWORD_INPUT_ID: &str = "e2e-password";

/// What the account banner says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BannerState {
    EnterPassword,
    SavePassword,
    Error,
    /// `E2E_Enable` was turned off while keys are loaded (§11.12).
    DisabledByAdmin,
}

pub fn banner_state(s: &ServerState) -> Option<BannerState> {
    let mgr = s.e2e.manager()?;
    // Settings not loaded yet: `enabled` is a guess, say nothing.
    if !outgoing::settings_loaded(&s.workspace) {
        return None;
    }
    let enabled = outgoing::settings(&s.workspace).enabled;
    match mgr.account_state() {
        AccountE2eState::EnterPassword { .. } if enabled && !s.e2e.banner_hidden => {
            Some(BannerState::EnterPassword)
        }
        AccountE2eState::SavePassword if enabled => Some(BannerState::SavePassword),
        AccountE2eState::Error(_) if enabled => Some(BannerState::Error),
        state if !enabled && state.has_keys() => Some(BannerState::DisabledByAdmin),
        _ => None,
    }
}

/// The account banner above the room area.
pub fn banner(s: &ServerState) -> Option<Element<'_, Message>> {
    let state = banner_state(s)?;
    Some(match state {
        BannerState::EnterPassword => status_strip(
            BannerKind::Warning,
            t!("e2e.banner_enter_password"),
            Some(icon::sized(Icon::KeyRound, 14.0).into()),
            Some((
                t!("e2e.banner_enter_password_action").into_owned(),
                Message::OpenModal,
            )),
        ),
        BannerState::SavePassword => status_strip(
            BannerKind::Info,
            t!("e2e.banner_save_password"),
            Some(icon::sized(Icon::KeyRound, 14.0).into()),
            Some((
                t!("e2e.banner_save_password_action").into_owned(),
                Message::OpenModal,
            )),
        ),
        BannerState::Error => status_strip(
            BannerKind::Error,
            t!("e2e.banner_error"),
            None,
            Some((t!("e2e.retry").into_owned(), Message::Retry)),
        ),
        BannerState::DisabledByAdmin => status_strip(
            BannerKind::Info,
            t!("e2e.banner_disabled_by_admin"),
            Some(icon::sized(Icon::LockOpen, 14.0).into()),
            None,
        ),
    })
}

/// The open account modal.
pub fn modal(s: &ServerState) -> Option<Element<'_, Message>> {
    let insecure = insecure_server(s);
    match s.e2e.modal.as_ref()? {
        Modal::EnterPassword { input, busy, error } => {
            Some(enter_password(input, *busy, error, insecure))
        }
        Modal::SavePassword { passphrase, busy } => {
            Some(save_password(passphrase, *busy, insecure))
        }
        Modal::SignOutWarning { passphrase, action } => Some(sign_out_warning(passphrase, *action)),
    }
}

fn passphrase_box(passphrase: &str) -> Element<'_, Message> {
    container(
        row![
            text(passphrase.to_owned())
                .font(font_mono())
                .size(TEXT_BODY)
                .width(Length::Fill),
            ghost_button(t!("e2e.copy"), Some(Message::CopyPassphrase)).icon(Icon::Copy),
        ]
        .spacing(SPACE_2)
        .align_y(Alignment::Center),
    )
    .padding(SPACE_3)
    .width(Length::Fill)
    .style(style::container_code_block)
    .into()
}

/// Signing out would delete the generated passphrase the user never saved.
fn sign_out_warning(passphrase: &str, action: super::SignOut) -> Element<'_, Message> {
    let (confirm, body) = match action {
        super::SignOut::Logout { .. } => {
            (t!("e2e.sign_out_anyway"), t!("e2e.sign_out_warning_body"))
        }
        super::SignOut::RemoveServer => (t!("e2e.remove_anyway"), t!("e2e.remove_warning_body")),
    };
    let body = column![
        text(body).size(TEXT_BODY),
        passphrase_box(passphrase),
        text(t!("e2e.sign_out_warning_hint"))
            .size(TEXT_CAPTION)
            .style(style::text_secondary),
    ]
    .spacing(SPACE_3);
    dialog(
        t!("e2e.sign_out_warning_title"),
        body,
        vec![
            secondary_button(t!("e2e.cancel"), Some(Message::CloseModal)).into(),
            danger_button(confirm, Some(Message::ConfirmSignOut)).into(),
        ],
        Some(Message::CloseModal),
    )
}

fn enter_password<'a>(
    input: &'a str,
    busy: bool,
    error: &'a Option<String>,
    insecure: bool,
) -> Element<'a, Message> {
    let mut field = text_field(t!("e2e.password_label"), input, Message::PasswordInput)
        .password(true)
        .id(PASSWORD_INPUT_ID)
        .disabled(busy);
    if !busy {
        field = field.on_submit(Message::SubmitPassword);
    }
    if let Some(e) = error {
        field = field.error(Some(e.clone()));
    }
    let mut body = column![
        text(t!("e2e.enter_password_body")).size(TEXT_BODY),
        field,
        row![
            ghost_button(t!("e2e.forgot_password"), Some(Message::ForgotPassword))
                .icon(Icon::CircleQuestionMark),
        ],
    ]
    .spacing(SPACE_3);
    if insecure {
        body = body.push(insecure_note());
    }
    dialog(
        t!("e2e.enter_password_title"),
        body,
        vec![
            secondary_button(t!("e2e.later"), (!busy).then_some(Message::Later)).into(),
            primary_button(
                t!("e2e.enable_encryption"),
                (!input.is_empty() && !busy).then_some(Message::SubmitPassword),
            )
            .loading(busy)
            .into(),
        ],
        (!busy).then_some(Message::CloseModal),
    )
}

fn save_password(passphrase: &str, busy: bool, insecure: bool) -> Element<'_, Message> {
    let code = passphrase_box(passphrase);
    let mut body = column![
        text(t!("e2e.save_password_body")).size(TEXT_BODY),
        text(t!("e2e.save_password_once"))
            .size(TEXT_BODY)
            .font(font_semibold()),
        code,
        text(t!("e2e.save_password_later_hint"))
            .size(TEXT_CAPTION)
            .style(style::text_secondary),
    ]
    .spacing(SPACE_3);
    if insecure {
        body = body.push(insecure_note());
    }
    dialog(
        t!("e2e.save_password_title"),
        body,
        vec![
            secondary_button(t!("e2e.later"), (!busy).then_some(Message::Later)).into(),
            primary_button(t!("e2e.i_saved"), (!busy).then_some(Message::ConfirmSaved))
                .loading(busy)
                .into(),
        ],
        (!busy).then_some(Message::CloseModal),
    )
}

/// The text of a message that is not shown decrypted.
pub fn placeholder_text(state: State) -> Option<String> {
    let key = match state {
        State::Decrypted { .. } => return None,
        State::Locked => "e2e.msg_locked",
        State::Waiting => "e2e.msg_waiting",
        State::NoKey => "e2e.msg_no_key",
        State::KeyError => "e2e.msg_key_error",
        State::Unsupported => "e2e.msg_unsupported",
        State::Raw => "e2e.encrypted_message",
    };
    Some(t!(key).into_owned())
}

/// The body of an encrypted message that cannot be shown (pending or
/// failed): a lock and an explanation. `None` for decrypted and plain
/// messages.
pub fn placeholder<M: 'static>(m: &RcMessage) -> Option<Element<'static, M>> {
    let state = shown::state(m)?;
    let label = placeholder_text(state)?;
    let ic = match state {
        State::Waiting | State::Locked | State::Raw => Icon::Lock,
        _ => Icon::TriangleAlert,
    };
    Some(
        row![
            icon::sized(ic, 13.0).style(style::text_hint),
            text(label).size(TEXT_SMALL).style(style::text_hint),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center)
        .into(),
    )
}

/// A note under a decrypted message whose server copy also carries
/// plaintext (§11.14).
pub fn plaintext_note<M: 'static>(m: &RcMessage) -> Option<Element<'static, M>> {
    if shown::state(m) != Some(State::Decrypted { plaintext: true }) {
        return None;
    }
    Some(
        row![
            icon::sized(Icon::TriangleAlert, 12.0).style(style::text_danger),
            text(t!("e2e.msg_plaintext_edit"))
                .size(TEXT_CAPTION)
                .style(style::text_danger),
        ]
        .spacing(SPACE_1)
        .align_y(Alignment::Center)
        .into(),
    )
}

/// A plain `http://` server that is not local: keys and the encrypted key
/// backup travel over an unprotected connection (§7.1).
fn insecure_server(s: &ServerState) -> bool {
    insecure_url(s.url.url())
}

fn insecure_url(url: &url::Url) -> bool {
    let local = match url.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    };
    url.scheme() == "http" && !local
}

fn insecure_note<'a>() -> Element<'a, Message> {
    info_banner(BannerKind::Warning, t!("e2e.insecure_server"), None)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn insecure_urls() {
        let u = |s: &str| url::Url::parse(s).unwrap();
        assert!(!insecure_url(&u("http://localhost:3000")));
        assert!(!insecure_url(&u("http://127.0.0.2/")));
        assert!(!insecure_url(&u("http://[::1]:3000")));
        assert!(!insecure_url(&u("https://chat.example.org")));
        assert!(insecure_url(&u("http://localhost.example.org")));
        assert!(insecure_url(&u("http://127.0.0.1.attacker.net")));
        assert!(insecure_url(&u("http://chat.example.org")));
    }
}
