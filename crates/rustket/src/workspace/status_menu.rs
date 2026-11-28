//! Status menu: the user's avatar in the sidebar header and its popover —
//! online/away/busy/invisible, pausing notifications (`crate::quiet`), the
//! server's custom statuses, a custom status text (with recently used texts
//! and "Clear after"), my account, preferences, logout. Status changes go
//! through `users.setStatus`; "Clear after" goes with it as `expiresAt` where
//! the server clears statuses itself (Rocket.Chat 8.6+), so web and mobile
//! show it too, and is done by the app on older servers
//! (`Workspace::status_expiry_on_server`). Owner: desktop agent. Strings:
//! `locales/status_menu.yml` (`status_menu.*`), `locales/quiet.yml`.

use iced::widget::{button, column, container, pick_list, row, scrollable, text, text_input};
use iced::{Alignment, Element, Length, Subscription, Task, Theme};
use rocketchat::RcDate;
use rocketchat::models::UserStatus;
use rust_i18n::t;
use rustket_core::config::AppConfig;
use rustket_core::quiet::{ClearAfter, PausePreset, StatusExpiry};
use rustket_core::room_list::Avatar;
use rustket_core::{ServerId, Workspace};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION, font_semibold, tokens,
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
    /// The keyboard shortcuts cheat sheet.
    Shortcuts,
    /// Settings → Help (the indicators reference).
    Help,
    /// Administration (shown with any admin permission).
    Admin,
    Logout,
    LogoutEverywhere,
    RemoveServer,
    /// Omnichannel agent availability (`crate::omnichannel::status`).
    Omni(crate::omnichannel::status::Msg),
    /// An app action button (`crate::uikit::buttons::user_menu`).
    UiKit(Box<crate::uikit::UiKitMsg>),
    /// Show or hide the pause presets.
    TogglePause,
    Pause(PausePreset),
    PauseCustom,
    Resume,
    /// "Clear after" of the next status set here.
    ClearAfter(ClearChoice),
    ClearDate(String),
    ClearTime(String),
    /// The current status no longer clears itself.
    CancelExpiry,
}

/// A "Clear after" option: a preset, the current status's expiry, or a
/// custom date and time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClearChoice {
    Preset(ClearAfter),
    /// When the current status clears (UTC ms; set here, in another client,
    /// or by the app), with its label ("Until Nov 28 00:00"): kept for the
    /// next status set here. Picked, it follows the current expiry (another
    /// client may change it while the menu is open).
    Until(i64, String),
    Custom,
}

impl std::fmt::Display for ClearChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Preset(c) => f.write_str(&crate::quiet::clear_label(*c)),
            Self::Until(_, label) => f.write_str(label),
            Self::Custom => f.write_str(&t!("later.preset_custom")),
        }
    }
}

impl ClearChoice {
    /// The choices, with the current expiry (`until`) after "Never".
    fn all(until: Option<&ClearChoice>) -> Vec<Self> {
        let mut v: Vec<Self> = ClearAfter::ALL.into_iter().map(Self::Preset).collect();
        if let Some(u) = until {
            v.insert(1, u.clone());
        }
        v.push(Self::Custom);
        v
    }

    /// The choice after this one in `all` (the last wraps).
    fn next(&self, all: &[ClearChoice]) -> Self {
        let i = all.iter().position(|c| c == self).map_or(0, |i| i + 1);
        all.get(i).cloned().unwrap_or_default()
    }

    /// "Until …" for an expiry at `at` (UTC ms).
    fn until(at: i64, clock_24h: bool) -> Self {
        let when = crate::format::date_time(at, chrono::Local::now(), clock_24h);
        Self::Until(at, t!("quiet.clear_until", time = when).into_owned())
    }
}

/// [`ClearChoice::Until`] of the current expiry, if it is still ahead.
fn live_until(ws: &Workspace, config: &AppConfig, sid: &ServerId) -> Option<ClearChoice> {
    let now = chrono::Utc::now().timestamp_millis();
    current_expiry(ws, config, sid, now)
        .filter(|(at, _)| *at > now)
        .map(|(at, _)| ClearChoice::until(at, config.ui.clock_24h))
}

/// When the current status clears (UTC ms): the server's `statusExpiresAt`
/// if it is ahead of `now_ms`, else the one the app keeps
/// (`ServerEntry::status_expiry`, servers before 8.6); `true` for the
/// server's.
pub fn current_expiry(
    ws: &Workspace,
    config: &AppConfig,
    sid: &ServerId,
    now_ms: i64,
) -> Option<(i64, bool)> {
    if let Some(at) = ws.my_status_expiry(now_ms) {
        return Some((at.timestamp_millis(), true));
    }
    config
        .server(sid)
        .and_then(|e| e.status_expiry.as_ref())
        .filter(|e| Some(e.user_id.as_str()) == ws.user_id())
        .map(|e| (e.at, false))
}

#[derive(Debug, Default)]
pub struct StatusMenu {
    pub open: bool,
    /// "Remove workspace" is not offered: the enterprise policy keeps the
    /// server (managed, or adding and removing is off). Set by the app.
    pub remove_refused: bool,
    /// Custom status text being edited.
    pub text: String,
    /// Recently used status texts, most recent first (a copy of
    /// `AppConfig::recent_status_texts`, which is persisted through
    /// `Effect::RecentStatusTexts`).
    pub recent: Vec<String>,
    /// A `users.setStatus` call is running.
    pub busy: bool,
    /// The pause presets are shown.
    pub pause_open: bool,
    /// "Clear after" of the next status set from the open menu (a status
    /// set with the menu closed, from the tray, does not clear). Opens on
    /// the current status's expiry, if any (as the web client's status
    /// dialog does), which a text edit keeps and a status pick drops.
    pub clear: ClearChoice,
    /// `clear` was picked in the open menu (not preselected on opening).
    clear_picked: bool,
    pub clear_date: String,
    pub clear_time: String,
    /// The custom clear time is invalid or past.
    pub clear_error: bool,
    /// The expiry the app keeps for the running call (`Some(None)`: none,
    /// drops an old one).
    pending_expiry: Option<Option<StatusExpiry>>,
    /// `pending_expiry` is the current one kept with a new text.
    pending_expiry_kept: bool,
    /// The `expiresAt` the running call sent (UTC ms), recorded on success
    /// (`None`: the server does not clear statuses, nothing to record).
    pending_server_expiry: Option<Option<i64>>,
}

impl Default for ClearChoice {
    fn default() -> Self {
        Self::Preset(ClearAfter::Never)
    }
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

/// `users.setStatus` arguments (status, text; `None`: kept) that set the
/// current status again, which drops its server expiry: the chosen status
/// alone, so the text stays without the text permission
/// (`Accounts_AllowUserStatusMessageChange` refuses any non-empty text);
/// the text alone when the chosen status is not known (the server keeps
/// `statusDefault`).
fn keep_status_args(ws: &Workspace) -> (Option<UserStatus>, Option<String>) {
    match ws.my_chosen_status() {
        Some(st) => (Some(st), None),
        None => (None, Some(my_text(ws))),
    }
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

    /// When a status set now clears itself ("Clear after"); `Err` for an
    /// invalid or past custom time.
    /// `current`: when the current status clears (for
    /// [`ClearChoice::Until`]; `None`: it no longer does).
    fn clear_at(&mut self, current: Option<i64>) -> Result<Option<i64>, ()> {
        let now = chrono::Local::now();
        match &self.clear {
            ClearChoice::Preset(c) => Ok(c.at(&now).map(|t| t.timestamp_millis())),
            ClearChoice::Until(..) => Ok(current.filter(|at| *at > now.timestamp_millis())),
            ClearChoice::Custom => {
                let at = crate::uikit::elements::parse_date(&self.clear_date)
                    .and_then(|d| chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
                    .zip(crate::uikit::elements::parse_time(&self.clear_time))
                    .and_then(|(d, (h, m))| {
                        rustket_core::reminders::custom_at(d, h, m, &chrono::Local)
                    })
                    .filter(|at| *at > now);
                self.clear_error = at.is_none();
                at.map(|t| Some(t.timestamp_millis())).ok_or(())
            }
        }
    }

    /// [`Self::clear_at`] of the open menu; a status set with the menu
    /// closed (the tray) does not clear, whatever was picked there last.
    fn menu_clear_at(&mut self, ctx: &Ctx<'_>) -> Result<Option<i64>, ()> {
        if !self.open {
            return Ok(None);
        }
        let now = chrono::Utc::now().timestamp_millis();
        let current =
            current_expiry(ctx.workspace, ctx.config, ctx.server_id, now).map(|(at, _)| at);
        self.clear_at(current)
    }

    /// The expiry of `status` with `text` set now; `None` when it does not
    /// clear (or there is nothing to clear: online without a text).
    /// `restore` is the status before (the app maps it to a pause's held
    /// status and drops it when it is `status`, `quiet::expiry_for_hold`).
    fn expiry(
        &self,
        at: Option<i64>,
        status: UserStatus,
        text: &str,
        restore: Option<UserStatus>,
        ws: &Workspace,
    ) -> Option<StatusExpiry> {
        let at = at?;
        if status == UserStatus::Online && text.is_empty() {
            return None;
        }
        Some(StatusExpiry {
            user_id: ws.user_id()?.to_owned(),
            at,
            status,
            text: text.to_owned(),
            restore,
        })
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
        // The current expiry belongs to the current status: a preset or
        // custom status picked over it does not inherit it (the web menu's
        // statuses send no expiry either); only a clear-after picked here
        // goes with it, "Until …" too.
        let at = if matches!(self.clear, ClearChoice::Until(..)) && !self.clear_picked {
            None
        } else {
            let Ok(at) = self.menu_clear_at(ctx) else {
                return Action::none();
            };
            at
        };
        let text = text.filter(|_| text_allowed(ctx.workspace));
        let shown = text.clone().unwrap_or_else(|| my_text(ctx.workspace));
        let before = ctx
            .workspace
            .my_chosen_status()
            .unwrap_or_else(|| ctx.workspace.my_status());
        let expiry = self.expiry(at, status, &shown, Some(before), ctx.workspace);
        let expires = self.plan_expiry(expiry, ctx.workspace);
        self.busy = true;
        self.open = false;
        let t2 = text.clone();
        Action::task(ctx.rest_task(
            move |rest| async move { rest.users_set_status(status, t2.as_deref(), expires).await },
            move |r| Message::StatusSet(Some(status), text.clone(), r),
        ))
    }

    /// Records how the status set now clears: sent as `expiresAt` where the
    /// server clears statuses (returned), else kept by the app. Either way
    /// the other one is dropped.
    fn plan_expiry(&mut self, expiry: Option<StatusExpiry>, ws: &Workspace) -> Option<RcDate> {
        self.pending_expiry_kept = false;
        if ws.status_expiry_on_server() {
            let at = expiry.map(|e| e.at);
            self.pending_expiry = Some(None);
            self.pending_server_expiry = Some(at);
            at.and_then(RcDate::from_millis)
        } else {
            self.pending_expiry = Some(expiry);
            self.pending_server_expiry = None;
            None
        }
    }

    /// The app's expiry of the current status when the open menu keeps it
    /// ([`ClearChoice::Until`] and no server expiry).
    fn app_expiry_kept(&self, ctx: &Ctx<'_>) -> Option<StatusExpiry> {
        if !self.open || !matches!(self.clear, ClearChoice::Until(..)) {
            return None;
        }
        let now = chrono::Utc::now().timestamp_millis();
        if ctx.workspace.my_status_expiry(now).is_some() {
            return None;
        }
        ctx.config
            .server(ctx.server_id)
            .and_then(|e| e.status_expiry.clone())
            .filter(|e| Some(e.user_id.as_str()) == ctx.workspace.user_id() && e.at > now)
    }

    /// Changes only the status text: the server keeps the status the user
    /// chose, so a transient auto-away is not pinned.
    fn set_text(&mut self, text: String, ctx: &mut Ctx<'_>) -> Action<Message> {
        if !text_allowed(ctx.workspace) {
            return Action::none();
        }
        let Ok(at) = self.menu_clear_at(ctx) else {
            return Action::none();
        };
        // The current expiry kept with a new text: the app's (servers
        // before 8.6) still restores the status it would have restored.
        let kept = self.app_expiry_kept(ctx);
        let (status, restore) = kept.as_ref().map_or_else(
            || (ctx.workspace.my_status(), None),
            |e| (e.status, e.restore),
        );
        let expiry = self.expiry(at, status, &text, restore, ctx.workspace);
        let expires = self.plan_expiry(expiry, ctx.workspace);
        self.pending_expiry_kept = kept.is_some();
        self.busy = true;
        self.open = false;
        let t2 = text.clone();
        Action::task(ctx.rest_task(
            move |rest| async move { rest.users_set_status_message(&t2, expires).await },
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
                    self.pause_open = false;
                    self.clear =
                        live_until(ctx.workspace, ctx.config, ctx.server_id).unwrap_or_default();
                    self.clear_picked = false;
                    self.clear_error = false;
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
                let expiry = self.pending_expiry.take();
                let kept = std::mem::take(&mut self.pending_expiry_kept);
                let server_expiry = self.pending_server_expiry.take();
                match r {
                    Ok(()) => {
                        // Before the stream event confirms it.
                        if let (Some(at), Some(uid)) =
                            (server_expiry, ctx.workspace.user_id().map(str::to_owned))
                        {
                            ctx.workspace
                                .apply_status_expiry(&uid, at.and_then(RcDate::from_millis));
                        }
                        // The expiry first: set during a busy pause, it goes
                        // back to the status before the pause (the hold's).
                        let mut effects: Vec<Effect> = expiry
                            .map(|e| match e {
                                Some(e) if kept => {
                                    Effect::Quiet(crate::quiet::Request::KeptExpiry(e))
                                }
                                e => Effect::Quiet(crate::quiet::Request::Expiry(e)),
                            })
                            .into_iter()
                            .collect();
                        let ws = &mut *ctx.workspace;
                        match status {
                            // A status the user chose: recorded as chosen, and
                            // a pause no longer restores over it.
                            Some(st) => {
                                ws.apply_my_status(st, text.as_deref());
                                effects.push(Effect::Quiet(crate::quiet::Request::ReleaseHold));
                            }
                            None => {
                                let shown = ws.my_status();
                                if let (Some(uid), username) = (
                                    ws.user_id().map(str::to_owned),
                                    ws.username().map(str::to_owned),
                                ) {
                                    ws.apply_user_status(
                                        &uid,
                                        username.as_deref(),
                                        shown,
                                        text.as_deref(),
                                    );
                                }
                            }
                        }
                        Action {
                            task: Task::none(),
                            effects,
                        }
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
            Message::Shortcuts => {
                self.open = false;
                Action::effect(Effect::Navigate(Nav::Shortcuts))
            }
            Message::Help => {
                self.open = false;
                Action::effect(Effect::Navigate(Nav::HelpSettings))
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
            Message::RemoveServer if self.remove_refused => Action::none(),
            Message::RemoveServer => {
                self.open = false;
                Action::effect(Effect::RemoveServer)
            }
            Message::Omni(m) => crate::omnichannel::status::update(m, ctx).map(Message::Omni),
            Message::UiKit(m) => {
                self.open = false;
                Action::effect(Effect::UiKit(m))
            }
            Message::TogglePause => {
                self.pause_open = !self.pause_open;
                Action::none()
            }
            Message::Pause(p) => {
                self.open = false;
                let until = p.until(&chrono::Local::now(), ctx.config.ui.morning_hour());
                Action::effect(Effect::Quiet(crate::quiet::Request::Pause(
                    until.timestamp_millis(),
                )))
            }
            Message::PauseCustom => {
                self.open = false;
                Action::effect(Effect::Quiet(crate::quiet::Request::PauseCustom))
            }
            Message::Resume => {
                self.open = false;
                Action::effect(Effect::Quiet(crate::quiet::Request::Resume))
            }
            Message::ClearAfter(c) => {
                let custom = c == ClearChoice::Custom;
                self.clear = c;
                self.clear_picked = true;
                self.clear_error = false;
                if custom && self.clear_date.is_empty() {
                    let base = chrono::Local::now() + chrono::Duration::hours(1);
                    self.clear_date = base.format("%Y-%m-%d").to_string();
                    self.clear_time = base.format("%H:%M").to_string();
                }
                Action::none()
            }
            Message::ClearDate(d) => {
                self.clear_date = d;
                self.clear_error = false;
                Action::none()
            }
            Message::ClearTime(t) => {
                self.clear_time = t;
                self.clear_error = false;
                Action::none()
            }
            Message::CancelExpiry => {
                if matches!(self.clear, ClearChoice::Until(..)) {
                    self.clear = ClearChoice::default();
                }
                let now = chrono::Utc::now().timestamp_millis();
                if ctx.workspace.my_status_expiry(now).is_none() {
                    return Action::effect(Effect::Quiet(crate::quiet::Request::Expiry(None)));
                }
                // The server's: the chosen status again, without
                // `expiresAt` (the text stays).
                let (status, text) = keep_status_args(ctx.workspace);
                self.pending_expiry = Some(None);
                self.pending_server_expiry = Some(None);
                self.busy = true;
                let t2 = text.clone();
                Action::task(ctx.rest_task(
                    move |rest| async move {
                        match status {
                            Some(st) => rest.users_set_status(st, None, None).await,
                            None => {
                                rest.users_set_status_message(t2.as_deref().unwrap_or(""), None)
                                    .await
                            }
                        }
                    },
                    move |r| Message::StatusSet(None, text.clone(), r),
                ))
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
        let local = chrono::Local::now();
        let quiet = rustket_core::quiet::quiet_state(
            &ctx.config.pause,
            &ctx.config.ui.working_hours,
            &local,
        );
        // Paused notifications: a bell-off mark with the end time.
        let paused: Element<'a, Message> = if quiet.paused {
            rustket_ui::widget::tooltip(
                icon::sized(Icon::BellOff, 14.0).style(style::text_secondary),
                crate::quiet::paused_line(&quiet, local, ctx.config.ui.clock_24h),
                rustket_ui::widget::TooltipPosition::Bottom,
            )
        } else {
            iced::widget::space().width(0).into()
        };
        let trigger = button(
            row![
                me_avatar,
                column![
                    ellipsis(name.clone()).size(TEXT_BODY).font(font_semibold()),
                    ellipsis(subtitle)
                        .size(TEXT_CAPTION)
                        .style(|theme: &Theme| Some(tokens(theme).text_secondary)),
                ]
                .width(Length::Fill),
                paused,
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
        let popup = self.popup(ctx, name, username, src, status);
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
        ctx: &ViewCtx<'a>,
        name: String,
        username: String,
        src: rustket_ui::widget::AvatarSource,
        status: UserStatus,
    ) -> Element<'a, Message> {
        let ws = ctx.workspace;
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
                    ellipsis(name).size(TEXT_BODY).font(font_semibold()),
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
        self.pause_rows(&mut rows, ctx);
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
            // Enter on the row picks the next "Clear after" (the keyboard
            // does not reach the pick list).
            let (shown, choices) = self.clear_choices(ctx);
            rows.keyed(
                self.clear_after_view(ctx),
                enabled.then(|| Message::ClearAfter(shown.next(&choices))),
            );
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
        rows.item(
            labelled(Icon::Keyboard, t!("shortcuts.title").into_owned()),
            Some(Message::Shortcuts),
            false,
        );
        rows.item(
            labelled(Icon::CircleHelp, t!("help.title").into_owned()),
            Some(Message::Help),
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
        if !self.remove_refused {
            rows.item(
                labelled(Icon::Trash2, t!("status_menu.remove_server").into_owned()),
                Some(Message::RemoveServer),
                true,
            );
        }
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

impl StatusMenu {
    /// "Pause notifications" with its presets in one row of chips, or the
    /// pause with "Resume" (kept compact: the menu fits 800 px windows).
    fn pause_rows<'a>(&'a self, rows: &mut Rows<'a>, ctx: &ViewCtx<'a>) {
        let local = chrono::Local::now();
        let ui = &ctx.config.ui;
        let q = rustket_core::quiet::quiet_state(&ctx.config.pause, &ui.working_hours, &local);
        rows.plain(divider());
        if q.paused {
            rows.item(
                row![
                    icon::sized(Icon::BellOff, 16.0).style(style::text_secondary),
                    column![
                        text(t!("quiet.resume").into_owned()).size(TEXT_BODY),
                        ellipsis(crate::quiet::paused_line(&q, local, ui.clock_24h))
                            .size(TEXT_CAPTION)
                            .style(|theme: &Theme| Some(tokens(theme).text_secondary)),
                    ]
                    .width(Length::Fill),
                ]
                .spacing(SPACE_3)
                .align_y(Alignment::Center),
                Some(Message::Resume),
                false,
            );
            return;
        }
        let chevron = if self.pause_open {
            Icon::ChevronUp
        } else {
            Icon::ChevronDown
        };
        rows.item(
            row![
                icon::sized(Icon::BellOff, 16.0),
                ellipsis(t!("quiet.pause").into_owned())
                    .size(TEXT_BODY)
                    .width(Length::Fill),
                icon::sized(chevron, 14.0),
            ]
            .spacing(SPACE_3)
            .align_y(Alignment::Center),
            Some(Message::TogglePause),
            false,
        );
        if !self.pause_open {
            return;
        }
        // One entry per preset, so the keyboard reaches them (`menu_nav`).
        let preset = |label: String, when: Option<String>| -> Element<'a, Message> {
            let mut r = row![ellipsis(label).size(TEXT_BODY).width(Length::Fill)]
                .spacing(SPACE_2)
                .align_y(Alignment::Center);
            if let Some(w) = when {
                r = r.push(
                    text(w)
                        .size(TEXT_CAPTION)
                        .style(style::text_secondary)
                        .wrapping(text::Wrapping::None),
                );
            }
            container(r)
                .padding(iced::Padding::default().left(16.0 + SPACE_3))
                .into()
        };
        for p in PausePreset::ALL {
            let at = p.until(&local, ui.morning_hour()).timestamp_millis();
            rows.item(
                preset(
                    crate::quiet::preset_label(p),
                    Some(crate::format::moment(at, local, ui.clock_24h)),
                ),
                Some(Message::Pause(p)),
                false,
            );
        }
        rows.item(
            preset(format!("{}…", t!("later.preset_custom")), None),
            Some(Message::PauseCustom),
            false,
        );
    }

    /// The "Clear after" shown (a picked [`ClearChoice::Until`] as the
    /// current expiry now; "Never" once that is gone) and the choices.
    fn clear_choices(&self, ctx: &ViewCtx<'_>) -> (ClearChoice, Vec<ClearChoice>) {
        let until = live_until(ctx.workspace, ctx.config, ctx.server_id);
        let shown = match &self.clear {
            ClearChoice::Until(..) => until.clone().unwrap_or_default(),
            c => c.clone(),
        };
        (shown, ClearChoice::all(until.as_ref()))
    }

    /// "Clear after" of the next status, and the current status's expiry.
    fn clear_after_view<'a>(&'a self, ctx: &ViewCtx<'a>) -> Element<'a, Message> {
        let enabled = !self.busy;
        let (shown, choices) = self.clear_choices(ctx);
        let mut col = column![
            row![
                text(t!("quiet.clear_after").into_owned())
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary)
                    .width(Length::Fill),
                pick_list(choices, Some(shown), Message::ClearAfter)
                    .text_size(TEXT_CAPTION)
                    .padding([2, 8])
                    .style(style::pick_list)
                    .menu_style(style::menu),
            ]
            .spacing(SPACE_2)
            .align_y(Alignment::Center)
        ]
        .spacing(SPACE_1);
        if self.clear == ClearChoice::Custom {
            col = col.push(
                row![
                    text_input(&t!("later.custom_date"), &self.clear_date)
                        .on_input(Message::ClearDate)
                        .size(TEXT_CAPTION)
                        .padding([2, 6])
                        .style(style::text_input),
                    text_input(&t!("later.custom_time"), &self.clear_time)
                        .on_input(Message::ClearTime)
                        .size(TEXT_CAPTION)
                        .padding([2, 6])
                        .width(Length::Fixed(70.0))
                        .style(style::text_input),
                ]
                .spacing(SPACE_1),
            );
            if self.clear_error {
                col = col.push(
                    text(t!("later.custom_invalid").into_owned())
                        .size(TEXT_CAPTION)
                        .style(style::text_danger),
                );
            }
        }
        let local = chrono::Local::now();
        let expiry = current_expiry(
            ctx.workspace,
            ctx.config,
            ctx.server_id,
            local.timestamp_millis(),
        );
        if let Some((at, _)) = expiry {
            col = col.push(
                row![
                    icon::sized(Icon::Timer, 12.0).style(style::text_secondary),
                    text(
                        t!(
                            "quiet.clears_at",
                            time = crate::format::date_time(at, local, ctx.config.ui.clock_24h)
                        )
                        .into_owned()
                    )
                    .size(TEXT_CAPTION)
                    .style(style::text_secondary)
                    .width(Length::Fill),
                    rustket_ui::widget::icon_button_with(
                        Icon::X,
                        enabled.then_some(Message::CancelExpiry),
                        t!("quiet.keep_status"),
                        false,
                        20.0,
                    ),
                ]
                .spacing(SPACE_1)
                .align_y(Alignment::Center),
            );
        }
        container(col)
            .padding(iced::Padding::from([0.0, SPACE_2]).bottom(SPACE_2))
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

    /// A row of its own widgets that `msg` (if any) selects with the
    /// keyboard.
    fn keyed(&mut self, el: impl Into<Element<'a, Message>>, msg: Option<Message>) {
        self.els.push(el.into());
        self.acts.push(msg);
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
        m.update(Message::Toggle(true), &mut fx.ctx());
        let a = m.update(Message::Help, &mut fx.ctx());
        assert!(!m.open);
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::Navigate(Nav::HelpSettings)]
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

    #[test]
    fn enter_on_clear_after_cycles_its_choices() {
        let until = ClearChoice::Until(1, "Until x".into());
        for all in [ClearChoice::all(None), ClearChoice::all(Some(&until))] {
            let mut c = ClearChoice::default();
            let mut seen = vec![c.clone()];
            for _ in 1..all.len() {
                c = c.next(&all);
                seen.push(c.clone());
            }
            assert_eq!(seen, all);
            assert_eq!(c.next(&all), ClearChoice::default(), "wraps");
        }
        assert_eq!(ClearChoice::all(Some(&until))[1], until);
    }

    /// A server that clears statuses itself (8.6+).
    fn on_server(fx: &mut Fx, version: &str) {
        fx.ws
            .apply_server_info(serde_json::from_value(json!({ "version": version })).unwrap());
    }

    fn in_hours(h: i64) -> i64 {
        (chrono::Utc::now() + chrono::Duration::hours(h)).timestamp_millis() / 1000 * 1000
    }

    fn view_ctx(fx: &Fx) -> ViewCtx<'_> {
        ViewCtx {
            server_id: &fx.sid,
            server: &fx.url,
            workspace: &fx.ws,
            images: &fx.images,
            config: &fx.config,
            connected: true,
            now: std::time::Instant::now(),
        }
    }

    /// The open menu offers "Remove workspace" only while the policy lets
    /// the user remove the server.
    #[test]
    fn remove_item_follows_the_policy() {
        rust_i18n::set_locale("en");
        let label = t!("status_menu.remove_server").into_owned();
        for refused in [false, true] {
            let mut fx = Fx::new();
            let mut m = StatusMenu::new();
            m.remove_refused = refused;
            m.update(Message::Toggle(true), &mut fx.ctx());
            let texts = crate::tree_probe::texts(m.view(&view_ctx(&fx)));
            assert_eq!(texts.contains(&label), !refused, "{refused}: {texts:?}");
        }
    }

    #[test]
    fn the_server_expiry_is_the_current_clear_after() {
        let mut fx = Fx::new();
        on_server(&mut fx, "9.0.0");
        // Set in another client ("until November 28").
        let at = in_hours(48);
        fx.ws.apply_status_expiry("me", RcDate::from_millis(at));
        assert_eq!(
            current_expiry(&fx.ws, &fx.config, &fx.sid, at - 1),
            Some((at, true))
        );
        let mut m = StatusMenu::new();
        m.update(Message::Toggle(true), &mut fx.ctx());
        let ClearChoice::Until(shown, label) = &m.clear else {
            panic!("{:?}", m.clear)
        };
        assert_eq!(*shown, at);
        let when = crate::format::date_time(at, chrono::Local::now(), true);
        assert!(label.contains(&when), "{label}");
        let (shown, choices) = m.clear_choices(&view_ctx(&fx));
        assert_eq!(shown, m.clear);
        assert!(choices.contains(&m.clear));
        // The open menu renders with it.
        let _ = m.view(&view_ctx(&fx));
        // Another client moves it while the menu is open: shown and kept.
        let moved = in_hours(50);
        fx.ws.apply_status_expiry("me", RcDate::from_millis(moved));
        assert!(
            matches!(m.clear_choices(&view_ctx(&fx)).0, ClearChoice::Until(t, _) if t == moved)
        );
        fx.ws.apply_status_expiry("me", RcDate::from_millis(at));
        // A new text keeps it: sent again as `expiresAt`.
        m.text = "Vacation, still".into();
        m.update(Message::SubmitText, &mut fx.ctx());
        assert_eq!(m.pending_server_expiry, Some(Some(at)));
        assert_eq!(m.pending_expiry, Some(None), "the app keeps none");
        m.update(
            Message::StatusSet(None, Some("Vacation, still".into()), Ok(())),
            &mut fx.ctx(),
        );
        assert_eq!(fx.ws.my_status_expiry(0), RcDate::from_millis(at));
        // Another client clears the status; the next open shows "Never".
        fx.ws.apply_status_expiry("me", None);
        m.update(Message::Toggle(true), &mut fx.ctx());
        assert_eq!(m.clear, ClearChoice::default());
        assert_eq!(current_expiry(&fx.ws, &fx.config, &fx.sid, 0), None);
        let _ = m.view(&view_ctx(&fx));
        // Or changes it: the next open shows the new time.
        let later = in_hours(72);
        fx.ws.apply_status_expiry("me", RcDate::from_millis(later));
        m.update(Message::Toggle(true), &mut fx.ctx());
        assert!(matches!(m.clear, ClearChoice::Until(t, _) if t == later));
        // Past already (the server is about to clear it): not offered.
        fx.ws
            .apply_status_expiry("me", RcDate::from_millis(in_hours(-1)));
        m.update(Message::Toggle(true), &mut fx.ctx());
        assert_eq!(m.clear, ClearChoice::default());
    }

    #[test]
    fn clear_after_is_sent_to_a_server_that_clears_statuses() {
        use crate::quiet::Request;
        let mut fx = Fx::new();
        on_server(&mut fx, "8.9");
        let mut m = StatusMenu::new();
        m.update(Message::Toggle(true), &mut fx.ctx());
        m.update(
            Message::ClearAfter(ClearChoice::Preset(ClearAfter::Hour1)),
            &mut fx.ctx(),
        );
        let before = chrono::Utc::now().timestamp_millis();
        m.update(Message::SetStatus(UserStatus::Busy), &mut fx.ctx());
        let Some(Some(at)) = m.pending_server_expiry else {
            panic!("{:?}", m.pending_server_expiry)
        };
        assert!(at >= before + 3_590_000 && at <= before + 3_610_000);
        let a = m.update(
            Message::StatusSet(Some(UserStatus::Busy), Some(String::new()), Ok(())),
            &mut fx.ctx(),
        );
        // The app's own expiry (an old one, if any) is dropped.
        assert!(
            a.effects
                .iter()
                .any(|e| matches!(e, Effect::Quiet(Request::Expiry(None))))
        );
        assert_eq!(fx.ws.my_status_expiry(0), RcDate::from_millis(at));
        // "Never": sent without it, which clears it on the server.
        m.update(Message::Toggle(true), &mut fx.ctx());
        m.update(Message::ClearAfter(ClearChoice::default()), &mut fx.ctx());
        m.update(Message::SetStatus(UserStatus::Away), &mut fx.ctx());
        assert_eq!(m.pending_server_expiry, Some(None));
        m.update(
            Message::StatusSet(Some(UserStatus::Away), Some(String::new()), Ok(())),
            &mut fx.ctx(),
        );
        assert_eq!(fx.ws.my_status_expiry(0), None);
        // A failed call records nothing.
        fx.ws.apply_status_expiry("me", RcDate::from_millis(at));
        m.update(Message::Toggle(true), &mut fx.ctx());
        m.update(Message::ClearAfter(ClearChoice::default()), &mut fx.ctx());
        m.update(Message::SetStatus(UserStatus::Online), &mut fx.ctx());
        let err = crate::error::Failure::from(rocketchat::Error::Unauthorized {
            message: None,
            code: None,
        });
        m.update(
            Message::StatusSet(Some(UserStatus::Online), None, Err(err)),
            &mut fx.ctx(),
        );
        assert_eq!(fx.ws.my_status_expiry(0), RcDate::from_millis(at));
        assert_eq!(m.pending_server_expiry, None);
    }

    #[test]
    fn a_status_pick_does_not_inherit_the_current_expiry() {
        use crate::quiet::Request;
        // Set in web: busy "Vacation" until a later day.
        let mut fx = Fx::new();
        on_server(&mut fx, "9.0.0");
        fx.ws
            .apply_status_expiry("me", RcDate::from_millis(in_hours(48)));
        let mut m = StatusMenu::new();
        m.update(Message::Toggle(true), &mut fx.ctx());
        assert!(matches!(m.clear, ClearChoice::Until(..)));
        m.update(Message::SetStatus(UserStatus::Away), &mut fx.ctx());
        assert_eq!(m.pending_server_expiry, Some(None), "no expiresAt");
        assert_eq!(m.pending_expiry, Some(None));
        // A clear-after picked explicitly still goes with it.
        m.busy = false;
        m.update(Message::Toggle(true), &mut fx.ctx());
        m.update(
            Message::ClearAfter(ClearChoice::Preset(ClearAfter::Hour1)),
            &mut fx.ctx(),
        );
        m.update(Message::SetStatus(UserStatus::Away), &mut fx.ctx());
        assert!(matches!(m.pending_server_expiry, Some(Some(_))));

        // Before 8.6, the app's expiry (busy for an hour, back to online).
        let mut fx = Fx::new();
        on_server(&mut fx, "8.5.1");
        let sid = fx.config.add_server(rustket_core::config::ServerEntry {
            id: fx.sid.clone(),
            url: "http://localhost:3000".into(),
            ..Default::default()
        });
        fx.config.server_mut(&sid).unwrap().status_expiry = Some(StatusExpiry {
            user_id: "me".into(),
            at: in_hours(1),
            status: UserStatus::Busy,
            text: "Meeting".into(),
            restore: Some(UserStatus::Online),
        });
        let mut m = StatusMenu::new();
        m.update(Message::Toggle(true), &mut fx.ctx());
        assert!(matches!(m.clear, ClearChoice::Until(..)));
        m.update(Message::SetStatus(UserStatus::Away), &mut fx.ctx());
        assert_eq!(m.pending_expiry, Some(None), "the old expiry is dropped");
        assert_eq!(m.pending_server_expiry, None);
        let a = m.update(
            Message::StatusSet(Some(UserStatus::Away), Some(String::new()), Ok(())),
            &mut fx.ctx(),
        );
        assert!(
            a.effects
                .iter()
                .any(|e| matches!(e, Effect::Quiet(Request::Expiry(None))))
        );
    }

    #[test]
    fn keeping_a_server_status_sends_it_again_without_the_expiry() {
        let mut fx = Fx::new();
        on_server(&mut fx, "8.9.1");
        let at = in_hours(5);
        fx.ws.apply_status_expiry("me", RcDate::from_millis(at));
        let mut m = StatusMenu::new();
        m.update(Message::Toggle(true), &mut fx.ctx());
        let a = m.update(Message::CancelExpiry, &mut fx.ctx());
        assert!(m.busy && a.effects.is_empty());
        assert_eq!(m.pending_server_expiry, Some(None));
        assert_eq!(m.clear, ClearChoice::default());
        m.update(
            Message::StatusSet(None, Some("Lunch".into()), Ok(())),
            &mut fx.ctx(),
        );
        assert_eq!(fx.ws.my_status_expiry(0), None);
        assert_eq!(my_text(&fx.ws), "Lunch");
    }

    #[test]
    fn servers_before_8_6_keep_the_app_expiry() {
        use crate::quiet::Request;
        let mut fx = Fx::new();
        on_server(&mut fx, "8.5.1");
        assert!(!fx.ws.status_expiry_on_server());
        let mut m = StatusMenu::new();
        m.update(Message::Toggle(true), &mut fx.ctx());
        m.update(
            Message::ClearAfter(ClearChoice::Preset(ClearAfter::Hour4)),
            &mut fx.ctx(),
        );
        m.update(Message::SetStatus(UserStatus::Busy), &mut fx.ctx());
        assert_eq!(m.pending_server_expiry, None, "no expiresAt");
        let a = m.update(
            Message::StatusSet(Some(UserStatus::Busy), Some(String::new()), Ok(())),
            &mut fx.ctx(),
        );
        assert!(
            a.effects
                .iter()
                .any(|e| matches!(e, Effect::Quiet(Request::Expiry(Some(_)))))
        );
        // A user with a server expiry tells the server clears them.
        fx.ws
            .apply_status_expiry("me", RcDate::from_millis(in_hours(1)));
        assert!(fx.ws.status_expiry_on_server());
    }

    #[test]
    fn clear_after_sets_and_clears_the_expiry() {
        use crate::quiet::Request;
        let mut fx = Fx::new();
        let mut m = StatusMenu::new();
        let expiry = |a: &Action<Message>| {
            a.effects.iter().find_map(|e| match e {
                Effect::Quiet(Request::Expiry(x)) => Some(x.clone()),
                _ => None,
            })
        };
        // A text for an hour: only the text clears.
        m.update(Message::Toggle(true), &mut fx.ctx());
        m.update(
            Message::ClearAfter(ClearChoice::Preset(ClearAfter::Hour1)),
            &mut fx.ctx(),
        );
        m.text = "Focus".into();
        let before = chrono::Utc::now().timestamp_millis();
        m.update(Message::SubmitText, &mut fx.ctx());
        let a = m.update(
            Message::StatusSet(None, Some("Focus".into()), Ok(())),
            &mut fx.ctx(),
        );
        let e = expiry(&a).flatten().expect("an expiry");
        assert_eq!((e.text.as_str(), e.restore), ("Focus", None));
        assert!(e.at >= before + 3_590_000);
        // Busy for an hour: back to online afterwards.
        m.update(Message::Toggle(true), &mut fx.ctx());
        m.update(
            Message::ClearAfter(ClearChoice::Preset(ClearAfter::Hour1)),
            &mut fx.ctx(),
        );
        m.update(Message::SetStatus(UserStatus::Busy), &mut fx.ctx());
        let a = m.update(
            Message::StatusSet(Some(UserStatus::Busy), Some(String::new()), Ok(())),
            &mut fx.ctx(),
        );
        let e = expiry(&a).flatten().expect("an expiry");
        assert_eq!(
            (e.status, e.restore),
            (UserStatus::Busy, Some(UserStatus::Online))
        );
        // "Never" (the default when the menu opens) drops an old expiry.
        m.update(Message::Toggle(true), &mut fx.ctx());
        assert_eq!(m.clear, ClearChoice::default());
        m.update(Message::SetStatus(UserStatus::Away), &mut fx.ctx());
        let a = m.update(
            Message::StatusSet(Some(UserStatus::Away), Some(String::new()), Ok(())),
            &mut fx.ctx(),
        );
        assert_eq!(expiry(&a), Some(None));
        // A failed call leaves the expiry alone.
        m.update(Message::SetStatus(UserStatus::Away), &mut fx.ctx());
        let err = crate::error::Failure::from(rocketchat::Error::Unauthorized {
            message: None,
            code: None,
        });
        let a = m.update(
            Message::StatusSet(Some(UserStatus::Away), None, Err(err)),
            &mut fx.ctx(),
        );
        assert_eq!(expiry(&a), None);
        // A custom time in the past is refused before any call.
        m.update(Message::Toggle(true), &mut fx.ctx());
        m.update(Message::ClearAfter(ClearChoice::Custom), &mut fx.ctx());
        m.update(Message::ClearDate("2020-01-01".into()), &mut fx.ctx());
        m.update(Message::SetStatus(UserStatus::Busy), &mut fx.ctx());
        assert!(!m.busy && m.clear_error);
        // The menu closed, a status from the tray neither reuses what was
        // picked there nor is refused for it.
        m.update(Message::Toggle(false), &mut fx.ctx());
        m.update(Message::SetStatus(UserStatus::Busy), &mut fx.ctx());
        assert!(m.busy);
        let a = m.update(
            Message::StatusSet(Some(UserStatus::Busy), Some(String::new()), Ok(())),
            &mut fx.ctx(),
        );
        assert_eq!(expiry(&a), Some(None));
        m.update(Message::Toggle(true), &mut fx.ctx());
        m.update(
            Message::ClearAfter(ClearChoice::Preset(ClearAfter::Hour1)),
            &mut fx.ctx(),
        );
        m.update(Message::Toggle(false), &mut fx.ctx());
        m.update(Message::SetStatus(UserStatus::Away), &mut fx.ctx());
        let a = m.update(
            Message::StatusSet(Some(UserStatus::Away), Some(String::new()), Ok(())),
            &mut fx.ctx(),
        );
        assert_eq!(expiry(&a), Some(None), "no stale \"Clear after\"");
        // Cancel and pause requests go to the app.
        let a = m.update(Message::CancelExpiry, &mut fx.ctx());
        assert_eq!(expiry(&a), Some(None));
        let a = m.update(Message::Resume, &mut fx.ctx());
        assert!(matches!(
            a.effects.as_slice(),
            [Effect::Quiet(Request::Resume)]
        ));
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

    /// Review: before 8.6, a new text over the preselected "Until …" kept
    /// the time but dropped the status restore (busy forever).
    #[test]
    fn a_text_change_keeps_the_app_expiry_restore() {
        use crate::quiet::Request;
        let mut fx = Fx::new();
        on_server(&mut fx, "8.5.1");
        let sid = fx.config.add_server(rustket_core::config::ServerEntry {
            id: fx.sid.clone(),
            url: "http://localhost:3000".into(),
            ..Default::default()
        });
        let at = in_hours(1);
        fx.config.server_mut(&sid).unwrap().status_expiry = Some(StatusExpiry {
            user_id: "me".into(),
            at,
            status: UserStatus::Busy,
            text: "Meeting".into(),
            restore: Some(UserStatus::Online),
        });
        let mut m = StatusMenu::new();
        m.update(Message::Toggle(true), &mut fx.ctx());
        assert!(matches!(m.clear, ClearChoice::Until(..)));
        m.text = "Meeting with X".into();
        m.update(Message::SubmitText, &mut fx.ctx());
        let a = m.update(
            Message::StatusSet(None, Some("Meeting with X".into()), Ok(())),
            &mut fx.ctx(),
        );
        let e = a
            .effects
            .iter()
            .find_map(|e| match e {
                Effect::Quiet(Request::KeptExpiry(x)) => Some(x.clone()),
                _ => None,
            })
            .expect("a kept expiry");
        assert_eq!(
            (e.at, e.status, e.text.as_str(), e.restore),
            (
                at,
                UserStatus::Busy,
                "Meeting with X",
                Some(UserStatus::Online)
            )
        );
    }

    /// Review: "Until …" picked from the list goes with the status picked
    /// next (only the preselected one does not).
    #[test]
    fn a_picked_until_goes_with_the_status() {
        let mut fx = Fx::new();
        on_server(&mut fx, "9.0.0");
        let at = in_hours(48);
        fx.ws.apply_status_expiry("me", RcDate::from_millis(at));
        let mut m = StatusMenu::new();
        m.update(Message::Toggle(true), &mut fx.ctx());
        let until = m.clear.clone();
        assert!(matches!(until, ClearChoice::Until(..)));
        m.update(Message::ClearAfter(until), &mut fx.ctx());
        m.update(Message::SetStatus(UserStatus::Away), &mut fx.ctx());
        assert_eq!(m.pending_server_expiry, Some(Some(at)));
        // Reopened: preselected again, dropped by a status pick.
        m.busy = false;
        m.update(Message::Toggle(true), &mut fx.ctx());
        m.update(Message::SetStatus(UserStatus::Away), &mut fx.ctx());
        assert_eq!(m.pending_server_expiry, Some(None));
    }

    /// Review: cancelling the server's expiry sent the text again, which
    /// the server refuses when status texts are not allowed; the chosen
    /// status alone drops the expiry and keeps the text.
    #[test]
    fn cancelling_a_server_expiry_sends_the_status_alone() {
        use rocketchat::rest::set_status_body;
        let mut fx = Fx::new();
        fx.ws.apply_me(
            serde_json::from_value(json!({
                "_id": "me", "username": "me", "status": "busy",
                "statusDefault": "busy", "statusText": "Vacation"
            }))
            .unwrap(),
        );
        let (st, text) = keep_status_args(&fx.ws);
        assert_eq!(
            set_status_body(st, text.as_deref(), None),
            json!({"status": "busy"})
        );
        // The chosen status not known (offline without statusDefault): the
        // text, which keeps statusDefault on the server.
        fx.ws.apply_me(
            serde_json::from_value(json!({
                "_id": "me", "username": "me", "status": "offline", "statusText": "Vacation"
            }))
            .unwrap(),
        );
        let (st, text) = keep_status_args(&fx.ws);
        assert_eq!(
            set_status_body(st, text.as_deref(), None),
            json!({"message": "Vacation"})
        );
    }
}
