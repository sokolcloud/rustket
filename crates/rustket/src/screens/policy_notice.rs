//! Plain screens of the server policy (docs/policy.md section 9.1): a
//! configured server the policy suspends, and no server at all while the
//! user may not add one. Strings: `locales/policy.yml` (`policy.*`).

use iced::widget::{column, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::policy::Policy;
use rustket_ui::style;
use rustket_ui::theme::{SPACE_3, TEXT_BODY};
use rustket_ui::widget::{danger_button, ghost_button, secondary_button};

use crate::policy::servers::Suspension;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Opens Settings → Policies.
    Details,
    /// Opens the policy's support link (read from the policy in force).
    Support,
    /// Asks to remove the suspended server (offered while the policy lets
    /// the user remove it; the handler checks again).
    Remove(ServerId),
}

/// Why a server is suspended, as shown on its screen.
pub fn reason_text(reason: Suspension) -> String {
    match reason {
        Suspension::NotAllowed => t!("policy.server_not_allowed").into_owned(),
        Suspension::HttpsRequired => t!("policy.server_https_required").into_owned(),
        Suspension::Blocked => t!("policy.server_blocked").into_owned(),
    }
}

/// The organization line, the support link and "Details", after `lead`.
fn body<'a>(policy: &Policy, lead: Option<Element<'a, Message>>) -> Element<'a, Message> {
    let mut col = column![].spacing(SPACE_3).align_x(Alignment::Center);
    if let Some(lead) = lead {
        col = col.push(lead);
    }
    col = col.push(
        text(crate::screens::settings::managed_text(policy))
            .size(TEXT_BODY)
            .style(style::text_secondary)
            .width(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center),
    );
    if policy.support_url().is_some() {
        col = col.push(
            ghost_button(t!("policy.contact_support"), Some(Message::Support)).width(Length::Fill),
        );
    }
    col.push(secondary_button(t!("policy.details"), Some(Message::Details)).width(Length::Fill))
        .into()
}

/// A configured server the policy suspends: its name, the mark, why, and
/// "Remove workspace" when `remove` is given (the user may remove it).
pub fn suspended<'a>(
    name: &str,
    reason: Suspension,
    policy: &Policy,
    remove: Option<ServerId>,
) -> Element<'a, Message> {
    let mark = text(t!("policy.suspended").into_owned())
        .size(TEXT_BODY)
        .width(Length::Fill)
        .align_x(iced::alignment::Horizontal::Center);
    let mut col = column![body(policy, Some(mark.into()))]
        .spacing(SPACE_3)
        .align_x(Alignment::Center);
    if let Some(sid) = remove {
        col = col.push(
            danger_button(t!("status_menu.remove_server"), Some(Message::Remove(sid)))
                .width(Length::Fill),
        );
    }
    super::card(name.to_owned(), Some(reason_text(reason)), col.into())
}

/// No server is configured and the organization manages the list.
pub fn no_servers<'a>(policy: &Policy) -> Element<'a, Message> {
    super::card(
        t!("policy.no_servers_title").into_owned(),
        Some(t!("policy.no_servers").into_owned()),
        body(policy, None),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use rustket_core::policy::{RawValue, Snapshot, SourceId, Trust, resolve};

    use super::*;

    fn org() -> Policy {
        let snap = Snapshot::new(SourceId::Test("t".into()), Trust::Full)
            .with("OrganizationName", RawValue::String("Acme".into()))
            .with(
                "SupportUrl",
                RawValue::String("https://help.acme.example".into()),
            );
        resolve(&[snap], &crate::policy::context())
    }

    #[test]
    fn suspended_screen_names_server_reason_and_organization() {
        rust_i18n::set_locale("en");
        let texts =
            crate::tree_probe::texts(suspended("Corp", Suspension::NotAllowed, &org(), None));
        for needle in [
            "Corp",
            "Suspended by your organization's policy",
            "Your organization's policy does not allow this workspace",
            "Managed by Acme",
            "Contact support",
            "Details",
        ] {
            assert!(texts.iter().any(|t| t == needle), "{needle}: {texts:?}");
        }
        let texts = crate::tree_probe::texts(suspended(
            "Corp",
            Suspension::HttpsRequired,
            &Policy::default(),
            None,
        ));
        assert!(
            texts
                .iter()
                .any(|t| t == "Your organization's policy requires https:// for this workspace")
        );
        assert!(texts.iter().any(|t| t == "Managed by your organization"));
        assert!(!texts.iter().any(|t| t == "Contact support"), "no link set");
        assert!(
            !texts.iter().any(|t| t == "Remove workspace"),
            "not removable"
        );
        let texts = crate::tree_probe::texts(suspended(
            "Corp",
            Suspension::NotAllowed,
            &Policy::default(),
            Some(ServerId("s1".into())),
        ));
        assert!(texts.iter().any(|t| t == "Remove workspace"), "{texts:?}");
    }

    #[test]
    fn no_servers_screen() {
        rust_i18n::set_locale("en");
        let texts = crate::tree_probe::texts(no_servers(&org()));
        for needle in [
            "No workspaces",
            "Your organization manages the list of workspaces, and none is configured",
            "Managed by Acme",
            "Contact support",
            "Details",
        ] {
            assert!(texts.iter().any(|t| t == needle), "{needle}: {texts:?}");
        }
    }
}
