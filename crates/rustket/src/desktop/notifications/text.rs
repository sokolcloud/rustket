//! Pure helpers of the Windows and macOS backends (unit-tested everywhere):
//! toast XML, and the identifiers, categories, response routing and sound
//! names of macOS notifications.

#[cfg(any(windows, test))]
use std::path::Path;

#[cfg(any(windows, test))]
use super::{ACTION_DEFAULT, Outgoing, TIMEOUT_NEVER, URGENCY_CRITICAL};

#[cfg(any(windows, test))]
/// Toast `<audio>` of the message sound.
pub const TOAST_SOUND: &str = "ms-winsoundevent:Notification.IM";

#[cfg(any(windows, test))]
/// Who plays the sound of toast `n`: Windows plays its message sound
/// ([`TOAST_SOUND`]) with every toast that has a sound, so Focus Assist
/// applies (unpackaged apps cannot play their own files in toasts).
pub fn toast_route(n: &Outgoing) -> crate::desktop::sound::Route {
    use crate::desktop::sound::{Platform, Toast, route};
    route(
        Platform::Windows,
        Toast::Shown { plays_sound: true },
        n.sound.as_ref(),
        None,
    )
}

#[cfg(any(windows, test))]
/// Text as XML character data / attribute value: markup is escaped and
/// characters XML 1.0 forbids are dropped.
pub fn escape_xml(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 || c == '\u{fffe}' || c == '\u{ffff}' => {}
            c => out.push(c),
        }
    }
    out
}

#[cfg(any(windows, test))]
/// `file:///` URI of an absolute path (backslashes become slashes).
pub fn file_uri(path: &Path) -> String {
    let p = path.to_string_lossy().replace('\\', "/");
    format!("file:///{}", p.trim_start_matches('/'))
}

#[cfg(any(windows, test))]
/// Toast XML of `n`: clicking the toast activates with [`ACTION_DEFAULT`],
/// every other action is a button whose activation argument is its key.
/// `image`: the avatar file. Calls (critical, never expiring) use the
/// incoming-call scenario, other never-expiring ones stay long.
pub fn toast_xml(n: &Outgoing, image: Option<&Path>) -> String {
    let mut attrs = format!(r#" launch="{}""#, escape_xml(ACTION_DEFAULT));
    if n.timeout == TIMEOUT_NEVER {
        if n.urgency >= URGENCY_CRITICAL {
            attrs.push_str(r#" scenario="incomingCall""#);
        } else {
            attrs.push_str(r#" duration="long""#);
        }
    }
    let mut x = format!(
        r#"<toast{attrs}><visual><binding template="ToastGeneric"><text>{}</text><text>{}</text>"#,
        escape_xml(&n.title),
        escape_xml(&n.body)
    );
    if let Some(img) = image {
        x.push_str(&format!(
            r#"<image placement="appLogoOverride" hint-crop="circle" src="{}"/>"#,
            escape_xml(&file_uri(img))
        ));
    }
    x.push_str("</binding></visual>");
    let buttons: Vec<_> = n
        .actions
        .iter()
        .filter(|(k, _)| k != ACTION_DEFAULT)
        .collect();
    if !buttons.is_empty() {
        x.push_str("<actions>");
        for (key, label) in buttons {
            x.push_str(&format!(
                r#"<action content="{}" arguments="{}" activationType="foreground"/>"#,
                escape_xml(label),
                escape_xml(key)
            ));
        }
        x.push_str("</actions>");
    }
    if toast_route(n).by_os() {
        x.push_str(&format!(r#"<audio src="{TOAST_SOUND}"/>"#));
    } else {
        x.push_str(r#"<audio silent="true"/>"#);
    }
    x.push_str("</toast>");
    x
}

#[cfg(any(target_os = "macos", test))]
/// Prefix of the identifiers of our macOS notifications.
pub const MAC_PREFIX: &str = "rustket-";

#[cfg(any(target_os = "macos", test))]
/// Nonce of this process launch in macOS notification identifiers (no `-`).
pub fn launch_nonce(pid: u32, unix_millis: u128) -> String {
    format!("{pid:x}t{unix_millis:x}")
}

#[cfg(any(target_os = "macos", test))]
/// `UNNotificationRequest.identifier` of notification `id`: posting again
/// with the same identifier replaces the delivered notification.
pub fn mac_identifier(nonce: &str, id: u32) -> String {
    format!("{MAC_PREFIX}{nonce}-{id}")
}

#[cfg(any(target_os = "macos", test))]
/// Our id in `identifier`; `None` for other launches' and foreign ones.
pub fn parse_mac_identifier(nonce: &str, identifier: &str) -> Option<u32> {
    let digits = identifier
        .strip_prefix(MAC_PREFIX)?
        .strip_prefix(nonce)?
        .strip_prefix('-')?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(any(target_os = "macos", test))]
/// Prefix of our `UNNotificationCategory` identifiers.
pub const MAC_CATEGORY_PREFIX: &str = "rustket";

#[cfg(any(target_os = "macos", test))]
/// The `UNNotificationCategory` of a notification's actions: every action
/// but the default (clicking the notification) is a button, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MacCategory<'a> {
    /// `rustket` plus `.<key>` per button: one category per button set.
    pub id: String,
    /// `(key, label)`.
    pub buttons: Vec<(&'a str, &'a str)>,
}

#[cfg(any(target_os = "macos", test))]
pub fn mac_category(actions: &[(String, String)]) -> MacCategory<'_> {
    let buttons: Vec<(&str, &str)> = actions
        .iter()
        .filter(|(k, _)| k != super::ACTION_DEFAULT)
        .map(|(k, l)| (k.as_str(), l.as_str()))
        .collect();
    let mut id = MAC_CATEGORY_PREFIX.to_owned();
    for (k, _) in &buttons {
        id.push('.');
        id.push_str(k);
    }
    MacCategory { id, buttons }
}

#[cfg(any(target_os = "macos", test))]
/// A `UNNotificationResponse`'s `actionIdentifier`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MacResponse<'a> {
    /// `UNNotificationDefaultActionIdentifier`: the notification was clicked.
    Default,
    /// `UNNotificationDismissActionIdentifier`: it was dismissed (sent for
    /// categories with `CustomDismissAction`).
    Dismiss,
    /// A button: our action key.
    Action(&'a str),
}

#[cfg(any(target_os = "macos", test))]
/// The signal of a response.
pub fn mac_signal(response: MacResponse<'_>) -> super::Signal {
    use super::{ACTION_DEFAULT, Signal};
    match response {
        MacResponse::Default => Signal::Action(ACTION_DEFAULT.to_owned()),
        MacResponse::Dismiss => Signal::Closed,
        MacResponse::Action(key) => Signal::Action(key.to_owned()),
    }
}

#[cfg(any(target_os = "macos", test))]
/// The `UNNotificationSound` name of a sound the notification plays: the
/// file name (with its extension) in `Library/Sounds`; `None`: the default
/// sound.
pub fn mac_sound_name(file: &std::path::Path) -> Option<&str> {
    file.file_name().and_then(|n| n.to_str())
}

#[cfg(test)]
mod tests {
    use super::super::{ACTION_JOIN, TIMEOUT_DEFAULT, URGENCY_NORMAL};
    use super::*;

    fn out() -> Outgoing {
        Outgoing {
            replaces: None,
            title: "bob <b>".into(),
            body: "a & b \"c\" 'd'\u{1}\u{7}<script>".into(),
            image: None,
            sound: None,
            actions: vec![(ACTION_DEFAULT.into(), "Open".into())],
            timeout: TIMEOUT_DEFAULT,
            urgency: URGENCY_NORMAL,
        }
    }

    #[test]
    fn text_is_escaped() {
        assert_eq!(
            escape_xml("a<b>&\"'\u{0}\tz"),
            "a&lt;b&gt;&amp;&quot;&apos;\tz"
        );
        let x = toast_xml(&out(), None);
        assert_eq!(
            x,
            r#"<toast launch="default"><visual><binding template="ToastGeneric"><text>bob &lt;b&gt;</text><text>a &amp; b &quot;c&quot; &apos;d&apos;&lt;script&gt;</text></binding></visual><audio silent="true"/></toast>"#
        );
    }

    #[test]
    fn buttons_sound_image_and_scenarios() {
        let mut n = out();
        n.sound = Some(crate::desktop::sound::RcSound {
            name: "highbell".into(),
            volume: 50,
        });
        assert!(toast_route(&n).by_os());
        n.actions.push((ACTION_JOIN.into(), "Join \"now\"".into()));
        n.timeout = TIMEOUT_NEVER;
        n.urgency = URGENCY_CRITICAL;
        let x = toast_xml(&n, Some(Path::new(r"C:\Users\me\a b.png")));
        assert!(x.starts_with(r#"<toast launch="default" scenario="incomingCall">"#));
        assert!(x.contains(
            r#"<action content="Join &quot;now&quot;" arguments="join" activationType="foreground"/>"#
        ));
        assert!(!x.contains(r#"arguments="default""#));
        assert!(x.contains(r#"src="file:///C:/Users/me/a b.png""#));
        assert!(x.contains(r#"<audio src="ms-winsoundevent:Notification.IM"/>"#));
        // The Windows message sound, never the Rocket.Chat sound's file.
        assert!(!x.contains("highbell") && !x.contains(".wav"));
        n.urgency = URGENCY_NORMAL;
        assert!(toast_xml(&n, None).starts_with(r#"<toast launch="default" duration="long">"#));
    }

    #[test]
    fn mac_identifiers_carry_the_launch_nonce() {
        let nonce = launch_nonce(0x1f, 0xabc);
        assert_eq!(nonce, "1ftabc");
        assert!(!nonce.contains('-'));
        let ident = mac_identifier(&nonce, 42);
        assert_eq!(ident, "rustket-1ftabc-42");
        assert_eq!(parse_mac_identifier(&nonce, &ident), Some(42));
        // Another launch, foreign or malformed identifiers.
        assert_eq!(parse_mac_identifier("1ftabd", &ident), None);
        assert_eq!(parse_mac_identifier(&nonce, "rustket-1ftabc1-42"), None);
        assert_eq!(parse_mac_identifier(&nonce, "rustket-1ftabc-"), None);
        assert_eq!(parse_mac_identifier(&nonce, "rustket-1ftabc-+4"), None);
        assert_eq!(parse_mac_identifier(&nonce, "rustket-1ftabc-4x"), None);
        assert_eq!(
            parse_mac_identifier(&nonce, "rustket-1ftabc-99999999999"),
            None
        );
        assert_eq!(
            parse_mac_identifier(&nonce, "5D2E1D3C-1111-2222-3333-444455556666"),
            None
        );
        assert_eq!(file_uri(Path::new("/tmp/a.png")), "file:///tmp/a.png");
    }

    #[test]
    fn mac_categories_and_response_routing() {
        use super::super::Signal;
        let open = (ACTION_DEFAULT.to_owned(), "Open".to_owned());
        let join = (ACTION_JOIN.to_owned(), "Join".to_owned());
        let other = ("other".to_owned(), "Other".to_owned());
        assert_eq!(
            mac_category(std::slice::from_ref(&open)),
            MacCategory {
                id: "rustket".into(),
                buttons: Vec::new()
            }
        );
        assert_eq!(
            mac_category(&[open.clone(), join.clone()]),
            MacCategory {
                id: "rustket.join".into(),
                buttons: vec![("join", "Join")]
            }
        );
        assert_eq!(
            mac_category(&[open, join, other]),
            MacCategory {
                id: "rustket.join.other".into(),
                buttons: vec![("join", "Join"), ("other", "Other")]
            }
        );
        assert_eq!(
            mac_signal(MacResponse::Default),
            Signal::Action("default".into())
        );
        assert_eq!(mac_signal(MacResponse::Dismiss), Signal::Closed);
        assert_eq!(
            mac_signal(MacResponse::Action("join")),
            Signal::Action("join".into())
        );
        assert_eq!(
            mac_sound_name(Path::new("/Users/u/Library/Sounds/rustket-chime-v1-80.wav")),
            Some("rustket-chime-v1-80.wav")
        );
        assert_eq!(mac_sound_name(Path::new("/")), None);
    }
}
