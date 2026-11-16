//! Work after every update: derived views and image fetches.

use std::time::Instant;

use iced::Task;
use rocketchat::RestClient;
use rustket_core::ServerId;
use rustket_core::config::{SectionId, ThreadsMode, UiPrefs};
use rustket_core::room_list;

use super::{AVATAR_PREFETCH_ROWS, App};
use crate::images;
use crate::message::Message;
use crate::room::UpdateCtx;

/// How the sidebar Threads loader runs: mode, cap and whether the section
/// counts as collapsed, from the prefs. While Activity is open (`activity`)
/// every unread thread is loaded, with the section off, capped or
/// collapsed too.
fn threads_load(
    prefs: &UiPrefs,
    collapsed: bool,
    activity: bool,
) -> (ThreadsMode, crate::workspace::threads::Cap, bool) {
    let mode = match prefs.sidebar_threads {
        ThreadsMode::Off if activity => ThreadsMode::Unread,
        m => m,
    };
    let cap = crate::workspace::threads::Cap {
        unread_all: activity,
        ..crate::workspace::threads::Cap::from_prefs(prefs)
    };
    (mode, cap, collapsed && !activity)
}

impl App {
    /// Applies pending workspace changes to derived views and fetches wanted images.
    pub(super) fn after_update(&mut self) -> Task<Message> {
        let mut tasks = vec![self.sync_room_visibility()];
        if images::set_disk_cap_mb(self.config.ui.image_cache_mb) {
            tasks.push(images::trim_task(self.images.disk_dir(), Message::Noop));
        }
        let active = self.active.clone();
        let sids: Vec<ServerId> = self.servers.keys().cloned().collect();
        let mut effects = Vec::new();
        let mut save_synced = false;
        let mut missed = false;
        let mut dropped: Vec<(String, usize)> = Vec::new();
        let now_ms = chrono::Utc::now().timestamp_millis();
        // The missed message being rescheduled in the dialog.
        let missed_held: Option<(ServerId, String)> = self.modals.iter().find_map(|m| match m {
            super::Modal::Missed(state) => state
                .reschedule
                .as_ref()
                .map(|(sid, id, _)| (sid.clone(), id.clone())),
            _ => None,
        });
        for sid in sids {
            let Some(s) = self.servers.get_mut(&sid) else {
                continue;
            };
            // Scheduled messages whose time came join the outbox (sent
            // below with the queued ones); late ones wait for the user.
            // One with an open edit or reschedule dialog does not go out.
            let held = s
                .drafts_view()
                .and_then(crate::drafts::DraftsView::held_id)
                .map(str::to_owned)
                .or_else(|| {
                    missed_held
                        .as_ref()
                        .filter(|(h, _)| *h == sid)
                        .map(|(_, id)| id.clone())
                });
            s.workspace.hold_scheduled(held.as_deref());
            s.workspace
                .set_scheduled_storage(s.cache.persists(&self.offline));
            missed |= !crate::cache::tick_scheduled(s, now_ms).is_empty();
            dropped.extend(s.workspace.take_dropped_scheduled());
            // E2E: start/feed the manager and decrypt what is shown.
            tasks.push(crate::e2e::after_update(s, self.secrets.as_ref()));
            // Offline cache: open, write changes, outbox, offline reading.
            let last_room = self
                .config
                .server(&sid)
                .and_then(|e| e.last_room.as_deref());
            tasks.push(crate::cache::after_update(s, &self.offline, last_room));
            // Sidebar Threads: mode and kept threads from the config, loads.
            // Activity keeps its override a while after it closes (see
            // `crate::inbox::ACTIVITY_GRACE`).
            let now = Instant::now();
            let activity_open = s
                .inbox_view()
                .is_some_and(|i| i.shows(crate::inbox::InboxKind::Activity));
            let activity = s.inbox.activity_hold(activity_open, now);
            // Feeds of closed inbox views go after a while.
            if s.inbox.release_feeds(s.inbox_view().is_some(), now) {
                s.workspace.release_feeds();
            }
            let threads_collapsed = self.ui_state.collapsed_sections.contains(
                SectionId::Builtin(room_list::SectionKind::Threads)
                    .collapse_key()
                    .as_ref(),
            );
            let (mode, cap, threads_collapsed) =
                threads_load(&self.config.ui, threads_collapsed, activity);
            let kept = self
                .config
                .server(&sid)
                .into_iter()
                .flat_map(|e| e.kept_threads().map(|k| (k.rid.as_str(), k.tmid.as_str())));
            s.workspace.set_thread_config(mode, kept);
            // Starred-message scans serve the starred threads in Favorites
            // and Later, which always tracks stars (`Workspace::scans_starred`
            // is what the loader's `plan` follows, together with the
            // Activity override above).
            s.workspace
                .set_starred_threads(self.config.ui.sidebar_starred_threads);
            s.workspace.set_later_tracking(true);
            // Synced keys (flags, mention groups, Later, reminders): the
            // account's pending edits, one push of every key that has some,
            // persisting them.
            let entry = self.config.server(&sid);
            let uid = entry
                .and_then(|e| e.user_id.as_deref())
                .filter(|_| s.is_logged_in());
            s.flags.follow_account(uid, |u| {
                entry.map(|e| e.pending_blob(u)).unwrap_or_default()
            });
            s.mention_groups.follow_account(uid, |u| {
                entry.map(|e| e.pending_blob(u)).unwrap_or_default()
            });
            s.later.follow_account(uid, |u| {
                entry.map(|e| e.pending_blob(u)).unwrap_or_default()
            });
            s.reminders.follow_account(uid, |u| {
                entry.map(|e| e.pending_blob(u)).unwrap_or_default()
            });
            // First use of Later on this account: stars made before now
            // count as archived (`rustket_core::later::Seed`).
            if s.later.needs_seed(&s.workspace) {
                let (device, created) = self.config.ensure_device_id();
                save_synced |= created;
                let seed = rustket_core::later::LaterEdit::Seed {
                    at_ms: chrono::Utc::now().timestamp_millis(),
                };
                if let Err(e) = s.later.edit(&mut s.workspace, &seed, &device, now) {
                    tracing::warn!(server = %sid, error = %e, "Later seed refused");
                }
            }
            let task = s.flags.tick(&mut s.workspace, s.session.as_ref(), now);
            let (fsid, epoch) = (sid.clone(), s.generation);
            tasks.push(task.map(move |m| Message::Flags(fsid.clone(), epoch, m)));
            let task = s
                .mention_groups
                .tick(&mut s.workspace, s.session.as_ref(), now);
            let gsid = sid.clone();
            tasks.push(task.map(move |m| Message::MentionGroups(gsid.clone(), epoch, m)));
            let task = s.later.tick(&mut s.workspace, s.session.as_ref(), now);
            let lsid = sid.clone();
            tasks.push(task.map(move |m| Message::LaterSync(lsid.clone(), epoch, m)));
            let task = s.reminders.tick(&mut s.workspace, s.session.as_ref(), now);
            let rsid = sid.clone();
            tasks.push(task.map(move |m| Message::ReminderSync(rsid.clone(), epoch, m)));
            if let Some(task) = s.start_service_push(now) {
                let psid = sid.clone();
                tasks.push(task.map(move |o| Message::ServicePushed(psid.clone(), epoch, o)));
            }
            if let Some((uid, blob)) = s.flags.take_save()
                && let Some(e) = self.config.server_mut(&sid)
            {
                save_synced |= e.set_pending_blob(&uid, &blob);
            }
            if let Some((uid, blob)) = s.mention_groups.take_save()
                && let Some(e) = self.config.server_mut(&sid)
            {
                save_synced |= e.set_pending_blob(&uid, &blob);
            }
            if let Some((uid, blob)) = s.later.take_save()
                && let Some(e) = self.config.server_mut(&sid)
            {
                save_synced |= e.set_pending_blob(&uid, &blob);
            }
            if let Some((uid, blob)) = s.reminders.take_save()
                && let Some(e) = self.config.server_mut(&sid)
            {
                save_synced |= e.set_pending_blob(&uid, &blob);
            }
            let task = s.threads.tick(
                &s.workspace,
                s.session.as_ref(),
                mode,
                cap,
                threads_collapsed,
                Instant::now(),
            );
            let (tsid, epoch) = (sid.clone(), s.generation);
            tasks.push(task.map(move |m| Message::Threads(tsid.clone(), epoch, m)));
            // Shown while it runs and the section is expanded.
            let progress = s
                .threads
                .progress()
                .filter(|_| !threads_collapsed && !s.workspace.from_cache());
            s.sidebar.set_thread_progress(progress);
            let active_thread = s
                .active_room
                .as_ref()
                .and_then(|r| s.rooms.get(r))
                .and_then(|rv| rv.panel.thread_tmid());
            s.sidebar.track_active(
                &s.workspace,
                s.active_room.as_deref(),
                active_thread,
                &self.config.ui,
            );
            s.sidebar.refresh(
                &s.workspace,
                &self.config.ui,
                &self.images,
                &s.id,
                &s.url,
                AVATAR_PREFETCH_ROWS,
            );
            tasks.push(self.tick_inbox(&sid));
            tasks.push(self.tick_later(&sid));
            tasks.push(self.tick_search(&sid));
            tasks.push(self.tick_hosted_user(&sid, super::hosted_user::Host::Inbox));
            tasks.push(self.tick_hosted_user(&sid, super::hosted_user::Host::Later));
            let Some(s) = self.servers.get_mut(&sid) else {
                continue;
            };
            if s.workspace.pending_changes().is_empty() {
                continue;
            }
            let changes = s.workspace.take_changes();
            // Close views of rooms that disappeared (kicked, left, deleted).
            let gone: Vec<String> = s
                .rooms
                .keys()
                .filter(|rid| s.workspace.room(rid).is_none())
                .cloned()
                .collect();
            for rid in gone {
                if let Some(rv) = s.rooms.remove(&rid) {
                    tasks.push(rv.close(&mut s.workspace, s.session.as_ref()).discard());
                    crate::uikit::controller::room_closed(&mut s.workspace, &rid);
                }
                if s.active_room.as_deref() == Some(rid.as_str()) {
                    s.active_room = None;
                }
                if let Some(e) = self.config.server_mut(&sid)
                    && e.last_room.as_deref() == Some(rid.as_str())
                {
                    e.last_room = None;
                }
            }
            let epoch = s.generation;
            for (rid, rv) in s.rooms.iter_mut() {
                let mut ctx = UpdateCtx {
                    server_id: &s.id,
                    server: &s.url,
                    rid,
                    workspace: &mut s.workspace,
                    session: s.session.as_ref(),
                    images: &self.images,
                    prefs: &self.config.ui,
                };
                let mut action = rv.on_changes(&changes, &mut ctx);
                // Mark the room read when new messages arrive while it is
                // readable (not when it was marked unread elsewhere) ...
                if (changes.rooms.contains(rid) || changes.timelines.contains(rid))
                    && rv.wants_auto_read(ctx.workspace)
                {
                    action.task = Task::batch([action.task, rv.mark_read(&ctx)]);
                }
                // ... and its open thread (`tunread`) when new replies arrive
                // while it is shown.
                if rv.is_visible() && changes.rooms.contains(rid) {
                    action.task = Task::batch([action.task, rv.mark_thread_read(&ctx)]);
                }
                tasks.push(Self::room_task(&sid, epoch, rid, action.task));
                for e in action.effects {
                    effects.push((sid.clone(), rid.clone(), e));
                }
            }
        }
        for (sid, rid, e) in effects {
            tasks.push(self.handle_effects(&sid, &rid, vec![e]));
        }
        // Avatar working set of all servers: every row of the active server,
        // the prefetched rows of the others, plus room views.
        let working: usize = self
            .servers
            .iter()
            .map(|(id, s)| {
                let rooms = s.workspace.rooms().len();
                let rows = if active.as_ref() == Some(id) {
                    rooms
                } else {
                    rooms.min(AVATAR_PREFETCH_ROWS)
                };
                rows + 64
            })
            .sum();
        self.images.set_working_set(working);
        tasks.push(self.fetch_images());
        tasks.push(crate::videoconf::fetch_wanted(self));
        tasks.push(self.reminders_tick());
        if missed {
            self.missed_scheduled();
        }
        // Removed elsewhere (sent, deleted, rescheduled): nothing to ask.
        if self
            .modals
            .iter()
            .any(|m| matches!(m, super::Modal::Missed(_)))
            && self.missed_items().is_empty()
        {
            self.modals
                .retain(|m| !matches!(m, super::Modal::Missed(_)));
        }
        for (room, count) in dropped {
            tasks.push(self.toast(
                rustket_ui::widget::BannerKind::Warning,
                rust_i18n::t!("scheduled.dropped_title"),
                rust_i18n::t!("scheduled.dropped_body", room = room, count = count),
            ));
        }
        tasks.push(self.quiet_tick());
        if save_synced {
            tasks.push(self.save_config());
        }
        Task::batch(tasks)
    }

    fn fetch_images(&mut self) -> Task<Message> {
        let mut tasks = Vec::new();
        let servers = &self.servers;
        let wanted = self.images.take_wanted(|r| {
            servers
                .get(&r.server_id)
                .is_some_and(|s| s.url.is_credentialed_asset(&r.url))
        });
        for req in wanted {
            let generation = self.images.generation(&req.server_id);
            let Some(s) = self.servers.get(&req.server_id) else {
                self.images.finish(
                    &req.server_id,
                    generation,
                    req.url,
                    Err("server gone".into()),
                );
                continue;
            };
            let rest = match &s.session {
                Some(h) => h.rest().clone(),
                None => match RestClient::new(s.url.clone(), &s.tls) {
                    Ok(r) => r,
                    Err(e) => {
                        self.images
                            .finish(&req.server_id, generation, req.url, Err(e.to_string()));
                        continue;
                    }
                },
            };
            let dir = self.images.server_dir(&req.server_id);
            let (sid, url) = (req.server_id.clone(), req.url.clone());
            tasks.push(Task::perform(images::fetch(rest, dir, req.url), move |r| {
                Message::ImageLoaded(sid.clone(), generation, url.clone(), r)
            }));
        }
        Task::batch(tasks)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Activity open: the Threads loader loads every unread thread, with
    /// the section off or collapsed; closed, the prefs rule.
    #[test]
    fn activity_loads_every_unread_thread() {
        let prefs = UiPrefs {
            sidebar_threads: ThreadsMode::Off,
            ..UiPrefs::default()
        };
        let (mode, cap, collapsed) = threads_load(&prefs, true, false);
        assert_eq!(mode, ThreadsMode::Off);
        assert!(!cap.unread_all && collapsed);
        let (mode, cap, collapsed) = threads_load(&prefs, true, true);
        assert_eq!(mode, ThreadsMode::Unread);
        assert!(cap.unread_all && !collapsed);
        let following = UiPrefs {
            sidebar_threads: ThreadsMode::Following,
            ..UiPrefs::default()
        };
        assert_eq!(
            threads_load(&following, false, true).0,
            ThreadsMode::Following
        );
    }
}
