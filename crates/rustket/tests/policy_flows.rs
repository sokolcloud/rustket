//! The enterprise policy in the app (docs/policy.md sections 7 and 8):
//! forced preferences are effective, guarded and never saved.
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::sync::Arc;

use rustket::app::{App, Flags, Modal, Screen, SecretsMode};
use rustket::message::{Message, ModalMessage, SecretsReady};
use rustket::screens::settings::Message as S;
use rustket::workspace::rail;
use rustket_core::config::{AppConfig, Language};
use rustket_core::policy::{Policy, RawValue, Snapshot, SourceId, Trust, resolve};
use rustket_core::secrets::{FilePolicy, MemoryStore, SecretBackend, SwitchableStore};

/// A policy of one full-trust test source, resolved for this build.
fn policy(values: &[(&str, RawValue)]) -> Arc<Policy> {
    let mut snap = Snapshot::new(SourceId::Test("admin".into()), Trust::Full);
    for (name, value) in values {
        snap = snap.with(name, value.clone());
    }
    Arc::new(resolve(&[snap], &rustket::policy::context()))
}

fn no_preview() -> Arc<Policy> {
    policy(&[("NotificationPreview", RawValue::Bool(false))])
}

/// A config past the terms screen with the preview on, saved to `dir`.
/// Its language is English, so `App::new` never switches the process
/// locale to the system's language.
fn write_config(dir: &Path, edit: impl FnOnce(&mut AppConfig)) {
    let mut cfg = AppConfig::default();
    rustket::legal::accept_terms(&mut cfg);
    cfg.ui.language = Language::new("en").unwrap();
    cfg.ui.notification_preview = true;
    edit(&mut cfg);
    cfg.save(dir).unwrap();
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

/// The preferences as saved on disk.
fn saved(app: &App, dir: &Path) -> rustket_core::config::UiPrefs {
    app.flush_config();
    AppConfig::load(dir).unwrap().ui
}

/// Puts a marker on disk that any later save overwrites: `clock_24h`
/// flipped against the app's value. Returns the marker.
fn plant_marker(app: &App, dir: &Path) -> bool {
    app.flush_config();
    let mut cfg = AppConfig::load(dir).unwrap();
    let mark = !app.config.ui.clock_24h;
    cfg.ui.clock_24h = mark;
    cfg.save(dir).unwrap();
    mark
}

/// Whether no save happened since [`plant_marker`].
fn nothing_saved(app: &App, dir: &Path, mark: bool) -> bool {
    app.flush_config();
    AppConfig::load(dir).unwrap().ui.clock_24h == mark
}

/// The marker itself works: a save overwrites it.
#[test]
fn marker_detects_a_save() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), no_preview());
    open_settings(&mut a);
    let mark = plant_marker(&a, dir.path());
    assert!(nothing_saved(&a, dir.path(), mark));
    let _ = a.update(Message::Settings(S::SetSound(!a.config.ui.sound)));
    assert!(!nothing_saved(&a, dir.path(), mark));
}

fn open_settings(app: &mut App) {
    let _ = app.update(Message::Rail(rail::Message::Settings));
    assert!(matches!(app.screen, Screen::Settings(_)), "settings open");
}

/// Tests 1 and 2: a forced value is effective and never written; removing
/// the policy restores the user's own value.
#[test]
fn forced_value_is_effective_and_never_saved() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), no_preview());
    assert!(!a.config.ui.notification_preview, "forced");
    assert!(a.user_view().notification_preview, "the user's own value");
    let sound = a.config.ui.sound;
    open_settings(&mut a);
    let _ = a.update(Message::Settings(S::SetSound(!sound)));
    assert_eq!(a.config.ui.sound, !sound);
    assert!(!a.config.ui.notification_preview);
    let disk = saved(&a, dir.path());
    assert!(
        disk.notification_preview,
        "the forced value reached the file"
    );
    assert_eq!(disk.sound, !sound, "the free preference is saved");
    drop(a);

    let b = app(dir.path(), Arc::default());
    assert!(b.config.ui.notification_preview, "the user's value is back");
    assert_eq!(b.config.ui.sound, !sound);
}

/// Test 3: the Settings handler of a forced field does nothing (no save
/// queued); a stray write to a forced field is undone by the next update,
/// one to a free field is kept and saved.
#[test]
fn forced_field_is_guarded_and_stray_writes_are_undone() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(
        dir.path(),
        policy(&[("NotificationPreview", RawValue::Bool(true))]),
    );
    assert!(a.config.ui.notification_preview);
    open_settings(&mut a);
    let mark = plant_marker(&a, dir.path());
    let _ = a.update(Message::Settings(S::SetPreview(false)));
    assert!(a.config.ui.notification_preview);
    assert!(a.user_view().notification_preview);
    assert!(
        nothing_saved(&a, dir.path(), mark),
        "the guard let it through"
    );

    // A user whose own value differs from the forced one.
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), no_preview());
    open_settings(&mut a);
    let mark = plant_marker(&a, dir.path());
    let _ = a.update(Message::Settings(S::SetPreview(true)));
    assert!(!a.config.ui.notification_preview);
    assert!(
        nothing_saved(&a, dir.path(), mark),
        "the guard let it through"
    );

    let clock = a.config.ui.clock_24h;
    a.config.ui.notification_preview = true;
    a.config.ui.clock_24h = !clock;
    let _ = a.update(Message::Noop);
    assert!(
        !a.config.ui.notification_preview,
        "a forced write is undone"
    );
    assert_eq!(a.config.ui.clock_24h, !clock, "a free write is kept");
    let _ = a.update(Message::Settings(S::SetSound(!a.config.ui.sound)));
    let disk = saved(&a, dir.path());
    assert_eq!(disk.clock_24h, !clock);
    assert!(disk.notification_preview);
}

/// A stray write to a forced field is not adopted into the user's copy, also
/// when the user's own value equals the forced one.
#[test]
fn stray_write_to_a_forced_field_stays_out_of_the_user_copy() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |c| c.ui.notification_preview = false);
    let mut a = app(dir.path(), no_preview());
    open_settings(&mut a);
    a.config.ui.notification_preview = true;
    let _ = a.update(Message::Noop);
    assert!(!a.config.ui.notification_preview);
    assert!(!a.user_view().notification_preview, "the write was adopted");
    let _ = a.update(Message::Settings(S::SetSound(!a.config.ui.sound)));
    assert!(!saved(&a, dir.path()).notification_preview);
}

/// With the empty policy the reconcile changes nothing: direct writes to
/// every field stay and are saved.
#[test]
fn empty_policy_is_neutral() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), Arc::default());
    let before = a.config.ui.clone();
    let _ = a.update(Message::Noop);
    assert_eq!(a.config.ui, before);
    a.config.ui.notification_preview = false;
    a.config.ui.hardware_acceleration = false;
    a.config.ui.secrets_memory_only = true;
    a.config.ui.check_updates = false;
    a.config.ui.update_prereleases = true;
    let _ = a.update(Message::Noop);
    assert!(!a.config.ui.notification_preview);
    assert!(!a.config.ui.hardware_acceleration);
    assert!(a.config.ui.secrets_memory_only);
    assert!(!a.config.ui.check_updates);
    assert!(a.config.ui.update_prereleases);
    assert_eq!(a.user_view(), a.config.ui);
    let _ = a.update(Message::SaveDebounced(0));
    assert_eq!(saved(&a, dir.path()), a.config.ui);
}

/// Test 4: replacing the policy keeps the user's own values; the file never
/// receives a forced value.
#[test]
fn set_policy_in_both_directions() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), Arc::default());
    assert!(a.config.ui.notification_preview);

    // Not forced -> forced.
    let _ = a.set_policy(no_preview());
    assert!(!a.config.ui.notification_preview);
    assert!(a.user_view().notification_preview);
    open_settings(&mut a);
    let sound = a.config.ui.sound;
    let _ = a.update(Message::Settings(S::SetSound(!sound)));
    assert!(saved(&a, dir.path()).notification_preview);

    // Forced -> not forced: the user's value returns, the free change stays.
    let _ = a.set_policy(Arc::default());
    assert!(a.config.ui.notification_preview);
    assert_eq!(a.config.ui.sound, !sound);

    // A choice made while not forced survives a forcing policy.
    let _ = a.update(Message::Settings(S::SetPreview(false)));
    assert!(!saved(&a, dir.path()).notification_preview);
    let _ = a.set_policy(policy(&[("NotificationPreview", RawValue::Bool(true))]));
    assert!(a.config.ui.notification_preview, "forced on");
    let _ = a.update(Message::Settings(S::SetSound(sound)));
    assert!(
        !saved(&a, dir.path()).notification_preview,
        "the forced value reached the file"
    );
    // The value forced by the old policy does not leak across a change.
    let _ = a.set_policy(Arc::default());
    assert!(!a.config.ui.notification_preview);
    assert!(!a.user_view().notification_preview);
}

/// Test 5: a policy that forbids the secrets file opens the store with the
/// "never write the file" policy and locks the memory-only switch.
#[tokio::test]
async fn secrets_file_forbidden() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(
        dir.path(),
        policy(&[("AllowSecretsFile", RawValue::Bool(false))]),
    );
    if cfg!(feature = "mas") {
        // Not applicable: the store flavor never writes the file anyway.
        assert_eq!(a.secrets_file_policy(), FilePolicy::Forbidden);
        assert!(!a.config.ui.secrets_memory_only);
        return;
    }
    assert!(a.config.ui.secrets_memory_only);
    assert_eq!(a.secrets_file_policy(), FilePolicy::Never);
    assert!(!a.user_view().secrets_memory_only);
    // A switchable store: a switch that got through would start moving it.
    let _ = a.update(Message::SecretsReady(SecretsReady {
        store: Arc::new(SwitchableStore::new(
            Box::new(MemoryStore::new()),
            SecretBackend::MemoryByPolicy,
            None,
        )),
        tokens: vec![],
        notices: vec![],
    }));
    assert!(!a.secrets_switching);

    open_settings(&mut a);
    let mark = plant_marker(&a, dir.path());
    let _ = a.update(Message::Settings(S::SetSecretsMemoryOnly(false)));
    assert!(a.config.ui.secrets_memory_only, "the switch turned off");
    let _ = a.update(Message::Settings(S::SetSecretsMemoryOnly(true)));
    assert!(
        !a.modals
            .iter()
            .any(|m| matches!(m, Modal::SecretsMemoryOnly { .. })),
        "no confirmation for a forced switch"
    );
    let _ = a.update(Message::Modal(ModalMessage::ConfirmSecretsMemoryOnly));
    assert!(
        !a.secrets_switching,
        "the app's guard let the switch through"
    );
    // A failed switch never rolls the forced field back (the rollback
    // would save), and is not retried.
    let _ = a.update(Message::SecretsPolicyApplied(
        true,
        SecretBackend::File,
        Some("keyring failed".into()),
    ));
    assert!(a.config.ui.secrets_memory_only);
    assert!(!a.secrets_switching, "retried");
    assert!(nothing_saved(&a, dir.path(), mark), "a save was queued");
    assert!(!saved(&a, dir.path()).secrets_memory_only, "file untouched");
}

/// A secret store on the plaintext file, as `SecretsReady` delivers it.
#[cfg(not(feature = "mas"))]
fn file_store_ready(backend: SecretBackend) -> Message {
    Message::SecretsReady(SecretsReady {
        store: Arc::new(SwitchableStore::new(
            Box::new(MemoryStore::new()),
            backend,
            None,
        )),
        tokens: vec![],
        notices: vec![],
    })
}

#[cfg(not(feature = "mas"))]
fn no_secrets_file() -> Arc<Policy> {
    policy(&[("AllowSecretsFile", RawValue::Bool(false))])
}

#[cfg(not(feature = "mas"))]
fn memory_only_modal(a: &App) -> bool {
    a.modals
        .iter()
        .any(|m| matches!(m, Modal::SecretsMemoryOnly { .. }))
}

/// A policy that starts forcing memory-only closes the confirmation and
/// moves the store; one removed during that switch moves it back after.
#[cfg(not(feature = "mas"))]
#[tokio::test]
async fn set_policy_moves_the_secret_store() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), Arc::default());
    let _ = a.update(file_store_ready(SecretBackend::File));
    assert!(!a.secrets_switching);
    open_settings(&mut a);
    let _ = a.update(Message::Settings(S::SetSecretsMemoryOnly(true)));
    assert!(memory_only_modal(&a));

    let _ = a.set_policy(no_secrets_file());
    assert!(!memory_only_modal(&a), "the confirmation stays open");
    assert!(a.secrets_switching, "the store does not move");
    assert!(a.config.ui.secrets_memory_only);
    assert!(!a.user_view().secrets_memory_only);

    // Removed while the switch runs: the store follows back once it ends.
    let _ = a.set_policy(Arc::default());
    assert!(!a.config.ui.secrets_memory_only);
    let _ = a.update(Message::SecretsPolicyApplied(
        true,
        SecretBackend::MemoryByPolicy,
        None,
    ));
    assert!(a.secrets_switching, "the store stays in memory");
    let _ = a.update(Message::SecretsPolicyApplied(
        false,
        SecretBackend::File,
        None,
    ));
    assert!(!a.secrets_switching);
    assert!(!saved(&a, dir.path()).secrets_memory_only);
}

/// A forcing policy that arrives while the user's own switch runs starts no
/// second switch; the store follows the forced value once it ends.
#[cfg(not(feature = "mas"))]
#[tokio::test]
async fn set_policy_during_a_switch_applies_after_it() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |c| c.ui.secrets_memory_only = true);
    let mut a = app(dir.path(), Arc::default());
    let _ = a.update(file_store_ready(SecretBackend::MemoryByPolicy));
    assert!(!a.secrets_switching, "opened as the preference says");
    open_settings(&mut a);
    let _ = a.update(Message::Settings(S::SetSecretsMemoryOnly(false)));
    assert!(a.secrets_switching);
    let _ = a.set_policy(no_secrets_file());
    assert!(a.config.ui.secrets_memory_only);
    let _ = a.update(Message::SecretsPolicyApplied(
        false,
        SecretBackend::File,
        None,
    ));
    assert!(a.secrets_switching, "the forced value is not applied");
    let _ = a.update(Message::SecretsPolicyApplied(
        true,
        SecretBackend::MemoryByPolicy,
        None,
    ));
    assert!(!a.secrets_switching);
    assert!(
        !saved(&a, dir.path()).secrets_memory_only,
        "the user's choice is kept"
    );
}

/// A policy that changes before the secret store is open moves the store
/// as soon as it opens.
#[cfg(not(feature = "mas"))]
#[tokio::test]
async fn set_policy_before_the_store_opens() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), Arc::default());
    assert_eq!(a.secrets_file_policy(), FilePolicy::Allowed);
    let _ = a.set_policy(no_secrets_file());
    assert!(!a.secrets_switching, "no store yet");
    let _ = a.update(file_store_ready(SecretBackend::File));
    assert!(a.secrets_switching, "the store stays on the file");
}

/// The other lockable preferences follow the same guard: their handlers
/// queue no save, and a save made while they are forced writes the user's
/// values.
#[test]
fn other_forced_preferences_are_guarded() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |c| c.ui.update_prereleases = true);
    let mut a = app(
        dir.path(),
        policy(&[
            ("HardwareAcceleration", RawValue::Bool(false)),
            ("UpdateMode", RawValue::String("off".into())),
        ]),
    );
    open_settings(&mut a);
    let mark = plant_marker(&a, dir.path());
    if !cfg!(feature = "mas") {
        assert!(!a.config.ui.hardware_acceleration);
        let _ = a.update(Message::Settings(S::SetHardwareAcceleration(true)));
        assert!(!a.config.ui.hardware_acceleration);
    }
    #[cfg(feature = "self-update")]
    {
        assert!(!a.config.ui.check_updates);
        assert!(!a.config.ui.update_prereleases);
        let _ = a.update(Message::Settings(S::SetCheckUpdates(true)));
        let _ = a.update(Message::Settings(S::SetUpdatePrereleases(true)));
        let _ = a.update(Message::Settings(S::SetUpdatePrereleases(false)));
        assert!(!a.config.ui.check_updates);
        assert!(!a.config.ui.update_prereleases);
    }
    assert!(
        nothing_saved(&a, dir.path(), mark),
        "a guard let a change through"
    );
    let _ = a.update(Message::Settings(S::SetSound(!a.config.ui.sound)));
    let disk = saved(&a, dir.path());
    assert!(
        disk.hardware_acceleration,
        "a forced value reached the file"
    );
    assert!(disk.check_updates, "a forced value reached the file");
    assert!(disk.update_prereleases, "a forced value reached the file");
}

/// Test 6: `--dump-policy` is parsed before anything else (the parsing
/// table is in `cli.rs`).
#[test]
fn dump_policy_flag() {
    use rustket::cli::{Action, parse};
    assert_eq!(parse(["--dump-policy"]), Action::DumpPolicy(None));
    assert_eq!(
        parse(["--dump-policy", "out.json"]),
        Action::DumpPolicy(Some("out.json".into()))
    );
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("p.json");
    rustket::policy::dump(&no_preview(), Some(&out)).unwrap();
    let v: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(v["rows"][0]["key"], "NotificationPreview");
    assert_eq!(v["rows"][0]["status"], "applied");
}

/// Section 7: a failed store move that the policy started never changes or
/// saves the user's own preference; the store keeps its backend.
#[cfg(not(feature = "mas"))]
#[tokio::test]
async fn failed_policy_move_keeps_the_user_preference() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |c| c.ui.secrets_memory_only = false);
    let mut a = app(dir.path(), no_secrets_file());
    assert!(a.config.ui.secrets_memory_only, "forced");
    let _ = a.update(file_store_ready(SecretBackend::MemoryByPolicy));
    assert!(!a.secrets_switching);
    let mark = plant_marker(&a, dir.path());

    // The policy is removed: the store moves back to the file, and fails.
    assert_eq!(a.set_policy(Arc::default()).units(), 1);
    assert!(a.secrets_switching);
    assert!(!a.config.ui.secrets_memory_only);
    let _ = a.update(Message::SecretsPolicyApplied(
        false,
        SecretBackend::MemoryByPolicy,
        Some("keyring failed".into()),
    ));
    assert!(!a.secrets_switching, "retried");
    assert_eq!(a.secret_backend, Some(SecretBackend::MemoryByPolicy));
    assert!(
        !a.user_view().secrets_memory_only,
        "the failure changed the user's preference"
    );
    assert!(!a.config.ui.secrets_memory_only);
    assert!(nothing_saved(&a, dir.path(), mark), "a save was queued");
    assert!(!saved(&a, dir.path()).secrets_memory_only);
}

/// The user's own failed switch still rolls the preference back and saves
/// it: the store did not move.
#[cfg(not(feature = "mas"))]
#[tokio::test]
async fn failed_user_switch_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |c| c.ui.secrets_memory_only = true);
    let mut a = app(dir.path(), Arc::default());
    let _ = a.update(file_store_ready(SecretBackend::MemoryByPolicy));
    open_settings(&mut a);
    let _ = a.update(Message::Settings(S::SetSecretsMemoryOnly(false)));
    assert!(a.secrets_switching);
    assert!(!saved(&a, dir.path()).secrets_memory_only);
    let _ = a.update(Message::SecretsPolicyApplied(
        false,
        SecretBackend::MemoryByPolicy,
        Some("file failed".into()),
    ));
    assert!(!a.secrets_switching);
    assert!(a.config.ui.secrets_memory_only, "rolled back");
    assert!(saved(&a, dir.path()).secrets_memory_only, "and saved");
}

/// `set_policy` starts exactly one store move, and none while one runs.
#[cfg(not(feature = "mas"))]
#[tokio::test]
async fn set_policy_task_starts_one_store_move() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |c| c.ui.update_prereleases = false);
    let mut a = app(dir.path(), Arc::default());
    let _ = a.update(file_store_ready(SecretBackend::File));
    assert!(!a.secrets_switching);

    assert_eq!(a.set_policy(no_preview()).units(), 0, "nothing to move");
    assert_eq!(a.set_policy(no_secrets_file()).units(), 1, "one move");
    assert!(a.secrets_switching);
    // Removed while the move runs: the store follows when it ends, no
    // second move now.
    assert_eq!(a.set_policy(Arc::default()).units(), 0, "a second move");
    assert!(!a.config.ui.secrets_memory_only);
    assert!(a.secrets_switching);
    // Forced again, still running: still none.
    assert_eq!(a.set_policy(no_secrets_file()).units(), 0, "a second move");
    let _ = a.update(Message::SecretsPolicyApplied(
        true,
        SecretBackend::MemoryByPolicy,
        None,
    ));
    assert!(!a.secrets_switching, "already where the policy wants it");
}

#[cfg(not(feature = "mas"))]
fn warnings(a: &App) -> usize {
    a.toasts
        .iter()
        .filter(|t| t.kind == rustket_ui::widget::BannerKind::Warning)
        .count()
}

/// The "stored in an unencrypted file" warning is not shown for a file the
/// store is about to leave (the policy changed while it opened).
#[cfg(not(feature = "mas"))]
#[tokio::test]
async fn no_plain_file_warning_for_a_file_being_left() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), Arc::default());
    let _ = a.update(file_store_ready(SecretBackend::File));
    assert_eq!(warnings(&a), 1, "the warning of a store on the file");

    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), Arc::default());
    let _ = a.set_policy(no_secrets_file());
    let _ = a.update(file_store_ready(SecretBackend::File));
    assert!(a.secrets_switching, "the store leaves the file");
    assert_eq!(warnings(&a), 0, "warned about a state being left");
    // The move fails: the store stays on the file, so the warning shows now.
    let _ = a.update(Message::SecretsPolicyApplied(
        true,
        SecretBackend::File,
        Some("keyring failed".into()),
    ));
    assert_eq!(warnings(&a), 1, "the store stays on the file unwarned");
    // Shown once: a later switch does not repeat it.
    let _ = a.update(Message::SecretsPolicyApplied(
        true,
        SecretBackend::File,
        Some("keyring failed".into()),
    ));
    assert_eq!(warnings(&a), 1);

    // The move succeeds: no warning at all.
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), Arc::default());
    let _ = a.set_policy(no_secrets_file());
    let _ = a.update(file_store_ready(SecretBackend::File));
    let _ = a.update(Message::SecretsPolicyApplied(
        true,
        SecretBackend::MemoryByPolicy,
        None,
    ));
    assert_eq!(warnings(&a), 0);
}

/// The deferred warning survives a follow-up move that takes the store back
/// to the file: shown once, when the switches settle there.
#[cfg(not(feature = "mas"))]
#[tokio::test]
async fn deferred_plain_file_warning_survives_a_move_back() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), Arc::default());
    // The policy arrives before the store opens; the store opens on the file
    // and starts moving to memory.
    let _ = a.set_policy(no_secrets_file());
    let _ = a.update(file_store_ready(SecretBackend::File));
    assert!(a.secrets_switching);
    assert_eq!(warnings(&a), 0);
    // The policy is removed during the move.
    let _ = a.set_policy(Arc::default());
    let _ = a.update(Message::SecretsPolicyApplied(
        true,
        SecretBackend::MemoryByPolicy,
        None,
    ));
    assert!(a.secrets_switching, "the follow-up returns to the file");
    assert_eq!(warnings(&a), 0, "warned before the switches settled");
    let _ = a.update(Message::SecretsPolicyApplied(
        false,
        SecretBackend::File,
        None,
    ));
    assert!(!a.secrets_switching);
    assert_eq!(warnings(&a), 1, "the deferred warning was lost");
    // Not repeated by a later switch.
    let _ = a.update(Message::SecretsPolicyApplied(
        false,
        SecretBackend::File,
        None,
    ));
    assert_eq!(warnings(&a), 1);
}

/// A test source as the refresh reads it.
fn read(values: &[(&str, RawValue)]) -> Vec<Snapshot> {
    let mut snap = Snapshot::new(SourceId::Test("admin".into()), Trust::Full);
    for (name, value) in values {
        snap = snap.with(name, value.clone());
    }
    vec![snap]
}

/// Section 8: a refresh read with changed sources replaces the policy and
/// forces the preference without writing it; the same read again changes
/// nothing and returns no task.
#[test]
fn refresh_read_replaces_the_policy() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), Arc::default());
    let mark = plant_marker(&a, dir.path());
    assert_eq!(a.update(Message::Noop).units(), 0, "a quiet update");

    let snaps = read(&[("NotificationPreview", RawValue::Bool(false))]);
    let _ = a.update(Message::PolicyRead(Some(snaps.clone())));
    assert!(!a.config.ui.notification_preview, "forced");
    assert!(a.user_view().notification_preview);
    assert_eq!(
        *a.policy,
        *policy(&[("NotificationPreview", RawValue::Bool(false))])
    );
    assert!(nothing_saved(&a, dir.path(), mark), "a save was queued");

    let before = a.policy.clone();
    let task = a.update(Message::PolicyRead(Some(snaps)));
    assert_eq!(task.units(), 0, "a task for an unchanged policy");
    assert!(Arc::ptr_eq(&before, &a.policy), "the policy was replaced");

    // A change only in the report (an unknown name) still replaces it.
    let snaps = read(&[
        ("NotificationPreview", RawValue::Bool(false)),
        ("Typo", RawValue::Bool(true)),
    ]);
    let _ = a.update(Message::PolicyRead(Some(snaps)));
    assert!(!Arc::ptr_eq(&before, &a.policy));
    assert_eq!(a.policy.rows().len(), 2);

    // Removed: the empty read is held for one poll, then accepted.
    let _ = a.update(Message::PolicyRead(Some(read(&[]))));
    assert!(!a.config.ui.notification_preview, "held for one poll");
    let _ = a.update(Message::PolicyRead(Some(read(&[]))));
    assert!(a.config.ui.notification_preview, "the user's value is back");
    assert!(nothing_saved(&a, dir.path(), mark), "a save was queued");
}

/// The debounce starts from the start-up snapshots: a first refresh that
/// finds the source empty is held.
#[test]
fn refresh_starts_from_the_startup_sources() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let snaps = read(&[("NotificationPreview", RawValue::Bool(false))]);
    let (mut a, _) = App::new(Flags {
        config_dir: Some(dir.path().to_owned()),
        cache_dir: Some(dir.path().join("cache")),
        secrets: SecretsMode::Memory,
        policy: Arc::new(resolve(&snaps, &rustket::policy::context())),
        policy_sources: snaps,
        ..Flags::default()
    });
    assert!(!a.config.ui.notification_preview);
    let task = a.update(Message::PolicyRead(Some(read(&[]))));
    assert_eq!(task.units(), 0);
    assert!(!a.config.ui.notification_preview, "held for one poll");
}

/// A poll while a read is in flight starts no second read; the read's
/// result (or its failure) allows the next one.
#[test]
fn refresh_poll_is_not_doubled() {
    let dir = tempfile::tempdir().unwrap();
    write_config(dir.path(), |_| {});
    let mut a = app(dir.path(), Arc::default());
    assert_eq!(a.update(Message::PolicyPoll).units(), 1, "a read");
    assert_eq!(a.update(Message::PolicyPoll).units(), 0, "a second read");
    let _ = a.update(Message::PolicyRead(None));
    assert_eq!(a.update(Message::PolicyPoll).units(), 1, "after a failure");
    let _ = a.update(Message::PolicyRead(Some(Vec::new())));
    assert_eq!(a.update(Message::PolicyPoll).units(), 1, "after a result");
}
