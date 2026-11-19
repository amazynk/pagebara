fn main() {
    // Debug info keeps element ids, which `--script` can target (`#Component::id`).
    let mut config = slint_build::CompilerConfiguration::new()
        .with_style("fluent".into())
        .with_debug_info(true)
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    // The translations go into the binary (see crates/i18n); no files to find at run time.
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    if let Some(dir) = pagebara_i18n::slint_bundle(&out) {
        config = config.with_bundled_translations(dir);
    }
    slint_build::compile_with_config("ui/app.slint", config).expect("compiling the Slint UI");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        windows_resources();
    }
}

/// The exe's icon and version information (Explorer's Details tab, installers).
fn windows_resources() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let icon = std::path::Path::new(&dir).join("../../packaging/icons/app.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&icon.to_string_lossy())
        .set("ProductName", pagebara_identity::NAME)
        .set("FileDescription", pagebara_identity::NAME)
        .set("CompanyName", pagebara_identity::PUBLISHER)
        .set("InternalName", pagebara_identity::NAME)
        .set("LegalCopyright", "MIT OR Apache-2.0");
    if let Err(e) = res.compile() {
        panic!("Windows resources: {e}");
    }
}
