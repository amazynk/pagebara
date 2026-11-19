//! Reading a page's `/Annots` into the model.

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::geom::{Color, Quad, Rect, numbers, points};
use crate::model::*;
use crate::text;

/// The annotations of `page` (its dictionary), in `/Annots` order. Entries that are not
/// annotation dictionaries are skipped.
pub fn annotations(doc: &Document, page: &Dict) -> Vec<Annotation> {
    let annots = doc.get_in(page, b"Annots");
    let Some(list) = annots.as_array() else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|o| {
            let id = o.as_objref();
            let d = doc.resolve(o);
            from_dict(doc, d.as_dict()?, id)
        })
        .collect()
}

/// The annotation stored at `r`.
pub fn read(doc: &Document, r: ObjRef) -> Option<Annotation> {
    let o = doc.get(r)?;
    from_dict(doc, o.as_dict()?, Some(r))
}

/// The replies to `parent` among `all` (direct replies only), oldest first.
pub fn replies(all: &[Annotation], parent: ObjRef) -> Vec<&Annotation> {
    let mut v: Vec<&Annotation> = all
        .iter()
        .filter(|a| a.in_reply_to == Some(parent) && a.reply_type == ReplyType::Reply)
        .collect();
    v.sort_by(|a, b| a.created.cmp(&b.created));
    v
}

pub(crate) fn from_dict(doc: &Document, d: &Dict, id: Option<ObjRef>) -> Option<Annotation> {
    let subtype = d.name(b"Subtype")?;
    let rect = Rect::from_object(&doc.get_in(d, b"Rect")).unwrap_or_default();
    let string = |k: &[u8]| doc.get_in(d, k).as_string().map(text::decode);
    let color = |k: &[u8]| Color::from_object(&doc.get_in(d, k));
    let endings = || {
        let le = doc.get_in(d, b"LE");
        let n = |i: usize| {
            le.as_array()
                .and_then(|a| a.get(i))
                .and_then(Object::as_name)
                .map_or(LineEnding::None, LineEnding::from_name)
        };
        [n(0), n(1)]
    };
    let pts = |k: &[u8]| points(&doc.get_in(d, k)).unwrap_or_default();
    let quads = |k: &[u8]| -> Vec<Quad> {
        pts(k)
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| Quad([c[0], c[1], c[2], c[3]]))
            .collect()
    };
    let kind = match subtype {
        b"Text" => Kind::Text {
            icon: d
                .name(b"Name")
                .map_or("Note".into(), |n| String::from_utf8_lossy(n).into_owned()),
            open: doc.get_in(d, b"Open").as_bool().unwrap_or(false),
        },
        b"Link" => Kind::Link {
            action: crate::link::read(doc, d),
            quads: quads(b"QuadPoints"),
        },
        b"FreeText" => {
            let (font_size, text_color) = parse_da(&string(b"DA").unwrap_or_default());
            Kind::FreeText {
                font_size,
                text_color,
                align: match d.int(b"Q") {
                    Some(1) => Align::Center,
                    Some(2) => Align::Right,
                    _ => Align::Left,
                },
            }
        }
        b"Line" => {
            let l = pts(b"L");
            Kind::Line {
                start: l.first().copied().unwrap_or_default(),
                end: l.get(1).copied().unwrap_or_default(),
                endings: endings(),
                interior: color(b"IC"),
            }
        }
        b"Square" => Kind::Square {
            interior: color(b"IC"),
        },
        b"Circle" => Kind::Circle {
            interior: color(b"IC"),
        },
        b"Polygon" => Kind::Polygon {
            vertices: pts(b"Vertices"),
            interior: color(b"IC"),
        },
        b"PolyLine" => Kind::PolyLine {
            vertices: pts(b"Vertices"),
            endings: endings(),
            interior: color(b"IC"),
        },
        b"Highlight" | b"Underline" | b"StrikeOut" | b"Squiggly" => Kind::Markup {
            kind: match subtype {
                b"Highlight" => MarkupKind::Highlight,
                b"Underline" => MarkupKind::Underline,
                b"StrikeOut" => MarkupKind::StrikeOut,
                _ => MarkupKind::Squiggly,
            },
            quads: quads(b"QuadPoints"),
        },
        b"Ink" => Kind::Ink {
            strokes: doc
                .get_in(d, b"InkList")
                .as_array()
                .map(|a| a.iter().filter_map(|s| points(&doc.resolve(s))).collect())
                .unwrap_or_default(),
        },
        b"Stamp" => Kind::Stamp {
            icon: d
                .name(b"Name")
                .map(|n| String::from_utf8_lossy(n).into_owned()),
        },
        b"Popup" => Kind::Popup {
            parent: d.get(b"Parent").and_then(Object::as_objref),
            open: doc.get_in(d, b"Open").as_bool().unwrap_or(false),
        },
        b"Widget" => widget(doc, d),
        other => Kind::Other(String::from_utf8_lossy(other).into_owned()),
    };
    let reply_type = match d.name(b"RT") {
        Some(b"Group") => ReplyType::Group,
        _ => ReplyType::Reply,
    };
    Some(Annotation {
        id,
        kind,
        rect,
        contents: string(b"Contents"),
        author: string(b"T"),
        subject: string(b"Subj"),
        name: string(b"NM"),
        modified: string(b"M"),
        created: string(b"CreationDate"),
        color: color(b"C"),
        opacity: doc
            .get_in(d, b"CA")
            .as_f64()
            .map_or(1.0, |v| v.clamp(0.0, 1.0)),
        border: border(doc, d),
        flags: Flags(d.int(b"F").unwrap_or(0) as u32),
        in_reply_to: d.get(b"IRT").and_then(Object::as_objref),
        reply_type,
        popup: d.get(b"Popup").and_then(Object::as_objref),
        rotate: doc
            .get_in(d, b"Rotate")
            .as_int()
            .map(crate::appearance::normalize_rotation),
    })
}

fn border(doc: &Document, d: &Dict) -> Border {
    if let Some(bs) = doc.get_in(d, b"BS").as_dict() {
        let width = doc.get_in(bs, b"W").as_f64().unwrap_or(1.0).max(0.0);
        let style = match bs.name(b"S") {
            Some(b"D") => BorderStyle::Dashed(numbers(&doc.get_in(bs, b"D")).unwrap_or(vec![3.0])),
            Some(b"B") => BorderStyle::Beveled,
            Some(b"I") => BorderStyle::Inset,
            Some(b"U") => BorderStyle::Underline,
            _ => BorderStyle::Solid,
        };
        return Border { width, style };
    }
    match numbers(&doc.get_in(d, b"Border")).as_deref() {
        Some([_, _, w, ..]) => Border {
            width: w.max(0.0),
            style: match doc.get_in(d, b"Border").as_array().and_then(|a| a.get(3)) {
                Some(dash) => BorderStyle::Dashed(numbers(dash).unwrap_or_default()),
                None => BorderStyle::Solid,
            },
        },
        _ => Border::default(),
    }
}

fn widget(doc: &Document, d: &Dict) -> Kind {
    // Field attributes may be inherited from parent fields.
    let mut field_type = None;
    let mut names = Vec::new();
    let mut cur = d.clone();
    for _ in 0..32 {
        if field_type.is_none() {
            field_type = cur
                .name(b"FT")
                .map(|n| String::from_utf8_lossy(n).into_owned());
        }
        if let Some(t) = doc.get_in(&cur, b"T").as_string() {
            names.push(text::decode(t));
        }
        let parent = doc.get_in(&cur, b"Parent");
        match parent.as_dict() {
            Some(p) => cur = p.clone(),
            None => break,
        }
    }
    names.reverse();
    Kind::Widget {
        field_type,
        field_name: (!names.is_empty()).then(|| names.join(".")),
    }
}

/// Font size and colour from a default appearance string such as `/Helv 12 Tf 0 0 1 rg`.
pub(crate) fn parse_da(da: &str) -> (f64, Color) {
    let toks: Vec<&str> = da.split_whitespace().collect();
    let mut size = 12.0;
    let mut color = Color::BLACK;
    let num = |i: usize| -> Option<f32> { toks.get(i)?.parse().ok() };
    for (i, t) in toks.iter().enumerate() {
        match *t {
            "Tf" => size = i.checked_sub(1).and_then(num).map_or(12.0, f64::from),
            "g" => color = Color::Gray(i.checked_sub(1).and_then(num).unwrap_or(0.0)),
            "rg" if i >= 3 => {
                if let (Some(r), Some(g), Some(b)) = (num(i - 3), num(i - 2), num(i - 1)) {
                    color = Color::Rgb(r, g, b);
                }
            }
            "k" if i >= 4 => {
                if let (Some(c), Some(m), Some(y), Some(k)) =
                    (num(i - 4), num(i - 3), num(i - 2), num(i - 1))
                {
                    color = Color::Cmyk(c, m, y, k);
                }
            }
            _ => {}
        }
    }
    (if size > 0.0 { size } else { 12.0 }, color)
}
