//! Which message block actions may be emitted (docs/uikit.md §3.2).
//!
//! Anyone can post `blocks` carrying a real or core app id, and `private`,
//! `groupable`, `t` and `_id` of a message are client-chosen. Only messages
//! delivered on `stream-notify-user` `{uid}/message` (ephemeral, never
//! persisted, writable only by the server) can prove a core-app origin, so the
//! "ephemeral" flag comes from the store, never from message fields.

use rocketchat::models::Message;

/// Sender of server-generated ephemeral messages.
pub const ROCKET_CAT: &str = "rocket.cat";
/// App id the web falls back to when an element has none; no app has it.
pub const FALLBACK_APP_ID: &str = "core";

/// What a message block element may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permit {
    /// Emit the block action (and open the URL of a link button).
    Emit,
    /// Only open the element's URL locally; emit nothing.
    OpenUrlOnly,
    /// Render disabled.
    Disabled,
}

/// Whether a block action with the resolved `app_id` may be sent for `msg`.
/// `is_ephemeral` must come from the store (`Workspace::is_ephemeral`);
/// `in_video_conf_block` is true only for the actions of a `video_conf` block
/// (`join`/`callBack` are handled locally, `info` only reads call info; never
/// send `join` to `videoconf-core`, which does not check room access).
pub fn actions_allowed(
    msg: &Message,
    is_ephemeral: bool,
    in_video_conf_block: bool,
    app_id: Option<&str>,
) -> bool {
    if in_video_conf_block {
        return true;
    }
    match app_id.filter(|a| !a.is_empty() && *a != FALLBACK_APP_ID) {
        None => false,
        Some(app) if app.ends_with("-core") => is_ephemeral && msg.u.id == ROCKET_CAT,
        Some(_) => true,
    }
}

/// [`actions_allowed`] for an element: a `url` button whose action may not be
/// emitted still opens its URL.
pub fn element_permit(
    msg: &Message,
    is_ephemeral: bool,
    in_video_conf_block: bool,
    app_id: Option<&str>,
    has_url: bool,
) -> Permit {
    if actions_allowed(msg, is_ephemeral, in_video_conf_block, app_id) {
        Permit::Emit
    } else if has_url {
        Permit::OpenUrlOnly
    } else {
        Permit::Disabled
    }
}

/// URL schemes a UIKit link may open.
pub fn is_openable_url(url: &str) -> bool {
    let lower = url.trim().to_ascii_lowercase();
    ["http://", "https://", "mailto:"]
        .iter()
        .any(|s| lower.starts_with(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rocketchat::models::UserRef;

    fn msg(uid: &str) -> Message {
        Message {
            id: "1794088390781".into(),
            rid: "r".into(),
            u: UserRef {
                id: uid.into(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    #[test]
    fn core_apps_need_a_server_ephemeral() {
        let cat = msg(ROCKET_CAT);
        assert!(actions_allowed(&cat, true, false, Some("mention-core")));
        // Persisted message from rocket.cat (or a forged sender): disabled.
        assert!(!actions_allowed(&cat, false, false, Some("mention-core")));
        // Ephemeral but not from rocket.cat.
        assert!(!actions_allowed(
            &msg("alice"),
            true,
            false,
            Some("mention-core")
        ));
    }

    #[test]
    fn forged_private_videoconf_message_stays_disabled() {
        let mut m = msg("alice");
        m.t = Some("videoconf".into());
        m.groupable = Some(false);
        assert!(!actions_allowed(&m, false, false, Some("videoconf-core")));
        // The video_conf block's own actions stay enabled.
        assert!(actions_allowed(&m, false, true, Some("videoconf-core")));
    }

    #[test]
    fn regular_apps_and_missing_ids() {
        let m = msg("alice");
        assert!(actions_allowed(&m, false, false, Some("fake-app-id")));
        assert!(!actions_allowed(&m, false, false, None));
        assert!(!actions_allowed(&m, false, false, Some("core")));
        assert!(!actions_allowed(&m, true, false, Some("")));
    }

    #[test]
    fn url_buttons_still_open() {
        let m = msg("alice");
        assert_eq!(
            element_permit(&m, false, false, Some("videoconf-core"), true),
            Permit::OpenUrlOnly
        );
        assert_eq!(
            element_permit(&m, false, false, Some("videoconf-core"), false),
            Permit::Disabled
        );
        assert_eq!(
            element_permit(&m, false, false, Some("app"), true),
            Permit::Emit
        );
        assert!(is_openable_url("HTTPS://x.y"));
        assert!(is_openable_url("mailto:a@b"));
        assert!(!is_openable_url("javascript:alert(1)"));
        assert!(!is_openable_url("file:///etc/passwd"));
    }
}
