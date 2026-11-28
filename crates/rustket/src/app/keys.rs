//! App-wide keyboard shortcuts (`crate::keymap`), the Escape chain of the
//! server view, and "Mark all as read" (Shift+Esc) through the read queue
//! (`crate::reads`).

use iced::Task;
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::reads::{Done, ReadReq};
use rustket_core::room_list;
use rustket_ui::widget::BannerKind;

use super::{App, Modal, Screen};
use crate::component::Nav;
use crate::keymap::Shortcut;
use crate::message::Message;
use crate::reads::Origin;
use crate::room;

impl App {
    pub(super) fn on_shortcut(&mut self, s: Shortcut) -> Task<Message> {
        match s {
            // The listener hears these chords even while a dialog has the
            // keyboard: they act only with nothing over the screen (Ctrl+K
            // with the quick switcher open stays a no-op, as before).
            Shortcut::QuickSwitcher => {
                if self.overlay_shown() {
                    return Task::none();
                }
                self.navigate(Nav::QuickSwitcher)
            }
            Shortcut::CheatSheet => {
                if matches!(self.modals.last(), Some(Modal::Shortcuts)) {
                    self.modals.pop();
                    return Task::none();
                }
                self.navigate(Nav::Shortcuts)
            }
            Shortcut::Settings => {
                let away = matches!(
                    self.screen,
                    Screen::Settings(_)
                        | Screen::Loading
                        | Screen::AddServer(_)
                        | Screen::NoServers
                );
                if away || self.overlay_shown() {
                    return Task::none();
                }
                self.navigate(Nav::Settings)
            }
            Shortcut::MarkAllRead => self.ask_read_all(),
            Shortcut::ReactLast => self.react_last(),
            Shortcut::AllUnreads => self.inbox_shortcut(crate::inbox::InboxKind::Unreads),
            Shortcut::Activity => self.inbox_shortcut(crate::inbox::InboxKind::Activity),
            // Reserved: `keymap::global` does not produce it yet.
            Shortcut::Later => self.later_shortcut(),
            Shortcut::Drafts => self.drafts_shortcut(),
            Shortcut::Search => self.search_shortcut(),
        }
    }

    /// Whether a macOS main menu pick may open settings page `nav`. AppKit
    /// takes ⌘, for the menu before the app's listener sees it, so the menu
    /// keeps the shortcut's rules: not while starting up or under a dialog,
    /// and "Settings…" not again on the settings screen. Unlike the
    /// shortcut it works on the add-server screen, so About and Help are
    /// reachable before the first server is added. Nothing while the terms
    /// screen or the policy's blocking screen covers the window.
    pub(super) fn menu_may_navigate(&self, nav: &Nav) -> bool {
        if matches!(self.screen, Screen::Loading) || self.overlay_shown() || !self.accounts_live() {
            return false;
        }
        !(*nav == Nav::Settings && matches!(self.screen, Screen::Settings(_)))
    }

    /// A dialog or overlay is on screen: an app modal, or the screen's own
    /// (certificate prompt, E2E password, login code, UIKit modal). The same
    /// check the view makes, so the two cannot drift apart.
    fn overlay_shown(&self) -> bool {
        self.overlay().0.is_some()
    }

    /// The active server's view with nothing over it (no dialog, page or
    /// login): the server, its active room and its view.
    fn room_on_screen(&self) -> Option<(ServerId, u64, String)> {
        if !matches!(self.screen, Screen::Server) || !self.modals.is_empty() {
            return None;
        }
        let s = self.active.as_ref().and_then(|sid| self.servers.get(sid))?;
        if s.login.is_some() || s.room_area_taken() || crate::e2e::view::modal(s).is_some() {
            return None;
        }
        let rid = s.active_room.clone().filter(|r| s.rooms.contains_key(r))?;
        Some((s.id.clone(), s.generation, rid))
    }

    /// Ctrl/Cmd+Shift+\ with no composer focused: react to the newest
    /// message of the open room (a focused composer handles the chord for
    /// its own timeline).
    fn react_last(&mut self) -> Task<Message> {
        let Some((sid, epoch, rid)) = self.room_on_screen() else {
            return Task::none();
        };
        Task::done(Message::Room(
            sid,
            epoch,
            rid,
            room::Message::Composer(room::composer::Message::ReactLast),
        ))
    }

    /// Escape no widget took, on the server view. In order:
    /// 1. a view in place of the room area closes (`MainView`, the contact
    ///    center);
    /// 2. the room timeline's message selection is cleared;
    /// 3. the right panel closes;
    /// 4. an unread room is marked read with its threads (Slack;
    ///    `UiPrefs::escape_marks_read`; Rocket.Chat marks it on Escape too),
    ///    unless the composer's emoji picker, upload or confirmation dialog
    ///    is open or it holds an edit or quotes.
    pub(super) fn escape_on_server(&mut self) -> Task<Message> {
        let Some(s) = self.active.as_ref().and_then(|sid| self.servers.get(sid)) else {
            return Task::none();
        };
        let (sid, epoch) = (s.id.clone(), s.generation);
        if s.main_view.is_some() {
            if let Some(s) = self.servers.get_mut(&sid) {
                s.close_main_view();
            }
            return Task::none();
        }
        if s.omni.contact_center.is_some() {
            return Task::done(Message::Omni(
                sid,
                epoch,
                crate::omnichannel::Message::ContactCenter(
                    crate::omnichannel::contact_center::Msg::Close,
                ),
            ));
        }
        let Some(rid) = s.active_room.clone() else {
            return Task::none();
        };
        let Some(rv) = s.rooms.get(&rid) else {
            return Task::none();
        };
        if rv.picking() {
            return Task::done(Message::Room(
                sid,
                epoch,
                rid,
                room::Message::Timeline(room::timeline::Message::ClearPicks),
            ));
        }
        if rv.panel.kind().is_some() {
            return Task::done(Message::Room(
                sid,
                epoch,
                rid,
                room::Message::Panel(room::panel::Message::Close),
            ));
        }
        // The subscription itself, not the sidebar's unread state: "Hide
        // unread status" only hides the badge, the room still waits for its
        // first unread message (Rocket.Chat marks it read on Esc too).
        let unread = s
            .workspace
            .subscription(&rid)
            .is_some_and(|sub| sub.alert || sub.unread > 0 || sub.has_unread_threads());
        // An edit or a quote waits in the composer (its Escape cancels it
        // once the composer has the focus).
        let busy = rv.composer.is_picker_open()
            || rv.composer.is_upload_dialog_open()
            || rv.composer.is_confirm_open()
            || rv.composer.is_editing()
            || !rv.composer.quoted().is_empty();
        if !self.config.ui.escape_marks_read || !unread || busy || s.room_area_taken() {
            return Task::none();
        }
        Task::done(Message::Room(sid, epoch, rid, room::Message::MarkAllRead))
    }

    // --- "Mark all as read" ----------------------------------------------------------

    /// Shift+Esc: asks to mark every unread room of the active server read
    /// (the rooms are fixed when the dialog opens, so the count it shows is
    /// what is sent).
    pub(super) fn ask_read_all(&mut self) -> Task<Message> {
        if !matches!(self.screen, Screen::Server) || self.overlay_shown() {
            return Task::none();
        }
        let Some(s) = self.active.as_ref().and_then(|sid| self.servers.get(sid)) else {
            return Task::none();
        };
        if s.login.is_some() {
            return Task::none();
        }
        if s.session.is_none() {
            return self.toast(BannerKind::Warning, t!("connection.offline"), String::new());
        }
        let rids = room_list::read_all_targets(&s.workspace);
        if rids.is_empty() {
            return self.toast(
                BannerKind::Info,
                t!("shortcuts.read_all_nothing"),
                String::new(),
            );
        }
        let server_id = s.id.clone();
        let stamp = self.next_stamp();
        self.modals.push(Modal::ReadAll {
            server_id,
            rids,
            busy: false,
            done: 0,
            failed: 0,
            stamp,
        });
        Task::none()
    }

    /// "Mark all as read" confirmed: every room goes into the read queue.
    pub(super) fn confirm_read_all(&mut self) -> Task<Message> {
        let Some(Modal::ReadAll {
            server_id,
            rids,
            busy,
            stamp,
            ..
        }) = self.modals.last_mut()
        else {
            return Task::none();
        };
        if *busy {
            return Task::none();
        }
        *busy = true;
        let (sid, stamp) = (server_id.clone(), *stamp);
        let rids = rids.clone();
        let Some(s) = self.servers.get_mut(&sid) else {
            self.modals.pop();
            return Task::none();
        };
        for rid in rids {
            s.reads
                .push(ReadReq::Room { rid, threads: true }, Origin::ReadAll(stamp));
        }
        self.pump_reads(&sid)
    }

    /// Runs the next step of server `sid`'s read queue.
    pub(super) fn pump_reads(&mut self, sid: &ServerId) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let (task, done) = crate::reads::pump(&mut s.reads, s.session.as_ref());
        let task = Self::reads_task(sid, s.generation, task);
        Task::batch([task, self.reads_done(sid, done)])
    }

    pub(super) fn on_reads(
        &mut self,
        sid: ServerId,
        epoch: u64,
        m: crate::reads::Msg,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(&sid).filter(|s| s.is_current(epoch)) else {
            return Task::none();
        };
        let (task, done) = crate::reads::update(&mut s.reads, m, s.session.as_ref());
        let task = Self::reads_task(&sid, epoch, task);
        Task::batch([task, self.reads_done(&sid, done)])
    }

    fn reads_task(sid: &ServerId, epoch: u64, task: Task<crate::reads::Msg>) -> Task<Message> {
        let sid = sid.clone();
        task.map(move |m| Message::Reads(sid.clone(), epoch, m))
    }

    /// Hands finished read marks to whoever asked for them.
    fn reads_done(&mut self, sid: &ServerId, done: Vec<Done<Origin>>) -> Task<Message> {
        let mut tasks = Vec::new();
        for d in done {
            for origin in &d.origins {
                match origin {
                    Origin::ReadAll(stamp) => tasks.push(self.read_all_progress(*stamp, d.ok)),
                    Origin::Inbox => tasks.push(self.inbox_read_done(sid, &d.req, d.ok)),
                }
            }
        }
        Task::batch(tasks)
    }

    /// One room of "Mark all as read" run `stamp` ended; the last one closes
    /// the dialog (with an error toast when some failed).
    fn read_all_progress(&mut self, stamp: u64, ok: bool) -> Task<Message> {
        let Some(pos) = self
            .modals
            .iter()
            .position(|m| matches!(m, Modal::ReadAll { stamp: s, .. } if *s == stamp))
        else {
            return Task::none();
        };
        let Modal::ReadAll {
            rids, done, failed, ..
        } = &mut self.modals[pos]
        else {
            return Task::none();
        };
        if ok {
            *done += 1;
        } else {
            *failed += 1;
        }
        let (total, failed) = (rids.len(), *failed);
        if *done + failed < total {
            return Task::none();
        }
        self.modals.remove(pos);
        tracing::debug!(total, failed, "mark all as read finished");
        if failed == 0 {
            return Task::none();
        }
        let key = format!(
            "shortcuts.read_all_failed_{}",
            crate::room::render::plural(total as u64)
        );
        self.toast(
            BannerKind::Error,
            t!(key, failed = failed, count = total),
            String::new(),
        )
    }

    /// A running "Mark all as read" whose server signed out (its queue was
    /// cleared) or went away: the dialog closes.
    pub(super) fn drop_orphaned_read_all(&mut self) {
        let servers = &self.servers;
        self.modals.retain(|m| match m {
            Modal::ReadAll {
                server_id,
                busy: true,
                stamp,
                ..
            } => servers
                .get(server_id)
                .is_some_and(|s| s.reads.has_origin(&Origin::ReadAll(*stamp))),
            _ => true,
        });
    }
}
