//! Test documents, rendering, extraction and external checks.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::process::Command;
use std::sync::{Arc, OnceLock};

use pagebara_font::fallback::FontDb;
use pagebara_render::{Annotations, PixelRect, RenderRequest, Renderer};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};
use pagebara_typeset::{Changes, Encoded, RunFont};

pub const DEJAVU: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";
pub const DEJAVU_SERIF: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSerif.ttf";
pub const NIMBUS_OTF: &str = "/usr/share/fonts/opentype/urw-base35/NimbusSans-Regular.otf";
pub const NIMBUS_T1: &str = "/usr/share/fonts/type1/urw-base35/NimbusSans-Regular.t1";
pub const NOTO_CJK: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";

/// A system font, or `None` (the test is skipped) when it is not installed.
pub fn system(path: &str) -> Option<Vec<u8>> {
    let d = std::fs::read(path).ok();
    if d.is_none() {
        eprintln!("skipped: {path} not installed");
    }
    d
}

/// The system font database, scanned once per test binary.
pub fn db() -> Arc<FontDb> {
    static DB: OnceLock<Arc<FontDb>> = OnceLock::new();
    DB.get_or_init(|| Arc::new(FontDb::system())).clone()
}

/// A minimal PDF writer: objects 1 and 2 are the catalog and page tree.
pub struct Pdf {
    objects: Vec<Vec<u8>>,
    pages: Vec<usize>,
}

impl Pdf {
    pub fn new() -> Self {
        Pdf {
            objects: vec![Vec::new(), Vec::new()],
            pages: Vec::new(),
        }
    }

    pub fn add(&mut self, body: impl AsRef<[u8]>) -> usize {
        self.objects.push(body.as_ref().to_vec());
        self.objects.len()
    }

    pub fn stream(&mut self, dict: &str, data: &[u8]) -> usize {
        let mut b = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        b.extend_from_slice(data);
        b.extend_from_slice(b"\nendstream");
        self.add(b)
    }

    pub fn page(&mut self, resources: &str, content: &str) -> usize {
        let c = self.stream("", content.as_bytes());
        let p = self.add(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Resources << {resources} >> /Contents {c} 0 R >>"
        ));
        self.pages.push(p);
        p
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.objects[0] = b"<< /Type /Catalog /Pages 2 0 R >>".to_vec();
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
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            self.objects.len() + 1
        );
        out.extend_from_slice(x.as_bytes());
        out
    }
}

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|v| format!("{v:02X}")).collect()
}

/// A document with one page showing `original` at (20, 250) in font `/F1` = `font_obj`
/// (the body of the font object; `extra` adds objects first, referenced by `{n}` as the
/// object number of the i-th extra object). Returns the document and the font's reference.
pub fn doc_with_font(
    extra: &[(&str, Vec<u8>)],
    font_body: &dyn Fn(&[usize]) -> String,
    original_codes: &[u8],
) -> (Document, ObjRef) {
    let mut p = Pdf::new();
    let nums: Vec<usize> = extra
        .iter()
        .map(|(dict, data)| {
            if dict.starts_with("<<") || dict.starts_with('[') {
                p.add(dict.as_bytes())
            } else {
                p.stream(dict, data)
            }
        })
        .collect();
    let f = p.add(font_body(&nums));
    p.page(
        &format!("/Font << /F1 {f} 0 R >>"),
        &format!("BT /F1 16 Tf 20 250 Td <{}> Tj ET\n", hex(original_codes)),
    );
    let doc = Document::open(p.finish()).unwrap();
    let r = doc.reference(f as u32).unwrap();
    (doc, r)
}

/// Content showing `enc` at (x, y) in size `size`, one `TJ` per run; `F1` for the
/// original font, `FB<n>` for fallbacks.
pub fn show(enc: &Encoded, x: f32, y: f32, size: f32) -> String {
    let mut s = format!("BT {x} {y} Td\n");
    for run in &enc.runs {
        let _ = writeln!(
            s,
            "/{} {size} Tf {} TJ",
            res_name(run.font),
            run.tj_operand()
        );
    }
    s.push_str("ET\n");
    s
}

pub fn res_name(f: RunFont) -> String {
    match f {
        RunFont::Font(_) => "F1".into(),
        RunFont::Fallback(id) => format!("FB{}", id.0),
    }
}

/// Appends `content` to page 0 and adds the fallback fonts (and copied fonts) of `changes`
/// to its resources; returns the saved file.
pub fn add_to_page(doc: &mut Document, changes: &Changes, content: &str) -> Vec<u8> {
    let tree = pagebara_pages::PageTree::load(doc);
    let pr = tree.pages()[0].r;
    let mut pd = doc.get(pr).unwrap().as_dict().unwrap().clone();
    let mut res = doc.get_in(&pd, b"Resources").as_dict().unwrap().clone();
    let mut fonts = doc.get_in(&res, b"Font").as_dict().unwrap().clone();
    for (id, r) in &changes.fallbacks {
        fonts.set(format!("FB{}", id.0).as_bytes(), Object::Ref(*r));
    }
    for c in &changes.fonts {
        if c.copied {
            fonts.set(b"F1", Object::Ref(c.object));
        }
    }
    res.set(b"Font", Object::Dict(fonts));
    pd.set(b"Resources", Object::Dict(res));
    let old = doc.get_in(&pd, b"Contents").as_stream().unwrap().clone();
    let mut data = doc.decode(&old).unwrap().data.to_vec();
    data.extend_from_slice(content.as_bytes());
    let c = doc.add(Stream::new(Dict::new(), data));
    pd.set(b"Contents", Object::Ref(c));
    doc.set(pr, pd);
    doc.save_full().unwrap()
}

/// Renders page 0 at 2 px/pt.
pub fn render(doc: &Document) -> (u32, u32, Vec<u8>) {
    let r = Renderer::new(doc.clone());
    let (w, h) = r.page_size(0).unwrap();
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: (w * 2.0).round() as u32,
        h: (h * 2.0).round() as u32,
    };
    let mut req = RenderRequest::new(2.0, rect);
    req.annotations = Annotations::None;
    let img = r.render(0, &req).unwrap();
    (img.width, img.height, img.data)
}

/// Non-white pixels in a page-space rectangle of a 400 × 300 page rendered at 2 px/pt.
pub fn ink(img: &(u32, u32, Vec<u8>), x0: f32, y0: f32, x1: f32, y1: f32) -> usize {
    let (w, h) = (img.0 as i64, img.1 as i64);
    let px = |x: f32| (x * 2.0).round() as i64;
    let py = |y: f32| ((300.0 - y) * 2.0).round() as i64;
    let mut n = 0;
    for y in py(y1).max(0)..py(y0).min(h) {
        for x in px(x0).max(0)..px(x1).min(w) {
            let i = ((y * w + x) * 4) as usize;
            if img.2[i..i + 3].iter().any(|&v| v < 200) {
                n += 1;
            }
        }
    }
    n
}

pub fn page_text(doc: &Document) -> pagebara_content::TextPage {
    let tree = pagebara_pages::PageTree::load(doc);
    let p = doc.get(tree.pages()[0].r).unwrap();
    let pd = p.as_dict().unwrap();
    let res = doc
        .get_in(pd, b"Resources")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let list = pagebara_content::Interpreter::new(doc).page(pd, &res);
    pagebara_content::extract_text(doc, &list)
}

/// The extracted characters on the line with baseline `y` (origin x, advance end x, text).
pub fn line_chars(page: &pagebara_content::TextPage, y: f32) -> Vec<(f32, f32, String)> {
    page.chars
        .iter()
        .filter(|c| c.glyph != pagebara_content::display::NONE)
        .filter(|c| c.quad[0].y <= y + 0.01 && c.quad[2].y > y)
        .map(|c| (c.quad[0].x, c.quad[1].x, page.char_text(c).to_string()))
        .collect()
}

fn tool(name: &str) -> bool {
    Command::new(name).arg("--version").output().is_ok()
        || Command::new(name).arg("-v").output().is_ok()
}

/// qpdf --check and mutool pass, and the external extractors find `texts`. Skipped for
/// tools that are not installed.
pub fn external_checks(file: &[u8], texts: &[&str]) {
    let dir = std::env::temp_dir().join(format!("pagebara-typeset-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let path = dir.join(format!(
        "t{}.pdf",
        N.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::write(&path, file).unwrap();
    if let Ok(d) = std::env::var("TYPESET_DUMP") {
        let name = std::thread::current()
            .name()
            .unwrap_or("t")
            .replace("::", "-");
        let _ = std::fs::write(std::path::Path::new(&d).join(format!("{name}.pdf")), file);
    }
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
    } else {
        eprintln!("skipped: qpdf not installed");
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
            assert!(text.contains(t), "mutool text lacks {t:?}: {text:?}");
        }
    } else {
        eprintln!("skipped: mutool not installed");
    }
    if tool("pdftotext") {
        let o = Command::new("pdftotext")
            .arg(&path)
            .arg("-")
            .output()
            .unwrap();
        assert!(o.status.success());
        let text = String::from_utf8_lossy(&o.stdout);
        for t in texts {
            assert!(text.contains(t), "pdftotext text lacks {t:?}: {text:?}");
        }
    }
    let _ = std::fs::remove_file(&path);
}

/// A FontFile stream body for a binary Type 1 program (PFA header, binary eexec part,
/// zeros trailer): its data and `Length1`..`Length3`.
pub fn type1_file(data: &[u8]) -> (Vec<u8>, [usize; 3]) {
    let find = |n: &[u8]| data.windows(n.len()).position(|w| w == n);
    let e = find(b"eexec").unwrap() + 5;
    let mut l1 = e;
    while l1 < data.len() && (data[l1] == b'\r' || data[l1] == b'\n') {
        l1 += 1;
    }
    let ctm = data.windows(11).rposition(|w| w == b"cleartomark").unwrap();
    let mut t = ctm;
    while t > l1 && matches!(data[t - 1], b'0' | b'\n' | b'\r' | b' ') {
        t -= 1;
    }
    (data.to_vec(), [l1, t - l1, data.len() - t])
}

/// The `CFF ` table of an OpenType font.
pub fn cff_table(otf: &[u8]) -> Vec<u8> {
    let f = pagebara_font::Font::parse(otf.to_vec()).unwrap();
    f.as_sfnt().unwrap().table(b"CFF ").unwrap().to_vec()
}

/// Program advance widths (thousandths) of the glyphs a simple font with WinAnsi codes
/// 32..=126 shows.
pub fn ascii_widths(program: &[u8]) -> String {
    let f = pagebara_font::Font::parse(program.to_vec()).unwrap();
    let m = f.font_matrix();
    (32u8..=126)
        .map(|c| {
            let g = f.glyph_for_char(char::from(c)).unwrap_or(0);
            let w = f.advance(g).unwrap_or(0.0);
            format!(
                "{} ",
                ((m.apply(w, 0.0).0 - m.apply(0.0, 0.0).0) * 1000.0).round()
            )
        })
        .collect()
}
