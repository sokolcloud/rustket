//! Desktop notifications of encrypted messages (§6, §7.7): the server sends
//! `text: "<sender>: "` and the encrypted `content`; the text is replaced by
//! the decrypted message, or by "Encrypted message" when it cannot be read.

use rocketchat::session::DesktopNotification;
use rust_i18n::t;
use rustket_core::Workspace;

/// The notification with its text decrypted (unchanged when it is not
/// encrypted).
pub fn prepare(ws: &Workspace, mut n: Box<DesktopNotification>) -> Box<DesktopNotification> {
    let Some(content) = n.payload.message.content.clone() else {
        return n;
    };
    let decrypted = ws.e2e().and_then(|m| {
        m.decrypt_notification(&n.payload.rid, &serde_json::json!({ "content": content }))
    });
    let body = match decrypted {
        Some(text) if !text.trim().is_empty() => text,
        _ => t!("e2e.encrypted_message").into_owned(),
    };
    // Keep the server's "<sender>: " prefix (group rooms), replace the rest.
    let prefix = n.text.trim_end();
    n.text = if prefix.is_empty() {
        body.clone()
    } else if prefix.ends_with(':') {
        format!("{prefix} {body}")
    } else {
        format!("{prefix}: {body}")
    };
    n.payload.message.msg = body;
    n.payload.message.content = None;
    n
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn undecryptable_notifications_say_encrypted() {
        let ws = Workspace::new();
        let mut n = DesktopNotification {
            text: "bob: ".into(),
            ..Default::default()
        };
        n.payload.rid = "r".into();
        n.payload.message.t = Some("e2e".into());
        n.payload.message.content = Some(serde_json::json!({"algorithm": "rc.v2.aes-sha2"}));
        let n = prepare(&ws, Box::new(n));
        assert_eq!(n.text, format!("bob: {}", t!("e2e.encrypted_message")));
        assert!(n.payload.message.content.is_none());
        // Plain notifications are untouched.
        let p = DesktopNotification {
            text: "hello".into(),
            ..Default::default()
        };
        assert_eq!(prepare(&ws, Box::new(p)).text, "hello");
    }
}
