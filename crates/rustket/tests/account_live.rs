//! Live checks of the account agent's server calls (account screen, directory,
//! create dialogs, quick switcher opening) against the dev server. Skipped
//! without `RUSTKET_TEST_SERVER`. Creates throwaway users and rooms and deletes
//! them at the end (also when a check fails). The profile test makes several
//! `users.updateOwnBasicInfo` calls in a row, so it needs the dev server's
//! disabled API rate limiter (the default allows one per minute).
//!
//! ```sh
//! RUSTKET_TEST_SERVER=http://localhost:3000 cargo test -p rustket --test account_live -- --test-threads=1
//! ```
#![allow(clippy::unwrap_used)]

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rocketchat::auth::TwoFactorCode;
use rocketchat::rest::CreateRoom as CreateParams;
use rocketchat::{Error, RestClient, ServerUrl, TlsConfig, TwoFactorMethod};
use rustket::dialogs::create_room::{self, Request};
use rustket::screens::account::api;
use rustket::screens::directory::open::{self, Target};
use rustket::screens::directory::{self, Sort, Tab};
use serde_json::{Value, json};

// --- RFC 6238 TOTP (SHA-1), enough for the tests -------------------------------

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 80];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[4 * i],
                chunk[4 * i + 1],
                chunk[4 * i + 2],
                chunk[4 * i + 3],
            ]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A827999),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    let mut out = [0u8; 20];
    for (i, v) in h.iter().enumerate() {
        out[4 * i..4 * i + 4].copy_from_slice(&v.to_be_bytes());
    }
    out
}

fn hmac_sha1(key: &[u8], msg: &[u8]) -> [u8; 20] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..20].copy_from_slice(&sha1(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    inner.extend_from_slice(msg);
    let mut outer: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    outer.extend_from_slice(&sha1(&inner));
    sha1(&outer)
}

fn base32_decode(s: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let (mut buf, mut bits) = (0u32, 0u32);
    for c in s.chars().filter(|c| *c != '=' && !c.is_whitespace()) {
        let v = match c.to_ascii_uppercase() {
            c @ 'A'..='Z' => c as u32 - 'A' as u32,
            c @ '2'..='7' => c as u32 - '2' as u32 + 26,
            _ => panic!("bad base32"),
        };
        buf = (buf << 5) | v;
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

fn hotp(key: &[u8], counter: u64) -> String {
    let h = hmac_sha1(key, &counter.to_be_bytes());
    let o = (h[19] & 0x0f) as usize;
    let n = u32::from_be_bytes([h[o] & 0x7f, h[o + 1], h[o + 2], h[o + 3]]);
    format!("{:06}", n % 1_000_000)
}

fn totp(secret: &str) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    hotp(&base32_decode(secret), now / 30)
}

#[test]
fn totp_matches_rfc_vectors() {
    assert_eq!(
        hex(&sha1(b"abc")),
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    // RFC 4226 appendix D.
    let key = b"12345678901234567890";
    assert_eq!(hotp(key, 0), "755224");
    assert_eq!(hotp(key, 9), "520489");
    // RFC 6238 appendix B (SHA-1, 8 digits → last 6 digits).
    assert_eq!(hotp(key, 59 / 30), "287082");
    assert_eq!(hotp(key, 1111111109 / 30), "081804");
    assert_eq!(base32_decode("GEZDGNBVGY3TQOJQ"), b"1234567890");
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

// --- fixture ----------------------------------------------------------------------

#[derive(Default)]
struct Ledger {
    users: Vec<String>,
    rooms: Vec<String>,
    teams: Vec<String>,
}

struct Env {
    server: ServerUrl,
    admin: RestClient,
    ledger: Arc<Mutex<Ledger>>,
    suffix: String,
}

fn server() -> Option<ServerUrl> {
    let s = std::env::var("RUSTKET_TEST_SERVER").ok()?;
    Some(ServerUrl::parse(&s).unwrap())
}

fn client(server: &ServerUrl) -> RestClient {
    let tls = TlsConfig::new().build().unwrap();
    RestClient::new(server.clone(), &tls).unwrap()
}

impl Env {
    async fn new() -> Option<Self> {
        let server = server()?;
        let admin = client(&server);
        admin
            .login_password("admin", "adminpass", None)
            .await
            .unwrap();
        let suffix = format!(
            "{:x}",
            std::process::id()
                ^ (SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_millis() as u32)
        );
        Some(Self {
            server,
            admin,
            ledger: Arc::default(),
            suffix,
        })
    }

    async fn user(&self, tag: &str, password: &str) -> (RestClient, String, String) {
        let username = format!("rk-acct-{tag}-{}", self.suffix);
        let v: Value = self
            .admin
            .post(
                "users.create",
                &json!({
                    "username": username, "email": format!("{username}@rustket.local"),
                    "name": format!("Acct {tag}"), "password": password, "verified": true,
                    "joinDefaultChannels": false, "sendWelcomeEmail": false,
                    "requirePasswordChange": false,
                }),
            )
            .await
            .unwrap();
        let id = v["user"]["_id"].as_str().unwrap().to_owned();
        self.ledger.lock().unwrap().users.push(id.clone());
        let c = client(&self.server);
        c.login_password(&username, password, None).await.unwrap();
        (c, id, username)
    }

    fn room(&self, rid: &str) {
        self.ledger.lock().unwrap().rooms.push(rid.to_owned());
    }

    async fn cleanup(&self) {
        let l = std::mem::take(&mut *self.ledger.lock().unwrap());
        for t in l.teams {
            let r: Result<Value, _> = self.admin.post("teams.delete", &json!({"teamId": t})).await;
            if let Err(e) = r {
                eprintln!("teams.delete {t}: {e}");
            }
        }
        for rid in l.rooms {
            let _: Result<Value, _> = self
                .admin
                .post("rooms.delete", &json!({"roomId": rid}))
                .await;
        }
        for id in l.users {
            let r: Result<Value, _> = self
                .admin
                .post(
                    "users.delete",
                    &json!({"userId": id, "confirmRelinquish": true}),
                )
                .await;
            if let Err(e) = r {
                eprintln!("users.delete {id}: {e}");
            }
        }
    }
}

/// Runs `f`, then always cleans up.
async fn with_env<F, Fut>(f: F)
where
    F: FnOnce(Arc<Env>) -> Fut,
    Fut: Future<Output = ()> + Send + 'static,
{
    let Some(env) = Env::new().await else {
        eprintln!("RUSTKET_TEST_SERVER not set; skipping");
        return;
    };
    let env = Arc::new(env);
    let r = tokio::spawn(f(env.clone())).await;
    env.cleanup().await;
    if let Err(e) = r {
        std::panic::resume_unwind(e.into_panic());
    }
}

/// The latest 2FA code mailed to `address` (Mailpit), polled for a while.
async fn mailed_code(address: &str, newer_than: Option<&str>) -> (String, String) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    for _ in 0..40 {
        let mut s = tokio::net::TcpStream::connect("127.0.0.1:8025")
            .await
            .unwrap();
        let req = format!(
            "GET /api/v1/search?query=to:{address}&limit=1 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"
        );
        s.write_all(req.as_bytes()).await.unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        let body = text.split_once("\r\n\r\n").map(|x| x.1).unwrap_or_default();
        // Chunked or not: take the JSON object.
        let start = body.find('{').unwrap_or(0);
        let end = body.rfind('}').map_or(body.len(), |e| e + 1);
        if let Ok(v) = serde_json::from_str::<Value>(&body[start..end])
            && let Some(m) = v["messages"].as_array().and_then(|a| a.first())
        {
            let id = m["ID"].as_str().unwrap_or_default().to_owned();
            let snippet = m["Snippet"].as_str().unwrap_or_default();
            if Some(id.as_str()) != newer_than
                && let Some(code) = snippet
                    .split(|c: char| !(c.is_ascii_digit() || c == '-'))
                    .find(|w| w.len() == 7 && w.as_bytes()[3] == b'-')
            {
                return (id, code.replace('-', ""));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
    }
    panic!("no 2FA mail for {address}");
}

fn challenge(e: &Error) -> TwoFactorMethod {
    match e {
        Error::TwoFactorRequired(c) => c.method.clone(),
        other => panic!("expected totp-required, got {other:?}"),
    }
}

// --- tests ------------------------------------------------------------------------

#[tokio::test]
async fn profile_preferences_and_avatar() {
    with_env(|env| async move {
        let pw = "Acct-pass-1234!x";
        let (c, uid, username) = env.user("prof", pw).await;

        let policy = api::password_policy(&c).await.unwrap();
        assert!(policy.enabled && !policy.rules.is_empty());
        assert!(policy.unmet("Another-pass-5678!").is_empty());

        assert!(!api::username_available(&c, "alice").await.unwrap());
        assert!(api::username_available(&c, &format!("{username}-free")).await.unwrap());

        // Plain fields: no second factor.
        let u = api::update_own_basic_info(
            &c,
            json!({"nickname": "nick", "bio": "About me", "statusText": "Busy coding", "name": "Acct Renamed"}),
            None,
        )
        .await
        .unwrap();
        assert_eq!(u.nickname.as_deref(), Some("nick"));
        assert_eq!(u.bio.as_deref(), Some("About me"));
        assert_eq!(u.name.as_deref(), Some("Acct Renamed"));

        // Password: the server asks for the current password (no 2FA set up).
        let new_pw = "Changed-pass-9876!";
        let e = api::update_own_basic_info(&c, json!({"newPassword": new_pw}), None)
            .await
            .unwrap_err();
        assert_eq!(challenge(&e), TwoFactorMethod::Password);
        let e = api::update_own_basic_info(
            &c,
            json!({"newPassword": new_pw}),
            Some(TwoFactorCode::password("wrong")),
        )
        .await
        .unwrap_err();
        assert!(matches!(e, Error::TwoFactorInvalid(_)), "{e:?}");
        // A password that breaks the policy is refused by the server too.
        let e = api::update_own_basic_info(
            &c,
            json!({"newPassword": "short"}),
            Some(TwoFactorCode::password(pw)),
        )
        .await
        .unwrap_err();
        assert!(e.to_string().contains("password-policy"), "{e}");
        api::update_own_basic_info(
            &c,
            json!({"newPassword": new_pw}),
            Some(TwoFactorCode::password(pw)),
        )
        .await
        .unwrap();
        let check = client(&env.server);
        check.login_password(&username, new_pw, None).await.unwrap();
        let _ = check.logout().await;

        // E-mail change, also behind the password.
        let email = format!("{username}-new@rustket.local");
        let e = api::update_own_basic_info(&c, json!({"email": email}), None)
            .await
            .unwrap_err();
        assert_eq!(challenge(&e), TwoFactorMethod::Password);
        let u = api::update_own_basic_info(
            &c,
            json!({"email": email}),
            Some(TwoFactorCode::password(new_pw)),
        )
        .await
        .unwrap();
        let first = u.emails.unwrap().into_iter().next().unwrap();
        assert_eq!(first.address, email);
        assert!(!first.verified);
        api::send_confirmation_email(&c, &email).await.unwrap();

        // Preferences.
        api::set_preferences(
            &c,
            json!({"hideRoles": true, "clockMode": 2, "highlights": ["deploy"], "idleTimeLimit": 600, "language": "ru"}),
        )
        .await
        .unwrap();
        let me = c.me().await.unwrap();
        let p = me.preferences().unwrap();
        assert_eq!(p["hideRoles"], json!(true));
        assert_eq!(p["clockMode"], json!(2));
        assert_eq!(p["highlights"], json!(["deploy"]));
        assert_eq!(p["idleTimeLimit"], json!(600));
        assert_eq!(me.language.as_deref(), Some("ru"));

        // Avatar upload (multipart) and reset.
        let img = image::RgbaImage::from_fn(64, 64, |x, y| {
            image::Rgba([(x * 4) as u8, (y * 4) as u8, 200, 255])
        });
        let mut png = Vec::new();
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let before = c.download(&format!("/avatar/{username}")).await.unwrap();
        assert!(
            before.content_type.as_deref().is_some_and(|t| t.contains("svg")),
            "default avatar is SVG: {:?}",
            before.content_type
        );
        c.users_set_avatar("me.png", "image/png", png).await.unwrap();
        // `/avatar/…` needs the rc_uid/rc_token cookies (blocked for
        // anonymous requests by default); the download sends them.
        let mut etag = None;
        for _ in 0..20 {
            etag = env
                .admin
                .users_info(rocketchat::rest::UserLookup::Id(&uid))
                .await
                .unwrap()
                .avatar_etag;
            if etag.is_some() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(etag.is_some(), "uploaded avatar has an etag");
        let after = c.download(&format!("/avatar/{username}")).await.unwrap();
        let ct = after.content_type.clone().unwrap_or_default();
        assert!(ct.starts_with("image/") && !ct.contains("svg"), "{ct}");
        let bytes = after.bytes().await.unwrap();
        assert!(image::load_from_memory(&bytes).is_ok());
        c.users_reset_avatar(&uid).await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn totp_and_tokens() {
    with_env(|env| async move {
        let pw = "Acct-pass-1234!x";
        let (c, _uid, _username) = env.user("totp", pw).await;

        // Tokens without 2FA (a fresh user is inside the remember window).
        let t1 = api::generate_token(&c, "t1", false, None).await.unwrap();
        assert!(t1.len() > 20);
        let list = api::list_tokens(&c).await.unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "t1");
        assert!(t1.ends_with(&list[0].last_token_part));

        // TOTP setup (`users.enableTotp` / `users.validateTotp` take the
        // password as second factor; a fresh user may skip it).
        let second = Some(TwoFactorCode::password(pw));
        let e = api::totp_enable(&c, Some(TwoFactorCode::password("wrong"))).await;
        if let Err(e) = e {
            assert!(matches!(e, Error::TwoFactorInvalid(_)), "{e:?}");
        }
        let setup = api::totp_enable(&c, second.clone()).await.unwrap();
        assert!(setup.url.starts_with("otpauth://totp/"));
        assert!(setup.url.contains(&setup.secret));
        assert_eq!(
            api::totp_validate(&c, "000000", second.clone())
                .await
                .unwrap(),
            None
        );
        let codes = api::totp_validate(&c, &totp(&setup.secret), second.clone())
            .await
            .unwrap()
            .expect("backup codes");
        assert_eq!(codes.len(), 12);
        assert_eq!(api::totp_codes_remaining(&c).await.unwrap(), 12);
        assert!(c.me().await.unwrap().totp_enabled());

        // Protected calls now ask for TOTP; the code goes in the headers.
        let e = api::generate_token(&c, "t2", true, None).await.unwrap_err();
        assert_eq!(challenge(&e), TwoFactorMethod::Totp);
        let t2 = api::generate_token(
            &c,
            "t2",
            true,
            Some(TwoFactorCode::totp(&totp(&setup.secret))),
        )
        .await
        .unwrap();
        let regenerated = api::regenerate_token(&c, "t2", None).await.unwrap();
        assert_ne!(t2, regenerated);
        let list = api::list_tokens(&c).await.unwrap();
        assert!(list.iter().any(|t| t.name == "t2" && t.bypass_two_factor));
        api::remove_token(&c, "t1", None).await.unwrap();
        assert_eq!(api::list_tokens(&c).await.unwrap().len(), 1);

        // A PAT works like a login token.
        let pat = client(&env.server);
        pat.set_credentials(Some(rocketchat::Credentials::new(
            c.user_id().unwrap(),
            regenerated,
        )));
        assert_eq!(pat.me().await.unwrap().id, c.user_id().unwrap());

        // Backup codes: wrong code → None, right code → new set.
        assert_eq!(
            api::totp_regenerate_codes(&c, "111111").await.unwrap(),
            None
        );
        let fresh = api::totp_regenerate_codes(&c, &totp(&setup.secret))
            .await
            .unwrap()
            .expect("new codes");
        assert_ne!(fresh, codes);

        // Disable.
        assert!(!api::totp_disable(&c, "123123").await.unwrap());
        assert!(api::totp_disable(&c, &totp(&setup.secret)).await.unwrap());
        assert!(!c.me().await.unwrap().totp_enabled());
    })
    .await;
}

#[tokio::test]
async fn email_two_factor() {
    with_env(|env| async move {
        let pw = "Acct-pass-1234!x";
        let (c, _uid, username) = env.user("mail", pw).await;
        let address = format!("{username}@rustket.local");
        api::email_2fa_enable(&c).await.unwrap();
        assert!(c.me().await.unwrap().email_2fa_enabled());
        let e = api::email_2fa_disable(&c, None).await.unwrap_err();
        assert_eq!(challenge(&e), TwoFactorMethod::Email);
        let (_, code) = mailed_code(&address, None).await;
        api::email_2fa_disable(&c, Some(TwoFactorCode::email(&code)))
            .await
            .unwrap();
        assert!(!c.me().await.unwrap().email_2fa_enabled());
    })
    .await;
}

#[tokio::test]
async fn create_rooms_directory_and_open() {
    with_env(|env| async move {
        let pw = "Acct-pass-1234!x";
        let (c, _uid, username) = env.user("rooms", pw).await;
        let (_peer, _pid, peer) = env.user("peer", pw).await;
        let s = &env.suffix;

        // Channel with topic, member, read-only + broadcast.
        let name = format!("rk-acct-ch-{s}");
        assert!(!create_room::name_exists(&c, &name).await.unwrap());
        let o = create_room::create(
            c.clone(),
            Request::Room {
                private: false,
                params: CreateParams {
                    name: name.clone(),
                    members: vec![peer.clone()],
                    read_only: true,
                    extra_data: Some(json!({"topic": "Hello topic", "broadcast": true})),
                },
            },
        )
        .await
        .unwrap();
        env.room(&o.room.id);
        assert_eq!(o.room.topic.as_deref(), Some("Hello topic"));
        assert_eq!(o.room.ro, Some(true));
        assert_eq!(o.room.broadcast, Some(true));
        assert!(o.sub.is_some(), "creator is subscribed");
        assert!(create_room::name_exists(&c, &name).await.unwrap());
        let channel = o.room.id.clone();

        // Invalid names are refused by the server as well.
        let e = c
            .groups_create(&CreateParams::new("bad name!"))
            .await
            .unwrap_err();
        assert!(e.to_string().contains("error-invalid-room-name"), "{e}");

        // Private group.
        let o = create_room::create(
            c.clone(),
            Request::Room {
                private: true,
                params: CreateParams::new(format!("rk-acct-grp-{s}")),
            },
        )
        .await
        .unwrap();
        env.room(&o.room.id);
        assert_eq!(o.room.t, rocketchat::models::RoomType::Private);

        // Private team.
        let team_name = format!("rk-acct-team-{s}");
        let o = create_room::create(
            c.clone(),
            Request::Team(json!({"name": team_name, "type": 1, "members": [peer],
                "room": {"readOnly": false, "extraData": {"topic": "Team topic"}}})),
        )
        .await
        .unwrap();
        env.ledger
            .lock()
            .unwrap()
            .teams
            .push(o.room.team_id.clone().unwrap());
        assert_eq!(o.room.team_main, Some(true));
        assert_eq!(o.room.topic.as_deref(), Some("Team topic"));

        // Discussion with a first message.
        let o = create_room::create(
            c.clone(),
            Request::Discussion {
                prid: channel.clone(),
                name: format!("rk-acct disc {s}"),
                reply: Some("first message".into()),
                users: vec![peer.clone()],
                pmid: None,
            },
        )
        .await
        .unwrap();
        env.room(&o.room.id);
        assert_eq!(o.room.prid.as_deref(), Some(channel.as_str()));

        // Discussion started from a message ("Start a discussion"): the
        // server takes the message text as the discussion description.
        let parent: serde_json::Value = c
            .post(
                "chat.postMessage",
                &serde_json::json!({"roomId": channel, "text": format!("parent {s}")}),
            )
            .await
            .unwrap();
        let pmid = parent["message"]["_id"].as_str().unwrap().to_owned();
        let o = create_room::create(
            c.clone(),
            Request::Discussion {
                prid: channel.clone(),
                name: format!("rk-acct disc-msg {s}"),
                reply: None,
                users: vec![],
                pmid: Some(pmid.clone()),
            },
        )
        .await
        .unwrap();
        env.room(&o.room.id);
        assert_eq!(o.room.prid.as_deref(), Some(channel.as_str()));
        assert_eq!(o.room.description, Some(format!("parent {s}")));

        // DM and group DM.
        let o = create_room::create(c.clone(), Request::Direct(vec![peer.clone()]))
            .await
            .unwrap();
        env.room(&o.room.id);
        assert_eq!(o.room.t, rocketchat::models::RoomType::Direct);
        let dm = o.room.id.clone();
        let o = create_room::create(
            c.clone(),
            Request::Direct(vec![peer.clone(), "alice".into()]),
        )
        .await
        .unwrap();
        env.room(&o.room.id);
        assert_eq!(o.room.usernames.as_ref().map(Vec::len), Some(3));

        // Directory: channels (sorted, filtered), teams, users.
        let req = directory::Request {
            tab: Tab::Channels,
            text: format!("rk-acct-ch-{s}"),
            sort: Sort::Members,
            ascending: false,
            offset: 0,
        };
        let page = directory::fetch(c.clone(), req.clone()).await.unwrap();
        assert_eq!(page.total, 1);
        assert_eq!(page.rows[0].topic.as_deref(), Some("Hello topic"));
        assert_eq!(page.rows[0].users_count, Some(2));
        let teams = directory::fetch(
            c.clone(),
            directory::Request {
                tab: Tab::Teams,
                text: team_name.clone(),
                ..req.clone()
            },
        )
        .await
        .unwrap();
        assert_eq!(teams.rows.len(), 1);
        assert!(matches!(
            teams.rows[0].kind,
            directory::EntryKind::Team { private: true }
        ));
        let users = directory::fetch(
            c.clone(),
            directory::Request {
                tab: Tab::Users,
                text: username.clone(),
                sort: Sort::Username,
                ascending: true,
                offset: 0,
            },
        )
        .await
        .unwrap();
        assert_eq!(users.rows[0].username.as_deref(), Some(username.as_str()));

        // Spotlight finds the other user; join + preview of a channel the user
        // is not in.
        let r = c.spotlight(&peer).await.unwrap();
        assert!(
            r.users
                .iter()
                .any(|u| u.username.as_deref() == Some(peer.as_str()))
        );
        let other = env
            .admin
            .channels_create(&CreateParams::new(format!("rk-acct-other-{s}")))
            .await
            .unwrap();
        env.room(&other.id);
        let preview = open::resolve(c.clone(), Target::Room(other.id.clone()))
            .await
            .unwrap();
        assert!(preview.sub.is_none(), "not joined yet");
        let joined = open::resolve(c.clone(), Target::Join(other.id.clone()))
            .await
            .unwrap();
        assert!(joined.sub.is_some());

        // A hidden DM is shown again when opened.
        c.rooms_hide(&dm).await.unwrap();
        assert!(!c.subscriptions_get_one(&dm).await.unwrap().open);
        let reopened = open::resolve(c.clone(), Target::Room(dm.clone()))
            .await
            .unwrap();
        assert!(reopened.sub.unwrap().open);
        assert!(c.subscriptions_get_one(&dm).await.unwrap().open);
    })
    .await;
}
