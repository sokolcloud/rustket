//! Live tests against a running Rocket.Chat (see CLAUDE.md). Skipped unless
//! `RUSTKET_TEST_SERVER` is set, e.g. `RUSTKET_TEST_SERVER=http://localhost:3000`.

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use rocketchat::auth::{
    AuthDeepLink, DeepLink, OAuthOptions, TwoFactorCode, oauth_login, parse_deep_link,
};
use rocketchat::models::{OutgoingMessage, UserStatus};
use rocketchat::rest::{CreateRoom, UploadRequest, UserLookup};
use rocketchat::{Credentials, Error, RcDate, RestClient, ServerUrl, TlsConfig, TwoFactorMethod};
use serde_json::{Value, json};

fn server() -> Option<ServerUrl> {
    match std::env::var("RUSTKET_TEST_SERVER") {
        Ok(s) if !s.is_empty() => Some(ServerUrl::parse(&s).expect("RUSTKET_TEST_SERVER")),
        _ => {
            eprintln!("RUSTKET_TEST_SERVER not set; skipping live test");
            None
        }
    }
}

fn mailpit() -> String {
    std::env::var("RUSTKET_TEST_MAILPIT").unwrap_or_else(|_| "http://localhost:8025".into())
}

fn client(server: &ServerUrl) -> RestClient {
    let tls = TlsConfig::new().build().unwrap();
    RestClient::new(server.clone(), &tls).unwrap()
}

async fn login(server: &ServerUrl, user: &str) -> RestClient {
    let c = client(server);
    c.login_password(user, &format!("{user}pass"), None)
        .await
        .unwrap_or_else(|e| panic!("login {user}: {e}"));
    c
}

fn unique(prefix: &str) -> String {
    format!(
        "{prefix}-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        rocketchat::ids::random_id_len(4).to_lowercase()
    )
}

fn plain_http() -> reqwest::Client {
    let tls = TlsConfig::new().build().unwrap();
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls.http_client_config())
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}

/// Newest 2FA code e-mail to `to` in Mailpit: `(id, text)`.
async fn newest_mail(to: &str) -> Option<(String, String)> {
    let http = plain_http();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("query", &format!("to:{to} subject:\"Authentication code\""))
        .append_pair("limit", "1")
        .finish();
    let list: Value = http
        .get(format!("{}/api/v1/search?{query}", mailpit()))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    let id = list["messages"][0]["ID"].as_str()?.to_owned();
    let msg: Value = http
        .get(format!("{}/api/v1/message/{id}", mailpit()))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()?;
    Some((id, msg["Text"].as_str().unwrap_or_default().to_owned()))
}

fn extract_code(text: &str) -> Option<String> {
    let idx = text.find("authentication code:")?;
    let code: String = text[idx + "authentication code:".len()..]
        .trim_start()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '-')
        .collect();
    (!code.is_empty()).then_some(code)
}

/// Waits for a 2FA code mailed to `to` (newer than `before` when the server says
/// it generated a new one).
async fn wait_code(to: &str, before: Option<&str>, generated: Option<bool>) -> String {
    for _ in 0..50 {
        if let Some((id, text)) = newest_mail(to).await {
            let fresh = before != Some(id.as_str());
            if (fresh || generated != Some(true))
                && let Some(code) = extract_code(&text)
            {
                return code;
            }
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    panic!("no 2FA code for {to} in Mailpit");
}

/// A throwaway user created by the admin, so state-sensitive tests never touch
/// shared accounts. Call [`TempUser::delete`] at the end.
struct TempUser {
    admin: RestClient,
    id: String,
    username: String,
    email: String,
    password: String,
}

impl TempUser {
    async fn create(server: &ServerUrl, prefix: &str) -> Self {
        let admin = login(server, "admin").await;
        let username = unique(prefix);
        let email = format!("{username}@rustket.local");
        let password = format!("{username}-pw");
        let r: Value = admin
            .post(
                "users.create",
                &json!({
                    "username": username, "email": email, "password": password,
                    "name": username, "verified": true, "requirePasswordChange": false,
                }),
            )
            .await
            .unwrap();
        let id = r["user"]["_id"].as_str().unwrap().to_owned();
        Self {
            admin,
            id,
            username,
            email,
            password,
        }
    }

    async fn login(&self, server: &ServerUrl) -> RestClient {
        let c = client(server);
        c.login_password(&self.username, &self.password, None)
            .await
            .unwrap();
        c
    }

    async fn delete(self) {
        let _: Value = self
            .admin
            .post(
                "users.delete",
                &json!({ "userId": self.id, "confirmRelinquish": true }),
            )
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn password_login_and_me() {
    let Some(s) = server() else { return };
    let c = client(&s);
    let r = c.login_password("alice", "alicepass", None).await.unwrap();
    assert_eq!(r.me.as_ref().unwrap().username.as_deref(), Some("alice"));
    assert_eq!(c.user_id().as_deref(), Some(r.user_id.as_str()));
    let me = c.me().await.unwrap();
    assert_eq!(me.username.as_deref(), Some("alice"));
    assert!(me.preferences().is_some());
    // E-mail works as the user too.
    let c2 = client(&s);
    c2.login_password("alice@rustket.local", "alicepass", None)
        .await
        .unwrap();
}

/// App review #6: an image URL in a message pointing at `GET /api/v1/logout`
/// on the user's own server must not log the user out when rendered.
#[tokio::test]
async fn downloading_an_api_url_sends_no_credentials() {
    let Some(s) = server() else { return };
    let c = login(&s, "alice").await;
    let logout = s.absolute("/api/v1/logout");
    let e = c.download(&logout).await.unwrap_err();
    assert!(e.is_unauthorized(), "{e:?}");
    c.me().await.expect("the token survives");
    // Avatars still get the credentials.
    let avatar = c.download(&s.avatar_user("alice", Some(32))).await.unwrap();
    assert!(avatar.content_type.unwrap().starts_with("image/"));
    c.logout().await.unwrap();
}

#[tokio::test]
async fn wrong_password_is_unauthorized() {
    let Some(s) = server() else { return };
    let c = client(&s);
    let e = c
        .login_password("alice", "definitely-wrong", None)
        .await
        .unwrap_err();
    assert!(e.is_unauthorized(), "{e:?}");
    assert!(!c.is_authenticated());
}

#[tokio::test]
async fn email_two_factor_login() {
    let Some(s) = server() else { return };
    let user = TempUser::create(&s, "rk2fa").await;
    let c = user.login(&s).await;
    c.post_ok("users.2fa.enableEmail", &json!({}))
        .await
        .unwrap();
    c.logout().await.unwrap();

    let before = newest_mail(&user.email).await.map(|m| m.0);
    let challenge = match c.login_password(&user.username, &user.password, None).await {
        Err(Error::TwoFactorRequired(ch)) => ch,
        other => panic!("expected TwoFactorRequired, got {other:?}"),
    };
    assert_eq!(challenge.method, TwoFactorMethod::Email);
    assert!(
        challenge
            .available_methods
            .contains(&TwoFactorMethod::Email)
    );
    assert_eq!(
        challenge.email_or_username.as_deref(),
        Some(user.username.as_str())
    );
    let code = wait_code(&user.email, before.as_deref(), challenge.code_generated).await;

    let bad = c
        .login_password(
            &user.username,
            &user.password,
            Some(&TwoFactorCode::email("000000")),
        )
        .await
        .unwrap_err();
    assert!(matches!(bad, Error::TwoFactorInvalid(_)), "{bad:?}");
    // LDAP-style login (falls back to password while LDAP is off) with the code
    // wrapped as {"totp": {"login": .., "code"}}.
    let ldap = rocketchat::rest::LdapLogin {
        username: user.username.clone(),
        password: user.password.clone(),
    };
    let ok = c
        .login_ldap(&ldap, Some(&TwoFactorCode::email(&code)))
        .await
        .unwrap();
    assert!(ok.me.unwrap().email_2fa_enabled());
    c.logout().await.unwrap();

    // Password login with a top-level code (each code works once: get a new one).
    let before = newest_mail(&user.email).await.map(|m| m.0);
    let ch = match c.login_password(&user.username, &user.password, None).await {
        Err(Error::TwoFactorRequired(ch)) => ch,
        other => panic!("expected TwoFactorRequired, got {other:?}"),
    };
    let code = wait_code(&user.email, before.as_deref(), ch.code_generated).await;
    c.login_password(
        &user.username,
        &user.password,
        Some(&TwoFactorCode::email(&code)),
    )
    .await
    .unwrap();
    c.logout().await.unwrap();
    user.delete().await;
}

#[tokio::test]
async fn resume_login() {
    let Some(s) = server() else { return };
    let first = login(&s, "alice").await.credentials().unwrap();
    let c = client(&s);
    let r = c.login_resume(&first.auth_token).await.unwrap();
    assert_eq!(r.user_id, first.user_id);
    // 8.9 returns the same token for a REST resume.
    assert!(!r.auth_token.is_empty());
    c.me().await.unwrap();
    let bad = client(&s).login_resume("not-a-token").await.unwrap_err();
    assert!(bad.is_unauthorized(), "{bad:?}");
}

#[tokio::test]
async fn personal_access_token_credentials() {
    let Some(s) = server() else { return };
    let a = login(&s, "alice").await;
    let name = unique("pat");
    // Protected endpoint: send the password fallback as the 2FA code.
    let r: Value = a
        .post_with(
            "users.generatePersonalAccessToken",
            &json!({ "tokenName": name, "bypassTwoFactor": true }),
            Some(&TwoFactorCode::password("alicepass")),
        )
        .await
        .unwrap();
    let token = r["token"].as_str().unwrap().to_owned();
    let pat = client(&s);
    pat.set_credentials(Some(Credentials::new(a.user_id().unwrap(), token)));
    assert_eq!(pat.me().await.unwrap().username.as_deref(), Some("alice"));
    a.post_with::<Value, _>(
        "users.removePersonalAccessToken",
        &json!({ "tokenName": name }),
        Some(&TwoFactorCode::password("alicepass")),
    )
    .await
    .unwrap();
    assert!(pat.me().await.unwrap_err().is_unauthorized());
}

#[tokio::test]
async fn logout_invalidates_token() {
    let Some(s) = server() else { return };
    let c = login(&s, "bob").await;
    let creds = c.credentials().unwrap();
    c.logout().await.unwrap();
    assert!(!c.is_authenticated());
    let other = client(&s);
    other.set_credentials(Some(creds));
    let e = other.me().await.unwrap_err();
    assert!(e.is_unauthorized(), "{e:?}");
}

#[tokio::test]
async fn subscriptions_and_rooms_updated_since() {
    let Some(s) = server() else { return };
    let user = TempUser::create(&s, "rksubs").await;
    let a = user.login(&s).await;
    let subs = a.subscriptions_get(None).await.unwrap();
    assert!(subs.update.iter().any(|x| x.rid == "GENERAL"));
    let rooms = a.rooms_get(None).await.unwrap();
    assert!(rooms.update.iter().any(|x| x.id == "GENERAL"));

    let since = RcDate::now();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let room = a
        .channels_create(&CreateRoom::new(unique("delta")))
        .await
        .unwrap();
    let subs = a.subscriptions_get(Some(since)).await.unwrap();
    assert!(subs.update.iter().any(|x| x.rid == room.id));
    assert_eq!(
        subs.update.len(),
        1,
        "fresh user: only the new room changed"
    );
    let rooms = a.rooms_get(Some(since)).await.unwrap();
    assert!(rooms.update.iter().any(|x| x.id == room.id));
    let one = a.subscriptions_get_one(&room.id).await.unwrap();
    assert!(one.roles.unwrap_or_default().contains(&"owner".to_owned()));
    let info = a.rooms_info(&room.id).await.unwrap();
    assert_eq!(info.id, room.id);

    a.rooms_favorite(&room.id, true).await.unwrap();
    assert!(a.subscriptions_get_one(&room.id).await.unwrap().f);
    a.rooms_hide(&room.id).await.unwrap();
    assert!(!a.subscriptions_get_one(&room.id).await.unwrap().open);
    a.rooms_open(&room.id).await.unwrap();
    assert!(a.subscriptions_get_one(&room.id).await.unwrap().open);

    let b = login(&s, "bob").await;
    b.channels_join(&room.id).await.unwrap();
    let members = a.room_members(&room.id, &Default::default()).await.unwrap();
    assert_eq!(members.total, 2);
    b.rooms_leave(&room.id).await.unwrap();
    a.rooms_delete(&room.id).await.unwrap();
    user.delete().await;
}

#[tokio::test]
async fn message_lifecycle() {
    let Some(s) = server() else { return };
    let a = login(&s, "alice").await;
    let b = login(&s, "bob").await;
    let room = a
        .channels_create(&CreateRoom {
            members: vec!["bob".into()],
            ..CreateRoom::new(unique("msgs"))
        })
        .await
        .unwrap();
    let start = RcDate::now();

    let out = OutgoingMessage::new(&room.id, "hello **world** @bob");
    let sent = a.send_message(&out).await.unwrap();
    assert_eq!(Some(&sent.id), out.id.as_ref());
    assert!(sent.md.is_some());
    assert!(
        sent.mentions
            .unwrap()
            .iter()
            .any(|m| m.username.as_deref() == Some("bob"))
    );

    assert_eq!(
        b.get_mentioned(&room.id, 0, 10).await.unwrap().items.len(),
        1
    );
    let edited = a
        .update_message(&room.id, &sent.id, "edited text")
        .await
        .unwrap();
    assert!(edited.is_edited());
    b.react(&sent.id, ":thumbsup:", Some(true)).await.unwrap();
    a.react(&sent.id, ":thumbsup:", None).await.unwrap();
    let pin = a.pin_message(&sent.id).await.unwrap();
    assert_eq!(pin.t.as_deref(), Some("message_pinned"));
    a.star_message(&sent.id).await.unwrap();

    let m = a.get_message(&sent.id).await.unwrap();
    assert_eq!(m.msg, "edited text");
    assert_eq!(
        m.reactions.as_ref().unwrap()[":thumbsup:"].usernames.len(),
        2
    );
    assert_eq!(m.pinned, Some(true));
    assert!(m.is_starred_by(&a.user_id().unwrap()));
    assert_eq!(a.get_pinned(&room.id, 0, 10).await.unwrap().total, 1);
    assert_eq!(a.get_starred(&room.id, 0, 10).await.unwrap().items.len(), 1);

    let reply = b
        .send_message(&OutgoingMessage::new(&room.id, "a reply").in_thread(&sent.id, false))
        .await
        .unwrap();
    assert_eq!(reply.tmid.as_deref(), Some(sent.id.as_str()));
    let thread = a.thread_messages(&sent.id, 0, 50).await.unwrap();
    assert_eq!(thread.items.len(), 1);
    let latest = a.thread_messages_latest(&sent.id, 0, 50).await.unwrap();
    assert_eq!(latest.items[0].id, reply.id);
    // Newest first, paged by offset.
    let mut later = Vec::new();
    for text in ["reply 2", "reply 3"] {
        let m = b
            .send_message(&OutgoingMessage::new(&room.id, text).in_thread(&sent.id, false))
            .await
            .unwrap();
        later.push(m.id);
    }
    let ids = |p: rocketchat::models::Paginated<rocketchat::models::Message>| {
        p.items.into_iter().map(|m| m.id).collect::<Vec<_>>()
    };
    assert_eq!(
        ids(a.thread_messages_latest(&sent.id, 0, 2).await.unwrap()),
        [later[1].clone(), later[0].clone()]
    );
    assert_eq!(
        ids(a.thread_messages_latest(&sent.id, 2, 2).await.unwrap()),
        std::slice::from_ref(&reply.id)
    );
    // The parent's author follows the thread: the reply is unread for them
    // until `chat.readThread`.
    let tunread = |s: rocketchat::models::Subscription| s.tunread.unwrap_or_default();
    let mut unread = Vec::new();
    for _ in 0..20 {
        unread = tunread(a.subscriptions_get_one(&room.id).await.unwrap());
        if !unread.is_empty() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(unread.contains(&sent.id), "{unread:?}");
    a.read_thread(&sent.id).await.unwrap();
    let unread = tunread(a.subscriptions_get_one(&room.id).await.unwrap());
    assert!(!unread.contains(&sent.id), "{unread:?}");
    let threads = a.threads_list(&room.id, None, None, 0, 50).await.unwrap();
    assert_eq!(threads.items[0].id, sent.id);
    assert_eq!(
        a.get_mentioned(&room.id, 0, 10).await.unwrap().items.len(),
        0
    );
    let found = a.search_messages(&room.id, "reply", 0, 10).await.unwrap();
    assert!(found.iter().any(|m| m.id == reply.id));

    a.unpin_message(&sent.id).await.unwrap();
    a.unstar_message(&sent.id).await.unwrap();
    b.subscriptions_read(&room.id).await.unwrap();
    a.delete_message(&room.id, &reply.id).await.unwrap();
    let sync = a.chat_sync_messages(&room.id, start).await.unwrap();
    assert!(sync.deleted.iter().any(|d| d.id == reply.id));
    assert!(sync.updated.iter().any(|m| m.id == sent.id));

    let err = a.get_message("doesnotexist").await.unwrap_err();
    assert!(matches!(err, Error::Api { .. }), "{err:?}");
    a.rooms_delete(&room.id).await.unwrap();
}

/// The automatic read mark (`subscriptions.read {rid}`, like the web's
/// `ReadStateManager`) leaves unread threads and their alert; only the
/// explicit "Mark as read" (`readThreads`) clears them.
#[tokio::test]
async fn read_mark_keeps_unread_threads_unless_asked() {
    let Some(s) = server() else { return };
    let a = login(&s, "alice").await;
    let b = login(&s, "bob").await;
    let room = a
        .channels_create(&CreateRoom {
            members: vec!["bob".into()],
            ..CreateRoom::new(unique("readthreads"))
        })
        .await
        .unwrap();
    let parent = a
        .send_message(&OutgoingMessage::new(&room.id, "parent"))
        .await
        .unwrap();
    a.subscriptions_read_with(&room.id, false).await.unwrap();
    b.send_message(&OutgoingMessage::new(&room.id, "reply").in_thread(&parent.id, false))
        .await
        .unwrap();
    let mut sub = a.subscriptions_get_one(&room.id).await.unwrap();
    for _ in 0..20 {
        if sub.has_unread_threads() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        sub = a.subscriptions_get_one(&room.id).await.unwrap();
    }
    assert!(sub.alert && sub.has_unread_threads(), "{sub:?}");
    a.subscriptions_read_with(&room.id, false).await.unwrap();
    let sub = a.subscriptions_get_one(&room.id).await.unwrap();
    assert!(sub.alert && sub.has_unread_threads(), "{sub:?}");
    a.subscriptions_read(&room.id).await.unwrap();
    let sub = a.subscriptions_get_one(&room.id).await.unwrap();
    assert!(!sub.alert && !sub.has_unread_threads(), "{sub:?}");
    a.rooms_delete(&room.id).await.unwrap();
}

#[tokio::test]
async fn history_pagination() {
    let Some(s) = server() else { return };
    let a = login(&s, "alice").await;
    let room = a
        .channels_create(&CreateRoom::new(unique("hist")))
        .await
        .unwrap();
    for i in 0..7 {
        a.send_message(&OutgoingMessage::new(&room.id, format!("m{i}")))
            .await
            .unwrap();
    }
    // `before` is inclusive: page with the oldest ts and drop known ids.
    let mut seen = std::collections::HashSet::new();
    let mut texts = Vec::new();
    let mut before = None;
    for _ in 0..10 {
        let page = a.load_history(&room.id, before, 3, None).await.unwrap();
        let new: Vec<_> = page
            .messages
            .iter()
            .filter(|m| seen.insert(m.id.clone()))
            .collect();
        if new.is_empty() {
            break;
        }
        texts.extend(new.iter().filter(|m| m.t.is_none()).map(|m| m.msg.clone()));
        before = page.messages.last().unwrap().ts;
    }
    assert_eq!(texts, vec!["m6", "m5", "m4", "m3", "m2", "m1", "m0"]);
    let first = a.load_history(&room.id, None, 3, None).await.unwrap();
    assert_eq!(first.messages[0].msg, "m6");
    a.rooms_delete(&room.id).await.unwrap();
}

const PNG: &[u8] = &[
    0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f, 0x15, 0xc4,
    0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0xf0,
    0x1f, 0x00, 0x05, 0x00, 0x01, 0xff, 0x89, 0x99, 0x3d, 0x1d, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45,
    0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
];

#[tokio::test]
async fn upload_and_download() {
    let Some(s) = server() else { return };
    let a = login(&s, "alice").await;
    let room = a
        .channels_create(&CreateRoom::new(unique("files")))
        .await
        .unwrap();
    let progress = Arc::new(AtomicU64::new(0));
    let p = progress.clone();
    let mut req = UploadRequest::new("dot.png", "image/png", PNG.to_vec());
    req.description = Some("a dot".into());
    req.msg = Some("look".into());
    req.progress = Some(Arc::new(move |sent, total| {
        assert_eq!(total, PNG.len() as u64);
        p.store(sent, Ordering::SeqCst);
    }));
    let msg = a.upload_file(&room.id, req).await.unwrap();
    assert_eq!(progress.load(Ordering::SeqCst), PNG.len() as u64);
    assert_eq!(msg.msg, "look");
    let att = &msg.attachments.as_ref().unwrap()[0];
    assert_eq!(att.image_type.as_deref(), Some("image/png"));
    let link = att.title_link.clone().unwrap();
    assert!(link.starts_with("/file-upload/"));

    let d = a.download(&link).await.unwrap();
    assert_eq!(d.content_type.as_deref(), Some("image/png"));
    assert_eq!(&d.bytes().await.unwrap()[..], PNG);
    let d = a
        .download(&s.absolute(att.image_url.as_deref().unwrap()))
        .await
        .unwrap();
    assert_eq!(&d.bytes().await.unwrap()[..], PNG);

    let anon = client(&s);
    let e = anon.download(&link).await.unwrap_err();
    assert!(
        matches!(
            e,
            Error::Status { status: 403, .. } | Error::Api { status: 403, .. }
        ),
        "{e:?}"
    );
    let avatar = a.download(&s.avatar_user("alice", Some(32))).await.unwrap();
    assert!(avatar.content_type.unwrap().starts_with("image/"));
    a.rooms_delete(&room.id).await.unwrap();
}

#[tokio::test]
async fn large_upload_reports_monotonic_progress() {
    let Some(s) = server() else { return };
    let a = login(&s, "alice").await;
    let room = a
        .channels_create(&CreateRoom::new(unique("big")))
        .await
        .unwrap();
    let data: Vec<u8> = (0..(1536 * 1024)).map(|i: u32| (i % 251) as u8).collect();
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let seen2 = seen.clone();
    let mut req = UploadRequest::new("blob.bin", "application/octet-stream", data.clone());
    req.progress = Some(Arc::new(move |sent, total| {
        seen2.lock().unwrap().push((sent, total));
    }));
    let msg = a.upload_file(&room.id, req).await.unwrap();
    let seen = seen.lock().unwrap().clone();
    assert!(seen.len() > 10, "multi-chunk progress: {}", seen.len());
    assert!(seen.windows(2).all(|w| w[0].0 < w[1].0), "monotonic");
    assert!(seen.iter().all(|&(_, t)| t == data.len() as u64));
    assert_eq!(seen.last().unwrap().0, data.len() as u64);
    let link = msg.attachments.unwrap()[0].title_link.clone().unwrap();
    let back = a.download(&link).await.unwrap().bytes().await.unwrap();
    assert_eq!(back.len(), data.len());
    assert!(back[..] == data[..]);
    a.rooms_delete(&room.id).await.unwrap();
}

#[tokio::test]
async fn upload_non_ascii_filename() {
    let Some(s) = server() else { return };
    let a = login(&s, "alice").await;
    let room = a
        .channels_create(&CreateRoom::new(unique("utf8")))
        .await
        .unwrap();
    let fname = "отчёт \"q\" 1.txt";
    let m = a
        .upload_file(
            &room.id,
            UploadRequest::new(fname, "text/plain", b"hello".to_vec()),
        )
        .await
        .unwrap();
    assert_eq!(m.file.as_ref().unwrap().name, fname);
    let link = m.attachments.unwrap()[0].title_link.clone().unwrap();
    assert_eq!(
        &a.download(&link).await.unwrap().bytes().await.unwrap()[..],
        b"hello"
    );
    a.rooms_delete(&room.id).await.unwrap();
}

#[tokio::test]
async fn method_call_round_trip() {
    let Some(s) = server() else { return };
    let a = login(&s, "alice").await;
    let v = a
        .method_call_raw("loadHistory", json!(["GENERAL", null, 2, null]), false)
        .await
        .unwrap();
    assert!(v["messages"].is_array());
    let e = a
        .method_call_raw("noSuchMethodForRustket", json!([]), false)
        .await
        .unwrap_err();
    match e {
        Error::Method(m) => assert_eq!(m.code().as_deref(), Some("404")),
        other => panic!("{other:?}"),
    }
    let anon = client(&s);
    let e = anon
        .method_call_raw("noSuchMethodForRustket", json!([]), true)
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Method(_)), "{e:?}");
}

#[tokio::test]
async fn public_endpoints() {
    let Some(s) = server() else { return };
    let c = client(&s);
    let info = c.info().await.unwrap();
    assert!(info.version_tuple().unwrap().0 >= 7);
    let all = c.settings_public_all().await.unwrap();
    let page = c.settings_public(0, 1).await.unwrap();
    assert_eq!(all.len() as u64, page.total);
    assert!(all.len() > 100);
    let some = c
        .settings_public_by_ids(&["Site_Name", "Accounts_OAuth_Use_Modern_Flow"])
        .await
        .unwrap();
    assert_eq!(some.len(), 2);
    let services = c.settings_oauth().await.unwrap();
    let _ = c.service_configurations().await.unwrap();
    assert!(services.iter().all(|s| !s.service_name().is_empty()));
    c.send_email_2fa_code("nobody-rustket").await.unwrap();
}

#[tokio::test]
async fn users_and_lists() {
    let Some(s) = server() else { return };
    let a = login(&s, "alice").await;
    let sp = a.spotlight("bo").await.unwrap();
    assert!(
        sp.users
            .iter()
            .any(|u| u.username.as_deref() == Some("bob"))
    );
    let cmds = a.commands_list().await.unwrap();
    assert!(cmds.len() > 10);
    assert!(cmds.iter().any(|c| c.command == "leave"));
    let ac = a.users_autocomplete("bo").await.unwrap();
    assert!(ac.iter().any(|u| u.username.as_deref() == Some("bob")));
    let rooms = a
        .rooms_autocomplete_channel_and_private("dev")
        .await
        .unwrap();
    assert!(
        rooms
            .iter()
            .any(|r| r.name.as_deref() == Some("dev-public"))
    );
    let bob = a.users_info(UserLookup::Username("bob")).await.unwrap();
    let again = a.users_info(UserLookup::Id(&bob.id)).await.unwrap();
    assert_eq!(again.username.as_deref(), Some("bob"));
    let user = TempUser::create(&s, "rkstatus").await;
    let t = user.login(&s).await;
    t.users_set_status(UserStatus::Busy, Some("testing"))
        .await
        .unwrap();
    let me = t.me().await.unwrap();
    assert_eq!(me.status_text.as_deref(), Some("testing"));
    let status = me.status;
    t.users_set_status_message("text only").await.unwrap();
    let me = t.me().await.unwrap();
    assert_eq!(me.status_text.as_deref(), Some("text only"));
    assert_eq!(me.status, status, "status kept");
    // The chosen status survives a text-only change.
    let raw: serde_json::Value = t.get("me", &rocketchat::rest::Query::new()).await.unwrap();
    assert_eq!(raw["statusDefault"], "busy");
    user.delete().await;
    let _ = a.users_presence(None, &[]).await.unwrap();
    let perms = a.permissions_list_all(None).await.unwrap();
    assert!(perms.update.len() > 100);
    let roles = a.roles_list().await.unwrap();
    assert!(roles.iter().any(|r| r.id == "admin"));
    let _ = a.emoji_custom_list(None).await.unwrap();
    let _ = a.custom_user_status_list().await.unwrap();
    let dm = a.im_create("bob").await.unwrap();
    assert_eq!(dm.t, rocketchat::models::RoomType::Direct);
    let r = a
        .commands_run("status", "rustket test", &dm.id, None, None)
        .await;
    assert!(r.is_ok() || matches!(r, Err(Error::Api { .. })), "{r:?}");
}

fn cookie_header(jar: &mut Vec<(String, String)>, resp: &reqwest::Response) {
    for v in resp.headers().get_all(reqwest::header::SET_COOKIE) {
        let Ok(v) = v.to_str() else { continue };
        let Some((pair, _)) = v.split_once(';').or(Some((v, ""))) else {
            continue;
        };
        if let Some((k, val)) = pair.split_once('=') {
            jar.retain(|(name, _)| name != k.trim());
            jar.push((k.trim().to_owned(), val.trim().to_owned()));
        }
    }
}

fn jar_string(jar: &[(String, String)]) -> String {
    jar.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Unescapes repeatedly: the RC redirect page double-escapes the deep link.
fn html_unescape(s: &str) -> String {
    let mut cur = s.to_owned();
    loop {
        let next = html_unescape_once(&cur);
        if next == cur {
            return cur;
        }
        cur = next;
    }
}

fn html_unescape_once(s: &str) -> String {
    s.replace("&#x2F;", "/")
        .replace("&#47;", "/")
        .replace("&quot;", "\"")
        .replace("&#34;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Drives the dev Keycloak login for `kcuser` through the legacy OAuth flow and
/// returns the deep-link credentials, or `None` when Keycloak is not set up.
async fn keycloak_credentials(s: &ServerUrl, c: &RestClient) -> Option<(String, String)> {
    let services = c.settings_oauth().await.unwrap();
    let Some(kc) = services.iter().find(|x| x.service_name() == "keycloak") else {
        eprintln!("keycloak login service not configured; skipping");
        return None;
    };
    let login = oauth_login(kc, s, &OAuthOptions::default()).unwrap();
    let http = plain_http();
    let mut jar = Vec::new();
    let Ok(page) = http.get(&login.url).send().await else {
        eprintln!("keycloak not reachable; skipping");
        return None;
    };
    assert_eq!(page.status().as_u16(), 200, "{}", login.url);
    cookie_header(&mut jar, &page);
    let html = page.text().await.unwrap();
    let start = html.find("action=\"").expect("login form") + "action=\"".len();
    let action = html_unescape(&html[start..start + html[start..].find('"').unwrap()]);
    let body = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("username", "kcuser")
        .append_pair("password", "kcpass")
        .append_pair("credentialId", "")
        .finish();
    let resp = http
        .post(&action)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("cookie", jar_string(&jar))
        .body(body)
        .send()
        .await
        .unwrap();
    assert!(
        resp.status().is_redirection(),
        "keycloak login failed: {}",
        resp.status()
    );
    let callback = resp.headers()["location"].to_str().unwrap().to_owned();
    assert!(
        callback.starts_with(&format!("{}/_oauth/keycloak", s.as_str())),
        "{callback}"
    );
    let page = http
        .get(&callback)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let start = page.find("id=\"config\"").expect("config div");
    let open = start + page[start..].find('>').unwrap() + 1;
    let close = open + page[open..].find('<').unwrap();
    let config: Value = serde_json::from_str(&html_unescape(&page[open..close])).unwrap();
    let link = config["redirectUrl"].as_str().unwrap();
    let Some(deep @ DeepLink::Auth(AuthDeepLink::OAuth { .. })) = parse_deep_link(link) else {
        panic!("unexpected deep link {link}");
    };
    assert!(deep.matches(s));
    let DeepLink::Auth(AuthDeepLink::OAuth {
        credential_token,
        credential_secret,
        ..
    }) = deep
    else {
        unreachable!()
    };
    assert_eq!(Some(&credential_token), login.credential_token.as_ref());
    Some((credential_token, credential_secret))
}

/// Legacy OAuth through the dev Keycloak (custom OAuth service `keycloak`):
/// authorize URL → Keycloak login form → `/_oauth/keycloak` page → deep link → login.
#[tokio::test]
async fn oauth_keycloak_legacy_flow() {
    let Some(s) = server() else { return };
    let c = client(&s);
    let Some((token, secret)) = keycloak_credentials(&s, &c).await else {
        return;
    };
    let r = c.login_oauth(&token, &secret, None).await.unwrap();
    assert_eq!(r.me.unwrap().username.as_deref(), Some("kcuser"));
    c.logout().await.unwrap();
}

/// OAuth + email 2FA: the first `login_oauth` answers `totp-required`; repeating it
/// with the code (wrapped as `{"totp": {"login": {"oauth": ..}, "code"}}`) and the
/// same credentials succeeds. Temporarily enables email 2FA for `kcuser`.
#[tokio::test]
async fn oauth_keycloak_with_email_two_factor() {
    let Some(s) = server() else { return };
    let c = client(&s);
    let Some((token, secret)) = keycloak_credentials(&s, &c).await else {
        return;
    };
    let r = c.login_oauth(&token, &secret, None).await.unwrap();
    let email = r.me.as_ref().unwrap().emails.as_ref().unwrap()[0]
        .address
        .clone();
    c.post_ok("users.2fa.enableEmail", &json!({}))
        .await
        .unwrap();

    let attempt = async {
        let c2 = client(&s);
        let (token, secret) = keycloak_credentials(&s, &c2).await.unwrap();
        let before = newest_mail(&email).await.map(|m| m.0);
        let ch = match c2.login_oauth(&token, &secret, None).await {
            Err(Error::TwoFactorRequired(ch)) => ch,
            other => panic!("expected TwoFactorRequired, got {other:?}"),
        };
        assert_eq!(ch.method, TwoFactorMethod::Email);
        let code = wait_code(&email, before.as_deref(), ch.code_generated).await;
        c2.login_oauth(&token, &secret, Some(&TwoFactorCode::email(&code)))
            .await
            .map(|r| r.me.unwrap().username)
    };
    let result = attempt.await;

    // Disabling email 2FA itself requires a code (x-2fa-* headers).
    let before = newest_mail(&email).await.map(|m| m.0);
    let disabled = match c.post_ok("users.2fa.disableEmail", &json!({})).await {
        Ok(()) => Ok(()),
        Err(Error::TwoFactorRequired(ch)) => {
            let code = wait_code(&email, before.as_deref(), ch.code_generated).await;
            c.post_with::<Value, _>(
                "users.2fa.disableEmail",
                &json!({}),
                Some(&TwoFactorCode::email(&code)),
            )
            .await
            .map(|_| ())
        }
        Err(e) => Err(e),
    };
    disabled.expect("disable kcuser email 2FA");
    assert_eq!(result.unwrap().as_deref(), Some("kcuser"));
}
