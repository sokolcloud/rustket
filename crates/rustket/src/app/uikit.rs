//! Glue between the app and the UIKit controller (`crate::uikit`): runs its
//! updates per server and turns its effects into app actions. The logic
//! lives in `crate::uikit::controller`.

use iced::{Element, Task};
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_ui::widget::BannerKind;

use super::App;
use crate::message::Message;
use crate::room;
use crate::uikit::controller::{self, Ctx, Effect, Out};
use crate::uikit::modal::AppCtx;
use crate::uikit::{self, Msg, SessionHook, UiKitMsg};
use crate::workspace::ServerState;

impl App {
    fn uikit_ctx<'a>(s: &'a mut ServerState) -> Ctx<'a> {
        Ctx {
            ws: &mut s.workspace,
            local: &mut s.uikit,
            rest: s.session.as_ref().map(|h| h.rest().clone()),
            active_rid: s.active_room.as_deref(),
        }
    }

    /// A `Message::UiKit`: dropped when it belongs to an earlier session.
    pub(super) fn on_uikit_in(&mut self, sid: ServerId, epoch: u64, m: Msg) -> Task<Message> {
        if !self.servers.get(&sid).is_some_and(|s| s.is_current(epoch)) {
            tracing::debug!("UIKit result of an earlier session dropped");
            return Task::none();
        }
        self.on_uikit(sid, m)
    }

    /// A UIKit controller message of server `sid` (current session).
    pub(super) fn on_uikit(&mut self, sid: ServerId, m: Msg) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        let out = controller::update(&mut Self::uikit_ctx(s), m);
        self.uikit_out(&sid, out)
    }

    /// A realtime UIKit event (`Followup::UiKit`).
    pub(super) fn on_uikit_session(&mut self, sid: &ServerId, hook: SessionHook) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let out = controller::session(&mut Self::uikit_ctx(s), hook);
        self.uikit_out(sid, out)
    }

    /// Action buttons, app translations and banners after login / reconnect.
    pub(super) fn uikit_bootstrap(&self, sid: &ServerId) -> Task<Message> {
        let Some(s) = self.servers.get(sid) else {
            return Task::none();
        };
        let (sid, epoch) = (sid.clone(), s.generation);
        controller::bootstrap(s.session.as_ref().map(|h| h.rest().clone()))
            .map(move |m| Message::UiKit(sid.clone(), epoch, m))
    }

    fn uikit_out(&mut self, sid: &ServerId, out: Out) -> Task<Message> {
        let s2 = sid.clone();
        let epoch = self.servers.get(sid).map_or(0, |s| s.generation);
        let mut tasks = vec![out.task.map(move |m| Message::UiKit(s2.clone(), epoch, m))];
        for e in out.effects {
            tasks.push(self.uikit_effect(sid, e));
        }
        Task::batch(tasks)
    }

    fn uikit_effect(&mut self, sid: &ServerId, e: Effect) -> Task<Message> {
        match e {
            Effect::Toast(kind, title, body) => self.toast(kind, title, body),
            Effect::OpenUrl(url) => self.open_url(url),
            Effect::Relogin => self.logged_out(sid, Some(String::from("rejected"))),
            Effect::Focus(id) => iced::widget::operation::focus(iced::widget::Id::from(id)),
            Effect::ShowBar { rid, view_id } => {
                self.panel_message(sid, &rid, room::panel::Message::ShowApp(view_id))
            }
            Effect::HideBar { rid, view_id } => {
                self.panel_message(sid, &rid, room::panel::Message::HideApp(view_id))
            }
            Effect::AppsChanged { commands, admin } => {
                if commands {
                    room::composer::forget_commands(sid);
                }
                // An open apps administration list reloads (§6.5).
                match &self.screen {
                    super::Screen::Admin(id, a)
                        if admin
                            && id == sid
                            && matches!(a.page, crate::screens::admin::Page::Apps(_)) =>
                    {
                        Task::done(Message::Admin(
                            sid.clone(),
                            crate::screens::admin::Message::Apps(
                                crate::screens::admin::apps::Msg::Reload,
                            ),
                        ))
                    }
                    _ => Task::none(),
                }
            }
            Effect::OpenUser(username) => {
                match self.servers.get(sid).and_then(|s| s.active_room.clone()) {
                    Some(rid) => self.panel_message(
                        sid,
                        &rid,
                        room::panel::Message::Open(room::panel::Kind::UserInfo(username)),
                    ),
                    None => Task::none(),
                }
            }
            Effect::OpenRoom(name) => {
                let found = self
                    .servers
                    .get(sid)
                    .and_then(|s| crate::room::render::resolve_room(&s.workspace, &name));
                match found {
                    Some(rid) => self.open_room(sid, &rid),
                    None => self.toast(
                        BannerKind::Warning,
                        t!("uikit.room_not_found", name = name.as_str()),
                        String::new(),
                    ),
                }
            }
        }
    }

    /// Delivers a panel message to the open view of `rid`.
    fn panel_message(
        &mut self,
        sid: &ServerId,
        rid: &str,
        m: room::panel::Message,
    ) -> Task<Message> {
        let Some(s) = self.servers.get(sid) else {
            return Task::none();
        };
        if !s.rooms.contains_key(rid) {
            return Task::none();
        }
        Task::done(Message::Room(
            sid.clone(),
            s.generation,
            rid.to_owned(),
            room::Message::Panel(m),
        ))
    }

    /// The UIKit confirmation dialog or modal of the active server.
    pub(super) fn uikit_overlay<'a>(
        &'a self,
        s: &'a ServerState,
    ) -> (Option<Element<'a, Message>>, Option<Message>) {
        let app = self.uikit_app_ctx(s);
        let (sid, epoch) = (s.id.clone(), s.generation);
        let wrap =
            move |msg: UiKitMsg| Message::UiKit(sid.clone(), epoch, Msg::Ui { rid: None, msg });
        match uikit::overlay(app, &s.uikit) {
            Some((_, el, dismiss)) => (Some(el.map(wrap.clone())), Some(wrap(dismiss))),
            None => (None, None),
        }
    }

    /// Widget-tree key of the app's overlay: the UIKit modal's view id (a
    /// modal replacing another starts with fresh widget state), else fixed.
    pub(super) fn overlay_key(&self) -> String {
        if self.modals.is_empty()
            && matches!(self.screen, super::Screen::Server)
            && let Some(s) = self.active.as_ref().and_then(|id| self.servers.get(id))
            && s.login.is_none()
            && s.e2e.modal.is_none()
            && let Some(key) = uikit::overlay_key(&s.workspace)
        {
            return format!("uikit:{key}");
        }
        String::from("app")
    }

    fn uikit_app_ctx<'a>(&'a self, s: &'a ServerState) -> AppCtx<'a> {
        AppCtx {
            server_id: &s.id,
            server: &s.url,
            workspace: &s.workspace,
            images: &self.images,
            prefs: &self.config.ui,
        }
    }

    /// The banner strip above the room area of `s`.
    pub(super) fn uikit_strips<'a>(&'a self, s: &'a ServerState) -> Vec<Element<'a, Message>> {
        let (sid, epoch) = (s.id.clone(), s.generation);
        uikit::strips(self.uikit_app_ctx(s))
            .into_iter()
            .map(|e| {
                let sid = sid.clone();
                e.map(move |msg| Message::UiKit(sid.clone(), epoch, Msg::Ui { rid: None, msg }))
            })
            .collect()
    }

    /// The room area of `s` with the floating "Loading" pill over it while
    /// app requests run (it does not move the layout, like the web's
    /// `ActionManagerBusyState`).
    pub(super) fn uikit_busy_over<'a>(
        &'a self,
        s: &'a ServerState,
        area: Element<'a, Message>,
    ) -> Element<'a, Message> {
        // Always a stack, so the pill coming and going keeps the area's
        // widget state.
        let pill: Element<'a, Message> = match uikit::modal::busy_pill(&s.workspace) {
            Some(pill) => iced::widget::container(pill)
                .padding(iced::Padding::ZERO.top(rustket_ui::theme::SPACE_2))
                .center_x(iced::Length::Fill)
                .into(),
            None => iced::widget::space().into(),
        };
        iced::widget::stack![area, pill].into()
    }
}
