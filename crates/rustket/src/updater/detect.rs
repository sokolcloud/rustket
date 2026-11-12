//! How this copy was installed: the packaging marker cross-checked against
//! where the executable runs from (and the Inno Setup registry entry).

use std::ffi::OsString;
use std::io::Read;
use std::path::{Path, PathBuf};

use super::MARKER_FILE;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallKind {
    Deb {
        exe: PathBuf,
    },
    Rpm {
        exe: PathBuf,
    },
    /// `image`: `$APPIMAGE` as the runtime set it.
    AppImage {
        image: PathBuf,
    },
    InnoSetup {
        scope: InnoScope,
        dir: PathBuf,
    },
    PortableZip {
        dir: PathBuf,
    },
    MacApp {
        bundle: PathBuf,
    },
    /// Source, dev and distribution builds: notify only.
    Unmanaged,
}

impl InstallKind {
    pub fn can_install(&self) -> bool {
        !matches!(self, Self::Unmanaged)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InnoScope {
    CurrentUser,
    AllUsers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    Windows,
    MacOs,
    Other,
}

impl Platform {
    pub fn current() -> Self {
        if cfg!(target_os = "linux") {
            Self::Linux
        } else if cfg!(windows) {
            Self::Windows
        } else if cfg!(target_os = "macos") {
            Self::MacOs
        } else {
            Self::Other
        }
    }
}

/// The marker word written by `packaging/*` (`install_marker`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marker {
    Deb,
    Rpm,
    AppImage,
    Zip,
    Dmg,
}

pub fn read_marker(text: &str) -> Option<Marker> {
    match text.trim().to_ascii_lowercase().as_str() {
        "deb" => Some(Marker::Deb),
        "rpm" => Some(Marker::Rpm),
        "appimage" => Some(Marker::AppImage),
        "zip" => Some(Marker::Zip),
        "dmg" => Some(Marker::Dmg),
        _ => None,
    }
}

/// What [`detect`] reads from the system; tests inject a fake.
pub trait Host {
    fn current_exe(&self) -> Option<PathBuf>;
    fn var(&self, name: &str) -> Option<OsString>;
    /// The file's text, `None` if missing, unreadable or over 64 KiB.
    fn read_small(&self, path: &Path) -> Option<String>;
    fn is_file(&self, path: &Path) -> bool;
    /// The file system holding `path` is mounted read-only.
    fn read_only(&self, path: &Path) -> bool;
    /// The Inno Setup install dir of `scope` from the uninstall registry key.
    fn inno_app_path(&self, scope: InnoScope) -> Option<String>;
}

const READ_MAX: u64 = 64 * 1024;
const BUNDLE_ID: &str = crate::BUNDLE_ID;
const LINUX_EXE: &str = "/usr/bin/rustket";
const DPKG_LIST: &str = "/var/lib/dpkg/info/rustket.list";
/// Present on rpm-ostree systems, where `/usr` is read-only.
const OSTREE_BOOTED: &str = "/run/ostree-booted";
/// Read-only on image-based systems without the ostree marker (openSUSE
/// Aeon/MicroOS, …): the package manager cannot change it.
const USR: &str = "/usr";

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemHost;

impl Host for SystemHost {
    fn current_exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }

    fn var(&self, name: &str) -> Option<OsString> {
        std::env::var_os(name)
    }

    fn read_small(&self, path: &Path) -> Option<String> {
        let file = std::fs::File::open(path).ok()?;
        let mut text = String::new();
        file.take(READ_MAX + 1).read_to_string(&mut text).ok()?;
        (text.len() as u64 <= READ_MAX).then_some(text)
    }

    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    #[cfg(unix)]
    fn read_only(&self, path: &Path) -> bool {
        rustix::fs::statvfs(path)
            .is_ok_and(|s| s.f_flag.contains(rustix::fs::StatVfsMountFlags::RDONLY))
    }

    #[cfg(not(unix))]
    fn read_only(&self, _path: &Path) -> bool {
        false
    }

    #[cfg(windows)]
    fn inno_app_path(&self, scope: InnoScope) -> Option<String> {
        let root = match scope {
            InnoScope::CurrentUser => windows_registry::CURRENT_USER,
            InnoScope::AllUsers => windows_registry::LOCAL_MACHINE,
        };
        let key = root
            .open(format!(
                r"Software\Microsoft\Windows\CurrentVersion\Uninstall\{}_is1",
                super::INNO_APP_ID
            ))
            .ok()?;
        key.get_string("Inno Setup: App Path")
            .or_else(|_| key.get_string("InstallLocation"))
            .ok()
            .filter(|s| !s.is_empty())
    }

    #[cfg(not(windows))]
    fn inno_app_path(&self, _scope: InnoScope) -> Option<String> {
        None
    }
}

/// The install kind of the running app on `platform` (see the rules in the
/// arms below); anything unexpected is `Unmanaged`.
pub fn detect(host: &dyn Host, platform: Platform) -> InstallKind {
    let Some(exe) = host.current_exe().map(|e| undeleted(host, e)) else {
        return InstallKind::Unmanaged;
    };
    let kind = match platform {
        Platform::Linux => linux(host, &exe),
        Platform::Windows => windows(host, &exe),
        Platform::MacOs => macos(host, &exe),
        Platform::Other => None,
    };
    let kind = kind.unwrap_or(InstallKind::Unmanaged);
    tracing::debug!("updater: install kind {kind:?} (exe {})", exe.display());
    kind
}

/// Linux reports a replaced executable as `<path> (deleted)`.
fn undeleted(host: &dyn Host, exe: PathBuf) -> PathBuf {
    exe.to_str()
        .and_then(|s| s.strip_suffix(" (deleted)"))
        .map(PathBuf::from)
        .filter(|p| host.is_file(p))
        .unwrap_or(exe)
}

fn marker(host: &dyn Host, path: &Path) -> Option<Marker> {
    host.read_small(path).as_deref().and_then(read_marker)
}

fn non_empty_var(host: &dyn Host, name: &str) -> Option<OsString> {
    host.var(name).filter(|v| !v.is_empty())
}

/// `<prefix>/bin/rustket` → `<prefix>/lib/rustket/install-kind`, lexically.
fn linux_marker_path(exe: &Path) -> Option<PathBuf> {
    Some(
        exe.parent()?
            .parent()?
            .join("lib")
            .join("rustket")
            .join(MARKER_FILE),
    )
}

fn linux(host: &dyn Host, exe: &Path) -> Option<InstallKind> {
    // Same rule as `url_handler::handler_executable`.
    if let (Some(image), Some(appdir)) = (
        non_empty_var(host, "APPIMAGE"),
        non_empty_var(host, "APPDIR"),
    ) {
        let image = PathBuf::from(image);
        if exe.starts_with(&appdir) && host.is_file(&image) {
            let ok = marker(host, &linux_marker_path(exe)?) == Some(Marker::AppImage);
            return ok.then_some(InstallKind::AppImage { image });
        }
    }
    if exe != Path::new(LINUX_EXE)
        || host.is_file(Path::new(OSTREE_BOOTED))
        || host.read_only(Path::new(USR))
    {
        return None;
    }
    match marker(host, &linux_marker_path(exe)?)? {
        Marker::Deb => host
            .read_small(Path::new(DPKG_LIST))?
            .lines()
            .any(|l| l.trim_end() == LINUX_EXE)
            .then(|| InstallKind::Deb {
                exe: exe.to_owned(),
            }),
        Marker::Rpm => Some(InstallKind::Rpm {
            exe: exe.to_owned(),
        }),
        _ => None,
    }
}

/// The directory part of a Windows path string, with either separator.
fn win_parent(path: &str) -> Option<&str> {
    path.rfind(['\\', '/']).map(|i| &path[..i])
}

fn win_dir_eq(a: &str, b: &str) -> bool {
    let norm = |s: &str| s.replace('/', "\\").trim_end_matches('\\').to_owned();
    let (a, b) = (norm(a), norm(b));
    !a.is_empty() && a.eq_ignore_ascii_case(&b)
}

fn windows(host: &dyn Host, exe: &Path) -> Option<InstallKind> {
    let dir = win_parent(exe.to_str()?)?;
    for scope in [InnoScope::CurrentUser, InnoScope::AllUsers] {
        if host
            .inno_app_path(scope)
            .is_some_and(|app| win_dir_eq(&app, dir))
        {
            return Some(InstallKind::InnoSetup {
                scope,
                dir: PathBuf::from(dir),
            });
        }
    }
    let dir = exe.parent()?;
    (marker(host, &dir.join(MARKER_FILE))? == Marker::Zip).then(|| InstallKind::PortableZip {
        dir: dir.to_owned(),
    })
}

fn macos(host: &dyn Host, exe: &Path) -> Option<InstallKind> {
    let macos_dir = exe.parent()?;
    let contents = macos_dir.parent()?;
    let bundle = contents.parent()?;
    let named = |p: &Path, name: &str| p.file_name().is_some_and(|n| n == name);
    if !named(exe, "rustket")
        || !named(macos_dir, "MacOS")
        || !named(contents, "Contents")
        || bundle.extension().is_none_or(|e| e != "app")
    {
        return None;
    }
    if marker(host, &contents.join("Resources").join(MARKER_FILE))? != Marker::Dmg {
        return None;
    }
    let plist = host.read_small(&contents.join("Info.plist"))?;
    (plist_string(&plist, "CFBundleIdentifier") == Some(BUNDLE_ID)).then(|| InstallKind::MacApp {
        bundle: bundle.to_owned(),
    })
}

/// The `<string>` value following `<key>key</key>` in an XML plist.
pub fn plist_string<'a>(xml: &'a str, key: &str) -> Option<&'a str> {
    let tag = format!("<key>{key}</key>");
    let rest = &xml[xml.find(&tag)? + tag.len()..];
    let rest = rest.trim_start().strip_prefix("<string>")?;
    let end = rest.find("</string>")?;
    Some(rest[..end].trim())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[derive(Default)]
    struct FakeHost {
        exe: Option<PathBuf>,
        vars: HashMap<&'static str, OsString>,
        files: HashMap<PathBuf, String>,
        inno: HashMap<&'static str, String>,
        read_only: Vec<PathBuf>,
    }

    impl FakeHost {
        fn exe(path: &str) -> Self {
            Self {
                exe: Some(PathBuf::from(path)),
                ..Self::default()
            }
        }
        fn file(mut self, path: &str, text: &str) -> Self {
            self.files.insert(PathBuf::from(path), text.to_owned());
            self
        }
        fn var(mut self, name: &'static str, value: &str) -> Self {
            self.vars.insert(name, value.into());
            self
        }
        fn read_only(mut self, path: &str) -> Self {
            self.read_only.push(PathBuf::from(path));
            self
        }
        fn inno(mut self, scope: InnoScope, path: &str) -> Self {
            let key = match scope {
                InnoScope::CurrentUser => "hkcu",
                InnoScope::AllUsers => "hklm",
            };
            self.inno.insert(key, path.to_owned());
            self
        }
    }

    impl Host for FakeHost {
        fn current_exe(&self) -> Option<PathBuf> {
            self.exe.clone()
        }
        fn var(&self, name: &str) -> Option<OsString> {
            self.vars.get(name).cloned()
        }
        fn read_small(&self, path: &Path) -> Option<String> {
            self.files.get(path).cloned()
        }
        fn is_file(&self, path: &Path) -> bool {
            self.files.contains_key(path)
        }
        fn read_only(&self, path: &Path) -> bool {
            self.read_only.iter().any(|p| p == path)
        }
        fn inno_app_path(&self, scope: InnoScope) -> Option<String> {
            let key = match scope {
                InnoScope::CurrentUser => "hkcu",
                InnoScope::AllUsers => "hklm",
            };
            self.inno.get(key).cloned()
        }
    }

    fn linux(h: &FakeHost) -> InstallKind {
        detect(h, Platform::Linux)
    }

    const MOUNT: &str = "/tmp/.mount_RustkeXYZ";
    const IMAGE: &str = "/home/u/Apps/Rustket-0.2.2-x86_64.AppImage";

    fn appimage_host() -> FakeHost {
        FakeHost::exe(&format!("{MOUNT}/usr/bin/rustket"))
            .var("APPIMAGE", IMAGE)
            .var("APPDIR", MOUNT)
            .file(IMAGE, "")
    }

    #[test]
    fn markers() {
        assert_eq!(read_marker("deb\n"), Some(Marker::Deb));
        assert_eq!(read_marker(" RPM \r\n"), Some(Marker::Rpm));
        assert_eq!(read_marker("AppImage"), Some(Marker::AppImage));
        assert_eq!(read_marker("zip"), Some(Marker::Zip));
        assert_eq!(read_marker("dmg"), Some(Marker::Dmg));
        assert_eq!(read_marker("deb rpm"), None);
        assert_eq!(read_marker(""), None);
    }

    #[test]
    fn appimage() {
        let marker = format!("{MOUNT}/usr/lib/rustket/install-kind");
        let h = appimage_host().file(&marker, "appimage\n");
        assert_eq!(
            linux(&h),
            InstallKind::AppImage {
                image: PathBuf::from(IMAGE)
            }
        );
        // No marker: an AppImage someone else built.
        assert_eq!(linux(&appimage_host()), InstallKind::Unmanaged);
        // Wrong marker word.
        let h = appimage_host().file(&marker, "deb\n");
        assert_eq!(linux(&h), InstallKind::Unmanaged);
        // The executable is not inside $APPDIR (inherited variables).
        let h = FakeHost::exe("/opt/rustket/usr/bin/rustket")
            .var("APPIMAGE", IMAGE)
            .var("APPDIR", MOUNT)
            .file(IMAGE, "")
            .file("/opt/rustket/usr/lib/rustket/install-kind", "appimage");
        assert_eq!(linux(&h), InstallKind::Unmanaged);
        // $APPIMAGE is not a file.
        let h = FakeHost::exe(&format!("{MOUNT}/usr/bin/rustket"))
            .var("APPIMAGE", IMAGE)
            .var("APPDIR", MOUNT)
            .file(&marker, "appimage");
        assert_eq!(linux(&h), InstallKind::Unmanaged);
        // Empty $APPDIR is ignored.
        let h = appimage_host().var("APPDIR", "").file(&marker, "appimage");
        assert_eq!(linux(&h), InstallKind::Unmanaged);
    }

    const DEB_MARKER: &str = "/usr/lib/rustket/install-kind";

    #[test]
    fn deb() {
        let list = "/.\n/usr\n/usr/bin\n/usr/bin/rustket\n/usr/share/doc/rustket\n";
        let h = FakeHost::exe("/usr/bin/rustket")
            .file(DEB_MARKER, "deb\n")
            .file(DPKG_LIST, list);
        let deb = InstallKind::Deb {
            exe: PathBuf::from("/usr/bin/rustket"),
        };
        assert_eq!(linux(&h), deb);
        assert!(deb.can_install());
        // Marker without the dpkg database entry.
        let h = FakeHost::exe("/usr/bin/rustket").file(DEB_MARKER, "deb\n");
        assert_eq!(linux(&h), InstallKind::Unmanaged);
        let h = FakeHost::exe("/usr/bin/rustket")
            .file(DEB_MARKER, "deb\n")
            .file(DPKG_LIST, "/usr/bin/rustket-other\n");
        assert_eq!(linux(&h), InstallKind::Unmanaged);
        // Another prefix (e.g. a copied package tree).
        let h = FakeHost::exe("/usr/local/bin/rustket")
            .file("/usr/local/lib/rustket/install-kind", "deb\n")
            .file(DPKG_LIST, list);
        assert_eq!(linux(&h), InstallKind::Unmanaged);
        // Running a deleted (just replaced) executable.
        let h = FakeHost::exe("/usr/bin/rustket (deleted)")
            .file("/usr/bin/rustket", "")
            .file(DEB_MARKER, "deb\n")
            .file(DPKG_LIST, list);
        assert_eq!(linux(&h), deb);
    }

    #[test]
    fn rpm() {
        let h = FakeHost::exe("/usr/bin/rustket").file(DEB_MARKER, "rpm\n");
        assert_eq!(
            linux(&h),
            InstallKind::Rpm {
                exe: PathBuf::from("/usr/bin/rustket")
            }
        );
        // rpm-ostree: /usr is read-only.
        let ostree = FakeHost::exe("/usr/bin/rustket")
            .file(DEB_MARKER, "rpm\n")
            .file(OSTREE_BOOTED, "");
        assert_eq!(linux(&ostree), InstallKind::Unmanaged);
        // Transactional openSUSE: a read-only /usr without the ostree marker.
        let h = h.read_only("/usr");
        assert_eq!(linux(&h), InstallKind::Unmanaged);
        let h =
            FakeHost::exe("/home/u/bin/rustket").file("/home/u/lib/rustket/install-kind", "rpm");
        assert_eq!(linux(&h), InstallKind::Unmanaged);
    }

    #[test]
    fn no_marker_or_exe() {
        assert_eq!(
            linux(&FakeHost::exe("/usr/bin/rustket")),
            InstallKind::Unmanaged
        );
        assert_eq!(
            linux(&FakeHost::exe("/home/u/src/rustket/target/debug/rustket")),
            InstallKind::Unmanaged
        );
        assert_eq!(linux(&FakeHost::default()), InstallKind::Unmanaged);
        assert!(!InstallKind::Unmanaged.can_install());
        // Linux markers mean nothing elsewhere.
        let h = FakeHost::exe("/usr/bin/rustket").file(DEB_MARKER, "rpm\n");
        assert_eq!(detect(&h, Platform::Other), InstallKind::Unmanaged);
        assert_eq!(detect(&h, Platform::MacOs), InstallKind::Unmanaged);
    }

    #[test]
    fn inno() {
        let exe = r"C:\Users\U\AppData\Local\Programs\Rustket\rustket.exe";
        let h = FakeHost::exe(exe).inno(
            InnoScope::CurrentUser,
            r"c:\users\u\appdata\local\programs\rustket\",
        );
        assert_eq!(
            detect(&h, Platform::Windows),
            InstallKind::InnoSetup {
                scope: InnoScope::CurrentUser,
                dir: PathBuf::from(r"C:\Users\U\AppData\Local\Programs\Rustket"),
            }
        );
        let exe = r"C:\Program Files\Rustket\rustket.exe";
        let h = FakeHost::exe(exe)
            .inno(InnoScope::CurrentUser, r"D:\Other")
            .inno(InnoScope::AllUsers, "C:/Program Files/Rustket");
        assert_eq!(
            detect(&h, Platform::Windows),
            InstallKind::InnoSetup {
                scope: InnoScope::AllUsers,
                dir: PathBuf::from(r"C:\Program Files\Rustket"),
            }
        );
        // Registered elsewhere: this copy is not the installed one.
        let h = FakeHost::exe(exe).inno(InnoScope::AllUsers, r"C:\Program Files\Rustket2");
        assert_eq!(detect(&h, Platform::Windows), InstallKind::Unmanaged);
        // An empty registry value never matches.
        assert!(!win_dir_eq("", ""));
        assert!(!win_dir_eq(r"\", r"\"));
    }

    #[test]
    fn zip() {
        let h = FakeHost::exe("D:/Tools/Rustket/rustket.exe")
            .file("D:/Tools/Rustket/install-kind", "zip\r\n");
        assert_eq!(
            detect(&h, Platform::Windows),
            InstallKind::PortableZip {
                dir: PathBuf::from("D:/Tools/Rustket")
            }
        );
        let h = FakeHost::exe("D:/Tools/Rustket/rustket.exe");
        assert_eq!(detect(&h, Platform::Windows), InstallKind::Unmanaged);
        // Inno wins over a stray marker.
        let h = FakeHost::exe("D:/Tools/Rustket/rustket.exe")
            .file("D:/Tools/Rustket/install-kind", "zip")
            .inno(InnoScope::CurrentUser, r"d:\tools\rustket");
        assert!(matches!(
            detect(&h, Platform::Windows),
            InstallKind::InnoSetup {
                scope: InnoScope::CurrentUser,
                ..
            }
        ));
    }

    const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleExecutable</key>
  <string>rustket</string>
  <key>CFBundleIdentifier</key>
  <string>io.github.sokolcloud.rustket</string>
  <key>CFBundleShortVersionString</key>
  <string>0.2.2</string>
</dict>
</plist>
"#;

    #[test]
    fn plist() {
        assert_eq!(
            plist_string(PLIST, "CFBundleIdentifier"),
            Some("io.github.sokolcloud.rustket")
        );
        assert_eq!(
            plist_string(PLIST, "CFBundleShortVersionString"),
            Some("0.2.2")
        );
        assert_eq!(plist_string(PLIST, "CFBundleName"), None);
        assert_eq!(plist_string("<key>A</key><true/>", "A"), None);
        assert_eq!(plist_string("<key>A</key><string>x", "A"), None);
    }

    #[test]
    fn mac_bundle() {
        let app = "/Applications/Rustket.app";
        let host = |plist: &str| {
            FakeHost::exe(&format!("{app}/Contents/MacOS/rustket"))
                .file(&format!("{app}/Contents/Resources/install-kind"), "dmg\n")
                .file(&format!("{app}/Contents/Info.plist"), plist)
        };
        assert_eq!(
            detect(&host(PLIST), Platform::MacOs),
            InstallKind::MacApp {
                bundle: PathBuf::from(app)
            }
        );
        let other = PLIST.replace("io.github.sokolcloud.rustket", "com.example.fork");
        assert_eq!(
            detect(&host(&other), Platform::MacOs),
            InstallKind::Unmanaged
        );
        // No marker.
        let h = FakeHost::exe(&format!("{app}/Contents/MacOS/rustket"))
            .file(&format!("{app}/Contents/Info.plist"), PLIST);
        assert_eq!(detect(&h, Platform::MacOs), InstallKind::Unmanaged);
        // Not inside a bundle.
        let h = FakeHost::exe("/usr/local/Contents/MacOS/rustket")
            .file("/usr/local/Contents/Resources/install-kind", "dmg")
            .file("/usr/local/Contents/Info.plist", PLIST);
        assert_eq!(detect(&h, Platform::MacOs), InstallKind::Unmanaged);
    }

    #[test]
    fn system_host_reads_small_files_only() {
        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small");
        let big = dir.path().join("big");
        std::fs::write(&small, "deb\n").unwrap();
        std::fs::write(&big, vec![b'a'; READ_MAX as usize + 1]).unwrap();
        assert_eq!(SystemHost.read_small(&small).as_deref(), Some("deb\n"));
        assert_eq!(SystemHost.read_small(&big), None);
        assert_eq!(SystemHost.read_small(&dir.path().join("none")), None);
        assert!(SystemHost.is_file(&small));
        assert!(!SystemHost.is_file(dir.path()));
        // A dev build is never managed.
        assert_eq!(
            detect(&SystemHost, Platform::current()),
            InstallKind::Unmanaged
        );
    }
}
