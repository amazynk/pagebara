//! What the Edit tool draws over a page: the object under the pointer, the selection
//! with its handles, a drag in progress (outlines where the objects go, snapping guides,
//! what changes), the box being dragged out, the crop frame, and text being edited.

use pagebara_app_core::object_edit::{
    ImagePose, Raster, ViewRect, drag_images, outset, preview_scale, preview_stale,
};
use pagebara_app_core::tiles::RenderBackend;
use pagebara_app_core::view::Placement;
use pagebara_edit::{Matrix, PageObjects, Point, Rect};

use super::super::Tab;
use super::super::overlay::{at, fill, path};
use super::{Press, TabEdit, view_rect};
use crate::OverlayItem;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

const SELECTION: slint::Color = slint::Color::from_argb_u8(0x55, 0x25, 0x63, 0xEB);
const GUIDE: slint::Color = slint::Color::from_rgb_u8(0xDB, 0x27, 0x77);
const DIM: slint::Color = slint::Color::from_argb_u8(0x80, 0x18, 0x18, 0x1B);
const LABEL_BG: slint::Color = slint::Color::from_argb_u8(0xE6, 0x18, 0x18, 0x1B);

fn rect4(r: ViewRect) -> [f64; 4] {
    r.map(f64::from)
}

/// An accent outline just outside `r`.
fn outline(pl: &Placement, r: ViewRect) -> OverlayItem {
    OverlayItem {
        kind: 1,
        halo: true,
        width: 0.0,
        ..at(pl, rect4([r[0] - 1.0, r[1] - 1.0, r[2] + 2.0, r[3] + 2.0]))
    }
}

/// A raster as an image for the overlay.
fn image(r: &Raster) -> Image {
    Image::from_rgba8_premultiplied(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &r.pixels, r.width, r.height,
    ))
}

fn image_item(pl: &Placement, r: Rect, image: Image) -> OverlayItem {
    OverlayItem {
        kind: 4,
        image,
        ..at(pl, rect4([r.x0, r.y0, r.width(), r.height()]))
    }
}

/// Objects being dragged: the page where they were without them, and their own image
/// (and it upside down), moved, scaled, turned and mirrored with the drag.
#[derive(Debug)]
pub struct Dragged {
    page: usize,
    under: (Rect, Image),
    objects: (Rect, Image),
    flipped: Image,
    /// Device pixels per point they were rendered at.
    scale: f32,
}

impl Dragged {
    fn draw(&self, out: &mut Vec<OverlayItem>, pl: &Placement, view: &Matrix) {
        out.push(image_item(pl, self.under.0, self.under.1.clone()));
        let pose = ImagePose::of(view);
        let image = if pose.mirrored() {
            &self.flipped
        } else {
            &self.objects.1
        };
        out.push(OverlayItem {
            angle: pose.degrees,
            ..image_item(pl, pose.rect(view, self.objects.0), image.clone())
        });
    }
}

/// Device pixels per point of the view of `t` now.
pub fn view_scale(t: &Tab) -> f32 {
    (t.view.last.k * t.view.view.viewport.dpr()) as f32
}

/// Whether the zoom is still changing (a pinch or the wheel).
fn zooming(t: &Tab) -> bool {
    t.view.last.wake_at.is_some()
}

/// The scale the text being edited and dragged objects are shown at, when they are.
pub fn preview_made(t: &Tab) -> Option<f32> {
    t.edit.text.as_ref().and_then(|te| te.raster_scale()).or(t
        .edit
        .dragged
        .as_ref()
        .map(|d| d.scale))
}

/// Whether the previews were rendered for another zoom that has settled since: the
/// overlay must be drawn again to render them sharp.
pub fn previews_stale(t: &Tab) -> bool {
    preview_stale(view_scale(t), preview_made(t), zooming(t))
}

/// The selection of `page` rendered for a drag (in the tab's page colours).
pub fn dragged(t: &mut Tab, page: usize, backend: &dyn RenderBackend) -> Option<Dragged> {
    let objs = TabEdit::objs(t, page)?;
    let scale = view_scale(t);
    let theme = t.reading.state.theme;
    let d = drag_images(
        &t.session,
        &objs,
        page,
        &t.edit.sel.ids,
        backend,
        theme,
        scale,
    )?;
    Some(Dragged {
        page,
        under: (d.under.rect, image(&d.under)),
        objects: (d.objects.rect, image(&d.objects)),
        flipped: image(&d.flipped),
        scale: d.scale,
    })
}

/// A page-space quadrilateral as a closed path in view space.
fn quad_path(
    out: &mut Vec<OverlayItem>,
    pl: &Placement,
    objs: &PageObjects,
    q: &[Point; 4],
    m: Option<&pagebara_edit::Matrix>,
) {
    let pts: Vec<(f64, f64)> = q
        .iter()
        .map(|p| {
            let v = objs.frame.view_point(*p);
            let v = m.map_or(v, |m| m.apply(v));
            (f64::from(v.x), f64::from(v.y))
        })
        .collect();
    out.extend(
        path(
            pl,
            &pts,
            0.0,
            slint::Color::default(),
            slint::Color::from_argb_u8(0x14, 0x25, 0x63, 0xEB),
            true,
        )
        .map(|p| OverlayItem { halo: true, ..p }),
    );
}

/// The Edit tool's items for page `page` placed at `pl`; `backend` renders the page.
pub fn overlay(
    out: &mut Vec<OverlayItem>,
    t: &mut Tab,
    pl: &Placement,
    page: usize,
    backend: &dyn RenderBackend,
) {
    let Some(objs) = TabEdit::objs(t, page) else {
        return;
    };
    if t.edit.text.as_ref().is_some_and(|te| te.page == page) {
        return text(out, t, pl, &objs, backend);
    }
    let sel = TabEdit::current(t)
        .map(|(s, _)| s)
        .filter(|s| s.page == page);
    if let Some((hp, id)) = t.edit.hover
        && hp == page
        && !sel.as_ref().is_some_and(|s| s.contains(id))
        && let Some(ob) = objs.get(id)
    {
        out.push(outline(pl, view_rect(&objs, &ob.bbox)));
    }
    let preview = t.edit.preview.clone().filter(|p| p.page == page);
    if let Some((cp, id, r)) = t.edit.crop
        && cp == page
    {
        let r = match &preview {
            Some(p) => super::transformed(r, &p.result.view),
            None => r,
        };
        if let Some(img) = objs.get(id) {
            let b = view_rect(&objs, &img.bbox);
            let (x0, y0, x1, y1) = (b[0], b[1], b[0] + b[2], b[1] + b[3]);
            let (cx0, cy0, cx1, cy1) = (r[0], r[1], r[0] + r[2], r[1] + r[3]);
            for d in [
                [x0, y0, x1 - x0, cy0 - y0],
                [x0, cy1, x1 - x0, y1 - cy1],
                [x0, cy0, cx0 - x0, cy1 - cy0],
                [cx1, cy0, x1 - cx1, cy1 - cy0],
            ] {
                if d[2] > 0.0 && d[3] > 0.0 {
                    out.push(fill(pl, rect4(d), DIM));
                }
            }
        }
        out.push(OverlayItem {
            kind: 9,
            width: 0.0,
            ..at(pl, rect4(r))
        });
        return;
    }
    if let Some(sel) = &sel {
        match &preview {
            Some(p) => {
                // Rendered again for a zoom that has settled.
                if t.edit.dragged.is_some() && previews_stale(t) {
                    t.edit.dragged = dragged(t, page, backend);
                }
                if let Some(d) = t.edit.dragged.as_ref().filter(|d| d.page == page) {
                    d.draw(out, pl, &p.result.view);
                }
                for ob in sel.objects(&objs) {
                    quad_path(out, pl, &objs, &ob.quad, Some(&p.result.view));
                }
                for g in &p.result.guides {
                    let r = if g.vertical {
                        [g.at - 0.4, g.from, 0.8, g.to - g.from]
                    } else {
                        [g.from, g.at - 0.4, g.to - g.from, 0.8]
                    };
                    out.push(fill(pl, rect4(r), GUIDE));
                }
                let label = &p.result.label;
                let w = label.chars().count() as f32 * 5.6 + 10.0;
                out.push(OverlayItem {
                    kind: 3,
                    fill: LABEL_BG,
                    stroke: slint::Color::from_rgb_u8(0xFF, 0xFF, 0xFF),
                    text: label.as_str().into(),
                    font_size: 9.0,
                    ..at(pl, rect4([p.at.0 + 14.0, p.at.1 + 14.0, w, 15.0]))
                });
            }
            None => {
                if sel.ids.len() > 1 {
                    for ob in sel.objects(&objs) {
                        out.push(outline(pl, view_rect(&objs, &ob.bbox)));
                    }
                }
                for ob in sel.objects(&objs) {
                    // Rotated objects show their own frame inside the box.
                    let b = ob.bbox;
                    let on_box = ob.quad.iter().all(|p| {
                        ((p.x - b.x0).abs() < 0.5 || (p.x - b.x1).abs() < 0.5)
                            && ((p.y - b.y0).abs() < 0.5 || (p.y - b.y1).abs() < 0.5)
                    });
                    if !on_box {
                        quad_path(out, pl, &objs, &ob.quad, None);
                    }
                }
                if let Some(b) = sel.bbox(&objs) {
                    let pad = super::FRAME_PAD_PX / t.view.last.k.max(0.01) as f32;
                    out.push(OverlayItem {
                        kind: 9,
                        width: 1.0,
                        ..at(pl, rect4(outset(view_rect(&objs, &b), pad)))
                    });
                }
            }
        }
    }
    if let Some(Press::Marquee {
        page: mp, from, to, ..
    }) = &t.edit.press
        && *mp == page
    {
        let r = [
            from.0.min(to.0),
            from.1.min(to.1),
            (to.0 - from.0).abs(),
            (to.1 - from.1).abs(),
        ];
        if r[2] > 1.0 || r[3] > 1.0 {
            out.push(OverlayItem {
                kind: 7,
                ..at(pl, rect4(r))
            });
        }
    }
}

/// The paragraph being edited: the page there rendered with the edit (the engine draws it
/// as it will be written, over the page's own background and in its colours), the
/// selection and composition underline, the caret.
fn text(
    out: &mut Vec<OverlayItem>,
    t: &mut Tab,
    pl: &Placement,
    objs: &PageObjects,
    backend: &dyn RenderBackend,
) {
    let made = t.edit.text.as_ref().and_then(|te| te.raster_scale());
    let scale = preview_scale(view_scale(t), made, zooming(t));
    let theme = t.reading.state.theme;
    let edit = &mut t.edit;
    if let Some(te) = edit.text.as_mut()
        && let Some(r) = te.raster(&t.session, &mut edit.objects, backend, theme, scale)
    {
        let img = match &edit.text_image {
            Some((px, img)) if std::sync::Arc::ptr_eq(px, &r.pixels) => img.clone(),
            _ => {
                let img = image(&r);
                edit.text_image = Some((r.pixels.clone(), img.clone()));
                img
            }
        };
        out.push(image_item(pl, r.rect, img));
    }
    let Some(v) = &t.edit.text_view else { return };
    let to_view = objs.frame.to_view();
    let view = |p: Point| to_view.apply(p);
    for q in &v.selection {
        let r = pagebara_edit::Rect::from_points(&q.map(view));
        out.push(fill(
            pl,
            rect4([r.x0, r.y0, r.width(), r.height()]),
            SELECTION,
        ));
    }
    for q in &v.preedit {
        let a = view(q[0]);
        let b = view(q[1]);
        out.push(OverlayItem {
            halo: true,
            ..fill(
                pl,
                rect4([a.x.min(b.x), a.y.max(b.y), (b.x - a.x).abs(), 0.8]),
                slint::Color::default(),
            )
        });
    }
    let k = t.view.last.k.max(0.01);
    for m in &v.misspelled {
        for q in &m.quads {
            let (a, b, top) = (view(q[0]), view(q[1]), view(q[3]));
            out.extend(super::super::overlay::squiggle(
                pl,
                (a.x, a.y),
                (b.x, b.y),
                (top.x, top.y),
                k,
            ));
        }
    }
    let caret = v.caret.map(|c| (view(c.top), view(c.bottom)));
    if let Some(area) = super::text::area(v) {
        let mut f = objs.frame.view_rect(area);
        // The caret stands past the text after a space at the end of a line: the frame
        // takes it in.
        if let Some((a, b)) = caret {
            f.x0 = f.x0.min(a.x.min(b.x));
            f.x1 = f.x1.max(a.x.max(b.x));
            f.y0 = f.y0.min(a.y.min(b.y));
            f.y1 = f.y1.max(a.y.max(b.y));
        }
        out.push(OverlayItem {
            kind: 1,
            halo: true,
            width: 0.0,
            ..at(
                pl,
                rect4([f.x0 - 3.0, f.y0 - 3.0, f.width() + 6.0, f.height() + 6.0]),
            )
        });
    }
    if let Some((a, b)) = caret {
        out.push(OverlayItem {
            kind: 10,
            ..at(
                pl,
                rect4([a.x.min(b.x), a.y.min(b.y), 0.01, (a.y - b.y).abs()]),
            )
        });
    }
}
