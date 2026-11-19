//! Pages: whether they already have text, rendering them for recognition, and mapping
//! recognised words back to page space.

use std::sync::atomic::AtomicBool;

use pagebara_content::{Interpreter, Matrix, Point, Rect, extract_text};
use pagebara_pages::{Page, PageTree};
use pagebara_render::{Annotations, PixelRect, RenderError, RenderRequest, Renderer, page_matrix};
use pagebara_syntax::{Dict, Document};

use crate::Error;
use crate::engine::{Engine, EngineParams, ImageLine};
use crate::image::{RgbImage, RotRect};

/// Rendered pages are scaled down to stay within this many pixels.
const MAX_PIXELS: f32 = 48e6;

/// A recognised word in the page's default user space.
#[derive(Clone, Debug, PartialEq)]
pub struct Word {
    pub text: String,
    /// Bottom-left (the start of the baseline, below descenders), bottom-right, top-right
    /// and top-left corners, in reading direction.
    pub quad: [Point; 4],
    /// Mean probability of the word's characters, 0 to 1.
    pub confidence: f32,
    /// Where each character starts: byte offset in `text`, fraction along the word.
    pub cuts: Vec<(usize, f32)>,
}

/// A line of words in reading order.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub words: Vec<Word>,
}

/// The text recognised on one page.
#[derive(Clone, Debug, PartialEq)]
pub struct PageText {
    /// Page index, from 0.
    pub page: usize,
    pub lines: Vec<Line>,
}

impl PageText {
    /// Lines joined by `\n`, words by spaces.
    pub fn text(&self) -> String {
        let lines: Vec<String> = self
            .lines
            .iter()
            .map(|l| {
                l.words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        lines.join("\n")
    }

    pub fn word_count(&self) -> usize {
        self.lines.iter().map(|l| l.words.len()).sum()
    }
}

/// The page dictionary and its (possibly inherited) resources.
pub(crate) fn page_dict(doc: &Document, page: &Page) -> Option<(Dict, Dict)> {
    let d = doc.get(page.r)?.as_dict()?.clone();
    let mut node = d.clone();
    let mut res = Dict::new();
    for _ in 0..64 {
        if let Some(r) = doc.get_in(&node, b"Resources").as_dict() {
            res = r.clone();
            break;
        }
        match doc.get_in(&node, b"Parent").as_dict() {
            Some(p) => node = p.clone(),
            None => break,
        }
    }
    Some((d, res))
}

/// The text of a page as [`extract_text`] reads it (the OCR layer included).
pub fn page_text(doc: &Document, page: usize) -> String {
    let tree = PageTree::open(doc);
    let Some((d, res)) = tree.page(page).and_then(|p| page_dict(doc, &p)) else {
        return String::new();
    };
    let list = Interpreter::new(doc).page(&d, &res);
    extract_text(doc, &list).text
}

/// Non-space characters of the text a page already has (visible or not).
pub fn text_chars(doc: &Document, page: usize) -> usize {
    page_text(doc, page)
        .chars()
        .filter(|c| !c.is_whitespace())
        .count()
}

/// Renders a page at `dpi` (scaled down if the image would be huge). Returns the image and
/// the matrix from its pixels to the page's default user space.
pub fn render_page(
    renderer: &Renderer,
    page: usize,
    dpi: f32,
    cancel: &AtomicBool,
) -> Result<(RgbImage, Matrix), Error> {
    let tree = PageTree::open(renderer.document());
    let p = tree.page(page).ok_or(Error::NoSuchPage(page))?;
    let (w, h) = p.size();
    let mut scale = dpi / 72.0;
    let px = w as f32 * h as f32 * scale * scale;
    if px > MAX_PIXELS {
        scale *= (MAX_PIXELS / px).sqrt();
    }
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: ((w as f32 * scale).round() as u32).max(1),
        h: ((h as f32 * scale).round() as u32).max(1),
    };
    let mut req = RenderRequest::new(scale, rect);
    req.annotations = Annotations::None;
    req.cancel = Some(cancel);
    let img = renderer.render(page, &req).map_err(|e| match e {
        RenderError::Cancelled => Error::Cancelled,
        RenderError::NoSuchPage(p) => Error::NoSuchPage(p),
        e => Error::Render(e.to_string()),
    })?;
    // Opaque over white, so premultiplied and straight values agree.
    let rgb = RgbImage::from_rgba(img.width as usize, img.height as usize, &img.data);
    drop(img);
    let c = p.crop_box;
    let crop = Rect::new(c.x0 as f32, c.y0 as f32, c.x1 as f32, c.y1 as f32);
    let m = page_matrix(crop, p.rotate, scale * p.user_unit as f32, 0, 0);
    let inv = m.invert().ok_or(Error::Render("degenerate page".into()))?;
    Ok((rgb, inv))
}

/// Maps lines found in an image to page space with `to_page`.
pub fn to_page_space(page: usize, lines: Vec<ImageLine>, to_page: &Matrix) -> PageText {
    let map = |r: &RotRect| {
        let p = |s, t| {
            let [x, y] = r.at(s, t);
            to_page.apply(Point::new(x, y))
        };
        [p(0.0, 1.0), p(1.0, 1.0), p(1.0, 0.0), p(0.0, 0.0)]
    };
    PageText {
        page,
        lines: lines
            .into_iter()
            .map(|l| Line {
                words: l
                    .words
                    .into_iter()
                    .map(|w| Word {
                        quad: map(&w.rect),
                        text: w.text,
                        confidence: w.confidence,
                        cuts: w.cuts,
                    })
                    .collect(),
            })
            .collect(),
    }
}

/// Renders and recognises one page.
pub fn recognize_page(
    engine: &Engine,
    renderer: &Renderer,
    page: usize,
    dpi: f32,
    params: &EngineParams,
    cancel: &AtomicBool,
) -> Result<PageText, Error> {
    let (img, to_page) = render_page(renderer, page, dpi, cancel)?;
    let lines = engine.recognize(&img, params, cancel)?;
    Ok(to_page_space(page, lines, &to_page))
}
