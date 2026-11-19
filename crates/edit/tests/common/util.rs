//! Rendering, comparing and checking.

use std::process::Command;

use pagebara_content::Rect;
use pagebara_render::{Annotations, PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;

pub fn flate(data: &[u8]) -> Vec<u8> {
    pagebara_filters::encode_flate(data, 6)
}

pub const HELV: &str = "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >> >>";

pub type Image = (u32, u32, Vec<u8>);

/// Renders a page at 2 px/pt without annotations.
pub fn render(doc: &Document, page: usize) -> Image {
    render_at(doc, page, 2.0)
}

pub fn render_at(doc: &Document, page: usize, scale: f32) -> Image {
    let r = Renderer::new(doc.clone());
    let (w, h) = r.page_size(page).unwrap();
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: (w * scale).round() as u32,
        h: (h * scale).round() as u32,
    };
    let mut req = RenderRequest::new(scale, rect);
    req.annotations = Annotations::None;
    let img = r.render(page, &req).unwrap();
    (img.width, img.height, img.data)
}

/// Device pixel box (at 2 px/pt) of a page-space rectangle of an unrotated page with the
/// given height, grown by `margin` pixels.
pub fn device_box(r: &Rect, page_h: f32, margin: i64) -> (i64, i64, i64, i64) {
    (
        (r.x0 * 2.0).floor() as i64 - margin,
        ((page_h - r.y1) * 2.0).floor() as i64 - margin,
        (r.x1 * 2.0).ceil() as i64 + margin,
        ((page_h - r.y0) * 2.0).ceil() as i64 + margin,
    )
}

/// Pixels differing by more than `tolerance` in a channel outside the given boxes.
pub fn diff_outside(a: &Image, b: &Image, boxes: &[(i64, i64, i64, i64)], tolerance: u8) -> usize {
    assert_eq!((a.0, a.1), (b.0, b.1));
    let mut n = 0;
    for y in 0..a.1 as i64 {
        for x in 0..a.0 as i64 {
            if boxes
                .iter()
                .any(|&(x0, y0, x1, y1)| x >= x0 && x < x1 && y >= y0 && y < y1)
            {
                continue;
            }
            let i = ((y * a.0 as i64 + x) * 4) as usize;
            if a.2[i..i + 4]
                .iter()
                .zip(&b.2[i..i + 4])
                .any(|(p, q)| p.abs_diff(*q) > tolerance)
            {
                if n < 8 {
                    eprintln!(
                        "differs at {x},{y}: {:?} vs {:?}",
                        &a.2[i..i + 4],
                        &b.2[i..i + 4]
                    );
                }
                n += 1;
            }
        }
    }
    n
}

pub fn diff(a: &Image, b: &Image, tolerance: u8) -> usize {
    diff_outside(a, b, &[], tolerance)
}

/// Pixels inside a box that are not white.
pub fn ink_inside(img: &Image, b: (i64, i64, i64, i64)) -> usize {
    let mut n = 0;
    for y in b.1.max(0)..b.3.min(img.1 as i64) {
        for x in b.0.max(0)..b.2.min(img.0 as i64) {
            let i = ((y * img.0 as i64 + x) * 4) as usize;
            if img.2[i..i + 3].iter().any(|&v| v < 200) {
                n += 1;
            }
        }
    }
    n
}

/// The pixel at page-space point (x, y) of an unrotated page of height `page_h`.
pub fn pixel(img: &Image, x: f32, y: f32, page_h: f32) -> [u8; 4] {
    let (px, py) = ((x * 2.0) as usize, ((page_h - y) * 2.0) as usize);
    let i = (py * img.0 as usize + px) * 4;
    [img.2[i], img.2[i + 1], img.2[i + 2], img.2[i + 3]]
}

pub fn page_text(doc: &Document, page: usize) -> String {
    let tree = pagebara_pages::PageTree::load(doc);
    let p = doc.get(tree.pages()[page].r).unwrap();
    let pd = p.as_dict().unwrap();
    let res = res_of(doc, pd);
    let list = pagebara_content::Interpreter::new(doc).page(pd, &res);
    pagebara_content::extract_text(doc, &list).text
}

fn res_of(doc: &Document, pd: &pagebara_syntax::Dict) -> pagebara_syntax::Dict {
    let mut d = pd.clone();
    for _ in 0..16 {
        if let Some(r) = d.get(b"Resources") {
            return doc.resolve(r).as_dict().cloned().unwrap_or_default();
        }
        let Some(p) = d.get(b"Parent").and_then(|p| p.as_objref()) else {
            break;
        };
        d = doc.get(p).unwrap().as_dict().unwrap().clone();
    }
    Default::default()
}

/// The decoded content of the streams a page draws: its contents, then forms by name.
pub fn page_streams(doc: &Document, page: usize) -> Vec<Vec<u8>> {
    let tree = pagebara_pages::PageTree::load(doc);
    let pd = doc
        .get(tree.pages()[page].r)
        .unwrap()
        .as_dict()
        .unwrap()
        .clone();
    let mut out = vec![pagebara_content::page_contents(doc, &pd)];
    let mut seen = std::collections::HashSet::new();
    forms(doc, &res_of(doc, &pd), &mut out, &mut seen);
    out
}

fn forms(
    doc: &Document,
    res: &pagebara_syntax::Dict,
    out: &mut Vec<Vec<u8>>,
    seen: &mut std::collections::HashSet<pagebara_syntax::ObjRef>,
) {
    let xo = doc.get_in(res, b"XObject");
    let Some(xo) = xo.as_dict() else { return };
    let mut names: Vec<_> = xo.iter().collect();
    names.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    for (_, v) in names {
        let Some(r) = v.as_objref() else { continue };
        if !seen.insert(r) {
            continue;
        }
        let o = doc.get(r).unwrap();
        let Some(s) = o.as_stream() else { continue };
        if s.dict.name(b"Subtype") != Some(b"Form") {
            continue;
        }
        out.push(doc.decode(s).unwrap().data);
        if let Some(fr) = doc.get_in(&s.dict, b"Resources").as_dict() {
            forms(doc, fr, out, seen);
        }
    }
}

fn tool(name: &str) -> bool {
    Command::new(name).arg("--version").output().is_ok()
}

/// qpdf finds no problems and mutool draws the file and finds `texts` in it. Skipped for
/// tools that are not installed.
pub fn external_checks(file: &[u8], texts: &[&str]) {
    let dir = std::env::temp_dir().join(format!("pagebara-edit-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("t{}.pdf", rand_id()));
    std::fs::write(&path, file).unwrap();
    if tool("qpdf") {
        let o = Command::new("qpdf")
            .arg("--check")
            .arg(&path)
            .output()
            .unwrap();
        let out = String::from_utf8_lossy(&o.stdout);
        assert!(
            o.status.success(),
            "qpdf --check failed: {out} {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(!out.contains("WARNING"), "qpdf warnings: {out}");
    }
    if tool("mutool") {
        let o = Command::new("mutool")
            .args(["draw", "-q", "-F", "txt", "-o", "-"])
            .arg(&path)
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(o.status.success(), "mutool failed: {err}");
        assert!(!err.contains("error"), "mutool errors: {err}");
        let text = String::from_utf8_lossy(&o.stdout);
        for t in texts {
            assert!(text.contains(t), "mutool does not find {t:?} in {text:?}");
        }
    }
    let _ = std::fs::remove_file(&path);
}

fn rand_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

/// Saves in full and reads back.
pub fn reopen(doc: &Document) -> (Document, Vec<u8>) {
    let bytes = doc.save_full().unwrap();
    (Document::open(bytes.clone()).unwrap(), bytes)
}

pub fn open(bytes: Vec<u8>) -> Document {
    Document::open(bytes).unwrap()
}
