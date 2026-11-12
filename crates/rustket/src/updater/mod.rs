//! In-app updates for the packages published on GitHub (Cargo feature
//! `self-update`, off by default; release builds turn it on).
//!
//! The app checks the repository's GitHub releases, and on a click downloads
//! the package that matches how it was installed, verifies it against the
//! release's `SHA256SUMS` and installs it the same way (deb, rpm, AppImage,
//! Windows installer or portable zip, macOS app), then restarts. The install
//! kind comes from the `install-kind` marker file the packaging scripts write
//! next to the binary, cross-checked at runtime; without a marker
//! (source, dev and distribution builds) the app only notifies and links to
//! the release page.
//!
//! Threat model: the checksum file comes from the same release as the
//! packages, so it catches corrupted, truncated or mismatched downloads but
//! does not authenticate the release. Whoever can publish releases on the
//! repository can ship an update to every user of the updater; packages are
//! not signed. A process running as the same user can tamper with downloaded
//! files and is out of scope.
//!
//! UI strings live in `locales/updater.yml`.

use std::path::PathBuf;
use std::time::Duration;

pub mod app;
pub mod assets;
pub mod detect;
pub mod download;
pub mod error;
pub mod github;
pub mod install;
pub mod relaunch;
pub mod state;
pub mod version;
pub mod view;

pub use detect::{InstallKind, Platform};
pub use error::UpdateError;
pub use github::{Asset, Endpoints, Release};
pub use state::Updater;

/// GitHub `owner/name` the releases come from; release builds set
/// `RUSTKET_UPDATE_REPO` to the repository that builds them.
pub const REPO: &str = match option_env!("RUSTKET_UPDATE_REPO") {
    Some(repo) => repo,
    None => "sokolcloud/rustket",
};

/// `AppId` of `packaging/windows/rustket.iss`; the uninstall registry key is
/// `<INNO_APP_ID>_is1`.
pub const INNO_APP_ID: &str = "{77810904-1236-4291-B123-CCC8D0A6CCF1}";

/// Name of the marker file written by the packaging scripts (`deb`, `rpm`,
/// `appimage`, `zip` or `dmg`, one word and a newline).
pub const MARKER_FILE: &str = "install-kind";

/// Interval between automatic checks.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(12 * 60 * 60);
/// A startup check runs only when the last one is at least this old.
pub const STARTUP_MIN_AGE: Duration = Duration::from_secs(60 * 60);
/// How often the app looks whether a check is due.
pub const TICK: Duration = Duration::from_secs(60 * 60);
/// Delay of the startup check after the app has booted.
pub const STARTUP_DELAY: Duration = Duration::from_secs(20);

/// Downloads and installer logs: `<cache>/updates/<version>/`.
pub fn cache_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "rustket").map(|d| d.cache_dir().join("updates"))
}

/// The running version. `CARGO_PKG_VERSION` is always valid semver (Cargo
/// enforces it), so `None` is not expected; it disables the updater.
pub fn current_version() -> Option<semver::Version> {
    match semver::Version::parse(env!("CARGO_PKG_VERSION")) {
        Ok(version) => Some(version),
        Err(e) => {
            tracing::warn!("updater: cannot parse the package version: {e}");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_version_parses() {
        assert_eq!(
            current_version().map(|v| v.to_string()).as_deref(),
            Some(env!("CARGO_PKG_VERSION"))
        );
        assert!(REPO.contains('/'));
        assert!(cache_dir().is_none_or(|d| d.ends_with("updates")));
    }
}
