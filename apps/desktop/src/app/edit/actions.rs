//! What the Edit tool's panel, menu and keys do with the selection: colours, opacity,
//! text size, arrangement, duplicating and deleting, images (add, replace, crop, save) and
//! new text.

use std::path::PathBuf;
use std::rc::Rc;

use pagebara_app_core::edit::{EditOp, ImageData, StyleChange};
use pagebara_app_core::object_edit::{self as oe, Drag, DragKind, Handle, Selection};
use pagebara_edit::{
    Color, ImageFit, Kind, Matrix, ObjectData, PageObjects, PathInfo, Point, Rect, Target,
    TextStyle, ZOrder,
};
use pagebara_i18n::{tr, tr_noop};
use slint::ComponentHandle;

use super::super::input::COLOURS;
use super::super::{App, State, pick};
use super::{Press, TabEdit, view_rect};
use crate::{AppWindow, EditState, ViewState};

/// The swatch nearest to a colour, -1 when none is close.
pub(super) fn swatch(rgb: Option<[f32; 3]>) -> i32 {
    let Some(c) = rgb else { return -1 };
    let [r, g, b] = c.map(|v| v * 255.0);
    COLOURS
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let d = (f32::from(p[0]) - r).powi(2)
                + (f32::from(p[1]) - g).powi(2)
                + (f32::from(p[2]) - b).powi(2);
            (i, d)
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .filter(|(_, d)| *d < 2000.0)
        .map_or(-1, |(i, _)| i as i32)
}

fn palette(i: i32) -> Color {
    let [r, g, b] = COLOURS[(i.max(0) as usize).min(COLOURS.len() - 1)];
    Color::Rgb(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    )
}

/// The fill colour and opacity of an object as shown.
pub fn fill_of(objs: &PageObjects, ob: &pagebara_edit::PageObject) -> (Option<[f32; 3]>, f32) {
    match &ob.data {
        ObjectData::Path(p) => (p.fill.as_ref().and_then(|f| f.rgb()), p.fill_alpha),
        ObjectData::Text(t) => t
            .runs()
            .next()
            .and_then(|r| objs.run(r))
            .map_or((None, 1.0), |r| (r.fill.rgb(), r.fill_alpha)),
        ObjectData::Image(i) => (i.is_mask.then(|| i.fill.rgb()).flatten(), i.alpha),
        _ => (None, 1.0),
    }
}

/// Whether a drawing's fill is switched off ("No fill"): it has one that is fully
/// transparent, while its line, when it has one, is not. A drawing whose line is
/// transparent too was faded out with the opacity slider: that is opacity 0, not this.
pub fn fill_off(p: &PathInfo) -> bool {
    p.fill.is_some() && p.fill_alpha <= 0.0 && (p.stroke.is_none() || p.stroke_alpha > 0.0)
}

/// The opacity of an object as the panel shows it: of its line when its fill is switched
/// off, of its fill otherwise.
pub fn opacity_of(objs: &PageObjects, ob: &pagebara_edit::PageObject) -> f32 {
    match ob.path() {
        Some(p) if fill_off(p) && p.stroke.is_some() => p.stroke_alpha,
        _ => fill_of(objs, ob).1,
    }
}

/// Fills the panel for the selection.
pub fn show(es: &EditState, objs: &PageObjects, sel: &Selection) {
    let obs: Vec<_> = sel.objects(objs).collect();
    let Some(first) = obs.first() else {
        es.set_count(0);
        return;
    };
    es.set_count(obs.len() as i32);
    es.set_summary(oe::describe_selection(objs, sel).into());
    let text = sel.single_text(objs);
    es.set_is_text(text.is_some());
    es.set_is_image(sel.single_image(objs).is_some());
    let path = obs.iter().find_map(|o| match &o.data {
        ObjectData::Path(p) => Some(p),
        _ => None,
    });
    es.set_has_fill(obs.iter().any(|o| match &o.data {
        ObjectData::Path(p) => p.fill.is_some(),
        ObjectData::Text(_) => true,
        ObjectData::Image(i) => i.is_mask,
        _ => false,
    }));
    es.set_has_stroke(path.is_some_and(|p| p.stroke.is_some()));
    let (fill, _) = fill_of(objs, first);
    // "No fill" is offered for drawings, and chosen when the first one's fill is off.
    let off = first.path().is_some_and(fill_off);
    es.set_fill_clearable(
        text.is_none()
            && obs
                .iter()
                .any(|o| o.path().is_some_and(|p| p.fill.is_some())),
    );
    es.set_fill_none(off);
    es.set_fill(if off { -1 } else { swatch(fill) });
    es.set_stroke(swatch(
        path.and_then(|p| p.stroke.as_ref()).and_then(|s| s.rgb()),
    ));
    es.set_opacity((opacity_of(objs, first) * 100.0).round());
    let width = path.map_or(0.0, |p| p.line_width * oe::scale_of(&p.ctm));
    es.set_line_width((width * 10.0).round() / 10.0);
    es.set_dashed(path.is_some_and(|p| p.dashed));
    if let Some(look) = text.and_then(|t| oe::text_look(objs, t)) {
        es.set_style_bold(i32::from(look.bold));
        es.set_style_italic(i32::from(look.italic));
        es.set_font(look.font.into());
        es.set_font_size((look.size * 10.0).round() / 10.0);
    }
}

/// Reads an image file for adding or replacing.
fn image_file(path: &PathBuf) -> Result<(ImageData, (u32, u32)), String> {
    let bytes = std::fs::read(path).map_err(|e| tr!("Could not read the image: {}.", e))?;
    let unreadable = || tr!("That is not a PNG or JPEG image this app can read.");
    let img = ImageData::from_file(bytes).ok_or_else(unreadable)?;
    let size = img.size().ok().filter(|&(w, h)| w > 0 && h > 0);
    Ok((img, size.ok_or_else(unreadable)?))
}

impl App {
    /// An action of the Edit tool's panel or menu, or of its keys.
    pub fn edit_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str, value: &str) {
        let v: f32 = value.parse().unwrap_or(0.0);
        match what {
            "text-done" => return self.edit_text_done(s, ui, true),
            "text-bold" => return self.text_style(s, ui, StyleChange::Bold(value == "1")),
            "text-italic" => return self.text_style(s, ui, StyleChange::Italic(value == "1")),
            "text-size" => return self.text_style(s, ui, StyleChange::Size(v)),
            "text-fill" => return self.text_style(s, ui, StyleChange::Fill(palette(v as i32))),
            "text-family" => return self.text_style(s, ui, StyleChange::Family(value.to_owned())),
            "edit-text" => return self.start_text_edit(s, ui, None),
            "add-text" => {
                if let Some(t) = s.tab() {
                    t.edit.placing_text = true;
                }
                ui.global::<ViewState>().set_cursor(3);
                return self.message(s, &tr!("Click on the page where the text goes, or press Enter to put it in the middle."));
            }
            "add-text-centre" => {
                let Some((page, at)) = self.view_centre(s) else {
                    return;
                };
                return self.new_text_box(s, ui, page, at);
            }
            "add-text-here" => {
                let Some((page, pt)) = s.input.menu_at else {
                    return;
                };
                let Some(t) = s.tab() else { return };
                let Some(objs) = TabEdit::objs(t, page) else {
                    return;
                };
                let v = objs.frame.view_point(Point::new(pt.x as f32, pt.y as f32));
                return self.new_text_box(s, ui, page, (v.x, v.y));
            }
            "add-image" => {
                return pick::open(
                    self,
                    &tr!("Add an image"),
                    (
                        tr_noop!("Images"),
                        &["png", "jpg", "jpeg", "PNG", "JPG", "JPEG"],
                    ),
                    |app, s, ui, p| app.add_image_file(s, ui, &p),
                );
            }
            "select-all" => {
                let Some(t) = s.tab() else { return };
                let page = t.view.current_page;
                let Some(objs) = TabEdit::objs(t, page) else {
                    return;
                };
                let ids = oe::tab_order(&objs);
                TabEdit::select(t, page, ids);
                return self.edit_selection_changed(s, ui, true);
            }
            "crop-cancel" => {
                if let Some(t) = s.tab() {
                    t.edit.crop = None;
                    t.edit.press = None;
                }
                super::sync(s, ui);
                return self.overlay_changed(s, ui);
            }
            "crop-apply" => return self.crop_apply(s, ui),
            _ => {}
        }
        let Some(t) = s.tab() else { return };
        let Some((sel, objs)) = TabEdit::current(t) else {
            return self.message(s, &tr!("Select something on the page first."));
        };
        let targets = sel.targets();
        let id = Matrix::IDENTITY;
        match what {
            "delete" => {
                let op = EditOp::Delete(targets);
                let label = op.label();
                self.edit_apply_as(s, ui, vec![op], &label, |_, _, _| Vec::new())
            }
            "duplicate" => {
                let m = oe::move_by_view(&objs.frame, 12.0, 12.0);
                self.edit_apply(s, ui, vec![EditOp::Duplicate(targets, m)], m)
            }
            "front" | "back" | "forward" | "backward" => {
                let z = match what {
                    "front" => ZOrder::Front,
                    "back" => ZOrder::Back,
                    "forward" => ZOrder::Forward,
                    _ => ZOrder::Backward,
                };
                self.edit_apply(s, ui, vec![EditOp::Reorder(targets, z)], id)
            }
            "fill" => {
                let c = palette(v as i32);
                let mut ops = Vec::new();
                let (text, other): (Vec<_>, Vec<_>) =
                    sel.objects(&objs).partition(|o| o.kind == Kind::Text);
                for o in &text {
                    match oe::block_len(&objs, o.id) {
                        Some(len) => ops.push(EditOp::BlockStyle {
                            block: o.id,
                            range: 0..len,
                            change: StyleChange::Fill(c),
                        }),
                        None => ops.push(EditOp::TextStyle(
                            vec![Target::Object(o.id)],
                            TextStyle {
                                fill: Some(c),
                                ..Default::default()
                            },
                        )),
                    }
                }
                if !other.is_empty() {
                    ops.push(EditOp::Fill(
                        other.iter().map(|o| Target::Object(o.id)).collect(),
                        c,
                    ));
                }
                // A colour is chosen to be seen: a fill that was switched off, or faded
                // out to nothing, shows again, as opaque as the drawing's line.
                for o in &other {
                    let Some(p) = o.path().filter(|p| p.fill.is_some() && p.fill_alpha <= 0.0)
                    else {
                        continue;
                    };
                    let (fill, line) = match p.stroke {
                        Some(_) if p.stroke_alpha > 0.0 => (p.stroke_alpha, None),
                        Some(_) => (1.0, Some(1.0)),
                        None => (1.0, None),
                    };
                    ops.push(EditOp::Opacity(
                        vec![Target::Object(o.id)],
                        Some(fill),
                        line,
                    ));
                }
                let label = if other.is_empty() {
                    tr!("Change text colour")
                } else {
                    tr!("Change fill colour")
                };
                self.edit_apply_as(s, ui, ops, &label, |o, n, i| oe::refind(o, n, i, &id))
            }
            "stroke" => {
                let c = palette(v as i32);
                self.edit_apply(s, ui, vec![EditOp::Stroke(targets, c)], id)
            }
            // "No fill": the fill of drawings becomes fully transparent; their lines stay.
            "fill-none" => {
                let filled: Vec<_> = sel
                    .objects(&objs)
                    .filter(|o| o.path().is_some_and(|p| p.fill.is_some()))
                    .map(|o| Target::Object(o.id))
                    .collect();
                if filled.is_empty() {
                    return;
                }
                let ops = vec![EditOp::Opacity(filled, Some(0.0), None)];
                let label = tr!("Change fill colour");
                self.edit_apply_as(s, ui, ops, &label, |o, n, i| oe::refind(o, n, i, &id))
            }
            "opacity" => {
                let a = (v / 100.0).clamp(0.0, 1.0);
                // A drawing whose fill is switched off keeps it off: only its line changes.
                let (line_only, rest): (Vec<_>, Vec<_>) = sel
                    .objects(&objs)
                    .partition(|o| o.path().is_some_and(|p| fill_off(p) && p.stroke.is_some()));
                let ids = |v: &[&pagebara_edit::PageObject]| -> Vec<_> {
                    v.iter().map(|o| Target::Object(o.id)).collect()
                };
                let mut ops = Vec::new();
                if !rest.is_empty() {
                    ops.push(EditOp::Opacity(ids(&rest), Some(a), Some(a)));
                }
                if !line_only.is_empty() {
                    ops.push(EditOp::Opacity(ids(&line_only), None, Some(a)));
                }
                let Some(label) = ops.first().map(EditOp::label) else {
                    return;
                };
                self.edit_apply_as(s, ui, ops, &label, |o, n, i| oe::refind(o, n, i, &id))
            }
            "line-width" if v > 0.0 => {
                let op = EditOp::Line {
                    targets,
                    width: Some(v),
                    dash: None,
                };
                self.edit_apply(s, ui, vec![op], id)
            }
            "dashed" => {
                // Dashes three line widths long, gaps two.
                let w = sel
                    .objects(&objs)
                    .find_map(|o| o.path().map(|p| p.line_width * oe::scale_of(&p.ctm)))
                    .unwrap_or(1.0)
                    .max(0.5);
                let dash = if value == "1" {
                    vec![3.0 * w, 2.0 * w]
                } else {
                    Vec::new()
                };
                let op = EditOp::Line {
                    targets,
                    width: None,
                    dash: Some(dash),
                };
                self.edit_apply(s, ui, vec![op], id)
            }
            "font-size" | "bold" | "italic" | "family" => {
                // The whole block restyled and laid out again.
                let Some(block) = sel.single_text(&objs) else {
                    return self.message(s, &tr!("Select one block of text to change its style."));
                };
                let Some(len) = oe::block_len(&objs, block.id) else {
                    return;
                };
                let change = match what {
                    "font-size" => StyleChange::Size(v),
                    "bold" => StyleChange::Bold(value == "1"),
                    "italic" => StyleChange::Italic(value == "1"),
                    _ => StyleChange::Family(value.to_owned()),
                };
                let op = EditOp::BlockStyle {
                    block: block.id,
                    range: 0..len,
                    change,
                };
                let bbox = block.bbox;
                self.edit_apply_as(
                    s,
                    ui,
                    vec![op],
                    &tr!("Change text style"),
                    move |_, n, _| super::text::written_block(n, &bbox),
                )
            }
            "crop" => {
                let Some(img) = sel.single_image(&objs) else {
                    return self.message(s, &tr!("Select one image to crop it."));
                };
                let r = view_rect(&objs, &img.bbox);
                let (page, oid) = (sel.page, img.id);
                if let Some(t) = s.tab() {
                    t.edit.crop = Some((page, oid, r));
                }
                super::sync(s, ui);
                self.message(
                    s,
                    &tr!(
                        "Cropping: drag the handles, then press Enter to apply or Escape to cancel."
                    ),
                );
                self.overlay_changed(s, ui)
            }
            "replace-image" => {
                if sel.single_image(&objs).is_none() {
                    return self.message(s, &tr!("Select one image to replace it."));
                }
                pick::open(
                    self,
                    &tr!("Replace the image"),
                    (
                        tr_noop!("Images"),
                        &["png", "jpg", "jpeg", "PNG", "JPG", "JPEG"],
                    ),
                    |app, s, ui, p| app.replace_image_file(s, ui, &p),
                )
            }
            "extract-image" => {
                let Some(img) = sel.single_image(&objs) else {
                    return self.message(s, &tr!("Select one image to save it."));
                };
                let png = match pagebara_edit::extract_image(t.session.doc(), sel.page, img.id) {
                    Ok(b) => b,
                    Err(e) => {
                        return self.show_message(
                            s,
                            ui,
                            &tr!("Could not save the image"),
                            &e.to_string(),
                        );
                    }
                };
                let name =
                    pick::derived_name(&t.session.title(), &format!("image p{}", sel.page + 1))
                        .replace(".pdf", ".png");
                pick::save_file(
                    self,
                    &tr!("Save the image"),
                    (tr_noop!("PNG images"), &["png"]),
                    &name,
                    move |app, s, ui, p| match pagebara_app_core::session::write_atomic(&p, &png) {
                        Ok(()) => app.message(s, &tr!("Saved the image as {}.", p.display())),
                        Err(e) => app.show_message(
                            s,
                            ui,
                            &tr!("Could not save the image"),
                            &e.to_string(),
                        ),
                    },
                )
            }
            _ => log::warn!("unknown edit action {what}"),
        }
    }

    /// The middle of the visible part of the current page (view space).
    fn view_centre(&self, s: &State) -> Option<(usize, (f32, f32))> {
        let (vw, vh) = s.view_size;
        let t = s.tab_ref()?;
        let page = t.view.current_page;
        let (u, v) = t.view.screen_to_page_of(page, vw / 2.0, vh / 2.0)?;
        let (pw, ph) = t.session.geoms[page].size();
        Some((page, (u.clamp(0.0, pw) as f32, v.clamp(0.0, ph) as f32)))
    }

    fn add_image_file(self: &Rc<Self>, s: &mut State, ui: &AppWindow, path: &PathBuf) {
        let (img, (iw, ih)) = match image_file(path) {
            Ok(v) => v,
            Err(e) => return self.show_message(s, ui, &tr!("Could not add the image"), &e),
        };
        let Some((page, at)) = self.view_centre(s) else {
            return;
        };
        let Some(t) = s.tab() else { return };
        let Some(objs) = TabEdit::objs(t, page) else {
            return;
        };
        let (pw, ph) = objs.frame.size();
        let aspect = ih as f32 / iw as f32;
        let w = (pw * 0.4).min(240.0).min(ph * 0.4 / aspect);
        let h = w * aspect;
        let (x, y) = (
            (at.0 - w / 2.0).clamp(0.0, (pw - w).max(0.0)),
            (at.1 - h / 2.0).clamp(0.0, (ph - h).max(0.0)),
        );
        let a = objs.frame.page_point(Point::new(x, y));
        let b = objs.frame.page_point(Point::new(x + w, y + h));
        let rect = Rect::from_points(&[a, b]);
        TabEdit::select(t, page, Vec::new());
        self.edit_apply_as(
            s,
            ui,
            vec![EditOp::AddImage(img, rect)],
            &tr!("Add image"),
            |_, new, _| {
                new.top_level()
                    .filter(|o| o.kind == Kind::Image)
                    .map(|o| o.id)
                    .last()
                    .into_iter()
                    .collect()
            },
        );
        self.edit_selection_changed(s, ui, true);
    }

    fn replace_image_file(self: &Rc<Self>, s: &mut State, ui: &AppWindow, path: &PathBuf) {
        let (data, _) = match image_file(path) {
            Ok(v) => v,
            Err(e) => return self.show_message(s, ui, &tr!("Could not replace the image"), &e),
        };
        let Some(t) = s.tab() else { return };
        let Some((sel, objs)) = TabEdit::current(t) else {
            return;
        };
        let Some(img) = sel.single_image(&objs) else {
            return;
        };
        let target = Target::Object(img.id);
        let bbox = img.bbox;
        self.edit_apply_as(
            s,
            ui,
            vec![EditOp::ReplaceImage(target, data, ImageFit::Contain)],
            &tr!("Replace image"),
            move |_, new, _| {
                // The new image sits inside the old one's frame.
                new.top_level()
                    .filter(|o| o.kind == Kind::Image)
                    .filter(|o| pagebara_edit::geom::intersects(&o.bbox, &bbox))
                    .map(|o| o.id)
                    .last()
                    .into_iter()
                    .collect()
            },
        );
    }

    pub(super) fn crop_press(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        page: usize,
        at: (f32, f32),
        k: f32,
    ) {
        let Some(t) = s.tab() else { return };
        let Some((cp, _, r)) = t.edit.crop else {
            return;
        };
        let Some(objs) = TabEdit::objs(t, page) else {
            return;
        };
        let inside = cp == page
            && at.0 >= r[0]
            && at.0 <= r[0] + r[2]
            && at.1 >= r[1]
            && at.1 <= r[1] + r[3];
        let kind = match super::App::edit_handle_at(t, page, at, k) {
            Some(h) if h != Handle::Rotate => DragKind::Scale(h),
            _ if inside => DragKind::Move,
            // A click elsewhere applies the crop.
            _ => return self.crop_apply(s, ui),
        };
        t.edit.press = Some(Press::Crop {
            drag: Drag::new(kind, objs.frame, at, r),
        });
    }

    fn crop_apply(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab() else { return };
        let Some((page, id, r)) = t.edit.crop.take() else {
            return;
        };
        t.edit.press = None;
        let Some(objs) = TabEdit::objs(t, page) else {
            return;
        };
        let a = objs.frame.page_point(Point::new(r[0], r[1]));
        let b = objs.frame.page_point(Point::new(r[0] + r[2], r[1] + r[3]));
        let rect = Rect::from_points(&[a, b]);
        TabEdit::select(t, page, vec![id]);
        self.edit_apply(
            s,
            ui,
            vec![EditOp::Crop(vec![Target::Object(id)], rect)],
            Matrix::IDENTITY,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagebara_edit::Paint;

    fn path(fill: Option<f32>, stroke: Option<f32>) -> PathInfo {
        PathInfo {
            paints: 1,
            fill: fill.map(|_| Paint::black()),
            stroke: stroke.map(|_| Paint::black()),
            line_width: 1.0,
            dashed: false,
            ctm: Matrix::IDENTITY,
            fill_alpha: fill.unwrap_or(1.0),
            stroke_alpha: stroke.unwrap_or(1.0),
            clips: false,
            outline: Vec::new(),
        }
    }

    #[test]
    fn no_fill_is_a_transparent_fill_beside_a_visible_line() {
        // A filled shape, with and without an outline.
        assert!(fill_off(&path(Some(0.0), None)));
        assert!(fill_off(&path(Some(0.0), Some(1.0))));
        assert!(fill_off(&path(Some(0.0), Some(0.4))));
        assert!(!fill_off(&path(Some(1.0), Some(1.0))));
        assert!(!fill_off(&path(Some(0.5), None)));
        // Faded out whole with the opacity slider: opacity 0, not "No fill".
        assert!(!fill_off(&path(Some(0.0), Some(0.0))));
        // A line has no fill to switch off.
        assert!(!fill_off(&path(None, Some(1.0))));
    }
}
