//! Grey conversion of content: the RGB and CMYK colour operators (`rg`, `RG`, `k`, `K`)
//! become grey ones. Images are converted in [`crate::images`]; shadings and colour spaces
//! set with `cs` stay as they are.

use std::io::Write as _;

use pagebara_content::{ContentParser, Op};
use pagebara_syntax::{Dict, Document, Object, Stream};

use crate::Report;

fn luma(r: f32, g: f32, b: f32) -> f32 {
    0.299 * r + 0.587 * g + 0.114 * b
}

/// The content with colour operators turned grey, or `None` when nothing changed.
pub(crate) fn rewrite(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len());
    let mut last = 0;
    let mut changed = false;
    let mut p = ContentParser::new(data);
    loop {
        let start = p.pos();
        let Some(i) = p.next() else { break };
        let v = match i.op {
            Op::FillRgb | Op::StrokeRgb => i.numbers::<3>().map(|[r, g, b]| luma(r, g, b)),
            Op::FillCmyk | Op::StrokeCmyk => i
                .numbers::<4>()
                .map(|[c, m, y, k]| 1.0 - (luma(c, m, y) + k).min(1.0)),
            _ => None,
        };
        let Some(v) = v else { continue };
        let op = if matches!(i.op, Op::FillRgb | Op::FillCmyk) {
            "g"
        } else {
            "G"
        };
        out.extend_from_slice(&data[last..start]);
        let _ = write!(
            out,
            " {} {op}",
            (v.clamp(0.0, 1.0) * 1000.0).round() / 1000.0
        );
        last = i.end;
        changed = true;
    }
    if !changed {
        return None;
    }
    out.extend_from_slice(&data[last..]);
    Some(out)
}

fn flate_stream(dict: &Dict, data: &[u8]) -> Stream {
    let mut d = dict.clone();
    for k in [&b"Filter"[..], b"DecodeParms", b"Length", b"DL"] {
        d.remove(k);
    }
    d.set(b"Filter", Object::name(b"FlateDecode"));
    Stream::new(d, pagebara_filters::encode_flate(data, 6))
}

pub(crate) fn convert(doc: &mut Document, report: &mut Report) {
    let _ = report;
    for page in pagebara_pages::PageTree::load(doc).refs() {
        let Some(po) = doc.get(page) else { continue };
        let Some(pd) = po.as_dict() else { continue };
        let data = pagebara_content::page_contents(doc, pd);
        if let Some(new) = rewrite(&data) {
            let mut pd = pd.clone();
            let r = doc.add(flate_stream(&Dict::new(), &new));
            pd.set(b"Contents", r);
            doc.set(page, pd);
        }
    }
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else {
            continue;
        };
        let Some(o) = doc.get(r) else { continue };
        let Some(s) = o.as_stream() else { continue };
        let content = s.dict.name(b"Subtype") == Some(b"Form")
            || doc.get_in(&s.dict, b"PatternType").as_int() == Some(1);
        if !content {
            continue;
        }
        let Ok(d) = doc.decode(s) else { continue };
        if let Some(new) = rewrite(&d.data) {
            let st = flate_stream(&s.dict, &new);
            doc.set(r, st);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_operators_become_grey() {
        let out = rewrite(b"q 1 0 0 rg 0 0 1 RG 0 0 0 1 k 10 10 m S Q").unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("0.299 g"), "{s}");
        assert!(s.contains("0.114 G"), "{s}");
        assert!(s.contains(" 0 g"), "{s}");
        assert!(s.ends_with("10 10 m S Q"), "{s}");
    }
}
