fn main() {
    // `rust_i18n::i18n!` embeds the locale files at compile time.
    println!("cargo:rerun-if-changed=locales");
    #[cfg(windows)]
    windows_resources();
}

/// Icon and version info of rustket.exe. Build scripts are compiled for the
/// host, so this only runs on a Windows host (which has a resource compiler);
/// a Linux type-check of a Windows target (dev/cross-check.sh) skips it.
#[cfg(windows)]
fn windows_resources() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let icon = "assets/icons/rustket.ico";
    println!("cargo:rerun-if-changed={icon}");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(icon)
        .set("ProductName", "Rustket")
        // Task Manager and "Open with" show the file description as the name.
        .set("FileDescription", "Rustket")
        .set("LegalCopyright", "Copyright (c) 2026 Sergey Sokolov")
        .set("InternalName", "rustket")
        .set("OriginalFilename", "rustket.exe");
    if let Err(e) = res.compile_for("rustket") {
        panic!("embedding the Windows resources failed: {e}");
    }
}
