//! Soft masks from ExtGState `/SMask` (§11.6.5.2): the group rendered over its backdrop,
//! reduced to luminosity or alpha, through the transfer function.

use std::sync::Arc;

use pagebara_content::display::Id;
use pagebara_content::{Matrix, Rect};

use crate::canvas::Pixmap;
use crate::color::luminosity;
use crate::draw::Ctx;
use crate::function::Function;
use crate::list::Prepared;
use crate::raster::{IRect, Mask};

pub fn render(
    ctx: &mut Ctx<'_>,
    p: &Arc<Prepared>,
    id: Id,
    base: &Matrix,
    rect: IRect,
) -> Option<Mask> {
    let r = ctx.r;
    let doc = &r.doc;
    let sm = p.list.soft_masks.get(id as usize)?;
    let d = doc.resolve(&sm.object);
    let d = d.as_dict()?;
    let luminous = d.name(b"S") != Some(b"Alpha");
    let g = d.get(b"G")?;
    let gs = doc.resolve(g);
    let gs = gs.as_stream()?;
    let tr = match d.get(b"TR").map(|o| doc.resolve(o)) {
        Some(o) if o.as_name() == Some(b"Identity") => None,
        Some(o) => Function::parse(doc, &o),
        None => None,
    };
    let list = r.form_list(g, sm.ctm)?;
    // The backdrop colour in the group's colour space.
    let backdrop = if luminous {
        let group = doc.get_in(&gs.dict, b"Group");
        let cs = group
            .as_dict()
            .and_then(|g| g.get(b"CS").cloned())
            .and_then(|cs| r.resolve_space(&cs))
            .unwrap_or_else(|| r.gray.clone());
        let bc: Vec<f32> = doc
            .get_in(d, b"BC")
            .as_array()
            .unwrap_or_default()
            .iter()
            .filter_map(|o| doc.resolve(o).as_f64().map(|v| v as f32))
            .collect();
        let bc = if bc.len() == cs.components() {
            bc
        } else {
            vec![0.0; cs.components()]
        };
        let c = cs.to_rgb(&bc).map(|v| (v * 255.0 + 0.5) as u8);
        [c[0], c[1], c[2], 255]
    } else {
        [0; 4]
    };
    let pix = ctx.offscreen(&list, *base, Pixmap::filled(rect, backdrop), None, None);
    let lut: Option<Vec<u8>> = tr.map(|f| {
        (0..256)
            .map(|i| {
                let mut o = [0f32; 4];
                f.eval(&[i as f32 / 255.0], &mut o);
                (o[0].clamp(0.0, 1.0) * 255.0 + 0.5) as u8
            })
            .collect()
    });
    // An alpha mask is zero outside the group's box, whatever its transfer function makes
    // of zero inside (as in MuPDF and pdf.js).
    let inside = (!luminous)
        .then(|| {
            let b = crate::function::numbers(doc, &gs.dict, b"BBox").filter(|b| b.len() == 4)?;
            let fm = gs
                .dict
                .get(b"Matrix")
                .and_then(|o| Matrix::from_object(&doc.resolve(o)))
                .unwrap_or_default();
            let d = Rect::new(b[0], b[1], b[2], b[3]).transform(&fm.then(&sm.ctm).then(base));
            Some(IRect::covering([d.x0, d.y0, d.x1, d.y1]))
        })
        .flatten();
    let mut m = Mask::new(rect);
    let w = rect.width().max(1) as usize;
    for (i, (px, v)) in pix
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .zip(m.data.iter_mut())
        .enumerate()
    {
        let x = if luminous {
            let l = luminosity(px[0] as f32, px[1] as f32, px[2] as f32);
            (l + 0.5).min(255.0) as u8
        } else {
            px[3]
        };
        let (dx, dy) = (rect.x0 + (i % w) as i32, rect.y0 + (i / w) as i32);
        let out = inside.is_some_and(|b| dx < b.x0 || dx >= b.x1 || dy < b.y0 || dy >= b.y1);
        *v = if out {
            0
        } else {
            lut.as_ref().map_or(x, |l| l[x as usize])
        };
    }
    Some(m)
}
