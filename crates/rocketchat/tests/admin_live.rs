//! Live admin tests against the dev server (docs/admin.md §23). Skipped unless
//! `RUSTKET_TEST_SERVER` is set. Read-only calls run as `admin`; writes only touch
//! throwaway `rkadm-*` objects created here, which are deleted in every path.
//! Global settings, permissions and roles are never changed.

#![allow(clippy::unwrap_used)]

use std::future::Future;
use std::panic::AssertUnwindSafe;

use futures::FutureExt;
use rocketchat::auth::TwoFactorCode;
use rocketchat::models::UserStatus;
use rocketchat::models::admin::*;
use rocketchat::rest::admin::ModerationFilter;
use rocketchat::rest::{AdminUpload, CreateRoom, Page, Sort, UserLookup};
use rocketchat::{Error, RcDate, RestClient, ServerUrl, TlsConfig};
use serde_json::json;

fn server() -> Option<ServerUrl> {
    match std::env::var("RUSTKET_TEST_SERVER") {
        Ok(s) if !s.is_empty() => Some(ServerUrl::parse(&s).expect("RUSTKET_TEST_SERVER")),
        _ => {
            eprintln!("RUSTKET_TEST_SERVER not set; skipping live test");
            None
        }
    }
}

async fn admin(server: &ServerUrl) -> RestClient {
    let tls = TlsConfig::new().build().unwrap();
    let c = RestClient::new(server.clone(), &tls).unwrap();
    c.login_password("admin", "adminpass", None).await.unwrap();
    c
}

fn unique(prefix: &str) -> String {
    format!(
        "rkadm-{prefix}-{}-{}",
        chrono::Utc::now().timestamp_millis(),
        rocketchat::ids::random_id_len(4).to_lowercase()
    )
}

fn password_code() -> TwoFactorCode {
    TwoFactorCode::password("adminpass")
}

/// Runs `body`, then `cleanup` even if `body` panicked.
async fn guarded(body: impl Future<Output = ()>, cleanup: impl Future<Output = ()>) {
    let r = AssertUnwindSafe(body).catch_unwind().await;
    cleanup.await;
    if let Err(p) = r {
        std::panic::resume_unwind(p);
    }
}

/// Calls `f` without a code; on a 2FA challenge for the password method retries
/// with the password hash.
async fn with_2fa<T, F, Fut>(f: F) -> rocketchat::Result<T>
where
    F: Fn(Option<TwoFactorCode>) -> Fut,
    Fut: Future<Output = rocketchat::Result<T>>,
{
    match f(None).await {
        Err(Error::TwoFactorRequired(ch)) => {
            assert_eq!(ch.method, rocketchat::TwoFactorMethod::Password);
            f(Some(password_code())).await
        }
        r => r,
    }
}

const PNG_1X1: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
    0x42, 0x60, 0x82,
];

/// A tiny 8-bit mono WAV.
fn wav() -> Vec<u8> {
    let samples = vec![128u8; 800];
    let mut v = Vec::new();
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + samples.len() as u32).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&8000u32.to_le_bytes());
    v.extend_from_slice(&8000u32.to_le_bytes());
    v.extend_from_slice(&1u16.to_le_bytes());
    v.extend_from_slice(&8u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    v.extend_from_slice(&samples);
    v
}

#[tokio::test]
async fn admin_read_only_endpoints() {
    let Some(server) = server() else { return };
    let c = admin(&server).await;
    guarded(
        async {
            let info = c.admin_server_info().await.unwrap();
            assert!(
                info.info.is_some(),
                "authenticated /api/info has build info"
            );

            let st = c.statistics(false).await.unwrap();
            assert!(st.version.is_some());
            assert!(st.total_users.is_some());
            assert!(!st.extra.contains_key("success"));
            let list = c.statistics_list(&Page::new(0, 1)).await.unwrap();
            assert!(list.total >= 1);
            assert!(!c.instances_get().await.unwrap().is_empty());
            let lic = c.licenses_info(true).await.unwrap();
            assert!(lic.limits.contains_key("activeUsers"));
            c.licenses_max_active_users().await.unwrap();
            c.cloud_registration_status().await.unwrap();
            c.smtp_check().await.unwrap();

            let users = c
                .admin_users_list(
                    &UsersFilter::active(),
                    &Page::new(0, 5).sorted(Sort::asc("username")),
                )
                .await
                .unwrap();
            assert!(!users.items.is_empty());
            assert!(users.items.iter().all(AdminUser::is_active));
            let pending = c
                .admin_users_list(&UsersFilter::pending(), &Page::new(0, 1))
                .await
                .unwrap();
            assert!(pending.items.iter().all(|u| !u.is_active()));
            let searched = c
                .admin_users_list(
                    &UsersFilter {
                        search_term: Some("alice".into()),
                        ..UsersFilter::all()
                    },
                    &Page::new(0, 10),
                )
                .await
                .unwrap();
            assert!(
                searched
                    .items
                    .iter()
                    .any(|u| u.username.as_deref() == Some("alice"))
            );
            let me = c
                .admin_user_info(UserLookup::Username("admin"))
                .await
                .unwrap();
            assert!(me.roles.contains(&"admin".to_owned()));

            let rooms = c
                .admin_rooms("", &[], &Page::new(0, 5).sorted(Sort::asc("name")))
                .await
                .unwrap();
            assert!(rooms.total > 0);
            let dms = c
                .admin_rooms("alice", &[AdminRoomType::Direct], &Page::new(0, 5))
                .await
                .unwrap();
            assert!(
                dms.items
                    .iter()
                    .all(|r| r.t == rocketchat::models::RoomType::Direct)
            );
            assert_eq!(c.admin_room("GENERAL").await.unwrap().id, "GENERAL");
            let ac = c.admin_rooms_autocomplete("gen").await.unwrap();
            assert!(ac.iter().any(|r| r.id == "GENERAL"));

            c.list_invites().await.unwrap();
            c.emoji_custom_all("", &Page::new(0, 5)).await.unwrap();
            c.custom_sounds_list("", &Page::new(0, 5)).await.unwrap();
            c.custom_user_status_page("", &Page::new(0, 5))
                .await
                .unwrap();
            c.integrations_list("", None, &Page::new(0, 5))
                .await
                .unwrap();
            c.oauth_apps_list().await.unwrap();
            c.email_inbox_list(&Page::new(0, 5)).await.unwrap();
            assert!(
                c.email_inbox_search("nobody@example.invalid")
                    .await
                    .unwrap()
                    .is_none()
            );
            assert!(matches!(
                c.email_inbox_get("rkadm-missing").await,
                Err(Error::Api { status: 404, .. })
            ));
            c.moderation_reports_by_users(&ModerationFilter::default(), &Page::new(0, 5))
                .await
                .unwrap();
            c.moderation_user_reports(&ModerationFilter::default(), &Page::new(0, 5))
                .await
                .unwrap();
            c.admin_banners_list("web").await.unwrap();

            let perms = c.permissions_list_all_full(None).await.unwrap();
            assert!(perms.update.iter().any(|p| p.is_setting_permission()));
            assert!(perms.update.iter().any(|p| p.id == "view-statistics"));
            let roles = c.roles_list().await.unwrap();
            assert!(roles.iter().any(|r| r.id == "admin"));
            c.roles_sync(RcDate::from_millis(0).unwrap()).await.unwrap();
            let in_role = c
                .roles_get_users_in_role("admin", None, &Page::new(0, 10))
                .await
                .unwrap();
            assert!(
                in_role
                    .items
                    .iter()
                    .any(|u| u.username.as_deref() == Some("admin"))
            );

            // Full private settings decode completely.
            let raw = c
                .method_call_raw("private-settings/get", json!([]), false)
                .await
                .unwrap();
            let n = raw.as_array().unwrap().len();
            let defs = parse_private_settings(raw);
            assert_eq!(defs.len(), n);
            assert!(n > 800, "{n}");
            assert!(
                defs.iter()
                    .all(|d| !matches!(d.kind, SettingType::Unknown(_)))
            );
            let since = c.private_settings_since(RcDate::now()).await.unwrap();
            assert!(since.update.len() < n);
            assert_eq!(c.private_settings().await.unwrap().len(), n);
            c.setting_value("Site_Name").await.unwrap();
            c.settings_lookup("v1/video-conference.providers")
                .await
                .unwrap();

            let en = c.server_translations("en").await.unwrap();
            assert_eq!(
                en.get("Reset_section_settings").map(String::as_str),
                Some("Restore defaults")
            );
            assert!(en.len() > 5000);
            let ru = c.server_translations("ru").await.unwrap();
            assert!(!ru.is_empty() && ru.len() < en.len());
            assert!(c.server_translations("xx").await.is_err());
            assert!(c.server_translations("../x").await.is_err());

            // Error paths that change nothing.
            match c.ldap_test_connection().await {
                Err(e) => assert_eq!(e.api_error_code(), Some("LDAP_disabled")),
                Ok(m) => assert!(!m.is_empty()),
            }
            let mail = MailerRequest {
                from: "admin@example.invalid".into(),
                subject: "rkadm".into(),
                body: "no placeholder".into(),
                dryrun: Some(true),
                query: None,
            };
            assert!(!mail.has_unsubscribe());
            assert_eq!(
                c.mailer_send(&mail).await.unwrap_err().api_error_code(),
                Some("error-missing-unsubscribe-link")
            );
            if !lic.has_module("device-management") {
                let e = c
                    .sessions_list_all(None, &Page::new(0, 1), None)
                    .await
                    .unwrap_err();
                assert_eq!(e.api_error_code(), Some("error-action-not-allowed"));
            }
        },
        async {
            let _ = c.logout().await;
        },
    )
    .await;
}

#[tokio::test]
async fn admin_settings_write_path_without_writing() {
    let Some(server) = server() else { return };
    let c = admin(&server).await;
    let id = format!("Rustket_Nonexistent_{}", rocketchat::ids::random_id_len(6));
    guarded(
        async {
            let changes = vec![SettingChange {
                id: id.clone(),
                value: json!("x"),
                editor: None,
            }];
            // The bulk save asks for 2FA every time, before anything is written.
            match c.save_settings(&changes, None).await {
                Err(Error::TwoFactorRequired(ch)) => {
                    assert_eq!(ch.method, rocketchat::TwoFactorMethod::Password)
                }
                other => panic!("{other:?}"),
            }
            // A wrong code is rejected.
            let wrong = TwoFactorCode::password("not-the-password");
            assert!(matches!(
                c.save_settings(&changes, Some(&wrong)).await,
                Err(Error::TwoFactorInvalid(_))
            ));
            // An unknown id passes the checks and writes nothing.
            c.save_settings(&changes, Some(&password_code()))
                .await
                .unwrap();
            assert!(c.setting_value(&id).await.is_err());
            // Asked again (no "remember" for the bulk save).
            assert!(matches!(
                c.save_settings(&changes, None).await,
                Err(Error::TwoFactorRequired(_))
            ));
            // An empty change set is not sent at all.
            c.save_settings(&[], None).await.unwrap();
        },
        async {
            let _ = c.logout().await;
        },
    )
    .await;
}

#[tokio::test]
async fn admin_user_lifecycle() {
    let Some(server) = server() else { return };
    let c = admin(&server).await;
    let username = unique("u");
    let created = c
        .admin_user_create(&NewUser {
            email: format!("{username}@example.invalid"),
            name: "Rkadm User".into(),
            password: format!("{username}-Pass1!"),
            username: username.clone(),
            join_default_channels: Some(false),
            verified: Some(true),
            ..NewUser::default()
        })
        .await
        .unwrap();
    let uid = created.id.clone();
    guarded(
        async {
            assert_eq!(created.username.as_deref(), Some(username.as_str()));
            let info = c.admin_user_info(UserLookup::Id(&uid)).await.unwrap();
            assert_eq!(info.name.as_deref(), Some("Rkadm User"));

            let upd = UserUpdate {
                name: Some("Rkadm Renamed".into()),
                bio: Some("bio".into()),
                ..UserUpdate::default()
            };
            let u = with_2fa(|tf| {
                let upd = &upd;
                let c = &c;
                let uid = &uid;
                async move { c.admin_user_update(uid, upd, false, tf.as_ref()).await }
            })
            .await
            .unwrap();
            assert_eq!(u.name.as_deref(), Some("Rkadm Renamed"));

            let r = c.admin_set_active_status(&uid, false, false).await.unwrap();
            assert!(!r.active);
            let r = c.admin_set_active_status(&uid, true, false).await.unwrap();
            assert!(r.active);

            let role = c.roles_add_user("bot", &username, None).await.unwrap();
            assert_eq!(role.id, "bot");
            let again = c.roles_add_user("bot", &username, None).await.unwrap_err();
            assert_eq!(again.api_error_code(), Some("error-user-already-in-role"));
            c.roles_remove_user("bot", &username, None).await.unwrap();

            // Every time 2FA: the first call is always challenged.
            assert!(matches!(
                c.admin_reset_totp(&uid, None).await,
                Err(Error::TwoFactorRequired(_))
            ));
            c.admin_reset_totp(&uid, Some(&password_code()))
                .await
                .unwrap();
            assert!(matches!(
                c.admin_reset_e2e_key(&uid, None).await,
                Err(Error::TwoFactorRequired(_))
            ));
            c.admin_reset_e2e_key(&uid, Some(&password_code()))
                .await
                .unwrap();

            c.admin_set_user_avatar(
                &uid,
                AdminUpload::new("a.png", "image/png", PNG_1X1.to_vec()),
            )
            .await
            .unwrap();
            c.users_reset_avatar(&uid).await.unwrap();

            // Moderation: report the user, list it, dismiss.
            c.moderation_report_user(&uid, "rkadm live test")
                .await
                .unwrap();
            let reports = c
                .moderation_user_reports(
                    &ModerationFilter {
                        selector: Some(username.clone()),
                        ..ModerationFilter::default()
                    },
                    &Page::new(0, 10),
                )
                .await
                .unwrap();
            assert!(reports.items.iter().any(|r| r.reported_user.id == uid));
            let by_user = c
                .moderation_user_reports_by_user_id(&uid, &Page::new(0, 10))
                .await
                .unwrap();
            assert!(!by_user.reports.is_empty());
            c.moderation_dismiss_user_reports(&uid, Some("test"), None)
                .await
                .unwrap();
        },
        async {
            c.admin_user_delete(&uid, true).await.unwrap();
            let _ = c.logout().await;
        },
    )
    .await;
}

#[tokio::test]
async fn admin_room_settings_and_invites() {
    let Some(server) = server() else { return };
    let c = admin(&server).await;
    let name = unique("room");
    let room = c
        .channels_create(&CreateRoom::new(name.clone()))
        .await
        .unwrap();
    let rid = room.id.clone();
    guarded(
        async {
            let found = c
                .admin_rooms(&name, &[AdminRoomType::Channel], &Page::new(0, 5))
                .await
                .unwrap();
            assert_eq!(found.items.len(), 1);
            c.admin_save_room_settings(
                &rid,
                &RoomSettingsUpdate {
                    room_topic: Some("rkadm topic".into()),
                    default: Some(false),
                    favorite: Some(FavoriteSetting::default()),
                    ..RoomSettingsUpdate::default()
                },
            )
            .await
            .unwrap();
            let r = c.admin_room(&rid).await.unwrap();
            assert_eq!(r.topic.as_deref(), Some("rkadm topic"));
            c.admin_change_archivation_state(&rid, true).await.unwrap();
            assert_eq!(c.admin_room(&rid).await.unwrap().archived, Some(true));
            c.admin_change_archivation_state(&rid, false).await.unwrap();

            let inv = c.find_or_create_invite(&rid, 1, 5).await.unwrap();
            assert!(!inv.url.is_empty());
            assert_eq!(inv.uses_left(), Some(5));
            let listed = c.list_invites().await.unwrap();
            let stored = listed.iter().find(|i| i.id == inv.id).unwrap();
            assert!(stored.url.is_empty());
            assert!(c.validate_invite_token(&inv.id).await.unwrap());
            c.remove_invite(&inv.id).await.unwrap();
            assert!(!c.validate_invite_token(&inv.id).await.unwrap());

            // Webhooks in the throwaway room.
            let incoming = c
                .integration_create(&IntegrationParams {
                    script_enabled: false,
                    ..IntegrationParams::incoming(&unique("in"), &format!("#{name}"), "rocket.cat")
                })
                .await
                .unwrap();
            let outgoing = c
                .integration_create(&IntegrationParams::outgoing(
                    &unique("out"),
                    OutgoingEvent::RoomCreated,
                    vec!["https://example.invalid/hook".into()],
                    "rocket.cat",
                ))
                .await
                .unwrap();
            let r = async {
                assert!(incoming.webhook_url("http://localhost:3000").is_some());
                let got = c.integration_get(&incoming.id).await.unwrap();
                assert_eq!(got.channel, vec![format!("#{name}")]);
                let mut p = IntegrationParams::from_integration(&got);
                p.alias = Some("rkadm".into());
                assert_eq!(
                    c.integration_update(&p).await.unwrap().alias.as_deref(),
                    Some("rkadm")
                );
                let mut p = IntegrationParams::from_integration(&outgoing);
                p.enabled = false;
                assert!(!c.integration_update(&p).await.unwrap().enabled);
                let h = c
                    .integration_history(&outgoing.id, &Page::new(0, 5))
                    .await
                    .unwrap();
                assert_eq!(h.total, 0);
                c.integration_clear_history(&outgoing.id).await.unwrap();
                assert_eq!(
                    c.integration_replay(&outgoing.id, "missing")
                        .await
                        .unwrap_err()
                        .api_error_code(),
                    Some("error-invalid-integration-history")
                );
                let listed = c
                    .integrations_list(
                        "rkadm-out",
                        Some(IntegrationType::Outgoing),
                        &Page::new(0, 50),
                    )
                    .await
                    .unwrap();
                assert!(listed.items.iter().any(|i| i.id == outgoing.id));
            };
            let res = AssertUnwindSafe(r).catch_unwind().await;
            c.integration_remove(IntegrationType::Incoming, &incoming.id)
                .await
                .unwrap();
            c.integration_remove(IntegrationType::Outgoing, &outgoing.id)
                .await
                .unwrap();
            if let Err(p) = res {
                std::panic::resume_unwind(p);
            }
        },
        async {
            c.rooms_delete(&rid).await.unwrap();
            let _ = c.logout().await;
        },
    )
    .await;
}

#[tokio::test]
async fn admin_emoji_sounds_statuses_oauth() {
    let Some(server) = server() else { return };
    let c = admin(&server).await;
    let emoji = unique("e").replace('-', "_");
    let sound = unique("s");
    let status = unique("st");
    let app = unique("app");
    guarded(
        async {
            c.emoji_custom_create(
                AdminUpload::new("e.png", "image/png", PNG_1X1.to_vec()),
                &emoji,
                "rkadm_alias_a, rkadm alias b",
            )
            .await
            .unwrap();
            let e = c.emoji_custom_all(&emoji, &Page::new(0, 5)).await.unwrap();
            let e = e.items.iter().find(|x| x.name == emoji).unwrap().clone();
            assert_eq!(e.extension, "png");
            assert!(
                e.aliases.contains(&"rkadm_alias_b".to_owned()),
                "{:?}",
                e.aliases
            );
            c.emoji_custom_update(&e.id, &emoji, "", None)
                .await
                .unwrap();
            let e2 = c.emoji_custom_all(&emoji, &Page::new(0, 5)).await.unwrap();
            assert!(e2.items[0].aliases.is_empty());
            let bad = c
                .emoji_custom_update(&e.id, &emoji, "a|b", None)
                .await
                .unwrap_err();
            assert_eq!(
                bad.api_error_code(),
                Some("error-input-is-not-a-valid-field")
            );

            let sid = c
                .custom_sound_create(AdminUpload::new("s.wav", "audio/wav", wav()), &sound)
                .await
                .unwrap();
            assert!(!sid.is_empty());
            let got = c.custom_sound_get(&sid).await.unwrap();
            assert_eq!(got.extension, "wav");
            assert!(got.path().ends_with(".wav"));
            c.custom_sound_update(&sid, &format!("{sound}-x"), None)
                .await
                .unwrap();
            assert_eq!(
                c.custom_sound_get(&sid).await.unwrap().name,
                format!("{sound}-x")
            );
            let rejected = c
                .custom_sound_create(
                    AdminUpload::new("s.bin", "audio/wav", wav()),
                    &format!("{sound}-bad"),
                )
                .await;
            assert!(rejected.is_err());

            let st = c
                .custom_user_status_create(&status, UserStatus::Busy)
                .await
                .unwrap();
            let st2 = c
                .custom_user_status_update(&st.id, &format!("{status}-x"), UserStatus::Busy)
                .await
                .unwrap();
            assert_eq!(st2.status_type, UserStatus::Busy);
            let page = c
                .custom_user_status_page(&status, &Page::new(0, 5))
                .await
                .unwrap();
            assert!(page.items.iter().any(|s| s.id == st.id));

            let a = c
                .oauth_app_create(&app, true, "http://localhost/cb")
                .await
                .unwrap();
            assert!(a.client_secret.as_deref().is_some_and(|s| !s.is_empty()));
            let a2 = c
                .oauth_app_update(
                    &a.id,
                    &app,
                    false,
                    "http://localhost/cb,http://localhost/cb2",
                )
                .await
                .unwrap();
            assert!(!a2.active);
            assert_eq!(c.oauth_app_get(&a.id).await.unwrap().name, app);
        },
        async {
            if let Ok(p) = c.emoji_custom_all(&emoji, &Page::new(0, 5)).await {
                for e in p.items.iter().filter(|e| e.name == emoji) {
                    c.emoji_custom_delete(&e.id).await.unwrap();
                }
            }
            if let Ok(p) = c.custom_sounds_list(&sound, &Page::new(0, 20)).await {
                for s in p.items.iter().filter(|s| s.name.starts_with(&sound)) {
                    c.custom_sound_delete(&s.id).await.unwrap();
                }
            }
            if let Ok(p) = c.custom_user_status_page(&status, &Page::new(0, 20)).await {
                for s in p.items.iter().filter(|s| s.name.starts_with(&status)) {
                    c.custom_user_status_delete(&s.id).await.unwrap();
                }
            }
            if let Ok(apps) = c.oauth_apps_list().await {
                for a in apps.iter().filter(|a| a.name == app) {
                    c.oauth_app_delete(&a.id).await.unwrap();
                }
            }
            let _ = c.logout().await;
        },
    )
    .await;
}
