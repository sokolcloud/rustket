//! The sign-in keys of the enterprise policy in the app (docs/policy.md
//! section 9.4): the login screen follows `AllowPasswordLogin` and
//! `AllowTokenLogin` at once, its refused messages do nothing, and a token
//! from a browser sign-in the client started is still accepted.
#![allow(clippy::unwrap_used)]

use std::sync::Arc;

use rocketchat::Credentials;
use rocketchat::models::User;
use rustket::app::{App, Flags, Modal, SecretsMode};
use rustket::message::{Message, ModalMessage, SecretsReady};
use rustket::screens::login::{self, SignInRules};
use rustket_core::ServerId;
use rustket_core::config::{AppConfig, Language, ServerEntry};
use rustket_core::policy::{Policy, RawValue, Snapshot, SourceId, Trust, resolve};
use rustket_core::secrets::{MemoryStore, SecretBackend, SwitchableStore};

const SERVER: &str = "http://localhost:3000";

fn policy(values: &[(&str, RawValue)]) -> Arc<Policy> {
    let mut snap = Snapshot::new(SourceId::Test("admin".into()), Trust::Full);
    for (name, value) in values {
        snap = snap.with(name, value.clone());
    }
    Arc::new(resolve(&[snap], &rustket::policy::context()))
}

/// One configured server without an account (its login screen shows),
/// under `p`, past the terms screen and with the secret store open.
fn signed_out(dir: &std::path::Path, p: Arc<Policy>) -> (App, ServerId) {
    let mut cfg = AppConfig::default();
    rustket::legal::accept_terms(&mut cfg);
    // English in the config: `App::new` selects it itself and never
    // switches the process locale to the system's language.
    cfg.ui.language = Language::new("en").unwrap();
    let sid = cfg.add_server(ServerEntry::new(SERVER, "Local"));
    cfg.save(dir).unwrap();
    let (mut app, _) = App::new(Flags {
        config_dir: Some(dir.to_owned()),
        cache_dir: Some(dir.join("cache")),
        secrets: SecretsMode::Memory,
        policy: p,
        ..Flags::default()
    });
    let _ = app.update(Message::SecretsReady(SecretsReady {
        store: Arc::new(SwitchableStore::new(
            Box::new(MemoryStore::new()),
            SecretBackend::Memory,
            None,
        )),
        tokens: vec![],
        notices: vec![],
    }));
    assert!(app.servers[&sid].login.is_some());
    (app, sid)
}

fn rules(app: &App, sid: &ServerId) -> SignInRules {
    app.servers[sid].login.as_ref().unwrap().rules().clone()
}

/// The login screen opens under the policy's rules and follows a refresh
/// at once.
#[tokio::test]
async fn login_screen_follows_the_policy() {
    let dir = tempfile::tempdir().unwrap();
    let off = policy(&[
        ("AllowPasswordLogin", RawValue::Bool(false)),
        ("AllowTokenLogin", RawValue::Bool(false)),
        ("OrganizationName", RawValue::String("Acme".into())),
    ]);
    let (mut app, sid) = signed_out(dir.path(), off.clone());
    let r = rules(&app, &sid);
    assert!(!r.password && !r.token);
    assert_eq!(r.organization.as_deref(), Some("Acme"));

    let _ = app.set_policy(Arc::default());
    assert_eq!(rules(&app, &sid), SignInRules::default());

    // Turned off while the token entry is open: it closes at once.
    let _ = app.update(Message::Login(sid.clone(), login::Message::ShowToken(true)));
    assert!(app.servers[&sid].login.as_ref().unwrap().token_mode);
    let _ = app.set_policy(policy(&[("AllowTokenLogin", RawValue::Bool(false))]));
    let l = app.servers[&sid].login.as_ref().unwrap();
    assert!(!l.token_mode);
    assert!(l.rules().password && !l.rules().token);
}

/// Refused messages: no password sign-in, no token entry, no token check.
#[tokio::test]
async fn refused_messages_do_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let off = policy(&[
        ("AllowPasswordLogin", RawValue::Bool(false)),
        ("AllowTokenLogin", RawValue::Bool(false)),
    ]);
    let (mut app, sid) = signed_out(dir.path(), off);
    for m in [
        login::Message::User("alice".into()),
        login::Message::Password("secret".into()),
        login::Message::Submit,
        login::Message::SubmitUser,
        login::Message::ShowToken(true),
        login::Message::PatUserId("U1".into()),
        login::Message::PatToken("T1".into()),
        login::Message::SubmitPat,
    ] {
        let task = app.update(Message::Login(sid.clone(), m));
        drop(task);
    }
    let l = app.servers[&sid].login.as_ref().unwrap();
    assert!(l.password.is_empty() && !l.busy && !l.token_mode);
    assert!(l.pat_user_id.is_empty() && l.pat_token.is_empty());
    assert!(app.servers[&sid].credentials.is_none());

    // Filled fields: only the policy's guards stop the sign-ins.
    for m in [
        login::Message::Submit,
        login::Message::SubmitUser,
        login::Message::SubmitPat,
    ] {
        let what = format!("{m:?}");
        let l = app.servers.get_mut(&sid).unwrap().login.as_mut().unwrap();
        l.user = "alice".into();
        l.password = "secret".into();
        l.pat_user_id = "U1".into();
        l.pat_token = "T1".into();
        let task = app.update(Message::Login(sid.clone(), m));
        assert_eq!(task.units(), 0, "{what}");
        assert!(!app.servers[&sid].login.as_ref().unwrap().busy, "{what}");
    }
    assert!(app.servers[&sid].credentials.is_none());
}

/// A token arriving through a `rocketchat://auth` link for a browser
/// sign-in the client started is accepted with the confirmation, also
/// with the token login and the form turned off.
#[tokio::test]
async fn token_link_of_a_browser_sign_in_is_still_accepted() {
    let dir = tempfile::tempdir().unwrap();
    let off = policy(&[
        ("AllowPasswordLogin", RawValue::Bool(false)),
        ("AllowTokenLogin", RawValue::Bool(false)),
    ]);
    let (mut app, sid) = signed_out(dir.path(), off);
    let _ = app.update(Message::Login(sid.clone(), login::Message::BrowserLogin));
    assert!(
        app.servers[&sid]
            .login
            .as_ref()
            .is_some_and(|l| l.pending.is_some())
    );
    let link = format!("rocketchat://auth?host={SERVER}&userId=U1&token=T1");
    let _ = app.update(Message::DeepLinks(vec![link]));
    let _ = app.update(Message::TokenUser(
        sid.clone(),
        Credentials::new("U1", "T1"),
        Ok(User {
            id: "U1".into(),
            username: Some("u1".into()),
            ..Default::default()
        }),
    ));
    let stamp = match app.modals.last() {
        Some(Modal::ConfirmToken { stamp, .. }) => *stamp,
        _ => panic!("no confirmation"),
    };
    let _ = app.update(Message::ArmConfirm(stamp));
    let _ = app.update(Message::Modal(ModalMessage::ConfirmToken));
    assert_eq!(
        app.servers[&sid]
            .credentials
            .as_ref()
            .map(|c| c.user_id.as_str()),
        Some("U1")
    );
}
