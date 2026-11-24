//! The sidebar and room lists under the Rocket.Chat indicator scheme
//! (explicit scheme in the prefs; nothing here switches the shared look).

#![allow(clippy::unwrap_used)]

use super::*;
use rocketchat::RcDate;
use rocketchat::models::{Room, RoomType, Subscription, User, UserRef};
use rustket_core::Change;
use rustket_core::inbox::InboxCounts;

fn sub(rid: &str, t: RoomType) -> Subscription {
    Subscription {
        id: format!("sub-{rid}"),
        rid: rid.into(),
        name: rid.into(),
        t,
        u: UserRef {
            id: "me".into(),
            username: Some("me".into()),
            name: None,
        },
        open: true,
        ..Default::default()
    }
}

fn room(rid: &str, t: RoomType, lm: i64) -> Room {
    Room {
        id: rid.into(),
        t,
        name: Some(rid.into()),
        lm: RcDate::from_millis(lm),
        ..Default::default()
    }
}

fn prefs(scheme: IndicatorScheme) -> UiPrefs {
    UiPrefs {
        indicator_scheme: scheme,
        ..UiPrefs::default()
    }
}

struct Fx {
    ws: Workspace,
    images: Images,
    prefs: UiPrefs,
    collapsed: BTreeSet<String>,
    sid: ServerId,
    url: ServerUrl,
    sidebar: Sidebar,
}

impl Fx {
    /// Rooms from `subs` (the newest first), under the Rocket.Chat scheme.
    fn new(subs: Vec<Subscription>) -> Self {
        let mut ws = Workspace::new();
        ws.apply_me(User {
            id: "me".into(),
            username: Some("me".into()),
            ..Default::default()
        });
        for (i, s) in subs.into_iter().enumerate() {
            let lm = 1_700_000_000_000 - i as i64 * 1000;
            let mut r = room(&s.rid, s.t, lm);
            if s.t == RoomType::Direct {
                r.usernames = Some(vec!["me".into(), s.rid.clone()]);
                r.uids = Some(vec!["me".into(), format!("{}-id", s.rid)]);
            }
            ws.apply_room(Change::Inserted, r);
            ws.apply_subscription(Change::Inserted, s);
        }
        Self {
            ws,
            images: Images::new(None),
            prefs: prefs(IndicatorScheme::RocketChat),
            collapsed: BTreeSet::new(),
            sid: ServerId("s".into()),
            url: ServerUrl::parse("http://localhost:3000").unwrap(),
            sidebar: Sidebar::default(),
        }
    }

    fn refresh(&mut self) {
        self.sidebar
            .refresh(&self.ws, &self.prefs, &self.images, &self.sid, &self.url, 0);
    }

    fn ctx(&self) -> SidebarCtx<'_> {
        SidebarCtx {
            server_id: &self.sid,
            server: &self.url,
            workspace: &self.ws,
            images: &self.images,
            prefs: &self.prefs,
            collapsed: &self.collapsed,
            active_room: None,
            active_thread: None,
            inbox_open: None,
            later_open: false,
            drafts_open: false,
            search_open: false,
            now: Instant::now(),
        }
    }

    fn item(&self, rid: &str) -> RoomListItem {
        self.sidebar
            .sections
            .iter()
            .flat_map(|s| &s.items)
            .find(|i| i.rid == rid)
            .unwrap()
            .clone()
    }

    fn props(&self, rid: &str) -> RoomListItemProps<Message> {
        self.sidebar.row_props(&self.item(rid), &self.ctx())
    }

    /// Threads mode Unread; parents for every thread in `tmids` of `rid`.
    fn threads(&mut self, rid: &str, tmids: &[&str]) {
        use rustket_core::config::ThreadsMode;
        self.prefs.sidebar_threads = ThreadsMode::Unread;
        self.ws.set_thread_config(ThreadsMode::Unread, []);
        let parents = tmids
            .iter()
            .enumerate()
            .map(|(i, t)| rocketchat::models::Message {
                id: (*t).into(),
                rid: rid.into(),
                msg: format!("topic {t}"),
                tcount: Some(2),
                tlm: RcDate::from_millis(1_750_000_000_000 - i as i64 * 1000),
                u: UserRef {
                    id: "bobid".into(),
                    username: Some("bob".into()),
                    name: None,
                },
                ..Default::default()
            })
            .collect();
        self.ws.apply_thread_parents(parents);
    }
}

fn ids(v: &[&str]) -> Option<Vec<String>> {
    Some(v.iter().map(|s| (*s).to_owned()).collect())
}

/// The rooms of the live probe (rc-live-observations.md) drawn as RC rows:
/// the variant and full counter, never a dot, the highlighted name by
/// `highlightUnread`; muting changes nothing but the bell icon.
#[test]
fn rows_draw_the_rocketchat_badge() {
    let plain = Subscription {
        alert: true,
        ..sub("plain", RoomType::Channel)
    };
    let dm = Subscription {
        unread: 2,
        alert: true,
        ..sub("bob", RoomType::Direct)
    };
    let muted = Subscription {
        unread: 1,
        user_mentions: 1,
        alert: true,
        disable_notifications: Some(true),
        ..sub("muted", RoomType::Channel)
    };
    let thread_and_all = Subscription {
        unread: 1,
        group_mentions: 1,
        alert: true,
        tunread: ids(&["t1"]),
        ..sub("thread-and-all", RoomType::Channel)
    };
    let busy = Subscription {
        unread: 137,
        alert: true,
        ..sub("busy", RoomType::Channel)
    };
    let hidden = Subscription {
        unread: 1,
        user_mentions: 1,
        alert: true,
        hide_unread_status: Some(true),
        ..sub("hidden", RoomType::Channel)
    };
    let mute_group = Subscription {
        unread: 1,
        group_mentions: 1,
        alert: true,
        mute_group_mentions: Some(true),
        ..sub("mute-group", RoomType::Channel)
    };
    let read = sub("read", RoomType::Channel);
    let mut fx = Fx::new(vec![
        plain,
        dm,
        muted,
        thread_and_all,
        busy,
        hidden,
        mute_group,
        read,
    ]);
    fx.refresh();
    let row = |rid: &str| {
        let p = fx.props(rid);
        (p.badge_kind(), p.badge_count(), p.is_bold())
    };
    assert_eq!(row("plain"), (None, 0, true), "highlight only, no dot");
    assert_eq!(row("bob"), (Some(BadgeKind::Unread), 2, true));
    assert_eq!(row("muted"), (Some(BadgeKind::Mention), 1, true));
    assert!(fx.props("muted").is_muted, "the bell-off icon stays");
    assert_eq!(row("thread-and-all"), (Some(BadgeKind::Thread), 2, true));
    assert_eq!(row("busy"), (Some(BadgeKind::Unread), 137, true));
    assert_eq!(row("hidden"), (Some(BadgeKind::Mention), 1, false));
    assert_eq!(row("mute-group"), (Some(BadgeKind::GroupMention), 1, true));
    assert_eq!(row("read"), (None, 0, false));
    for rid in ["bob", "muted", "busy", "hidden"] {
        let p = fx.props(rid);
        assert_eq!(p.cap, BadgeCap::None, "{rid}");
        assert!(p.badge_tip.is_some(), "{rid}: the itemised tooltip");
    }
    assert!(fx.props("plain").badge_tip.is_none());
    assert!(fx.props("read").badge_tip.is_none());

    // The same rooms under Rustket keep today's rows.
    fx.prefs = prefs(IndicatorScheme::Rustket);
    fx.refresh();
    let p = fx.props("plain");
    assert_eq!(
        (p.badge_kind(), p.badge_count()),
        (Some(BadgeKind::Unread), 0)
    );
    assert_eq!(fx.props("bob").badge_kind(), Some(BadgeKind::Mention));
    for rid in ["plain", "bob", "busy"] {
        let p = fx.props(rid);
        assert_eq!((p.cap, p.badge_tip.is_some()), (BadgeCap::At99, false));
    }
}

/// `set_row_badge_rc` draws exactly the core badge.
#[test]
fn row_badge_props_match_the_rc_badge() {
    for info in [
        UnreadInfo::default(),
        UnreadInfo {
            alert: true,
            ..UnreadInfo::default()
        },
        UnreadInfo {
            unread: 5,
            user_mentions: 2,
            tunread: 1,
            alert: true,
            ..UnreadInfo::default()
        },
        UnreadInfo {
            unread: 1000,
            ..UnreadInfo::default()
        },
    ] {
        let b = indicators::rc_badge_of(info);
        let mut p = RoomListItemProps::new(
            rustket_ui::widget::AvatarSource::initials("r", "r"),
            "r",
            (),
        );
        set_row_badge_rc(&mut p, &b);
        assert_eq!(p.badge_kind(), b.kind().map(widget_kind), "{info:?}");
        assert_eq!(u64::from(p.badge_count()), b.count, "{info:?}");
        assert_eq!(p.is_bold(), b.highlighted);
        assert_eq!(p.badge_tip.is_some(), b.count > 0);
    }
}

/// RC's `getUnreadTitle` in the five locales: parts in RC's order, plural
/// forms, the locale's separator.
#[test]
fn itemised_title_in_every_locale() {
    let probe = UnreadTitle {
        mentions: 3,
        threads: 2,
        group_mentions: 0,
        rest: 1,
    };
    assert_eq!(
        rc_unread_title_in(&probe, "en"),
        "3 mentions, 2 unread threaded messages, 1 unread message"
    );
    let all = UnreadTitle {
        mentions: 1,
        threads: 1,
        group_mentions: 1,
        rest: 1,
    };
    assert_eq!(
        rc_unread_title_in(&all, "en"),
        "1 mention, 1 unread threaded message, 1 group mention, 1 unread message"
    );
    assert_eq!(
        rc_unread_title_in(&probe, "ru"),
        "3 упоминания, 2 непрочитанных сообщения в тредах, 1 непрочитанное сообщение"
    );
    let five = UnreadTitle {
        rest: 5,
        ..UnreadTitle::default()
    };
    assert_eq!(rc_unread_title_in(&five, "ru"), "5 непрочитанных сообщений");
    assert_eq!(rc_unread_title_in(&five, "es"), "5 mensajes no leídos");
    assert_eq!(
        rc_unread_title_in(&probe, "zh"),
        "3 条提及，2 条未读讨论串消息，1 条未读消息"
    );
    assert_eq!(
        rc_unread_title_in(&probe, "th"),
        "การกล่าวถึง 3 รายการ ข้อความในเธรดที่ยังไม่ได้อ่าน 2 รายการ ข้อความที่ยังไม่ได้อ่าน 1 รายการ"
    );
    assert_eq!(rc_unread_title_in(&UnreadTitle::default(), "en"), "");
}

/// A collapsed section counts like RC's collapser: the hidden rooms' unread
/// and threads, one per alert-only room, hidden-status rooms skipped; the
/// colour of the strongest RC kind.
#[test]
fn collapsed_counter_sums_the_hidden_rows() {
    let mention = Subscription {
        unread: 3,
        user_mentions: 3,
        alert: true,
        ..sub("a-mention", RoomType::Channel)
    };
    let threads = Subscription {
        alert: true,
        tunread: ids(&["x1", "x2"]),
        ..sub("b-threads", RoomType::Channel)
    };
    let plain = Subscription {
        alert: true,
        ..sub("c-plain", RoomType::Channel)
    };
    let hidden = Subscription {
        unread: 9,
        user_mentions: 9,
        alert: true,
        hide_unread_status: Some(true),
        ..sub("d-hidden", RoomType::Channel)
    };
    let mut fx = Fx::new(vec![mention, threads, plain, hidden]);
    fx.refresh();
    // All four rooms in one section, wherever the prefs list them.
    let all = Section {
        items: fx
            .sidebar
            .sections
            .iter()
            .flat_map(|s| s.items.clone())
            .collect(),
        threads: Vec::new(),
        ..fx.sidebar.sections[0].clone()
    };
    assert_eq!(all.items.len(), 4);
    let b = collapsed_rc(&all, &fx.ws).unwrap();
    assert_eq!((b.count, b.variant), (6, BadgeVariant::Mention));
    assert!(b.highlighted, "the bold header");
    assert_eq!(
        rc_unread_title_in(&b.title, "en"),
        "3 mentions, 2 unread threaded messages, 1 unread message"
    );
    // Nothing to count: no counter, no emphasis.
    let mut fx = Fx::new(vec![sub("read", RoomType::Channel)]);
    fx.refresh();
    assert!(
        fx.sidebar
            .sections
            .iter()
            .all(|s| collapsed_rc(s, &fx.ws).is_none())
    );
}

/// Thread rows keep Rustket's presence (listing, order) in both schemes;
/// under RC their colour follows the subscription, room settings aside, and
/// each adds one unread thread to a collapsed header (a thread mention red).
#[test]
fn thread_rows_keep_presence_and_take_rc_colours() {
    let s = Subscription {
        alert: true,
        disable_notifications: Some(true),
        tunread: ids(&["t-plain", "t-me", "t-all"]),
        tunread_user: ids(&["t-me"]),
        tunread_group: ids(&["t-all"]),
        ..sub("room", RoomType::Channel)
    };
    let hidden = Subscription {
        alert: true,
        hide_unread_status: Some(true),
        tunread: ids(&["h-plain"]),
        ..sub("hidden", RoomType::Channel)
    };
    let mut fx = Fx::new(vec![s, hidden]);
    fx.threads("room", &["t-plain", "t-me", "t-all"]);
    let parent = rocketchat::models::Message {
        id: "h-plain".into(),
        rid: "hidden".into(),
        msg: "topic".into(),
        tcount: Some(1),
        tlm: RcDate::from_millis(1_740_000_000_000),
        ..Default::default()
    };
    fx.ws.apply_thread_parents(vec![parent]);
    let listed = |fx: &mut Fx, scheme| {
        fx.prefs.indicator_scheme = scheme;
        fx.refresh();
        fx.sidebar
            .sections
            .iter()
            .find(|s| s.is(SectionKind::Threads))
            .unwrap()
            .clone()
    };
    let rustket = listed(&mut fx, IndicatorScheme::Rustket);
    let rc = listed(&mut fx, IndicatorScheme::RocketChat);
    let tmids = |s: &Section| s.threads.iter().map(|t| t.tmid.clone()).collect::<Vec<_>>();
    assert_eq!(tmids(&rustket), tmids(&rc), "same rows, same order");
    assert!(tmids(&rc).contains(&"t-plain".to_owned()));
    for t in &rc.threads {
        assert_eq!(
            thread_kind_in(&fx.ws, t, IndicatorScheme::Rustket).is_some(),
            thread_kind_in(&fx.ws, t, IndicatorScheme::RocketChat).is_some(),
            "{}",
            t.tmid
        );
    }
    let kind = |tmid: &str| {
        let t = rc.threads.iter().find(|t| t.tmid == tmid).unwrap();
        (
            thread_kind_in(&fx.ws, t, IndicatorScheme::Rustket),
            thread_kind_in(&fx.ws, t, IndicatorScheme::RocketChat),
        )
    };
    // Muted room: Rustket's grey, RC's colours.
    assert_eq!(kind("t-plain").1, Some(BadgeKind::Thread));
    assert_eq!(kind("t-me").1, Some(BadgeKind::Mention));
    assert_eq!(kind("t-all").1, Some(BadgeKind::GroupMention));
    assert_eq!(kind("t-plain").0, Some(BadgeKind::Muted));
    // The drawn row: the RC colour, highlighted while the dot shows
    // (Rustket's grey under the Rustket scheme).
    let t = rc.threads.iter().find(|t| t.tmid == "t-plain").unwrap();
    let p = thread_row_props(t, &fx.ctx());
    assert_eq!(
        (p.badge_kind(), p.is_bold()),
        (Some(BadgeKind::Thread), true)
    );
    fx.prefs.indicator_scheme = IndicatorScheme::Rustket;
    let p = thread_row_props(t, &fx.ctx());
    assert_eq!(p.badge_kind(), Some(BadgeKind::Muted));
    fx.prefs.indicator_scheme = IndicatorScheme::RocketChat;
    // Collapsed: 3 unread threads, one of them a mention.
    let b = collapsed_rc(&rc, &fx.ws).unwrap();
    assert_eq!((b.count, b.variant), (3, BadgeVariant::Mention));
    assert_eq!(
        rc_unread_title_in(&b.title, "en"),
        "1 mention, 3 unread threaded messages"
    );
    // With the room's own row in the section (Favorites holding a room and
    // its starred thread), its threads count once, through the room.
    let room_row = fx
        .sidebar
        .sections
        .iter()
        .flat_map(|s| &s.items)
        .find(|i| i.rid == "room")
        .unwrap()
        .clone();
    let both = Section {
        items: vec![room_row.clone()],
        ..rc.clone()
    };
    let alone = Section {
        items: vec![room_row],
        threads: Vec::new(),
        ..rc.clone()
    };
    assert_eq!(collapsed_rc(&both, &fx.ws), collapsed_rc(&alone, &fx.ws));
}

/// The pinned rows under RC: Activity shows its RC total (threads plus
/// main-timeline mentions) in RC's colour order; Drafts' quiet counter is
/// grey; the counters are not capped.
#[test]
fn pinned_rows_follow_rocketchat() {
    let c = |mentions, group_mentions, threads| InboxCounts {
        unread_rooms: 0,
        mentions,
        group_mentions,
        threads,
    };
    assert_eq!(
        activity_badge_rc(&c(2, 3, 4), 6),
        Some((6, BadgeKind::Mention))
    );
    assert_eq!(
        activity_badge_rc(&c(0, 3, 4), 4),
        Some((4, BadgeKind::Thread))
    );
    assert_eq!(
        activity_badge_rc(&c(0, 3, 0), 3),
        Some((3, BadgeKind::GroupMention))
    );
    assert_eq!(activity_badge_rc(&c(0, 0, 0), 0), None);
    // The pinned rows dispatch per scheme; the RC total is asked for under
    // Rocket.Chat only.
    let counts = InboxCounts {
        unread_rooms: 150,
        ..c(1, 0, 1)
    };
    let (rk, rk_cap) = pinned_badges(
        IndicatorScheme::Rustket,
        &counts,
        || unreachable!("RC total under Rustket"),
        2,
        3,
        0,
    );
    assert_eq!(
        rk,
        [
            Some((150, BadgeKind::Unread)),
            Some((1, BadgeKind::Mention)),
            Some((2, BadgeKind::Mention)),
            Some((3, BadgeKind::Muted)),
        ]
    );
    assert_eq!(rk_cap, BadgeCap::At99);
    let (rc, rc_cap) = pinned_badges(IndicatorScheme::RocketChat, &counts, || 1, 2, 3, 0);
    assert_eq!(
        rc,
        [
            Some((150, BadgeKind::Unread)),
            Some((1, BadgeKind::Mention)),
            Some((2, BadgeKind::Mention)),
            Some((3, BadgeKind::Unread)),
        ]
    );
    assert_eq!(rc_cap, BadgeCap::None);
    assert_eq!(drafts_badge_rc(4, 1), Some((1, BadgeKind::Mention)));
    assert_eq!(drafts_badge_rc(4, 0), Some((4, BadgeKind::Unread)));
    assert_eq!(drafts_badge_rc(0, 0), None);
    let _: Element<'_, ()> = pinned_row_in(
        Icon::Inbox,
        "All unreads".into(),
        None,
        Some((150, BadgeKind::Unread)),
        BadgeCap::None,
        false,
        (),
    );
}

/// Omnichannel groups under RC: queued chats add nothing to the collapsed
/// counter; chats in progress count like rooms.
#[test]
fn omnichannel_groups_count_only_their_rooms() {
    use rustket_core::omnichannel::room_list::{OmnichannelSectionKind, QueueRow};
    let queue = QueueRow {
        inquiry_id: "inq".into(),
        rid: "q".into(),
        name: "visitor".into(),
        preview: None,
        ts: None,
        decor: Default::default(),
    };
    let only_queue = crate::omnichannel::sidebar::Group {
        kind: OmnichannelSectionKind::Queued,
        queue: vec![queue],
        rooms: Vec::new(),
    };
    assert_eq!(crate::omnichannel::sidebar::collapsed_rc(&only_queue), None);
    let mut fx = Fx::new(vec![Subscription {
        unread: 2,
        alert: true,
        ..sub("chat", RoomType::Channel)
    }]);
    fx.refresh();
    let item = fx.item("chat");
    let open = crate::omnichannel::sidebar::Group {
        kind: OmnichannelSectionKind::Open,
        queue: Vec::new(),
        rooms: vec![&item],
    };
    let b = crate::omnichannel::sidebar::collapsed_rc(&open).unwrap();
    assert_eq!((b.count, b.variant), (2, BadgeVariant::Unread));
}

/// The quick switcher carries the RC badge under RC (its row draws it in the
/// content palette) and nothing extra under Rustket.
#[test]
fn quick_switcher_carries_the_rc_badge() {
    use crate::dialogs::quick_switcher::{Item, results};
    let mut fx = Fx::new(vec![
        Subscription {
            alert: true,
            ..sub("dev-plain", RoomType::Channel)
        },
        Subscription {
            unread: 120,
            user_mentions: 1,
            alert: true,
            ..sub("dev-busy", RoomType::Channel)
        },
    ]);
    fx.refresh();
    let rc_of = |opts: &RoomListOptions, rid: &str| {
        results(&fx.ws, opts, "dev", None)
            .into_iter()
            .find_map(|i| match i {
                Item::Local { rid: r, rc, .. } if r == rid => Some(rc),
                _ => None,
            })
            .unwrap()
    };
    let rc = RoomListOptions::from_prefs(&prefs(IndicatorScheme::RocketChat), &fx.ws);
    let b = rc_of(&rc, "dev-busy");
    assert_eq!((b.count, b.kind()), (120, Some(BadgeVariant::Mention)));
    let b = rc_of(&rc, "dev-plain");
    assert_eq!((b.count, b.kind(), b.highlighted), (0, None, true));
    let rustket = RoomListOptions::from_prefs(&prefs(IndicatorScheme::Rustket), &fx.ws);
    assert_eq!(rc_of(&rustket, "dev-busy"), RcBadge::default());
}

/// The sidebar builds with collapsed sections, a loading Threads header and
/// the pinned rows under RC.
#[test]
fn rocketchat_sidebar_builds() {
    let mut fx = Fx::new(vec![
        Subscription {
            unread: 3,
            user_mentions: 1,
            alert: true,
            tunread: ids(&["t1"]),
            ..sub("a", RoomType::Channel)
        },
        Subscription {
            alert: true,
            ..sub("b", RoomType::Channel)
        },
    ]);
    fx.threads("a", &["t1"]);
    fx.prefs.sidebar_pinned_views = true;
    fx.refresh();
    fx.sidebar.set_thread_progress(Some((1, 3)));
    for s in &fx.sidebar.sections {
        fx.collapsed.insert(s.id.collapse_key().into_owned());
    }
    let view = fx.sidebar.view(&fx.ctx(), iced::widget::space().into());
    let _ = iced::advanced::widget::Tree::new(view.as_widget());
}
