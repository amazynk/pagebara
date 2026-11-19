//! Visual differences: both pages rendered at a low resolution, pixels compared with a
//! tolerance, differing cells grouped into rectangles. Pages whose content and resources
//! are the same objects byte for byte are not rendered.

use std::collections::HashSet;

use pagebara_render::{Annotations, PixelRect, RenderRequest, Renderer};
use pagebara_syntax::{Document, ObjRef, Object};

use crate::text::Box4;

/// Grid cell size in pixels for grouping differences.
const CELL: u32 = 6;
/// Objects compared before two pages count as different without rendering.
const MAX_NODES: usize = 20_000;

/// Whether page `pa` of `a` and page `pb` of `b` draw from equal objects (so they look the
/// same): content, resources and boxes compared structurally.
pub(crate) fn same_page(a: &Document, ra: ObjRef, b: &Document, rb: ObjRef) -> bool {
    let (Some(da), Some(db)) = (a.get(ra), b.get(rb)) else {
        return false;
    };
    let (Some(da), Some(db)) = (da.as_dict(), db.as_dict()) else {
        return false;
    };
    let mut seen = HashSet::new();
    let mut budget = MAX_NODES;
    for key in [
        &b"Contents"[..],
        b"Resources",
        b"MediaBox",
        b"CropBox",
        b"Rotate",
    ] {
        let (x, y) = (da.get(key), db.get(key));
        let same = match (x, y) {
            (None, None) => true,
            (Some(x), Some(y)) => equal(a, x, b, y, &mut seen, &mut budget),
            _ => false,
        };
        if !same {
            return false;
        }
    }
    // Inherited attributes would need the parents; pages with none of their own are
    // rendered to be safe.
    da.contains(b"Resources") && da.contains(b"MediaBox")
}

fn equal(
    a: &Document,
    x: &Object,
    b: &Document,
    y: &Object,
    seen: &mut HashSet<(ObjRef, ObjRef)>,
    budget: &mut usize,
) -> bool {
    if *budget == 0 {
        return false;
    }
    *budget -= 1;
    if let (Object::Ref(rx), Object::Ref(ry)) = (x, y) {
        if !seen.insert((*rx, *ry)) {
            return true;
        }
        let (Some(ox), Some(oy)) = (a.get(*rx), b.get(*ry)) else {
            return false;
        };
        return equal(a, &ox, b, &oy, seen, budget);
    }
    match (x, y) {
        (Object::Ref(r), o) => a.get(*r).is_some_and(|v| equal(a, &v, b, o, seen, budget)),
        (o, Object::Ref(r)) => b.get(*r).is_some_and(|v| equal(a, o, b, &v, seen, budget)),
        (Object::Array(p), Object::Array(q)) => {
            p.len() == q.len()
                && p.iter()
                    .zip(q)
                    .all(|(p, q)| equal(a, p, b, q, seen, budget))
        }
        (Object::Dict(p), Object::Dict(q)) => dict_equal(a, p, b, q, seen, budget),
        (Object::Stream(p), Object::Stream(q)) => {
            a.raw(p) == b.raw(q) && dict_equal(a, &p.dict, b, &q.dict, seen, budget)
        }
        (p, q) => p == q,
    }
}

fn dict_equal(
    a: &Document,
    p: &pagebara_syntax::Dict,
    b: &Document,
    q: &pagebara_syntax::Dict,
    seen: &mut HashSet<(ObjRef, ObjRef)>,
    budget: &mut usize,
) -> bool {
    let skip = |k: &[u8]| matches!(k, b"Length" | b"Parent" | b"Annots" | b"StructParents");
    let pk: Vec<_> = p.iter().filter(|(k, _)| !skip(k.as_bytes())).collect();
    let qn = q.iter().filter(|(k, _)| !skip(k.as_bytes())).count();
    pk.len() == qn
        && pk.iter().all(|(k, v)| {
            q.get(k.as_bytes())
                .is_some_and(|w| equal(a, v, b, w, seen, budget))
        })
}

/// A rendered page.
pub(crate) struct Raster {
    pub w: u32,
    pub h: u32,
    pub px: Vec<u8>,
}

pub(crate) fn render(r: &Renderer, page: usize, dpi: f32) -> Option<Raster> {
    let (w, h) = r.page_size(page)?;
    let scale = dpi / 72.0;
    let (pw, ph) = (
        ((w * scale).round() as u32).max(1),
        ((h * scale).round() as u32).max(1),
    );
    let mut req = RenderRequest::new(
        scale,
        PixelRect {
            x: 0,
            y: 0,
            w: pw,
            h: ph,
        },
    );
    req.annotations = Annotations::None;
    let img = r.render(page, &req).ok()?;
    Some(Raster {
        w: img.width,
        h: img.height,
        px: img.data,
    })
}

/// Rectangles (shown points from the top left) where the two pictures differ by more than
/// `tolerance` in a channel, and the share of differing pixels. Different page sizes are
/// compared over the area both cover, the rest counting as different.
pub(crate) fn differences(a: &Raster, b: &Raster, tolerance: u8, dpi: f32) -> (Vec<Box4>, f32) {
    let (w, h) = (a.w.max(b.w), a.h.max(b.h));
    let (cw, ch) = (w.div_ceil(CELL), h.div_ceil(CELL));
    let mut cells = vec![false; (cw * ch) as usize];
    let mut count = 0usize;
    for y in 0..h {
        for x in 0..w {
            let pa = (x < a.w && y < a.h).then(|| ((y * a.w + x) * 4) as usize);
            let pb = (x < b.w && y < b.h).then(|| ((y * b.w + x) * 4) as usize);
            let differs = match (pa, pb) {
                (Some(i), Some(j)) => (0..3).any(|c| a.px[i + c].abs_diff(b.px[j + c]) > tolerance),
                _ => true,
            };
            if differs {
                count += 1;
                cells[((y / CELL) * cw + x / CELL) as usize] = true;
            }
        }
    }
    // Connected groups of cells (8-neighbourhood), as boxes.
    let mut boxes = Vec::new();
    let mut seen = vec![false; cells.len()];
    let k = 72.0 / dpi;
    for start in 0..cells.len() {
        if !cells[start] || seen[start] {
            continue;
        }
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        let mut todo = vec![start];
        seen[start] = true;
        while let Some(c) = todo.pop() {
            let (cx, cy) = (c as u32 % cw, c as u32 / cw);
            (x0, y0, x1, y1) = (x0.min(cx), y0.min(cy), x1.max(cx), y1.max(cy));
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let (nx, ny) = (cx as i32 + dx, cy as i32 + dy);
                    if nx < 0 || ny < 0 || nx >= cw as i32 || ny >= ch as i32 {
                        continue;
                    }
                    let n = (ny as u32 * cw + nx as u32) as usize;
                    if cells[n] && !seen[n] {
                        seen[n] = true;
                        todo.push(n);
                    }
                }
            }
        }
        boxes.push([
            (x0 * CELL) as f32 * k,
            (y0 * CELL) as f32 * k,
            ((x1 + 1) * CELL).min(w) as f32 * k,
            ((y1 + 1) * CELL).min(h) as f32 * k,
        ]);
    }
    (boxes, count as f32 / (w * h).max(1) as f32)
}
