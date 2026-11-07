//! Account of one server: profile, preferences, security (2FA) and personal
//! access tokens. Owner: account agent. Strings: `locales/account.yml`
//! (`account.*`).

pub mod api;
pub mod preferences;
pub mod profile;
pub mod security;
pub mod tokens;
pub mod two_factor;

use std::sync::atomic::{AtomicU64, Ordering};

use iced::widget::{button, column, container, row, text};
use iced::{Element, Length, Subscription, Task};
use rust_i18n::t;
use rustket_ui::style;
use rustket_ui::theme::{FONT_SEMIBOLD, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_SUBTITLE};
use rustket_ui::widget::{BannerKind, loading_placeholder, modal};

use crate::component::{Action, Ctx, Effect, ViewCtx};
use crate::error::Res;
use api::PasswordPolicy;
use two_factor::{Done, Op, Outcome, Prompt};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Tab {
    #[default]
    Profile,
    Preferences,
    Security,
    Tokens,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Profile, Tab::Preferences, Tab::Security, Tab::Tokens];

    pub fn i18n_key(self) -> &'static str {
        match self {
            Tab::Profile => "account.profile",
            Tab::Security => "account.security",
            Tab::Tokens => "account.tokens",
            Tab::Preferences => "account.preferences",
        }
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    /// Sent once when the screen opens (from the subscription).
    Init,
    Tab(Tab),
    Close,
    /// Copy a text to the clipboard.
    Copy(String),
    PolicyLoaded(Res<PasswordPolicy>),
    Profile(profile::Msg),
    Prefs(preferences::Msg),
    Security(security::Msg),
    Tokens(tokens::Msg),
    Tfa(two_factor::Msg),
    /// A call that may ask for a second factor finished.
    Guarded(Op, Res<Done>),
}

static INSTANCES: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct Account {
    pub tab: Tab,
    /// Distinguishes screen instances (the init subscription fires once per
    /// instance).
    instance: u64,
    pub policy: PasswordPolicy,
    pub profile: profile::Profile,
    pub prefs: preferences::Preferences,
    pub security: security::Security,
    pub tokens: tokens::Tokens,
    pub prompt: Option<Prompt>,
}

#[allow(clippy::new_without_default)]
impl Account {
    pub fn new() -> Self {
        Self {
            tab: Tab::default(),
            instance: INSTANCES.fetch_add(1, Ordering::Relaxed),
            policy: PasswordPolicy::default(),
            profile: Default::default(),
            prefs: Default::default(),
            security: Default::default(),
            tokens: Default::default(),
            prompt: None,
        }
    }

    pub fn with_tab(tab: Tab) -> Self {
        Self { tab, ..Self::new() }
    }

    pub fn update(&mut self, msg: Message, ctx: &mut Ctx<'_>) -> Action<Message> {
        self.profile.sync(ctx.workspace);
        self.prefs.sync(ctx.workspace);
        match msg {
            Message::Init => {
                let policy = ctx.rest_task(
                    |rest| async move { api::password_policy(&rest).await },
                    Message::PolicyLoaded,
                );
                let tokens = self.tokens.load(ctx);
                let sec = self.security.refresh(ctx);
                Action::task(Task::batch([policy, tokens.task, sec.task]))
            }
            Message::PolicyLoaded(r) => {
                if let Ok(p) = r {
                    self.policy = p;
                }
                Action::none()
            }
            Message::Tab(t) => {
                self.tab = t;
                match t {
                    Tab::Tokens => self.tokens.load(ctx),
                    Tab::Security => self.security.refresh(ctx),
                    _ => Action::none(),
                }
            }
            Message::Close => Action::effect(Effect::Close),
            Message::Copy(s) => Action::task(iced::clipboard::write(s)).with(Effect::Toast(
                BannerKind::Success,
                t!("account.copied").into_owned(),
                String::new(),
            )),
            Message::Profile(m) => self.profile.update(m, ctx, &self.policy),
            Message::Prefs(m) => self.prefs.update(m, ctx),
            Message::Security(m) => self.security.update(m, ctx),
            Message::Tokens(m) => self.tokens.update(m, ctx),
            Message::Tfa(m) => self.on_prompt(m, ctx),
            Message::Guarded(op, r) => match r {
                Ok(done) => {
                    if self.prompt.as_ref().is_some_and(|p| p.op == op) {
                        self.prompt = None;
                    }
                    self.finish(op, Some(Ok(done)), ctx)
                }
                Err(f) => match two_factor::on_failure(&op, self.prompt.as_ref(), f) {
                    Outcome::Prompt(p) => {
                        self.prompt = Some(p);
                        Action::task(iced::widget::operation::focus(two_factor::INPUT_ID))
                    }
                    Outcome::Invalid => {
                        if let Some(p) = self.prompt.as_mut() {
                            p.busy = false;
                            p.code.clear();
                            p.error = Some(t!("account.tfa_invalid").into_owned());
                        }
                        Action::none()
                    }
                    Outcome::Failed(f) => {
                        self.prompt = None;
                        self.finish(op, Some(Err(f.user_text())), ctx)
                    }
                },
            },
        }
    }

    fn on_prompt(&mut self, m: two_factor::Msg, ctx: &mut Ctx<'_>) -> Action<Message> {
        use two_factor::Msg as M;
        match m {
            M::Code(c) => {
                if let Some(p) = self.prompt.as_mut() {
                    p.code = c;
                }
                Action::none()
            }
            M::Submit => {
                let Some(p) = self.prompt.as_mut() else {
                    return Action::none();
                };
                if p.busy || p.code.trim().is_empty() {
                    return Action::none();
                }
                p.busy = true;
                p.error = None;
                let (op, code) = (p.op.clone(), p.code());
                Action::task(two_factor::run(ctx, op, Some(code)))
            }
            M::Cancel => match self.prompt.take() {
                Some(p) => self.finish(p.op, None, ctx),
                None => Action::none(),
            },
            M::Resend => {
                let Some(who) = self
                    .prompt
                    .as_ref()
                    .and_then(|p| p.email_or_username.clone())
                    .or_else(|| ctx.workspace.username().map(str::to_owned))
                else {
                    return Action::none();
                };
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.send_email_2fa_code(&who).await },
                    |r| Message::Tfa(M::Resent(r)),
                ))
            }
            M::Resent(r) => {
                if let Some(p) = self.prompt.as_mut() {
                    match r {
                        Ok(()) => p.resent = true,
                        Err(e) => p.error = Some(e.user_text()),
                    }
                }
                Action::none()
            }
        }
    }

    /// Hands the outcome of a guarded call to its tab (`None`: cancelled).
    fn finish(
        &mut self,
        op: Op,
        r: Option<Result<Done, String>>,
        ctx: &mut Ctx<'_>,
    ) -> Action<Message> {
        match op {
            Op::SaveProfile { .. } => {
                match r {
                    Some(Ok(Done::Profile(u))) => self.profile.saved(&u, ctx.workspace),
                    Some(Err(e)) => self.profile.failed(e),
                    _ => self.profile.saving = false,
                }
                Action::none()
            }
            Op::GenerateToken { .. } | Op::RegenerateToken(_) | Op::RemoveToken(_) => {
                let r = match r {
                    Some(Ok(Done::Token { name, token })) => {
                        Ok(tokens::TokenDone::Token { name, token })
                    }
                    Some(Ok(Done::Removed(n))) => Ok(tokens::TokenDone::Removed(n)),
                    Some(Ok(_)) | None => Ok(tokens::TokenDone::Cancelled),
                    Some(Err(e)) => Err(e),
                };
                self.tokens.done(r, ctx)
            }
            Op::DisableEmail2fa => {
                let r = r.map(|r| r.map(|_| ()));
                self.security.email_disabled(r, ctx.workspace);
                Action::none()
            }
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::run_with(self.instance, |_| {
            use iced::futures::StreamExt;
            iced::futures::stream::once(async { Message::Init })
                .chain(iced::futures::stream::pending())
        })
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let tabs = row(Tab::ALL.iter().map(|&tab| {
            button(text(t!(tab.i18n_key()).into_owned()).size(TEXT_BODY))
                .padding([6, 12])
                .style(if tab == self.tab {
                    style::button_primary
                } else {
                    style::button_secondary
                })
                .on_press(Message::Tab(tab))
                .into()
        }))
        .spacing(SPACE_2);
        let loading = || loading_placeholder(None);
        let content: Element<'a, Message> = match self.tab {
            Tab::Profile if self.profile.loaded_for.is_none() => loading(),
            Tab::Profile => self.profile.view(ctx, &self.policy),
            Tab::Preferences if self.prefs.loaded_for.is_none() => loading(),
            Tab::Preferences => self.prefs.view(ctx),
            Tab::Security => self.security.view(ctx),
            Tab::Tokens => self.tokens.view(ctx),
        };
        let body = column![tabs, content].spacing(SPACE_4);
        let page = crate::screens::page::page(
            t!("account.title").into_owned(),
            body.into(),
            Message::Close,
        );
        let (overlay, dismiss) = if let Some(p) = &self.prompt {
            (
                Some(two_factor::view(p)),
                Some(Message::Tfa(two_factor::Msg::Cancel)),
            )
        } else if let Some(d) = self.security.codes_dialog() {
            (Some(d), Some(Message::Security(security::Msg::CloseCodes)))
        } else if let Some(d) = self.tokens.shown_dialog(ctx) {
            (Some(d), Some(Message::Tokens(tokens::Msg::CloseShown)))
        } else {
            (None, None)
        };
        modal(page, overlay, dismiss)
    }
}

/// A titled card of the account tabs.
pub(crate) fn section<'a, M: 'a>(title: String, content: Element<'a, M>) -> Element<'a, M> {
    container(
        column![text(title).size(TEXT_SUBTITLE).font(FONT_SEMIBOLD), content].spacing(SPACE_3),
    )
    .padding(SPACE_4)
    .width(Length::Fill)
    .style(style::container_card)
    .into()
}
