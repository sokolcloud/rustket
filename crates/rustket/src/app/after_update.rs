//! Work after every update: derived views and image fetches.

use std::time::Instant;

use iced::Task;
use rocketchat::RestClient;
use rustket_core::ServerId;
use rustket_core::config::SectionId;
use rustket_core::room_list;

use super::{AVATAR_PREFETCH_ROWS, App};
use crate::images;
use crate::message::Message;
use crate::room::UpdateCtx;

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
        for sid in sids {
            let Some(s) = self.servers.get_mut(&sid) else {
                continue;
            };
            // E2E: start/feed the manager and decrypt what is shown.
            tasks.push(crate::e2e::after_update(s, self.secrets.as_ref()));
            // Offline cache: open, write changes, outbox, offline reading.
            let last_room = self
                .config
                .server(&sid)
                .and_then(|e| e.last_room.as_deref());
            tasks.push(crate::cache::after_update(s, &self.offline, last_room));
            // Sidebar Threads: mode and kept threads from the config, loads.
            let mode = self.config.ui.sidebar_threads;
            let kept = self
                .config
                .server(&sid)
                .into_iter()
                .flat_map(|e| e.kept_threads().map(|k| (k.rid.as_str(), k.tmid.as_str())));
            s.workspace.set_thread_config(mode, kept);
            s.workspace
                .set_starred_threads(self.config.ui.sidebar_starred_threads);
            // Synced keys (flags, mention groups): the account's pending
            // edits, one push of every key that has some, persisting them.
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
            let now = Instant::now();
            let task = s.flags.tick(&mut s.workspace, s.session.as_ref(), now);
            let (fsid, epoch) = (sid.clone(), s.generation);
            tasks.push(task.map(move |m| Message::Flags(fsid.clone(), epoch, m)));
            let task = s
                .mention_groups
                .tick(&mut s.workspace, s.session.as_ref(), now);
            let gsid = sid.clone();
            tasks.push(task.map(move |m| Message::MentionGroups(gsid.clone(), epoch, m)));
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
            let threads_collapsed = self.ui_state.collapsed_sections.contains(
                SectionId::Builtin(room_list::SectionKind::Threads)
                    .collapse_key()
                    .as_ref(),
            );
            let task = s.threads.tick(
                &s.workspace,
                s.session.as_ref(),
                mode,
                crate::workspace::threads::Cap::from_prefs(&self.config.ui),
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
