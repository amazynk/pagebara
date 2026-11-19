//! `cargo run -p pagebara-conform --example conform -- validate FILE PROFILE [--json]`,
//! `… convert IN OUT PROFILE` or `… ua-fix IN OUT [LANG]`.

use pagebara_conform::{Profile, pdfa, pdfua, validate};
use pagebara_syntax::Document;

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let open = |p: &str| Document::open(std::fs::read(p).expect("read")).expect("open");
    match a.first().map(String::as_str) {
        Some("validate") => {
            let r = validate(&open(&a[1]), Profile::parse(&a[2]).expect("profile"));
            if a.iter().any(|x| x == "--json") {
                println!("{}", r.to_json());
            } else {
                print!("{}", r.to_text());
            }
        }
        Some("convert") => {
            match pdfa::convert(&open(&a[1]), Profile::parse(&a[3]).expect("profile")) {
                Ok(c) => {
                    std::fs::write(&a[2], &c.bytes).expect("write");
                    for ch in &c.changes {
                        println!("- {ch}");
                    }
                    print!("{}", c.report.to_text());
                }
                Err(e) => println!("refused: {e}"),
            }
        }
        Some("ua-fix") => {
            let mut d = open(&a[1]);
            let opts = pdfua::FixOptions {
                lang: Some(a.get(3).cloned().unwrap_or_else(|| "en-US".into())),
                title: Some("Untitled".into()),
            };
            for ch in pdfua::fix_safe(&mut d, &opts) {
                println!("- {ch}");
            }
            let bytes = d.save_full().expect("save");
            std::fs::write(&a[2], &bytes).expect("write");
            print!(
                "{}",
                validate(&Document::open(bytes).unwrap(), Profile::PdfUa1).to_text()
            );
        }
        _ => eprintln!("usage: conform validate|convert|ua-fix …"),
    }
}
