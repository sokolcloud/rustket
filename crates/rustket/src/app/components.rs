//! Routing of app-level components (settings, account, directory, dialogs,
//! status menu, desktop integration) and their effects.

use iced::Task;
use rustket_core::ServerId;
use rustket_core::workspace::ConnectionState;

use super::{App, Modal, Screen, SecretsMode};
use crate::component::{self, Ctx, Effect, Nav};
use crate::desktop;
use crate::dialogs::{create_room, quick_switcher};
use crate::message::Message;
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
        let sid = self.active.clone();
        match (nav, sid) {
            (Nav::Settings, _) => {
                // The real autostart directory only with real desktop integration.
                let home = self
                    .desktop
                    .is_enabled()
                    .then(crate::desktop::autostart::config_home)
                    .flatten();
                let screen = settings::Settings::with_autostart_home(home);
                let load = screen.load().map(Message::Settings);
                self.screen = Screen::Settings(Box::new(screen));
                load
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
            (Nav::CreateRoom(kind), Some(sid)) => {
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
                    Some(sid) => {
                        let a = self.activate(sid.clone());
                        Task::batch([a, self.open_room(sid, &rid)])
                    }
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
                    Some(sid) => self.logout(sid, everywhere),
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
                Effect::TestNotification => self
                    .desktop
                    .test_notification(&self.config.ui)
                    .map(Message::Desktop),
                Effect::RecentStatusTexts(list) => {
                    self.config.set_recent_status_texts(list);
                    self.save_config()
                }
            });
        }
        Task::batch(tasks)
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
        let task = a.task.map(move |m| Message::CreateRoom(s2.clone(), m));
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
        let task = a.task.map(move |m| Message::QuickSwitcher(s2.clone(), m));
        Task::batch([
            task,
            self.component_effects(Some(&sid), Source::QuickSwitcher, a.effects),
        ])
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
                desktop::Request::OpenRoom { server_id, rid } => {
                    let a = self.activate(server_id.clone());
                    Task::batch([a, self.open_room(&server_id, &rid)])
                }
                desktop::Request::OpenThread {
                    server_id,
                    rid,
                    tmid,
                } => {
                    let a = self.activate(server_id.clone());
                    let open = self.open_room(&server_id, &rid);
                    if let Some(rv) = self
                        .servers
                        .get_mut(&server_id)
                        .and_then(|s| s.rooms.get_mut(&rid))
                    {
                        let kind = crate::room::panel::Kind::Thread(tmid);
                        if rv.panel.kind() != Some(&kind) {
                            rv.panel.open(kind);
                        }
                    }
                    Task::batch([a, open])
                }
                desktop::Request::ShowWindow => self.desktop.show_window().map(Message::Desktop),
                desktop::Request::Quit => {
                    self.flush_config();
                    iced::exit()
                }
                desktop::Request::IdleTime(idle) => self.idle_presence(idle),
                desktop::Request::SetStatus(status) => self.set_status_everywhere(status),
                desktop::Request::RoomStep(step) => self.room_step(step),
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

    /// Drops every server's image cache (memory and disk).
    fn clear_image_cache(&mut self) -> Task<Message> {
        let mut dirs = Vec::new();
        for s in self.servers.values() {
            dirs.extend(self.images.purge_server(&s.id, &s.url));
        }
        if dirs.is_empty() {
            return Task::none();
        }
        Task::perform(
            async move {
                let _ = tokio::task::spawn_blocking(move || {
                    for d in dirs {
                        let _ = std::fs::remove_dir_all(d);
                    }
                })
                .await;
            },
            |_| Message::Noop,
        )
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
            handler_registered: self.handler_registered,
            secret_backend: self.secret_backend,
            secrets_in_memory: self.secrets_mode == SecretsMode::System,
            tray: self.desktop.tray_support(),
        }
    }
}
