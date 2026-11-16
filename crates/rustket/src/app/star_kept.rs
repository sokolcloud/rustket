//! The paced star/unstar queue: "Move kept threads to Favorites" (Settings)
//! stars the parents of the signed-in accounts' kept threads, and Later's
//! "Remove done items" unstars messages, one at a time, paced for the
//! `chat.starMessage` / `chat.unStarMessage` rate limits (per IP and route,
//! by default 10 a minute, shared with the user's own stars). Failures are
//! reported together per job when the queue is empty.

use std::collections::{BTreeMap, VecDeque};
use std::time::{Duration, Instant};

use iced::Task;
use rocketchat::models::Message as RcMessage;
use rust_i18n::t;
use rustket_core::ServerId;
use rustket_core::later::{LaterEdit, LaterStatus, ts_ms};
use rustket_ui::widget::BannerKind;

use super::App;
use crate::error::Failure;
use crate::message::Message;
use crate::room::render::plural;

/// Time between two calls of the queue.
pub const STAR_INTERVAL: Duration = Duration::from_secs(7);
/// Pause after a rate-limit response without a reset time.
const RATE_LIMIT_PAUSE: Duration = Duration::from_secs(60);

/// Why a star change is queued.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum StarJob {
    /// Star a kept thread's parent ("Move kept threads to Favorites").
    KeptThread,
    /// Unstar a done Later item ("Remove done items from Later").
    ClearDone,
}

/// One queued change.
#[derive(Debug, Clone)]
struct Queued {
    sid: ServerId,
    rid: String,
    mid: String,
    job: StarJob,
    /// The message unstarred at once when it was queued (Later shows the
    /// change while the call waits its turn).
    shown: Option<Box<RcMessage>>,
}

impl Queued {
    fn same(&self, o: &Queued) -> bool {
        self.sid == o.sid && self.mid == o.mid && self.job == o.job
    }
}

/// Star changes waiting to be made.
#[derive(Debug, Default)]
pub struct StarKept {
    queue: VecDeque<Queued>,
    /// A pacing timer is pending.
    running: bool,
    not_before: Option<Instant>,
    failed: BTreeMap<StarJob, usize>,
}

impl StarKept {
    /// Whether changes of `job` wait (Settings shows its job as busy).
    pub fn has_job(&self, job: StarJob) -> bool {
        self.queue.iter().any(|q| q.job == job)
    }

    /// Queues `q` once. The user's Later removals go before the kept
    /// threads' bulk job, which can take minutes.
    fn push(&mut self, q: Queued) {
        if self.queue.iter().any(|x| x.same(&q)) {
            return;
        }
        if q.job == StarJob::ClearDone {
            let at = self
                .queue
                .iter()
                .position(|x| x.job != StarJob::ClearDone)
                .unwrap_or(self.queue.len());
            self.queue.insert(at, q);
        } else {
            self.queue.push_back(q);
        }
    }

    /// Starts the pacing loop unless it runs.
    fn start(&mut self) -> Task<Message> {
        if self.running || self.queue.is_empty() {
            return Task::none();
        }
        self.running = true;
        Task::done(Message::StarKeptNext)
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
                self.star_kept.push(Queued {
                    sid: sid.clone(),
                    rid,
                    mid: tmid,
                    job: StarJob::KeptThread,
                    shown: None,
                });
            }
        }
        self.star_kept.start()
    }

    /// Queues unstarring `(rid, mid)` of server `sid` (Later's "Remove done
    /// items", "Remove from Later").
    pub(super) fn queue_unstar(
        &mut self,
        sid: &ServerId,
        items: Vec<(String, String)>,
    ) -> Task<Message> {
        for (rid, mid) in items {
            // Unstarred in the view at once; the call reverts a failure.
            let shown = self.servers.get_mut(sid).and_then(|s| {
                let m = s
                    .workspace
                    .later_index()
                    .starred()
                    .find(|m| m.id == mid)
                    .cloned()?;
                s.workspace.set_message_starred(&m, false);
                Some(Box::new(m))
            });
            self.star_kept.push(Queued {
                sid: sid.clone(),
                rid,
                mid,
                job: StarJob::ClearDone,
                shown,
            });
        }
        self.star_kept.start()
    }

    /// The pacing timer: makes the next queued change.
    pub(super) fn on_star_kept_next(&mut self) -> Task<Message> {
        let now = Instant::now();
        if let Some(at) = self.star_kept.not_before.filter(|at| *at > now) {
            return Task::perform(tokio::time::sleep(at - now), |_| Message::StarKeptNext);
        }
        while let Some(q) = self.star_kept.queue.pop_front() {
            let Some(s) = self.servers.get_mut(&q.sid) else {
                continue;
            };
            if s.session.is_none() {
                // Signed out meanwhile: the view shows the star again.
                if let Some(m) = &q.shown {
                    s.workspace.set_message_starred(m, true);
                }
                continue;
            }
            let call = match q.job {
                StarJob::KeptThread => {
                    if s.workspace.thread_index().is_starred(&q.mid) {
                        continue;
                    }
                    self.star_thread_call(&q.sid, q.rid, q.mid, true, true)
                }
                StarJob::ClearDone => {
                    let m = match q.shown {
                        Some(m) => *m,
                        None => {
                            let Some(m) = s
                                .workspace
                                .later_index()
                                .starred()
                                .find(|m| m.id == q.mid)
                                .cloned()
                            else {
                                continue;
                            };
                            m
                        }
                    };
                    self.star_message_call(&q.sid, m, false, Some(q.job))
                }
            };
            self.star_kept.not_before = Some(now + STAR_INTERVAL);
            let next = Task::perform(tokio::time::sleep(STAR_INTERVAL), |_| Message::StarKeptNext);
            return Task::batch([call, next]);
        }
        self.star_kept.running = false;
        self.star_kept.not_before = None;
        let failed = std::mem::take(&mut self.star_kept.failed);
        let tasks: Vec<_> = failed
            .into_iter()
            .filter(|(_, n)| *n > 0)
            .map(|(job, n)| {
                let title = match job {
                    StarJob::KeptThread => t!("sidebar.star_kept_failed", count = n),
                    StarJob::ClearDone => t!(
                        format!("later.clear_done_failed_{}", plural(n as u64)),
                        count = n
                    ),
                };
                self.toast(BannerKind::Error, title, String::new())
            })
            .collect();
        Task::batch(tasks)
    }

    /// Stars or unstars message `m`: Later shows the change right away, the
    /// realtime echo confirms it, a failure reverts it. `job`: queued.
    pub(super) fn star_message_call(
        &mut self,
        sid: &ServerId,
        m: RcMessage,
        on: bool,
        job: Option<StarJob>,
    ) -> Task<Message> {
        let Some(s) = self.servers.get_mut(sid) else {
            return Task::none();
        };
        let Some(rest) = s.session.as_ref().map(|h| h.rest().clone()) else {
            return Task::none();
        };
        s.workspace.set_message_starred(&m, on);
        let (sid, epoch) = (sid.clone(), s.generation);
        let mid = m.id.clone();
        let m = Box::new(m);
        Task::perform(
            async move {
                if on {
                    rest.star_message(&mid).await
                } else {
                    rest.unstar_message(&mid).await
                }
                .map_err(Failure::from)
            },
            move |result| Message::MessageStarred {
                sid: sid.clone(),
                epoch,
                m: m.clone(),
                on,
                job,
                result,
            },
        )
    }

    /// A star change of [`Self::star_message_call`] ended.
    pub(super) fn on_message_starred(
        &mut self,
        sid: ServerId,
        epoch: u64,
        m: RcMessage,
        on: bool,
        job: Option<StarJob>,
        result: Result<(), Failure>,
    ) -> Task<Message> {
        match result {
            Ok(()) => {
                if !on {
                    return self.set_later(&sid, LaterEdit::Forget { mid: m.id });
                }
                Task::none()
            }
            Err(e) => {
                tracing::warn!(server = %sid, on, error = %e, "message star failed");
                if let Some(s) = self.servers.get_mut(&sid)
                    && s.is_current(epoch)
                {
                    s.workspace.set_message_starred(&m, !on);
                }
                if let Some(job) = job {
                    self.star_queue_failed(job, sid, m.rid, m.id, &e);
                    return Task::none();
                }
                self.toast(BannerKind::Error, t!("later.edit_failed"), e.user_text())
            }
        }
    }

    /// A queued star of a kept thread's parent was stored: Later archives it
    /// (moving threads to Favorites is a sidebar matter, not a to-do).
    pub(super) fn star_kept_done(
        &mut self,
        sid: &ServerId,
        rid: &str,
        tmid: &str,
    ) -> Task<Message> {
        let ts = self.servers.get(sid).and_then(|s| {
            s.workspace
                .thread_index()
                .parent(tmid)
                .or_else(|| s.workspace.later_message(tmid))
                .and_then(ts_ms)
        });
        self.set_later(
            sid,
            LaterEdit::Status {
                rid: rid.to_owned(),
                mid: tmid.to_owned(),
                ts_ms: ts,
                status: LaterStatus::Archived,
            },
        )
    }

    /// A queued change failed (already reverted): a rate-limited one is
    /// tried again after the pause, others are counted for one message at
    /// the end.
    pub(super) fn star_queue_failed(
        &mut self,
        job: StarJob,
        sid: ServerId,
        rid: String,
        mid: String,
        e: &Failure,
    ) {
        if let rocketchat::Error::RateLimited { retry_after, .. } = e.error() {
            let wait = retry_after.unwrap_or(RATE_LIMIT_PAUSE);
            self.star_kept.not_before = Some(Instant::now() + wait);
            self.star_kept.queue.push_front(Queued {
                sid,
                rid,
                mid,
                job,
                shown: None,
            });
        } else {
            *self.star_kept.failed.entry(job).or_default() += 1;
        }
    }

    /// A queued star of a kept thread failed ([`Self::star_queue_failed`]).
    pub(super) fn star_kept_failed(
        &mut self,
        sid: ServerId,
        rid: String,
        tmid: String,
        e: &Failure,
    ) {
        self.star_queue_failed(StarJob::KeptThread, sid, rid, tmid, e);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn q(mid: &str, job: StarJob) -> Queued {
        Queued {
            sid: ServerId("s".into()),
            rid: "r".into(),
            mid: mid.into(),
            job,
            shown: None,
        }
    }

    /// Later removals go before the kept threads' bulk job (in their own
    /// order), and only that job makes the Settings button busy.
    #[test]
    fn later_removals_jump_the_bulk_job() {
        let mut k = StarKept::default();
        k.push(q("t1", StarJob::ClearDone));
        assert!(!k.has_job(StarJob::KeptThread));
        k.push(q("k1", StarJob::KeptThread));
        k.push(q("k2", StarJob::KeptThread));
        k.push(q("t2", StarJob::ClearDone));
        k.push(q("t2", StarJob::ClearDone));
        let order: Vec<&str> = k.queue.iter().map(|q| q.mid.as_str()).collect();
        assert_eq!(order, ["t1", "t2", "k1", "k2"]);
        assert!(k.has_job(StarJob::KeptThread));
    }
}
