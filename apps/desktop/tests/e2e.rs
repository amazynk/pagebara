//! End-to-end UI tests: the scripts in `tests/e2e` run against the real window on a virtual
//! display. Ignored by default (they need Xvfb and take a minute):
//! `cargo test -p pagebara-app --test e2e -- --ignored`.

use std::path::Path;
use std::process::Command;

#[test]
#[ignore = "needs Xvfb; run with --ignored"]
fn ui_scripts() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e");
    let out = std::env::temp_dir().join(format!("pagebara-e2e-{}", std::process::id()));
    let status = Command::new(dir.join("run.sh"))
        .arg(&out)
        .env("PAGEBARA", env!("CARGO_BIN_EXE_pagebara"))
        .status()
        .expect("running tests/e2e/run.sh");
    assert!(
        status.success(),
        "UI scripts failed; logs and screenshots in {}",
        out.display()
    );
}
