//! Help tab: the "Indicators and notifications" reference. Every value is
//! computed by `crate::help::model` from the current settings; this file
//! only lays it out, with live samples drawn by the sidebar's own widgets.
//! Strings: `locales/help.yml` (`help.*`).

use chrono::{DateTime, Local, TimeZone};
use iced::widget::{Column, Row, column, container, row, space, text};
use iced::{Alignment, Element, Length};
use rocketchat::models::UserStatus;
use rust_i18n::t;
use rustket_core::config::{AppConfig, Theme as ThemePref, ThreadsMode};
use rustket_ui::icon::{self, Icon};
use rustket_ui::style;
use rustket_ui::theme::{
    Mode, SPACE_1, SPACE_2, SPACE_3, SPACE_4, TEXT_BODY, TEXT_CAPTION, TEXT_SMALL, font_mono,
    font_semibold,
};
use rustket_ui::widget::{
    AvatarSource, BadgeKind, RailIcon, badge, dot, inert, room_list_item, title_bar_workspace,
    unread_separator, workspace_rail_item,
};

use super::widgets::{action_row, caption, section};
use super::{Message, Tab};
use crate::desktop::tray::TrayLevel;
use crate::help::model::{
    self, AccountIcon, Cell, Code, Connection, Family, Feed, Global, HelpEnv, IconPlace, KindUse,
    Note, Outcome, PLACEHOLDER_USER_ID, PinnedOutcome, Placement, RowOutcome, ServerOutcome, Sound,
    SoundKind, SoundOff, Surface, ThreadListing, ThreadRowOutcome, Toast, ToastBlocked,
    WindowOutcome, WorkingHoursState,
};
use crate::help::server::{DesktopPref, ServerBlock, UnreadCount};

/// Width of the live samples (about the sidebar's).
const SAMPLE_WIDTH: f32 = 280.0;
/// Width of the surface labels.
const LABEL_WIDTH: f32 = 190.0;
/// Width of the code column of the R matrix.
const CODE_WIDTH: f32 = 40.0;
/// Width of the surface labels of the colour legend.
const USE_LABEL_WIDTH: f32 = 150.0;
/// The room of the samples.
const SAMPLE_ROOM: &str = "general";

/// What the tab is computed from.
pub(crate) struct Data<'a> {
    pub cfg: &'a AppConfig,
    pub env: &'a HelpEnv,
    /// The theme mode in use (for the hex values of the legend).
    pub mode: Mode,
    /// The theme's display name.
    pub theme_name: String,
}

/// The tab's cards.
pub(crate) fn view<'a>(d: &Data<'_>) -> Vec<Element<'a, Message>> {
    let mut cards = vec![header(d), current_settings(d), legend(d)];
    for f in [
        Family::Messages,
        Family::Threads,
        Family::Later,
        Family::Calls,
    ] {
        cards.push(family_card(f, d));
    }
    cards.push(room_card(d));
    cards.push(global_card(d));
    cards.push(section(
        t!("settings.keyboard").into_owned(),
        vec![action_row(
            t!("shortcuts.title").into_owned(),
            caption(t!("help.shortcuts_caption").into_owned()).into(),
            t!("settings.shortcuts_show").into_owned(),
            Some(Message::ShowShortcuts),
        )],
    ));
    cards
}

// --- small pieces -------------------------------------------------------------

fn now_local(env: &HelpEnv) -> DateTime<Local> {
    Local
        .timestamp_millis_opt(env.now_ms)
        .single()
        .unwrap_or_else(Local::now)
}

/// "18:00", "tomorrow at 09:00", …
fn moment(at: i64, d: &Data<'_>) -> String {
    crate::format::moment(at, now_local(d.env), d.cfg.ui.clock_24h)
}

fn on_off(b: bool) -> String {
    if b { t!("help.on") } else { t!("help.off") }.into_owned()
}

/// "text (R1)".
fn with_code(s: String, code: Option<Code>) -> String {
    match code {
        Some(c) => format!("{s} ({})", c.code()),
        None => s,
    }
}

fn chip<'a>(code: Code) -> Element<'a, Message> {
    container(text(code.code()).size(TEXT_CAPTION).font(font_mono()))
        .padding([1, 6])
        .style(style::container_inline_code)
        .into()
}

/// A code's heading: chip and title.
fn heading<'a>(code: Code) -> Element<'a, Message> {
    row![
        chip(code),
        text(t!(code.title_key()).into_owned())
            .size(TEXT_BODY)
            .font(font_semibold()),
    ]
    .spacing(SPACE_2)
    .align_y(Alignment::Center)
    .into()
}

/// "Label    value" (one surface).
fn line<'a>(label: String, value: String) -> Element<'a, Message> {
    row![
        text(label)
            .size(TEXT_SMALL)
            .style(style::text_secondary)
            .width(Length::Fixed(LABEL_WIDTH)),
        text(value).size(TEXT_SMALL).width(Length::Fill),
    ]
    .spacing(SPACE_3)
    .into()
}

/// The role name of a badge kind ("mention colour").
fn kind_name(k: BadgeKind) -> String {
    match k {
        BadgeKind::Unread => t!("help.kind.unread"),
        BadgeKind::Mention => t!("help.kind.mention"),
        BadgeKind::GroupMention => t!("help.kind.group_mention"),
        BadgeKind::Thread => t!("help.kind.thread"),
        BadgeKind::Muted => t!("help.kind.muted"),
    }
    .into_owned()
}

fn kind_meaning(k: BadgeKind) -> String {
    match k {
        BadgeKind::Unread => t!("help.kind_meaning.unread"),
        BadgeKind::Mention => t!("help.kind_meaning.mention"),
        BadgeKind::GroupMention => t!("help.kind_meaning.group_mention"),
        BadgeKind::Thread => t!("help.kind_meaning.thread"),
        BadgeKind::Muted => t!("help.kind_meaning.muted"),
    }
    .into_owned()
}

fn kind_token(k: BadgeKind) -> &'static str {
    match k {
        BadgeKind::Unread => "badge_unread",
        BadgeKind::Mention => "badge_mention",
        BadgeKind::GroupMention => "badge_group_mention",
        BadgeKind::Thread => "badge_thread",
        BadgeKind::Muted => "badge_muted",
    }
}

/// "counter 3, mention colour" / "dot, thread colour" / "nothing".
fn badge_text(b: Option<(u64, BadgeKind)>) -> String {
    match b {
        None => t!("help.badge.none"),
        Some((0, k)) => t!("help.badge.dot", kind = kind_name(k)),
        Some((n, k)) => t!("help.badge.counter", n = n, kind = kind_name(k)),
    }
    .into_owned()
}

/// A badge or dot as the sidebar draws it; `None`: a dash.
fn badge_sample<'a>(b: Option<(u64, BadgeKind)>) -> Element<'a, Message> {
    match b {
        Some((0, k)) => dot(k, 8.0),
        Some((n, k)) => badge(u32::try_from(n).unwrap_or(u32::MAX), k),
        None => text("—").size(TEXT_SMALL).style(style::text_hint).into(),
    }
}

fn desktop_pref_label(p: Option<DesktopPref>) -> String {
    match p {
        Some(DesktopPref::All) => t!("room_panel.alert_all"),
        Some(DesktopPref::Mentions) => t!("room_panel.alert_mentions"),
        Some(DesktopPref::Nothing) => t!("room_panel.alert_nothing"),
        None => t!("room_panel.alert_default"),
    }
    .into_owned()
}

/// The label of an account sound name, as My account → Preferences shows
/// it (the raw name when unknown).
fn account_sound_label(name: Option<&str>) -> String {
    match name {
        None | Some("default") => t!("account.opt_default").into_owned(),
        Some(name) => crate::screens::account::preferences::SOUNDS
            .iter()
            .find(|(v, _)| *v == name)
            .map_or_else(|| name.to_owned(), |(_, key)| t!(*key).into_owned()),
    }
}

fn count_label(c: UnreadCount) -> String {
    match c {
        UnreadCount::AllMessages => t!("help.count.all_messages"),
        UnreadCount::UserMentionsOnly => t!("help.count.user_mentions_only"),
        UnreadCount::GroupMentionsOnly => t!("help.count.group_mentions_only"),
        UnreadCount::UserAndGroupMentionsOnly => t!("help.count.user_and_group_mentions_only"),
    }
    .into_owned()
}

fn threads_mode_label(m: ThreadsMode) -> String {
    match m {
        ThreadsMode::Off => t!("settings.threads_off"),
        ThreadsMode::Unread => t!("settings.threads_unread"),
        ThreadsMode::UnreadKept => t!("settings.threads_unread_kept"),
        ThreadsMode::Following => t!("settings.threads_following"),
    }
    .into_owned()
}

fn connection_label(c: Option<Connection>) -> String {
    match c {
        None => t!("help.no_account_short"),
        Some(Connection::Connected) => t!("help.connection.connected"),
        Some(Connection::Connecting) => t!("help.connection.connecting"),
        Some(Connection::Offline { cache: true }) => t!("help.connection.offline_cache"),
        Some(Connection::Offline { cache: false }) => t!("help.connection.offline"),
        Some(Connection::SignedOut) => t!("help.connection.signed_out"),
    }
    .into_owned()
}

fn status_label(s: Option<UserStatus>) -> String {
    match s {
        Some(s) => crate::workspace::status_menu::status_label(s),
        None => t!("help.no_account_short").into_owned(),
    }
}

// --- outcome texts --------------------------------------------------------------

fn block_text(b: &ServerBlock) -> String {
    let s = match b {
        ServerBlock::RoomMuted => t!("help.block.room_muted").into_owned(),
        ServerBlock::GroupMentionsMuted => t!("help.block.group_muted").into_owned(),
        ServerBlock::Busy => t!("help.block.busy").into_owned(),
        ServerBlock::Offline => t!("help.block.offline").into_owned(),
        ServerBlock::Preference { pref, room: true } => t!(
            "help.block.room_pref",
            pref = desktop_pref_label(Some(*pref))
        )
        .into_owned(),
        ServerBlock::Preference { pref, room: false } => t!(
            "help.block.account_pref",
            pref = desktop_pref_label(Some(*pref))
        )
        .into_owned(),
        ServerBlock::ServerDefault(p) => {
            t!("help.block.server_default", pref = desktop_pref_label(*p)).into_owned()
        }
        ServerBlock::LargeRoom { limit } => t!("help.block.large_room", n = limit).into_owned(),
    };
    with_code(s, model::server_block_code(b))
}

fn paused_text(until: Option<i64>, cause: Code, d: &Data<'_>) -> String {
    let s = match until {
        Some(at) => t!("help.block.paused_until", time = moment(at, d)).into_owned(),
        None => t!("help.block.paused").into_owned(),
    };
    with_code(s, Some(cause))
}

fn blocked_text(b: &ToastBlocked, d: &Data<'_>) -> String {
    match b {
        ToastBlocked::NoAccount => t!("help.block.no_account").into_owned(),
        ToastBlocked::Server(s) => t!("help.block.server", reason = block_text(s)).into_owned(),
        ToastBlocked::NotConnected(_) => {
            with_code(t!("help.block.not_connected").into_owned(), Some(Code::Q5))
        }
        ToastBlocked::Disabled => t!("help.block.disabled").into_owned(),
        ToastBlocked::Paused { until, cause } => paused_text(*until, *cause, d),
        ToastBlocked::Busy => with_code(t!("help.block.busy").into_owned(), Some(Code::Q3)),
        ToastBlocked::RemindersOff => t!("help.block.reminders_off").into_owned(),
    }
}

fn toast_text(code: Code, toast: &Toast, d: &Data<'_>) -> String {
    match toast {
        Toast::Shown {
            preview,
            sticky,
            unfocused_only,
        } => {
            let mut parts = vec![shown_text(code, *preview)];
            if *sticky {
                parts.push(t!("help.toast.sticky").into_owned());
            }
            if *unfocused_only {
                parts.push(t!("help.toast.unfocused").into_owned());
            }
            parts.join(" · ")
        }
        Toast::Blocked(v) if v.is_empty() => t!("help.toast.not_shown").into_owned(),
        Toast::Blocked(v) => {
            let reasons: Vec<String> = v.iter().map(|b| blocked_text(b, d)).collect();
            t!("help.toast.blocked", reasons = reasons.join("; ")).into_owned()
        }
    }
}

/// What a shown notification of `code` contains (the texts the app sends).
fn shown_text(code: Code, preview: bool) -> String {
    let shown_as = |title: String, body: String| {
        t!(
            "help.toast.shown_as",
            title = inner_quotes(&title),
            body = inner_quotes(&body)
        )
        .into_owned()
    };
    match code {
        Code::L1 => {
            let title = t!("desktop.reminder_title", room = SAMPLE_ROOM).into_owned();
            let s = if preview {
                t!(
                    "help.toast.reminder_text",
                    title = inner_quotes(&title),
                    hidden = inner_quotes(&t!("desktop.reminder_body_hidden"))
                )
                .into_owned()
            } else {
                shown_as(title, t!("desktop.reminder_body_hidden").into_owned())
            };
            let buttons = t!(
                "help.toast.buttons",
                first = t!("desktop.reminder_snooze"),
                second = t!("desktop.reminder_done")
            );
            format!("{s} · {buttons}")
        }
        Code::L2 => shown_as(
            t!("desktop.reminders_missed_title_many", count = "N").into_owned(),
            t!("desktop.reminders_missed_body").into_owned(),
        ),
        Code::C1 => {
            let s = shown_as(
                t!("videoconf.incoming_from", name = "bob").into_owned(),
                t!("videoconf.incoming_body").into_owned(),
            );
            let button = t!("help.toast.button", name = t!("videoconf.accept"));
            format!("{s} · {button}")
        }
        _ if preview => t!("help.toast.shown_text").into_owned(),
        _ => t!(
            "help.toast.shown_hidden",
            text = inner_quotes(&t!("desktop.hidden_body"))
        )
        .into_owned(),
    }
}

/// A text quoted in «…» on the page: in Russian its own «…» become „…“
/// (outer «», inner „“).
fn inner_quotes(s: &str) -> String {
    if &*rust_i18n::locale() == "ru" {
        s.replace('«', "„").replace('»', "“")
    } else {
        s.to_owned()
    }
}

fn sound_off_text(o: &SoundOff, d: &Data<'_>) -> String {
    match o {
        SoundOff::NoToast => t!("help.sound_off.no_toast").into_owned(),
        SoundOff::NotNotified => t!("help.sound_off.not_notified").into_owned(),
        SoundOff::Setting => t!("help.sound_off.setting").into_owned(),
        SoundOff::Room => t!("help.sound_off.room").into_owned(),
        SoundOff::Account => t!("help.sound_off.account").into_owned(),
        SoundOff::AccountNewRoom => t!("help.sound_off.account_room").into_owned(),
        SoundOff::Volume => t!("help.sound_off.volume").into_owned(),
        SoundOff::Paused { until, cause } => paused_text(*until, *cause, d),
        SoundOff::Busy => with_code(t!("help.block.busy").into_owned(), Some(Code::Q3)),
        SoundOff::ReminderSoundOff => t!("help.sound_off.reminder").into_owned(),
    }
}

fn sound_text(s: &Sound, d: &Data<'_>) -> String {
    match s {
        Sound::Plays(SoundKind::Message(name)) => {
            t!("help.sound.message", name = account_sound_label(Some(name))).into_owned()
        }
        Sound::Plays(SoundKind::Reminder(name)) => t!(
            "help.sound.reminder",
            name = account_sound_label(Some(name))
        )
        .into_owned(),
        Sound::Plays(SoundKind::Ring) => t!("help.sound.ring").into_owned(),
        Sound::Plays(SoundKind::WindowsNotification) => t!("help.sound.windows").into_owned(),
        Sound::Plays(SoundKind::NewChat(name)) => t!(
            "help.sound.new_chat",
            name = account_sound_label(Some(name))
        )
        .into_owned(),
        Sound::Silent(o) => t!("help.sound.silent", reason = sound_off_text(o, d)).into_owned(),
    }
}

fn row_text(r: &RowOutcome) -> String {
    let mut parts = vec![badge_text(r.kind.map(|k| (u64::from(r.count), k)))];
    parts.push(if r.bold {
        t!("help.row.bold").into_owned()
    } else {
        t!("help.row.regular").into_owned()
    });
    if r.muted {
        parts.push(t!("help.row.bell_off").into_owned());
    }
    parts.join(" · ")
}

fn thread_text(t: &ThreadRowOutcome) -> String {
    let mut s = match t.listing {
        ThreadListing::Threads { limit } => {
            let mut s = format!(
                "{} · {}",
                t!("help.thread.listed"),
                badge_text(t.kind.map(|k| (0, k)))
            );
            if limit > 0 {
                s = format!("{s} · {}", t!("help.thread.limit", n = limit));
            }
            s
        }
        ThreadListing::SectionOff => t!("help.thread.section_off").into_owned(),
        ThreadListing::NotUnread => t!("help.thread.not_unread").into_owned(),
        ThreadListing::ServerOff => t!("help.thread.server_off").into_owned(),
    };
    if t.starred_in_favorites {
        s = format!("{s} · {}", t!("help.thread.starred"));
    }
    if t.listing != ThreadListing::ServerOff {
        s = format!(
            "{s} · {}",
            t!(
                "help.thread.header_button",
                badge = badge_text(t.header_button.map(|k| (0, k)))
            )
        );
    }
    s
}

fn placement_text(p: &Placement) -> String {
    match p {
        Placement::Unread => t!("help.section.unread").into_owned(),
        Placement::Section { first: true, .. } => t!("help.section.first").into_owned(),
        Placement::Section { .. } => t!("help.section.stays").into_owned(),
    }
}

fn collapsed_text(k: Option<BadgeKind>) -> String {
    match k {
        Some(k) => t!("help.collapsed.dot", kind = kind_name(k)).into_owned(),
        None => t!("help.collapsed.none").into_owned(),
    }
}

fn pinned_text(p: Option<&PinnedOutcome>) -> String {
    let Some(p) = p else {
        return t!("help.pinned.off").into_owned();
    };
    let mut parts = Vec::new();
    let item = |label: String, b: Option<(u64, BadgeKind)>| format!("{label}: {}", badge_text(b));
    if p.later.is_none() && p.drafts.is_none() {
        parts.push(if p.in_all_unreads {
            item(t!("inbox.all_unreads").into_owned(), p.all_unreads)
        } else {
            format!(
                "{}: {}",
                t!("inbox.all_unreads"),
                t!("help.pinned.not_counted")
            )
        });
        parts.push(item(t!("inbox.activity").into_owned(), p.activity));
    }
    if p.later.is_some() {
        parts.push(item(t!("later.title").into_owned(), p.later));
    }
    if p.drafts.is_some() {
        parts.push(item(t!("scheduled.title").into_owned(), p.drafts));
    }
    parts.join(" · ")
}

fn icon_label(a: &AccountIcon) -> String {
    match a.place {
        IconPlace::TitleBar => t!("help.surface.icon_title_bar"),
        IconPlace::Rail => t!("help.surface.icon_rail"),
    }
    .into_owned()
}

fn window_text(w: &WindowOutcome) -> String {
    let title = if w.signal.any() {
        t!(
            "help.window.title",
            title = crate::app::window_title(w.signal)
        )
        .into_owned()
    } else {
        t!("help.window.title_same").into_owned()
    };
    let tray = match w.tray {
        None => t!("help.window.no_tray"),
        Some(TrayLevel::Mention) => t!("help.window.tray_dot"),
        Some(TrayLevel::GroupMention) => t!("help.window.tray_group"),
        Some(TrayLevel::Unread) => t!("help.window.tray_quiet"),
        Some(TrayLevel::None) => t!("help.window.tray_same"),
    };
    format!("{title} · {tray}")
}

fn feed_text(f: &Feed) -> String {
    let mut parts = vec![
        if f.highlighted {
            t!("help.feed.highlighted")
        } else {
            t!("help.feed.plain")
        }
        .into_owned(),
    ];
    match f.chip_mine {
        Some(true) => parts.push(t!("help.feed.chip_mine").into_owned()),
        Some(false) => parts.push(t!("help.feed.chip_other").into_owned()),
        None => {}
    }
    if f.word_marked {
        parts.push(t!("help.feed.word_marked").into_owned());
    }
    if f.divider {
        parts.push(t!("help.feed.divider").into_owned());
    }
    if f.thread_dot {
        parts.push(t!("help.feed.thread_dot").into_owned());
    }
    parts.join(" · ")
}

/// What the server stores and whether it notifies.
fn server_lines<'a>(s: &ServerOutcome) -> Vec<Element<'a, Message>> {
    let sub = &s.sub;
    let has = |v: &Option<Vec<String>>| v.as_ref().is_some_and(|v| !v.is_empty());
    let mut parts = Vec::new();
    if sub.unread > 0 {
        parts.push(t!("help.counter.unread", n = sub.unread).into_owned());
    }
    if sub.user_mentions > 0 {
        parts.push(t!("help.counter.mentions", n = sub.user_mentions).into_owned());
    }
    if sub.group_mentions > 0 {
        parts.push(t!("help.counter.group", n = sub.group_mentions).into_owned());
    }
    if has(&sub.tunread) {
        parts.push(t!("help.counter.thread").into_owned());
    }
    if has(&sub.tunread_user) {
        parts.push(t!("help.counter.thread_mention").into_owned());
    }
    if has(&sub.tunread_group) {
        parts.push(t!("help.counter.thread_group").into_owned());
    }
    if parts.is_empty() {
        parts.push(if sub.alert {
            t!("help.counter.alert_only").into_owned()
        } else {
            t!("help.counter.none").into_owned()
        });
    }
    let notify = match &s.desktop {
        Ok(()) => t!("help.server.notifies").into_owned(),
        Err(b) => t!("help.server.no_notification", reason = block_text(b)).into_owned(),
    };
    let mut lines = vec![
        line(t!("help.surface.server").into_owned(), parts.join(" · ")),
        line(t!("help.surface.server_toast").into_owned(), notify),
    ];
    if let Some(l) = &s.large_room {
        let value = match &l.desktop {
            Ok(()) => t!("help.server.notifies").into_owned(),
            Err(_) if l.fallback => t!("help.server.none_rustket_shows").into_owned(),
            Err(_) if l.highlights_would_help => t!("help.server.none_no_highlights").into_owned(),
            Err(b) => t!("help.server.no_notification", reason = block_text(b)).into_owned(),
        };
        lines.push(line(
            t!("help.surface.large_room", n = l.limit).into_owned(),
            value,
        ));
    }
    lines
}

fn note_text(n: Note) -> String {
    match n {
        Note::NoHighlightWords => t!("help.note.no_highlights"),
        Note::ThreadsDisabled => t!("help.note.threads_disabled"),
        Note::NoAccount => t!("help.note.no_account"),
    }
    .into_owned()
}

// --- samples --------------------------------------------------------------------

/// Samples sit on the sidebar's surface.
fn on_sidebar<'a>(content: Element<'a, Message>) -> Element<'a, Message> {
    container(content)
        .width(Length::Fixed(SAMPLE_WIDTH))
        .padding(SPACE_1)
        .style(style::container_sidebar)
        .into()
}

fn room_sample<'a>(code: Code, r: &RowOutcome) -> Element<'a, Message> {
    let title = if code == Code::M2 { "bob" } else { SAMPLE_ROOM };
    let mut p = r.props(title, Message::Noop);
    p.subtitle = Some(if code == Code::M2 {
        t!("help.sample.text").into_owned()
    } else {
        format!("bob: {}", t!("help.sample.text"))
    });
    p.time = Some("12:00".into());
    room_list_item(p)
}

fn thread_sample<'a>(t: &ThreadRowOutcome) -> Element<'a, Message> {
    let mut p = t.props(&t!("help.sample.thread"), Message::Noop);
    p.subtitle = Some(format!("bob: {}", t!("help.sample.text")));
    room_list_item(p)
}

fn account_sample<'a>(a: &AccountIcon) -> Element<'a, Message> {
    let src = AvatarSource::initials("Dev", "Dev");
    match a.place {
        IconPlace::TitleBar => container(title_bar_workspace(
            &src,
            "Dev",
            a.badge,
            false,
            false,
            None::<Message>,
        ))
        .padding(SPACE_2)
        .style(style::container_header)
        .into(),
        IconPlace::Rail => container(workspace_rail_item(
            RailIcon::Avatar(src),
            "Dev",
            a.badge,
            false,
            Message::Noop,
        ))
        .padding(SPACE_2)
        .style(style::container_rail)
        .into(),
    }
}

fn pinned_sample<'a>(p: &PinnedOutcome) -> Element<'a, Message> {
    use crate::workspace::sidebar::pinned_row;
    let mut rows: Vec<Element<'a, Message>> = Vec::new();
    let mut add = |i: Icon, label: String, b: Option<(u64, BadgeKind)>| {
        rows.push(pinned_row(i, label, None, b, false, Message::Noop));
    };
    if p.later.is_none() && p.drafts.is_none() {
        add(
            Icon::Inbox,
            t!("inbox.all_unreads").into_owned(),
            p.all_unreads,
        );
        add(Icon::AtSign, t!("inbox.activity").into_owned(), p.activity);
    }
    if p.later.is_some() {
        add(Icon::BookmarkCheck, t!("later.title").into_owned(), p.later);
    }
    if p.drafts.is_some() {
        add(
            Icon::NotebookPen,
            t!("scheduled.title").into_owned(),
            p.drafts,
        );
    }
    on_sidebar(Column::with_children(rows).into())
}

fn feed_sample<'a>(code: Code, f: &Feed, username: &str, word: &str) -> Element<'a, Message> {
    let mut r = row![text("bob").size(TEXT_SMALL).font(font_semibold())]
        .spacing(SPACE_2)
        .align_y(Alignment::Center);
    if let Some(mine) = f.chip_mine {
        let name = if matches!(code, Code::M3 | Code::T3) {
            "all"
        } else {
            username
        };
        r = r.push(
            container(
                text(format!("@{name}"))
                    .size(TEXT_SMALL)
                    .font(font_semibold()),
            )
            .padding([0, 2])
            .style(style::container_mention(mine)),
        );
    }
    r = r.push(text(t!("help.sample.text").into_owned()).size(TEXT_SMALL));
    if f.word_marked {
        r = r.push(
            container(text(word.to_owned()).size(TEXT_SMALL).font(font_semibold()))
                .style(style::container_mention(true)),
        );
    }
    let msg = container(r)
        .padding([SPACE_1, SPACE_3])
        .width(Length::Fill)
        .style(style::container_message(f.highlighted, false));
    let mut col = Column::new();
    if f.divider {
        col = col.push(unread_separator(t!("room_timeline.unread").into_owned()));
    }
    container(col.push(msg))
        .width(Length::Fixed(SAMPLE_WIDTH))
        .style(style::container_room)
        .into()
}

// --- cards ----------------------------------------------------------------------

fn header<'a>(d: &Data<'_>) -> Element<'a, Message> {
    let account = match &d.env.account {
        Some(a) => text(t!("help.computed_for", account = a.label.clone()).into_owned())
            .size(TEXT_BODY)
            .font(font_semibold()),
        None => text(t!("help.no_account").into_owned())
            .size(TEXT_BODY)
            .style(style::text_danger),
    };
    section(
        t!("help.title").into_owned(),
        vec![
            text(t!("help.intro").into_owned()).size(TEXT_BODY).into(),
            account.into(),
        ],
    )
}

/// One input: label, value and a button to the tab where it is changed.
fn setting<'a>(label: String, value: String, tab: Option<Tab>) -> Element<'a, Message> {
    match tab {
        Some(tab) => action_row(
            label,
            caption(value).into(),
            t!("help.open").into_owned(),
            Some(Message::Tab(tab)),
        ),
        None => column![text(label).size(TEXT_BODY), caption(value)]
            .spacing(SPACE_1)
            .into(),
    }
}

fn global_text(g: Global, d: &Data<'_>) -> String {
    match g {
        Global::Pause { until: Some(at) } => {
            t!("help.q.paused_until", time = moment(at, d)).into_owned()
        }
        Global::Pause { until: None } => t!("help.q.not_paused").into_owned(),
        Global::WorkingHours(w) => match w {
            WorkingHoursState::Off => t!("help.q.wh_off").into_owned(),
            WorkingHoursState::Inside => t!("help.q.wh_inside").into_owned(),
            WorkingHoursState::Outside { until: Some(at) } => {
                t!("help.q.wh_outside_until", time = moment(at, d)).into_owned()
            }
            WorkingHoursState::Outside { until: None } => t!("help.q.wh_outside").into_owned(),
            WorkingHoursState::Resumed { until: Some(at) } => {
                t!("help.q.wh_resumed_until", time = moment(at, d)).into_owned()
            }
            WorkingHoursState::Resumed { until: None } => t!("help.q.wh_resumed").into_owned(),
        },
        Global::Status(s) => status_label(s),
        Global::OnScreen => {
            let label = t!("account.pref_mute_focused");
            match &d.env.account {
                Some(a) if a.user_id != PLACEHOLDER_USER_ID && a.prefs.mute_focused => {
                    t!("help.q.on_screen_muted", label = label).into_owned()
                }
                Some(a) if a.user_id != PLACEHOLDER_USER_ID => {
                    t!("help.q.on_screen_notifies", label = label).into_owned()
                }
                _ => t!("help.q.on_screen", label = label).into_owned(),
            }
        }
        Global::Connection(c) => connection_label(c),
    }
}

fn global_value(code: Code, d: &Data<'_>) -> String {
    model::global(code, d.cfg, d.env)
        .map(|g| global_text(g, d))
        .unwrap_or_default()
}

fn current_settings<'a>(d: &Data<'_>) -> Element<'a, Message> {
    let ui = &d.cfg.ui;
    let code_label = |c: Code| format!("{} · {}", c.code(), t!(c.title_key()));
    let mut rows = vec![
        setting(
            t!("settings.notifications_enable").into_owned(),
            on_off(ui.desktop_notifications),
            Some(Tab::Notifications),
        ),
        setting(
            t!("settings.notifications_preview").into_owned(),
            on_off(ui.notification_preview),
            Some(Tab::Notifications),
        ),
        setting(
            t!("settings.notifications_sound").into_owned(),
            on_off(ui.sound),
            Some(Tab::Notifications),
        ),
        setting(code_label(Code::Q1), global_value(Code::Q1, d), None),
        setting(
            code_label(Code::Q2),
            global_value(Code::Q2, d),
            Some(Tab::Notifications),
        ),
        setting(code_label(Code::Q3), global_value(Code::Q3, d), None),
    ];
    match &d.env.account {
        Some(a) => {
            let server = a.server;
            let in_account = t!("help.change_in_account").into_owned();
            // `/me` merges the server default in; a value the user never
            // set is not on the subscriptions and works as the default.
            let desktop = match a.prefs.desktop {
                Some(p) if a.desktop_stored => desktop_pref_label(Some(p)),
                Some(p) => t!(
                    "help.server_default_suffix",
                    value = desktop_pref_label(Some(p))
                )
                .into_owned(),
                None => t!(
                    "help.server_default_value",
                    value = desktop_pref_label(server.default_desktop)
                )
                .into_owned(),
            };
            let mine = |key: &str| t!("help.account_pref", pref = t!(key)).into_owned();
            rows.push(setting(
                mine("account.pref_desktop"),
                format!("{desktop} · {in_account}"),
                None,
            ));
            rows.push(setting(
                mine("account.pref_sound_message"),
                format!(
                    "{} · {in_account}",
                    account_sound_label(a.prefs.sound.new_message.as_deref())
                ),
                None,
            ));
            rows.push(setting(
                mine("account.pref_sound_room"),
                format!(
                    "{} · {in_account}",
                    account_sound_label(a.prefs.sound.new_room.as_deref())
                ),
                None,
            ));
            let words = if a.prefs.highlights.is_empty() {
                t!("help.none").into_owned()
            } else {
                a.prefs.highlights.join(", ")
            };
            rows.push(setting(
                mine("account.pref_highlights"),
                format!("{words} · {in_account}"),
                None,
            ));
            rows.push(setting(
                t!("help.server_counting").into_owned(),
                t!(
                    "help.counting_value",
                    channels = count_label(server.unread_count),
                    dm = count_label(server.unread_count_dm)
                )
                .into_owned(),
                None,
            ));
        }
        None => rows.push(setting(
            t!("help.server_side").into_owned(),
            t!("help.sign_in_to_see").into_owned(),
            None,
        )),
    }
    let mode = match d.mode {
        Mode::Light => t!("settings.theme_light"),
        Mode::Dark => t!("settings.theme_dark"),
    };
    let theme = if ui.theme == ThemePref::System {
        format!(
            "{} ({mode}) · {}",
            t!("settings.theme_system"),
            d.theme_name
        )
    } else {
        format!("{mode} · {}", d.theme_name)
    };
    rows.extend([
        setting(
            t!("settings.threads_mode").into_owned(),
            threads_mode_label(ui.sidebar_threads),
            Some(Tab::Sidebar),
        ),
        setting(
            t!("settings.unread_on_top").into_owned(),
            on_off(ui.unread_on_top),
            Some(Tab::Sidebar),
        ),
        setting(
            t!("settings.pinned_views").into_owned(),
            on_off(ui.sidebar_pinned_views),
            Some(Tab::Sidebar),
        ),
        setting(
            t!("settings.theme").into_owned(),
            theme,
            Some(Tab::Appearance),
        ),
        setting(
            t!("help.tray").into_owned(),
            if d.env.tray {
                t!("help.tray_yes")
            } else {
                t!("help.tray_no")
            }
            .into_owned(),
            None,
        ),
    ]);
    section(t!("help.current_title").into_owned(), rows)
}

fn legend<'a>(d: &Data<'_>) -> Element<'a, Message> {
    let tokens = rustket_ui::theme::active_tokens(d.mode);
    let mut rows: Vec<Element<'a, Message>> = model::legend(d.cfg, d.env)
        .iter()
        .map(|(k, uses)| {
            let hex = rustket_ui::theme_file::color_hex(k.colors(tokens).0);
            row![
                row![badge(3, *k), dot(*k, 8.0)]
                    .spacing(SPACE_2)
                    .align_y(Alignment::Center)
                    .width(Length::Fixed(64.0)),
                column![
                    text(kind_name(*k)).size(TEXT_BODY).font(font_semibold()),
                    text(format!("{} {hex}", kind_token(*k)))
                        .size(TEXT_CAPTION)
                        .font(font_mono())
                        .style(style::text_secondary),
                    caption(kind_meaning(*k)),
                    uses_by_surface(uses, d),
                ]
                .spacing(SPACE_1)
                .width(Length::Fill),
            ]
            .spacing(SPACE_3)
            .into()
        })
        .collect();
    rows.insert(0, caption(t!("help.legend_caption").into_owned()).into());
    section(t!("help.legend_title").into_owned(), rows)
}

fn surface_name(s: Surface, d: &Data<'_>) -> String {
    match s {
        Surface::Row => t!("help.surface.row"),
        Surface::ThreadRow => t!("help.surface.thread_row"),
        Surface::ThreadsButton => t!("help.surface.threads_button"),
        Surface::AccountIcon if d.env.custom_chrome => t!("help.surface.icon_title_bar"),
        Surface::AccountIcon => t!("help.surface.icon_rail"),
        Surface::AllUnreads => t!("inbox.all_unreads"),
        Surface::Activity => t!("inbox.activity"),
        Surface::Later => t!("later.title"),
        Surface::Drafts => t!("scheduled.title"),
    }
    .into_owned()
}

/// Where each code draws a colour: a line per surface, with the codes
/// ("M5 · dot": a dot, not a counter).
fn uses_by_surface<'a>(uses: &[KindUse], d: &Data<'_>) -> Element<'a, Message> {
    let mut surfaces: Vec<Surface> = Vec::new();
    for u in uses {
        if !surfaces.contains(&u.surface) {
            surfaces.push(u.surface);
        }
    }
    let lines = surfaces.into_iter().map(|s| {
        let chips = uses.iter().filter(|u| u.surface == s).map(|u| {
            let label = if u.dot {
                format!("{} · {}", u.code.code(), t!("help.legend.dot"))
            } else {
                u.code.code().to_owned()
            };
            container(text(label).size(TEXT_CAPTION).font(font_mono()))
                .padding([1, 6])
                .style(style::container_inline_code)
                .into()
        });
        row![
            text(surface_name(s, d))
                .size(TEXT_CAPTION)
                .style(style::text_secondary)
                .width(Length::Fixed(USE_LABEL_WIDTH)),
            Row::with_children(chips).spacing(SPACE_1).wrap(),
        ]
        .spacing(SPACE_2)
        .into()
    });
    Column::with_children(lines).spacing(SPACE_1).into()
}

fn family_card<'a>(f: Family, d: &Data<'_>) -> Element<'a, Message> {
    let rows = f.codes().map(|c| code_block(c, d)).collect();
    section(t!(f.title_key()).into_owned(), rows)
}

/// The trigger line of `code` (L1 names the sound and Q1 what else
/// silences the sounds on the platform).
fn trigger_text(code: Code, d: &Data<'_>) -> String {
    match code {
        Code::L1 => {
            let reminder = crate::desktop::sound::RcSound {
                name: "chime".into(),
                volume: 100,
            };
            let routed = model::routed_sound(d.env.platform, true, reminder, SoundKind::Reminder);
            let sound = if routed == SoundKind::WindowsNotification {
                t!("help.code.l1.sound_windows")
            } else {
                t!("help.code.l1.sound")
            };
            t!(code.trigger_key(), sound = sound).into_owned()
        }
        Code::Q1 => t!(code.trigger_key(), dnd = d.env.platform.dnd_text()).into_owned(),
        _ => t!(code.trigger_key()).into_owned(),
    }
}

/// One code: heading, trigger, live samples and a line per surface.
fn code_block<'a>(code: Code, d: &Data<'_>) -> Element<'a, Message> {
    let o = model::evaluate(code, d.cfg, d.env);
    let mut col = column![heading(code), caption(trigger_text(code, d))]
        .spacing(SPACE_2)
        .width(Length::Fill);
    if let Some(n) = o.note {
        col = col.push(
            text(note_text(n))
                .size(TEXT_CAPTION)
                .style(style::text_danger),
        );
    }
    let samples = samples(&o, d);
    if !samples.is_empty() {
        col = col.push(
            Row::with_children(samples)
                .spacing(SPACE_4)
                .wrap()
                .vertical_spacing(SPACE_2),
        );
    }
    col.extend(surface_lines(&o, d)).into()
}

/// The live samples, inert (drawn by the app's widgets, never pressed).
fn samples<'a>(o: &Outcome, d: &Data<'_>) -> Vec<Element<'a, Message>> {
    let mut v = Vec::new();
    if let Some(r) = &o.row {
        let mut col = column![room_sample(o.code, r)];
        if let Some(t) = &o.thread_row
            && matches!(t.listing, ThreadListing::Threads { .. })
        {
            col = col.push(thread_sample(t));
        }
        v.push(on_sidebar(col.into()));
    }
    if let Some(p) = &o.pinned {
        v.push(pinned_sample(p));
    }
    if let Some(a) = &o.account_icon {
        v.push(account_sample(a));
    }
    if let Some(f) = &o.feed {
        let username = match &d.env.account {
            Some(a) if a.user_id != PLACEHOLDER_USER_ID => a.username.clone(),
            _ => t!("help.sample.me").into_owned(),
        };
        let word = d
            .env
            .account
            .as_ref()
            .and_then(|a| a.prefs.highlights.first().cloned())
            .unwrap_or_else(|| t!("help.sample.word").into_owned());
        v.push(feed_sample(o.code, f, &username, &word));
    }
    v.into_iter().map(|e| inert(e).into()).collect()
}

fn surface_lines<'a>(o: &Outcome, d: &Data<'_>) -> Vec<Element<'a, Message>> {
    let mut v = Vec::new();
    let mut add = |key: String, value: String| v.push(line(key, value));
    if let Some(r) = &o.row {
        add(t!("help.surface.row").into_owned(), row_text(r));
    }
    if let Some(t) = &o.thread_row {
        add(t!("help.surface.thread_row").into_owned(), thread_text(t));
    }
    if let Some(p) = &o.section {
        add(t!("help.surface.section").into_owned(), placement_text(p));
    }
    if o.row.is_some() {
        let mut s = collapsed_text(o.collapsed);
        if o.thread_row
            .as_ref()
            .is_some_and(|t| matches!(t.listing, ThreadListing::Threads { .. }))
        {
            s = format!(
                "{s} · {}: {}",
                t!("settings.threads"),
                collapsed_text(o.collapsed_threads)
            );
        }
        add(t!("help.surface.collapsed").into_owned(), s);
    }
    if o.row.is_some() || o.pinned.is_some() {
        add(
            t!("help.surface.pinned").into_owned(),
            pinned_text(o.pinned.as_ref()),
        );
    }
    if let Some(a) = &o.account_icon {
        add(
            icon_label(a),
            badge_text(a.badge.map(|(n, k)| (u64::from(n), k))),
        );
    }
    if let Some(w) = &o.window {
        add(t!("help.surface.window").into_owned(), window_text(w));
    }
    if o.popup {
        add(
            t!("help.surface.in_app").into_owned(),
            t!("help.call_card").into_owned(),
        );
    }
    if o.dialog {
        add(
            t!("help.surface.in_app").into_owned(),
            t!("help.scheduled_dialog").into_owned(),
        );
        if let Some(p) = &o.pinned {
            add(
                t!("help.surface.scheduled", row = t!("scheduled.title")).into_owned(),
                badge_text(p.drafts_on_time),
            );
        }
    }
    if let Some(t) = &o.toast {
        let mut s = toast_text(o.code, t, d);
        if o.once_per_run {
            s = format!("{s} · {}", t!("help.once_per_run"));
        }
        add(t!("help.surface.toast").into_owned(), s);
    }
    if let Some(s) = &o.sound {
        add(t!("help.surface.sound").into_owned(), sound_text(s, d));
    }
    if let Some(f) = &o.feed {
        add(t!("help.surface.feed").into_owned(), feed_text(f));
    }
    if let Some(s) = &o.server {
        v.extend(server_lines(s));
    }
    v
}

fn usage_text(code: Code, d: &Data<'_>) -> Option<String> {
    let u = d.env.account.as_ref()?.usage?;
    let n = match code {
        Code::R1 => u.notifications_off,
        Code::R2 => u.counter_hidden,
        Code::R3 => u.mentions_hidden,
        Code::R4 => u.group_mentions_muted,
        Code::R5 => u.alert_all + u.alert_mentions + u.alert_nothing,
        _ => return None,
    };
    let n = n as u64;
    Some(
        t!(
            &format!("help.usage_{}", crate::room::render::plural(n)),
            n = n
        )
        .into_owned(),
    )
}

fn matrix_cell<'a>(c: &Cell) -> Element<'a, Message> {
    let bell = if c.server_toast {
        Icon::Bell
    } else {
        Icon::BellOff
    };
    let b = c.row.kind.map(|k| (u64::from(c.row.count), k));
    row![
        badge_sample(b),
        icon::sized(bell, 12.0).style(style::text_hint)
    ]
    .spacing(SPACE_1)
    .align_y(Alignment::Center)
    .width(Length::FillPortion(1))
    .into()
}

fn room_card<'a>(d: &Data<'_>) -> Element<'a, Message> {
    let mut rows: Vec<Element<'a, Message>> = Vec::new();
    for code in Family::Room.codes() {
        let mut col = column![heading(code), caption(trigger_text(code, d))].spacing(SPACE_1);
        if let Some(u) = usage_text(code, d) {
            col = col.push(caption(u));
        }
        rows.push(col.into());
    }
    // The matrix: a row per message code, a column per setting.
    let head = |s: String| -> Element<'a, Message> {
        text(s)
            .size(TEXT_CAPTION)
            .font(font_semibold())
            .width(Length::FillPortion(1))
            .into()
    };
    // Flexible columns: the matrix fits the narrowest settings page.
    let mut header = row![space().width(Length::Fixed(CODE_WIDTH))].spacing(SPACE_2);
    for m in model::MATRIX_MODS {
        header = header.push(head(match m {
            Some(c) => c.code().to_owned(),
            None => t!("help.matrix.as_is").into_owned(),
        }));
    }
    let mut grid = column![header].spacing(SPACE_2);
    for (code, cells) in model::matrix(d.cfg, d.env) {
        let mut r = row![container(chip(code)).width(Length::Fixed(CODE_WIDTH))]
            .spacing(SPACE_2)
            .align_y(Alignment::Center);
        for c in &cells {
            r = r.push(matrix_cell(c));
        }
        grid = grid.push(r);
    }
    rows.push(
        column![
            caption(t!("help.matrix.caption").into_owned()),
            on_matrix(grid.into())
        ]
        .spacing(SPACE_2)
        .into(),
    );
    // R5: which codes the server notifies for, per room alert.
    let mut alerts = column![
        text(t!("help.r5.title").into_owned())
            .size(TEXT_BODY)
            .font(font_semibold())
    ]
    .spacing(SPACE_1);
    for (pref, codes) in model::alert_table(d.cfg, d.env) {
        let on: Vec<&str> = codes
            .iter()
            .filter(|(_, b)| *b)
            .map(|(c, _)| c.code())
            .collect();
        let value = if on.is_empty() {
            t!("help.r5.none").into_owned()
        } else {
            t!("help.r5.codes", codes = on.join(", ")).into_owned()
        };
        alerts = alerts.push(line(desktop_pref_label(Some(pref)), value));
    }
    rows.push(alerts.into());
    section(t!(Family::Room.title_key()).into_owned(), rows)
}

fn on_matrix<'a>(grid: Element<'a, Message>) -> Element<'a, Message> {
    container(grid)
        .width(Length::Fill)
        .padding(SPACE_2)
        .style(style::container_sidebar)
        .into()
}

fn global_card<'a>(d: &Data<'_>) -> Element<'a, Message> {
    let rows = Family::Global
        .codes()
        .map(|code| {
            column![
                heading(code),
                line(t!("help.q.now").into_owned(), global_value(code, d)),
                caption(trigger_text(code, d)),
            ]
            .spacing(SPACE_1)
            .into()
        })
        .collect();
    section(t!(Family::Global.title_key()).into_owned(), rows)
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
#[path = "help_tests.rs"]
mod tests;
