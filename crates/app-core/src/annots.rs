//! Annotations as the editor uses them: reading a page's, hit testing, moving and resizing,
//! and the document-wide comment list.

use pagebara_annot::{Annotation, Kind, LineEnding, MarkupKind, Point, Quad, Rect};
use pagebara_i18n::tr;
use pagebara_pages::PageTree;
use pagebara_syntax::{Document, ObjRef, Stream};

use crate::session::format_pdf_date;

/// The annotations on page `r`.
pub fn on_page(doc: &Document, r: ObjRef) -> Vec<Annotation> {
    doc.get(r)
        .and_then(|o| o.as_dict().map(|d| pagebara_annot::annotations(doc, d)))
        .unwrap_or_default()
}

/// Shown and editable on the page: not popups, replies, links or form fields.
pub fn is_visible_markup(a: &Annotation) -> bool {
    a.in_reply_to.is_none()
        && !matches!(
            a.kind,
            Kind::Popup { .. } | Kind::Link { .. } | Kind::Widget { .. }
        )
        && !a.flags.contains(pagebara_annot::Flags::HIDDEN)
}

/// A name for the kind of annotation, as the user knows it.
pub fn kind_label(a: &Annotation) -> String {
    match &a.kind {
        // TRANSLATORS: noun, a kind of annotation (a sticky note)
        Kind::Text { .. } => tr!("Note"),
        Kind::FreeText { .. } => tr!("Text box"),
        Kind::Line { endings, .. } if endings[1] != LineEnding::None => tr!("Arrow"),
        // TRANSLATORS: noun, a kind of annotation
        Kind::Line { .. } => tr!("Line"),
        Kind::Square { .. } => tr!("Rectangle"),
        Kind::Circle { .. } => tr!("Ellipse"),
        Kind::Polygon { .. } => tr!("Polygon"),
        Kind::PolyLine { .. } => tr!("Polyline"),
        Kind::Markup { kind, .. } => match kind {
            // TRANSLATORS: noun, a kind of annotation
            MarkupKind::Highlight => tr!("Highlight"),
            MarkupKind::Underline => tr!("Underline"),
            MarkupKind::StrikeOut => tr!("Strikethrough"),
            MarkupKind::Squiggly => tr!("Squiggly underline"),
        },
        Kind::Ink { .. } => tr!("Drawing"),
        Kind::Stamp { .. } if a.subject.as_deref() == Some("Signature") => tr!("Signature"),
        // TRANSLATORS: noun, a kind of annotation
        Kind::Stamp { .. } => tr!("Stamp"),
        Kind::Link { .. } => tr!("Link"),
        Kind::Popup { .. } => tr!("Popup"),
        Kind::Widget { .. } => tr!("Form field"),
        Kind::Other(_) => tr!("Annotation"),
    }
}

fn seg_dist(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b.x - a.x, b.y - a.y);
    let len2 = dx * dx + dy * dy;
    let t = if len2 == 0.0 {
        0.0
    } else {
        (((p.x - a.x) * dx + (p.y - a.y) * dy) / len2).clamp(0.0, 1.0)
    };
    p.dist(Point::new(a.x + t * dx, a.y + t * dy))
}

fn polyline_dist(p: Point, pts: &[Point]) -> f64 {
    match pts {
        [] => f64::INFINITY,
        [one] => p.dist(*one),
        _ => pts
            .windows(2)
            .map(|w| seg_dist(p, w[0], w[1]))
            .fold(f64::INFINITY, f64::min),
    }
}

fn inside(r: &Rect, p: Point, tol: f64) -> bool {
    p.x >= r.x0 - tol && p.x <= r.x1 + tol && p.y >= r.y0 - tol && p.y <= r.y1 + tol
}

/// Whether `p` (user space) is on `a`, within `tol` points.
pub fn hits(a: &Annotation, p: Point, tol: f64) -> bool {
    match &a.kind {
        Kind::Ink { strokes } => strokes
            .iter()
            .any(|s| polyline_dist(p, s) <= tol + a.border.width / 2.0),
        Kind::Line { start, end, .. } => seg_dist(p, *start, *end) <= tol + a.border.width,
        Kind::PolyLine { vertices, .. } => polyline_dist(p, vertices) <= tol + a.border.width,
        Kind::Markup { quads, .. } => quads
            .iter()
            .any(|q| Rect::bounding(q.0).is_some_and(|r| inside(&r, p, tol / 2.0))),
        _ => inside(&a.rect, p, tol),
    }
}

/// The topmost annotation of `list` under `p` that the editor shows, by index.
pub fn hit_test(list: &[Annotation], p: Point, tol: f64) -> Option<usize> {
    list.iter()
        .enumerate()
        .rev()
        .find(|(_, a)| is_visible_markup(a) && hits(a, p, tol))
        .map(|(i, _)| i)
}

/// The link under `p`, if any.
pub fn link_at(list: &[Annotation], p: Point) -> Option<&Annotation> {
    list.iter().rev().find(|a| {
        let Kind::Link { quads, .. } = &a.kind else {
            return false;
        };
        if quads.is_empty() {
            inside(&a.rect, p, 0.0)
        } else {
            quads
                .iter()
                .any(|q| Rect::bounding(q.0).is_some_and(|r| inside(&r, p, 0.0)))
        }
    })
}

/// `a` with every point mapped by `f`.
fn map_points(a: &Annotation, f: impl Fn(Point) -> Point) -> Annotation {
    let mut a = a.clone();
    let r = a.rect;
    let (p0, p1) = (f(Point::new(r.x0, r.y0)), f(Point::new(r.x1, r.y1)));
    a.rect = Rect::new(p0.x, p0.y, p1.x, p1.y);
    let quad = |q: &mut Quad| q.0 = q.0.map(&f);
    match &mut a.kind {
        Kind::Ink { strokes } => {
            for s in strokes {
                for p in s.iter_mut() {
                    *p = f(*p);
                }
            }
        }
        Kind::Line { start, end, .. } => {
            *start = f(*start);
            *end = f(*end);
        }
        Kind::Polygon { vertices, .. } | Kind::PolyLine { vertices, .. } => {
            for p in vertices {
                *p = f(*p);
            }
        }
        Kind::Markup { quads, .. } | Kind::Link { quads, .. } => quads.iter_mut().for_each(quad),
        _ => {}
    }
    a
}

/// `a` moved by `dx`, `dy` points.
pub fn translated(a: &Annotation, dx: f64, dy: f64) -> Annotation {
    map_points(a, |p| Point::new(p.x + dx, p.y + dy))
}

/// `a` scaled from its rectangle to `to`.
pub fn resized(a: &Annotation, to: Rect) -> Annotation {
    let from = a.rect;
    let sx = to.width() / from.width().max(1e-6);
    let sy = to.height() / from.height().max(1e-6);
    let mut b = map_points(a, |p| {
        Point::new(to.x0 + (p.x - from.x0) * sx, to.y0 + (p.y - from.y0) * sy)
    });
    b.rect = to;
    b
}

/// A gently smoothed stroke: each inner point averaged with its neighbours.
pub fn smooth(points: &[Point]) -> Vec<Point> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut out = Vec::with_capacity(points.len());
    out.push(points[0]);
    for w in points.windows(3) {
        out.push(Point::new(
            (w[0].x + 2.0 * w[1].x + w[2].x) / 4.0,
            (w[0].y + 2.0 * w[1].y + w[2].y) / 4.0,
        ));
    }
    out.push(points[points.len() - 1]);
    out
}

/// Whether the app draws `a` over the page itself (it can then be edited without
/// rendering the page again). The page renderer draws all other annotations from their
/// appearance streams: form fields, links, and stamps or other kinds made by other apps.
pub fn drawn_by_app(doc: &Document, a: &Annotation) -> bool {
    if !is_visible_markup(a) {
        return false;
    }
    match a.kind {
        Kind::Stamp { .. } => {
            with_stamp_image(doc, a, |_, _, _, _| Some(())).is_some() || !has_appearance(doc, a)
        }
        Kind::Text { .. }
        | Kind::FreeText { .. }
        | Kind::Line { .. }
        | Kind::Square { .. }
        | Kind::Circle { .. }
        | Kind::Polygon { .. }
        | Kind::PolyLine { .. }
        | Kind::Markup { .. }
        | Kind::Ink { .. } => true,
        _ => false,
    }
}

/// Whether `a` has a normal appearance stream.
fn has_appearance(doc: &Document, a: &Annotation) -> bool {
    let Some(d) = a.id.and_then(|id| doc.get(id)) else {
        return false;
    };
    let Some(d) = d.as_dict() else { return false };
    let ap = doc.get_in(d, b"AP");
    ap.as_dict().is_some_and(|ap| ap.get(b"N").is_some())
}

/// Runs `f` on the image of an image stamp (placed signatures): its first image XObject,
/// when it is 8-bit grey or RGB, with width, height and components.
fn with_stamp_image<T>(
    doc: &Document,
    a: &Annotation,
    f: impl FnOnce(&Stream, u32, u32, usize) -> Option<T>,
) -> Option<T> {
    let d = doc.get(a.id?)?;
    let ap = doc.get_in(d.as_dict()?, b"AP");
    let n = doc.get_in(ap.as_dict()?, b"N");
    let form = n.as_stream()?;
    let res = doc.get_in(&form.dict, b"Resources");
    let xo = doc.get_in(res.as_dict()?, b"XObject");
    let first = xo.as_dict()?.iter().next()?.1.clone();
    let im = doc.resolve(&first);
    let s = im.as_stream()?;
    if s.dict.name(b"Subtype") != Some(b"Image") {
        return None;
    }
    let w = u32::try_from(doc.get_in(&s.dict, b"Width").as_int()?).ok()?;
    let h = u32::try_from(doc.get_in(&s.dict, b"Height").as_int()?).ok()?;
    let n_px = (w as usize).checked_mul(h as usize)?;
    if n_px == 0 || n_px > 16 << 20 || doc.get_in(&s.dict, b"BitsPerComponent").as_int() != Some(8)
    {
        return None;
    }
    let comps = match doc.get_in(&s.dict, b"ColorSpace").as_name() {
        Some(b"DeviceGray") => 1,
        Some(b"DeviceRGB") => 3,
        _ => return None,
    };
    f(s, w, h, comps)
}

/// The picture of an image stamp as 8-bit RGBA with straight alpha, for drawing it over
/// the page.
pub fn stamp_image(doc: &Document, a: &Annotation) -> Option<(u32, u32, Vec<u8>)> {
    with_stamp_image(doc, a, |s, w, h, comps| {
        let n_px = w as usize * h as usize;
        let data = doc.decode(s).ok().filter(|d| d.image.is_none())?.data;
        if data.len() < n_px * comps {
            return None;
        }
        let alpha = doc
            .get_in(&s.dict, b"SMask")
            .as_stream()
            .and_then(|m| doc.decode(m).ok())
            .map(|m| m.data)
            .filter(|m| m.len() >= n_px);
        let mut out = Vec::with_capacity(n_px * 4);
        for i in 0..n_px {
            let px = &data[i * comps..(i + 1) * comps];
            let a = alpha.as_ref().map_or(255, |m| m[i]);
            match comps {
                1 => out.extend_from_slice(&[px[0], px[0], px[0], a]),
                _ => out.extend_from_slice(&[px[0], px[1], px[2], a]),
            }
        }
        Some((w, h, out))
    })
}

/// One entry of the comment list.
#[derive(Clone, Debug, PartialEq)]
pub struct CommentEntry {
    pub page: usize,
    pub id: ObjRef,
    pub kind: String,
    pub author: String,
    pub date: String,
    pub contents: String,
    /// The text a highlight, underline or strike-out marks (its subject).
    pub quote: String,
    pub replies: Vec<Reply>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Reply {
    pub id: Option<ObjRef>,
    pub author: String,
    pub date: String,
    pub contents: String,
}

fn when(a: &Annotation) -> String {
    a.modified
        .as_deref()
        .or(a.created.as_deref())
        .and_then(format_pdf_date)
        .unwrap_or_default()
}

pub fn reply_of(a: &Annotation) -> Reply {
    Reply {
        id: a.id,
        author: a.author.clone().unwrap_or_default(),
        date: when(a),
        contents: a.contents.clone().unwrap_or_default(),
    }
}

/// The comment entry of `a` among `all` on page `page`.
pub fn entry(page: usize, a: &Annotation, all: &[Annotation]) -> Option<CommentEntry> {
    let id = a.id?;
    Some(CommentEntry {
        page,
        id,
        kind: kind_label(a),
        author: a.author.clone().unwrap_or_default(),
        date: when(a),
        contents: a.contents.clone().unwrap_or_default(),
        quote: match a.kind {
            Kind::Markup { .. } => a.subject.clone().unwrap_or_default(),
            _ => String::new(),
        },
        replies: pagebara_annot::replies(all, id)
            .into_iter()
            .map(reply_of)
            .collect(),
    })
}

/// Every annotation the editor shows, page by page, with replies.
pub fn comments(doc: &Document, tree: &PageTree) -> Vec<CommentEntry> {
    let mut out = Vec::new();
    for (i, p) in tree.pages().iter().enumerate() {
        let all = on_page(doc, p.r);
        out.extend(
            all.iter()
                .filter(|a| is_visible_markup(a))
                .filter_map(|a| entry(i, a, &all)),
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hit_move_resize() {
        let mut a = Annotation::new(
            Kind::Ink {
                strokes: vec![vec![Point::new(10.0, 10.0), Point::new(50.0, 10.0)]],
            },
            Rect::new(8.0, 8.0, 52.0, 12.0),
        );
        a.id = Some(ObjRef::new(9, 0));
        assert!(hits(&a, Point::new(30.0, 11.0), 2.0));
        assert!(!hits(&a, Point::new(30.0, 30.0), 2.0));
        let m = translated(&a, 5.0, 100.0);
        let Kind::Ink { strokes } = &m.kind else {
            panic!()
        };
        assert_eq!(strokes[0][0], Point::new(15.0, 110.0));
        let r = resized(&a, Rect::new(8.0, 8.0, 96.0, 12.0));
        let Kind::Ink { strokes } = &r.kind else {
            panic!()
        };
        assert_eq!(strokes[0][1].x, 92.0);
        assert_eq!(hit_test(&[a], Point::new(12.0, 10.0), 1.0), Some(0));
    }

    #[test]
    fn smoothing_keeps_ends() {
        let pts = [
            Point::new(0.0, 0.0),
            Point::new(1.0, 4.0),
            Point::new(2.0, 0.0),
        ];
        let s = smooth(&pts);
        assert_eq!(s.len(), 3);
        assert_eq!(s[0], pts[0]);
        assert_eq!(s[2], pts[2]);
        assert!(s[1].y < 4.0);
    }
}
