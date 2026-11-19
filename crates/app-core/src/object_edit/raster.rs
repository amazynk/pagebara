//! Parts of a page rendered as they are or will be, for previews drawn over the page: the
//! paragraph being edited, and the objects being dragged with the page under them.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use pagebara_edit::{Matrix, ObjectId, Point, Rect, Target};
use pagebara_render::{Annotations, RenderRequest, Renderer};
use pagebara_syntax::Document;

use crate::page_theme::{PageTheme, ThemedRenderer};
use crate::session::Session;
use crate::tiles::{Job, Key, PixelRect, RenderBackend, RenderSource};

/// Pixels of a region of a page.
#[derive(Clone, Debug)]
pub struct Raster {
    /// The region in view space (points from the displayed page's top left).
    pub rect: Rect,
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA8, rows top to bottom.
    pub pixels: Arc<Vec<u8>>,
}

impl Raster {
    /// The same pixels upside down.
    pub fn flipped(&self) -> Raster {
        let row = self.width as usize * 4;
        let mut pixels = Vec::with_capacity(self.pixels.len());
        for r in self.pixels.chunks_exact(row.max(1)).rev() {
            pixels.extend_from_slice(r);
        }
        Raster {
            pixels: Arc::new(pixels),
            ..self.clone()
        }
    }
}

/// How view-space matrix `m` (scaling and turning, no shearing) shows an image: scaled by
/// `sx` along its width and `sy` along its height, turned clockwise by `degrees` about its
/// centre; a negative `sy` shows it upside down (a mirror image once turned).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImagePose {
    pub sx: f32,
    pub sy: f32,
    pub degrees: f32,
}

impl ImagePose {
    pub fn of(m: &Matrix) -> ImagePose {
        let sx = m.a.hypot(m.b);
        let sy = if sx > 0.0 { m.determinant() / sx } else { 0.0 };
        ImagePose {
            sx,
            sy,
            degrees: m.b.atan2(m.a).to_degrees(),
        }
    }

    /// Where `r` goes (unturned, the turn being about its centre): its centre moved by
    /// `m`, its size scaled.
    pub fn rect(&self, m: &Matrix, r: Rect) -> Rect {
        let c = m.apply(Point::new((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0));
        let (w, h) = (r.width() * self.sx, r.height() * self.sy.abs());
        Rect::new(c.x - w / 2.0, c.y - h / 2.0, c.x + w / 2.0, c.y + h / 2.0)
    }

    pub fn mirrored(&self) -> bool {
        self.sy < 0.0
    }
}

/// The pixels covering view rectangle `v` grown by `pad`, within the page; with the region
/// they cover exactly.
pub(crate) fn pixel_rect(
    s: &Session,
    page: usize,
    v: Rect,
    pad: f32,
    scale: f32,
) -> Option<(PixelRect, Rect)> {
    let (pw, ph) = s.geoms.get(page)?.size();
    let (pw, ph) = (pw as f32, ph as f32);
    let px = |v: f32, max: f32| f64::from(v.clamp(0.0, max) * scale);
    let x0 = px(v.x0 - pad, pw).floor();
    let y0 = px(v.y0 - pad, ph).floor();
    let x1 = px(v.x1 + pad, pw).ceil();
    let y1 = px(v.y1 + pad, ph).ceil();
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let k = f64::from(scale);
    let rect = PixelRect {
        x: x0 as u32,
        y: y0 as u32,
        w: (x1 - x0) as u32,
        h: (y1 - y0) as u32,
    };
    let area = Rect::new(
        (x0 / k) as f32,
        (y0 / k) as f32,
        (x1 / k) as f32,
        (y1 / k) as f32,
    );
    Some((rect, area))
}

/// `rect` of `page` of `doc` (a version of the session's document) as the view shows it:
/// by the renderer `backend` makes, form fields as displayed, in `theme`'s colours.
pub(crate) fn render_page(
    s: &Session,
    doc: &Document,
    page: usize,
    (rect, area): (PixelRect, Rect),
    backend: &dyn RenderBackend,
    theme: PageTheme,
    scale: f32,
) -> Option<Raster> {
    let r = ThemedRenderer::wrap(
        backend.renderer(RenderSource {
            doc: crate::forms::for_display(doc),
            refs: s.tree.refs(),
        }),
        theme,
    );
    let job = Job {
        key: Key::preview(page),
        page,
        page_size: s.geoms.get(page)?.size(),
        scale: f64::from(scale),
        rect,
    };
    let mut pixels = vec![0; rect.w as usize * rect.h as usize * 4];
    r.render(&job, &AtomicBool::new(false), &mut pixels)
        .then(|| Raster {
            rect: area,
            width: rect.w,
            height: rect.h,
            pixels: Arc::new(pixels),
        })
}

/// The scale (device pixels per point) to render a preview at, `view` being the view's
/// now: during a zoom gesture a preview already rendered at `made` stays, shown scaled,
/// so pinching does not render it again every frame; once the zoom settles it is
/// rendered sharp at the new scale.
pub fn preview_scale(view: f32, made: Option<f32>, zooming: bool) -> f32 {
    match made {
        Some(m) if zooming => m,
        _ => view,
    }
}

/// Whether a preview rendered at `made` should be rendered again for the view at `view`.
pub fn preview_stale(view: f32, made: Option<f32>, zooming: bool) -> bool {
    made.is_some_and(|m| preview_scale(view, made, zooming) != m)
}

/// What dragging objects shows: the objects alone (transparent around them) to move, and
/// the page where they were without them.
#[derive(Clone, Debug)]
pub struct DragImages {
    pub objects: Raster,
    /// `objects` upside down, for a drag that mirrors them.
    pub flipped: Raster,
    pub under: Raster,
    /// The scale they were rendered at.
    pub scale: f32,
}

/// Renders the top-level objects `ids` of `page` for a drag, at `scale` device pixels per
/// point in `theme`'s colours; `objs` are the page's objects in the session's document.
pub fn drag_images(
    s: &Session,
    objs: &pagebara_edit::PageObjects,
    page: usize,
    ids: &[ObjectId],
    backend: &dyn RenderBackend,
    theme: PageTheme,
    scale: f32,
) -> Option<DragImages> {
    let bbox = ids
        .iter()
        .filter_map(|&i| objs.get(i).map(|o| o.bbox))
        .reduce(|a, b| a.union(&b))?;
    let region = pixel_rect(s, page, objs.frame.view_rect(bbox), 2.0, scale)?;
    let targets: Vec<Target> = ids.iter().map(|&i| Target::Object(i)).collect();
    let mut without = s.doc().clone();
    pagebara_edit::delete(&mut without, page, &targets).ok()?;
    let under = render_page(s, &without, page, region, backend, theme, scale)?;
    // The objects alone: everything else deleted, drawn on nothing.
    let others: Vec<Target> = objs
        .top_level()
        .filter(|o| !ids.contains(&o.id))
        .map(|o| Target::Object(o.id))
        .collect();
    let mut alone = s.doc().clone();
    if !others.is_empty() {
        pagebara_edit::delete(&mut alone, page, &others).ok()?;
    }
    let (rect, area) = region;
    let mut req = RenderRequest::new(
        scale,
        pagebara_render::PixelRect {
            x: rect.x as i32,
            y: rect.y as i32,
            w: rect.w,
            h: rect.h,
        },
    );
    req.background = None;
    req.annotations = Annotations::None;
    let mut img = Renderer::new(alone).render(page, &req).ok()?;
    theme.apply(&mut img.data);
    let objects = Raster {
        rect: area,
        width: rect.w,
        height: rect.h,
        pixels: Arc::new(img.data),
    };
    Some(DragImages {
        flipped: objects.flipped(),
        objects,
        under,
        scale,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::edit::PageEditing;
    use crate::test_pdf;

    #[test]
    fn poses_of_mirrored_and_turned_matrices() {
        let r = Rect::new(0.0, 0.0, 20.0, 10.0);
        // Mirrored left to right about x = 30: upside down, turned half round.
        let m = pagebara_edit::geom::scale_about(Point::new(30.0, 0.0), -1.0, 1.0);
        let p = ImagePose::of(&m);
        assert!(p.mirrored());
        assert!((p.degrees.abs() - 180.0).abs() < 1e-3);
        assert_eq!(p.rect(&m, r), Rect::new(40.0, 0.0, 60.0, 10.0));
        // Flipped top to bottom: upside down, not turned.
        let p = ImagePose::of(&pagebara_edit::geom::scale_about(
            Point::new(0.0, 0.0),
            2.0,
            -0.5,
        ));
        assert!(p.mirrored() && p.degrees.abs() < 1e-3);
        assert_eq!((p.sx, p.sy), (2.0, -0.5));
        // Turned a quarter: neither.
        let p = ImagePose::of(&pagebara_edit::geom::rotate_about(
            Point::new(0.0, 0.0),
            90.0,
        ));
        assert!(!p.mirrored() && (p.degrees - 90.0).abs() < 1e-3);
        let px = Raster {
            rect: r,
            width: 1,
            height: 2,
            pixels: Arc::new(vec![1, 1, 1, 1, 2, 2, 2, 2]),
        };
        assert_eq!(*px.flipped().pixels, vec![2, 2, 2, 2, 1, 1, 1, 1]);
    }

    #[test]
    fn previews_keep_their_scale_while_zooming() {
        assert_eq!(preview_scale(3.0, Some(2.0), true), 2.0);
        assert_eq!(preview_scale(3.0, Some(2.0), false), 3.0);
        assert_eq!(preview_scale(3.0, None, true), 3.0);
        assert!(!preview_stale(3.0, Some(2.0), true));
        assert!(preview_stale(3.0, Some(2.0), false));
        assert!(!preview_stale(2.0, Some(2.0), false));
        assert!(!preview_stale(2.0, None, false));
    }

    /// A dragged text block: its image is the text on nothing, the page under it is blank.
    #[test]
    fn drag_images_split_the_objects_from_the_page() {
        let d = Document::open(test_pdf::text(&["First line", "Second line"])).unwrap();
        let s = Session::new(None, d);
        let mut ed = PageEditing::new();
        let objs = ed.objects(&s, 0).unwrap();
        let id = objs.top_level().find(|o| o.text().is_some()).unwrap().id;
        let backend = crate::tiles::EngineBackend;
        let d = drag_images(&s, &objs, 0, &[id], &backend, PageTheme::Normal, 2.0).unwrap();
        let px = |r: &Raster| r.pixels.chunks(4).map(|p| [p[0], p[3]]).collect::<Vec<_>>();
        let objects = px(&d.objects);
        assert!(objects.iter().any(|p| p[1] > 200), "no ink");
        assert!(objects.iter().any(|p| p[1] == 0), "not transparent around");
        assert!(
            px(&d.under).iter().all(|p| p == &[255, 255]),
            "text left under"
        );
        assert_eq!(d.objects.rect, d.under.rect);
    }
}
