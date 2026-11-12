//! Packaging metadata stays consistent with the app (`packaging/`). The full
//! `.deb`/AppDir build is the ignored `package_script` test:
//! `cargo test -p rustket --test packaging -- --ignored` (runs a `dist` build).

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn desktop_file_matches_the_app() {
    let desktop = read("packaging/linux/rustket.desktop");
    let lines: Vec<&str> = desktop.lines().collect();
    let id = rustket::APP_ID;
    for want in [
        "[Desktop Entry]".to_owned(),
        format!("Exec={} %u", rustket::cli::BIN),
        format!("Icon={id}"),
        // Taskbar grouping and the icon follow the window's application id.
        format!("StartupWMClass={id}"),
        "Categories=Network;InstantMessaging;Chat;".to_owned(),
        format!("MimeType={};", rustket::url_handler::MIME),
    ] {
        assert!(lines.contains(&want.as_str()), "missing {want}");
    }
    // iced 0.14 never completes startup notification.
    assert!(!lines.contains(&"StartupNotify=true"));
    // The packaged file name is what url_handler prefers.
    assert_eq!(
        rustket::url_handler::PACKAGED_DESKTOP_FILE,
        format!("{id}.desktop")
    );
    assert_eq!(
        read("crates/rustket/src/main.rs")
            .matches("application_id: rustket::APP_ID")
            .count(),
        1,
        "main.rs sets the window's application id from APP_ID"
    );
    let metainfo = read("packaging/linux/rustket.metainfo.xml");
    assert!(metainfo.contains(&format!("<id>{id}</id>")));
    assert!(metainfo.contains(&format!(
        "<launchable type=\"desktop-id\">{id}.desktop</launchable>"
    )));
    assert!(metainfo.contains(&format!("<binary>{}</binary>", rustket::cli::BIN)));
    assert!(metainfo.contains(&format!(
        "<release version=\"{}\"",
        env!("CARGO_PKG_VERSION")
    )));
}

fn icon_sizes() -> Vec<String> {
    read("packaging/common.sh")
        .lines()
        .find_map(|l| l.strip_prefix("ICON_SIZES=\""))
        .and_then(|l| l.strip_suffix('"'))
        .expect("ICON_SIZES in common.sh")
        .split(' ')
        .map(str::to_owned)
        .collect()
}

#[test]
fn icons_for_every_packaged_size() {
    let sizes = icon_sizes();
    assert!(sizes.iter().any(|s| s == "512"));
    for size in sizes {
        let path = root().join(format!("crates/rustket/assets/icons/rustket-{size}.png"));
        let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let info = png::Decoder::new(std::io::BufReader::new(file))
            .read_info()
            .unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let size: u32 = size.parse().unwrap_or_default();
        assert_eq!((info.info().width, info.info().height), (size, size));
    }
}

/// `[package.metadata.deb]` (cargo-deb, not exercised) follows packaging/deb.
#[test]
fn cargo_deb_metadata_in_sync() {
    let manifest: toml::Table = toml::from_str(&read("crates/rustket/Cargo.toml")).unwrap();
    let deb = &manifest["package"]["metadata"]["deb"];
    let field = |k: &str| deb[k].as_str().unwrap().to_owned();
    let file = |k: &str| read(&format!("packaging/deb/{k}")).trim().to_owned();
    assert_eq!(field("depends"), format!("$auto, {}", file("depends")));
    assert_eq!(field("recommends"), file("recommends"));
    assert_eq!(field("suggests"), file("suggests"));
    // The skip drops our header paragraph (cargo-deb writes its own).
    let header = read("packaging/deb/copyright")
        .lines()
        .take_while(|l| !l.is_empty())
        .count();
    assert_eq!(
        deb["license-file"][1].as_str(),
        Some(header.to_string().as_str())
    );
    let assets: Vec<String> = deb["assets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| format!("{} -> {}", a[0].as_str().unwrap(), a[1].as_str().unwrap()))
        .collect();
    for size in icon_sizes() {
        let want = format!(
            "assets/icons/rustket-{size}.png -> usr/share/icons/hicolor/{size}x{size}/apps/rustket.png"
        );
        assert!(assets.contains(&want), "missing {want}");
    }
    for (src, dest) in [
        ("rustket.1", "usr/share/man/man1/rustket.1"),
        (
            "THIRD-PARTY-NOTICES",
            "usr/share/doc/rustket/THIRD-PARTY-NOTICES",
        ),
        ("lintian-overrides", "usr/share/lintian/overrides/rustket"),
    ] {
        assert!(
            assets.iter().any(|a| a.contains(src) && a.ends_with(dest)),
            "missing {src}"
        );
    }
}

fn bytes(rel: &str) -> Vec<u8> {
    std::fs::read(root().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// rustket.ico (exe resource, installer) and rustket.icns (app bundle), both
/// written by examples/gen_icons.rs.
#[test]
fn windows_and_macos_icons() {
    let ico = bytes("crates/rustket/assets/icons/rustket.ico");
    assert_eq!(ico[..4], [0, 0, 1, 0]);
    let count = u16::from_le_bytes([ico[4], ico[5]]) as usize;
    let sizes: Vec<u32> = (0..count)
        .map(|i| match ico[6 + 16 * i] {
            0 => 256,
            n => u32::from(n),
        })
        .collect();
    assert_eq!(sizes, [16, 24, 32, 48, 64, 128, 256]);

    let icns = bytes("crates/rustket/assets/icons/rustket.icns");
    assert_eq!(&icns[..4], b"icns");
    let len = |at: usize| u32::from_be_bytes(icns[at..at + 4].try_into().unwrap()) as usize;
    assert_eq!(len(4), icns.len());
    let mut kinds = Vec::new();
    let mut at = 8;
    while at < icns.len() {
        kinds.push(String::from_utf8_lossy(&icns[at..at + 4]).into_owned());
        assert_eq!(&icns[at + 8..at + 12], b"\x89PNG", "PNG element");
        at += len(at + 4);
    }
    assert_eq!(at, icns.len());
    for kind in [
        "ic07", "ic08", "ic09", "ic10", "ic11", "ic12", "ic13", "ic14",
    ] {
        assert!(kinds.iter().any(|k| k == kind), "missing {kind}");
    }

    assert!(read("crates/rustket/build.rs").contains("\"assets/icons/rustket.ico\""));
    assert!(read("packaging/windows/rustket.iss").contains("assets\\icons\\rustket.ico"));
    assert!(read("packaging/macos/build.sh").contains("$ICONS/rustket.icns"));
}

/// Identifiers shared by the app and the Windows/macOS packages.
#[test]
fn windows_and_macos_identifiers() {
    let id = "io.github.sokolcloud.rustket";
    let scheme = rustket::url_handler::MIME
        .strip_prefix("x-scheme-handler/")
        .unwrap();
    let iss = read("packaging/windows/rustket.iss");
    assert!(iss.contains(&format!("#define AppUserModelID \"{id}\"")));
    assert!(iss.contains(&format!("#define Scheme \"{scheme}\"")));
    for value in [
        r#"ValueName: ""; ValueData: "URL:Rocket.Chat Protocol""#,
        r#"ValueName: "URL Protocol"; ValueData: """#,
        r#"ValueData: """{app}\rustket.exe"",0""#,
        r#"ValueData: """{app}\rustket.exe"" ""%1""""#,
    ] {
        assert!(iss.contains(value), "missing {value}");
    }
    let mac = read("packaging/macos/build.sh");
    assert!(mac.contains(&format!("<string>{id}</string>")));
    assert!(mac.contains(&format!("<string>{scheme}</string>")));
    assert!(mac.contains("export MACOSX_DEPLOYMENT_TARGET=11.0"));
}

/// The rpm requires the same dlopen'ed libraries as the deb (by soname).
#[test]
fn rpm_spec_mirrors_deb_depends() {
    let spec = read("packaging/rpm/rustket.spec");
    let requires: Vec<&str> = spec
        .lines()
        .filter_map(|l| l.strip_prefix("Requires:"))
        .map(str::trim)
        .collect();
    let deb = read("packaging/deb/depends");
    assert_eq!(requires.len(), deb.trim().split(", ").count());
    for lib in [
        "libxkbcommon.so.0",
        "libxkbcommon-x11.so.0",
        "libX11.so.6",
        "libX11-xcb.so.1",
        "libXcursor.so.1",
        "libXi.so.6",
    ] {
        assert!(
            requires.contains(&format!("{lib}()(64bit)").as_str()),
            "missing {lib}"
        );
    }
    assert!(spec.contains("Recommends:     libdbus-1.so.3()(64bit)"));
    assert!(spec.contains("%global debug_package %{nil}"));
}

#[test]
#[ignore = "builds the dist profile and the .deb (minutes); run with --ignored"]
fn package_script() {
    let status = std::process::Command::new("bash")
        .arg(root().join("packaging/test.sh"))
        .status()
        .unwrap_or_else(|e| panic!("packaging/test.sh: {e}"));
    assert!(status.success(), "packaging/test.sh failed");
}

fn index_of(text: &str, needle: &str, what: &str) -> usize {
    text.find(needle)
        .unwrap_or_else(|| panic!("{what}: missing {needle}"))
}

/// Every package writes the `install-kind` marker the self-updater reads.
#[test]
fn install_kind_markers() {
    let marker = "usr/lib/rustket/install-kind";
    let common = read("packaging/common.sh");
    assert!(common.contains("install_marker()"));
    for (script, kind) in [
        ("packaging/deb/build.sh", "deb"),
        ("packaging/rpm/build.sh", "rpm"),
        ("packaging/appimage/build.sh", "appimage"),
    ] {
        let text = read(script);
        assert!(
            text.contains(&format!("{marker}\" {kind}\n")),
            "{script} writes the {kind} marker"
        );
    }
    let spec = read("packaging/rpm/rustket.spec");
    let files = index_of(&spec, "%files", "rustket.spec");
    assert!(spec[files..].contains("%dir /usr/lib/rustket\n"));
    assert!(spec[files..].contains(&format!("/{marker}\n")));

    let ps1 = read("packaging/windows/build.ps1");
    let write = index_of(
        &ps1,
        "Set-Content -LiteralPath (Join-Path $AppDir 'install-kind') -Value 'zip'",
        "build.ps1",
    );
    assert!(write < index_of(&ps1, "Compress-Archive", "build.ps1"));
    // The installer's file list is explicit and must not ship the zip marker.
    assert!(!read("packaging/windows/rustket.iss").contains("install-kind"));

    let mac = read("packaging/macos/build.sh");
    let write = index_of(
        &mac,
        "install_marker \"$APP/Contents/Resources/install-kind\" dmg",
        "macos/build.sh",
    );
    assert!(write < index_of(&mac, "codesign", "macos/build.sh"));

    let test = read("packaging/test.sh");
    assert!(test.contains(&format!("./{marker}\n")));
    assert!(test.contains(&format!("    {marker}; do")));
    assert!(test.contains("= deb ]"));
    assert!(test.contains("= appimage ]"));
    assert!(read(".github/workflows/release.yml").contains(&format!("/{marker}; do")));
}

/// A silent update started by the app waits for it to quit and starts it
/// again (packaging/windows/rustket.iss).
#[test]
fn inno_silent_relaunch() {
    let iss = read("packaging/windows/rustket.iss");
    assert!(iss.contains("\nPrivilegesRequiredOverridesAllowed=commandline dialog\n"));
    assert!(iss.contains(
        "Filename: \"{app}\\rustket.exe\"; Flags: nowait runasoriginaluser; Check: RelaunchRequested"
    ));
    assert!(
        iss.contains("Result := WizardSilent() and (ExpandConstant('{param:relaunch|0}') = '1');")
    );
    let prepare = index_of(&iss, "function PrepareToInstall", "rustket.iss");
    let body = &iss[prepare..];
    assert!(body.contains("if RelaunchRequested() then"));
    assert!(body.contains("WaitForUpdatingApp()"));
    let wait = index_of(&iss, "function WaitForUpdatingApp", "rustket.iss");
    let wait = &iss[wait..prepare.max(wait)];
    assert!(wait.contains("while IsRustketRunning() and (I < 240) do"));
    assert!(wait.contains("Sleep(250);"));
    // An unfinished update is reported (`updater::relaunch::RESULT_FILE`)
    // and the old copy started again.
    assert!(body.contains("RelaunchExe := ExePath();"));
    let deinit = index_of(&iss, "procedure DeinitializeSetup", "rustket.iss");
    let deinit = &iss[deinit..];
    assert!(deinit.contains("if (RelaunchExe = '') or Installed then"));
    assert!(
        deinit.contains("SaveStringToFile(ExpandConstant('{src}\\result'), 'failed:' + FailedStep")
    );
    assert!(deinit.contains("ExecAsOriginalUser(RelaunchExe, "));
    assert!(iss.contains("if CurStep = ssPostInstall then\n    Installed := True;"));
}

#[cfg(feature = "self-update")]
#[test]
fn inno_app_id_matches() {
    let iss = read("packaging/windows/rustket.iss");
    // `{{` escapes the brace in Inno's AppId.
    let id = rustket::updater::INNO_APP_ID;
    assert!(
        iss.contains(&format!("AppId={{{id}")),
        "rustket.iss AppId is {id}"
    );
}

/// Release packages are built with the self-updater.
#[test]
fn release_builds_enable_the_updater() {
    let release = read(".github/workflows/release.yml");
    assert!(release.contains("\n  RUSTKET_CARGO_FEATURES: self-update\n"));
    assert!(release.contains("\n  RUSTKET_UPDATE_REPO: ${{ github.repository }}\n"));
    assert!(release.contains(
        "run: cargo build --locked --profile dist -p rustket --bin rustket --features \"$RUSTKET_CARGO_FEATURES\"\n"
    ));
    for script in ["packaging/common.sh", "packaging/macos/build.sh"] {
        assert!(
            read(script)
                .contains("${RUSTKET_CARGO_FEATURES:+--features \"$RUSTKET_CARGO_FEATURES\"}"),
            "{script} passes RUSTKET_CARGO_FEATURES"
        );
    }
    let ps1 = read("packaging/windows/build.ps1");
    assert!(ps1.contains("@('--features', $env:RUSTKET_CARGO_FEATURES)"));
    assert!(ps1.contains("--target $Triple @Features"));
    let manifest: toml::Table = toml::from_str(&read("crates/rustket/Cargo.toml")).unwrap();
    let feature = manifest["features"]["self-update"].as_array().unwrap();
    assert!(feature.iter().any(|f| f.as_str() == Some("dep:reqwest")));
    assert!(
        manifest["features"].get("default").is_none(),
        "self-update stays off by default"
    );
}

/// GitHub release notes come from CHANGELOG.md.
#[test]
fn release_notes_script() {
    let release = read(".github/workflows/release.yml");
    let changelog = index_of(&release, "\n  changelog:\n", "release.yml");
    assert!(release[changelog..].contains("bash packaging/release-notes.sh \"$version\""));
    assert_eq!(release.matches("    needs: [changelog]\n").count(), 3);
    assert!(release.contains("    needs: [linux, windows, macos]\n"));
    let publish = index_of(&release, "\n  publish:\n", "release.yml");
    let notes = index_of(
        &release,
        "bash packaging/release-notes.sh \"$VERSION\" >\"$RUNNER_TEMP/notes.md\"",
        "release.yml",
    );
    let create = index_of(&release, "gh release create", "release.yml");
    assert!(publish < notes && notes < create);
    assert!(release[create..].contains("--notes-file \"$RUNNER_TEMP/notes.md\" --generate-notes"));
}

/// `## [x.y.z] - YYYY-MM-DD` → `x.y.z`.
fn changelog_heading(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("## [")?;
    let (version, rest) = rest.split_once(']')?;
    Some((version, rest))
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// Every version bump needs its CHANGELOG.md section (the GitHub release
/// notes are taken from it, see packaging/release-notes.sh).
#[test]
fn changelog_documents_the_current_version() {
    let version = env!("CARGO_PKG_VERSION");
    let text = read("CHANGELOG.md");
    let lines: Vec<&str> = text.lines().collect();
    let headings: Vec<(usize, &str, &str)> = lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| changelog_heading(l).map(|(v, rest)| (i, v, rest)))
        .collect();
    assert_eq!(
        headings.first().map(|h| (h.1, h.2)),
        Some(("Unreleased", "")),
        "CHANGELOG.md starts with ## [Unreleased]"
    );

    let mut previous: Option<semver::Version> = None;
    for &(_, v, rest) in &headings[1..] {
        let parsed = semver::Version::parse(v).unwrap_or_else(|e| panic!("## [{v}]: {e}"));
        let date = rest
            .strip_prefix(" - ")
            .unwrap_or_else(|| panic!("## [{v}] needs \" - YYYY-MM-DD\""));
        assert!(is_date(date), "## [{v}]: bad date {date:?}");
        if let Some(prev) = &previous {
            assert!(parsed < *prev, "## [{v}] must come after older versions");
        }
        previous = Some(parsed);
    }

    let at = headings
        .iter()
        .position(|h| h.1 == version)
        .unwrap_or_else(|| panic!("CHANGELOG.md has no ## [{version}] section"));
    assert_eq!(at, 1, "## [{version}] is the newest release section");
    let start = headings[at].0 + 1;
    let end = headings.get(at + 1).map_or(lines.len(), |h| h.0);
    assert!(
        lines[start..end]
            .iter()
            .any(|l| !l.trim().is_empty() && !l.starts_with('[')),
        "## [{version}] is empty"
    );

    let link = |name: &str| {
        lines
            .iter()
            .find_map(|l| l.strip_prefix(&format!("[{name}]: ")))
            .unwrap_or_else(|| panic!("CHANGELOG.md has no [{name}] link"))
    };
    assert!(
        link("Unreleased").ends_with(&format!("/compare/v{version}...HEAD")),
        "[Unreleased] compares v{version}...HEAD"
    );
    assert!(link(version).contains(&format!("v{version}")));
}

/// packaging/release-notes.sh prints exactly the version's section.
#[cfg(unix)]
#[test]
fn release_notes_script_extracts_the_section() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("CHANGELOG.md");
    std::fs::write(
        &file,
        "# Changelog\n\n## [Unreleased]\n\n## [1.2.10] - 2026-01-02\n\n- ten\n\n\
         ## [1.2.1] - 2026-01-01\n\n### Fixed\n\n- one\n\n## [1.2.0] - 2025-12-31\n\n\
         [Unreleased]: https://example.invalid/compare/v1.2.10...HEAD\n",
    )
    .unwrap();
    let run = |version: &str| {
        std::process::Command::new("bash")
            .arg(root().join("packaging/release-notes.sh"))
            .arg(version)
            .arg(&file)
            .output()
            .unwrap()
    };
    let out = run("1.2.1");
    assert!(out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "### Fixed\n\n- one\n");
    let out = run("1.2.10");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "- ten\n");
    // Empty, missing and link-only sections fail.
    for version in ["1.2.0", "9.9.9", "Unreleased"] {
        assert!(!run(version).status.success(), "{version}");
    }
}
