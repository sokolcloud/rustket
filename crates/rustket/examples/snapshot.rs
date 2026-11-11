//! Headless screenshots of the real app, driven through `App::update` against
//! the dev server. No window and no GPU: iced's `UserInterface` with the
//! tiny-skia renderer, tasks and subscriptions executed by a small driver.
//!
//! ```sh
//! RUSTKET_TEST_SERVER=http://localhost:3000 cargo run -p rustket --example snapshot -- <out-dir>
//! ```
//!
//! `LC_ALL=ru_RU.UTF-8` renders the Russian UI, `RUSTKET_SNAPSHOT_WIDTH=1024` a
//! narrow window.
//!
//! Creates throwaway users (one with e-mail 2FA) and two throwaway channels,
//! posts a few messages and a DM, then renders: add-server, login (with the
//! Keycloak button), 2FA dialog, pending browser login, token-link
//! confirmation, workspace light/dark and an open room. Everything created is
//! recorded as soon as it exists and deleted at the end, also after a failure
//! or Ctrl-C. Shared rooms and accounts are not touched.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use iced::advanced::renderer::Headless;
use iced::advanced::subscription::{Hasher, into_recipes};
use iced::futures::{StreamExt, stream};
use iced::{Pixels, Size, mouse};
use iced_runtime::user_interface::{Cache, UserInterface};
use rocketchat::{Credentials, RestClient, ServerUrl, TlsConfig};
use rustket::app::{App, Flags, Modal, Screen, SecretsMode};
use rustket::message::{Message, ModalMessage};
use rustket::screens::{add_server, login};
use rustket::workspace::sidebar;
use serde_json::{Value, json};
use tokio::sync::mpsc;

#[path = "snapshot_panel/mod.rs"]
mod snapshot_panel;
use snapshot_panel::panel_shots;

const SIZE: Size = Size::new(1280.0, 800.0);

/// `RUSTKET_SNAPSHOT_WIDTH` (e.g. 1024 for narrow-window checks) or 1280.
fn shot_size() -> Size {
    let width = std::env::var("RUSTKET_SNAPSHOT_WIDTH")
        .ok()
        .and_then(|w| w.parse().ok())
        .unwrap_or(SIZE.width);
    Size::new(width, SIZE.height)
}

struct Driver {
    app: App,
    tx: mpsc::UnboundedSender<Message>,
    rx: mpsc::UnboundedReceiver<Message>,
    subs: HashMap<u64, tokio::task::JoinHandle<()>>,
}

impl Driver {
    fn new(flags: Flags) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let (app, task) = App::new(flags);
        let mut d = Self {
            app,
            tx,
            rx,
            subs: HashMap::new(),
        };
        d.spawn(task);
        d.sync_subscriptions();
        d
    }

    fn spawn(&self, task: iced::Task<Message>) {
        let Some(mut s) = iced_runtime::task::into_stream(task) else {
            return;
        };
        let tx = self.tx.clone();
        tokio::spawn(async move {
            while let Some(action) = s.next().await {
                if let iced_runtime::Action::Output(m) = action {
                    let _ = tx.send(m);
                }
            }
        });
    }

    fn sync_subscriptions(&mut self) {
        let recipes = into_recipes(self.app.subscription());
        let mut alive = Vec::new();
        for r in recipes {
            let mut h = Hasher::default();
            r.hash(&mut h);
            let id = std::hash::Hasher::finish(&h);
            alive.push(id);
            if self.subs.contains_key(&id) {
                continue;
            }
            let tx = self.tx.clone();
            let mut s = r.stream(stream::pending().boxed());
            self.subs.insert(
                id,
                tokio::spawn(async move {
                    while let Some(m) = s.next().await {
                        let _ = tx.send(m);
                    }
                }),
            );
        }
        self.subs.retain(|id, h| {
            let keep = alive.contains(id);
            if !keep {
                h.abort();
            }
            keep
        });
    }

    fn send(&mut self, m: Message) {
        let t = self.app.update(m);
        self.spawn(t);
        self.sync_subscriptions();
    }

    /// Processes messages until `done` holds (checked after each message) or
    /// `timeout` passes.
    async fn pump_until(&mut self, what: &str, timeout: Duration, done: impl Fn(&App) -> bool) {
        let deadline = Instant::now() + timeout;
        while !done(&self.app) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                panic!("timed out waiting for {what}");
            }
            match tokio::time::timeout(left.min(Duration::from_millis(200)), self.rx.recv()).await {
                Ok(Some(m)) => self.send(m),
                Ok(None) => panic!("channel closed"),
                Err(_) => {}
            }
        }
    }

    /// Processes whatever arrives for `d` (e.g. avatars).
    async fn settle(&mut self, d: Duration) {
        let deadline = Instant::now() + d;
        while let Ok(Some(m)) = tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            self.rx.recv(),
        )
        .await
        {
            self.send(m);
        }
    }

    fn shoot(&self, dir: &Path, name: &str) {
        self.shoot_at(dir, name, shot_size());
    }

    /// Like `shoot` with another window size (e.g. a whole settings page).
    fn shoot_at(&self, dir: &Path, name: &str, size: Size) {
        let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
            rustket_ui::theme::DEFAULT_FONT,
            Pixels(rustket_ui::theme::TEXT_BODY),
        ));
        let theme = self.app.theme();
        let t = rustket_ui::tokens(&theme);
        let mut ui = UserInterface::build(self.app.view(), size, Cache::default(), &mut renderer);
        let cursor = mouse::Cursor::Available(iced::Point::new(-10.0, -10.0));
        let mut msgs = Vec::new();
        let _ = ui.update(
            &[iced::Event::Window(iced::window::Event::RedrawRequested(
                Instant::now(),
            ))],
            cursor,
            &mut renderer,
            &mut NoClip,
            &mut msgs,
        );
        ui.draw(
            &mut renderer,
            &theme,
            &iced::advanced::renderer::Style {
                text_color: t.text_primary,
            },
            cursor,
        );
        let rgba = renderer.screenshot(
            Size::new(size.width as u32, size.height as u32),
            1.0,
            t.surface_room,
        );
        let path = dir.join(format!("{name}.png"));
        write_png(&path, size.width as u32, size.height as u32, &rgba);
        println!("wrote {}", path.display());
    }
}

struct NoClip;

impl iced::advanced::Clipboard for NoClip {
    fn read(&self, _: iced::advanced::clipboard::Kind) -> Option<String> {
        None
    }
    fn write(&mut self, _: iced::advanced::clipboard::Kind, _: String) {}
}

fn write_png(path: &Path, w: u32, h: u32, rgba: &[u8]) {
    let file = std::fs::File::create(path).expect("create png");
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(rgba).expect("png data");
}

/// Everything the example created on the shared server; filled in as soon as
/// each object exists so cleanup works after any failure.
#[derive(Default)]
struct Created {
    users: Vec<String>,
    rooms: Vec<String>,
    /// Team ids (deleted before the rooms).
    teams: Vec<String>,
}

type Ledger = Arc<Mutex<Created>>;

struct Fixture {
    user: (String, String, String), // id, username, password
    tfa_user: (String, String),     // username, password (email 2FA)
    ledger: Ledger,
}

async fn rest(server: &ServerUrl) -> RestClient {
    let tls = TlsConfig::new().build().expect("tls");
    RestClient::new(server.clone(), &tls).expect("rest")
}

async fn admin(server: &ServerUrl) -> RestClient {
    let admin = rest(server).await;
    admin
        .login_password("admin", "adminpass", None)
        .await
        .expect("admin login");
    admin
}

async fn create_user(
    admin: &RestClient,
    ledger: &Ledger,
    username: &str,
    password: &str,
) -> String {
    let v: Value = admin
        .post(
            "users.create",
            &json!({
                "username": username,
                "email": format!("{username}@rustket.local"),
                "name": format!("Snap {}", &username[username.len() - 4..]),
                "password": password,
                "verified": true,
                "joinDefaultChannels": false,
                "requirePasswordChange": false,
                "sendWelcomeEmail": false,
            }),
        )
        .await
        .expect("users.create");
    let id = v["user"]["_id"].as_str().expect("user id").to_owned();
    ledger.lock().expect("ledger").users.push(id.clone());
    id
}

async fn setup(server: &ServerUrl, ledger: Ledger) -> Fixture {
    let admin = admin(server).await;
    let suffix = format!(
        "{:x}",
        std::process::id() ^ (chrono::Utc::now().timestamp_millis() as u32)
    );
    let user_name = format!("rk-snap-{suffix}");
    let other_name = format!("rk-peer-{suffix}");
    let tfa_name = format!("rk-tfa-{suffix}");
    let password = format!("pw-{suffix}-Aa1!");
    let uid = create_user(&admin, &ledger, &user_name, &password).await;
    create_user(&admin, &ledger, &other_name, &password).await;
    create_user(&admin, &ledger, &tfa_name, &password).await;

    // Throwaway rooms: nothing is posted into shared channels.
    let mut rooms = Vec::new();
    for (name, topic) in [
        (format!("rk-snap-dev-{suffix}"), "Release coordination"),
        (format!("rk-snap-chat-{suffix}"), "Off-topic"),
    ] {
        let v: Value = admin
            .post(
                "channels.create",
                &json!({"name": name, "members": [user_name, other_name]}),
            )
            .await
            .expect("channels.create");
        let rid = v["channel"]["_id"].as_str().expect("rid").to_owned();
        ledger.lock().expect("ledger").rooms.push(rid.clone());
        let _: Value = admin
            .post("channels.setTopic", &json!({"roomId": rid, "topic": topic}))
            .await
            .expect("topic");
        rooms.push(rid);
    }
    // A read-only throwaway channel (composer notice).
    let v: Value = admin
        .post(
            "channels.create",
            &json!({"name": format!("rk-snap-ro-{suffix}"), "members": [user_name], "readOnly": true}),
        )
        .await
        .expect("channels.create read-only");
    let ro = v["channel"]["_id"].as_str().expect("rid").to_owned();
    ledger.lock().expect("ledger").rooms.push(ro);

    let user = rest(server).await;
    user.login_password(&user_name, &password, None)
        .await
        .expect("user login");
    let other = rest(server).await;
    other
        .login_password(&other_name, &password, None)
        .await
        .expect("peer login");
    let _: Value = user
        .post(
            "chat.postMessage",
            &json!({"roomId": rooms[1], "text": "Hello from the snapshot user"}),
        )
        .await
        .expect("post");
    for (rid, text) in [
        (
            &rooms[1],
            "Welcome! This message comes from a throwaway peer.",
        ),
        (&rooms[0], "Build is green, deploying to staging."),
        (&rooms[0], "@{user} could you review the release notes?"),
    ] {
        let text = text.replace("{user}", &user_name);
        let _: Value = other
            .post("chat.postMessage", &json!({"roomId": rid, "text": text}))
            .await
            .expect("post");
    }
    let dm: Value = other
        .post("im.create", &json!({"username": user_name}))
        .await
        .expect("im.create");
    let rid = dm["room"]["_id"].as_str().unwrap_or_default().to_owned();
    let _: Value = other
        .post(
            "chat.postMessage",
            &json!({"roomId": rid, "text": "Hi! Do you have a minute?"}),
        )
        .await
        .expect("dm");
    timeline_fixture::setup(
        &admin,
        &user,
        &other,
        &ledger,
        &suffix,
        [&user_name, &other_name, &tfa_name],
    )
    .await;
    let _ = user.logout().await;
    let _ = other.logout().await;

    // A throwaway account with e-mail 2FA (instead of the shared `carol`).
    let tfa = rest(server).await;
    tfa.login_password(&tfa_name, &password, None)
        .await
        .expect("2fa user login");
    tfa.post_ok("users.2fa.enableEmail", &json!({}))
        .await
        .expect("enable email 2FA");
    let _ = tfa.logout().await;
    let _ = admin.logout().await;
    Fixture {
        user: (uid, user_name, password.clone()),
        tfa_user: (tfa_name, password),
        ledger,
    }
}

/// Deletes everything recorded in the ledger (rooms first, then users).
async fn teardown(server: &ServerUrl, ledger: &Ledger) {
    timeline_fixture::teardown(server).await;
    let (rooms, users) = {
        let mut g = ledger.lock().expect("ledger");
        (std::mem::take(&mut g.rooms), std::mem::take(&mut g.users))
    };
    if rooms.is_empty() && users.is_empty() {
        return;
    }
    let admin = admin(server).await;
    let teams = std::mem::take(&mut ledger.lock().expect("ledger").teams);
    for id in teams {
        let r: Result<Value, _> = admin.post("teams.delete", &json!({"teamId": id})).await;
        if let Err(e) = r {
            eprintln!("teams.delete {id}: {e}");
        }
    }
    for rid in rooms {
        let r: Result<Value, _> = admin.post("channels.delete", &json!({"roomId": rid})).await;
        match r {
            Ok(_) => println!("deleted room {rid}"),
            Err(_) => match admin.rooms_delete(&rid).await {
                Ok(()) => println!("deleted room {rid}"),
                Err(e) => eprintln!("rooms.delete {rid}: {e}"),
            },
        }
    }
    for id in users {
        let r: Result<Value, _> = admin
            .post(
                "users.delete",
                &json!({"userId": id, "confirmRelinquish": true}),
            )
            .await;
        match r {
            Ok(_) => println!("deleted user {id}"),
            Err(e) => eprintln!("users.delete {id}: {e}"),
        }
    }
    let _ = admin.logout().await;
}

fn active_login(app: &App) -> Option<&login::Login> {
    let sid = app.active.as_ref()?;
    app.servers.get(sid)?.login.as_ref()
}

/// Resolves on SIGTERM or SIGHUP with the signal's name; never on Windows.
async fn terminated() -> &'static str {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        let mut hup = signal(SignalKind::hangup()).expect("SIGHUP handler");
        tokio::select! {
            _ = term.recv() => "terminated",
            _ = hup.recv() => "hangup",
        }
    }
    #[cfg(not(unix))]
    std::future::pending().await
}

#[tokio::main]
async fn main() {
    let server_str =
        std::env::var("RUSTKET_TEST_SERVER").unwrap_or_else(|_| "http://localhost:3000".into());
    let server = ServerUrl::parse(&server_str).expect("server url");
    let out: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("snapshots"));
    std::fs::create_dir_all(&out).expect("out dir");
    // Icons need the bundled lucide font (the app loads it via `settings()`).
    for f in rustket_ui::fonts() {
        iced::advanced::graphics::text::font_system()
            .write()
            .expect("font system")
            .load_font(f);
    }
    rust_i18n::set_locale("en");

    let ledger: Ledger = Arc::default();
    let work = {
        let (server, ledger) = (server.clone(), ledger.clone());
        tokio::spawn(async move {
            if titlebar::titlebar_shots(&out).await {
                return;
            }
            if admin_shots::admin_shots(&server, &out).await {
                return;
            }
            let fixture = setup(&server, ledger).await;
            if cache_shots::cache_shots(&server, &out, &fixture.user).await {
                return;
            }
            run(server, out, fixture).await;
        })
    };
    let abort = work.abort_handle();
    // Setup or run failing, finishing, or Ctrl-C: always clean up.
    // Setup or run failing, finishing, Ctrl-C, SIGTERM (e.g. an outer
    // `timeout`) or SIGHUP: always clean up.
    let result = tokio::select! {
        r = work => r.err(),
        _ = tokio::signal::ctrl_c() => {
            eprintln!("interrupted; cleaning up");
            abort.abort();
            None
        }
        why = terminated() => {
            eprintln!("{why}; cleaning up");
            abort.abort();
            None
        }
    };
    teardown(&server, &ledger).await;
    if let Some(e) = result
        && e.is_panic()
    {
        std::panic::resume_unwind(e.into_panic());
    }
}

/// The link-handler modal: a stale registration of another build, then ours.
fn handler_shots(d: &mut Driver, out: &Path) {
    let modal = |current: &str| Modal::RegisterHandler {
        current: Some(current.to_owned()),
        loaded: true,
        busy: false,
        error: None,
        warnings: Vec::new(),
    };
    d.app.handler_registered = false;
    d.app.modals.push(modal("rustket-url-handler.desktop"));
    d.shoot(out, "handler-stale");
    d.app.modals.pop();
    d.app.handler_registered = true;
    d.app.modals.push(modal("rustket.desktop"));
    d.shoot(out, "handler-registered");
    d.app.modals.pop();
    d.app.handler_registered = false;
}

fn confirm_modals(app: &App) -> usize {
    app.modals
        .iter()
        .filter(|m| matches!(m, Modal::ConfirmToken { .. }))
        .count()
}

async fn run(server: ServerUrl, out: PathBuf, fixture: Fixture) {
    let user = fixture.user;
    let cache = tempfile::tempdir().expect("cache dir");
    let mut d = Driver::new(Flags {
        ephemeral: true,
        cache_dir: Some(cache.path().to_owned()),
        secrets: SecretsMode::Memory,
        open_urls: false,
        ..Flags::default()
    });
    d.pump_until("add-server screen", Duration::from_secs(10), |a| {
        matches!(a.screen, Screen::AddServer(_))
    })
    .await;
    d.send(Message::AddServer(add_server::Message::UrlChanged(
        server.as_str().to_owned(),
    )));
    d.shoot(&out, "01-add-server");
    handler_shots(&mut d, &out);
    if std::env::var("RUSTKET_SNAPSHOT_ONLY").as_deref() == Ok("handler") {
        return;
    }

    d.send(Message::AddServer(add_server::Message::Submit));
    d.pump_until("login screen", Duration::from_secs(20), |a| {
        active_login(a).is_some_and(|l| l.meta.is_some())
    })
    .await;
    let sid = d.app.active.clone().expect("active server");
    assert!(
        active_login(&d.app)
            .and_then(|l| l.meta.as_ref())
            .is_some_and(|m| m.services.iter().any(|s| s.service_name() == "keycloak")),
        "keycloak button expected"
    );
    d.shoot(&out, "02-login");

    // 2FA dialog: the throwaway user has e-mail 2FA.
    d.send(Message::Login(
        sid.clone(),
        login::Message::User(fixture.tfa_user.0.clone()),
    ));
    d.send(Message::Login(
        sid.clone(),
        login::Message::Password(fixture.tfa_user.1.clone()),
    ));
    d.send(Message::Login(sid.clone(), login::Message::Submit));
    d.pump_until("2FA dialog", Duration::from_secs(20), |a| {
        active_login(a).is_some_and(|l| l.two_factor.is_some() && !l.busy)
    })
    .await;
    d.shoot(&out, "03-two-factor");
    d.send(Message::Login(sid.clone(), login::Message::CancelCode));
    d.send(Message::Login(
        sid.clone(),
        login::Message::User(String::new()),
    ));
    d.send(Message::Login(
        sid.clone(),
        login::Message::Password(String::new()),
    ));

    // Pending browser login.
    d.send(Message::Login(sid.clone(), login::Message::BrowserLogin));
    assert!(active_login(&d.app).is_some_and(|l| l.pending.is_some()));
    d.shoot(&out, "04-pending-browser");

    // A token link for another host is ignored outright (no lookup started);
    // one for this server while the browser login is pending asks for
    // confirmation.
    let token_client = rest(&server).await;
    let login_result = token_client
        .login_password(&user.1, &user.2, None)
        .await
        .expect("token login");
    let link = format!(
        "rocketchat://auth?host={}&userId={}&token={}",
        server.as_str(),
        login_result.user_id,
        login_result.auth_token
    );
    let ignored = d.app.update(Message::DeepLinks(vec![
        link.replace("localhost", "127.0.0.1"),
    ]));
    assert_eq!(ignored.units(), 0, "foreign host must be ignored");
    assert!(d.app.modals.is_empty());
    d.send(Message::DeepLinks(vec![link]));
    d.pump_until("armed token confirmation", Duration::from_secs(10), |a| {
        a.modals
            .iter()
            .any(|m| matches!(m, Modal::ConfirmToken { armed: true, .. }))
    })
    .await;
    assert_eq!(confirm_modals(&d.app), 1);
    d.settle(Duration::from_millis(300)).await;
    d.shoot(&out, "05-confirm-token");
    d.send(Message::Modal(ModalMessage::ConfirmToken));
    assert_eq!(confirm_modals(&d.app), 0);

    d.pump_until("workspace loaded", Duration::from_secs(30), |a| {
        a.servers
            .get(a.active.as_ref().expect("active"))
            .is_some_and(|s| {
                s.login.is_none()
                    && s.workspace.last_sync().is_some()
                    && s.sidebar.room_count() >= 3
            })
    })
    .await;
    d.settle(Duration::from_secs(2)).await;
    d.shoot(&out, "06-workspace-light");
    d.app.config.ui.theme = rustket_core::config::Theme::Dark;
    d.shoot(&out, "07-workspace-dark");
    d.send(Message::OpenQuickSwitcher);
    d.send(Message::QuickSwitcher(
        sid.clone(),
        rustket::dialogs::quick_switcher::Message::Query("rk-snap".into()),
    ));
    d.shoot(&out, "07b-quick-switcher-dark");
    d.send(Message::CloseModal);
    d.send(Message::Rail(rustket::workspace::rail::Message::Settings));
    d.shoot(&out, "07c-settings-dark");
    // The whole settings page, dark and light.
    let tall = Size::new(SIZE.width, 1900.0);
    d.settle(Duration::from_millis(300)).await;
    d.shoot_at(&out, "07d-settings-full-dark", tall);
    d.app.config.ui.theme = rustket_core::config::Theme::Light;
    d.shoot_at(&out, "07e-settings-full-light", tall);
    d.app.config.ui.theme = rustket_core::config::Theme::Dark;
    d.send(Message::Settings(
        rustket::screens::settings::Message::Close,
    ));

    // Status menu: set a status text through it, then open it.
    {
        use rustket::workspace::status_menu::Message as Sm;
        d.send(Message::StatusMenu(sid.clone(), Sm::Toggle(true)));
        d.send(Message::StatusMenu(
            sid.clone(),
            Sm::TextChanged("Shipping the release".into()),
        ));
        d.send(Message::StatusMenu(sid.clone(), Sm::SubmitText));
        d.pump_until("status text set", Duration::from_secs(15), |a| {
            a.servers[&sid]
                .workspace
                .me()
                .and_then(|m| m.status_text.as_deref())
                == Some("Shipping the release")
        })
        .await;
        d.send(Message::StatusMenu(sid.clone(), Sm::Toggle(true)));
        d.settle(Duration::from_millis(500)).await;
        d.shoot(&out, "07f-status-menu-dark");
        d.app.config.ui.theme = rustket_core::config::Theme::Light;
        d.shoot(&out, "07g-status-menu-light");
        d.app.config.ui.theme = rustket_core::config::Theme::Dark;
        d.send(Message::StatusMenu(sid.clone(), Sm::Toggle(false)));
    }

    // Open the throwaway dev channel through the sidebar.
    let rid = {
        let s = &d.app.servers[&sid];
        s.workspace
            .rooms()
            .values()
            .find(|e| e.name().starts_with("rk-snap-dev-"))
            .map(|e| e.rid().to_owned())
            .expect("dev channel in sidebar")
    };
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open(rid.clone()),
    ));
    d.pump_until("room open", Duration::from_secs(20), |a| {
        a.servers[&sid]
            .rooms
            .get(&rid)
            .is_some_and(|r| r.loaded && r.has_watch())
    })
    .await;
    d.pump_until("room marked read", Duration::from_secs(10), |a| {
        a.servers[&sid]
            .workspace
            .subscription(&rid)
            .is_some_and(|s| s.unread == 0)
    })
    .await;
    d.settle(Duration::from_secs(1)).await;
    d.shoot(&out, "08-room-dark");
    d.app.config.ui.theme = rustket_core::config::Theme::Light;
    d.shoot(&out, "09-room-light");
    // `RUSTKET_SNAPSHOT_SELECTION`: only the text selection shots after the room.
    if std::env::var("RUSTKET_SNAPSHOT_SELECTION").is_ok() {
        selection::selection_shots(&mut d, &server, &out, &sid, &rid).await;
        return;
    }
    shell::shell_shots(&mut d, &out, &sid, &rid).await;
    // `RUSTKET_SNAPSHOT_POLISH`: focus rings, storage settings, 1024 px.
    if std::env::var("RUSTKET_SNAPSHOT_POLISH").is_ok() {
        polish::polish_shots(&mut d, &server, &out, &sid, &rid, &user).await;
        return;
    }
    // `RUSTKET_SNAPSHOT_SIDEBAR`: only the configurable sidebar shots.
    if std::env::var("RUSTKET_SNAPSHOT_SIDEBAR").is_ok() {
        sidebar_layout::sidebar_shots(&mut d, &server, &out, &sid, &user, &fixture.ledger).await;
        return;
    }
    // `RUSTKET_SNAPSHOT_SHELL`: only the shots up to the shell states.
    if std::env::var("RUSTKET_SNAPSHOT_SHELL").is_ok() {
        return;
    }
    e2e::e2e_shots(&mut d, &server, &out, &sid, &user, &fixture.ledger).await;
    // Any `RUSTKET_SNAPSHOT_ONLY` value skips the shared component shots.
    let only_glue = std::env::var("RUSTKET_SNAPSHOT_ONLY").is_ok();
    if !only_glue {
        composer_snapshots(&mut d, &out, &sid, &rid).await;
        timeline_fixture::shoot(&mut d, &out, &sid).await;
        panel_shots(&mut d, &server, &out, &sid, &rid, &user, &fixture.ledger).await;
        account_shots(&mut d, &out, &sid).await;
    }
    glue::glue_shots(&mut d, &server, &out, &sid, &rid, &fixture.ledger).await;
    omni::omni_shots(&mut d, &server, &out, &sid, &user).await;
    uikit::shoot(&mut d, &out, &sid, &rid).await;
    // Last: it leaves a team room open.
    teams::teams_shots(&mut d, &server, &out, &sid, &user, &fixture.ledger).await;

    // Log out through the app: the login token is revoked on the server.
    let creds: Credentials = d.app.servers[&sid].credentials.clone().expect("creds");
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Status(rustket::workspace::status_menu::Message::Logout),
    ));
    d.pump_until("login after logout", Duration::from_secs(20), |a| {
        a.servers[&sid]
            .login
            .as_ref()
            .is_some_and(|l| l.meta.is_some())
    })
    .await;
    let check = rest(&server).await;
    check.set_credentials(Some(creds));
    let mut revoked = false;
    for _ in 0..20 {
        if matches!(check.me().await, Err(e) if e.is_unauthorized()) {
            revoked = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    assert!(revoked, "logout must revoke the login token");
    println!("done");
}

/// Composer states: idle, `@` autocomplete, emoji picker, upload dialog (via
/// a file dropped onto the window), edit mode and the read-only notice.
async fn composer_snapshots(d: &mut Driver, out: &Path, sid: &rustket_core::ServerId, rid: &str) {
    use iced::widget::text_editor::{Action as EditorAction, Edit};
    use rustket::room::{self, composer};

    fn send(d: &mut Driver, sid: &rustket_core::ServerId, rid: &str, m: composer::Message) {
        let epoch = d.app.servers[sid].generation;
        d.send(Message::Room(
            sid.clone(),
            epoch,
            rid.to_owned(),
            room::Message::Composer(m),
        ));
    }
    fn type_text(d: &mut Driver, sid: &rustket_core::ServerId, rid: &str, text: &str) {
        for ch in text.chars() {
            send(
                d,
                sid,
                rid,
                composer::Message::Editor(EditorAction::Edit(Edit::Insert(ch))),
            );
        }
    }
    fn clear(d: &mut Driver, sid: &rustket_core::ServerId, rid: &str) {
        send(
            d,
            sid,
            rid,
            composer::Message::Editor(EditorAction::SelectAll),
        );
        send(
            d,
            sid,
            rid,
            composer::Message::Editor(EditorAction::Edit(Edit::Backspace)),
        );
    }
    fn both(d: &mut Driver, out: &Path, name: &str) {
        d.app.config.ui.theme = rustket_core::config::Theme::Light;
        d.shoot(out, &format!("{name}-light"));
        d.app.config.ui.theme = rustket_core::config::Theme::Dark;
        d.shoot(out, &format!("{name}-dark"));
    }
    fn composer_in<'a>(
        a: &'a App,
        sid: &rustket_core::ServerId,
        rid: &str,
    ) -> Option<&'a composer::Composer> {
        a.servers.get(sid)?.rooms.get(rid).map(|r| &r.composer)
    }
    let composer_of = |a: &App| {
        composer_in(a, sid, rid).map(|c| {
            (
                c.popup_items().len(),
                c.is_upload_dialog_open(),
                c.is_editing(),
            )
        })
    };

    both(d, out, "10-composer-idle");

    type_text(d, sid, rid, "Hi @rk");
    d.pump_until("@ autocomplete", Duration::from_secs(10), |a| {
        composer_of(a).is_some_and(|c| c.0 >= 2)
    })
    .await;
    d.settle(Duration::from_millis(800)).await;
    both(d, out, "11-composer-mention");
    send(d, sid, rid, composer::Message::AcClose);
    clear(d, sid, rid);

    send(d, sid, rid, composer::Message::TogglePicker);
    d.settle(Duration::from_millis(300)).await;
    both(d, out, "12-composer-emoji-picker");
    send(d, sid, rid, composer::Message::TogglePicker);

    let tmp = tempfile::tempdir().expect("tmp");
    let png = tmp.path().join("release-diagram.png");
    let img = image_bytes(480, 300);
    std::fs::write(&png, img).expect("write png");
    d.send(Message::FileDropped(png));
    d.pump_until("upload dialog", Duration::from_secs(10), |a| {
        composer_of(a).is_some_and(|c| c.1)
    })
    .await;
    both(d, out, "13-composer-upload");
    send(
        d,
        sid,
        rid,
        composer::Message::Upload(rustket::room::upload::Message::Close),
    );

    type_text(d, sid, rid, "Release notes are ready for review");
    send(d, sid, rid, composer::Message::Send);
    d.pump_until("message sent", Duration::from_secs(10), |a| {
        a.servers[sid].workspace.pending_messages(rid).is_empty()
            && a.servers[sid].workspace.timeline(rid).is_some_and(|t| {
                t.messages()
                    .any(|m| m.msg == "Release notes are ready for review")
            })
    })
    .await;
    send(d, sid, rid, composer::Message::EditLast);
    assert!(composer_of(&d.app).is_some_and(|c| c.2), "edit mode");
    type_text(d, sid, rid, " (v2)");
    both(d, out, "14-composer-edit");
    send(d, sid, rid, composer::Message::Cancel);

    // Slash commands: descriptions and parameter hints are translated.
    type_text(d, sid, rid, "/");
    d.pump_until("/ autocomplete", Duration::from_secs(10), |a| {
        composer_of(a).is_some_and(|c| c.0 >= 5)
    })
    .await;
    both(d, out, "16-composer-commands");
    send(d, sid, rid, composer::Message::AcClose);
    clear(d, sid, rid);

    let ro = d.app.servers[sid]
        .workspace
        .rooms()
        .values()
        .find(|e| e.name().starts_with("rk-snap-ro-"))
        .map(|e| e.rid().to_owned())
        .expect("read-only channel");
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Open(ro.clone()),
    ));
    d.pump_until("read-only room open", Duration::from_secs(20), |a| {
        a.servers[sid].rooms.get(&ro).is_some_and(|r| r.loaded)
    })
    .await;
    d.settle(Duration::from_millis(500)).await;
    both(d, out, "15-composer-read-only");
    d.app.config.ui.theme = rustket_core::config::Theme::Light;
}

/// A small gradient PNG for the upload dialog.
fn image_bytes(w: u32, h: u32) -> Vec<u8> {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&[(x * 255 / w) as u8, (y * 255 / h) as u8, 200, 255]);
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut wr = enc.write_header().expect("png header");
    wr.write_image_data(&data).expect("png data");
    drop(wr);
    out
}
#[path = "snapshot/e2e.rs"]
mod e2e;
#[path = "snapshot/glue.rs"]
mod glue;
#[path = "snapshot/polish.rs"]
mod polish;
#[path = "snapshot/selection.rs"]
mod selection;
#[path = "snapshot/shell.rs"]
mod shell;
#[path = "snapshot/sidebar_layout.rs"]
mod sidebar_layout;

#[path = "snapshot/admin.rs"]
mod admin_shots;
#[path = "snapshot/cache.rs"]
mod cache_shots;
#[path = "snapshot/omni.rs"]
mod omni;
#[path = "snapshot/teams.rs"]
mod teams;
#[path = "snapshot/timeline_fixture.rs"]
mod timeline_fixture;
#[path = "snapshot/titlebar.rs"]
mod titlebar;
#[path = "snapshot/uikit.rs"]
mod uikit;
/// Quick switcher, directory, create dialogs and account tabs (light + dark).
/// Nothing is created on the server except a personal access token and a
/// pending TOTP secret of the throwaway user.
async fn account_shots(d: &mut Driver, out: &Path, sid: &rustket_core::ServerId) {
    use rustket::dialogs::{create_room, quick_switcher};
    use rustket::screens::{account, directory};
    use rustket_core::config::Theme;

    fn both(d: &mut Driver, out: &Path, name: &str) {
        d.app.config.ui.theme = Theme::Light;
        d.shoot(out, &format!("{name}-light"));
        d.app.config.ui.theme = Theme::Dark;
        d.shoot(out, &format!("{name}-dark"));
    }
    fn qs(app: &App) -> Option<&quick_switcher::QuickSwitcher> {
        app.modals.iter().find_map(|m| match m {
            Modal::QuickSwitcher(_, q) => Some(&**q),
            _ => None,
        })
    }
    fn create(app: &App) -> Option<&create_room::CreateRoom> {
        app.modals.iter().find_map(|m| match m {
            Modal::CreateRoom(_, c) => Some(&**c),
            _ => None,
        })
    }
    fn acct(app: &App) -> Option<&account::Account> {
        match &app.screen {
            Screen::Account(_, a) => Some(a),
            _ => None,
        }
    }

    // Quick switcher: local rooms plus spotlight people.
    d.send(Message::OpenQuickSwitcher);
    d.send(Message::QuickSwitcher(
        sid.clone(),
        quick_switcher::Message::Query("rk".into()),
    ));
    d.pump_until("spotlight", Duration::from_secs(10), |a| {
        qs(a).is_some_and(|q| !q.searching && q.remote.is_some())
    })
    .await;
    d.send(Message::QuickSwitcher(
        sid.clone(),
        quick_switcher::Message::Down,
    ));
    d.settle(Duration::from_millis(500)).await;
    both(d, out, "20-quick-switcher");
    d.send(Message::CloseModal);

    // Directory.
    d.send(Message::Sidebar(sid.clone(), sidebar::Message::Directory));
    d.pump_until(
        "directory",
        Duration::from_secs(10),
        |a| matches!(&a.screen, Screen::Directory(_, x) if x.page.is_some() && !x.loading),
    )
    .await;
    d.settle(Duration::from_millis(800)).await;
    both(d, out, "21-directory-channels");
    d.send(Message::Directory(
        sid.clone(),
        directory::Message::Tab(directory::Tab::Users),
    ));
    d.pump_until(
        "directory users",
        Duration::from_secs(10),
        |a| matches!(&a.screen, Screen::Directory(_, x) if x.page.is_some() && !x.loading),
    )
    .await;
    d.settle(Duration::from_millis(800)).await;
    both(d, out, "22-directory-users");
    d.send(Message::Directory(sid.clone(), directory::Message::Close));

    // Create dialogs.
    let peer = d.app.servers[sid]
        .workspace
        .rooms()
        .values()
        .find(|e| e.name().starts_with("rk-peer-"))
        .map(|e| e.name().to_owned())
        .unwrap_or_default();
    let parent = d.app.servers[sid]
        .workspace
        .rooms()
        .values()
        .find(|e| e.name().starts_with("rk-snap-dev-"))
        .map(|e| (e.rid().to_owned(), e.name().to_owned()))
        .expect("dev channel");
    let cr = |m| Message::CreateRoom(sid.clone(), m);
    d.send(Message::Sidebar(sid.clone(), sidebar::Message::Create));
    for (kind, name) in [
        (create_room::Kind::Channel, "23-create-channel"),
        (create_room::Kind::Team, "24-create-team"),
        (create_room::Kind::DirectMessage, "25-create-dm"),
        (create_room::Kind::Discussion, "26-create-discussion"),
    ] {
        d.send(cr(create_room::Message::Kind(kind)));
        match kind {
            create_room::Kind::DirectMessage => {}
            create_room::Kind::Discussion => {
                d.send(cr(create_room::Message::Parent(
                    parent.0.clone(),
                    parent.1.clone(),
                )));
                d.send(cr(create_room::Message::Name("Release checklist".into())));
                d.send(cr(create_room::Message::FirstMessage(
                    "What is left before Friday?".into(),
                )));
            }
            _ => {
                d.send(cr(create_room::Message::Name("project-alpha".into())));
                d.send(cr(create_room::Message::Topic(
                    "Planning and releases".into(),
                )));
            }
        }
        if !peer.is_empty() {
            d.send(cr(create_room::Message::MemberQuery("rk-peer".into())));
            d.pump_until("member suggestions", Duration::from_secs(10), |a| {
                create(a).is_some_and(|c| !c.suggestions.is_empty())
            })
            .await;
            if kind != create_room::Kind::Discussion {
                d.send(cr(create_room::Message::AddMember(peer.clone())));
                d.send(cr(create_room::Message::MemberQuery("rk-peer".into())));
                d.settle(Duration::from_millis(600)).await;
            }
        }
        d.settle(Duration::from_millis(600)).await;
        both(d, out, name);
        for m in create(&d.app)
            .map(|c| c.members.clone())
            .unwrap_or_default()
        {
            d.send(cr(create_room::Message::RemoveMember(m)));
        }
        d.send(cr(create_room::Message::MemberQuery(String::new())));
        d.send(cr(create_room::Message::Name(String::new())));
        d.send(cr(create_room::Message::Topic(String::new())));
    }
    d.send(cr(create_room::Message::Close));

    // Account tabs.
    let am = |m| Message::Account(sid.clone(), m);
    d.send(Message::Sidebar(
        sid.clone(),
        sidebar::Message::Status(rustket::workspace::status_menu::Message::Account),
    ));
    d.pump_until("account", Duration::from_secs(10), |a| {
        acct(a).is_some_and(|x| x.profile.loaded_for.is_some() && x.policy.enabled)
    })
    .await;
    d.send(am(account::Message::Profile(
        account::profile::Msg::NewPassword("Weak-pass".into()),
    )));
    d.settle(Duration::from_millis(800)).await;
    both(d, out, "27-account-profile");
    d.send(am(account::Message::Profile(account::profile::Msg::Revert)));
    d.send(am(account::Message::Tab(account::Tab::Preferences)));
    both(d, out, "28-account-preferences");
    d.send(am(account::Message::Tab(account::Tab::Security)));
    both(d, out, "29-account-security");
    d.send(am(account::Message::Security(
        account::security::Msg::EnableTotp,
    )));
    d.pump_until("totp secret", Duration::from_secs(10), |a| {
        acct(a).is_some_and(|x| x.security.pending.is_some())
    })
    .await;
    both(d, out, "30-account-totp-qr");
    d.send(am(account::Message::Security(
        account::security::Msg::CancelSetup,
    )));
    d.send(am(account::Message::Tab(account::Tab::Tokens)));
    d.send(am(account::Message::Tokens(account::tokens::Msg::Name(
        "ci-bot".into(),
    ))));
    d.send(am(account::Message::Tokens(account::tokens::Msg::Generate)));
    d.pump_until("token", Duration::from_secs(10), |a| {
        acct(a).is_some_and(|x| x.tokens.shown.is_some() && !x.tokens.loading)
    })
    .await;
    both(d, out, "31-account-token-shown");
    d.send(am(account::Message::Tokens(
        account::tokens::Msg::CloseShown,
    )));
    d.send(am(account::Message::Tokens(
        account::tokens::Msg::AskRegenerate("ci-bot".into()),
    )));
    both(d, out, "32-account-tokens");
    d.send(am(account::Message::Tokens(
        account::tokens::Msg::CancelRemove,
    )));

    // A password change: without 2FA the current password is asked inline and
    // Save waits for it (one `updateOwnBasicInfo` call; nothing is sent here).
    d.send(am(account::Message::Tab(account::Tab::Profile)));
    for m in [
        account::profile::Msg::NewPassword("Snapshot-pass-2468!".into()),
        account::profile::Msg::ConfirmPassword("Snapshot-pass-2468!".into()),
        account::profile::Msg::Save,
    ] {
        d.send(am(account::Message::Profile(m)));
    }
    d.settle(Duration::from_millis(500)).await;
    both(d, out, "33-account-password-change");
    d.send(am(account::Message::Profile(account::profile::Msg::Revert)));
    d.send(am(account::Message::Close));
    d.app.config.ui.theme = Theme::Light;
}
