fn main() {
    // `rust_i18n::i18n!` embeds the locale files at compile time.
    println!("cargo:rerun-if-changed=locales");
}
