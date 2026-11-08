//! Administration screen shots: a second app instance signs in as `admin`
//! and renders the overview, workspace info, users (list, details, edit
//! form), the settings editor (group list and the Accounts page with changed
//! fields — never saved), the permission matrix, rooms and the outgoing
//! webhook editor (never saved). Read-only on the shared server; the login
//! token is revoked at the end.
//!
//! Runs only with `RUSTKET_SNAPSHOT_ONLY=admin` (no shared fixture is set up).

use std::path::Path;
use std::time::Duration;

use rocketchat::ServerUrl;
use rocketchat::models::admin::IntegrationType;
use rustket::app::{App, Flags, Screen, SecretsMode};
use rustket::message::Message;
use rustket::screens::admin::{self, Page};
use rustket::screens::{add_server, login};
use rustket_core::config::Theme;
use serde_json::json;

use super::{Driver, active_login};

fn both(d: &mut Driver, out: &Path, name: &str) {
    d.app.config.ui.theme = Theme::Light;
    d.shoot(out, &format!("{name}-light"));
    d.app.config.ui.theme = Theme::Dark;
    d.shoot(out, &format!("{name}-dark"));
}

fn both_tall(d: &mut Driver, out: &Path, name: &str, h: f32) {
    let size = iced::Size::new(1280.0, h);
    d.app.config.ui.theme = Theme::Light;
    d.shoot_at(out, &format!("{name}-light"), size);
    d.app.config.ui.theme = Theme::Dark;
    d.shoot_at(out, &format!("{name}-dark"), size);
}

fn adm(app: &App) -> Option<&admin::Admin> {
    match &app.screen {
        Screen::Admin(_, a) => Some(a),
        _ => None,
    }
}

/// Renders the admin shots only for `RUSTKET_SNAPSHOT_ONLY=admin` (before
/// and instead of the shared fixture setup); returns whether it ran.
pub async fn admin_shots(server: &ServerUrl, out: &Path) -> bool {
    if std::env::var("RUSTKET_SNAPSHOT_ONLY").as_deref() != Ok("admin") {
        return false;
    }
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
    d.send(Message::AddServer(add_server::Message::Submit));
    d.pump_until("login screen", Duration::from_secs(20), |a| {
        active_login(a).is_some_and(|l| l.meta.is_some())
    })
    .await;
    let sid = d.app.active.clone().expect("active server");
    d.send(Message::Login(
        sid.clone(),
        login::Message::User("admin".into()),
    ));
    d.send(Message::Login(
        sid.clone(),
        login::Message::Password("adminpass".into()),
    ));
    d.send(Message::Login(sid.clone(), login::Message::Submit));
    d.pump_until("admin workspace", Duration::from_secs(40), |a| {
        a.servers
            .get(a.active.as_ref().expect("active"))
            .is_some_and(|s| s.login.is_none() && s.workspace.last_sync().is_some())
    })
    .await;
    d.settle(Duration::from_secs(1)).await;

    // Entry in the status menu.
    {
        use rustket::workspace::status_menu::Message as Sm;
        d.send(Message::StatusMenu(sid.clone(), Sm::Toggle(true)));
        d.settle(Duration::from_millis(300)).await;
        d.app.config.ui.theme = Theme::Light;
        d.shoot(out, "admin-00-status-menu-light");
        d.send(Message::StatusMenu(sid.clone(), Sm::Admin));
    }
    d.pump_until("admin screen", Duration::from_secs(20), |a| {
        adm(a).is_some_and(|x| x.license.is_some() && x.i18n_loaded)
    })
    .await;
    both(&mut d, out, "admin-01-home");

    let am = |m| Message::Admin(sid.clone(), m);
    let select = |s| Message::Admin(sid.clone(), admin::Message::Select(Some(s)));
    use rustket_core::admin::AdminSection as S;

    // Workspace info.
    d.send(select(S::Workspace));
    d.pump_until("statistics", Duration::from_secs(20), |a| {
        matches!(adm(a).map(|x| &x.page), Some(Page::Workspace(p)) if p.stats.is_some() && p.info.is_some())
    })
    .await;
    d.settle(Duration::from_millis(500)).await;
    both(&mut d, out, "admin-02-workspace");

    // Users: list, details of alice, edit form (not saved).
    d.send(select(S::Users));
    d.pump_until("users", Duration::from_secs(20), |a| {
        matches!(adm(a).map(|x| &x.page), Some(Page::Users(p)) if p.list.loaded && !p.list.loading)
    })
    .await;
    both(&mut d, out, "admin-03-users");
    let alice = {
        let rest = super::admin(server).await;
        let v: serde_json::Value = rest
            .get(
                "users.info",
                &rocketchat::rest::Query::new().push("username", "alice"),
            )
            .await
            .expect("users.info");
        let _ = rest.logout().await;
        v["user"]["_id"].as_str().expect("alice id").to_owned()
    };
    d.send(am(admin::Message::Users(admin::users::Msg::Open(alice))));
    d.pump_until(
        "user details",
        Duration::from_secs(15),
        |a| matches!(adm(a).map(|x| &x.page), Some(Page::Users(p)) if p.dialog.is_some()),
    )
    .await;
    both(&mut d, out, "admin-04-user-details");
    d.send(am(admin::Message::Users(admin::users::Msg::Edit)));
    d.send(am(admin::Message::Users(admin::users::Msg::Field(
        admin::users::F::Nickname,
        "Snapshot (not saved)".into(),
    ))));
    both_tall(&mut d, out, "admin-05-user-edit", 1000.0);
    d.send(am(admin::Message::Users(admin::users::Msg::CloseDialog)));

    // Rooms.
    d.send(select(S::Rooms));
    d.pump_until("rooms", Duration::from_secs(20), |a| {
        matches!(adm(a).map(|x| &x.page), Some(Page::Rooms(p)) if p.list.loaded && !p.list.loading)
    })
    .await;
    both(&mut d, out, "admin-06-rooms");

    // Settings: group list, then Accounts with changed fields (never saved).
    d.send(select(S::Settings));
    d.pump_until(
        "settings",
        Duration::from_secs(30),
        |a| matches!(adm(a).map(|x| &x.page), Some(Page::Settings(p)) if p.loaded),
    )
    .await;
    both(&mut d, out, "admin-07-settings");
    let sm = |m| Message::Admin(sid.clone(), admin::Message::Settings(m));
    use admin::settings::Msg as St;
    d.send(sm(St::OpenGroup("Accounts".into())));
    // Change a toggle, a number and a select locally to show the changed state.
    d.send(sm(St::Set(
        "Accounts_AllowUserProfileChange".into(),
        json!(false),
    )));
    d.send(sm(St::Text("Accounts_LoginExpiration".into(), "30".into())));
    d.settle(Duration::from_millis(500)).await;
    both_tall(&mut d, out, "admin-08-settings-accounts", 2400.0);
    // An invalid number: inline error and the footer hint.
    d.send(sm(St::Text(
        "Accounts_LoginExpiration".into(),
        "abc".into(),
    )));
    d.settle(Duration::from_millis(300)).await;
    d.app.config.ui.theme = Theme::Light;
    d.shoot_at(
        out,
        "admin-08b-settings-invalid-light",
        iced::Size::new(1280.0, 2400.0),
    );
    // A tabbed group (LDAP) for the tab bar and the header buttons.
    d.send(sm(St::Cancel));
    d.send(sm(St::OpenGroup("LDAP".into())));
    both(&mut d, out, "admin-09-settings-ldap");
    d.send(sm(St::Back));
    assert!(
        matches!(adm(&d.app).map(|x| &x.page), Some(Page::Settings(p)) if p.group.is_none()),
        "nothing left unsaved"
    );

    // Permission matrix.
    d.send(select(S::Permissions));
    d.pump_until(
        "permissions",
        Duration::from_secs(20),
        |a| matches!(adm(a).map(|x| &x.page), Some(Page::Permissions(p)) if p.loaded),
    )
    .await;
    both(&mut d, out, "admin-10-permissions");

    // Integrations: list and the outgoing webhook editor (not saved).
    d.send(select(S::Integrations));
    d.pump_until("integrations", Duration::from_secs(20), |a| {
        matches!(adm(a).map(|x| &x.page), Some(Page::Integrations(p)) if p.list.loaded && !p.list.loading)
    })
    .await;
    both(&mut d, out, "admin-11-integrations");
    use admin::integrations::{F, Msg as Im};
    let im = |m| Message::Admin(sid.clone(), admin::Message::Integrations(m));
    d.send(im(Im::New(IntegrationType::Outgoing)));
    d.send(im(Im::Field(F::Name, "Deploy notifier".into())));
    d.send(im(Im::Field(F::Channel, "#general".into())));
    d.send(im(Im::Field(F::TriggerWords, "deploy, release".into())));
    both_tall(&mut d, out, "admin-12-integration-editor", 1700.0);
    d.send(im(Im::Back));

    // Sign out: the admin login token is revoked.
    d.send(Message::Admin(sid.clone(), admin::Message::Close));
    d.send(Message::Sidebar(
        sid.clone(),
        rustket::workspace::sidebar::Message::Status(
            rustket::workspace::status_menu::Message::Logout,
        ),
    ));
    d.pump_until("logged out", Duration::from_secs(20), |a| {
        a.servers[&sid].login.is_some()
    })
    .await;
    true
}
