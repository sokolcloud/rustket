#![allow(clippy::unwrap_used)]
use super::*;
use crate::room_list::{RoomListOptions, compute, in_unread_block};
use crate::test_util::{me, room, sub};
use crate::workspace::Change;
use rocketchat::models::RoomType;

use BadgeVariant::{GroupMention, Mention, Muted, Thread, Unread};

type Fill = fn(&mut Subscription);

fn ids(n: usize, prefix: &str) -> Option<Vec<String>> {
    Some((0..n).map(|i| format!("{prefix}{i}")).collect())
}

/// A subscription built by `f` from a plain open channel.
fn s_with(t: RoomType, f: impl FnOnce(&mut Subscription)) -> Subscription {
    let mut s = sub("r", "room", t);
    f(&mut s);
    s
}

fn entry(s: Subscription) -> RoomEntry {
    RoomEntry {
        room: None,
        sub: Some(s),
    }
}

fn badge_of(t: RoomType, f: impl FnOnce(&mut Subscription)) -> RcBadge {
    rc_badge(&entry(s_with(t, f)))
}

/// (count, variant when counted, highlighted)
fn shown(b: RcBadge) -> (u64, Option<BadgeVariant>, bool) {
    (b.count, b.kind(), b.highlighted)
}

fn title(mentions: u64, threads: u64, group_mentions: u64, rest: u64) -> UnreadTitle {
    UnreadTitle {
        mentions,
        threads,
        group_mentions,
        rest,
    }
}

// --- the worked examples of research/rc-sidebar.md §11 ---------------------

#[test]
fn ex01_plain_channel_message_brightens_only() {
    let b = badge_of(RoomType::Channel, |s| s.alert = true);
    assert_eq!(shown(b), (0, None, true));
    assert!(b.marked);
    assert!(b.title.is_empty());
}

#[test]
fn ex02_mention_is_danger() {
    let b = badge_of(RoomType::Channel, |s| {
        s.alert = true;
        s.unread = 1;
        s.user_mentions = 1;
    });
    assert_eq!(shown(b), (1, Some(Mention), true));
    assert_eq!(b.title, title(1, 0, 0, 0));
}

#[test]
fn ex03_mention_all_and_plain() {
    let b = badge_of(RoomType::Channel, |s| {
        s.alert = true;
        s.unread = 3;
        s.user_mentions = 1;
        s.group_mentions = 1;
    });
    assert_eq!(shown(b), (3, Some(Mention), true));
    assert_eq!(b.title, title(1, 0, 1, 1));
}

#[test]
fn ex04_group_mentions_are_warning() {
    let b = badge_of(RoomType::Channel, |s| {
        s.alert = true;
        s.unread = 2;
        s.group_mentions = 2;
    });
    assert_eq!(shown(b), (2, Some(GroupMention), true));
    assert_eq!(b.title, title(0, 0, 2, 0));
}

#[test]
fn ex05_dm_is_secondary() {
    let b = badge_of(RoomType::Direct, |s| {
        s.alert = true;
        s.unread = 3;
    });
    assert_eq!(shown(b), (3, Some(Unread), true));
    assert_eq!(b.title, title(0, 0, 0, 3));
}

#[test]
fn ex06_dm_with_thread_is_primary() {
    let b = badge_of(RoomType::Direct, |s| {
        s.alert = true;
        s.unread = 3;
        s.tunread = ids(1, "t");
    });
    assert_eq!(shown(b), (4, Some(Thread), true));
    assert_eq!(b.title, title(0, 1, 0, 3));
}

#[test]
fn ex07_thread_reply_as_participant() {
    let b = badge_of(RoomType::Channel, |s| {
        s.alert = true;
        s.tunread = ids(1, "t");
    });
    assert_eq!(shown(b), (1, Some(Thread), true));
    assert_eq!(b.title, title(0, 1, 0, 0));
}

#[test]
fn ex08_thread_reply_as_follower_only_is_dim() {
    let b = badge_of(RoomType::Channel, |s| s.tunread = ids(1, "t"));
    assert_eq!(shown(b), (1, Some(Thread), false));
    assert!(b.marked);
}

#[test]
fn ex09_thread_mention_is_danger() {
    let b = badge_of(RoomType::Channel, |s| {
        s.alert = true;
        s.tunread = ids(1, "t");
        s.tunread_user = ids(1, "t");
    });
    assert_eq!(shown(b), (1, Some(Mention), true));
    assert_eq!(b.title, title(1, 1, 0, 0));
}

#[test]
fn ex10_thread_group_mention_is_ignored() {
    let b = badge_of(RoomType::Channel, |s| {
        s.alert = true;
        s.tunread = ids(1, "t");
        s.tunread_group = ids(1, "t");
    });
    assert_eq!(shown(b), (1, Some(Thread), true));
    assert_eq!(b.title, title(0, 1, 0, 0));
}

#[test]
fn ex11_threads_beat_group_mentions() {
    let b = badge_of(RoomType::Channel, |s| {
        s.unread = 1;
        s.group_mentions = 1;
        s.tunread = ids(1, "t");
    });
    assert_eq!(shown(b), (2, Some(Thread), true));
    assert_eq!(b.title, title(0, 1, 1, 0));
}

#[test]
fn ex12_no_cap() {
    let b = badge_of(RoomType::Direct, |s| s.unread = 1234);
    assert_eq!(shown(b), (1234, Some(Unread), true));
    assert_eq!(b.title, title(0, 0, 0, 1234));
}

#[test]
fn ex13_ex14_room_settings_change_nothing() {
    let muted = badge_of(RoomType::Channel, |s| {
        s.alert = true;
        s.unread = 1;
        s.user_mentions = 1;
        s.disable_notifications = Some(true);
    });
    assert_eq!(shown(muted), (1, Some(Mention), true));
    let mute_group = badge_of(RoomType::Channel, |s| {
        s.alert = true;
        s.unread = 2;
        s.group_mentions = 2;
        s.mute_group_mentions = Some(true);
    });
    assert_eq!(shown(mute_group), (2, Some(GroupMention), true));
}

#[test]
fn ex15_hidden_unread_status_hides_plain_counts() {
    let b = badge_of(RoomType::Direct, |s| {
        s.alert = true;
        s.unread = 5;
        s.hide_unread_status = Some(true);
    });
    assert_eq!(shown(b), (0, None, false));
    assert!(!b.marked);
}

#[test]
fn ex16_hidden_unread_status_keeps_mentions() {
    let b = badge_of(RoomType::Channel, |s| {
        s.alert = true;
        s.unread = 2;
        s.user_mentions = 1;
        s.hide_unread_status = Some(true);
    });
    assert_eq!(shown(b), (2, Some(Mention), false));
    assert!(b.marked);
    assert_eq!(b.title, title(1, 0, 0, 1));
}

#[test]
fn ex17_hidden_unread_and_mention_status() {
    let b = badge_of(RoomType::Channel, |s| {
        s.unread = 1;
        s.user_mentions = 1;
        s.hide_unread_status = Some(true);
        s.hide_mention_status = Some(true);
    });
    assert_eq!(shown(b), (0, None, false));
    assert!(!b.marked);
}

#[test]
fn ex18_hidden_mention_status_alone_changes_nothing() {
    let b = badge_of(RoomType::Channel, |s| {
        s.unread = 1;
        s.user_mentions = 1;
        s.hide_mention_status = Some(true);
    });
    assert_eq!(shown(b), (1, Some(Mention), true));
}

#[test]
fn ex19_mark_unread() {
    let b = badge_of(RoomType::Channel, |s| {
        s.alert = true;
        s.unread = 1;
    });
    assert_eq!(shown(b), (1, Some(Unread), true));
    assert_eq!(b.title, title(0, 0, 0, 1));
}

#[test]
fn ex20_ex21_open_room_is_not_special() {
    let threads = badge_of(RoomType::Channel, |s| {
        s.alert = true;
        s.tunread = ids(1, "t");
    });
    assert_eq!(shown(threads), (1, Some(Thread), true));
    let dm = badge_of(RoomType::Direct, |s| {
        s.alert = true;
        s.unread = 2;
    });
    assert_eq!(shown(dm), (2, Some(Unread), true));
}

#[test]
fn ex27_livechat_counts_grey() {
    let b = badge_of(RoomType::Livechat, |s| {
        s.alert = true;
        s.unread = 2;
    });
    assert_eq!(shown(b), (2, Some(Unread), true));
}

#[test]
fn no_subscription_no_badge() {
    let e = RoomEntry {
        room: Some(room("r", "room", RoomType::Channel)),
        sub: None,
    };
    assert_eq!(rc_badge(&e), RcBadge::default());
}

// --- the live table of rc-live-observations.md -----------------------------

#[test]
fn live_table_is_reproduced() {
    type Row = (
        &'static str,
        RoomType,
        Fill,
        u64,
        Option<BadgeVariant>,
        bool,
    );
    let rows: &[Row] = &[
        ("read", RoomType::Channel, |_| {}, 0, None, false),
        (
            "plain",
            RoomType::Channel,
            |s| s.alert = true,
            0,
            None,
            true,
        ),
        (
            "mention",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.unread = 1;
                s.user_mentions = 1;
            },
            1,
            Some(Mention),
            true,
        ),
        (
            "all",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.unread = 1;
                s.group_mentions = 1;
            },
            1,
            Some(GroupMention),
            true,
        ),
        (
            "here",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.unread = 1;
                s.group_mentions = 1;
            },
            1,
            Some(GroupMention),
            true,
        ),
        (
            "thread",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.tunread = ids(1, "t");
            },
            1,
            Some(Thread),
            true,
        ),
        (
            "thread-mention",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.tunread = ids(1, "t");
                s.tunread_user = ids(1, "t");
            },
            1,
            Some(Mention),
            true,
        ),
        (
            "thread-all",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.tunread = ids(1, "t");
                s.tunread_group = ids(1, "t");
            },
            1,
            Some(Thread),
            true,
        ),
        (
            "thread-and-all",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.unread = 1;
                s.group_mentions = 1;
                s.tunread = ids(1, "t");
            },
            2,
            Some(Thread),
            true,
        ),
        (
            "mixed",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.unread = 1;
                s.user_mentions = 1;
                s.tunread = ids(1, "t");
            },
            2,
            Some(Mention),
            true,
        ),
        (
            "muted",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.unread = 1;
                s.user_mentions = 1;
                s.disable_notifications = Some(true);
            },
            1,
            Some(Mention),
            true,
        ),
        (
            "muted-all",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.unread = 1;
                s.group_mentions = 1;
                s.disable_notifications = Some(true);
            },
            1,
            Some(GroupMention),
            true,
        ),
        (
            "muted-plain",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.disable_notifications = Some(true);
            },
            0,
            None,
            true,
        ),
        (
            "hidden",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.unread = 1;
                s.user_mentions = 1;
                s.hide_unread_status = Some(true);
            },
            1,
            Some(Mention),
            false,
        ),
        (
            "mute-group",
            RoomType::Channel,
            |s| {
                s.alert = true;
                s.unread = 1;
                s.group_mentions = 1;
                s.mute_group_mentions = Some(true);
            },
            1,
            Some(GroupMention),
            true,
        ),
        (
            "private",
            RoomType::Private,
            |s| {
                s.alert = true;
                s.unread = 1;
                s.user_mentions = 1;
            },
            1,
            Some(Mention),
            true,
        ),
        (
            "DM bob",
            RoomType::Direct,
            |s| {
                s.alert = true;
                s.unread = 2;
            },
            2,
            Some(Unread),
            true,
        ),
    ];
    for (name, t, fill, count, kind, bright) in rows {
        let b = badge_of(*t, fill);
        assert_eq!(shown(b), (*count, *kind, *bright), "{name}");
    }
    // Title: "11 unread messages" over the non-hidden rooms, threads aside
    // (the seed has one @all and one @here room).
    let mut ws = Workspace::new();
    ws.apply_me(me("me", "me"));
    for (i, (_, t, fill, ..)) in rows.iter().enumerate() {
        let mut s = s_with(*t, fill);
        s.rid = format!("r{i}");
        s.id = format!("s{i}");
        ws.apply_subscription(Change::Inserted, s);
    }
    assert_eq!(
        rc_workspace_signal(&ws),
        RcSignal {
            unread: 11,
            bullet: true
        }
    );
}

// --- collapsed sections ------------------------------------------------------

fn info(f: impl FnOnce(&mut Subscription)) -> UnreadInfo {
    UnreadInfo::of(&s_with(RoomType::Channel, f))
}

#[test]
fn collapsed_probe_is_danger_six() {
    let mention = || {
        info(|s| {
            s.alert = true;
            s.unread = 1;
            s.user_mentions = 1;
        })
    };
    let thread = || {
        info(|s| {
            s.alert = true;
            s.tunread = ids(1, "t");
        })
    };
    let alert_only = info(|s| s.alert = true);
    let read = info(|_| {});
    let b = rc_collapsed([
        mention(),
        mention(),
        mention(),
        thread(),
        thread(),
        alert_only,
        read,
    ])
    .unwrap();
    assert_eq!(b.count, 6);
    assert_eq!(b.variant, Mention);
    assert!(b.highlighted && b.marked);
    assert_eq!(b.title, title(3, 2, 0, 1));
}

#[test]
fn collapsed_counts_one_per_alert_only_room() {
    let b = rc_collapsed((0..4).map(|_| info(|s| s.alert = true))).unwrap();
    assert_eq!((b.count, b.variant), (4, Unread));
    assert_eq!(b.title, title(0, 0, 0, 4));
    // An alert with a thread adds only the thread.
    let b = rc_collapsed([info(|s| {
        s.alert = true;
        s.tunread = ids(1, "t");
    })])
    .unwrap();
    assert_eq!((b.count, b.variant), (1, Thread));
}

#[test]
fn collapsed_skips_hidden_rooms() {
    let hidden = info(|s| {
        s.alert = true;
        s.unread = 1;
        s.user_mentions = 1;
        s.hide_unread_status = Some(true);
    });
    assert_eq!(rc_collapsed([hidden]), None);
    assert_eq!(rc_collapsed([]), None);
    assert_eq!(rc_collapsed([info(|_| {})]), None);
    let plain = info(|s| {
        s.alert = true;
        s.unread = 2;
        s.group_mentions = 2;
    });
    let b = rc_collapsed([hidden, plain]).unwrap();
    assert_eq!((b.count, b.variant), (2, GroupMention));
}

#[test]
fn collapsed_thread_rows() {
    let b = rc_collapsed([UnreadInfo::thread(Thread), UnreadInfo::thread(GroupMention)]).unwrap();
    assert_eq!((b.count, b.variant), (2, Thread));
    assert_eq!(b.title, title(0, 2, 0, 0));
    let b = rc_collapsed([UnreadInfo::thread(Mention)]).unwrap();
    assert_eq!((b.count, b.variant), (1, Mention));
    assert_eq!(b.title, title(1, 1, 0, 0));
}

// --- threads -----------------------------------------------------------------

#[test]
fn thread_kind_follows_the_lists_only() {
    let s = s_with(RoomType::Channel, |s| {
        s.tunread = Some(vec!["a".into(), "b".into(), "c".into()]);
        s.tunread_user = Some(vec!["a".into(), "x".into()]);
        s.tunread_group = Some(vec!["a".into(), "b".into()]);
        s.disable_notifications = Some(true);
        s.hide_unread_status = Some(true);
        s.hide_mention_status = Some(true);
        s.mute_group_mentions = Some(true);
    });
    assert_eq!(rc_thread_kind(&s, "a"), Some(Mention));
    assert_eq!(rc_thread_kind(&s, "b"), Some(GroupMention));
    assert_eq!(rc_thread_kind(&s, "c"), Some(Thread));
    // Not in `tunread`: no dot.
    assert_eq!(rc_thread_kind(&s, "x"), None);
    assert_eq!(rc_thread_kind(&s, "z"), None);
}

#[test]
fn threads_button_counts_and_orders() {
    let none = s_with(RoomType::Channel, |_| {});
    assert_eq!(rc_threads_button(&none), None);
    let one = s_with(RoomType::Channel, |s| s.tunread = ids(1, "t"));
    assert_eq!(rc_threads_button(&one), Some((1, Thread)));
    let many = s_with(RoomType::Channel, |s| s.tunread = ids(100, "t"));
    assert_eq!(rc_threads_button(&many), Some((100, Thread)));
    let group = s_with(RoomType::Channel, |s| {
        s.tunread = ids(2, "t");
        s.tunread_group = ids(1, "t");
    });
    assert_eq!(rc_threads_button(&group), Some((2, GroupMention)));
    // Mention > GroupMention > Thread (the header's own order).
    let both = s_with(RoomType::Channel, |s| {
        s.tunread = ids(2, "t");
        s.tunread_group = ids(1, "t");
        s.tunread_user = ids(1, "t");
        s.disable_notifications = Some(true);
        s.hide_mention_status = Some(true);
    });
    assert_eq!(rc_threads_button(&both), Some((2, Mention)));
}

// --- the workspace number (G0) ----------------------------------------------

fn sig(f: impl FnOnce(&mut Subscription), pref: bool) -> RcSignal {
    rc_entry_signal(&entry(s_with(RoomType::Channel, f)), pref)
}

#[test]
fn workspace_number_sums_unread() {
    assert_eq!(sig(|_| {}, true), RcSignal::default());
    assert_eq!(
        sig(
            |s| {
                s.alert = true;
                s.unread = 3;
            },
            true
        ),
        RcSignal {
            unread: 3,
            bullet: true
        }
    );
    // Threads never add; their alert asks for the bullet.
    let t = sig(
        |s| {
            s.alert = true;
            s.tunread = ids(4, "t");
        },
        true,
    );
    assert_eq!(
        t,
        RcSignal {
            unread: 0,
            bullet: true
        }
    );
    assert!(t.shows_bullet() && t.any());
    // A follower without alert: nothing.
    assert_eq!(sig(|s| s.tunread = ids(1, "t"), true), RcSignal::default());
    // Muted rooms count.
    assert_eq!(
        sig(
            |s| {
                s.unread = 2;
                s.disable_notifications = Some(true);
            },
            true
        )
        .unread,
        2
    );
}

#[test]
fn workspace_number_skips_hidden_closed_archived_and_service_rooms() {
    let unread = |s: &mut Subscription| {
        s.alert = true;
        s.unread = 2;
    };
    assert_eq!(
        sig(
            |s| {
                unread(s);
                s.hide_unread_status = Some(true);
            },
            true
        ),
        RcSignal::default()
    );
    assert_eq!(
        sig(
            |s| {
                unread(s);
                s.open = false;
            },
            true
        ),
        RcSignal::default()
    );
    assert_eq!(
        sig(
            |s| {
                unread(s);
                s.archived = Some(true);
            },
            true
        ),
        RcSignal::default()
    );
    let mut svc = sub("SVC", "rustket-sync-me", RoomType::Private);
    svc.u.id = "me".into();
    svc.roles = Some(vec!["owner".into()]);
    svc.custom_fields = Some(rocketchat::models::ServiceFields::new("me").to_custom_fields());
    let svc_room = crate::test_util::service_room(&svc, "me", 1);
    unread(&mut svc);
    let e = RoomEntry {
        room: Some(svc_room),
        sub: Some(svc),
    };
    assert!(e.is_service_room());
    assert_eq!(rc_entry_signal(&e, true), RcSignal::default());
}

#[test]
fn bullet_follows_unread_alert() {
    fn alert(v: Option<&'static str>) -> impl FnOnce(&mut Subscription) {
        move |s: &mut Subscription| {
            s.alert = true;
            s.unread_alert = v.map(str::to_owned);
        }
    }
    // Default / mentions / absent follow the user preference.
    for v in [None, Some("default"), Some("mentions")] {
        assert!(sig(alert(v), true).bullet, "{v:?}");
        assert!(!sig(alert(v), false).bullet, "{v:?}");
    }
    assert!(!sig(alert(Some("nothing")), true).bullet);
    assert!(!sig(alert(Some("nothing")), false).bullet);
    assert!(sig(alert(Some("all")), true).bullet);
    assert!(sig(alert(Some("all")), false).bullet);
    // `unread` without `alert`: counted, no bullet.
    let s = sig(|s| s.unread = 1, true);
    assert_eq!(
        s,
        RcSignal {
            unread: 1,
            bullet: false
        }
    );
    // A number wins over the bullet.
    let s = sig(
        |s| {
            s.alert = true;
            s.unread = 1;
        },
        true,
    );
    assert!(!s.shows_bullet() && s.any());
}

#[test]
fn signals_merge_by_sum_and_or() {
    let a = RcSignal {
        unread: 2,
        bullet: false,
    };
    let b = RcSignal {
        unread: 0,
        bullet: true,
    };
    assert_eq!(
        a.merge(b),
        RcSignal {
            unread: 2,
            bullet: true
        }
    );
    assert_eq!([a, b, a].into_iter().sum::<RcSignal>().unread, 4);
    assert_eq!(
        std::iter::empty::<RcSignal>().sum::<RcSignal>(),
        RcSignal::default()
    );
    assert!(!RcSignal::default().any());
}

fn ws_with_prefs(prefs: serde_json::Value) -> Workspace {
    let mut u = me("me", "me");
    u.settings = serde_json::from_value(serde_json::json!({ "preferences": prefs })).unwrap();
    let mut ws = Workspace::new();
    ws.apply_me(u);
    ws
}

#[test]
fn account_preferences() {
    let ws = Workspace::new();
    assert!(ws.unread_alert_pref());
    assert!(!ws.mentions_with_symbol());
    let ws = ws_with_prefs(serde_json::json!({}));
    assert!(ws.unread_alert_pref());
    assert!(!ws.mentions_with_symbol());
    let ws = ws_with_prefs(serde_json::json!({ "unreadAlert": false, "mentionsWithSymbol": true }));
    assert!(!ws.unread_alert_pref());
    assert!(ws.mentions_with_symbol());
    let ws = ws_with_prefs(serde_json::json!({ "unreadAlert": "no", "mentionsWithSymbol": 1 }));
    assert!(ws.unread_alert_pref());
    assert!(!ws.mentions_with_symbol());
}

#[test]
fn workspace_signal_reads_the_account_preference() {
    let mut ws = ws_with_prefs(serde_json::json!({ "unreadAlert": false }));
    ws.apply_subscription(
        Change::Inserted,
        s_with(RoomType::Channel, |s| s.alert = true),
    );
    assert_eq!(rc_workspace_signal(&ws), RcSignal::default());
    let mut ws = ws_with_prefs(serde_json::json!({}));
    ws.apply_subscription(
        Change::Inserted,
        s_with(RoomType::Channel, |s| s.alert = true),
    );
    assert!(rc_workspace_signal(&ws).shows_bullet());
}

// --- ranking -----------------------------------------------------------------

#[test]
fn rank_rustket_is_the_derived_order() {
    let all = [Muted, Thread, Unread, GroupMention, Mention];
    for a in all {
        for b in all {
            assert_eq!(
                a.rank(IndicatorScheme::Rustket)
                    .cmp(&b.rank(IndicatorScheme::Rustket)),
                a.cmp(&b),
                "{a:?} {b:?}"
            );
        }
    }
    assert_eq!(
        strongest_in([Thread, Unread], IndicatorScheme::Rustket),
        Some(Unread)
    );
    assert_eq!(
        strongest_in([Thread, GroupMention], IndicatorScheme::Rustket),
        Some(GroupMention)
    );
}

#[test]
fn rank_rocketchat_puts_threads_above_group_mentions() {
    let rc = IndicatorScheme::RocketChat;
    let order: Vec<u8> = [Muted, Unread, GroupMention, Thread, Mention]
        .iter()
        .map(|k| k.rank(rc))
        .collect();
    assert_eq!(order, [0, 1, 2, 3, 4]);
    assert_eq!(strongest_in([Thread, GroupMention], rc), Some(Thread));
    assert_eq!(strongest_in([Thread, Unread], rc), Some(Thread));
    assert_eq!(strongest_in([Thread, Mention, Unread], rc), Some(Mention));
    assert_eq!(strongest_in([], rc), None);
}

#[test]
fn variant_order() {
    assert_eq!(rc_variant(0, 0, 0), Unread);
    assert_eq!(rc_variant(0, 0, 1), GroupMention);
    assert_eq!(rc_variant(0, 1, 1), Thread);
    assert_eq!(rc_variant(1, 1, 1), Mention);
}

#[test]
fn scheme_serde_values() {
    #[derive(Serialize, Deserialize)]
    struct W {
        s: IndicatorScheme,
    }
    let text = toml::to_string(&W {
        s: IndicatorScheme::RocketChat,
    })
    .unwrap();
    assert_eq!(text.trim(), "s = \"rocketchat\"");
    let w: W = toml::from_str("s = \"rustket\"").unwrap();
    assert_eq!(w.s, IndicatorScheme::Rustket);
}

// --- room list rows ----------------------------------------------------------

fn list_ws() -> Workspace {
    let mut ws = Workspace::new();
    ws.apply_me(me("me", "me"));
    let rows: [(&str, Fill); 3] = [
        ("a-read", |_| {}),
        ("b-alert", |s| s.alert = true),
        ("c-follower", |s| s.tunread = ids(1, "t")),
    ];
    for (name, fill) in rows {
        let mut s = sub(name, name, RoomType::Channel);
        fill(&mut s);
        ws.apply_subscription(Change::Inserted, s);
        ws.apply_room(Change::Inserted, room(name, name, RoomType::Channel));
    }
    ws
}

/// `list_ws` plus the one row the schemes mark differently: hidden unread
/// status and muted group mentions with an @all. Rustket: no count (the
/// mention is muted), no dot (hidden). Rocket.Chat: a warning badge.
fn diverging_ws() -> Workspace {
    let mut ws = list_ws();
    let mut s = sub("d-muted-all", "d-muted-all", RoomType::Channel);
    s.hide_unread_status = Some(true);
    s.mute_group_mentions = Some(true);
    s.unread = 1;
    s.group_mentions = 1;
    ws.apply_subscription(Change::Inserted, s);
    ws.apply_room(
        Change::Inserted,
        room("d-muted-all", "d-muted-all", RoomType::Channel),
    );
    ws
}

#[test]
fn rows_carry_the_rc_badge_only_under_rocketchat() {
    let ws = list_ws();
    let e = ws.room("c-follower").unwrap();
    let rustket = crate::room_list::item(&ws, e, &RoomListOptions::default());
    assert_eq!(rustket.rc, RcBadge::default());
    let opts = RoomListOptions {
        scheme: IndicatorScheme::RocketChat,
        ..RoomListOptions::default()
    };
    let rc = crate::room_list::item(&ws, e, &opts);
    assert_eq!(rc.rc, rc_badge(e));
    assert_eq!(rc.badge, rustket.badge);
    assert_eq!((rc.rc.count, rc.rc.kind()), (1, Some(Thread)));
}

#[test]
fn marked_per_scheme() {
    let ws = list_ws();
    let opts = RoomListOptions {
        scheme: IndicatorScheme::RocketChat,
        ..RoomListOptions::default()
    };
    let item = |rid: &str| crate::room_list::item(&ws, ws.room(rid).unwrap(), &opts);
    for (rid, marked) in [("a-read", false), ("b-alert", true), ("c-follower", true)] {
        let i = item(rid);
        assert_eq!(i.marked(IndicatorScheme::RocketChat), marked, "{rid}");
        assert_eq!(
            i.marked(IndicatorScheme::Rustket),
            i.badge.count > 0 || i.badge.dot,
            "{rid}"
        );
        assert_eq!(in_unread_block(&i, &opts), marked, "{rid}");
    }
    // An alert with hidden unread status: dim, unmarked.
    let mut ws = list_ws();
    let mut s = ws.subscription("b-alert").unwrap().clone();
    s.hide_unread_status = Some(true);
    ws.apply_subscription(Change::Updated, s);
    let i = crate::room_list::item(&ws, ws.room("b-alert").unwrap(), &opts);
    assert!(!i.marked(IndicatorScheme::RocketChat));
}

#[test]
fn muted_all_mention_with_hidden_status_is_marked_only_under_rocketchat() {
    let ws = diverging_ws();
    let e = ws.room("d-muted-all").unwrap();
    let rc_opts = RoomListOptions {
        scheme: IndicatorScheme::RocketChat,
        ..RoomListOptions::default()
    };
    let rustket_opts = RoomListOptions::default();
    let rc = crate::room_list::item(&ws, e, &rc_opts);
    let rustket = crate::room_list::item(&ws, e, &rustket_opts);
    assert!(!rustket.marked(IndicatorScheme::Rustket));
    assert!(rc.marked(IndicatorScheme::RocketChat));
    assert!(!in_unread_block(&rustket, &rustket_opts));
    assert!(in_unread_block(&rc, &rc_opts));
}

#[test]
fn unread_first_blocks_follow_the_scheme() {
    let ws = diverging_ws();
    let layout = |p: &mut crate::config::UiPrefs| {
        p.sidebar_section_prefs.insert(
            "channels".into(),
            crate::config::SectionPrefs {
                sort: Some(crate::config::SidebarSort::Alphabetical),
                unread_first: Some(true),
            },
        );
    };
    let names = |scheme: IndicatorScheme| {
        let mut p = crate::config::UiPrefs {
            indicator_scheme: scheme,
            ..Default::default()
        };
        layout(&mut p);
        let opts = RoomListOptions::from_prefs(&p, &ws);
        assert_eq!(opts.scheme, scheme);
        let s = compute(&ws, &opts, "");
        let ch = s
            .iter()
            .find(|s| s.kind() == Some(crate::room_list::SectionKind::Channels))
            .unwrap();
        ch.items
            .iter()
            .map(|i| i.display_name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(IndicatorScheme::RocketChat),
        ["b-alert", "c-follower", "d-muted-all", "a-read"]
    );
    assert_eq!(
        names(IndicatorScheme::Rustket),
        ["b-alert", "c-follower", "a-read", "d-muted-all"]
    );
}
