//! Lists the objects of a page and times parsing it.
//!
//! `cargo run --release -p pagebara-edit --example objects -- FILE [PAGE] [--quiet]`
//! (without PAGE: every page, reporting the slowest).

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(path) = args.first() else {
        eprintln!("usage: objects FILE [PAGE] [--quiet]");
        std::process::exit(2);
    };
    let quiet = args.iter().any(|a| a == "--quiet");
    let page: Option<usize> = args.get(1).and_then(|p| p.parse().ok());
    let doc = pagebara_syntax::Document::open(std::fs::read(path).unwrap()).unwrap();
    let n = pagebara_pages::PageTree::load(&doc).len();
    let pages: Vec<usize> = match page {
        Some(p) => vec![p],
        None => (0..n).collect(),
    };
    let mut worst = (0.0f64, 0usize, 0usize, 0usize);
    let mut total = 0.0;
    for &p in &pages {
        // The first parse loads fonts; time the best of three.
        let mut best = f64::MAX;
        let mut objs = None;
        for _ in 0..3 {
            let t = Instant::now();
            let o = pagebara_edit::parse(&doc, p).unwrap();
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
            objs = Some(o);
        }
        let objs = objs.unwrap();
        total += best;
        let glyphs: usize = objs.runs.iter().map(|r| r.glyphs.len()).sum();
        if best > worst.0 {
            worst = (best, p, objs.objects.len(), glyphs);
        }
        if !quiet {
            println!(
                "page {p}: {} objects, {} runs, {glyphs} glyphs in {best:.2} ms",
                objs.objects.len(),
                objs.runs.len()
            );
            if page.is_some() {
                for o in &objs.objects {
                    let what = match &o.data {
                        pagebara_edit::ObjectData::Text(t) => {
                            format!("{:?}", t.text.chars().take(60).collect::<String>())
                        }
                        _ => String::new(),
                    };
                    println!(
                        "  {:>4} {:?} parent {:?} [{:.1} {:.1} {:.1} {:.1}] {what}",
                        o.id.0,
                        o.kind,
                        o.parent.map(|p| p.0),
                        o.bbox.x0,
                        o.bbox.y0,
                        o.bbox.x1,
                        o.bbox.y1
                    );
                }
            }
        }
    }
    println!(
        "{} pages, mean {:.2} ms; slowest page {} ({} objects, {} glyphs): {:.2} ms",
        pages.len(),
        total / pages.len().max(1) as f64,
        worst.1,
        worst.2,
        worst.3,
        worst.0
    );
}
