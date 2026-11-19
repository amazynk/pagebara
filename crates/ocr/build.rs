//! Bundles the detection and Latin recognition models into the library when
//! `PAGEBARA_OCR_BUNDLE` names a directory holding them (release packaging sets it), so text
//! in pictures can be recognised without a download. Without it nothing is bundled and
//! debug builds stay quick to link. The files are verified like downloaded ones on first use.

use std::fmt::Write as _;
use std::path::PathBuf;

const FILES: [(&str, &str); 3] = [
    ("DET", "ppocrv5-mobile-det.onnx"),
    ("LATIN_REC", "ppocrv5-latin-rec.onnx"),
    ("LATIN_DICT", "ppocrv5-latin-rec.yml"),
];

fn main() {
    println!("cargo:rerun-if-env-changed=PAGEBARA_OCR_BUNDLE");
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR")).join("bundled.rs");
    let dir = std::env::var_os("PAGEBARA_OCR_BUNDLE").filter(|d| !d.is_empty());
    let mut code = String::new();
    let mut list = Vec::new();
    for (ident, name) in FILES {
        let value = match &dir {
            Some(d) => {
                let p = PathBuf::from(d).join(name);
                assert!(
                    p.is_file(),
                    "PAGEBARA_OCR_BUNDLE: {} is missing",
                    p.display()
                );
                println!("cargo:rerun-if-changed={}", p.display());
                let p = p.canonicalize().expect("model path");
                format!("Some(include_bytes!({p:?}))")
            }
            None => "None".into(),
        };
        let _ = writeln!(code, "pub static {ident}: Option<&[u8]> = {value};");
        list.push(format!("({name:?}, {ident})"));
    }
    let _ = writeln!(
        code,
        "pub static FILES: [(&str, Option<&[u8]>); {}] = [{}];",
        list.len(),
        list.join(", ")
    );
    std::fs::write(out, code).expect("writing bundled.rs");
}
