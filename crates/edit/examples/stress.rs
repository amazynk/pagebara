//! Rewrites every object of sampled pages without visible change (an identity
//! transform) and deletes one object, checking that pages render the same where they
//! should.
//!
//! `cargo run -p pagebara-edit --example stress -- FILE [EVERY_NTH_PAGE]`

use pagebara_edit::{Matrix, Target};
use pagebara_render::{Annotations, PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;

fn render(doc: &Document, page: usize) -> Vec<u8> {
    let r = Renderer::new(doc.clone());
    let (w, h) = r.page_size(page).unwrap();
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: w.round() as u32,
        h: h.round() as u32,
    };
    let mut req = RenderRequest::new(1.0, rect);
    req.annotations = Annotations::None;
    r.render(page, &req).unwrap().data
}

fn differing(a: &[u8], b: &[u8]) -> usize {
    a.as_chunks::<4>()
        .0
        .iter()
        .zip(b.as_chunks::<4>().0)
        .filter(|(p, q)| p.iter().zip(q.iter()).any(|(x, y)| x.abs_diff(*y) > 48))
        .count()
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let doc = Document::open(std::fs::read(&args[0]).unwrap()).unwrap();
    let every: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(50);
    let n = pagebara_pages::PageTree::load(&doc).len();
    let (mut pages, mut bad) = (0, 0);
    for page in (0..n).step_by(every) {
        let objs = pagebara_edit::parse(&doc, page).unwrap();
        let all: Vec<Target> = objs.objects.iter().map(|o| Target::Object(o.id)).collect();
        let mut d = doc.clone();
        if let Err(e) = pagebara_edit::transform(&mut d, page, &all, Matrix::IDENTITY) {
            println!("page {page}: {e}");
            bad += 1;
            continue;
        }
        let d = Document::open(d.save_full().unwrap()).unwrap();
        let before = render(&doc, page);
        let px = differing(&before, &render(&d, page));
        if px > 20 {
            println!("page {page}: {px} pixels differ after identity edits");
            bad += 1;
        }
        // Deleting the largest top-level object leaves the others parsed alike.
        if let Some(big) = objs.top_level().max_by(|a, b| {
            (a.bbox.width() * a.bbox.height()).total_cmp(&(b.bbox.width() * b.bbox.height()))
        }) {
            let mut d = doc.clone();
            match pagebara_edit::delete(&mut d, page, &[Target::Object(big.id)]) {
                Ok(()) => {
                    let after = pagebara_edit::parse(&d, page).unwrap();
                    // Text left may regroup into other blocks.
                    let count = |o: &pagebara_edit::PageObjects| {
                        o.top_level()
                            .filter(|o| o.kind != pagebara_edit::Kind::Text)
                            .count()
                    };
                    let others = count(&objs) - usize::from(big.kind != pagebara_edit::Kind::Text);
                    let runs = objs.runs.len() - big.text().map_or(0, |t| t.runs().count());
                    // A form takes its text along.
                    let form = big.kind == pagebara_edit::Kind::Form;
                    if count(&after) != others || (!form && after.runs.len() != runs) {
                        println!(
                            "page {page}: {} objects and {} runs left of {others} and {runs} ({:?})",
                            count(&after),
                            after.runs.len(),
                            big.kind
                        );
                        bad += 1;
                    }
                }
                Err(e) => {
                    println!("page {page}: delete: {e}");
                    bad += 1;
                }
            }
        }
        pages += 1;
    }
    println!("{pages} pages checked, {bad} problems");
}
