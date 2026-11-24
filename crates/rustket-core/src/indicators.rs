//! Indicator schemes: Rustket's own unread look, or a copy of the stock
//! Rocket.Chat 8.9 web client and desktop app. The Rustket rules live in
//! [`crate::room_list`] ([`badge`](crate::room_list::badge),
//! [`workspace_signal`](crate::room_list::workspace_signal), …); this module
//! holds the Rocket.Chat rules next to them. Pure: every function takes the
//! scheme or the data explicitly.

use rocketchat::models::Subscription;
use serde::{Deserialize, Serialize};

use crate::room_list::BadgeVariant;
use crate::workspace::{RoomEntry, Workspace};

/// Which look unread indicators follow (`ui.indicator_scheme`).
///
/// The enum default is Rustket (today's behaviour for struct defaults such as
/// `RoomListOptions::default()`); `UiPrefs::default()` picks Rocket.Chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IndicatorScheme {
    #[default]
    Rustket,
    RocketChat,
}

/// The inputs of Rocket.Chat's `getSubscriptionUnreadData` for one room, or
/// a collapsed section's sum. Thread lists are counted by length.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UnreadInfo {
    pub user_mentions: u64,
    pub group_mentions: u64,
    /// `tunread.length`.
    pub tunread: u64,
    /// `tunreadUser.length`.
    pub tunread_user: u64,
    pub unread: u64,
    pub alert: bool,
    /// `hideUnreadStatus`.
    pub hide_unread: bool,
    /// `hideMentionStatus`.
    pub hide_mention: bool,
}

impl UnreadInfo {
    /// The fields of subscription `s` (`tunreadGroup`, `disableNotifications`
    /// and `muteGroupMentions` are not inputs).
    pub fn of(s: &Subscription) -> Self {
        let len = |v: &Option<Vec<String>>| v.as_ref().map_or(0, |v| v.len() as u64);
        Self {
            user_mentions: s.user_mentions,
            group_mentions: s.group_mentions,
            tunread: len(&s.tunread),
            tunread_user: len(&s.tunread_user),
            unread: s.unread,
            alert: s.alert,
            hide_unread: s.hide_unread_status == Some(true),
            hide_mention: s.hide_mention_status == Some(true),
        }
    }

    /// A Rustket thread row inside a collapsed section: one unread thread,
    /// a mention when its drawn kind is [`BadgeVariant::Mention`].
    pub fn thread(kind: BadgeVariant) -> Self {
        Self {
            tunread: 1,
            tunread_user: u64::from(kind == BadgeVariant::Mention),
            ..Self::default()
        }
    }

    /// `userMentions + tunreadUser.length`.
    pub fn mentions(&self) -> u64 {
        self.user_mentions + self.tunread_user
    }

    /// `unread + tunread.length`: the number on the badge.
    pub fn total(&self) -> u64 {
        self.unread + self.tunread
    }
}

/// The parts of Rocket.Chat's itemised badge tooltip (`getUnreadTitle`),
/// in its order: "N mentions, N unread threaded messages, N group mentions,
/// N unread messages". A part is shown when it is not 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UnreadTitle {
    pub mentions: u64,
    pub threads: u64,
    pub group_mentions: u64,
    /// `total - mentions - groupMentions - threads`, 0 when negative.
    pub rest: u64,
}

impl UnreadTitle {
    pub fn of(info: &UnreadInfo) -> Self {
        let mentions = info.mentions();
        let threads = info.tunread;
        let group_mentions = info.group_mentions;
        Self {
            mentions,
            threads,
            group_mentions,
            rest: info
                .total()
                .saturating_sub(mentions + group_mentions + threads),
        }
    }

    /// No part to show.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// What Rocket.Chat draws for one room row (or a collapsed header).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RcBadge {
    /// The number on the badge, never capped; 0: no badge (and never a dot).
    pub count: u64,
    /// The badge colour ([`rc_variant`]); meaningful when `count > 0`.
    pub variant: BadgeVariant,
    /// `highlightUnread`: the highlighted name (`!hideUnreadStatus && (alert ||
    /// unread)`).
    pub highlighted: bool,
    /// The row has something unread to show: `count > 0 || (!hideUnreadStatus
    /// && alert)`. Unread-first ordering and next/previous unread use it.
    pub marked: bool,
    /// The tooltip parts.
    pub title: UnreadTitle,
    /// The inputs.
    pub info: UnreadInfo,
}

impl RcBadge {
    /// The badge's kind; `None` without a badge.
    pub fn kind(&self) -> Option<BadgeVariant> {
        (self.count > 0).then_some(self.variant)
    }
}

/// Rocket.Chat's variant order: danger (mentions, thread mentions included)
/// > primary (threads) > warning (group mentions) > secondary.
pub fn rc_variant(mentions: u64, threads: u64, group: u64) -> BadgeVariant {
    if mentions > 0 {
        BadgeVariant::Mention
    } else if threads > 0 {
        BadgeVariant::Thread
    } else if group > 0 {
        BadgeVariant::GroupMention
    } else {
        BadgeVariant::Unread
    }
}

/// `getSubscriptionUnreadData` over `info`.
pub fn rc_badge_of(info: UnreadInfo) -> RcBadge {
    let mentions = info.mentions();
    let total = info.total();
    let show = total > 0
        && (!info.hide_unread || (!info.hide_mention && (mentions > 0 || info.group_mentions > 0)));
    let count = if show { total } else { 0 };
    RcBadge {
        count,
        variant: rc_variant(mentions, info.tunread, info.group_mentions),
        highlighted: !info.hide_unread && (info.alert || info.unread > 0),
        marked: count > 0 || (!info.hide_unread && info.alert),
        title: UnreadTitle::of(&info),
        info,
    }
}

/// The Rocket.Chat badge of a room row (`getSubscriptionUnreadData.ts`).
/// The room type, `disableNotifications`, `muteGroupMentions` and
/// `tunreadGroup` change nothing. Without a subscription: no badge.
pub fn rc_badge(e: &RoomEntry) -> RcBadge {
    e.sub
        .as_ref()
        .map(|s| rc_badge_of(UnreadInfo::of(s)))
        .unwrap_or_default()
}

/// A collapsed section's badge over its hidden rows (`useRoomList.ts`):
/// rows with hidden unread status are skipped, the rest are summed, and a
/// row with only `alert` adds 1 unread. `None` when the sum is 0.
pub fn rc_collapsed(rows: impl IntoIterator<Item = UnreadInfo>) -> Option<RcBadge> {
    let mut acc = UnreadInfo::default();
    for r in rows.into_iter().filter(|r| !r.hide_unread) {
        acc.user_mentions += r.user_mentions;
        acc.group_mentions += r.group_mentions;
        acc.tunread += r.tunread;
        acc.tunread_user += r.tunread_user;
        acc.unread += r.unread;
        if r.unread == 0 && r.tunread == 0 && r.alert {
            acc.unread += 1;
        }
    }
    let b = rc_badge_of(acc);
    (b.count > 0).then_some(RcBadge {
        highlighted: true,
        ..b
    })
}

/// The dot of thread `tmid` in the timeline, the Threads panel and thread
/// rows (`ThreadMetricsUnreadBadge`): `None` unless `tunread` has it, then
/// Mention (`tunreadUser`), else GroupMention (`tunreadGroup`), else Thread.
/// Room notification settings change nothing.
pub fn rc_thread_kind(s: &Subscription, tmid: &str) -> Option<BadgeVariant> {
    let has = |v: &Option<Vec<String>>| v.as_ref().is_some_and(|v| v.iter().any(|t| t == tmid));
    if !has(&s.tunread) {
        None
    } else if has(&s.tunread_user) {
        Some(BadgeVariant::Mention)
    } else if has(&s.tunread_group) {
        Some(BadgeVariant::GroupMention)
    } else {
        Some(BadgeVariant::Thread)
    }
}

/// The room header's Threads counter (`useThreadRoomAction`): the number
/// of unread threads (uncapped here; the header draws 99+), Mention when
/// any `tunreadUser`, else GroupMention when any `tunreadGroup`, else
/// Thread. `None` with no unread thread.
pub fn rc_threads_button(s: &Subscription) -> Option<(u64, BadgeVariant)> {
    let len = |v: &Option<Vec<String>>| v.as_ref().map_or(0, |v| v.len());
    let n = len(&s.tunread) as u64;
    let kind = if len(&s.tunread_user) > 0 {
        BadgeVariant::Mention
    } else if len(&s.tunread_group) > 0 {
        BadgeVariant::GroupMention
    } else {
        BadgeVariant::Thread
    };
    (n > 0).then_some((n, kind))
}

/// Rocket.Chat's workspace number (`useUnread.ts`; the desktop app's
/// badge): the summed `unread`, else a bullet "•" for an alert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RcSignal {
    /// Σ `unread` (threads never add); uncapped.
    pub unread: u64,
    /// Some counted room asks for the bullet; shown only when `unread` is 0
    /// ([`Self::shows_bullet`]).
    pub bullet: bool,
}

impl RcSignal {
    /// Both signals together (several rooms, several accounts): sum / or.
    pub fn merge(self, o: Self) -> Self {
        Self {
            unread: self.unread + o.unread,
            bullet: self.bullet || o.bullet,
        }
    }

    /// The bullet form: nothing to count, but an alert.
    pub fn shows_bullet(&self) -> bool {
        self.unread == 0 && self.bullet
    }

    /// A number or a bullet shows.
    pub fn any(&self) -> bool {
        self.unread > 0 || self.bullet
    }
}

impl std::iter::Sum for RcSignal {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::default(), Self::merge)
    }
}

/// One room's part of [`rc_workspace_signal`]: an open, non-archived,
/// non-service subscription without hidden unread status, with `alert` or
/// `unread`, adds its `unread`; its `alert` asks for the bullet unless the
/// subscription's `unreadAlert` is `nothing`, and only with `alert_pref`
/// (the user's "Unread Tray Icon Alert") or `unreadAlert: all`.
/// Muted rooms count.
pub fn rc_entry_signal(e: &RoomEntry, alert_pref: bool) -> RcSignal {
    let Some(s) = e.sub.as_ref().filter(|s| {
        s.open && s.hide_unread_status != Some(true) && !e.is_archived() && !e.is_service_room()
    }) else {
        return RcSignal::default();
    };
    if !(s.alert || s.unread > 0) {
        return RcSignal::default();
    }
    let sub_alert = s.unread_alert.as_deref();
    RcSignal {
        unread: s.unread,
        bullet: s.alert && sub_alert != Some("nothing") && (sub_alert == Some("all") || alert_pref),
    }
}

/// The [`RcSignal`] of a workspace.
pub fn rc_workspace_signal(ws: &Workspace) -> RcSignal {
    let pref = ws.unread_alert_pref();
    ws.rooms().values().map(|e| rc_entry_signal(e, pref)).sum()
}

impl BadgeVariant {
    /// The aggregation rank under scheme `s` (greater is stronger). Rustket:
    /// the derived order (Muted < Thread < Unread < GroupMention < Mention).
    /// Rocket.Chat: Mention > Thread > GroupMention > Unread > Muted (never
    /// produced there).
    pub fn rank(self, s: IndicatorScheme) -> u8 {
        match (s, self) {
            (_, Self::Muted) => 0,
            (IndicatorScheme::Rustket, Self::Thread) => 1,
            (IndicatorScheme::Rustket, Self::Unread) => 2,
            (IndicatorScheme::Rustket, Self::GroupMention) => 3,
            (IndicatorScheme::RocketChat, Self::Unread) => 1,
            (IndicatorScheme::RocketChat, Self::GroupMention) => 2,
            (IndicatorScheme::RocketChat, Self::Thread) => 3,
            (_, Self::Mention) => 4,
        }
    }
}

/// The strongest of `kinds` by [`BadgeVariant::rank`] under `s`.
pub fn strongest_in(
    kinds: impl IntoIterator<Item = BadgeVariant>,
    s: IndicatorScheme,
) -> Option<BadgeVariant> {
    kinds.into_iter().max_by_key(|k| k.rank(s))
}

#[cfg(test)]
mod tests;
