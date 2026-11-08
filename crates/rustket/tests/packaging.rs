//! Packaging metadata stays consistent with the app (`packaging/`). The full
//! `.deb`/AppDir build is the ignored `package_script` test:
//! `cargo test -p rustket --test packaging -- --ignored` (runs a `dist` build).

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
    for want in [
        "[Desktop Entry]",
        "Exec=rustket %u",
        "Icon=rustket",
        "StartupWMClass=rustket",
        "Categories=Network;InstantMessaging;Chat;",
        &format!("MimeType={};", rustket::url_handler::MIME),
    ] {
        assert!(lines.contains(&want), "missing {want}");
    }
    // The packaged file name is what url_handler prefers.
    assert_eq!(
        rustket::url_handler::PACKAGED_DESKTOP_FILE,
        "rustket.desktop"
    );
    let metainfo = read("packaging/linux/rustket.metainfo.xml");
    assert!(metainfo.contains("<launchable type=\"desktop-id\">rustket.desktop</launchable>"));
    assert!(metainfo.contains("<binary>rustket</binary>"));
    assert!(metainfo.contains(&format!(
        "<release version=\"{}\"",
        env!("CARGO_PKG_VERSION")
    )));
}

#[test]
fn icons_for_every_packaged_size() {
    let common = read("packaging/common.sh");
    let sizes = common
        .lines()
        .find_map(|l| l.strip_prefix("ICON_SIZES=\""))
        .and_then(|l| l.strip_suffix('"'))
        .expect("ICON_SIZES in common.sh");
    assert!(sizes.split(' ').any(|s| s == "512"));
    for size in sizes.split(' ') {
        let path = root().join(format!("crates/rustket/assets/icons/rustket-{size}.png"));
        let file = std::fs::File::open(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let info = png::Decoder::new(std::io::BufReader::new(file))
            .read_info()
            .unwrap_or_else(|e| panic!("{path:?}: {e}"));
        let size: u32 = size.parse().unwrap_or_default();
        assert_eq!((info.info().width, info.info().height), (size, size));
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
