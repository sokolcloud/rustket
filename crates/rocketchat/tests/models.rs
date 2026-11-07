//! Deserialisation of responses captured from the Rocket.Chat 8.9.1 dev server.

#![allow(clippy::unwrap_used)]

use rocketchat::models::md::{self, Block, Inline};
use rocketchat::models::*;
use rocketchat::rest::LoginResult;
use rocketchat::{Error, RcDate, TwoFactorMethod};
use serde::de::DeserializeOwned;
use serde_json::Value;

fn raw(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"));
    serde_json::from_str(&text).unwrap()
}

fn get<T: DeserializeOwned>(name: &str, key: &str) -> T {
    let v = raw(name);
    let inner = if key.is_empty() { v } else { v[key].clone() };
    serde_json::from_value(inner).unwrap_or_else(|e| panic!("{name}.{key}: {e}"))
}

fn messages(name: &str) -> Vec<Message> {
    let v = raw(name);
    let list = if v.get("result").is_some() {
        v["result"]["messages"].clone()
    } else {
        v["messages"].clone()
    };
    serde_json::from_value(list).unwrap_or_else(|e| panic!("{name}: {e}"))
}

fn by_msg<'a>(msgs: &'a [Message], text: &str) -> &'a Message {
    msgs.iter()
        .find(|m| m.msg.starts_with(text))
        .unwrap_or_else(|| panic!("no message starting with {text:?}"))
}

fn first_inlines(m: &Message) -> &[Inline] {
    match &m.md.as_ref().expect("md")[0] {
        Block::Paragraph { value } => value,
        b => panic!("expected paragraph, got {b:?}"),
    }
}

/// Every md node anywhere in the fixtures must be a known variant, and must
/// round-trip unchanged.
fn assert_no_unknown(v: &Value) {
    let msg: Message = serde_json::from_value(v.clone()).unwrap();
    let roots = std::iter::once(&msg.md)
        .chain(
            msg.attachments
                .iter()
                .flatten()
                .flat_map(|a| [&a.md, &a.description_md]),
        )
        .flatten();
    for md in roots {
        assert!(
            !md::contains_unknown(md),
            "unknown md node in {:?}: {md:?}",
            msg.msg
        );
    }
    let json = strip_nulls(serde_json::to_value(&msg.md).unwrap());
    let orig = strip_nulls(v.get("md").cloned().unwrap_or(Value::Null));
    assert_eq!(json, orig, "md round trip for {}", msg.msg);
}

fn strip_nulls(v: Value) -> Value {
    match v {
        Value::Object(m) => Value::Object(
            m.into_iter()
                .filter(|(_, v)| !v.is_null())
                .map(|(k, v)| (k, strip_nulls(v)))
                .collect(),
        ),
        Value::Array(a) => Value::Array(a.into_iter().map(strip_nulls).collect()),
        other => other,
    }
}

#[test]
fn server_info() {
    let i: ServerInfo = get("info", "");
    assert_eq!(i.version.as_deref(), Some("8.9"));
    assert_eq!(i.version_tuple(), Some((8, 9, 0)));
    assert!(i.supported_versions.is_some());
    assert_eq!(parse_version("8.9.1-rc.2"), Some((8, 9, 1)));
}

#[test]
fn settings() {
    let s: Vec<Setting> = get("settings_public_page", "settings");
    assert_eq!(s.len(), 25);
    assert_eq!(
        raw("settings_public_page")["total"]
            .as_u64()
            .map(|t| t > 300),
        Some(true)
    );
    assert!(s.iter().all(|x| !x.id.is_empty()));
}

#[test]
fn login_services_real() {
    let s: Vec<LoginServiceConfig> = get("settings_oauth", "services");
    let names: Vec<&str> = s.iter().map(LoginServiceConfig::service_name).collect();
    for n in ["cas", "gitlab", "google", "github", "keycloak", "saml"] {
        assert!(names.contains(&n), "{n} in {names:?}");
    }
    let kc = s.iter().find(|x| x.service_name() == "keycloak").unwrap();
    assert_eq!(kc.kind(), LoginServiceKind::CustomOAuth);
    assert_eq!(kc.client_id(), Some("rocketchat"));
    assert_eq!(kc.authorize_path.as_deref(), Some("/auth"));
    assert_eq!(kc.scope.as_deref(), Some("openid email profile"));
    assert_eq!(kc.login_style.as_deref(), Some("redirect"));
    assert!(kc.is_visible());
    let google = s.iter().find(|x| x.service_name() == "google").unwrap();
    assert_eq!(google.kind(), LoginServiceKind::OAuth);
    assert_eq!(google.button_label(), "Google");
    let saml = s.iter().find(|x| x.service_name() == "saml").unwrap();
    assert_eq!(saml.kind(), LoginServiceKind::Saml);
    assert_eq!(
        saml.client_config.as_ref().unwrap().provider.as_deref(),
        Some("rustket-saml")
    );
    assert_eq!(
        saml.entry_point.as_deref(),
        Some("https://idp.example.com/saml/sso")
    );
    assert_eq!(saml.button_label(), "Company SSO");
    let cas = s.iter().find(|x| x.service_name() == "cas").unwrap();
    assert_eq!(cas.kind(), LoginServiceKind::Cas);
    assert_eq!(
        cas.login_url.as_deref(),
        Some("https://cas.example.com/cas/login")
    );
}

#[test]
fn login_services_synthetic() {
    let s: Vec<LoginServiceConfig> = get("settings_oauth_synthetic", "services");
    assert_eq!(s.len(), 8);
    assert_eq!(s[0].client_id(), Some("fb-app-id"));
    assert_eq!(s[1].client_id(), Some("tw-consumer-key"));
    assert_eq!(
        s[2].client_config
            .as_ref()
            .unwrap()
            .request_permissions
            .as_ref()
            .unwrap()
            .len(),
        2
    );
    assert!(!s[6].is_visible());
    assert_eq!(s[7].service_name(), "future-kind");
}

#[test]
fn login_responses() {
    let ok: LoginResult = get("login_success", "data");
    assert_eq!(ok.me.as_ref().unwrap().username.as_deref(), Some("alice"));
    assert!(!ok.auth_token.is_empty());
    let me = ok.me.unwrap();
    assert_eq!(me.preferences().unwrap()["sendOnEnter"], "normal");

    let body = serde_json::to_vec(&raw("login_totp_required_email")).unwrap();
    match Error::from_response(401, None, &body) {
        Error::TwoFactorRequired(c) => {
            assert_eq!(c.method, TwoFactorMethod::Email);
            assert_eq!(c.email_or_username.as_deref(), Some("carol"));
        }
        e => panic!("{e:?}"),
    }
    let body = serde_json::to_vec(&raw("login_unauthorized")).unwrap();
    assert!(Error::from_response(401, None, &body).is_unauthorized());
    let body = serde_json::to_vec(&raw("error_not_logged_in")).unwrap();
    assert!(Error::from_response(401, None, &body).is_unauthorized());
    let body = serde_json::to_vec(&raw("error_invalid_room")).unwrap();
    assert_eq!(
        Error::from_response(400, None, &body).api_error_code(),
        Some("error-room-not-found")
    );
}

#[test]
fn me_and_users() {
    let me: User = get("me", "");
    assert_eq!(me.username.as_deref(), Some("alice"));
    assert_eq!(me.status, Some(UserStatus::Offline));
    assert!(me.has_role("user"));
    assert_eq!(
        me.emails.as_ref().unwrap()[0].address,
        "alice@rustket.local"
    );
    assert!(!me.totp_enabled());
    let bob: User = get("users_info", "user");
    assert_eq!(bob.display_name(), "Bob");
    assert_eq!(bob.active, Some(true));
    let p: Presence = get("users_presence", "");
    assert!(p.full);
    let ac: Vec<UserSummary> = get("users_autocomplete", "items");
    assert_eq!(ac[0].username.as_deref(), Some("alice"));
    let sp: SpotlightResult = get("spotlight", "");
    assert_eq!(sp.users[0].username.as_deref(), Some("bob"));
    let rooms: Vec<RoomSummary> = get("rooms_autocomplete", "items");
    assert!(rooms.iter().any(|r| r.t == RoomType::Private));
    let members: Vec<UserSummary> = get("room_members", "members");
    assert_eq!(members[0].roles.as_deref(), Some(&["owner".to_owned()][..]));
    let members: Vec<UserSummary> = get("channels_members", "members");
    assert_eq!(members.len(), 4);
}

#[test]
fn subscriptions_and_rooms() {
    let subs: UpdatedSince<Subscription> = get("subscriptions_get", "");
    assert!(!subs.update.is_empty());
    let general = subs.update.iter().find(|s| s.rid == "GENERAL").unwrap();
    assert_eq!(general.t, RoomType::Channel);
    assert!(general.open);
    let _: UpdatedSince<Subscription> = get("subscriptions_get_updated_since", "");
    let one: Subscription = get("subscriptions_get_one", "subscription");
    assert_eq!(one.u.username.as_deref(), Some("bob"));

    let rooms: UpdatedSince<Room> = get("rooms_get", "");
    assert!(rooms.update.iter().any(|r| r.t == RoomType::Direct));
    assert!(rooms.update.iter().any(|r| r.t == RoomType::Private));
    let fixture = rooms
        .update
        .iter()
        .find(|r| r.name.as_deref().is_some_and(|n| n.starts_with("fixture-")))
        .unwrap();
    assert_eq!(fixture.topic.as_deref(), Some("fixture topic"));
    assert_eq!(
        fixture.announcement.as_deref(),
        Some("fixture announcement")
    );
    assert!(fixture.last_message.as_ref().unwrap().urls.is_some());
    assert!(rooms.update.iter().any(Room::is_discussion));
    let _: UpdatedSince<Room> = get("rooms_get_updated_since", "");
    let info: Room = get("rooms_info", "room");
    assert_eq!(info.description.as_deref(), Some("fixture description"));
    let created: Room = get("channels_create", "channel");
    assert_eq!(created.ro, Some(false));
    let disc: Room = get("rooms_create_discussion", "discussion");
    assert!(disc.is_discussion());
    assert_eq!(
        disc.display_name(),
        created.display_name().to_owned() + "-discussion"
    );
}

#[test]
fn history_rest_and_ddp_dates_agree() {
    let rest = messages("channels_history");
    let ddp = messages("load_history");
    assert!(rest.len() >= 40);
    for m in &rest {
        if let Some(d) = ddp.iter().find(|d| d.id == m.id) {
            assert_eq!(d.ts, m.ts, "{}", m.msg);
            assert_eq!(d.md, m.md);
        }
    }
    let raw_history = raw("load_history");
    let h: History = serde_json::from_value(raw_history["result"].clone()).unwrap();
    assert_eq!(h.messages.len(), ddp.len());
    for v in raw("channels_history")["messages"].as_array().unwrap() {
        assert_no_unknown(v);
    }
    let wrapped: Value = raw("method_call_load_history_raw");
    let inner: Value = serde_json::from_str(wrapped["message"].as_str().unwrap()).unwrap();
    let h2: History = serde_json::from_value(inner["result"].clone()).unwrap();
    assert!(!h2.messages.is_empty());
}

#[test]
fn markdown_constructs() {
    let msgs = messages("channels_history");
    let m = by_msg(&msgs, "**bold**");
    let i = first_inlines(m);
    assert!(
        matches!(&i[0], Inline::Bold { value } if matches!(&value[0], Inline::PlainText { value } if value == "bold"))
    );
    assert!(i.iter().any(|x| matches!(x, Inline::Italic { .. })));
    assert!(i.iter().any(|x| matches!(x, Inline::Strike { .. })));
    assert!(
        i.iter()
            .any(|x| matches!(x, Inline::InlineCode { value } if value.value == "inline code"))
    );

    let code = by_msg(&msgs, "```rust");
    let Block::Code { language, value } = &code.md.as_ref().unwrap()[0] else {
        panic!()
    };
    assert_eq!(language.as_deref(), Some("rust"));
    assert_eq!(value.len(), 3);
    assert_eq!(value[1].value.value, "    println!(\"hi\");");

    let q = by_msg(&msgs, "> quoted");
    assert!(matches!(&q.md.as_ref().unwrap()[0], Block::Quote { value } if value.len() == 2));

    let h = by_msg(&msgs, "# Heading");
    let levels: Vec<u8> =
        h.md.as_ref()
            .unwrap()
            .iter()
            .filter_map(|b| match b {
                Block::Heading { level, .. } => Some(*level),
                _ => None,
            })
            .collect();
    assert_eq!(levels, vec![1, 2, 3, 4]);

    let ul = by_msg(&msgs, "- item one");
    assert!(
        matches!(&ul.md.as_ref().unwrap()[0], Block::UnorderedList { value } if value.len() == 2)
    );
    let ol = by_msg(&msgs, "1. first");
    let Block::OrderedList { value } = &ol.md.as_ref().unwrap()[0] else {
        panic!()
    };
    assert_eq!(value[2].number, Some(3));
    let tasks = by_msg(&msgs, "- [ ] todo");
    let Block::Tasks { value } = &tasks.md.as_ref().unwrap()[0] else {
        panic!()
    };
    assert_eq!((value[0].status, value[1].status), (false, true));
    let lb = by_msg(&msgs, "line one");
    assert!(
        lb.md
            .as_ref()
            .unwrap()
            .iter()
            .any(|b| matches!(b, Block::LineBreak))
    );

    let links = by_msg(&msgs, "[link label]");
    let Inline::Link { value } = &first_inlines(links)[0] else {
        panic!()
    };
    assert_eq!(value.src.value, "https://example.com");
    assert!(
        matches!(&value.label.as_slice()[0], Inline::PlainText { value } if value == "link label")
    );

    let img = by_msg(&msgs, "![image alt]");
    let Inline::Image { value } = &first_inlines(img)[0] else {
        panic!()
    };
    assert_eq!(value.src.value, "https://example.com/image.png");
    assert_eq!(value.label.as_slice().len(), 1);

    let sp = by_msg(&msgs, "||spoiler");
    assert!(matches!(&first_inlines(sp)[0], Inline::Spoiler { .. }));
    let spb = by_msg(&msgs, "||\nblock");
    assert!(matches!(
        &spb.md.as_ref().unwrap()[0],
        Block::SpoilerBlock { .. }
    ));

    let ts = by_msg(&msgs, "time <t:");
    let Inline::Timestamp { value, .. } = &first_inlines(ts)[1] else {
        panic!()
    };
    assert_eq!(value.format.as_deref(), Some("R"));
    assert_eq!(value.date(), RcDate::from_millis(1_700_000_000_000));

    let katex = by_msg(&msgs, "\\[ x^2");
    let md = katex.md.as_ref().unwrap();
    assert!(matches!(&md[0], Block::Katex { value } if value.contains("x^2")));
    assert!(
        matches!(&md[1], Block::Paragraph { value } if value.iter().any(|i| matches!(i, Inline::InlineKatex { .. })))
    );

    let hr = by_msg(&msgs, "---");
    assert!(matches!(
        &hr.md.as_ref().unwrap()[0],
        Block::HorizontalRule { .. }
    ));
    let table = by_msg(&msgs, "| a | b |");
    let Block::Table { value, .. } = &table.md.as_ref().unwrap()[0] else {
        panic!()
    };
    assert_eq!(value.header.len(), 2);
    assert_eq!(value.header[1].align.as_deref(), Some("right"));
    assert_eq!(value.rows.len(), 1);

    let nested = by_msg(&msgs, "nested");
    assert_eq!(nested.plain_text(), "nested bold italic strike end");
}

#[test]
fn mentions_and_emoji() {
    let msgs = messages("channels_history");
    let m = by_msg(&msgs, "hello @alice");
    let users: Vec<&str> = first_inlines(m)
        .iter()
        .filter_map(|i| match i {
            Inline::MentionUser { value } => Some(value.value.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(users, vec!["alice", "bob", "all", "here"]);
    assert!(
        first_inlines(m)
            .iter()
            .any(|i| matches!(i, Inline::MentionChannel { value } if value.value == "general"))
    );
    let mentions = m.mentions.as_ref().unwrap();
    assert!(mentions.iter().any(|x| x.id == "all"));
    assert!(
        mentions
            .iter()
            .any(|x| x.username.as_deref() == Some("bob"))
    );
    assert_eq!(
        m.channels.as_ref().unwrap()[0].name.as_deref(),
        Some("general")
    );
    assert_eq!(
        m.plain_text(),
        "hello @alice and @bob and @all and @here in #general"
    );

    let big = by_msg(&msgs, ":smile:");
    let Block::BigEmoji { value } = &big.md.as_ref().unwrap()[0] else {
        panic!()
    };
    assert!(matches!(&value[0], Inline::Emoji(e) if e.short_code.as_deref() == Some("smile")));
    let uni = by_msg(&msgs, "😀");
    let Block::BigEmoji { value } = &uni.md.as_ref().unwrap()[0] else {
        panic!()
    };
    assert!(
        matches!(&value[1], Inline::Emoji(e) if e.unicode.as_deref() == Some("🎉") && e.value.is_none())
    );
    let mixed = by_msg(&msgs, "text with emoji");
    assert_eq!(
        mixed.plain_text(),
        "text with emoji :thumbsup: and 🙂 and :rocket:"
    );
}

#[test]
fn threads_quotes_files_reactions() {
    let msgs = messages("channels_history");
    let parent = by_msg(&msgs, "thread parent");
    assert_eq!(parent.tcount, Some(2));
    assert!(parent.tlm.is_some());
    assert_eq!(parent.replies.as_ref().unwrap().len(), 2);
    let shown = by_msg(&msgs, "reply also to channel");
    assert_eq!(shown.tmid.as_deref(), Some(parent.id.as_str()));
    assert_eq!(shown.tshow, Some(true));
    assert!(shown.is_thread_reply());
    let replies: Vec<Message> = get("chat_get_thread_messages", "messages");
    assert_eq!(replies.len(), 2);
    assert!(
        replies
            .iter()
            .all(|r| r.tmid.as_deref() == Some(parent.id.as_str()))
    );
    let threads: Vec<Message> = get("chat_get_threads_list", "threads");
    assert_eq!(threads[0].id, parent.id);

    let quote = by_msg(&msgs, "[ ](http");
    let att = &quote.attachments.as_ref().unwrap()[0];
    assert!(att.is_quote());
    assert_eq!(att.text.as_deref(), Some("message to be quoted"));
    assert_eq!(att.author_name.as_deref(), Some("alice"));
    assert!(att.md.is_some());
    assert_eq!(quote.urls.as_ref().unwrap()[0].ignore_parse, Some(true));

    let image = by_msg(&msgs, "image caption");
    let file = image.file.as_ref().unwrap();
    assert_eq!(file.mime.as_deref(), Some("image/png"));
    assert_eq!(file.type_group.as_deref(), Some("image"));
    let a = &image.attachments.as_ref().unwrap()[0];
    assert!(a.is_image());
    assert_eq!(
        a.image_dimensions,
        Some(Dimensions {
            width: 8,
            height: 8
        })
    );
    assert!(
        a.title_link
            .as_deref()
            .unwrap()
            .starts_with("/file-upload/")
    );
    assert!(a.image_preview.is_some());
    assert_eq!(a.image_alt.as_deref(), Some("red square"));

    let txt = msgs
        .iter()
        .find(|m| m.file.as_ref().is_some_and(|f| f.name == "notes.txt"))
        .unwrap();
    let a = &txt.attachments.as_ref().unwrap()[0];
    assert_eq!(a.kind.as_deref(), Some("file"));
    assert_eq!(a.title_link_download, Some(true));
    assert!(a.description_md.is_some());
    assert!(!a.is_image());

    let r = by_msg(&msgs, "react to me");
    let reactions = r.reactions.as_ref().unwrap();
    assert_eq!(reactions[":thumbsup:"].usernames, vec!["alice", "bob"]);
    assert_eq!(reactions[":tada:"].usernames, vec!["bob"]);

    let pinned = by_msg(&msgs, "pin me");
    assert_eq!(pinned.pinned, Some(true));
    assert!(pinned.pinned_by.is_some());
    let starred = by_msg(&msgs, "star me");
    assert!(starred.is_starred_by("Er3h8XYKahaopbSA7"));
    let edited = by_msg(&msgs, "after **edit**");
    assert!(edited.is_edited());
    assert_eq!(
        edited.edited_by.as_ref().unwrap().username.as_deref(),
        Some("alice")
    );

    let preview = by_msg(&msgs, "message with url preview");
    let url = &preview.urls.as_ref().unwrap()[0];
    assert_eq!(url.meta_str("pageTitle"), Some("Example Domain"));
    assert!(url.headers.as_ref().unwrap().content_type.is_some());
}

#[test]
fn system_messages() {
    let msgs = messages("channels_history");
    let types: Vec<&str> = msgs.iter().filter_map(|m| m.t.as_deref()).collect();
    for t in [
        "room_changed_topic",
        "room_changed_announcement",
        "room_changed_description",
        "message_pinned",
        "discussion-created",
        "au",
    ] {
        assert!(types.contains(&t), "{t} in {types:?}");
    }
    let disc = msgs
        .iter()
        .find(|m| m.t.as_deref() == Some("discussion-created"))
        .unwrap();
    assert!(disc.is_system());
    assert!(disc.drid.is_some());
    assert_eq!(disc.dcount, Some(1));
    assert!(disc.dlm.is_some());
    let pin = msgs
        .iter()
        .find(|m| m.t.as_deref() == Some("message_pinned"))
        .unwrap();
    assert_eq!(
        pin.attachments.as_ref().unwrap()[0].text.as_deref(),
        Some("pin me")
    );

    let sys = messages("load_history_system");
    let types: Vec<&str> = sys.iter().filter_map(|m| m.t.as_deref()).collect();
    for t in ["uj", "ul", "r", "user-muted", "subscription-role-added"] {
        assert!(types.contains(&t), "{t} in {types:?}");
    }
}

#[test]
fn message_endpoints() {
    let sent: Message = get("chat_send_message", "message");
    assert_eq!(sent.msg, "plain text message");
    assert!(sent.ts.is_some());
    let updated: Message = get("chat_update", "message");
    assert!(updated.edited_at.is_some());
    let pin: Message = get("chat_pin_message", "message");
    assert_eq!(pin.t.as_deref(), Some("message_pinned"));
    let got: Message = get("chat_get_message", "message");
    assert_eq!(got.tcount, Some(2));
    let pinned: Vec<Message> = get("chat_get_pinned", "messages");
    assert_eq!(pinned.len(), 1);
    let starred: Vec<Message> = get("chat_get_starred", "messages");
    assert_eq!(starred.len(), 1);
    let found: Vec<Message> = get("chat_search", "messages");
    assert!(!found.is_empty());
    let _: Vec<Message> = get("chat_get_mentioned", "messages");
    let sync: SyncMessages = get("chat_sync_messages", "result");
    assert!(sync.updated.len() >= 40);
    assert_eq!(sync.deleted.len(), 1);
    assert!(sync.deleted[0].deleted_at.is_some());
    let up: UploadedFile = get("rooms_media", "file");
    assert!(up.url.starts_with("/file-upload/"));
    let confirmed: Message = get("rooms_media_confirm", "message");
    assert_eq!(confirmed.file.as_ref().unwrap().id, up.id);
}

#[test]
fn misc_lists() {
    let perms: UpdatedSince<Permission> = get("permissions_list_all", "");
    assert_eq!(perms.update.len(), 8);
    assert!(perms.update[0].updated_at.is_some());
    let roles: Vec<Role> = get("roles_list", "roles");
    assert!(
        roles
            .iter()
            .any(|r| r.id == "admin" && r.scope.as_deref() == Some("Users"))
    );
    let cmds: Vec<SlashCommand> = get("commands_list", "commands");
    let gimme = cmds.iter().find(|c| c.command == "gimme").unwrap();
    assert!(gimme.client_only);
    assert!(cmds.iter().any(|c| c.permission.is_some()));
    let emoji: UpdatedSince<CustomEmoji> = get("emoji_custom_list", "emojis");
    assert!(emoji.update.is_empty());
    let statuses: Vec<CustomUserStatus> = get("custom_user_status_list", "statuses");
    assert!(statuses.is_empty());
    let err: Value = raw("error_method_call");
    let inner: Value = serde_json::from_str(err["message"].as_str().unwrap()).unwrap();
    let me: rocketchat::MethodError = serde_json::from_value(inner["error"].clone()).unwrap();
    assert_eq!(me.code().as_deref(), Some("404"));
}

#[test]
fn unknown_md_is_detected() {
    let mut v = raw("chat_send_message")["message"].clone();
    v["md"] = serde_json::json!([{"type":"PARAGRAPH","value":[{"type":"FUTURE_NODE","value":1}]}]);
    let msg: Message = serde_json::from_value(v.clone()).unwrap();
    assert!(md::contains_unknown(msg.md.as_ref().unwrap()));
    assert!(std::panic::catch_unwind(|| assert_no_unknown(&v)).is_err());
}

#[test]
fn tolerant_nested_values() {
    // Legacy integration attachment with a numeric-string ts, plus garbage elsewhere.
    let j = r#"{"_id":"m","rid":"r","msg":"x","ts":"not a date",
        "attachments":[{"text":"t","ts":"1481302386"},{"text":"bad","fields":"oops","image_dimensions":7},42],
        "reactions":{":ok:":{"usernames":["a"]},":bad:":5},
        "mentions":[{"_id":"u1"},"oops"],"urls":[{"url":"https://x"},{"url":3}],
        "starred":"yes","u":"weird"}"#;
    let m: Message = serde_json::from_str(j).unwrap();
    assert!(m.ts.is_none());
    let atts = m.attachments.unwrap();
    assert_eq!(atts.len(), 2);
    assert_eq!(atts[0].ts.unwrap().timestamp_millis(), 1_481_302_386_000);
    assert!(atts[1].fields.is_none());
    assert_eq!(m.reactions.unwrap().len(), 1);
    assert_eq!(m.mentions.unwrap().len(), 1);
    assert_eq!(m.urls.unwrap().len(), 1);
    assert!(m.starred.is_none());

    let rooms = r#"{"update":[{"_id":"a","t":"c","lastMessage":{"_id":1}},{"_id":"b","t":"c","ts":"bad"},"garbage",{"_id":"c","t":{"x":1}}],"remove":[]}"#;
    let r: UpdatedSince<Room> = serde_json::from_str(rooms).unwrap();
    assert_eq!(r.update.len(), 2);
    assert!(r.update[0].last_message.is_none());
    assert!(r.update[1].ts.is_none());
    let h: History =
        serde_json::from_str(r#"{"messages":[{"_id":"a","rid":"r","msg":"ok"},{"_id":5}]}"#)
            .unwrap();
    assert_eq!(h.messages.len(), 1);
}

#[test]
fn invited_subscription() {
    let s: Subscription = serde_json::from_str(
        r#"{"_id":"s","rid":"r","name":"n","t":"p","status":"INVITED","inviter":{"_id":"u","username":"bob"}}"#,
    )
    .unwrap();
    assert_eq!(s.status.as_deref(), Some("INVITED"));
    assert_eq!(s.inviter.unwrap().username.as_deref(), Some("bob"));
}

#[test]
fn debug_redacts_secrets() {
    let l = rocketchat::rest::LdapLogin {
        username: "u".into(),
        password: "PLAINTEXT-PW".into(),
    };
    let link = rocketchat::auth::parse_deep_link(
        "rocketchat://auth?host=x.org&type=oauth&credentialToken=CRED-TOKEN&credentialSecret=CRED-SECRET",
    )
    .unwrap();
    let tok = rocketchat::auth::parse_deep_link(
        "rocketchat://auth?host=x.org&userId=u&token=LOGIN-TOKEN",
    )
    .unwrap();
    let lr: LoginResult =
        serde_json::from_str(r#"{"userId":"u","authToken":"AUTH-TOKEN"}"#).unwrap();
    let creds = rocketchat::Credentials::new("u", "CREDS-TOKEN");
    let code = rocketchat::auth::TwoFactorCode::totp("987654");
    let srv = rocketchat::ServerUrl::parse("https://x.org").unwrap();
    let bl = rocketchat::auth::saml_login(&srv, "p");
    let bl_token = bl.credential_token.clone().unwrap();
    let all = format!("{l:?} {link:?} {tok:?} {lr:?} {creds:?} {code:?} {bl:?}");
    for s in [
        "PLAINTEXT-PW",
        "CRED-TOKEN",
        "CRED-SECRET",
        "LOGIN-TOKEN",
        "AUTH-TOKEN",
        "CREDS-TOKEN",
        "987654",
        &bl_token,
    ] {
        assert!(!all.contains(s), "{s} leaked via Debug: {all}");
    }
}

#[test]
fn dropped_list_elements_are_counted() {
    let h: History = serde_json::from_str(
        r#"{"messages":[{"_id":"a","rid":"r","msg":"ok"},{"_id":"b","rid":"r","msg":42},{"_id":"c","rid":"r","msg":"ok","t":7}]}"#,
    )
    .unwrap();
    assert_eq!((h.messages.len(), h.raw_len, h.dropped), (1, 3, 2));
    let u: UpdatedSince<Subscription> = serde_json::from_str(
        r#"{"update":[{"_id":"s","rid":"r","name":"n"},5],"remove":[{"_id":1}]}"#,
    )
    .unwrap();
    assert_eq!((u.update.len(), u.remove.len(), u.dropped), (1, 0, 2));
    let s: SyncMessages = serde_json::from_str(
        r#"{"updated":[{"_id":"a","rid":"r","msg":"x"},{"msg":[]}],"deleted":[]}"#,
    )
    .unwrap();
    assert_eq!((s.updated.len(), s.dropped), (1, 1));
}
