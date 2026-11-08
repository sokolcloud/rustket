//! Status menu: the user's avatar in the sidebar header and its popover —
//! online/away/busy/invisible, the server's custom statuses, a custom status
//! text (with recently used texts), my account, preferences, logout. Status
//! changes go through `users.setStatus`. Owner: desktop agent. Strings:
//! `locales/status_menu.yml` (`status_menu.*`).

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Length, Subscription, Task, Theme};
use rocketchat::models::UserStatus;
use rust_i18n::t;
use rustket_core::Workspace;
use rustket_core::room_list::Avatar;
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    FONT_SEMIBOLD, SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, tokens,
};
use rustket_ui::widget::{
    AvatarShape, BannerKind, Placement, avatar, divider, ellipsis, focus_ring, menu_nav, popover,
    presence_dot, text_field,
};

use crate::component::{Action, Ctx, Effect, Nav, ViewCtx};
use crate::error::Res;
use crate::workspace::sidebar::presence;

/// Most recent custom status texts kept.
pub const MAX_RECENT: usize = rustket_core::config::MAX_RECENT_STATUS_TEXTS;
/// Longest status text the server accepts.
pub const MAX_TEXT: usize = 120;
pub const TEXT_INPUT_ID: &str = "status-menu-text";
const POPUP_WIDTH: f32 = 272.0;

#[derive(Debug, Clone)]
pub enum Message {
    Toggle(bool),
    SetStatus(UserStatus),
    /// A server-defined custom status (`custom-user-status.list` id).
    SetCustom(String),
    TextChanged(String),
    SubmitText,
    UseRecent(String),
    ClearText,
    /// `users.setStatus` finished with this status (`None`: text only) and
    /// text.
    StatusSet(Option<UserStatus>, Option<String>, Res<()>),
    Account,
    Settings,
    /// Administration (shown with any admin permission).
    Admin,
    Logout,
    LogoutEverywhere,
    RemoveServer,
    /// Omnichannel agent availability (`crate::omnichannel::status`).
    Omni(crate::omnichannel::status::Msg),
    /// An app action button (`crate::uikit::buttons::user_menu`).
    UiKit(Box<crate::uikit::UiKitMsg>),
}

#[derive(Debug, Default)]
pub struct StatusMenu {
    pub open: bool,
    /// Custom status text being edited.
    pub text: String,
    /// Recently used status texts, most recent first (a copy of
    /// `AppConfig::recent_status_texts`, which is persisted through
    /// `Effect::RecentStatusTexts`).
    pub recent: Vec<String>,
    /// A `users.setStatus` call is running.
    pub busy: bool,
}

/// Whether `Accounts_AllowInvisibleStatusOption` allows "invisible".
pub fn invisible_allowed(ws: &Workspace) -> bool {
    ws.settings()
        .bool_or("Accounts_AllowInvisibleStatusOption", true)
}

/// Whether `Accounts_AllowUserStatusMessageChange` allows status texts.
pub fn text_allowed(ws: &Workspace) -> bool {
    ws.settings()
        .bool_or("Accounts_AllowUserStatusMessageChange", true)
}

/// Statuses offered on this server, in menu order.
pub fn offered(ws: &Workspace) -> Vec<UserStatus> {
    let mut v = vec![UserStatus::Online, UserStatus::Away, UserStatus::Busy];
    if invisible_allowed(ws) {
        v.push(UserStatus::Offline);
    }
    v
}

/// The server's custom statuses this user may pick: none when status texts
/// are not allowed (a custom status is a text), no invisible ones when
/// invisible is not allowed.
pub fn offered_custom(ws: &Workspace) -> Vec<&rocketchat::models::CustomUserStatus> {
    if !text_allowed(ws) {
        return Vec::new();
    }
    let invisible = invisible_allowed(ws);
    ws.custom_statuses()
        .filter(|c| invisible || c.status_type != UserStatus::Offline)
        .collect()
}

pub fn status_label(s: UserStatus) -> String {
    match s {
        UserStatus::Online => t!("status_menu.online"),
        UserStatus::Away => t!("status_menu.away"),
        UserStatus::Busy => t!("status_menu.busy"),
        UserStatus::Offline => t!("status_menu.invisible"),
        UserStatus::Unknown => t!("status_menu.unknown"),
    }
    .into_owned()
}

fn my_text(ws: &Workspace) -> String {
    ws.me()
        .and_then(|m| m.status_text.clone())
        .unwrap_or_default()
}

/// Adds `text` to the front of `recent` (deduplicated, bounded).
pub fn remember(recent: &mut Vec<String>, text: &str) {
    let text = text.trim();
    if text.is_empty() {
        return;
    }
    recent.retain(|r| r != text);
    recent.insert(0, text.to_owned());
    recent.truncate(MAX_RECENT);
}

impl StatusMenu {
    pub fn new() -> Self {
        Self::default()
    }

    /// Calls `users.setStatus(status, text)`, updating the local state right away.
    fn set(
        &mut self,
        status: UserStatus,
        text: Option<String>,
        ctx: &mut Ctx<'_>,
    ) -> Action<Message> {
        if !offered(ctx.workspace).contains(&status) {
            return Action::effect(Effect::Toast(
                BannerKind::Error,
                t!("status_menu.failed").into_owned(),
                t!("status_menu.not_allowed").into_owned(),
            ));
        }
        let text = text.filter(|_| text_allowed(ctx.workspace));
        self.busy = true;
        self.open = false;
        let t2 = text.clone();
        Action::task(ctx.rest_task(
            move |rest| async move { rest.users_set_status(status, t2.as_deref()).await },
            move |r| Message::StatusSet(Some(status), text.clone(), r),
        ))
    }

    /// Changes only the status text: the server keeps the status the user
    /// chose, so a transient auto-away is not pinned.
    fn set_text(&mut self, text: String, ctx: &mut Ctx<'_>) -> Action<Message> {
        if !text_allowed(ctx.workspace) {
            return Action::none();
        }
        self.busy = true;
        self.open = false;
        let t2 = text.clone();
        Action::task(ctx.rest_task(
            move |rest| async move { rest.users_set_status_message(&t2).await },
            move |r| Message::StatusSet(None, Some(text.clone()), r),
        ))
    }

    pub fn update(&mut self, msg: Message, ctx: &mut Ctx<'_>) -> Action<Message> {
        self.recent = ctx.config.recent_status_texts.clone();
        let action = self.update_inner(msg, ctx);
        if self.recent != ctx.config.recent_status_texts {
            return action.with(Effect::RecentStatusTexts(self.recent.clone()));
        }
        action
    }

    fn update_inner(&mut self, msg: Message, ctx: &mut Ctx<'_>) -> Action<Message> {
        match msg {
            Message::Toggle(open) => {
                self.open = open;
                if open {
                    self.text = my_text(ctx.workspace);
                    let current = self.text.clone();
                    remember(&mut self.recent, &current);
                    if text_allowed(ctx.workspace) {
                        return Action::task(Self::focus());
                    }
                }
                Action::none()
            }
            Message::SetStatus(status) => {
                // A preset status clears the text (as the web client does).
                self.set(status, Some(String::new()), ctx)
            }
            Message::SetCustom(id) => {
                let Some(custom) = offered_custom(ctx.workspace)
                    .into_iter()
                    .find(|c| c.id == id)
                    .cloned()
                else {
                    return Action::none();
                };
                remember(&mut self.recent, &custom.name);
                self.set(custom.status_type, Some(custom.name), ctx)
            }
            Message::TextChanged(s) => {
                self.text = s.chars().take(MAX_TEXT).collect();
                Action::none()
            }
            Message::SubmitText => {
                let text = self.text.trim().to_owned();
                remember(&mut self.recent, &text);
                self.set_text(text, ctx)
            }
            Message::UseRecent(text) => {
                remember(&mut self.recent, &text);
                self.text = text.clone();
                self.set_text(text, ctx)
            }
            Message::ClearText => {
                self.text.clear();
                self.set_text(String::new(), ctx)
            }
            Message::StatusSet(status, text, r) => {
                self.busy = false;
                match r {
                    Ok(()) => {
                        let ws = &mut *ctx.workspace;
                        let status = status.unwrap_or_else(|| ws.my_status());
                        if let (Some(uid), username) = (
                            ws.user_id().map(str::to_owned),
                            ws.username().map(str::to_owned),
                        ) {
                            ws.apply_user_status(
                                &uid,
                                username.as_deref(),
                                status,
                                text.as_deref(),
                            );
                        }
                        Action::none()
                    }
                    Err(f) => Action::effect(Effect::Toast(
                        BannerKind::Error,
                        t!("status_menu.failed").into_owned(),
                        f.user_text(),
                    )),
                }
            }
            Message::Account => {
                self.open = false;
                Action::effect(Effect::Navigate(Nav::Account))
            }
            Message::Settings => {
                self.open = false;
                Action::effect(Effect::Navigate(Nav::Settings))
            }
            Message::Admin => {
                self.open = false;
                Action::effect(Effect::Navigate(Nav::Admin))
            }
            Message::Logout => {
                self.open = false;
                Action::effect(Effect::Logout { everywhere: false })
            }
            Message::LogoutEverywhere => {
                self.open = false;
                Action::effect(Effect::Logout { everywhere: true })
            }
            Message::RemoveServer => {
                self.open = false;
                Action::effect(Effect::RemoveServer)
            }
            Message::Omni(m) => crate::omnichannel::status::update(m, ctx).map(Message::Omni),
            Message::UiKit(m) => {
                self.open = false;
                Action::effect(Effect::UiKit(m))
            }
        }
    }

    /// Focuses the status text field (after opening).
    pub fn focus<M: Send + 'static>() -> Task<M> {
        iced::widget::operation::focus(TEXT_INPUT_ID)
    }

    pub fn subscription(&self) -> Subscription<Message> {
        Subscription::none()
    }

    /// The header trigger (avatar, name) with the popover.
    pub fn view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let ws = ctx.workspace;
        let username = ws.username().unwrap_or_default().to_owned();
        let name = ws
            .me()
            .and_then(|m| m.name.clone())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| username.clone());
        let src = ctx.images.avatar(
            ctx.server_id,
            ctx.server,
            &Avatar::User {
                username: username.clone(),
            },
            &name,
        );
        let status = ws.my_status();
        let status_text = my_text(ws);
        let subtitle = if status_text.is_empty() {
            format!("@{username}")
        } else {
            status_text.clone()
        };
        let me_avatar = avatar(src.clone(), 32.0, AvatarShape::Rounded)
            .presence(Some(presence(status)))
            .ring_with(|t| t.surface_sidebar);
        let trigger = button(
            row![
                me_avatar,
                column![
                    ellipsis(name.clone()).size(TEXT_BODY).font(FONT_SEMIBOLD),
                    ellipsis(subtitle)
                        .size(TEXT_CAPTION)
                        .style(|theme: &Theme| Some(tokens(theme).text_secondary)),
                ]
                .width(Length::Fill),
                crate::omnichannel::status::indicator(ws),
                icon::sized(Icon::ChevronDown, 14.0),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center),
        )
        .padding([4, 6])
        .width(Length::Fill)
        .style(style::button_ghost)
        .on_press(Message::Toggle(!self.open));
        // Tab reaches the trigger; Enter/Space opens the menu.
        let trigger = focus_ring(trigger, Some(Message::Toggle(!self.open)));
        let popup = self.popup(ws, name, username, src, status);
        popover(
            trigger,
            popup,
            self.open,
            Placement::BelowStart,
            Message::Toggle(false),
        )
    }

    fn popup<'a>(
        &'a self,
        ws: &'a Workspace,
        name: String,
        username: String,
        src: rustket_ui::widget::AvatarSource,
        status: UserStatus,
    ) -> Element<'a, Message> {
        let enabled = !self.busy;
        let labelled = |ic: Icon, label: String| -> Element<'a, Message> {
            row![icon::sized(ic, 16.0), ellipsis(label).size(TEXT_BODY)]
                .spacing(SPACE_3)
                .align_y(Alignment::Center)
                .into()
        };
        let caption = |s: String| -> Element<'a, Message> {
            container(text(s).size(TEXT_CAPTION).style(style::text_hint))
                .padding([SPACE_1, SPACE_2])
                .into()
        };
        let mut rows = Rows::default();
        rows.plain(
            row![
                avatar(src, 40.0, AvatarShape::Rounded),
                column![
                    ellipsis(name).size(TEXT_BODY).font(FONT_SEMIBOLD),
                    ellipsis(format!("@{username}"))
                        .size(TEXT_CAPTION)
                        .style(|theme: &Theme| Some(tokens(theme).text_secondary)),
                ]
                .width(Length::Fill),
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center)
            .padding([SPACE_2, SPACE_2]),
        );
        rows.plain(divider());
        rows.plain(caption(t!("status_menu.status").into_owned()));
        for s in offered(ws) {
            let mut r = row![
                container(presence_dot(presence(s), 10.0)).padding([0, 3]),
                ellipsis(status_label(s))
                    .size(TEXT_BODY)
                    .width(Length::Fill),
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center);
            if s == status {
                r = r.push(icon::sized(Icon::Check, 14.0));
            }
            rows.item(r, enabled.then_some(Message::SetStatus(s)), false);
        }
        if let Some(e) = crate::omnichannel::status::menu_entry(ws, enabled, Message::Omni) {
            rows.plain(divider());
            rows.plain(e);
        }
        let customs = offered_custom(ws);
        if !customs.is_empty() {
            rows.plain(caption(t!("status_menu.custom_statuses").into_owned()));
            for c in customs {
                rows.item(
                    row![
                        container(presence_dot(presence(c.status_type), 10.0)).padding([0, 3]),
                        ellipsis(c.name.clone()).size(TEXT_BODY),
                    ]
                    .spacing(SPACE_3)
                    .align_y(Alignment::Center),
                    enabled.then(|| Message::SetCustom(c.id.clone())),
                    false,
                );
            }
        }
        if text_allowed(ws) {
            rows.plain(divider());
            rows.plain(caption(t!("status_menu.status_text").into_owned()));
            let mut field = row![
                text_field("", &self.text, Message::TextChanged)
                    .placeholder(t!("status_menu.text_placeholder"))
                    .on_submit(Message::SubmitText)
                    .id(TEXT_INPUT_ID)
                    .disabled(!enabled)
            ]
            .spacing(SPACE_1)
            .align_y(Alignment::Center);
            if !my_text(ws).is_empty() {
                field = field.push(rustket_ui::widget::icon_button(
                    Icon::X,
                    enabled.then_some(Message::ClearText),
                    t!("status_menu.clear_text"),
                ));
            }
            rows.plain(container(field).padding([SPACE_1, SPACE_2]));
            let current = my_text(ws);
            for r in self.recent.iter().filter(|r| **r != current) {
                rows.item(
                    labelled(Icon::MessageCircle, r.clone()),
                    enabled.then(|| Message::UseRecent(r.clone())),
                    false,
                );
            }
        }
        let apps = crate::uikit::buttons::user_menu(ws);
        if !apps.is_empty() {
            rows.plain(divider());
            rows.plain(caption(t!("uikit.apps").into_owned()));
            for (label, m, danger) in apps {
                rows.item(
                    labelled(crate::uikit::buttons::COMPOSER_ICON, label),
                    Some(Message::UiKit(Box::new(m))),
                    danger,
                );
            }
        }
        rows.plain(divider());
        rows.item(
            labelled(Icon::User, t!("status_menu.account").into_owned()),
            Some(Message::Account),
            false,
        );
        rows.item(
            labelled(Icon::Settings, t!("status_menu.settings").into_owned()),
            Some(Message::Settings),
            false,
        );
        if rustket_core::admin::can_view_admin(ws) {
            rows.item(
                labelled(Icon::Shield, t!("admin.entry").into_owned()),
                Some(Message::Admin),
                false,
            );
        }
        rows.plain(divider());
        rows.item(
            labelled(Icon::LogOut, t!("status_menu.logout").into_owned()),
            Some(Message::Logout),
            false,
        );
        rows.item(
            labelled(
                Icon::LogOut,
                t!("status_menu.logout_everywhere").into_owned(),
            ),
            Some(Message::LogoutEverywhere),
            false,
        );
        rows.item(
            labelled(Icon::Trash2, t!("status_menu.remove_server").into_owned()),
            Some(Message::RemoveServer),
            true,
        );
        let col = column(rows.els).spacing(0).padding(SPACE_1);
        container(
            scrollable(menu_nav(col, rows.acts))
                .style(style::scrollable)
                .height(Length::Shrink),
        )
        .width(POPUP_WIDTH)
        .max_height(560)
        .style(style::container_popover)
        .into()
    }
}

/// Rows of the popup and what each one selects with the keyboard
/// (`menu_nav`: Up/Down/Home/End, Enter/Space).
#[derive(Default)]
struct Rows<'a> {
    els: Vec<Element<'a, Message>>,
    acts: Vec<Option<Message>>,
}

impl<'a> Rows<'a> {
    fn plain(&mut self, el: impl Into<Element<'a, Message>>) {
        self.els.push(el.into());
        self.acts.push(None);
    }

    /// A menu entry (disabled without `msg`).
    fn item(
        &mut self,
        content: impl Into<Element<'a, Message>>,
        msg: Option<Message>,
        danger: bool,
    ) {
        self.els.push(
            button(content.into())
                .width(Length::Fill)
                .padding([6, 8])
                .style(style::button_menu_item(danger))
                .on_press_maybe(msg.clone())
                .into(),
        );
        self.acts.push(msg);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::images::Images;
    use rocketchat::ServerUrl;
    use rustket_core::ServerId;
    use rustket_core::config::AppConfig;
    use serde_json::json;

    struct Fx {
        sid: ServerId,
        url: ServerUrl,
        ws: Workspace,
        images: Images,
        config: AppConfig,
    }

    impl Fx {
        fn new() -> Self {
            let mut ws = Workspace::new();
            ws.apply_me(
                serde_json::from_value(json!({
                    "_id": "me", "username": "me", "status": "online", "statusText": "Lunch"
                }))
                .unwrap(),
            );
            Self {
                sid: ServerId("s".into()),
                url: ServerUrl::parse("http://localhost:3000").unwrap(),
                ws,
                images: Images::new(None),
                config: AppConfig::default(),
            }
        }

        fn ctx(&mut self) -> Ctx<'_> {
            Ctx {
                server_id: &self.sid,
                server: &self.url,
                workspace: &mut self.ws,
                session: None,
                images: &self.images,
                config: &self.config,
            }
        }
    }

    #[test]
    fn recent_texts_are_deduplicated_and_bounded() {
        let mut r = Vec::new();
        for t in ["a", "b", "a", " ", "c", "d", "e", "f"] {
            remember(&mut r, t);
        }
        assert_eq!(r, ["f", "e", "d", "c", "a"]);
    }

    #[test]
    fn opening_prefills_the_text_and_entries_close_it() {
        let mut fx = Fx::new();
        let mut m = StatusMenu::new();
        m.update(Message::Toggle(true), &mut fx.ctx());
        assert!(m.open);
        assert_eq!(m.text, "Lunch");
        assert_eq!(m.recent, ["Lunch"]);
        let a = m.update(Message::Settings, &mut fx.ctx());
        assert!(!m.open);
        // The recent list lives in the config (persisted by the app).
        assert!(m.recent.is_empty());
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::Navigate(Nav::Settings)]
        ));
        let a = m.update(Message::LogoutEverywhere, &mut fx.ctx());
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::Logout { everywhere: true }]
        ));
    }

    #[test]
    fn recent_texts_are_persisted_through_the_config() {
        let mut fx = Fx::new();
        fx.config.recent_status_texts = vec!["Old".into()];
        let mut m = StatusMenu::new();
        let a = m.update(Message::Toggle(true), &mut fx.ctx());
        let saved = a.effects.iter().find_map(|e| match e {
            Effect::RecentStatusTexts(v) => Some(v.clone()),
            _ => None,
        });
        assert_eq!(saved.unwrap(), ["Lunch", "Old"]);
        // The app stores them; the next open starts from the config.
        fx.config.recent_status_texts = vec!["Lunch".into(), "Old".into()];
        let mut m = StatusMenu::new();
        let a = m.update(Message::Toggle(true), &mut fx.ctx());
        assert!(a.effects.is_empty());
        assert_eq!(m.recent, ["Lunch", "Old"]);
        m.text = "Away for a bit".into();
        let a = m.update(Message::SubmitText, &mut fx.ctx());
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::RecentStatusTexts(v)] if v[0] == "Away for a bit" && v.len() == 3
        ));
    }

    #[test]
    fn server_settings_limit_the_choices() {
        let mut fx = Fx::new();
        assert_eq!(offered(&fx.ws).len(), 4);
        fx.ws.apply_setting(
            rustket_core::workspace::Change::Inserted,
            setting("Accounts_AllowInvisibleStatusOption", false),
        );
        assert_eq!(
            offered(&fx.ws),
            [UserStatus::Online, UserStatus::Away, UserStatus::Busy]
        );
        let mut m = StatusMenu::new();
        let a = m.update(Message::SetStatus(UserStatus::Offline), &mut fx.ctx());
        assert!(!m.busy, "invisible is refused locally");
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::Toast(BannerKind::Error, ..)]
        ));
        m.update(Message::SetStatus(UserStatus::Busy), &mut fx.ctx());
        assert!(m.busy);
    }

    #[test]
    fn custom_statuses_follow_the_server_settings() {
        let mut fx = Fx::new();
        for (id, ty) in [("c1", "busy"), ("c2", "offline")] {
            fx.ws.apply_custom_status(
                rustket_core::workspace::Change::Inserted,
                serde_json::from_value(json!({"_id": id, "name": id, "statusType": ty})).unwrap(),
            );
        }
        assert_eq!(offered_custom(&fx.ws).len(), 2);
        fx.ws.apply_setting(
            rustket_core::workspace::Change::Inserted,
            setting("Accounts_AllowInvisibleStatusOption", false),
        );
        let ids: Vec<_> = offered_custom(&fx.ws)
            .iter()
            .map(|c| c.id.clone())
            .collect();
        assert_eq!(ids, ["c1"]);
        let mut m = StatusMenu::new();
        m.update(Message::SetCustom("c2".into()), &mut fx.ctx());
        assert!(!m.busy);
        fx.ws.apply_setting(
            rustket_core::workspace::Change::Inserted,
            setting("Accounts_AllowUserStatusMessageChange", false),
        );
        assert!(offered_custom(&fx.ws).is_empty());
    }

    #[test]
    fn text_changes_keep_the_status_and_auto_away_is_not_pinned() {
        let mut fx = Fx::new();
        // Auto-away set the connection away; the user changes only the text.
        fx.ws
            .apply_user_status("me", Some("me"), UserStatus::Away, Some("Lunch"));
        let mut m = StatusMenu::new();
        m.text = "Back".into();
        let a = m.update(Message::SubmitText, &mut fx.ctx());
        // Only the recent list is persisted; no toast or navigation.
        assert!(m.busy);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::RecentStatusTexts(v)] if v == &["Back"]
        ));
        m.update(
            Message::StatusSet(None, Some("Back".into()), Ok(())),
            &mut fx.ctx(),
        );
        assert_eq!(fx.ws.my_status(), UserStatus::Away, "status untouched");
        assert_eq!(my_text(&fx.ws), "Back");
    }

    fn setting(id: &str, value: bool) -> rocketchat::models::Setting {
        serde_json::from_value(json!({"_id": id, "value": value})).unwrap()
    }

    #[test]
    fn status_result_updates_the_workspace_or_toasts() {
        let mut fx = Fx::new();
        let mut m = StatusMenu::new();
        m.busy = true;
        m.update(
            Message::StatusSet(Some(UserStatus::Busy), Some("Focus".into()), Ok(())),
            &mut fx.ctx(),
        );
        assert!(!m.busy);
        assert_eq!(fx.ws.my_status(), UserStatus::Busy);
        assert_eq!(my_text(&fx.ws), "Focus");
        let err = crate::error::Failure::from(rocketchat::Error::Unauthorized {
            message: None,
            code: None,
        });
        let a = m.update(
            Message::StatusSet(Some(UserStatus::Away), None, Err(err)),
            &mut fx.ctx(),
        );
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::Toast(BannerKind::Error, ..)]
        ));
        assert_eq!(fx.ws.my_status(), UserStatus::Busy);
    }

    #[test]
    fn custom_status_uses_its_type_and_name() {
        let mut fx = Fx::new();
        fx.ws.apply_custom_status(
            rustket_core::workspace::Change::Inserted,
            serde_json::from_value(
                json!({"_id": "c1", "name": "In a meeting", "statusType": "busy"}),
            )
            .unwrap(),
        );
        let mut m = StatusMenu::new();
        m.update(Message::SetCustom("c1".into()), &mut fx.ctx());
        assert!(m.busy);
        assert_eq!(m.recent, ["In a meeting"]);
        m.busy = false;
        m.update(Message::SetCustom("nope".into()), &mut fx.ctx());
        assert!(!m.busy);
    }
}
