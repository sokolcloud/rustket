//! What the Rocket.Chat server (8.9.1) does with one message from someone
//! else: the subscription counters it changes and whether it sends the
//! receiver a desktop notification. The client never sees these rules, so
//! they are modelled here from the server sources:
//! - counters: `apps/meteor/server/hooks/messages/notifyUsersOnMessage.ts`
//!   (`updateUsersSubscriptions`) and `packages/models/src/models/Subscriptions.ts`;
//!   thread replies: `apps/meteor/server/hooks/messages/processThreads.ts`,
//!   `apps/meteor/server/lib/messaging/threads/functions.ts` (`reply`) and
//!   `Subscriptions.addUnreadThreadByRoomIdAndUserIds`;
//! - desktop notifications: `sendNotificationsOnMessage.ts`
//!   (`sendMessageNotifications`, `sendNotification`) and
//!   `server/lib/notifications/message/desktop.ts` (`shouldNotifyDesktop`).

use rocketchat::models::{RoomType, Subscription, UserStatus};
use rustket_core::settings::Settings;

/// `Unread_Count`, `Unread_Count_DM`, `Unread_Count_Omni`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnreadCount {
    AllMessages,
    UserMentionsOnly,
    GroupMentionsOnly,
    UserAndGroupMentionsOnly,
}

impl UnreadCount {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "all_messages" => Self::AllMessages,
            "user_mentions_only" => Self::UserMentionsOnly,
            "group_mentions_only" => Self::GroupMentionsOnly,
            "user_and_group_mentions_only" => Self::UserAndGroupMentionsOnly,
            // `Unread_Count_DM` / `Unread_Count_Omni`: there every mention
            // counts once and a plain message does not.
            "mentions_only" => Self::UserAndGroupMentionsOnly,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AllMessages => "all_messages",
            Self::UserMentionsOnly => "user_mentions_only",
            Self::GroupMentionsOnly => "group_mentions_only",
            Self::UserAndGroupMentionsOnly => "user_and_group_mentions_only",
        }
    }
}

/// A desktop notification preference (account default, room or server
/// default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopPref {
    All,
    Mentions,
    Nothing,
}

impl DesktopPref {
    /// `all` / `mentions` / `nothing`; `default` and anything else: not set.
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "all" => Self::All,
            "mentions" => Self::Mentions,
            "nothing" => Self::Nothing,
            _ => return None,
        })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Mentions => "mentions",
            Self::Nothing => "nothing",
        }
    }
}

/// The server settings the model reads (public settings).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerFacts {
    pub unread_count: UnreadCount,
    pub unread_count_dm: UnreadCount,
    pub unread_count_omni: UnreadCount,
    /// `Accounts_Default_User_Preferences_desktopNotifications`.
    pub default_desktop: Option<DesktopPref>,
    /// `Accounts_Default_User_Preferences_emailNotificationMode`
    /// (`nothing` / `mentions`).
    pub default_email: Option<DesktopPref>,
    /// `Notifications_Max_Room_Members` (0 = no limit).
    pub max_room_members: u64,
    pub threads_enabled: bool,
    /// `Message_AllowStarring` (starred threads in Favorites need it).
    pub allow_starring: bool,
}

impl Default for ServerFacts {
    /// Rocket.Chat's defaults (the dev server's values).
    fn default() -> Self {
        Self {
            unread_count: UnreadCount::UserAndGroupMentionsOnly,
            unread_count_dm: UnreadCount::AllMessages,
            unread_count_omni: UnreadCount::AllMessages,
            default_desktop: Some(DesktopPref::All),
            default_email: Some(DesktopPref::Mentions),
            max_room_members: 100,
            threads_enabled: true,
            allow_starring: true,
        }
    }
}

impl ServerFacts {
    /// From the server's public settings; missing values take Rocket.Chat's
    /// defaults.
    pub fn from_settings(s: &Settings) -> Self {
        let d = Self::default();
        let count = |id: &str, default| s.str(id).and_then(UnreadCount::parse).unwrap_or(default);
        Self {
            unread_count: count("Unread_Count", d.unread_count),
            unread_count_dm: count("Unread_Count_DM", d.unread_count_dm),
            unread_count_omni: count("Unread_Count_Omni", d.unread_count_omni),
            default_desktop: match s.str("Accounts_Default_User_Preferences_desktopNotifications") {
                Some(v) => DesktopPref::parse(v),
                None => d.default_desktop,
            },
            default_email: match s.str("Accounts_Default_User_Preferences_emailNotificationMode") {
                Some(v) => DesktopPref::parse(v),
                None => d.default_email,
            },
            max_room_members: s.notifications_max_room_members(),
            threads_enabled: s.threads_enabled(),
            allow_starring: s.allow_starring(),
        }
    }

    /// `getUnreadSettingCount`.
    pub fn unread_count_for(&self, t: RoomType) -> UnreadCount {
        match t {
            RoomType::Direct => self.unread_count_dm,
            RoomType::Livechat => self.unread_count_omni,
            _ => self.unread_count,
        }
    }
}

/// What the message carries for the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    Plain,
    /// `@username` of the receiver.
    MentionMe,
    /// `@all` / `@here`.
    MentionAll,
    /// One of the receiver's highlight words.
    Highlight,
}

/// One message from someone else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Incoming {
    pub room: RoomType,
    /// A reply only in a thread the receiver follows.
    pub thread: bool,
    pub trigger: Trigger,
}

/// The receiver's side of the decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Receiver {
    pub status: UserStatus,
    /// Has a live connection (`statusConnection` not offline).
    pub connected: bool,
    /// Has highlight words (`userHighlights` in every subscription).
    pub has_highlights: bool,
    /// Members of the room.
    pub room_members: u64,
}

/// Why the server sends no desktop notification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerBlock {
    /// The room's "Receive notifications" is off (`disableNotifications`).
    RoomMuted,
    /// `@all` / `@here` in a room with "Mute @all and @here".
    GroupMentionsMuted,
    /// The receiver's status is Busy.
    Busy,
    /// The receiver has no live connection.
    Offline,
    /// The desktop preference filters it out: the room's own value
    /// (`room`), else the account's.
    Preference { pref: DesktopPref, room: bool },
    /// No preference set and the server default filters it out.
    ServerDefault(Option<DesktopPref>),
    /// The room has more members than `Notifications_Max_Room_Members`:
    /// only explicit per-room "all", highlights and thread replies notify.
    LargeRoom { limit: u64 },
}

/// The subscription counters after `msg` (the stored values, read by the
/// client on the next sync): `tmid` is the thread of a thread reply.
pub fn apply(
    sub: &mut Subscription,
    msg: &Incoming,
    tmid: &str,
    facts: &ServerFacts,
    r: &Receiver,
) {
    // A highlight word the receiver does not have is a plain message.
    let trigger = match msg.trigger {
        Trigger::Highlight if !r.has_highlights => Trigger::Plain,
        t => t,
    };
    sub.alert = true;
    sub.open = true;
    if msg.thread {
        // `reply`: followers get `tunread` (and `tunreadGroup` for
        // `@all`/`@here`), mentioned users and highlight words `tunread` and
        // `tunreadUser`; `getMentions` drops `all`/`here` from the mentioned
        // users, so `@all` never adds `tunreadUser`. The room counters do not
        // change. Verified on the dev server (8.9.1).
        let push =
            |v: &mut Option<Vec<String>>| v.get_or_insert_with(Vec::new).push(tmid.to_owned());
        push(&mut sub.tunread);
        match trigger {
            Trigger::Plain => {}
            Trigger::MentionMe | Trigger::Highlight => push(&mut sub.tunread_user),
            Trigger::MentionAll => push(&mut sub.tunread_group),
        }
        return;
    }
    let count = facts.unread_count_for(msg.room);
    let dm_or_omni = matches!(msg.room, RoomType::Direct | RoomType::Livechat);
    // `getUserMentions` / `getGroupMentions`: passed as the `unread`
    // increment of `incUserMentionsAndUnread…` / `incGroupMentionsAndUnread…`,
    // whose mention increment is always 1.
    let user_inc = u64::from(dm_or_omni || count != UnreadCount::GroupMentionsOnly);
    let group_inc = u64::from(dm_or_omni || count != UnreadCount::UserMentionsOnly);
    match trigger {
        Trigger::MentionMe | Trigger::Highlight => {
            sub.user_mentions += 1;
            sub.unread += user_inc;
        }
        Trigger::MentionAll => {
            sub.group_mentions += 1;
            sub.unread += group_inc;
        }
        Trigger::Plain => {
            if count == UnreadCount::AllMessages {
                sub.unread += 1;
            }
        }
    }
}

/// Whether the subscription query of `sendMessageNotifications` selects
/// `sub` (`disableNotifications` aside): highlight words, thread followers,
/// or one of the desktop, mobile and email clauses. Each clause matches the
/// stored value "all" (in rooms above the member limit only when not copied
/// from the account) and, below the limit, "mentions" in DMs or when the
/// receiver is mentioned, and an unset value when the server default is
/// "all", for `@all`/`@here` and in DMs, or when the default is "mentions"
/// and the receiver is mentioned. The mobile clause queries the field
/// `mobilePush`, which no subscription has (the stored one is
/// `mobilePushNotifications`), and its default
/// `Accounts_Default_User_Preferences_mobileNotifications`, which does not
/// exist (the setting is `…_pushNotifications`): it selects every
/// subscription for `@all`/`@here` and in DMs below the limit, nothing else.
fn selected(
    sub: &Subscription,
    msg: &Incoming,
    facts: &ServerFacts,
    r: &Receiver,
    large: bool,
) -> bool {
    if r.has_highlights || msg.thread {
        return true;
    }
    let direct = msg.room == RoomType::Direct;
    let to_all = msg.trigger == Trigger::MentionAll;
    let mentioned = msg.trigger == Trigger::MentionMe;
    let clause = |value: Option<&str>, origin: Option<&str>, default: Option<DesktopPref>| {
        if value == Some("all") && !(large && origin == Some("user")) {
            return true;
        }
        if large {
            return false;
        }
        match value {
            Some(v) => v == "mentions" && (direct || mentioned),
            None => {
                default == Some(DesktopPref::All)
                    || to_all
                    || direct
                    || (default == Some(DesktopPref::Mentions) && mentioned)
            }
        }
    };
    clause(
        sub.desktop_notifications.as_deref(),
        sub.desktop_pref_origin.as_deref(),
        facts.default_desktop,
    ) || clause(None, None, None)
        || clause(
            sub.email_notifications.as_deref(),
            sub.email_pref_origin.as_deref(),
            facts.default_email,
        )
}

/// Whether the server sends the receiver a desktop notification for `msg`
/// (`sub`: the receiver's subscription, its preferences;
/// `desktop_pref_origin` "user" marks a value copied from the account).
pub fn desktop(
    sub: &Subscription,
    msg: &Incoming,
    facts: &ServerFacts,
    r: &Receiver,
) -> Result<(), ServerBlock> {
    let to_all = msg.trigger == Trigger::MentionAll;
    let highlighted = msg.trigger == Trigger::Highlight && r.has_highlights;
    let reply = msg.thread;
    // `mentionIds` holds the mentioned users and the thread's followers.
    let mention_to_user = msg.trigger == Trigger::MentionMe || reply;
    let direct = msg.room == RoomType::Direct;
    let large = facts.max_room_members != 0 && r.room_members > facts.max_room_members;
    let pref = sub
        .desktop_notifications
        .as_deref()
        .and_then(DesktopPref::parse);
    let from_user = sub.desktop_pref_origin.as_deref() == Some("user");

    if sub.disable_notifications == Some(true) {
        return Err(ServerBlock::RoomMuted);
    }
    let selected = selected(sub, msg, facts, r, large);
    let not_selected = || {
        if large {
            ServerBlock::LargeRoom {
                limit: facts.max_room_members,
            }
        } else {
            match pref {
                Some(p) => ServerBlock::Preference {
                    pref: p,
                    room: !from_user,
                },
                None => ServerBlock::ServerDefault(facts.default_desktop),
            }
        }
    };
    if !selected {
        return Err(not_selected());
    }
    // `sendNotification`.
    if !mention_to_user && !reply && sub.mute_group_mentions == Some(true) && to_all {
        return Err(ServerBlock::GroupMentionsMuted);
    }
    // `shouldNotifyDesktop`.
    if large && pref.is_none() && !highlighted && !mention_to_user && !reply {
        return Err(not_selected());
    }
    if !r.connected {
        return Err(ServerBlock::Offline);
    }
    if r.status == UserStatus::Busy {
        return Err(ServerBlock::Busy);
    }
    if let Some(p @ DesktopPref::Nothing) = pref {
        return Err(ServerBlock::Preference {
            pref: p,
            room: !from_user,
        });
    }
    if pref.is_none() {
        match facts.default_desktop {
            Some(DesktopPref::All) if !msg.thread || reply => return Ok(()),
            Some(DesktopPref::Nothing) => {
                return Err(ServerBlock::ServerDefault(facts.default_desktop));
            }
            _ => {}
        }
    }
    let notify = (direct
        || (!large && to_all)
        || highlighted
        || pref == Some(DesktopPref::All)
        || mention_to_user)
        && (highlighted || !msg.thread || reply);
    if notify { Ok(()) } else { Err(not_selected()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receiver() -> Receiver {
        Receiver {
            status: UserStatus::Online,
            connected: true,
            has_highlights: true,
            room_members: 3,
        }
    }

    fn fresh(t: RoomType) -> Subscription {
        Subscription {
            rid: "r".into(),
            t,
            open: true,
            ..Default::default()
        }
    }

    /// (alert, unread, userMentions, groupMentions, tunread, tunreadUser,
    /// tunreadGroup) after one message.
    fn after(
        room: RoomType,
        thread: bool,
        trigger: Trigger,
    ) -> (bool, u64, u64, u64, bool, bool, bool) {
        let mut s = fresh(room);
        let msg = Incoming {
            room,
            thread,
            trigger,
        };
        apply(&mut s, &msg, "t1", &ServerFacts::default(), &receiver());
        let has = |v: &Option<Vec<String>>| v.as_ref().is_some_and(|v| v.iter().any(|t| t == "t1"));
        (
            s.alert,
            s.unread,
            s.user_mentions,
            s.group_mentions,
            has(&s.tunread),
            has(&s.tunread_user),
            has(&s.tunread_group),
        )
    }

    /// The table verified on the dev server (8.9.1, default settings).
    #[test]
    fn counters_match_the_verified_table() {
        use RoomType::{Channel, Direct};
        use Trigger::*;
        let f = false;
        let t = true;
        assert_eq!(after(Channel, f, Plain), (t, 0, 0, 0, f, f, f));
        assert_eq!(after(Channel, f, MentionMe), (t, 1, 1, 0, f, f, f));
        assert_eq!(after(Channel, f, MentionAll), (t, 1, 0, 1, f, f, f));
        assert_eq!(after(Channel, f, Highlight), (t, 1, 1, 0, f, f, f));
        assert_eq!(after(Channel, t, Plain), (t, 0, 0, 0, t, f, f));
        assert_eq!(after(Channel, t, MentionMe), (t, 0, 0, 0, t, t, f));
        assert_eq!(after(Channel, t, MentionAll), (t, 0, 0, 0, t, f, t));
        assert_eq!(after(Channel, t, Highlight), (t, 0, 0, 0, t, t, f));
        assert_eq!(after(Direct, f, Plain), (t, 1, 0, 0, f, f, f));
    }

    #[test]
    fn unread_count_settings_change_the_counters() {
        let run = |count, room, trigger| {
            let facts = ServerFacts {
                unread_count: count,
                ..ServerFacts::default()
            };
            let mut s = fresh(room);
            let msg = Incoming {
                room,
                thread: false,
                trigger,
            };
            apply(&mut s, &msg, "t", &facts, &receiver());
            (s.unread, s.user_mentions, s.group_mentions)
        };
        use RoomType::{Channel, Direct};
        use UnreadCount::*;
        assert_eq!(run(AllMessages, Channel, Trigger::Plain), (1, 0, 0));
        assert_eq!(
            run(GroupMentionsOnly, Channel, Trigger::MentionMe),
            (0, 1, 0)
        );
        assert_eq!(
            run(UserMentionsOnly, Channel, Trigger::MentionAll),
            (0, 0, 1)
        );
        // DMs follow Unread_Count_DM; mentions there always count.
        assert_eq!(
            run(GroupMentionsOnly, Direct, Trigger::MentionMe),
            (1, 1, 0)
        );
        // Without highlight words a "highlight" is a plain message.
        let mut s = fresh(Channel);
        let r = Receiver {
            has_highlights: false,
            ..receiver()
        };
        let msg = Incoming {
            room: Channel,
            thread: false,
            trigger: Trigger::Highlight,
        };
        apply(&mut s, &msg, "t", &ServerFacts::default(), &r);
        assert_eq!((s.alert, s.unread, s.user_mentions), (true, 0, 0));
    }

    fn decide(
        sub: &Subscription,
        room: RoomType,
        thread: bool,
        trigger: Trigger,
        r: &Receiver,
    ) -> Result<(), ServerBlock> {
        let msg = Incoming {
            room,
            thread,
            trigger,
        };
        desktop(sub, &msg, &ServerFacts::default(), r)
    }

    /// Who gets a desktop notification with the server defaults (desktop
    /// notifications "all") and the account's "all" copied to the room.
    #[test]
    fn desktop_notifications_by_default() {
        use RoomType::{Channel, Direct};
        let r = receiver();
        let mut sub = fresh(Channel);
        for trigger in [
            Trigger::Plain,
            Trigger::MentionMe,
            Trigger::MentionAll,
            Trigger::Highlight,
        ] {
            assert_eq!(decide(&sub, Channel, false, trigger, &r), Ok(()));
            assert_eq!(decide(&sub, Channel, true, trigger, &r), Ok(()));
        }
        sub.desktop_notifications = Some("all".into());
        sub.desktop_pref_origin = Some("user".into());
        assert_eq!(decide(&sub, Direct, false, Trigger::Plain, &r), Ok(()));
        // Account "mentions": plain channel messages do not notify.
        sub.desktop_notifications = Some("mentions".into());
        assert_eq!(
            decide(&sub, Channel, false, Trigger::Plain, &r),
            Err(ServerBlock::Preference {
                pref: DesktopPref::Mentions,
                room: false
            })
        );
        assert_eq!(decide(&sub, Channel, false, Trigger::MentionMe, &r), Ok(()));
        assert_eq!(
            decide(&sub, Channel, false, Trigger::MentionAll, &r),
            Ok(())
        );
        assert_eq!(decide(&sub, Direct, false, Trigger::Plain, &r), Ok(()));
        assert_eq!(decide(&sub, Channel, true, Trigger::Plain, &r), Ok(()));
        // "nothing" silences everything, DMs too.
        sub.desktop_notifications = Some("nothing".into());
        sub.desktop_pref_origin = Some("subscription".into());
        assert_eq!(
            decide(&sub, Direct, false, Trigger::MentionMe, &r),
            Err(ServerBlock::Preference {
                pref: DesktopPref::Nothing,
                room: true
            })
        );
    }

    #[test]
    fn room_flags_status_and_size_block_the_notification() {
        use RoomType::Channel;
        let r = receiver();
        let mut sub = fresh(Channel);
        sub.disable_notifications = Some(true);
        assert_eq!(
            decide(&sub, Channel, false, Trigger::MentionMe, &r),
            Err(ServerBlock::RoomMuted)
        );
        let mut sub = fresh(Channel);
        sub.mute_group_mentions = Some(true);
        assert_eq!(
            decide(&sub, Channel, false, Trigger::MentionAll, &r),
            Err(ServerBlock::GroupMentionsMuted)
        );
        assert_eq!(decide(&sub, Channel, true, Trigger::MentionAll, &r), Ok(()));
        let busy = Receiver {
            status: UserStatus::Busy,
            ..r
        };
        assert_eq!(
            decide(&sub, Channel, false, Trigger::MentionMe, &busy),
            Err(ServerBlock::Busy)
        );
        let offline = Receiver {
            connected: false,
            ..r
        };
        assert_eq!(
            decide(&sub, Channel, false, Trigger::MentionMe, &offline),
            Err(ServerBlock::Offline)
        );
        // Above Notifications_Max_Room_Members a mention of a user without
        // highlight words and room preference does not notify.
        let large = Receiver {
            room_members: 101,
            has_highlights: false,
            ..r
        };
        let sub = fresh(Channel);
        assert_eq!(
            decide(&sub, Channel, false, Trigger::MentionMe, &large),
            Err(ServerBlock::LargeRoom { limit: 100 })
        );
        assert_eq!(decide(&sub, Channel, true, Trigger::Plain, &large), Ok(()));
    }

    /// The subscription query's desktop, mobile and email clauses.
    #[test]
    fn query_clauses_select_the_subscription() {
        use RoomType::Channel;
        let r = Receiver {
            has_highlights: false,
            ..receiver()
        };
        let facts = ServerFacts::default();
        let run = |sub: &Subscription, trigger, r: &Receiver, facts: &ServerFacts| {
            let msg = Incoming {
                room: Channel,
                thread: false,
                trigger,
            };
            desktop(sub, &msg, facts, r)
        };
        // @all in a small room with desktop "mentions" and mobile/email set:
        // the mobile clause queries `mobilePush` (never stored), so
        // `{mobilePush: {$exists: false}}` selects it whatever
        // `mobilePushNotifications` holds, and `shouldNotifyDesktop` lets
        // @all through.
        let mut sub = fresh(Channel);
        sub.desktop_notifications = Some("mentions".into());
        sub.desktop_pref_origin = Some("user".into());
        sub.mobile_push_notifications = Some("nothing".into());
        sub.email_notifications = Some("nothing".into());
        assert_eq!(run(&sub, Trigger::MentionAll, &r, &facts), Ok(()));
        // The opposite: a plain message is selected by no clause.
        assert_eq!(
            run(&sub, Trigger::Plain, &r, &facts),
            Err(ServerBlock::Preference {
                pref: DesktopPref::Mentions,
                room: false
            })
        );
        // A stored mobile "all" selects nothing either (wrong field).
        sub.mobile_push_notifications = Some("all".into());
        assert_eq!(
            run(&sub, Trigger::Plain, &r, &facts),
            Err(ServerBlock::Preference {
                pref: DesktopPref::Mentions,
                room: false
            })
        );

        // A mention with no desktop value and an unknown server default:
        // only the email clause (default "mentions") selects it.
        let unknown = ServerFacts {
            default_desktop: None,
            ..facts
        };
        let mut sub = fresh(Channel);
        assert_eq!(run(&sub, Trigger::MentionMe, &r, &unknown), Ok(()));
        sub.email_notifications = Some("nothing".into());
        assert_eq!(
            run(&sub, Trigger::MentionMe, &r, &unknown),
            Err(ServerBlock::ServerDefault(None))
        );
        sub.email_notifications = None;
        let no_email = ServerFacts {
            default_email: Some(DesktopPref::Nothing),
            ..unknown
        };
        assert_eq!(
            run(&sub, Trigger::MentionMe, &r, &no_email),
            Err(ServerBlock::ServerDefault(None))
        );

        // Above the limit a per-room email "all" selects it; one copied
        // from the account does not.
        let large = Receiver {
            room_members: 101,
            ..r
        };
        let mut sub = fresh(Channel);
        sub.desktop_notifications = Some("mentions".into());
        sub.desktop_pref_origin = Some("user".into());
        assert_eq!(
            run(&sub, Trigger::MentionMe, &large, &facts),
            Err(ServerBlock::LargeRoom { limit: 100 })
        );
        sub.email_notifications = Some("all".into());
        sub.email_pref_origin = Some("subscription".into());
        assert_eq!(run(&sub, Trigger::MentionMe, &large, &facts), Ok(()));
        sub.email_pref_origin = Some("user".into());
        assert_eq!(
            run(&sub, Trigger::MentionMe, &large, &facts),
            Err(ServerBlock::LargeRoom { limit: 100 })
        );
    }

    #[test]
    fn facts_from_settings() {
        let mut s = Settings::new();
        assert_eq!(ServerFacts::from_settings(&s), ServerFacts::default());
        s.set_value("Unread_Count", "all_messages".into());
        s.set_value(
            "Accounts_Default_User_Preferences_desktopNotifications",
            "mentions".into(),
        );
        s.set_value("Notifications_Max_Room_Members", 0.into());
        s.set_value(
            "Accounts_Default_User_Preferences_emailNotificationMode",
            "nothing".into(),
        );
        s.set_value("Message_AllowStarring", false.into());
        let f = ServerFacts::from_settings(&s);
        assert!(!f.allow_starring);
        assert_eq!(f.unread_count, UnreadCount::AllMessages);
        assert_eq!(f.default_desktop, Some(DesktopPref::Mentions));
        assert_eq!(f.default_email, Some(DesktopPref::Nothing));
        assert_eq!(f.max_room_members, 0);
        for c in [
            UnreadCount::AllMessages,
            UnreadCount::UserMentionsOnly,
            UnreadCount::GroupMentionsOnly,
            UnreadCount::UserAndGroupMentionsOnly,
        ] {
            assert_eq!(UnreadCount::parse(c.as_str()), Some(c));
        }
    }
}
