//! A page's content in reading order for assistive technology: screen reader text, reading
//! aloud and the reading view. Tagged pages follow their structure tree (ISO 32000-2
//! §14.7); other pages, and tagged pages the tree has nothing for, follow the layout
//! analysis of their text.

use std::cell::OnceCell;
use std::ops::Range;

use pagebara_annot::{Dest, DestPage, Kind, LinkAction};
use pagebara_content::{Interpreter, Point, TextPage};
use pagebara_pages::PageTree;
use pagebara_syntax::{Document, ObjRef};
use pagebara_tagged::StructTree;

pub use pagebara_tagged::ItemKind;

use crate::{annots, pages, reflow, text};

/// A block of the page: a heading, paragraph, list item, figure, …
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    pub kind: ItemKind,
    /// The text to present (for a figure, its alternate text; may be empty).
    pub text: String,
    pub lang: Option<String>,
    /// Boxes of the text in page space (PDF user space, y up), one per character, in text
    /// order, with each character's byte range in `text`: for highlighting the sentence
    /// being read. Spaces that join lines have none; empty for figures without text.
    pub chars: Vec<(Range<u32>, [Point; 4])>,
}

/// A link on the page.
#[derive(Clone, Debug, PartialEq)]
pub struct PageLink {
    /// The page text under the link.
    pub text: String,
    /// Where it goes, in words: "page 4", the URL, "another file: report.pdf".
    pub target: String,
    /// x0, y0, x1, y1 in page space.
    pub rect: [f32; 4],
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PageReading {
    /// The items come from the structure tree.
    pub tagged: bool,
    pub items: Vec<Item>,
    /// In tag order when tagged (links not in the tree last), else top to bottom, left to
    /// right.
    pub links: Vec<PageLink>,
}

/// The reading of `page`. `text` is the page's layout-analysed text (what
/// `Session::text.page(i)` gives). A tagged page is interpreted again for the text of its
/// marked content, which takes a few milliseconds for usual pages: callers wanting to stay
/// responsive on heavy pages run this on a worker (see also `Session::reading`).
pub fn page_reading(
    doc: &Document,
    tree: Option<&StructTree>,
    page: ObjRef,
    text: &TextPage,
) -> PageReading {
    let pages = OnceCell::new();
    let labels = OnceCell::new();
    reading(doc, tree, page, text, &|d| {
        let t = pages.get_or_init(|| PageTree::open(doc));
        let i = match d {
            DestPage::Ref(r) => t.index_of(r)?,
            DestPage::Index(i) => i as usize,
        };
        let label = labels
            .get_or_init(|| pagebara_pages::page_labels(doc, t.len()))
            .as_ref()
            .and_then(|l| l.get(i).cloned());
        Some(label.unwrap_or_else(|| (i + 1).to_string()))
    })
}

/// [`page_reading`] with `page_name` naming a link's target page (its label).
pub(crate) fn reading(
    doc: &Document,
    tree: Option<&StructTree>,
    page: ObjRef,
    text: &TextPage,
    page_name: &dyn Fn(DestPage) -> Option<String>,
) -> PageReading {
    let (items, order, tagged) = match tree.and_then(|t| tagged_items(doc, t, page)) {
        Some((items, order)) => (items, order, true),
        None => (layout_items(text, doc_lang(doc)), Vec::new(), false),
    };
    PageReading {
        tagged,
        items,
        links: links(doc, page, text, &order, page_name),
    }
}

/// The items of a tagged page and the objects (annotations) in tag order; `None` when the
/// tree has nothing for the page. Interprets the page.
pub(crate) fn tagged_items(
    doc: &Document,
    tree: &StructTree,
    page: ObjRef,
) -> Option<(Vec<Item>, Vec<ObjRef>)> {
    if !tree.has_page(page) {
        return None;
    }
    let o = doc.get(page)?;
    let d = o.as_dict()?;
    let list = Interpreter::new(doc).page(d, &pages::resources(doc, d));
    let content = tree.page_content(doc, page, &list);
    let items: Vec<Item> = content
        .blocks
        .into_iter()
        .map(|b| {
            let (text, chars) = normalise(&b.text, &b.chars);
            Item {
                kind: b.kind,
                text,
                lang: b.lang,
                chars,
            }
        })
        .filter(|i| i.kind == ItemKind::Figure || !i.text.is_empty())
        .collect();
    (!items.is_empty()).then_some((items, content.objects))
}

/// One paragraph per block of the layout analysis, larger type as headings, page numbers
/// dropped.
fn layout_items(text: &TextPage, lang: Option<String>) -> Vec<Item> {
    let mut paras: Vec<(Item, f32, usize)> = Vec::new();
    for b in &text.blocks {
        let lines = &text.lines[b.lines.0 as usize..b.lines.1 as usize];
        let (Some(first), Some(last)) = (lines.first(), lines.last()) else {
            continue;
        };
        let (c0, c1) = (first.chars.0 as usize, last.chars.1 as usize);
        if c0 >= c1 {
            continue;
        }
        let start = text.chars[c0].text.0;
        let end = text.chars[c1 - 1].text.1.max(start);
        let chars: Vec<(Range<u32>, [Point; 4])> = text.chars[c0..c1]
            .iter()
            .filter(|c| c.text.0 < c.text.1)
            .map(|c| (c.text.0 - start..c.text.1 - start, c.quad))
            .collect();
        let (t, chars) = normalise(&text.text[start as usize..end as usize], &chars);
        if t.is_empty() || reflow::is_page_number(&t) {
            continue;
        }
        let mut sizes: Vec<f32> = lines
            .iter()
            .filter(|l| !text.line_text(l).trim().is_empty())
            .map(reflow::line_size)
            .collect();
        let item = Item {
            kind: ItemKind::Paragraph,
            text: t,
            lang: lang.clone(),
            chars,
        };
        paras.push((item, reflow::median(&mut sizes), sizes.len()));
    }
    let body = reflow::body_size(paras.iter().map(|p| (p.1, p.2)));
    paras
        .into_iter()
        .map(|(mut item, size, lines)| {
            if let Some(l) = reflow::heading_level(size, lines, body) {
                item.kind = ItemKind::Heading(l);
            }
            item
        })
        .collect()
}

/// A character's byte range in a text and its box.
type CharBox = (Range<u32>, [Point; 4]);

/// Text with lines joined as the reading view joins them (hyphenated words made whole)
/// and white space runs as one space, and the character boxes moved along with it.
fn normalise(raw: &str, chars: &[CharBox]) -> (String, Vec<CharBox>) {
    const GONE: u32 = u32::MAX;
    let mut out = String::with_capacity(raw.len());
    // Where each byte of `raw` that starts a kept character went.
    let mut map = vec![GONE; raw.len()];
    let mut last = None;
    let mut at = 0;
    for line in raw.split('\n') {
        let base = at + line.len() - line.trim_start().len();
        at += line.len() + 1;
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        if !out.is_empty() {
            if reflow::joins_hyphenated(&out, t) {
                out.pop();
                if let Some(h) = last {
                    map[h] = GONE;
                }
            } else {
                out.push(' ');
            }
        }
        let mut space = false;
        for (i, c) in t.char_indices() {
            if c.is_whitespace() {
                if !space {
                    map[base + i] = out.len() as u32;
                    out.push(' ');
                }
                space = true;
            } else {
                space = false;
                map[base + i] = out.len() as u32;
                out.push(c);
                last = Some(base + i);
            }
        }
    }
    let span = |r: &Range<u32>| {
        let s = raw.get(r.start as usize..r.end as usize)?;
        let (mut lo, mut hi) = (GONE, 0);
        for (i, c) in s.char_indices() {
            let m = map[r.start as usize + i];
            if m != GONE {
                lo = lo.min(m);
                hi = hi.max(
                    m + if c.is_whitespace() {
                        1
                    } else {
                        c.len_utf8() as u32
                    },
                );
            }
        }
        (lo != GONE).then_some(lo..hi)
    };
    let chars = chars
        .iter()
        .filter_map(|(r, q)| Some((span(r)?, *q)))
        .collect();
    (out, chars)
}

/// The catalog's `/Lang`.
fn doc_lang(doc: &Document) -> Option<String> {
    let cat = doc.catalog()?;
    let lang = doc.get_in(cat.as_dict()?, b"Lang");
    let s = text::decode(lang.as_string()?);
    (!s.is_empty()).then_some(s)
}

/// The page's links, ordered by `order` (annotations in tag order) first, then by position.
fn links(
    doc: &Document,
    page: ObjRef,
    text: &TextPage,
    order: &[ObjRef],
    page_name: &dyn Fn(DestPage) -> Option<String>,
) -> Vec<PageLink> {
    let mut found = Vec::new();
    for a in annots::on_page(doc, page) {
        let Kind::Link {
            action: Some(action),
            quads,
        } = &a.kind
        else {
            continue;
        };
        let rect = bounds(a.rect);
        let areas: Vec<[f32; 4]> = if quads.is_empty() {
            vec![rect]
        } else {
            quads
                .iter()
                .filter_map(|q| pagebara_annot::Rect::bounding(q.0))
                .map(bounds)
                .collect()
        };
        let tag =
            a.id.and_then(|id| order.iter().position(|o| *o == id))
                .unwrap_or(usize::MAX);
        let link = PageLink {
            text: text_under(text, &areas),
            target: target(doc, action, page_name),
            rect,
        };
        found.push((tag, link));
    }
    found.sort_by_key(|(tag, l)| (*tag, -(l.rect[3] / 4.0).round() as i64, l.rect[0] as i64));
    found.into_iter().map(|(_, l)| l).collect()
}

fn bounds(r: pagebara_annot::Rect) -> [f32; 4] {
    [
        r.x0.min(r.x1) as f32,
        r.y0.min(r.y1) as f32,
        r.x0.max(r.x1) as f32,
        r.y0.max(r.y1) as f32,
    ]
}

/// The text of the characters whose centres are in one of `areas`.
fn text_under(text: &TextPage, areas: &[[f32; 4]]) -> String {
    let mut out = String::new();
    let mut prev_end = None;
    for c in &text.chars {
        if c.text.0 == c.text.1 {
            continue;
        }
        let b = c.bbox();
        let (x, y) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
        if !areas
            .iter()
            .any(|a| x >= a[0] && x <= a[2] && y >= a[1] && y <= a[3])
        {
            continue;
        }
        if prev_end.is_some_and(|e| e != c.text.0) {
            out.push(' ');
        }
        out.push_str(text.char_text(c));
        prev_end = Some(c.text.1);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn target(
    doc: &Document,
    action: &LinkAction,
    page_name: &dyn Fn(DestPage) -> Option<String>,
) -> String {
    match action {
        LinkAction::Uri(u) => u.clone(),
        LinkAction::GoTo(d) => {
            let dest = match d {
                Dest::Explicit(d) => Some(*d),
                Dest::Named(n) => pagebara_annot::named_destination(doc, n),
            };
            dest.and_then(|d| page_name(d.page)).map_or_else(
                // TRANSLATORS: where a link goes
                || pagebara_i18n::tr!("a place in this document"),
                // TRANSLATORS: where a link goes
                |p| pagebara_i18n::tr!("page {}", p),
            )
        }
        // TRANSLATORS: where a link goes
        LinkAction::GoToR { file, .. } => pagebara_i18n::tr!("another file: {}", file),
        // TRANSLATORS: where a link goes
        LinkAction::Launch { file, .. } => pagebara_i18n::tr!("a file: {}", file),
        // TRANSLATORS: where a link goes; the action's PDF name follows
        LinkAction::Other(s) => pagebara_i18n::tr!("an action: {}", s),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::Session;

    fn session(bytes: Vec<u8>) -> Session {
        Session::new(None, Document::open(bytes).unwrap())
    }

    #[test]
    fn tagged_order_wins_over_content_and_layout_order() {
        let s = session(crate::test_pdf::tagged());
        let r = s.reading(0);
        assert!(r.tagged);
        let items: Vec<(ItemKind, &str)> =
            r.items.iter().map(|i| (i.kind, i.text.as_str())).collect();
        use ItemKind::*;
        assert_eq!(
            items,
            [
                (Heading(1), "Tagged title"),
                (Paragraph, "First paragraph of text that is hyphenated."),
                (ListItem, "Apples"),
                (Figure, "A red circle"),
                (Heading(5), "Deep heading"),
                (Paragraph, "Go to page two"),
                (Paragraph, "Note at the top, tagged last."),
            ]
        );
        assert!(r.items.iter().all(|i| i.lang.as_deref() == Some("en-GB")));
        // The layout analysis reads the note first.
        assert!(s.text.page(0).text.starts_with("Note at the top"));
        // Character boxes map into the joined text.
        let p = &r.items[1];
        for (range, q) in &p.chars {
            let t = &p.text[range.start as usize..range.end as usize];
            assert!(!t.is_empty() && !t.contains('-'));
            assert!(q[0].x >= 72.0 && q[0].x < 400.0 && q[0].y > 690.0 && q[0].y < 725.0);
        }
        let hy = p.text.find("hyphenated").unwrap() as u32;
        let a = p.chars.iter().find(|c| c.0.start == hy + 5).unwrap();
        let b = p.chars.iter().find(|c| c.0.start == hy + 6).unwrap();
        assert_eq!(
            (
                &p.text[a.0.start as usize..][..1],
                &p.text[b.0.start as usize..][..1]
            ),
            ("n", "a")
        );
        assert!(
            a.1[0].y > b.1[0].y,
            "\"hyphen-\" is on the line above \"ated\""
        );
        // The figure's alternate text has one box: what it draws.
        let fig = &r.items[3].chars;
        assert_eq!(fig.len(), 1);
        assert_eq!((fig[0].1[0].x, fig[0].1[0].y), (72.0, 560.0));
        // Links: the tagged one first.
        let links: Vec<(&str, &str)> = r
            .links
            .iter()
            .map(|l| (l.text.as_str(), l.target.as_str()))
            .collect();
        assert_eq!(
            links,
            [
                ("Go to page two", "page 2"),
                ("Note at the top, tagged last.", "https://example.org/")
            ]
        );
        assert_eq!(r.links[0].rect, [70.0, 495.0, 160.0, 512.0]);
        // Cached until the document changes; page 2 is tagged too.
        assert!(std::sync::Arc::ptr_eq(&r, &s.reading(0)));
        let r2 = s.reading(1);
        assert_eq!(r2.items.len(), 1);
        assert_eq!(r2.items[0].text, "Second page");
        // The free function agrees.
        let tree = s.struct_tree();
        let free = page_reading(
            s.doc(),
            tree.as_deref(),
            s.page_ref(0).unwrap(),
            &s.text.page(0),
        );
        assert_eq!(free, *r);
    }

    #[test]
    fn untagged_pages_read_in_layout_order_with_headings() {
        let content = "BT /F1 24 Tf 72 760 Td (Big heading) Tj ET\n\
             BT /F1 12 Tf 14 TL 72 700 Td (A paragraph of body text, first line,) Tj T* \
             (and its second line with exam-) Tj T* (ples.) Tj ET\n\
             BT /F1 12 Tf 14 TL 72 600 Td (Another paragraph of body text here.) Tj ET\n\
             BT /F1 9 Tf 290 30 Td (- 3 -) Tj ET";
        let stream = format!("<</Length {}>>stream\n{content}\nendstream", content.len());
        let s = session(crate::test_pdf::classic(
            &[
                "<</Type/Catalog/Pages 2 0 R/Lang(de)>>",
                "<</Type/Pages/Kids[3 0 R]/Count 1>>",
                "<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>/Annots[7 0 R]>>",
                &stream,
                "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>",
                "<</Title(Plain)>>",
                "<</Type/Annot/Subtype/Link/Rect[70 595 300 612]/A<</S/GoToR/F(other.pdf)/D[0/Fit]>>>>",
            ],
            1,
        ));
        assert!(s.struct_tree().is_none());
        let r = s.reading(0);
        assert!(!r.tagged);
        let items: Vec<(ItemKind, &str)> =
            r.items.iter().map(|i| (i.kind, i.text.as_str())).collect();
        assert_eq!(
            items,
            [
                (ItemKind::Heading(1), "Big heading"),
                (
                    ItemKind::Paragraph,
                    "A paragraph of body text, first line, and its second line with examples."
                ),
                (ItemKind::Paragraph, "Another paragraph of body text here."),
            ]
        );
        assert_eq!(r.items[0].lang.as_deref(), Some("de"));
        for i in &r.items {
            assert_eq!(i.chars.first().unwrap().0.start, 0);
            for (range, _) in &i.chars {
                assert!(
                    i.text
                        .get(range.start as usize..range.end as usize)
                        .is_some()
                );
            }
        }
        assert_eq!(r.links.len(), 1);
        assert_eq!(r.links[0].target, "another file: other.pdf");
        assert_eq!(r.links[0].text, "Another paragraph of body text here.");
    }

    #[test]
    fn whitespace_and_hyphens_normalise_with_boxes() {
        let q = [Point { x: 0.0, y: 0.0 }; 4];
        let raw = "  ab  c-\n\ndef ";
        let chars: Vec<_> = raw
            .char_indices()
            .map(|(i, c)| (i as u32..(i + c.len_utf8()) as u32, q))
            .collect();
        let (t, c) = normalise(raw, &chars);
        assert_eq!(t, "ab cdef");
        let ranges: Vec<_> = c.iter().map(|c| c.0.clone()).collect();
        assert_eq!(ranges, [0..1, 1..2, 2..3, 3..4, 4..5, 5..6, 6..7]);
    }
}
