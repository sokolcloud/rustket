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

#[test]
#[ignore = "builds the dist profile and the .deb (minutes); run with --ignored"]
fn package_script() {
    let status = std::process::Command::new("bash")
        .arg(root().join("packaging/test.sh"))
        .status()
        .unwrap_or_else(|e| panic!("packaging/test.sh: {e}"));
    assert!(status.success(), "packaging/test.sh failed");
}
