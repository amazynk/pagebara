fn main() {
    // Debug info keeps element ids for `--script` on desktop hosts.
    let mut config = slint_build::CompilerConfiguration::new()
        .with_debug_info(true)
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    // The translations go into the binary (see crates/i18n); no files to find at run time.
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    if let Some(dir) = pagebara_i18n::slint_bundle(&out) {
        config = config.with_bundled_translations(dir);
    }
    slint_build::compile_with_config("ui/main.slint", config).expect("compiling the Slint UI");
}
