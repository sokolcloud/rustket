//! Picking the release asset for this installation and its checksum.

use super::UpdateError;
use super::detect::InstallKind;
use super::github::Asset;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arch {
    X86_64,
    Aarch64,
    Other,
}

impl Arch {
    pub fn current() -> Self {
        if cfg!(target_arch = "x86_64") {
            Self::X86_64
        } else if cfg!(target_arch = "aarch64") {
            Self::Aarch64
        } else {
            Self::Other
        }
    }

    /// The Windows package suffix (`build.ps1`).
    fn windows(self) -> Result<&'static str, UpdateError> {
        match self {
            Self::X86_64 => Ok("x64"),
            Self::Aarch64 => Ok("arm64"),
            Self::Other => Err(UpdateError::NoAsset),
        }
    }

    fn require_x86_64(self) -> Result<(), UpdateError> {
        match self {
            Self::X86_64 => Ok(()),
            _ => Err(UpdateError::NoAsset),
        }
    }
}

/// Exact name, prefix and suffix of the package for `kind` (`packaging/*`).
fn expected(
    kind: &InstallKind,
    arch: Arch,
    version: &str,
) -> Result<(String, &'static str, String), UpdateError> {
    Ok(match kind {
        InstallKind::Deb { .. } => {
            arch.require_x86_64()?;
            (
                format!("rustket_{version}-1_amd64.deb"),
                "rustket_",
                "_amd64.deb".into(),
            )
        }
        InstallKind::Rpm { .. } => {
            arch.require_x86_64()?;
            let v = version.replace('-', "~");
            (
                format!("rustket-{v}-1.x86_64.rpm"),
                "rustket-",
                ".x86_64.rpm".into(),
            )
        }
        InstallKind::AppImage { .. } => {
            arch.require_x86_64()?;
            (
                format!("Rustket-{version}-x86_64.AppImage"),
                "Rustket-",
                "-x86_64.AppImage".into(),
            )
        }
        InstallKind::InnoSetup { .. } => {
            let a = arch.windows()?;
            (
                format!("Rustket-{version}-{a}-setup.exe"),
                "Rustket-",
                format!("-{a}-setup.exe"),
            )
        }
        InstallKind::PortableZip { .. } => {
            let a = arch.windows()?;
            (
                format!("rustket-{version}-windows-{a}.zip"),
                "rustket-",
                format!("-windows-{a}.zip"),
            )
        }
        InstallKind::MacApp { .. } => (
            format!("Rustket-{version}-macos-universal.dmg"),
            "Rustket-",
            "-macos-universal.dmg".into(),
        ),
        InstallKind::Unmanaged => return Err(UpdateError::Unsupported),
    })
}

/// The package for `kind` on `arch`: the exact expected name, else the only
/// asset with the expected prefix and suffix. None or several → `NoAsset`.
pub fn select<'a>(
    kind: &InstallKind,
    arch: Arch,
    version: &str,
    assets: &'a [Asset],
) -> Result<&'a Asset, UpdateError> {
    let (exact, prefix, suffix) = expected(kind, arch, version)?;
    if let Some(a) = assets.iter().find(|a| a.name == exact) {
        return Ok(a);
    }
    let mut matching = assets.iter().filter(|a| {
        a.name.len() > prefix.len() + suffix.len()
            && a.name.starts_with(prefix)
            && a.name.ends_with(&suffix)
    });
    match (matching.next(), matching.next()) {
        (Some(a), None) => Ok(a),
        _ => Err(UpdateError::NoAsset),
    }
}

/// Parses `sha256sum` output: `<64 hex> <' ' or '*'><name>` per line. Other
/// lines are ignored.
pub fn parse_sums(text: &str) -> Vec<(String, [u8; 32])> {
    text.lines()
        .filter_map(|line| {
            let line = line.strip_suffix('\r').unwrap_or(line);
            let hex = line.get(..64)?;
            let mut sha = [0u8; 32];
            hex::decode_to_slice(hex, &mut sha).ok()?;
            let name = line[64..]
                .strip_prefix("  ")
                .or_else(|| line[64..].strip_prefix(" *"))?;
            (!name.is_empty()).then(|| (name.to_owned(), sha))
        })
        .collect()
}

/// The checksum of `asset_name`: an exact match, else the only entry equal
/// after [`normalize`] (GitHub renames characters such as rpm's `~` on upload).
pub fn lookup(sums: &[(String, [u8; 32])], asset_name: &str) -> Option<[u8; 32]> {
    if let Some((_, sha)) = sums.iter().find(|(name, _)| name == asset_name) {
        return Some(*sha);
    }
    let want = normalize(asset_name);
    let mut found = sums.iter().filter(|(name, _)| normalize(name) == want);
    match (found.next(), found.next()) {
        (Some((_, sha)), None) => Some(*sha),
        _ => None,
    }
}

/// Every character outside `[A-Za-z0-9._+-]` becomes `.`.
fn normalize(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '+' | '-') {
                c
            } else {
                '.'
            }
        })
        .collect()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::updater::detect::InnoScope;
    use crate::updater::github::parse_list;

    const LIST: &[u8] = include_bytes!("../../tests/fixtures/github_releases.json");

    fn kinds() -> Vec<InstallKind> {
        let p = PathBuf::from;
        vec![
            InstallKind::Deb {
                exe: p("/usr/bin/rustket"),
            },
            InstallKind::Rpm {
                exe: p("/usr/bin/rustket"),
            },
            InstallKind::AppImage {
                image: p("/home/u/Rustket.AppImage"),
            },
            InstallKind::InnoSetup {
                scope: InnoScope::CurrentUser,
                dir: p("C:/Rustket"),
            },
            InstallKind::PortableZip {
                dir: p("C:/Rustket"),
            },
            InstallKind::MacApp {
                bundle: p("/Applications/Rustket.app"),
            },
            InstallKind::Unmanaged,
        ]
    }

    fn pick(
        kind: &InstallKind,
        arch: Arch,
        version: &str,
        assets: &[Asset],
    ) -> Result<String, UpdateError> {
        select(kind, arch, version, assets).map(|a| a.name.clone())
    }

    #[test]
    fn every_kind_and_arch() {
        let stable = crate::updater::github::parse_list(LIST, false)
            .unwrap()
            .unwrap();
        let v = stable.version.to_string();
        let a = &stable.assets;
        let expect: [[Result<&str, UpdateError>; 3]; 7] = [
            [
                Ok("rustket_0.2.2-1_amd64.deb"),
                Err(UpdateError::NoAsset),
                Err(UpdateError::NoAsset),
            ],
            [
                Ok("rustket-0.2.2-1.x86_64.rpm"),
                Err(UpdateError::NoAsset),
                Err(UpdateError::NoAsset),
            ],
            [
                Ok("Rustket-0.2.2-x86_64.AppImage"),
                Err(UpdateError::NoAsset),
                Err(UpdateError::NoAsset),
            ],
            [
                Ok("Rustket-0.2.2-x64-setup.exe"),
                Ok("Rustket-0.2.2-arm64-setup.exe"),
                Err(UpdateError::NoAsset),
            ],
            [
                Ok("rustket-0.2.2-windows-x64.zip"),
                Ok("rustket-0.2.2-windows-arm64.zip"),
                Err(UpdateError::NoAsset),
            ],
            [
                Ok("Rustket-0.2.2-macos-universal.dmg"),
                Ok("Rustket-0.2.2-macos-universal.dmg"),
                Ok("Rustket-0.2.2-macos-universal.dmg"),
            ],
            [
                Err(UpdateError::Unsupported),
                Err(UpdateError::Unsupported),
                Err(UpdateError::Unsupported),
            ],
        ];
        for (kind, row) in kinds().iter().zip(expect) {
            for (arch, want) in [Arch::X86_64, Arch::Aarch64, Arch::Other]
                .into_iter()
                .zip(row)
            {
                assert_eq!(
                    pick(kind, arch, &v, a),
                    want.map(str::to_owned),
                    "{kind:?} {arch:?}"
                );
            }
        }
    }

    #[test]
    fn prerelease_rpm_falls_back_to_the_suffix() {
        let rc = parse_list(LIST, true).unwrap().unwrap();
        let v = rc.version.to_string();
        assert_eq!(v, "0.3.0-rc1");
        // GitHub stored `rustket-0.3.0.rc1-1.x86_64.rpm`, not the `~` name.
        assert_eq!(
            pick(&kinds()[1], Arch::X86_64, &v, &rc.assets),
            Ok("rustket-0.3.0.rc1-1.x86_64.rpm".into())
        );
        assert_eq!(
            pick(&kinds()[0], Arch::X86_64, &v, &rc.assets),
            Ok("rustket_0.3.0-rc1-1_amd64.deb".into())
        );
    }

    #[test]
    fn ambiguous_or_missing() {
        let asset = |name: &str| Asset {
            name: name.into(),
            size: 1,
            url: String::new(),
        };
        let two = [
            asset("Rustket-1.0.0-x86_64.AppImage"),
            asset("Rustket-1.0.0-debug-x86_64.AppImage"),
        ];
        // The exact name wins over the ambiguity.
        assert_eq!(
            pick(&kinds()[2], Arch::X86_64, "1.0.0", &two),
            Ok("Rustket-1.0.0-x86_64.AppImage".into())
        );
        assert_eq!(
            pick(&kinds()[2], Arch::X86_64, "1.0.1", &two),
            Err(UpdateError::NoAsset)
        );
        let one = [asset("Rustket-1.0.0-x86_64.AppImage")];
        assert_eq!(
            pick(&kinds()[2], Arch::X86_64, "1.0.1", &one),
            Ok("Rustket-1.0.0-x86_64.AppImage".into())
        );
        // A bare prefix+suffix is not a package.
        let bare = [asset("Rustket--x86_64.AppImage")];
        assert_eq!(
            pick(&kinds()[2], Arch::X86_64, "1.0.1", &bare),
            Err(UpdateError::NoAsset)
        );
        assert_eq!(
            pick(&kinds()[3], Arch::X86_64, "1.0.0", &[]),
            Err(UpdateError::NoAsset)
        );
    }

    #[test]
    fn sums() {
        let a = "a".repeat(64);
        let b = "B".repeat(64);
        let c = "0123456789abcdef".repeat(4);
        let text = format!(
            "{a}  rustket_1.0.0-1_amd64.deb\r\n\
             {b} *Rustket-1.0.0-x64-setup.exe\n\
             junk line\n\
             \n\
             {c}  \n\
             {c} single-space.txt\n\
             zz{}  bad-hex.txt\n\
             \\{c}  escaped\\nname\n\
             {c}  name with spaces.txt\n",
            "0".repeat(62)
        );
        let sums = parse_sums(&text);
        let names: Vec<_> = sums.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            [
                "rustket_1.0.0-1_amd64.deb",
                "Rustket-1.0.0-x64-setup.exe",
                "name with spaces.txt"
            ]
        );
        assert_eq!(sums[0].1, [0xaa; 32]);
        assert_eq!(sums[1].1, [0xbb; 32]);
        assert_eq!(
            sums[2].1[..8],
            [0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef]
        );
        assert!(parse_sums("").is_empty());
        assert!(parse_sums("щ".repeat(40).as_str()).is_empty());
    }

    #[test]
    fn lookup_normalizes() {
        let sums = vec![
            ("rustket-0.3.0~rc1-1.x86_64.rpm".to_owned(), [1u8; 32]),
            ("rustket_0.3.0-rc1-1_amd64.deb".to_owned(), [2u8; 32]),
        ];
        assert_eq!(
            lookup(&sums, "rustket_0.3.0-rc1-1_amd64.deb"),
            Some([2; 32])
        );
        assert_eq!(
            lookup(&sums, "rustket-0.3.0~rc1-1.x86_64.rpm"),
            Some([1; 32])
        );
        assert_eq!(
            lookup(&sums, "rustket-0.3.0.rc1-1.x86_64.rpm"),
            Some([1; 32])
        );
        assert_eq!(lookup(&sums, "rustket-0.3.0.rc2-1.x86_64.rpm"), None);
        assert_eq!(lookup(&sums, "missing"), None);
        // Two entries normalizing to the same name: refuse to guess.
        let clash = vec![("a~b".to_owned(), [1u8; 32]), ("a^b".to_owned(), [2u8; 32])];
        assert_eq!(lookup(&clash, "a.b"), None);
        assert_eq!(lookup(&clash, "a~b"), Some([1; 32]));
    }

    #[test]
    fn arch_current_is_known_on_ci_targets() {
        if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
            assert_ne!(Arch::current(), Arch::Other);
        }
    }
}
