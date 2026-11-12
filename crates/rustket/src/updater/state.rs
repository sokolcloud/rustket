//! The updater's state machine: pure reducers, no I/O (`updater::app` runs
//! the tasks).

use std::path::PathBuf;
use std::sync::Arc;

use semver::Version;

use super::detect::InstallKind;
use super::github::Release;
use super::relaunch::StaleFailure;
use super::version::offer;
use super::{CHECK_INTERVAL, REPO, STARTUP_MIN_AGE, UpdateError};

/// What started a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOrigin {
    /// Shortly after boot (`STARTUP_DELAY`).
    Startup,
    /// The hourly subscription.
    Tick,
    /// "Check now" in Settings.
    Manual,
}

#[derive(Debug, Clone, Default)]
pub enum Phase {
    #[default]
    Idle,
    /// `prev`: an offer, a failed install or an installed release the
    /// check started from; it stays shown and comes back when the check
    /// fails.
    Checking {
        origin: CheckOrigin,
        prev: Option<Box<Phase>>,
    },
    UpToDate,
    CheckFailed(UpdateError),
    /// `manual`: found by "Check now" (shown even when skipped).
    Available {
        release: Arc<Release>,
        manual: bool,
    },
    Downloading {
        release: Arc<Release>,
        received: u64,
        total: u64,
    },
    /// Downloaded and verified; the install waits for unsaved admin
    /// settings to be discarded (or another click).
    Ready {
        release: Arc<Release>,
        file: PathBuf,
        sha: [u8; 32],
    },
    Installing {
        release: Arc<Release>,
        file: PathBuf,
    },
    /// `release`: `None` for a failure reported by an earlier run (macOS
    /// helper), which knows only the version.
    Failed {
        release: Option<Arc<Release>>,
        version: Version,
        error: UpdateError,
        file: Option<PathBuf>,
    },
    /// Installed, but the new process could not be started.
    InstalledNoRestart {
        release: Arc<Release>,
    },
}

/// What the app does after a check result.
#[derive(Debug, Clone, PartialEq)]
pub struct CheckResult {
    /// Store the check time (not after network errors or rate limits, so
    /// the next tick retries).
    pub record: bool,
    /// For manual checks: what to toast.
    pub notice: Option<Notice>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    UpToDate,
    Available,
    Error(UpdateError),
}

#[derive(Debug, Default)]
pub struct Updater {
    /// Real desktop run (`Flags::desktop`); tests and snapshots never check.
    pub enabled: bool,
    /// `None` until detected.
    pub kind: Option<InstallKind>,
    pub phase: Phase,
    /// Unix seconds before which no check runs (GitHub rate limit reset).
    pub not_before: Option<i64>,
    /// "Later": the card of this version stays hidden for this run.
    pub hidden: Option<Version>,
    pub download: Option<iced::task::Handle>,
}

fn secs(d: std::time::Duration) -> i64 {
    i64::try_from(d.as_secs()).unwrap_or(i64::MAX)
}

/// The repository's release list, for failures without a release.
pub fn releases_page() -> String {
    format!("https://github.com/{REPO}/releases")
}

impl Updater {
    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            ..Self::default()
        }
    }

    /// A check, download or install is running.
    pub fn busy(&self) -> bool {
        matches!(
            self.phase,
            Phase::Checking { .. }
                | Phase::Downloading { .. }
                | Phase::Ready { .. }
                | Phase::Installing { .. }
        )
    }

    /// Whether a check of `origin` should run now. Manual checks only wait
    /// for a running one; automatic checks also leave the card of a failed
    /// or finished install alone until it is dismissed, and keep to the
    /// interval and the rate limit.
    pub fn due(&self, now: i64, last: Option<i64>, origin: CheckOrigin) -> bool {
        if self.busy() {
            return false;
        }
        if origin == CheckOrigin::Manual {
            return true;
        }
        if matches!(
            self.phase,
            Phase::Failed { .. } | Phase::InstalledNoRestart { .. }
        ) && self.hidden.as_ref() != self.version()
        {
            return false;
        }
        if self.not_before.is_some_and(|t| now < t) {
            return false;
        }
        let min_age = if origin == CheckOrigin::Startup {
            secs(STARTUP_MIN_AGE)
        } else {
            secs(CHECK_INTERVAL)
        };
        // A clock set back makes `now - l` negative: check then.
        last.is_none_or(|l| now - l >= min_age || now < l)
    }

    /// The phase the card and the actions are about: during a check, the
    /// one it started from.
    pub fn shown(&self) -> &Phase {
        match &self.phase {
            Phase::Checking {
                prev: Some(prev), ..
            } => prev,
            p => p,
        }
    }

    /// The release the phase is about.
    pub fn release(&self) -> Option<&Arc<Release>> {
        match self.shown() {
            Phase::Available { release, .. }
            | Phase::Downloading { release, .. }
            | Phase::Ready { release, .. }
            | Phase::Installing { release, .. }
            | Phase::InstalledNoRestart { release } => Some(release),
            Phase::Failed { release, .. } => release.as_ref(),
            _ => None,
        }
    }

    /// The version the phase is about.
    pub fn version(&self) -> Option<&Version> {
        match self.shown() {
            Phase::Failed { version, .. } => Some(version),
            _ => self.release().map(|r| &r.version),
        }
    }

    /// A release "Install" (or "Download") can start from.
    pub fn installable(&self) -> Option<&Arc<Release>> {
        match self.shown() {
            Phase::Available { release, .. } => Some(release),
            Phase::Failed {
                release: Some(release),
                ..
            } => Some(release),
            _ => None,
        }
    }

    /// Whether this copy installs updates itself (else "Download" opens the
    /// release page).
    pub fn can_install(&self) -> bool {
        self.kind.as_ref().is_some_and(InstallKind::can_install)
    }

    /// Whether the corner card shows. `skipped`: `UiState::update_skipped`.
    pub fn card_visible(&self, skipped: Option<&str>) -> bool {
        let hidden = |v: &Version| self.hidden.as_ref() == Some(v);
        match self.shown() {
            Phase::Available { release, manual } => {
                let v = &release.version;
                !hidden(v) && (*manual || skipped != Some(v.to_string().as_str()))
            }
            Phase::Downloading { .. } | Phase::Ready { .. } | Phase::Installing { .. } => true,
            Phase::Failed { version, .. } => !hidden(version),
            Phase::InstalledNoRestart { release } => !hidden(&release.version),
            Phase::Idle | Phase::Checking { .. } | Phase::UpToDate | Phase::CheckFailed(_) => false,
        }
    }

    /// Starts a check; `false` while one (or a download/install) runs.
    pub fn start_check(&mut self, origin: CheckOrigin) -> bool {
        if self.busy() {
            return false;
        }
        let prev = match std::mem::take(&mut self.phase) {
            p @ (Phase::Available { .. }
            | Phase::Failed {
                release: Some(_), ..
            }
            | Phase::InstalledNoRestart { .. }) => Some(Box::new(p)),
            _ => None,
        };
        self.phase = Phase::Checking { origin, prev };
        true
    }

    /// A check finished. `current`: the running version.
    pub fn on_checked(
        &mut self,
        origin: CheckOrigin,
        result: Result<Option<Release>, UpdateError>,
        current: &Version,
        prereleases: bool,
    ) -> CheckResult {
        let manual = origin == CheckOrigin::Manual;
        let Phase::Checking { prev, .. } = &mut self.phase else {
            return CheckResult {
                record: false,
                notice: None,
            };
        };
        let prev = prev.take().map(|p| *p);
        // Installed but not restarted: only something newer than that counts.
        let installed = match &prev {
            Some(Phase::InstalledNoRestart { release }) => Some(release.version.clone()),
            _ => None,
        };
        match result {
            Ok(Some(r))
                if offer(&r.version, current, prereleases)
                    && installed.as_ref().is_none_or(|v| r.version > *v) =>
            {
                if manual {
                    self.hidden = None;
                }
                self.phase = Phase::Available {
                    release: Arc::new(r),
                    manual,
                };
                CheckResult {
                    record: true,
                    notice: manual.then_some(Notice::Available),
                }
            }
            Ok(_) => {
                self.phase = match prev {
                    Some(p @ Phase::InstalledNoRestart { .. }) => p,
                    _ => Phase::UpToDate,
                };
                CheckResult {
                    record: true,
                    notice: manual.then_some(Notice::UpToDate),
                }
            }
            Err(e) => {
                if let UpdateError::RateLimited { reset } = &e {
                    self.not_before = *reset;
                }
                let record =
                    !matches!(e, UpdateError::Network(_) | UpdateError::RateLimited { .. });
                self.phase = prev.unwrap_or_else(|| Phase::CheckFailed(e.clone()));
                CheckResult {
                    record,
                    notice: manual.then_some(Notice::Error(e)),
                }
            }
        }
    }

    /// A download started for `release` (`total`: asset size).
    pub fn on_download_started(
        &mut self,
        release: Arc<Release>,
        total: u64,
        handle: iced::task::Handle,
    ) {
        self.phase = Phase::Downloading {
            release,
            received: 0,
            total,
        };
        self.download = Some(handle);
    }

    pub fn on_progress(&mut self, got: u64, size: u64) {
        if let Phase::Downloading {
            received, total, ..
        } = &mut self.phase
        {
            *received = got;
            *total = size;
        }
    }

    /// The download finished. Returns `true` when the file is ready for
    /// install (the phase is `Ready`); a stale result (after Cancel) is
    /// dropped.
    pub fn on_downloaded(&mut self, result: Result<(PathBuf, [u8; 32]), UpdateError>) -> bool {
        let Phase::Downloading { release, .. } = &self.phase else {
            return false;
        };
        let release = release.clone();
        self.download = None;
        match result {
            Ok((file, sha)) => {
                self.phase = Phase::Ready { release, file, sha };
                true
            }
            Err(e) => {
                self.fail(release, e, None);
                false
            }
        }
    }

    /// `Ready` → `Installing`; the file and digest to install.
    pub fn begin_install(&mut self) -> Option<(Arc<Release>, PathBuf, [u8; 32])> {
        let Phase::Ready { release, file, sha } = &self.phase else {
            return None;
        };
        let out = (release.clone(), file.clone(), *sha);
        self.phase = Phase::Installing {
            release: out.0.clone(),
            file: out.1.clone(),
        };
        Some(out)
    }

    /// The installer failed. Cancelled authentication goes back to
    /// `Available` (`true`: tell the user). A file that no longer matches
    /// its checksum is not offered as a fallback.
    pub fn on_install_failed(&mut self, e: UpdateError) -> bool {
        let Phase::Installing { release, file } = &self.phase else {
            return false;
        };
        let (release, file) = (release.clone(), file.clone());
        if e == UpdateError::AuthCancelled {
            self.phase = Phase::Available {
                release,
                manual: true,
            };
            return true;
        }
        let file = (!e.is_mismatch()).then_some(file);
        self.fail(release, e, file);
        false
    }

    /// Installed, but the relaunch failed.
    pub fn on_installed_no_restart(&mut self) {
        if let Phase::Installing { release, .. } = &self.phase {
            self.phase = Phase::InstalledNoRestart {
                release: release.clone(),
            };
        }
    }

    pub fn fail(&mut self, release: Arc<Release>, error: UpdateError, file: Option<PathBuf>) {
        self.phase = Phase::Failed {
            version: release.version.clone(),
            release: Some(release),
            error,
            file,
        };
    }

    /// A failure an earlier run left behind (macOS helper).
    pub fn on_stale_failure(&mut self, f: StaleFailure) {
        if matches!(self.phase, Phase::Idle) {
            self.phase = Phase::Failed {
                release: None,
                version: f.version,
                error: f.error,
                file: f.file,
            };
        }
    }

    /// Cancel: back to `Available`. Returns the handle to abort.
    pub fn cancel_download(&mut self) -> Option<iced::task::Handle> {
        let Phase::Downloading { release, .. } = &self.phase else {
            return None;
        };
        self.phase = Phase::Available {
            release: release.clone(),
            manual: true,
        };
        self.download.take()
    }

    /// "Include pre-releases" was switched off: a pre-release on offer (or
    /// whose install failed) is withdrawn. Downloads and installs the user
    /// started are left alone.
    pub fn drop_prereleases(&mut self) {
        let pre = |p: &Phase| match p {
            Phase::Available { release, .. }
            | Phase::Failed {
                release: Some(release),
                ..
            } => !release.version.pre.is_empty(),
            _ => false,
        };
        match &mut self.phase {
            Phase::Checking { prev, .. } if prev.as_deref().is_some_and(pre) => *prev = None,
            p if pre(p) => *p = Phase::Idle,
            _ => {}
        }
    }

    /// "Later" / the card's close button: hides the card of this version for
    /// this run. A downloaded file waits for the next "Install".
    pub fn later(&mut self) {
        if let Phase::Ready { release, .. } = &self.phase {
            self.phase = Phase::Available {
                release: release.clone(),
                manual: false,
            };
        }
        if let Phase::Failed { release: None, .. } = &self.phase {
            self.phase = Phase::Idle;
            return;
        }
        self.hidden = self.version().cloned();
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    const H: i64 = 3600;

    fn release(v: &str) -> Release {
        Release {
            version: Version::parse(v).unwrap(),
            tag: format!("v{v}"),
            html_url: format!("https://github.com/{REPO}/releases/tag/v{v}"),
            notes: String::new(),
            prerelease: false,
            assets: Vec::new(),
            sums: None,
        }
    }

    fn cur() -> Version {
        Version::parse("0.2.2").unwrap()
    }

    fn available(u: &mut Updater, v: &str, origin: CheckOrigin) {
        assert!(u.start_check(origin));
        u.on_checked(origin, Ok(Some(release(v))), &cur(), false);
    }

    fn handle() -> iced::task::Handle {
        let (_task, handle) = iced::Task::<()>::none().abortable();
        handle
    }

    #[test]
    fn due_by_origin_interval_and_rate_limit() {
        let u = Updater::new(true);
        let now = 100 * H;
        for origin in [CheckOrigin::Startup, CheckOrigin::Tick, CheckOrigin::Manual] {
            assert!(u.due(now, None, origin), "{origin:?}");
        }
        // Startup: at least an hour since the last check.
        assert!(!u.due(now, Some(now - H + 1), CheckOrigin::Startup));
        assert!(u.due(now, Some(now - H), CheckOrigin::Startup));
        // Tick: the 12 h interval.
        assert!(!u.due(now, Some(now - 11 * H), CheckOrigin::Tick));
        assert!(u.due(now, Some(now - 12 * H), CheckOrigin::Tick));
        assert!(
            u.due(now, Some(now + H), CheckOrigin::Tick),
            "clock set back"
        );
        assert!(u.due(now, Some(now), CheckOrigin::Manual));
        // Rate limit reset.
        let mut u = Updater::new(true);
        u.not_before = Some(now + 10);
        assert!(!u.due(now, None, CheckOrigin::Tick));
        assert!(u.due(now + 10, None, CheckOrigin::Tick));
        assert!(u.due(now, None, CheckOrigin::Manual));
        // Busy, failed or installed: no automatic check.
        let mut u = Updater::new(true);
        u.phase = Phase::Checking {
            origin: CheckOrigin::Tick,
            prev: None,
        };
        assert!(!u.due(now, None, CheckOrigin::Manual));
        u.phase = Phase::InstalledNoRestart {
            release: Arc::new(release("0.3.0")),
        };
        assert!(!u.due(now, None, CheckOrigin::Tick));
        assert!(u.due(now, None, CheckOrigin::Manual));
        // ... until its card is dismissed.
        u.later();
        assert!(u.due(now, None, CheckOrigin::Tick));
        u.phase = Phase::Failed {
            release: Some(Arc::new(release("0.3.0"))),
            version: Version::parse("0.3.0").unwrap(),
            error: UpdateError::NoAsset,
            file: None,
        };
        u.hidden = None;
        assert!(!u.due(now, None, CheckOrigin::Startup));
        u.later();
        assert!(u.due(now, None, CheckOrigin::Startup));
    }

    /// A re-check keeps what is on offer: the card stays during the check
    /// and a failed check does not withdraw it.
    #[test]
    fn recheck_keeps_the_offer() {
        let mut u = Updater::new(true);
        available(&mut u, "0.3.0", CheckOrigin::Tick);
        assert!(u.start_check(CheckOrigin::Tick));
        assert!(u.busy() && u.card_visible(None));
        assert_eq!(u.installable().unwrap().version.to_string(), "0.3.0");
        let r = u.on_checked(
            CheckOrigin::Tick,
            Err(UpdateError::Http(502)),
            &cur(),
            false,
        );
        assert!(r.record);
        assert!(
            matches!(&u.phase, Phase::Available { release, manual: false }
            if release.version.to_string() == "0.3.0")
        );
        assert!(u.card_visible(None) && u.installable().is_some());
        // A failed manual check: toasted, the offer stays.
        u.start_check(CheckOrigin::Manual);
        let e = UpdateError::Network("x".into());
        let r = u.on_checked(CheckOrigin::Manual, Err(e.clone()), &cur(), false);
        assert_eq!(r.notice, Some(Notice::Error(e)));
        assert!(matches!(u.phase, Phase::Available { .. }));
        // A newer release replaces it; no release at all withdraws it.
        available(&mut u, "0.3.1", CheckOrigin::Tick);
        assert_eq!(u.version().unwrap().to_string(), "0.3.1");
        u.start_check(CheckOrigin::Tick);
        u.on_checked(CheckOrigin::Tick, Ok(None), &cur(), false);
        assert!(matches!(u.phase, Phase::UpToDate));
        // Without anything on offer a failure shows.
        u.start_check(CheckOrigin::Tick);
        assert!(!u.card_visible(None));
        u.on_checked(
            CheckOrigin::Tick,
            Err(UpdateError::Http(502)),
            &cur(),
            false,
        );
        assert!(matches!(u.phase, Phase::CheckFailed(_)));
    }

    /// A dismissed failure lets automatic checks find a fixed release.
    #[test]
    fn dismissed_failure_and_installed_release_recheck() {
        let mut u = Updater::new(true);
        available(&mut u, "0.3.0", CheckOrigin::Tick);
        let r = u.installable().unwrap().clone();
        u.fail(r, UpdateError::NoAsset, None);
        u.later();
        assert!(u.due(0, None, CheckOrigin::Tick));
        // The same version again stays hidden; an error keeps the failure.
        u.start_check(CheckOrigin::Tick);
        assert!(!u.card_visible(None));
        u.on_checked(
            CheckOrigin::Tick,
            Err(UpdateError::Http(500)),
            &cur(),
            false,
        );
        assert!(matches!(u.phase, Phase::Failed { .. }) && !u.card_visible(None));
        available(&mut u, "0.3.0", CheckOrigin::Tick);
        assert!(!u.card_visible(None));
        available(&mut u, "0.3.1", CheckOrigin::Tick);
        assert!(u.card_visible(None));

        // Installed, not restarted: only a newer release replaces that.
        u.phase = Phase::InstalledNoRestart {
            release: Arc::new(release("0.3.1")),
        };
        u.later();
        u.start_check(CheckOrigin::Tick);
        u.on_checked(CheckOrigin::Tick, Ok(Some(release("0.3.1"))), &cur(), false);
        assert!(matches!(u.phase, Phase::InstalledNoRestart { .. }));
        u.start_check(CheckOrigin::Tick);
        u.on_checked(CheckOrigin::Tick, Ok(None), &cur(), false);
        assert!(matches!(u.phase, Phase::InstalledNoRestart { .. }));
        available(&mut u, "0.3.2", CheckOrigin::Tick);
        assert!(matches!(u.phase, Phase::Available { .. }) && u.card_visible(None));
    }

    #[test]
    fn prereleases_off_withdraws_an_offered_prerelease() {
        let pre = || Arc::new(release("0.3.0-rc.1"));
        let mut u = Updater::new(true);
        u.phase = Phase::Available {
            release: pre(),
            manual: false,
        };
        u.drop_prereleases();
        assert!(matches!(u.phase, Phase::Idle));
        u.fail(pre(), UpdateError::NoAsset, None);
        u.drop_prereleases();
        assert!(matches!(u.phase, Phase::Idle));
        // During a check the kept offer goes.
        u.phase = Phase::Available {
            release: pre(),
            manual: false,
        };
        u.start_check(CheckOrigin::Tick);
        u.drop_prereleases();
        assert!(u.installable().is_none() && u.busy());
        let e = Err(UpdateError::Http(500));
        u.on_checked(CheckOrigin::Tick, e, &cur(), false);
        assert!(matches!(u.phase, Phase::CheckFailed(_)));
        // Releases, and pre-releases being downloaded, stay.
        available(&mut u, "0.3.0", CheckOrigin::Tick);
        u.drop_prereleases();
        assert!(u.installable().is_some());
        u.on_download_started(pre(), 1, handle());
        u.drop_prereleases();
        assert!(matches!(u.phase, Phase::Downloading { .. }));
    }

    #[test]
    fn checked_results() {
        let mut u = Updater::new(true);
        // Not checking: a stray result is ignored.
        let r = u.on_checked(CheckOrigin::Tick, Ok(None), &cur(), false);
        assert!(!r.record && matches!(u.phase, Phase::Idle));

        assert!(u.start_check(CheckOrigin::Tick));
        assert!(!u.start_check(CheckOrigin::Manual), "already checking");
        let r = u.on_checked(CheckOrigin::Tick, Ok(None), &cur(), false);
        assert_eq!(
            r,
            CheckResult {
                record: true,
                notice: None
            }
        );
        assert!(matches!(u.phase, Phase::UpToDate));

        // Same version: up to date, toasted when manual.
        u.start_check(CheckOrigin::Manual);
        let r = u.on_checked(
            CheckOrigin::Manual,
            Ok(Some(release("0.2.2"))),
            &cur(),
            false,
        );
        assert_eq!(r.notice, Some(Notice::UpToDate));

        // A pre-release is offered only when allowed.
        u.start_check(CheckOrigin::Tick);
        u.on_checked(
            CheckOrigin::Tick,
            Ok(Some(release("0.3.0-rc.1"))),
            &cur(),
            false,
        );
        assert!(matches!(u.phase, Phase::UpToDate));
        u.start_check(CheckOrigin::Tick);
        u.on_checked(
            CheckOrigin::Tick,
            Ok(Some(release("0.3.0-rc.1"))),
            &cur(),
            true,
        );
        assert!(
            matches!(&u.phase, Phase::Available { release, manual: false }
            if release.version.to_string() == "0.3.0-rc.1")
        );

        // Errors: network and rate limit are not recorded; the reset is kept.
        u.phase = Phase::Idle;
        u.start_check(CheckOrigin::Tick);
        let r = u.on_checked(
            CheckOrigin::Tick,
            Err(UpdateError::Network("x".into())),
            &cur(),
            false,
        );
        assert!(!r.record && r.notice.is_none());
        assert!(matches!(
            u.phase,
            Phase::CheckFailed(UpdateError::Network(_))
        ));
        u.start_check(CheckOrigin::Manual);
        let e = UpdateError::RateLimited { reset: Some(42) };
        let r = u.on_checked(CheckOrigin::Manual, Err(e.clone()), &cur(), false);
        assert_eq!(
            r,
            CheckResult {
                record: false,
                notice: Some(Notice::Error(e))
            }
        );
        assert_eq!(u.not_before, Some(42));
        u.start_check(CheckOrigin::Tick);
        let r = u.on_checked(
            CheckOrigin::Tick,
            Err(UpdateError::Http(500)),
            &cur(),
            false,
        );
        assert!(r.record);
    }

    #[test]
    fn card_visibility_with_skip_later_and_manual() {
        let mut u = Updater::new(true);
        assert!(!u.card_visible(None));
        available(&mut u, "0.3.0", CheckOrigin::Tick);
        assert!(u.card_visible(None));
        assert!(u.card_visible(Some("0.2.9")));
        assert!(!u.card_visible(Some("0.3.0")), "skipped");
        // A manual check shows a skipped version.
        available(&mut u, "0.3.0", CheckOrigin::Manual);
        assert!(u.card_visible(Some("0.3.0")));
        // Later hides it for this run, until the next manual check.
        u.later();
        assert!(!u.card_visible(None));
        available(&mut u, "0.3.0", CheckOrigin::Tick);
        assert!(!u.card_visible(None));
        available(&mut u, "0.3.1", CheckOrigin::Tick);
        assert!(u.card_visible(None), "a newer version shows again");
        u.later();
        available(&mut u, "0.3.1", CheckOrigin::Manual);
        assert!(u.card_visible(None));
    }

    #[test]
    fn download_install_flow() {
        let mut u = Updater::new(true);
        available(&mut u, "0.3.0", CheckOrigin::Tick);
        let r = u.installable().unwrap().clone();
        u.on_download_started(r.clone(), 100, handle());
        assert!(u.busy() && u.card_visible(Some("0.3.0")));
        assert!(!u.start_check(CheckOrigin::Manual));
        u.on_progress(40, 100);
        assert!(matches!(
            u.phase,
            Phase::Downloading {
                received: 40,
                total: 100,
                ..
            }
        ));
        assert!(u.on_downloaded(Ok((PathBuf::from("/c/f"), [1; 32]))));
        assert!(u.download.is_none() && matches!(u.phase, Phase::Ready { .. }));
        let (rel, file, sha) = u.begin_install().unwrap();
        assert_eq!(
            (rel.version.to_string(), file, sha),
            ("0.3.0".into(), PathBuf::from("/c/f"), [1; 32])
        );
        assert!(u.begin_install().is_none());
        // Cancelled authentication: back to Available, told once.
        assert!(u.on_install_failed(UpdateError::AuthCancelled));
        assert!(matches!(u.phase, Phase::Available { manual: true, .. }));
        // Another failure keeps the file for the fallbacks.
        u.on_download_started(r.clone(), 100, handle());
        u.on_downloaded(Ok((PathBuf::from("/c/f"), [1; 32])));
        u.begin_install();
        assert!(!u.on_install_failed(UpdateError::NoAuthAgent));
        assert!(
            matches!(&u.phase, Phase::Failed { error: UpdateError::NoAuthAgent, file: Some(f), release: Some(_), .. } if f == &PathBuf::from("/c/f"))
        );
        assert!(u.installable().is_some(), "retry from Settings");
        assert!(!u.due(0, None, CheckOrigin::Tick));
        // A file that fails its checksum is not offered.
        for e in [UpdateError::ChecksumMismatch, UpdateError::SizeMismatch] {
            u.on_download_started(r.clone(), 100, handle());
            u.on_downloaded(Ok((PathBuf::from("/c/f"), [1; 32])));
            u.begin_install();
            u.on_install_failed(e.clone());
            assert!(matches!(&u.phase, Phase::Failed { error, file: None, .. } if error == &e));
        }
        // Relaunch failure.
        u.on_download_started(r.clone(), 100, handle());
        u.on_downloaded(Ok((PathBuf::from("/c/f"), [1; 32])));
        u.begin_install();
        u.on_installed_no_restart();
        assert!(matches!(u.phase, Phase::InstalledNoRestart { .. }));
        assert!(u.card_visible(None));
        u.later();
        assert!(!u.card_visible(None));
    }

    #[test]
    fn download_failure_cancel_and_stale_results() {
        let mut u = Updater::new(true);
        available(&mut u, "0.3.0", CheckOrigin::Tick);
        let r = u.installable().unwrap().clone();
        u.on_download_started(r.clone(), 100, handle());
        assert!(!u.on_downloaded(Err(UpdateError::ChecksumMismatch)));
        assert!(matches!(
            u.phase,
            Phase::Failed {
                error: UpdateError::ChecksumMismatch,
                file: None,
                ..
            }
        ));
        u.on_download_started(r, 100, handle());
        assert!(u.cancel_download().is_some());
        assert!(matches!(u.phase, Phase::Available { .. }));
        // The aborted stream's last event does not count.
        assert!(!u.on_downloaded(Ok((PathBuf::from("/x"), [0; 32]))));
        assert!(matches!(u.phase, Phase::Available { .. }));
        // Later on a ready download goes back to Available.
        let r = u.installable().unwrap().clone();
        u.on_download_started(r, 1, handle());
        u.on_downloaded(Ok((PathBuf::from("/x"), [0; 32])));
        u.later();
        assert!(matches!(u.phase, Phase::Available { manual: false, .. }));
        assert!(!u.card_visible(None));
    }

    #[test]
    fn stale_failure_from_an_earlier_run() {
        let mut u = Updater::new(true);
        u.on_stale_failure(StaleFailure {
            version: Version::parse("0.3.0").unwrap(),
            error: UpdateError::Tool {
                tool: "swap (mv)".into(),
                code: None,
            },
            file: Some(PathBuf::from("/c/Rustket.app")),
        });
        assert!(u.card_visible(None));
        assert!(u.installable().is_none() && u.release().is_none());
        assert_eq!(
            u.version().map(ToString::to_string).as_deref(),
            Some("0.3.0")
        );
        assert!(u.due(0, None, CheckOrigin::Manual));
        u.later();
        assert!(matches!(u.phase, Phase::Idle));
        assert!(releases_page().ends_with("/releases"));
    }
}
