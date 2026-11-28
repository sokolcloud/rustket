//! App wiring: messages, the boot hook, the hourly subscription and the
//! tasks that check, download, install and relaunch.

use std::path::PathBuf;

use iced::{Subscription, Task};
use rust_i18n::t;
use rustket_ui::widget::BannerKind;

use super::download::{DlEvent, download};
use super::github::{self, Endpoints, Release};
use super::install::{self, Outcome};
use super::mode::{self, UpdateMode};
use super::relaunch::{self, StaleFailure};
use super::state::{CheckOrigin, Notice, releases_page};
use super::view::error_text;
use super::{
    InstallKind, Platform, STARTUP_DELAY, TICK, UpdateError, assets, cache_dir, current_version,
    detect,
};
use crate::app::App;
use crate::message::Message;

#[derive(Debug, Clone)]
pub enum Msg {
    /// The install kind, and a failure an earlier run left behind.
    Detected(InstallKind, Option<StaleFailure>),
    /// Hourly: check when due.
    Tick,
    Check(CheckOrigin),
    Checked(CheckOrigin, Result<Option<Box<Release>>, UpdateError>),
    /// Install (or, for unmanaged copies, open the release page).
    Install,
    Download(DlEvent),
    /// Install the downloaded file (after unsaved admin settings were
    /// discarded, or from the card).
    InstallReady,
    Installed(Result<Outcome, UpdateError>),
    CancelDownload,
    /// Hide the card of this version for this run (also "Dismiss").
    Later,
    Skip,
    OpenReleasePage,
    OpenFile(PathBuf),
    /// Show the file in its folder.
    RevealFile(PathBuf),
    /// "Include pre-releases" was switched off.
    PrereleasesOff,
}

fn wrap(msg: Msg) -> Message {
    Message::Updater(msg)
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

/// The effective update mode of the running app (docs/policy.md 9.3): the
/// policy's `UpdateMode` and the detected install kind through
/// [`mode::effective`]. Every entry of the updater is gated by it.
pub fn mode(app: &App) -> UpdateMode {
    mode::effective(app.policy.update_mode(), app.updater.kind.as_ref())
}

/// Automatic checks run: the preference is on and the mode is not `off`.
fn auto_checks(app: &App) -> bool {
    app.config.ui.check_updates && mode(app) != UpdateMode::Off
}

/// The mode changed from `old` (a policy refresh, or the detected kind):
/// leaving `prompt` cancels a download; in `notify` what could still be
/// installed becomes the report of its release, `off` drops it and also
/// ends a check under way and its status. An install that runs is not
/// interrupted. Nothing is requested: a mode that allows
/// checks again checks at the next tick or start.
pub fn mode_changed(app: &mut App, old: UpdateMode) -> Task<Message> {
    let new = mode(app);
    if new == old || new == UpdateMode::Prompt {
        return Task::none();
    }
    tracing::info!(?old, ?new, "updater: the update mode changed");
    if let Some(h) = app.updater.withdraw(new == UpdateMode::Off) {
        h.abort();
    }
    Task::none()
}

/// After boot (desktop runs only): detect the install kind, clean up old
/// downloads and schedule the startup check (not in the mode `off`).
pub fn boot(app: &App) -> Task<Message> {
    if !app.updater.enabled || current_version().is_none() {
        return Task::none();
    }
    let detect = Task::perform(
        async {
            tokio::task::spawn_blocking(|| {
                let kind = detect::detect(&detect::SystemHost, Platform::current());
                tracing::info!(?kind, "updater: install kind");
                let stale = relaunch::cleanup_stale(&kind);
                (kind, stale)
            })
            .await
            .unwrap_or((InstallKind::Unmanaged, None))
        },
        |(kind, stale)| wrap(Msg::Detected(kind, stale)),
    );
    let startup = if auto_checks(app) {
        Task::perform(tokio::time::sleep(STARTUP_DELAY), |_| {
            wrap(Msg::Check(CheckOrigin::Startup))
        })
    } else {
        Task::none()
    };
    Task::batch([detect, startup])
}

/// The hourly tick, while automatic checks are on (not in the mode `off`).
pub fn subscription(app: &App) -> Option<Subscription<Message>> {
    (app.updater.enabled && auto_checks(app))
        .then(|| iced::time::every(TICK).map(|_| wrap(Msg::Tick)))
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Detected(kind, stale) => {
            let old = mode(app);
            app.updater.kind = Some(kind);
            let task = mode_changed(app, old);
            if let Some(f) = stale {
                tracing::warn!(version = %f.version, error = %f.error, "updater: the last install failed");
                // Its card offers the downloaded file: only where installs are.
                if mode(app) == UpdateMode::Prompt {
                    app.updater.on_stale_failure(f);
                }
            }
            task
        }
        Msg::Tick => check(app, CheckOrigin::Tick),
        Msg::Check(origin) => check(app, origin),
        Msg::Checked(origin, result) => checked(app, origin, result.map(|r| r.map(|b| *b))),
        Msg::Install => start_install(app),
        // Downloads and installs only in the mode `prompt`; leaving it
        // cancelled the download (`mode_changed`).
        Msg::Download(_) | Msg::InstallReady | Msg::OpenFile(_)
            if mode(app) != UpdateMode::Prompt =>
        {
            tracing::info!("updater: no install in this update mode");
            Task::none()
        }
        Msg::Download(DlEvent::Progress { received, total }) => {
            app.updater.on_progress(received, total);
            Task::none()
        }
        Msg::Download(DlEvent::Done(result)) => {
            if app.updater.on_downloaded(result) {
                install_ready(app)
            } else {
                Task::none()
            }
        }
        Msg::InstallReady => install_ready(app),
        Msg::Installed(result) => installed(app, result),
        Msg::CancelDownload => {
            if let Some(h) = app.updater.cancel_download() {
                h.abort();
            }
            Task::none()
        }
        Msg::Later => {
            app.updater.later();
            Task::none()
        }
        Msg::Skip => {
            let Some(v) = app.updater.version().cloned() else {
                return Task::none();
            };
            app.ui_state.update_skipped = Some(v.to_string());
            app.updater.hidden = Some(v);
            app.save_config()
        }
        Msg::OpenReleasePage => {
            let url = app
                .updater
                .release()
                .map_or_else(releases_page, |r| r.html_url.clone());
            app.open_url(url)
        }
        Msg::PrereleasesOff => {
            app.updater.drop_prereleases();
            Task::none()
        }
        Msg::OpenFile(path) => open_path(path),
        Msg::RevealFile(path) => match path.parent() {
            Some(dir) => open_path(dir.to_owned()),
            None => Task::none(),
        },
    }
}

fn open_path(path: PathBuf) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || open::that_detached(&path))
                .await
                .map_err(std::io::Error::other)
                .and_then(|r| r)
        },
        |r| {
            if let Err(e) = r {
                tracing::warn!(error = %e, "updater: cannot open the file");
            }
            Message::Noop
        },
    )
}

/// A check of `origin`; none of any kind in the mode `off`.
fn check(app: &mut App, origin: CheckOrigin) -> Task<Message> {
    let u = &app.updater;
    if !u.enabled || current_version().is_none() || mode(app) == UpdateMode::Off {
        return Task::none();
    }
    if origin != CheckOrigin::Manual && !app.config.ui.check_updates {
        return Task::none();
    }
    if !u.due(now(), app.ui_state.update_last_check, origin) || !app.updater.start_check(origin) {
        return Task::none();
    }
    let prereleases = app.config.ui.update_prereleases;
    Task::perform(
        async move {
            let http = github::client()?;
            github::fetch_release(&http, &Endpoints::current(), prereleases).await
        },
        move |r| wrap(Msg::Checked(origin, r.map(|o| o.map(Box::new)))),
    )
}

fn checked(
    app: &mut App,
    origin: CheckOrigin,
    result: Result<Option<Release>, UpdateError>,
) -> Task<Message> {
    let Some(current) = current_version() else {
        return Task::none();
    };
    // No result counts in the mode `off` (switching to it ended the check).
    if mode(app) == UpdateMode::Off {
        return Task::none();
    }
    if let Err(e) = &result {
        tracing::info!(?origin, "updater: check failed: {e}");
    }
    let prereleases = app.config.ui.update_prereleases;
    let out = app
        .updater
        .on_checked(origin, result, &current, prereleases);
    let mut tasks = Vec::new();
    if out.record {
        app.ui_state.update_last_check = Some(now());
        tasks.push(app.save_config());
    }
    match out.notice {
        Some(Notice::UpToDate) => {
            tasks.push(app.toast(BannerKind::Success, t!("updater.up_to_date"), String::new()))
        }
        Some(Notice::Error(e)) => tasks.push(app.toast(
            BannerKind::Error,
            t!("updater.check_failed"),
            error_text(&e),
        )),
        Some(Notice::Available) | None => {}
    }
    Task::batch(tasks)
}

/// "Install" (or "Download" for a copy that cannot install): only in the
/// mode `prompt`.
fn start_install(app: &mut App) -> Task<Message> {
    if mode(app) != UpdateMode::Prompt {
        tracing::info!("updater: no install in this update mode");
        return Task::none();
    }
    let Some(release) = app.updater.installable().cloned() else {
        return Task::none();
    };
    let kind = match &app.updater.kind {
        Some(k) if k.can_install() => k.clone(),
        _ => return app.open_url(release.html_url.clone()),
    };
    if !app.updater.enabled {
        return Task::none();
    }
    if !app.may_leave(wrap(Msg::Install)) {
        return Task::none();
    }
    let version = release.version.to_string();
    let asset = match assets::select(&kind, assets::Arch::current(), &version, &release.assets) {
        Ok(a) => a.clone(),
        Err(e) => {
            app.updater.fail(release, e, None);
            return Task::none();
        }
    };
    let Some(dir) = cache_dir().map(|d| d.join(&version)) else {
        let e = UpdateError::Io {
            op: "locate the cache dir",
            kind: std::io::ErrorKind::NotFound,
        };
        app.updater.fail(release, e, None);
        return Task::none();
    };
    let http = match github::client() {
        Ok(h) => h,
        Err(e) => {
            app.updater.fail(release, e, None);
            return Task::none();
        }
    };
    tracing::info!("updater: downloading {}", asset.name);
    let total = asset.size;
    let stream = download(http, Release::clone(&release), asset, dir);
    let (task, handle) = Task::run(stream, |e| wrap(Msg::Download(e))).abortable();
    app.updater.on_download_started(release, total, handle);
    task
}

/// Runs the installer for a `Ready` download, unless unsaved admin settings
/// must be discarded first (the dialog replays `InstallReady`).
fn install_ready(app: &mut App) -> Task<Message> {
    if !matches!(app.updater.phase, super::state::Phase::Ready { .. }) {
        return Task::none();
    }
    if app.admin_dirty() && !app.may_leave(wrap(Msg::InstallReady)) {
        return Task::none();
    }
    let (Some(kind), Some((release, file, sha))) =
        (app.updater.kind.clone(), app.updater.begin_install())
    else {
        return Task::none();
    };
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || install::install(&kind, &file, sha, &release))
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!("updater: install task failed: {e}");
                    Err(UpdateError::Cancelled)
                })
        },
        |r| wrap(Msg::Installed(r)),
    )
}

fn installed(app: &mut App, result: Result<Outcome, UpdateError>) -> Task<Message> {
    if !matches!(app.updater.phase, super::state::Phase::Installing { .. }) {
        return Task::none();
    }
    match result {
        Ok(Outcome::Relaunch(r)) => match relaunch::spawn(&r) {
            Ok(()) => {
                tracing::info!("updater: installed, restarting");
                app.quit_now()
            }
            Err(e) => {
                tracing::warn!("updater: installed, but the restart failed: {e}");
                app.updater.on_installed_no_restart();
                Task::none()
            }
        },
        Ok(Outcome::Quit) => {
            tracing::info!("updater: the installer takes over, quitting");
            app.quit_now()
        }
        Err(e) => {
            tracing::warn!("updater: install failed: {e}");
            // A cached file that no longer matches is downloaded anew.
            let remove = match &app.updater.phase {
                super::state::Phase::Installing { file, .. } if e.is_mismatch() => {
                    remove_file(file.clone())
                }
                _ => Task::none(),
            };
            let cancelled = app.updater.on_install_failed(e);
            // A cancelled password prompt puts the release back on offer:
            // not in the mode `off`, which offers nothing.
            if cancelled
                && mode(app) == UpdateMode::Off
                && let Some(h) = app.updater.withdraw(true)
            {
                h.abort();
            }
            let toast = if cancelled {
                app.toast(
                    BannerKind::Warning,
                    t!("updater.auth_cancelled"),
                    String::new(),
                )
            } else {
                Task::none()
            };
            Task::batch([remove, toast])
        }
    }
}

fn remove_file(path: PathBuf) -> Task<Message> {
    Task::perform(
        async move {
            tokio::task::spawn_blocking(move || std::fs::remove_file(&path))
                .await
                .map_err(std::io::Error::other)
                .and_then(|r| r)
        },
        |r| {
            if let Err(e) = r {
                tracing::debug!("updater: cannot remove the download: {e}");
            }
            Message::Noop
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::app::{Flags, SecretsMode};
    use crate::updater::state::Phase;

    fn app() -> App {
        let (app, _) = App::new(Flags {
            ephemeral: true,
            secrets: SecretsMode::Memory,
            ..Flags::default()
        });
        app
    }

    fn release(v: &str) -> Release {
        Release {
            version: semver::Version::parse(v).unwrap(),
            tag: format!("v{v}"),
            html_url: format!(
                "https://github.com/{}/releases/tag/v{v}",
                crate::updater::REPO
            ),
            notes: String::new(),
            prerelease: false,
            assets: Vec::new(),
            sums: None,
        }
    }

    fn send(app: &mut App, msg: Msg) {
        let _ = app.update(Message::Updater(msg));
    }

    fn available(app: &mut App, v: &str, origin: CheckOrigin) {
        assert!(app.updater.start_check(origin));
        send(app, Msg::Checked(origin, Ok(Some(Box::new(release(v))))));
    }

    #[test]
    fn tests_and_snapshots_never_check() {
        let mut app = app();
        assert!(!app.updater.enabled);
        assert!(boot(&app).units() == 0);
        assert!(subscription(&app).is_none());
        send(&mut app, Msg::Check(CheckOrigin::Manual));
        assert!(matches!(app.updater.phase, Phase::Idle));
    }

    #[tokio::test]
    async fn check_results_are_recorded_and_toasted() {
        let mut app = app();
        assert!(app.updater.start_check(CheckOrigin::Manual));
        send(&mut app, Msg::Checked(CheckOrigin::Manual, Ok(None)));
        assert!(app.ui_state.update_last_check.is_some());
        assert_eq!(app.toasts.len(), 1);
        assert_eq!(app.toasts[0].title, t!("updater.up_to_date"));

        app.ui_state.update_last_check = None;
        assert!(app.updater.start_check(CheckOrigin::Tick));
        let e = UpdateError::Network("down".into());
        send(&mut app, Msg::Checked(CheckOrigin::Tick, Err(e)));
        assert!(
            app.ui_state.update_last_check.is_none(),
            "retried next tick"
        );
        assert_eq!(app.toasts.len(), 1, "automatic checks fail silently");

        available(&mut app, "999.0.0", CheckOrigin::Tick);
        assert!(crate::updater::view::card(&app).is_some());
        let _ = app.view();
        send(&mut app, Msg::Skip);
        assert_eq!(app.ui_state.update_skipped.as_deref(), Some("999.0.0"));
        assert!(crate::updater::view::card(&app).is_none());
    }

    #[test]
    fn unmanaged_copies_only_open_the_release_page() {
        let mut app = app();
        available(&mut app, "999.0.0", CheckOrigin::Manual);
        app.updater.kind = Some(InstallKind::Unmanaged);
        send(&mut app, Msg::Install);
        assert!(matches!(app.updater.phase, Phase::Available { .. }));
        assert!(app.updater.download.is_none());
    }

    #[test]
    fn missing_package_fails_with_the_release_page_fallback() {
        let mut app = app();
        app.updater.enabled = true;
        app.updater.kind = Some(InstallKind::AppImage {
            image: "/tmp/x.AppImage".into(),
        });
        available(&mut app, "999.0.0", CheckOrigin::Manual);
        send(&mut app, Msg::Install);
        assert!(matches!(
            app.updater.phase,
            Phase::Failed {
                error: UpdateError::NoAsset,
                file: None,
                ..
            }
        ));
        assert!(crate::updater::view::card(&app).is_some());
        let _ = app.view();
    }

    #[tokio::test]
    async fn download_install_and_cancelled_authentication() {
        let mut app = app();
        app.updater.kind = Some(InstallKind::AppImage {
            image: "/tmp/x.AppImage".into(),
        });
        available(&mut app, "999.0.0", CheckOrigin::Tick);
        let r = app.updater.installable().unwrap().clone();
        let (_t, handle) = Task::<()>::none().abortable();
        app.updater.on_download_started(Arc::clone(&r), 10, handle);
        send(
            &mut app,
            Msg::Download(DlEvent::Progress {
                received: 5,
                total: 10,
            }),
        );
        let _ = app.view();
        // The returned install task is dropped: nothing runs here.
        send(
            &mut app,
            Msg::Download(DlEvent::Done(Ok((PathBuf::from("/c/f"), [0; 32])))),
        );
        assert!(
            matches!(&app.updater.phase, Phase::Installing { file, .. } if file == &PathBuf::from("/c/f"))
        );
        let _ = app.view();
        send(&mut app, Msg::Installed(Err(UpdateError::AuthCancelled)));
        assert!(matches!(app.updater.phase, Phase::Available { .. }));
        assert_eq!(
            app.toasts.last().map(|t| t.title.clone()),
            Some(t!("updater.auth_cancelled").into_owned())
        );
        // A result for a phase that moved on is ignored.
        send(&mut app, Msg::Installed(Ok(Outcome::Quit)));
        assert!(matches!(app.updater.phase, Phase::Available { .. }));
    }

    /// A download that no longer matches its checksum is not offered to
    /// the system handler.
    #[tokio::test]
    async fn checksum_failure_offers_no_file() {
        let mut app = app();
        app.updater.kind = Some(InstallKind::Deb {
            exe: "/usr/bin/rustket".into(),
        });
        available(&mut app, "999.0.0", CheckOrigin::Tick);
        let r = app.updater.installable().unwrap().clone();
        let (_t, handle) = Task::<()>::none().abortable();
        app.updater.on_download_started(r, 10, handle);
        let file = PathBuf::from("/nonexistent/rustket_999.0.0-1_amd64.deb");
        send(&mut app, Msg::Download(DlEvent::Done(Ok((file, [0; 32])))));
        send(&mut app, Msg::Installed(Err(UpdateError::ChecksumMismatch)));
        assert!(matches!(
            app.updater.phase,
            Phase::Failed {
                error: UpdateError::ChecksumMismatch,
                file: None,
                ..
            }
        ));
        let _ = app.view();
    }

    #[test]
    fn prereleases_off_withdraws_an_offered_prerelease() {
        let mut app = app();
        app.config.ui.update_prereleases = true;
        available(&mut app, "999.0.0-rc.1", CheckOrigin::Tick);
        assert!(app.updater.installable().is_some());
        send(&mut app, Msg::PrereleasesOff);
        assert!(matches!(app.updater.phase, Phase::Idle));
        assert!(crate::updater::view::card(&app).is_none());
    }

    // --- the effective update mode (docs/policy.md 9.3) -------------------

    use rustket_core::policy::{Policy, RawValue, Snapshot, SourceId, Trust, resolve};

    use crate::updater::detect::InnoScope;

    fn policy(values: &[(&str, RawValue)]) -> Arc<Policy> {
        let mut snap = Snapshot::new(SourceId::Test("admin".into()), Trust::Full);
        for (k, v) in values {
            snap = snap.with(k, v.clone());
        }
        Arc::new(resolve(&[snap], &crate::policy::context()))
    }

    fn key(mode: Option<&str>) -> Arc<Policy> {
        match mode {
            Some(m) => policy(&[("UpdateMode", RawValue::String(m.into()))]),
            None => Arc::default(),
        }
    }

    fn appimage() -> InstallKind {
        InstallKind::AppImage {
            image: "/tmp/x.AppImage".into(),
        }
    }

    fn inno_all_users() -> InstallKind {
        InstallKind::InnoSetup {
            scope: InnoScope::AllUsers,
            dir: r"C:\Program Files\Rustket".into(),
        }
    }

    /// An app of [`app_with`] and its config directory, removed on drop.
    struct TestApp {
        app: App,
        _dir: tempfile::TempDir,
    }

    impl std::ops::Deref for TestApp {
        type Target = App;
        fn deref(&self) -> &App {
            &self.app
        }
    }

    impl std::ops::DerefMut for TestApp {
        fn deref_mut(&mut self) -> &mut App {
            &mut self.app
        }
    }

    impl Drop for TestApp {
        /// Lets the config writer finish first, so it does not re-create the
        /// directory after `_dir` removed it.
        fn drop(&mut self) {
            self.app.flush_config();
        }
    }

    /// A desktop-like app (the updater on, as `Flags::desktop` turns it on)
    /// under `p`, with the install kind `kind` detected.
    fn app_with(p: Arc<Policy>, kind: InstallKind) -> TestApp {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = rustket_core::config::AppConfig::default();
        crate::legal::accept_terms(&mut cfg);
        // English: the tests read English texts, whatever the system's language.
        cfg.ui.language = rustket_core::config::Language::new("en").unwrap();
        cfg.save(dir.path()).unwrap();
        let (mut app, _) = App::new(Flags {
            config_dir: Some(dir.path().to_owned()),
            cache_dir: Some(dir.path().join("cache")),
            secrets: SecretsMode::Memory,
            policy: p,
            ..Flags::default()
        });
        app.updater.enabled = true;
        app.updater.kind = Some(kind);
        TestApp { app, _dir: dir }
    }

    /// The three modes, each from the key and (for `notify`) from the
    /// default of a copy the user cannot update.
    fn modes() -> Vec<(&'static str, TestApp, UpdateMode)> {
        vec![
            (
                "key unset",
                app_with(key(None), appimage()),
                UpdateMode::Prompt,
            ),
            (
                "prompt",
                app_with(key(Some("prompt")), appimage()),
                UpdateMode::Prompt,
            ),
            (
                "notify",
                app_with(key(Some("notify")), appimage()),
                UpdateMode::Notify,
            ),
            (
                "all users",
                app_with(key(None), inno_all_users()),
                UpdateMode::Notify,
            ),
            (
                "off",
                app_with(key(Some("off")), appimage()),
                UpdateMode::Off,
            ),
        ]
    }

    /// Start-up check, timer and manual check: run in `prompt` and
    /// `notify`, none in `off` (no task at all, so no request to the feed).
    /// The tasks are counted, never run.
    #[tokio::test]
    async fn checks_per_mode() {
        for (name, mut app, want) in modes() {
            assert_eq!(mode(&app), want, "{name}");
            let checks = want != UpdateMode::Off;
            // Boot: the detection, plus the start-up check unless `off`.
            assert_eq!(boot(&app).units(), 1 + usize::from(checks), "{name}");
            assert_eq!(subscription(&app).is_some(), checks, "{name}");
            for origin in [CheckOrigin::Startup, CheckOrigin::Tick, CheckOrigin::Manual] {
                app.updater.phase = Phase::Idle;
                let task = update(&mut app, Msg::Check(origin));
                assert_eq!(task.units() > 0, checks, "{name} {origin:?}");
                assert_eq!(
                    matches!(app.updater.phase, Phase::Checking { .. }),
                    checks,
                    "{name} {origin:?}"
                );
            }
            // A result that arrives in `off` changes nothing, even for a
            // phase that still waits for it.
            if !checks {
                app.updater.phase = Phase::Checking {
                    origin: CheckOrigin::Tick,
                    prev: None,
                };
                let r = Ok(Some(Box::new(release("999.0.0"))));
                assert_eq!(
                    update(&mut app, Msg::Checked(CheckOrigin::Tick, r)).units(),
                    0
                );
                assert!(
                    matches!(app.updater.phase, Phase::Checking { .. }),
                    "{name}"
                );
                assert!(app.ui_state.update_last_check.is_none(), "{name}");
            }
        }
    }

    /// Download and install only in `prompt`: elsewhere "Install", a
    /// download's events, "Install" of a ready file and the file fallback
    /// are refused.
    #[test]
    fn installs_per_mode() {
        for (name, mut app, want) in modes() {
            let installs = want == UpdateMode::Prompt;
            app.updater.phase = Phase::Available {
                release: Arc::new(release("999.0.0")),
                manual: true,
            };
            assert_eq!(app.updater.can_install(want), installs, "{name}");
            // Without a package in the release, an install that starts
            // fails with `NoAsset`, before any request.
            let task = update(&mut app, Msg::Install);
            assert_eq!(task.units(), 0, "{name}");
            if installs {
                assert!(
                    matches!(
                        app.updater.phase,
                        Phase::Failed {
                            error: UpdateError::NoAsset,
                            ..
                        }
                    ),
                    "{name}"
                );
            } else {
                assert!(
                    matches!(app.updater.phase, Phase::Available { .. }),
                    "{name}"
                );
                assert!(app.updater.download.is_none(), "{name}");
            }
            if installs {
                continue;
            }
            // Messages of a download or install that arrive anyway.
            let r = app.updater.installable().unwrap().clone();
            let (_t, handle) = Task::<()>::none().abortable();
            app.updater.on_download_started(Arc::clone(&r), 10, handle);
            let done = DlEvent::Done(Ok((PathBuf::from("/c/f"), [0; 32])));
            assert_eq!(update(&mut app, Msg::Download(done)).units(), 0, "{name}");
            assert!(
                matches!(app.updater.phase, Phase::Downloading { .. }),
                "{name}"
            );
            app.updater.phase = Phase::Ready {
                release: r,
                file: PathBuf::from("/c/f"),
                sha: [0; 32],
            };
            assert_eq!(update(&mut app, Msg::InstallReady).units(), 0, "{name}");
            assert!(matches!(app.updater.phase, Phase::Ready { .. }), "{name}");
            let open = Msg::OpenFile(PathBuf::from("/c/f.deb"));
            assert_eq!(update(&mut app, open).units(), 0, "{name}");
        }
    }

    /// An installation for all users without the key reports a new version
    /// with the release page and never installs; one for the current user
    /// installs as before.
    #[test]
    fn all_users_default_reports_and_does_not_install() {
        rust_i18n::set_locale("en");
        let mut app = app_with(key(None), inno_all_users());
        available(&mut app, "999.0.0", CheckOrigin::Tick);
        assert!(crate::updater::view::card(&app).is_some());
        let texts = crate::tree_probe::texts(crate::updater::view::card(&app).unwrap());
        assert!(
            texts.iter().any(|t| t == "Version 999.0.0 is available. Ask your administrator to install it."),
            "{texts:?}"
        );
        let install = t!("updater.install").into_owned();
        assert!(!texts.contains(&install), "{texts:?}");
        // Its action opens the release page, and says so.
        assert!(texts.iter().any(|t| t == "Release page"), "{texts:?}");
        assert!(!texts.iter().any(|t| t == "Download"), "{texts:?}");
        // The release page once, not again as "Release notes".
        let notes = t!("updater.release_notes").into_owned();
        assert!(!texts.contains(&notes), "{texts:?}");
        let d = crate::updater::view::settings_data(&app.updater, mode(&app), None, None, None);
        assert!(d.install.is_none() && d.release_page && d.check && d.managed.is_none());
        let _ = update(&mut app, Msg::Install);
        assert!(matches!(app.updater.phase, Phase::Available { .. }));
        assert!(app.updater.download.is_none());

        let mut app = app_with(
            key(None),
            InstallKind::InnoSetup {
                scope: InnoScope::CurrentUser,
                dir: "C:/R".into(),
            },
        );
        assert_eq!(mode(&app), UpdateMode::Prompt);
        available(&mut app, "999.0.0", CheckOrigin::Tick);
        let texts = crate::tree_probe::texts(crate::updater::view::card(&app).unwrap());
        assert!(texts.contains(&install), "{texts:?}");
        assert!(texts.contains(&notes), "{texts:?}");
        assert!(
            !texts.iter().any(|t| t.contains("Ask your administrator")),
            "{texts:?}"
        );
        let d = crate::updater::view::settings_data(&app.updater, mode(&app), None, None, None);
        assert_eq!(d.install, Some(install));
        assert!(!d.release_page);
    }

    /// A copy without an install kind marker (or not detected yet) under
    /// `prompt` keeps the release notes link next to "Download", as without
    /// policy.
    #[test]
    fn unmanaged_copy_keeps_release_notes() {
        rust_i18n::set_locale("en");
        let mut app = app_with(key(None), InstallKind::Unmanaged);
        assert_eq!(mode(&app), UpdateMode::Prompt);
        available(&mut app, "999.0.0", CheckOrigin::Tick);
        let notes = t!("updater.release_notes").into_owned();
        let texts = crate::tree_probe::texts(crate::updater::view::card(&app).unwrap());
        assert!(texts.contains(&notes), "{texts:?}");
        assert!(texts.iter().any(|t| t == "Download"), "{texts:?}");
        app.updater.kind = None;
        let texts = crate::tree_probe::texts(crate::updater::view::card(&app).unwrap());
        assert!(texts.contains(&notes), "{texts:?}");
    }

    /// A failure left by an earlier run, delivered with the install kind,
    /// is shown only in `prompt` (its card offers the downloaded file).
    #[test]
    fn stale_failure_at_detection_only_in_prompt() {
        rust_i18n::set_locale("en");
        let stale = || {
            Some(StaleFailure {
                version: semver::Version::new(0, 3, 0),
                error: UpdateError::NoAuthAgent,
                file: Some(PathBuf::from("/c/rustket_0.3.0-1_amd64.deb")),
            })
        };
        let open = t!("updater.open_package").into_owned();
        for (name, p, kind, shown) in [
            ("notify", key(Some("notify")), appimage(), false),
            ("off", key(Some("off")), appimage(), false),
            ("all users", key(None), inno_all_users(), false),
            ("prompt", key(Some("prompt")), appimage(), true),
        ] {
            let mut app = app_with(p, appimage());
            app.updater.kind = None;
            let _ = update(&mut app, Msg::Detected(kind, stale()));
            assert_eq!(
                matches!(app.updater.phase, Phase::Failed { .. }),
                shown,
                "{name}"
            );
            let texts = crate::updater::view::card(&app)
                .map(crate::tree_probe::texts)
                .unwrap_or_default();
            assert_eq!(texts.contains(&open), shown, "{name}: {texts:?}");
        }
    }

    /// Leaving `prompt` at a refresh cancels a download; `notify` keeps
    /// what was on offer as the report of its release, `off` drops it and a
    /// check under way. A running install is left alone. Coming back from
    /// `off` requests nothing by itself.
    #[tokio::test]
    async fn refresh_transitions() {
        let reported = |app: &App| {
            matches!(&app.updater.phase, Phase::Available { release, .. }
                if release.version.to_string() == "999.0.0")
        };
        for to in ["notify", "off"] {
            let off = to == "off";
            // An offer: the report in `notify`, nothing in `off`.
            let mut app = app_with(key(Some("prompt")), appimage());
            available(&mut app, "999.0.0", CheckOrigin::Tick);
            let _ = app.set_policy(key(Some(to)));
            if off {
                assert!(matches!(app.updater.phase, Phase::Idle), "{to}");
                assert!(crate::updater::view::card(&app).is_none(), "{to}");
            } else {
                assert!(reported(&app), "{to}");
                let texts = crate::tree_probe::texts(crate::updater::view::card(&app).unwrap());
                assert!(
                    texts.iter().any(|t| t.contains("Ask your administrator")),
                    "{texts:?}"
                );
                assert!(!texts.contains(&t!("updater.install").into_owned()));
                assert!(!texts.contains(&t!("updater.release_notes").into_owned()));
            }

            // A download in progress: cancelled.
            let mut app = app_with(key(None), appimage());
            available(&mut app, "999.0.0", CheckOrigin::Tick);
            let r = app.updater.installable().unwrap().clone();
            let (task, handle) = Task::<()>::none().abortable();
            app.updater.on_download_started(r, 10, handle);
            let _ = app.set_policy(key(Some(to)));
            assert_eq!(matches!(app.updater.phase, Phase::Idle), off, "{to}");
            assert_eq!(reported(&app), !off, "{to}");
            assert!(app.updater.download.is_none(), "{to}");
            drop(task);

            // A running install is not interrupted; its result still counts.
            let mut app = app_with(key(None), appimage());
            app.updater.phase = Phase::Installing {
                release: Arc::new(release("999.0.0")),
                file: PathBuf::from("/c/f"),
            };
            let _ = app.set_policy(key(Some(to)));
            assert!(
                matches!(app.updater.phase, Phase::Installing { .. }),
                "{to}"
            );
            assert!(crate::updater::view::card(&app).is_some(), "{to}");
            let _ = update(&mut app, Msg::Installed(Err(UpdateError::NoAuthAgent)));
            assert!(
                matches!(app.updater.phase, Phase::Failed { file: Some(_), .. }),
                "{to}"
            );
            // Outside `prompt` the failure offers the release page, never
            // the downloaded file.
            let texts = crate::tree_probe::texts(crate::updater::view::card(&app).unwrap());
            assert!(
                texts.contains(&t!("updater.release_page").into_owned()),
                "{to}: {texts:?}"
            );
            for file_action in [t!("updater.open_package"), t!("updater.show_file")] {
                assert!(
                    !texts.contains(&file_action.into_owned()),
                    "{to}: {texts:?}"
                );
            }

            // A cancelled password prompt: the release is reported again in
            // `notify`; `off` offers nothing.
            let mut app = app_with(key(None), appimage());
            app.updater.phase = Phase::Installing {
                release: Arc::new(release("999.0.0")),
                file: PathBuf::from("/c/f"),
            };
            let _ = app.set_policy(key(Some(to)));
            let _ = update(&mut app, Msg::Installed(Err(UpdateError::AuthCancelled)));
            assert_eq!(matches!(app.updater.phase, Phase::Idle), off, "{to}");
            assert_eq!(reported(&app), !off, "{to}");
            assert_eq!(crate::updater::view::card(&app).is_none(), off, "{to}");
        }
        // `notify` to `off`: a report and a check under way go.
        let mut app = app_with(key(Some("notify")), appimage());
        available(&mut app, "999.0.0", CheckOrigin::Tick);
        assert!(app.updater.start_check(CheckOrigin::Tick));
        let _ = app.set_policy(key(Some("off")));
        assert!(matches!(app.updater.phase, Phase::Idle));
        // `prompt` to `notify` during a check: the offer it kept stays as
        // the report, the check goes on and reports.
        let mut app = app_with(key(None), appimage());
        available(&mut app, "999.0.0", CheckOrigin::Tick);
        assert!(app.updater.start_check(CheckOrigin::Tick));
        let _ = app.set_policy(key(Some("notify")));
        assert!(
            matches!(&app.updater.phase, Phase::Checking { prev: Some(p), .. }
                if matches!(**p, Phase::Available { .. }))
        );
        let r = Ok(Some(Box::new(release("999.0.0"))));
        let _ = update(&mut app, Msg::Checked(CheckOrigin::Tick, r));
        assert!(reported(&app));

        // From `off`: no check starts with the change; the timer is back.
        let mut app = app_with(key(Some("off")), appimage());
        assert!(subscription(&app).is_none());
        let _ = app.set_policy(key(Some("notify")));
        assert!(matches!(app.updater.phase, Phase::Idle));
        assert!(subscription(&app).is_some());
        let _ = app.set_policy(key(Some("prompt")));
        assert!(matches!(app.updater.phase, Phase::Idle));
    }

    /// The detected kind can change the mode: an offer made before an
    /// installation for all users was detected becomes its report.
    #[test]
    fn detection_into_notify_reports_an_offer() {
        let mut app = app_with(key(None), appimage());
        app.updater.kind = None;
        available(&mut app, "999.0.0", CheckOrigin::Manual);
        let _ = update(&mut app, Msg::Detected(inno_all_users(), None));
        assert_eq!(mode(&app), UpdateMode::Notify);
        assert!(matches!(app.updater.phase, Phase::Available { .. }));
        assert!(!app.updater.can_install(mode(&app)));
        let _ = update(&mut app, Msg::Install);
        assert!(app.updater.download.is_none());
        // The key keeps `prompt`: nothing changes.
        let mut app = app_with(key(Some("prompt")), appimage());
        app.updater.kind = None;
        available(&mut app, "999.0.0", CheckOrigin::Manual);
        let _ = update(&mut app, Msg::Detected(inno_all_users(), None));
        assert!(matches!(app.updater.phase, Phase::Available { .. }));
        assert!(app.updater.can_install(mode(&app)));
    }

    /// The About tab's update card per mode, rendered headless. The corner
    /// card is hidden first ("Later" keeps the release known), so only the
    /// About card can satisfy the assertions.
    #[test]
    fn about_card_per_mode() {
        use crate::component::Nav;
        use crate::message::Go;
        rust_i18n::set_locale("en");
        let about = |app: &mut App| {
            let known = app.updater.release().map(|r| r.version.clone());
            send(app, Msg::Later);
            assert!(crate::updater::view::card(app).is_none());
            assert_eq!(app.updater.release().map(|r| r.version.clone()), known);
            let _ = app.update(Message::Go(Go::Nav(Nav::AboutSettings)));
            assert!(matches!(app.screen, crate::app::Screen::Settings(_)));
            crate::tree_probe::texts(app.view())
        };
        let has = |t: &[String], s: &str| t.iter().any(|x| x == s);
        let ask = "Version 999.0.0 is available. Ask your administrator to install it.";
        let install = t!("updater.install").into_owned();

        // `off`: the line, no "Check now", no release.
        let mut app = app_with(key(Some("off")), appimage());
        let t = about(&mut app);
        assert!(
            has(&t, "Your organization manages updates of Rustket"),
            "{t:?}"
        );
        assert!(!has(&t, "Check now"), "{t:?}");
        let org = policy(&[
            ("UpdateMode", RawValue::String("off".into())),
            ("OrganizationName", RawValue::String("Acme".into())),
        ]);
        let mut app = app_with(org, appimage());
        let t = about(&mut app);
        assert!(has(&t, "Acme manages updates of Rustket"), "{t:?}");
        assert!(!has(&t, "Check now"), "{t:?}");

        // `notify`, from the key and from the all-users default.
        for mut app in [
            app_with(key(Some("notify")), appimage()),
            app_with(key(None), inno_all_users()),
        ] {
            available(&mut app, "999.0.0", CheckOrigin::Manual);
            let t = about(&mut app);
            assert!(has(&t, "Check now"), "{t:?}");
            assert!(has(&t, ask), "{t:?}");
            assert!(has(&t, "Release page"), "{t:?}");
            // The About card has no install button.
            assert!(!has(&t, &install), "{t:?}");
            assert!(!t.iter().any(|x| x.contains("manages updates")), "{t:?}");
        }

        // `prompt`: as before.
        let mut app = app_with(key(None), appimage());
        available(&mut app, "999.0.0", CheckOrigin::Manual);
        let t = about(&mut app);
        assert!(has(&t, "Check now") && has(&t, &install), "{t:?}");
        assert!(has(&t, "Version 999.0.0 is available"), "{t:?}");
        assert!(!has(&t, ask) && !has(&t, "Release page"), "{t:?}");
    }

    /// The new strings exist in every complete locale and carry none of the
    /// markers the Mac App Store binary must not contain.
    #[test]
    fn mode_strings_are_translated() {
        let entries = crate::i18n::tests::locale_entries();
        for key in [
            "updater.kind_msi",
            "updater.kind_pkg",
            "updater.notify_available",
            "updater.managed_off",
            "updater.managed_off_by",
        ] {
            let texts = entries.get(key).unwrap_or_else(|| panic!("{key}"));
            for loc in crate::i18n::tests::BUNDLED_COMPLETE {
                let text = texts.get(*loc).unwrap_or_else(|| panic!("{key}: {loc}"));
                assert!(!text.is_empty(), "{key}: {loc}");
                for marker in [
                    "api.github.com",
                    "install-kind",
                    "SHA256SUMS",
                    "didDismissAlert",
                ] {
                    assert!(!text.contains(marker), "{key}: {loc}");
                }
            }
        }
    }

    /// A policy that forces pre-releases off withdraws an offered one, as
    /// the settings switch does.
    #[test]
    fn policy_forcing_prereleases_off_withdraws_an_offered_prerelease() {
        use rustket_core::policy::{RawValue, Snapshot, SourceId, Trust, resolve};
        let mut app = app();
        app.config.ui.update_prereleases = true;
        available(&mut app, "999.0.0-rc.1", CheckOrigin::Tick);
        assert!(app.updater.installable().is_some());
        let snap = Snapshot::new(SourceId::Test("t".into()), Trust::Full)
            .with("UpdateMode", RawValue::String("prompt".into()));
        let policy = resolve(&[snap], &crate::policy::context());
        let _ = app.set_policy(std::sync::Arc::new(policy));
        assert!(!app.config.ui.update_prereleases);
        assert!(matches!(app.updater.phase, Phase::Idle));
        assert!(crate::updater::view::card(&app).is_none());
    }
}
