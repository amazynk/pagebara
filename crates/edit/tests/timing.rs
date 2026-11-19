//! How long reading a dense page into objects takes, on a book from the local corpus
//! (skipped when it is not there; the corpus is never committed). `cargo run --release
//! --example objects` gives figures for every page.

use std::time::Instant;

const BOOK: &str = "corpus/325462-093-sdm-vol-1-2abcd-3abcd-4.pdf";

#[test]
fn dense_pages_parse_in_milliseconds() {
    // The corpus of the checkout this crate is in (or of one it is a worktree of).
    let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(bytes) = here
        .ancestors()
        .find_map(|d| std::fs::read(d.join(BOOK)).ok())
    else {
        eprintln!("skipped: {BOOK} is missing");
        return;
    };
    let doc = pagebara_syntax::Document::open(bytes).unwrap();
    let n = pagebara_pages::PageTree::load(&doc).len();
    // The densest of a spread of pages.
    let mut dense = (0, 0);
    for page in (0..n).step_by((n / 60).max(1)) {
        let objs = pagebara_edit::parse(&doc, page).unwrap();
        let glyphs: usize = objs.runs.iter().map(|r| r.glyphs.len()).sum();
        if glyphs > dense.1 {
            dense = (page, glyphs);
        }
    }
    let mut best = f64::MAX;
    let mut objects = 0;
    for _ in 0..5 {
        let t = Instant::now();
        let objs = pagebara_edit::parse(&doc, dense.0).unwrap();
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
        objects = objs.objects.len();
    }
    eprintln!(
        "page {}: {} glyphs, {objects} objects in {best:.2} ms",
        dense.0, dense.1
    );
    // Generous for unoptimised builds; release builds take a few milliseconds.
    assert!(best < 250.0, "{best} ms");
}
