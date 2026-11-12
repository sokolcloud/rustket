//! App wiring: messages, the boot hook, the hourly subscription and the
//! tasks that check, download, install and relaunch.

use std::path::PathBuf;

use iced::{Subscription, Task};
use rust_i18n::t;
use rustket_ui::widget::BannerKind;

use super::download::{DlEvent, download};
use super::github::{self, Endpoints, Release};
use super::install::{self, Outcome};
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

/// After boot (desktop runs only): detect the install kind, clean up old
/// downloads and schedule the startup check.
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
    let startup = if app.config.ui.check_updates {
        Task::perform(tokio::time::sleep(STARTUP_DELAY), |_| {
            wrap(Msg::Check(CheckOrigin::Startup))
        })
    } else {
        Task::none()
    };
    Task::batch([detect, startup])
}

/// The hourly tick, while automatic checks are on.
pub fn subscription(app: &App) -> Option<Subscription<Message>> {
    (app.updater.enabled && app.config.ui.check_updates)
        .then(|| iced::time::every(TICK).map(|_| wrap(Msg::Tick)))
}

pub fn update(app: &mut App, msg: Msg) -> Task<Message> {
    match msg {
        Msg::Detected(kind, stale) => {
            app.updater.kind = Some(kind);
            if let Some(f) = stale {
                tracing::warn!(version = %f.version, error = %f.error, "updater: the last install failed");
                app.updater.on_stale_failure(f);
            }
            Task::none()
        }
        Msg::Tick => check(app, CheckOrigin::Tick),
        Msg::Check(origin) => check(app, origin),
        Msg::Checked(origin, result) => checked(app, origin, result.map(|r| r.map(|b| *b))),
        Msg::Install => start_install(app),
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

fn check(app: &mut App, origin: CheckOrigin) -> Task<Message> {
    let u = &app.updater;
    if !u.enabled || current_version().is_none() {
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

fn start_install(app: &mut App) -> Task<Message> {
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
            let toast = if app.updater.on_install_failed(e) {
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
}
