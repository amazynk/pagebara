//! A sample for live text: a page of real text with a screenshot of a dialog and a title
//! drawn as glyph outlines (text the page shows but does not have), then a scanned page.

use std::fmt::Write as _;

use pagebara_content::{Cmd, Interpreter, Matrix};
use pagebara_render::{DefaultGlyphs, GlyphSource, Renderer};
use pagebara_syntax::Document;

use super::{Page, Pdf, build, lit};

/// The dialog shown in the screenshot, drawn on a page of `W` × `H` points.
const W: f64 = 300.0;
const H: f64 = 180.0;

fn dialog() -> Vec<u8> {
    let mut p = Page::new(W, H);
    let mut ops = String::new();
    let rect = |ops: &mut String, rgb: &str, x: f64, y: f64, w: f64, h: f64| {
        let _ = writeln!(ops, "{rgb} rg {x} {} {w} {h} re f", H - y - h);
    };
    let text = |ops: &mut String, font: &str, size: f64, rgb: &str, x: f64, y: f64, s: &str| {
        let _ = writeln!(
            ops,
            "BT /{font} {size} Tf {rgb} rg {x} {} Td {} Tj ET",
            H - y,
            lit(s)
        );
    };
    rect(&mut ops, "0.97 0.97 0.98", 0.0, 0.0, W, H);
    rect(&mut ops, "0.2 0.22 0.26", 0.0, 0.0, W, 24.0);
    text(&mut ops, "F2", 10.0, "1 1 1", 12.0, 16.0, "Export settings");
    let rows = [("Paper size", "A4 portrait"), ("Margins", "Normal 25 mm")];
    for (i, (k, v)) in rows.iter().enumerate() {
        let y = 50.0 + i as f64 * 26.0;
        text(&mut ops, "F1", 9.0, "0.3 0.3 0.33", 14.0, y, k);
        rect(&mut ops, "1 1 1", 110.0, y - 13.0, 170.0, 18.0);
        text(&mut ops, "F1", 9.0, "0 0 0", 116.0, y, v);
    }
    text(
        &mut ops,
        "F1",
        9.0,
        "0 0 0",
        14.0,
        112.0,
        "Include comments and highlights",
    );
    rect(&mut ops, "0.15 0.39 0.92", 150.0, 142.0, 74.0, 24.0);
    text(&mut ops, "F2", 9.0, "1 1 1", 162.0, 158.0, "Export PDF");
    rect(&mut ops, "0.88 0.88 0.9", 232.0, 142.0, 56.0, 24.0);
    text(&mut ops, "F1", 9.0, "0 0 0", 244.0, 158.0, "Cancel");
    p.ops = ops;
    build(vec![p], &[], |_, _, _| String::new())
}

/// Pixels (RGB) of page 0 of `pdf` at `scale` pixels per point.
fn pixels(pdf: Vec<u8>, scale: f32) -> (Vec<u8>, usize, usize) {
    let r = Renderer::new(Document::open(pdf).expect("pdf"));
    let (w, h) = r.page_size(0).expect("page");
    let rect = pagebara_render::PixelRect {
        x: 0,
        y: 0,
        w: (w * scale).round() as u32,
        h: (h * scale).round() as u32,
    };
    let img = r
        .render(0, &pagebara_render::RenderRequest::new(scale, rect))
        .expect("render");
    let rgb = img
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    (rgb, img.width as usize, img.height as usize)
}

/// Content drawing the text of `pdf`'s first page as filled outlines, moved by `(dx, dy)`.
fn outlines(pdf: Vec<u8>, dx: f32, dy: f32) -> String {
    let doc = Document::open(pdf).expect("pdf");
    let tree = pagebara_pages::PageTree::open(&doc);
    let page = tree.page(0).expect("page");
    let d = doc
        .get(page.r)
        .and_then(|o| o.as_dict().cloned())
        .expect("page");
    let res = doc
        .get_in(&d, b"Resources")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let list = Interpreter::new(&doc).page(&d, &res);
    let shift = Matrix::translate(dx, dy);
    let mut out = String::from("0.12 0.3 0.55 rg\n");
    for cmd in &list.cmds {
        let Cmd::Text { run } = *cmd else { continue };
        let run = &list.runs[run as usize];
        let fr = &list.fonts[run.font as usize];
        let Some(glyphs) = DefaultGlyphs.load(&doc, &fr.font) else {
            continue;
        };
        for g in list.run_glyphs(run) {
            let Some(o) = glyphs.outline(&doc, &fr.font, g.code) else {
                continue;
            };
            let m = run.glyph_matrix(g).then(&shift);
            let mut pts = o.points.iter().map(|p| m.apply(*p));
            for v in &o.verbs {
                use pagebara_content::display::Verb;
                let mut take = |n: usize| -> Vec<pagebara_content::Point> {
                    (0..n).filter_map(|_| pts.next()).collect()
                };
                match v {
                    Verb::Move => {
                        if let [p] = take(1)[..] {
                            let _ = writeln!(out, "{:.2} {:.2} m", p.x, p.y);
                        }
                    }
                    Verb::Line => {
                        if let [p] = take(1)[..] {
                            let _ = writeln!(out, "{:.2} {:.2} l", p.x, p.y);
                        }
                    }
                    Verb::Cubic => {
                        if let [a, b, c] = take(3)[..] {
                            let _ = writeln!(
                                out,
                                "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c",
                                a.x, a.y, b.x, b.y, c.x, c.y
                            );
                        }
                    }
                    Verb::Close => out.push_str("h\n"),
                }
            }
            out.push_str("f\n");
        }
    }
    out
}

/// Text in pictures: a page of real text with a screenshot and an outlined title, and a
/// page that is only a scan.
pub fn live_text() -> Vec<u8> {
    live_text_in(None)
}

/// [`live_text`] marked as written in `lang` (the catalog's `/Lang`).
pub fn live_text_in(lang: Option<&str>) -> Vec<u8> {
    let (rgb, iw, ih) = pixels(dialog(), 3.0);
    let mut title = Page::new(400.0, 60.0);
    title.line_at("F2", 26.0, 0.0, 20.0, "Northwind Office");
    let title_ops = outlines(
        build(vec![title], &[], |_, _, _| String::new()),
        72.0,
        700.0,
    );

    let mut p = Page::new(612.0, 792.0);
    p.y -= 60.0;
    p.para(
        "Exporting a document keeps its comments when the option below is ticked. The window \
         looks like this:",
        11.0,
    );
    // The screenshot: 300 × 180 points, under the paragraph.
    let top = p.y - 6.0;
    let _ = writeln!(p.ops, "q {W} 0 0 {H} 72 {:.2} cm /Shot Do Q", top - H);
    p.y = top - H - 18.0;
    p.para("Figure 1: the export window.", 9.0);
    p.para(
        "Choose Export PDF to save the copy, or Cancel to go back without saving.",
        11.0,
    );
    p.ops.push_str(&title_ops);

    let (gray, sw, sh) = super::more::scan_image();
    let mut scan = Page::new(612.0, 792.0);
    let _ = writeln!(scan.ops, "q 612 0 0 792 0 0 cm /Scan Do Q");

    let shot = image_object(&rgb, iw, ih, "DeviceRGB");
    let scanned = image_object(&gray, sw, sh, "DeviceGray");
    build(
        vec![p, scan],
        &[("Title", "Text in pictures")],
        move |pdf: &mut Pdf, pages, _| {
            let a = pdf.add(shot);
            let b = pdf.add(scanned);
            // Each page's resources get its picture.
            for (&page, (name, obj)) in pages.iter().zip([("Shot", a), ("Scan", b)]) {
                let o = &mut pdf.objs[page - 1];
                *o = o.replacen(
                    "/Resources<<",
                    &format!("/Resources<</XObject<</{name} {obj} 0 R>>"),
                    1,
                );
            }
            lang.map(|l| format!("/Lang({l})")).unwrap_or_default()
        },
    )
}

fn image_object(data: &[u8], w: usize, h: usize, cs: &str) -> String {
    let packed = pagebara_filters::encode_flate(data, 9);
    let mut hex = String::with_capacity(packed.len() * 2 + 1);
    for b in packed {
        let _ = write!(hex, "{b:02X}");
    }
    hex.push('>');
    format!(
        "<</Type/XObject/Subtype/Image/Width {w}/Height {h}/ColorSpace/{cs}/BitsPerComponent 8\
         /Filter[/ASCIIHexDecode/FlateDecode]/Length {}>>stream\n{hex}\nendstream",
        hex.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_text_sample_shows_text_it_does_not_have() {
        let d = Document::open(live_text()).unwrap();
        let s = pagebara_app_core::session::Session::new(None, d);
        assert_eq!(s.page_count(), 2);
        let t = s.text.page(0).text.clone();
        assert!(t.contains("Exporting a document"), "{t}");
        assert!(t.contains("Figure 1"));
        // The screenshot's and the title's words are not text.
        assert!(!t.contains("Export settings") && !t.contains("Northwind"));
        assert!(s.text.page(1).text.trim().is_empty());
    }
}
