//! The server keys of the enterprise policy in the app (docs/policy.md
//! sections 9.1 and 9.2): reconcile, suspension at start and at refresh,
//! adding, removing, links, the empty state and certificate exceptions.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use rocketchat::{Credentials, ServerUrl};
use rustket::app::{App, Flags, Modal, Screen, SecretsMode};
use rustket::component::Nav;
use rustket::message::{Go, Message, ModalMessage, SecretsReady};
use rustket::screens::add_server;
use rustket::screens::policy_notice;
use rustket::workspace::{rail, status_menu};
use rustket_core::ServerId;
use rustket_core::config::{AppConfig, Language, PinnedCertificate, ServerEntry};
use rustket_core::policy::{Policy, RawValue, Snapshot, SourceId, Trust, resolve};
use rustket_core::secrets::{MemoryStore, SecretBackend, SecretStore, SwitchableStore, secret_key};

fn policy(values: &[(&str, RawValue)]) -> Arc<Policy> {
    let mut snap = Snapshot::new(SourceId::Test("admin".into()), Trust::Full);
    for (name, value) in values {
        snap = snap.with(name, value.clone());
    }
    Arc::new(resolve(&[snap], &rustket::policy::context()))
}

fn list(items: &[&str]) -> RawValue {
    RawValue::List(
        items
            .iter()
            .map(|s| RawValue::String((*s).into()))
            .collect(),
    )
}

fn allowed(patterns: &[&str]) -> Arc<Policy> {
    policy(&[("AllowedServers", list(patterns))])
}

fn no_user_servers() -> Arc<Policy> {
    policy(&[("AllowUserServers", RawValue::Bool(false))])
}

/// An empty config in English: `App::new` then selects English itself and
/// never switches the process locale to the system's language (the
/// assertions read English texts).
fn english_config() -> AppConfig {
    let mut cfg = AppConfig::default();
    cfg.ui.language = Language::new("en").unwrap();
    cfg
}

/// A config past the terms screen with `servers` (url, user id), saved to
/// `dir`; returns their ids.
fn write_config(dir: &Path, servers: &[(&str, Option<&str>)]) -> Vec<ServerId> {
    let mut cfg = english_config();
    rustket::legal::accept_terms(&mut cfg);
    let ids = servers
        .iter()
        .map(|(url, uid)| {
            let mut e = ServerEntry::new(*url, "");
            e.user_id = uid.map(str::to_owned);
            cfg.add_server(e)
        })
        .collect();
    cfg.save(dir).unwrap();
    ids
}

fn app(dir: &Path, policy: Arc<Policy>) -> App {
    let (app, _) = App::new(Flags {
        config_dir: Some(dir.to_owned()),
        cache_dir: Some(dir.join("cache")),
        secrets: SecretsMode::Memory,
        policy,
        ..Flags::default()
    });
    app
}

/// [`app`] with the offline cache on (in `dir/offline`).
fn app_with_cache(dir: &Path, policy: Arc<Policy>) -> App {
    let (app, _) = App::new(Flags {
        config_dir: Some(dir.to_owned()),
        cache_dir: Some(dir.join("cache")),
        offline_cache: Some(dir.join("offline")),
        secrets: SecretsMode::Memory,
        policy,
        ..Flags::default()
    });
    app
}

/// Opens the secret store with `store`, handing over the tokens the
/// start-up would have read (`App::startup_token_reads`). Returns the
/// number of tasks the start-up issued.
fn boot(app: &mut App, store: &Arc<MemoryStore>) -> usize {
    let tokens = app
        .startup_token_reads()
        .into_iter()
        .map(|(sid, uid)| {
            let t = store.get(&secret_key(&sid, &uid)).unwrap();
            (sid, t)
        })
        .collect();
    app.update(Message::SecretsReady(SecretsReady {
        store: Arc::new(SwitchableStore::new(
            Box::new(store.clone()),
            SecretBackend::Keyring,
            None,
        )),
        tokens,
        notices: vec![],
    }))
    .units()
}

/// Runs `task` and returns the messages it produced, taken one by one: it
/// stops once no output comes within a short deadline (a long timer, such
/// as a toast's, is still running), keeping what arrived before.
async fn run_out(task: iced::Task<Message>) -> Vec<Message> {
    use iced::futures::StreamExt;
    let Some(mut s) = iced_runtime::task::into_stream(task) else {
        return Vec::new();
    };
    let deadline = std::time::Duration::from_millis(500);
    let mut out = Vec::new();
    while let Ok(Some(a)) = tokio::time::timeout(deadline, s.next()).await {
        if let iced_runtime::Action::Output(m) = a {
            out.push(m);
        }
    }
    out
}

/// The first message `task` produces (bounded wait).
async fn first_out(task: iced::Task<Message>) -> Option<Message> {
    use iced::futures::StreamExt;
    let mut s = iced_runtime::task::into_stream(task)?;
    let deadline = std::time::Duration::from_secs(1);
    while let Ok(Some(a)) = tokio::time::timeout(deadline, s.next()).await {
        if let iced_runtime::Action::Output(m) = a {
            return Some(m);
        }
    }
    None
}

/// A store holding token `T-<uid>` for each (server, user).
fn store_with(accounts: &[(&ServerId, &str)]) -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::new());
    for (sid, uid) in accounts {
        store
            .set(&secret_key(sid, uid), &format!("T-{uid}"))
            .unwrap();
    }
    store
}

fn signed_in(app: &App, sid: &ServerId) -> bool {
    app.servers[sid].credentials.is_some()
}

/// No session, no login and no token read (`restoring` is set while one
/// runs).
fn stopped(app: &App, sid: &ServerId) -> bool {
    let s = &app.servers[sid];
    s.credentials.is_none()
        && s.session.is_none()
        && s.spec().is_none()
        && s.login.is_none()
        && !s.restoring
}

fn on_disk(dir: &Path) -> AppConfig {
    AppConfig::load(dir).unwrap()
}

// --- reconcile -----------------------------------------------------------------------

/// A missing managed server is added once (saved; a second start adds
/// nothing); its name comes from the policy, else the host.
#[test]
fn managed_servers_are_added_once() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &[]);
    let p = policy(&[(
        "ManagedServers",
        list(&["https://chat.corp.example Corp chat", "https://b.example"]),
    )]);
    let a = app(dir.path(), p.clone());
    let names: Vec<&str> = a
        .config
        .sorted_servers()
        .iter()
        .map(|e| e.display_name.as_str())
        .collect();
    assert_eq!(names, ["Corp chat", "b.example"]);
    assert_eq!(a.servers.len(), 2);
    a.flush_config();
    drop(a);
    assert_eq!(on_disk(dir.path()).servers.len(), 2, "saved");
    let a = app(dir.path(), p);
    assert_eq!(a.config.servers.len(), 2, "a second start adds nothing");
}

/// An entry that differs only by case, default port or trailing slash is
/// the managed server, used as it is.
#[test]
fn existing_entry_is_recognised() {
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(dir.path(), &[("HTTPS://Chat.Corp.Example:443/", None)]);
    let a = app(
        dir.path(),
        policy(&[("ManagedServers", list(&["https://chat.corp.example Corp"]))]),
    );
    assert_eq!(a.config.servers.len(), 1);
    assert_eq!(a.config.servers[0].id, ids[0]);
    assert!(!a.may_remove_server(&ids[0]), "it is managed");
}

/// An unnamed managed server takes the server's site name once its public
/// settings arrive; a named one and an unmanaged one keep theirs.
#[tokio::test]
async fn unnamed_managed_server_takes_the_site_name() {
    use rustket::screens::login::{self, LoginMeta};
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(dir.path(), &[("https://own.example", None)]);
    let mut a = app(
        dir.path(),
        policy(&[(
            "ManagedServers",
            list(&["https://a.example", "https://b.example Bee"]),
        )]),
    );
    boot(&mut a, &Arc::new(MemoryStore::new()));
    let by_url = |a: &App, url: &str| {
        a.config
            .servers
            .iter()
            .find(|e| e.url == url)
            .unwrap()
            .id
            .clone()
    };
    let unnamed = by_url(&a, "https://a.example");
    let named = by_url(&a, "https://b.example");
    for (sid, site) in [(&unnamed, "Alpha"), (&named, "Other"), (&ids[0], "Own")] {
        let l = a.servers.get_mut(sid).unwrap().login.as_mut().unwrap();
        l.meta = Some(LoginMeta {
            site_name: site.into(),
            ..Default::default()
        });
        let _ = a.update(Message::Login(
            sid.clone(),
            login::Message::User("x".into()),
        ));
    }
    assert_eq!(a.servers[&unnamed].name, "Alpha");
    assert_eq!(a.config.server(&unnamed).unwrap().display_name, "Alpha");
    assert_eq!(a.servers[&named].name, "Bee");
    assert_eq!(a.servers[&ids[0]].name, "own.example");
    a.flush_config();
    let disk = on_disk(dir.path());
    assert!(disk.servers.iter().any(|e| e.display_name == "Alpha"));
}

/// A signed-in managed server still named by its host takes the site name
/// its session reports (public settings); one named by the policy item, an
/// unmanaged entry and a later site name do not rename.
#[tokio::test]
async fn signed_in_managed_server_takes_the_site_name() {
    use rocketchat::models::Setting;
    use rocketchat::session::{ChangeKind, SessionEvent};
    use rustket::session::ServerEvent;
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(
        dir.path(),
        &[
            ("https://a.example", Some("U1")),
            ("https://b.example", Some("U1")),
            ("https://own.example", Some("U1")),
        ],
    );
    let mut a = app(
        dir.path(),
        policy(&[(
            "ManagedServers",
            list(&["https://a.example", "https://b.example"]),
        )]),
    );
    // Written with an empty name: the entries take their hosts.
    for (sid, name) in ids.iter().zip(["a.example", "Bee", "own.example"]) {
        let e = a.config.servers.iter_mut().find(|e| e.id == *sid).unwrap();
        e.display_name = name.into();
        a.servers.get_mut(sid).unwrap().name = name.into();
    }
    let store = store_with(&[(&ids[0], "U1"), (&ids[1], "U1"), (&ids[2], "U1")]);
    boot(&mut a, &store);
    let site = |a: &mut App, sid: &ServerId, name: &str| {
        let generation = a.servers[sid].generation;
        let ev = SessionEvent::SettingChanged {
            kind: ChangeKind::Updated,
            setting: Box::new(Setting {
                id: "Site_Name".into(),
                value: name.into(),
                ..Default::default()
            }),
            public: true,
        };
        let _ = a.update(Message::Server(
            sid.clone(),
            generation,
            ServerEvent::Event(Box::new(ev)),
        ));
    };
    for sid in &ids {
        assert!(signed_in(&a, sid));
        site(&mut a, sid, "Site");
    }
    assert_eq!(
        a.servers[&ids[0]].workspace.settings().str("Site_Name"),
        Some("Site")
    );
    assert_eq!(a.servers[&ids[0]].name, "Site");
    assert_eq!(a.config.server(&ids[0]).unwrap().display_name, "Site");
    assert_eq!(a.servers[&ids[1]].name, "Bee");
    assert_eq!(a.servers[&ids[2]].name, "own.example");
    // Named once: a later site name does not rename it.
    site(&mut a, &ids[0], "Later");
    assert_eq!(a.servers[&ids[0]].name, "Site");
    a.flush_config();
    let disk = on_disk(dir.path());
    assert_eq!(disk.server(&ids[0]).unwrap().display_name, "Site");
    assert_eq!(disk.server(&ids[2]).unwrap().display_name, "own.example");
}

/// A server that leaves the policy stays as an ordinary, removable server.
#[test]
fn server_that_left_the_policy_stays_and_is_removable() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &[]);
    let mut a = app(
        dir.path(),
        policy(&[("ManagedServers", list(&["https://m.example"]))]),
    );
    let sid = a.config.servers[0].id.clone();
    assert!(!a.may_remove_server(&sid));
    let _ = a.update(Message::Noop);
    assert!(a.servers[&sid].status_menu.remove_refused);
    let _ = a.set_policy(Arc::default());
    assert!(a.servers.contains_key(&sid));
    assert!(a.may_remove_server(&sid));
    let _ = a.update(Message::Noop);
    assert!(!a.servers[&sid].status_menu.remove_refused);
}

// --- start ---------------------------------------------------------------------------

/// A config.toml entry the policy does not allow is suspended: no token
/// read, no session, no login, no cache; listed with the mark.
#[tokio::test]
async fn not_allowed_entry_is_not_started() {
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(
        dir.path(),
        &[
            ("https://ok.corp.example", Some("U1")),
            ("https://evil.example", Some("U2")),
        ],
    );
    let (ok, evil) = (&ids[0], &ids[1]);
    let mut a = app_with_cache(dir.path(), allowed(&["*.corp.example"]));
    assert!(a.is_suspended(evil));
    assert!(!a.is_suspended(ok));
    assert_eq!(
        a.startup_token_reads(),
        [(ok.clone(), "U1".to_owned())],
        "a secret read for a suspended server"
    );
    let store = store_with(&[(ok, "U1"), (evil, "U2")]);
    let units = boot(&mut a, &store);
    assert!(signed_in(&a, ok));
    assert!(stopped(&a, evil));
    let _ = a.update(Message::Noop);
    assert!(stopped(&a, evil));
    // The allowed account's cache opens; the suspended one's does not.
    assert!(a.servers[ok].cache.is_opening());
    let c = &a.servers[evil].cache;
    assert!(!c.is_open() && !c.is_opening());
    // The suspended entry adds no task to the start-up (no token read).
    let dir2 = tempfile::tempdir().unwrap();
    let only = write_config(dir2.path(), &[("https://ok.corp.example", Some("U1"))]);
    let mut b = app_with_cache(dir2.path(), allowed(&["*.corp.example"]));
    assert_eq!(boot(&mut b, &store_with(&[(&only[0], "U1")])), units);
    // Even a token handed over for it starts nothing.
    let _ = a.update(Message::SecretsReady(SecretsReady {
        store: Arc::new(SwitchableStore::new(
            Box::new(store.clone()),
            SecretBackend::Keyring,
            None,
        )),
        tokens: vec![(evil.clone(), Some("T-U2".into()))],
        notices: vec![],
    }));
    assert!(stopped(&a, evil));
    // A retry connects nothing either.
    let _ = a.update(Message::RetryConnection(evil.clone()));
    assert!(stopped(&a, evil));
    let entries = a.rail_entries();
    let mark = |sid: &ServerId| entries.iter().find(|e| e.id == *sid).unwrap().suspended;
    assert!(mark(evil));
    assert!(!mark(ok));
}

/// `RequireHttps` suspends an http server, managed or not.
#[test]
fn require_https_suspends_http_servers() {
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(
        dir.path(),
        &[
            ("http://plain.example", None),
            ("https://tls.example", None),
        ],
    );
    let a = app(
        dir.path(),
        policy(&[
            ("RequireHttps", RawValue::Bool(true)),
            ("ManagedServers", list(&["http://managed.example"])),
        ]),
    );
    let managed = a
        .config
        .servers
        .iter()
        .find(|e| e.url == "http://managed.example")
        .unwrap()
        .id
        .clone();
    assert!(a.is_suspended(&ids[0]));
    assert!(a.is_suspended(&managed), "managed or not");
    assert!(!a.is_suspended(&ids[1]));
}

/// At start a suspended active server gives way to an allowed one; when all
/// are suspended the stored one is shown with its suspended screen.
#[tokio::test]
async fn active_server_at_start() {
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(
        dir.path(),
        &[
            ("https://ok.corp.example", None),
            ("https://evil.example", None),
        ],
    );
    let mut cfg = on_disk(dir.path());
    cfg.active_server = Some(ids[1].clone());
    cfg.save(dir.path()).unwrap();
    let a = app(dir.path(), allowed(&["*.corp.example"]));
    assert_eq!(a.active.as_ref(), Some(&ids[0]));

    let mut a = app(dir.path(), allowed(&["nothing.example"]));
    assert_eq!(a.active.as_ref(), Some(&ids[1]), "the stored one");
    boot(&mut a, &Arc::new(MemoryStore::new()));
    assert!(matches!(a.screen, Screen::Server));
    assert!(a.is_suspended(&ids[1]));
    let _ = a.view();
}

// --- refresh -------------------------------------------------------------------------

/// Allowed → suspended stops the running server without deleting
/// anything; suspended → allowed starts it as at start-up.
#[tokio::test]
async fn refresh_suspends_and_resumes() {
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(dir.path(), &[("https://chat.corp.example", Some("U1"))]);
    let sid = &ids[0];
    let mut a = app(dir.path(), Arc::default());
    let store = store_with(&[(sid, "U1")]);
    boot(&mut a, &store);
    assert!(signed_in(&a, sid));
    a.servers
        .get_mut(sid)
        .unwrap()
        .workspace
        .set_draft("r1", None, "unsent words");

    let _ = a.set_policy(allowed(&["other.example"]));
    assert!(a.is_suspended(sid));
    assert!(stopped(&a, sid));
    assert_eq!(
        a.servers[sid].workspace.draft("r1", None),
        Some("unsent words"),
        "the draft is kept"
    );
    assert_eq!(
        a.config.server(sid).unwrap().user_id.as_deref(),
        Some("U1"),
        "the account is kept"
    );
    assert!(
        a.config.pending_secret_deletions.is_empty(),
        "nothing deleted"
    );
    assert_eq!(
        store.get(&secret_key(sid, "U1")).unwrap().as_deref(),
        Some("T-U1")
    );
    a.flush_config();
    assert!(on_disk(dir.path()).server(sid).is_some());

    // Allowed again: the token is read and the session starts.
    let task = a.set_policy(Arc::default());
    assert!(task.units() > 0, "no token read");
    assert!(a.servers[sid].restoring);
    assert!(!signed_in(&a, sid));
    let _ = a.update(Message::ServerTokenRead(
        sid.clone(),
        "U1".into(),
        Some(Credentials::new("U1", "T-U1")),
    ));
    assert!(signed_in(&a, sid));
    assert!(a.config.pending_secret_deletions.is_empty());
}

/// The suspension closes the offline cache without deleting it (its outbox
/// included) and keeps the image cache; the resumed account opens it again.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_keeps_the_offline_cache() {
    use rocketchat::models::{Room, RoomType, Subscription};
    use rustket::cache::Msg;
    use rustket_core::cache::{CacheHandle, server_dir};
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(dir.path(), &[("https://chat.corp.example", Some("U1"))]);
    let sid = &ids[0];
    let mut a = app_with_cache(dir.path(), Arc::default());
    boot(&mut a, &store_with(&[(sid, "U1")]));
    let _ = a.update(Message::Noop);
    assert!(a.servers[sid].cache.is_opening());
    let offline = server_dir(&dir.path().join("offline"), sid);
    let open = |offline: std::path::PathBuf| async move {
        CacheHandle::open(offline, "U1".into(), None)
            .await
            .map_err(|e| e.to_string())
    };
    let opened = open(offline.clone()).await;
    let _ = a.update(Message::Cache(Msg::Opened(
        sid.clone(),
        "U1".into(),
        Box::new(opened),
    )));
    assert!(a.servers[sid].cache.is_open());
    let epoch = a.servers[sid].generation;
    let _ = a.update(Message::InitialLoaded(
        sid.clone(),
        epoch,
        Ok(Box::new(rustket_core::sync::InitialData {
            me: rocketchat::models::User {
                id: "U1".into(),
                ..Default::default()
            },
            subscriptions: vec![Subscription {
                id: "s-R1".into(),
                rid: "R1".into(),
                name: "R1".into(),
                t: RoomType::Channel,
                open: true,
                ..Default::default()
            }],
            rooms: vec![Room {
                id: "R1".into(),
                t: RoomType::Channel,
                name: Some("R1".into()),
                ..Default::default()
            }],
            ..Default::default()
        })),
    ));
    {
        let ws = &mut a.servers.get_mut(sid).unwrap().workspace;
        let pending = ws.new_local_message("R1", "written offline", None, false);
        ws.add_pending(pending);
    }
    let _ = a.update(Message::Noop);
    let images = a.images.server_dir(sid).unwrap();
    std::fs::create_dir_all(&images).unwrap();
    std::fs::write(images.join("kept"), b"x").unwrap();

    let task = a.set_policy(allowed(&["other.example"]));
    assert!(stopped(&a, sid));
    assert!(!a.servers[sid].cache.is_open(), "the cache is closed");
    let _ = run_out(task).await;
    assert!(offline.exists(), "the offline cache is kept");
    assert!(images.join("kept").exists(), "the image cache is kept");
    // The outbox is still on disk (the closed worker finishes its writes).
    let mut outbox = Vec::new();
    for _ in 0..50 {
        if let Ok(o) = open(offline.clone()).await {
            outbox = o.outbox;
            if !outbox.is_empty() {
                break;
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    assert_eq!(outbox.len(), 1, "the outbox is kept");
    assert_eq!(outbox[0].message.msg, "written offline");

    // Allowed again: the account starts and its cache opens with the outbox.
    let _ = a.set_policy(Arc::default());
    let _ = a.update(Message::ServerTokenRead(
        sid.clone(),
        "U1".into(),
        Some(Credentials::new("U1", "T-U1")),
    ));
    let _ = a.update(Message::Noop);
    assert!(a.servers[sid].cache.is_opening());
    let opened = open(offline.clone()).await;
    let _ = a.update(Message::Cache(Msg::Opened(
        sid.clone(),
        "U1".into(),
        Box::new(opened),
    )));
    let s = &a.servers[sid];
    assert!(s.cache.is_open());
    let pending: Vec<_> = s.workspace.outbox_items().collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].message.msg, "written offline");
}

/// A server suspended while the user may remove servers offers "Remove
/// workspace" on its screen; a managed (or protected) one does not.
#[tokio::test]
async fn suspended_server_can_be_removed_when_allowed() {
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(
        dir.path(),
        &[
            ("https://chat.corp.example", None),
            ("http://evil.example", None),
        ],
    );
    let evil = &ids[1];
    for p in [
        policy(&[
            ("RequireHttps", RawValue::Bool(true)),
            ("ManagedServers", list(&["http://evil.example"])),
        ]),
        policy(&[
            ("RequireHttps", RawValue::Bool(true)),
            ("AllowUserServers", RawValue::Bool(false)),
        ]),
    ] {
        let mut a = app(dir.path(), p);
        boot(&mut a, &Arc::new(MemoryStore::new()));
        assert!(a.is_suspended(evil));
        let _ = a.update(Message::PolicyNotice(policy_notice::Message::Remove(
            evil.clone(),
        )));
        assert!(a.modals.is_empty(), "the confirmation opened");
    }
    let mut a = app(
        dir.path(),
        policy(&[("RequireHttps", RawValue::Bool(true))]),
    );
    boot(&mut a, &Arc::new(MemoryStore::new()));
    assert!(a.is_suspended(evil));
    let _ = a.update(Message::PolicyNotice(policy_notice::Message::Remove(
        evil.clone(),
    )));
    assert!(matches!(a.modals.last(), Some(Modal::ConfirmRemove(s)) if s == evil));
    let task = a.update(Message::Modal(ModalMessage::ConfirmRemove(evil.clone())));
    assert!(!a.servers.contains_key(evil));
    assert!(a.config.server(evil).is_none());
    drop(task);
}

/// Managed servers brought by a refresh while the terms wait (no server
/// before): accepting the terms shows one of them, not an empty spinner.
#[tokio::test]
async fn managed_servers_added_before_the_terms_are_shown() {
    use rustket::screens::terms::Message as T;
    let dir = tempfile::tempdir().unwrap();
    english_config().save(dir.path()).unwrap();
    let mut a = app(dir.path(), Arc::default());
    boot(&mut a, &Arc::new(MemoryStore::new()));
    assert!(a.active.is_none());
    assert!(!matches!(a.screen, Screen::Server), "the terms wait");
    let _ = a.set_policy(policy(&[(
        "ManagedServers",
        list(&["https://m.example M"]),
    )]));
    assert_eq!(a.servers.len(), 1);
    let _ = a.update(Message::Terms(T::Accept));
    let sid = a.config.servers[0].id.clone();
    assert_eq!(a.active.as_ref(), Some(&sid));
    assert!(matches!(a.screen, Screen::Server));
    assert!(a.servers[&sid].login.is_some(), "the login screen shows");
    let _ = a.view();
}

/// A token read that arrives after the server was suspended again starts
/// nothing; one for an account that signed out shows the login.
#[tokio::test]
async fn late_token_reads_are_checked() {
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(dir.path(), &[("https://chat.corp.example", Some("U1"))]);
    let sid = &ids[0];
    let mut a = app(dir.path(), allowed(&["other.example"]));
    boot(&mut a, &store_with(&[(sid, "U1")]));
    assert!(stopped(&a, sid));
    let _ = a.set_policy(Arc::default());
    let _ = a.set_policy(allowed(&["other.example"]));
    let _ = a.update(Message::ServerTokenRead(
        sid.clone(),
        "U1".into(),
        Some(Credentials::new("U1", "T-U1")),
    ));
    assert!(stopped(&a, sid));

    let _ = a.set_policy(Arc::default());
    a.config.mark_secret_deletion(&secret_key(sid, "U1"));
    let _ = a.update(Message::ServerTokenRead(
        sid.clone(),
        "U1".into(),
        Some(Credentials::new("U1", "T-U1")),
    ));
    assert!(!signed_in(&a, sid));
    assert!(a.servers[sid].login.is_some());
}

/// A managed server added by a refresh joins the list (saved).
#[tokio::test]
async fn refresh_adds_managed_servers() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &[]);
    let mut a = app(dir.path(), Arc::default());
    boot(&mut a, &Arc::new(MemoryStore::new()));
    assert!(matches!(a.screen, Screen::AddServer(_)));
    let _ = a.set_policy(policy(&[
        ("ManagedServers", list(&["https://m.example M"])),
        ("AllowUserServers", RawValue::Bool(false)),
    ]));
    assert_eq!(a.servers.len(), 1);
    assert!(matches!(a.screen, Screen::Server));
    let sid = a.config.servers[0].id.clone();
    assert!(a.servers[&sid].login.is_some(), "started as at start-up");
    a.flush_config();
    assert_eq!(on_disk(dir.path()).servers.len(), 1);
}

// --- adding --------------------------------------------------------------------------

fn add_screen(a: &App) -> Option<&add_server::AddServer> {
    match &a.screen {
        Screen::AddServer(s) => Some(s),
        _ => None,
    }
}

/// With `AllowUserServers = false` no message opens the add-server screen.
#[tokio::test]
async fn add_screen_cannot_be_opened_when_adding_is_off() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &[("https://chat.corp.example", None)]);
    let mut a = app(dir.path(), no_user_servers());
    boot(&mut a, &Arc::new(MemoryStore::new()));
    let url = ServerUrl::parse("https://new.example").unwrap();
    for m in [
        Message::ShowAddServer,
        Message::Rail(rail::Message::Add),
        Message::Go(Go::AddServer(url.clone())),
    ] {
        let task = a.update(m);
        // A message the handler hands on (the rail's Add) is delivered too.
        if let Some(next) = first_out(task).await {
            let _ = a.update(next);
        }
        assert!(add_screen(&a).is_none());
    }
    a.modals.push(Modal::OfferAddServer { url });
    let _ = a.update(Message::Modal(ModalMessage::AddOfferedServer));
    assert!(add_screen(&a).is_none());
    assert!(!a.toasts.is_empty(), "the refusal is shown");
}

/// A not-allowed address is refused inline before any request; an allowed
/// one starts the check.
#[tokio::test]
async fn submit_checks_the_policy_first() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &[("https://chat.corp.example", None)]);
    let mut a = app(
        dir.path(),
        policy(&[
            ("AllowedServers", list(&["*.corp.example"])),
            ("RequireHttps", RawValue::Bool(true)),
        ]),
    );
    boot(&mut a, &Arc::new(MemoryStore::new()));
    let _ = a.update(Message::ShowAddServer);
    for (typed, needle) in [
        ("https://Evil.Example/", "does not allow this server"),
        ("http://new.corp.example", "requires https://"),
    ] {
        let _ = a.update(Message::AddServer(add_server::Message::UrlChanged(
            typed.into(),
        )));
        let task = a.update(Message::AddServer(add_server::Message::Submit));
        assert_eq!(task.units(), 0, "a request for {typed}");
        let s = add_screen(&a).unwrap();
        assert!(!s.busy);
        let error = s.error.clone().unwrap();
        assert!(error.contains(needle), "{error}");
    }
    let _ = a.update(Message::AddServer(add_server::Message::UrlChanged(
        "https://new.corp.example".into(),
    )));
    let task = a.update(Message::AddServer(add_server::Message::Submit));
    assert_eq!(task.units(), 1, "the check of an allowed address");
}

/// The policy is asked again right before the server is added.
#[tokio::test]
async fn policy_is_checked_again_before_adding() {
    use rustket::screens::login::{LoginMeta, ServerProbe};
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &[("https://chat.corp.example", None)]);
    let mut a = app(dir.path(), Arc::default());
    boot(&mut a, &Arc::new(MemoryStore::new()));
    let _ = a.update(Message::ShowAddServer);
    let _ = a.update(Message::AddServer(add_server::Message::UrlChanged(
        "https://new.example".into(),
    )));
    let _ = a.update(Message::AddServer(add_server::Message::Submit));
    assert!(add_screen(&a).unwrap().busy);
    let _ = a.set_policy(allowed(&["*.corp.example"]));
    let url = ServerUrl::parse("https://new.example").unwrap();
    let probe = ServerProbe {
        url: url.clone(),
        info: Default::default(),
        meta: LoginMeta::default(),
    };
    let _ = a.update(Message::AddServer(add_server::Message::Checked(
        2,
        url,
        Ok(Box::new(probe)),
    )));
    assert_eq!(a.servers.len(), 1, "added against the policy");
    let error = add_screen(&a).unwrap().error.clone().unwrap();
    assert!(error.contains("does not allow"), "{error}");
}

// --- links ---------------------------------------------------------------------------

const FOREIGN_LINK: &str = "rocketchat://room?host=https://new.example&rid=R";

/// A link to a server that is not configured is refused with a toast and
/// no dialog when adding is off or the server is not allowed.
#[tokio::test]
async fn refused_links_show_a_toast_and_no_dialog() {
    for p in [no_user_servers(), allowed(&["*.corp.example"])] {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), &[("https://chat.corp.example", None)]);
        let mut a = app(dir.path(), p);
        boot(&mut a, &Arc::new(MemoryStore::new()));
        let task = a.update(Message::DeepLinks(vec![FOREIGN_LINK.into()]));
        assert!(a.modals.is_empty(), "an offer dialog");
        assert_eq!(a.toasts.len(), 1);
        assert_eq!(a.toasts[0].title, "Link not opened");
        drop(task);
    }
    // Without a policy the offer is made, as before.
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &[("https://chat.corp.example", None)]);
    let mut a = app(dir.path(), Arc::default());
    boot(&mut a, &Arc::new(MemoryStore::new()));
    let _ = a.update(Message::DeepLinks(vec![FOREIGN_LINK.into()]));
    assert!(matches!(
        a.modals.last(),
        Some(Modal::OfferAddServer { .. })
    ));
}

/// A link to a suspended server opens its suspended screen and connects to
/// nothing.
#[tokio::test]
async fn link_to_a_suspended_server() {
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(
        dir.path(),
        &[
            ("https://chat.corp.example", None),
            ("https://evil.example", None),
        ],
    );
    let mut a = app(dir.path(), allowed(&["*.corp.example"]));
    boot(&mut a, &Arc::new(MemoryStore::new()));
    assert_eq!(a.active.as_ref(), Some(&ids[0]));
    let _ = a.update(Message::DeepLinks(vec![
        "rocketchat://room?host=https://evil.example&rid=R".into(),
    ]));
    assert_eq!(a.active.as_ref(), Some(&ids[1]));
    assert!(matches!(a.screen, Screen::Server));
    assert!(stopped(&a, &ids[1]));
    assert!(a.servers[&ids[1]].pending_room.is_none());
}

/// An offer dialog open when the policy changes adds nothing the new
/// policy refuses.
#[tokio::test]
async fn open_offer_is_checked_on_confirm() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &[("https://chat.corp.example", None)]);
    let mut a = app(dir.path(), Arc::default());
    boot(&mut a, &Arc::new(MemoryStore::new()));
    let _ = a.update(Message::DeepLinks(vec![FOREIGN_LINK.into()]));
    assert!(matches!(
        a.modals.last(),
        Some(Modal::OfferAddServer { .. })
    ));
    let _ = a.set_policy(allowed(&["*.corp.example"]));
    let task = a.update(Message::Modal(ModalMessage::AddOfferedServer));
    assert!(add_screen(&a).is_none());
    assert!(a.modals.is_empty());
    assert_eq!(a.servers.len(), 1);
    drop(task);
}

// --- removing ------------------------------------------------------------------------

/// Removal is refused through every entry for a managed server, and for
/// any server while adding and removing is off.
#[tokio::test]
async fn removal_is_refused_through_every_entry() {
    for managed in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let ids = write_config(dir.path(), &[("https://chat.corp.example", None)]);
        let sid = &ids[0];
        let p = if managed {
            policy(&[("ManagedServers", list(&["https://chat.corp.example"]))])
        } else {
            no_user_servers()
        };
        let mut a = app(dir.path(), p);
        boot(&mut a, &Arc::new(MemoryStore::new()));
        assert!(!a.may_remove_server(sid));
        let _ = a.update(Message::StatusMenu(
            sid.clone(),
            status_menu::Message::RemoveServer,
        ));
        assert!(a.modals.is_empty(), "the confirmation opened");
        assert!(a.servers[sid].status_menu.remove_refused);
        let _ = a.update(Message::Modal(ModalMessage::ConfirmRemove(sid.clone())));
        assert!(a.servers.contains_key(sid));
        a.modals.push(Modal::ConfirmRemove(sid.clone()));
        let _ = a.update(Message::Modal(ModalMessage::ConfirmRemove(sid.clone())));
        assert!(a.servers.contains_key(sid), "removed (managed: {managed})");
        assert!(a.config.server(sid).is_some());
    }
}

/// A confirmation open when the policy starts protecting the server goes.
#[tokio::test]
async fn open_confirmation_closes_when_the_server_becomes_managed() {
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(dir.path(), &[("https://chat.corp.example", None)]);
    let mut a = app(dir.path(), Arc::default());
    boot(&mut a, &Arc::new(MemoryStore::new()));
    let _ = a.update(Message::StatusMenu(
        ids[0].clone(),
        status_menu::Message::RemoveServer,
    ));
    assert!(matches!(a.modals.last(), Some(Modal::ConfirmRemove(_))));
    let _ = a.set_policy(policy(&[(
        "ManagedServers",
        list(&["https://chat.corp.example"]),
    )]));
    assert!(a.modals.is_empty());
}

// --- empty state ---------------------------------------------------------------------

/// Without servers the fallbacks show the policy's plain screen while
/// adding is off, the add-server screen otherwise.
#[tokio::test]
async fn empty_state_fallbacks() {
    for (p, managed_empty) in [(no_user_servers(), true), (Arc::default(), false)] {
        let dir = tempfile::tempdir().unwrap();
        write_config(dir.path(), &[]);
        let mut a = app(dir.path(), p);
        let expect = |a: &App| {
            if managed_empty {
                matches!(a.screen, Screen::NoServers)
            } else {
                matches!(a.screen, Screen::AddServer(_))
            }
        };
        boot(&mut a, &Arc::new(MemoryStore::new()));
        assert!(expect(&a), "start-up");
        let _ = a.view();
        let _ = a.update(Message::Go(Go::Nav(Nav::Settings)));
        assert!(matches!(a.screen, Screen::Settings(_)));
        let _ = a.update(Message::Go(Go::Nav(Nav::Workspace)));
        assert!(expect(&a), "back from the settings");
    }
}

/// The add-server screen's Cancel and a removal of the last server fall
/// back to the same screens. Once adding is off the add screen cannot be
/// open, so there is no Cancel to try then.
#[tokio::test]
async fn cancel_and_removal_fall_back() {
    let dir = tempfile::tempdir().unwrap();
    let ids = write_config(dir.path(), &[("https://chat.corp.example", None)]);
    let mut a = app(dir.path(), Arc::default());
    boot(&mut a, &Arc::new(MemoryStore::new()));
    a.modals.push(Modal::ConfirmRemove(ids[0].clone()));
    let _ = a.update(Message::Modal(ModalMessage::ConfirmRemove(ids[0].clone())));
    assert!(a.servers.is_empty());
    assert!(matches!(a.screen, Screen::AddServer(_)));
    let _ = a.update(Message::AddServer(add_server::Message::Cancel));
    assert!(matches!(a.screen, Screen::AddServer(_)));
    // Adding turned off with the screen open: the plain screen replaces it.
    let _ = a.set_policy(no_user_servers());
    assert!(matches!(a.screen, Screen::NoServers));
}

/// "Details" opens Settings → Policies.
#[tokio::test]
async fn details_open_the_policies_tab() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &[]);
    let mut a = app(dir.path(), no_user_servers());
    boot(&mut a, &Arc::new(MemoryStore::new()));
    let _ = a.update(Message::PolicyNotice(policy_notice::Message::Details));
    match &a.screen {
        Screen::Settings(s) => {
            assert_eq!(s.tab, rustket::screens::settings::Tab::Policies)
        }
        _ => panic!("settings not open"),
    }
}

// --- certificates --------------------------------------------------------------------

fn pinned_config(dir: &Path) -> ServerId {
    let mut cfg = english_config();
    rustket::legal::accept_terms(&mut cfg);
    let mut e = ServerEntry::new("https://self-signed.example", "S");
    e.user_id = Some("U1".into());
    e.pinned_certificates = vec![PinnedCertificate {
        host: "self-signed.example".into(),
        sha256: rocketchat::tls::fingerprint_hex(&[7; 32]),
    }];
    let sid = cfg.add_server(e);
    cfg.save(dir).unwrap();
    sid
}

fn no_exceptions() -> Arc<Policy> {
    policy(&[("AllowCertificateExceptions", RawValue::Bool(false))])
}

/// Five signed-in pinned servers whose pins leave effect at once: one
/// notice names all of them (the app keeps only four toasts).
#[tokio::test]
async fn one_pins_notice_names_every_server() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = english_config();
    rustket::legal::accept_terms(&mut cfg);
    let names = ["Alpha", "Bravo", "Charlie", "Delta", "Echo"];
    let ids: Vec<ServerId> = names
        .iter()
        .map(|name| {
            let host = format!("{}.example", name.to_lowercase());
            let mut e = ServerEntry::new(format!("https://{host}"), *name);
            e.user_id = Some("U1".into());
            e.pinned_certificates = vec![PinnedCertificate {
                host,
                sha256: rocketchat::tls::fingerprint_hex(&[7; 32]),
            }];
            cfg.add_server(e)
        })
        .collect();
    cfg.save(dir.path()).unwrap();
    let mut a = app(dir.path(), Arc::default());
    let accounts: Vec<(&ServerId, &str)> = ids.iter().map(|sid| (sid, "U1")).collect();
    boot(&mut a, &store_with(&accounts));
    assert!(ids.iter().all(|sid| signed_in(&a, sid)));
    let toasts = a.toasts.len();
    let _ = a.set_policy(no_exceptions());
    assert_eq!(a.toasts.len(), toasts + 1, "one notice");
    let notice = a.toasts.last().unwrap();
    assert_eq!(
        notice.title,
        "The certificate policy changed for: Alpha, Bravo, Charlie, Delta, Echo. \
         It takes effect the next time you sign in to these workspaces, at the latest when \
         Rustket is started again."
    );
}

/// `AllowCertificateExceptions = false`: the TLS configuration gets no
/// pins; they stay in the file and apply again without the key.
#[tokio::test]
async fn pins_are_ignored_while_exceptions_are_off() {
    let dir = tempfile::tempdir().unwrap();
    let sid = pinned_config(dir.path());
    let mut a = app(dir.path(), no_exceptions());
    let s = &a.servers[&sid];
    assert!(s.pins.is_empty());
    assert_eq!(s.stored_pins.len(), 1, "the user's data");
    let _ = a.set_policy(Arc::default());
    assert_eq!(a.servers[&sid].pins.len(), 1, "back without the key");
    a.flush_config();
    assert_eq!(
        on_disk(dir.path())
            .server(&sid)
            .unwrap()
            .pinned_certificates
            .len(),
        1
    );
}

/// A local Rocket.Chat answering just enough for a session to start (REST
/// `me` and public settings, DDP connect, login and subscriptions). Counts
/// the DDP connections that ended.
struct MockServer {
    url: ServerUrl,
    closed: Arc<std::sync::atomic::AtomicUsize>,
}

impl MockServer {
    async fn start() -> Self {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let closed = Arc::new(AtomicUsize::new(0));
        let counter = closed.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let counter = counter.clone();
                tokio::spawn(async move {
                    if Self::serve(stream).await {
                        counter.fetch_add(1, Ordering::SeqCst);
                    }
                });
            }
        });
        MockServer {
            url: ServerUrl::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
            closed,
        }
    }

    /// Serves one connection; returns whether it was a DDP connection.
    async fn serve(mut stream: tokio::net::TcpStream) -> bool {
        use iced::futures::{SinkExt, StreamExt};
        use serde_json::{Value, json};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_tungstenite::tungstenite::Message as Ws;
        let mut head = [0u8; 16];
        let n = stream.peek(&mut head).await.unwrap_or(0);
        if head[..n].starts_with(b"GET /api/") {
            let mut req = Vec::new();
            let mut buf = [0u8; 4096];
            while !req.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf).await {
                    Ok(0) | Err(_) => return false,
                    Ok(n) => req.extend_from_slice(&buf[..n]),
                }
            }
            let body = if req.starts_with(b"GET /api/v1/me") {
                json!({"_id": "U1", "username": "u1", "success": true})
            } else {
                json!({"settings": [], "count": 0, "offset": 0, "total": 0, "success": true})
            }
            .to_string();
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            let _ = stream.shutdown().await;
            return false;
        }
        let Ok(mut ws) = tokio_tungstenite::accept_async(stream).await else {
            return false;
        };
        let text = |v: Value| Ws::text(v.to_string());
        if ws.send(text(json!({"server_id": "0"}))).await.is_err() {
            return true;
        }
        while let Some(Ok(frame)) = ws.next().await {
            let Ws::Text(t) = frame else {
                if matches!(frame, Ws::Close(_)) {
                    break;
                }
                continue;
            };
            let v: Value = serde_json::from_str(&t).unwrap_or_default();
            let id = v["id"].clone();
            let reply = match v["msg"].as_str().unwrap_or_default() {
                "connect" => Some(json!({"msg": "connected", "session": "s1"})),
                "ping" => Some(json!({"msg": "pong", "id": id})),
                "method" if v["method"] == "login" => Some(
                    json!({"msg": "result", "id": id, "result": {"id": "U1", "token": "T-U1", "type": "resume"}}),
                ),
                "method" => Some(json!({"msg": "result", "id": id, "result": null})),
                "sub" => Some(json!({"msg": "ready", "subs": [id]})),
                _ => None,
            };
            if let Some(r) = reply
                && ws.send(text(r)).await.is_err()
            {
                break;
            }
        }
        true
    }

    fn closed(&self) -> usize {
        self.closed.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Waits (bounded) until `n` DDP connections ended.
    async fn wait_closed(&self, n: usize) -> bool {
        for _ in 0..100 {
            if self.closed() >= n {
                return true;
            }
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
        false
    }
}

/// A live session on `mock` handed to signed-in `sid` the way its session
/// subscription hands one over (`ServerEvent::Ready`). The events are
/// returned to keep the session's stream open.
async fn go_live(
    a: &mut App,
    sid: &ServerId,
    mock: &MockServer,
) -> rocketchat::session::SessionEvents {
    use rocketchat::session::{Session, SessionOptions};
    use rustket::session::{ServerEvent, SessionHandle};
    let tls = rocketchat::TlsConfig::new().build().unwrap();
    let (session, events) = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        Session::start(
            mock.url.clone(),
            &tls,
            Credentials::new("U1", "T-U1"),
            SessionOptions::default(),
        ),
    )
    .await
    .expect("the mock session did not start")
    .unwrap();
    let generation = a.servers[sid].generation;
    let _ = a.update(Message::Server(
        sid.clone(),
        generation,
        ServerEvent::Ready(SessionHandle::new(Arc::new(session))),
    ));
    assert!(a.servers[sid].session.is_some());
    events
}

/// The session states a signed-in server can be in when a refresh comes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPhase {
    /// The session subscription runs, no session yet.
    Starting,
    /// A live session, connected.
    Connected,
    /// A live session whose connection dropped and is retried.
    Reconnecting,
    /// No session yet, its first start is retried.
    ReconnectingBeforeReady,
    /// The live session ended (Retry shown).
    Ended,
    /// The start failed (the error shown with Retry).
    StartFailed,
}

const PHASES: [SessionPhase; 6] = [
    SessionPhase::Starting,
    SessionPhase::Connected,
    SessionPhase::Reconnecting,
    SessionPhase::ReconnectingBeforeReady,
    SessionPhase::Ended,
    SessionPhase::StartFailed,
];

impl SessionPhase {
    /// Whether a session handle is held in this phase.
    fn live(self) -> bool {
        matches!(self, SessionPhase::Connected | SessionPhase::Reconnecting)
    }
}

/// Brings signed-in server `sid` to `phase` (a live session runs on
/// `mock`), with a sync running, a resync pending, a failed sync counted
/// and a draft. Returns the events of a session started on `mock`.
async fn enter_phase(
    a: &mut App,
    sid: &ServerId,
    phase: SessionPhase,
    mock: &MockServer,
) -> Option<rocketchat::session::SessionEvents> {
    use rocketchat::session::{ConnectionState as RcConn, SessionEvent};
    use rustket::session::ServerEvent;
    let conn = |state: RcConn| ServerEvent::Event(Box::new(SessionEvent::Connection(state)));
    let reconnecting = || {
        conn(RcConn::Reconnecting {
            attempt: 1,
            retry_in: std::time::Duration::from_secs(1),
            reason: "gone".into(),
        })
    };
    let mut events = None;
    let send = |a: &mut App, ev: ServerEvent| {
        let generation = a.servers[sid].generation;
        let _ = a.update(Message::Server(sid.clone(), generation, ev));
    };
    if matches!(
        phase,
        SessionPhase::Connected | SessionPhase::Reconnecting | SessionPhase::Ended
    ) {
        events = Some(go_live(a, sid, mock).await);
        send(a, conn(RcConn::Connected));
    }
    match phase {
        SessionPhase::Reconnecting | SessionPhase::ReconnectingBeforeReady => {
            send(a, reconnecting());
        }
        SessionPhase::Ended => send(a, ServerEvent::Ended),
        SessionPhase::StartFailed => send(
            a,
            ServerEvent::StartFailed(rustket::error::Failure::from(rocketchat::Error::Timeout(
                "start",
            ))),
        ),
        SessionPhase::Starting | SessionPhase::Connected => {}
    }
    let s = a.servers.get_mut(sid).unwrap();
    assert_eq!(s.session.is_some(), phase.live(), "{phase:?}");
    assert_eq!(
        s.start_error.is_some(),
        phase == SessionPhase::StartFailed,
        "{phase:?}"
    );
    s.syncing = true;
    s.resync_pending = true;
    s.sync_attempt = 2;
    s.sync_error = Some("old sync failed".into());
    s.workspace.set_draft("r1", None, "unsent words");
    events
}

fn server_dir_of(dir: &Path, sid: &ServerId) -> std::path::PathBuf {
    rustket_core::cache::server_dir(&dir.join("offline"), sid)
}

/// A signed-in pinned server with its offline cache open.
async fn pinned_running(dir: &Path, start: Arc<Policy>) -> (App, ServerId, Arc<MemoryStore>) {
    use rustket::cache::Msg;
    use rustket_core::cache::CacheHandle;
    let sid = pinned_config(dir);
    let mut a = app_with_cache(dir, start);
    let store = store_with(&[(&sid, "U1")]);
    boot(&mut a, &store);
    assert!(signed_in(&a, &sid));
    let _ = a.update(Message::Noop);
    assert!(a.servers[&sid].cache.is_opening());
    let opened = CacheHandle::open(server_dir_of(dir, &sid), "U1".into(), None)
        .await
        .map_err(|e| e.to_string());
    let _ = a.update(Message::Cache(Msg::Opened(
        sid.clone(),
        "U1".into(),
        Box::new(opened),
    )));
    assert!(a.servers[&sid].cache.is_open());
    (a, sid, store)
}

/// The initial load of plain room `R1` and encrypted room `E1`.
fn initial_data() -> Box<rustket_core::sync::InitialData> {
    use rocketchat::models::{Room, RoomType, Subscription};
    let sub = |rid: &str| Subscription {
        id: format!("s-{rid}"),
        rid: rid.into(),
        name: rid.into(),
        t: RoomType::Channel,
        open: true,
        ..Default::default()
    };
    let room = |rid: &str, encrypted: bool| Room {
        id: rid.into(),
        t: RoomType::Channel,
        name: Some(rid.into()),
        encrypted: encrypted.then_some(true),
        ..Default::default()
    };
    let subscriptions = vec![sub("R1"), sub("E1")];
    let rooms = vec![room("R1", false), room("E1", true)];
    Box::new(rustket_core::sync::InitialData {
        me: rocketchat::models::User {
            id: "U1".into(),
            ..Default::default()
        },
        subscriptions,
        rooms,
        ..Default::default()
    })
}

/// How often message `id` is in the outbox of `sid`.
fn in_outbox(a: &App, sid: &ServerId, id: &str) -> usize {
    a.servers[sid]
        .workspace
        .outbox_items()
        .filter(|i| i.message.id == id)
        .count()
}

/// What a refresh must leave as it was on a signed-in server whose pins
/// leave effect (section 9.2).
#[derive(Debug, PartialEq)]
struct Kept {
    credentials: Option<Credentials>,
    rooms: Vec<bool>,
    open_rooms: Vec<String>,
    active_room: Option<String>,
    draft: Option<String>,
    composer_draft: Option<String>,
    pending_room: Option<(Option<String>, Option<String>, u64)>,
    outbox: Vec<usize>,
    cache: (bool, bool),
    login: bool,
    restoring: bool,
    sync: (bool, bool, u32, Option<String>),
    start_error: bool,
}

/// Unsent message ids of `R1` and `E1`.
type Unsent = [String; 2];

fn kept(a: &App, sid: &ServerId, unsent: &Unsent) -> Kept {
    let s = &a.servers[sid];
    let ws = &s.workspace;
    let mut open_rooms: Vec<String> = s.rooms.keys().cloned().collect();
    open_rooms.sort();
    Kept {
        credentials: s.credentials.clone(),
        rooms: ["R1", "E1"]
            .iter()
            .map(|r| ws.room(r).is_some() && ws.subscription(r).is_some())
            .collect(),
        open_rooms,
        active_room: s.active_room.clone(),
        draft: ws.draft("r1", None).map(str::to_owned),
        composer_draft: ws.draft("E1", None).map(str::to_owned),
        pending_room: s.pending_room.clone(),
        outbox: unsent.iter().map(|id| in_outbox(a, sid, id)).collect(),
        cache: (s.cache.is_open(), s.cache.is_opening()),
        login: s.login.is_some(),
        restoring: s.restoring,
        sync: (
            s.syncing,
            s.resync_pending,
            s.sync_attempt,
            s.sync_error.clone(),
        ),
        start_error: s.start_error.is_some(),
    }
}

/// Fills the workspace of signed-in `sid`: rooms `R1` and `E1` loaded,
/// `R1` open, an unsent message in each (the plain one written to the
/// cache), drafts and a room link waiting for the first load.
fn populate(a: &mut App, sid: &ServerId) -> Unsent {
    let epoch = a.servers[sid].generation;
    let _ = a.update(Message::InitialLoaded(
        sid.clone(),
        epoch,
        Ok(initial_data()),
    ));
    let _ = a.open_room(sid, "R1");
    let unsent = ["R1", "E1"].map(|rid| {
        let ws = &mut a.servers.get_mut(sid).unwrap().workspace;
        let m = ws.new_local_message(rid, "unsent", None, false);
        let id = m.id.clone();
        ws.add_pending(m);
        id
    });
    let _ = a.update(Message::Noop);
    let s = a.servers.get_mut(sid).unwrap();
    s.workspace.set_draft("E1", None, "typed in E1");
    s.pending_room = Some((Some("R9".into()), None, 7));
    assert!(s.rooms.contains_key("R1"));
    unsent
}

/// The causes that take the stored pins of the pinned server out of
/// effect: exceptions turned off, or the server becoming managed.
fn pins_off() -> [(&'static str, Arc<Policy>); 2] {
    [
        ("exceptions off", no_exceptions()),
        (
            "managed",
            policy(&[("ManagedServers", list(&["https://self-signed.example"]))]),
        ),
    ]
}

/// The notice of the pinned server `S` (section 9.2).
const PINS_LATER: &str = "The certificate policy changed for: S. It takes effect the next time \
     you sign in to these workspaces, at the latest when Rustket is started again.";

fn pins_notices(a: &App) -> usize {
    a.toasts.iter().filter(|t| t.title == PINS_LATER).count()
}

/// (a) A refresh takes the pins of a signed-in server with a populated
/// workspace out of effect, in every session state: no session is closed
/// or started, a live session handle stays, the generation and the pins of
/// the session are unchanged, nothing of the server changes, and one
/// notice is shown, not again when the same policy comes a second time.
/// After a failed start or an ended session there is no notice: Retry
/// applies the rule.
/// With exceptions off the accept action of the certificate step is gone.
#[tokio::test(flavor = "multi_thread")]
async fn pins_leaving_effect_restart_nothing() {
    for (cause, p) in pins_off() {
        for phase in PHASES {
            let what = format!("{cause}, {phase:?}");
            let dir = tempfile::tempdir().unwrap();
            let mock = MockServer::start().await;
            let (mut a, sid, store) = pinned_running(dir.path(), Arc::default()).await;
            let unsent = populate(&mut a, &sid);
            let _events = enter_phase(&mut a, &sid, phase, &mock).await;
            let handle = a.servers[&sid].session.clone();
            let before = kept(&a, &sid, &unsent);
            assert_eq!(before.rooms, vec![true, true], "{what}");
            assert_eq!(before.outbox, vec![1, 1], "{what}");
            assert_eq!(before.cache, (true, false), "{what}");
            let generation = a.servers[&sid].generation;
            assert_eq!(a.servers[&sid].spec().unwrap().pins.len(), 1, "{what}");
            if phase == SessionPhase::Ended {
                // The ended session's connection goes on its own.
                assert!(mock.wait_closed(1).await, "{what}");
            }
            let closed = mock.closed();

            // After a failed start or an ended session Retry applies the
            // rule at once: no notice.
            let retry_applies = matches!(phase, SessionPhase::StartFailed | SessionPhase::Ended);
            let notices = usize::from(!retry_applies);
            let task = a.set_policy(p.clone());
            assert!(!a.is_suspended(&sid), "{what}");
            assert_eq!(pins_notices(&a), notices, "{what}: the notice");
            // Independent of timing: the only task is the notice's timer.
            assert_eq!(task.units(), notices, "{what}: a task");
            let out = run_out(task).await;
            assert!(
                !out.iter().any(|m| matches!(
                    m,
                    Message::ServerTokenRead(..) | Message::Cache(rustket::cache::Msg::Opened(..))
                )),
                "{what}: a token read or a cache open"
            );
            for m in out {
                let _ = a.update(m);
            }
            let _ = a.update(Message::Noop);
            assert_eq!(mock.closed(), closed, "{what}: a session was closed");
            let s = &a.servers[&sid];
            assert_eq!(s.generation, generation, "{what}: a new session");
            match (&handle, &s.session) {
                (Some(old), Some(now)) => assert!(Arc::ptr_eq(&old.0, &now.0), "{what}"),
                (None, None) => {}
                _ => panic!("{what}: the session handle changed"),
            }
            assert_eq!(s.pins.len(), 1, "{what}: the session keeps its pins");
            let spec = s.spec().expect("the session subscription stopped");
            assert_eq!(spec.pins.len(), 1, "{what}: the session spec changed");
            assert_eq!(spec.generation, generation, "{what}");
            assert_eq!(kept(&a, &sid, &unsent), before, "{what}: something changed");
            assert_eq!(
                store.get(&secret_key(&sid, "U1")).unwrap().as_deref(),
                Some("T-U1"),
                "{what}"
            );
            a.flush_config();
            let entry = on_disk(dir.path()).server(&sid).cloned().unwrap();
            assert_eq!(entry.pinned_certificates.len(), 1, "{what}: pins deleted");

            // The same policy again: nothing changed, no second notice.
            let _ = a.set_policy(p.clone());
            assert_eq!(pins_notices(&a), notices, "{what}: a second notice");
            assert_eq!(a.servers[&sid].generation, generation, "{what}");

            if retry_applies {
                let _ = a.update(Message::RetryConnection(sid.clone()));
                let s = &a.servers[&sid];
                assert!(s.pins.is_empty(), "{what}: Retry kept the pins");
                assert!(s.spec().unwrap().pins.is_empty(), "{what}");
            }

            if cause == "exceptions off" {
                assert!(!a.policy.allow_certificate_exceptions(), "{what}");
                assert_trust_refused(&mut a);
            }
        }
    }
}

/// The certificate step offers no accept action: the accept message is
/// refused.
fn assert_trust_refused(a: &mut App) {
    use rocketchat::CertificateInfo;
    use rustket::screens::add_server::CertPrompt;
    use rustket::screens::cert::CertView;
    let _ = a.update(Message::ShowAddServer);
    let chain = vec![CertificateInfo::from_der(b"leaf")];
    let Screen::AddServer(s) = &mut a.screen else {
        panic!("the add-server screen did not open");
    };
    s.url = "https://other-self-signed.example".into();
    s.cert_prompt = Some(CertPrompt {
        url: ServerUrl::parse("https://other-self-signed.example").unwrap(),
        host: "other-self-signed.example".into(),
        views: chain.iter().map(CertView::new).collect(),
        chain,
    });
    let task = a.update(Message::AddServer(add_server::Message::TrustCertificate));
    assert_eq!(task.units(), 0, "a check with the exception");
    let s = add_screen(a).unwrap();
    assert!(s.pins.is_empty() && !s.busy);
}

/// (b) The same server after a new start of the app on the same config
/// directory under the policy that took its pins out of effect: its
/// session starts without them; the pin stays in the file.
#[tokio::test(flavor = "multi_thread")]
async fn pins_out_of_effect_are_not_used_after_a_new_start() {
    for (cause, p) in pins_off() {
        let dir = tempfile::tempdir().unwrap();
        let mock = MockServer::start().await;
        let (mut a, sid, store) = pinned_running(dir.path(), Arc::default()).await;
        let _events = enter_phase(&mut a, &sid, SessionPhase::Connected, &mock).await;
        let _ = a.set_policy(p.clone());
        assert_eq!(a.servers[&sid].spec().unwrap().pins.len(), 1, "{cause}");
        a.flush_config();
        drop(a);
        let mut a = app(dir.path(), p);
        boot(&mut a, &store);
        assert!(signed_in(&a, &sid), "{cause}");
        let s = &a.servers[&sid];
        assert!(
            s.spec().unwrap().pins.is_empty(),
            "{cause}: started with the pin"
        );
        assert_eq!(s.stored_pins.len(), 1, "{cause}");
        assert_eq!(pins_notices(&a), 0, "{cause}: a notice at start");
    }
}

/// (c) Retry after a failed start: the pins in effect are decided as the
/// new session starts. Under the policy that took them out of effect it
/// starts without them; under one that allows them again, with them.
/// Otherwise Retry does what it always did: a new generation, the start
/// error cleared, nothing else of the server touched.
#[tokio::test(flavor = "multi_thread")]
async fn retry_after_a_failed_start_decides_the_pins() {
    for (cause, p) in pins_off() {
        let dir = tempfile::tempdir().unwrap();
        let mock = MockServer::start().await;
        let (mut a, sid, _store) = pinned_running(dir.path(), Arc::default()).await;
        let unsent = populate(&mut a, &sid);
        let retry = |a: &mut App, pins: usize, what: &str| {
            let old = a.servers[&sid].generation;
            let before = Kept {
                start_error: false,
                ..kept(a, &sid, &unsent)
            };
            let task = a.update(Message::RetryConnection(sid.clone()));
            assert_eq!(task.units(), 0, "{what}: a task");
            let s = &a.servers[&sid];
            assert_eq!(s.generation, old + 1, "{what}: not one new session");
            assert!(s.start_error.is_none(), "{what}");
            let spec = s.spec().expect("no session is started");
            assert_eq!(spec.pins.len(), pins, "{what}: the pins of the new session");
            assert_eq!(spec.generation, s.generation, "{what}");
            assert_eq!(kept(a, &sid, &unsent), before, "{what}: something changed");
        };
        let _events = enter_phase(&mut a, &sid, SessionPhase::StartFailed, &mock).await;
        let _ = a.set_policy(p.clone());
        assert_eq!(a.servers[&sid].spec().unwrap().pins.len(), 1, "{cause}");
        retry(&mut a, 0, cause);
        let _events = enter_phase(&mut a, &sid, SessionPhase::StartFailed, &mock).await;
        let _ = a.set_policy(Arc::default());
        assert_eq!(
            pins_notices(&a),
            0,
            "{cause}: no notice after a failed start, none for pins coming back"
        );
        assert!(a.servers[&sid].spec().unwrap().pins.is_empty(), "{cause}");
        retry(&mut a, 1, &format!("{cause}, allowed again"));
    }
}

/// Like [`boot`], with the secret store opened on `backend`.
fn boot_on(app: &mut App, store: &Arc<MemoryStore>, backend: SecretBackend) {
    let tokens = app
        .startup_token_reads()
        .into_iter()
        .map(|(sid, uid)| {
            let t = store.get(&secret_key(&sid, &uid)).unwrap();
            (sid, t)
        })
        .collect();
    let _ = app.update(Message::SecretsReady(SecretsReady {
        store: Arc::new(SwitchableStore::new(Box::new(store.clone()), backend, None)),
        tokens,
        notices: vec![],
    }));
}

/// (e) A signed-in server that stays allowed is not touched by a refresh
/// of any key, in any session state, and gets no notice: no session is
/// closed, restarted or replaced, the generation, credentials, session
/// spec, sync state and token stay. Each forcing policy is followed by
/// removing the policy again.
#[tokio::test(flavor = "multi_thread")]
async fn server_without_pins_is_untouched() {
    // The server a managed entry adds: its login fetch goes to a local mock,
    // never to an external host.
    let extra = MockServer::start().await;
    let s = |v: &str| RawValue::String(v.into());
    let b = RawValue::Bool;
    let cases: Vec<(&str, Arc<Policy>, SecretBackend)> = vec![
        ("exceptions off", no_exceptions(), SecretBackend::Keyring),
        (
            "managed",
            policy(&[("ManagedServers", list(&["https://chat.corp.example"]))]),
            SecretBackend::Keyring,
        ),
        (
            "managed, another server added",
            policy(&[(
                "ManagedServers",
                list(&["https://chat.corp.example", extra.url.as_str()]),
            )]),
            SecretBackend::Keyring,
        ),
        (
            "sign-in and organization keys",
            policy(&[
                ("AllowPasswordLogin", b(false)),
                ("AllowTokenLogin", b(false)),
                ("OrganizationName", s("Acme")),
                ("SupportUrl", s("https://help.corp.example")),
            ]),
            SecretBackend::Keyring,
        ),
        (
            "secrets file off",
            policy(&[("AllowSecretsFile", b(false))]),
            SecretBackend::File,
        ),
        (
            "preference keys",
            policy(&[
                ("UpdateMode", s("off")),
                ("NotificationPreview", b(false)),
                ("HardwareAcceleration", b(false)),
                ("LaunchAtLogin", b(true)),
            ]),
            SecretBackend::Keyring,
        ),
        (
            "server keys that still allow it",
            policy(&[
                ("AllowUserServers", b(false)),
                ("AllowedServers", list(&["*.corp.example"])),
                ("RequireHttps", b(true)),
                ("MinimumVersion", s("0.0.1")),
                ("SomeFutureKey", b(true)),
            ]),
            SecretBackend::Keyring,
        ),
    ];
    for (case, p, backend) in &cases {
        for phase in PHASES {
            let what = format!("{case}, {phase:?}");
            let mock = MockServer::start().await;
            let dir = tempfile::tempdir().unwrap();
            let ids = write_config(dir.path(), &[("https://chat.corp.example", Some("U1"))]);
            let sid = &ids[0];
            let mut a = app(dir.path(), Arc::default());
            // Not a desktop run: no autostart location, so `LaunchAtLogin`
            // writes nothing to this machine.
            assert!(!Flags::default().desktop);
            let store = store_with(&[(sid, "U1")]);
            boot_on(&mut a, &store, *backend);
            let _events = enter_phase(&mut a, sid, phase, &mock).await;
            if phase == SessionPhase::Ended {
                // The ended session's connection goes on its own.
                assert!(mock.wait_closed(1).await, "{what}");
            }
            let closed = mock.closed();
            let snap = |a: &App| {
                let s = &a.servers[sid];
                (
                    s.generation,
                    s.credentials.clone(),
                    s.spec().map(|spec| spec.generation),
                    s.start_error.is_some(),
                    (
                        s.syncing,
                        s.resync_pending,
                        s.sync_attempt,
                        s.sync_error.clone(),
                    ),
                )
            };
            let before = snap(&a);
            assert_eq!(before.2, Some(before.0), "{what}: no session spec");
            let handle = a.servers[sid].session.clone();
            for (step, next) in [("forced", p.clone()), ("removed", Arc::default())] {
                let what = format!("{what}, {step}");
                let toasts = a.toasts.len();
                let task = a.set_policy(next);
                let notices = a.toasts.len() - toasts;
                let out = run_out(task).await;
                assert!(
                    !out.iter().any(|m| matches!(
                        m,
                        Message::ServerTokenRead(id, ..) | Message::Cache(rustket::cache::Msg::Opened(id, ..))
                            if id == sid
                    )),
                    "{what}: a token read or a cache open"
                );
                for m in out {
                    let _ = a.update(m);
                }
                let _ = a.update(Message::Noop);
                assert!(
                    !a.toasts
                        .iter()
                        .rev()
                        .take(notices)
                        .any(|t| t.title.starts_with("The certificate policy changed")),
                    "{what}: a pins notice"
                );
                assert_eq!(snap(&a), before, "{what}: the server changed");
                let s = &a.servers[sid];
                match (&handle, &s.session) {
                    (Some(old), Some(now)) => assert!(Arc::ptr_eq(&old.0, &now.0), "{what}"),
                    (None, None) => {}
                    _ => panic!("{what}: the session handle changed"),
                }
                assert!(!s.restoring && s.login.is_none(), "{what}");
                assert_eq!(mock.closed(), closed, "{what}: a session was closed");
                // The store moves only for the secrets key (off the file into
                // memory; back to plain memory, since this store has no
                // folder to write to); the key does not apply to the Mac App
                // Store flavor.
                let moved = *case == "secrets file off" && !cfg!(feature = "mas");
                let expected = match (moved, step) {
                    (false, _) => *backend,
                    (true, "forced") => SecretBackend::MemoryByPolicy,
                    (true, _) => SecretBackend::Memory,
                };
                assert_eq!(a.secret_backend, Some(expected), "{what}: the store");
                let own = a.secrets.as_ref().expect("the store is open");
                assert_eq!(
                    own.get(&secret_key(sid, "U1")).unwrap().as_deref(),
                    Some("T-U1"),
                    "{what}: the token in the app's store"
                );
            }
        }
    }
}

/// (d) A pinned server whose token read is in flight gets no new session;
/// the session that starts once the token arrives has no pins.
#[tokio::test]
async fn pinned_server_reading_its_token_starts_without_pins() {
    let dir = tempfile::tempdir().unwrap();
    let sid = pinned_config(dir.path());
    let mut a = app(dir.path(), Arc::default());
    boot(&mut a, &Arc::new(MemoryStore::new()));
    // A token read in flight, as when a suspended server is allowed again.
    let _ = a.set_policy(allowed(&["other.example"]));
    let _ = a.set_policy(Arc::default());
    let s = &a.servers[&sid];
    assert!(s.restoring && s.credentials.is_none(), "no token read");
    assert_eq!(s.pins.len(), 1);
    let before = s.generation;
    let task = a.set_policy(no_exceptions());
    assert_eq!(task.units(), 0, "a task");
    let s = &a.servers[&sid];
    assert!(s.restoring, "the read was dropped");
    assert_eq!(s.generation, before, "a new session");
    assert!(s.pins.is_empty());
    let _ = a.update(Message::ServerTokenRead(
        sid.clone(),
        "U1".into(),
        Some(Credentials::new("U1", "T-U1")),
    ));
    let s = &a.servers[&sid];
    assert!(signed_in(&a, &sid));
    assert!(s.spec().unwrap().pins.is_empty(), "started with the pin");
}

/// The accept message of the certificate step is refused while exceptions
/// are off.
#[tokio::test]
async fn certificate_accept_is_refused() {
    use rocketchat::CertificateInfo;
    use rustket::screens::add_server::CertPrompt;
    use rustket::screens::cert::CertView;
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), &[("https://chat.corp.example", None)]);
    let mut a = app(dir.path(), no_exceptions());
    boot(&mut a, &Arc::new(MemoryStore::new()));
    let _ = a.update(Message::ShowAddServer);
    let chain = vec![CertificateInfo::from_der(b"leaf")];
    if let Screen::AddServer(s) = &mut a.screen {
        s.url = "https://self-signed.example".into();
        s.cert_prompt = Some(CertPrompt {
            url: ServerUrl::parse("https://self-signed.example").unwrap(),
            host: "self-signed.example".into(),
            views: chain.iter().map(CertView::new).collect(),
            chain,
        });
    }
    let task = a.update(Message::AddServer(add_server::Message::TrustCertificate));
    assert_eq!(task.units(), 0, "a check with the exception");
    let s = add_screen(&a).unwrap();
    assert!(s.pins.is_empty());
    assert!(!s.busy);
}

/// A managed server's certificate must be trusted by the OS: its stored
/// pins are ignored also while exceptions are allowed.
#[test]
fn managed_server_ignores_its_pins() {
    let dir = tempfile::tempdir().unwrap();
    let sid = pinned_config(dir.path());
    let mut a = app(
        dir.path(),
        policy(&[("ManagedServers", list(&["https://self-signed.example"]))]),
    );
    assert!(a.servers[&sid].pins.is_empty());
    let _ = a.set_policy(Arc::default());
    assert_eq!(a.servers[&sid].pins.len(), 1, "an ordinary server again");
}

// --- blocking state ------------------------------------------------------------------

/// The two blocking kinds (docs/policy.md section 6).
fn blocking_kinds() -> [(&'static str, Arc<Policy>); 2] {
    use rustket_core::policy::{SourceError, SourceErrorKind};
    let unreadable = Snapshot::unreadable(
        SourceId::LinuxFile("corp.json".into()),
        SourceError::new(SourceErrorKind::TooLarge),
    );
    [
        (
            "unreadable",
            Arc::new(resolve(&[unreadable], &rustket::policy::context())),
        ),
        (
            "minimum version, managed server",
            policy(&[
                ("MinimumVersion", RawValue::String("999.0.0".into())),
                ("ManagedServers", list(&["https://chat.corp.example"])),
            ]),
        ),
    ]
}

/// A refresh into the block tears every running server down like a
/// suspension (managed ones included): token, offline cache, config entry
/// and drafts stay. A link waits. Out of the block the servers start as at
/// start-up and the link is handled.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_into_and_out_of_the_block() {
    use rustket::cache::Msg;
    use rustket_core::cache::{CacheHandle, server_dir};
    for (kind, blocked) in blocking_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let ids = write_config(
            dir.path(),
            &[
                ("https://chat.corp.example", Some("U1")),
                ("https://other.corp.example", None),
            ],
        );
        let (sid, other) = (&ids[0], &ids[1]);
        let managed = policy(&[("ManagedServers", list(&["https://chat.corp.example"]))]);
        let mut a = app_with_cache(dir.path(), managed);
        let store = store_with(&[(sid, "U1")]);
        boot(&mut a, &store);
        assert!(signed_in(&a, sid), "{kind}");
        assert!(a.servers[other].login.is_some(), "{kind}");
        let _ = a.update(Message::Noop);
        let offline = server_dir(&dir.path().join("offline"), sid);
        let opened = CacheHandle::open(offline.clone(), "U1".into(), None)
            .await
            .map_err(|e| e.to_string());
        let _ = a.update(Message::Cache(Msg::Opened(
            sid.clone(),
            "U1".into(),
            Box::new(opened),
        )));
        assert!(a.servers[sid].cache.is_open(), "{kind}");
        a.servers
            .get_mut(sid)
            .unwrap()
            .workspace
            .set_draft("r1", None, "unsent words");

        // A blocked policy suspends managed servers too.
        let _ = a.set_policy(blocked.clone());
        for s in [sid, other] {
            assert!(a.is_suspended(s), "{kind}");
            assert!(stopped(&a, s), "{kind}");
        }
        assert!(
            !a.servers[sid].cache.is_open(),
            "{kind}: the cache stays open"
        );
        assert!(offline.exists(), "{kind}: the offline cache is gone");
        assert_eq!(
            a.servers[sid].workspace.draft("r1", None),
            Some("unsent words"),
            "{kind}"
        );
        assert_eq!(
            store.get(&secret_key(sid, "U1")).unwrap().as_deref(),
            Some("T-U1"),
            "{kind}"
        );
        assert!(a.config.pending_secret_deletions.is_empty(), "{kind}");
        a.flush_config();
        let disk = on_disk(dir.path());
        assert_eq!(
            disk.server(sid).unwrap().user_id.as_deref(),
            Some("U1"),
            "{kind}"
        );
        assert!(disk.server(other).is_some(), "{kind}");

        // A link waits; nothing reconnects meanwhile.
        let _ = a.update(Message::DeepLinks(vec![FOREIGN_LINK.into()]));
        assert!(a.modals.is_empty() && a.toasts.is_empty(), "{kind}");
        let _ = a.update(Message::RetryConnection(sid.clone()));
        let _ = a.update(Message::Noop);
        assert!(stopped(&a, sid), "{kind}");
        // A token read that was in flight starts nothing.
        let _ = a.update(Message::ServerTokenRead(
            sid.clone(),
            "U1".into(),
            Some(Credentials::new("U1", "T-U1")),
        ));
        assert!(stopped(&a, sid), "{kind}");

        // Out of the block: started as at start-up, then the link.
        let task = a.set_policy(Arc::default());
        assert!(task.units() > 0, "{kind}: no token read");
        assert!(a.servers[sid].restoring, "{kind}");
        assert!(a.servers[other].login.is_some(), "{kind}");
        assert!(
            matches!(a.modals.last(), Some(Modal::OfferAddServer { .. })),
            "{kind}: the waiting link"
        );
        let _ = a.update(Message::ServerTokenRead(
            sid.clone(),
            "U1".into(),
            Some(Credentials::new("U1", "T-U1")),
        ));
        assert!(signed_in(&a, sid), "{kind}");
        assert_eq!(
            a.servers[sid].workspace.draft("r1", None),
            Some("unsent words"),
            "{kind}"
        );
    }
}

/// A refresh into the block leaves nothing behind that leads to a server:
/// a ringing call and its popup go, an open start-call dialog closes, a
/// toast button that would navigate
/// loses it (a quit button stays), and a window hidden to the tray comes
/// back, because the tray falls silent.
#[tokio::test(flavor = "multi_thread")]
async fn refresh_into_the_block_leaves_no_way_behind_it() {
    use rocketchat::models::{UserRef, VideoConfKind, VideoConfStatus, VideoConference};
    use rustket_ui::widget::BannerKind;
    for (kind, blocked) in blocking_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let ids = write_config(dir.path(), &[("https://chat.corp.example", Some("U1"))]);
        let sid = &ids[0];
        let mut a = app_with_cache(dir.path(), Arc::default());
        let store = store_with(&[(sid, "U1")]);
        boot(&mut a, &store);
        assert!(signed_in(&a, sid), "{kind}");
        let call = VideoConference {
            id: "call-1".into(),
            kind: VideoConfKind::Direct,
            rid: "dm-1".into(),
            status: VideoConfStatus::Calling,
            created_by: Some(UserRef {
                id: "id-peer".into(),
                username: Some("peer".into()),
                name: Some("Peer".into()),
            }),
            provider_name: "jitsi".into(),
            ..Default::default()
        };
        a.calls.inject_incoming(sid, "U1", &call);
        assert!(
            a.calls
                .manager(sid)
                .is_some_and(|m| m.incoming().next().is_some()),
            "{kind}: no ringing call"
        );
        // The start-call dialog of the server, as the capabilities open it.
        let _ = a.update(Message::Calls(rustket::videoconf::Message::Capabilities(
            sid.clone(),
            "dm-1".into(),
            Ok(rocketchat::models::ProviderCapabilities {
                provider_name: "jitsi".into(),
                capabilities: Default::default(),
            }),
        )));
        assert!(
            matches!(
                &a.calls.dialog,
                Some((d, rustket::videoconf::Dialog::Start { .. })) if d == sid
            ),
            "{kind}: no start-call dialog"
        );
        let _ = a.toast_with_action(
            BannerKind::Info,
            "settings",
            "",
            "Open",
            Message::Go(Go::Nav(Nav::SecretStoreSettings)),
        );
        let _ = a.toast_with_action(BannerKind::Info, "quit", "", "Quit", Message::Go(Go::Quit));
        a.desktop.hidden = true;

        let _ = a.set_policy(blocked.clone());
        assert!(!a.desktop.hidden, "{kind}: the window stays hidden");
        let _ = a.update(Message::Noop);
        assert!(a.calls.manager(sid).is_none(), "{kind}: the call rings on");
        assert!(
            a.calls.dialog.is_none(),
            "{kind}: the start-call dialog stays"
        );
        let action = |a: &App, title: &str| {
            a.toasts
                .iter()
                .find(|t| t.title == title)
                .map(|t| t.action.is_some())
        };
        assert_eq!(action(&a, "settings"), Some(false), "{kind}");
        assert_eq!(action(&a, "quit"), Some(true), "{kind}");
        // A toast added while blocked gets no button that leads nowhere.
        let _ = a.toast_with_action(
            BannerKind::Info,
            "later",
            "",
            "Open",
            Message::Go(Go::Nav(Nav::SecretStoreSettings)),
        );
        assert_eq!(action(&a, "later"), Some(false), "{kind}");
        // The block's end does not show a hidden window again.
        a.desktop.hidden = true;
        let _ = a.set_policy(Arc::default());
        assert!(a.desktop.hidden, "{kind}");
    }
}

/// Blocked from the start: no token is read, no server starts, no cache
/// opens, links wait; the empty policy afterwards starts everything.
#[tokio::test]
async fn blocked_at_start_starts_nothing() {
    for (kind, blocked) in blocking_kinds() {
        let dir = tempfile::tempdir().unwrap();
        let ids = write_config(
            dir.path(),
            &[
                ("https://chat.corp.example", Some("U1")),
                ("https://other.corp.example", None),
            ],
        );
        let mut a = app_with_cache(dir.path(), blocked);
        assert!(a.startup_token_reads().is_empty(), "{kind}");
        let _ = a.update(Message::DeepLinks(vec![FOREIGN_LINK.into()]));
        boot(&mut a, &store_with(&[(&ids[0], "U1")]));
        let _ = a.update(Message::Noop);
        for sid in &ids {
            assert!(stopped(&a, sid), "{kind}");
            let c = &a.servers[sid].cache;
            assert!(!c.is_open() && !c.is_opening(), "{kind}");
        }
        assert!(a.modals.is_empty(), "{kind}: the link was handled");
        assert_eq!(a.title(), "Rustket", "{kind}");
        let tray = a.desktop.tray.state();
        assert_eq!((tray.unread, tray.mentions), (0, 0), "{kind}");
        let _ = a.update(Message::Go(Go::Nav(Nav::Settings)));
        assert!(!matches!(a.screen, Screen::Settings(_)), "{kind}");

        let _ = a.set_policy(Arc::default());
        assert!(a.servers[&ids[0]].restoring, "{kind}");
        assert!(a.servers[&ids[1]].login.is_some(), "{kind}");
        assert!(
            matches!(a.modals.last(), Some(Modal::OfferAddServer { .. })),
            "{kind}"
        );
    }
}
