//! Transparency flattening by rendering: each area painted with transparency (fill or
//! stroke opacity, soft-masked images, blend modes or soft masks in the graphics state)
//! is rendered with everything under and over it, the transparent objects are deleted and
//! the picture is drawn in their place.

use pagebara_content::Rect;
use pagebara_edit::{ObjectData, PageObject, PageObjects, Target};
use pagebara_pages::PageTree;
use pagebara_render::{Annotations, PixelRect, RenderRequest, Renderer};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::Report;

fn transparent(objs: &PageObjects, o: &PageObject) -> bool {
    match &o.data {
        ObjectData::Text(t) => t.runs().any(|r| {
            objs.run(r)
                .is_some_and(|r| r.fill_alpha < 1.0 || r.stroke_alpha < 1.0)
        }),
        ObjectData::Path(p) => {
            (p.fill.is_some() && p.fill_alpha < 1.0) || (p.stroke.is_some() && p.stroke_alpha < 1.0)
        }
        ObjectData::Image(i) => i.alpha < 1.0 || i.has_soft_mask,
        ObjectData::Form(f) => f
            .children
            .iter()
            .filter_map(|c| objs.get(*c))
            .any(|c| transparent(objs, c)),
        ObjectData::Shading(_) => false,
    }
}

/// Graphics states the page uses that blend or mask (which the object model does not
/// show per object): the whole page counts then.
fn page_blends(doc: &Document, page: &Dict) -> bool {
    let res = doc.get_in(page, b"Resources");
    let Some(res) = res.as_dict() else {
        return false;
    };
    let gs = doc.get_in(res, b"ExtGState");
    let Some(gs) = gs.as_dict() else { return false };
    gs.iter().any(|(_, g)| {
        let g = doc.resolve(g);
        let Some(g) = g.as_dict() else { return false };
        let bm = doc.get_in(g, b"BM");
        let blend = match &*bm {
            Object::Name(n) => !matches!(n.as_bytes(), b"Normal" | b"Compatible"),
            Object::Array(a) => a
                .first()
                .and_then(Object::as_name)
                .is_some_and(|n| !matches!(n, b"Normal" | b"Compatible")),
            _ => false,
        };
        blend || doc.get_in(g, b"SMask").as_dict().is_some()
    })
}

fn overlaps(a: &Rect, b: &Rect) -> bool {
    a.x0 <= b.x1 && b.x0 <= a.x1 && a.y0 <= b.y1 && b.y0 <= a.y1
}

fn merge(mut rects: Vec<Rect>) -> Vec<Rect> {
    loop {
        let mut merged = false;
        let mut out: Vec<Rect> = Vec::new();
        for r in rects {
            if let Some(o) = out.iter_mut().find(|o| overlaps(o, &r)) {
                *o = o.union(&r);
                merged = true;
            } else {
                out.push(r);
            }
        }
        rects = out;
        if !merged {
            return rects;
        }
    }
}

/// Shown page coordinates (origin bottom left, as displayed) to user space.
fn frame(crop: &pagebara_pages::Rect, rotate: u16) -> ([f64; 6], f64, f64) {
    let c = crop;
    let (cw, ch) = (c.width(), c.height());
    match rotate {
        90 => ([0.0, 1.0, -1.0, 0.0, c.x1, c.y0], ch, cw),
        180 => ([-1.0, 0.0, 0.0, -1.0, c.x1, c.y1], cw, ch),
        270 => ([0.0, -1.0, 1.0, 0.0, c.x0, c.y1], ch, cw),
        _ => ([1.0, 0.0, 0.0, 1.0, c.x0, c.y0], cw, ch),
    }
}

fn apply(m: &[f64; 6], x: f64, y: f64) -> (f64, f64) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}

fn invert(m: &[f64; 6]) -> [f64; 6] {
    let det = m[0] * m[3] - m[1] * m[2];
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    [a, b, c, d, -(m[4] * a + m[5] * c), -(m[4] * b + m[5] * d)]
}

fn num(v: f64) -> String {
    let r = (v * 10_000.0).round() / 10_000.0;
    format!("{r}")
}

pub(crate) fn flatten(doc: &mut Document, dpi: f32, report: &mut Report) {
    let tree = PageTree::load(doc);
    let pages: Vec<_> = tree.pages().to_vec();
    let scale = dpi / 72.0;
    for (i, p) in pages.iter().enumerate() {
        let Some(pd) = doc.get(p.r).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        let Ok(objs) = pagebara_edit::parse(doc, i) else {
            continue;
        };
        let crop = Rect::new(
            p.crop_box.x0 as f32,
            p.crop_box.y0 as f32,
            p.crop_box.x1 as f32,
            p.crop_box.y1 as f32,
        );
        let whole = page_blends(doc, &pd);
        let mut targets = Vec::new();
        let mut rects = Vec::new();
        for o in objs.top_level() {
            if o.hidden || !(whole || transparent(&objs, o)) {
                continue;
            }
            let mut b = o.bbox;
            if let Some(c) = o.clip {
                b = Rect::new(
                    b.x0.max(c.x0),
                    b.y0.max(c.y0),
                    b.x1.min(c.x1),
                    b.y1.min(c.y1),
                );
            }
            let b = Rect::new(
                b.x0.max(crop.x0) - 1.0,
                b.y0.max(crop.y0) - 1.0,
                b.x1.min(crop.x1) + 1.0,
                b.y1.min(crop.y1) + 1.0,
            );
            targets.push(Target::Object(o.id));
            if b.x1 > b.x0 && b.y1 > b.y0 {
                rects.push(b);
            }
        }
        if targets.is_empty() {
            continue;
        }
        let rects = merge(rects);
        let (fm, w, h) = frame(&p.crop_box, p.rotate);
        let inv = invert(&fm);
        let renderer = Renderer::new(doc.clone());
        let mut pictures = Vec::new();
        for r in &rects {
            // The area as shown, in pixels from the top left.
            let pts = [
                apply(&inv, f64::from(r.x0), f64::from(r.y0)),
                apply(&inv, f64::from(r.x1), f64::from(r.y0)),
                apply(&inv, f64::from(r.x0), f64::from(r.y1)),
                apply(&inv, f64::from(r.x1), f64::from(r.y1)),
            ];
            let sx0 = pts.iter().map(|p| p.0).fold(f64::MAX, f64::min).max(0.0);
            let sx1 = pts.iter().map(|p| p.0).fold(f64::MIN, f64::max).min(w);
            let sy0 = pts.iter().map(|p| p.1).fold(f64::MAX, f64::min).max(0.0);
            let sy1 = pts.iter().map(|p| p.1).fold(f64::MIN, f64::max).min(h);
            let s = f64::from(scale);
            let px = (sx0 * s).floor() as i32;
            let py = ((h - sy1) * s).floor() as i32;
            let pw = ((sx1 * s).ceil() as i32 - px).max(1) as u32;
            let ph = (((h - sy0) * s).ceil() as i32 - py).max(1) as u32;
            let mut req = RenderRequest::new(
                scale,
                PixelRect {
                    x: px,
                    y: py,
                    w: pw,
                    h: ph,
                },
            );
            req.annotations = Annotations::None;
            let Ok(img) = renderer.render(i, &req) else {
                continue;
            };
            let Ok((im, _, _)) = pagebara_edit::embed_image(
                doc,
                &pagebara_edit::NewImage::Rgba {
                    width: img.width,
                    height: img.height,
                    pixels: &img.data,
                },
            ) else {
                continue;
            };
            // Back to shown points from the pixel grid.
            let x0 = f64::from(px) / s;
            let y1 = h - f64::from(py) / s;
            let (iw, ih) = (f64::from(pw) / s, f64::from(ph) / s);
            pictures.push((im, [iw, 0.0, 0.0, ih, x0, y1 - ih]));
        }
        if pictures.len() != rects.len() {
            report.warnings.push(format!(
                "page {}: transparency could not be rendered",
                i + 1
            ));
            continue;
        }
        if pagebara_edit::delete(doc, i, &targets).is_err() {
            continue;
        }
        draw_on_top(doc, p.r, &pictures, fm);
        report.transparent_areas += pictures.len();
    }
}

fn concat(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[1] * b[2],
        a[0] * b[1] + a[1] * b[3],
        a[2] * b[0] + a[3] * b[2],
        a[2] * b[1] + a[3] * b[3],
        a[4] * b[0] + a[5] * b[2] + b[4],
        a[4] * b[1] + a[5] * b[3] + b[5],
    ]
}

/// Draws images (placed in shown coordinates) over the page content.
fn draw_on_top(doc: &mut Document, page: ObjRef, pictures: &[(ObjRef, [f64; 6])], fm: [f64; 6]) {
    let Some(mut d) = doc.get(page).and_then(|o| o.as_dict().cloned()) else {
        return;
    };
    let mut res = doc
        .get_in(&d, b"Resources")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let mut xo = doc
        .get_in(&res, b"XObject")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let mut ops = String::from("Q\nq\n");
    for (im, m) in pictures {
        let name = format!("PagebaraFlat{}", im.num);
        xo.set(name.as_bytes(), *im);
        let m = concat(*m, fm);
        ops.push_str(&format!(
            "q {} {} {} {} {} {} cm /{name} Do Q\n",
            num(m[0]),
            num(m[1]),
            num(m[2]),
            num(m[3]),
            num(m[4]),
            num(m[5])
        ));
    }
    ops.push_str("Q\n");
    res.set(b"XObject", xo);
    d.set(b"Resources", res);
    let mut list: Vec<Object> = match d.get(b"Contents") {
        Some(Object::Array(a)) => a.clone(),
        Some(o @ Object::Ref(_)) => match doc.resolve(o).as_array() {
            Some(a) => a.to_vec(),
            None => vec![o.clone()],
        },
        _ => Vec::new(),
    };
    let open = doc.add(Stream::new(Dict::new(), &b"q\n"[..]));
    list.insert(0, open.into());
    let ours = doc.add(Stream::new(Dict::new(), ops.into_bytes()));
    list.push(ours.into());
    d.set(b"Contents", list);
    doc.set(page, d);
}
