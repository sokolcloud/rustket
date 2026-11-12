//! macOS: the app bundle is copied out of the dmg and checked; a helper
//! script swaps it in after the app quits and reopens it. The script text and
//! the checks are platform-independent; the commands need macOS.

use std::path::Path;

use crate::updater::UpdateError;
use crate::updater::detect::plist_string;

/// The prepared bundle in the download dir (when the app's dir needs admin
/// rights).
pub const NEW_BUNDLE: &str = "Rustket.app";
/// The prepared bundle next to the app (same volume, so the swap is a rename).
pub const SIBLING_BUNDLE: &str = ".Rustket-update.app";
pub const HELPER: &str = "helper.sh";

/// Arguments: `<pid> <app> <new> <admin 0|1>`. Waits up to 60 s for `pid` to
/// exit, swaps `new` in for `app` (as root through osascript when `admin`;
/// root then owns the bundle, which the user copied, so the user cannot
/// change it afterwards), writes `ok` or `failed:<step>` to `result` next to itself and reopens
/// the app as the user. All values are positional arguments; nothing from
/// them is evaluated.
pub fn mac_helper_script() -> &'static str {
    r#"#!/bin/sh
pid=$1 app=$2 new=$3 admin=$4
result=${0%/*}/result
i=0
while kill -0 "$pid" 2>/dev/null; do
  i=$((i + 1))
  if [ "$i" -gt 600 ]; then echo failed:wait >"$result"; exit 1; fi
  sleep 0.1
done
old="$app.rustket-old"
swap='/bin/rm -rf "$2" && /bin/mv "$1" "$2" || exit 1; /bin/mv "$3" "$1" || { /bin/mv "$2" "$1"; exit 1; }; /bin/rm -rf "$2"'
swap_root='/bin/rm -rf "$2" && /bin/mv "$1" "$2" || exit 1; /bin/mv "$3" "$1" && /usr/sbin/chown -R root:wheel "$1" && /bin/chmod -R go-w "$1" || { /bin/rm -rf "$1"; /bin/mv "$2" "$1"; exit 1; }; /bin/rm -rf "$2"'
run_swap() {
  if [ "$admin" = 1 ]; then
    /usr/bin/osascript -e 'on run argv' \
      -e 'do shell script "/bin/sh -c " & quoted form of item 1 of argv & " _ " & quoted form of item 2 of argv & " " & quoted form of item 3 of argv & " " & quoted form of item 4 of argv with administrator privileges' \
      -e 'end run' "$swap_root" "$app" "$old" "$new"
  else
    /bin/sh -c "$swap" _ "$app" "$old" "$new"
  fi
}
if run_swap; then
  echo ok >"$result"
  exec /usr/bin/open -n "$app"
fi
echo failed:swap >"$result"
[ -e "$app" ] || /bin/mv "$old" "$app" 2>/dev/null
exec /usr/bin/open "$app"
"#
}

/// The copied bundle must be this app at the release's version.
pub fn check_plist(xml: &str, version: &semver::Version) -> Result<(), UpdateError> {
    let id = plist_string(xml, "CFBundleIdentifier");
    if id != Some(crate::BUNDLE_ID) {
        return Err(UpdateError::BadRelease(format!("bundle id {id:?}")));
    }
    let short = plist_string(xml, "CFBundleShortVersionString");
    if short != Some(version.to_string().as_str()) {
        return Err(UpdateError::BadRelease(format!("bundle version {short:?}")));
    }
    Ok(())
}

/// Gatekeeper runs quarantined apps from a random read-only path.
pub fn translocated(bundle: &Path) -> bool {
    bundle.to_string_lossy().contains("/AppTranslocation/")
}

#[cfg(target_os = "macos")]
pub fn install_dmg(
    bundle: &Path,
    file: &Path,
    release: &crate::updater::Release,
) -> Result<super::Outcome, UpdateError> {
    use std::ffi::OsString;
    use std::process::{Command, Stdio};

    use super::{probe_writable, run_quiet, run_tool};

    if translocated(bundle) {
        return Err(UpdateError::Translocated);
    }
    let (Some(parent), Some(work)) = (bundle.parent(), file.parent()) else {
        return Err(UpdateError::Unsupported);
    };
    let admin = probe_writable(parent).is_err();
    if admin && bundle.starts_with("/Volumes") {
        // Running from the mounted dmg.
        return Err(UpdateError::Translocated);
    }
    let mnt = work.join("mnt");
    std::fs::create_dir_all(&mnt).map_err(|e| UpdateError::io("create the mount point", &e))?;
    let hdiutil = Path::new("/usr/bin/hdiutil");
    let attach: Vec<OsString> = vec![
        "attach".into(),
        "-nobrowse".into(),
        "-readonly".into(),
        "-noautoopen".into(),
        "-noverify".into(),
        "-mountpoint".into(),
        mnt.clone().into(),
        file.into(),
    ];
    run_tool(hdiutil, &attach, &run_quiet)?;
    let new = if admin {
        work.join(NEW_BUNDLE)
    } else {
        parent.join(SIBLING_BUNDLE)
    };
    remove_bundle(&new);
    let copied = run_tool(
        Path::new("/usr/bin/ditto"),
        &[mnt.join(NEW_BUNDLE).into(), new.clone().into()],
        &run_quiet,
    );
    let detach = |extra: &[&str]| {
        let mut a = vec![OsString::from("detach")];
        a.extend(extra.iter().map(OsString::from));
        a.push(mnt.clone().into());
        run_tool(hdiutil, &a, &run_quiet)
    };
    if let Err(e) = detach(&[]).or_else(|_| detach(&["-force"])) {
        tracing::warn!("updater: cannot detach {}: {e}", mnt.display());
    }
    let checked = copied.and_then(|()| verify_bundle(&new, &release.version));
    if let Err(e) = checked {
        remove_bundle(&new);
        return Err(e);
    }
    let quarantine: Vec<OsString> = vec![
        "-dr".into(),
        "com.apple.quarantine".into(),
        new.clone().into(),
    ];
    if let Err(e) = run_tool(Path::new("/usr/bin/xattr"), &quarantine, &run_quiet) {
        tracing::debug!("updater: xattr: {e}");
    }
    let helper = work.join(HELPER);
    let _ = std::fs::remove_file(work.join(crate::updater::relaunch::RESULT_FILE));
    std::fs::write(&helper, mac_helper_script())
        .map_err(|e| UpdateError::io("write the update helper", &e))?;
    let mut cmd = Command::new("/bin/sh");
    cmd.arg(&helper)
        .arg(std::process::id().to_string())
        .arg(bundle)
        .arg(&new)
        .arg(if admin { "1" } else { "0" })
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::updater::relaunch::detach(&mut cmd);
    cmd.spawn()
        .map_err(|e| UpdateError::io("start the update helper", &e))?;
    Ok(super::Outcome::Quit)
}

#[cfg(target_os = "macos")]
fn verify_bundle(new: &Path, version: &semver::Version) -> Result<(), UpdateError> {
    use std::ffi::OsString;
    super::run_tool(
        Path::new("/usr/bin/codesign"),
        &[
            OsString::from("--verify"),
            OsString::from("--deep"),
            OsString::from("--strict"),
            new.into(),
        ],
        &super::run_quiet,
    )?;
    let plist = std::fs::read_to_string(new.join("Contents/Info.plist"))
        .map_err(|e| UpdateError::io("read Info.plist", &e))?;
    check_plist(&plist, version)
}

#[cfg(target_os = "macos")]
fn remove_bundle(path: &Path) {
    match std::fs::remove_dir_all(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            tracing::debug!("updater: cannot remove {}: {e}", path.display());
        }
        _ => {}
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn helper_script_shape() {
        let s = mac_helper_script();
        assert!(s.starts_with("#!/bin/sh\n"));
        // No command substitution (arithmetic `$((` is fine).
        assert!(!s.replace("$((", "").contains("$("));
        assert!(!s.contains('`'));
        assert!(!s.contains("eval"));
        for needle in [
            "kill -0 \"$pid\"",
            "with administrator privileges",
            "quoted form of item 4 of argv",
            "exec /usr/bin/open -n \"$app\"",
            "echo ok >\"$result\"",
            "echo failed:swap >\"$result\"",
            "echo failed:wait >\"$result\"",
            "-e 'end run' \"$swap_root\" ",
        ] {
            assert!(s.contains(needle), "{needle}");
        }
        // Root takes the bundle over after moving it in (not before: the
        // user could swap the path in their cache dir in between).
        let root = s.lines().find(|l| l.starts_with("swap_root=")).unwrap();
        let mv = root.find("/bin/mv \"$3\" \"$1\"").unwrap();
        let chown = root.find("/usr/sbin/chown -R root:wheel \"$1\"").unwrap();
        assert!(mv < chown && root.contains("/bin/chmod -R go-w \"$1\""));
        assert!(
            !s.lines()
                .any(|l| l.starts_with("swap=") && l.contains("chown"))
        );
        // Tools by absolute path only.
        for line in s.lines() {
            for tool in ["mv", "rm", "sh", "open", "chown", "chmod"] {
                for prefix in [" ", "'", "{ ", "|| "] {
                    let bare = format!("{prefix}{tool} ");
                    assert!(
                        !line.contains(&bare) || line.contains(&format!("/{tool} ")),
                        "{line}"
                    );
                }
            }
        }
    }

    /// Runs the non-admin branch with `/usr/bin/open` replaced by `true`.
    #[cfg(unix)]
    fn run_helper(dir: &Path, app: &Path, new: &Path) -> String {
        let script = dir.join(HELPER);
        std::fs::write(
            &script,
            mac_helper_script().replace("/usr/bin/open", "true"),
        )
        .unwrap();
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let pid = gone.id();
        gone.wait().unwrap();
        let status = std::process::Command::new("/bin/sh")
            .arg(&script)
            .arg(pid.to_string())
            .arg(app)
            .arg(new)
            .arg("0")
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::read_to_string(dir.join("result")).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn helper_swaps_the_bundle() {
        let tmp = tempfile::tempdir().unwrap();
        let work = tmp.path().join("updates/0.3.0");
        let apps = tmp.path().join("Applications with space");
        std::fs::create_dir_all(&work).unwrap();
        let app = apps.join("Rustket.app");
        let new = apps.join(SIBLING_BUNDLE);
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        std::fs::create_dir_all(new.join("Contents")).unwrap();
        std::fs::write(app.join("Contents/v"), "old").unwrap();
        std::fs::write(new.join("Contents/v"), "new").unwrap();
        assert_eq!(run_helper(&work, &app, &new), "ok\n");
        assert_eq!(
            std::fs::read_to_string(app.join("Contents/v")).unwrap(),
            "new"
        );
        assert!(!new.exists());
        assert!(!apps.join("Rustket.app.rustket-old").exists());

        // The new bundle is missing: the old app is put back.
        assert_eq!(run_helper(&work, &app, &new), "failed:swap\n");
        assert_eq!(
            std::fs::read_to_string(app.join("Contents/v")).unwrap(),
            "new"
        );
        assert!(!apps.join("Rustket.app.rustket-old").exists());
    }

    /// The root swap, with `chown` (root only) replaced by `ok`.
    #[cfg(unix)]
    fn run_root_swap(app: &Path, new: &Path, ok: &str) -> bool {
        let s = mac_helper_script();
        let line = s.lines().find(|l| l.starts_with("swap_root=")).unwrap();
        let cmd = line
            .trim_start_matches("swap_root='")
            .trim_end_matches('\'')
            .replace("/usr/sbin/chown -R root:wheel", ok);
        let old = format!("{}.rustket-old", app.display());
        std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(cmd)
            .arg("_")
            .arg(app)
            .arg(old)
            .arg(new)
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    #[cfg(unix)]
    #[test]
    fn root_swap_fixes_permissions_or_rolls_back() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("Rustket.app");
        let new = tmp.path().join("new.app");
        let v = |p: &Path| std::fs::read_to_string(p.join("Contents/v")).unwrap();
        for (dir, text) in [(&app, "old"), (&new, "new")] {
            std::fs::create_dir_all(dir.join("Contents")).unwrap();
            std::fs::write(dir.join("Contents/v"), text).unwrap();
            let all = std::fs::Permissions::from_mode(0o777);
            std::fs::set_permissions(dir.join("Contents"), all).unwrap();
        }
        // The ownership change fails: the old app is put back.
        assert!(!run_root_swap(&app, &new, "false"));
        assert_eq!(v(&app), "old");
        assert!(!tmp.path().join("Rustket.app.rustket-old").exists());

        std::fs::create_dir_all(new.join("Contents")).unwrap();
        std::fs::write(new.join("Contents/v"), "new").unwrap();
        let all = std::fs::Permissions::from_mode(0o777);
        std::fs::set_permissions(new.join("Contents"), all).unwrap();
        assert!(run_root_swap(&app, &new, "true"));
        assert_eq!(v(&app), "new");
        let mode = std::fs::metadata(app.join("Contents"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o022, 0, "{mode:o}");
        assert!(!new.exists() && !tmp.path().join("Rustket.app.rustket-old").exists());
    }

    #[test]
    fn plist_checks() {
        let plist = |id: &str, v: &str| {
            format!(
                "<plist><dict><key>CFBundleIdentifier</key><string>{id}</string>\
                 <key>CFBundleShortVersionString</key><string>{v}</string></dict></plist>"
            )
        };
        let v = semver::Version::parse("0.3.0-rc1").unwrap();
        assert_eq!(
            check_plist(&plist(crate::BUNDLE_ID, "0.3.0-rc1"), &v),
            Ok(())
        );
        assert!(check_plist(&plist(crate::BUNDLE_ID, "0.3.0"), &v).is_err());
        assert!(check_plist(&plist("org.fork.rustket", "0.3.0-rc1"), &v).is_err());
        assert!(check_plist("", &v).is_err());
    }

    #[test]
    fn translocation() {
        assert!(translocated(Path::new(
            "/private/var/folders/x/AppTranslocation/ABC/d/Rustket.app"
        )));
        assert!(!translocated(Path::new("/Applications/Rustket.app")));
    }
}
