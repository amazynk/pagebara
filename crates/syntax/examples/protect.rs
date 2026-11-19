//! protect IN OUT USER OWNER: full save with AES-256 protection (owner gets all rights).
use pagebara_syntax::{Document, Permissions};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let mut doc = Document::open(std::fs::read(&a[1]).unwrap()).expect("unprotected input");
    let mut p = Permissions::ALL;
    p.print = false;
    doc.set_password(&a[3], &a[4], p).unwrap();
    std::fs::write(&a[2], doc.save_full().unwrap()).unwrap();
}
