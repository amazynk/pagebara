//! Text of a range of pages, for the command line (`pagebara --text`, `--text --json`) and
//! other tools.

use std::io::{self, Write};

use pagebara_content::{Interpreter, Rect, TextPage, extract_text};
use pagebara_pages::Page;
use pagebara_render::page_matrix;
use pagebara_syntax::{Document, ObjRef};
use rayon::prelude::*;
use serde::Serialize;

use crate::pages;

/// Pages extracted in parallel before their text is written out, in order.
const CHUNK: usize = 256;

/// Parses a page list such as `5-10`, `3`, `1,4,7-9` or `8-` (to the end), 1-based, into
/// 0-based page indices of a document with `count` pages.
pub fn parse_pages(spec: &str, count: usize) -> Result<Vec<usize>, String> {
    let num = |s: &str| {
        s.trim()
            .parse::<usize>()
            .ok()
            .filter(|&n| n >= 1)
            .ok_or_else(|| format!("not a page number: {s:?}"))
    };
    let mut out = Vec::new();
    for part in spec.split(',') {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) if b.trim().is_empty() => (num(a)?, count),
            Some((a, b)) => (num(a)?, num(b)?),
            None => (num(part)?, num(part)?),
        };
        if a > count {
            return Err(format!("page {a} is past the end ({count} pages)"));
        }
        if a > b {
            return Err(format!("backwards range {a}-{b}"));
        }
        out.extend(a - 1..b.min(count));
    }
    Ok(out)
}

/// Writes the text of `pages` (0-based) to `out`, each after a `--- page N ---` line.
pub fn write_text(
    doc: &Document,
    refs: &[ObjRef],
    pages: &[usize],
    out: &mut impl Write,
) -> io::Result<()> {
    for chunk in pages.chunks(CHUNK) {
        let texts: Vec<String> = chunk
            .par_iter()
            .map_init(
                || Interpreter::new(doc),
                |interp, &i| page_text(doc, interp, refs.get(i)),
            )
            .collect();
        for (&i, text) in chunk.iter().zip(texts) {
            writeln!(out, "--- page {} ---", i + 1)?;
            out.write_all(text.trim_end().as_bytes())?;
            out.write_all(b"\n\n")?;
        }
    }
    out.flush()
}

fn page_text(doc: &Document, interp: &mut Interpreter, r: Option<&ObjRef>) -> String {
    text_page(doc, interp, r).text
}

fn text_page(doc: &Document, interp: &mut Interpreter, r: Option<&ObjRef>) -> TextPage {
    let Some(page) = r.and_then(|&r| doc.get(r)) else {
        return TextPage::default();
    };
    let Some(page) = page.as_dict() else {
        return TextPage::default();
    };
    let res = pages::resources(doc, page);
    extract_text(doc, &interp.page(page, &res))
}

/// One line of `--text --json` output.
#[derive(Debug, Serialize)]
struct JsonPage {
    /// 1-based.
    page: usize,
    width: f32,
    height: f32,
    text: String,
    blocks: Vec<JsonBlock>,
}

#[derive(Debug, Serialize)]
struct JsonBlock {
    /// Left, top, right, bottom.
    bbox: [f32; 4],
    text: String,
}

/// Writes one JSON object per page of `pages` (0-based indices into `all`), one per line:
/// `{"page":N,"width":W,"height":H,"text":"…","blocks":[{"bbox":[x0,y0,x1,y1],"text":"…"}]}`.
/// Sizes and boxes are in points of the page as displayed (crop box, rotation and user
/// unit applied) with the origin at its top left, y growing down.
pub fn write_json(
    doc: &Document,
    all: &[Page],
    pages: &[usize],
    out: &mut impl Write,
) -> io::Result<()> {
    for chunk in pages.chunks(CHUNK) {
        let lines: Vec<serde_json::Result<String>> = chunk
            .par_iter()
            .map_init(
                || Interpreter::new(doc),
                |interp, &i| serde_json::to_string(&json_page(doc, interp, all.get(i), i)),
            )
            .collect();
        for line in lines {
            out.write_all(line.map_err(io::Error::other)?.as_bytes())?;
            out.write_all(b"\n")?;
        }
    }
    out.flush()
}

fn json_page(doc: &Document, interp: &mut Interpreter, page: Option<&Page>, i: usize) -> JsonPage {
    let round = |v: f32| (v * 100.0).round() / 100.0;
    let Some(page) = page else {
        return JsonPage {
            page: i + 1,
            width: 0.0,
            height: 0.0,
            text: String::new(),
            blocks: Vec::new(),
        };
    };
    let t = text_page(doc, interp, Some(&page.r));
    let c = page.crop_box;
    let crop = Rect::new(c.x0 as f32, c.y0 as f32, c.x1 as f32, c.y1 as f32);
    let to_shown = page_matrix(crop, page.rotate, page.user_unit as f32, 0, 0);
    let blocks = t
        .blocks
        .iter()
        .map(|b| {
            let r = b.bbox.transform(&to_shown);
            let lines = &t.lines[b.lines.0 as usize..b.lines.1 as usize];
            let text: Vec<&str> = lines.iter().map(|l| t.line_text(l)).collect();
            JsonBlock {
                bbox: [r.x0, r.y0, r.x1, r.y1].map(round),
                text: text.join("\n"),
            }
        })
        .collect();
    let (w, h) = page.size();
    JsonPage {
        page: i + 1,
        width: round(w as f32),
        height: round(h as f32),
        text: t.text.trim_end().to_owned(),
        blocks,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_lists() {
        assert_eq!(parse_pages("5-7", 10), Ok(vec![4, 5, 6]));
        assert_eq!(parse_pages("1,3,9-", 10), Ok(vec![0, 2, 8, 9]));
        assert_eq!(parse_pages("8-20", 10), Ok(vec![7, 8, 9]));
        assert!(parse_pages("11", 10).is_err());
        assert!(parse_pages("0", 10).is_err());
        assert!(parse_pages("5-2", 10).is_err());
        assert!(parse_pages("x", 10).is_err());
    }

    #[test]
    fn writes_pages_in_order() {
        let doc = Document::open(crate::test_pdf::text(&["first line", "second line"])).unwrap();
        let one = pagebara_pages::PageTree::load(&doc).refs()[0];
        // The same page twice, then one that is missing.
        let refs = [one, one];
        let mut out = Vec::new();
        write_text(&doc, &refs, &[1, 0, 2], &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        let page = "first line\nsecond line";
        assert_eq!(
            s,
            format!("--- page 2 ---\n{page}\n\n--- page 1 ---\n{page}\n\n--- page 3 ---\n\n\n")
        );
    }

    #[test]
    fn json_lines_with_top_left_boxes() {
        let doc = Document::open(crate::test_pdf::text(&["first line", "second line"])).unwrap();
        let pages = pagebara_pages::PageTree::load(&doc).pages().to_vec();
        let mut out = Vec::new();
        write_json(&doc, &pages, &[0, 1], &mut out).unwrap();
        let s = String::from_utf8(out).unwrap();
        let lines: Vec<serde_json::Value> = s
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        let p = &lines[0];
        assert_eq!(
            (p["page"].as_u64(), p["width"].as_f64()),
            (Some(1), Some(595.0))
        );
        assert_eq!(p["text"], "first line\nsecond line");
        let b = &p["blocks"][0];
        assert_eq!(b["text"], "first line\nsecond line");
        let bbox: Vec<f64> = b["bbox"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_f64().unwrap())
            .collect();
        // Baseline 770 of 842 from the bottom: near the top, starting at x = 72.
        assert!((bbox[0] - 72.0).abs() < 1.0, "{bbox:?}");
        assert!(
            bbox[1] > 50.0 && bbox[1] < 72.0 && bbox[3] > bbox[1],
            "{bbox:?}"
        );
        // A page past the end is an empty object.
        assert_eq!(lines[1]["page"], 2);
        assert_eq!(lines[1]["blocks"].as_array().map(Vec::len), Some(0));
    }
}
