//! Manual testing of page operations. Pages are 1-based, as in `1,3-5`; set
//! `PDF_PASSWORD` for protected files.
//!
//!   pages info IN
//!   pages delete|extract|duplicate IN OUT PAGES
//!   pages move IN OUT PAGES BEFORE      (BEFORE = count + 1 for the end)
//!   pages rotate IN OUT PAGES QUARTER_TURNS
//!   pages crop IN OUT PAGES X0,Y0,X1,Y1
//!   pages insert IN OUT AT FROM [PAGES]
//!   pages merge OUT IN...
//!   pages split IN OUT_PREFIX RANGES     (e.g. 1-3,4-9 writes OUT_PREFIX-1.pdf ...)
//!   pages gc IN OUT

use std::process::exit;

use pagebara_pages::{PageTree, Rect};
use pagebara_syntax::{Document, Error};

fn open(path: &str) -> Document {
    let bytes = std::fs::read(path).unwrap_or_else(|e| die(&format!("{path}: {e}")));
    match Document::open(bytes) {
        Ok(d) => d,
        Err(Error::PasswordRequired(l)) => {
            let pw = std::env::var("PDF_PASSWORD").unwrap_or_default();
            l.unlock(&pw)
                .unwrap_or_else(|_| die("wrong password (set PDF_PASSWORD)"))
        }
        Err(e) => die(&format!("{path}: {e}")),
    }
}

fn save(doc: &Document, path: &str, incremental: bool) {
    let bytes = match incremental.then(|| doc.save_incremental()) {
        Some(Ok(b)) => b,
        _ => doc.save_full().unwrap_or_else(|e| die(&e.to_string())),
    };
    std::fs::write(path, bytes).unwrap_or_else(|e| die(&format!("{path}: {e}")));
}

fn die(msg: &str) -> ! {
    eprintln!("pages: {msg}");
    exit(1)
}

/// `1,3-5` into 0-based indices.
fn pages(spec: &str) -> Vec<usize> {
    ranges(spec).into_iter().flatten().collect()
}

fn ranges(spec: &str) -> Vec<std::ops::Range<usize>> {
    let num = |s: &str| match s.trim().parse::<usize>() {
        Ok(n) if n > 0 => n,
        _ => die(&format!("bad page number {s:?}")),
    };
    spec.split(',')
        .map(|part| match part.split_once('-') {
            Some((a, b)) => num(a) - 1..num(b),
            None => num(part) - 1..num(part),
        })
        .collect()
}

fn main() {
    let a: Vec<String> = std::env::args().skip(1).collect();
    let arg = |i: usize| {
        a.get(i)
            .map(String::as_str)
            .unwrap_or_else(|| die("missing argument"))
    };
    let result = match arg(0) {
        "info" => {
            let doc = open(arg(1));
            let t0 = std::time::Instant::now();
            let tree = PageTree::open(&doc);
            let first: Vec<_> = (0..tree.len().min(50))
                .map_while(|i| tree.page(i))
                .collect();
            println!(
                "{} pages, first {} in {:?}{}",
                tree.len(),
                first.len(),
                t0.elapsed(),
                if tree.repaired() {
                    " (tree repaired)"
                } else {
                    ""
                }
            );
            for (i, p) in first.iter().enumerate() {
                let (w, h) = p.size();
                println!(
                    "{:5} {:?} {w:.1} x {h:.1} pt, rotate {}",
                    i + 1,
                    p.r,
                    p.rotate
                );
            }
            Ok(())
        }
        cmd @ ("delete" | "extract" | "duplicate" | "move" | "rotate" | "crop") => {
            let mut doc = open(arg(1));
            let sel = pages(arg(3));
            let r = match cmd {
                "delete" => pagebara_pages::delete(&mut doc, &sel),
                "duplicate" => pagebara_pages::duplicate(&mut doc, &sel),
                "move" => pagebara_pages::move_pages(&mut doc, &sel, num_arg(arg(4)) - 1),
                "rotate" => {
                    let t = arg(4).parse().unwrap_or_else(|_| die("bad quarter turns"));
                    pagebara_pages::rotate(&mut doc, &sel, t)
                }
                "crop" => {
                    let v: Vec<f64> = arg(4)
                        .split(',')
                        .filter_map(|x| x.trim().parse().ok())
                        .collect();
                    if v.len() != 4 {
                        die("crop box needs X0,Y0,X1,Y1");
                    }
                    pagebara_pages::crop(&mut doc, &sel, Some(Rect::new(v[0], v[1], v[2], v[3])))
                }
                _ => pagebara_pages::extract(&doc, &sel).map(|d| doc = d),
            };
            r.map(|()| save(&doc, arg(2), cmd != "extract"))
        }
        "insert" => {
            let mut doc = open(arg(1));
            let from = open(arg(4));
            let sel = match a.get(5) {
                Some(s) => pages(s),
                None => (0..PageTree::load(&from).len()).collect(),
            };
            pagebara_pages::insert(&mut doc, num_arg(arg(3)) - 1, &from, &sel)
                .map(|()| save(&doc, arg(2), true))
        }
        "merge" => {
            let docs: Vec<Document> = a[2..].iter().map(|p| open(p)).collect();
            pagebara_pages::merge(&docs.iter().collect::<Vec<_>>()).map(|d| save(&d, arg(1), false))
        }
        "split" => {
            let doc = open(arg(1));
            pagebara_pages::split(&doc, &ranges(arg(3))).map(|parts| {
                for (i, d) in parts.iter().enumerate() {
                    save(d, &format!("{}-{}.pdf", arg(2), i + 1), false);
                }
            })
        }
        "gc" => {
            let mut doc = open(arg(1));
            println!("{} objects removed", pagebara_pages::compact(&mut doc));
            save(&doc, arg(2), false);
            Ok(())
        }
        _ => die("unknown command; see the source for usage"),
    };
    if let Err(e) = result {
        die(&e.to_string());
    }
}

fn num_arg(s: &str) -> usize {
    s.parse()
        .unwrap_or_else(|_| die(&format!("bad number {s:?}")))
}
