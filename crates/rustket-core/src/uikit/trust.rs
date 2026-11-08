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

/// App id of the core video-conference app.
pub const VIDEOCONF_APP: &str = "videoconf-core";
/// `video_conf` block actions the client handles itself (§3.4).
pub const VIDEOCONF_LOCAL_ACTIONS: [&str; 2] = ["join", "callBack"];

/// What a message block element may do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permit {
    /// Emit the block action (and open the URL of a link button).
    Emit,
    /// Handled by the client, never sent (`video_conf` `join` / `callBack`).
    Local,
    /// Only open the element's URL locally; emit nothing.
    OpenUrlOnly,
    /// Render disabled.
    Disabled,
}

fn resolved(app_id: Option<&str>) -> Option<&str> {
    app_id.filter(|a| !a.is_empty() && *a != FALLBACK_APP_ID)
}

/// Whether a `video_conf` block action is one the client handles itself.
/// A `video_conf` block without an app id acts as `videoconf-core`.
pub fn is_local_videoconf_action(
    in_video_conf_block: bool,
    app_id: Option<&str>,
    action_id: &str,
) -> bool {
    in_video_conf_block
        && resolved(app_id).is_none_or(|a| a == VIDEOCONF_APP)
        && VIDEOCONF_LOCAL_ACTIONS.contains(&action_id)
}

/// Whether the block action `action_id` with the resolved `app_id` may be sent
/// for `msg`. `is_ephemeral` must come from the store (`Workspace::is_ephemeral`);
/// `in_video_conf_block` is true only for the actions of a `video_conf` block.
/// There `videoconf-core` (also the default) may only receive `info`: `join` /
/// `callBack` are handled locally (and `videoconf-core` does not check room
/// access); a `video_conf` block naming another app follows the normal rule.
pub fn actions_allowed(
    msg: &Message,
    is_ephemeral: bool,
    in_video_conf_block: bool,
    app_id: Option<&str>,
    action_id: &str,
) -> bool {
    let app = resolved(app_id);
    if in_video_conf_block && app.is_none_or(|a| a == VIDEOCONF_APP) {
        return action_id == "info";
    }
    match app {
        None => false,
        Some(app) if app.ends_with("-core") => is_ephemeral && msg.u.id == ROCKET_CAT,
        Some(_) => true,
    }
}

/// [`actions_allowed`] for an element: `video_conf` `join` / `callBack` are
/// local, and a `url` button whose action may not be emitted still opens its URL.
pub fn element_permit(
    msg: &Message,
    is_ephemeral: bool,
    in_video_conf_block: bool,
    app_id: Option<&str>,
    action_id: &str,
    has_url: bool,
) -> Permit {
    if is_local_videoconf_action(in_video_conf_block, app_id, action_id) {
        Permit::Local
    } else if actions_allowed(msg, is_ephemeral, in_video_conf_block, app_id, action_id) {
        Permit::Emit
    } else if has_url {
        Permit::OpenUrlOnly
    } else {
        Permit::Disabled
    }
}

/// URL a UIKit link may open: http/https with a host, or mailto. Returns the
/// trimmed URL to hand to the opener.
pub fn openable_url(url: &str) -> Option<&str> {
    let url = url.trim();
    let parsed = url::Url::parse(url).ok()?;
    match parsed.scheme() {
        "http" | "https" => parsed
            .host_str()
            .is_some_and(|h| !h.is_empty())
            .then_some(url),
        "mailto" => (!parsed.path().is_empty()).then_some(url),
        _ => None,
    }
}

/// Whether [`openable_url`] accepts `url`.
pub fn is_openable_url(url: &str) -> bool {
    openable_url(url).is_some()
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
        assert!(actions_allowed(
            &cat,
            true,
            false,
            Some("mention-core"),
            "add-users"
        ));
        // Persisted message from rocket.cat (or a forged sender): disabled.
        assert!(!actions_allowed(
            &cat,
            false,
            false,
            Some("mention-core"),
            "add-users"
        ));
        // Ephemeral but not from rocket.cat.
        assert!(!actions_allowed(
            &msg("alice"),
            true,
            false,
            Some("mention-core"),
            "add-users"
        ));
    }

    #[test]
    fn forged_private_videoconf_message_stays_disabled() {
        let mut m = msg("alice");
        m.t = Some("videoconf".into());
        m.groupable = Some(false);
        assert!(!actions_allowed(
            &m,
            false,
            false,
            Some("videoconf-core"),
            "info"
        ));
        // The video_conf block's `info` stays enabled; `join` / `callBack` are local.
        assert!(actions_allowed(
            &m,
            false,
            true,
            Some("videoconf-core"),
            "info"
        ));
        assert!(actions_allowed(&m, false, true, None, "info"));
        for a in ["join", "callBack", "add-users"] {
            assert!(
                !actions_allowed(&m, false, true, Some("videoconf-core"), a),
                "{a}"
            );
        }
        assert_eq!(
            element_permit(&m, false, true, None, "join", false),
            Permit::Local
        );
        assert_eq!(
            element_permit(&m, false, true, Some("videoconf-core"), "callBack", false),
            Permit::Local
        );
    }

    #[test]
    fn video_conf_block_does_not_exempt_other_core_apps() {
        let m = msg("mallory");
        for app in ["mention-core", "cloud-announcements-core", "nps-core"] {
            assert!(
                !actions_allowed(&m, false, true, Some(app), "add-users"),
                "{app}"
            );
            assert!(
                !actions_allowed(&m, false, true, Some(app), "info"),
                "{app}"
            );
            assert_eq!(
                element_permit(&m, false, true, Some(app), "join", false),
                Permit::Disabled
            );
        }
        // A regular app's video_conf block follows the normal rule.
        assert!(actions_allowed(&m, false, true, Some("some-app"), "join"));
    }

    #[test]
    fn regular_apps_and_missing_ids() {
        let m = msg("alice");
        assert!(actions_allowed(&m, false, false, Some("fake-app-id"), "a"));
        assert!(!actions_allowed(&m, false, false, None, "a"));
        assert!(!actions_allowed(&m, false, false, Some("core"), "a"));
        assert!(!actions_allowed(&m, true, false, Some(""), "a"));
    }

    #[test]
    fn url_buttons_still_open() {
        let m = msg("alice");
        assert_eq!(
            element_permit(
                &m,
                false,
                false,
                Some("videoconf-core"),
                "joinLivechat",
                true
            ),
            Permit::OpenUrlOnly
        );
        assert_eq!(
            element_permit(
                &m,
                false,
                false,
                Some("videoconf-core"),
                "joinLivechat",
                false
            ),
            Permit::Disabled
        );
        assert_eq!(
            element_permit(&m, false, false, Some("app"), "a", true),
            Permit::Emit
        );
    }

    #[test]
    fn openable_urls() {
        assert!(is_openable_url("HTTPS://x.y"));
        assert!(is_openable_url("mailto:a@b"));
        assert_eq!(openable_url("  https://x.y/p "), Some("https://x.y/p"));
        for bad in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "https://",
            "mailto:",
            "smb://h/share",
            "x.desktop",
        ] {
            assert!(!is_openable_url(bad), "{bad}");
        }
    }
}
