//! Legal and support details shown in the app: the About page, the Help
//! tab, the first-launch terms screen and (on macOS) the Help menu. The URLs
//! live only here.

use std::path::{Path, PathBuf};

use rustket_core::config::AppConfig;

/// Developer and seller of Rustket.
pub const LEGAL_ENTITY: &str = "SOKOL CLOUD & PRODUCTION COMPANY LIMITED";
pub const COPYRIGHT: &str = "Copyright © 2026 Sergey Sokolov. MIT License.";
pub const PRIVACY_URL: &str = "https://sokolcloud.github.io/rustket/privacy/";
pub const TERMS_URL: &str = "https://sokolcloud.github.io/rustket/terms/";
pub const SUPPORT_URL: &str = "https://sokolcloud.github.io/rustket/support/";
/// Rocket.Chat's own documentation: a neutral secondary link only.
pub const ROCKET_CHAT_DOCS_URL: &str = "https://docs.rocket.chat";
/// The non-affiliation sentence (`legal.disclaimer` translates it).
pub const DISCLAIMER: &str = "Independent client for Rocket.Chat servers; not affiliated with or \
     endorsed by Rocket.Chat Technologies Corp. Rocket.Chat is a trademark of Rocket.Chat \
     Technologies Corp.";

/// Version of the in-app terms (`legal.terms_*`). Bump it when their
/// meaning changes: everyone accepts them again.
pub const TERMS_VERSION: u32 = 1;

/// Whether the terms screen must be shown (never accepted, or an older
/// version).
pub fn terms_pending(cfg: &AppConfig) -> bool {
    cfg.accepted_terms < TERMS_VERSION
}

/// Records the acceptance of the current terms.
pub fn accept_terms(cfg: &mut AppConfig) {
    cfg.accepted_terms = TERMS_VERSION;
}

/// Links of the Help menu and the Help tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HelpLinks {
    pub support: &'static str,
    pub privacy: &'static str,
    pub terms: &'static str,
}

pub const fn help_links() -> HelpLinks {
    HelpLinks {
        support: SUPPORT_URL,
        privacy: PRIVACY_URL,
        terms: TERMS_URL,
    }
}

/// The third-party notices shipped with this build, if any: the macOS
/// bundle's `Resources`, the Windows install folder, or
/// `share/doc/rustket` next to `bin` (rpm, AppImage). Looked up once. The
/// macOS and Windows copies carry `.txt`: without an extension no viewer
/// claims the file. The deb's copy is gzipped and so not offered.
pub fn third_party_notices() -> Option<PathBuf> {
    static FOUND: std::sync::OnceLock<Option<PathBuf>> = std::sync::OnceLock::new();
    FOUND
        .get_or_init(|| {
            let exe = std::env::current_exe().ok()?;
            let dir = exe.parent()?;
            notice_candidates(dir).into_iter().find(|p| p.is_file())
        })
        .clone()
}

fn notice_candidates(exe_dir: &Path) -> Vec<PathBuf> {
    let up = exe_dir.join("..");
    vec![
        up.join("Resources").join("THIRD-PARTY-NOTICES.txt"),
        exe_dir.join("THIRD-PARTY-NOTICES.txt"),
        up.join("share/doc/rustket/THIRD-PARTY-NOTICES"),
    ]
}

/// Copies `src` (a file of the app bundle) to `dir`, replacing an older
/// copy, read-only, and returns the copy. macOS opens the copy: a viewer
/// touching a file inside the app bundle raises the "App Management"
/// privacy prompt.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub fn viewer_copy(src: &Path, dir: &Path) -> std::io::Result<PathBuf> {
    let name = src
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    std::fs::create_dir_all(dir)?;
    let dst = dir.join(name);
    match std::fs::remove_file(&dst) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::write(&dst, std::fs::read(src)?)?;
    let mut perms = std::fs::metadata(&dst)?.permissions();
    #[cfg(unix)]
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o444);
    #[cfg(not(unix))]
    perms.set_readonly(true);
    std::fs::set_permissions(&dst, perms)?;
    Ok(dst)
}

/// Where [`viewer_copy`] puts the notices: `<cache>/notices`.
#[cfg(target_os = "macos")]
pub fn notices_copy_dir() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "rustket").map(|d| d.cache_dir().join("notices"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn viewer_copy_replaces_a_read_only_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("THIRD-PARTY-NOTICES.txt");
        std::fs::write(&src, "first").unwrap();
        let dir = tmp.path().join("cache").join("notices");
        let dst = viewer_copy(&src, &dir).unwrap();
        assert_eq!(dst, dir.join("THIRD-PARTY-NOTICES.txt"));
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "first");
        assert!(std::fs::metadata(&dst).unwrap().permissions().readonly());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&dst).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o444);
        }
        // The read-only older copy is replaced.
        std::fs::write(&src, "second").unwrap();
        let again = viewer_copy(&src, &dir).unwrap();
        assert_eq!(again, dst);
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "second");
    }

    #[test]
    fn viewer_copy_fails_without_the_source() {
        let tmp = tempfile::tempdir().unwrap();
        let missing = tmp.path().join("missing.txt");
        assert!(viewer_copy(&missing, &tmp.path().join("out")).is_err());
    }

    #[test]
    fn terms_are_gated_by_version() {
        let mut cfg = AppConfig::default();
        assert!(terms_pending(&cfg), "never accepted");
        accept_terms(&mut cfg);
        assert!(!terms_pending(&cfg));
        assert_eq!(cfg.accepted_terms, TERMS_VERSION);
        // An older acceptance asks again after a bump.
        cfg.accepted_terms = TERMS_VERSION - 1;
        assert!(terms_pending(&cfg));
        // A newer one (config from a later build) does not.
        cfg.accepted_terms = TERMS_VERSION + 1;
        assert!(!terms_pending(&cfg));
    }

    #[test]
    fn urls_are_https_and_in_one_place() {
        let l = help_links();
        for u in [l.support, l.privacy, l.terms, ROCKET_CHAT_DOCS_URL] {
            assert!(u.starts_with("https://"), "{u}");
            assert!(url::Url::parse(u).is_ok(), "{u}");
        }
        assert_ne!(l.support, l.privacy);
    }

    #[test]
    fn disclaimer_matches_the_english_text() {
        assert_eq!(rust_i18n::t!("legal.disclaimer", locale = "en"), DISCLAIMER);
    }

    #[test]
    fn notices_are_looked_up_next_to_the_binary() {
        let dir = std::env::temp_dir().join(format!("rustket-notices-{}", std::process::id()));
        let bin = dir.join("bin");
        std::fs::create_dir_all(bin.join("../share/doc/rustket")).unwrap();
        let c = notice_candidates(&bin);
        assert!(c.iter().all(|p| p.is_absolute()));
        assert!(c[0].ends_with("Resources/THIRD-PARTY-NOTICES.txt"));
        assert!(c.iter().all(|p| p.extension().is_none_or(|e| e == "txt")));
        std::fs::write(bin.join("../share/doc/rustket/THIRD-PARTY-NOTICES"), "x").unwrap();
        assert!(notice_candidates(&bin).into_iter().any(|p| p.is_file()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
