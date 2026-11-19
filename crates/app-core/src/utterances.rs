//! What reading aloud says: a page's or a selection's sentences in reading order, each
//! with the boxes to highlight while it is read.

use std::ops::Range;

use pagebara_content::{Point, TextPage};

use crate::reading_order::{ItemKind, PageReading};
use crate::speech;

/// One sentence to say, with where it is on its page (boxes in page space, x0 y0 x1 y1).
#[derive(Clone, Debug, PartialEq)]
pub struct Utterance {
    pub page: usize,
    pub text: String,
    pub lang: Option<String>,
    pub boxes: Vec<[f32; 4]>,
}

/// The sentences of a page in reading order.
pub fn page_utterances(reading: &PageReading, page: usize) -> Vec<Utterance> {
    let mut out = Vec::new();
    for item in &reading.items {
        if item.kind == ItemKind::Figure {
            let text = if item.text.is_empty() {
                pagebara_i18n::tr!("Figure.")
            } else {
                pagebara_i18n::tr!("Figure: {}", item.text)
            };
            out.push(Utterance {
                page,
                text,
                lang: item.lang.clone(),
                boxes: line_boxes(item.chars.iter().map(|(_, q)| *q)),
            });
            continue;
        }
        for r in speech::sentences(&item.text) {
            let quads = item
                .chars
                .iter()
                .filter(|(c, _)| (c.start as usize) < r.end && (c.end as usize) > r.start)
                .map(|(_, q)| *q);
            out.push(Utterance {
                page,
                text: item.text[r.clone()].to_owned(),
                lang: item.lang.clone(),
                boxes: line_boxes(quads),
            });
        }
    }
    out
}

/// The sentences of selected text (character range of a page's text).
pub fn selection(page_text: &TextPage, page: usize, chars: Range<usize>) -> Vec<Utterance> {
    let mut text = String::new();
    let mut map: Vec<(Range<usize>, [Point; 4])> = Vec::new();
    // Where paragraphs start in `text`: sentences never run across them.
    let mut paragraphs = vec![0];
    let mut prev_end: Option<u32> = None;
    for c in page_text.chars.get(chars).unwrap_or_default() {
        // Characters that are not next to each other in the page text are on other lines,
        // or in other paragraphs (an empty line between).
        if let Some(e) = prev_end.filter(|&e| e != c.text.0) {
            let gap = page_text
                .text
                .get(e as usize..c.text.0 as usize)
                .unwrap_or("");
            if gap.contains("\n\n") {
                paragraphs.push(text.len());
            } else if !text.ends_with(' ') {
                text.push(' ');
            }
        }
        prev_end = Some(c.text.1);
        let start = text.len();
        text.push_str(page_text.char_text(c));
        map.push((start..text.len(), c.quad));
    }
    paragraphs.push(text.len());
    paragraphs
        .windows(2)
        .flat_map(|p| {
            speech::sentences(&text[p[0]..p[1]])
                .into_iter()
                .map(move |r| r.start + p[0]..r.end + p[0])
        })
        .map(|r| Utterance {
            page,
            text: text[r.clone()].to_owned(),
            lang: None,
            boxes: line_boxes(
                map.iter()
                    .filter(|(c, _)| c.start < r.end && c.end > r.start)
                    .map(|(_, q)| *q),
            ),
        })
        .collect()
}

/// Character boxes joined into one box per line.
pub fn line_boxes(quads: impl Iterator<Item = [Point; 4]>) -> Vec<[f32; 4]> {
    let mut out: Vec<[f32; 4]> = Vec::new();
    for q in quads {
        let x0 = q.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
        let x1 = q.iter().map(|p| p.x).fold(f32::NEG_INFINITY, f32::max);
        let y0 = q.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        let y1 = q.iter().map(|p| p.y).fold(f32::NEG_INFINITY, f32::max);
        if !(x0.is_finite() && y0.is_finite()) {
            continue;
        }
        let same_line = out.last().is_some_and(|b| {
            let mid = (y0 + y1) / 2.0;
            mid > b[1] && mid < b[3] && x0 >= b[0] - (y1 - y0)
        });
        match out.last_mut() {
            Some(b) if same_line => {
                b[0] = b[0].min(x0);
                b[1] = b[1].min(y0);
                b[2] = b[2].max(x1);
                b[3] = b[3].max(y1);
            }
            _ => out.push([x0, y0, x1, y1]),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reading_order::Item;

    fn quad(x: f32, y: f32) -> [Point; 4] {
        [
            Point::new(x, y),
            Point::new(x + 5.0, y),
            Point::new(x + 5.0, y + 10.0),
            Point::new(x, y + 10.0),
        ]
    }

    #[test]
    fn sentences_of_a_page_keep_their_boxes() {
        let text = "One two. Three.";
        let chars = text
            .char_indices()
            .map(|(i, _)| {
                let x = i as f32 * 5.0;
                // "Three." is on the next line.
                let (x, y) = if i >= 9 { (x - 45.0, 80.0) } else { (x, 100.0) };
                (i as u32..i as u32 + 1, quad(x, y))
            })
            .collect();
        let reading = PageReading {
            tagged: true,
            items: vec![
                Item {
                    kind: ItemKind::Heading(1),
                    text: text.into(),
                    lang: Some("en".into()),
                    chars,
                },
                Item {
                    kind: ItemKind::Figure,
                    text: "A cat".into(),
                    lang: None,
                    chars: Vec::new(),
                },
            ],
            links: Vec::new(),
        };
        let u = page_utterances(&reading, 3);
        let texts: Vec<&str> = u.iter().map(|u| u.text.as_str()).collect();
        assert_eq!(texts, ["One two.", "Three.", "Figure: A cat"]);
        assert_eq!(u[0].boxes, [[0.0, 100.0, 40.0, 110.0]]);
        assert_eq!(u[1].boxes, [[0.0, 80.0, 30.0, 90.0]]);
        assert_eq!(u[0].lang.as_deref(), Some("en"));
        assert!(u.iter().all(|u| u.page == 3));
    }
}
