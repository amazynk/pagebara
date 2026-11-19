//! Synthetic test files and checks shared by the redaction tests.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::process::Command;

use pagebara_redact::{Rect, Region};
use pagebara_render::{Annotations, PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;

/// A minimal PDF writer: objects 1 and 2 are the catalog and page tree.
pub struct Pdf {
    objects: Vec<Vec<u8>>,
    pages: Vec<usize>,
    pub catalog: String,
    pub trailer: String,
}

impl Pdf {
    pub fn new() -> Self {
        Pdf {
            objects: vec![Vec::new(), Vec::new()],
            pages: Vec::new(),
            catalog: String::new(),
            trailer: String::new(),
        }
    }

    pub fn add(&mut self, body: impl AsRef<[u8]>) -> usize {
        self.objects.push(body.as_ref().to_vec());
        self.objects.len()
    }

    /// Reserves an object number to fill with [`Pdf::set`].
    pub fn reserve(&mut self) -> usize {
        self.add(b"null")
    }

    pub fn set(&mut self, n: usize, body: impl AsRef<[u8]>) {
        self.objects[n - 1] = body.as_ref().to_vec();
    }

    pub fn stream(&mut self, dict: &str, data: &[u8]) -> usize {
        let b = stream_body(dict, data);
        self.add(b)
    }

    pub fn page(&mut self, resources: &str, content: &str, extra: &str) -> usize {
        let c = self.stream("", content.as_bytes());
        let p = self.add(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << {resources} >> /Contents {c} 0 R {extra} >>"
        ));
        self.pages.push(p);
        p
    }

    pub fn pages_push(&mut self, p: usize) {
        self.pages.push(p);
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.objects[0] =
            format!("<< /Type /Catalog /Pages 2 0 R {} >>", self.catalog).into_bytes();
        let kids: String = self.pages.iter().map(|p| format!("{p} 0 R ")).collect();
        self.objects[1] = format!(
            "<< /Type /Pages /Kids [{kids}] /Count {} >>",
            self.pages.len()
        )
        .into_bytes();
        let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(o);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        let mut x = format!("xref\n0 {}\n0000000000 65535 f \n", self.objects.len() + 1);
        for o in offsets {
            let _ = writeln!(x, "{o:010} 00000 n ");
        }
        let _ = write!(
            x,
            "trailer\n<< /Size {} /Root 1 0 R {} >>\nstartxref\n{xref}\n%%EOF\n",
            self.objects.len() + 1,
            self.trailer
        );
        out.extend_from_slice(x.as_bytes());
        out
    }
}

pub fn stream_body(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut b = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    b.extend_from_slice(data);
    b.extend_from_slice(b"\nendstream");
    b
}

/// Appends an incremental update replacing or adding objects `(num, body)`.
pub fn append_update(mut file: Vec<u8>, objects: &[(usize, Vec<u8>)], size: usize) -> Vec<u8> {
    let prev = {
        let s = String::from_utf8_lossy(&file);
        let i = s.rfind("startxref").unwrap();
        s[i + 9..]
            .trim()
            .lines()
            .next()
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap()
    };
    let mut rows = Vec::new();
    for (n, body) in objects {
        rows.push((*n, file.len()));
        file.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        file.extend_from_slice(body);
        file.extend_from_slice(b"\nendobj\n");
    }
    let xref = file.len();
    let mut x = String::from("xref\n");
    for (n, off) in rows {
        let _ = write!(x, "{n} 1\n{off:010} 00000 n \n");
    }
    let _ = write!(
        x,
        "trailer\n<< /Size {size} /Root 1 0 R /Prev {prev} >>\nstartxref\n{xref}\n%%EOF\n"
    );
    file.extend_from_slice(x.as_bytes());
    file
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02X}")).collect()
}

pub fn flate(data: &[u8]) -> Vec<u8> {
    pagebara_filters::encode_flate(data, 6)
}

pub const HELV: &str = "/Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >>";

/// Renders a page at 2 px/pt without annotations.
pub fn render(doc: &Document, page: usize) -> (u32, u32, Vec<u8>) {
    let r = Renderer::new(doc.clone());
    let (w, h) = r.page_size(page).unwrap();
    let scale = 2.0;
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

/// Device pixel box (at 2 px/pt) of a page-space rectangle on a 300 × 200 page shown
/// rotated by `rotate`, grown by `margin` pixels.
pub fn device_box(r: &Rect, rotate: u16, margin: i64) -> (i64, i64, i64, i64) {
    let m = pagebara_render::page_matrix(
        pagebara_content::Rect::new(0.0, 0.0, 300.0, 200.0),
        rotate,
        2.0,
        0,
        0,
    );
    let ps: Vec<pagebara_content::Point> = r
        .corners()
        .iter()
        .map(|p| m.apply(pagebara_content::Point::new(p.x as f32, p.y as f32)))
        .collect();
    let b = pagebara_content::Rect::from_points(&ps);
    (
        b.x0.floor() as i64 - margin,
        b.y0.floor() as i64 - margin,
        b.x1.ceil() as i64 + margin,
        b.y1.ceil() as i64 + margin,
    )
}

/// Pixels that differ by more than `tolerance` in a channel outside the given device boxes.
pub fn diff_outside(
    a: &(u32, u32, Vec<u8>),
    b: &(u32, u32, Vec<u8>),
    boxes: &[(i64, i64, i64, i64)],
    tolerance: u8,
) -> usize {
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
                if n < 12 {
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

/// Pixels inside a device box that are not white.
pub fn non_white_inside(img: &(u32, u32, Vec<u8>), b: (i64, i64, i64, i64)) -> usize {
    let mut n = 0;
    for y in b.1.max(0)..b.3.min(img.1 as i64) {
        for x in b.0.max(0)..b.2.min(img.0 as i64) {
            let i = ((y * img.0 as i64 + x) * 4) as usize;
            if img.2[i..i + 3].iter().any(|&v| v < 250) {
                n += 1;
            }
        }
    }
    n
}

pub fn page_text(doc: &Document, page: usize) -> pagebara_content::TextPage {
    let tree = pagebara_pages::PageTree::load(doc);
    let p = doc.get(tree.pages()[page].r).unwrap();
    let pd = p.as_dict().unwrap();
    let res = res_of(doc, pd);
    let list = pagebara_content::Interpreter::new(doc).page(pd, &res);
    pagebara_content::extract_text(doc, &list)
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

/// Glyph boxes with their text, for comparing positions.
pub fn glyphs(doc: &Document, page: usize) -> Vec<(String, [i64; 4])> {
    let t = page_text(doc, page);
    t.chars
        .iter()
        .filter(|c| c.glyph != pagebara_content::display::NONE)
        .map(|c| {
            let b = c.bbox();
            (
                t.char_text(c).to_string(),
                [b.x0, b.y0, b.x1, b.y1].map(|v| (v * 100.0).round() as i64),
            )
        })
        .collect()
}

/// Every byte of a file plus every stream inflated, for searching.
pub fn all_bytes(file: &[u8]) -> Vec<u8> {
    let mut out = file.to_vec();
    let mut at = 0;
    while let Some(p) = find(&file[at..], b"stream") {
        let s = at + p + 6;
        let body = if file[s..].starts_with(b"\r\n") {
            s + 2
        } else {
            s + 1
        };
        let Some(e) = file.get(body..).and_then(|f| find(f, b"endstream")) else {
            break;
        };
        let d = pagebara_flate::decode_lenient(&file[body..body + e], 1 << 28);
        out.extend_from_slice(&d.data);
        at = body + e;
    }
    out
}

pub fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

pub fn contains_ci(hay: &[u8], needle: &str) -> bool {
    let n = needle.to_ascii_lowercase();
    let h = hay.to_ascii_lowercase();
    find(&h, n.as_bytes()).is_some()
}

fn tool(name: &str) -> bool {
    Command::new(name).arg("-v").output().is_ok()
        || Command::new(name).arg("--version").output().is_ok()
}

/// External checks: qpdf finds no problems, and neither pdftotext nor mutool extract any
/// of the secrets. Skipped for tools that are not installed.
pub fn external_checks(file: &[u8], secrets: &[&str]) {
    let dir = std::env::temp_dir().join(format!("pagebara-redact-{}", std::process::id()));
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
    if tool("pdftotext") {
        let o = Command::new("pdftotext")
            .arg(&path)
            .arg("-")
            .output()
            .unwrap();
        assert!(o.status.success());
        for s in secrets {
            assert!(!contains_ci(&o.stdout, s), "pdftotext finds {s}");
        }
    }
    if tool("mutool") {
        let o = Command::new("mutool")
            .args(["draw", "-q", "-F", "txt", "-o", "-"])
            .arg(&path)
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "mutool failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        for s in secrets {
            assert!(!contains_ci(&o.stdout, s), "mutool finds {s}");
        }
    }
    let _ = std::fs::remove_file(&path);
}

fn rand_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

pub fn region(x0: f64, y0: f64, x1: f64, y1: f64) -> Region {
    Region::from_rect(Rect::new(x0, y0, x1, y1))
}
