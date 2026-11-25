//! Packaging metadata stays consistent with the app (`packaging/`). The full
//! `.deb`/AppDir build is the ignored `package_script` test:
//! `cargo test -p rustket --test packaging -- --ignored` (runs a `dist` build).

#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Reads a repo file with `\n` line endings (Windows checkouts get CRLF).
fn read(rel: &str) -> String {
    std::fs::read_to_string(root().join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
        .replace("\r\n", "\n")
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

// Mac App Store flavor (docs/app-store.md, .github/workflows/app-store.yml).

/// The lines of `text` without YAML/shell comment lines.
fn without_comments(text: &str) -> String {
    text.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .map(|l| format!("{l}\n"))
        .collect()
}

/// The text of job `name` in a workflow (up to the next job), comments removed.
fn job(workflow: &str, name: &str, next: Option<&str>) -> String {
    let start = index_of(workflow, &format!("\n  {name}:\n"), name);
    let end = next.map_or(workflow.len(), |n| {
        index_of(workflow, &format!("\n  {n}:\n"), n)
    });
    assert!(start < end, "{name} comes before {next:?}");
    without_comments(&workflow[start..end])
}

#[test]
fn app_store_workflow() {
    let wf = read(".github/workflows/app-store.yml");
    let code = without_comments(&wf);
    assert!(wf.contains("\n  RUSTKET_CARGO_FEATURES: mas\n"));
    // Dependency build scripts in mas-build get a read-only token.
    assert!(wf.contains("\npermissions:\n  contents: read\n\n"));
    // A leading zero in the offset is not octal.
    assert_eq!(wf.matches("GITHUB_RUN_NUMBER + 10#$offset").count(), 2);
    assert!(
        !wf.contains("self-update"),
        "no updater in the store workflow"
    );
    assert!(!wf.contains("RUSTKET_UPDATE_REPO:"));
    assert!(wf.contains("\n  DEVELOPER_DIR: /Applications/Xcode_26.6.app/Contents/Developer\n"));
    assert!(wf.contains("\n    branches: ['app-store-dry-run']\n"));
    assert!(
        !wf.contains("\n  release:"),
        "releases made with GITHUB_TOKEN trigger nothing"
    );
    assert!(wf.contains("\n  group: app-store-${{ github.ref }}\n  cancel-in-progress: false\n"));

    let build = job(&wf, "mas-build", Some("mas-upload"));
    let upload = job(&wf, "mas-upload", Some("mas-sandbox-test"));
    let sandbox = job(&wf, "mas-sandbox-test", None);
    for (name, text) in [
        ("mas-build", &build),
        ("mas-upload", &upload),
        ("mas-sandbox-test", &sandbox),
    ] {
        assert!(text.contains("\n    runs-on: macos-26\n"), "{name} runs-on");
        assert!(
            text.contains("persist-credentials: false"),
            "{name} checkout"
        );
    }

    // The build job runs the app's code and gets no secrets.
    assert!(!build.contains("secrets."));
    assert!(!build.contains("environment:"));
    assert!(
        build.contains("MAS: 1\n          APP_ONLY: 1\n        run: packaging/macos/build.sh\n")
    );
    assert!(build.contains("-C \"$app/MacOS\" rustket -C ../Resources THIRD-PARTY-NOTICES.txt"));
    assert!(build.contains("retention-days: 7"));

    // The signing jobs run no cargo and never execute the binary.
    for (name, text) in [("mas-upload", &upload), ("mas-sandbox-test", &sandbox)] {
        for banned in ["cargo", "rustup", "--version", "--deep"] {
            assert!(!text.contains(banned), "{name} contains {banned}");
        }
        for run in text.match_indices("run: packaging/macos/build.sh") {
            let step = &text[text[..run.0].rfind("- name:").unwrap()..run.0];
            assert!(step.contains("MAS: 1\n"), "{name}: {step}");
            assert!(step.contains("PREBUILT_APP: 1\n"), "{name}: {step}");
            assert!(step.contains("MAS_BINARY: ${{ runner.temp }}/mas-binary/rustket\n"));
        }
        assert!(text.contains("if [ \"$listing\" != \"THIRD-PARTY-NOTICES.txt rustket \" ]; then"));
        assert!(text.contains("if: always()"), "{name} cleans up");
    }
    assert!(upload.contains("\n    environment: app-store\n"));
    assert!(upload.contains("\n    needs: [mas-build]\n"));
    assert!(upload.contains(
        "\n    if: startsWith(github.ref, 'refs/tags/v') || github.ref == 'refs/heads/app-store-dry-run'\n"
    ));
    assert!(sandbox.contains("\n    environment: macos-signing\n"));
    assert!(sandbox.contains("MAS_SANDBOX_TEST: 1\n"));
    assert!(sandbox.contains("github.ref == 'refs/heads/app-store-dry-run'"));

    // Upload only from v* tags, or on request from the dry-run branch.
    assert!(upload.contains(
        "      UPLOAD: ${{ startsWith(github.ref, 'refs/tags/v') || (github.event_name == 'workflow_dispatch' && github.ref == 'refs/heads/app-store-dry-run' && inputs.upload) }}\n"
    ));
    assert!(upload.contains("if: steps.signing.outputs.sign == 'true' && env.UPLOAD == 'true'"));
    assert_eq!(code.matches("xcrun altool --upload-package").count(), 1);
    assert_eq!(code.matches("xcrun altool --validate-app").count(), 1);
    assert_eq!(code.matches("--apple-id \"$MAS_APPLE_ID\"").count(), 2);
    assert_eq!(
        code.matches("python3 -I packaging/macos/altool_result.py \"$log\" \"$code\"")
            .count(),
        2
    );
    for banned in ["--auth-string", "--upload-app"] {
        assert!(!wf.contains(banned), "{banned}");
    }
    // The installer identity is not filtered by the codesigning policy.
    assert!(upload.contains("all=\"$(security find-identity -v \"$SIGNING_KEYCHAIN\")\""));
    assert!(upload.contains("-T /usr/bin/codesign -T /usr/bin/productbuild"));

    // Actions are pinned to the same commits as the release workflow.
    let release = read(".github/workflows/release.yml");
    for line in wf
        .lines()
        .filter(|l| l.trim_start().starts_with("- uses:") || l.trim_start().starts_with("uses:"))
    {
        let action = line.split("uses:").nth(1).unwrap().trim();
        let sha = action.split('@').nth(1).unwrap().split(' ').next().unwrap();
        assert_eq!(sha.len(), 40, "{action} is pinned to a commit");
        assert!(release.contains(action), "{action} as in release.yml");
    }
}

#[test]
fn release_pins_the_macos_toolchain() {
    let release = read(".github/workflows/release.yml");
    for name in ["macos-build", "macos"] {
        let start = index_of(&release, &format!("\n  {name}:\n"), name) + 1;
        // Up to the next job key or the comment above it.
        let job: String = release[start..]
            .lines()
            .enumerate()
            .take_while(|(i, l)| *i == 0 || !(l.starts_with("  ") && !l.starts_with("   ")))
            .map(|(_, l)| format!("{l}\n"))
            .collect();
        assert!(job.contains("\n    runs-on: macos-26\n"), "{name}");
        assert!(
            job.contains(
                "\n      DEVELOPER_DIR: /Applications/Xcode_26.6.app/Contents/Developer\n"
            ),
            "{name}"
        );
    }
    assert!(!release.contains("rustket/mas"));
    let ci = read(".github/workflows/ci.yml");
    assert_eq!(ci.matches("--features rustket/mas").count(), 4);
    assert!(ci.contains("- run: bash -n packaging/macos/build.sh\n"));
}

/// `<key>` → value lines of a flat XML plist (entitlements files).
fn plist_entries(text: &str) -> Vec<(String, String)> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    lines
        .iter()
        .enumerate()
        .filter_map(|(i, l)| {
            let key = l.strip_prefix("<key>")?.strip_suffix("</key>")?;
            Some((key.to_owned(), lines[i + 1].to_owned()))
        })
        .collect()
}

#[test]
fn mas_entitlements() {
    let id = rustket::BUNDLE_ID;
    let text = read("packaging/macos/rustket-mas.entitlements");
    let entries = plist_entries(&text);
    let keys: Vec<&str> = entries.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        keys,
        [
            "com.apple.application-identifier",
            "com.apple.developer.team-identifier",
            "com.apple.security.app-sandbox",
            "com.apple.security.files.user-selected.read-write",
            "com.apple.security.network.client",
        ]
    );
    let value = |k: &str| entries.iter().find(|(key, _)| key == k).unwrap().1.clone();
    let team = value("com.apple.developer.team-identifier");
    let team = team
        .strip_prefix("<string>")
        .and_then(|t| t.strip_suffix("</string>"))
        .unwrap();
    assert_eq!(team, "D6JXBXV9Q5");
    assert_eq!(
        value("com.apple.application-identifier"),
        format!("<string>{team}.{id}</string>")
    );
    for key in &keys[2..] {
        assert_eq!(value(key), "<true/>", "{key}");
    }

    let test = read("packaging/macos/rustket-sandbox-test.entitlements");
    let test_keys: Vec<String> = plist_entries(&test).into_iter().map(|(k, _)| k).collect();
    assert_eq!(test_keys, keys[2..]);
    for text in [&text, &test] {
        for banned in [
            "network.server",
            "temporary-exception",
            "automation.apple-events",
            "files.all",
            "get-task-allow",
            "security.cs.",
            "inherit",
        ] {
            assert!(!text.contains(banned), "{banned}");
        }
    }
    let mac = read("packaging/macos/build.sh");
    assert!(mac.contains("ENTITLEMENTS=\"$MACOS/rustket-mas.entitlements\""));
    assert!(mac.contains("ENTITLEMENTS=\"$MACOS/rustket-sandbox-test.entitlements\""));
}

/// `LOCALIZATIONS="…"` of packaging/macos/build.sh.
fn mac_localizations() -> Vec<String> {
    read("packaging/macos/build.sh")
        .lines()
        .find_map(|l| l.strip_prefix("LOCALIZATIONS=\""))
        .and_then(|l| l.strip_suffix('"'))
        .expect("LOCALIZATIONS in build.sh")
        .split(' ')
        .map(str::to_owned)
        .collect()
}

/// The bundle localizations follow the bundled UI languages (both flavors
/// share `write_info_plist` and `install_localizations`).
#[test]
fn mac_localizations_in_sync() {
    let i18n = read("crates/rustket/src/i18n.rs");
    let complete: Vec<String> = i18n
        .lines()
        .find_map(|l| {
            l.trim()
                .strip_prefix("pub(crate) const BUNDLED_COMPLETE: &[&str] = &[")
        })
        .and_then(|l| l.strip_suffix("];"))
        .expect("BUNDLED_COMPLETE")
        .split(", ")
        .map(|s| match s.trim_matches('"') {
            "zh" => "zh-Hans".to_owned(),
            other => other.to_owned(),
        })
        .collect();
    let langs = mac_localizations();
    assert_eq!(langs, complete);

    let mut dirs: Vec<String> = std::fs::read_dir(root().join("packaging/macos/InfoPlist"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    dirs.sort();
    let mut want: Vec<String> = langs.iter().map(|l| format!("{l}.lproj")).collect();
    want.sort();
    assert_eq!(dirs, want);
    for l in &langs {
        let strings = read(&format!(
            "packaging/macos/InfoPlist/{l}.lproj/InfoPlist.strings"
        ));
        let line = strings
            .lines()
            .find(|s| s.starts_with("\"NSLocalNetworkUsageDescription\" = \""))
            .unwrap_or_else(|| panic!("{l}: NSLocalNetworkUsageDescription"));
        assert!(
            line.ends_with("\";") && line.contains("Rustket"),
            "{l}: {line}"
        );
    }

    let mac = read("packaging/macos/build.sh");
    assert!(mac.contains(
        "    for l in $LOCALIZATIONS; do\n        langs+=\"        <string>$l</string>\"$'\\n'\n"
    ));
    assert!(
        mac.contains("    <key>CFBundleLocalizations</key>\n    <array>\n${langs}    </array>\n")
    );
    // Both assembly branches (MAS and dmg) install them.
    assert_eq!(mac.matches("\n    install_localizations\n").count(), 2);
    assert_eq!(mac.matches("\n    compile_assets\n").count(), 2);
    assert_eq!(
        mac.matches("\n    write_info_plist \"$APP/Contents/Info.plist\"\n")
            .count(),
        2
    );
}

#[test]
fn mas_info_plist() {
    let mac = read("packaging/macos/build.sh");
    let id = rustket::BUNDLE_ID;
    for want in [
        format!("            <key>CFBundleURLName</key>\n            <string>{id}</string>\n"),
        format!("    <key>CFBundleIdentifier</key>\n    <string>{id}</string>\n"),
        format!("BUNDLE_ID={id}\n"),
        "    <key>LSApplicationCategoryType</key>\n    <string>public.app-category.business</string>\n".to_owned(),
        "    <key>ITSAppUsesNonExemptEncryption</key>\n    <false/>".to_owned(),
        "    <key>CFBundleSupportedPlatforms</key>\n    <array>\n        <string>MacOSX</string>\n    </array>\n".to_owned(),
        "    <key>CFBundleIconFile</key>\n    <string>rustket</string>\n".to_owned(),
        "    <key>CFBundleIconName</key>\n    <string>AppIcon</string>".to_owned(),
        "    <key>CFBundleVersion</key>\n    <string>$bundle_version</string>\n".to_owned(),
        "        bundle_version=\"$BUILD_NUMBER\"\n".to_owned(),
        "    BUILD_NUMBER=\"${MAS_BUILD_NUMBER:-}\"\n".to_owned(),
        "    [[ \"$VERSION\" =~ ^[0-9]+\\.[0-9]+\\.[0-9]+$ ]] ||\n".to_owned(),
        "    <key>LSMinimumSystemVersion</key>\n    <string>$MACOSX_DEPLOYMENT_TARGET</string>\n".to_owned(),
        "    <key>NSHumanReadableCopyright</key>\n    <string>Copyright © 2026 Sergey Sokolov. MIT License.</string>\n".to_owned(),
        "    <key>NSLocalNetworkUsageDescription</key>\n".to_owned(),
        "STAGE=\"$OUT/macos-mas\"".to_owned(),
    ] {
        assert!(mac.contains(&want), "missing {want}");
    }
    for key in [
        "BuildMachineOSBuild",
        "DTCompiler",
        "DTPlatformBuild",
        "DTPlatformName",
        "DTPlatformVersion",
        "DTSDKBuild",
        "DTSDKName",
        "DTXcode",
        "DTXcodeBuild",
    ] {
        assert!(mac.contains(&format!("    <key>{key}</key>\n")), "{key}");
    }
    // The DT probe runs only when packaging/macos/xcode-probe exists.
    assert!(mac.contains(
        "    if [ ! -d \"$dir\" ]; then\n        echo \"notice: packaging/macos/xcode-probe"
    ));

    // The MAS branch writes no install-kind marker and runs the binary only
    // when it built it.
    let mas = index_of(
        &mac,
        "if [ \"$MAS\" = 1 ]; then\n    dt_values\n",
        "build.sh",
    );
    let dmg = index_of(&mac, "\nelif [ \"$PREBUILT\" = 1 ]; then\n", "build.sh");
    let branch = &mac[mas..dmg];
    assert!(!branch.contains("install_marker"));
    assert!(branch.contains("die \"install-kind marker in an App Store build\""));
    assert_eq!(mac.matches("install_marker \"").count(), 1);
    assert!(branch.contains(
        "    if [ \"$PREBUILT\" != 1 ]; then\n        \"$APP/Contents/MacOS/rustket\" --version"
    ));
    assert!(
        branch.contains("        install -m755 \"$MAS_BINARY\" \"$APP/Contents/MacOS/rustket\"\n")
    );
    assert!(branch.contains("    check_binary \"$APP/Contents/MacOS/rustket\"\n"));
}

#[test]
fn mas_signing_and_gates() {
    let mac = read("packaging/macos/build.sh");
    let sign = &mac[index_of(
        &mac,
        "    ENTITLEMENTS=\"$MACOS/rustket-mas.entitlements\"",
        "build.sh",
    )..];
    let sign = &sign[..index_of(sign, "    echo \"$PKG\"", "build.sh")];
    let order = [
        "    check_profile \"$MAS_PROVISIONING_PROFILE\"\n",
        "    install -m644 \"$MAS_PROVISIONING_PROFILE\" \"$APP/Contents/embedded.provisionprofile\"\n",
        "    xattr -cr \"$APP\"\n",
        "    codesign --force -o runtime --entitlements \"$ENTITLEMENTS\" --sign \"$IDENTITY\" \"$APP\"\n",
        "    codesign --verify --strict --verbose=2 \"$APP\"",
        "    check_entitlements \"$ENTITLEMENTS\"\n",
        "    productbuild --component \"$APP\" /Applications --sign \"$MAS_INSTALLER_IDENTITY\" \"$PKG\"",
        "    pkgutil --check-signature \"$PKG\"",
    ];
    let mut at = 0;
    for step in order {
        let i = index_of(sign, step, "MAS signing");
        assert!(i >= at, "{step} out of order");
        at = i;
    }
    assert_eq!(sign.matches("codesign --force").count(), 1, "one signature");
    assert!(!sign.contains("--deep"));
    assert!(!sign.contains("notarize"));
    // Profile checks.
    for want in [
        "\"com.apple.application-identifier\") != app_id",
        "\"ProvisionedDevices\" in p",
        "if days < 0:",
        "team not in (p.get(\"TeamIdentifier\") or [])",
    ] {
        assert!(mac.contains(want), "{want}");
    }
    assert!(mac.contains("TEAM_ID=\"${APPLE_TEAM_ID:-D6JXBXV9Q5}\"\n"));
    // Static gates on both slices; a failing nm fails the build.
    for want in [
        "    lipo \"$bin\" -verify_arch arm64 x86_64 ||",
        "    for arch in arm64 x86_64; do\n",
        "info=\"$(vtool -arch \"$arch\" -show-build \"$bin\")\" || die",
        "syms=\"$(nm -u -arch \"$arch\" \"$bin\")\" || die",
        "grep -E '^_(CGS|SLS)[A-Z]'",
        "for marker in didDismissAlert api.github.com install-kind SHA256SUMS; do",
        "[ \"$minos\" = \"$MACOSX_DEPLOYMENT_TARGET\" ]",
    ] {
        assert!(mac.contains(want), "{want}");
    }
    // Assets.car next to the icns.
    assert!(mac.contains(
        "xcrun actool --compile \"$APP/Contents/Resources\" --platform macosx --target-device mac \\\n        --app-icon AppIcon --minimum-deployment-target \"$MACOSX_DEPLOYMENT_TARGET\""
    ));
}

#[test]
fn mas_feature_excludes_updater() {
    let manifest: toml::Table = toml::from_str(&read("crates/rustket/Cargo.toml")).unwrap();
    let features = manifest["features"].as_table().unwrap();
    assert_eq!(features["mas"].as_array().map(Vec::len), Some(0));
    assert!(
        features.get("default").is_none(),
        "mas stays off by default"
    );
    let lib = read("crates/rustket/src/lib.rs");
    assert!(
        lib.contains("#[cfg(all(feature = \"mas\", feature = \"self-update\"))]\ncompile_error!(")
    );
    // The App Store build compiles with exactly the mas feature.
    let mac = read("packaging/macos/build.sh");
    assert!(
        mac.contains(
            "*\" self-update \"*) die \"the App Store build must not enable self-update\""
        )
    );
}

#[test]
fn winit_patch() {
    let manifest: toml::Table = toml::from_str(&read("Cargo.toml")).unwrap();
    assert_eq!(
        manifest["patch"]["crates-io"]["winit"]["path"].as_str(),
        Some("vendor/winit")
    );
    let exclude = manifest["workspace"]["exclude"].as_array().unwrap();
    assert!(exclude.iter().any(|e| e.as_str() == Some("vendor/winit")));
    let members = manifest["workspace"]["members"].as_array().unwrap();
    assert!(
        !members
            .iter()
            .any(|m| m.as_str().unwrap().starts_with("vendor/"))
    );

    let vendored: toml::Table = toml::from_str(&read("vendor/winit/Cargo.toml")).unwrap();
    let version = vendored["package"]["version"].as_str().unwrap();
    let lock = read("Cargo.lock");
    let entry = index_of(&lock, "[[package]]\nname = \"winit\"\n", "Cargo.lock");
    let entry = &lock[entry..];
    let entry = &entry[..entry[1..]
        .find("[[package]]")
        .map_or(entry.len(), |i| i + 1)];
    assert!(entry.contains(&format!("version = \"{version}\"\n")));
    assert!(
        !entry.contains("source = "),
        "Cargo.lock uses the path source"
    );

    for file in ["ffi.rs", "window_delegate.rs"] {
        let text = read(&format!("vendor/winit/src/platform_impl/macos/{file}"));
        assert!(!text.contains("fn CGSMainConnectionID"), "{file}");
        assert!(!text.contains("ffi::CGSMainConnectionID"), "{file}");
        assert!(
            !text.contains("ffi::CGSSetWindowBackgroundBlurRadius("),
            "{file}"
        );
        assert!(
            !text.contains("fn CGSSetWindowBackgroundBlurRadius"),
            "{file}"
        );
    }
    assert!(
        read("vendor/winit/src/platform_impl/macos/window_delegate.rs")
            .contains("    pub fn set_blur(&self, _blur: bool) {}\n")
    );
    assert!(read("vendor/winit/LICENSE").contains("Apache License"));
    assert!(read("vendor/winit/README.rustket.md").contains("```diff\n"));
}

/// The asset catalog icon set renders the same squircle tile as the icns
/// (examples/gen_icons.rs `--appiconset`).
#[test]
fn app_icon_set() {
    let dir = "packaging/macos/AppIcon.appiconset";
    let json: serde_json::Value =
        serde_json::from_str(&read(&format!("{dir}/Contents.json"))).unwrap();
    let images = json["images"].as_array().unwrap();
    let mut pixels = Vec::new();
    for image in images {
        assert_eq!(image["idiom"], "mac");
        let size = image["size"].as_str().unwrap();
        let points: u32 = size.split_once('x').unwrap().0.parse().unwrap();
        let scale: u32 = image["scale"]
            .as_str()
            .unwrap()
            .trim_end_matches('x')
            .parse()
            .unwrap();
        let file = image["filename"].as_str().unwrap();
        let data = bytes(&format!("{dir}/{file}"));
        let info = png::Decoder::new(std::io::Cursor::new(&data))
            .read_info()
            .unwrap();
        let px = points * scale;
        assert_eq!((info.info().width, info.info().height), (px, px), "{file}");
        pixels.push((px, data));
    }
    let mut points: Vec<String> = images
        .iter()
        .map(|i| {
            format!(
                "{}@{}",
                i["size"].as_str().unwrap(),
                i["scale"].as_str().unwrap()
            )
        })
        .collect();
    points.sort();
    assert_eq!(
        points,
        [
            "128x128@1x",
            "128x128@2x",
            "16x16@1x",
            "16x16@2x",
            "256x256@1x",
            "256x256@2x",
            "32x32@1x",
            "32x32@2x",
            "512x512@1x",
            "512x512@2x",
        ]
    );
    // Every PNG the icns has is byte-identical to the catalog's (ic10 is 1024).
    let icns = bytes("crates/rustket/assets/icons/rustket.icns");
    let len = |at: usize| u32::from_be_bytes(icns[at..at + 4].try_into().unwrap()) as usize;
    let mut at = 8;
    let mut compared = 0;
    while at < icns.len() {
        let element = &icns[at + 8..at + len(at + 4)];
        let info = png::Decoder::new(std::io::Cursor::new(element))
            .read_info()
            .unwrap();
        let px = info.info().width;
        let (_, data) = pixels.iter().find(|(p, _)| *p == px).unwrap();
        assert!(data == element, "{px} px differs from the icns");
        compared += 1;
        at += len(at + 4);
    }
    assert_eq!(compared, 8);
    assert!(read("packaging/macos/build.sh").contains("cp -R \"$MACOS/AppIcon.appiconset\""));
}

/// packaging/macos/altool_result.py fails on errors whatever the exit code.
#[cfg(unix)]
#[test]
fn altool_result_script() {
    if std::process::Command::new("python3")
        .arg("-V")
        .output()
        .is_err()
    {
        eprintln!("python3 not found; skipped");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let run = |log: &str, code: i32| {
        let file = dir.path().join("log");
        std::fs::write(&file, log).unwrap();
        std::process::Command::new("python3")
            .arg("-I")
            .arg(root().join("packaging/macos/altool_result.py"))
            .arg(&file)
            .arg(code.to_string())
            .output()
            .unwrap()
            .status
            .success()
    };
    let ok = r#"{"tool-version":"26.0","os-version":"26.1","success-message":"No errors validating archive"}"#;
    assert!(run(ok, 0));
    assert!(run("2026-11-25 No errors uploading. 0 errors.\n", 0));
    assert!(!run(ok, 1));
    assert!(!run("", 0));
    assert!(!run("ERROR: [ContentDelivery.Uploader] failed\n", 0));
    assert!(!run("*** Error: Validation failed (409)\n", 0));
    // Errors behind a log prefix, and JSON after other braces.
    assert!(!run(
        "2026-11-25 10:00:00.000 altool[123:456] *** Error: Error uploading 'Rustket.pkg'.\n",
        0
    ));
    assert!(!run(
        "2026-11-25 10:00:00.000 ERROR: [ContentDelivery.Uploader.0x1] failed\n",
        0
    ));
    assert!(!run(
        "2026-11-25 10:00:00.000 altool[1:2] ERROR ITMS-90296: \"App sandbox not enabled.\"\n",
        0
    ));
    assert!(!run(
        r#"{"product-errors":[{"code":-19208,"message":"Invalid Provisioning Profile"}]}"#,
        0
    ));
    assert!(!run(
        "note {not json}\n{\"product-errors\":[{\"code\":409}]}\n",
        0
    ));
    assert!(run(&format!("note {{not json}}\n{ok}\n"), 0));
}
