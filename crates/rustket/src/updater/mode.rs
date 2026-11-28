//! The effective update mode (docs/policy.md section 9.3): the one place
//! that knows the rule. Every entry of the updater asks
//! [`crate::updater::app::mode`], which applies it to the running app.

pub use rustket_core::policy::UpdateMode;

use super::detect::{InnoScope, InstallKind};

/// `key`: the policy's `UpdateMode`; `kind`: the detected install kind
/// (`None` until detected). The key when set; else `notify` for exactly these
/// kinds: an Inno Setup installation for all users and the kinds reserved for
/// the MSI and the macOS installer package (`Msi`, `MacPkg`); else `prompt`
/// for every other kind (deb and rpm stay `prompt`, although their install
/// asks for an administrator password).
///
/// An undetected kind counts as `prompt` for the checks; nothing installs
/// before the kind is known, and the detection runs right after boot. Until
/// then the blocking screen offers an update only when the key is `prompt`
/// (`App::update_mode` is `None` without the key and the kind).
pub fn effective(key: Option<UpdateMode>, kind: Option<&InstallKind>) -> UpdateMode {
    if let Some(mode) = key {
        return mode;
    }
    match kind {
        Some(
            InstallKind::InnoSetup {
                scope: InnoScope::AllUsers,
                ..
            }
            | InstallKind::Msi { .. }
            | InstallKind::MacPkg { .. },
        ) => UpdateMode::Notify,
        _ => UpdateMode::Prompt,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_mode_table() {
        use UpdateMode::{Notify, Off, Prompt};
        let kinds = [
            (None, Prompt),
            (Some(InstallKind::Deb { exe: "/e".into() }), Prompt),
            (Some(InstallKind::Rpm { exe: "/e".into() }), Prompt),
            (Some(InstallKind::AppImage { image: "/a".into() }), Prompt),
            (
                Some(InstallKind::InnoSetup {
                    scope: InnoScope::CurrentUser,
                    dir: "c".into(),
                }),
                Prompt,
            ),
            (
                Some(InstallKind::InnoSetup {
                    scope: InnoScope::AllUsers,
                    dir: "c".into(),
                }),
                Notify,
            ),
            (Some(InstallKind::PortableZip { dir: "c".into() }), Prompt),
            (
                Some(InstallKind::MacApp {
                    bundle: "/b".into(),
                }),
                Prompt,
            ),
            (Some(InstallKind::Msi { dir: "c".into() }), Notify),
            (
                Some(InstallKind::MacPkg {
                    bundle: "/b".into(),
                }),
                Notify,
            ),
            (Some(InstallKind::Unmanaged), Prompt),
        ];
        for (kind, default) in kinds {
            assert_eq!(effective(None, kind.as_ref()), default, "{kind:?}");
            for key in [Prompt, Notify, Off] {
                assert_eq!(effective(Some(key), kind.as_ref()), key, "{kind:?} {key:?}");
            }
        }
    }
}
