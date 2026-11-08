//! Security → End-to-end encryption (web `account/security/ChangePassphrase`
//! and `ResetPassphrase`, `docs/e2e.md` §7.1, §10): change the E2E
//! passphrase (web policy, printable ASCII only, §3.3.4) and reset the own
//! E2E keys (`users.resetE2EKey`, second factor through the account prompt;
//! the server revokes every login token, so the app logs out).

use iced::widget::{column, row, text};
use iced::{Alignment, Element, Length};
use rocketchat::e2e::Zeroizing;
use rust_i18n::t;
use rustket_core::e2e::AccountE2eState;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{
    BannerKind, ChipKind, banner, chip, danger_button, primary_button, secondary_button, text_field,
};

use super::two_factor::{self, Op};
use super::{Message, section};
use crate::component::{Action, Ctx, Effect, ViewCtx};

#[derive(Debug, Clone)]
pub enum Msg {
    New(String),
    Confirm(String),
    Change,
    Changed(Result<(), String>),
    AskReset,
    CancelReset,
    Reset,
}

#[derive(Default)]
pub struct E2eSection {
    new: Zeroizing<String>,
    confirm: Zeroizing<String>,
    busy: bool,
    error: Option<String>,
    notice: Option<String>,
    confirm_reset: bool,
    resetting: bool,
}

impl std::fmt::Debug for E2eSection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("E2eSection")
            .field("busy", &self.busy)
            .field("resetting", &self.resetting)
            .finish_non_exhaustive()
    }
}

/// The web passphrase policy, item by item (§3.3.4).
pub fn policy(p: &str) -> [(&'static str, bool); 6] {
    [
        (
            "e2e.account_rule_length",
            p.encode_utf16().count() >= rocketchat::e2e::PASSPHRASE_MIN_LENGTH,
        ),
        (
            "e2e.account_rule_lower",
            p.chars().any(|c| c.is_ascii_lowercase()),
        ),
        (
            "e2e.account_rule_upper",
            p.chars().any(|c| c.is_ascii_uppercase()),
        ),
        (
            "e2e.account_rule_digit",
            p.chars().any(|c| c.is_ascii_digit()),
        ),
        (
            "e2e.account_rule_special",
            p.chars().any(|c| !c.is_ascii_alphanumeric() && c != ' '),
        ),
        (
            "e2e.account_rule_ascii",
            !p.is_empty() && p.chars().all(|c| (' '..='~').contains(&c)),
        ),
    ]
}

fn wrap(m: Msg) -> Message {
    Message::Security(super::security::Msg::E2e(m))
}

impl E2eSection {
    pub fn update(&mut self, msg: Msg, ctx: &mut Ctx<'_>) -> Action<Message> {
        match msg {
            Msg::New(v) => {
                self.new = Zeroizing::new(v);
                self.error = None;
                self.notice = None;
                Action::none()
            }
            Msg::Confirm(v) => {
                self.confirm = Zeroizing::new(v);
                self.error = None;
                Action::none()
            }
            Msg::Change => {
                if self.busy || !self.can_submit() {
                    return Action::none();
                }
                let Some(mgr) = ctx.workspace.e2e().cloned() else {
                    return Action::none();
                };
                self.busy = true;
                self.error = None;
                let pw = self.new.clone();
                Action::task(iced::Task::perform(
                    async move { mgr.change_passphrase(&pw).await },
                    |r| wrap(Msg::Changed(r.map_err(|e| e.to_string()))),
                ))
            }
            Msg::Changed(r) => {
                self.busy = false;
                match r {
                    Ok(()) => {
                        self.new = Zeroizing::default();
                        self.confirm = Zeroizing::default();
                        self.notice = Some(t!("e2e.account_changed").into_owned());
                        Action::effect(Effect::Toast(
                            BannerKind::Success,
                            t!("e2e.account_changed").into_owned(),
                            String::new(),
                        ))
                    }
                    Err(e) => {
                        self.error = Some(e);
                        Action::none()
                    }
                }
            }
            Msg::AskReset => {
                self.confirm_reset = true;
                Action::none()
            }
            Msg::CancelReset => {
                if !self.resetting {
                    self.confirm_reset = false;
                }
                Action::none()
            }
            Msg::Reset => {
                if self.resetting {
                    return Action::none();
                }
                self.resetting = true;
                self.error = None;
                Action::task(two_factor::run(ctx, Op::ResetE2eKey, None))
            }
        }
    }

    fn can_submit(&self) -> bool {
        policy(&self.new).iter().all(|(_, ok)| *ok) && *self.new == *self.confirm
    }

    /// The reset finished (`None`: the second-factor prompt was cancelled).
    pub fn reset_done(&mut self, r: Option<Result<(), String>>) -> Action<Message> {
        self.resetting = false;
        self.confirm_reset = false;
        match r {
            Some(Ok(())) => Action::effect(Effect::Toast(
                BannerKind::Success,
                t!("e2e.account_reset_done").into_owned(),
                t!("e2e.account_reset_done_body").into_owned(),
            ))
            .with(Effect::Logout { everywhere: false }),
            Some(Err(e)) => {
                self.error = Some(e);
                Action::none()
            }
            None => Action::none(),
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<Element<'a, Message>> {
        let ws = ctx.workspace;
        if !ws.settings().bool_or("E2E_Enable", false) {
            return None;
        }
        let state = ws.e2e().map(|m| m.account_state());
        let ready = state.as_ref().is_some_and(AccountE2eState::is_ready);
        let (label, kind) = match &state {
            Some(s) if s.is_ready() => (t!("e2e.account_state_ready"), ChipKind::Success),
            Some(AccountE2eState::EnterPassword { .. }) => {
                (t!("e2e.account_state_locked"), ChipKind::Warning)
            }
            Some(AccountE2eState::LoadingKeys) => {
                (t!("e2e.account_state_loading"), ChipKind::Neutral)
            }
            Some(AccountE2eState::Error(_)) => (t!("e2e.account_state_error"), ChipKind::Danger),
            _ => (t!("e2e.account_state_off"), ChipKind::Neutral),
        };
        let mut c = column![
            row![
                text(t!("e2e.account_help"))
                    .size(TEXT_SMALL)
                    .style(style::text_secondary)
                    .width(Length::Fill),
                chip(label.into_owned(), kind),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
        ]
        .spacing(SPACE_3);
        if let Some(e) = &self.error {
            c = c.push(banner(BannerKind::Error, e.clone(), None));
        }
        if let Some(n) = &self.notice {
            c = c.push(banner(BannerKind::Success, n.clone(), None));
        }

        // Change the passphrase.
        c = c.push(text(t!("e2e.account_change_title")).size(TEXT_BODY));
        if ready {
            let mismatch = !self.confirm.is_empty() && *self.new != *self.confirm;
            let mut confirm = text_field(t!("e2e.account_confirm_label"), &self.confirm, |v| {
                wrap(Msg::Confirm(v))
            })
            .password(true)
            .disabled(self.busy)
            .on_submit(wrap(Msg::Change));
            if mismatch {
                confirm = confirm.error(Some(t!("e2e.account_mismatch").into_owned()));
            }
            c = c.push(
                text_field(t!("e2e.account_new_label"), &self.new, |v| {
                    wrap(Msg::New(v))
                })
                .password(true)
                .disabled(self.busy),
            );
            if !self.new.is_empty() {
                let rules = policy(&self.new).map(|(key, ok)| -> Element<'a, Message> {
                    row![
                        icon::sized(if ok { Icon::Check } else { Icon::X }, 12.0).style(if ok {
                            style::text_success
                        } else {
                            style::text_hint
                        }),
                        text(t!(key)).size(TEXT_CAPTION).style(if ok {
                            style::text_secondary
                        } else {
                            style::text_hint
                        }),
                    ]
                    .spacing(SPACE_1)
                    .align_y(Alignment::Center)
                    .into()
                });
                c = c.push(column(rules).spacing(2));
            }
            c = c.push(confirm);
            c = c.push(row![
                primary_button(
                    t!("e2e.account_change"),
                    (self.can_submit() && !self.busy).then_some(wrap(Msg::Change)),
                )
                .loading(self.busy)
            ]);
        } else {
            c = c.push(
                text(t!("e2e.account_change_unavailable"))
                    .size(TEXT_SMALL)
                    .style(style::text_hint),
            );
        }

        // Reset the keys.
        c = c.push(text(t!("e2e.account_reset_title")).size(TEXT_BODY));
        c = c.push(
            text(t!("e2e.account_reset_help"))
                .size(TEXT_SMALL)
                .style(style::text_secondary),
        );
        if self.confirm_reset {
            c = c.push(banner(
                BannerKind::Warning,
                t!("e2e.account_reset_confirm"),
                None,
            ));
            c = c.push(
                row![
                    secondary_button(
                        t!("account.cancel"),
                        (!self.resetting).then_some(wrap(Msg::CancelReset)),
                    ),
                    danger_button(
                        t!("e2e.account_reset"),
                        (!self.resetting).then_some(wrap(Msg::Reset)),
                    )
                    .loading(self.resetting),
                ]
                .spacing(SPACE_2),
            );
        } else {
            c = c.push(row![danger_button(
                t!("e2e.account_reset"),
                Some(wrap(Msg::AskReset)),
            )]);
        }
        Some(section(t!("e2e.account_title").into_owned(), c.into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passphrase_policy() {
        let ok = "Correct-Horse-Battery-Staple-42-xx";
        assert!(policy(ok).iter().all(|(_, v)| *v));
        assert!(!policy("short-A1")[0].1);
        // Non-ASCII is refused (decryptable by every client, §3.3.4).
        let p = policy("Ключ-очень-длинный-ABC-123-abcdef-ghijk");
        assert!(!p[5].1);
        let mut s = E2eSection {
            new: Zeroizing::new(ok.into()),
            confirm: Zeroizing::new(ok.into()),
            ..Default::default()
        };
        assert!(s.can_submit());
        s.confirm = Zeroizing::new("other".into());
        assert!(!s.can_submit());
    }

    #[test]
    fn reset_logs_out() {
        let mut s = E2eSection::default();
        let a = s.reset_done(Some(Ok(())));
        assert!(
            a.effects
                .iter()
                .any(|e| matches!(e, Effect::Logout { everywhere: false }))
        );
        let a = s.reset_done(None);
        assert!(a.effects.is_empty());
    }
}
