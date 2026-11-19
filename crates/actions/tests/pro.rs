//! The output tools on the command line: options, outputs and JSON reports.

use std::path::PathBuf;

use pagebara_actions::run_pro;

fn samples() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pagebara-pro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    pagebara_samples::write_all(&dir).unwrap();
    dir
}

fn args(s: &[&str]) -> Vec<String> {
    s.iter().map(|a| a.to_string()).collect()
}

#[test]
fn tools_write_copies_and_reports() {
    let dir = samples();
    let r = run_pro(
        "watermark",
        &args(&["--text", "DRAFT", "--opacity", "0.2", "--json", "lease.pdf"]),
        &dir,
    )
    .unwrap();
    assert_eq!(r.outputs, vec![dir.join("lease-watermarked.pdf")]);
    let json: serde_json::Value = serde_json::from_str(r.json.as_deref().unwrap()).unwrap();
    assert_eq!(json["files"][0]["pages"].as_array().unwrap().len(), 4);

    let r = run_pro(
        "watermark",
        &args(&["--remove", "lease-watermarked.pdf"]),
        &dir,
    )
    .unwrap();
    assert_eq!(r.outputs.len(), 1);

    let r = run_pro(
        "bates",
        &args(&[
            "--prefix",
            "X-",
            "--digits",
            "3",
            "--log",
            "log.csv",
            "lease.pdf",
            "report.pdf",
        ]),
        &dir,
    )
    .unwrap();
    assert_eq!(r.outputs.len(), 2);
    let log = std::fs::read_to_string(dir.join("log.csv")).unwrap();
    assert!(log.contains("X-001") && log.contains("X-005"), "{log}");

    let r = run_pro("flatten", &args(&["--json", "form.pdf"]), &dir).unwrap();
    let json: serde_json::Value = serde_json::from_str(r.json.as_deref().unwrap()).unwrap();
    assert_eq!(json["files"][0]["fields"], 11);

    let r = run_pro(
        "compare",
        &args(&[
            "signed.pdf",
            "signed-tampered.pdf",
            "--report",
            "cmp.pdf",
            "--json",
        ]),
        &dir,
    )
    .unwrap();
    let json: serde_json::Value = serde_json::from_str(r.json.as_deref().unwrap()).unwrap();
    assert_eq!(json["changes"][0]["old"], "1,250");
    assert!(dir.join("cmp.pdf").exists());

    assert!(run_pro("watermark", &args(&["--bogus", "lease.pdf"]), &dir).is_err());
    assert!(run_pro("compare", &args(&["lease.pdf"]), &dir).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}
