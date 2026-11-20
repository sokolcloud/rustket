//! The "Indicators and notifications" reference: a stable code for every
//! kind of signal and, for each, what every surface shows under the current
//! settings. Outcomes come from the functions the app itself uses, run on a
//! synthetic subscription that the server model ([`super::server`]) fills in;
//! only the server's rules are modelled. Pure: no iced, no strings (the view
//! translates the values).

use rocketchat::RcDate;
use rocketchat::models::{
    Mention, Message as RcMessage, Room, RoomType, Setting, Subscription, User, UserRef, UserStatus,
};
use rustket_core::config::{AppConfig, Density};
use rustket_core::notify::{
    IncomingNotification, NotificationMessage, NotificationPayload, NotificationPrefs,
    NotifyContext,
};
use rustket_core::quiet::{Quiet, UNTIL_BOUNDARY};
use rustket_core::room_list::{
    self, Badge, BadgeVariant, RoomKind, RoomListOptions, SectionKind, WorkspaceSignal,
};
use rustket_core::sidebar_layout::{SectionId, ThreadsMode, section_unread_first};
use rustket_core::workspace::{Change, ConnectionState, RoomEntry, Workspace};
use rustket_ui::icon::Icon;
use rustket_ui::widget::{AvatarSource, BadgeKind, RoomListItemProps};

use super::server::{self, DesktopPref, Incoming, Receiver, ServerBlock, ServerFacts, Trigger};
use crate::desktop::AccountSound;
use crate::desktop::notifications::{MessageSoundOff, message_sound, message_sound_off};
use crate::desktop::sound::{Platform, RcSound};
use crate::desktop::tray::TrayLevel;
use crate::workspace::{rail, sidebar};

/// A family of codes (one card of the page).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Family {
    /// M: messages in a room's main timeline.
    Messages,
    /// T: thread replies.
    Threads,
    /// L, D: reminders and scheduled messages.
    Later,
    /// C, O: calls and omnichannel.
    Calls,
    /// R: per-room settings.
    Room,
    /// Q: global states.
    Global,
}

impl Family {
    pub const ALL: [Family; 6] = [
        Family::Messages,
        Family::Threads,
        Family::Later,
        Family::Calls,
        Family::Room,
        Family::Global,
    ];

    /// i18n key of the family's card title.
    pub fn title_key(self) -> &'static str {
        match self {
            Family::Messages => "help.family.messages",
            Family::Threads => "help.family.threads",
            Family::Later => "help.family.later",
            Family::Calls => "help.family.calls",
            Family::Room => "help.family.room",
            Family::Global => "help.family.global",
        }
    }

    pub fn codes(self) -> impl Iterator<Item = Code> {
        Code::ALL
            .iter()
            .copied()
            .filter(move |c| c.family() == self)
    }
}

macro_rules! codes {
    ($($v:ident, $fam:ident, $title:literal, $trigger:literal;)*) => {
        /// A signal's stable code (never renumbered).
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub enum Code {
            $($v,)*
        }

        impl Code {
            pub const ALL: &'static [Code] = &[$(Code::$v,)*];

            /// "M1", "T2", …
            pub fn code(self) -> &'static str {
                match self {
                    $(Code::$v => stringify!($v),)*
                }
            }

            pub fn family(self) -> Family {
                match self {
                    $(Code::$v => Family::$fam,)*
                }
            }

            /// i18n key of the code's title.
            pub fn title_key(self) -> &'static str {
                match self {
                    $(Code::$v => $title,)*
                }
            }

            /// i18n key of the one-line trigger.
            pub fn trigger_key(self) -> &'static str {
                match self {
                    $(Code::$v => $trigger,)*
                }
            }
        }
    };
}

codes! {
    M1, Messages, "help.code.m1.title", "help.code.m1.trigger";
    M2, Messages, "help.code.m2.title", "help.code.m2.trigger";
    M3, Messages, "help.code.m3.title", "help.code.m3.trigger";
    M4, Messages, "help.code.m4.title", "help.code.m4.trigger";
    M5, Messages, "help.code.m5.title", "help.code.m5.trigger";
    T1, Threads, "help.code.t1.title", "help.code.t1.trigger";
    T2, Threads, "help.code.t2.title", "help.code.t2.trigger";
    T3, Threads, "help.code.t3.title", "help.code.t3.trigger";
    L1, Later, "help.code.l1.title", "help.code.l1.trigger";
    L2, Later, "help.code.l2.title", "help.code.l2.trigger";
    D1, Later, "help.code.d1.title", "help.code.d1.trigger";
    C1, Calls, "help.code.c1.title", "help.code.c1.trigger";
    O1, Calls, "help.code.o1.title", "help.code.o1.trigger";
    R1, Room, "help.code.r1.title", "help.code.r1.trigger";
    R2, Room, "help.code.r2.title", "help.code.r2.trigger";
    R3, Room, "help.code.r3.title", "help.code.r3.trigger";
    R4, Room, "help.code.r4.title", "help.code.r4.trigger";
    R5, Room, "help.code.r5.title", "help.code.r5.trigger";
    Q1, Global, "help.code.q1.title", "help.code.q1.trigger";
    Q2, Global, "help.code.q2.title", "help.code.q2.trigger";
    Q3, Global, "help.code.q3.title", "help.code.q3.trigger";
    Q4, Global, "help.code.q4.title", "help.code.q4.trigger";
    Q5, Global, "help.code.q5.title", "help.code.q5.trigger";
}

impl Code {
    /// The message scenario of an M/T code.
    pub fn scenario(self) -> Option<Incoming> {
        let (room, thread, trigger) = match self {
            Code::M1 => (RoomType::Channel, false, Trigger::MentionMe),
            Code::M2 => (RoomType::Direct, false, Trigger::Plain),
            Code::M3 => (RoomType::Channel, false, Trigger::MentionAll),
            Code::M4 => (RoomType::Channel, false, Trigger::Highlight),
            Code::M5 => (RoomType::Channel, false, Trigger::Plain),
            Code::T1 => (RoomType::Channel, true, Trigger::Plain),
            Code::T2 => (RoomType::Channel, true, Trigger::MentionMe),
            Code::T3 => (RoomType::Channel, true, Trigger::MentionAll),
            _ => return None,
        };
        Some(Incoming {
            room,
            thread,
            trigger,
        })
    }
}

/// The active account's connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    Connected,
    Connecting,
    /// Not connected; `cache`: showing the offline cache.
    Offline {
        cache: bool,
    },
    /// The token was rejected or the account signed out.
    SignedOut,
}

impl Connection {
    pub fn of(state: &ConnectionState, cache: bool) -> Self {
        match state {
            ConnectionState::Connected => Connection::Connected,
            ConnectionState::Connecting => Connection::Connecting,
            ConnectionState::LoggedOut => Connection::SignedOut,
            ConnectionState::Disconnected | ConnectionState::Reconnecting { .. } => {
                Connection::Offline { cache }
            }
        }
    }
}

/// The account's server-side preferences (`/me` settings.preferences,
/// merged with the server defaults).
#[derive(Debug, Clone, PartialEq)]
pub struct AccountPrefs {
    /// `desktopNotifications`.
    pub desktop: Option<DesktopPref>,
    pub sound: AccountSound,
    /// `highlights`.
    pub highlights: Vec<String>,
    /// `desktopNotificationRequireInteraction`.
    pub require_interaction: bool,
    /// `muteFocusedConversations` (Q4): the conversation on screen does
    /// not notify.
    pub mute_focused: bool,
}

impl Default for AccountPrefs {
    /// Rocket.Chat's defaults (`/me` of a user who never changed them).
    fn default() -> Self {
        Self {
            desktop: Some(DesktopPref::All),
            sound: AccountSound {
                new_message: Some("chime".into()),
                new_room: Some("door".into()),
                notifications_volume: Some(100.0),
                master_volume: Some(100.0),
            },
            highlights: Vec::new(),
            require_interaction: false,
            mute_focused: true,
        }
    }
}

impl AccountPrefs {
    pub fn of(ws: &Workspace) -> Self {
        let pref = |k: &str| crate::desktop::user_pref(ws, k);
        Self {
            desktop: pref("desktopNotifications")
                .and_then(|v| v.as_str())
                .and_then(DesktopPref::parse),
            sound: AccountSound::of(ws),
            highlights: crate::room::render::highlight_words(ws),
            require_interaction: crate::desktop::notifications::require_interaction_pref(ws),
            mute_focused: crate::desktop::notifications::mute_focused_pref(ws),
        }
    }
}

/// How many rooms of the account have each per-room setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RoomUsage {
    /// R1.
    pub notifications_off: usize,
    /// R2.
    pub counter_hidden: usize,
    /// R3.
    pub mentions_hidden: usize,
    /// R4.
    pub group_mentions_muted: usize,
    /// R5: rooms with their own desktop alert all / mentions / nothing.
    pub alert_all: usize,
    pub alert_mentions: usize,
    pub alert_nothing: usize,
}

impl RoomUsage {
    /// Over the open rooms of `ws`.
    pub fn of(ws: &Workspace) -> Self {
        let mut u = Self::default();
        for s in ws
            .rooms()
            .values()
            .filter_map(|e| e.sub.as_ref())
            .filter(|s| s.open)
        {
            let on = |v: Option<bool>| usize::from(v == Some(true));
            u.notifications_off += on(s.disable_notifications);
            u.counter_hidden += on(s.hide_unread_status);
            u.mentions_hidden += on(s.hide_mention_status);
            u.group_mentions_muted += on(s.mute_group_mentions);
            if s.desktop_pref_origin.as_deref() == Some("subscription") {
                match s
                    .desktop_notifications
                    .as_deref()
                    .and_then(DesktopPref::parse)
                {
                    Some(DesktopPref::All) => u.alert_all += 1,
                    Some(DesktopPref::Mentions) => u.alert_mentions += 1,
                    Some(DesktopPref::Nothing) => u.alert_nothing += 1,
                    None => {}
                }
            }
        }
        u
    }
}

/// The signed-in account the page is computed for.
#[derive(Debug, Clone, PartialEq)]
pub struct AccountEnv {
    /// "<user> @ <server>".
    pub label: String,
    pub user_id: String,
    pub username: String,
    pub status: UserStatus,
    pub connection: Connection,
    pub prefs: AccountPrefs,
    /// The account desktop preference is stored on the subscriptions.
    /// `/me` always has a value (the server merges
    /// `Accounts_Default_User_Preferences_*` into it), but Rocket.Chat
    /// writes `desktopNotifications` to a subscription only once the user
    /// set it (`saveUserPreferences` → `updateNotificationPreferences`, and
    /// `getDefaultSubscriptionPref` for new subscriptions). Taken as set
    /// when some subscription carries a value with origin "user".
    pub desktop_stored: bool,
    pub server: ServerFacts,
    /// The R1–R5 room counts (`None` in tests that do not need them).
    pub usage: Option<RoomUsage>,
}

/// The receiver of the samples while no account is signed in (no name is
/// shown for it).
pub const PLACEHOLDER_USER_ID: &str = "help-me";
pub const PLACEHOLDER_USERNAME: &str = "me";

impl AccountEnv {
    /// From the account's workspace (`cache`: it shows the offline cache).
    pub fn of(label: String, ws: &Workspace, cache: bool) -> Self {
        Self {
            label,
            user_id: ws.user_id().unwrap_or_default().to_owned(),
            username: ws.username().unwrap_or_default().to_owned(),
            status: ws.my_status(),
            connection: Connection::of(ws.connection(), cache),
            prefs: AccountPrefs::of(ws),
            desktop_stored: ws.rooms().values().filter_map(|e| e.sub.as_ref()).any(|s| {
                s.desktop_pref_origin.as_deref() == Some("user")
                    && s.desktop_notifications.is_some()
            }),
            server: ServerFacts::from_settings(ws.settings()),
            usage: Some(RoomUsage::of(ws)),
        }
    }

    /// An account signed in whose `/me` is not loaded yet (no cache): the
    /// server's facts and connection, Rocket.Chat's defaults for the rest.
    pub fn pending(label: String, ws: &Workspace, cache: bool) -> Self {
        let server = ServerFacts::from_settings(ws.settings());
        let placeholder = Self::placeholder();
        Self {
            label,
            connection: Connection::of(ws.connection(), cache),
            // What `/me` will show: the server default.
            prefs: AccountPrefs {
                desktop: server.default_desktop,
                ..placeholder.prefs
            },
            server,
            ..placeholder
        }
    }

    /// What the scenarios run with while no account is signed in:
    /// Rocket.Chat's defaults and a placeholder identity.
    pub fn placeholder() -> Self {
        Self {
            label: String::new(),
            user_id: PLACEHOLDER_USER_ID.into(),
            username: PLACEHOLDER_USERNAME.into(),
            status: UserStatus::Online,
            connection: Connection::Connected,
            prefs: AccountPrefs::default(),
            desktop_stored: false,
            server: ServerFacts::default(),
            usage: None,
        }
    }
}

#[cfg(test)]
impl Default for AccountEnv {
    /// alice on the dev server.
    fn default() -> Self {
        Self {
            label: "alice @ Dev".into(),
            user_id: "alice-id".into(),
            username: "alice".into(),
            ..Self::placeholder()
        }
    }
}

/// Everything besides `AppConfig` the page is computed from.
#[derive(Debug, Clone, PartialEq)]
pub struct HelpEnv {
    /// UTC ms.
    pub now_ms: i64,
    /// `quiet_state` now (`App::quiet_now`).
    pub quiet: Quiet,
    /// Now is inside the working hours (`WorkingHours::is_working`, true
    /// when they are off).
    pub working_now: bool,
    /// While outside working hours (`quiet.off_hours`): their next start
    /// (`WorkingHours::next_boundary`; `None`: no working day).
    pub off_hours_until: Option<i64>,
    /// A tray icon exists.
    pub tray: bool,
    /// Rustket draws its own title bar (workspace items there, else in the
    /// left rail).
    pub custom_chrome: bool,
    /// `None`: no account signed in.
    pub account: Option<AccountEnv>,
    /// The platform (who plays a notification's sound).
    pub platform: Platform,
}

#[cfg(test)]
impl Default for HelpEnv {
    /// The dev server facts, nothing paused.
    fn default() -> Self {
        Self {
            now_ms: 1_795_600_800_000,
            quiet: Quiet::default(),
            working_now: true,
            off_hours_until: None,
            tray: true,
            custom_chrome: true,
            account: Some(AccountEnv::default()),
            platform: Platform::Linux,
        }
    }
}

impl HelpEnv {
    /// The local parts at `now` (as `App::quiet_now` computes the pause).
    pub fn new<Tz: chrono::TimeZone>(
        cfg: &AppConfig,
        now: &chrono::DateTime<Tz>,
        tray: bool,
        custom_chrome: bool,
        account: Option<AccountEnv>,
    ) -> Self {
        let wh = &cfg.ui.working_hours;
        let quiet = rustket_core::quiet::quiet_state(&cfg.pause, wh, now);
        Self {
            now_ms: now.timestamp_millis(),
            quiet,
            working_now: wh.is_working(now),
            off_hours_until: quiet
                .off_hours
                .then(|| wh.next_boundary(now).map(|b| b.timestamp_millis()))
                .flatten(),
            tray,
            custom_chrome,
            account,
            platform: Platform::current(),
        }
    }

    /// Why notifications are paused now, and until when: Q1 (by hand) or
    /// Q2 (outside working hours). When both apply, the one that ends last
    /// (notifications resume then), with its own end: the Q1 or Q2 line
    /// shows the same time.
    pub fn pause(&self, cfg: &AppConfig) -> Option<(Code, Option<i64>)> {
        if !self.quiet.paused {
            return None;
        }
        let manual = cfg.pause.paused_until.filter(|u| *u > self.now_ms);
        Some(match (manual, self.quiet.off_hours) {
            // Off-hours without a next start never end.
            (Some(m), true) if self.off_hours_until.is_none_or(|w| w > m) => {
                (Code::Q2, self.off_hours_until)
            }
            (Some(m), _) => (Code::Q1, Some(m)),
            (None, true) => (Code::Q2, self.off_hours_until),
            (None, false) => (Code::Q1, self.quiet.until),
        })
    }

    fn busy(&self) -> bool {
        self.account
            .as_ref()
            .is_some_and(|a| a.status == UserStatus::Busy)
    }
}

/// Per-room settings a scenario runs with (R1–R5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RoomMods {
    /// R1.
    pub notifications_off: bool,
    /// R2.
    pub hide_counter: bool,
    /// R3.
    pub hide_mentions: bool,
    /// R4.
    pub mute_group_mentions: bool,
    /// R5: the room's own desktop alert.
    pub alert: Option<DesktopPref>,
}

impl RoomMods {
    /// The setting of an R1–R4 code turned on.
    pub fn of(code: Code) -> Self {
        let mut m = Self::default();
        match code {
            Code::R1 => m.notifications_off = true,
            Code::R2 => m.hide_counter = true,
            Code::R3 => m.hide_mentions = true,
            Code::R4 => m.mute_group_mentions = true,
            _ => {}
        }
        m
    }
}

/// Something the page should say about the scenario.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Note {
    /// M4 without highlight words: shown as if there were one.
    NoHighlightWords,
    /// T codes on a server with threads off: never happens.
    ThreadsDisabled,
    /// No account: server-side parts use Rocket.Chat's defaults.
    NoAccount,
}

/// The server's part of an M/T scenario.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerOutcome {
    /// The receiver's subscription after the message.
    pub sub: Subscription,
    /// Whether the server sends a desktop notification (as if the receiver
    /// were connected: Rustket's own connection is a local cause, Q5).
    pub desktop: Result<(), ServerBlock>,
    /// The same message in a room above `Notifications_Max_Room_Members`
    /// (`None`: the server has no limit, or a DM).
    pub large_room: Option<LargeRoom>,
}

/// The server's decision in a room with more members than
/// `Notifications_Max_Room_Members` (the `disableAllMessageNotifications`
/// branch of `sendMessageNotifications`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LargeRoom {
    /// `Notifications_Max_Room_Members`.
    pub limit: u64,
    /// Whether the server sends a desktop notification there.
    pub desktop: Result<(), ServerBlock>,
    /// The account has no highlight words, the server sends nothing and
    /// one highlight word would make it send.
    pub highlights_would_help: bool,
    /// The server sends nothing and Rustket shows the notification itself
    /// (`rustket_core::notify::large_room`).
    pub fallback: bool,
}

/// A sidebar room row.
#[derive(Debug, Clone, PartialEq)]
pub struct RowOutcome {
    pub badge: Badge,
    /// `disableNotifications`: the BellOff icon.
    pub muted: bool,
    pub room_kind: RoomKind,
    pub density: Density,
    /// The kind drawn; `None`: no badge.
    pub kind: Option<BadgeKind>,
    /// The number shown; 0 with a kind: a dot.
    pub count: u32,
    pub bold: bool,
}

impl RowOutcome {
    fn new(badge: Badge, muted: bool, room_kind: RoomKind, density: Density) -> Self {
        let mut r = Self {
            badge,
            muted,
            room_kind,
            density,
            kind: None,
            count: 0,
            bold: false,
        };
        let p = r.props("", ());
        r.kind = p.badge_kind();
        r.count = p.badge_count();
        r.bold = p.is_bold();
        r
    }

    /// The props the sidebar would give this row (avatar from initials).
    pub fn props<M>(&self, title: &str, on_press: M) -> RoomListItemProps<M> {
        let mut p = RoomListItemProps::new(AvatarSource::initials(title, title), title, on_press);
        p.density = widget_density(self.density);
        p.icon = Some(sidebar::room_icon(self.room_kind));
        sidebar::set_row_badge(&mut p, &self.badge);
        p.is_muted = self.muted;
        p
    }
}

fn widget_density(d: Density) -> rustket_ui::widget::Density {
    match d {
        Density::Extended => rustket_ui::widget::Density::Extended,
        Density::Condensed => rustket_ui::widget::Density::Condensed,
    }
}

/// Where a thread row is listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadListing {
    /// In the Threads section (cut to `limit` rows, 0 = no limit).
    Threads { limit: u32 },
    /// The Threads section is off (`UiPrefs::sidebar_threads`).
    SectionOff,
    /// Listed only while unread, and it is not (hidden unread status).
    NotUnread,
    /// The server has threads off.
    ServerOff,
}

/// A sidebar thread row (T codes).
#[derive(Debug, Clone, PartialEq)]
pub struct ThreadRowOutcome {
    pub unread: Option<BadgeVariant>,
    /// The dot drawn; `None`: no dot.
    pub kind: Option<BadgeKind>,
    pub listing: ThreadListing,
    /// Starred threads show in Favorites (whatever the Threads mode).
    pub starred_in_favorites: bool,
    pub density: Density,
    /// The dot of the room header's Threads button (any Threads mode;
    /// `None`: no dot, or threads are off on the server).
    pub header_button: Option<BadgeKind>,
}

impl ThreadRowOutcome {
    pub fn props<M>(&self, title: &str, on_press: M) -> RoomListItemProps<M> {
        let mut p = RoomListItemProps::new(AvatarSource::initials(title, title), title, on_press);
        p.density = widget_density(self.density);
        p.icon = Some(Icon::MessageCircle);
        p.badge = self.kind;
        p.bold = self.kind.is_some();
        p
    }
}

/// Where the room row goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placement {
    /// The Unread section (`unread_on_top`).
    Unread,
    /// Its own section; `first`: sorted into the section's unread block.
    Section {
        id: SectionId,
        unread_first: bool,
        first: bool,
    },
}

/// The pinned rows under the search (`sidebar_pinned_views`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PinnedOutcome {
    /// The room is counted in All unreads.
    pub in_all_unreads: bool,
    pub all_unreads: Option<(u64, BadgeKind)>,
    pub activity: Option<(u64, BadgeKind)>,
    pub later: Option<(u64, BadgeKind)>,
    pub drafts: Option<(u64, BadgeKind)>,
    /// D1, for comparison: the Drafts & scheduled row with one scheduled
    /// message still on time.
    pub drafts_on_time: Option<(u64, BadgeKind)>,
}

/// Where the account's workspace icon is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconPlace {
    TitleBar,
    Rail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccountIcon {
    pub place: IconPlace,
    /// `rail::badge`: (count, kind); count 0 is a dot.
    pub badge: Option<(u32, BadgeKind)>,
    /// `room_list::entry_signal`: what the room adds to the account's
    /// unread signal.
    pub signal: WorkspaceSignal,
}

/// Window title and tray, with this room the only unread one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WindowOutcome {
    /// The signal `app::window_title` and the tray get.
    pub signal: WorkspaceSignal,
    /// The tray icon's dot (`TrayLevel::of`; `None`: no tray).
    pub tray: Option<TrayLevel>,
}

/// Why a notification is not shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastBlocked {
    /// No account signed in.
    NoAccount,
    /// The server sends none.
    Server(ServerBlock),
    /// Rustket is not connected to the account's server, so it receives
    /// nothing (Q5); the server may still notify the user's other clients.
    NotConnected(Connection),
    /// Settings → Notifications: desktop notifications off.
    Disabled,
    /// Paused by hand (Q1) or outside working hours (Q2).
    Paused { until: Option<i64>, cause: Code },
    /// The user's status is Busy (Q3).
    Busy,
    /// Settings → Notifications: reminder notifications off.
    RemindersOff,
}

impl ToastBlocked {
    /// The code that causes it, if it has one.
    pub fn code(&self) -> Option<Code> {
        match self {
            ToastBlocked::Server(b) => server_block_code(b),
            ToastBlocked::Paused { cause, .. } => Some(*cause),
            ToastBlocked::Busy => Some(Code::Q3),
            ToastBlocked::NotConnected(_) => Some(Code::Q5),
            ToastBlocked::NoAccount | ToastBlocked::Disabled | ToastBlocked::RemindersOff => None,
        }
    }
}

/// The code of a server reason, if it has one.
pub fn server_block_code(b: &ServerBlock) -> Option<Code> {
    match b {
        ServerBlock::RoomMuted => Some(Code::R1),
        ServerBlock::GroupMentionsMuted => Some(Code::R4),
        ServerBlock::Busy => Some(Code::Q3),
        ServerBlock::Offline => Some(Code::Q5),
        ServerBlock::Preference { room: true, .. } => Some(Code::R5),
        ServerBlock::Preference { room: false, .. }
        | ServerBlock::ServerDefault(_)
        | ServerBlock::LargeRoom { .. } => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Toast {
    /// `preview`: the text (else "New message"); `sticky`: stays until
    /// dismissed; `unfocused_only`: only while Rustket's window is not
    /// focused (C1).
    Shown {
        preview: bool,
        sticky: bool,
        unfocused_only: bool,
    },
    /// Every cause, the server's first.
    Blocked(Vec<ToastBlocked>),
}

impl Toast {
    pub fn shown(&self) -> bool {
        matches!(self, Toast::Shown { .. })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoundKind {
    /// The message sound (the Rocket.Chat sound's name).
    Message(String),
    /// The reminder sound: the account's message sound (its name).
    Reminder(String),
    /// The incoming call ring.
    Ring,
    /// The new omnichannel chat sound (its name).
    NewChat(String),
    /// The Windows notification sound: a shown toast plays it in place of
    /// the message or reminder sound (`desktop::sound::route`).
    WindowsNotification,
}

/// The sound heard for `sound`, as `desktop::sound::route` decides: where
/// the shown toast cannot play the Rocket.Chat sound's file it plays the
/// system's notification sound instead (Windows). The toast is modelled as
/// one whose backend plays sounds, with the file prepared; `kind` names the
/// Rocket.Chat sound otherwise.
pub(crate) fn routed_sound(
    platform: Platform,
    toast_shown: bool,
    sound: RcSound,
    kind: fn(String) -> SoundKind,
) -> SoundKind {
    use crate::desktop::sound::{OsSound, Route, Toast as OsToast, route};
    let toast = if toast_shown {
        OsToast::Shown { plays_sound: true }
    } else {
        OsToast::None
    };
    let file = std::path::Path::new(&sound.name);
    match route(platform, toast, Some(&sound), Some(file)) {
        Route::Os(OsSound::Default) => SoundKind::WindowsNotification,
        Route::Os(OsSound::File(_)) | Route::Local { .. } | Route::Silent => kind(sound.name),
    }
}

/// Why no sound plays.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SoundOff {
    /// It comes only with a shown notification (reminders).
    NoToast,
    /// The message does not notify (the notification line says why).
    NotNotified,
    /// Settings → Notifications: sound off.
    Setting,
    /// The room's sound is "None".
    Room,
    /// The account's New message sound is "None".
    Account,
    /// The account's New room sound is "None" (O1).
    AccountNewRoom,
    /// The account's volume is zero.
    Volume,
    Paused {
        until: Option<i64>,
        cause: Code,
    },
    Busy,
    /// Settings → Notifications: reminder sound off.
    ReminderSoundOff,
}

impl SoundOff {
    fn of_reminder(
        o: crate::app::reminders::ReminderSoundOff,
        cfg: &AppConfig,
        env: &HelpEnv,
    ) -> Self {
        use crate::app::reminders::ReminderSoundOff as R;
        match o {
            R::Sound => SoundOff::Setting,
            R::Setting => SoundOff::ReminderSoundOff,
            R::Paused => {
                let (cause, until) = env.pause(cfg).unwrap_or((Code::Q1, None));
                SoundOff::Paused { until, cause }
            }
            R::Busy => SoundOff::Busy,
            R::Account => SoundOff::Account,
            R::Volume => SoundOff::Volume,
        }
    }
}

impl From<MessageSoundOff> for SoundOff {
    fn from(o: MessageSoundOff) -> Self {
        match o {
            MessageSoundOff::Setting => SoundOff::Setting,
            MessageSoundOff::Room => SoundOff::Room,
            MessageSoundOff::Account => SoundOff::Account,
            MessageSoundOff::Volume => SoundOff::Volume,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sound {
    Plays(SoundKind),
    Silent(SoundOff),
}

/// The message in the room's feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Feed {
    /// The row's mention highlight.
    pub highlighted: bool,
    /// The @chip, `true` in the "me" (red) colours.
    pub chip_mine: Option<bool>,
    /// The highlight word is marked in the text.
    pub word_marked: bool,
    /// An "Unread messages" divider when the room opens.
    pub divider: bool,
    /// The thread parent shows the unread-thread dot.
    pub thread_dot: bool,
}

/// What one code shows on every surface that applies (`None`: does not
/// apply).
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub code: Code,
    pub note: Option<Note>,
    pub server: Option<ServerOutcome>,
    pub row: Option<RowOutcome>,
    pub thread_row: Option<ThreadRowOutcome>,
    pub section: Option<Placement>,
    /// The dot of the room's section header while collapsed.
    pub collapsed: Option<BadgeKind>,
    /// The dot of the collapsed Threads header.
    pub collapsed_threads: Option<BadgeKind>,
    /// `None` while the pinned rows are off.
    pub pinned: Option<PinnedOutcome>,
    pub account_icon: Option<AccountIcon>,
    pub window: Option<WindowOutcome>,
    pub toast: Option<Toast>,
    pub sound: Option<Sound>,
    pub feed: Option<Feed>,
    /// C1: the in-app call card (always, even paused or Busy).
    pub popup: bool,
    /// D1: the "not sent on time" dialog.
    pub dialog: bool,
    /// L2: one notification per server per run.
    pub once_per_run: bool,
}

impl Outcome {
    fn empty(code: Code) -> Self {
        Self {
            code,
            note: None,
            server: None,
            row: None,
            thread_row: None,
            section: None,
            collapsed: None,
            collapsed_threads: None,
            pinned: None,
            account_icon: None,
            window: None,
            toast: None,
            sound: None,
            feed: None,
            popup: false,
            dialog: false,
            once_per_run: false,
        }
    }
}

const RID: &str = "help-room";
const TMID: &str = "help-thread";
const MID: &str = "help-message";
const SENDER_ID: &str = "help-sender";
/// Members of the scenario's room (below `Notifications_Max_Room_Members`).
const ROOM_MEMBERS: u64 = 5;

/// The outcome of `code` under the current settings.
pub fn evaluate(code: Code, cfg: &AppConfig, env: &HelpEnv) -> Outcome {
    evaluate_with(code, RoomMods::default(), cfg, env)
}

/// [`evaluate`] in a room with `mods` (M/T codes).
pub fn evaluate_with(code: Code, mods: RoomMods, cfg: &AppConfig, env: &HelpEnv) -> Outcome {
    if let Some(msg) = code.scenario() {
        return message(code, msg, mods, cfg, env);
    }
    match code {
        Code::L1 | Code::L2 => reminder(code, cfg, env),
        Code::D1 => scheduled(cfg),
        Code::C1 => call(cfg, env),
        Code::O1 => omnichannel(cfg, env),
        _ => Outcome::empty(code),
    }
}

/// The scenario's room, subscription and workspace.
struct Scene {
    entry: RoomEntry,
    ws: Workspace,
}

/// `lm`: the room's last main-timeline message (UTC ms); `facts` and
/// `starred_threads` (`UiPrefs::sidebar_starred_threads`) set what the
/// sidebar reads from the server settings.
fn scene(sub: Subscription, lm: i64, acc: &AccountEnv, starred_threads: bool) -> Scene {
    let room = Room {
        id: RID.into(),
        t: sub.t,
        name: Some(sub.name.clone()),
        lm: RcDate::from_millis(lm),
        ..Default::default()
    };
    let mut ws = Workspace::new();
    ws.apply_me(User {
        id: acc.user_id.clone(),
        username: Some(acc.username.clone()),
        ..Default::default()
    });
    for (id, on) in [
        ("Message_AllowStarring", acc.server.allow_starring),
        ("Threads_enabled", acc.server.threads_enabled),
    ] {
        ws.apply_setting(
            Change::Inserted,
            Setting {
                id: id.into(),
                value: on.into(),
                ..Default::default()
            },
        );
    }
    ws.set_starred_threads(starred_threads);
    ws.apply_subscription(Change::Inserted, sub.clone());
    ws.apply_room(Change::Inserted, room.clone());
    Scene {
        entry: RoomEntry {
            room: Some(room),
            sub: Some(sub),
        },
        ws,
    }
}

/// The receiver's subscription before the message: read at `ls` (UTC ms),
/// with `mods`. Without a room alert the account preference is on it only
/// when stored (`desktop_stored`); otherwise the server default decides.
fn base_sub(t: RoomType, ls: i64, mods: RoomMods, acc: &AccountEnv) -> Subscription {
    let on = |b: bool| b.then_some(true);
    let (desktop, origin) = match (mods.alert, acc.prefs.desktop) {
        (Some(p), _) => (Some(p), Some("subscription")),
        (None, Some(p)) if acc.desktop_stored => (Some(p), Some("user")),
        (None, _) => (None, None),
    };
    Subscription {
        id: format!("sub-{RID}"),
        rid: RID.into(),
        name: if t == RoomType::Direct {
            "bob"
        } else {
            "general"
        }
        .into(),
        t,
        u: UserRef {
            id: acc.user_id.clone(),
            username: Some(acc.username.clone()),
            name: None,
        },
        open: true,
        ls: RcDate::from_millis(ls),
        disable_notifications: on(mods.notifications_off),
        hide_unread_status: on(mods.hide_counter),
        hide_mention_status: on(mods.hide_mentions),
        mute_group_mentions: on(mods.mute_group_mentions),
        desktop_notifications: desktop.map(|p| p.as_str().to_owned()),
        desktop_pref_origin: origin.map(str::to_owned),
        user_highlights: (!acc.prefs.highlights.is_empty()).then(|| acc.prefs.highlights.clone()),
        ..Default::default()
    }
}

/// (`ls`, `lm`): read an hour ago; a thread reply leaves the room's last
/// message older.
fn times(env: &HelpEnv, thread: bool) -> (i64, i64) {
    let ls = env.now_ms - 3_600_000;
    (ls, if thread { ls - 60_000 } else { env.now_ms })
}

fn message(code: Code, msg: Incoming, mods: RoomMods, cfg: &AppConfig, env: &HelpEnv) -> Outcome {
    let ui = &cfg.ui;
    let mut out = Outcome::empty(code);
    let fallback = AccountEnv::placeholder();
    let acc = env.account.as_ref().unwrap_or(&fallback);
    let facts = acc.server;
    let highlight = msg.trigger == Trigger::Highlight;
    out.note = if env.account.is_none() {
        Some(Note::NoAccount)
    } else if msg.thread && !facts.threads_enabled {
        Some(Note::ThreadsDisabled)
    } else if highlight && acc.prefs.highlights.is_empty() {
        Some(Note::NoHighlightWords)
    } else {
        None
    };
    let receiver = Receiver {
        status: acc.status,
        // The server's side; Rustket's own connection is a local cause.
        connected: true,
        // M4 shows what one of the user's words does.
        has_highlights: highlight || !acc.prefs.highlights.is_empty(),
        room_members: ROOM_MEMBERS,
    };
    let (ls, lm) = times(env, msg.thread);
    let mut sub = base_sub(msg.room, ls, mods, acc);
    let before = sub.clone();
    server::apply(&mut sub, &msg, TMID, &facts, &receiver);
    let desktop = server::desktop(&before, &msg, &facts, &receiver);
    // DMs are capped by `DirectMesssage_maxUsers` (8 by default): never a
    // large room.
    let large_room = (facts.max_room_members != 0 && msg.room != RoomType::Direct).then(|| {
        let r = Receiver {
            room_members: facts.max_room_members.saturating_add(1),
            ..receiver
        };
        let desktop = server::desktop(&before, &msg, &facts, &r);
        let highlights_would_help = acc.prefs.highlights.is_empty()
            && desktop.is_err()
            && server::desktop(
                &Subscription {
                    user_highlights: Some(vec![String::from("word")]),
                    ..before.clone()
                },
                &msg,
                &facts,
                &Receiver {
                    has_highlights: true,
                    ..r
                },
            )
            .is_ok();
        let fallback = matches!(desktop, Err(ServerBlock::LargeRoom { .. }))
            && large_room_fallback(&before, &sub, &msg, acc, facts.max_room_members, lm);
        LargeRoom {
            limit: facts.max_room_members,
            desktop,
            highlights_would_help,
            fallback,
        }
    });
    let s = scene(sub.clone(), lm, acc, ui.sidebar_starred_threads);
    let opts = RoomListOptions::from_prefs(ui, &s.ws);
    let item = room_list::item(&s.ws, &s.entry, &opts);

    // Sidebar row, section, collapsed header.
    let row = RowOutcome::new(item.badge, item.muted, item.kind, ui.density);
    out.section = Some(match room_list::section_of(&s.entry, &item, &opts) {
        SectionId::Builtin(SectionKind::Unread) => Placement::Unread,
        id => {
            let unread_first = section_unread_first(ui, &id);
            Placement::Section {
                first: unread_first && room_list::in_unread_block(&item, &opts),
                unread_first,
                id,
            }
        }
    });
    out.collapsed = sidebar::collapsed_unread([row.kind]).map(|(k, _)| k);
    out.row = Some(row);

    // Thread row.
    if msg.thread {
        let unread = room_list::thread_unread(&sub, TMID);
        let kind = sidebar::thread_unread_kind(unread);
        // mirrors crates/rustket-core/src/room_list.rs (`all_thread_rows`):
        // Unread / UnreadKept list unread threads, Following every followed
        // one; nothing while the section is off or threads are off. Tested
        // against `all_thread_rows` (thread_listing_agrees_with_the_sidebar).
        let listing = if !facts.threads_enabled {
            ThreadListing::ServerOff
        } else if ui.sidebar_threads == ThreadsMode::Off {
            ThreadListing::SectionOff
        } else if unread.is_none() && ui.sidebar_threads != ThreadsMode::Following {
            ThreadListing::NotUnread
        } else {
            ThreadListing::Threads {
                limit: ui.sidebar_threads_limit,
            }
        };
        if matches!(listing, ThreadListing::Threads { .. }) {
            out.collapsed_threads = sidebar::collapsed_unread([kind]).map(|(k, _)| k);
        }
        out.thread_row = Some(ThreadRowOutcome {
            unread,
            kind,
            listing,
            starred_in_favorites: room_list::starred_threads_shown(&s.ws, &opts),
            density: ui.density,
            header_button: facts
                .threads_enabled
                .then(|| crate::room::header::threads_badge(Some(&sub)))
                .flatten(),
        });
    }

    // Pinned rows, account icon, window title and tray.
    if ui.sidebar_pinned_views {
        let c = rustket_core::inbox::entry_counts(&s.entry);
        out.pinned = Some(PinnedOutcome {
            in_all_unreads: c.unread_rooms > 0,
            all_unreads: sidebar::unreads_badge(c.unread_rooms),
            activity: sidebar::activity_badge(&c),
            ..PinnedOutcome::default()
        });
    }
    let signal = room_list::entry_signal(&s.entry);
    out.account_icon = Some(AccountIcon {
        place: if env.custom_chrome {
            IconPlace::TitleBar
        } else {
            IconPlace::Rail
        },
        badge: rail::badge(signal),
        signal,
    });
    out.window = Some(WindowOutcome {
        signal,
        tray: env.tray.then(|| TrayLevel::of(&signal)),
    });

    // Toast and sound: the server, then the app's own rules for a window in
    // the background.
    let incoming = notification(&msg);
    let ctx = NotifyContext {
        my_user_id: &acc.user_id,
        my_status: acc.status,
        focused_rid: None,
        focused_tmid: None,
        window_active: false,
        server_active: true,
        prefs: NotificationPrefs::from(ui),
        paused: env.quiet.paused,
        mute_focused: acc.prefs.mute_focused,
        reply_in_room: false,
    };
    let local = rustket_core::notify::evaluate(&incoming, &ctx);
    let mut blocked = Vec::new();
    if env.account.is_none() {
        blocked.push(ToastBlocked::NoAccount);
    } else {
        if let Err(b) = desktop {
            blocked.push(ToastBlocked::Server(b));
        }
        if acc.connection != Connection::Connected {
            blocked.push(ToastBlocked::NotConnected(acc.connection));
        }
    }
    // The notification reaches the app and passes its gates: the sound
    // plays, with or without the toast.
    let notified = blocked.is_empty() && local.is_some();
    match &local {
        None => {
            // The causes `notify::evaluate` checks, in its order.
            if !ui.desktop_notifications {
                blocked.push(ToastBlocked::Disabled);
            }
            if let Some((cause, until)) = env.pause(cfg) {
                blocked.push(ToastBlocked::Paused { until, cause });
            }
            if acc.status == UserStatus::Busy && desktop != Err(ServerBlock::Busy) {
                blocked.push(ToastBlocked::Busy);
            }
        }
        Some(spec) if !spec.toast => blocked.push(ToastBlocked::Disabled),
        Some(_) => {}
    }
    let toast = match (local, blocked.is_empty()) {
        (Some(spec), true) => Toast::Shown {
            preview: spec.body.is_some(),
            // mirrors crates/rustket/src/desktop/notifications/mod.rs
            // (`timeout_and_urgency`: the preference keeps it on screen);
            // tested there (help_sticky_agrees_with_the_timeout).
            sticky: acc.prefs.require_interaction,
            unfocused_only: false,
        },
        _ => Toast::Blocked(blocked),
    };
    out.sound = Some(
        match (message_sound(ui.sound, None, &acc.prefs.sound), notified) {
            (Err(MessageSoundOff::Setting), _) => Sound::Silent(SoundOff::Setting),
            (_, false) => Sound::Silent(SoundOff::NotNotified),
            (Ok(s), true) => Sound::Plays(routed_sound(
                env.platform,
                toast.shown(),
                s,
                SoundKind::Message,
            )),
            (Err(o), true) => Sound::Silent(o.into()),
        },
    );
    out.toast = Some(toast);

    // The feed.
    let words = feed_words(&msg, acc);
    let m = feed_message(&msg, acc, &words);
    let chip = match msg.trigger {
        Trigger::MentionMe => Some(acc.username.as_str()),
        Trigger::MentionAll => Some("all"),
        Trigger::Plain | Trigger::Highlight => None,
    };
    out.feed = Some(Feed {
        highlighted: crate::room::timeline::row::highlights_row(
            Some(&acc.user_id),
            sub.mute_group_mentions == Some(true),
            &words,
            &m,
        ),
        // mirrors crates/rustket/src/room/render/md.rs (`push_marked`:
        // someone else's text, matched by `rustket_core::highlight`).
        word_marked: rustket_core::highlight::contains(&m.msg, &words),
        chip_mine: chip.map(|n| crate::room::render::mention_is_mine(n, Some(&acc.username))),
        divider: !msg.thread,
        thread_dot: msg.thread && crate::room::timeline::parent_thread_unread(Some(&sub), TMID),
    });
    out.server = Some(ServerOutcome {
        sub,
        desktop,
        large_room,
    });
    out
}

/// Whether Rustket's large-room fallback notifies the scenario: the
/// server's update (`before` → `after`) counts a mention in a room above
/// `limit` whose last message (sent at `lm`; a thread reply is not the
/// room's last message) mentions the user, and the room allows it.
fn large_room_fallback(
    before: &Subscription,
    after: &Subscription,
    msg: &Incoming,
    acc: &AccountEnv,
    limit: u64,
    lm: i64,
) -> bool {
    use rustket_core::notify::large_room;
    let Some(about) = RcDate::from_millis(lm) else {
        return false;
    };
    let last = (!msg.thread).then(|| {
        Box::new(RcMessage {
            ts: Some(about),
            ..feed_message(msg, acc, &feed_words(msg, acc))
        })
    });
    let room = Room {
        id: RID.into(),
        t: msg.room,
        users_count: Some(limit.saturating_add(1)),
        last_message: last,
        ..Default::default()
    };
    // The subscription's change before the update came before the message.
    let Some(prev) = RcDate::from_millis(lm.saturating_sub(1)) else {
        return false;
    };
    large_room::triggers(Some(before), after, Some(&room), limit)
        && large_room::allowed(after, acc.prefs.desktop.map(DesktopPref::as_str))
        && large_room::mention(&room, after, prev, &acc.user_id, &acc.username).is_some()
}

fn sender() -> UserRef {
    UserRef {
        id: SENDER_ID.into(),
        username: Some("bob".into()),
        name: Some("Bob".into()),
    }
}

fn mentions(msg: &Incoming, acc: &AccountEnv) -> Option<Vec<Mention>> {
    let one = |id: &str, username: &str| Mention {
        id: id.into(),
        username: Some(username.into()),
        ..Default::default()
    };
    match msg.trigger {
        Trigger::MentionMe => Some(vec![one(&acc.user_id, &acc.username)]),
        Trigger::MentionAll => Some(vec![one("all", "all")]),
        Trigger::Plain | Trigger::Highlight => None,
    }
}

/// The `notification` event the server sends for the scenario.
fn notification(msg: &Incoming) -> IncomingNotification {
    let text = "Hello";
    IncomingNotification {
        title: if msg.room == RoomType::Direct {
            "Bob".into()
        } else {
            "#general".into()
        },
        text: text.into(),
        duration: None,
        payload: NotificationPayload {
            id: MID.into(),
            rid: RID.into(),
            tmid: msg.thread.then(|| TMID.to_owned()),
            sender: Some(sender()),
            room_type: Some(msg.room),
            name: (msg.room != RoomType::Direct).then(|| "general".to_owned()),
            message: Some(NotificationMessage {
                msg: text.into(),
                t: None,
            }),
        },
    }
}

/// The highlight words the feed sees: the account's, or a placeholder
/// word for M4/T2-by-word when it has none (the scenario shows what one
/// of the user's words does).
fn feed_words(msg: &Incoming, acc: &AccountEnv) -> Vec<String> {
    if msg.trigger == Trigger::Highlight && acc.prefs.highlights.is_empty() {
        vec![String::from("word")]
    } else {
        acc.prefs.highlights.clone()
    }
}

fn feed_message(msg: &Incoming, acc: &AccountEnv, words: &[String]) -> RcMessage {
    let text = match (msg.trigger, words.first()) {
        (Trigger::Highlight, Some(w)) => format!("Hello {w}"),
        _ => String::from("Hello"),
    };
    RcMessage {
        id: MID.into(),
        rid: RID.into(),
        msg: text,
        u: sender(),
        tmid: msg.thread.then(|| TMID.to_owned()),
        mentions: mentions(msg, acc),
        ..Default::default()
    }
}

/// L1 and L2: reminder notifications.
fn reminder(code: Code, cfg: &AppConfig, env: &HelpEnv) -> Outcome {
    use crate::app::reminders::{reminder_sound, reminder_toast};
    let ui = &cfg.ui;
    let mut out = Outcome::empty(code);
    let paused = env.quiet.paused;
    let toast = if reminder_toast(ui) {
        Toast::Shown {
            preview: ui.notification_preview,
            sticky: true,
            unfocused_only: false,
        }
    } else {
        Toast::Blocked(vec![ToastBlocked::RemindersOff])
    };
    let fallback = AccountPrefs::default();
    let prefs = env.account.as_ref().map_or(&fallback, |a| &a.prefs);
    out.sound = Some(if !toast.shown() {
        Sound::Silent(SoundOff::NoToast)
    } else {
        match reminder_sound(ui, paused, env.busy(), &prefs.sound) {
            Ok(s) => Sound::Plays(routed_sound(env.platform, true, s, SoundKind::Reminder)),
            Err(o) => Sound::Silent(SoundOff::of_reminder(o, cfg, env)),
        }
    });
    out.toast = Some(toast);
    out.once_per_run = code == Code::L2;
    if code == Code::L1 && ui.sidebar_pinned_views {
        out.pinned = Some(PinnedOutcome {
            later: sidebar::later_badge(1),
            ..PinnedOutcome::default()
        });
    }
    out
}

/// D1: a scheduled message not sent on time.
fn scheduled(cfg: &AppConfig) -> Outcome {
    let mut out = Outcome::empty(Code::D1);
    out.dialog = true;
    if cfg.ui.sidebar_pinned_views {
        out.pinned = Some(PinnedOutcome {
            drafts: sidebar::drafts_badge(1, 1),
            drafts_on_time: sidebar::drafts_badge(1, 0),
            ..PinnedOutcome::default()
        });
    }
    out
}

/// The pause/Busy causes that silence call and chat sounds, in order.
fn quiet_causes(cfg: &AppConfig, env: &HelpEnv) -> Option<SoundOff> {
    if let Some((cause, until)) = env.pause(cfg) {
        Some(SoundOff::Paused { until, cause })
    } else if env.busy() {
        Some(SoundOff::Busy)
    } else {
        None
    }
}

/// C1: an incoming call (window in the background).
fn call(cfg: &AppConfig, env: &HelpEnv) -> Outcome {
    let ui = &cfg.ui;
    let mut out = Outcome::empty(Code::C1);
    out.popup = true;
    let fallback = AccountPrefs::default();
    let prefs = env.account.as_ref().map_or(&fallback, |a| &a.prefs);
    let alerts = crate::videoconf::alerts_with(ui, env.quiet.paused, env.busy(), &prefs.sound);
    out.sound = Some(if alerts.sound {
        Sound::Plays(SoundKind::Ring)
    } else {
        Sound::Silent(
            quiet_causes(cfg, env)
                .or_else(|| message_sound_off(ui.sound, None, &prefs.sound).map(Into::into))
                .unwrap_or(SoundOff::Setting),
        )
    });
    // mirrors crates/rustket/src/videoconf/mod.rs (`!app.focused &&
    // alerts.notify`): the focus is App state, so the outcome says it.
    out.toast = Some(if alerts.notify {
        Toast::Shown {
            preview: true,
            sticky: true,
            unfocused_only: true,
        }
    } else {
        let mut v = Vec::new();
        if let Some((cause, until)) = env.pause(cfg) {
            v.push(ToastBlocked::Paused { until, cause });
        }
        if env.busy() {
            v.push(ToastBlocked::Busy);
        }
        if !ui.desktop_notifications {
            v.push(ToastBlocked::Disabled);
        }
        Toast::Blocked(v)
    });
    out
}

/// O1: a new omnichannel chat (sound only).
fn omnichannel(cfg: &AppConfig, env: &HelpEnv) -> Outcome {
    let ui = &cfg.ui;
    let mut out = Outcome::empty(Code::O1);
    let fallback = AccountPrefs::default();
    let prefs = env.account.as_ref().map_or(&fallback, |a| &a.prefs);
    let sound =
        crate::omnichannel::new_chat_sound(&prefs.sound, ui.sound, env.busy(), env.quiet.paused);
    out.sound = Some(match sound {
        Some(s) => Sound::Plays(SoundKind::NewChat(s.name)),
        None => Sound::Silent(quiet_causes(cfg, env).unwrap_or(if !ui.sound {
            SoundOff::Setting
        } else if prefs.sound.volume_muted() {
            SoundOff::Volume
        } else {
            SoundOff::AccountNewRoom
        })),
    });
    out
}

/// Rows of the R matrix.
pub const MATRIX_CODES: [Code; 7] = [
    Code::M1,
    Code::M2,
    Code::M3,
    Code::M5,
    Code::T1,
    Code::T2,
    Code::T3,
];
/// Columns of the R matrix: as is, then R1–R4 on.
pub const MATRIX_MODS: [Option<Code>; 5] = [
    None,
    Some(Code::R1),
    Some(Code::R2),
    Some(Code::R3),
    Some(Code::R4),
];

/// One cell of the R matrix.
#[derive(Debug, Clone, PartialEq)]
pub struct Cell {
    pub row: RowOutcome,
    /// The server sends a desktop notification.
    pub server_toast: bool,
}

/// The R matrix: for each of [`MATRIX_CODES`], a cell per [`MATRIX_MODS`].
pub fn matrix(cfg: &AppConfig, env: &HelpEnv) -> Vec<(Code, Vec<Cell>)> {
    MATRIX_CODES
        .iter()
        .map(|&code| {
            let cells = MATRIX_MODS
                .iter()
                .map(|m| {
                    let o = evaluate_with(code, m.map(RoomMods::of).unwrap_or_default(), cfg, env);
                    cell(&o)
                })
                .collect();
            (code, cells)
        })
        .collect()
}

fn cell(o: &Outcome) -> Cell {
    Cell {
        row: o.row.clone().unwrap_or_else(|| {
            RowOutcome::new(
                Badge::default(),
                false,
                RoomKind::Channel,
                Density::Extended,
            )
        }),
        server_toast: o.server.as_ref().is_some_and(|s| s.desktop.is_ok()),
    }
}

/// R5: whether the server notifies for each matrix code with the room's
/// desktop alert set to each value.
pub fn alert_table(cfg: &AppConfig, env: &HelpEnv) -> Vec<(DesktopPref, Vec<(Code, bool)>)> {
    [
        DesktopPref::All,
        DesktopPref::Mentions,
        DesktopPref::Nothing,
    ]
    .iter()
    .map(|&p| {
        let mods = RoomMods {
            alert: Some(p),
            ..RoomMods::default()
        };
        let row = MATRIX_CODES
            .iter()
            .map(|&c| (c, cell(&evaluate_with(c, mods, cfg, env)).server_toast))
            .collect();
        (p, row)
    })
    .collect()
}

/// Working hours now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkingHoursState {
    Off,
    Inside,
    /// Paused until the next start (`None`: no working day, no end).
    Outside {
        until: Option<i64>,
    },
    /// Outside, but resumed by hand until then (`None`: until the working
    /// hours get a working day).
    Resumed {
        until: Option<i64>,
    },
}

/// The current value of a Q code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Global {
    /// Q1: paused by hand until (`None`: not paused).
    Pause { until: Option<i64> },
    /// Q2.
    WorkingHours(WorkingHoursState),
    /// Q3: the account's status (`None`: no account).
    Status(Option<UserStatus>),
    /// Q4: a rule, no value.
    OnScreen,
    /// Q5 (`None`: no account).
    Connection(Option<Connection>),
}

/// The current value of `code` (Q codes only).
pub fn global(code: Code, cfg: &AppConfig, env: &HelpEnv) -> Option<Global> {
    Some(match code {
        Code::Q1 => Global::Pause {
            until: cfg.pause.paused_until.filter(|u| *u > env.now_ms),
        },
        Code::Q2 => Global::WorkingHours(if !cfg.ui.working_hours.enabled {
            WorkingHoursState::Off
        } else if env.quiet.off_hours {
            WorkingHoursState::Outside {
                until: env.off_hours_until,
            }
        } else if env.working_now {
            WorkingHoursState::Inside
        } else {
            WorkingHoursState::Resumed {
                until: cfg.pause.resumed_until.filter(|u| *u != UNTIL_BOUNDARY),
            }
        }),
        Code::Q3 => Global::Status(env.account.as_ref().map(|a| a.status)),
        Code::Q4 => Global::OnScreen,
        Code::Q5 => Global::Connection(env.account.as_ref().map(|a| a.connection)),
        _ => return None,
    })
}

/// A surface that draws a badge or dot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Surface {
    /// The room row in the sidebar.
    Row,
    /// The thread row (only while it is listed).
    ThreadRow,
    /// The room header's Threads button (any Threads section mode).
    ThreadsButton,
    /// The account's workspace icon (title bar or left rail).
    AccountIcon,
    /// The pinned All unreads row.
    AllUnreads,
    /// The pinned Activity row.
    Activity,
    /// The pinned Later row.
    Later,
    /// The pinned Drafts & scheduled row.
    Drafts,
}

/// One use of a colour: `code` draws it on `surface`, as a dot (`dot`) or a
/// counter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KindUse {
    pub code: Code,
    pub surface: Surface,
    pub dot: bool,
}

/// Every badge kind the M/T/L/D outcomes currently show, with where each
/// code shows it (the colour legend), in code order.
pub fn legend(cfg: &AppConfig, env: &HelpEnv) -> Vec<(BadgeKind, Vec<KindUse>)> {
    let mut out: Vec<(BadgeKind, Vec<KindUse>)> = Vec::new();
    for &code in Code::ALL {
        if !matches!(
            code.family(),
            Family::Messages | Family::Threads | Family::Later
        ) {
            continue;
        }
        let o = evaluate(code, cfg, env);
        let pinned = o.pinned.as_ref();
        let pin = |b: Option<(u64, BadgeKind)>| b.map(|(n, k)| (k, n == 0));
        let row = o
            .row
            .as_ref()
            .and_then(|r| r.kind.map(|k| (k, r.count == 0)));
        let thread = o
            .thread_row
            .as_ref()
            .filter(|t| matches!(t.listing, ThreadListing::Threads { .. }))
            .and_then(|t| t.kind.map(|k| (k, true)));
        let uses = [
            (Surface::Row, row),
            (Surface::ThreadRow, thread),
            (
                Surface::ThreadsButton,
                o.thread_row
                    .as_ref()
                    .and_then(|t| t.header_button.map(|k| (k, true))),
            ),
            (
                Surface::AccountIcon,
                o.account_icon
                    .and_then(|a| a.badge.map(|(n, k)| (k, n == 0))),
            ),
            (Surface::AllUnreads, pinned.and_then(|p| pin(p.all_unreads))),
            (Surface::Activity, pinned.and_then(|p| pin(p.activity))),
            (Surface::Later, pinned.and_then(|p| pin(p.later))),
            (Surface::Drafts, pinned.and_then(|p| pin(p.drafts))),
        ];
        for (surface, u) in uses {
            let Some((kind, dot)) = u else { continue };
            let u = KindUse { code, surface, dot };
            match out.iter_mut().find(|(k, _)| *k == kind) {
                Some((_, v)) => v.push(u),
                None => out.push((kind, vec![u])),
            }
        }
    }
    out
}

/// [`legend`] by code only: every badge kind in use with the codes that
/// show it.
#[cfg(test)]
pub fn kinds_in_use(cfg: &AppConfig, env: &HelpEnv) -> Vec<(BadgeKind, Vec<Code>)> {
    legend(cfg, env)
        .into_iter()
        .map(|(k, uses)| {
            let mut codes: Vec<Code> = Vec::new();
            for u in uses {
                if !codes.contains(&u.code) {
                    codes.push(u.code);
                }
            }
            (k, codes)
        })
        .collect()
}

#[cfg(test)]
#[path = "model_tests.rs"]
mod tests;
