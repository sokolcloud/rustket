//! Pure helpers of the Windows and macOS backends (unit-tested everywhere):
//! toast XML and the application a macOS notification is posted as.

use std::path::Path;

#[cfg(any(windows, test))]
use super::{ACTION_DEFAULT, Outgoing, TIMEOUT_NEVER, URGENCY_CRITICAL};

#[cfg(any(windows, test))]
/// Toast `<audio>` of the message sound.
pub const TOAST_SOUND: &str = "ms-winsoundevent:Notification.IM";

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
    if n.sound {
        x.push_str(&format!(r#"<audio src="{TOAST_SOUND}"/>"#));
    } else {
        x.push_str(r#"<audio silent="true"/>"#);
    }
    x.push_str("</toast>");
    x
}

#[cfg(any(target_os = "macos", test))]
/// Bundle id notifications are posted as on macOS: ours when running from
/// the app bundle (`*.app/Contents/MacOS/<exe>`), otherwise Terminal's so
/// development builds still show notifications.
pub fn mac_application(exe: &Path) -> &'static str {
    let named = |p: Option<&Path>, name: &str| {
        p.and_then(|p| p.file_name())
            .is_some_and(|n| n.eq_ignore_ascii_case(name))
    };
    let macos = exe.parent();
    let contents = macos.and_then(Path::parent);
    let app = contents.and_then(Path::parent);
    let bundled = named(macos, "MacOS")
        && named(contents, "Contents")
        && app
            .and_then(|a| a.extension())
            .is_some_and(|e| e.eq_ignore_ascii_case("app"));
    if bundled {
        crate::BUNDLE_ID
    } else {
        "com.apple.Terminal"
    }
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
            sound: false,
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
        n.sound = true;
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
        n.urgency = URGENCY_NORMAL;
        assert!(toast_xml(&n, None).starts_with(r#"<toast launch="default" duration="long">"#));
    }

    #[test]
    fn bundle_detection() {
        assert_eq!(
            mac_application(Path::new(
                "/Applications/Rustket.app/Contents/MacOS/rustket"
            )),
            crate::BUNDLE_ID
        );
        assert_eq!(
            mac_application(Path::new("/Users/me/src/target/release/rustket")),
            "com.apple.Terminal"
        );
        assert_eq!(
            mac_application(Path::new("/x/Contents/MacOS/rustket")),
            "com.apple.Terminal"
        );
        assert_eq!(file_uri(Path::new("/tmp/a.png")), "file:///tmp/a.png");
    }
}
