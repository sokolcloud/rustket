//! Routing of app-level components (settings, account, directory, dialogs,
//! status menu, desktop integration) and their effects.

use iced::Task;
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::workspace::ConnectionState;

use super::{App, Modal, Screen, SecretsMode};
use crate::component::{self, Ctx, Effect, Nav};
use crate::desktop;
use crate::dialogs::{create_room, quick_switcher};
use crate::e2e;
use crate::message::{Go, Message};
use crate::screens::{account, directory, settings};
use crate::workspace::{ServerState, status_menu};

/// Where an effect came from (decides what `Effect::Close` closes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Source {
    Screen,
    CreateRoom,
    QuickSwitcher,
    StatusMenu,
}

/// The create dialog for a discussion of `rid` (from message `pmid`), or
/// `None` when the room is unknown. Inside a discussion the parent is the
/// discussion's own parent (`room.prid || room._id`, as in the web client):
/// the server rejects nested discussions.
pub(super) fn discussion_dialog(
    ws: &rustket_core::Workspace,
    rid: &str,
    pmid: Option<&str>,
    real_names: Option<bool>,
) -> Option<create_room::CreateRoom> {
    ws.room(rid)?;
    let parent = rustket_core::permissions::discussion_parent(ws, rid);
    let use_real = ws.use_real_names(real_names);
    let name = ws
        .room(&parent)
        .map(|e| rustket_core::room_list::display_name(ws, e, use_real))
        .unwrap_or_else(|| parent.clone());
    let dialog = create_room::CreateRoom::discussion_in(&parent, &name);
    Some(match pmid {
        Some(mid) => {
            let excerpt = ws
                .timeline(rid)
                .and_then(|t| t.get(mid))
                // A thread root opened in the thread panel only.
                .or_else(|| ws.thread(mid).and_then(|t| t.get(mid)))
                // The discussion name is plaintext on the server: never
                // prefilled from an encrypted message.
                .filter(|i| !crate::e2e::room::is_encrypted_message(&i.message))
                .map(|i| crate::room::render::plain_text(&i.message))
                .unwrap_or_default();
            let excerpt: String = excerpt.chars().take(200).collect();
            dialog.from_message(mid, &excerpt)
        }
        None => dialog,
    })
}

impl App {
    /// Read-only context for a per-server component view.
    pub(super) fn component_view_ctx<'a>(&'a self, s: &'a ServerState) -> component::ViewCtx<'a> {
        component::ViewCtx {
            server_id: &s.id,
            server: &s.url,
            workspace: &s.workspace,
            images: &self.images,
            config: &self.config,
            connected: *s.workspace.connection() == ConnectionState::Connected,
            now: std::time::Instant::now(),
        }
    }

    pub(super) fn navigate(&mut self, nav: Nav) -> Task<Message> {
        let leaves = matches!(
            nav,
            Nav::Settings
                | Nav::SidebarSettings { .. }
                | Nav::Workspace
                | Nav::Account
                | Nav::Directory
                | Nav::Admin
        );
        if leaves && !self.may_leave(Message::Go(Go::Nav(nav.clone()))) {
            return Task::none();
        }
        let commit = if leaves {
            self.commit_flag_drafts()
        } else {
            Task::none()
        };
        let sid = self.active.clone();
        let task = self.navigate_to(nav, sid);
        Task::batch([commit, task])
    }

    /// Leaving the settings: flag names typed but not saved with Enter are
    /// saved (iced's `text_input` has no blur event).
    fn commit_flag_drafts(&mut self) -> Task<Message> {
        let Screen::Settings(s) = &mut self.screen else {
            return Task::none();
        };
        let mut tasks = Vec::new();
        for ((sid, color), draft) in std::mem::take(&mut s.flag_drafts) {
            let name = rustket_core::flags::clean_flag_name(&draft);
            let stored = self
                .servers
                .get(&sid)
                .and_then(|s| s.workspace.flags().name(color));
            if name.as_deref() != stored {
                tasks.push(
                    self.set_flag(&sid, rustket_core::flags::FlagEdit::Rename { color, name }),
                );
            }
        }
        Task::batch(tasks)
    }

    fn navigate_to(&mut self, nav: Nav, sid: Option<ServerId>) -> Task<Message> {
        match (nav, sid) {
            (nav @ (Nav::Settings | Nav::SidebarSettings { .. }), _) => {
                // The real autostart location only with real desktop integration.
                let at = self
                    .desktop
                    .is_enabled()
                    .then(crate::desktop::autostart::Autostart::system)
                    .flatten();
                let mut screen = settings::Settings::with_autostart(at);
                // Theme files edited meanwhile show up (and apply) here.
                self.themes.rescan();
                self.themes.apply(self.config.ui.theme_name.as_deref());
                // So are locale files.
                self.locales.reload(&self.config.ui.language);
                let load = screen.load().map(Message::Settings);
                let reveal = match nav {
                    Nav::SidebarSettings { ungroup, flag } => {
                        // The confirmation is in the group's row, far below
                        // the card's top; a flag's name in the Flags block.
                        let task = match (&ungroup, flag, &self.active) {
                            (Some(g), _, _) => settings::reveal_sidebar_group(g),
                            (None, Some(c), Some(sid)) => settings::reveal_flag(sid, c),
                            _ => settings::reveal_sidebar(),
                        };
                        screen.confirm_delete = ungroup;
                        task
                    }
                    _ => Task::none(),
                };
                self.screen = Screen::Settings(Box::new(screen));
                Task::batch([load, crate::cache::size_task(self), reveal])
            }
            (Nav::Workspace, _) => {
                self.screen = if self.servers.is_empty() {
                    Screen::AddServer(Box::new(crate::screens::add_server::AddServer::new(false)))
                } else {
                    Screen::Server
                };
                Task::none()
            }
            (Nav::Account, Some(sid)) => {
                self.screen = Screen::Account(sid, Box::new(account::Account::new()));
                Task::none()
            }
            (Nav::Directory, Some(sid)) => {
                self.screen = Screen::Directory(sid, Box::new(directory::Directory::new()));
                Task::none()
            }
            (Nav::Admin, Some(sid)) => self.open_admin(sid),
            (Nav::CreateRoom(kind), Some(sid)) => {
                self.dialog_instances.0 += 1;
                self.modals.push(Modal::CreateRoom(
                    sid,
                    Box::new(create_room::CreateRoom::new(kind)),
                ));
                Task::none()
            }
            (Nav::CreateDiscussion { rid, pmid }, Some(sid)) => {
                let real = self.config.ui.use_real_names;
                let Some(dialog) = self
                    .servers
                    .get(&sid)
                    .and_then(|s| discussion_dialog(&s.workspace, &rid, pmid.as_deref(), real))
                else {
                    return Task::none();
                };
                self.modals.retain(|m| !matches!(m, Modal::CreateRoom(..)));
                self.dialog_instances.0 += 1;
                self.modals.push(Modal::CreateRoom(sid, Box::new(dialog)));
                iced::widget::operation::focus(create_room::NAME_ID)
            }
            (Nav::QuickSwitcher, Some(sid)) => {
                if self
                    .modals
                    .iter()
                    .any(|m| matches!(m, Modal::QuickSwitcher(..)))
                {
                    return Task::none();
                }
                self.dialog_instances.1 += 1;
                self.modals.push(Modal::QuickSwitcher(
                    sid.clone(),
                    Box::new(quick_switcher::QuickSwitcher::new()),
                ));
                quick_switcher::QuickSwitcher::focus()
                    .map(move |m| Message::QuickSwitcher(sid.clone(), m))
            }
            (_, None) => Task::none(),
        }
    }

    pub(super) fn component_effects(
        &mut self,
        sid: Option<&ServerId>,
        source: Source,
        effects: Vec<Effect>,
    ) -> Task<Message> {
        let mut tasks = Vec::new();
        for e in effects {
            tasks.push(match e {
                Effect::Toast(kind, title, body) => self.toast(kind, title, body),
                Effect::Navigate(nav) => self.navigate(nav),
                Effect::OpenRoom(rid) => match sid {
                    Some(sid) => self.go(Go::Room {
                        sid: sid.clone(),
                        rid,
                        tmid: None,
                    }),
                    None => Task::none(),
                },
                Effect::OpenUrl(url) => self.open_url(url),
                Effect::Close => match source {
                    Source::Screen => self.navigate(Nav::Workspace),
                    Source::CreateRoom => {
                        self.modals.retain(|m| !matches!(m, Modal::CreateRoom(..)));
                        Task::none()
                    }
                    Source::QuickSwitcher => {
                        self.modals
                            .retain(|m| !matches!(m, Modal::QuickSwitcher(..)));
                        Task::none()
                    }
                    Source::StatusMenu => Task::none(),
                },
                Effect::SaveConfig => self.save_config(),
                Effect::Logout { everywhere } => match sid {
                    Some(sid) => self.go(Go::Logout {
                        sid: sid.clone(),
                        everywhere,
                    }),
                    None => Task::none(),
                },
                Effect::RemoveServer => {
                    if let Some(sid) = sid {
                        self.modals.push(Modal::ConfirmRemove(sid.clone()));
                    }
                    Task::none()
                }
                Effect::RegisterUrlHandler => self.open_register_handler(),
                Effect::ClearImageCache => self.clear_image_cache(),
                Effect::ClearOfflineCache => crate::cache::clear_all(self),
                Effect::TestNotification => self
                    .desktop
                    .test_notification(&self.config.ui)
                    .map(Message::Desktop),
                Effect::ApplyTheme { rescan } => {
                    if rescan {
                        self.themes.rescan();
                    }
                    self.themes.apply(self.config.ui.theme_name.as_deref());
                    Task::none()
                }
                Effect::OpenThemesFolder => self.open_themes_folder(),
                Effect::ReloadLocales => {
                    self.locales.reload(&self.config.ui.language);
                    Task::none()
                }
                Effect::OpenLocalesFolder => self.open_locales_folder(),
                Effect::RecentStatusTexts(list) => {
                    self.config.set_recent_status_texts(list);
                    self.save_config()
                }
                Effect::SetFlag(sid, edit) => self.set_flag(&sid, edit),
                Effect::StarKeptThreads => self.star_kept_threads(),
                Effect::UiKit(m) => match sid {
                    Some(sid) => {
                        self.on_uikit(sid.clone(), crate::uikit::Msg::Ui { rid: None, msg: *m })
                    }
                    None => Task::none(),
                },
                #[cfg(feature = "self-update")]
                Effect::Updater(m) => crate::updater::app::update(self, m),
            });
        }
        Task::batch(tasks)
    }

    /// End-to-end encryption flow of server `sid` (banner, modals, manager
    /// events).
    pub(super) fn on_e2e(&mut self, sid: ServerId, epoch: u64, m: e2e::Message) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid) else {
            return Task::none();
        };
        if !s.is_current(epoch) {
            return Task::none();
        }
        let (task, requests) = e2e::update(s, m);
        let s2 = sid.clone();
        let mut tasks = vec![task.map(move |m| Message::E2e(s2.clone(), epoch, m))];
        for r in requests {
            tasks.push(match r {
                e2e::Request::Toast(kind, title, body) => self.toast(kind, title, body),
                e2e::Request::OpenAccountSecurity => {
                    self.screen = Screen::Account(
                        sid.clone(),
                        Box::new(account::Account::with_tab(account::Tab::Security)),
                    );
                    Task::none()
                }
                e2e::Request::SignOut(e2e::SignOut::Logout { everywhere }) => {
                    self.logout(&sid, everywhere)
                }
                e2e::Request::SignOut(e2e::SignOut::RemoveServer) => self.remove_server(&sid),
                // The warning is a modal of the server screen.
                e2e::Request::ShowModal => self.activate(sid.clone()),
            });
        }
        Task::batch(tasks)
    }

    /// Logout or server removal the user asked for: first warns (and shows
    /// it again) when it would delete an unsaved generated E2E passphrase.
    pub(super) fn sign_out(&mut self, sid: &ServerId, action: e2e::SignOut) -> Task<Message> {
        let uid = self.config.server(sid).and_then(|e| e.user_id.clone());
        if let Some(s) = self.servers.get_mut(sid) {
            match e2e::check_sign_out(s, self.secrets.as_ref(), uid.as_deref(), action) {
                e2e::SignOutCheck::Now => {}
                // The warning is a modal of the server screen.
                e2e::SignOutCheck::Warned => return self.activate(sid.clone()),
                e2e::SignOutCheck::Pending(task) => return task,
            }
        }
        match action {
            e2e::SignOut::Logout { everywhere } => self.logout(sid, everywhere),
            e2e::SignOut::RemoveServer => self.remove_server(sid),
        }
    }

    /// A room component asked for the account E2E flow.
    pub(super) fn on_e2e_room(&mut self, sid: &ServerId, req: e2e::RoomRequest) -> Task<Message> {
        let Some(epoch) = self.servers.get(sid).map(|s| s.generation) else {
            return Task::none();
        };
        match req {
            e2e::RoomRequest::OpenModal => self.on_e2e(sid.clone(), epoch, e2e::Message::OpenModal),
        }
    }

    pub(super) fn on_settings(&mut self, m: settings::Message) -> Task<Message> {
        let Screen::Settings(s) = &mut self.screen else {
            return Task::none();
        };
        let mut ctx = settings::SettingsCtx {
            config: &mut self.config,
            ui_state: &mut self.ui_state,
        };
        let a = s.update(m, &mut ctx);
        // A draft that reads exactly like the stored name is no edit: it must
        // not shadow a rename made elsewhere.
        s.flag_drafts.retain(|(sid, color), draft| {
            let stored = self
                .servers
                .get(sid)
                .and_then(|s| s.workspace.flags().name(*color));
            draft.as_str() != stored.unwrap_or_default()
        });
        let task = a.task.map(Message::Settings);
        Task::batch([
            task,
            self.component_effects(None, Source::Screen, a.effects),
        ])
    }

    /// Runs `f` with the per-server context of `sid`.
    fn server_ctx<'a>(
        servers: &'a mut std::collections::BTreeMap<ServerId, ServerState>,
        images: &'a crate::images::Images,
        config: &'a rustket_core::config::AppConfig,
        sid: &ServerId,
    ) -> Option<(Ctx<'a>, &'a mut status_menu::StatusMenu)> {
        let s = servers.get_mut(sid)?;
        Some((
            Ctx {
                server_id: &s.id,
                server: &s.url,
                workspace: &mut s.workspace,
                session: s.session.as_ref(),
                images,
                config,
            },
            &mut s.status_menu,
        ))
    }

    pub(super) fn on_account(&mut self, sid: ServerId, m: account::Message) -> Task<Message> {
        let Screen::Account(id, c) = &mut self.screen else {
            return Task::none();
        };
        if *id != sid {
            return Task::none();
        }
        let Some((mut ctx, _)) =
            Self::server_ctx(&mut self.servers, &self.images, &self.config, &sid)
        else {
            return Task::none();
        };
        let a = c.update(m, &mut ctx);
        let s2 = sid.clone();
        let task = a.task.map(move |m| Message::Account(s2.clone(), m));
        Task::batch([
            task,
            self.component_effects(Some(&sid), Source::Screen, a.effects),
        ])
    }

    pub(super) fn on_directory(&mut self, sid: ServerId, m: directory::Message) -> Task<Message> {
        let Screen::Directory(id, c) = &mut self.screen else {
            return Task::none();
        };
        if *id != sid {
            return Task::none();
        }
        let Some((mut ctx, _)) =
            Self::server_ctx(&mut self.servers, &self.images, &self.config, &sid)
        else {
            return Task::none();
        };
        let a = c.update(m, &mut ctx);
        let s2 = sid.clone();
        let task = a.task.map(move |m| Message::Directory(s2.clone(), m));
        Task::batch([
            task,
            self.component_effects(Some(&sid), Source::Screen, a.effects),
        ])
    }

    pub(super) fn open_admin(&mut self, sid: ServerId) -> Task<Message> {
        let mut admin = Box::new(crate::screens::admin::Admin::new());
        let task = match Self::server_ctx(&mut self.servers, &self.images, &self.config, &sid) {
            Some((mut ctx, _)) => admin.open(&mut ctx),
            None => Task::none(),
        };
        self.screen = Screen::Admin(sid.clone(), admin);
        task.map(move |m| Message::Admin(sid.clone(), m))
    }

    pub(super) fn on_admin(
        &mut self,
        sid: ServerId,
        m: crate::screens::admin::Message,
    ) -> Task<Message> {
        let Screen::Admin(id, c) = &mut self.screen else {
            return Task::none();
        };
        if *id != sid {
            return Task::none();
        }
        let Some((mut ctx, _)) =
            Self::server_ctx(&mut self.servers, &self.images, &self.config, &sid)
        else {
            return Task::none();
        };
        let a = c.update(m, &mut ctx);
        let leave = c.take_leave();
        let s2 = sid.clone();
        let task = a.task.map(move |m| Message::Admin(s2.clone(), m));
        Task::batch([
            task,
            self.component_effects(Some(&sid), Source::Screen, a.effects),
            leave.map_or_else(Task::none, Task::done),
        ])
    }

    /// Every way away from the screen (rail, room links, notifications,
    /// quick switcher, calls, logout, window close) asks an admin screen with
    /// unsaved settings first: `false` when it asked; `then` is replayed
    /// after "Discard".
    pub(crate) fn may_leave(&mut self, then: Message) -> bool {
        match &mut self.screen {
            Screen::Admin(_, c) => c.ask_leave(then),
            _ => true,
        }
    }

    /// Whether the admin screen holds unsaved settings edits.
    pub(crate) fn admin_dirty(&self) -> bool {
        matches!(&self.screen, Screen::Admin(_, c) if c.is_dirty())
    }

    /// A navigation that leaves the current screen, after the admin check.
    pub(super) fn go(&mut self, go: Go) -> Task<Message> {
        if !self.may_leave(Message::Go(go.clone())) {
            return Task::none();
        }
        let commit = self.commit_flag_drafts();
        let task = self.go_to(go);
        Task::batch([commit, task])
    }

    fn go_to(&mut self, go: Go) -> Task<Message> {
        match go {
            Go::Nav(nav) => self.navigate(nav),
            Go::Room { sid, rid, tmid } => {
                let a = self.activate_for(sid.clone(), false);
                let open = self.open_room(&sid, &rid);
                if let Some(tmid) = tmid
                    && let Some(rv) = self
                        .servers
                        .get_mut(&sid)
                        .and_then(|s| s.rooms.get_mut(&rid))
                {
                    let kind = crate::room::panel::Kind::Thread(tmid);
                    if rv.panel.kind() != Some(&kind) {
                        rv.panel.open(kind);
                    }
                }
                Task::batch([a, open])
            }
            Go::AddServer(url) => {
                self.screen = Screen::AddServer(Box::new(
                    crate::screens::add_server::AddServer::new(!self.servers.is_empty())
                        .with_url(url.as_str()),
                ));
                Task::done(Message::AddServer(
                    crate::screens::add_server::Message::Submit,
                ))
            }
            Go::Logout { sid, everywhere } => {
                self.sign_out(&sid, e2e::SignOut::Logout { everywhere })
            }
            Go::Discussion { sid, rid, pmid } => {
                let a = self.activate(sid);
                let nav = self.navigate(Nav::CreateDiscussion { rid, pmid });
                Task::batch([a, nav])
            }
            Go::Quit => self.quit_now(),
        }
    }

    /// Quits without asking anything (after the admin check of `Go::Quit`,
    /// or once an update is installed and its relaunch is under way).
    pub(crate) fn quit_now(&mut self) -> Task<Message> {
        self.flush_config();
        crate::cache::on_quit(self);
        iced::exit()
    }

    /// Realtime events for an open admin screen of `sid`.
    pub(super) fn admin_event(&mut self, sid: &ServerId, ev: &crate::session::ServerEvent) {
        if let (Screen::Admin(id, c), crate::session::ServerEvent::Event(e)) =
            (&mut self.screen, ev)
            && id == sid
        {
            c.on_event(e);
        }
    }

    pub(super) fn on_create_room(
        &mut self,
        sid: ServerId,
        m: create_room::Message,
    ) -> Task<Message> {
        let Some(c) = self.modals.iter_mut().find_map(|md| match md {
            Modal::CreateRoom(id, c) if *id == sid => Some(c),
            _ => None,
        }) else {
            return Task::none();
        };
        let Some((mut ctx, _)) =
            Self::server_ctx(&mut self.servers, &self.images, &self.config, &sid)
        else {
            return Task::none();
        };
        let a = c.update(m, &mut ctx);
        let s2 = sid.clone();
        let instance = self.dialog_instances.0;
        let task = a
            .task
            .map(move |m| Message::Dialog(instance, Box::new(Message::CreateRoom(s2.clone(), m))));
        Task::batch([
            task,
            self.component_effects(Some(&sid), Source::CreateRoom, a.effects),
        ])
    }

    pub(super) fn on_quick_switcher(
        &mut self,
        sid: ServerId,
        m: quick_switcher::Message,
    ) -> Task<Message> {
        let Some(c) = self.modals.iter_mut().find_map(|md| match md {
            Modal::QuickSwitcher(id, c) if *id == sid => Some(c),
            _ => None,
        }) else {
            return Task::none();
        };
        let Some((mut ctx, _)) =
            Self::server_ctx(&mut self.servers, &self.images, &self.config, &sid)
        else {
            return Task::none();
        };
        let a = c.update(m, &mut ctx);
        let s2 = sid.clone();
        let instance = self.dialog_instances.1;
        let task = a.task.map(move |m| {
            Message::Dialog(instance, Box::new(Message::QuickSwitcher(s2.clone(), m)))
        });
        Task::batch([
            task,
            self.component_effects(Some(&sid), Source::QuickSwitcher, a.effects),
        ])
    }

    /// A dialog's task result: dropped unless that dialog instance is still
    /// the open one (a closed dialog's late result never lands in the next).
    pub(super) fn on_dialog_result(&mut self, instance: u64, m: Message) -> Task<Message> {
        match m {
            Message::CreateRoom(sid, m) if instance == self.dialog_instances.0 => {
                self.on_create_room(sid, m)
            }
            Message::QuickSwitcher(sid, m) if instance == self.dialog_instances.1 => {
                self.on_quick_switcher(sid, m)
            }
            _ => Task::none(),
        }
    }

    pub(super) fn on_status_menu(
        &mut self,
        sid: ServerId,
        m: status_menu::Message,
    ) -> Task<Message> {
        let Some((mut ctx, menu)) =
            Self::server_ctx(&mut self.servers, &self.images, &self.config, &sid)
        else {
            return Task::none();
        };
        let a = menu.update(m, &mut ctx);
        let s2 = sid.clone();
        let task = a.task.map(move |m| Message::StatusMenu(s2.clone(), m));
        Task::batch([
            task,
            self.component_effects(Some(&sid), Source::StatusMenu, a.effects),
        ])
    }

    pub(super) fn on_desktop(&mut self, m: desktop::Message) -> Task<Message> {
        let (task, requests) = self.desktop.update(m);
        let mut tasks = vec![task.map(Message::Desktop)];
        for r in requests {
            tasks.push(match r {
                desktop::Request::OpenRoom { server_id, rid } => self.go(Go::Room {
                    sid: server_id,
                    rid,
                    tmid: None,
                }),
                desktop::Request::OpenThread {
                    server_id,
                    rid,
                    tmid,
                } => self.go(Go::Room {
                    sid: server_id,
                    rid,
                    tmid: Some(tmid),
                }),
                desktop::Request::ShowWindow => self.desktop.show_window().map(Message::Desktop),
                // Unsaved admin settings: the window comes back for the question.
                desktop::Request::Quit if self.admin_dirty() => Task::batch([
                    self.desktop.show_window().map(Message::Desktop),
                    self.go(Go::Quit),
                ]),
                desktop::Request::Quit => self.go(Go::Quit),
                desktop::Request::IdleTime(idle) => self.idle_presence(idle),
                desktop::Request::SetStatus(status) => self.set_status_everywhere(status),
                desktop::Request::RoomStep(step) => self.room_step(step),
                desktop::Request::JoinCall { server_id, call_id } => {
                    self.on_calls(crate::videoconf::Message::Action(
                        server_id,
                        crate::videoconf::CallAction::Join(call_id),
                    ))
                }
                desktop::Request::ForeignSignal(id, signal) => {
                    self.on_calls(crate::videoconf::Message::Notify(
                        crate::videoconf::notify::Msg::Signal(id, signal),
                    ))
                }
            });
        }
        Task::batch(tasks)
    }

    /// `UserPresence:away` / `:online` on connected servers for an idle sample.
    fn idle_presence(&mut self, idle: std::time::Duration) -> Task<Message> {
        let servers: Vec<_> = self
            .servers
            .values()
            .filter(|s| {
                s.session.is_some() && *s.workspace.connection() == ConnectionState::Connected
            })
            .map(|s| (&s.id, &s.workspace))
            .collect();
        let calls = self.desktop.idle_calls(idle, servers);
        let tasks: Vec<_> = calls
            .into_iter()
            .filter_map(|(sid, call)| {
                let handle = self.servers.get(&sid)?.session.clone()?;
                Some(Task::perform(
                    async move { handle.session().call(call.method(), Vec::new()).await },
                    move |r| {
                        if let Err(e) = r {
                            tracing::debug!(error = %e, method = call.method(), "presence call failed");
                        }
                        Message::Noop
                    },
                ))
            })
            .collect();
        Task::batch(tasks)
    }

    /// Tray status menu: `users.setStatus` on every logged-in server.
    fn set_status_everywhere(&mut self, status: rocketchat::models::UserStatus) -> Task<Message> {
        let sids: Vec<ServerId> = self
            .servers
            .values()
            .filter(|s| s.session.is_some())
            .map(|s| s.id.clone())
            .collect();
        let tasks: Vec<_> = sids
            .into_iter()
            .map(|sid| self.on_status_menu(sid, status_menu::Message::SetStatus(status)))
            .collect();
        Task::batch(tasks)
    }

    /// Drops every server's image cache (memory and disk) and the media
    /// opened with the system player.
    fn clear_image_cache(&mut self) -> Task<Message> {
        let mut dirs = Vec::new();
        for s in self.servers.values() {
            dirs.extend(self.images.purge_server(&s.id, &s.url));
        }
        let media = crate::room::timeline::transfer::purge_media_task(
            self.images.disk_dir(),
            false,
            Message::Noop,
        );
        if dirs.is_empty() {
            return media;
        }
        let images = Task::perform(
            async move {
                let _ = tokio::task::spawn_blocking(move || {
                    for d in dirs {
                        let _ = std::fs::remove_dir_all(d);
                    }
                })
                .await;
            },
            |_| Message::Noop,
        );
        Task::batch([images, media])
    }

    /// Unread totals over all servers changed: tell the desktop layer.
    pub(super) fn unread_hook(&mut self) -> Task<Message> {
        let totals = self
            .servers
            .values()
            .map(|s| rustket_core::room_list::unread_totals(&s.workspace))
            .fold((0, 0), |(a, b), (c, d)| (a + c, b + d));
        let logged_in: Vec<_> = self
            .servers
            .values()
            .filter(|s| s.is_logged_in())
            .map(|s| (&s.id, &s.workspace))
            .collect();
        let state = self
            .desktop
            .on_state(&logged_in, self.active.as_ref())
            .map(Message::Desktop);
        if totals == self.last_unread {
            return state;
        }
        self.last_unread = totals;
        Task::batch([
            state,
            self.desktop
                .on_unread_changed(totals.0, totals.1)
                .map(Message::Desktop),
        ])
    }

    pub(super) fn settings_view_data(&self) -> settings::SettingsView<'_> {
        settings::SettingsView {
            config: &self.config,
            themes: &self.themes,
            locales: &self.locales,
            handler_registered: self.handler_registered,
            secret_backend: self.secret_backend,
            secrets_in_memory: self.secrets_mode == SecretsMode::System,
            tray: self.desktop.tray_support(),
            offline_cache: self
                .offline
                .enabled()
                .then_some((self.offline.size, self.offline.clearing)),
            flags: self.flag_accounts(),
            star_kept_busy: !self.star_kept.is_empty(),
            #[cfg(feature = "self-update")]
            updater: crate::updater::view::settings_data(
                &self.updater,
                self.ui_state.update_last_check,
                self.ui_state.update_skipped.as_deref(),
            ),
        }
    }

    /// The signed-in accounts' flags for the Settings, the active server's
    /// first.
    fn flag_accounts(&self) -> Vec<settings::FlagAccount<'_>> {
        let mut servers: Vec<&ServerState> = self
            .servers
            .values()
            .filter(|s| s.workspace.user_id().is_some())
            .collect();
        servers.sort_by_key(|s| (Some(&s.id) != self.active.as_ref(), s.name.to_lowercase()));
        servers
            .into_iter()
            .map(|s| settings::FlagAccount {
                sid: s.id.clone(),
                label: t!(
                    "settings.flags_account",
                    user = s.workspace.username().unwrap_or_default(),
                    server = s.name
                )
                .into_owned(),
                flags: s.workspace.flags(),
                usage: rustket_core::room_list::flag_usage(&s.workspace),
                status: s.flags.status(),
                kept_unstarred: if s.session.is_some() {
                    self.kept_unstarred(&s.id).len()
                } else {
                    0
                },
            })
            .collect()
    }
}
