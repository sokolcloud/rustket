//! Personal access tokens tab (`users.getPersonalAccessTokens`,
//! `users.generatePersonalAccessToken`, `users.regeneratePersonalAccessToken`,
//! `users.removePersonalAccessToken`; the last three may ask for 2FA).

use iced::widget::{column, container, row, text, toggler};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::permissions;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL, font_medium, font_mono,
};
use rustket_ui::widget::{
    BannerKind, ChipKind, above, banner, chip, danger_button, dialog, divider, ghost_button,
    loading_placeholder, primary_button, secondary_button, text_field,
};

use super::api::{self, PersonalToken};
use super::two_factor::{self, Op};
use super::{Message, section};
use crate::component::{Action, Ctx, ViewCtx};
use crate::error::Res;

pub const PERMISSION: &str = "create-personal-access-tokens";

#[derive(Debug, Clone)]
pub enum Msg {
    Load,
    Loaded(Res<Vec<PersonalToken>>),
    Name(String),
    Bypass(bool),
    Generate,
    AskRegenerate(String),
    Regenerate(String),
    AskRemove(String),
    Remove(String),
    CancelRemove,
    CloseShown,
}

#[derive(Debug, Default)]
pub struct Tokens {
    pub list: Option<Vec<PersonalToken>>,
    pub loading: bool,
    pub error: Option<String>,
    pub name: String,
    pub bypass: bool,
    /// Operation in flight (token name).
    pub busy: Option<String>,
    /// A freshly (re)generated token, shown once: (name, token).
    pub shown: Option<(String, String)>,
    pub confirm_remove: Option<String>,
    pub confirm_regenerate: Option<String>,
}

impl Tokens {
    pub fn load(&mut self, ctx: &Ctx<'_>) -> Action<Message> {
        if !permissions::has_permission(ctx.workspace, PERMISSION, None) {
            return Action::none();
        }
        self.loading = true;
        Action::task(ctx.rest_task(
            |rest| async move { api::list_tokens(&rest).await },
            |r| Message::Tokens(Msg::Loaded(r)),
        ))
    }

    /// A new name is valid when non-empty and not taken.
    pub fn name_error(&self) -> Option<String> {
        let n = self.name.trim();
        self.list
            .as_ref()
            .is_some_and(|l| l.iter().any(|t| t.name == n))
            .then(|| t!("account.token_name_taken").into_owned())
    }

    pub fn update(&mut self, msg: Msg, ctx: &mut Ctx<'_>) -> Action<Message> {
        match msg {
            Msg::Load => self.load(ctx),
            Msg::Loaded(r) => {
                self.loading = false;
                match r {
                    Ok(mut l) => {
                        l.sort_by_key(|a| a.name.to_lowercase());
                        self.list = Some(l);
                        self.error = None;
                    }
                    Err(e) => self.error = Some(e.user_text()),
                }
                Action::none()
            }
            Msg::Name(s) => {
                self.name = s;
                Action::none()
            }
            Msg::Bypass(b) => {
                self.bypass = b;
                Action::none()
            }
            Msg::Generate => {
                let name = self.name.trim().to_owned();
                if name.is_empty() || self.busy.is_some() || self.name_error().is_some() {
                    return Action::none();
                }
                self.busy = Some(name.clone());
                self.error = None;
                Action::task(two_factor::run(
                    ctx,
                    Op::GenerateToken {
                        name,
                        bypass: self.bypass,
                    },
                    None,
                ))
            }
            Msg::AskRegenerate(name) => {
                self.confirm_remove = None;
                self.confirm_regenerate = Some(name);
                Action::none()
            }
            Msg::Regenerate(name) => {
                if self.busy.is_some() {
                    return Action::none();
                }
                self.confirm_regenerate = None;
                self.busy = Some(name.clone());
                self.error = None;
                Action::task(two_factor::run(ctx, Op::RegenerateToken(name), None))
            }
            Msg::AskRemove(name) => {
                self.confirm_regenerate = None;
                self.confirm_remove = Some(name);
                Action::none()
            }
            Msg::CancelRemove => {
                self.confirm_remove = None;
                self.confirm_regenerate = None;
                Action::none()
            }
            Msg::Remove(name) => {
                if self.busy.is_some() {
                    return Action::none();
                }
                self.confirm_remove = None;
                self.busy = Some(name.clone());
                self.error = None;
                Action::task(two_factor::run(ctx, Op::RemoveToken(name), None))
            }
            Msg::CloseShown => {
                self.shown = None;
                Action::none()
            }
        }
    }

    /// A guarded token call finished.
    pub fn done(&mut self, r: Result<TokenDone, String>, ctx: &Ctx<'_>) -> Action<Message> {
        self.busy = None;
        match r {
            Ok(TokenDone::Token { name, token }) => {
                if self.name.trim() == name {
                    self.name.clear();
                    self.bypass = false;
                }
                self.shown = Some((name, token));
                self.load(ctx)
            }
            Ok(TokenDone::Removed(_)) => self.load(ctx),
            Ok(TokenDone::Cancelled) => Action::none(),
            Err(e) => {
                self.error = Some(e);
                Action::none()
            }
        }
    }

    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let k = |m| Message::Tokens(m);
        if !permissions::has_permission(ctx.workspace, PERMISSION, None) {
            return column![banner(
                BannerKind::Info,
                t!("account.tokens_not_allowed"),
                None
            )]
            .max_width(640)
            .into();
        }
        let mut body = column![].spacing(SPACE_4);

        let name_err = self.name_error();
        let can_generate =
            !self.name.trim().is_empty() && name_err.is_none() && self.busy.is_none();
        let generating = self.busy.as_deref().is_some_and(|b| b == self.name.trim());
        let form = column![
            text(t!("account.tokens_help"))
                .size(TEXT_SMALL)
                .style(style::text_secondary),
            row![
                text_field(t!("account.token_name"), &self.name, move |s| k(Msg::Name(
                    s
                )))
                .placeholder(t!("account.token_name_placeholder"))
                .error(name_err)
                .on_submit(k(Msg::Generate))
                .width(Length::Fixed(280.0)),
                container(
                    primary_button(
                        t!("account.token_generate"),
                        can_generate.then_some(k(Msg::Generate))
                    )
                    .icon(Icon::Plus)
                    .loading(generating)
                )
                .padding(iced::Padding {
                    top: 22.0,
                    ..Default::default()
                }),
            ]
            .spacing(SPACE_2),
            toggler(self.bypass)
                .label(t!("account.token_bypass"))
                .text_size(TEXT_BODY)
                .on_toggle(move |b| k(Msg::Bypass(b)))
                .style(style::toggler),
            text(t!("account.token_bypass_help"))
                .size(TEXT_CAPTION)
                .style(style::text_hint),
        ]
        .spacing(SPACE_3);
        body = body.push(section(t!("account.token_new").into_owned(), form.into()));

        let list: Element<'a, Message> = match &self.list {
            None if self.loading => loading_placeholder(None),
            None => column![].into(),
            Some(l) if l.is_empty() => text(t!("account.tokens_empty"))
                .size(TEXT_SMALL)
                .style(style::text_secondary)
                .into(),
            Some(l) => {
                let mut col = column![].spacing(SPACE_2);
                for (i, tok) in l.iter().enumerate() {
                    if i > 0 {
                        col = col.push(divider());
                    }
                    col = col.push(self.token_row(tok));
                }
                col.into()
            }
        };
        body = body.push(section(t!("account.tokens_list").into_owned(), list));
        // A failed request must not shift the name field (and its focus).
        let error = self
            .error
            .as_ref()
            .map(|e| banner(BannerKind::Error, e.clone(), None));
        above(error, SPACE_4, body).max_width(720).into()
    }

    fn token_row<'a>(&'a self, tok: &'a PersonalToken) -> Element<'a, Message> {
        let k = |m| Message::Tokens(m);
        let created = tok
            .created_at
            .map(crate::screens::directory::date_label)
            .unwrap_or_default();
        let mut meta = row![
            text(t!("account.token_created", date = created))
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
            text(format!("…{}", tok.last_token_part))
                .wrapping(iced::widget::text::Wrapping::None)
                .font(font_mono())
                .size(TEXT_CAPTION)
                .style(style::text_secondary),
        ]
        .spacing(SPACE_3)
        .align_y(Alignment::Center);
        if tok.bypass_two_factor {
            meta = meta.push(chip(t!("account.token_bypass_chip"), ChipKind::Warning));
        }
        let busy = self.busy.as_deref() == Some(tok.name.as_str());
        let idle = self.busy.is_none();
        let actions: Element<'a, Message> = if self.confirm_remove.as_deref() == Some(&tok.name) {
            row![
                secondary_button(t!("account.cancel"), Some(k(Msg::CancelRemove))),
                danger_button(
                    t!("account.token_remove"),
                    idle.then(|| k(Msg::Remove(tok.name.clone())))
                ),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into()
        } else if self.confirm_regenerate.as_deref() == Some(&tok.name) {
            row![
                secondary_button(t!("account.cancel"), Some(k(Msg::CancelRemove))),
                danger_button(
                    t!("account.token_regenerate"),
                    idle.then(|| k(Msg::Regenerate(tok.name.clone())))
                )
                .loading(busy),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into()
        } else {
            row![
                ghost_button(
                    t!("account.token_regenerate"),
                    idle.then(|| k(Msg::AskRegenerate(tok.name.clone())))
                )
                .icon(Icon::RefreshCw)
                .loading(busy),
                ghost_button(
                    t!("account.token_remove"),
                    idle.then(|| k(Msg::AskRemove(tok.name.clone())))
                )
                .icon(Icon::Trash2),
            ]
            .spacing(SPACE_1)
            .into()
        };
        let mut info = column![
            text(tok.name.as_str()).size(TEXT_BODY).font(font_medium()),
            meta
        ]
        .spacing(SPACE_1)
        .width(Length::Fill);
        let question = if self.confirm_remove.as_deref() == Some(&tok.name) {
            Some(t!("account.token_remove_confirm"))
        } else if self.confirm_regenerate.as_deref() == Some(&tok.name) {
            Some(t!("account.token_regenerate_confirm"))
        } else {
            None
        };
        if let Some(q) = question {
            info = info.push(text(q).size(TEXT_SMALL).style(style::text_danger));
        }
        row![info, actions,]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
            .into()
    }

    /// The token shown once after generating it.
    pub fn shown_dialog<'a>(&'a self, ctx: &ViewCtx<'a>) -> Option<Element<'a, Message>> {
        let (name, token) = self.shown.as_ref()?;
        let uid = ctx.workspace.user_id().unwrap_or_default();
        let k = |m| Message::Tokens(m);
        // The copy button sits on the label row so the value gets the whole
        // width and stays on one line.
        let value = |label: String, v: &'a str| -> Element<'a, Message> {
            column![
                row![
                    text(label)
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary)
                        .width(Length::Fill),
                    ghost_button(t!("account.copy"), Some(Message::Copy(v.to_owned())))
                        .icon(Icon::Copy),
                ]
                .align_y(Alignment::Center),
                container(
                    text(v)
                        .font(font_mono())
                        .size(TEXT_SMALL)
                        .wrapping(iced::widget::text::Wrapping::None),
                )
                .padding([SPACE_2, SPACE_3])
                .width(Length::Fill)
                .clip(true)
                .style(style::container_code_block),
            ]
            .spacing(SPACE_1)
            .into()
        };
        let body = column![
            banner(BannerKind::Warning, t!("account.token_shown_once"), None),
            value(t!("account.token_value").into_owned(), token.as_str()),
            value(t!("account.token_user_id").into_owned(), uid),
        ]
        .spacing(SPACE_3);
        Some(dialog(
            t!("account.token_shown_title", name = name.as_str()),
            body,
            vec![primary_button(t!("account.backup_codes_done"), Some(k(Msg::CloseShown))).into()],
            // Shown once: only "Done" closes it (no backdrop/Escape/close button).
            None,
        ))
    }
}

/// Outcome of a guarded token call, as the tab sees it.
#[derive(Debug, Clone)]
pub enum TokenDone {
    Token {
        name: String,
        token: String,
    },
    Removed(String),
    /// The 2FA prompt was cancelled.
    Cancelled,
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_names_are_refused() {
        let mut t = Tokens {
            list: Some(vec![PersonalToken {
                name: "ci".into(),
                created_at: None,
                last_token_part: "abc".into(),
                bypass_two_factor: false,
            }]),
            ..Default::default()
        };
        t.name = " ci ".into();
        assert!(t.name_error().is_some());
        t.name = "ci2".into();
        assert!(t.name_error().is_none());
    }
}
