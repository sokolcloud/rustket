//! App shell and admin flows driven through `App::update` / component
//! `update` without network (returned tasks are dropped): leaving the admin
//! screen with unsaved settings, logout while a server page is shown, late
//! answers of closed dialogs and forms.
#![allow(clippy::unwrap_used)]

use rocketchat::ServerUrl;
use rocketchat::SessionEvent;
use rocketchat::models::admin::{AdminUser, SamlMetadata, SettingDef};
use rocketchat::models::{Email, Permission, User};
use rustket::app::{App, Flags, Modal, Screen, SecretsMode};
use rustket::component::Ctx;
use rustket::images::Images;
use rustket::message::{Go, Message};
use rustket::screens::admin::common::Cx;
use rustket::screens::admin::{self, settings, users};
use rustket::session::ServerEvent;
use rustket::workspace::ServerState;
use rustket_core::admin::server_i18n::ServerI18n;
use rustket_core::config::ServerEntry;
use rustket_core::{AppConfig, Change, ServerId, Workspace};
use serde_json::json;

fn admin_ws() -> Workspace {
    let mut ws = Workspace::new();
    ws.apply_me(User {
        id: "me".into(),
        username: Some("admin".into()),
        roles: Some(vec!["admin".into(), "user".into()]),
        ..Default::default()
    });
    for p in [
        "edit-privileged-setting",
        "view-privileged-setting",
        "view-user-administration",
        "edit-other-user-info",
        "assign-admin-role",
    ] {
        ws.apply_permission(
            Change::Inserted,
            Permission {
                id: p.into(),
                roles: vec!["admin".into()],
                updated_at: None,
            },
        );
    }
    ws
}

fn with_cx<R>(ws: &mut Workspace, f: impl FnOnce(&mut Cx<'_, '_>) -> R) -> R {
    let sid = ServerId("s".into());
    let url = ServerUrl::parse("http://localhost:3000").unwrap();
    let images = Images::new(None);
    let config = AppConfig::default();
    let mut ctx = Ctx {
        server_id: &sid,
        server: &url,
        workspace: ws,
        session: None,
        images: &images,
        config: &config,
    };
    let i18n = ServerI18n::default();
    let mut cx = Cx {
        ctx: &mut ctx,
        i18n: &i18n,
        license: None,
        effects: Vec::new(),
        confirm: None,
    };
    f(&mut cx)
}

fn defs() -> Vec<SettingDef> {
    serde_json::from_value(json!([
        {"_id": "SAML", "type": "group", "i18nLabel": "SAML"},
        {"_id": "SAML_Custom_Default_entry_point", "type": "string", "group": "SAML",
         "value": "https://idp.example/sso", "packageValue": "", "sorter": 1},
        {"_id": "SAML_Custom_Default_cert", "type": "string", "multiline": true, "group": "SAML",
         "value": "OLD-CERT", "packageValue": "", "sorter": 2},
        {"_id": "Accounts", "type": "group", "i18nLabel": "Accounts"},
        {"_id": "A_bool", "type": "boolean", "group": "Accounts", "value": true,
         "packageValue": true, "sorter": 1},
    ]))
    .unwrap()
}

/// An app with servers `names` (the first one active), each with a logged-in
/// session state (credentials, admin workspace).
fn app_with(names: &[&str]) -> (App, Vec<ServerId>) {
    let (mut app, _t) = App::new(Flags {
        ephemeral: true,
        secrets: SecretsMode::Memory,
        open_urls: false,
        ..Flags::default()
    });
    let _ = app.update(Message::SecretsReady(rustket::message::SecretsReady {
        store: std::sync::Arc::new(rustket_core::secrets::MemoryStore::new()),
        backend: rustket_core::secrets::SecretBackend::Memory,
        tokens: vec![],
        notices: vec![],
    }));
    let mut ids = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let entry = ServerEntry::new(format!("http://localhost:{}", 3000 + i), *name);
        let sid = app.config.add_server(entry.clone());
        let mut state = ServerState::from_entry(&entry).unwrap();
        state.workspace = admin_ws();
        state.restoring = false;
        state.credentials = Some(rocketchat::Credentials {
            user_id: "me".into(),
            auth_token: "tok".into(),
        });
        app.servers.insert(sid.clone(), state);
        ids.push(sid);
    }
    app.active = Some(ids[0].clone());
    app.screen = Screen::Server;
    (app, ids)
}

/// Shows the admin settings page of `sid` with an unsaved edit.
fn dirty_admin(app: &mut App, sid: &ServerId) {
    let mut ws = admin_ws();
    let mut st = settings::State::new();
    with_cx(&mut ws, |cx| {
        let _ = st.update(settings::Msg::Loaded(Ok(defs())), cx);
        let _ = st.update(settings::Msg::OpenGroup("Accounts".into()), cx);
        let _ = st.update(settings::Msg::Set("A_bool".into(), json!(false)), cx);
    });
    assert!(st.is_dirty());
    let mut a = admin::Admin::new();
    a.page = admin::Page::Settings(Box::new(st));
    app.screen = Screen::Admin(sid.clone(), Box::new(a));
}

fn asked(app: &App) -> bool {
    matches!(&app.screen, Screen::Admin(_, a) if a.confirm.is_some())
}

#[tokio::test]
async fn unsaved_admin_settings_hold_back_every_exit() {
    let (mut app, ids) = app_with(&["One", "Two"]);
    let sid = ids[0].clone();
    let exits = [
        // Call popup "open room", quick switcher pick, notification click.
        Message::Calls(rustket::videoconf::Message::OpenRoom(
            sid.clone(),
            "GENERAL".into(),
        )),
        Message::Go(Go::Room {
            sid: ids[1].clone(),
            rid: "r2".into(),
            tmid: Some("t".into()),
        }),
        Message::DeepLinks(vec![
            "rocketchat://room?host=http://localhost:3000&rid=r2&path=channel/r2".to_owned(),
        ]),
        Message::Rail(rustket::workspace::rail::Message::Select(ids[1].clone())),
        Message::Rail(rustket::workspace::rail::Message::Settings),
        Message::ShowAddServer,
        Message::Go(Go::Logout {
            sid: sid.clone(),
            everywhere: false,
        }),
        Message::Go(Go::Nav(rustket::component::Nav::Account)),
        Message::CloseRequested(iced::window::Id::unique()),
    ];
    for m in exits {
        dirty_admin(&mut app, &sid);
        let label = format!("{m:?}");
        let _ = app.update(m);
        assert!(asked(&app), "left the admin page without asking: {label}");
        assert!(
            app.servers[&sid].credentials.is_some(),
            "acted before asking: {label}"
        );
        assert_eq!(app.active.as_ref(), Some(&sid), "{label}");
    }
}

#[test]
fn discard_replays_the_held_back_navigation() {
    let mut ws = admin_ws();
    let mut st = settings::State::new();
    with_cx(&mut ws, |cx| {
        let _ = st.update(settings::Msg::Loaded(Ok(defs())), cx);
        let _ = st.update(settings::Msg::OpenGroup("Accounts".into()), cx);
        let _ = st.update(settings::Msg::Set("A_bool".into(), json!(false)), cx);
    });
    let mut a = admin::Admin::new();
    a.page = admin::Page::Settings(Box::new(st));
    let go = Message::Go(Go::Nav(rustket::component::Nav::Account));
    assert!(!a.ask_leave(go));
    assert!(a.take_leave().is_none(), "not before the answer");
    with_cx(&mut ws, |cx| {
        let _ = a.update(admin::Message::Confirmed(true), cx.ctx);
    });
    assert!(!a.is_dirty());
    assert!(matches!(
        a.take_leave(),
        Some(Message::Go(Go::Nav(rustket::component::Nav::Account)))
    ));
    // Clean now: leaving needs no question.
    assert!(a.ask_leave(Message::ShowAddServer));
}

#[tokio::test]
async fn forced_logout_shows_the_login_of_the_open_page() {
    let (mut app, ids) = app_with(&["One"]);
    let sid = ids[0].clone();
    for page in 0..3 {
        app.servers.get_mut(&sid).unwrap().credentials = Some(rocketchat::Credentials {
            user_id: "me".into(),
            auth_token: "tok".into(),
        });
        app.screen = match page {
            0 => Screen::Account(
                sid.clone(),
                Box::new(rustket::screens::account::Account::new()),
            ),
            1 => Screen::Directory(
                sid.clone(),
                Box::new(rustket::screens::directory::Directory::new()),
            ),
            _ => {
                dirty_admin(&mut app, &sid);
                std::mem::replace(&mut app.screen, Screen::Loading)
            }
        };
        let g = app.servers[&sid].generation;
        let _ = app.update(Message::Server(
            sid.clone(),
            g,
            ServerEvent::Event(Box::new(SessionEvent::ForceLogout { session_id: None })),
        ));
        assert!(app.servers[&sid].credentials.is_none());
        assert!(app.servers[&sid].login.is_some());
        assert!(matches!(app.screen, Screen::Server), "page {page} stays up");
        // The login screen of the visible server explains it: no toast.
        assert!(app.toasts.is_empty());
    }
}

#[tokio::test]
async fn user_logout_from_the_account_page_shows_the_login() {
    let (mut app, ids) = app_with(&["One"]);
    let sid = ids[0].clone();
    app.screen = Screen::Account(
        sid.clone(),
        Box::new(rustket::screens::account::Account::new()),
    );
    app.modals.push(Modal::QuickSwitcher(
        sid.clone(),
        Box::new(rustket::dialogs::quick_switcher::QuickSwitcher::new()),
    ));
    let _ = app.update(Message::Go(Go::Logout {
        sid: sid.clone(),
        everywhere: false,
    }));
    assert!(matches!(app.screen, Screen::Server));
    assert!(app.modals.is_empty());
    assert!(app.servers[&sid].login.is_some());
}

#[tokio::test]
async fn background_server_logout_is_announced() {
    let (mut app, ids) = app_with(&["One", "Two"]);
    let bg = ids[1].clone();
    let g = app.servers[&bg].generation;
    let _ = app.update(Message::Server(
        bg.clone(),
        g,
        ServerEvent::Event(Box::new(SessionEvent::ForceLogout { session_id: None })),
    ));
    assert_eq!(app.active.as_ref(), Some(&ids[0]));
    assert_eq!(app.toasts.len(), 1);
    let t = &app.toasts[0];
    assert!(t.title.contains("Two"), "names the server: {}", t.title);
    assert!(!app.servers[&bg].is_logged_in());
}

#[tokio::test]
async fn late_result_of_a_closed_dialog_skips_the_next_one() {
    use rustket::dialogs::create_room::{self, Kind};
    let (mut app, ids) = app_with(&["One"]);
    let sid = ids[0].clone();
    let nav = |k| Message::Go(Go::Nav(rustket::component::Nav::CreateRoom(k)));
    // Dialog A submits, then is cancelled while busy.
    let _ = app.update(nav(Kind::Channel));
    let a_instance = 1;
    app.modals.clear();
    // Dialog B opens.
    let _ = app.update(nav(Kind::Group));
    let failure = rustket::error::Failure::from(rocketchat::Error::Timeout("create"));
    let _ = app.update(Message::Dialog(
        a_instance,
        Box::new(Message::CreateRoom(
            sid.clone(),
            create_room::Message::Created(Box::new(Err(failure))),
        )),
    ));
    let open = app
        .modals
        .iter()
        .filter(|m| matches!(m, Modal::CreateRoom(..)))
        .count();
    assert_eq!(open, 1, "the new dialog stays open");
    match app.modals.last() {
        Some(Modal::CreateRoom(_, c)) => assert!(c.error.is_none(), "A's error in B"),
        _ => panic!("dialog expected"),
    }
}

/// The SAML metadata answer of a page that was left never lands in the
/// group page open when it arrives.
#[test]
fn saml_metadata_answer_stays_with_its_page() {
    let mut ws = admin_ws();
    let mut st = settings::State::new();
    let late = with_cx(&mut ws, |cx| {
        let _ = st.update(settings::Msg::Loaded(Ok(defs())), cx);
        let _ = st.update(settings::Msg::OpenGroup("SAML".into()), cx);
        let instance = st.group.as_ref().unwrap().instance;
        let _ = st.update(settings::Msg::Saml, cx);
        let _ = st.update(
            settings::Msg::DialogText("https://idp.example/metadata.xml".into()),
            cx,
        );
        let _ = st.update(settings::Msg::SamlNow, cx);
        let _ = st.update(settings::Msg::Back, cx);
        let _ = st.update(settings::Msg::OpenGroup("Accounts".into()), cx);
        let meta = SamlMetadata {
            entry_point: Some("https://other.example/sso".into()),
            cert: Some("NEW-CERT".into()),
            ..SamlMetadata::default()
        };
        let _ = st.update(settings::Msg::SamlParsed(instance, Ok(meta.clone())), cx);
        meta
    });
    assert_eq!(st.group.as_ref().unwrap().page.group, "Accounts");
    assert!(!st.is_dirty());
    // On its own page the answer is applied.
    with_cx(&mut ws, |cx| {
        let _ = st.update(settings::Msg::Back, cx);
        let _ = st.update(settings::Msg::OpenGroup("SAML".into()), cx);
        let instance = st.group.as_ref().unwrap().instance;
        let _ = st.update(settings::Msg::SamlParsed(instance, Ok(late)), cx);
    });
    let g = st.group.as_ref().unwrap();
    let ids: Vec<String> = g
        .session
        .change_set(&st.store)
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(
        ids,
        [
            "SAML_Custom_Default_entry_point",
            "SAML_Custom_Default_cert"
        ]
    );
}

fn bob(roles: &[&str], name: &str) -> AdminUser {
    AdminUser {
        id: "u1".into(),
        username: Some("bob".into()),
        name: Some(name.into()),
        emails: vec![Email {
            address: "bob@x.io".into(),
            verified: true,
        }],
        roles: roles.iter().map(|r| (*r).to_owned()).collect(),
        ..AdminUser::default()
    }
}

/// A refresh while the edit form is open rebases untouched fields: a role
/// granted meanwhile is not revoked by saving another field.
#[test]
fn user_form_refresh_keeps_a_concurrent_role_change() {
    let mut ws = admin_ws();
    let mut st = users::State::new();
    with_cx(&mut ws, |cx| {
        let _ = st.update(
            users::Msg::Info("u1".into(), true, Ok(bob(&["user"], "Bob"))),
            cx,
        );
        let _ = st.update(users::Msg::ToggleAdmin("bob".into(), true), cx);
        let _ = st.update(users::Msg::Edit, cx);
        let _ = st.update(users::Msg::Field(users::F::Name, "Robert".into()), cx);
        let _ = st.update(
            users::Msg::Info("u1".into(), false, Ok(bob(&["user", "admin"], "Bobby"))),
            cx,
        );
        let _ = st.update(users::Msg::Field(users::F::Bio, "hello".into()), cx);
    });
    let Some(users::Dialog::Form(f)) = &st.dialog else {
        panic!("form expected");
    };
    let up = f.update();
    assert_eq!(up.bio.as_deref(), Some("hello"));
    assert_eq!(up.roles, None);
    // A field the admin typed in stays as typed.
    assert_eq!(up.name.as_deref(), Some("Robert"));
}

/// A late `oauth-apps.update` answer never retargets another open form.
#[test]
fn oauth_app_late_save_keeps_the_other_form() {
    use rocketchat::models::admin::OAuthApp;
    use rustket::screens::admin::oauth;
    let app = |id: &str, name: &str| OAuthApp {
        id: id.into(),
        name: name.into(),
        active: true,
        redirect_uri: format!("https://{name}.example/cb"),
        client_id: Some(format!("{id}-client")),
        ..OAuthApp::default()
    };
    let mut ws = admin_ws();
    let mut st = oauth::State::new();
    with_cx(&mut ws, |cx| {
        let _ = st.update(
            oauth::Msg::Loaded(Ok(vec![app("A", "alpha"), app("B", "beta")])),
            cx,
        );
        let _ = st.update(oauth::Msg::Open("A".into()), cx);
        let token = st.form.as_ref().unwrap().token;
        let _ = st.update(oauth::Msg::Save, cx);
        let _ = st.update(oauth::Msg::CloseDialog, cx);
        let _ = st.update(oauth::Msg::Open("B".into()), cx);
        let _ = st.update(oauth::Msg::Saved(token, Ok(app("A", "alpha"))), cx);
    });
    let f = st.form.as_ref().unwrap();
    assert_eq!(f.name, "beta");
    assert_eq!(f.app.as_ref().map(|a| a.id.as_str()), Some("B"));
}

/// A remote settings change re-syncs untouched editors at once: an edit
/// typed before the delta starts from the new text.
#[test]
fn remote_change_reaches_open_editors_at_once() {
    use iced::widget::text_editor::{Action, Edit};
    use rocketchat::models::Setting;
    use rocketchat::session::ChangeKind;
    let mut ws = admin_ws();
    let mut st = settings::State::new();
    let defs: Vec<SettingDef> = serde_json::from_value(json!([
        {"_id": "Layout", "type": "group"},
        {"_id": "Layout_Custom_CSS", "type": "code", "code": "text/css", "multiline": true,
         "group": "Layout", "value": "a{}", "packageValue": "", "sorter": 1},
    ]))
    .unwrap();
    with_cx(&mut ws, |cx| {
        let _ = st.update(settings::Msg::Loaded(Ok(defs)), cx);
        let _ = st.update(settings::Msg::OpenGroup("Layout".into()), cx);
    });
    st.on_setting_event(
        ChangeKind::Updated,
        &Setting {
            id: "Layout_Custom_CSS".into(),
            value: json!("b{color:red}"),
            ..Setting::default()
        },
    );
    with_cx(&mut ws, |cx| {
        let _ = st.update(
            settings::Msg::Editor("Layout_Custom_CSS".into(), Action::Edit(Edit::Insert('x'))),
            cx,
        );
    });
    let g = st.group.as_ref().unwrap();
    let cs = g.session.change_set(&st.store);
    assert_eq!(cs.len(), 1);
    assert_eq!(cs[0].value, json!("xb{color:red}"));
}

struct NoClip;
impl iced::advanced::Clipboard for NoClip {
    fn read(&self, _: iced::advanced::clipboard::Kind) -> Option<String> {
        None
    }
    fn write(&mut self, _: iced::advanced::clipboard::Kind, _: String) {}
}

/// Arrow keys (typed elsewhere) and Ctrl+wheel never move a range setting's
/// slider the pointer merely rests over.
#[tokio::test]
async fn keys_over_a_range_slider_do_not_edit_it() {
    use iced::keyboard::{Key, Location, Modifiers, key};
    use iced::{Pixels, Point, Size, mouse};
    use iced_runtime::user_interface::{Cache, UserInterface};
    let (mut app, ids) = app_with(&["One"]);
    let sid = ids[0].clone();
    let defs: Vec<SettingDef> = serde_json::from_value(json!([
        {"_id": "Accounts", "type": "group", "i18nLabel": "Accounts"},
        {"_id": "A_name", "type": "string", "group": "Accounts", "value": "x", "packageValue": "x", "sorter": 1},
        {"_id": "A_volume", "type": "range", "group": "Accounts", "value": 50, "packageValue": 50,
         "minValue": 0, "maxValue": 100, "sorter": 2},
    ]))
    .unwrap();
    let mut ws = admin_ws();
    let mut st = settings::State::new();
    with_cx(&mut ws, |cx| {
        let _ = st.update(settings::Msg::Loaded(Ok(defs)), cx);
        let _ = st.update(settings::Msg::OpenGroup("Accounts".into()), cx);
    });
    let mut a = admin::Admin::new();
    a.page = admin::Page::Settings(Box::new(st));
    a.section = Some(rustket_core::admin::AdminSection::Settings);
    app.screen = Screen::Admin(sid.clone(), Box::new(a));

    let size = Size::new(1280.0, 800.0);
    let mut renderer = iced::Renderer::Secondary(iced_tiny_skia::Renderer::new(
        rustket_ui::theme::DEFAULT_FONT,
        Pixels(rustket_ui::theme::TEXT_BODY),
    ));
    let mut ui = UserInterface::build(app.view(), size, Cache::default(), &mut renderer);
    let up = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
        key: Key::Named(key::Named::ArrowUp),
        modified_key: Key::Named(key::Named::ArrowUp),
        physical_key: key::Physical::Code(key::Code::ArrowUp),
        location: Location::Standard,
        modifiers: Modifiers::empty(),
        text: None,
        repeat: false,
    });
    let ctrl = iced::Event::Keyboard(iced::keyboard::Event::ModifiersChanged(Modifiers::CTRL));
    let wheel = iced::Event::Mouse(mouse::Event::WheelScrolled {
        delta: mouse::ScrollDelta::Lines { x: 0.0, y: 1.0 },
    });
    let mut hits = Vec::new();
    let mut presses = 0;
    for y in (0..800).step_by(6) {
        for x in (0..1280).step_by(20) {
            let cursor = mouse::Cursor::Available(Point::new(x as f32, y as f32));
            let mut msgs = Vec::new();
            let events = [up.clone(), ctrl.clone(), wheel.clone()];
            let _ = ui.update(&events, cursor, &mut renderer, &mut NoClip, &mut msgs);
            hits.extend(
                msgs.into_iter()
                    .map(|m| format!("{m:?}"))
                    .filter(|s| s.contains("A_volume")),
            );
            // A click still works (the slider is there).
            let press = [
                iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)),
                iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)),
            ];
            let mut msgs = Vec::new();
            let _ = ui.update(&press, cursor, &mut renderer, &mut NoClip, &mut msgs);
            presses += msgs
                .iter()
                .filter(|m| format!("{m:?}").contains("A_volume"))
                .count();
        }
    }
    assert!(hits.is_empty(), "edited by keys/wheel: {:?}", hits.first());
    assert!(presses > 0, "the slider takes clicks");
}
