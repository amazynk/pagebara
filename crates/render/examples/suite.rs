//! Writes the synthetic feature suite as PDF files into a directory, for `diff`.
//!
//! ```text
//! cargo run -p pagebara-render --example suite -- OUT_DIR
//! ```

#[path = "../tests/common/pdf.rs"]
mod pdf;
#[path = "../tests/common/suite.rs"]
mod suite;

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "suite".into());
    std::fs::create_dir_all(&dir).expect("output directory");
    for (name, data) in suite::suite() {
        let path = std::path::Path::new(&dir).join(format!("{name}.pdf"));
        std::fs::write(&path, data).expect("write");
        println!("{}", path.display());
    }
}
