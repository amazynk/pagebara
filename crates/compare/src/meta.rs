//! Differences beside the page content: annotations, document information, form values.

use pagebara_pages::{Page, PageTree};
use pagebara_syntax::{Document, Object};

use crate::text::{Box4, shown};

/// An annotation as compared: kind, place and text.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Note {
    pub subtype: String,
    pub rect: Box4,
    pub contents: String,
}

fn text(doc: &Document, o: &Object) -> String {
    doc.resolve(o)
        .as_string()
        .map(pagebara_stamp_text)
        .unwrap_or_default()
}

/// PDF text string to text (UTF-16 with BOM or PDFDocEncoding).
pub(crate) fn pagebara_stamp_text(b: &[u8]) -> String {
    if let Some(u) = b.strip_prefix(&[0xfe, 0xff]) {
        let units: Vec<u16> = u
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_be_bytes(*c))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    if let Some(u) = b.strip_prefix(&[0xef, 0xbb, 0xbf]) {
        return String::from_utf8_lossy(u).into_owned();
    }
    b.iter()
        .map(|&c| pagebara_syntax::text::pdfdoc_char(c))
        .collect()
}

pub(crate) fn notes(doc: &Document, p: &Page) -> Vec<Note> {
    let Some(d) = doc.get(p.r).and_then(|o| o.as_dict().cloned()) else {
        return Vec::new();
    };
    let to_shown = shown(p);
    doc.get_in(&d, b"Annots")
        .as_array()
        .unwrap_or_default()
        .iter()
        .filter_map(|o| {
            let a = doc.resolve(o);
            let a = a.as_dict()?;
            let subtype = String::from_utf8_lossy(a.name(b"Subtype")?).into_owned();
            if subtype == "Popup" {
                return None;
            }
            let r: Vec<f64> = doc
                .get_in(a, b"Rect")
                .as_array()?
                .iter()
                .filter_map(|v| doc.resolve(v).as_f64())
                .collect();
            if r.len() != 4 {
                return None;
            }
            let p0 = to_shown(r[0], r[1]);
            let p1 = to_shown(r[2], r[3]);
            let contents = a.get(b"Contents").map(|c| text(doc, c)).unwrap_or_default();
            Some(Note {
                subtype,
                rect: [
                    p0.0.min(p1.0),
                    p0.1.min(p1.1),
                    p0.0.max(p1.0),
                    p0.1.max(p1.1),
                ],
                contents,
            })
        })
        .collect()
}

fn near(a: &Box4, b: &Box4) -> bool {
    a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 2.0)
}

/// Annotation changes between two paired pages: (kind, rect on old, rect on new, text).
pub(crate) fn note_changes(
    old: &[Note],
    new: &[Note],
) -> Vec<(&'static str, Option<Box4>, Option<Box4>, String)> {
    let mut out = Vec::new();
    let mut used = vec![false; new.len()];
    let mut unmatched_old = Vec::new();
    for o in old {
        if let Some(k) = (0..new.len())
            .find(|&k| !used[k] && new[k] == *o)
            .or_else(|| {
                (0..new.len()).find(|&k| {
                    !used[k]
                        && new[k].subtype == o.subtype
                        && near(&new[k].rect, &o.rect)
                        && new[k].contents == o.contents
                })
            })
        {
            used[k] = true;
        } else {
            unmatched_old.push(o);
        }
    }
    for o in unmatched_old {
        // Same kind and place, other text: changed; same kind and text elsewhere: moved.
        let changed = (0..new.len()).find(|&k| {
            !used[k]
                && new[k].subtype == o.subtype
                && (near(&new[k].rect, &o.rect) || new[k].contents == o.contents)
        });
        match changed {
            Some(k) => {
                used[k] = true;
                let n = &new[k];
                let what = if n.contents != o.contents {
                    // TRANSLATORS: {} is the annotation's PDF type, like Highlight or Text
                    pagebara_i18n::tr!(
                        "{} note changed: “{}” → “{}”",
                        o.subtype,
                        o.contents,
                        n.contents
                    )
                } else {
                    pagebara_i18n::tr!("{} moved", o.subtype)
                };
                out.push(("changed", Some(o.rect), Some(n.rect), what));
            }
            None => out.push((
                "removed",
                Some(o.rect),
                None,
                pagebara_i18n::tr!("{} removed{}", o.subtype, quoted(&o.contents)),
            )),
        }
    }
    for (k, n) in new.iter().enumerate() {
        if !used[k] {
            out.push((
                "added",
                None,
                Some(n.rect),
                pagebara_i18n::tr!("{} added{}", n.subtype, quoted(&n.contents)),
            ));
        }
    }
    out
}

fn quoted(s: &str) -> String {
    if s.is_empty() {
        String::new()
    } else {
        format!(": \u{201c}{}\u{201d}", crate::shorten(s, 80))
    }
}

/// Document information entries and page sizes that differ: (what, old, new).
pub(crate) fn metadata(a: &Document, b: &Document) -> Vec<(String, String, String)> {
    let info = |d: &Document| {
        d.trailer()
            .get(b"Info")
            .and_then(|o| d.resolve(o).as_dict().cloned())
            .unwrap_or_default()
    };
    let (ia, ib) = (info(a), info(b));
    let mut out = Vec::new();
    // The keys are shown through `pagebara_i18n::translate`.
    for key in [
        pagebara_i18n::tr_noop!("Title"),
        pagebara_i18n::tr_noop!("Author"),
        pagebara_i18n::tr_noop!("Subject"),
        pagebara_i18n::tr_noop!("Keywords"),
        // TRANSLATORS: a document property: the application that made the document
        pagebara_i18n::tr_noop!("Creator"),
        // TRANSLATORS: the software that wrote the PDF file
        pagebara_i18n::tr_noop!("Producer"),
    ] {
        let va = ia
            .get(key.as_bytes())
            .map(|o| text(a, o))
            .unwrap_or_default();
        let vb = ib
            .get(key.as_bytes())
            .map(|o| text(b, o))
            .unwrap_or_default();
        if va != vb {
            out.push((key.to_owned(), va, vb));
        }
    }
    let (ta, tb) = (PageTree::load(a), PageTree::load(b));
    if ta.len() != tb.len() {
        let pages = pagebara_i18n::tr_noop!("Pages");
        out.push((pages.into(), ta.len().to_string(), tb.len().to_string()));
    }
    out
}

/// Form fields whose values differ, or that only one document has: (name, old, new).
pub(crate) fn forms(a: &Document, b: &Document) -> Vec<(String, Option<String>, Option<String>)> {
    let value = |f: &pagebara_forms::Field| f.value.display();
    let (fa, fb) = (pagebara_forms::Form::load(a), pagebara_forms::Form::load(b));
    let mut out = Vec::new();
    for f in &fa.fields {
        match fb.fields.iter().find(|g| g.name == f.name) {
            Some(g) if value(g) != value(f) => {
                out.push((f.name.clone(), Some(show(&f.value)), Some(show(&g.value))))
            }
            Some(_) => {}
            None => out.push((f.name.clone(), Some(show(&f.value)), None)),
        }
    }
    for g in &fb.fields {
        if !fa.fields.iter().any(|f| f.name == g.name) {
            out.push((g.name.clone(), None, Some(show(&g.value))));
        }
    }
    out
}

fn show(v: &pagebara_forms::FieldValue) -> String {
    crate::shorten(&v.display(), 60)
}
