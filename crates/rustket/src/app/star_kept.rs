//! "Move kept threads to Favorites" (Settings): stars the parents of the
//! signed-in accounts' kept threads one at a time, paced for the
//! `chat.starMessage` rate limit (per IP and route, by default 10 a minute,
//! shared with the user's own stars). The threads stay kept.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use iced::Task;
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_ui::widget::BannerKind;

use super::App;
use crate::error::Failure;
use crate::message::Message;

/// Time between two stars of the queue.
pub const STAR_INTERVAL: Duration = Duration::from_secs(7);
/// Pause after a rate-limit response without a reset time.
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(60);

/// Kept threads waiting to be starred.
#[derive(Debug, Default)]
pub struct StarKept {
    /// `(server, rid, tmid)`, in order.
    queue: VecDeque<(ServerId, String, String)>,
    /// A pacing timer is pending.
    running: bool,
    not_before: Option<Instant>,
    failed: usize,
}

impl StarKept {
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}

impl App {
    /// Kept threads of signed-in account `sid` whose parent is not starred
    /// (as far as the sidebar knows: the starred-threads switch is on).
    pub(super) fn kept_unstarred(&self, sid: &ServerId) -> Vec<(String, String)> {
        let (Some(s), Some(entry)) = (self.servers.get(sid), self.config.server(sid)) else {
            return Vec::new();
        };
        let idx = s.workspace.thread_index();
        entry
            .kept_threads()
            .filter(|k| !idx.is_starred(&k.tmid) && s.workspace.subscription(&k.rid).is_some())
            .map(|k| (k.rid.clone(), k.tmid.clone()))
            .collect()
    }

    /// Queues the kept threads of every signed-in account with a session.
    pub(super) fn star_kept_threads(&mut self) -> Task<Message> {
        let sids: Vec<ServerId> = self
            .servers
            .values()
            .filter(|s| s.session.is_some())
            .map(|s| s.id.clone())
            .collect();
        for sid in sids {
            for (rid, tmid) in self.kept_unstarred(&sid) {
                let item = (sid.clone(), rid, tmid);
                if !self.star_kept.queue.contains(&item) {
                    self.star_kept.queue.push_back(item);
                }
            }
        }
        if self.star_kept.running || self.star_kept.queue.is_empty() {
            return Task::none();
        }
        self.star_kept.running = true;
        Task::done(Message::StarKeptNext)
    }

    /// The pacing timer: stars the next queued thread.
    pub(super) fn on_star_kept_next(&mut self) -> Task<Message> {
        let now = Instant::now();
        if let Some(at) = self.star_kept.not_before.filter(|at| *at > now) {
            return Task::perform(tokio::time::sleep(at - now), |_| Message::StarKeptNext);
        }
        while let Some((sid, rid, tmid)) = self.star_kept.queue.pop_front() {
            let Some(s) = self.servers.get(&sid) else {
                continue;
            };
            if s.session.is_none() || s.workspace.thread_index().is_starred(&tmid) {
                continue;
            }
            let star = self.star_thread_call(&sid, rid, tmid, true, true);
            self.star_kept.not_before = Some(now + STAR_INTERVAL);
            let next = Task::perform(tokio::time::sleep(STAR_INTERVAL), |_| Message::StarKeptNext);
            return Task::batch([star, next]);
        }
        self.star_kept.running = false;
        self.star_kept.not_before = None;
        let failed = std::mem::take(&mut self.star_kept.failed);
        if failed == 0 {
            return Task::none();
        }
        self.toast(
            BannerKind::Error,
            t!("sidebar.star_kept_failed", count = failed),
            String::new(),
        )
    }

    /// A queued star failed (already reverted): a rate-limited one is tried
    /// again after the pause, others are counted for one message at the end.
    pub(super) fn star_kept_failed(
        &mut self,
        sid: ServerId,
        rid: String,
        tmid: String,
        e: &Failure,
    ) {
        if let rocketchat::Error::RateLimited { retry_after, .. } = e.error() {
            let wait = retry_after.unwrap_or(RATE_LIMIT_PAUSE);
            self.star_kept.not_before = Some(Instant::now() + wait);
            self.star_kept.queue.push_front((sid, rid, tmid));
        } else {
            self.star_kept.failed += 1;
        }
    }
}
