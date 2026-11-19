//! Edit mode: changing what the pages draw. A tap selects an object (a tap elsewhere lets
//! go), a finger on the selection moves it and on a corner resizes it, two fingers on it
//! resize and turn it; each change is one undo step when the fingers lift. The action bar
//! recolours, fades, arranges, duplicates, deletes and replaces images. A double tap on
//! text (or Edit text) edits it in place: a hidden text field takes the typing, so the
//! keyboard, input methods and the clipboard work as in any field, and the page shows the
//! new text over the old until Done writes it as one step.

use std::path::Path;
use std::rc::Rc;
use std::sync::Arc;

use pagebara_app_core::edit::{EditOp, ImageData, PageEditing};
use pagebara_app_core::object_edit::{self as oe, Drag, DragKind, Guide, Handle, Selection};
use pagebara_app_core::page_theme::PageTheme;
use pagebara_app_core::view::gesture::{self, PressState};
use pagebara_edit::hit::{self, HitOptions};
use pagebara_edit::{
    Color as PdfColor, ImageFit, Kind, Matrix, ObjectData, PageObject, PageObjects, Point, Rect,
    SimpleEncoder, Target, ZOrder,
};
use pagebara_i18n::tr;
use slint::{Color, ComponentHandle, Image, Rgba8Pixel, SharedPixelBuffer};

use super::marks::{at, fill};
use super::{App, Doc, State};
use crate::{AppState, Mark, MobileWindow, Tool};

/// How far (screen pixels) from a corner of the selection a touch resizes it.
const CORNER_REACH: f64 = 24.0;
/// Snapping distance while moving, in screen pixels.
const SNAP: f64 = 6.0;
/// The smallest a pinch makes an object, in points.
const MIN_SIZE: f32 = 6.0;
/// How far a duplicate lands from the original, in points (down and right).
const DUPLICATE_OFFSET: f32 = 16.0;

const GHOST: Color = Color::from_argb_u8(0x1F, 0x25, 0x63, 0xEB);
const GUIDE: Color = Color::from_rgb_u8(0xDB, 0x27, 0x77);

/// A drag or pinch in progress: what it would do.
#[derive(Clone, Debug)]
pub struct Preview {
    /// View space, for drawing; page space, to apply.
    pub view: Matrix,
    pub page: Matrix,
    pub guides: Vec<Guide>,
}

/// A finger on the pages in Edit mode.
#[derive(Clone, Debug)]
pub enum EditGesture {
    /// On the selection (or a corner of it): moves (or resizes) once the finger moves.
    Drag { page: usize, drag: Drag },
    /// Placing the caret or selecting text while editing.
    Caret,
}

/// Two fingers on the selection: its centre and frame in view space.
#[derive(Clone, Copy, Debug)]
pub struct PinchEdit {
    center: Point,
    rect: oe::ViewRect,
}

/// The Edit mode state of a document.
#[derive(Debug, Default)]
pub struct EditState {
    pub(super) pages: PageEditing,
    pub sel: Option<Selection>,
    /// The objects the selection's ids are of, and the session generation they were read in.
    pub(super) objs: Option<(u64, Arc<PageObjects>)>,
    pub preview: Option<Preview>,
    pub text: Option<super::text_edit::Editing>,
    /// The image picker was opened to replace the selected image.
    pub replacing: bool,
    /// The selection as images while it is dragged or pinched.
    dragged: Option<Dragged>,
}

/// Objects being dragged: the page where they were without them, and their own image
/// (and it upside down), moved, scaled, turned and mirrored with the gesture.
#[derive(Debug)]
struct Dragged {
    page: usize,
    under: (Rect, Image),
    objects: (Rect, Image),
    flipped: Image,
    /// Device pixels per point they were rendered at.
    scale: f32,
}

impl Dragged {
    fn marks(&self, pl: &pagebara_app_core::view::Placement, view: &Matrix, out: &mut Vec<Mark>) {
        let item = |r: Rect, image: &Image| Mark {
            image: image.clone(),
            ..at(pl, [r.x0, r.y0, r.width(), r.height()].map(f64::from), 5)
        };
        out.push(item(self.under.0, &self.under.1));
        let pose = oe::ImagePose::of(view);
        let image = if pose.mirrored() {
            &self.flipped
        } else {
            &self.objects.1
        };
        out.push(Mark {
            angle: pose.degrees,
            ..item(pose.rect(view, self.objects.0), image)
        });
    }
}

/// Device pixels per point of the view now.
fn view_scale(d: &Doc) -> f32 {
    (d.view.last.k * d.view.view.viewport.dpr()) as f32
}

/// Whether the zoom is still changing (a pinch).
fn zooming(d: &Doc) -> bool {
    d.view.last.wake_at.is_some()
}

/// The scale the text being edited or the dragged objects are shown at, when they are.
fn preview_made(d: &Doc) -> Option<f32> {
    d.edit.text.as_ref().and_then(|e| e.te.raster_scale()).or(d
        .edit
        .dragged
        .as_ref()
        .map(|g| g.scale))
}

/// The scale to render previews at now (see [`oe::preview_scale`]).
pub(super) fn preview_scale(d: &Doc, made: Option<f32>) -> f32 {
    oe::preview_scale(view_scale(d), made, zooming(d))
}

/// Whether the previews were rendered for another zoom that has settled since: the marks
/// must be made again to render them sharp.
pub fn previews_stale(d: &Doc) -> bool {
    oe::preview_stale(view_scale(d), preview_made(d), zooming(d))
}

/// A rendered region as an image for the marks.
pub(super) fn raster_image(r: &oe::Raster) -> Image {
    Image::from_rgba8_premultiplied(SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(
        &r.pixels, r.width, r.height,
    ))
}

impl EditState {
    /// The selection and its objects, when something is selected.
    fn selected(&self) -> Option<(&Selection, &Arc<PageObjects>)> {
        let sel = self.sel.as_ref().filter(|s| !s.is_empty())?;
        Some((sel, &self.objs.as_ref()?.1))
    }

    pub fn editing_text(&self) -> bool {
        self.text.is_some()
    }

    pub fn clear(&mut self) {
        self.sel = None;
        self.preview = None;
        self.text = None;
    }
}

/// Whether the document allows changing its content.
pub(super) fn may_edit(d: &Doc) -> bool {
    d.session.doc().permissions().modify
}

pub(super) fn view_rect(objs: &PageObjects, r: Rect) -> oe::ViewRect {
    let v = objs.frame.view_rect(r);
    [v.x0, v.y0, v.x1 - v.x0, v.y1 - v.y0]
}

fn rect_f64(r: oe::ViewRect) -> [f64; 4] {
    r.map(f64::from)
}

/// Whether view point `(u, v)` is in `r` grown by `pad`.
pub(super) fn inside(r: oe::ViewRect, u: f64, v: f64, pad: f64) -> bool {
    let [x, y, w, h] = rect_f64(r);
    u >= x - pad && v >= y - pad && u <= x + w + pad && v <= y + h + pad
}

fn pdf_colour(c: Color) -> PdfColor {
    let f = |v: u8| f32::from(v) / 255.0;
    PdfColor::Rgb(f(c.red()), f(c.green()), f(c.blue()))
}

pub(super) fn slint_colour(rgb: [f32; 3]) -> Color {
    let b = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color::from_rgb_u8(b(rgb[0]), b(rgb[1]), b(rgb[2]))
}

/// A page object's fill and stroke colours, as far as it has them.
pub fn paints(objs: &PageObjects, ob: &PageObject) -> (Option<[f32; 3]>, Option<[f32; 3]>) {
    match &ob.data {
        ObjectData::Path(p) => (
            p.fill.as_ref().and_then(|f| f.rgb()),
            p.stroke.as_ref().and_then(|f| f.rgb()),
        ),
        ObjectData::Text(_) => (oe::text_look(objs, ob).map(|l| l.color), None),
        _ => (None, None),
    }
}

/// The image's bytes as the editor takes them (PNG or JPEG).
fn image_data(path: &Path) -> Result<ImageData, String> {
    let bytes = std::fs::read(path).map_err(|e| tr!("The picture could not be read: {}.", e))?;
    ImageData::from_file(bytes).ok_or_else(|| tr!("Pick a PNG or JPEG picture."))
}

pub fn wire(app: &Rc<App>, ui: &MobileWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_edit_colour(move |c| a.act(move |app, s, ui| app.edit_colour(s, ui, c)));
    let a = app.clone();
    st.on_edit_opacity(move |p| a.act(move |app, s, ui| app.edit_opacity(s, ui, p)));
    let a = app.clone();
    st.on_edit_arrange(move |how| a.act(move |app, s, ui| app.edit_arrange(s, ui, how)));
    let a = app.clone();
    st.on_edit_duplicate(move || a.act(|app, s, ui| app.edit_duplicate(s, ui)));
    let a = app.clone();
    st.on_edit_delete(move || a.act(|app, s, ui| app.edit_delete(s, ui)));
    let a = app.clone();
    st.on_edit_replace_image(move || a.act(|app, s, _| app.edit_pick_image(s)));
    let a = app.clone();
    st.on_edit_text_start(move || a.act(|app, s, ui| app.edit_text_start(s, ui)));
    let a = app.clone();
    st.on_edit_text_input(move |text, cursor, anchor, preedit| {
        a.act(move |app, s, ui| {
            app.edit_text_input(
                s,
                ui,
                &text,
                cursor.max(0) as usize,
                anchor.max(0) as usize,
                &preedit,
            )
        })
    });
    let a = app.clone();
    st.on_edit_text_done(move |commit| a.act(move |app, s, ui| app.edit_text_done(s, ui, commit)));
    let a = app.clone();
    st.on_edit_text_move(move |m, extend| {
        a.act(move |app, s, ui| app.edit_text_move(s, ui, m, extend))
    });
    let a = app.clone();
    st.on_edit_text_style(move |w| a.act(move |app, s, ui| app.edit_text_style(s, ui, w)));
}

impl App {
    /// Selects `ids` of `page` (nothing when empty).
    pub(crate) fn edit_select(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        page: usize,
        ids: Vec<pagebara_edit::ObjectId>,
    ) {
        let Some(d) = &mut s.doc else { return };
        if ids.is_empty() {
            d.edit.sel = None;
        } else {
            match d.edit.pages.objects(&d.session, page) {
                Ok(objs) => {
                    d.edit.objs = Some((d.session.generation(), objs));
                    d.edit.sel = Some(Selection::new(page, ids));
                }
                Err(e) => {
                    let text = tr!("This page's content cannot be edited: {}.", e);
                    return self.message(s, &text);
                }
            }
        }
        d.edit.replacing = false;
        ui.global::<AppState>().set_edit_panel(0);
        d.marks_dirty = true;
        self.refresh(s, ui);
    }

    /// Selects object `n` (1-based, painting order) of `page`, for scripts.
    pub(crate) fn edit_select_nth(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        page: usize,
        n: usize,
    ) -> Result<(), String> {
        let d = s.doc.as_mut().ok_or("no document is open")?;
        let objs = d
            .edit
            .pages
            .objects(&d.session, page)
            .map_err(|e| e.to_string())?;
        let id = objs
            .top_level()
            .nth(n.saturating_sub(1))
            .map(|o| o.id)
            .ok_or_else(|| format!("page {} has no object {n}", page + 1))?;
        self.edit_select(s, ui, page, vec![id]);
        Ok(())
    }

    /// The objects of `page` now.
    pub(crate) fn edit_objects(d: &mut Doc, page: usize) -> Result<Arc<PageObjects>, String> {
        d.edit
            .pages
            .objects(&d.session, page)
            .map_err(|e| e.to_string())
    }

    /// A finger went down on `page` at view point `(u, v)` (screen `(x, y)`): takes it when
    /// it is on the selection or the text being edited.
    pub(super) fn edit_down(
        s: &mut State,
        page: usize,
        u: f64,
        v: f64,
        x: f64,
        y: f64,
    ) -> Option<EditGesture> {
        let d = s.doc.as_mut()?;
        let k = d.view.last.k.max(0.01);
        if d.edit.text.is_some() {
            return super::text_edit::down(d, page, u, v, x, y);
        }
        let (sel, objs) = d.edit.selected()?;
        if sel.page != page {
            return None;
        }
        let rect = view_rect(objs, sel.bbox(objs)?);
        let tol = (CORNER_REACH / k) as f32;
        let kind = match oe::handle_at(rect, (u as f32, v as f32), tol, 0.0) {
            Some(h @ Handle::Corner(_)) => DragKind::Scale(h),
            _ if inside(rect, u, v, gesture::SLOP / k) => DragKind::Move,
            _ => return None,
        };
        let mut drag = Drag::new(kind, objs.frame, (u as f32, v as f32), rect);
        drag.others = objs
            .top_level()
            .filter(|o| !o.hidden && !sel.contains(o.id))
            .map(|o| view_rect(objs, o.bbox))
            .collect();
        Some(EditGesture::Drag { page, drag })
    }

    /// The finger moved to screen `(x, y)`.
    pub(super) fn edit_moved(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        g: &EditGesture,
        x: f64,
        y: f64,
        state: PressState,
    ) {
        let Some(d) = &mut s.doc else { return };
        let k = d.view.last.k.max(0.01);
        match g {
            EditGesture::Drag { page, drag } => {
                if state != PressState::Drag {
                    return;
                }
                let Some((u, v)) = d.view.screen_to_page_of(*page, x, y) else {
                    return;
                };
                // Corners keep the proportions.
                let keep = matches!(drag.kind, DragKind::Scale(_));
                let r = drag.update((u as f32, v as f32), keep, (SNAP / k) as f32);
                ui.global::<AppState>()
                    .set_edit_description(r.label.as_str().into());
                d.edit.preview = Some(Preview {
                    view: r.view,
                    page: r.matrix,
                    guides: r.guides,
                });
            }
            EditGesture::Caret => return self.text_moved(s, ui, x, y),
        }
        d.marks_dirty = true;
        self.refresh(s, ui);
    }

    /// The finger lifted at screen `(x, y)`; `tap` when it did not move.
    pub(super) fn edit_up(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        g: EditGesture,
        (x, y): (f64, f64),
        tap: bool,
        now: std::time::Instant,
    ) {
        let Some(d) = &mut s.doc else { return };
        let preview = d.edit.preview.take();
        match g {
            EditGesture::Drag { .. } => match preview {
                Some(p) if !tap => {
                    let targets = d
                        .edit
                        .sel
                        .as_ref()
                        .map(Selection::targets)
                        .unwrap_or_default();
                    self.edit_apply(s, ui, EditOp::Transform(targets, p.page), p.page);
                }
                _ => self.edit_tap(s, ui, x, y, now),
            },
            EditGesture::Caret => self.text_up(s, ui, (x, y), tap, now),
        }
    }

    /// A tap at screen `(x, y)` in Edit mode: selects what is there (a double tap on text
    /// edits it), lets go of the selection elsewhere, ends text editing outside the text.
    pub(super) fn edit_tap(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        x: f64,
        y: f64,
        now: std::time::Instant,
    ) {
        let double = s.input.taps.tap(x, y, now);
        let Some(d) = &mut s.doc else { return };
        if d.edit.text.is_some() {
            return self.edit_text_done(s, ui, true);
        }
        let Some((page, u, v)) = d.view.screen_to_page(x, y) else {
            return self.edit_select(s, ui, 0, Vec::new());
        };
        let objs = match Self::edit_objects(d, page) {
            Ok(o) => o,
            Err(e) => {
                return self.message(s, &tr!("This page's content cannot be edited: {}.", e));
            }
        };
        let k = d.view.last.k.max(0.01);
        let opts = HitOptions {
            tolerance: (gesture::SLOP / k) as f32,
            ..HitOptions::default()
        };
        let p = objs.frame.page_point(Point::new(u as f32, v as f32));
        let hit = hit::object_at(&objs, p, &opts);
        let selected = d
            .edit
            .sel
            .as_ref()
            .is_some_and(|s| s.page == page && hit.is_some_and(|h| s.contains(h)));
        match hit {
            Some(id) if double && objs.get(id).is_some_and(|o| o.kind == Kind::Text) => {
                if !selected {
                    self.edit_select(s, ui, page, vec![id]);
                }
                self.edit_text_start(s, ui);
            }
            Some(_) if selected => {}
            Some(id) => self.edit_select(s, ui, page, vec![id]),
            None => self.edit_select(s, ui, page, Vec::new()),
        }
    }

    /// Two fingers came down: they act on the selection when it is under them (decided
    /// at their first move). True when the view should not zoom yet.
    pub(super) fn edit_pinch_start(s: &mut State, tool: Tool) -> bool {
        tool == Tool::Edit
            && s.doc
                .as_ref()
                .is_some_and(|d| d.edit.selected().is_some() && d.edit.text.is_none())
    }

    /// Whether two fingers centred at screen `(x, y)` are on the selection; starts acting on it.
    pub(super) fn edit_pinch_begin(s: &mut State, x: f64, y: f64) -> Option<PinchEdit> {
        let d = s.doc.as_mut()?;
        let (sel, objs) = d.edit.selected()?;
        let (u, v) = d.view.screen_to_page_of(sel.page, x, y)?;
        let rect = view_rect(objs, sel.bbox(objs)?);
        let k = d.view.last.k.max(0.01);
        inside(rect, u, v, CORNER_REACH / k).then(|| PinchEdit {
            center: Point::new(rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0),
            rect,
        })
    }

    /// Two fingers on the selection: scaled by `scale` and turned by `degrees`.
    pub(super) fn edit_pinch(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        p: PinchEdit,
        scale: f64,
        degrees: f64,
    ) {
        let Some(d) = &mut s.doc else { return };
        let Some(objs) = d.edit.objs.as_ref().map(|o| o.1.clone()) else {
            return;
        };
        let longest = p.rect[2].max(p.rect[3]).max(0.01);
        let sc = (scale as f32).max(MIN_SIZE / longest).min(20.0);
        // Small turns are a wobble of the fingers, not a rotation.
        let deg = if degrees.abs() < 4.0 {
            0.0
        } else {
            degrees as f32
        };
        let view = pagebara_edit::geom::scale_about(p.center, sc, sc)
            .then(&pagebara_edit::geom::rotate_about(p.center, deg));
        let label = if deg == 0.0 {
            tr!(
                "{} by {} points",
                format!("{:.0}", p.rect[2] * sc),
                format!("{:.0}", p.rect[3] * sc)
            )
        } else {
            tr!(
                "{}%, turned {}°",
                format!("{:.0}", sc * 100.0),
                format!("{deg:.0}")
            )
        };
        ui.global::<AppState>()
            .set_edit_description(label.as_str().into());
        d.edit.preview = Some(Preview {
            view,
            page: oe::to_page(&objs.frame, &view),
            guides: Vec::new(),
        });
        d.marks_dirty = true;
        self.refresh(s, ui);
    }

    /// The fingers lifted: applies the pinch.
    pub(super) fn edit_pinch_end(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(d) = &mut s.doc else { return };
        let Some(p) = d.edit.preview.take() else {
            return;
        };
        if pagebara_edit::geom::is_identity(&p.page) {
            d.marks_dirty = true;
            return self.refresh(s, ui);
        }
        let targets = d
            .edit
            .sel
            .as_ref()
            .map(Selection::targets)
            .unwrap_or_default();
        self.edit_apply(s, ui, EditOp::Transform(targets, p.page), p.page);
    }

    /// Applies `op` to the selection's page as one undo step; the selection follows its
    /// objects (`m`: where the operation moved them, in page space).
    pub(crate) fn edit_apply(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        op: EditOp,
        m: Matrix,
    ) {
        let Some(d) = &mut s.doc else { return };
        let Some(page) = d.edit.sel.as_ref().map(|s| s.page) else {
            return;
        };
        if !may_edit(d) {
            return self.message(
                s,
                &tr!("This document's security settings do not allow editing."),
            );
        }
        let res = d
            .edit
            .pages
            .apply(&mut d.session, page, &op, &SimpleEncoder);
        match res {
            Ok(_) => {
                if matches!(op, EditOp::Delete(_)) {
                    d.edit.sel = None;
                } else {
                    reselect(d, &m);
                }
            }
            Err(e) => self.message(s, &tr!("That did not work: {}.", e)),
        }
        self.after_edit(s, ui);
    }

    fn edit_targets(s: &State) -> Option<(Vec<Target>, Arc<PageObjects>, Selection)> {
        let (sel, objs) = s.doc.as_ref()?.edit.selected()?;
        Some((sel.targets(), objs.clone(), sel.clone()))
    }

    pub(crate) fn edit_colour(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, c: Color) {
        let Some((targets, objs, sel)) = Self::edit_targets(s) else {
            return;
        };
        let st = ui.global::<AppState>();
        let (fills, strokes) = can_paint(&objs, &sel);
        let stroke = strokes && (st.get_edit_stroke_mode() || !fills);
        let op = if stroke {
            EditOp::Stroke(targets, pdf_colour(c))
        } else {
            EditOp::Fill(targets, pdf_colour(c))
        };
        self.edit_apply(s, ui, op, Matrix::IDENTITY);
    }

    pub(crate) fn edit_opacity(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, percent: i32) {
        let Some((targets, ..)) = Self::edit_targets(s) else {
            return;
        };
        let a = (percent.clamp(0, 100) as f32) / 100.0;
        self.edit_apply(
            s,
            ui,
            EditOp::Opacity(targets, Some(a), Some(a)),
            Matrix::IDENTITY,
        );
    }

    pub(crate) fn edit_arrange(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, how: i32) {
        let Some((targets, ..)) = Self::edit_targets(s) else {
            return;
        };
        let z = match how {
            0 => ZOrder::Front,
            1 => ZOrder::Forward,
            2 => ZOrder::Backward,
            _ => ZOrder::Back,
        };
        self.edit_apply(s, ui, EditOp::Reorder(targets, z), Matrix::IDENTITY);
    }

    pub(crate) fn edit_duplicate(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some((targets, objs, _)) = Self::edit_targets(s) else {
            return;
        };
        let m = oe::move_by_view(&objs.frame, DUPLICATE_OFFSET, DUPLICATE_OFFSET);
        // The selection moves to the copy.
        self.edit_apply(s, ui, EditOp::Duplicate(targets, m), m);
    }

    pub(crate) fn edit_delete(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some((targets, ..)) = Self::edit_targets(s) else {
            return;
        };
        self.edit_apply(s, ui, EditOp::Delete(targets), Matrix::IDENTITY);
    }

    fn edit_pick_image(self: &Rc<Self>, s: &mut State) {
        let Some(d) = &mut s.doc else { return };
        if d.edit
            .selected()
            .and_then(|(sel, objs)| sel.single_image(objs))
            .is_none()
        {
            return;
        }
        d.edit.replacing = true;
        self.platform.pick_image();
    }

    /// Puts the picture at `path` in place of the selected image.
    pub(crate) fn edit_replace_image(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        path: &Path,
    ) {
        let Some(d) = &mut s.doc else { return };
        d.edit.replacing = false;
        let Some(id) = d
            .edit
            .selected()
            .and_then(|(sel, objs)| sel.single_image(objs))
            .map(|o| o.id)
        else {
            return self.message(s, &tr!("Select an image to replace first."));
        };
        match image_data(path) {
            Ok(img) => self.edit_apply(
                s,
                ui,
                EditOp::ReplaceImage(Target::Object(id), img, ImageFit::Contain),
                Matrix::IDENTITY,
            ),
            Err(e) => self.message(s, &e),
        }
    }
}

/// What colours the selection can change: fills, strokes.
fn can_paint(objs: &PageObjects, sel: &Selection) -> (bool, bool) {
    let mut out = (false, false);
    for ob in sel.objects(objs) {
        match &ob.data {
            ObjectData::Path(p) => {
                out.0 |= p.fill.is_some();
                out.1 |= p.stroke.is_some();
            }
            ObjectData::Text(_) => out.0 = true,
            _ => {}
        }
    }
    out
}

/// After text block `block` was rewritten (it may be wider or have more lines): selects
/// the text block that overlaps it most now.
pub(super) fn reselect_text(d: &mut Doc, block: pagebara_edit::ObjectId) {
    let Some((_, old)) = d.edit.objs.clone() else {
        return;
    };
    let (Some(page), Some(was)) = (d.edit.sel.as_ref().map(|s| s.page), old.get(block)) else {
        return;
    };
    let Ok(new) = d.edit.pages.objects(&d.session, page) else {
        d.edit.clear();
        return;
    };
    let w = was.bbox;
    let overlap = |r: &Rect| {
        pagebara_edit::geom::intersect(r, &w)
            .map(|i| i.width() * i.height())
            .filter(|a| *a > 0.0)
    };
    let best = new
        .top_level()
        .filter(|o| o.kind == Kind::Text)
        .filter_map(|o| overlap(&o.bbox).map(|a| (a, o.id)))
        .max_by(|a, b| a.0.total_cmp(&b.0));
    d.edit.sel = best.map(|(_, id)| Selection::new(page, vec![id]));
    d.edit.objs = Some((d.session.generation(), new));
}

/// After an edit moved the selected objects by `m` (page space): finds them again.
fn reselect(d: &mut Doc, m: &Matrix) {
    let Some(sel) = d.edit.sel.clone() else {
        return;
    };
    let Some((_, old)) = d.edit.objs.clone() else {
        return;
    };
    let Ok(new) = d.edit.pages.objects(&d.session, sel.page) else {
        d.edit.clear();
        return;
    };
    let ids = oe::refind(&old, &new, &sel.ids, m);
    d.edit.sel = (!ids.is_empty()).then(|| Selection::new(sel.page, ids));
    d.edit.objs = Some((d.session.generation(), new));
}

/// After undo, redo or another change: the selection follows its objects, or goes.
pub fn sync(d: &mut Doc) {
    let stale = d
        .edit
        .objs
        .as_ref()
        .is_some_and(|(g, _)| *g != d.session.generation());
    if !stale {
        return;
    }
    if d.edit.text.is_some() {
        // The text changed under the editor: start again from what the page has.
        d.edit.text = None;
    }
    reselect(d, &Matrix::IDENTITY);
}

/// A closed outline through view points, as a path mark.
fn outline(
    pl: &pagebara_app_core::view::Placement,
    pts: &[(f64, f64)],
    k: f64,
    fill_c: Color,
) -> Mark {
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for &(x, y) in pts {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let mut cmd = String::new();
    for (i, &(x, y)) in pts.iter().enumerate() {
        let op = if i == 0 { 'M' } else { 'L' };
        cmd.push_str(&format!("{op}{:.2} {:.2} ", x - x0, y - y0));
    }
    cmd.push('Z');
    Mark {
        halo: true,
        fill: fill_c,
        width: (2.0 / k) as f32,
        commands: cmd.into(),
        ..at(pl, [x0, y0, (x1 - x0).max(0.01), (y1 - y0).max(0.01)], 2)
    }
}

/// What Edit mode draws over page `page`: the selection's frame (following a drag or pinch,
/// with snapping guides), or the text being edited with its caret and selection.
pub fn marks(
    d: &mut Doc,
    page: usize,
    pl: &pagebara_app_core::view::Placement,
    out: &mut Vec<Mark>,
    backend: &dyn pagebara_app_core::tiles::RenderBackend,
    theme: PageTheme,
) {
    if d.edit.text.is_some() {
        return super::text_edit::marks(d, page, pl, out, backend, theme);
    }
    let k = d.view.last.k.max(0.01);
    if d.edit.preview.is_none() {
        d.edit.dragged = None;
    } else if (d.edit.dragged.is_none() || previews_stale(d))
        && let Some((sel, objs)) = d.edit.selected().filter(|(s, _)| s.page == page)
    {
        let scale = view_scale(d);
        let ids = sel.ids.clone();
        let objs = objs.clone();
        d.edit.dragged =
            oe::drag_images(&d.session, &objs, page, &ids, backend, theme, scale).map(|i| {
                Dragged {
                    page,
                    under: (i.under.rect, raster_image(&i.under)),
                    objects: (i.objects.rect, raster_image(&i.objects)),
                    flipped: raster_image(&i.flipped),
                    scale: i.scale,
                }
            });
    }
    let Some((_, objs)) = &d.edit.objs else {
        return;
    };
    let Some((sel, _)) = d.edit.selected().filter(|(s, _)| s.page == page) else {
        return;
    };
    let Some(bbox) = sel.bbox(objs) else { return };
    let r = rect_f64(view_rect(objs, bbox));
    match &d.edit.preview {
        Some(p) => {
            if let Some(dr) = d.edit.dragged.as_ref().filter(|g| g.page == page) {
                dr.marks(pl, &p.view, out);
            }
            let [x, y, w, h] = r;
            let pts: Vec<(f64, f64)> = [(x, y), (x + w, y), (x + w, y + h), (x, y + h)]
                .iter()
                .map(|&(px, py)| {
                    let q = p.view.apply(Point::new(px as f32, py as f32));
                    (f64::from(q.x), f64::from(q.y))
                })
                .collect();
            out.push(outline(pl, &pts, k, GHOST));
            let t = 1.0 / k;
            for g in &p.guides {
                let (at_, from, to) = (f64::from(g.at), f64::from(g.from), f64::from(g.to));
                let b = if g.vertical {
                    [at_ - t / 2.0, from, t, to - from]
                } else {
                    [from, at_ - t / 2.0, to - from, t]
                };
                out.push(fill(pl, b, GUIDE));
            }
        }
        None => {
            // Each object of a group selection outlined, the whole in a frame with handles.
            if sel.ids.len() > 1 {
                for ob in sel.objects(objs) {
                    out.push(Mark {
                        halo: true,
                        ..at(pl, rect_f64(view_rect(objs, ob.bbox)), 3)
                    });
                }
            }
            out.push(at(pl, r, 1));
        }
    }
}

/// Shows what the action bar needs about the selection.
pub fn show(d: &mut Doc, ui: &MobileWindow) {
    if d.edit.text.is_some() {
        super::text_edit::show(d, ui);
    }
    let st = ui.global::<AppState>();
    let editing = d.edit.text.is_some();
    let selected = d.edit.selected();
    st.set_edit_selected(selected.is_some() && !editing);
    let Some((sel, objs)) = selected else {
        st.set_edit_description("".into());
        return;
    };
    if d.edit.preview.is_none() {
        st.set_edit_description(oe::describe_selection(objs, sel).into());
    }
    let (fills, strokes) = can_paint(objs, sel);
    st.set_edit_can_fill(fills);
    st.set_edit_can_stroke(strokes);
    if !fills {
        st.set_edit_stroke_mode(true);
    } else if !strokes {
        st.set_edit_stroke_mode(false);
    }
    st.set_edit_is_image(sel.single_image(objs).is_some());
    st.set_edit_is_text(sel.single_text(objs).is_some());
}

fn hex(rgb: [f32; 3]) -> String {
    let c = slint_colour(rgb);
    format!("#{:02x}{:02x}{:02x}", c.red(), c.green(), c.blue())
}

/// The opacity of an object's fill, when it has one.
fn alpha(objs: &PageObjects, ob: &PageObject) -> Option<f32> {
    match &ob.data {
        ObjectData::Path(p) => Some(p.fill_alpha),
        ObjectData::Image(i) => Some(i.alpha),
        ObjectData::Text(t) => t
            .runs()
            .next()
            .and_then(|r| objs.run(r))
            .map(|r| r.fill_alpha),
        _ => None,
    }
}

/// Edit mode values for script asserts: `edit.selected` (the selection described),
/// `edit.count`, `edit.kind`, `edit.x`/`y`/`w`/`h` (the selection's frame in view points),
/// `edit.fill`, `edit.stroke` (`#rrggbb`), `edit.opacity` (percent), `edit.mirrored` (an
/// image drawn mirrored), `edit.bold` (the
/// selected text starts bold), `edit.text` and `edit.text-lines` (the text being edited
/// and its lines as laid out), `edit.preview-scale` (device pixels per point the text
/// being edited or the dragged objects are shown at), `edit.preview-sharp` (that is the
/// view's scale now), `edit.objects` (objects of the current page), `edit.bbox:N` (object
/// N's frame, `x,y,w,h`) and `page.text:N` (the text of page N).
pub fn edit_query(d: &mut Doc, key: &str) -> Result<Option<String>, String> {
    let page_arg = |p: &str| p.parse::<usize>().unwrap_or(1).saturating_sub(1);
    if let Some(p) = key.strip_prefix("page.text:") {
        return Ok(Some(d.session.text.page(page_arg(p)).text.clone()));
    }
    if let Some(n) = key.strip_prefix("edit.bbox:") {
        let page = d.view.current_page;
        let objs = App::edit_objects(d, page)?;
        let n: usize = n.parse().map_err(|_| "edit.bbox needs a number")?;
        let ob = objs
            .top_level()
            .nth(n.saturating_sub(1))
            .ok_or_else(|| format!("page {} has no object {n}", page + 1))?;
        let [x, y, w, h] = view_rect(&objs, ob.bbox);
        return Ok(Some(format!("{x:.0},{y:.0},{w:.0},{h:.0}")));
    }
    if key == "edit.objects" {
        let objs = App::edit_objects(d, d.view.current_page)?;
        return Ok(Some(objs.top_level().count().to_string()));
    }
    if key == "edit.text" {
        return Ok(Some(
            d.edit
                .text
                .as_ref()
                .map(|e| e.te.text())
                .unwrap_or_default(),
        ));
    }
    if key == "edit.preview-scale" {
        return Ok(Some(
            preview_made(d).map_or(String::new(), |v| format!("{v:.2}")),
        ));
    }
    if key == "edit.preview-sharp" {
        return Ok(Some((preview_made(d) == Some(view_scale(d))).to_string()));
    }
    if key == "edit.text-lines" {
        return Ok(Some(
            d.edit.text.as_ref().map_or(0, |e| e.lines()).to_string(),
        ));
    }
    let selected = d.edit.selected();
    let first = selected.and_then(|(sel, objs)| sel.objects(objs).next().map(|o| (o, objs)));
    let frame = selected.and_then(|(sel, objs)| sel.bbox(objs).map(|b| view_rect(objs, b)));
    let num = |i: usize| frame.map_or(String::new(), |r| format!("{:.1}", r[i]));
    Ok(Some(match key {
        "edit.selected" => selected.map_or(String::new(), |(sel, objs)| {
            oe::describe_selection(objs, sel)
        }),
        "edit.count" => selected.map_or(0, |(sel, _)| sel.ids.len()).to_string(),
        "edit.kind" => first.map_or(String::new(), |(o, _)| oe::kind_name(o.kind).to_lowercase()),
        "edit.mirrored" => first.is_some_and(|(o, _)| oe::mirrored(o)).to_string(),
        "edit.bold" => first
            .and_then(|(o, objs)| oe::text_look(objs, o))
            .is_some_and(|l| l.bold)
            .to_string(),
        "edit.x" => num(0),
        "edit.y" => num(1),
        "edit.w" => num(2),
        "edit.h" => num(3),
        "edit.fill" => first
            .and_then(|(o, objs)| paints(objs, o).0)
            .map(hex)
            .unwrap_or_default(),
        "edit.stroke" => first
            .and_then(|(o, objs)| paints(objs, o).1)
            .map(hex)
            .unwrap_or_default(),
        "edit.opacity" => first
            .and_then(|(o, objs)| alpha(objs, o))
            .map_or(String::new(), |a| format!("{:.0}", a * 100.0)),
        _ => return Ok(None),
    }))
}

impl App {
    /// Two fingers on the selection scale it by `scale` and turn it by `degrees`, for scripts.
    pub(crate) fn edit_pinch_script(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        scale: f64,
        degrees: f64,
    ) -> Result<(), String> {
        let d = s.doc.as_ref().ok_or("no document is open")?;
        let (sel, objs) = d.edit.selected().ok_or("nothing is selected")?;
        let r = view_rect(objs, sel.bbox(objs).ok_or("nothing is selected")?);
        let p = PinchEdit {
            center: Point::new(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0),
            rect: r,
        };
        for i in 1..=8 {
            let f = f64::from(i) / 8.0;
            self.edit_pinch(s, ui, p, 1.0 + (scale - 1.0) * f, degrees * f);
        }
        self.edit_pinch_end(s, ui);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touches_near_a_frame_are_inside_it() {
        let r = [10.0, 20.0, 100.0, 50.0];
        assert!(inside(r, 10.0, 20.0, 0.0));
        assert!(inside(r, 5.0, 75.0, 6.0));
        assert!(!inside(r, 5.0, 75.0, 2.0));
    }

    #[test]
    fn colours_convert_both_ways() {
        let c = Color::from_rgb_u8(0xDC, 0x26, 0x26);
        let PdfColor::Rgb(r, g, b) = pdf_colour(c) else {
            panic!("not rgb");
        };
        assert_eq!(hex([r, g, b]), "#dc2626");
    }

    #[test]
    fn pictures_are_png_or_jpeg() {
        let dir = std::env::temp_dir().join(format!("pagebara-mobile-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("a.png");
        std::fs::write(&png, b"\x89PNG\r\n\x1a\n").unwrap();
        assert!(matches!(image_data(&png), Ok(ImageData::Png(_))));
        let jpg = dir.join("a.jpg");
        std::fs::write(&jpg, [0xFF, 0xD8, 0xFF]).unwrap();
        assert!(matches!(image_data(&jpg), Ok(ImageData::Jpeg(_))));
        let txt = dir.join("a.txt");
        std::fs::write(&txt, b"hello").unwrap();
        assert!(image_data(&txt).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
