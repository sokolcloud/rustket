#![allow(clippy::unwrap_used)]
use std::collections::HashSet;

use rustket_core::config::IndicatorScheme;
use rustket_core::quiet::Quiet;
use rustket_core::room_list::SectionKind;

use super::*;
use crate::help::server::UnreadCount;

fn cfg() -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.ui.indicator_scheme = IndicatorScheme::Rustket;
    cfg
}

fn env() -> HelpEnv {
    HelpEnv::default()
}

fn acc(env: &mut HelpEnv) -> &mut AccountEnv {
    env.account.as_mut().unwrap()
}

/// (kind, count, bold) of the room row.
fn row(o: &Outcome) -> (Option<BadgeKind>, u32, bool) {
    let r = o.row.as_ref().unwrap();
    (r.kind, r.count, r.bold)
}

fn section(kind: SectionKind) -> Placement {
    Placement::Section {
        id: SectionId::Builtin(kind),
        unread_first: false,
        first: false,
    }
}

const SHOWN: Toast = Toast::Shown {
    preview: true,
    sticky: false,
    unfocused_only: false,
};

#[test]
fn codes_are_unique_and_keyed() {
    let codes: HashSet<_> = Code::ALL.iter().map(|c| c.code()).collect();
    assert_eq!(codes.len(), Code::ALL.len());
    assert_eq!(Code::ALL.len(), 23);
    let keys: HashSet<_> = Code::ALL
        .iter()
        .flat_map(|c| [c.title_key(), c.trigger_key()])
        .collect();
    assert_eq!(keys.len(), 2 * Code::ALL.len());
    for c in Code::ALL {
        let prefix = match c.family() {
            Family::Messages => "M",
            Family::Threads => "T",
            Family::Later => {
                if *c == Code::D1 {
                    "D"
                } else {
                    "L"
                }
            }
            Family::Calls => {
                if *c == Code::O1 {
                    "O"
                } else {
                    "C"
                }
            }
            Family::Room => "R",
            Family::Global => "Q",
        };
        assert!(c.code().starts_with(prefix), "{c:?}");
        assert!(
            c.title_key()
                .starts_with(&format!("help.code.{}.", c.code().to_lowercase()))
        );
        assert_eq!(
            c.scenario().is_some(),
            matches!(c.family(), Family::Messages | Family::Threads)
        );
    }
    let total: usize = Family::ALL.iter().map(|f| f.codes().count()).sum();
    assert_eq!(total, Code::ALL.len());
}

/// Each M code under the defaults (dev-server facts, `AppConfig::default`).
#[test]
fn messages_under_the_defaults() {
    let (cfg, env) = (cfg(), env());
    let o = |c| evaluate(c, &cfg, &env);

    let m1 = o(Code::M1);
    assert_eq!(m1.note, None);
    assert_eq!(row(&m1), (Some(BadgeKind::Mention), 1, true));
    assert_eq!(m1.section, Some(section(SectionKind::Channels)));
    assert_eq!(m1.collapsed, Some(BadgeKind::Mention));
    let p = m1.pinned.clone().unwrap();
    assert!(p.in_all_unreads);
    assert_eq!(p.all_unreads, Some((1, BadgeKind::Unread)));
    assert_eq!(p.activity, Some((1, BadgeKind::Mention)));
    let icon = m1.account_icon.unwrap();
    assert_eq!(icon.place, IconPlace::TitleBar);
    assert_eq!(icon.badge, Some((1, BadgeKind::Mention)));
    let w = m1.window.unwrap();
    assert_eq!(
        w,
        WindowOutcome {
            signal: WorkspaceSignal {
                mentions: 1,
                strongest: Some(BadgeVariant::Mention),
                rooms: 1
            },
            tray: Some(TrayLevel::Mention)
        }
    );
    assert_eq!(crate::app::window_title(w.signal), "Rustket (1)");
    assert_eq!(m1.toast, Some(SHOWN));
    assert_eq!(
        m1.sound,
        Some(Sound::Plays(SoundKind::Message("chime".into())))
    );
    assert_eq!(
        m1.feed,
        Some(Feed {
            highlighted: true,
            chip_mine: Some(true),
            word_marked: false,
            divider: true,
            thread_dot: false
        })
    );
    assert!(m1.thread_row.is_none());

    // M2: a DM is loud (mention colour) everywhere but Activity, which
    // lists no DMs.
    let m2 = o(Code::M2);
    assert_eq!(row(&m2), (Some(BadgeKind::Mention), 1, true));
    assert_eq!(m2.section, Some(section(SectionKind::DirectMessages)));
    assert_eq!(m2.collapsed, Some(BadgeKind::Mention));
    let p = m2.pinned.clone().unwrap();
    assert_eq!(p.all_unreads, Some((1, BadgeKind::Unread)));
    assert_eq!(p.activity, None);
    assert_eq!(
        m2.account_icon.unwrap().badge,
        Some((1, BadgeKind::Mention))
    );
    assert_eq!(m2.window.unwrap().signal.mentions, 1);
    assert_eq!(m2.window.unwrap().tray, Some(TrayLevel::Mention));
    assert_eq!(m2.toast, Some(SHOWN));
    assert!(!m2.feed.unwrap().highlighted);
    assert_eq!(m2.feed.unwrap().chip_mine, None);

    let m3 = o(Code::M3);
    assert_eq!(row(&m3), (Some(BadgeKind::GroupMention), 1, true));
    assert_eq!(
        m3.pinned.clone().unwrap().activity,
        Some((1, BadgeKind::GroupMention))
    );
    // @all is no mention for the account icon: an orange dot, no count.
    assert_eq!(
        m3.account_icon.unwrap().badge,
        Some((0, BadgeKind::GroupMention))
    );
    assert_eq!(m3.window.unwrap().signal.mentions, 0);
    assert_eq!(m3.window.unwrap().tray, Some(TrayLevel::GroupMention));
    assert_eq!(
        crate::app::window_title(m3.window.unwrap().signal),
        "Rustket •"
    );
    assert_eq!(m3.toast, Some(SHOWN));
    assert!(m3.feed.unwrap().highlighted);
    assert_eq!(m3.feed.unwrap().chip_mine, Some(true));

    // M4: counts like a mention; the feed highlights the row and marks
    // the word.
    let m4 = o(Code::M4);
    assert_eq!(m4.note, Some(Note::NoHighlightWords));
    assert_eq!(row(&m4), (Some(BadgeKind::Mention), 1, true));
    assert_eq!(m4.toast, Some(SHOWN));
    assert_eq!(
        m4.feed,
        Some(Feed {
            highlighted: true,
            chip_mine: None,
            word_marked: true,
            divider: true,
            thread_dot: false
        })
    );

    // M5: an unread dot, bold (alert), counted as unread.
    let m5 = o(Code::M5);
    assert_eq!(row(&m5), (Some(BadgeKind::Unread), 0, true));
    assert_eq!(m5.collapsed, Some(BadgeKind::Unread));
    let p = m5.pinned.clone().unwrap();
    assert!(p.in_all_unreads);
    assert_eq!(p.activity, None);
    assert_eq!(m5.account_icon.unwrap().badge, Some((0, BadgeKind::Unread)));
    let w = m5.window.unwrap();
    assert_eq!((w.signal.rooms, w.signal.mentions), (1, 0));
    assert_eq!(w.tray, Some(TrayLevel::Unread));
    assert_eq!(crate::app::window_title(w.signal), "Rustket •");
    assert_eq!(m5.toast, Some(SHOWN));
    assert_eq!(
        m5.sound,
        Some(Sound::Plays(SoundKind::Message("chime".into())))
    );
    assert!(!m5.feed.unwrap().highlighted);
    assert!(m5.feed.unwrap().divider);
}

#[test]
fn threads_under_the_defaults() {
    let (cfg, env) = (cfg(), env());
    let o = |c| evaluate(c, &cfg, &env);

    let t1 = o(Code::T1);
    assert_eq!(row(&t1), (Some(BadgeKind::Thread), 1, true));
    let t = t1.thread_row.clone().unwrap();
    assert_eq!(t.unread, Some(BadgeVariant::Thread));
    assert_eq!(t.kind, Some(BadgeKind::Thread));
    // The Threads section is off by default.
    assert_eq!(t.listing, ThreadListing::SectionOff);
    assert!(t.starred_in_favorites);
    assert_eq!(t1.collapsed_threads, None);
    let p = t1.pinned.clone().unwrap();
    assert!(!p.in_all_unreads);
    assert_eq!(p.activity, Some((0, BadgeKind::Thread)));
    assert_eq!(t1.account_icon.unwrap().badge, Some((0, BadgeKind::Thread)));
    assert_eq!(t1.window.unwrap().tray, Some(TrayLevel::Unread));
    assert_eq!(t1.toast, Some(SHOWN));
    assert_eq!(
        t1.feed,
        Some(Feed {
            highlighted: false,
            chip_mine: None,
            word_marked: false,
            divider: false,
            thread_dot: true
        })
    );
    // With the section off the room header's Threads button shows it.
    assert_eq!(t.header_button, Some(BadgeKind::Thread));

    // A thread mention: tunread + tunreadUser.
    let t2 = o(Code::T2);
    assert_eq!(row(&t2), (Some(BadgeKind::Mention), 1, true));
    assert_eq!(
        t2.thread_row.as_ref().unwrap().kind,
        Some(BadgeKind::Mention)
    );
    assert_eq!(
        t2.pinned.clone().unwrap().activity,
        Some((1, BadgeKind::Mention))
    );
    assert_eq!(
        t2.account_icon.unwrap().badge,
        Some((1, BadgeKind::Mention))
    );
    assert_eq!(t2.toast, Some(SHOWN));
    assert!(t2.feed.unwrap().highlighted);
    assert_eq!(
        t2.thread_row.as_ref().unwrap().header_button,
        Some(BadgeKind::Mention)
    );

    // A thread @all: tunread + tunreadGroup only; the room row and the
    // thread row both show the @all colour.
    let t3 = o(Code::T3);
    let sub = &t3.server.as_ref().unwrap().sub;
    assert_eq!(sub.tunread_user, None);
    assert_eq!(sub.tunread_group, Some(vec![TMID.to_owned()]));
    assert_eq!(row(&t3), (Some(BadgeKind::GroupMention), 1, true));
    assert_eq!(
        t3.thread_row.as_ref().unwrap().unread,
        Some(BadgeVariant::GroupMention)
    );
    assert_eq!(
        t3.thread_row.as_ref().unwrap().kind,
        Some(BadgeKind::GroupMention)
    );
    assert_eq!(
        t3.pinned.clone().unwrap().activity,
        Some((1, BadgeKind::GroupMention))
    );
    assert_eq!(
        t3.account_icon.unwrap().badge,
        Some((0, BadgeKind::GroupMention))
    );
    assert_eq!(t3.window.unwrap().tray, Some(TrayLevel::GroupMention));
    assert_eq!(t3.toast, Some(SHOWN));
    assert!(t3.feed.unwrap().highlighted);
    assert_eq!(
        t3.thread_row.as_ref().unwrap().header_button,
        Some(BadgeKind::GroupMention)
    );
}

#[test]
fn later_calls_and_omnichannel_under_the_defaults() {
    let (cfg, env) = (cfg(), env());
    let l1 = evaluate(Code::L1, &cfg, &env);
    let sticky = Toast::Shown {
        preview: true,
        sticky: true,
        unfocused_only: false,
    };
    assert_eq!(l1.toast, Some(sticky.clone()));
    assert_eq!(
        l1.sound,
        Some(Sound::Plays(SoundKind::Reminder("chime".into())))
    );
    assert_eq!(l1.pinned.unwrap().later, Some((1, BadgeKind::Mention)));
    let l2 = evaluate(Code::L2, &cfg, &env);
    assert!(l2.once_per_run);
    assert_eq!(l2.toast, Some(sticky.clone()));
    let d1 = evaluate(Code::D1, &cfg, &env);
    assert!(d1.dialog);
    let p = d1.pinned.unwrap();
    assert_eq!(p.drafts, Some((1, BadgeKind::Mention)));
    assert_eq!(p.drafts_on_time, Some((1, BadgeKind::Muted)));
    let c1 = evaluate(Code::C1, &cfg, &env);
    assert!(c1.popup);
    assert_eq!(c1.sound, Some(Sound::Plays(SoundKind::Ring)));
    // Only while the window is not focused.
    assert_eq!(
        c1.toast,
        Some(Toast::Shown {
            preview: true,
            sticky: true,
            unfocused_only: true
        })
    );
    let o1 = evaluate(Code::O1, &cfg, &env);
    assert_eq!(
        o1.sound,
        Some(Sound::Plays(SoundKind::NewChat("door".into())))
    );
    assert_eq!(o1.toast, None);
    for c in [Code::R1, Code::Q1] {
        assert_eq!(evaluate(c, &cfg, &env), Outcome::empty(c));
    }
}

#[test]
fn local_switches_change_toast_and_sound() {
    let env = env();
    let mut cfg = cfg();
    cfg.ui.desktop_notifications = false;
    let m1 = evaluate(Code::M1, &cfg, &env);
    assert_eq!(m1.toast, Some(Toast::Blocked(vec![ToastBlocked::Disabled])));
    // The sound plays without the notification.
    assert_eq!(
        m1.sound,
        Some(Sound::Plays(SoundKind::Message("chime".into())))
    );
    cfg.ui.sound = false;
    let m1 = evaluate(Code::M1, &cfg, &env);
    assert_eq!(m1.toast, Some(Toast::Blocked(vec![ToastBlocked::Disabled])));
    assert_eq!(m1.sound, Some(Sound::Silent(SoundOff::Setting)));
    cfg.ui.sound = true;
    // Reminders ignore the switch; calls do not.
    assert!(evaluate(Code::L1, &cfg, &env).toast.unwrap().shown());
    assert_eq!(
        evaluate(Code::C1, &cfg, &env).toast,
        Some(Toast::Blocked(vec![ToastBlocked::Disabled]))
    );

    let mut cfg = self::cfg();
    cfg.ui.sound = false;
    let m1 = evaluate(Code::M1, &cfg, &env);
    assert_eq!(m1.toast, Some(SHOWN));
    assert_eq!(m1.sound, Some(Sound::Silent(SoundOff::Setting)));
    assert_eq!(
        evaluate(Code::C1, &cfg, &env).sound,
        Some(Sound::Silent(SoundOff::Setting))
    );
    assert_eq!(
        evaluate(Code::O1, &cfg, &env).sound,
        Some(Sound::Silent(SoundOff::Setting))
    );
    // "Play a sound" off silences reminders too; with it on, the reminder
    // sound has its own switch.
    assert_eq!(
        evaluate(Code::L1, &cfg, &env).sound,
        Some(Sound::Silent(SoundOff::Setting))
    );
    cfg.ui.sound = true;
    assert_eq!(
        evaluate(Code::L1, &cfg, &env).sound,
        Some(Sound::Plays(SoundKind::Reminder("chime".into())))
    );
    cfg.ui.reminder_sound = false;
    assert_eq!(
        evaluate(Code::L1, &cfg, &env).sound,
        Some(Sound::Silent(SoundOff::ReminderSoundOff))
    );

    let mut cfg = self::cfg();
    cfg.ui.notification_preview = false;
    assert_eq!(
        evaluate(Code::M2, &cfg, &env).toast,
        Some(Toast::Shown {
            preview: false,
            sticky: false,
            unfocused_only: false
        })
    );
    cfg.ui.reminder_notifications = false;
    let l1 = evaluate(Code::L1, &cfg, &env);
    assert_eq!(
        l1.toast,
        Some(Toast::Blocked(vec![ToastBlocked::RemindersOff]))
    );
    assert_eq!(l1.sound, Some(Sound::Silent(SoundOff::NoToast)));
}

#[test]
fn pauses_and_busy() {
    let until = 1_795_620_000_000;
    // Q1: paused by hand.
    let mut cfg = cfg();
    cfg.pause.paused_until = Some(until);
    let mut env = env();
    env.quiet = Quiet {
        paused: true,
        until: Some(until),
        off_hours: false,
    };
    let paused = ToastBlocked::Paused {
        until: Some(until),
        cause: Code::Q1,
    };
    assert_eq!(paused.code(), Some(Code::Q1));
    let m1 = evaluate(Code::M1, &cfg, &env);
    assert_eq!(m1.toast, Some(Toast::Blocked(vec![paused])));
    assert_eq!(m1.sound, Some(Sound::Silent(SoundOff::NotNotified)));
    // Badges do not change.
    assert_eq!(row(&m1), (Some(BadgeKind::Mention), 1, true));
    let l1 = evaluate(Code::L1, &cfg, &env);
    assert!(l1.toast.unwrap().shown());
    let quiet = SoundOff::Paused {
        until: Some(until),
        cause: Code::Q1,
    };
    assert_eq!(l1.sound, Some(Sound::Silent(quiet)));
    let c1 = evaluate(Code::C1, &cfg, &env);
    assert!(c1.popup);
    assert_eq!(c1.sound, Some(Sound::Silent(quiet)));
    assert_eq!(
        evaluate(Code::O1, &cfg, &env).sound,
        Some(Sound::Silent(quiet))
    );
    assert_eq!(
        global(Code::Q1, &cfg, &env),
        Some(Global::Pause { until: Some(until) })
    );

    // Q2: outside working hours.
    let mut cfg = self::cfg();
    cfg.ui.working_hours.enabled = true;
    env.quiet.off_hours = true;
    env.working_now = false;
    env.off_hours_until = Some(until);
    assert_eq!(
        evaluate(Code::M5, &cfg, &env).toast,
        Some(Toast::Blocked(vec![ToastBlocked::Paused {
            until: Some(until),
            cause: Code::Q2
        }]))
    );
    assert_eq!(
        global(Code::Q2, &cfg, &env),
        Some(Global::WorkingHours(WorkingHoursState::Outside {
            until: Some(until)
        }))
    );
    assert_eq!(
        global(Code::Q1, &cfg, &env),
        Some(Global::Pause { until: None })
    );

    // Q3: Busy; the server already sends nothing.
    let cfg = self::cfg();
    let mut env = self::env();
    acc(&mut env).status = UserStatus::Busy;
    let m2 = evaluate(Code::M2, &cfg, &env);
    let busy = ToastBlocked::Server(ServerBlock::Busy);
    assert_eq!(busy.code(), Some(Code::Q3));
    assert_eq!(m2.toast, Some(Toast::Blocked(vec![busy])));
    // Busy acts like a pause for reminders: shown, without the alarm.
    let l1 = evaluate(Code::L1, &cfg, &env);
    assert!(l1.toast.unwrap().shown());
    assert_eq!(l1.sound, Some(Sound::Silent(SoundOff::Busy)));
    assert_eq!(
        evaluate(Code::C1, &cfg, &env).sound,
        Some(Sound::Silent(SoundOff::Busy))
    );
    assert_eq!(
        global(Code::Q3, &cfg, &env),
        Some(Global::Status(Some(UserStatus::Busy)))
    );
}

#[test]
fn account_preferences() {
    let cfg = cfg();
    let mut env = env();
    acc(&mut env).prefs.sound.new_message = Some("none".into());
    let m1 = evaluate(Code::M1, &cfg, &env);
    assert_eq!(m1.toast, Some(SHOWN));
    assert_eq!(m1.sound, Some(Sound::Silent(SoundOff::Account)));
    // Reminders use the account's message sound too.
    assert_eq!(
        evaluate(Code::L1, &cfg, &env).sound,
        Some(Sound::Silent(SoundOff::Account))
    );
    assert_eq!(
        evaluate(Code::C1, &cfg, &env).sound,
        Some(Sound::Silent(SoundOff::Account))
    );
    // The new-chat sound has its own preference.
    assert!(matches!(
        evaluate(Code::O1, &cfg, &env).sound,
        Some(Sound::Plays(_))
    ));
    let mut env = self::env();
    acc(&mut env).prefs.sound.new_room = Some("none".into());
    assert_eq!(
        evaluate(Code::O1, &cfg, &env).sound,
        Some(Sound::Silent(SoundOff::AccountNewRoom))
    );
    // The sound lines name the account's sounds.
    let mut env = self::env();
    acc(&mut env).prefs.sound.new_message = Some("ding".into());
    acc(&mut env).prefs.sound.new_room = Some("seasons".into());
    assert_eq!(
        evaluate(Code::M1, &cfg, &env).sound,
        Some(Sound::Plays(SoundKind::Message("ding".into())))
    );
    assert_eq!(
        evaluate(Code::L1, &cfg, &env).sound,
        Some(Sound::Plays(SoundKind::Reminder("ding".into())))
    );
    assert_eq!(
        evaluate(Code::O1, &cfg, &env).sound,
        Some(Sound::Plays(SoundKind::NewChat("seasons".into())))
    );
    acc(&mut env).prefs.sound.master_volume = Some(0.0);
    assert_eq!(
        evaluate(Code::L1, &cfg, &env).sound,
        Some(Sound::Silent(SoundOff::Volume))
    );

    // An account preference the user set is stored on the subscriptions.
    let mut env = self::env();
    acc(&mut env).desktop_stored = true;
    acc(&mut env).prefs.desktop = Some(DesktopPref::Mentions);
    let m5 = evaluate(Code::M5, &cfg, &env);
    let filtered = ToastBlocked::Server(ServerBlock::Preference {
        pref: DesktopPref::Mentions,
        room: false,
    });
    assert_eq!(filtered.code(), None);
    assert_eq!(m5.toast, Some(Toast::Blocked(vec![filtered])));
    for c in [Code::M1, Code::M2, Code::M3, Code::M4, Code::T1] {
        assert_eq!(evaluate(c, &cfg, &env).toast, Some(SHOWN), "{c:?}");
    }
    acc(&mut env).prefs.desktop = Some(DesktopPref::Nothing);
    for c in [Code::M1, Code::M2, Code::T2] {
        assert!(!evaluate(c, &cfg, &env).toast.unwrap().shown(), "{c:?}");
    }
    acc(&mut env).prefs.require_interaction = true;
    acc(&mut env).prefs.desktop = None;
    assert_eq!(
        evaluate(Code::M1, &cfg, &env).toast,
        Some(Toast::Shown {
            preview: true,
            sticky: true,
            unfocused_only: false
        })
    );
}

#[test]
fn sidebar_preferences() {
    let env = env();
    let mut cfg = cfg();
    cfg.ui.unread_on_top = true;
    for c in [Code::M2, Code::M5, Code::T1] {
        assert_eq!(
            evaluate(c, &cfg, &env).section,
            Some(Placement::Unread),
            "{c:?}"
        );
    }
    let mut cfg = self::cfg();
    cfg.ui.sidebar_threads = ThreadsMode::Unread;
    let t1 = evaluate(Code::T1, &cfg, &env);
    assert_eq!(
        t1.thread_row.unwrap().listing,
        ThreadListing::Threads { limit: 20 }
    );
    assert_eq!(t1.collapsed_threads, Some(BadgeKind::Thread));
    cfg.ui.sidebar_pinned_views = false;
    assert_eq!(evaluate(Code::M1, &cfg, &env).pinned, None);
    assert_eq!(evaluate(Code::L1, &cfg, &env).pinned, None);
    // No Drafts & scheduled row: no on-time badge to compare with.
    let d1 = evaluate(Code::D1, &cfg, &env);
    assert!(d1.dialog);
    assert_eq!(d1.pinned, None);
    cfg.ui.density = Density::Condensed;
    let m1 = evaluate(Code::M1, &cfg, &env);
    assert_eq!(
        m1.row.unwrap().props("general", ()).density,
        rustket_ui::widget::Density::Condensed
    );
    // Unread first in the room's own section.
    let mut cfg = self::cfg();
    cfg.ui.sidebar_section_prefs.insert(
        "channels".into(),
        rustket_core::sidebar_layout::SectionPrefs {
            sort: None,
            unread_first: Some(true),
        },
    );
    assert_eq!(
        evaluate(Code::M5, &cfg, &env).section,
        Some(Placement::Section {
            id: SectionId::Builtin(SectionKind::Channels),
            unread_first: true,
            first: true
        })
    );
    let mut env = self::env();
    env.custom_chrome = false;
    env.tray = false;
    let m1 = evaluate(Code::M1, &self::cfg(), &env);
    assert_eq!(m1.account_icon.unwrap().place, IconPlace::Rail);
    assert_eq!(m1.window.unwrap().tray, None);
}

#[test]
fn server_counting_changes_the_rows() {
    let cfg = cfg();
    let mut env = env();
    acc(&mut env).server.unread_count = UnreadCount::AllMessages;
    let m5 = evaluate(Code::M5, &cfg, &env);
    assert_eq!(row(&m5), (Some(BadgeKind::Unread), 1, true));
    assert_eq!(m5.server.unwrap().sub.unread, 1);
    acc(&mut env).server.threads_enabled = false;
    let t1 = evaluate(Code::T1, &cfg, &env);
    assert_eq!(t1.note, Some(Note::ThreadsDisabled));
    assert_eq!(t1.thread_row.unwrap().listing, ThreadListing::ServerOff);
    acc(&mut env).prefs.highlights = vec!["rust".into()];
    assert_eq!(evaluate(Code::M4, &cfg, &env).note, None);
    let env = HelpEnv {
        account: None,
        ..self::env()
    };
    let m1 = evaluate(Code::M1, &cfg, &env);
    assert_eq!(m1.note, Some(Note::NoAccount));
    assert_eq!(
        m1.toast,
        Some(Toast::Blocked(vec![ToastBlocked::NoAccount]))
    );
    assert_eq!(row(&m1), (Some(BadgeKind::Mention), 1, true));
}

/// (kind, count) of every R-matrix cell, and the server's bell.
#[test]
fn room_settings_matrix() {
    let m = matrix(&cfg(), &env());
    assert_eq!(m.len(), MATRIX_CODES.len());
    let cells = |code: Code| -> Vec<(Option<BadgeKind>, u32, bool)> {
        m.iter()
            .find(|(c, _)| *c == code)
            .unwrap()
            .1
            .iter()
            .map(|c| (c.row.kind, c.row.count, c.server_toast))
            .collect()
    };
    use BadgeKind::*;
    // normal, R1 (notifications off), R2 (no counter), R3 (no mention
    // badges), R4 (mute @all).
    // R1 keeps a mention red; R3 drops red and orange (the next kind).
    assert_eq!(
        cells(Code::M1),
        vec![
            (Some(Mention), 1, true),
            (Some(Mention), 1, false),
            (Some(Mention), 1, true),
            (Some(Unread), 1, true),
            (Some(Mention), 1, true),
        ]
    );
    // A DM is red; R1 greys it, R3 makes it blue.
    assert_eq!(
        cells(Code::M2),
        vec![
            (Some(Mention), 1, true),
            (Some(Muted), 1, false),
            (None, 0, true),
            (Some(Unread), 1, true),
            (Some(Mention), 1, true),
        ]
    );
    assert_eq!(
        cells(Code::M3),
        vec![
            (Some(GroupMention), 1, true),
            (Some(Muted), 1, false),
            (Some(GroupMention), 1, true),
            (Some(Unread), 1, true),
            (Some(Unread), 1, false),
        ]
    );
    assert_eq!(
        cells(Code::M5),
        vec![
            (Some(Unread), 0, true),
            (Some(Muted), 0, false),
            (None, 0, true),
            (Some(Unread), 0, true),
            (Some(Unread), 0, true),
        ]
    );
    assert_eq!(
        cells(Code::T1),
        vec![
            (Some(Thread), 1, true),
            (Some(Muted), 1, false),
            (None, 0, true),
            (Some(Thread), 1, true),
            (Some(Thread), 1, true),
        ]
    );
    assert_eq!(
        cells(Code::T2),
        vec![
            (Some(Mention), 1, true),
            (Some(Mention), 1, false),
            (Some(Mention), 1, true),
            (Some(Thread), 1, true),
            (Some(Mention), 1, true),
        ]
    );
    // A thread @all: orange unless R1 (grey), R3 or R4 (the thread colour).
    assert_eq!(
        cells(Code::T3),
        vec![
            (Some(GroupMention), 1, true),
            (Some(Muted), 1, false),
            (None, 0, true),
            (Some(Thread), 1, true),
            (Some(Thread), 1, true),
        ]
    );
    let muted = m[0].1[1].row.clone();
    assert!(muted.muted && muted.props("r", ()).is_muted);

    let alerts = alert_table(&cfg(), &env());
    let get = |p: DesktopPref| alerts.iter().find(|(x, _)| *x == p).unwrap().1.clone();
    assert!(get(DesktopPref::All).iter().all(|(_, on)| *on));
    assert!(get(DesktopPref::Nothing).iter().all(|(_, on)| !on));
    let mentions: Vec<bool> = get(DesktopPref::Mentions)
        .iter()
        .map(|(_, on)| *on)
        .collect();
    // M1, M2 (a DM), M3 (@all), T1, T2 and T3 (followed threads) notify; M5 not.
    assert_eq!(mentions, vec![true, true, true, false, true, true, true]);
    let r5 = evaluate_with(
        Code::M5,
        RoomMods {
            alert: Some(DesktopPref::Nothing),
            ..RoomMods::default()
        },
        &cfg(),
        &env(),
    );
    match r5.toast {
        Some(Toast::Blocked(v)) => assert_eq!(v[0].code(), Some(Code::R5)),
        t => panic!("{t:?}"),
    }
}

#[test]
fn globals() {
    let cfg = cfg();
    let mut env = env();
    assert_eq!(
        global(Code::Q2, &cfg, &env),
        Some(Global::WorkingHours(WorkingHoursState::Off))
    );
    assert_eq!(global(Code::Q4, &cfg, &env), Some(Global::OnScreen));
    assert_eq!(
        global(Code::Q5, &cfg, &env),
        Some(Global::Connection(Some(Connection::Connected)))
    );
    assert_eq!(global(Code::M1, &cfg, &env), None);
    // Offline is Rustket's own cause, not the server's decision.
    let offline = Connection::Offline { cache: true };
    acc(&mut env).connection = offline;
    let m1 = evaluate(Code::M1, &cfg, &env);
    assert_eq!(m1.server.as_ref().unwrap().desktop, Ok(()));
    match m1.toast {
        Some(Toast::Blocked(v)) => {
            assert_eq!(v, vec![ToastBlocked::NotConnected(offline)]);
            assert_eq!(v[0].code(), Some(Code::Q5));
        }
        t => panic!("{t:?}"),
    }
    assert!(matrix(&cfg, &env)[0].1[0].server_toast);
    acc(&mut env).connection = Connection::Connecting;
    assert_eq!(
        evaluate(Code::M2, &cfg, &env).toast,
        Some(Toast::Blocked(vec![ToastBlocked::NotConnected(
            Connection::Connecting
        )]))
    );
    assert_eq!(
        Connection::of(&ConnectionState::Reconnecting { attempt: 2 }, true),
        Connection::Offline { cache: true }
    );
}

#[test]
fn colour_legend_lists_the_codes() {
    let k = kinds_in_use(&cfg(), &env());
    let codes = |kind: BadgeKind| k.iter().find(|(x, _)| *x == kind).map(|(_, v)| v.clone());
    let mention = codes(BadgeKind::Mention).unwrap();
    for c in [Code::M1, Code::M4, Code::T2, Code::L1, Code::D1] {
        assert!(mention.contains(&c), "{c:?}");
    }
    assert_eq!(
        codes(BadgeKind::GroupMention),
        Some(vec![Code::M3, Code::T3])
    );
    assert!(codes(BadgeKind::Unread).unwrap().contains(&Code::M5));
    assert!(!codes(BadgeKind::Thread).unwrap().contains(&Code::M5));
    // Thread rows are not drawn while the Threads section is off.
    assert!(!codes(BadgeKind::Muted).is_some_and(|c| c.contains(&Code::T1)));
    // The room header's Threads button shows unread threads in every mode.
    let off = legend(&cfg(), &env());
    let off_uses = |kind: BadgeKind| off.iter().find(|(x, _)| *x == kind).unwrap().1.clone();
    assert!(off_uses(BadgeKind::Thread).contains(&KindUse {
        code: Code::T1,
        surface: Surface::ThreadsButton,
        dot: true
    }));
    assert!(off_uses(BadgeKind::Mention).contains(&KindUse {
        code: Code::T2,
        surface: Surface::ThreadsButton,
        dot: true
    }));
    assert!(
        !off_uses(BadgeKind::Thread)
            .iter()
            .any(|u| u.surface == Surface::ThreadRow)
    );
    let mut cfg = cfg();
    cfg.ui.sidebar_threads = ThreadsMode::Unread;
    let l = legend(&cfg, &env());
    let uses = |kind: BadgeKind| l.iter().find(|(x, _)| *x == kind).unwrap().1.clone();
    assert!(uses(BadgeKind::Thread).contains(&KindUse {
        code: Code::T1,
        surface: Surface::ThreadRow,
        dot: true
    }));
    assert!(uses(BadgeKind::GroupMention).contains(&KindUse {
        code: Code::T3,
        surface: Surface::ThreadRow,
        dot: true
    }));
    // M5 shows the unread colour as a row dot and as the All unreads
    // counter.
    assert!(uses(BadgeKind::Unread).contains(&KindUse {
        code: Code::M5,
        surface: Surface::Row,
        dot: true
    }));
    assert!(uses(BadgeKind::Unread).contains(&KindUse {
        code: Code::M5,
        surface: Surface::AllUnreads,
        dot: false
    }));
    // Every use is a real surface of the outcome.
    for (kind, us) in &l {
        for u in us {
            let o = evaluate(u.code, &cfg, &env());
            let p = o.pinned.clone().unwrap_or_default();
            let got = match u.surface {
                Surface::Row => o
                    .row
                    .as_ref()
                    .and_then(|r| r.kind.map(|k| (k, r.count == 0))),
                Surface::ThreadRow => o
                    .thread_row
                    .as_ref()
                    .and_then(|t| t.kind.map(|k| (k, true))),
                Surface::ThreadsButton => o
                    .thread_row
                    .as_ref()
                    .and_then(|t| t.header_button.map(|k| (k, true))),
                Surface::AccountIcon => o
                    .account_icon
                    .and_then(|a| a.badge.map(|(n, k)| (k, n == 0))),
                Surface::AllUnreads => p.all_unreads.map(|(n, k)| (k, n == 0)),
                Surface::Activity => p.activity.map(|(n, k)| (k, n == 0)),
                Surface::Later => p.later.map(|(n, k)| (k, n == 0)),
                Surface::Drafts => p.drafts.map(|(n, k)| (k, n == 0)),
            };
            assert_eq!(got, Some((*kind, u.dot)), "{u:?}");
        }
    }
}

/// The per-room parts agree with the app's whole-workspace functions on the
/// scenario's workspace, and the row with the sidebar's own badge kind.
#[test]
fn per_room_parts_agree_with_the_app() {
    let (cfg, env) = (cfg(), env());
    let acc = env.account.clone().unwrap();
    for &code in &MATRIX_CODES {
        for m in MATRIX_MODS {
            let o = evaluate_with(code, m.map(RoomMods::of).unwrap_or_default(), &cfg, &env);
            let thread = code.family() == Family::Threads;
            let sub = o.server.as_ref().unwrap().sub.clone();
            let s = scene(sub, times(&env, thread).1, &acc, true);
            let signal = room_list::workspace_signal(&s.ws);
            assert_eq!(o.account_icon.unwrap().signal, signal, "{code:?} {m:?}");
            assert_eq!(o.account_icon.unwrap().badge, rail::badge(signal));
            assert_eq!(o.window.unwrap().signal, signal);
            let counts = rustket_core::inbox::counts(&s.ws);
            let p = o.pinned.clone().unwrap();
            assert_eq!(p.all_unreads, sidebar::unreads_badge(counts.unread_rooms));
            assert_eq!(p.activity, sidebar::activity_badge(&counts));
            let item = room_list::item(
                &s.ws,
                &s.entry,
                &RoomListOptions::from_prefs(&cfg.ui, &s.ws),
            );
            let r = o.row.as_ref().unwrap();
            assert_eq!(r.kind, sidebar::badge_kind(&item.badge));
            assert_eq!(item.badge, room_list::badge(&s.entry));
        }
    }
}

#[test]
fn feed_rules() {
    use crate::room::render::mention_is_mine;
    use crate::room::timeline::row::highlights_row;
    let msg = |uid: &str, mentions: &[&str]| RcMessage {
        u: UserRef {
            id: uid.into(),
            ..Default::default()
        },
        mentions: Some(
            mentions
                .iter()
                .map(|id| Mention {
                    id: (*id).into(),
                    ..Default::default()
                })
                .collect(),
        ),
        ..Default::default()
    };
    assert!(highlights_row(Some("me"), false, &[], &msg("bob", &["me"])));
    assert!(highlights_row(
        Some("me"),
        false,
        &[],
        &msg("bob", &["here"])
    ));
    assert!(!highlights_row(Some("me"), false, &[], &msg("me", &["me"])));
    assert!(!highlights_row(
        Some("me"),
        false,
        &[],
        &msg("bob", &["carol"])
    ));
    // R4: @all / @here no longer highlight; a direct mention still does.
    assert!(!highlights_row(
        Some("me"),
        true,
        &[],
        &msg("bob", &["all"])
    ));
    assert!(highlights_row(
        Some("me"),
        true,
        &[],
        &msg("bob", &["all", "me"])
    ));
    assert!(mention_is_mine("all", None));
    assert!(mention_is_mine("me", Some("me")));
    assert!(!mention_is_mine("carol", Some("me")));
}

#[test]
fn sound_gates_agree_with_the_app_paths() {
    use crate::desktop::notifications::MessageSoundOff;
    let on = AccountSound::default();
    assert_eq!(message_sound_off(true, None, &on), None);
    assert_eq!(
        message_sound_off(false, None, &on),
        Some(MessageSoundOff::Setting)
    );
    assert_eq!(
        message_sound_off(true, Some("none"), &on),
        Some(MessageSoundOff::Room)
    );
    let none = AccountSound {
        new_message: Some("none".into()),
        ..AccountSound::default()
    };
    assert_eq!(
        message_sound_off(true, None, &none),
        Some(MessageSoundOff::Account)
    );
    let quiet = AccountSound {
        master_volume: Some(0.0),
        ..AccountSound::default()
    };
    assert_eq!(
        message_sound_off(true, None, &quiet),
        Some(MessageSoundOff::Volume)
    );
    // The workspace-reading wrappers read the same preferences.
    let mut ws = Workspace::new();
    ws.apply_me(
        serde_json::from_value(serde_json::json!({
            "_id": "me", "username": "me", "status": "online",
            "settings": {"preferences": {
                "newMessageNotification": "none",
                "newRoomNotification": "ding",
                "masterVolume": 50,
                "desktopNotifications": "mentions",
                "highlights": ["rust", " "]
            }}
        }))
        .unwrap(),
    );
    let a = AccountSound::of(&ws);
    assert_eq!(a.new_message.as_deref(), Some("none"));
    assert_eq!(a.master_volume, Some(50.0));
    let ui = rustket_core::config::UiPrefs::default();
    assert_eq!(
        crate::videoconf::alerts_for(&ws, &ui, false),
        crate::videoconf::alerts_with(&ui, false, false, &a)
    );
    assert_eq!(
        crate::omnichannel::new_chat_sound(&a, true, false, false).map(|s| (s.name, s.volume)),
        Some(("ding".to_owned(), 50))
    );
    assert_eq!(
        crate::omnichannel::new_chat_sound(&a, true, true, false),
        None
    );
    let p = AccountPrefs::of(&ws);
    assert_eq!(p.desktop, Some(DesktopPref::Mentions));
    assert_eq!(p.highlights, vec!["rust".to_owned()]);
}

/// Monday 2026-11-16 at `h`:`m` UTC.
fn monday(h: u32, m: u32) -> chrono::DateTime<chrono::Utc> {
    use chrono::TimeZone;
    chrono::Utc.with_ymd_and_hms(2026, 11, 16, h, m, 0).unwrap()
}

fn env_at(cfg: &AppConfig, now: &chrono::DateTime<chrono::Utc>) -> HelpEnv {
    HelpEnv::new(cfg, now, true, true, true, Some(AccountEnv::default()))
}

/// A manual pause and off-hours at once: the cause that ends last, with its
/// own end, which its Q line shows too.
#[test]
fn manual_pause_overlapping_off_hours() {
    let mut cfg = cfg();
    cfg.ui.working_hours.enabled = true;
    let now = monday(20, 0);
    let ms = |t: chrono::DateTime<chrono::Utc>| t.timestamp_millis();
    let tuesday_9 = ms(monday(9, 0) + chrono::Duration::days(1));

    // Paused by hand until 20:30: off-hours last longer (Q2, Tue 09:00).
    cfg.pause.paused_until = Some(ms(monday(20, 30)));
    let env = env_at(&cfg, &now);
    assert!(env.quiet.paused && env.quiet.off_hours);
    assert_eq!(env.off_hours_until, Some(tuesday_9));
    assert_eq!(env.pause(&cfg), Some((Code::Q2, Some(tuesday_9))));
    let paused = ToastBlocked::Paused {
        until: Some(tuesday_9),
        cause: Code::Q2,
    };
    assert_eq!(
        evaluate(Code::M1, &cfg, &env).toast,
        Some(Toast::Blocked(vec![paused]))
    );
    let quiet = Sound::Silent(SoundOff::Paused {
        until: Some(tuesday_9),
        cause: Code::Q2,
    });
    for c in [Code::C1, Code::O1, Code::L1] {
        assert_eq!(evaluate(c, &cfg, &env).sound, Some(quiet.clone()), "{c:?}");
    }
    assert_eq!(
        global(Code::Q1, &cfg, &env),
        Some(Global::Pause {
            until: Some(ms(monday(20, 30)))
        })
    );
    assert_eq!(
        global(Code::Q2, &cfg, &env),
        Some(Global::WorkingHours(WorkingHoursState::Outside {
            until: Some(tuesday_9)
        }))
    );

    // Paused by hand until Wednesday 12:00: the manual pause lasts longer
    // (Q1); Q2 still shows when working hours start.
    let wednesday_12 = ms(monday(12, 0) + chrono::Duration::days(2));
    cfg.pause.paused_until = Some(wednesday_12);
    let env = env_at(&cfg, &now);
    assert_eq!(env.quiet.until, Some(wednesday_12));
    assert_eq!(env.pause(&cfg), Some((Code::Q1, Some(wednesday_12))));
    assert_eq!(
        global(Code::Q2, &cfg, &env),
        Some(Global::WorkingHours(WorkingHoursState::Outside {
            until: Some(tuesday_9)
        }))
    );

    // No working day: off-hours never end, they outlast any manual pause.
    for d in &mut cfg.ui.working_hours.days {
        d.on = false;
    }
    let env = env_at(&cfg, &now);
    assert_eq!(env.off_hours_until, None);
    assert_eq!(env.pause(&cfg), Some((Code::Q2, None)));
}

/// Q2 inside working hours, and resumed by hand outside them, also without
/// a working day (the `UNTIL_BOUNDARY` sentinel).
#[test]
fn working_hours_inside_and_resumed() {
    let mut cfg = cfg();
    cfg.ui.working_hours.enabled = true;
    let wh = |s: Code, cfg: &AppConfig, now: chrono::DateTime<chrono::Utc>| {
        global(s, cfg, &env_at(cfg, &now))
    };
    assert_eq!(
        wh(Code::Q2, &cfg, monday(10, 0)),
        Some(Global::WorkingHours(WorkingHoursState::Inside))
    );
    let now = monday(20, 0);
    rustket_core::quiet::resume(&mut cfg.pause, &cfg.ui.working_hours.clone(), &now);
    let tuesday_9 = (monday(9, 0) + chrono::Duration::days(1)).timestamp_millis();
    assert_eq!(cfg.pause.resumed_until, Some(tuesday_9));
    let env = env_at(&cfg, &now);
    assert!(!env.quiet.paused);
    assert_eq!(env.pause(&cfg), None);
    assert_eq!(
        global(Code::Q2, &cfg, &env),
        Some(Global::WorkingHours(WorkingHoursState::Resumed {
            until: Some(tuesday_9)
        }))
    );

    let mut cfg = self::cfg();
    cfg.ui.working_hours.enabled = true;
    for d in &mut cfg.ui.working_hours.days {
        d.on = false;
    }
    rustket_core::quiet::resume(&mut cfg.pause, &cfg.ui.working_hours.clone(), &now);
    assert_eq!(cfg.pause.resumed_until, Some(UNTIL_BOUNDARY));
    assert_eq!(
        wh(Code::Q2, &cfg, now),
        Some(Global::WorkingHours(WorkingHoursState::Resumed {
            until: None
        }))
    );
}

#[test]
fn starred_threads_follow_the_server() {
    let mut cfg = cfg();
    let mut env = env();
    let starred = |cfg: &AppConfig, env: &HelpEnv| {
        evaluate(Code::T1, cfg, env)
            .thread_row
            .unwrap()
            .starred_in_favorites
    };
    assert!(starred(&cfg, &env));
    acc(&mut env).server.allow_starring = false;
    assert!(!starred(&cfg, &env));
    let env = self::env();
    cfg.ui.sidebar_starred_threads = false;
    assert!(!starred(&cfg, &env));
}

/// K3: the same message in a room above `Notifications_Max_Room_Members`.
#[test]
fn large_rooms() {
    let cfg = cfg();
    let large = |env: &HelpEnv, c: Code| evaluate(c, &cfg, env).server.unwrap().large_room.unwrap();
    let blocked = Err(ServerBlock::LargeRoom { limit: 100 });
    let small = |env: &HelpEnv, c: Code| evaluate(c, &cfg, env).server.unwrap().desktop;

    // Preference never set (not stored on the subscriptions), no highlight
    // words: the subscription is selected only as a thread follower.
    let mut env = env();
    assert!(!acc(&mut env).desktop_stored);
    // DMs are capped by `DirectMesssage_maxUsers`: never a large room.
    assert_eq!(
        evaluate(Code::M2, &cfg, &env).server.unwrap().large_room,
        None
    );
    for c in [Code::M1, Code::M3, Code::M5] {
        assert_eq!(large(&env, c).limit, 100);
        assert_eq!(large(&env, c).desktop, blocked, "{c:?}");
    }
    // Rustket notifies the plain @mention itself (`large_room`); never
    // @all/@here or a plain message.
    assert!(large(&env, Code::M1).fallback);
    for c in [Code::M3, Code::M5] {
        assert!(!large(&env, c).fallback, "{c:?}");
    }
    // A highlight word selects it, but `shouldNotifyDesktop`'s first gate
    // still drops @all, plain and DM messages without a stored preference.
    assert!(large(&env, Code::M1).highlights_would_help);
    for c in [Code::M3, Code::M5] {
        assert!(!large(&env, c).highlights_would_help, "{c:?}");
    }
    for c in [Code::T1, Code::T2, Code::T3, Code::M4] {
        let l = large(&env, c);
        assert_eq!(
            (l.desktop, l.highlights_would_help, l.fallback),
            (Ok(()), false, false),
            "{c:?}"
        );
    }
    // No fallback where the room would not notify anyway: "Receive
    // notifications" off, or the room's alert "nothing".
    for mods in [
        RoomMods::of(Code::R1),
        RoomMods {
            alert: Some(DesktopPref::Nothing),
            ..RoomMods::default()
        },
    ] {
        let o = evaluate_with(Code::M1, mods, &cfg, &env);
        assert!(!o.server.unwrap().large_room.unwrap().fallback, "{mods:?}");
    }
    // Small rooms with the defaults: the server default "all" notifies.
    for c in [Code::M1, Code::M3, Code::M5, Code::T1] {
        assert_eq!(small(&env, c), Ok(()), "{c:?}");
    }

    // Not stored, with highlight words.
    acc(&mut env).prefs.highlights = vec!["rust".into()];
    for c in [Code::M3, Code::M5] {
        let l = large(&env, c);
        assert_eq!(
            (l.desktop, l.highlights_would_help),
            (blocked, false),
            "{c:?}"
        );
    }
    for c in [Code::M1, Code::M4, Code::T1, Code::T2, Code::T3] {
        assert_eq!(large(&env, c).desktop, Ok(()), "{c:?}");
        assert!(!large(&env, c).fallback, "the server notifies {c:?}");
    }

    // Stored "all", with highlight words: selected and notified.
    acc(&mut env).desktop_stored = true;
    for c in [Code::M1, Code::M3, Code::M4, Code::M5, Code::T1] {
        assert_eq!(large(&env, c).desktop, Ok(()), "{c:?}");
    }
    for c in [Code::M1, Code::M3, Code::M5, Code::T1] {
        assert_eq!(small(&env, c), Ok(()), "{c:?}");
    }
    // Stored "all", no highlight words: a "user" origin does not select
    // it, one highlight word would.
    acc(&mut env).prefs.highlights.clear();
    for c in [Code::M1, Code::M3, Code::M5] {
        let l = large(&env, c);
        assert_eq!(
            (l.desktop, l.highlights_would_help, l.fallback),
            (blocked, true, c == Code::M1),
            "{c:?}"
        );
    }

    // A room alert "all" set on the room (not copied from the account).
    let env = self::env();
    let o = evaluate_with(
        Code::M5,
        RoomMods {
            alert: Some(DesktopPref::All),
            ..RoomMods::default()
        },
        &cfg,
        &env,
    );
    assert_eq!(o.server.unwrap().large_room.unwrap().desktop, Ok(()));
    // No limit: no line.
    let mut env = self::env();
    acc(&mut env).server.max_room_members = 0;
    assert_eq!(
        evaluate(Code::M1, &cfg, &env).server.unwrap().large_room,
        None
    );
}

/// A server default the user never changed: the server default decides
/// M5 (`ServerDefault`); once stored, the account preference does.
#[test]
fn server_default_or_stored_preference() {
    let cfg = cfg();
    let mut env = env();
    acc(&mut env).server.default_desktop = Some(DesktopPref::Mentions);
    acc(&mut env).prefs.desktop = Some(DesktopPref::Mentions);
    acc(&mut env).desktop_stored = false;
    let block = |env: &HelpEnv| evaluate(Code::M5, &cfg, env).server.unwrap().desktop;
    assert_eq!(
        block(&env),
        Err(ServerBlock::ServerDefault(Some(DesktopPref::Mentions)))
    );
    acc(&mut env).desktop_stored = true;
    assert_eq!(
        block(&env),
        Err(ServerBlock::Preference {
            pref: DesktopPref::Mentions,
            room: false
        })
    );
}

/// The scenario subscriptions leave mobile and email unset, so the server's
/// query decides by the desktop value and the server defaults; with
/// Rocket.Chat's defaults every M/T message notifies in a small room.
#[test]
fn scenario_subscriptions_leave_mobile_and_email_unset() {
    let cfg = cfg();
    let env = env();
    for c in [
        Code::M1,
        Code::M2,
        Code::M3,
        Code::M4,
        Code::M5,
        Code::T1,
        Code::T2,
        Code::T3,
    ] {
        let s = evaluate(c, &cfg, &env).server.unwrap();
        assert_eq!(s.sub.mobile_push_notifications, None, "{c:?}");
        assert_eq!(s.sub.email_notifications, None, "{c:?}");
        assert_eq!(s.desktop, Ok(()), "{c:?}");
    }
}

/// Before `/me` loads the account preference is the server default, not
/// stored on the subscriptions.
#[test]
fn pending_account_takes_the_server_default() {
    use rocketchat::models::Setting;
    let mut ws = Workspace::new();
    ws.apply_setting(
        Change::Inserted,
        Setting {
            id: "Accounts_Default_User_Preferences_desktopNotifications".into(),
            value: serde_json::json!("mentions"),
            ..Default::default()
        },
    );
    let a = AccountEnv::pending("x".into(), &ws, false);
    assert_eq!(a.prefs.desktop, Some(DesktopPref::Mentions));
    assert!(!a.desktop_stored);
    assert_eq!(a.server.default_desktop, Some(DesktopPref::Mentions));
}

/// No account: Rocket.Chat's defaults and no dev-server identity.
#[test]
fn no_account_uses_a_placeholder() {
    let env = HelpEnv {
        account: None,
        ..env()
    };
    let o = evaluate(Code::M1, &cfg(), &env);
    let sub = o.server.unwrap().sub;
    assert_eq!(sub.u.id, PLACEHOLDER_USER_ID);
    assert_eq!(sub.u.username.as_deref(), Some(PLACEHOLDER_USERNAME));
    assert_eq!(o.feed.unwrap().chip_mine, Some(true));
    let p = AccountEnv::placeholder();
    assert!(p.label.is_empty());
    assert_eq!(p.prefs, AccountPrefs::default());
    assert_eq!(p.server, ServerFacts::default());
}

/// `AccountEnv::of` and `RoomUsage::of` on a synthetic workspace.
#[test]
fn account_env_of_a_workspace() {
    use rocketchat::models::Setting;
    let mut ws = Workspace::new();
    ws.apply_me(
        serde_json::from_value(serde_json::json!({
            "_id": "u1", "username": "dana", "status": "busy",
            "settings": {"preferences": {
                "desktopNotifications": "nothing",
                "newMessageNotification": "ding",
                "desktopNotificationRequireInteraction": true,
                "muteFocusedConversations": false,
                "highlights": ["deploy"]
            }}
        }))
        .unwrap(),
    );
    for (id, v) in [
        ("Unread_Count", serde_json::json!("all_messages")),
        ("Notifications_Max_Room_Members", serde_json::json!(0)),
        ("Message_AllowStarring", serde_json::json!(false)),
        ("Threads_enabled", serde_json::json!(false)),
    ] {
        ws.apply_setting(
            Change::Inserted,
            Setting {
                id: id.into(),
                value: v,
                ..Default::default()
            },
        );
    }
    ws.apply_connection(ConnectionState::Reconnecting { attempt: 1 });
    let sub = |rid: &str, open: bool, extra: serde_json::Value| -> Subscription {
        let mut v = serde_json::json!({
            "_id": format!("s-{rid}"), "rid": rid, "name": rid, "t": "c", "open": open,
            "u": {"_id": "u1", "username": "dana"}
        });
        for (k, x) in extra.as_object().unwrap() {
            v[k] = x.clone();
        }
        serde_json::from_value(v).unwrap()
    };
    let subs = [
        sub(
            "a",
            true,
            serde_json::json!({"disableNotifications": true, "hideUnreadStatus": true,
                "desktopNotifications": "nothing", "desktopPrefOrigin": "subscription"}),
        ),
        sub(
            "b",
            true,
            serde_json::json!({"hideMentionStatus": true, "muteGroupMentions": true,
                "desktopNotifications": "mentions", "desktopPrefOrigin": "subscription"}),
        ),
        // Copied from the account: not a room setting.
        sub(
            "c",
            true,
            serde_json::json!({"desktopNotifications": "all", "desktopPrefOrigin": "user"}),
        ),
        // Closed rooms are not counted.
        sub(
            "d",
            false,
            serde_json::json!({"disableNotifications": true, "muteGroupMentions": true,
                "desktopNotifications": "all", "desktopPrefOrigin": "subscription"}),
        ),
        sub(
            "e",
            true,
            serde_json::json!({"desktopNotifications": "all", "desktopPrefOrigin": "subscription"}),
        ),
    ];
    // Without room "c" no subscription carries the account preference.
    let mut bare = Workspace::new();
    for s in subs.iter().filter(|s| s.rid != "c") {
        bare.apply_subscription(Change::Inserted, s.clone());
    }
    assert!(!AccountEnv::of(String::new(), &bare, false).desktop_stored);
    for s in subs {
        ws.apply_subscription(Change::Inserted, s);
    }
    let a = AccountEnv::of("dana @ Test".into(), &ws, true);
    // Room "c" carries the account preference.
    assert!(a.desktop_stored);
    assert_eq!(a.label, "dana @ Test");
    assert_eq!((a.user_id.as_str(), a.username.as_str()), ("u1", "dana"));
    assert_eq!(a.status, UserStatus::Busy);
    assert_eq!(a.connection, Connection::Offline { cache: true });
    assert_eq!(a.prefs.desktop, Some(DesktopPref::Nothing));
    assert_eq!(a.prefs.sound.new_message.as_deref(), Some("ding"));
    assert!(a.prefs.require_interaction);
    assert!(!a.prefs.mute_focused);
    assert_eq!(a.prefs.highlights, vec!["deploy".to_owned()]);
    assert_eq!(a.server.unread_count, UnreadCount::AllMessages);
    assert_eq!(a.server.max_room_members, 0);
    assert!(!a.server.allow_starring);
    assert!(!a.server.threads_enabled);
    assert_eq!(
        a.usage,
        Some(RoomUsage {
            notifications_off: 1,
            counter_hidden: 1,
            mentions_hidden: 1,
            group_mentions_muted: 1,
            alert_all: 1,
            alert_mentions: 1,
            alert_nothing: 1,
        })
    );
    for (state, want) in [
        (ConnectionState::Connected, Connection::Connected),
        (ConnectionState::Connecting, Connection::Connecting),
        (
            ConnectionState::Disconnected,
            Connection::Offline { cache: false },
        ),
        (ConnectionState::LoggedOut, Connection::SignedOut),
    ] {
        assert_eq!(Connection::of(&state, false), want);
    }
}

/// The modelled thread listing agrees with the sidebar's Threads section
/// (`room_list::all_thread_rows`) on the scenario's workspace.
#[test]
fn thread_listing_agrees_with_the_sidebar() {
    let mods = [None, Some(Code::R2), Some(Code::R3)];
    for mode in [
        ThreadsMode::Off,
        ThreadsMode::Unread,
        ThreadsMode::UnreadKept,
        ThreadsMode::Following,
    ] {
        for threads_enabled in [true, false] {
            let mut cfg = cfg();
            cfg.ui.sidebar_threads = mode;
            let mut env = env();
            acc(&mut env).server.threads_enabled = threads_enabled;
            let acc = env.account.clone().unwrap();
            for code in [Code::T1, Code::T2, Code::T3] {
                for m in mods {
                    let o =
                        evaluate_with(code, m.map(RoomMods::of).unwrap_or_default(), &cfg, &env);
                    let t = o.thread_row.clone().unwrap();
                    // Followed threads come from a crawl the scene does not
                    // have.
                    if mode == ThreadsMode::Following && t.unread.is_none() {
                        continue;
                    }
                    let sub = o.server.as_ref().unwrap().sub.clone();
                    let mut s = scene(
                        sub,
                        times(&env, true).1,
                        &acc,
                        cfg.ui.sidebar_starred_threads,
                    );
                    s.ws.set_thread_config(mode, []);
                    let opts = RoomListOptions::from_prefs(&cfg.ui, &s.ws);
                    let rows = room_list::all_thread_rows(&s.ws, &opts, "");
                    let row = rows.iter().find(|r| r.tmid == TMID);
                    let case = format!("{mode:?} {threads_enabled} {code:?} {m:?}");
                    assert_eq!(
                        matches!(t.listing, ThreadListing::Threads { .. }),
                        row.is_some(),
                        "{case}"
                    );
                    if let Some(r) = row {
                        assert_eq!(r.unread, t.unread, "{case}");
                        assert_eq!(sidebar::thread_unread_kind(r.unread), t.kind, "{case}");
                    }
                }
            }
        }
    }
}

/// Q4: `muteFocusedConversations` the user never set is Rocket.Chat's
/// default (on), as `Notifications::notify` reads it.
#[test]
fn mute_focused_defaults_on() {
    let ws = Workspace::new();
    assert!(AccountPrefs::of(&ws).mute_focused);
    assert!(AccountPrefs::default().mute_focused);
}

/// For every platform the Help page says what `desktop::sound::route`
/// decides for a shown toast whose backend plays sounds: the system's own
/// sound when the toast cannot play the Rocket.Chat file, the Rocket.Chat
/// sound otherwise; without a toast always the Rocket.Chat sound.
#[test]
fn help_sound_agrees_with_route() {
    use crate::desktop::sound::{OsSound, Route, Toast as OsToast, route};
    let chime = RcSound {
        name: "chime".into(),
        volume: 100,
    };
    let file = std::path::Path::new("chime.wav");
    for platform in [Platform::Linux, Platform::Windows, Platform::MacOs] {
        let env = HelpEnv { platform, ..env() };
        let mut cfg = cfg();
        let shown = route(
            platform,
            OsToast::Shown { plays_sound: true },
            Some(&chime),
            Some(file),
        );
        let system = shown == Route::Os(OsSound::Default);
        let message = if system {
            SoundKind::WindowsNotification
        } else {
            SoundKind::Message("chime".into())
        };
        let reminder = if system {
            SoundKind::WindowsNotification
        } else {
            SoundKind::Reminder("chime".into())
        };
        for code in [Code::M1, Code::M2, Code::T1] {
            assert_eq!(
                evaluate(code, &cfg, &env).sound,
                Some(Sound::Plays(message.clone())),
                "{platform:?} {code:?}"
            );
        }
        for code in [Code::L1, Code::L2] {
            assert_eq!(
                evaluate(code, &cfg, &env).sound,
                Some(Sound::Plays(reminder.clone())),
                "{platform:?} {code:?}"
            );
        }
        // No toast: Rustket plays the Rocket.Chat sound itself.
        assert!(matches!(
            route(platform, OsToast::None, Some(&chime), Some(file)),
            Route::Local { .. }
        ));
        cfg.ui.desktop_notifications = false;
        assert_eq!(
            evaluate(Code::M2, &cfg, &env).sound,
            Some(Sound::Plays(SoundKind::Message("chime".into()))),
            "{platform:?}"
        );
    }
}

/// On Windows a shown toast plays the Windows notification sound in place
/// of the Rocket.Chat sound; the Rocket.Chat sound plays only without a
/// toast (`desktop::sound::route`). Linux and macOS play the Rocket.Chat
/// sound with the notification.
#[test]
fn windows_toasts_play_the_windows_sound() {
    let win = HelpEnv {
        platform: Platform::Windows,
        ..env()
    };
    let mut cfg = cfg();
    let windows = Some(Sound::Plays(SoundKind::WindowsNotification));
    for code in [Code::M1, Code::M2, Code::T1, Code::L1, Code::L2] {
        assert_eq!(evaluate(code, &cfg, &win).sound, windows, "{code:?}");
    }
    let mac = HelpEnv {
        platform: Platform::MacOs,
        ..env()
    };
    assert_eq!(
        evaluate(Code::M2, &cfg, &mac).sound,
        Some(Sound::Plays(SoundKind::Message("chime".into())))
    );
    assert_eq!(
        evaluate(Code::L1, &cfg, &mac).sound,
        Some(Sound::Plays(SoundKind::Reminder("chime".into())))
    );
    // Calls and new chats keep their own sounds.
    assert_eq!(
        evaluate(Code::C1, &cfg, &win).sound,
        Some(Sound::Plays(SoundKind::Ring))
    );
    assert!(matches!(
        evaluate(Code::O1, &cfg, &win).sound,
        Some(Sound::Plays(SoundKind::NewChat(_)))
    ));
    // Desktop notifications off: the sound alone is Rocket.Chat's.
    cfg.ui.desktop_notifications = false;
    assert_eq!(
        evaluate(Code::M2, &cfg, &win).sound,
        Some(Sound::Plays(SoundKind::Message("chime".into())))
    );
}

// --- the Rocket.Chat scheme -------------------------------------------------

fn rc_cfg() -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.ui.indicator_scheme = IndicatorScheme::RocketChat;
    cfg
}

/// (count, kind) of a Rocket.Chat badge.
fn rc_of(b: Option<RcBadge>) -> Option<(u64, BadgeVariant)> {
    b.map(|b| (b.count, b.variant))
}

/// `AppConfig::default` is the Rocket.Chat scheme.
#[test]
fn the_default_config_is_rocketchat() {
    let o = evaluate(Code::M1, &AppConfig::default(), &env());
    assert_eq!(o.scheme, IndicatorScheme::RocketChat);
    assert!(o.row.unwrap().rc.is_some());
}

/// Each M code under the Rocket.Chat scheme (dev-server facts): one grey,
/// red, orange or no counter per room, full numbers, no dots, the highlighted
/// name; RC's own window, tray and Dock; no row highlight in the feed.
#[test]
fn rocketchat_messages_under_the_defaults() {
    let (cfg, env) = (rc_cfg(), env());
    let o = |c| evaluate(c, &cfg, &env);

    let m1 = o(Code::M1);
    assert_eq!(m1.scheme, IndicatorScheme::RocketChat);
    assert_eq!(row(&m1), (Some(BadgeKind::Mention), 1, true));
    assert_eq!(m1.row.as_ref().unwrap().rc.unwrap().count, 1);
    assert_eq!(m1.collapsed, None);
    assert_eq!(rc_of(m1.rc_collapsed), Some((1, BadgeVariant::Mention)));
    let p = m1.pinned.clone().unwrap();
    assert_eq!(p.all_unreads, Some((1, BadgeKind::Unread)));
    assert_eq!(p.activity, Some((1, BadgeKind::Mention)));
    let icon = m1.account_icon.unwrap();
    assert_eq!(icon.badge, None);
    assert_eq!(icon.rc, Some(WorkspaceBadge::Ghost(1)));
    assert_eq!(m1.window, None);
    let w = m1.rc_window.clone().unwrap();
    assert_eq!(w.rc.unread, 1);
    assert_eq!(w.art, Some(TrayArt::RcCount(1)));
    assert_eq!(w.title, "Rustket (1)");
    assert_eq!(w.dock, "1");
    let f = m1.feed.unwrap();
    assert!(!f.highlighted);
    assert_eq!(
        m1.rc_feed,
        Some(RcFeed {
            chip: Some(RcChip::Me),
            thread_dot: None
        })
    );
    // The toast and the sound are shared.
    assert_eq!(m1.toast, Some(SHOWN));

    // M2: a DM is a grey counter, not red.
    let m2 = o(Code::M2);
    assert_eq!(row(&m2), (Some(BadgeKind::Unread), 1, true));
    assert_eq!(rc_of(m2.rc_collapsed), Some((1, BadgeVariant::Unread)));
    assert_eq!(m2.pinned.clone().unwrap().activity, None);
    assert_eq!(m2.rc_window.unwrap().art, Some(TrayArt::RcCount(1)));

    let m3 = o(Code::M3);
    assert_eq!(row(&m3), (Some(BadgeKind::GroupMention), 1, true));
    assert_eq!(m3.account_icon.unwrap().rc, Some(WorkspaceBadge::Ghost(1)));
    assert_eq!(m3.rc_feed.unwrap().chip, Some(RcChip::All));
    assert!(!m3.feed.unwrap().highlighted);

    // M4: a red counter; the word is marked, the row is not highlighted.
    let m4 = o(Code::M4);
    assert_eq!(row(&m4), (Some(BadgeKind::Mention), 1, true));
    let f = m4.feed.unwrap();
    assert!(f.word_marked && !f.highlighted);
    assert_eq!(m4.rc_feed.unwrap().chip, None);

    // M5: no counter and no dot, only the highlighted name; the collapsed
    // header counts the alert as 1; the bullet everywhere else.
    let m5 = o(Code::M5);
    assert_eq!(row(&m5), (None, 0, true));
    assert_eq!(rc_of(m5.rc_collapsed), Some((1, BadgeVariant::Unread)));
    assert_eq!(
        m5.account_icon.unwrap().rc,
        Some(WorkspaceBadge::GhostBullet)
    );
    let w = m5.rc_window.clone().unwrap();
    assert!(w.rc.shows_bullet());
    assert_eq!(w.art, Some(TrayArt::RcDot));
    assert_eq!(w.title, "Rustket");
    assert_eq!(w.dock, "•");
    // Unread-first ordering still sees the room (`RcBadge::marked`).
    assert!(m5.row.unwrap().rc.unwrap().marked);
}

#[test]
fn rocketchat_threads_under_the_defaults() {
    let (cfg, env) = (rc_cfg(), env());
    let o = |c| evaluate(c, &cfg, &env);

    let t1 = o(Code::T1);
    assert_eq!(row(&t1), (Some(BadgeKind::Thread), 1, true));
    let t = t1.thread_row.clone().unwrap();
    // Presence is Rustket's, the colour Rocket.Chat's.
    assert_eq!(t.unread, Some(BadgeVariant::Thread));
    assert_eq!(t.kind, Some(BadgeKind::Thread));
    assert_eq!(t.header_button, None);
    assert_eq!(t.header_counter, Some((1, BadgeKind::Thread)));
    assert_eq!(t1.rc_feed.unwrap().thread_dot, Some(BadgeKind::Thread));
    assert_eq!(
        t1.pinned.clone().unwrap().activity,
        Some((1, BadgeKind::Thread))
    );
    // A thread reply adds no unread: the bullet.
    assert_eq!(t1.rc_window.unwrap().art, Some(TrayArt::RcDot));

    let t2 = o(Code::T2);
    assert_eq!(row(&t2), (Some(BadgeKind::Mention), 1, true));
    let t = t2.thread_row.clone().unwrap();
    assert_eq!(t.kind, Some(BadgeKind::Mention));
    assert_eq!(t.header_counter, Some((1, BadgeKind::Mention)));
    assert!(!t2.feed.unwrap().highlighted);

    // Threads beat @all in the room's counter; the thread itself is orange.
    let t3 = o(Code::T3);
    assert_eq!(row(&t3), (Some(BadgeKind::Thread), 1, true));
    let t = t3.thread_row.clone().unwrap();
    assert_eq!(t.kind, Some(BadgeKind::GroupMention));
    assert_eq!(t.header_counter, Some((1, BadgeKind::GroupMention)));
    assert_eq!(
        t3.rc_feed.unwrap().thread_dot,
        Some(BadgeKind::GroupMention)
    );

    // With the Threads section on, its collapsed header counts the thread.
    let mut cfg = rc_cfg();
    cfg.ui.sidebar_threads = ThreadsMode::Unread;
    let t2 = evaluate(Code::T2, &cfg, &env);
    assert_eq!(t2.collapsed_threads, None);
    assert_eq!(
        rc_of(t2.rc_collapsed_threads),
        Some((1, BadgeVariant::Mention))
    );
}

/// R1–R4 under Rocket.Chat: R1 and R4 change nothing visible, R2 keeps only
/// the mention counters, R3 alone changes nothing.
#[test]
fn rocketchat_matrix() {
    let (cfg, env) = (rc_cfg(), env());
    let m = matrix(&cfg, &env);
    let cell = |code: Code, col: usize| {
        let (_, cells) = m.iter().find(|(c, _)| *c == code).unwrap();
        let r = &cells[col].row;
        (r.kind, r.count, r.bold)
    };
    let as_is = |code| cell(code, 0);
    for code in MATRIX_CODES {
        // R1, R3 and R4 look like "as is".
        for col in [1, 3, 4] {
            assert_eq!(cell(code, col), as_is(code), "{code:?} column {col}");
        }
    }
    // R2: mentions keep their counter, without the highlighted name; the rest
    // disappears.
    assert_eq!(cell(Code::M1, 2), (Some(BadgeKind::Mention), 1, false));
    assert_eq!(cell(Code::M3, 2), (Some(BadgeKind::GroupMention), 1, false));
    assert_eq!(cell(Code::M2, 2), (None, 0, false));
    assert_eq!(cell(Code::M5, 2), (None, 0, false));
    assert_eq!(cell(Code::T1, 2), (None, 0, false));
    assert_eq!(cell(Code::T2, 2), (Some(BadgeKind::Mention), 1, false));
    // R1 still blocks the server's notification.
    let (_, cells) = m.iter().find(|(c, _)| *c == Code::M1).unwrap();
    assert!(cells[0].server_toast && !cells[1].server_toast);
}

/// The legend lists the Rocket.Chat kinds only (no grey Muted, no dots on
/// room rows), with the header's Threads counter.
#[test]
fn rocketchat_legend() {
    let (cfg, env) = (rc_cfg(), env());
    let legend = legend(&cfg, &env);
    let kinds: Vec<BadgeKind> = legend.iter().map(|(k, _)| *k).collect();
    assert!(!kinds.contains(&BadgeKind::Muted), "{kinds:?}");
    for k in [
        BadgeKind::Mention,
        BadgeKind::Unread,
        BadgeKind::GroupMention,
        BadgeKind::Thread,
    ] {
        assert!(kinds.contains(&k), "{k:?} missing");
    }
    let uses: Vec<&KindUse> = legend.iter().flat_map(|(_, u)| u).collect();
    assert!(
        !uses.iter().any(|u| u.surface == Surface::Row && u.dot),
        "a room row dot"
    );
    assert!(
        !uses.iter().any(|u| u.surface == Surface::AccountIcon),
        "the grey workspace badge has no kind"
    );
    assert!(
        uses.iter()
            .any(|u| u.code == Code::T3 && u.surface == Surface::ThreadsButton && !u.dot)
    );
    // Drafts on time are grey, not Muted.
    let d1 = evaluate(Code::D1, &cfg, &env);
    assert_eq!(d1.scheme, IndicatorScheme::RocketChat);
    assert_eq!(
        d1.pinned.unwrap().drafts_on_time,
        Some((1, BadgeKind::Unread))
    );
}

/// The account's "Unread Tray Icon Alert" off: an alert alone shows
/// nothing on the tray, the Dock or the workspace icon.
#[test]
fn rocketchat_unread_alert_preference() {
    let cfg = rc_cfg();
    let mut env = env();
    assert!(acc(&mut env).prefs.unread_alert);
    acc(&mut env).prefs.unread_alert = false;
    let m5 = evaluate(Code::M5, &cfg, &env);
    let w = m5.rc_window.unwrap();
    assert!(!w.rc.any());
    assert_eq!(w.art, Some(TrayArt::Plain));
    assert_eq!(w.dock, "");
    assert_eq!(m5.account_icon.unwrap().rc, Some(WorkspaceBadge::None));
    // A counted message still shows its number.
    let m1 = evaluate(Code::M1, &cfg, &env);
    assert_eq!(m1.rc_window.unwrap().art, Some(TrayArt::RcCount(1)));
    // No tray: no art; the window icon is badged on its own.
    env.tray = false;
    let w = evaluate(Code::M1, &cfg, &env).rc_window.unwrap();
    assert_eq!(w.art, None);
    assert_eq!(w.icon, Some(TrayArt::RcCount(1)));
    // Wayland: the tray only.
    env.tray = true;
    env.window_icon = false;
    let w = evaluate(Code::M1, &cfg, &env).rc_window.unwrap();
    assert_eq!(w.art, Some(TrayArt::RcCount(1)));
    assert_eq!(w.icon, None);
}

/// M2 on a server whose DMs count mentions only (`Unread_Count_DM =
/// mentions_only`): no counter, only the highlighted name.
#[test]
fn rocketchat_dm_mentions_only() {
    let mut settings = rustket_core::settings::Settings::new();
    settings.set_value("Unread_Count_DM", "mentions_only".into());
    let facts = crate::help::server::ServerFacts::from_settings(&settings);
    assert_ne!(facts.unread_count_dm, UnreadCount::AllMessages);
    let cfg = rc_cfg();
    let mut env = env();
    assert_eq!(
        row(&evaluate(Code::M2, &cfg, &env)),
        (Some(BadgeKind::Unread), 1, true)
    );
    acc(&mut env).server = facts;
    assert_eq!(row(&evaluate(Code::M2, &cfg, &env)), (None, 0, true));
}

/// The Rocket.Chat outcomes are the app's own: the sidebar row, collapsed
/// header and Threads counter functions give the same values.
#[test]
fn rocketchat_outcomes_agree_with_the_app() {
    let (cfg, env) = (rc_cfg(), env());
    for code in [Code::M1, Code::M3, Code::T2, Code::T3] {
        let o = evaluate(code, &cfg, &env);
        let sub = &o.server.as_ref().unwrap().sub;
        let rc = rustket_core::indicators::rc_badge_of(UnreadInfo::of(sub));
        assert_eq!(o.row.as_ref().unwrap().rc, Some(rc), "{code:?}");
        if let Some(t) = &o.thread_row {
            assert_eq!(
                t.header_counter,
                rustket_core::indicators::rc_threads_button(sub)
                    .map(|(n, v)| (n, sidebar::widget_kind(v))),
                "{code:?}"
            );
            assert_eq!(
                o.rc_feed.unwrap().thread_dot,
                crate::room::timeline::parent_thread_kind(
                    Some(sub),
                    TMID,
                    IndicatorScheme::RocketChat
                )
            );
        }
    }
}
