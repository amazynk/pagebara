//! resave IN PASSWORD OUT_PREFIX: writes OUT_PREFIX-inc.pdf, -full.pdf and -plain.pdf.
use pagebara_syntax::{Document, Error, Object};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let bytes = std::fs::read(&a[1]).unwrap();
    let mut doc = match Document::open(bytes) {
        Ok(d) => d,
        Err(Error::PasswordRequired(l)) => l.unlock(&a[2]).expect("password"),
        Err(e) => panic!("{e}"),
    };
    doc.add(Object::string(b"added"));
    if let Ok(b) = doc.save_incremental() {
        std::fs::write(format!("{}-inc.pdf", a[3]), b).unwrap();
    }
    std::fs::write(format!("{}-full.pdf", a[3]), doc.save_full().unwrap()).unwrap();
    if doc.remove_security().is_err() {
        return;
    }
    std::fs::write(format!("{}-plain.pdf", a[3]), doc.save_full().unwrap()).unwrap();
}
