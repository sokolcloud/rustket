//! Admin models from fixtures captured on the dev server (docs/admin.md), and admin
//! REST envelopes against in-process HTTP servers.

#![allow(clippy::unwrap_used)]

mod common;

use common::{body_json, http_response, json_ok, serve};
use rocketchat::auth::TwoFactorCode;
use rocketchat::models::admin::*;
use rocketchat::models::{Paginated, Role, UpdatedSince};
use rocketchat::rest::admin::{Page, Sort};
use rocketchat::{Credentials, Error, RestClient, ServerUrl, TlsConfig, TwoFactorMethod};
use serde_json::{Value, json};

fn fixture(name: &str) -> Value {
    let path = format!("{}/tests/fixtures/admin/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
}

fn client(port: u16) -> RestClient {
    let tls = TlsConfig::new().build().unwrap();
    let c = RestClient::new(
        ServerUrl::parse(&format!("http://127.0.0.1:{port}")).unwrap(),
        &tls,
    )
    .unwrap();
    c.set_credentials(Some(Credentials::new("UID", "TOKEN")));
    c
}

#[test]
fn private_settings_fixture() {
    let raw = fixture("private_settings.json");
    let n = raw.as_array().unwrap().len();
    let defs = parse_private_settings(raw);
    assert_eq!(defs.len(), n);
    let get = |id: &str| defs.iter().find(|d| d.id == id).unwrap();
    let ldap = get("LDAP_Enable");
    assert_eq!(ldap.kind, SettingType::Boolean);
    assert_eq!(ldap.group.as_deref(), Some("LDAP"));
    assert!(ldap.updated_at.is_some());
    let host = get("LDAP_Host");
    assert_eq!(
        host.enable_query.as_ref().unwrap().filters,
        vec![json!({"_id": "LDAP_Enable", "value": true})]
    );
    assert!(defs.iter().any(|d| d.kind == SettingType::Group));
    assert!(
        defs.iter()
            .any(|d| d.kind == SettingType::Select && !d.values.is_empty())
    );
    assert!(defs.iter().any(|d| d.kind == SettingType::Timespan));
    assert!(
        defs.iter()
            .any(|d| d.kind == SettingType::Asset && d.asset.is_some())
    );
    assert!(defs.iter().any(|d| d.enterprise && !d.modules.is_empty()));
    let pw = defs
        .iter()
        .find(|d| d.kind == SettingType::Password)
        .unwrap();
    assert!(pw.is_sensitive());
    // Custom OAuth secrets are plain strings without `secret: true`.
    let kc = defs
        .iter()
        .find(|d| d.id == "Accounts_OAuth_Custom-Keycloak-secret")
        .unwrap();
    assert!(!kc.secret && kc.kind == SettingType::String);
    assert!(kc.is_sensitive());
    assert!(
        !defs
            .iter()
            .find(|d| d.id == "LDAP_Enable")
            .unwrap()
            .is_sensitive()
    );
    // `Debug` never prints values.
    let mut probe = kc.clone();
    probe.value = Some(json!("probe-secret-value"));
    assert!(!format!("{probe:?}").contains("probe-secret-value"));
    // Unknown types and string-or-JSON queries.
    let odd: SettingDef = serde_json::from_value(json!({
        "_id": "x", "type": "fancy", "enableQuery": {"_id": "a", "value": 1},
        "displayQuery": "not json", "group": null, "sorter": 3.0
    }))
    .unwrap();
    assert_eq!(odd.kind, SettingType::Unknown("fancy".into()));
    assert_eq!(odd.enable_query.unwrap().filters.len(), 1);
    assert!(odd.display_query.is_none());
    assert_eq!(odd.sorter, 3);
}

#[test]
fn delta_and_change_bodies() {
    let d: UpdatedSince<SettingDef> = serde_json::from_value(json!({
        "update": [{"_id": "A", "type": "int", "value": 3, "_updatedAt": {"$date": 1_700_000_000_000_i64}}],
        "remove": [{"_id": "B", "_deletedAt": {"$date": 1_700_000_000_000_i64}}]
    }))
    .unwrap();
    assert_eq!(d.update[0].value, Some(json!(3)));
    assert_eq!(d.remove[0].id, "B");
    let c = SettingChange {
        id: "Site_Name".into(),
        value: json!("secret-ish"),
        editor: None,
    };
    assert_eq!(
        serde_json::to_value(&c).unwrap(),
        json!({"_id": "Site_Name", "value": "secret-ish"})
    );
    assert!(!format!("{c:?}").contains("secret-ish"));
}

#[test]
fn statistics_fixture() {
    let s: Statistics = serde_json::from_value(fixture("statistics.json")).unwrap();
    assert_eq!(s.version.as_deref(), Some("8.9.1"));
    assert!(s.total_users.is_some());
    assert!(s.process.as_ref().unwrap().node_version.is_some());
    assert!(s.migration.as_ref().unwrap().version.is_some());
    assert!(s.updated_at.is_some());
    assert!(s.extra.contains_key("os"));
    // Re-serialises everything for "Download info".
    let back = serde_json::to_value(&s).unwrap();
    assert_eq!(back, fixture("statistics.json"));
    // A fresh snapshot has `_updatedAt: null`.
    let mut raw = fixture("statistics.json");
    raw["_updatedAt"] = Value::Null;
    raw["uploadsTotalSize"] = json!(97.0);
    let fresh: Statistics = serde_json::from_value(raw).unwrap();
    assert!(fresh.updated_at.is_none());
    assert_eq!(fresh.uploads_total_size, Some(97));
}

#[test]
fn workspace_fixtures() {
    let v = fixture("instances_get.json");
    let inst: Vec<Instance> = serde_json::from_value(v["instances"].clone()).unwrap();
    assert_eq!(inst[0].current_status.local, Some(true));
    assert!(inst[0].instance_record.created_at.is_some());
    let lic: LicenseInfo =
        serde_json::from_value(fixture("licenses_info.json")["license"].clone()).unwrap();
    assert!(!lic.has_valid_license);
    assert!(lic.active_modules.is_empty());
    assert_eq!(lic.limits["marketplaceApps"].max, 5);
    assert!(lic.is_prevented("privateApps"));
    assert!(!lic.is_prevented("activeUsers"));
    let m: MaxActiveUsers =
        serde_json::from_value(fixture("licenses_max_active_users.json")).unwrap();
    assert!(m.active_users.is_some());
}

#[test]
fn users_rooms_roles_fixtures() {
    let v = fixture("users_list_by_status.json");
    let users: Vec<AdminUser> = serde_json::from_value(v["users"].clone()).unwrap();
    let admin = users
        .iter()
        .find(|u| u.username.as_deref() == Some("admin"))
        .unwrap();
    assert_eq!(admin.roles, vec!["admin"]);
    assert!(admin.is_active());
    assert!(admin.last_login.is_some());
    assert!(admin.primary_email().is_some());

    let v = fixture("rooms_admin_rooms.json");
    let rooms: Vec<AdminRoom> = serde_json::from_value(v["rooms"].clone()).unwrap();
    let dm = rooms.iter().find(|r| r.name.is_none()).unwrap();
    assert_eq!(dm.display_name(), dm.usernames.join(" x "));
    let general: AdminRoom = serde_json::from_value(fixture("rooms_admin_get_room.json")).unwrap();
    assert_eq!(general.id, "GENERAL");
    assert_eq!(general.default, Some(true));
    assert_eq!(general.display_name(), "general");

    let roles: Vec<Role> =
        serde_json::from_value(fixture("roles_list.json")["roles"].clone()).unwrap();
    let owner = roles.iter().find(|r| r.id == "owner").unwrap();
    let p = RoleParams::update_of(owner);
    let body = serde_json::to_value(&p).unwrap();
    assert_eq!(body["scope"], "Subscriptions");
    assert_eq!(body["name"], owner.name);
    assert!(body.get("description").is_some());

    let v = fixture("roles_get_users_in_role.json");
    let u: Vec<UserInRole> = serde_json::from_value(v["users"].clone()).unwrap();
    assert_eq!(u[0].username.as_deref(), Some("admin"));

    let perms: UpdatedSince<PermissionDef> =
        serde_json::from_value(fixture("permissions_list_all.json")).unwrap();
    let setting_perm = perms
        .update
        .iter()
        .find(|p| p.is_setting_permission())
        .unwrap();
    assert!(setting_perm.setting_id.is_some());
    assert_eq!(setting_perm.group.as_deref(), Some("LDAP"));
    assert!(setting_perm.group_permission_id.as_deref() == Some("change-setting-LDAP"));
}

#[test]
fn error_bodies() {
    let body = serde_json::to_vec(&fixture("error_totp_required_password.json")).unwrap();
    match Error::from_response(400, None, &body) {
        Error::TwoFactorRequired(c) => {
            assert_eq!(c.method, TwoFactorMethod::Password);
            assert_eq!(c.code_generated, Some(false));
        }
        e => panic!("{e:?}"),
    }
    let body = serde_json::to_vec(&fixture("error_user_last_owner.json")).unwrap();
    let e = Error::from_response(400, None, &body);
    let d = LastOwnerDetails::from_error(&e).unwrap();
    assert_eq!(d.should_change_owner, vec!["rkadm-chk-room-7383"]);
    assert!(d.should_be_removed.is_empty());
    assert!(LastOwnerDetails::from_error(&Error::Protocol("x".into())).is_none());
}

#[test]
fn integration_models() {
    let i: Integration = serde_json::from_value(json!({
        "_id": "I1", "type": "webhook-incoming", "name": "n", "enabled": true,
        "channel": ["#room"], "username": "rocket.cat", "token": "SECRETTOKEN",
        "scriptEnabled": true, "script": "SECRETSCRIPT", "_createdAt": "2026-11-08T00:00:00.000Z",
        "_createdBy": {"_id": "u", "username": "admin"}
    }))
    .unwrap();
    assert_eq!(i.kind, IntegrationType::Incoming);
    assert_eq!(
        i.webhook_url("http://localhost:3000/").as_deref(),
        Some("http://localhost:3000/hooks/I1/SECRETTOKEN")
    );
    let dbg = format!("{i:?}");
    assert!(!dbg.contains("SECRETTOKEN") && !dbg.contains("SECRETSCRIPT"));
    let o: Integration = serde_json::from_value(json!({
        "_id": "O1", "type": "webhook-outgoing", "event": "sendMessage", "channel": "#a, #b",
        "urls": ["https://x"], "retryDelay": "powers-of-ten", "token": "t"
    }))
    .unwrap();
    assert_eq!(o.channel, vec!["#a", "#b"]);
    assert_eq!(o.event, Some(OutgoingEvent::SendMessage));
    assert_eq!(o.retry_delay, Some(RetryDelay::PowersOfTen));
    let p = IntegrationParams::from_integration(&o);
    let body = serde_json::to_value(&p).unwrap();
    assert_eq!(body["type"], "webhook-outgoing");
    assert_eq!(body["integrationId"], "O1");
    assert_eq!(body["channel"], "#a, #b");
    assert_eq!(body["token"], "t");
    assert_eq!(body["event"], "sendMessage");
    let new = IntegrationParams::outgoing("n", OutgoingEvent::RoomCreated, vec![], "rocket.cat");
    assert!(new.token.as_deref().is_some_and(|t| t.len() >= 24));
    assert!(!OutgoingEvent::RoomCreated.uses_channel());
}

#[tokio::test]
async fn bare_envelopes() {
    let (port, log) = serve(|req| {
        let line = req.lines().next().unwrap_or_default();
        Some(if line.contains("listInvites") {
            json_ok(r#"[{"_id":"5BAhAq","days":7,"maxUses":5,"rid":"R","userId":"U","createdAt":"2026-11-08T00:00:00.000Z","expires":null,"url":"","uses":2,"_updatedAt":"2026-11-08T00:00:00.000Z"}]"#)
        } else if line.contains("removeInvite") || line.contains("oauth-apps.delete") {
            json_ok("true")
        } else if line.contains("oauth-apps.update") {
            json_ok(r#"{"_id":"A","name":"n","active":true,"redirectUri":"x","clientId":"c","clientSecret":"SECRET","success":true}"#)
        } else if line.contains("integrations.update") {
            json_ok(r#"{"integration":{"_id":"I","type":"webhook-outgoing"},"success":true}"#)
        } else {
            http_response("404 Not Found", &[], "")
        })
    })
    .await;
    let c = client(port);
    let inv = c.list_invites().await.unwrap();
    assert_eq!(inv[0].uses_left(), Some(3));
    assert!(inv[0].expires.is_none());
    c.remove_invite("5BAhAq").await.unwrap();
    c.oauth_app_delete("A").await.unwrap();
    let app = c.oauth_app_update("A", "n", true, "x").await.unwrap();
    assert!(!format!("{app:?}").contains("SECRET"));
    let mut p = IntegrationParams::outgoing("n", OutgoingEvent::RoomCreated, vec![], "rocket.cat");
    assert!(c.integration_update(&p).await.is_err());
    p.integration_id = Some("I".into());
    c.integration_update(&p).await.unwrap();
    let log = log.lock().unwrap();
    assert!(
        log.iter()
            .any(|r| r.starts_with("DELETE /api/v1/removeInvite/5BAhAq"))
    );
    assert!(
        log.iter()
            .any(|r| r.starts_with("PUT /api/v1/integrations.update"))
    );
}

#[tokio::test]
async fn two_factor_headers_and_list_params() {
    let (port, log) = serve(|req| {
        let line = req.lines().next().unwrap_or_default();
        Some(if line.contains("users.listByStatus") {
            json_ok(r#"{"users":[{"_id":"u1","username":"a"}],"count":1,"offset":0,"total":3,"success":true}"#)
        } else if req.to_ascii_lowercase().contains("x-2fa-code") {
            json_ok(r#"{"success":true}"#)
        } else {
            http_response(
                "400 Bad Request",
                &[("Content-Type", "application/json")],
                r#"{"success":false,"error":"TOTP Required [totp-required]","errorType":"totp-required","details":{"method":"password","codeGenerated":false,"availableMethods":[]}}"#,
            )
        })
    })
    .await;
    let c = client(port);
    let changes = vec![SettingChange {
        id: "Nonexistent_Setting".into(),
        value: json!("x"),
        editor: None,
    }];
    let challenge = match c.save_settings(&changes, None).await {
        Err(Error::TwoFactorRequired(ch)) => ch,
        other => panic!("{other:?}"),
    };
    assert_eq!(challenge.method, TwoFactorMethod::Password);
    let code = TwoFactorCode::password("adminpass");
    c.save_settings(&changes, Some(&code)).await.unwrap();
    assert!(c.admin_reset_totp("", Some(&code)).await.is_err());

    let page: Paginated<AdminUser> = c
        .admin_users_list(
            &UsersFilter {
                search_term: Some("al".into()),
                roles: vec!["admin".into(), "user".into()],
                ..UsersFilter::deactivated()
            },
            &Page::new(0, 1).sorted(Sort::asc("name")),
        )
        .await
        .unwrap();
    assert!(page.has_more());
    let log = log.lock().unwrap();
    let saved = log
        .iter()
        .find(|r| r.to_ascii_lowercase().contains("x-2fa-code"))
        .unwrap();
    let lower = saved.to_ascii_lowercase();
    assert!(lower.contains(&format!(
        "x-2fa-code: {}",
        rocketchat::auth::sha256_hex("adminpass")
    )));
    assert!(lower.contains("x-2fa-method: password"));
    assert_eq!(
        body_json(saved),
        json!({"settings": [{"_id": "Nonexistent_Setting", "value": "x"}]})
    );
    let list = log
        .iter()
        .find(|r| r.contains("users.listByStatus"))
        .unwrap();
    let first = list.lines().next().unwrap();
    for part in [
        "status=deactivated",
        "inactiveReason%5B%5D=deactivated",
        "inactiveReason%5B%5D=idle_too_long",
        "roles%5B%5D=admin",
        "searchTerm=al",
        "sort=%7B%22name%22%3A1%7D",
        "count=1",
    ] {
        assert!(first.contains(part), "{part}: {first}");
    }
}

#[tokio::test]
async fn users_in_role_page_without_offset_keys() {
    // `roles.getUsersInRole` answers only `{users, total}`.
    let (port, _log) = serve(|_| {
        Some(json_ok(
            r#"{"users":[{"_id":"u5","username":"e"}],"total":5,"success":true}"#,
        ))
    })
    .await;
    let p = client(port)
        .roles_get_users_in_role("user", None, &Page::new(4, 2))
        .await
        .unwrap();
    assert_eq!((p.offset, p.count, p.total, p.raw_len), (4, 1, 5, 1));
    assert!(!p.has_more());
}
