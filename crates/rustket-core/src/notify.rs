//! Desktop notifications.
//!
//! Rocket.Chat decides server-side which messages notify a user (DMs, mentions,
//! per-room and account desktop preferences, the server default
//! `Accounts_Default_User_Preferences_desktopNotifications`,
//! `disableNotifications`, the user's own messages…) and pushes the result on
//! the `stream-notify-user` stream as `{uid}/notification`. Whether a plain
//! channel message notifies depends on those preferences (Rocket.Chat's
//! default "all" notifies it). The client only applies local suppression
//! rules here, plus a fallback for mentions in large rooms, where the server
//! sends nothing ([`large_room`]).
//!
//! Payload (8.9):
//! `{title, text, duration?, payload: {_id, rid, tmid?, sender: {_id, username, name},
//! type, name, message: {msg, t?}, senderId?}}`.

use rocketchat::models::{RoomType, UserRef, UserStatus};
use serde::{Deserialize, Serialize};

pub mod large_room;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationMessage {
    pub msg: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationPayload {
    /// Message id.
    #[serde(rename = "_id")]
    pub id: String,
    pub rid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tmid: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "rocketchat::models::lenient::opt"
    )]
    pub sender: Option<UserRef>,
    #[serde(
        rename = "type",
        skip_serializing_if = "Option::is_none",
        deserialize_with = "rocketchat::models::lenient::opt"
    )]
    pub room_type: Option<RoomType>,
    /// Room name (absent for DMs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(
        skip_serializing_if = "Option::is_none",
        deserialize_with = "rocketchat::models::lenient::opt"
    )]
    pub message: Option<NotificationMessage>,
}

/// The `{uid}/notification` event argument.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct IncomingNotification {
    pub title: String,
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration: Option<u64>,
    pub payload: NotificationPayload,
}

impl IncomingNotification {
    pub fn rid(&self) -> &str {
        &self.payload.rid
    }

    pub fn sender_id(&self) -> Option<&str> {
        self.payload.sender.as_ref().map(|s| s.id.as_str())
    }
}

/// Local notification preferences (from [`crate::config::UiPrefs`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationPrefs {
    pub enabled: bool,
    pub show_preview: bool,
    pub sound: bool,
}

impl From<&crate::config::UiPrefs> for NotificationPrefs {
    fn from(p: &crate::config::UiPrefs) -> Self {
        Self {
            enabled: p.desktop_notifications,
            show_preview: p.notification_preview,
            sound: p.sound,
        }
    }
}

/// What the app currently shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotifyContext<'a> {
    pub my_user_id: &'a str,
    pub my_status: UserStatus,
    /// Room open in the active server's main view.
    pub focused_rid: Option<&'a str>,
    /// Thread open in the side panel.
    pub focused_tmid: Option<&'a str>,
    /// The window is visible and has input focus.
    pub window_active: bool,
    /// The notification's server is the one shown.
    pub server_active: bool,
    pub prefs: NotificationPrefs,
    /// Notifications are paused (by hand or outside working hours,
    /// [`crate::quiet`]).
    pub paused: bool,
    /// The account's `muteFocusedConversations`: no notification for the
    /// conversation on screen.
    pub mute_focused: bool,
    /// The notified thread reply is also shown in the room's main timeline
    /// (sent to the channel, `tshow`).
    pub reply_in_room: bool,
}

/// A notification to show.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationSpec {
    pub title: String,
    /// Message text, or `None` when previews are disabled (the UI shows a generic
    /// "new message" string from i18n key [`HIDDEN_BODY_KEY`]).
    pub body: Option<String>,
    pub rid: String,
    pub mid: String,
    pub tmid: Option<String>,
    /// Show the notification (desktop notifications are on).
    pub toast: bool,
    /// Play the message sound (with or without the notification).
    pub sound: bool,
}

/// i18n key for the body when previews are disabled.
pub const HIDDEN_BODY_KEY: &str = "notification.new_message";

/// Applies local suppression rules: own messages, paused notifications,
/// busy (do not disturb), and, with
/// [`NotifyContext::mute_focused`], the conversation being on screen in an
/// active window: the open room for its messages, the open thread panel for
/// its replies (a reply also counts in the open room when it is shown in the
/// main timeline). What passes shows a notification when desktop
/// notifications are on and plays the sound when sound is on, each without
/// the other; `None` when neither is on.
pub fn evaluate(n: &IncomingNotification, ctx: &NotifyContext<'_>) -> Option<NotificationSpec> {
    if ctx.paused || !(ctx.prefs.enabled || ctx.prefs.sound) {
        return None;
    }
    if n.sender_id() == Some(ctx.my_user_id) {
        return None;
    }
    if ctx.my_status == UserStatus::Busy {
        return None;
    }
    let p = &n.payload;
    if ctx.mute_focused && ctx.window_active && ctx.server_active {
        let in_room = ctx.focused_rid == Some(p.rid.as_str());
        let visible = match p.tmid.as_deref() {
            Some(tmid) => ctx.focused_tmid == Some(tmid) || (in_room && ctx.reply_in_room),
            None => in_room,
        };
        if visible {
            return None;
        }
    }
    let title = if n.title.is_empty() {
        p.sender
            .as_ref()
            .and_then(|s| s.name.clone().or(s.username.clone()))
            .unwrap_or_default()
    } else {
        n.title.clone()
    };
    let body = ctx.prefs.show_preview.then(|| {
        if n.text.is_empty() {
            p.message
                .as_ref()
                .map(|m| m.msg.clone())
                .unwrap_or_default()
        } else {
            n.text.clone()
        }
    });
    Some(NotificationSpec {
        title,
        body,
        rid: p.rid.clone(),
        mid: p.id.clone(),
        tmid: p.tmid.clone(),
        toast: ctx.prefs.enabled,
        sound: ctx.prefs.sound,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn sample() -> IncomingNotification {
        serde_json::from_value(json!({
            "title": "@bob",
            "text": "hello there",
            "duration": 4000,
            "payload": {
                "_id": "mid1",
                "rid": "rid1",
                "sender": {"_id": "bobid", "username": "bob", "name": "Bob"},
                "type": "d",
                "message": {"msg": "hello there"}
            }
        }))
        .unwrap()
    }

    fn ctx() -> NotifyContext<'static> {
        NotifyContext {
            my_user_id: "me",
            my_status: UserStatus::Online,
            focused_rid: None,
            focused_tmid: None,
            window_active: true,
            server_active: true,
            prefs: NotificationPrefs {
                enabled: true,
                show_preview: true,
                sound: true,
            },
            paused: false,
            mute_focused: true,
            reply_in_room: false,
        }
    }

    #[test]
    fn parses_server_payload() {
        let n = sample();
        assert_eq!(n.rid(), "rid1");
        assert_eq!(n.payload.room_type, Some(RoomType::Direct));
        assert_eq!(n.sender_id(), Some("bobid"));
        // Tolerates garbage in nested fields.
        let n: IncomingNotification =
            serde_json::from_value(json!({"payload": {"rid": "r", "sender": 5, "type": []}}))
                .unwrap();
        assert_eq!(n.rid(), "r");
        assert!(n.payload.sender.is_none());
    }

    #[test]
    fn shows_by_default() {
        let s = evaluate(&sample(), &ctx()).unwrap();
        assert_eq!(s.title, "@bob");
        assert_eq!(s.body.as_deref(), Some("hello there"));
        assert_eq!((s.rid.as_str(), s.mid.as_str()), ("rid1", "mid1"));
        assert!(s.toast && s.sound);
    }

    #[test]
    fn suppression_rules() {
        let n = sample();
        let mut c = ctx();
        c.prefs.enabled = false;
        // Sound without notifications: the sound alone.
        let s = evaluate(&n, &c).unwrap();
        assert!(!s.toast && s.sound);
        c.prefs.sound = false;
        assert!(evaluate(&n, &c).is_none());
        let mut c = ctx();
        c.prefs.sound = false;
        let s = evaluate(&n, &c).unwrap();
        assert!(s.toast && !s.sound);
        c.prefs.enabled = false;
        c.prefs.sound = true;
        c.my_status = UserStatus::Busy;
        assert!(
            evaluate(&n, &c).is_none(),
            "the sound alone follows the gates"
        );

        let mut c = ctx();
        c.my_user_id = "bobid";
        assert!(evaluate(&n, &c).is_none(), "own message");

        let mut c = ctx();
        c.my_status = UserStatus::Busy;
        assert!(evaluate(&n, &c).is_none(), "do not disturb");

        let mut c = ctx();
        c.window_active = false;
        c.paused = true;
        assert!(evaluate(&n, &c).is_none(), "paused");

        let mut c = ctx();
        c.focused_rid = Some("rid1");
        assert!(evaluate(&n, &c).is_none(), "room on screen");
        c.window_active = false;
        assert!(evaluate(&n, &c).is_some(), "window inactive");
        c.window_active = true;
        c.server_active = false;
        assert!(evaluate(&n, &c).is_some(), "other server shown");

        let mut c = ctx();
        c.prefs.show_preview = false;
        c.prefs.sound = false;
        let s = evaluate(&n, &c).unwrap();
        assert_eq!(s.body, None);
        assert!(!s.sound);
    }

    #[test]
    fn threads() {
        let mut n = sample();
        n.payload.tmid = Some("t1".into());
        let mut c = ctx();
        c.focused_rid = Some("rid1");
        assert!(
            evaluate(&n, &c).is_some(),
            "room open, no thread panel: the reply is not on screen"
        );
        c.reply_in_room = true;
        assert!(evaluate(&n, &c).is_none(), "sent to the channel too");
        c.reply_in_room = false;
        c.focused_tmid = Some("other");
        assert!(evaluate(&n, &c).is_some(), "different thread open");
        c.reply_in_room = true;
        assert!(
            evaluate(&n, &c).is_none(),
            "shown in the room beside another thread"
        );
        c.reply_in_room = false;
        c.focused_tmid = Some("t1");
        assert!(evaluate(&n, &c).is_none());
        c.focused_rid = Some("x");
        assert!(evaluate(&n, &c).is_none(), "thread panel shows it");
        c.window_active = false;
        assert!(evaluate(&n, &c).is_some(), "window inactive");
    }

    /// `muteFocusedConversations` off: the conversation on screen notifies
    /// too, own messages still do not.
    #[test]
    fn focused_conversations_not_muted() {
        let n = sample();
        let mut c = ctx();
        c.focused_rid = Some("rid1");
        c.mute_focused = false;
        let s = evaluate(&n, &c).unwrap();
        assert!(s.sound);
        let mut t = sample();
        t.payload.tmid = Some("t1".into());
        c.focused_tmid = Some("t1");
        assert!(evaluate(&t, &c).is_some());
        c.my_user_id = "bobid";
        assert!(evaluate(&n, &c).is_none(), "own message");
        let mut c = ctx();
        c.focused_rid = Some("rid1");
        c.mute_focused = false;
        c.my_status = UserStatus::Busy;
        assert!(evaluate(&n, &c).is_none(), "Busy still applies");
    }
}
