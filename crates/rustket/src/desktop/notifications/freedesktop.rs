//! Freedesktop backend: `org.freedesktop.Notifications` over the shared
//! zbus connection. `ActionInvoked` / `NotificationClosed` come from one
//! app-wide signal subscription and are routed by id.

use std::collections::HashMap;

use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt};
use rust_i18n::t;

use super::{Event, Message, Notifier, Outgoing, Signal};

/// The real backend: `org.freedesktop.Notifications` on the shared session
/// bus connection.
#[derive(Debug, Default)]
pub struct ZbusNotifier;

const NOTIFICATIONS: &str = "org.freedesktop.Notifications";
const NOTIFICATIONS_PATH: &str = "/org/freedesktop/Notifications";

static CAPABILITIES: tokio::sync::OnceCell<Vec<String>> = tokio::sync::OnceCell::const_new();

/// The server's capabilities (cached once a server answered).
async fn capabilities() -> Vec<String> {
    if let Some(c) = CAPABILITIES.get() {
        return c.clone();
    }
    let caps = match super::super::dbus::call(
        NOTIFICATIONS,
        NOTIFICATIONS_PATH,
        NOTIFICATIONS,
        "GetCapabilities",
        &(),
    )
    .await
    {
        Some(m) => m.body().deserialize::<Vec<String>>().unwrap_or_default(),
        None => return Vec::new(),
    };
    let _ = CAPABILITIES.set(caps.clone());
    caps
}

/// Escapes text for servers that parse the body as markup.
pub fn escape_markup(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            c => out.push(c),
        }
    }
    out
}

type Hints = HashMap<&'static str, zbus::zvariant::Value<'static>>;

/// Hints of `n` (`server_sound`: the server plays sound hints).
fn hints(n: &Outgoing, server_sound: bool) -> Hints {
    use zbus::zvariant::{Structure, Value};
    let mut h = Hints::new();
    h.insert("category", Value::from("im.received"));
    h.insert("urgency", Value::U8(n.urgency));
    if n.sound && server_sound {
        h.insert(
            "sound-name",
            Value::from(super::super::sound::MESSAGE_SOUND),
        );
    } else {
        h.insert("suppress-sound", Value::Bool(true));
    }
    if let Some((w, height, px)) = &n.image
        && let (Ok(w), Ok(height)) = (i32::try_from(*w), i32::try_from(*height))
        && i64::from(w) * i64::from(height) * 4 == px.len() as i64
    {
        let s = Structure::from((w, height, w * 4, true, 8i32, 4i32, px.clone()));
        h.insert("image-data", Value::Structure(s));
    }
    h
}

type NotifyArgs<'a> = (
    &'a str,
    u32,
    &'a str,
    &'a str,
    String,
    Vec<&'a str>,
    Hints,
    i32,
);

/// Arguments of `Notify` (`caps`: the server's capabilities).
fn notify_args<'a>(n: &'a Outgoing, app_name: &'a str, caps: &[String]) -> NotifyArgs<'a> {
    let server_sound = caps.iter().any(|c| c == "sound");
    let body = if caps.iter().any(|c| c == "body-markup") {
        escape_markup(&n.body)
    } else {
        n.body.clone()
    };
    let actions = n
        .actions
        .iter()
        .flat_map(|(k, l)| [k.as_str(), l.as_str()])
        .collect();
    (
        app_name,
        n.replaces.unwrap_or(0),
        "",
        n.title.as_str(),
        body,
        actions,
        hints(n, server_sound),
        n.timeout,
    )
}

impl Notifier for ZbusNotifier {
    fn show(&self, n: Outgoing) -> BoxFuture<'static, Event> {
        async move {
            let caps = capabilities().await;
            let server_sound = caps.iter().any(|c| c == "sound");
            let app_name = t!("desktop.app_name");
            let args = notify_args(&n, &app_name, &caps);
            match super::super::dbus::call(
                NOTIFICATIONS,
                NOTIFICATIONS_PATH,
                NOTIFICATIONS,
                "Notify",
                &args,
            )
            .await
            {
                Some(reply) => match reply.body().deserialize::<u32>() {
                    Ok(id) => Event::Shown { id, server_sound },
                    Err(e) => Event::Failed(e.to_string()),
                },
                None => Event::Failed("Notify failed".into()),
            }
        }
        .boxed()
    }

    fn close(&self, id: u32) -> BoxFuture<'static, ()> {
        async move {
            let _ = super::super::dbus::call(
                NOTIFICATIONS,
                NOTIFICATIONS_PATH,
                NOTIFICATIONS,
                "CloseNotification",
                &(id,),
            )
            .await;
        }
        .boxed()
    }

    fn listens(&self) -> bool {
        true
    }
}

/// `ActionInvoked` / `NotificationClosed` of every notification (one match
/// rule on the shared connection).
pub(super) fn signals() -> impl futures::Stream<Item = Message> {
    iced::stream::channel(
        32,
        async |mut out: futures::channel::mpsc::Sender<Message>| {
            use futures::SinkExt;
            let Some(bus) = super::super::dbus::session_bus().await else {
                return;
            };
            let rule = zbus::MatchRule::builder()
                .msg_type(zbus::message::Type::Signal)
                .sender(NOTIFICATIONS)
                .and_then(|b| b.interface(NOTIFICATIONS))
                .and_then(|b| b.path(NOTIFICATIONS_PATH))
                .map(|b| b.build());
            let rule = match rule {
                Ok(r) => r,
                Err(e) => {
                    tracing::debug!(error = %e, "notification match rule");
                    return;
                }
            };
            let mut stream = match zbus::MessageStream::for_match_rule(rule, &bus, Some(64)).await {
                Ok(s) => s,
                Err(e) => {
                    tracing::debug!(error = %e, "cannot watch notification signals");
                    return;
                }
            };
            while let Some(m) = stream.next().await {
                let Ok(m) = m else { continue };
                let header = m.header();
                let msg = match header.member().map(|m| m.as_str()) {
                    Some("ActionInvoked") => {
                        m.body()
                            .deserialize::<(u32, String)>()
                            .ok()
                            .map(|(id, key)| Message::Signal {
                                id,
                                signal: Signal::Action(key),
                            })
                    }
                    Some("NotificationClosed") => {
                        m.body()
                            .deserialize::<(u32, u32)>()
                            .ok()
                            .map(|(id, _)| Message::Signal {
                                id,
                                signal: Signal::Closed,
                            })
                    }
                    _ => None,
                };
                if let Some(msg) = msg
                    && out.send(msg).await.is_err()
                {
                    break;
                }
            }
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::super::*;
    use super::*;

    #[test]
    fn markup_is_escaped_and_hints_are_built() {
        assert_eq!(
            escape_markup(r#"x <b>bold</b> & <a href="u">y</a>"#),
            r#"x &lt;b&gt;bold&lt;/b&gt; &amp; &lt;a href="u"&gt;y&lt;/a&gt;"#
        );
        let out = Outgoing {
            replaces: None,
            title: "t".into(),
            body: "b".into(),
            image: Some((2, 1, vec![0; 8])),
            sound: true,
            actions: Vec::new(),
            timeout: TIMEOUT_DEFAULT,
            urgency: URGENCY_NORMAL,
        };
        let h = hints(&out, true);
        assert!(h.contains_key("sound-name") && h.contains_key("image-data"));
        let h = hints(&out, false);
        assert!(h.contains_key("suppress-sound") && !h.contains_key("sound-name"));
        let bad = Outgoing {
            image: Some((2, 2, vec![0; 8])),
            ..out
        };
        assert!(!hints(&bad, false).contains_key("image-data"));
    }

    #[test]
    fn notify_call_signature() {
        use zbus::zvariant::DynamicType;
        let out = Outgoing {
            replaces: Some(3),
            title: "t".into(),
            body: "a < b".into(),
            image: Some((1, 1, vec![0; 4])),
            sound: false,
            actions: vec![("default".into(), "Open".into())],
            timeout: TIMEOUT_DEFAULT,
            urgency: URGENCY_NORMAL,
        };
        let caps = vec!["body-markup".to_owned()];
        let args = notify_args(&out, "Rustket", &caps);
        assert_eq!(args.signature().to_string(), "(susssasa{sv}i)");
        assert_eq!(args.4, "a &lt; b");
        assert_eq!(args.5, ["default", "Open"]);
        assert_eq!(
            args.6["image-data"].value_signature().to_string(),
            "(iiibiiay)"
        );
        assert_eq!(notify_args(&out, "Rustket", &[]).4, "a < b");
    }
}
