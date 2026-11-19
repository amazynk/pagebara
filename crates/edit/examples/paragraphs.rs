//! Edits the largest paragraph of pages of a PDF and reports what happened: whether the
//! edited text reads back (and as how many blocks), how long layout (a preview) and the
//! edit take.
//!
//! `cargo run --release -p pagebara-edit --example paragraphs -- FILE [PAGES]`

use std::time::Instant;

use pagebara_edit::{Editor, Fonts};

fn main() {
    let mut args = std::env::args().skip(1);
    let path = args.next().expect("usage: paragraphs FILE [PAGES]");
    let pages: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(40);
    let doc = pagebara_syntax::Document::open(std::fs::read(&path).expect("read")).expect("open");
    let n = pagebara_pages::PageTree::load(&doc).len();
    let mut fonts = Fonts::system();
    let (mut ok, mut failed, mut lost, mut split) = (0, 0, 0, 0);
    let (mut lay_ms, mut edit_ms) = (0.0f64, 0.0f64);
    for page in (0..n).step_by((n / pages).max(1)) {
        let objs = pagebara_edit::parse(&doc, page).unwrap();
        let Some(b) = objs
            .objects
            .iter()
            .filter(|o| o.parent.is_none())
            .filter_map(|o| o.text().map(|t| (o.id, t.text.len())))
            .max_by_key(|x| x.1)
            .map(|x| x.0)
        else {
            continue;
        };
        let bt = match pagebara_edit::block_text(&objs, b) {
            Ok(bt) => bt,
            Err(e) => {
                eprintln!("page {page}: {e}");
                failed += 1;
                continue;
            }
        };
        let text = bt.text();
        let new = format!("Edited: {text}");
        let spans = pagebara_edit::restyle_plain(&bt.spans, &new);
        let t = Instant::now();
        let lay = pagebara_edit::layout_block(&doc, &mut fonts, &bt.frame, &spans);
        let t1 = t.elapsed().as_secs_f64() * 1000.0;
        let t = Instant::now();
        let lay2 = pagebara_edit::layout_block(&doc, &mut fonts, &bt.frame, &spans);
        lay_ms = lay_ms.max(t.elapsed().as_secs_f64() * 1000.0);
        if lay.is_err() || lay2.is_err() {
            failed += 1;
            continue;
        }
        let mut d = doc.clone();
        let t = Instant::now();
        let r = Editor::open(&mut d, page).and_then(|mut e| {
            let rep = e.edit_block_text(b, &new, &mut fonts)?;
            e.commit()?;
            Ok(rep)
        });
        edit_ms = edit_ms.max(t.elapsed().as_secs_f64() * 1000.0);
        match r {
            Ok(rep) => {
                let objs = pagebara_edit::parse(&d, page).unwrap();
                // Hyphens come and go where lines break.
                let flat = |s: &str| {
                    s.chars()
                        .filter(|c| !c.is_whitespace() && !matches!(c, '-' | '\u{ad}'))
                        .collect::<String>()
                };
                // Blocks the new text was read into: those inside its bounds.
                let inside = |r: &pagebara_edit::Rect| {
                    let (cx, cy) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
                    let b = rep.bbox;
                    b.x0 <= cx && cx <= b.x1 && b.y0 <= cy && cy <= b.y1
                };
                let blocks = objs
                    .objects
                    .iter()
                    .filter(|o| o.parent.is_none() && o.text().is_some() && inside(&o.bbox))
                    .count();
                if blocks > 1 {
                    split += 1;
                    eprintln!("page {page}: the edited paragraph reads back as {blocks} blocks");
                }
                let all: String = objs
                    .objects
                    .iter()
                    .filter_map(|o| o.text().map(|t| t.text.clone()))
                    .collect::<Vec<_>>()
                    .join(" ");
                if flat(&all).contains(&flat(&new)) {
                    ok += 1;
                } else {
                    lost += 1;
                    let (a, b) = (flat(&new), flat(&all));
                    let k = (0..a.len())
                        .rev()
                        .find(|&k| a.is_char_boundary(k) && b.contains(&a[..k]))
                        .unwrap_or(0);
                    eprintln!(
                        "page {page}: edited text does not read back after {:?}",
                        &a[k.saturating_sub(30)..(k + 30).min(a.len())]
                    );
                }
                println!(
                    "page {page}: {} lines, overflow {:.1}, first layout {t1:.1} ms",
                    rep.lines, rep.overflow
                );
            }
            Err(e) => {
                eprintln!("page {page}: {e}");
                failed += 1;
            }
        }
    }
    println!(
        "{ok} edited, {lost} not read back, {split} read back as several blocks, {failed} failed; layout ≤ {lay_ms:.2} ms, edit ≤ {edit_ms:.1} ms"
    );
}
