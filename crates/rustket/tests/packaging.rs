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
