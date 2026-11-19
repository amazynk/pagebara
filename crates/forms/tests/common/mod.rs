//! Test PDFs built in code, and checks with qpdf and mutool.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

/// A one-page file (`width` x `height` points, page object 3) with the given content.
pub fn one_page(width: u32, height: u32, content: &str) -> Vec<u8> {
    page_with(width, height, content, "")
}

/// [`one_page`] with extra page dictionary entries, e.g. `/Rotate 90`.
pub fn page_with(width: u32, height: u32, content: &str, extra: &str) -> Vec<u8> {
    let objs = [
        "<</Type/Catalog/Pages 2 0 R>>".to_string(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
        format!(
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 {width} {height}]/Contents 4 0 R\
             /Resources<</Font<</F1 5 0 R>>>>{extra}>>"
        ),
        format!(
            "<</Length {}>>stream\n{content}\nendstream",
            content.len() + 1
        ),
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".to_string(),
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let x = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objs.len() + 1).bytes());
    for o in offs {
        out.extend(format!("{o:010} 00000 n\r\n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<</Size {} /Root 1 0 R>>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    out
}

pub fn tool(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from("/usr/bin").join(name);
    p.exists().then_some(p)
}

pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("forms-tests");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// Runs `qpdf --check` (skipped without qpdf).
pub fn qpdf_check(pdf: &[u8], name: &str) {
    let Some(qpdf) = tool("qpdf") else {
        eprintln!("qpdf not found; skipping check");
        return;
    };
    // Tests run in parallel and may check the same name.
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = scratch(&format!("{name}-{}-{n}.pdf", std::process::id()));
    std::fs::write(&path, pdf).unwrap();
    let out = Command::new(qpdf)
        .arg("--check")
        .arg(&path)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !text.contains("WARNING"),
        "qpdf --check failed for {name}:\n{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// An RGB rendering at 72 dpi, so one pixel is one point.
pub struct Raster {
    pub width: usize,
    pub height: usize,
    pub rgb: Vec<u8>,
}

impl Raster {
    /// The pixel at PDF user-space coordinates (origin bottom left).
    pub fn at(&self, x: f64, y: f64) -> [u8; 3] {
        let px = (x.floor() as usize).min(self.width - 1);
        let py = ((self.height as f64 - y).floor() as usize).min(self.height - 1);
        let i = (py * self.width + px) * 3;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }

    /// The pixel at image coordinates (origin top left).
    pub fn px(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y.min(self.height - 1) * self.width + x.min(self.width - 1)) * 3;
        [self.rgb[i], self.rgb[i + 1], self.rgb[i + 2]]
    }

    /// Pixels of the image-space box `x0..x1, y0..y1` matching `f`, and their bounds.
    pub fn count_px(
        &self,
        (x0, y0, x1, y1): (usize, usize, usize, usize),
        f: impl Fn([u8; 3]) -> bool,
    ) -> (usize, Option<(usize, usize, usize, usize)>) {
        let mut n = 0;
        let mut b: Option<(usize, usize, usize, usize)> = None;
        for y in y0..y1.min(self.height) {
            for x in x0..x1.min(self.width) {
                if f(self.px(x, y)) {
                    n += 1;
                    b = Some(match b {
                        None => (x, y, x + 1, y + 1),
                        Some(b) => (b.0.min(x), b.1.min(y), b.2.max(x + 1), b.3.max(y + 1)),
                    });
                }
            }
        }
        (n, b)
    }

    /// Whether any pixel in the box matches `f`.
    pub fn any(&self, x0: f64, y0: f64, x1: f64, y1: f64, f: impl Fn([u8; 3]) -> bool) -> bool {
        let mut y = y0;
        while y < y1 {
            let mut x = x0;
            while x < x1 {
                if f(self.at(x, y)) {
                    return true;
                }
                x += 1.0;
            }
            y += 1.0;
        }
        false
    }
}

/// Renders page 1 with `mutool draw` (`None` without mutool).
pub fn render(pdf: &[u8], name: &str) -> Option<Raster> {
    let Some(mutool) = tool("mutool") else {
        eprintln!("mutool not found; skipping rendering");
        return None;
    };
    let src = scratch(&format!("{name}.pdf"));
    let dst = scratch(&format!("{name}.ppm"));
    std::fs::write(&src, pdf).unwrap();
    let out = Command::new(mutool)
        .args(["draw", "-q", "-r", "72", "-o"])
        .arg(&dst)
        .arg(&src)
        .arg("1")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "mutool failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Some(parse_ppm(&std::fs::read(&dst).unwrap()))
}

/// Renders page 1 with `pdftoppm` (`None` without it).
pub fn render_poppler(pdf: &[u8], name: &str) -> Option<Raster> {
    let Some(pdftoppm) = tool("pdftoppm") else {
        eprintln!("pdftoppm not found; skipping rendering");
        return None;
    };
    let src = scratch(&format!("{name}.pdf"));
    let prefix = scratch(&format!("{name}-poppler"));
    std::fs::write(&src, pdf).unwrap();
    let out = Command::new(pdftoppm)
        .args(["-r", "72", "-f", "1", "-l", "1", "-singlefile"])
        .arg(&src)
        .arg(&prefix)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "pdftoppm failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut ppm = prefix.into_os_string();
    ppm.push(".ppm");
    Some(parse_ppm(&std::fs::read(ppm).unwrap()))
}

/// Renderings by every available renderer.
pub fn render_all(pdf: &[u8], name: &str) -> Vec<(&'static str, Raster)> {
    [
        ("mutool", render(pdf, name)),
        ("pdftoppm", render_poppler(pdf, name)),
    ]
    .into_iter()
    .filter_map(|(n, r)| Some((n, r?)))
    .collect()
}

/// Where page point `(x, y)` lands in the image of a `w` x `h` page shown with `/Rotate rot`
/// (image coordinates, origin top left, one pixel per point).
pub fn page_to_image(x: f64, y: f64, w: f64, h: f64, rot: u16) -> (f64, f64) {
    match rot {
        90 => (y, x),
        180 => (w - x, y),
        270 => (h - y, w - x),
        _ => (x, h - y),
    }
}

/// The image-space box of a page rectangle, as `count_px` takes it.
pub fn image_box(
    r: (f64, f64, f64, f64),
    w: f64,
    h: f64,
    rot: u16,
) -> (usize, usize, usize, usize) {
    let a = page_to_image(r.0, r.1, w, h, rot);
    let b = page_to_image(r.2, r.3, w, h, rot);
    (
        a.0.min(b.0).round() as usize,
        a.1.min(b.1).round() as usize,
        a.0.max(b.0).round() as usize,
        a.1.max(b.1).round() as usize,
    )
}

fn parse_ppm(b: &[u8]) -> Raster {
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while b[i].is_ascii_whitespace() {
            i += 1;
        }
        if b[i] == b'#' {
            while b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let s = i;
        while !b[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(String::from_utf8_lossy(&b[s..i]).into_owned());
    }
    assert_eq!(fields[0], "P6");
    let width: usize = fields[1].parse().unwrap();
    let height: usize = fields[2].parse().unwrap();
    let rgb = b[i + 1..i + 1 + width * height * 3].to_vec();
    Raster { width, height, rgb }
}

pub fn is_white(p: [u8; 3]) -> bool {
    p.iter().all(|&c| c > 240)
}

pub fn is_dark(p: [u8; 3]) -> bool {
    p.iter().all(|&c| c < 110)
}

pub fn is_red(p: [u8; 3]) -> bool {
    p[0] > 180 && p[1] < 90 && p[2] < 90
}

pub fn is_blue(p: [u8; 3]) -> bool {
    p[2] > 120 && p[0] < 90 && p[1] < 110
}

pub fn is_yellow(p: [u8; 3]) -> bool {
    p[0] > 200 && p[1] > 180 && p[2] < 120
}

// Forms.

mod sample;
pub use sample::sample_form;

use pagebara_syntax::parser::Parser;
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

pub const PAGE: ObjRef = ObjRef::new(3, 0);

/// An object from PDF syntax.
pub fn o(s: &str) -> Object {
    Parser::new(s.as_bytes(), 0).object().unwrap()
}

/// Adds `s` (PDF syntax) as a new object.
pub fn add(doc: &mut Document, s: &str) -> ObjRef {
    let obj = o(s);
    doc.add(obj)
}

/// A stream object with the given dictionary and data.
pub fn add_stream(doc: &mut Document, dict: &str, data: &[u8]) -> ObjRef {
    let d = o(dict).as_dict().cloned().unwrap_or_default();
    doc.add(Stream::new(d, data.to_vec()))
}

/// Edits object `r` as a dictionary.
pub fn edit(doc: &mut Document, r: ObjRef, f: impl FnOnce(&mut Dict)) {
    let mut d = doc.get(r).unwrap().as_dict().unwrap().clone();
    f(&mut d);
    doc.set(r, d);
}

fn list(v: &[ObjRef]) -> String {
    v.iter()
        .map(|r| format!("{} 0 R", r.num))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Wires `fields` into the catalog's form and `widgets` into page 3's `/Annots`, then
/// saves a full file and reopens it, so the form is the "original" file.
pub fn finish(
    mut doc: Document,
    form_extra: &str,
    fields: &[ObjRef],
    widgets: &[ObjRef],
) -> Document {
    let helv = add(
        &mut doc,
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>",
    );
    let zadb = add(
        &mut doc,
        "<</Type/Font/Subtype/Type1/BaseFont/ZapfDingbats>>",
    );
    let af = add(
        &mut doc,
        &format!(
            "<</Fields[{}]/DR<</Font<</Helv {} 0 R/ZaDb {} 0 R>>>>/DA(/Helv 0 Tf 0 g){form_extra}>>",
            list(fields),
            helv.num,
            zadb.num
        ),
    );
    edit(&mut doc, ObjRef::new(1, 0), |d| d.set(b"AcroForm", af));
    let annots = o(&format!("[{}]", list(widgets)));
    edit(&mut doc, PAGE, |d| d.set(b"Annots", annots));
    Document::open(doc.save_full().unwrap()).unwrap()
}

/// `qpdf --json` form field values by full name.
pub fn qpdf_values(pdf: &[u8], name: &str) -> Option<Vec<(String, String)>> {
    let qpdf = tool("qpdf")?;
    let path = scratch(&format!("{name}-json.pdf"));
    std::fs::write(&path, pdf).unwrap();
    let out = Command::new(qpdf)
        .args(["--json", "--json-key=acroform"])
        .arg(&path)
        .output()
        .unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let fields = v["acroform"]["fields"].as_array()?.clone();
    let strip = |s: &str| s.trim_start_matches("u:").to_owned();
    Some(
        fields
            .iter()
            .map(|f| {
                let val = match &f["value"] {
                    serde_json::Value::String(s) => strip(s),
                    other => other.to_string(),
                };
                (strip(f["fullname"].as_str().unwrap_or("")), val)
            })
            .collect(),
    )
}

/// `pdftk dump_data_fields_utf8` as (name, values) pairs.
pub fn pdftk_values(pdf: &[u8], name: &str) -> Option<Vec<(String, String)>> {
    let pdftk = tool("pdftk")?;
    let path = scratch(&format!("{name}-pdftk.pdf"));
    std::fs::write(&path, pdf).unwrap();
    let out = Command::new(pdftk)
        .arg(&path)
        .arg("dump_data_fields_utf8")
        .output()
        .unwrap();
    if !out.status.success() {
        eprintln!("pdftk failed: {}", String::from_utf8_lossy(&out.stderr));
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let mut v = Vec::new();
    for block in text.split("---") {
        let get = |k: &str| -> Vec<String> {
            block
                .lines()
                .filter_map(|l| l.strip_prefix(k))
                .map(|s| s.trim().to_owned())
                .collect()
        };
        if let Some(n) = get("FieldName:").pop() {
            v.push((n, get("FieldValue:").join("|")));
        }
    }
    Some(v)
}

pub fn value_of<'a>(v: &'a [(String, String)], name: &str) -> Option<&'a str> {
    v.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

/// Anything drawn: not white and not a light tint.
pub fn is_ink(p: [u8; 3]) -> bool {
    p.iter().any(|&c| c < 160)
}
