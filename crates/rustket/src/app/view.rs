//! `App::view` and its parts.

use std::time::Instant;

use iced::widget::{column, container, row, text};
use iced::{Alignment, Element, Length};
use rust_i18n::t;
use rustket_core::config::IndicatorScheme;
use rustket_core::room_list;
use rustket_core::workspace::ConnectionState;
use rustket_ui::icon::Icon;
use rustket_ui::style;
use rustket_ui::theme::{
    SIDEBAR_MAX_WIDTH, SIDEBAR_MIN_WIDTH, SPACE_1, SPACE_2, SPACE_3, TEXT_BODY, TEXT_CAPTION,
    TEXT_SUBTITLE, font_mono, font_semibold,
};
use rustket_ui::widget::{
    AvatarShape, AvatarSource, BannerKind, Toast, above, avatar, banner, dialog, empty_state,
    loading_placeholder, modal_keyed, primary_button, secondary_button, spinner, split,
    status_strip, toasts_with, window_frame, window_titled,
};

use super::{App, Modal, Screen};
use crate::message::{Message, ModalMessage};
use crate::room::ViewCtx;
use crate::screens::add_server;
use crate::screens::login;
use crate::url_handler;
use crate::workspace::sidebar::SidebarCtx;
use crate::workspace::{ServerState, rail, titlebar};
use rustket_core::ServerId;

/// `base` under the app's layers: the open modal (keyed by dialog identity),
/// `calls` (the call dialog, above the app modal) and the corner column of
/// toasts and `call_cards` (ringing calls), drawn above every modal and taking
/// input only over the cards. Every wrapper is always there, so a layer coming
/// and going keeps the base's widget state.
pub(crate) fn layered<'a, M: Clone + 'a>(
    base: Element<'a, M>,
    overlay: Option<(String, Element<'a, M>)>,
    dismiss: Option<M>,
    calls: impl FnOnce(Element<'a, M>) -> Element<'a, M>,
    call_cards: Vec<Element<'a, M>>,
    list: &'a [Toast<M>],
    on_close: impl Fn(u64) -> M + 'a,
) -> Element<'a, M> {
    toasts_with(
        calls(modal_keyed(base, overlay, dismiss)),
        call_cards,
        list,
        on_close,
    )
}

impl App {
    // --- view --------------------------------------------------------------------

    /// Pinned corner cards: call popups, then the update card.
    fn corner_cards(&self) -> Vec<Element<'_, Message>> {
        #[allow(unused_mut)]
        let mut cards = crate::videoconf::view::popups(self);
        #[cfg(feature = "self-update")]
        cards.extend(crate::updater::view::card(self));
        cards
    }

    pub fn view(&self) -> Element<'_, Message> {
        if crate::legal::terms_pending(&self.config) {
            return self.terms_view();
        }
        if self.policy.is_blocked() {
            return self.blocked_view();
        }
        let base: Element<'_, Message> = match &self.screen {
            Screen::Loading => loading_placeholder(None),
            Screen::AddServer(a) => {
                let screen = a.view().map(Message::AddServer);
                if self.servers.is_empty() {
                    screen
                } else {
                    self.with_rail(screen)
                }
            }
            Screen::NoServers => {
                crate::screens::policy_notice::no_servers(&self.policy).map(Message::PolicyNotice)
            }
            Screen::Server => self.server_view(),
            Screen::Settings(c) => {
                self.with_rail(c.view(self.settings_view_data()).map(Message::Settings))
            }
            Screen::Account(sid, c) => self.server_page(sid, |s| {
                let sid = sid.clone();
                c.view(&self.component_view_ctx(s))
                    .map(move |m| Message::Account(sid.clone(), m))
            }),
            Screen::Directory(sid, c) => self.server_page(sid, |s| {
                let sid = sid.clone();
                c.view(&self.component_view_ctx(s))
                    .map(move |m| Message::Directory(sid.clone(), m))
            }),
            Screen::Admin(sid, c) => self.server_page(sid, |s| {
                let sid = sid.clone();
                crate::screens::admin::view_in(c, self.component_view_ctx(s))
                    .map(move |m| Message::Admin(sid.clone(), m))
            }),
        };
        let (overlay, dismiss) = self.overlay();
        let blocked = overlay.is_some() || self.calls.dialog.is_some();
        let key = self.overlay_key();
        let content = layered(
            base,
            overlay.map(|o| (key, o)),
            dismiss,
            |c| crate::videoconf::view::layer(self, c),
            self.corner_cards(),
            &self.toasts,
            Message::CloseToast,
        );
        self.chrome(content, !blocked)
    }

    /// `content` in the window chrome. Counts presses (title bar included)
    /// so text selections yield keyboard shortcuts to whatever was clicked
    /// since. Under a native frame (macOS) a title row always runs below the
    /// traffic lights. Modals cover `content` only: the window can be moved,
    /// resized and closed with one open.
    fn chrome<'a>(&'a self, content: Element<'a, Message>, enabled: bool) -> Element<'a, Message> {
        let framed = if self.native_frame {
            let title = if self.custom_chrome() {
                self.title_bar(enabled)
            } else {
                titlebar::mac_strip(self.title()).map(Message::TitleBar)
            };
            window_titled(title, content)
        } else if self.custom_chrome() {
            window_frame(
                self.title_bar(enabled),
                content,
                self.titlebar.maximized,
                |d| Message::TitleBar(titlebar::Message::Resize(d)),
            )
        } else {
            content
        };
        rustket_ui::widget::selectable::press_guard(framed)
    }

    /// The terms of use over the whole window, in the same chrome as the
    /// rest of the app so the window can still be moved and closed.
    fn terms_view(&self) -> Element<'_, Message> {
        let page = crate::screens::terms::view().map(Message::Terms);
        let content = toasts_with(page, Vec::new(), &self.toasts, Message::CloseToast);
        self.chrome(content, false)
    }

    /// The blocking screen of the policy over the whole window (after the
    /// terms), in the same chrome without workspace buttons. Only the update
    /// card of the manual check it offers stays in its corner: no call
    /// popups (every server is suspended).
    fn blocked_view(&self) -> Element<'_, Message> {
        let update = crate::screens::policy_block::offers_update(self.update_mode());
        let page =
            crate::screens::policy_block::view(&self.policy, self.policy_block_details, update)
                .map(Message::PolicyBlock);
        #[cfg(feature = "self-update")]
        let cards: Vec<_> = crate::updater::view::card(self).into_iter().collect();
        #[cfg(not(feature = "self-update"))]
        let cards = Vec::new();
        let content = toasts_with(page, cards, &self.toasts, Message::CloseToast);
        self.chrome(content, false)
    }

    /// `page` after the left rail (system title bar) or alone (the
    /// workspaces are in the custom title bar).
    fn with_rail<'a>(&'a self, page: Element<'a, Message>) -> Element<'a, Message> {
        if self.custom_chrome() {
            page
        } else {
            row![self.rail(), page].into()
        }
    }

    fn title_bar(&self, interactive: bool) -> Element<'_, Message> {
        let workspace_buttons = !self.servers.is_empty()
            && !matches!(self.screen, Screen::Loading)
            && self.accounts_live();
        titlebar::view(titlebar::View {
            entries: if workspace_buttons {
                self.rail_entries()
            } else {
                Vec::new()
            },
            workspace_buttons,
            add_button: self.may_add_servers(),
            settings_active: matches!(self.screen, Screen::Settings(_)),
            title: self.title(),
            maximized: self.titlebar.maximized,
            interactive,
            native_frame: self.native_frame,
        })
        .map(Message::TitleBar)
    }

    /// Rail plus a page of server `sid`.
    fn server_page<'a>(
        &'a self,
        sid: &ServerId,
        page: impl FnOnce(&'a ServerState) -> Element<'a, Message>,
    ) -> Element<'a, Message> {
        match self.servers.get(sid) {
            Some(s) => self.with_rail(page(s)),
            None => loading_placeholder(None),
        }
    }

    fn rail(&self) -> Element<'_, Message> {
        rail::view(self.rail_entries(), self.may_add_servers()).map(Message::Rail)
    }

    pub fn rail_entries(&self) -> Vec<rail::RailEntry> {
        let adding = !matches!(
            self.screen,
            Screen::Server | Screen::Account(..) | Screen::Directory(..) | Screen::Admin(..)
        );
        self.config
            .sorted_servers()
            .into_iter()
            .filter_map(|e| self.servers.get(&e.id))
            .map(|s| rail::RailEntry {
                id: s.id.clone(),
                name: s.name.clone(),
                avatar: AvatarSource::initials(&s.name, s.url.as_str()),
                signal: room_list::workspace_signal(&s.workspace),
                rc: match self.config.ui.indicator_scheme {
                    IndicatorScheme::Rustket => Default::default(),
                    IndicatorScheme::RocketChat => {
                        rustket_core::indicators::rc_workspace_signal(&s.workspace)
                    }
                },
                selected: !adding && self.active.as_ref() == Some(&s.id),
                signed_out: !s.is_logged_in() && !s.restoring,
                suspended: self.is_suspended(&s.id),
            })
            .collect()
    }

    fn server_view(&self) -> Element<'_, Message> {
        let Some(s) = self.active.as_ref().and_then(|id| self.servers.get(id)) else {
            return loading_placeholder(None);
        };
        let main: Element<'_, Message> = if let Some(reason) = self
            .standing_of(&s.id)
            .and_then(crate::policy::servers::Standing::suspension)
        {
            let remove = self.may_remove_server(&s.id).then(|| s.id.clone());
            crate::screens::policy_notice::suspended(&s.name, reason, &self.policy, remove)
                .map(Message::PolicyNotice)
        } else if let Some(l) = &s.login {
            let sid = s.id.clone();
            l.view().map(move |m| Message::Login(sid.clone(), m))
        } else if s.restoring {
            loading_placeholder(None)
        } else {
            self.workspace_view(s)
        };
        self.with_rail(main)
    }

    fn workspace_view<'a>(&'a self, s: &'a ServerState) -> Element<'a, Message> {
        let now = Instant::now();
        let sid = s.id.clone();
        let sidebar_ctx = SidebarCtx {
            server_id: &s.id,
            server: &s.url,
            workspace: &s.workspace,
            images: &self.images,
            prefs: &self.config.ui,
            collapsed: &self.ui_state.collapsed_sections,
            // A main view (All unreads, Activity, Later) hides the room:
            // only its pinned row is selected then.
            active_room: s.active_room.as_deref().filter(|_| s.main_view.is_none()),
            active_thread: s
                .active_room
                .as_ref()
                .filter(|_| s.main_view.is_none())
                .and_then(|r| s.rooms.get(r))
                .and_then(|rv| rv.panel.thread_tmid()),
            inbox_open: s.inbox_view().map(crate::inbox::InboxScreen::kind),
            later_open: s.later_view().is_some(),
            drafts_open: s.drafts_view().is_some(),
            search_open: s.search_view().is_some(),
            now,
        };
        // The cached workspace is shown instead of the placeholder.
        let loading = s.workspace.last_sync().is_none() && s.syncing && !s.workspace.from_cache();
        let sidebar_view: Element<'a, Message> = if loading {
            container(loading_placeholder(Some(
                t!("app.loading_rooms").into_owned(),
            )))
            .style(style::container_sidebar)
            .into()
        } else {
            let sid = sid.clone();
            let status = s.status_menu.view(&self.component_view_ctx(s));
            s.sidebar
                .view(&sidebar_ctx, status)
                .map(move |m| Message::Sidebar(sid.clone(), m))
        };
        let room_area: Element<'a, Message> =
            match s.active_room.as_ref().and_then(|r| s.rooms.get(r)) {
                Some(rv) => {
                    crate::room::render::set_mode(self.mode());
                    let ctx = ViewCtx {
                        server_id: &s.id,
                        server: &s.url,
                        rid: &rv.rid,
                        workspace: &s.workspace,
                        images: &self.images,
                        prefs: &self.config.ui,
                        connected: s.workspace.connection().clone() == ConnectionState::Connected,
                        panel: rv.panel.kind(),
                        now,
                    };
                    let (sid, rid, epoch) = (sid.clone(), rv.rid.clone(), s.generation);
                    rv.view(&ctx)
                        .map(move |m| Message::Room(sid.clone(), epoch, rid.clone(), m))
                }
                None => container(empty_state(
                    Icon::MessagesSquare,
                    t!("app.no_room_title"),
                    t!("app.no_room_subtitle"),
                ))
                .style(style::container_room)
                .width(Length::Fill)
                .height(Length::Fill)
                .into(),
            };
        let room_area = self
            .inbox_screen(s)
            .or_else(|| self.later_screen(s))
            .or_else(|| self.drafts_screen(s))
            .or_else(|| self.search_screen(s))
            .or_else(|| self.omni_screen(s))
            .unwrap_or(room_area);
        // Strips above the room (connection, E2E banner, app banners) come
        // and go in one slot; the room keeps its widget state (composer
        // focus, timeline scroll) meanwhile. The UIKit busy pill floats.
        let mut strips: Vec<Element<'_, Message>> = Vec::new();
        let retry = Message::RetryConnection(s.id.clone());
        if let Some(strip) = crate::cache::view::strip(s, self.config.ui.clock_24h, retry)
            .or_else(|| self.connection_strip(s))
        {
            strips.push(strip);
        }
        if let Some(b) = crate::e2e::view::banner(s) {
            let (sid, epoch) = (sid.clone(), s.generation);
            strips.push(b.map(move |m| Message::E2e(sid.clone(), epoch, m)));
        }
        strips.extend(self.uikit_strips(s));
        let strips = (!strips.is_empty()).then(|| column(strips).into());
        let area = above(strips, 0.0, self.uikit_busy_over(s, room_area));
        split(
            sidebar_view,
            area.width(Length::Fill).height(Length::Fill),
            self.ui_state.sidebar_width,
            SIDEBAR_MIN_WIDTH,
            SIDEBAR_MAX_WIDTH,
            Message::SidebarResized,
        )
    }

    fn connection_strip<'a>(&'a self, s: &'a ServerState) -> Option<Element<'a, Message>> {
        let retry = || {
            Some((
                t!("connection.retry").into_owned(),
                Message::RetryConnection(s.id.clone()),
            ))
        };
        match s.workspace.connection() {
            ConnectionState::Connected => s.sync_error.as_ref().map(|e| {
                status_strip(
                    BannerKind::Warning,
                    t!("connection.sync_failed", error = e),
                    None,
                    Some((
                        t!("connection.retry").into_owned(),
                        Message::RetrySync(s.id.clone(), s.generation),
                    )),
                )
            }),
            ConnectionState::LoggedOut => None,
            ConnectionState::Connecting => Some(status_strip(
                BannerKind::Info,
                t!("connection.connecting"),
                Some(spinner(12.0)),
                None,
            )),
            ConnectionState::Reconnecting { attempt } => Some(status_strip(
                BannerKind::Warning,
                t!("connection.reconnecting", attempt = attempt),
                Some(spinner(12.0)),
                retry(),
            )),
            ConnectionState::Disconnected => Some(status_strip(
                BannerKind::Error,
                match &s.start_error {
                    Some(e) => t!("connection.failed", error = e).into_owned(),
                    None => t!("connection.offline").into_owned(),
                },
                None,
                retry(),
            )),
        }
    }

    pub(super) fn overlay(&self) -> (Option<Element<'_, Message>>, Option<Message>) {
        if let Some(m) = self.modals.last() {
            return (Some(self.modal_view(m)), Some(Message::CloseModal));
        }
        match &self.screen {
            Screen::AddServer(a) => match a.overlay(self.policy.allow_certificate_exceptions()) {
                Some(o) => (
                    Some(o.map(Message::AddServer)),
                    Some(Message::AddServer(add_server::Message::RejectCertificate)),
                ),
                None => (None, None),
            },
            Screen::Server => {
                let Some(s) = self.active.as_ref().and_then(|id| self.servers.get(id)) else {
                    return (None, None);
                };
                if let Some(o) = crate::e2e::view::modal(s) {
                    let (sid, epoch) = (s.id.clone(), s.generation);
                    let close = Message::E2e(sid.clone(), epoch, crate::e2e::Message::CloseModal);
                    return (
                        Some(o.map(move |m| Message::E2e(sid.clone(), epoch, m))),
                        Some(close),
                    );
                }
                let Some(l) = &s.login else {
                    return self.uikit_overlay(s);
                };
                let sid = s.id.clone();
                match l.overlay() {
                    Some(o) => {
                        let sid2 = sid.clone();
                        (
                            Some(o.map(move |m| Message::Login(sid.clone(), m))),
                            Some(Message::Login(sid2, login::Message::CancelCode)),
                        )
                    }
                    None => (None, None),
                }
            }
            _ => (None, None),
        }
    }

    fn modal_view<'a>(&'a self, m: &'a Modal) -> Element<'a, Message> {
        match m {
            Modal::ConfirmToken {
                server_id,
                user,
                armed,
                ..
            } => {
                let host = self
                    .servers
                    .get(server_id)
                    .map(|s| s.url.as_str().to_owned())
                    .unwrap_or_default();
                let username = user.username.clone().unwrap_or_default();
                let display = login::quote_untrusted(user.name.as_deref().unwrap_or(&username), 64);
                let src = self.servers.get(server_id).map_or_else(
                    || AvatarSource::initials(&username, &username),
                    |s| {
                        self.images.avatar(
                            server_id,
                            &s.url,
                            &room_list::Avatar::User {
                                username: username.clone(),
                            },
                            &username,
                        )
                    },
                );
                let username_line = login::quote_untrusted(&format!("@{username}"), 64);
                dialog(
                    t!("deeplink.confirm_title"),
                    column![
                        text(t!("deeplink.confirm_question")).size(TEXT_BODY),
                        row![
                            avatar(src, 40.0, AvatarShape::Rounded),
                            column![
                                text(username_line.trim_matches(['“', '”']).to_owned())
                                    .size(TEXT_SUBTITLE)
                                    .font(font_semibold()),
                                text(t!("deeplink.confirm_display_name", name = display))
                                    .size(TEXT_CAPTION)
                                    .style(style::text_secondary),
                            ]
                            .spacing(SPACE_1),
                        ]
                        .spacing(SPACE_3)
                        .align_y(Alignment::Center),
                        text(t!("deeplink.confirm_server"))
                            .size(TEXT_CAPTION)
                            .style(style::text_secondary),
                        container(text(host).font(font_mono()).size(TEXT_BODY))
                            .padding(SPACE_2)
                            .width(Length::Fill)
                            .style(style::container_code_block),
                        banner(BannerKind::Warning, t!("deeplink.confirm_warning"), None),
                    ]
                    .spacing(SPACE_3),
                    vec![
                        secondary_button(t!("app.cancel"), Some(Message::CloseModal)).into(),
                        primary_button(
                            t!("deeplink.confirm_sign_in"),
                            armed.then_some(Message::Modal(ModalMessage::ConfirmToken)),
                        )
                        .into(),
                    ],
                    Some(Message::CloseModal),
                )
            }
            Modal::OfferAddServer { url } => dialog(
                t!("deeplink.add_server_title"),
                text(t!("deeplink.add_server_body", server = url.as_str())).size(TEXT_BODY),
                vec![
                    secondary_button(t!("app.cancel"), Some(Message::CloseModal)).into(),
                    primary_button(
                        t!("deeplink.add_server_confirm"),
                        Some(Message::Modal(ModalMessage::AddOfferedServer)),
                    )
                    .into(),
                ],
                Some(Message::CloseModal),
            ),
            Modal::RegisterHandler {
                current,
                loaded,
                busy,
                error,
                warnings,
            } => {
                let mut body = column![text(t!("handler.body")).size(TEXT_BODY)].spacing(SPACE_3);
                if *loaded {
                    match current.as_deref() {
                        Some(id) if url_handler::is_ours(id) && self.handler_registered => {
                            body =
                                body.push(banner(BannerKind::Success, t!("handler.already"), None));
                        }
                        Some(id) if url_handler::is_ours(id) => {
                            body = body.push(banner(
                                BannerKind::Warning,
                                t!("handler.stale", current = id),
                                None,
                            ));
                        }
                        Some(other) => {
                            body = body.push(banner(
                                BannerKind::Warning,
                                t!("handler.replaces", current = other),
                                None,
                            ));
                        }
                        None => {}
                    }
                }
                for w in warnings {
                    body = body.push(banner(
                        BannerKind::Warning,
                        t!("handler.shared_group", path = w),
                        None,
                    ));
                }
                if let Some(e) = error {
                    body = body.push(banner(BannerKind::Error, e.clone(), None));
                }
                dialog(
                    t!("handler.title"),
                    body,
                    vec![
                        secondary_button(t!("app.cancel"), Some(Message::CloseModal)).into(),
                        primary_button(
                            t!("handler.register"),
                            (*loaded && error.is_none())
                                .then_some(Message::Modal(ModalMessage::RegisterHandler)),
                        )
                        .loading(*busy)
                        .into(),
                    ],
                    Some(Message::CloseModal),
                )
            }
            Modal::CreateRoom(sid, c) => match self.servers.get(sid) {
                Some(s) => {
                    let sid = sid.clone();
                    c.view(&self.component_view_ctx(s))
                        .map(move |m| Message::CreateRoom(sid.clone(), m))
                }
                None => text("").into(),
            },
            Modal::QuickSwitcher(sid, c) => match self.servers.get(sid) {
                Some(s) => {
                    let sid = sid.clone();
                    c.view(&self.component_view_ctx(s))
                        .map(move |m| Message::QuickSwitcher(sid.clone(), m))
                }
                None => text("").into(),
            },
            Modal::ConfirmRemove(sid) => {
                let name = self
                    .servers
                    .get(sid)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                dialog(
                    t!("app.remove_title"),
                    text(t!("app.remove_body", server = name)).size(TEXT_BODY),
                    vec![
                        secondary_button(t!("app.cancel"), Some(Message::CloseModal)).into(),
                        rustket_ui::widget::danger_button(
                            t!("app.remove_confirm"),
                            Some(Message::Modal(ModalMessage::ConfirmRemove(sid.clone()))),
                        )
                        .into(),
                    ],
                    Some(Message::CloseModal),
                )
            }
            Modal::Shortcuts => crate::dialogs::shortcuts::view(self.config.ui.escape_marks_read),
            Modal::Missed(state) => crate::drafts::missed::view(
                state,
                self.missed_items(),
                chrono::Local::now(),
                self.config.ui.morning_hour(),
                self.config.ui.clock_24h,
                Message::Missed,
                Message::CloseModal,
            ),
            Modal::PauseCustom(picker) => crate::quiet::custom_dialog(
                picker,
                self.config.ui.pause_sets_busy,
                chrono::Local::now(),
                self.config.ui.morning_hour(),
                self.config.ui.clock_24h,
                Message::CloseModal,
                Message::Quiet,
            ),
            Modal::ReadAll {
                server_id,
                rids,
                busy,
                done,
                failed,
                ..
            } => {
                let server = self
                    .servers
                    .get(server_id)
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                let count = rids.len();
                let key = format!(
                    "shortcuts.read_all_body_{}",
                    crate::room::render::plural(count as u64)
                );
                let mut body =
                    column![text(t!(key, count = count, server = server)).size(TEXT_BODY)]
                        .spacing(SPACE_3);
                if *busy {
                    body = body.push(
                        row![
                            spinner(14.0),
                            text(t!(
                                "shortcuts.read_all_progress",
                                done = done + failed,
                                count = count
                            ))
                            .size(TEXT_CAPTION)
                            .style(style::text_secondary),
                        ]
                        .spacing(SPACE_2)
                        .align_y(Alignment::Center),
                    );
                }
                dialog(
                    t!("shortcuts.read_all_title"),
                    body,
                    vec![
                        secondary_button(t!("app.cancel"), (!*busy).then_some(Message::CloseModal))
                            .into(),
                        primary_button(
                            t!("shortcuts.read_all_confirm"),
                            (!*busy).then_some(Message::Modal(ModalMessage::ConfirmReadAll)),
                        )
                        .loading(*busy)
                        .into(),
                    ],
                    (!*busy).then_some(Message::CloseModal),
                )
            }
            Modal::SecretsMemoryOnly { pending } => {
                let mut body = column![].spacing(SPACE_3);
                if !pending.is_empty() {
                    let names: Vec<String> = pending
                        .iter()
                        .map(|sid| {
                            self.servers
                                .get(sid)
                                .map_or_else(|| sid.to_string(), |s| s.name.clone())
                        })
                        .collect();
                    body = body.push(banner(
                        BannerKind::Error,
                        t!("app.secrets_memory_pending", accounts = names.join(", ")),
                        None,
                    ));
                }
                body = body.push(text(t!("app.secrets_memory_body")).size(TEXT_BODY));
                if self.secret_backend == Some(rustket_core::secrets::SecretBackend::File) {
                    body = body.push(text(t!("app.secrets_memory_file_note")).size(TEXT_BODY));
                }
                dialog(
                    t!("app.secrets_memory_title"),
                    body,
                    vec![
                        secondary_button(t!("app.cancel"), Some(Message::CloseModal)).into(),
                        primary_button(
                            t!("app.secrets_memory_confirm"),
                            Some(Message::Modal(ModalMessage::ConfirmSecretsMemoryOnly)),
                        )
                        .into(),
                    ],
                    Some(Message::CloseModal),
                )
            }
        }
    }
}
