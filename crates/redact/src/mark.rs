//! Redaction marks: `/Redact` annotations (ISO 32000-2 §12.5.6.23), created and read.

use pagebara_content::{ContentParser, Op};
use pagebara_syntax::{Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::emit;
use crate::geom::{Point, Rect, Region, round};
use crate::{Error, Result};

/// Horizontal alignment of overlay text (`/Q`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

/// An area to redact on one page, with how it looks once applied.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    /// Quadrilaterals in default user space, vertices in any order (`/QuadPoints`).
    pub quads: Vec<[Point; 4]>,
    /// Fill of the redacted area once applied (`/IC`); `None` leaves it unfilled.
    pub fill: Option<[f64; 3]>,
    /// Outline colour while marked (`/C`).
    pub outline: [f64; 3],
    /// Text drawn over the redacted area (`/OverlayText`).
    pub overlay_text: Option<String>,
    /// Repeat the overlay text to fill the area (`/Repeat`).
    pub repeat: bool,
    pub text_color: [f64; 3],
    /// Overlay text size; 0 fits it to the area.
    pub font_size: f64,
    pub align: Align,
}

impl Default for Mark {
    fn default() -> Self {
        Mark {
            quads: Vec::new(),
            fill: Some([0.0; 3]),
            outline: [1.0, 0.0, 0.0],
            overlay_text: None,
            repeat: false,
            text_color: [1.0; 3],
            font_size: 0.0,
            align: Align::Center,
        }
    }
}

impl Mark {
    /// A black box over `r`.
    pub fn rect(r: Rect) -> Mark {
        Mark {
            quads: vec![r.corners()],
            ..Mark::default()
        }
    }

    /// A black box over each quadrilateral (a text selection or search hit).
    pub fn quads(quads: Vec<[Point; 4]>) -> Mark {
        Mark {
            quads,
            ..Mark::default()
        }
    }

    pub fn regions(&self) -> Vec<Region> {
        self.quads
            .iter()
            .filter_map(|q| Region::from_points(q))
            .collect()
    }

    pub fn bbox(&self) -> Option<Rect> {
        let pts: Vec<Point> = self.quads.iter().flatten().copied().collect();
        (!pts.is_empty()).then(|| Rect::from_points(&pts))
    }
}

fn color(c: [f64; 3]) -> Object {
    Object::Array(c.iter().map(|&v| Object::Real(round(v))).collect())
}

fn text_string(s: &str) -> Object {
    let bytes = match pagebara_syntax::text::pdfdoc_encode(s) {
        Some(b) => b,
        None => {
            let mut b = vec![0xFE, 0xFF];
            for u in s.encode_utf16() {
                b.extend_from_slice(&u.to_be_bytes());
            }
            b
        }
    };
    Object::String(PdfString { bytes, hex: false })
}

/// Adds a redaction mark to the page `page`, returning the annotation. Nothing is removed
/// until [`apply`](crate::apply).
pub fn add_mark(doc: &mut Document, page: ObjRef, mark: &Mark) -> Result<ObjRef> {
    if !doc.permissions().annotate && !doc.is_owner() {
        return Err(Error::NotPermitted);
    }
    let p = doc.get(page).ok_or(Error::NotAPage)?;
    let pd = p.as_dict().ok_or(Error::NotAPage)?.clone();
    let bbox = mark.bbox().ok_or(Error::EmptyMark)?;
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"Annot"));
    d.set(b"Subtype", Object::name(b"Redact"));
    d.set(b"Rect", bbox.to_object());
    d.set(
        b"QuadPoints",
        Object::Array(
            mark.quads
                .iter()
                .flat_map(|q| {
                    // Upper left, upper right, lower left, lower right, as readers expect.
                    let r = Rect::from_points(q);
                    let ordered = if is_axis_rect(q) {
                        [
                            Point::new(r.x0, r.y1),
                            Point::new(r.x1, r.y1),
                            Point::new(r.x0, r.y0),
                            Point::new(r.x1, r.y0),
                        ]
                    } else {
                        *q
                    };
                    ordered
                        .into_iter()
                        .flat_map(|p| [Object::Real(round(p.x)), Object::Real(round(p.y))])
                })
                .collect(),
        ),
    );
    if let Some(f) = mark.fill {
        d.set(b"IC", color(f));
    }
    d.set(b"C", color(mark.outline));
    d.set(b"F", 4);
    d.set(b"P", Object::Ref(page));
    if let Some(t) = &mark.overlay_text {
        d.set(b"OverlayText", text_string(t));
        let mut da = Vec::new();
        emit::nums(&mut da, &mark.text_color);
        da.extend_from_slice(b"rg /Helv ");
        emit::num(&mut da, mark.font_size);
        da.extend_from_slice(b" Tf");
        d.set(b"DA", Object::string(&da));
        d.set(
            b"Q",
            match mark.align {
                Align::Left => 0,
                Align::Center => 1,
                Align::Right => 2,
            },
        );
        if mark.repeat {
            d.set(b"Repeat", true);
        }
    }
    d.set(b"M", text_string(&pagebara_annot::pdf_now()));
    // The marked state: an outline in the mark's colour.
    let mut ap = Vec::new();
    emit::nums(&mut ap, &mark.outline);
    ap.extend_from_slice(b"RG 1 w\n");
    for q in &mark.quads {
        if let Some(r) = Region::from_points(q) {
            for (k, p) in r.points().iter().enumerate() {
                emit::nums(&mut ap, &[p.x, p.y]);
                ap.extend_from_slice(if k == 0 { b"m\n" } else { b"l\n" });
            }
            ap.extend_from_slice(b"h S\n");
        }
    }
    let mut sd = Dict::new();
    sd.set(b"Type", Object::name(b"XObject"));
    sd.set(b"Subtype", Object::name(b"Form"));
    sd.set(b"BBox", bbox.to_object());
    let ap = doc.add(Stream::new(sd, ap));
    let mut apd = Dict::new();
    apd.set(b"N", Object::Ref(ap));
    d.set(b"AP", apd);
    let r = doc.add(d);
    add_to_annots(doc, page, &pd, r);
    Ok(r)
}

fn is_axis_rect(q: &[Point; 4]) -> bool {
    let r = Rect::from_points(q);
    q.iter()
        .all(|p| (p.x == r.x0 || p.x == r.x1) && (p.y == r.y0 || p.y == r.y1))
}

fn add_to_annots(doc: &mut Document, page: ObjRef, pd: &Dict, annot: ObjRef) {
    match pd.get(b"Annots") {
        Some(Object::Ref(ar)) => {
            let ar = *ar;
            let mut a = doc
                .get(ar)
                .and_then(|o| o.as_array().map(<[Object]>::to_vec))
                .unwrap_or_default();
            a.push(Object::Ref(annot));
            doc.set(ar, Object::Array(a));
        }
        other => {
            let mut a = other
                .and_then(Object::as_array)
                .map(<[Object]>::to_vec)
                .unwrap_or_default();
            a.push(Object::Ref(annot));
            let mut pd = pd.clone();
            pd.set(b"Annots", Object::Array(a));
            doc.set(page, pd);
        }
    }
}

/// The page's annotation references.
pub(crate) fn annots(doc: &Document, page: &Dict) -> Vec<ObjRef> {
    doc.get_in(page, b"Annots")
        .as_array()
        .unwrap_or_default()
        .iter()
        .filter_map(Object::as_objref)
        .collect()
}

/// The redaction marks on a page.
pub fn marks(doc: &Document, page: ObjRef) -> Vec<(ObjRef, Mark)> {
    let Some(p) = doc.get(page) else {
        return Vec::new();
    };
    let Some(pd) = p.as_dict() else {
        return Vec::new();
    };
    annots(doc, pd)
        .into_iter()
        .filter_map(|r| {
            let o = doc.get(r)?;
            let d = o.as_dict()?;
            (d.name(b"Subtype") == Some(b"Redact")).then(|| (r, read(doc, d)))
        })
        .filter(|(_, m)| !m.quads.is_empty())
        .collect()
}

/// Reads a `/Redact` annotation dictionary.
pub fn read(doc: &Document, d: &Dict) -> Mark {
    let nums = |k: &[u8]| -> Vec<f64> {
        doc.get_in(d, k)
            .as_array()
            .unwrap_or_default()
            .iter()
            .filter_map(|o| doc.resolve(o).as_f64())
            .collect()
    };
    let rgb = |v: Vec<f64>| -> Option<[f64; 3]> {
        match v.len() {
            1 => Some([v[0]; 3]),
            3 => Some([v[0], v[1], v[2]]),
            4 => {
                let k = v[3];
                Some([0, 1, 2].map(|i| (1.0 - v[i]) * (1.0 - k)))
            }
            _ => None,
        }
    };
    let mut quads: Vec<[Point; 4]> = nums(b"QuadPoints")
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| [0, 1, 2, 3].map(|i| Point::new(c[2 * i], c[2 * i + 1])))
        .collect();
    if quads.is_empty()
        && let Some(r) = d.get(b"Rect").and_then(|o| Rect::from_object(doc, o))
    {
        quads.push(r.corners());
    }
    let mut m = Mark {
        quads,
        fill: rgb(nums(b"IC")),
        outline: rgb(nums(b"C")).unwrap_or([1.0, 0.0, 0.0]),
        overlay_text: d
            .get(b"OverlayText")
            .map(|o| pagebara_content::extract::text_string(&doc.resolve(o)))
            .filter(|s| !s.is_empty()),
        repeat: doc.get_in(d, b"Repeat").as_bool() == Some(true),
        text_color: [0.0; 3],
        font_size: 0.0,
        align: match doc.get_in(d, b"Q").as_int() {
            Some(0) => Align::Left,
            Some(2) => Align::Right,
            _ => Align::Center,
        },
    };
    if let Some(da) = doc.get_in(d, b"DA").as_string() {
        let mut p = ContentParser::new(da);
        while let Some(i) = p.next() {
            match i.op {
                Op::FillGray => {
                    if let Some([g]) = i.numbers() {
                        m.text_color = [f64::from(g); 3];
                    }
                }
                Op::FillRgb => {
                    if let Some(c) = i.numbers::<3>() {
                        m.text_color = c.map(f64::from);
                    }
                }
                Op::FillCmyk => {
                    if let Some(c) = i.numbers::<4>() {
                        m.text_color = rgb(c.map(f64::from).to_vec()).unwrap_or_default();
                    }
                }
                Op::Font => m.font_size = i.number().map_or(0.0, |v| f64::from(v).abs()),
                _ => {}
            }
        }
    }
    m
}

/// Removes a redaction mark (or any annotation) from the page without applying it.
pub fn remove_mark(doc: &mut Document, page: ObjRef, annot: ObjRef) -> Result<()> {
    let p = doc.get(page).ok_or(Error::NotAPage)?;
    let pd = p.as_dict().ok_or(Error::NotAPage)?.clone();
    if !annots(doc, &pd).contains(&annot) {
        return Err(Error::NotFound);
    }
    crate::apply::remove_annots(doc, page, &[annot]);
    Ok(())
}
