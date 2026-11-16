use std::time::Instant;

use rocketchat::RcDate;
use rocketchat::models::{
    CustomEmoji, CustomUserStatus, Message, Permission, Role, Room, ServerInfo, Setting,
    Subscription, User, UserStatus,
};

use crate::timeline::Timeline;
use crate::uikit::UiKitStore;

use super::{Change, ConnectionState, Dirty, NEW_MESSAGE_WINDOW_MS, Workspace, is_older};

impl Workspace {
    // --- realtime: user, server, settings ---------------------------------------

    /// The current user's document (a fresh `/me`; the session re-reads it
    /// after `userData` hints, `SessionEvent::MeChanged`). Its status also
    /// updates the presence cache that [`Self::my_status`] reads.
    pub fn apply_me(&mut self, me: User) {
        self.me_generation += 1;
        if me.status.is_some() {
            let m = self.realtime.next();
            self.realtime.presence.insert(me.id.clone(), m);
        }
        if let Some(status) = me.status
            && self.presence.update(
                &me.id,
                me.username.as_deref(),
                status,
                Some(me.status_text.as_deref().unwrap_or("")),
            )
        {
            self.mark(Dirty::PRESENCE);
        }
        if self.me.as_ref() != Some(&me) {
            self.me = Some(me);
            self.mark(Dirty::ME);
            // The user, or the service rooms their preferences bind, changed.
            self.refresh_service_data();
        }
    }

    pub fn apply_server_info(&mut self, info: ServerInfo) {
        if self.server_info.as_ref() != Some(&info) {
            self.server_info = Some(info);
            self.mark(Dirty::SERVER_INFO);
        }
    }

    /// A settings stream event. Only public ones are kept: the private stream
    /// (`private-settings-changed`) carries admin-only values in clear, server
    /// secrets included, and this store is written to the offline cache.
    pub fn apply_setting_event(&mut self, change: Change, setting: Setting, public: bool) {
        if public {
            self.apply_setting(change, setting);
        }
    }

    /// Realtime change of a public setting (kept over REST snapshots that
    /// started earlier).
    pub fn apply_setting(&mut self, change: Change, setting: Setting) {
        let m = self.realtime.next();
        match change {
            Change::Removed => {
                self.realtime.settings.insert(setting.id.clone(), (m, None));
                self.settings.remove(&setting.id);
            }
            _ => {
                self.realtime
                    .settings
                    .insert(setting.id.clone(), (m, Some(setting.clone())));
                self.settings.set(setting);
            }
        }
        self.mark(Dirty::SETTINGS);
    }

    /// Realtime permission change (kept over REST snapshots that started earlier).
    pub fn apply_permission(&mut self, change: Change, p: Permission) {
        let m = self.realtime.next();
        match change {
            Change::Removed => {
                self.realtime.permissions.insert(p.id.clone(), (m, None));
                self.permissions.remove(&p.id);
            }
            _ => {
                self.realtime
                    .permissions
                    .insert(p.id.clone(), (m, Some(p.roles.clone())));
                if self.permissions.get(&p.id) == Some(&p.roles) {
                    return;
                }
                self.permissions.insert(p.id, p.roles);
            }
        }
        self.mark(Dirty::PERMISSIONS);
    }

    /// Realtime role change (kept over REST snapshots that started earlier).
    pub fn apply_role(&mut self, change: Change, role: Role) {
        let m = self.realtime.next();
        match change {
            Change::Removed => {
                self.realtime.roles.insert(role.id.clone(), (m, None));
                self.roles.remove(&role.id);
            }
            _ => {
                self.realtime
                    .roles
                    .insert(role.id.clone(), (m, Some(role.clone())));
                self.roles.insert(role.id.clone(), role);
            }
        }
        self.mark(Dirty::ROLES);
    }

    /// Re-applies settings, permissions and roles changed in realtime after a
    /// snapshot (started at `started`) was requested.
    pub(super) fn reapply_realtime_config(&mut self, started: Option<Instant>) {
        let marks = &self.realtime;
        let settings: Vec<(String, Option<Setting>)> = marks
            .settings
            .iter()
            .filter(|(_, (m, _))| marks.after(Some(m), started))
            .map(|(k, (_, v))| (k.clone(), v.clone()))
            .collect();
        let permissions: Vec<(String, Option<Vec<String>>)> = marks
            .permissions
            .iter()
            .filter(|(_, (m, _))| marks.after(Some(m), started))
            .map(|(k, (_, v))| (k.clone(), v.clone()))
            .collect();
        let roles: Vec<(String, Option<Role>)> = marks
            .roles
            .iter()
            .filter(|(_, (m, _))| marks.after(Some(m), started))
            .map(|(k, (_, v))| (k.clone(), v.clone()))
            .collect();
        for (id, v) in settings {
            match v {
                Some(s) => self.settings.set(s),
                None => {
                    self.settings.remove(&id);
                }
            }
            self.mark(Dirty::SETTINGS);
        }
        for (id, v) in permissions {
            match v {
                Some(r) => {
                    self.permissions.insert(id, r);
                }
                None => {
                    self.permissions.remove(&id);
                }
            }
            self.mark(Dirty::PERMISSIONS);
        }
        for (id, v) in roles {
            match v {
                Some(r) => {
                    self.roles.insert(id, r);
                }
                None => {
                    self.roles.remove(&id);
                }
            }
            self.mark(Dirty::ROLES);
        }
    }

    /// The current user's global roles changed (`roles-change` with scope `Users`)
    /// or a room role changed (`scope` = rid; updates the subscription's roles).
    pub fn apply_user_role_change(
        &mut self,
        uid: &str,
        role: &str,
        added: bool,
        scope: Option<&str>,
    ) {
        if self.user_id() != Some(uid) {
            return;
        }
        let edit = |roles: &mut Option<Vec<String>>| {
            let list = roles.get_or_insert_with(Vec::new);
            let has = list.iter().any(|r| r == role);
            if added && !has {
                list.push(role.to_owned());
                true
            } else if !added && has {
                list.retain(|r| r != role);
                true
            } else {
                false
            }
        };
        match scope {
            Some(rid) if rid != "Users" => {
                if let Some(sub) = self.rooms.get_mut(rid).and_then(|e| e.sub.as_mut())
                    && edit(&mut sub.roles)
                {
                    self.mark_room(rid);
                    self.mark(Dirty::PERMISSIONS);
                }
            }
            _ => {
                if let Some(me) = self.me.as_mut()
                    && edit(&mut me.roles)
                {
                    self.mark(Dirty::ME | Dirty::PERMISSIONS);
                }
            }
        }
    }

    pub fn apply_custom_emoji(&mut self, change: Change, e: CustomEmoji) {
        match change {
            Change::Removed => {
                if !self.emoji.remove(&e.id) && !self.emoji.remove(&e.name) {
                    return;
                }
            }
            _ => self.emoji.upsert(e),
        }
        self.mark(Dirty::EMOJI);
    }

    pub fn apply_custom_status(&mut self, change: Change, s: CustomUserStatus) {
        match change {
            Change::Removed => {
                self.custom_statuses.remove(&s.id);
            }
            _ => {
                self.custom_statuses.insert(s.id.clone(), s);
            }
        }
        self.mark(Dirty::CUSTOM_STATUSES);
    }

    pub fn apply_user_status(
        &mut self,
        uid: &str,
        username: Option<&str>,
        status: UserStatus,
        status_text: Option<&str>,
    ) {
        let m = self.realtime.next();
        self.realtime.presence.insert(uid.to_owned(), m);
        if self.presence.update(uid, username, status, status_text) {
            self.mark(Dirty::PRESENCE);
            if self.user_id() == Some(uid) {
                if let Some(me) = self.me.as_mut() {
                    me.status = Some(status);
                    if let Some(t) = status_text {
                        me.status_text = (!t.is_empty()).then(|| t.to_owned());
                    }
                }
                self.mark(Dirty::ME);
            }
        }
    }

    pub fn apply_connection(&mut self, state: ConnectionState) {
        if self.connection != state {
            if state != ConnectionState::Connected {
                self.typing.clear();
                self.mark(Dirty::TYPING);
                if self.feeds.mark_all_stale() {
                    self.mark(Dirty::FEEDS);
                }
            }
            self.connection = state;
            self.mark(Dirty::CONNECTION);
        }
    }

    pub fn apply_logged_out(&mut self) {
        self.apply_connection(ConnectionState::LoggedOut);
        if self.thread_index.clear() {
            self.mark(Dirty::THREAD_INDEX);
        }
    }

    // --- realtime: rooms and subscriptions --------------------------------------

    /// A realtime subscription change. A copy older than the stored one (by
    /// `_updatedAt`) is ignored; ties apply.
    pub fn apply_subscription(&mut self, change: Change, sub: Subscription) {
        let m = self.realtime.next();
        if change == Change::Removed {
            self.realtime.rooms.remove(&sub.rid);
            self.realtime.removed.insert(sub.rid.clone(), m);
            self.remove_subscription(&sub.rid);
            return;
        }
        self.realtime.removed.remove(&sub.rid);
        self.realtime.rooms.insert(sub.rid.clone(), m);
        self.cached_rooms.remove(&sub.rid);
        let rid = sub.rid.clone();
        let entry = self.rooms.entry(rid.clone()).or_default();
        if let Some(old) = entry.sub.as_ref()
            && (old == &sub || is_older(sub.updated_at, old.updated_at))
        {
            return;
        }
        entry.sub = Some(sub);
        self.mark_room(&rid);
    }

    /// The user left or was removed: drops the room, its timelines, typing and
    /// pending messages.
    pub(super) fn remove_subscription(&mut self, rid: &str) {
        let name = self.room_label(rid);
        if self.rooms.remove(rid).is_some() {
            self.mark_room(rid);
        }
        self.drop_room_timelines(rid, name);
        self.index_remove_room(rid);
    }

    /// The room's shown name (its id when unknown).
    fn room_label(&self, rid: &str) -> String {
        self.rooms
            .get(rid)
            .map(|e| e.fname().unwrap_or(e.name()).to_owned())
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| rid.to_owned())
    }

    fn drop_room_timelines(&mut self, rid: &str, name: String) {
        if self.feeds.drop_room(rid) {
            self.mark(Dirty::FEEDS);
        }
        self.later_forget_room(rid);
        self.outbox.retain(|_, i| i.message.rid != rid);
        let before = self.scheduled.len();
        if self.scheduled.retain_rooms(|r| r != rid) {
            // The app tells the user (`take_dropped_scheduled`).
            self.dropped_scheduled
                .push((name, before - self.scheduled.len()));
            self.mark(Dirty::SCHEDULED);
        }
        self.ephemeral.remove(rid);
        if self.timelines.remove(rid).is_some() {
            self.mark_timeline(rid);
        }
        let threads: Vec<String> = self
            .threads
            .iter()
            .filter(|(_, t)| t.rid() == rid)
            .map(|(k, _)| k.clone())
            .collect();
        for tmid in threads {
            self.threads.remove(&tmid);
            self.mark_thread(&tmid);
        }
    }

    /// A realtime room change (same rules as [`Self::apply_subscription`]).
    pub fn apply_room(&mut self, change: Change, room: Room) {
        let m = self.realtime.next();
        if change == Change::Removed {
            self.realtime.rooms.remove(&room.id);
            self.realtime.removed.insert(room.id.clone(), m);
            self.remove_room(&room.id);
            return;
        }
        self.realtime.removed.remove(&room.id);
        self.realtime.rooms.insert(room.id.clone(), m);
        self.cached_rooms.remove(&room.id);
        let rid = room.id.clone();
        let entry = self.rooms.entry(rid.clone()).or_default();
        if let Some(old) = entry.room.as_ref()
            && (old == &room || is_older(room.updated_at, old.updated_at))
        {
            return;
        }
        entry.room = Some(room);
        self.mark_room(&rid);
    }

    /// Drops a room that is only known because it was previewed (no
    /// subscription: opened from the directory or the quick switcher without
    /// joining): the room entry, its timelines and pending messages. Joined
    /// rooms are left alone. Returns whether something was dropped.
    pub fn forget_preview(&mut self, rid: &str) -> bool {
        if !self.rooms.get(rid).is_some_and(|e| e.sub.is_none()) {
            return false;
        }
        self.realtime.rooms.remove(rid);
        self.remove_room(rid);
        true
    }

    pub(super) fn remove_room(&mut self, rid: &str) {
        let name = self.room_label(rid);
        if self.rooms.remove(rid).is_some() {
            self.mark_room(rid);
        }
        self.drop_room_timelines(rid, name);
        self.index_remove_room(rid);
    }

    // --- realtime: messages -------------------------------------------------------

    /// A new or changed message from the stream (or a REST call result). Updates
    /// the room timeline and the thread timeline if they are open; a parent update
    /// (`tcount`, `tlm`, `replies`) reaches both. Confirms a pending message with
    /// the same `_id`. A newly created message stops its author's typing indicator.
    pub fn apply_message(&mut self, m: Message) {
        self.apply_message_from(m, true);
    }

    pub(super) fn apply_message_from(&mut self, m: Message, realtime: bool) {
        // Before the hidden check: a parent that becomes hidden leaves the
        // index, a hidden message leaves the inbox feeds.
        self.index_message(&m);
        let me = self
            .me
            .as_ref()
            .and_then(|u| Some((u.username.as_deref()?, u.id.as_str())));
        if self.feeds.apply(&m, me) {
            self.mark(Dirty::FEEDS);
        }
        self.later_observe(&m);
        if m.hidden == Some(true) {
            return;
        }
        let rid = m.rid.clone();
        self.forget_ephemeral(&rid, &m.id);
        if self.outbox.remove(&m.id).is_some() {
            self.mark(Dirty::TIMELINES);
        }
        if realtime && self.is_newly_created(&m) {
            let name = self.activity_name(&m.u).map(str::to_owned);
            if let Some(name) = name
                && self
                    .typing
                    .apply(&rid, m.tmid.as_deref(), &name, &[], Instant::now())
            {
                self.mark(Dirty::TYPING);
            }
        }
        if let Some(tmid) = m.tmid.clone()
            && let Some(t) = self.threads.get_mut(&tmid)
            && t.upsert(m.clone())
        {
            self.mark_thread(&tmid);
        }
        if let Some(t) = self.threads.get_mut(&m.id)
            && t.upsert(m.clone())
        {
            let id = m.id.clone();
            self.mark_thread(&id);
        }
        if let Some(t) = self.timelines.get_mut(&rid)
            && t.upsert(m)
        {
            self.mark_timeline(&rid);
        }
    }

    /// A message that was just created rather than an update of an existing one
    /// (edit, reaction, thread counters, URL preview of an old message).
    fn is_newly_created(&self, m: &Message) -> bool {
        if m.edited_at.is_some() {
            return false;
        }
        let shown = self
            .timelines
            .get(&m.rid)
            .is_some_and(|t| t.contains(&m.id))
            || m.tmid
                .as_ref()
                .and_then(|t| self.threads.get(t))
                .is_some_and(|t| t.contains(&m.id));
        if shown {
            return false;
        }
        match (m.ts, m.updated_at) {
            (Some(ts), Some(u)) => {
                u.timestamp_millis() - ts.timestamp_millis() < NEW_MESSAGE_WINDOW_MS
            }
            _ => true,
        }
    }

    pub fn apply_message_deleted(&mut self, rid: &str, id: &str) {
        self.forget_ephemeral(rid, id);
        self.later_remove(id);
        if self.feeds.remove(rid, id) {
            self.mark(Dirty::FEEDS);
        }
        if self.thread_index.remove_message(rid, id) {
            self.mark(Dirty::THREAD_INDEX);
        }
        if let Some(t) = self.timelines.get_mut(rid)
            && t.remove(id)
        {
            self.mark_timeline(rid);
        }
        let mut changed = Vec::new();
        for (tmid, t) in self.threads.iter_mut().filter(|(_, t)| t.rid() == rid) {
            if t.remove(id) {
                changed.push(tmid.clone());
            }
        }
        for tmid in changed {
            self.mark_thread(&tmid);
        }
    }

    /// Read receipts: messages before `until` (exclusive, like the server's
    /// `ts: {$lt: until}`) are no longer unread.
    pub fn apply_messages_read(&mut self, rid: &str, until: Option<RcDate>, tmid: Option<&str>) {
        let limit = until.map_or(i64::MAX, |d| d.timestamp_millis());
        let mark = |t: &mut Timeline| {
            let ids: Vec<Message> = t
                .messages()
                .filter(|m| m.unread == Some(true))
                .filter(|m| m.ts.is_some_and(|ts| ts.timestamp_millis() < limit))
                .cloned()
                .collect();
            let mut changed = false;
            for mut m in ids {
                m.unread = None;
                changed |= t.upsert(m);
            }
            changed
        };
        match tmid {
            Some(tmid) => {
                if let Some(t) = self.threads.get_mut(tmid)
                    && mark(t)
                {
                    self.mark_thread(tmid);
                }
            }
            None => {
                if let Some(t) = self.timelines.get_mut(rid)
                    && mark(t)
                {
                    self.mark_timeline(rid);
                }
            }
        }
    }

    /// `user-activity` event (empty `activities` = stopped).
    pub fn apply_user_activity(
        &mut self,
        rid: &str,
        username: &str,
        activities: &[String],
        tmid: Option<&str>,
    ) {
        self.apply_user_activity_at(rid, username, activities, tmid, Instant::now());
    }

    pub fn apply_user_activity_at(
        &mut self,
        rid: &str,
        username: &str,
        activities: &[String],
        tmid: Option<&str>,
        now: Instant,
    ) {
        if self.is_my_activity_name(username) {
            return;
        }
        if self.typing.apply(rid, tmid, username, activities, now) {
            self.mark(Dirty::TYPING);
        }
    }

    /// Expires stale typing indicators; schedule with `typing().next_expiry()`.
    pub fn expire_typing(&mut self, now: Instant) {
        if self.typing.expire(now) {
            self.mark(Dirty::TYPING);
        }
    }

    // --- ephemeral messages and UIKit ----------------------------------------------

    /// An ephemeral message (`SessionEvent::EphemeralMessage`): upserted like any
    /// message (by `_id`, room and thread timelines), and flagged as ephemeral
    /// while that version is the one shown. The flag is dropped when a message
    /// with the same `_id` arrives any other way (stream, history, sync), is
    /// deleted, or the room timeline is closed or reset.
    pub fn apply_ephemeral_message(&mut self, m: Message) {
        if m.hidden == Some(true) {
            return;
        }
        let (rid, id) = (m.rid.clone(), m.id.clone());
        let tmid = m.tmid.clone();
        let shown = m.clone();
        self.apply_message_from(m, true);
        let stored = |t: Option<&Timeline>| {
            t.and_then(|t| t.get(&id))
                .is_some_and(|i| i.message == shown)
        };
        let in_room = stored(self.timelines.get(&rid));
        let in_thread = stored(tmid.as_ref().and_then(|t| self.threads.get(t)));
        if in_room || in_thread {
            self.ephemeral.entry(rid.clone()).or_default().insert(id);
            self.mark_timeline(&rid);
        }
    }

    /// Whether the shown version of message `mid` in `rid` arrived as an
    /// ephemeral message (never derived from message fields).
    pub fn is_ephemeral(&self, rid: &str, mid: &str) -> bool {
        self.ephemeral.get(rid).is_some_and(|s| s.contains(mid))
    }

    fn forget_ephemeral(&mut self, rid: &str, id: &str) {
        if let Some(set) = self.ephemeral.get_mut(rid)
            && set.remove(id)
        {
            if set.is_empty() {
                self.ephemeral.remove(rid);
            }
            self.mark_timeline(rid);
        }
    }

    /// UIKit state (open views, triggers, action buttons, app translations).
    pub fn uikit(&self) -> &UiKitStore {
        &self.uikit
    }

    pub fn uikit_mut(&mut self) -> &mut UiKitStore {
        &mut self.uikit
    }

    /// Video-conference data (block info cache, capabilities, license).
    pub fn videoconf(&self) -> &crate::videoconf::VideoConfStore {
        &self.videoconf
    }

    pub fn videoconf_mut(&mut self) -> &mut crate::videoconf::VideoConfStore {
        &mut self.videoconf
    }
}
