//! Shared by the targets: writing small PDF files around fuzzed parts, and exercising a
//! document the way the app does.

#![allow(dead_code)]

use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;

/// A classic file: object `i + 1` is `objs[i]`, object 1 the catalog.
pub fn classic(objs: &[Vec<u8>]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::with_capacity(objs.len());
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
        out.extend_from_slice(o);
        out.extend_from_slice(b"\nendobj\n");
    }
    let x = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f\r\n", objs.len() + 1).as_bytes());
    for o in offs {
        out.extend_from_slice(format!("{o:010} 00000 n\r\n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// A stream object body.
pub fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut b = format!("<<{dict}/Length {}>>stream\n", data.len()).into_bytes();
    b.extend_from_slice(data);
    b.extend_from_slice(b"\nendstream");
    b
}

/// A one-page file: objects 1–4 are the catalog, the page tree, the page (with
/// `resources`) and its contents; `extra` follow as objects 5, 6, …
pub fn one_page(resources: &str, content: &[u8], extra: &[Vec<u8>]) -> Vec<u8> {
    let mut objs = vec![
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
        format!("<</Type/Page/Parent 2 0 R/MediaBox[0 0 64 64]/Resources<<{resources}>>/Contents 4 0 R>>")
            .into_bytes(),
        stream("", content),
    ];
    objs.extend_from_slice(extra);
    classic(&objs)
}

/// Renders up to `max_pages` pages fitted into `side` pixels.
pub fn render(doc: &Document, max_pages: usize, side: f32) {
    let r = Renderer::new(doc.clone());
    for i in 0..r.page_count().min(max_pages) {
        let Some((w, h)) = r.page_size(i) else { continue };
        let scale = side / w.max(h).max(1.0);
        let rect = PixelRect {
            x: 0,
            y: 0,
            w: ((w * scale).ceil() as u32).clamp(1, 512),
            h: ((h * scale).ceil() as u32).clamp(1, 512),
        };
        let _ = r.render(i, &RenderRequest::new(scale, rect));
    }
}
