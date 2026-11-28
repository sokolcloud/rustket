//! The blocking screen of the enterprise policy (docs/policy.md sections 6
//! and 9.7): while the policy cannot be read or requires a newer version it
//! replaces the whole window, after the terms screen. It needs nothing but
//! the policy: no server, no secret store, no writable config. Strings:
//! `locales/policy.yml` (`policy.*`).

use iced::widget::text::Wrapping;
use iced::widget::{Column, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::policy::{Blocking, Policy, UpdateMode};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, SPACE_6, TEXT_BODY, TEXT_CAPTION, TEXT_TITLE, font_semibold,
};
use rustket_ui::widget::{ghost_button, primary_button, secondary_button};

use crate::policy::page;
use crate::screens::settings::{dump_command, source_label, sources_card};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    /// Shows or hides the policy sources.
    ToggleDetails,
    /// Puts the report (the `--dump-policy` JSON) on the clipboard.
    CopyReport,
    /// Opens the policy's support link.
    Support,
    /// Runs the updater's manual check (offered only when it may install).
    CheckUpdates,
    Quit,
}

/// Whether the screen offers "Check for updates": exactly when the
/// updater's effective mode (docs/policy.md 9.3) is `prompt`. `mode`: that
/// mode (`App::update_mode`), `None` in a build without the updater.
pub fn offers_update(mode: Option<UpdateMode>) -> bool {
    mode == Some(UpdateMode::Prompt)
}

fn wrapped<'a>(s: String) -> iced::widget::Text<'a> {
    text(s).size(TEXT_BODY).wrapping(Wrapping::WordOrGlyph)
}

fn secondary<'a>(s: String) -> iced::widget::Text<'a> {
    text(s)
        .size(TEXT_CAPTION)
        .style(style::text_secondary)
        .wrapping(Wrapping::WordOrGlyph)
}

/// One block of a blocking state.
fn state_block<'a>(b: &Blocking, update: bool) -> Element<'a, Message> {
    let col = match b {
        Blocking::SourceUnreadable { source, error } => {
            let kind = page::error_key(error.kind);
            let mut c = column![
                wrapped(t!("policy.block_unreadable").into_owned()).font(font_semibold()),
                wrapped(format!("{} · {}", source_label(source), t!(&kind))),
            ]
            .spacing(SPACE_1);
            if let Some(detail) = &error.detail {
                c = c.push(secondary(detail.clone()));
            }
            c.push(wrapped(t!("policy.contact_admin").into_owned()))
        }
        Blocking::BelowMinimumVersion { required, client } => {
            let c = column![
                wrapped(t!("policy.update_required").into_owned()).font(font_semibold()),
                wrapped(
                    t!(
                        "policy.update_required_body",
                        required = required.to_string(),
                        client = client.clone()
                    )
                    .into_owned()
                ),
            ]
            .spacing(SPACE_1);
            if update {
                c.push(
                    primary_button(t!("policy.check_updates"), Some(Message::CheckUpdates))
                        .icon(Icon::RefreshCw),
                )
            } else {
                c.push(wrapped(t!("policy.update_or_admin").into_owned()))
            }
        }
    };
    container(col.spacing(SPACE_2))
        .padding(SPACE_4)
        .width(Length::Fill)
        .style(style::container_banner_error)
        .into()
}

/// The screen: the heading, one block per blocking state, the organization
/// line and support link when set, "Details" (the sources of the Policies
/// page while `details`), "Copy report", the `--dump-policy` hint and
/// "Quit". `update`: [`offers_update`].
pub fn view<'a>(policy: &'a Policy, details: bool, update: bool) -> Element<'a, Message> {
    let mut body = Column::new().spacing(SPACE_4).max_width(560);
    body = body.push(
        row![
            icon::sized(Icon::CircleAlert, 22.0),
            text(t!("policy.block_heading").into_owned())
                .size(TEXT_TITLE)
                .font(font_semibold())
                .wrapping(Wrapping::WordOrGlyph),
        ]
        .spacing(SPACE_3)
        .align_y(Alignment::Center),
    );
    for b in policy.blocking() {
        body = body.push(state_block(b, update));
    }
    if let Some(name) = policy.organization_name() {
        body = body.push(secondary(t!("policy.managed_by", name = name).into_owned()));
    }
    if policy.support_url().is_some() {
        body = body.push(ghost_button(
            t!("policy.contact_support"),
            Some(Message::Support),
        ));
    }
    body = body.push(
        row![
            secondary_button(t!("policy.details"), Some(Message::ToggleDetails)).icon(if details {
                Icon::ChevronUp
            } else {
                Icon::ChevronDown
            }),
            secondary_button(t!("policy.copy_report"), Some(Message::CopyReport)),
        ]
        .spacing(SPACE_2),
    );
    if details && !policy.sources().is_empty() {
        body = body.push(sources_card(policy.sources()));
    }
    body = body.push(secondary(
        t!("policy.dump_hint", command = dump_command()).into_owned(),
    ));
    body = body.push(row![
        iced::widget::space().width(Length::Fill),
        secondary_button(t!("desktop.tray_quit"), Some(Message::Quit)),
    ]);
    container(scrollable(
        container(
            container(body)
                .padding(SPACE_6)
                .style(style::container_card),
        )
        .padding(SPACE_6)
        .center_x(Length::Fill),
    ))
    .center(Length::Fill)
    .into()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use rustket_core::policy::{RawValue, Snapshot, SourceError, SourceErrorKind, SourceId, Trust};

    use super::*;

    fn resolve(snaps: &[Snapshot]) -> Policy {
        rustket_core::policy::resolve(snaps, &crate::policy::context())
    }

    /// The effective update mode (docs/policy.md 9.3) of a copy whose
    /// detected kind updates itself by default (an AppImage, say): the key
    /// when set, else `prompt`; `None` in a build without the updater.
    fn mode_of(p: &Policy) -> Option<UpdateMode> {
        #[cfg(feature = "self-update")]
        return Some(crate::updater::mode::effective(p.update_mode(), None));
        #[cfg(not(feature = "self-update"))]
        {
            let _ = p;
            None
        }
    }

    fn view(p: &Policy, details: bool) -> Element<'_, Message> {
        super::view(p, details, offers_update(mode_of(p)))
    }

    fn unreadable() -> Snapshot {
        Snapshot::unreadable(
            SourceId::LinuxFile("corp.json".into()),
            SourceError::new(SourceErrorKind::InvalidJson).with_detail("EOF at line 3"),
        )
    }

    fn admin(values: &[(&str, RawValue)]) -> Snapshot {
        let mut snap = Snapshot::new(SourceId::Test("admin".into()), Trust::Full);
        for (k, v) in values {
            snap = snap.with(k, v.clone());
        }
        snap
    }

    fn org() -> [(&'static str, RawValue); 2] {
        [
            ("OrganizationName", RawValue::String("Acme".into())),
            (
                "SupportUrl",
                RawValue::String("https://help.acme.example".into()),
            ),
        ]
    }

    fn has(texts: &[String], needle: &str) -> bool {
        texts.iter().any(|t| t == needle)
    }

    #[test]
    fn unreadable_source_with_and_without_organization() {
        rust_i18n::set_locale("en");
        let p = resolve(&[unreadable()]);
        assert!(p.is_blocked());
        let texts = crate::tree_probe::texts(view(&p, false));
        for needle in [
            "Rustket cannot read your organization's policy",
            "File corp.json · not valid JSON",
            "EOF at line 3",
            "Contact your administrator",
            "Details",
            "Copy report",
            "Quit",
        ] {
            assert!(has(&texts, needle), "{needle}: {texts:?}");
        }
        assert!(
            !texts.iter().any(|t| t.starts_with("Managed by")),
            "{texts:?}"
        );
        assert!(!has(&texts, "Contact support"));
        assert!(texts.iter().any(|t| t.contains("--dump-policy")));

        let p = resolve(&[admin(&org()), unreadable()]);
        let texts = crate::tree_probe::texts(view(&p, false));
        assert!(has(&texts, "Managed by Acme"), "{texts:?}");
        assert!(has(&texts, "Contact support"));
    }

    #[test]
    fn update_required_and_the_update_action() {
        rust_i18n::set_locale("en");
        let min = ("MinimumVersion", RawValue::String("999.0.0".into()));
        let client = crate::policy::context().client_version;
        let body = format!(
            "Your organization requires Rustket 999.0.0 or newer. This is version {client}."
        );
        for (mode, offered) in [
            (None, true),
            (Some("prompt"), true),
            (Some("notify"), false),
            (Some("off"), false),
        ] {
            let mut values = vec![min.clone()];
            if let Some(m) = mode {
                values.push(("UpdateMode", RawValue::String(m.into())));
            }
            let p = resolve(&[admin(&values)]);
            assert!(p.is_blocked(), "{mode:?}");
            let offered = offered && cfg!(feature = "self-update");
            assert_eq!(offers_update(mode_of(&p)), offered, "{mode:?}");
            let texts = crate::tree_probe::texts(view(&p, false));
            assert!(has(&texts, "An update is required"), "{texts:?}");
            assert!(has(&texts, &body), "{texts:?}");
            assert_eq!(has(&texts, "Check for updates"), offered, "{mode:?}");
            assert_eq!(
                has(&texts, "Install the new version or ask your administrator."),
                !offered,
                "{mode:?}"
            );
            assert!(!has(&texts, "Contact your administrator"));
            assert!(
                !texts.iter().any(|t| t.starts_with("Managed by")),
                "{texts:?}"
            );
            assert!(!has(&texts, "Contact support"), "{mode:?}");
        }
        let mut values = org().to_vec();
        values.push(min);
        let texts = crate::tree_probe::texts(view(&resolve(&[admin(&values)]), false));
        assert!(has(&texts, "Managed by Acme") && has(&texts, "Contact support"));
    }

    /// The update action exactly when the effective mode is `prompt`, and
    /// never in a build without the updater (`None`).
    #[test]
    fn update_action_per_effective_mode() {
        rust_i18n::set_locale("en");
        assert!(offers_update(Some(UpdateMode::Prompt)));
        assert!(!offers_update(Some(UpdateMode::Notify)));
        assert!(!offers_update(Some(UpdateMode::Off)));
        assert!(!offers_update(None));
        let p = resolve(&[admin(&[(
            "MinimumVersion",
            RawValue::String("999.0.0".into()),
        )])]);
        for (update, shown) in [(true, true), (false, false)] {
            let texts = crate::tree_probe::texts(super::view(&p, false, update));
            assert_eq!(has(&texts, "Check for updates"), shown, "{texts:?}");
        }
    }

    /// Both states at once: one block each, the contact line and the
    /// update line or action exactly once.
    #[test]
    fn both_blocking_states() {
        rust_i18n::set_locale("en");
        let min = ("MinimumVersion", RawValue::String("999.0.0".into()));
        let p = resolve(&[admin(&[min]), unreadable()]);
        assert_eq!(p.blocking().len(), 2);
        let texts = crate::tree_probe::texts(view(&p, false));
        let count = |needle: &str| texts.iter().filter(|t| *t == needle).count();
        for needle in [
            "Rustket cannot read your organization's policy",
            "File corp.json · not valid JSON",
            "EOF at line 3",
            "Contact your administrator",
            "An update is required",
        ] {
            assert_eq!(count(needle), 1, "{needle}: {texts:?}");
        }
        let update = count("Check for updates");
        let line = count("Install the new version or ask your administrator.");
        assert_eq!(update + line, 1, "{texts:?}");
        assert_eq!(update == 1, cfg!(feature = "self-update"), "{texts:?}");
    }

    #[test]
    fn details_show_the_sources() {
        rust_i18n::set_locale("en");
        let p = resolve(&[admin(&org()), unreadable()]);
        let hidden = crate::tree_probe::texts(view(&p, false));
        assert!(!has(&hidden, "Sources"), "{hidden:?}");
        let shown = crate::tree_probe::texts(view(&p, true));
        assert!(has(&shown, "Sources"), "{shown:?}");
        assert!(has(&shown, "linux:corp.json"), "{shown:?}");
        assert!(has(&shown, "test:admin"), "{shown:?}");
    }

    /// Every string of the screen exists in every complete locale.
    #[test]
    fn strings_are_translated() {
        let entries = crate::i18n::tests::locale_entries();
        for key in [
            "policy.block_heading",
            "policy.block_unreadable",
            "policy.contact_admin",
            "policy.update_required",
            "policy.update_required_body",
            "policy.update_or_admin",
            "policy.check_updates",
            "desktop.tray_quit",
        ] {
            let texts = entries.get(key).unwrap_or_else(|| panic!("{key}"));
            for loc in crate::i18n::tests::BUNDLED_COMPLETE {
                assert!(
                    texts.get(*loc).is_some_and(|t| !t.is_empty()),
                    "{key}: {loc}"
                );
            }
        }
    }
}
