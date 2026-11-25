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

/// The exe's icon, version information (Explorer's Details tab, installers) and
/// application manifest.
fn windows_resources() {
    let dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let icon = std::path::Path::new(&dir).join("../../packaging/icons/app.ico");
    println!("cargo:rerun-if-changed={}", icon.display());
    // The manifest goes in as a file, byte for byte: written into the resource script it
    // would start with a space, and an XML declaration has to be the first thing.
    let manifest_file =
        std::path::Path::new(&std::env::var("OUT_DIR").expect("OUT_DIR")).join("app.manifest");
    std::fs::write(&manifest_file, manifest()).expect("writing the manifest");
    let mut res = winresource::WindowsResource::new();
    res.set_icon(&icon.to_string_lossy())
        .set("ProductName", pagebara_identity::NAME)
        .set("FileDescription", pagebara_identity::NAME)
        .set("CompanyName", pagebara_identity::PUBLISHER)
        .set("InternalName", pagebara_identity::NAME)
        .set("LegalCopyright", "MIT OR Apache-2.0")
        .set_manifest_file(&manifest_file.to_string_lossy());
    if let Err(e) = res.compile() {
        panic!("Windows resources: {e}");
    }
}

/// The application manifest. The app runs as the user who started it (no elevation
/// prompt), declares Windows 10 and 11 (so version checks see the real system), may use
/// paths longer than 260 characters (where the system allows them: the LongPathsEnabled
/// policy), has UTF-8 as its code page, is sharp on every display (per-monitor DPI, also
/// before the window toolkit sets it) and gets the version 6 common controls (today's look
/// for message boxes and file dialogs).
fn manifest() -> String {
    // The assembly version is four numbers: major, minor, patch and 0.
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let mut n: Vec<u16> = version
        .split(['.', '-', '+'])
        .take(3)
        .map(|p| p.parse().unwrap_or(0))
        .collect();
    n.resize(4, 0);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <assemblyIdentity type="win32" name="{id}" version="{}.{}.{}.{}"/>
  <trustInfo xmlns="urn:schemas-microsoft-com:asm.v3">
    <security>
      <requestedPrivileges>
        <requestedExecutionLevel level="asInvoker" uiAccess="false"/>
      </requestedPrivileges>
    </security>
  </trustInfo>
  <compatibility xmlns="urn:schemas-microsoft-com:compatibility.v1">
    <application>
      <!-- Windows 10 and Windows 11 -->
      <supportedOS Id="{{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}}"/>
    </application>
  </compatibility>
  <application xmlns="urn:schemas-microsoft-com:asm.v3">
    <windowsSettings>
      <longPathAware xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">true</longPathAware>
      <activeCodePage xmlns="http://schemas.microsoft.com/SMI/2019/WindowsSettings">UTF-8</activeCodePage>
      <dpiAware xmlns="http://schemas.microsoft.com/SMI/2005/WindowsSettings">true/pm</dpiAware>
      <dpiAwareness xmlns="http://schemas.microsoft.com/SMI/2016/WindowsSettings">PerMonitorV2, PerMonitor</dpiAwareness>
    </windowsSettings>
  </application>
  <dependency>
    <dependentAssembly>
      <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
    </dependentAssembly>
  </dependency>
</assembly>
"#,
        n[0],
        n[1],
        n[2],
        n[3],
        id = pagebara_identity::APP_ID,
    )
}
