use rocketchat::RcDate;
use rocketchat::models::{Message, Paginated};
use rocketchat::rest::HistoryCursor;

use crate::timeline::{DeliveryState, PageTicket, Timeline, TimelineItem};

use super::{Dirty, Workspace};

impl Workspace {
    // --- timelines ----------------------------------------------------------------

    /// Opens (or re-opens) a room timeline, captures the subscription's `ls` for
    /// the unread divider and restores the room's pending messages.
    pub fn open_room(&mut self, rid: &str) -> &Timeline {
        let ls = self.subscription(rid).and_then(|s| s.ls);
        let t = self
            .timelines
            .entry(rid.to_owned())
            .or_insert_with(|| Timeline::new_room(rid));
        t.set_unread_since(ls);
        for item in self.outbox.values() {
            if t.accepts(&item.message) {
                t.restore_pending(item.clone());
            }
        }
        self.mark_timeline(rid);
        &self.timelines[rid]
    }

    /// Moves the unread divider of an open room to the subscription's
    /// current `ls` (the room became unread while nobody looked at it).
    pub fn refresh_unread_since(&mut self, rid: &str) {
        let ls = self.subscription(rid).and_then(|s| s.ls);
        let Some(t) = self.timelines.get_mut(rid) else {
            return;
        };
        if t.unread_since() != ls {
            t.set_unread_since(ls);
            self.mark_timeline(rid);
        }
    }

    /// Reloads a room's history from scratch (e.g. `MessagesImported`): the
    /// open room and thread timelines keep only pending messages and the next
    /// `begin_load_older` / `begin_load_thread` starts over.
    pub fn reset_room_timelines(&mut self, rid: &str) {
        self.ephemeral.remove(rid);
        if let Some(t) = self.timelines.get_mut(rid) {
            t.reset();
            self.mark_timeline(rid);
        }
        let threads: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, t)| t.rid() == rid)
            .map(|(k, _)| k.clone())
            .collect();
        for tmid in threads {
            if let Some(t) = self.threads.get_mut(&tmid) {
                t.reset();
            }
            self.mark_thread(&tmid);
        }
    }

    /// Forgets a room timeline (memory); it is reloaded on the next open.
    /// Pending messages survive in the outbox.
    pub fn close_room(&mut self, rid: &str) {
        self.ephemeral.remove(rid);
        if self.timelines.remove(rid).is_some() {
            self.mark_timeline(rid);
        }
    }

    /// Opens a thread timeline; the parent is taken from the room timeline
    /// (else the thread index) if known.
    pub fn open_thread(&mut self, rid: &str, tmid: &str) -> &Timeline {
        let parent = self
            .timelines
            .get(rid)
            .and_then(|t| t.get(tmid))
            .map(|i| i.message.clone())
            .or_else(|| {
                self.thread_index
                    .parent(tmid)
                    .filter(|p| p.rid == rid)
                    .cloned()
            });
        let t = self
            .threads
            .entry(tmid.to_owned())
            .or_insert_with(|| Timeline::new_thread(rid, tmid));
        if let Some(p) = parent.as_ref() {
            t.upsert(p.clone());
        }
        for item in self.outbox.values() {
            if t.accepts(&item.message) {
                t.restore_pending(item.clone());
            }
        }
        self.mark_thread(tmid);
        if let Some(p) = parent {
            self.index_parents(&[p]);
        }
        &self.threads[tmid]
    }

    pub fn close_thread(&mut self, tmid: &str) {
        if self.threads.remove(tmid).is_some() {
            self.mark_thread(tmid);
        }
    }

    /// See [`Timeline::begin_load_older`]. Prefer [`Self::begin_load_older_ticket`]:
    /// without a ticket a page requested before a reset or a close/reopen cannot
    /// always be told apart from the current one.
    pub fn begin_load_older(&mut self, rid: &str) -> Option<HistoryCursor> {
        self.begin_load_older_ticket(rid).map(|(c, _)| c)
    }

    /// Starts a history request; pass the ticket to [`Self::finish_load_older_ticket`].
    pub fn begin_load_older_ticket(&mut self, rid: &str) -> Option<(HistoryCursor, PageTicket)> {
        let c = self.timelines.get_mut(rid)?.begin_load_older_ticket();
        if c.is_some() {
            self.mark_timeline(rid);
        }
        c
    }

    /// Completes a history request (dropped when none is outstanding).
    pub fn finish_load_older(
        &mut self,
        rid: &str,
        cursor: HistoryCursor,
        result: Result<Vec<Message>, String>,
    ) {
        self.finish_older(rid, None, cursor, result);
    }

    /// Completes [`Self::begin_load_older_ticket`]; a stale result (the timeline
    /// was reset, closed or replaced meanwhile) is dropped. Returns whether it
    /// was applied.
    pub fn finish_load_older_ticket(
        &mut self,
        rid: &str,
        ticket: PageTicket,
        cursor: HistoryCursor,
        result: Result<Vec<Message>, String>,
    ) -> bool {
        self.finish_older(rid, Some(ticket), cursor, result)
    }

    fn finish_older(
        &mut self,
        rid: &str,
        ticket: Option<PageTicket>,
        cursor: HistoryCursor,
        result: Result<Vec<Message>, String>,
    ) -> bool {
        let watermark = self.room_watermark(rid);
        let Some(t) = self.timelines.get_mut(rid) else {
            return false;
        };
        let ids: Vec<String> = result
            .as_ref()
            .map(|ms| ms.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default();
        let parents: Vec<Message> = if self.thread_index.indexing() {
            result
                .as_ref()
                .map(|ms| {
                    ms.iter()
                        .filter(|m| m.tmid.is_none() && m.tcount.is_some())
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        } else {
            Vec::new()
        };
        let applied = match ticket {
            Some(tk) => t.finish_load_older_ticket(tk, cursor, result),
            None => t.finish_load_older(cursor, result),
        };
        if applied {
            if let Some(w) = watermark {
                t.set_sync_watermark_if_missing(w);
            }
            self.confirm_delivered(&ids);
            self.mark_timeline(rid);
            self.index_parents(&parents);
        }
        applied
    }

    /// A server time not later than now that bounds the room's history, used as
    /// the sync watermark of a timeline without messages.
    fn room_watermark(&self, rid: &str) -> Option<RcDate> {
        let e = self.rooms.get(rid);
        let r = e.and_then(|e| e.room.as_ref());
        let s = e.and_then(|e| e.sub.as_ref());
        [
            r.and_then(|r| r.updated_at),
            r.and_then(|r| r.lm),
            s.and_then(|s| s.updated_at),
            s.and_then(|s| s.ts),
        ]
        .into_iter()
        .flatten()
        .max()
        .or_else(|| Some(RcDate(RcDate::now().0 - crate::sync::CLOCK_SKEW_MARGIN)))
    }

    /// Server copies of pending messages arrived (e.g. in a page): they are sent.
    /// Persisted copies also end the ephemeral trust of a message with that id.
    fn confirm_delivered(&mut self, ids: &[String]) {
        if !self.ephemeral.is_empty() {
            let mut changed = false;
            for set in self.ephemeral.values_mut() {
                for id in ids {
                    changed |= set.remove(id);
                }
            }
            self.ephemeral.retain(|_, s| !s.is_empty());
            if changed {
                self.mark(Dirty::TIMELINES);
            }
        }
        let before = self.outbox.len();
        for id in ids {
            self.outbox.remove(id);
        }
        if self.outbox.len() != before {
            self.mark(Dirty::TIMELINES);
        }
    }

    /// See [`Timeline::begin_load_thread`]; prefer [`Self::begin_load_thread_ticket`].
    pub fn begin_load_thread(&mut self, tmid: &str) -> Option<u64> {
        self.begin_load_thread_ticket(tmid).map(|(o, _)| o)
    }

    /// Starts a thread page request; pass the ticket to [`Self::finish_load_thread_ticket`].
    /// The offset counts from the newest reply: request it with
    /// `RestClient::thread_messages_latest` (not the oldest-first
    /// `thread_messages`).
    pub fn begin_load_thread_ticket(&mut self, tmid: &str) -> Option<(u64, PageTicket)> {
        let o = self.threads.get_mut(tmid)?.begin_load_thread_ticket();
        if o.is_some() {
            self.mark_thread(tmid);
        }
        o
    }

    /// Completes a thread page request (dropped when none is outstanding).
    pub fn finish_load_thread(&mut self, tmid: &str, result: Result<Paginated<Message>, String>) {
        self.finish_thread(tmid, None, result);
    }

    /// Completes [`Self::begin_load_thread_ticket`]; a stale result is dropped.
    pub fn finish_load_thread_ticket(
        &mut self,
        tmid: &str,
        ticket: PageTicket,
        result: Result<Paginated<Message>, String>,
    ) -> bool {
        self.finish_thread(tmid, Some(ticket), result)
    }

    fn finish_thread(
        &mut self,
        tmid: &str,
        ticket: Option<PageTicket>,
        result: Result<Paginated<Message>, String>,
    ) -> bool {
        let rid = self.threads.get(tmid).map(|t| t.rid().to_owned());
        let watermark = rid.as_deref().and_then(|r| self.room_watermark(r));
        let Some(t) = self.threads.get_mut(tmid) else {
            return false;
        };
        let ids: Vec<String> = result
            .as_ref()
            .map(|p| p.items.iter().map(|m| m.id.clone()).collect())
            .unwrap_or_default();
        let newest_reply = result.as_ref().ok().and_then(|p| {
            p.items
                .iter()
                .filter(|m| m.hidden != Some(true))
                .filter_map(|m| m.ts)
                .max()
        });
        let applied = match ticket {
            Some(tk) => t.finish_load_thread_ticket(tk, result),
            None => t.finish_load_thread(result),
        };
        if applied {
            if let Some(w) = watermark {
                t.set_sync_watermark_if_missing(w);
            }
            let parent = t.parent().cloned();
            self.confirm_delivered(&ids);
            self.mark_thread(tmid);
            if let Some(p) = parent {
                self.index_parents(&[p]);
            }
            if self.thread_index.bump_activity(tmid, newest_reply) {
                self.mark(Dirty::THREAD_INDEX);
            }
        }
        applied
    }

    /// Builds a local message for the composer (client `_id`, author = me, ts = now).
    pub fn new_local_message(
        &self,
        rid: &str,
        text: &str,
        tmid: Option<&str>,
        tshow: bool,
    ) -> Message {
        let u = self
            .me
            .as_ref()
            .map(|m| rocketchat::models::UserRef {
                id: m.id.clone(),
                username: m.username.clone(),
                name: m.name.clone(),
            })
            .unwrap_or_default();
        Message {
            id: rocketchat::ids::random_id(),
            rid: rid.to_owned(),
            msg: text.to_owned(),
            ts: Some(RcDate::now()),
            u,
            tmid: tmid.map(str::to_owned),
            tshow: (tmid.is_some() && tshow).then_some(true),
            temp: Some(true),
            ..Default::default()
        }
    }

    /// Adds a pending message to the outbox and the open room/thread timelines.
    pub fn add_pending(&mut self, m: Message) {
        self.outbox.insert(
            m.id.clone(),
            TimelineItem {
                message: m.clone(),
                state: DeliveryState::Sending,
            },
        );
        self.mark(Dirty::TIMELINES);
        if let Some(tmid) = m.tmid.clone()
            && let Some(t) = self.threads.get_mut(&tmid)
            && t.insert_pending(m.clone())
        {
            self.mark_thread(&tmid);
        }
        let rid = m.rid.clone();
        if let Some(t) = self.timelines.get_mut(&rid)
            && t.accepts(&m)
            && t.insert_pending(m)
        {
            self.mark_timeline(&rid);
        }
    }

    fn for_pending(&mut self, rid: &str, tmid: Option<&str>, f: impl Fn(&mut Timeline) -> bool) {
        if let Some(tmid) = tmid
            && let Some(t) = self.threads.get_mut(tmid)
            && f(t)
        {
            self.mark_thread(tmid);
        }
        if let Some(t) = self.timelines.get_mut(rid)
            && f(t)
        {
            self.mark_timeline(rid);
        }
    }

    fn set_outbox_state(&mut self, id: &str, state: DeliveryState) {
        if let Some(item) = self.outbox.get_mut(id)
            && item.state != state
        {
            item.state = state;
            self.mark(Dirty::TIMELINES);
        }
    }

    pub fn mark_pending_failed(&mut self, rid: &str, tmid: Option<&str>, id: &str, error: &str) {
        self.set_outbox_state(
            id,
            DeliveryState::Failed {
                error: error.to_owned(),
            },
        );
        self.for_pending(rid, tmid, |t| t.mark_failed(id, error));
    }

    pub fn mark_pending_sending(&mut self, rid: &str, tmid: Option<&str>, id: &str) {
        self.set_outbox_state(id, DeliveryState::Sending);
        self.for_pending(rid, tmid, |t| t.mark_sending(id));
    }

    pub fn discard_pending(&mut self, rid: &str, tmid: Option<&str>, id: &str) {
        if self.outbox.remove(id).is_some() {
            self.mark(Dirty::TIMELINES);
        }
        self.for_pending(rid, tmid, |t| t.discard_pending(id));
    }
}
