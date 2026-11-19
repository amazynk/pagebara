//! Synthetic scans: text pages made here, rendered, degraded and embedded as image-only
//! pages, with the text they show as ground truth.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::path::PathBuf;

use pagebara_ocr::{ModelSource, Script};
use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;

/// A minimal PDF from object bodies (object 1 must be the catalog).
pub fn pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n".to_vec();
    let mut offsets = Vec::new();
    for (i, o) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).bytes());
        out.extend(o);
        out.extend(b"\nendobj\n");
    }
    let xref = out.len();
    let mut s = format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1);
    for o in offsets {
        let _ = writeln!(s, "{o:010} 00000 n ");
    }
    let _ = write!(
        s,
        "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    out.extend(s.bytes());
    out
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut o = format!("<<{dict}/Length {}>>stream\n", data.len()).into_bytes();
    o.extend(data);
    o.extend(b"\nendstream");
    o
}

fn escape(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            '(' | ')' | '\\' => format!("\\{c}"),
            c => c.to_string(),
        })
        .collect()
}

/// A Letter page of Helvetica (and a Times heading) showing `lines` at `size` points.
pub fn text_pdf(title: &str, lines: &[&str], size: f32) -> Vec<u8> {
    let mut c = format!("BT /F2 18 Tf 72 720 Td ({}) Tj ET\n", escape(title));
    let _ = writeln!(c, "BT /F1 {size} Tf {} TL 72 690 Td", size * 1.35);
    for l in lines {
        let _ = writeln!(c, "({}) Tj T*", escape(l));
    }
    c.push_str("ET\n");
    pdf(&[
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
        b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R\
          /Resources<</Font<</F1 5 0 R/F2 6 0 R>>>>>>"
            .to_vec(),
        stream("", c.as_bytes()),
        b"<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>".to_vec(),
        b"<</Type/Font/Subtype/Type1/BaseFont/Times-Bold/Encoding/WinAnsiEncoding>>".to_vec(),
    ])
}

/// Renders page 0 of a PDF to 8-bit gray at `dpi`.
pub fn render_gray(data: &[u8], dpi: f32) -> (Vec<u8>, usize, usize) {
    let doc = Document::open(data.to_vec()).expect("pdf");
    let r = Renderer::new(doc);
    let (w, h) = r.page_size(0).expect("page");
    let s = dpi / 72.0;
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: (w * s).round() as u32,
        h: (h * s).round() as u32,
    };
    let img = r.render(0, &RenderRequest::new(s, rect)).expect("render");
    let gray = img
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| ((u32::from(p[0]) * 3 + u32::from(p[1]) * 6 + u32::from(p[2])) / 10) as u8)
        .collect();
    (gray, rect.w as usize, rect.h as usize)
}

/// A small deterministic generator.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407))
    }

    pub fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32) / (1u64 << 24) as f32
    }
}

/// Rotated by `degrees` about the centre (white fill), with Gaussian-ish noise of amplitude
/// `noise` grey levels and a blur, like a mediocre scan.
pub fn degrade(gray: &[u8], w: usize, h: usize, degrees: f32, noise: f32, seed: u64) -> Vec<u8> {
    let (s, c) = degrees.to_radians().sin_cos();
    let (cx, cy) = (w as f32 / 2.0, h as f32 / 2.0);
    let at = |x: isize, y: isize| -> f32 {
        if x < 0 || y < 0 || x >= w as isize || y >= h as isize {
            255.0
        } else {
            f32::from(gray[y as usize * w + x as usize])
        }
    };
    let mut rng = Rng::new(seed);
    let mut out = vec![255u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let (dx, dy) = (x as f32 - cx, y as f32 - cy);
            let (sx, sy) = (c * dx + s * dy + cx, -s * dx + c * dy + cy);
            let (x0, y0) = (sx.floor(), sy.floor());
            let (tx, ty) = (sx - x0, sy - y0);
            let (x0, y0) = (x0 as isize, y0 as isize);
            let v = at(x0, y0) * (1.0 - tx) * (1.0 - ty)
                + at(x0 + 1, y0) * tx * (1.0 - ty)
                + at(x0, y0 + 1) * (1.0 - tx) * ty
                + at(x0 + 1, y0 + 1) * tx * ty;
            let n = (rng.next() + rng.next() + rng.next() - 1.5) * noise;
            out[y * w + x] = (v + n).round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// A one-page PDF with media box `media` showing an 8-bit gray image placed by `cm`, with
/// extra page dictionary entries (`/Rotate`, `/UserUnit`, a `/CropBox`).
pub fn scan_pdf(
    gray: &[u8],
    w: usize,
    h: usize,
    media: [f32; 4],
    cm: [f32; 6],
    extra: &str,
) -> Vec<u8> {
    let z = miniz_oxide::deflate::compress_to_vec_zlib(gray, 6);
    let [a, b, c, d, e, f] = cm;
    let content = format!("q {a} {b} {c} {d} {e} {f} cm /Im0 Do Q\n");
    let [x0, y0, x1, y1] = media;
    pdf(&[
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
        format!(
            "<</Type/Page/Parent 2 0 R/MediaBox[{x0} {y0} {x1} {y1}]{extra}/Contents 4 0 R\
             /Resources<</XObject<</Im0 5 0 R>>>>>>"
        )
        .into_bytes(),
        stream("", content.as_bytes()),
        stream(
            &format!(
                "/Type/XObject/Subtype/Image/Width {w}/Height {h}/ColorSpace/DeviceGray\
                 /BitsPerComponent 8/Filter/FlateDecode"
            ),
            &z,
        ),
    ])
}

/// Edit distance over characters.
pub fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (diag + usize::from(ca != cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// Whitespace collapsed to single spaces.
pub fn normalize(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Character error rate of `got` against `want`, whitespace normalised.
pub fn cer(got: &str, want: &str) -> f32 {
    let (g, w) = (normalize(got), normalize(want));
    levenshtein(&g, &w) as f32 / w.chars().count().max(1) as f32
}

/// The model directory from `PAGEBARA_OCR_MODELS`, when it holds the Latin models.
pub fn models() -> Option<Vec<ModelSource>> {
    let dir = PathBuf::from(std::env::var_os("PAGEBARA_OCR_MODELS")?);
    let src = ModelSource::Dir(dir);
    if Script::Latin.files().iter().all(|f| src.has(f)) {
        Some(vec![src])
    } else {
        None
    }
}

pub const TITLE: &str = "Quarterly Report on Rivers";

pub const LINES: &[&str] = &[
    "The river rises in the northern hills and runs for about ninety miles",
    "before it reaches the sea. Its valley holds three market towns, each",
    "with a stone bridge built in the eighteenth century. Water levels were",
    "measured twice a day at seven stations between March and June 2024.",
    "Average flow at the lower gauge was 41.7 cubic metres per second, a",
    "rise of 12% on the same period last year (see Table 3, page 18).",
    "Fishing licences sold: 1,284. Complaints received about pollution: 37.",
    "Contact the survey office at rivers@example.org for the full data set.",
];
