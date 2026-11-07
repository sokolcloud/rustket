//! Profile tab: name, username, nickname, bio, status text, e-mail, avatar and
//! password (`users.updateOwnBasicInfo`, `users.setAvatar`,
//! `users.resetAvatar`).

use std::time::Duration;

use iced::widget::{column, container, row, text};
use iced::{Alignment, Element, Length, Task};
use rocketchat::auth::TwoFactorCode;
use rocketchat::models::User;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::room_list::Avatar;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_CAPTION, TEXT_SMALL};
use rustket_ui::widget::{
    AvatarShape, BannerKind, avatar, banner, ghost_button, primary_button, secondary_button,
    text_field,
};
use serde_json::{Map, Value};

use super::api::PasswordPolicy;
use super::two_factor::{self, Op};
use super::{Message, section};
use crate::component::{Action, Ctx, Effect, ViewCtx};
use crate::dialogs::create_room::names::{self, NameError};
use crate::error::{Failure, Res};

pub const MAX_BIO: usize = 260;
pub const MAX_NICKNAME: usize = 120;
pub const MAX_STATUS_TEXT: usize = 120;
const USERNAME_DEBOUNCE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Fields {
    pub name: String,
    pub username: String,
    pub nickname: String,
    pub bio: String,
    pub status_text: String,
    pub email: String,
}

impl Fields {
    pub fn from_user(u: &User) -> Self {
        Self {
            name: u.name.clone().unwrap_or_default(),
            username: u.username.clone().unwrap_or_default(),
            nickname: u.nickname.clone().unwrap_or_default(),
            bio: u.bio.clone().unwrap_or_default(),
            status_text: u.status_text.clone().unwrap_or_default(),
            email: primary_email(u).unwrap_or_default().to_owned(),
        }
    }
}

fn primary_email(u: &User) -> Option<&str> {
    u.emails
        .as_ref()
        .and_then(|e| e.first())
        .map(|e| e.address.as_str())
        .or(u.email.as_deref())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Availability {
    #[default]
    Unknown,
    Checking,
    Available,
    Taken,
}

#[derive(Debug, Clone)]
pub enum Msg {
    Name(String),
    Username(String),
    Nickname(String),
    Bio(String),
    StatusText(String),
    Email(String),
    NewPassword(String),
    ConfirmPassword(String),
    CurrentPassword(String),
    Save,
    Revert,
    CheckUsername(u64),
    UsernameChecked(u64, String, Res<bool>),
    PickAvatar,
    AvatarSet(Res<bool>),
    ResetAvatar,
    AvatarReset(Res<()>),
    ResendVerification,
    VerificationSent(Res<()>),
}

/// Validation errors per field (already translated).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Errors {
    pub name: Option<String>,
    pub username: Option<String>,
    pub email: Option<String>,
    pub nickname: Option<String>,
    pub bio: Option<String>,
    pub status_text: Option<String>,
    pub password: Option<String>,
    pub confirm: Option<String>,
    /// A required field is still empty (no message shown).
    pub incomplete: bool,
}

impl Errors {
    pub fn any(&self) -> bool {
        *self != Self::default()
    }
}

#[derive(Debug, Default)]
pub struct Profile {
    /// User id the form was filled from.
    pub loaded_for: Option<String>,
    pub form: Fields,
    pub original: Fields,
    pub new_password: String,
    pub confirm_password: String,
    pub current_password: String,
    pub saving: bool,
    pub error: Option<String>,
    pub saved: bool,
    pub username_seq: u64,
    pub username_state: Availability,
    pub avatar_busy: bool,
    pub notice: Option<String>,
}

/// Which profile changes the server allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Allowed {
    pub profile: bool,
    pub name: bool,
    pub username: bool,
    pub email: bool,
    pub status_text: bool,
    pub password: bool,
    pub avatar: bool,
}

impl Allowed {
    pub fn from(ws: &Workspace) -> Self {
        let s = ws.settings();
        let profile = s.bool_or("Accounts_AllowUserProfileChange", true);
        Self {
            profile,
            name: profile && s.allow_real_name_change(),
            username: profile && s.allow_username_change(),
            email: profile && s.allow_email_change(),
            status_text: profile && s.allow_status_message_change(),
            password: profile && s.allow_password_change(),
            avatar: s.allow_avatar_change(),
        }
    }
}

fn valid_email(e: &str) -> bool {
    let Some((local, domain)) = e.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !e.chars().any(char::is_whitespace)
}

impl Profile {
    /// Fills the form from the current user (once per user, or after a save).
    pub fn sync(&mut self, ws: &Workspace) {
        let Some(me) = ws.me() else {
            return;
        };
        if self.loaded_for.as_deref() == Some(me.id.as_str()) {
            return;
        }
        self.loaded_for = Some(me.id.clone());
        self.original = Fields::from_user(me);
        self.form = self.original.clone();
    }

    /// The changed, allowed fields as `updateOwnBasicInfo` `data`.
    pub fn changes(&self, allowed: Allowed) -> Map<String, Value> {
        let mut m = Map::new();
        let (f, o) = (&self.form, &self.original);
        let mut put = |k: &str, new: &str, old: &str, ok: bool| {
            if ok && new.trim() != old.trim() {
                m.insert(k.to_owned(), Value::String(new.trim().to_owned()));
            }
        };
        put("name", &f.name, &o.name, allowed.name);
        put("username", &f.username, &o.username, allowed.username);
        put("nickname", &f.nickname, &o.nickname, allowed.profile);
        put("bio", &f.bio, &o.bio, allowed.profile);
        put(
            "statusText",
            &f.status_text,
            &o.status_text,
            allowed.status_text,
        );
        put("email", &f.email, &o.email, allowed.email);
        if allowed.password && !self.new_password.is_empty() {
            m.insert(
                "newPassword".to_owned(),
                Value::String(self.new_password.clone()),
            );
        }
        m
    }

    pub fn validate(&self, ws: &Workspace, policy: &PasswordPolicy) -> Errors {
        let mut e = Errors::default();
        let (f, o) = (&self.form, &self.original);
        if f.name.trim() != o.name.trim() {
            if f.name.trim().is_empty()
                && ws.settings().bool_or("Accounts_RequireNameForSignUp", true)
            {
                e.name = Some(t!("account.err_required").into_owned());
            } else if f.name.contains(['<', '>', '\\', '/']) {
                e.name = Some(t!("account.err_name_chars").into_owned());
            }
        }
        if f.username.trim() != o.username.trim() {
            e.username = match names::check_username(ws, f.username.trim()) {
                Err(NameError::Empty) => Some(t!("account.err_required").into_owned()),
                Err(NameError::Invalid) => Some(t!("account.err_username").into_owned()),
                Ok(()) if self.username_state == Availability::Taken => {
                    Some(t!("account.err_username_taken").into_owned())
                }
                Ok(()) => None,
            };
        }
        if f.email.trim() != o.email.trim() && !valid_email(f.email.trim()) {
            e.email = Some(t!("account.err_email").into_owned());
        }
        let too_long = |s: &str, max: usize| {
            (s.chars().count() > max).then(|| t!("account.err_too_long", max = max).into_owned())
        };
        e.nickname = too_long(&f.nickname, MAX_NICKNAME);
        e.bio = too_long(&f.bio, MAX_BIO);
        e.status_text = too_long(&f.status_text, MAX_STATUS_TEXT);
        if !self.new_password.is_empty() {
            if !policy.unmet(&self.new_password).is_empty() {
                e.password = Some(t!("account.err_password_policy").into_owned());
            }
            if self.confirm_password.is_empty() {
                // Not an error to show yet, but nothing to save either.
                e.incomplete = true;
            } else if self.confirm_password != self.new_password {
                e.confirm = Some(t!("account.err_password_mismatch").into_owned());
            }
        }
        e
    }

    /// The server wants the current password for an e-mail or password change
    /// (as the 2FA fallback) when the user has no other second factor.
    pub fn wants_current_password(&self, ws: &Workspace, allowed: Allowed) -> bool {
        let c = self.changes(allowed);
        let me = ws.me();
        (c.contains_key("email") || c.contains_key("newPassword"))
            && !me.is_some_and(|m| m.totp_enabled() || m.email_2fa_enabled())
    }

    pub fn update(
        &mut self,
        msg: Msg,
        ctx: &mut Ctx<'_>,
        policy: &PasswordPolicy,
    ) -> Action<Message> {
        let p = |m| Message::Profile(m);
        match msg {
            Msg::Name(s) => self.edit(|f| f.name = s),
            Msg::Nickname(s) => self.edit(|f| f.nickname = s),
            Msg::Bio(s) => self.edit(|f| f.bio = s),
            Msg::StatusText(s) => self.edit(|f| f.status_text = s),
            Msg::Email(s) => self.edit(|f| f.email = s),
            Msg::Username(s) => {
                self.edit(|f| f.username = s);
                self.username_seq += 1;
                self.username_state = Availability::Unknown;
                let name = self.form.username.trim().to_owned();
                if name == self.original.username
                    || names::check_username(ctx.workspace, &name).is_err()
                {
                    return Action::none();
                }
                let seq = self.username_seq;
                Action::task(Task::perform(
                    tokio::time::sleep(USERNAME_DEBOUNCE),
                    move |_| p(Msg::CheckUsername(seq)),
                ))
            }
            Msg::CheckUsername(seq) => {
                if seq != self.username_seq {
                    return Action::none();
                }
                self.username_state = Availability::Checking;
                let name = self.form.username.trim().to_owned();
                let n2 = name.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move { super::api::username_available(&rest, &name).await },
                    move |r| p(Msg::UsernameChecked(seq, n2, r)),
                ))
            }
            Msg::UsernameChecked(seq, name, r) => {
                if seq == self.username_seq && name == self.form.username.trim() {
                    self.username_state = match r {
                        Ok(true) => Availability::Available,
                        Ok(false) => Availability::Taken,
                        Err(_) => Availability::Unknown,
                    };
                }
                Action::none()
            }
            Msg::NewPassword(s) => {
                self.new_password = s;
                self.saved = false;
                Action::none()
            }
            Msg::ConfirmPassword(s) => {
                self.confirm_password = s;
                Action::none()
            }
            Msg::CurrentPassword(s) => {
                self.current_password = s;
                Action::none()
            }
            Msg::Revert => {
                self.form = self.original.clone();
                self.new_password.clear();
                self.confirm_password.clear();
                self.current_password.clear();
                self.error = None;
                self.username_state = Availability::Unknown;
                Action::none()
            }
            Msg::Save => {
                let allowed = Allowed::from(ctx.workspace);
                let changes = self.changes(allowed);
                if self.saving || changes.is_empty() || self.validate(ctx.workspace, policy).any() {
                    return Action::none();
                }
                self.saving = true;
                self.error = None;
                self.saved = false;
                let code = (!self.current_password.is_empty()
                    && self.wants_current_password(ctx.workspace, allowed))
                .then(|| TwoFactorCode::password(&self.current_password));
                Action::task(two_factor::run(
                    ctx,
                    Op::SaveProfile {
                        data: Value::Object(changes),
                    },
                    code,
                ))
            }
            Msg::PickAvatar => {
                if self.avatar_busy {
                    return Action::none();
                }
                self.avatar_busy = true;
                self.notice = None;
                let rest = ctx.rest();
                let max = ctx.workspace.settings().file_upload_max_size();
                Action::task(Task::perform(pick_and_upload(rest, max), move |r| {
                    p(Msg::AvatarSet(r))
                }))
            }
            Msg::AvatarSet(r) => {
                self.avatar_busy = false;
                match r {
                    Ok(true) => {
                        self.notice = Some(t!("account.avatar_updated").into_owned());
                        Action::none()
                    }
                    Ok(false) => Action::none(),
                    Err(e) => failed_toast(t!("account.avatar_failed"), &e),
                }
            }
            Msg::ResetAvatar => {
                let Some(uid) = ctx.workspace.user_id().map(str::to_owned) else {
                    return Action::none();
                };
                self.avatar_busy = true;
                self.notice = None;
                Action::task(ctx.rest_task(
                    move |rest| async move { rest.users_reset_avatar(&uid).await },
                    move |r| p(Msg::AvatarReset(r)),
                ))
            }
            Msg::AvatarReset(r) => {
                self.avatar_busy = false;
                match r {
                    Ok(()) => {
                        self.notice = Some(t!("account.avatar_reset_done").into_owned());
                        Action::none()
                    }
                    Err(e) => failed_toast(t!("account.avatar_failed"), &e),
                }
            }
            Msg::ResendVerification => {
                let email = self.original.email.clone();
                if email.is_empty() {
                    return Action::none();
                }
                Action::task(
                    ctx.rest_task(
                        move |rest| async move {
                            super::api::send_confirmation_email(&rest, &email).await
                        },
                        move |r| p(Msg::VerificationSent(r)),
                    ),
                )
            }
            Msg::VerificationSent(r) => match r {
                Ok(()) => {
                    self.notice = Some(t!("account.verification_sent").into_owned());
                    Action::none()
                }
                Err(e) => failed_toast(t!("account.verification_failed"), &e),
            },
        }
    }

    fn edit(&mut self, f: impl FnOnce(&mut Fields)) -> Action<Message> {
        f(&mut self.form);
        self.saved = false;
        Action::none()
    }

    /// `updateOwnBasicInfo` succeeded: keep the workspace user current (the
    /// realtime `userData` diff may lag or miss fields) and reset the form.
    pub fn saved(&mut self, user: &User, ws: &mut Workspace) {
        self.saving = false;
        self.saved = true;
        self.error = None;
        self.new_password.clear();
        self.confirm_password.clear();
        self.current_password.clear();
        if let Some(mut me) = ws.me().cloned() {
            me.name = user.name.clone().or(me.name);
            me.username = user.username.clone().or(me.username);
            me.nickname = user.nickname.clone();
            me.bio = user.bio.clone();
            me.status_text = user.status_text.clone().or(me.status_text);
            if user.emails.is_some() {
                me.emails = user.emails.clone();
            }
            self.original = Fields::from_user(&me);
            ws.apply_me(me);
        } else {
            self.original = Fields::from_user(user);
        }
        self.form = self.original.clone();
        self.username_state = Availability::Unknown;
    }

    pub fn failed(&mut self, text: String) {
        self.saving = false;
        self.error = Some(text);
    }

    pub fn view<'a>(
        &'a self,
        ctx: &ViewCtx<'a>,
        policy: &'a PasswordPolicy,
    ) -> Element<'a, Message> {
        let p = |m| Message::Profile(m);
        let ws = ctx.workspace;
        let allowed = Allowed::from(ws);
        let errors = self.validate(ws, policy);
        let me = ws.me();

        // Avatar.
        let username = self.original.username.clone();
        let src = ctx.images.avatar(
            ctx.server_id,
            ctx.server,
            &Avatar::User {
                username: username.clone(),
            },
            if self.original.name.is_empty() {
                &self.original.username
            } else {
                &self.original.name
            },
        );
        let mut avatar_actions = column![].spacing(SPACE_2);
        if allowed.avatar {
            avatar_actions = avatar_actions
                .push(
                    secondary_button(
                        t!("account.avatar_upload"),
                        (!self.avatar_busy).then_some(p(Msg::PickAvatar)),
                    )
                    .icon(Icon::Upload)
                    .loading(self.avatar_busy),
                )
                .push(
                    ghost_button(
                        t!("account.avatar_reset"),
                        (!self.avatar_busy).then_some(p(Msg::ResetAvatar)),
                    )
                    .icon(Icon::RotateCcw),
                );
        } else {
            avatar_actions = avatar_actions.push(
                text(t!("account.avatar_locked"))
                    .size(TEXT_SMALL)
                    .style(style::text_secondary),
            );
        }
        let avatar_row = row![avatar(src, 96.0, AvatarShape::Rounded), avatar_actions]
            .spacing(SPACE_4)
            .align_y(Alignment::Center);

        let locked = |ok: bool| {
            if ok {
                None
            } else {
                Some(t!("account.locked").into_owned())
            }
        };
        let field = |label: String,
                     value: &'a str,
                     ok: bool,
                     err: Option<String>,
                     on: fn(String) -> Msg| {
            let mut f = text_field(label, value, move |s| p(on(s)))
                .disabled(!ok)
                .error(err)
                .on_submit(p(Msg::Save));
            if let Some(h) = locked(ok) {
                f = f.help(h);
            }
            Element::from(f)
        };

        let username_help = match self.username_state {
            Availability::Checking => Some(t!("account.username_checking").into_owned()),
            Availability::Available => Some(t!("account.username_available").into_owned()),
            _ => None,
        };
        let mut username_field =
            text_field(t!("account.username"), &self.form.username, move |s| {
                p(Msg::Username(s))
            })
            .disabled(!allowed.username)
            .error(errors.username.clone())
            .on_submit(p(Msg::Save));
        if let Some(h) = username_help.or_else(|| locked(allowed.username)) {
            username_field = username_field.help(h);
        }

        let mut details = column![
            field(
                t!("account.name").into_owned(),
                &self.form.name,
                allowed.name,
                errors.name.clone(),
                Msg::Name
            ),
            username_field,
            field(
                t!("account.nickname").into_owned(),
                &self.form.nickname,
                allowed.profile,
                errors.nickname.clone(),
                Msg::Nickname
            ),
            field(
                t!("account.bio").into_owned(),
                &self.form.bio,
                allowed.profile,
                errors.bio.clone(),
                Msg::Bio
            ),
            field(
                t!("account.status_text").into_owned(),
                &self.form.status_text,
                allowed.status_text,
                errors.status_text.clone(),
                Msg::StatusText
            ),
            field(
                t!("account.email").into_owned(),
                &self.form.email,
                allowed.email,
                errors.email.clone(),
                Msg::Email
            ),
        ]
        .spacing(SPACE_3);
        let verified = me
            .and_then(|m| m.emails.as_ref())
            .and_then(|e| e.first())
            .map(|e| e.verified);
        match verified {
            Some(false) => {
                details = details.push(banner(
                    BannerKind::Warning,
                    t!("account.email_unverified"),
                    Some((
                        t!("account.email_resend").into_owned(),
                        p(Msg::ResendVerification),
                    )),
                ));
            }
            Some(true) => {
                details = details.push(
                    row![
                        icon::sized(Icon::BadgeCheck, 14.0).style(style::text_success),
                        text(t!("account.email_verified"))
                            .size(TEXT_CAPTION)
                            .style(style::text_success)
                    ]
                    .spacing(SPACE_1)
                    .align_y(Alignment::Center),
                );
            }
            None => {}
        }
        if self.form.email.trim() != self.original.email.trim() {
            details = details.push(
                text(t!("account.email_change_hint"))
                    .size(TEXT_CAPTION)
                    .style(style::text_hint),
            );
        }

        let mut password = column![].spacing(SPACE_3);
        if allowed.password {
            password = password
                .push(
                    text_field(t!("account.new_password"), &self.new_password, move |s| {
                        p(Msg::NewPassword(s))
                    })
                    .password(true)
                    .error(errors.password.clone()),
                )
                .push(
                    text_field(
                        t!("account.confirm_password"),
                        &self.confirm_password,
                        move |s| p(Msg::ConfirmPassword(s)),
                    )
                    .password(true)
                    .error(errors.confirm.clone()),
                );
            if policy.enabled && !policy.rules.is_empty() {
                let mut hints = column![
                    text(t!("account.pw_policy_title"))
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary)
                ]
                .spacing(SPACE_1);
                for rule in &policy.rules {
                    let ok = !self.new_password.is_empty() && rule.satisfied(&self.new_password);
                    hints = hints.push(
                        row![
                            icon::sized(if ok { Icon::Check } else { Icon::Dot }, 14.0).style(
                                if ok {
                                    style::text_success
                                } else {
                                    style::text_hint
                                }
                            ),
                            text(rule.hint()).size(TEXT_CAPTION).style(if ok {
                                style::text_success
                            } else {
                                style::text_secondary
                            }),
                        ]
                        .spacing(SPACE_1)
                        .align_y(Alignment::Center),
                    );
                }
                password = password.push(hints);
            }
        } else {
            password = password.push(
                text(t!("account.password_locked"))
                    .size(TEXT_SMALL)
                    .style(style::text_secondary),
            );
        }

        let changes = self.changes(allowed);
        let mut actions = column![].spacing(SPACE_3);
        if self.wants_current_password(ws, allowed) {
            actions = actions.push(
                text_field(
                    t!("account.current_password"),
                    &self.current_password,
                    move |s| p(Msg::CurrentPassword(s)),
                )
                .password(true)
                .help(t!("account.current_password_help"))
                .on_submit(p(Msg::Save)),
            );
        }
        if let Some(e) = &self.error {
            actions = actions.push(banner(BannerKind::Error, e.clone(), None));
        }
        if self.saved {
            actions = actions.push(banner(BannerKind::Success, t!("account.saved"), None));
        }
        if let Some(n) = &self.notice {
            actions = actions.push(banner(BannerKind::Info, n.clone(), None));
        }
        let can_save = !changes.is_empty() && !errors.any() && !self.saving;
        actions = actions.push(
            row![
                secondary_button(
                    t!("account.revert"),
                    (!changes.is_empty() && !self.saving).then_some(p(Msg::Revert))
                ),
                primary_button(t!("account.save"), can_save.then_some(p(Msg::Save)))
                    .loading(self.saving),
            ]
            .spacing(SPACE_2),
        );

        let left = column![
            section(t!("account.avatar").into_owned(), avatar_row.into()),
            section(t!("account.details").into_owned(), details.into()),
        ]
        .spacing(SPACE_4)
        .width(Length::Fill);
        let mut right = column![
            section(t!("account.change_password").into_owned(), password.into()),
            actions,
        ]
        .spacing(SPACE_4)
        .width(Length::Fill);
        if !allowed.profile {
            right = right.push(banner(BannerKind::Info, t!("account.profile_locked"), None));
        }
        container(row![left, right].spacing(SPACE_4))
            .max_width(1100)
            .width(Length::Fill)
            .into()
    }
}

fn failed_toast(title: impl Into<String>, e: &Failure) -> Action<Message> {
    Action::effect(Effect::Toast(
        BannerKind::Error,
        title.into(),
        e.user_text(),
    ))
}

/// Image picker → `users.setAvatar`. `Ok(false)`: cancelled.
async fn pick_and_upload(rest: Option<rocketchat::RestClient>, max: Option<u64>) -> Res<bool> {
    let Some(rest) = rest else {
        return Err(Failure::from(rocketchat::Error::Unauthorized {
            message: Some("not connected".into()),
            code: None,
        }));
    };
    let Some(file) = rfd::AsyncFileDialog::new()
        .set_title(t!("account.avatar_pick_title"))
        .add_filter(
            t!("account.avatar_images").into_owned(),
            &["png", "jpg", "jpeg", "gif", "webp"],
        )
        .pick_file()
        .await
    else {
        return Ok(false);
    };
    let name = file.file_name();
    let bytes = file.read().await;
    if max.is_some_and(|m| m > 0 && bytes.len() as u64 > m) {
        return Err(Failure::from(rocketchat::Error::Api {
            status: 413,
            error: None,
            error_type: None,
            message: Some(t!("account.avatar_too_large").into_owned()),
            details: None,
        }));
    }
    let mime = image_mime(&name);
    rest.users_set_avatar(&name, mime, bytes)
        .await
        .map_err(Failure::from)?;
    Ok(true)
}

pub fn image_mime(name: &str) -> &'static str {
    let ext = name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "image/png",
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rocketchat::models::{Email, Setting};
    use rustket_core::workspace::Change;
    use serde_json::json;

    fn me() -> User {
        User {
            id: "u1".into(),
            username: Some("alice".into()),
            name: Some("Alice".into()),
            emails: Some(vec![Email {
                address: "alice@x.io".into(),
                verified: true,
            }]),
            ..Default::default()
        }
    }

    fn ws() -> Workspace {
        let mut ws = Workspace::new();
        ws.apply_me(me());
        ws
    }

    fn all() -> Allowed {
        Allowed::from(&Workspace::new())
    }

    #[test]
    fn changes_only_contain_edited_allowed_fields() {
        let ws = ws();
        let mut p = Profile::default();
        p.sync(&ws);
        assert_eq!(p.form.email, "alice@x.io");
        assert!(p.changes(all()).is_empty());
        p.form.nickname = " nick ".into();
        p.form.name = "Alice".into();
        p.new_password = "Secret-pass-123!".into();
        let c = p.changes(all());
        assert_eq!(c.get("nickname"), Some(&json!("nick")));
        assert_eq!(c.get("newPassword"), Some(&json!("Secret-pass-123!")));
        assert!(!c.contains_key("name"));
        let mut locked = all();
        locked.password = false;
        assert!(!p.changes(locked).contains_key("newPassword"));
        // Password change without 2FA: the current password is asked inline.
        assert!(p.wants_current_password(&ws, all()));
        p.new_password.clear();
        assert!(!p.wants_current_password(&ws, all()));
    }

    #[test]
    fn settings_lock_fields() {
        let mut ws = ws();
        ws.apply_setting(
            Change::Inserted,
            Setting {
                id: "Accounts_AllowUsernameChange".into(),
                value: json!(false),
                ..Default::default()
            },
        );
        let a = Allowed::from(&ws);
        assert!(!a.username && a.name && a.email);
        let mut p = Profile::default();
        p.sync(&ws);
        p.form.username = "bob".into();
        assert!(p.changes(a).is_empty());
        ws.apply_setting(
            Change::Inserted,
            Setting {
                id: "Accounts_AllowUserProfileChange".into(),
                value: json!(false),
                ..Default::default()
            },
        );
        let a = Allowed::from(&ws);
        assert!(!a.name && !a.email && !a.password && !a.status_text);
    }

    #[test]
    fn validation() {
        let ws = ws();
        let policy = PasswordPolicy {
            enabled: true,
            rules: vec![super::super::api::PolicyRule::MinLength(8)],
        };
        let mut p = Profile::default();
        p.sync(&ws);
        assert!(!p.validate(&ws, &policy).any());
        p.form.username = "bad name".into();
        p.form.email = "nope".into();
        p.form.name = "a<b".into();
        p.form.bio = "x".repeat(MAX_BIO + 1);
        p.new_password = "short".into();
        assert!(p.validate(&ws, &policy).incomplete);
        p.confirm_password = "other".into();
        let e = p.validate(&ws, &policy);
        assert!(e.username.is_some() && e.email.is_some() && e.name.is_some());
        assert!(e.bio.is_some() && e.password.is_some() && e.confirm.is_some());
        p.form.username = "bob".into();
        p.username_state = Availability::Taken;
        assert!(p.validate(&ws, &policy).username.is_some());
        p.form.name.clear();
        assert!(p.validate(&ws, &policy).name.is_some());
        assert!(valid_email("a.b@c.de") && !valid_email("a@b") && !valid_email("a b@c.d"));
    }

    #[test]
    fn saved_updates_the_workspace_user() {
        let mut ws = ws();
        let mut p = Profile::default();
        p.sync(&ws);
        p.form.nickname = "nick".into();
        p.new_password = "x".into();
        let mut u = me();
        u.nickname = Some("nick".into());
        u.bio = Some("bio".into());
        p.saved(&u, &mut ws);
        assert_eq!(ws.me().unwrap().nickname.as_deref(), Some("nick"));
        assert_eq!(p.original.bio, "bio");
        assert_eq!(p.form, p.original);
        assert!(p.new_password.is_empty() && p.saved);
    }

    #[test]
    fn mime_from_extension() {
        assert_eq!(image_mime("a.JPG"), "image/jpeg");
        assert_eq!(image_mime("a.webp"), "image/webp");
        assert_eq!(image_mime("noext"), "image/png");
    }
}
