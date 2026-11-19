//! Operations on files written by qpdf: object streams and encryption.

mod common;

use std::process::Command;

use common::*;
use pagebara_pages::PageTree;

/// `(file, user password)`: `rich()` rewritten by qpdf.
const FILES: [(&str, &str); 4] = [
    ("rich-objstm.pdf", ""),
    ("rich-aes256-objstm.pdf", "user"),
    ("rich-aes128.pdf", "user"),
    ("rich-rc4.pdf", "user"),
];

/// Writes the fixtures; run with `--ignored` after changing `rich()`.
#[test]
#[ignore]
fn regenerate() {
    let dir = format!("{}/tests/fixtures", env!("CARGO_MANIFEST_DIR"));
    let base = format!("{dir}/rich.pdf");
    std::fs::write(&base, rich()).unwrap();
    let enc = |bits: &str| {
        vec![
            "--allow-weak-crypto".to_string(),
            "--encrypt".into(),
            "--user-password=user".into(),
            "--owner-password=owner".into(),
            format!("--bits={bits}"),
            "--".into(),
        ]
    };
    let runs: [(&str, Vec<String>); 4] = [
        ("rich-objstm.pdf", vec!["--object-streams=generate".into()]),
        ("rich-aes256-objstm.pdf", {
            let mut v = vec!["--object-streams=generate".to_string()];
            v.extend(enc("256"));
            v
        }),
        ("rich-aes128.pdf", {
            let mut v = enc("128");
            v.insert(v.len() - 1, "--use-aes=y".into());
            v
        }),
        ("rich-rc4.pdf", enc("128")),
    ];
    for (name, args) in runs {
        let st = Command::new("qpdf")
            .args(&args)
            .arg(&base)
            .arg(format!("{dir}/{name}"))
            .status()
            .unwrap();
        assert!(st.success(), "{name}");
    }
}

#[test]
fn operations_on_compressed_and_encrypted_files() {
    for (name, pw) in FILES {
        let src = open_pw(fixture(name), pw);
        assert_eq!(texts(&src).len(), 6, "{name}");

        let mut doc = src.clone();
        pagebara_pages::delete(&mut doc, &[1, 3]).unwrap();
        pagebara_pages::rotate(&mut doc, &[0], 1).unwrap();
        pagebara_pages::move_pages(&mut doc, &[3], 0).unwrap();
        for d in saved(&doc, pw) {
            assert_eq!(check_tree(&d), 4, "{name}");
            assert_eq!(
                texts(&d),
                ["Page 6", "Page 1", "Page 3", "Page 5"],
                "{name}"
            );
            assert_eq!(PageTree::load(&d).pages()[1].rotate, 180, "{name}");
            assert_eq!(
                pagebara_pages::page_labels(&d, 4).unwrap(),
                ["4", "i", "1", "3"],
                "{name}"
            );
        }

        // Extracted pages are decrypted copies in a new, unprotected file.
        let ex = pagebara_pages::extract(&src, &[2, 3]).unwrap();
        for d in saved(&ex, "") {
            assert!(!d.is_encrypted());
            assert_eq!(texts(&d), ["Page 3", "Page 4"], "{name}");
        }

        let mut ins = src.clone();
        pagebara_pages::insert(&mut ins, 6, &open(rich()), &[0]).unwrap();
        for d in saved(&ins, pw) {
            assert_eq!(texts(&d).len(), 7, "{name}");
            assert_eq!(texts(&d)[6], "Page 1");
        }
    }
}
