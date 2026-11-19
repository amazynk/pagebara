//! Opening a file's bytes and drawing or reading its pages.

use pagebara_content::{Interpreter, extract_text};
use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_syntax::{Dict, Document, Error, Object};

use crate::image;

/// Largest bitmap side drawn (the preview pane on a big screen stays well below).
pub const MAX_SIDE: u32 = 8192;

/// The document in `bytes`; a protected file opens only when it needs no password to read.
pub fn open(bytes: Vec<u8>) -> Option<Document> {
    if bytes.len() as u64 > crate::MAX_FILE {
        return None;
    }
    match Document::open(bytes) {
        Ok(d) => Some(d),
        Err(Error::PasswordRequired(locked)) => locked.unlock("").ok(),
        Err(_) => None,
    }
}

/// A page drawn on white to fit `max_w` × `max_h` pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// Top-down rows of BGRA, alpha premultiplied (opaque here).
    pub bgra: Vec<u8>,
}

/// Draws `page` (0-based) fitted into the box; `None` when there is no such page or it
/// cannot be drawn.
pub fn render_fit(r: &Renderer, page: usize, max_w: u32, max_h: u32) -> Option<Bitmap> {
    let (w, h) = r.page_size(page)?;
    let (pw, ph, scale) = image::fit(w, h, max_w.min(MAX_SIDE), max_h.min(MAX_SIDE));
    let req = RenderRequest::new(
        scale,
        PixelRect {
            x: 0,
            y: 0,
            w: pw,
            h: ph,
        },
    );
    let mut img = r.render(page, &req).ok()?;
    image::rgba_to_bgra(&mut img.data);
    Some(Bitmap {
        width: img.width,
        height: img.height,
        bgra: img.data,
    })
}

/// The text of each page, in reading order; stops once `limit` characters are collected.
pub fn page_texts(doc: &Document, limit: usize) -> Vec<String> {
    let tree = pagebara_pages::PageTree::load(doc);
    let mut interp = Interpreter::new(doc);
    let mut out = Vec::new();
    let mut total = 0;
    for p in tree.pages() {
        let text = doc
            .get(p.r)
            .and_then(|o| {
                let page = o.as_dict()?;
                let res = resources(doc, page);
                Some(extract_text(doc, &interp.page(page, &res)).text)
            })
            .unwrap_or_default();
        total += text.len();
        out.push(text);
        if total >= limit {
            break;
        }
    }
    out
}

/// The document title from its information dictionary.
pub fn title(doc: &Document) -> Option<String> {
    let info = doc.get_in(doc.trailer(), b"Info");
    let t = doc.get_in(info.as_dict()?, b"Title");
    let s = text_string(&t)?;
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_owned())
}

/// A page's resources: its own or inherited from the page tree.
fn resources(doc: &Document, page: &Dict) -> Dict {
    let mut cur = page.clone();
    for _ in 0..64 {
        if let Some(r) = doc.get_in(&cur, b"Resources").as_dict() {
            return r.clone();
        }
        let parent = doc.get_in(&cur, b"Parent");
        let Some(p) = parent.as_dict() else { break };
        cur = p.clone();
    }
    Dict::new()
}

/// A PDF text string: UTF-16BE or UTF-8 with a byte order mark, else PDFDocEncoding.
fn text_string(o: &Object) -> Option<String> {
    let b = o.as_string()?;
    Some(if let Some(u) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units: Vec<u16> = u
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&c| u16::from_be_bytes(c))
            .collect();
        String::from_utf16_lossy(&units)
    } else if let Some(u) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        String::from_utf8_lossy(u).into_owned()
    } else {
        b.iter()
            .map(|&c| pagebara_syntax::text::pdfdoc_char(c))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A one-page document with a title and the word "Hello".
    fn sample() -> Vec<u8> {
        let content = b"BT /F1 24 Tf 72 720 Td (Hello) Tj ET";
        let objs = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
             /Resources << /Font << /F1 5 0 R >> >> >>"
                .to_owned(),
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                String::from_utf8_lossy(content)
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
            "<< /Title (Quarterly report) >>".to_owned(),
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{o}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).as_bytes(),
        );
        for o in offsets {
            out.extend_from_slice(format!("{o:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objs.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn renders_page_one_fitted() {
        let doc = open(sample()).unwrap();
        let r = Renderer::new(doc);
        let b = render_fit(&r, 0, 256, 256).unwrap();
        assert_eq!((b.width, b.height), (198, 256));
        assert_eq!(b.bgra.len(), 198 * 256 * 4);
        // White background, opaque.
        assert_eq!(&b.bgra[..4], &[255, 255, 255, 255]);
        // Some ink from the text.
        assert!(b.bgra.as_chunks::<4>().0.iter().any(|p| p[0] < 128));
        assert!(render_fit(&r, 1, 256, 256).is_none());
    }

    #[test]
    fn reads_text_and_title() {
        let doc = open(sample()).unwrap();
        let texts = page_texts(&doc, usize::MAX);
        assert_eq!(texts.len(), 1);
        assert!(texts[0].contains("Hello"), "{texts:?}");
        assert_eq!(title(&doc).as_deref(), Some("Quarterly report"));
    }

    #[test]
    fn not_a_pdf() {
        assert!(open(b"hello, world".to_vec()).is_none());
    }

    #[test]
    fn text_strings() {
        let utf16 = Object::String(pagebara_syntax::PdfString {
            bytes: vec![0xFE, 0xFF, 0, b'H', 0, b'i'],
            hex: true,
        });
        assert_eq!(text_string(&utf16).as_deref(), Some("Hi"));
    }
}
