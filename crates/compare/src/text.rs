//! The words of a document with their places on the pages as shown.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use pagebara_content::{Interpreter, TextPage};
use pagebara_pages::{Page, PageTree};
use pagebara_syntax::{Dict, Document};
use rayon::prelude::*;

/// A rectangle on a page as shown, in points from its top-left corner: x0, y0, x1, y1.
pub type Box4 = [f32; 4];

#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: Box<str>,
    /// Hash of the text, for comparing.
    pub key: u64,
    pub page: u32,
    pub rect: Box4,
}

/// All words of a document in reading order, page after page.
#[derive(Clone, Debug, Default)]
pub struct DocText {
    pub words: Vec<Word>,
    /// Index of each page's first word; one more entry than pages.
    pub page_start: Vec<usize>,
    /// Each page's shown size in points.
    pub sizes: Vec<(f32, f32)>,
}

impl DocText {
    pub fn pages(&self) -> usize {
        self.sizes.len()
    }

    pub fn page_words(&self, p: usize) -> &[Word] {
        &self.words[self.page_start[p]..self.page_start[p + 1]]
    }

    /// Extracts the text of every page, in parallel.
    pub fn extract(doc: &Document) -> DocText {
        let tree = PageTree::load(doc);
        let pages = tree.pages().to_vec();
        let texts: Vec<Arc<TextPage>> = pages
            .par_iter()
            .map_init(
                || Interpreter::new(doc),
                |it, p| Arc::new(extract_page(doc, it, p)),
            )
            .collect();
        DocText::from_pages(doc, &texts)
    }

    /// From text already extracted (one per page, as the app's text index keeps it).
    pub fn from_pages(doc: &Document, texts: &[Arc<TextPage>]) -> DocText {
        let tree = PageTree::load(doc);
        let mut out = DocText::default();
        for (i, p) in tree.pages().iter().enumerate() {
            out.page_start.push(out.words.len());
            let (w, h) = p.size();
            out.sizes.push((w as f32, h as f32));
            let Some(t) = texts.get(i) else { continue };
            let to_shown = shown(p);
            for word in &t.words {
                let text = t.word_text(word).trim();
                if text.is_empty() {
                    continue;
                }
                let r = word.bbox;
                let pts = [(r.x0, r.y0), (r.x1, r.y0), (r.x0, r.y1), (r.x1, r.y1)]
                    .map(|(x, y)| to_shown(f64::from(x), f64::from(y)));
                let x0 = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
                let x1 = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max);
                let y0 = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min);
                let y1 = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max);
                out.words.push(Word {
                    key: key(text),
                    text: text.into(),
                    page: i as u32,
                    // Glyph boxes reach from the baseline up: add room for descenders.
                    rect: [x0, y0, x1, y1 + 0.25 * (y1 - y0)],
                });
            }
        }
        out.page_start.push(out.words.len());
        out
    }
}

pub(crate) fn key(s: &str) -> u64 {
    let mut h = DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

fn resources(doc: &Document, page: &Dict) -> Dict {
    let mut cur = page.clone();
    for _ in 0..64 {
        if let Some(d) = doc.get_in(&cur, b"Resources").as_dict() {
            return d.clone();
        }
        let parent = doc.get_in(&cur, b"Parent");
        match parent.as_dict() {
            Some(p) => cur = p.clone(),
            None => break,
        }
    }
    Dict::new()
}

pub(crate) fn extract_page(doc: &Document, it: &mut Interpreter<'_>, p: &Page) -> TextPage {
    let Some(o) = doc.get(p.r) else {
        return TextPage::default();
    };
    let Some(d) = o.as_dict() else {
        return TextPage::default();
    };
    let list = it.page(d, &resources(doc, d));
    pagebara_content::extract_text(doc, &list)
}

/// User space of a page to shown points from the top left.
pub(crate) fn shown(p: &Page) -> impl Fn(f64, f64) -> (f32, f32) + use<> {
    let c = p.crop_box;
    let rot = p.rotate;
    let (cw, ch) = (c.width(), c.height());
    move |x, y| {
        let (u, v) = (x - c.x0, y - c.y0);
        let (sx, sy_top) = match rot {
            90 => (v, u),
            180 => (cw - u, v),
            270 => (ch - v, cw - u),
            _ => (u, ch - v),
        };
        (sx as f32, sy_top as f32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shown_corners() {
        let doc = Document::open(pagebara_samples::rotated()).unwrap();
        let tree = PageTree::load(&doc);
        let p = tree.page(0).unwrap();
        let f = shown(&p);
        // Rotated 90 clockwise: the user-space bottom left is the shown top left.
        assert_eq!(f(0.0, 0.0), (0.0, 0.0));
        assert_eq!(f(0.0, 792.0), (792.0, 0.0));
        assert_eq!(f(612.0, 0.0), (0.0, 612.0));
    }
}
