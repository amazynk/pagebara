//! What a screen reader gets from the app, checked with a real AT-SPI client
//! (`tests/a11y/run.sh`). Skipped when Xvfb, D-Bus, the AT-SPI bus launcher or the Python
//! AT-SPI bindings are missing.

#[cfg(target_os = "linux")]
#[test]
fn screen_reader_view() {
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/a11y/run.sh");
    let out = std::env::temp_dir().join(format!("pagebara-a11y-{}", std::process::id()));
    let status = std::process::Command::new("sh")
        .arg(script)
        .arg(&out)
        .env("PAGEBARA", env!("CARGO_BIN_EXE_pagebara"))
        .status()
        .expect("running tests/a11y/run.sh");
    match status.code() {
        Some(0) => {
            let _ = std::fs::remove_dir_all(&out);
        }
        Some(77) => eprintln!("skipped: the AT-SPI tools are not installed"),
        _ => panic!("screen reader checks failed, see {}", out.display()),
    }
}
