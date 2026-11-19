//! Page labels (ISO 32000-2 §12.4.2), expanded to one label per page so operations can
//! move them with their pages, then compressed back into ranges.

use std::collections::HashSet;

use pagebara_syntax::{Dict, Document, Name, Object, PdfString};

use crate::edit::{catalog_ref, update_catalog};

const MAX_DEPTH: u32 = 64;

/// The label of one page: numbering style, prefix and the number shown.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Label {
    style: Option<Name>,
    prefix: Option<PdfString>,
    number: i64,
}

impl Label {
    fn decimal(number: i64) -> Self {
        Label {
            style: Some(Name::new(b"D")),
            prefix: None,
            number,
        }
    }

    fn follows(&self, prev: &Label) -> bool {
        self.style == prev.style
            && self.prefix == prev.prefix
            && (self.style.is_none() || self.number == prev.number.wrapping_add(1))
    }
}

/// Per-page labels, or `None` when the document has no `/PageLabels`.
pub(crate) fn read(doc: &Document, count: usize) -> Option<Vec<Label>> {
    let cat = doc.catalog()?;
    let root = cat.as_dict()?.get(b"PageLabels")?;
    let mut ranges = Vec::new();
    collect(doc, root, 0, &mut HashSet::new(), &mut ranges);
    ranges.sort_by_key(|r| r.0);
    let mut out = Vec::with_capacity(count);
    let mut ri = 0;
    for i in 0..count {
        while ranges.get(ri + 1).is_some_and(|r| r.0 <= i as i64) {
            ri += 1;
        }
        let label = match ranges.get(ri) {
            Some((start, d)) if *start <= i as i64 => Label {
                style: d.name(b"S").map(Name::new),
                prefix: doc.get_in(d, b"P").as_string().map(|b| PdfString {
                    bytes: b.to_vec(),
                    hex: false,
                }),
                number: doc
                    .get_in(d, b"St")
                    .as_int()
                    .unwrap_or(1)
                    .saturating_add(i as i64 - start),
            },
            _ => Label::decimal(i as i64 + 1),
        };
        out.push(label);
    }
    Some(out)
}

impl Label {
    /// The label as shown to the user, e.g. `iv`, `A-3` or `AA`.
    pub(crate) fn text(&self) -> String {
        let mut s = self
            .prefix
            .as_ref()
            .map(|p| text_string(&p.bytes))
            .unwrap_or_default();
        let n = self.number.max(1);
        match self.style.as_ref().map(Name::as_bytes) {
            Some(b"D") => s += &n.to_string(),
            Some(b"R") => s += &roman(n),
            Some(b"r") => s += &roman(n).to_lowercase(),
            Some(b"A") => s += &letters(n),
            Some(b"a") => s += &letters(n).to_lowercase(),
            _ => {}
        }
        s
    }
}

/// Every page's label, or `None` when the document defines none (pages are then
/// numbered 1, 2, 3...).
pub fn page_labels(doc: &Document, count: usize) -> Option<Vec<String>> {
    Some(read(doc, count)?.iter().map(Label::text).collect())
}

fn roman(mut n: i64) -> String {
    const DIGITS: [(i64, &str); 13] = [
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    if n >= 10_000 {
        return n.to_string();
    }
    let mut s = String::new();
    for (v, d) in DIGITS {
        while n >= v {
            s += d;
            n -= v;
        }
    }
    s
}

/// A..Z, then AA..ZZ, AAA..: the letter repeated.
fn letters(n: i64) -> String {
    let c = char::from(b'A' + ((n - 1) % 26) as u8);
    std::iter::repeat_n(c, ((n - 1) / 26 + 1).min(1000) as usize).collect()
}

fn text_string(b: &[u8]) -> String {
    if let Some(u) = b.strip_prefix(b"\xfe\xff") {
        let units = u.as_chunks::<2>().0.iter().map(|&c| u16::from_be_bytes(c));
        return char::decode_utf16(units)
            .map(|c| c.unwrap_or('\u{fffd}'))
            .collect();
    }
    if let Some(u) = b.strip_prefix(b"\xef\xbb\xbf") {
        return String::from_utf8_lossy(u).into_owned();
    }
    b.iter()
        .map(|&c| pagebara_syntax::text::pdfdoc_char(c))
        .collect()
}

/// Labels for a document without `/PageLabels`, as viewers number such pages.
pub(crate) fn default(count: usize) -> Vec<Label> {
    (1..=count as i64).map(Label::decimal).collect()
}

fn collect(
    doc: &Document,
    node: &Object,
    depth: u32,
    seen: &mut HashSet<u32>,
    out: &mut Vec<(i64, Dict)>,
) {
    if depth > MAX_DEPTH || node.as_objref().is_some_and(|r| !seen.insert(r.num)) {
        return;
    }
    let n = doc.resolve(node);
    let Some(n) = n.as_dict() else { return };
    if let Some(a) = doc.get_in(n, b"Nums").as_array() {
        for p in a.as_chunks::<2>().0 {
            if let (Some(k), Some(d)) = (doc.resolve(&p[0]).as_int(), doc.resolve(&p[1]).as_dict())
                && k >= 0
            {
                out.push((k, d.clone()));
            }
        }
    }
    if let Some(kids) = doc.get_in(n, b"Kids").as_array() {
        for k in kids {
            collect(doc, k, depth + 1, seen, out);
        }
    }
}

/// Writes `labels` as a flat number tree, replacing the old one.
pub(crate) fn write(doc: &mut Document, labels: &[Label]) {
    let mut nums = Vec::new();
    for (i, l) in labels.iter().enumerate() {
        if i > 0 && l.follows(&labels[i - 1]) {
            continue;
        }
        let mut d = Dict::new();
        if let Some(s) = &l.style {
            d.set(b"S", s.clone());
        }
        if let Some(p) = &l.prefix {
            d.set(b"P", p.clone());
        }
        if l.number != 1 {
            d.set(b"St", l.number.max(1));
        }
        nums.push(Object::Int(i as i64));
        nums.push(d.into());
    }
    let mut tree = Dict::new();
    tree.set(b"Nums", nums);
    let existing = catalog_ref(doc)
        .and_then(|c| doc.get(c))
        .and_then(|c| c.as_dict()?.get(b"PageLabels")?.as_objref());
    match existing {
        Some(r) => doc.set(r, tree),
        None => update_catalog(doc, |_, cat| {
            cat.set(b"PageLabels", tree);
            true
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn label_text() {
        let l = |s: &[u8], p: Option<&[u8]>, n| Label {
            style: Some(Name::new(s)),
            prefix: p.map(|b| PdfString {
                bytes: b.to_vec(),
                hex: false,
            }),
            number: n,
        };
        assert_eq!(l(b"R", None, 1994).text(), "MCMXCIV");
        assert_eq!(l(b"r", Some(b"A-"), 4).text(), "A-iv");
        assert_eq!(l(b"A", None, 28).text(), "BB");
        assert_eq!(l(b"a", None, 26).text(), "z");
        assert_eq!(l(b"D", Some(b"\xfe\xff\x00x"), 7).text(), "x7");
    }
}
