//! The reading view: page text re-flowed into one column. Tagged pages follow their
//! structure tree; other pages are built from the extracted text in reading order: its
//! paragraphs become blocks, larger type becomes headings, and images are marked with a
//! placeholder where they are on the page.

use pagebara_content::{Cmd, DisplayList, Interpreter, Point, Rect, TextLine, TextPage};
use pagebara_syntax::{Document, ObjRef};
use pagebara_tagged::{ItemKind, StructTree};

use crate::pages;
use crate::reading_order;

/// Type this much larger than the body text is a heading.
const HEADING: f32 = 1.2;
/// Headings have at most this many lines.
const HEADING_LINES: usize = 3;
/// Images smaller than this (points, either side) are decoration.
const MIN_IMAGE: f32 = 36.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockKind {
    /// Level 1 (largest) to 3.
    Heading(u8),
    Paragraph,
    /// An image; its text is the alternate description, when the page is tagged.
    Image,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Block {
    pub page: usize,
    pub kind: BlockKind,
    pub text: String,
}

/// A paragraph of a page before headings are known.
#[derive(Clone, Debug)]
struct Para {
    text: String,
    size: f32,
    lines: usize,
    /// Its top in page space (y up).
    top: f32,
}

/// One page's paragraphs and image tops, to be joined with the others by [`build`].
#[derive(Clone, Debug, Default)]
pub struct PageFlow {
    paras: Vec<Para>,
    images: Vec<f32>,
    /// The blocks of a tagged page, from its structure tree.
    tagged: Vec<(BlockKind, String)>,
}

impl PageFlow {
    pub fn new(text: &TextPage, images: Vec<f32>) -> Self {
        let mut paras = Vec::new();
        for b in &text.blocks {
            let lines = &text.lines[b.lines.0 as usize..b.lines.1 as usize];
            let mut out = String::new();
            let mut sizes: Vec<f32> = Vec::new();
            for l in lines {
                let t = text.line_text(l).trim();
                if t.is_empty() {
                    continue;
                }
                join_line(&mut out, t);
                sizes.push(line_size(l));
            }
            if out.is_empty() || is_page_number(&out) {
                continue;
            }
            paras.push(Para {
                text: out,
                size: median(&mut sizes),
                lines: sizes.len(),
                top: b.bbox.y1,
            });
        }
        PageFlow {
            paras,
            images,
            tagged: Vec::new(),
        }
    }

    /// The flow of `page` (whose layout-analysed text is `text`): from the structure tree
    /// when the page is tagged, else from the text. Interprets the page.
    pub fn of_page(
        doc: &Document,
        tree: Option<&StructTree>,
        page: ObjRef,
        text: &TextPage,
    ) -> Self {
        if let Some((items, _)) = tree.and_then(|t| reading_order::tagged_items(doc, t, page)) {
            let tagged = items
                .into_iter()
                .map(|i| match i.kind {
                    ItemKind::Heading(l) => (BlockKind::Heading(l.min(3)), i.text),
                    ItemKind::Figure => (BlockKind::Image, i.text),
                    ItemKind::ListItem => (BlockKind::Paragraph, bulleted(i.text)),
                    _ => (BlockKind::Paragraph, i.text),
                })
                .collect();
            return PageFlow {
                tagged,
                ..Default::default()
            };
        }
        PageFlow::new(text, image_tops(doc, page))
    }

    pub fn is_empty(&self) -> bool {
        self.paras.is_empty() && self.images.is_empty() && self.tagged.is_empty()
    }
}

/// A list item's text with a bullet, unless it starts with its own label.
fn bulleted(text: String) -> String {
    let labelled = text.split_whitespace().next().is_some_and(|w| {
        w.chars().all(|c| !c.is_alphanumeric())
            || w.len() <= 4 && (w.ends_with('.') || w.ends_with(')'))
    });
    if labelled {
        text
    } else {
        format!("• {text}")
    }
}

/// The size of a line's type: its height across the writing direction.
pub(crate) fn line_size(l: &TextLine) -> f32 {
    if l.vertical {
        l.bbox.width()
    } else {
        l.bbox.height()
    }
}

/// Appends a line to a paragraph, joining words hyphenated across lines.
pub(crate) fn join_line(out: &mut String, line: &str) {
    if out.is_empty() {
        out.push_str(line);
        return;
    }
    if joins_hyphenated(out, line) {
        out.pop();
    } else {
        out.push(' ');
    }
    out.push_str(line);
}

/// Whether `out` ends in a hyphen that joins it to the next line, `line`.
pub(crate) fn joins_hyphenated(out: &str, line: &str) -> bool {
    let next_lower = line.chars().next().is_some_and(char::is_lowercase);
    next_lower && out.ends_with('-') && !out.ends_with(" -")
}

pub(crate) fn is_page_number(s: &str) -> bool {
    let s = s.trim();
    s.len() <= 12
        && s.chars().all(|c| {
            c.is_ascii_digit() || c.is_whitespace() || "-–/.ivxlcIVXLCpageofPageOf".contains(c)
        })
        && s.chars()
            .any(|c| c.is_ascii_digit() || "ivxIVX".contains(c))
}

pub(crate) fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(f32::total_cmp);
    v[v.len() / 2]
}

/// The body text size: the most common paragraph size (of `(size, lines)`), weighted by
/// lines.
pub(crate) fn body_size(paras: impl IntoIterator<Item = (f32, usize)>) -> f32 {
    let mut sizes: Vec<(f32, usize)> = Vec::new();
    for (size, lines) in paras {
        let s = (size * 2.0).round() / 2.0;
        match sizes.iter_mut().find(|(v, _)| *v == s) {
            Some((_, n)) => *n += lines,
            None => sizes.push((s, lines)),
        }
    }
    sizes
        .iter()
        .max_by(|a, b| a.1.cmp(&b.1).then(b.0.total_cmp(&a.0)))
        .map_or(0.0, |s| s.0)
}

/// The heading level (1 to 3) of a paragraph of type `size` and `lines` lines, given the
/// body size.
pub(crate) fn heading_level(size: f32, lines: usize, body: f32) -> Option<u8> {
    if body <= 0.0 || size < body * HEADING || lines > HEADING_LINES {
        return None;
    }
    let r = size / body;
    Some(if r >= 1.8 {
        1
    } else if r >= 1.4 {
        2
    } else {
        3
    })
}

/// The blocks of `pages` (page index and flow) in reading order.
pub fn build(pages: &[(usize, PageFlow)]) -> Vec<Block> {
    let body = body_size(
        pages
            .iter()
            .flat_map(|(_, f)| f.paras.iter().map(|p| (p.size, p.lines))),
    );
    let mut out = Vec::new();
    for (page, f) in pages {
        out.extend(f.tagged.iter().map(|(kind, text)| Block {
            page: *page,
            kind: *kind,
            text: text.clone(),
        }));
        let mut images = f.images.clone();
        images.sort_by(|a, b| b.total_cmp(a));
        let mut images = images.into_iter().peekable();
        for p in &f.paras {
            while images.next_if(|&top| top >= p.top).is_some() {
                out.push(image(*page));
            }
            let kind = heading_level(p.size, p.lines, body)
                .map_or(BlockKind::Paragraph, BlockKind::Heading);
            out.push(Block {
                page: *page,
                kind,
                text: p.text.clone(),
            });
        }
        out.extend(images.map(|_| image(*page)));
    }
    out
}

fn image(page: usize) -> Block {
    Block {
        page,
        kind: BlockKind::Image,
        text: String::new(),
    }
}

/// Tops (page space) of the images drawn on a page, large ones only.
pub fn image_tops(doc: &Document, page: ObjRef) -> Vec<f32> {
    let Some(o) = doc.get(page) else {
        return Vec::new();
    };
    let Some(d) = o.as_dict() else {
        return Vec::new();
    };
    let res = pages::resources(doc, d);
    let list = Interpreter::new(doc).page(d, &res);
    tops_in(&list)
}

fn tops_in(list: &DisplayList) -> Vec<f32> {
    list.cmds
        .iter()
        .filter_map(|c| match c {
            Cmd::Image { ctm, .. } => {
                let m = list.matrix(*ctm);
                let unit = [
                    Point { x: 0.0, y: 0.0 },
                    Point { x: 1.0, y: 0.0 },
                    Point { x: 1.0, y: 1.0 },
                    Point { x: 0.0, y: 1.0 },
                ];
                let r = Rect::from_points(&unit.map(|p| m.apply(p)));
                (r.width() >= MIN_IMAGE && r.height() >= MIN_IMAGE).then_some(r.y1)
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn para(text: &str, size: f32, lines: usize, top: f32) -> Para {
        Para {
            text: text.into(),
            size,
            lines,
            top,
        }
    }

    #[test]
    fn headings_by_size_and_images_in_order() {
        let p1 = PageFlow {
            paras: vec![
                para("Annual report", 24.0, 1, 780.0),
                para("Body one", 11.0, 6, 700.0),
                para("Results", 16.0, 1, 500.0),
                para("Body two", 11.0, 4, 450.0),
            ],
            images: vec![600.0],
            tagged: Vec::new(),
        };
        let p2 = PageFlow {
            paras: vec![para("Body three", 11.0, 8, 780.0)],
            images: vec![100.0],
            tagged: Vec::new(),
        };
        let b = build(&[(0, p1), (1, p2)]);
        let kinds: Vec<BlockKind> = b.iter().map(|b| b.kind).collect();
        use BlockKind::*;
        assert_eq!(
            kinds,
            [
                Heading(1),
                Paragraph,
                Image,
                Heading(2),
                Paragraph,
                Paragraph,
                Image
            ]
        );
        assert_eq!(b[6].page, 1);
    }

    #[test]
    fn joins_lines_and_drops_page_numbers() {
        let mut s = String::new();
        join_line(&mut s, "A long exam-");
        join_line(&mut s, "ple of text -");
        join_line(&mut s, "and more");
        assert_eq!(s, "A long example of text - and more");
        assert!(is_page_number("12"));
        assert!(is_page_number("Page 3 of 4"));
        assert!(is_page_number("iv"));
        assert!(!is_page_number("Results"));
        assert_eq!(bulleted("Milk".into()), "• Milk");
        assert_eq!(bulleted("• Milk".into()), "• Milk");
        assert_eq!(bulleted("2. Eggs".into()), "2. Eggs");
    }

    #[test]
    fn real_text_flows() {
        let doc = Document::open(crate::test_pdf::text(&[
            "First line of a para-",
            "graph that goes on",
        ]))
        .unwrap();
        let s = crate::session::Session::new(None, doc);
        let flow = PageFlow::new(&s.text.page(0), Vec::new());
        let b = build(&[(0, flow)]);
        assert_eq!(b.len(), 1);
        assert_eq!(b[0].text, "First line of a paragraph that goes on");
    }

    #[test]
    fn tagged_pages_follow_the_structure_tree() {
        let doc = Document::open(crate::test_pdf::tagged()).unwrap();
        let s = crate::session::Session::new(None, doc);
        let tree = s.struct_tree();
        let page = s.page_ref(0).unwrap();
        let flow = PageFlow::of_page(s.doc(), tree.as_deref(), page, &s.text.page(0));
        let b: Vec<(BlockKind, String)> = build(&[(0, flow)])
            .into_iter()
            .map(|b| (b.kind, b.text))
            .collect();
        use BlockKind::*;
        assert_eq!(
            b,
            [
                (Heading(1), "Tagged title".into()),
                (
                    Paragraph,
                    "First paragraph of text that is hyphenated.".into()
                ),
                (Paragraph, "• Apples".into()),
                (Image, "A red circle".into()),
                (Heading(3), "Deep heading".into()),
                (Paragraph, "Go to page two".into()),
                (Paragraph, "Note at the top, tagged last.".into()),
            ]
        );
    }
}
