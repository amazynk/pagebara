//! The Edit tool: selecting what pages draw (text, images, drawings) and changing it.
//! Objects are picked by clicking (Shift or Ctrl adds), by dragging a box or with Tab;
//! dragging the selection or its handles moves, scales and rotates it (with snapping),
//! arrows nudge it; the panel and the context menu recolour, arrange, duplicate, delete
//! and replace or crop images. Text is changed in place (see `text`). Every change is one
//! undoable step; the status line announces what happened for screen readers.

mod actions;
mod draw;
pub mod script;
mod text;

use std::rc::Rc;
use std::sync::Arc;

use pagebara_app_core::edit::{EditOp, PageEditing};
use pagebara_app_core::object_edit::{
    self as oe, Drag, DragKind, DragResult, Handle, Selection, TextEdit, ViewRect,
};
use pagebara_edit::hit::HitOptions;
use pagebara_edit::{Matrix, ObjectId, PageObjects, Point, Rect};
use pagebara_i18n::tr;
use slint::ComponentHandle;

pub use draw::{overlay, preview_made, previews_stale, view_scale};
pub use text::refresh_view as refresh_text_view;

use super::input::Pointer;
use super::{App, State, Tab};
use crate::{AppState, AppWindow, EditState, ViewState};

/// `AppState.tool` of the Edit tool.
pub const TOOL: i32 = 7;
/// Handle size and the rotation handle's distance above the frame, in screen pixels.
const HANDLE_PX: f32 = 6.0;
const ROTATE_PX: f32 = 18.5;
/// How far the selection frame sits outside the objects, in screen pixels.
pub const FRAME_PAD_PX: f32 = 5.0;

/// The Edit tool's state in one tab.
#[derive(Debug, Default)]
pub struct TabEdit {
    pub objects: PageEditing,
    pub sel: Selection,
    /// The page objects `sel` was made for.
    sel_objs: Option<Arc<PageObjects>>,
    pub hover: Option<(usize, ObjectId)>,
    press: Option<Press>,
    pub text: Option<TextEdit>,
    /// What the paragraph being edited looks like now.
    pub text_view: Option<oe::TextView>,
    /// Its rendering as an image for the overlay, and the pixels it was made from.
    text_image: Option<(Arc<Vec<u8>>, slint::Image)>,
    /// An image being cropped: page, object and the kept part (view space).
    pub crop: Option<(usize, ObjectId, ViewRect)>,
    /// The next click adds a text box.
    pub placing_text: bool,
    /// What a drag in progress does, for the overlay.
    preview: Option<Preview>,
    /// The dragged objects as images: the page under them, and themselves to move.
    dragged: Option<draw::Dragged>,
}

#[derive(Debug)]
enum Press {
    Drag {
        page: usize,
        drag: Drag,
        moved: bool,
    },
    Marquee {
        page: usize,
        from: (f32, f32),
        to: (f32, f32),
        add: bool,
    },
    /// In the text being edited: the selection the press made, and what a drag from it
    /// selects by.
    TextSelect {
        first: (usize, usize),
        unit: oe::DragUnit,
    },
    Crop {
        drag: Drag,
    },
}

#[derive(Clone, Debug)]
struct Preview {
    page: usize,
    result: DragResult,
    /// Where the pointer is (view space), for the label.
    at: (f32, f32),
}

impl TabEdit {
    /// The objects of `page` now.
    pub fn objs(t: &mut Tab, page: usize) -> Option<Arc<PageObjects>> {
        t.edit.objects.objects(&t.session, page).ok()
    }

    /// The selection, checked against the current objects: after an undo, redo or another
    /// change the same objects are found again by where they are.
    pub fn current(t: &mut Tab) -> Option<(Selection, Arc<PageObjects>)> {
        if t.edit.sel.is_empty() {
            return None;
        }
        let page = t.edit.sel.page;
        let objs = Self::objs(t, page)?;
        if let Some(old) = &t.edit.sel_objs
            && !Arc::ptr_eq(old, &objs)
        {
            let ids = oe::refind(old, &objs, &t.edit.sel.ids, &Matrix::IDENTITY);
            t.edit.sel.ids = ids;
        }
        t.edit.sel_objs = Some(objs.clone());
        (!t.edit.sel.is_empty()).then(|| (t.edit.sel.clone(), objs))
    }

    fn select(t: &mut Tab, page: usize, ids: Vec<ObjectId>) {
        t.edit.sel = Selection::new(page, ids);
        t.edit.sel_objs = Self::objs(t, page);
    }
}

/// A view-space rectangle as left, top, width, height.
fn view_rect(objs: &PageObjects, r: &Rect) -> ViewRect {
    let v = objs.frame.view_rect(*r);
    [v.x0, v.y0, v.width(), v.height()]
}

fn hit_options(k: f32) -> HitOptions {
    HitOptions {
        tolerance: 3.0 / k,
        ..Default::default()
    }
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let a = app.clone();
    ui.global::<EditState>().on_action(move |what, value| {
        let (what, value) = (what.to_string(), value.to_string());
        a.act(move |app, s, ui| app.edit_action(s, ui, &what, &value))
    });
    text::wire(app, ui);
}

/// Shows the selection in the panel (after any change, undo or redo).
pub fn sync(s: &mut State, ui: &AppWindow) {
    let tool = ui.global::<AppState>().get_tool();
    let Some(t) = s.tab() else { return };
    let es = ui.global::<EditState>();
    if tool != TOOL {
        es.set_count(0);
        return;
    }
    es.set_text_editing(t.edit.text.is_some());
    es.set_cropping(t.edit.crop.is_some());
    let Some((sel, objs)) = TabEdit::current(t) else {
        es.set_count(0);
        es.set_summary("".into());
        return;
    };
    actions::show(&es, &objs, &sel);
}

impl App {
    /// Pointer events over the pages with the Edit tool.
    pub fn edit_pointer(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        match p.kind {
            0 if p.button == 1 => self.edit_context(s, ui, p),
            0 if p.button == 0 => self.edit_press(s, ui, p),
            2 => self.edit_release(s, ui, p),
            4 => {
                if let Some(t) = s.tab() {
                    t.edit.press = None;
                    t.edit.preview = None;
                    t.edit.dragged = None;
                }
                self.overlay_changed(s, ui);
            }
            1 => self.edit_drag(s, ui, p),
            3 if s.tab_ref().is_some_and(|t| t.edit.press.is_some()) => self.edit_drag(s, ui, p),
            3 => self.edit_hover(s, ui, p),
            _ => {}
        }
    }

    /// The page and view-space point under the pointer, and screen pixels per point.
    fn edit_hit(s: &State, x: f64, y: f64) -> Option<(usize, (f32, f32), f32)> {
        let t = s.tab_ref()?;
        let (page, u, v) = t.view.screen_to_page(x, y)?;
        Some((page, (u as f32, v as f32), t.view.last.k.max(0.01) as f32))
    }

    /// The handle of the selection frame (or the crop frame) at a view point.
    fn edit_handle_at(t: &mut Tab, page: usize, at: (f32, f32), k: f32) -> Option<Handle> {
        if let Some((cp, _, r)) = t.edit.crop {
            return (cp == page)
                .then(|| oe::handle_at(r, at, HANDLE_PX / k, -1e6))
                .flatten()
                .filter(|h| *h != Handle::Rotate);
        }
        let (sel, objs) = TabEdit::current(t)?;
        if sel.page != page {
            return None;
        }
        let r = oe::outset(view_rect(&objs, &sel.bbox(&objs)?), FRAME_PAD_PX / k);
        oe::handle_at(r, at, HANDLE_PX / k, ROTATE_PX / k)
    }

    fn edit_hover(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let mut hover = None;
        let cursor = (|| {
            let (page, at, k) = Self::edit_hit(s, p.x, p.y)?;
            let t = s.tab()?;
            if t.edit.placing_text {
                return Some(3);
            }
            if let Some(te) = &t.edit.text
                && te.page == page
                && text::inside(t, at, k)
            {
                return Some(1);
            }
            if let Some(h) = Self::edit_handle_at(t, page, at, k) {
                return Some(h.cursor());
            }
            let objs = TabEdit::objs(t, page)?;
            let pt = objs.frame.page_point(Point::new(at.0, at.1));
            let id = pagebara_edit::hit::object_at(&objs, pt, &hit_options(k))?;
            hover = Some((page, id));
            Some(4)
        })()
        .unwrap_or(0);
        let vs = ui.global::<ViewState>();
        if vs.get_cursor() != cursor {
            vs.set_cursor(cursor);
        }
        let Some(t) = s.tab() else { return };
        if t.edit.hover != hover {
            t.edit.hover = hover;
            self.overlay_changed(s, ui);
        }
    }

    fn edit_press(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let clicks = s.input.press(p.x, p.y);
        let Some((page, at, k)) = Self::edit_hit(s, p.x, p.y) else {
            return;
        };
        let Some(t) = s.tab() else { return };
        // Clicks in the text being edited place the caret; elsewhere they finish it.
        if t.edit.text.as_ref().is_some_and(|te| te.page == page) && text::inside(t, at, k) {
            return self.text_press(s, ui, at, clicks, p.shift);
        }
        if t.edit.text.is_some() {
            self.edit_text_done(s, ui, true);
        }
        let Some(t) = s.tab() else { return };
        if t.edit.crop.is_some() {
            return self.crop_press(s, ui, page, at, k);
        }
        if t.edit.placing_text {
            t.edit.placing_text = false;
            return self.new_text_box(s, ui, page, at);
        }
        let Some(objs) = TabEdit::objs(t, page) else {
            return;
        };
        let pt = objs.frame.page_point(Point::new(at.0, at.1));
        let hit = pagebara_edit::hit::object_at(&objs, pt, &hit_options(k));
        // A double click on text edits it, even over a handle of its frame.
        if clicks >= 2
            && let Some(id) = hit
            && objs.get(id).is_some_and(|o| o.text().is_some())
        {
            t.edit.press = None;
            TabEdit::select(t, page, vec![id]);
            return self.start_text_edit(s, ui, Some(at));
        }
        if let Some(h) = Self::edit_handle_at(t, page, at, k) {
            let Some(r) = t.edit.sel.bbox(&objs).map(|b| view_rect(&objs, &b)) else {
                return;
            };
            let kind = if h == Handle::Rotate {
                DragKind::Rotate
            } else {
                DragKind::Scale(h)
            };
            t.edit.press = Some(Press::Drag {
                page,
                drag: Drag::new(kind, objs.frame, at, r),
                moved: false,
            });
            return;
        }
        match hit {
            Some(id) => {
                if p.shift || p.ctrl {
                    if t.edit.sel.page != page {
                        TabEdit::select(t, page, Vec::new());
                    }
                    t.edit.sel.toggle(id);
                    t.edit.sel_objs = Some(objs);
                    return self.edit_selection_changed(s, ui, true);
                }
                let changed = !(t.edit.sel.page == page && t.edit.sel.contains(id));
                if changed {
                    TabEdit::select(t, page, vec![id]);
                }
                let r = t
                    .edit
                    .sel
                    .bbox(&objs)
                    .map_or([at.0, at.1, 0.0, 0.0], |b| view_rect(&objs, &b));
                let mut drag = Drag::new(DragKind::Move, objs.frame, at, r);
                drag.others = objs
                    .top_level()
                    .filter(|o| !o.hidden && !t.edit.sel.contains(o.id))
                    .map(|o| view_rect(&objs, &o.bbox))
                    .collect();
                t.edit.press = Some(Press::Drag {
                    page,
                    drag,
                    moved: false,
                });
                if changed {
                    self.edit_selection_changed(s, ui, true);
                }
            }
            None => {
                let add = p.shift || p.ctrl;
                if !add && !t.edit.sel.is_empty() {
                    t.edit.sel = Selection::default();
                    t.edit.sel_objs = None;
                    self.edit_selection_changed(s, ui, false);
                }
                if let Some(t) = s.tab() {
                    t.edit.press = Some(Press::Marquee {
                        page,
                        from: at,
                        to: at,
                        add,
                    });
                }
            }
        }
    }

    fn edit_drag(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let backend = s.backend.clone();
        let Some(t) = s.tabs.get_mut(s.active) else {
            return;
        };
        let k = t.view.last.k.max(0.01) as f32;
        let page = match &t.edit.press {
            Some(Press::Drag { page, .. } | Press::Marquee { page, .. }) => *page,
            Some(Press::TextSelect { .. }) => t.edit.text.as_ref().map_or(0, |e| e.page),
            Some(Press::Crop { .. }) => t.edit.crop.map_or(0, |c| c.0),
            None => return,
        };
        let Some((u, v)) = t.view.screen_to_page_of(page, p.x, p.y) else {
            return;
        };
        let at = (u as f32, v as f32);
        let mut started = false;
        match &mut t.edit.press {
            Some(Press::Drag { drag, moved, page }) => {
                let d = (at.0 - drag.start.0).hypot(at.1 - drag.start.1) * k;
                if !*moved && d < 3.0 {
                    return;
                }
                started = !*moved;
                *moved = true;
                let snap = if p.alt { 0.0 } else { 5.0 / k };
                let result = drag.update(at, p.shift, snap);
                t.edit.preview = Some(Preview {
                    page: *page,
                    result,
                    at,
                });
            }
            Some(Press::Marquee { to, .. }) => *to = at,
            Some(Press::TextSelect { first, unit }) => {
                let (first, unit) = (*first, *unit);
                return self.text_drag(s, ui, at, unit, first);
            }
            Some(Press::Crop { drag }) => {
                let result = drag.update(at, p.shift, 0.0);
                t.edit.preview = Some(Preview { page, result, at });
            }
            None => return,
        }
        if started {
            t.edit.dragged = draw::dragged(t, page, &*backend);
        }
        self.overlay_changed(s, ui);
    }

    fn edit_release(self: &Rc<Self>, s: &mut State, ui: &AppWindow, _p: Pointer) {
        let Some(t) = s.tab() else { return };
        t.edit.dragged = None;
        let press = t.edit.press.take();
        let preview = t.edit.preview.take();
        match press {
            Some(Press::Drag { moved: true, .. }) => {
                if let Some(pv) = preview {
                    let m = pv.result.matrix;
                    if !pagebara_edit::geom::is_identity(&m) {
                        let targets = t.edit.sel.targets();
                        return self.edit_apply(s, ui, vec![EditOp::Transform(targets, m)], m);
                    }
                }
                self.overlay_changed(s, ui);
            }
            Some(Press::Marquee {
                page,
                from,
                to,
                add,
                ..
            }) => {
                let k = t.view.last.k.max(0.01) as f32;
                if (to.0 - from.0).abs() * k < 3.0 && (to.1 - from.1).abs() * k < 3.0 {
                    return self.overlay_changed(s, ui);
                }
                let Some(objs) = TabEdit::objs(t, page) else {
                    return;
                };
                let a = objs.frame.page_point(Point::new(from.0, from.1));
                let b = objs.frame.page_point(Point::new(to.0, to.1));
                let r = Rect::from_points(&[a, b]);
                let found = pagebara_edit::hit::objects_in(&objs, r, false, &hit_options(k));
                if !add || t.edit.sel.page != page {
                    TabEdit::select(t, page, Vec::new());
                }
                for id in found {
                    if !t.edit.sel.contains(id) {
                        t.edit.sel.ids.push(id);
                    }
                }
                t.edit.sel_objs = Some(objs);
                self.edit_selection_changed(s, ui, true);
            }
            Some(Press::Crop { .. }) => {
                if let (Some(pv), Some((_, _, r))) = (preview, t.edit.crop.as_mut()) {
                    *r = transformed(*r, &pv.result.view);
                }
                self.overlay_changed(s, ui);
            }
            _ => self.overlay_changed(s, ui),
        }
    }

    /// A right press: selects what is there (unless it is selected) and sets up the menu.
    fn edit_context(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let vs = ui.global::<ViewState>();
        vs.set_ctx_spell(false);
        if s.tab_ref().is_some_and(|t| t.edit.text.is_some()) {
            // On a misspelled word: its corrections; elsewhere the editing ends.
            if let Some(range) = Self::text_misspelled_at(s, p.x, p.y)
                && self.spell_open_menu(s, ui, super::spell::PAGE_TEXT, range)
            {
                vs.set_ctx_edit(true);
                vs.set_ctx_spell(true);
                return;
            }
            self.edit_text_done(s, ui, true);
        }
        s.input.menu_at = None;
        if let Some((page, at, k)) = Self::edit_hit(s, p.x, p.y)
            && let Some(t) = s.tab()
            && let Some(objs) = TabEdit::objs(t, page)
        {
            let pt = objs.frame.page_point(Point::new(at.0, at.1));
            let menu_at = pagebara_annot::Point::new(f64::from(pt.x), f64::from(pt.y));
            let hit = pagebara_edit::hit::object_at(&objs, pt, &hit_options(k));
            match hit {
                Some(id) if !(t.edit.sel.page == page && t.edit.sel.contains(id)) => {
                    TabEdit::select(t, page, vec![id]);
                    self.edit_selection_changed(s, ui, true);
                }
                None => {
                    TabEdit::select(t, page, Vec::new());
                    self.edit_selection_changed(s, ui, false);
                }
                _ => {}
            }
            s.input.menu_at = Some((page, menu_at));
        }
        let vs = ui.global::<ViewState>();
        vs.set_ctx_edit(true);
        let Some(t) = s.tab() else { return };
        let (has, text, image) = match TabEdit::current(t) {
            Some((sel, objs)) => (
                true,
                sel.single_text(&objs).is_some(),
                sel.single_image(&objs).is_some(),
            ),
            None => (false, false, false),
        };
        vs.set_ctx_edit_sel(has);
        vs.set_ctx_edit_text(text);
        vs.set_ctx_edit_image(image);
    }

    /// The selection changed: panel, overlay, and (when `announce`) the status line.
    pub fn edit_selection_changed(self: &Rc<Self>, s: &mut State, ui: &AppWindow, announce: bool) {
        sync(s, ui);
        if announce
            && let Some(t) = s.tab()
            && let Some((sel, objs)) = TabEdit::current(t)
        {
            let mut text = oe::describe_selection(&objs, &sel);
            if sel.ids.len() == 1 {
                let order = oe::tab_order(&objs);
                if let Some(i) = order.iter().position(|&id| id == sel.ids[0]) {
                    text = tr!("{}, {} of {}", text, i + 1, order.len());
                }
            }
            self.message(s, &tr!("Selected: {}.", text));
        }
        self.overlay_changed(s, ui);
    }

    /// Keys with the Edit tool; `None` when they are not the tool's.
    pub fn edit_keys(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        c: char,
        ctrl: bool,
        shift: bool,
    ) -> Option<bool> {
        use slint::platform::Key;
        let key = |k: Key| c == char::from(k);
        let t = s.tab()?;
        if t.edit.crop.is_some() {
            if key(Key::Escape) {
                self.edit_action(s, ui, "crop-cancel", "");
                return Some(true);
            }
            if key(Key::Return) {
                self.edit_action(s, ui, "crop-apply", "");
                return Some(true);
            }
        }
        if ctrl && c.eq_ignore_ascii_case(&'e') {
            return None;
        }
        let has = !t.edit.sel.is_empty();
        if ctrl {
            let what = match c {
                'd' | 'D' if has => "duplicate",
                'a' | 'A' => "select-all",
                ']' if has => "forward",
                '[' if has => "backward",
                '}' if has => "front",
                '{' if has => "back",
                _ => return None,
            };
            let what = match (what, shift) {
                ("forward", true) => "front",
                ("backward", true) => "back",
                (w, _) => w,
            };
            self.edit_action(s, ui, what, "");
            return Some(true);
        }
        if key(Key::Tab) || key(Key::Backtab) {
            self.edit_cycle(s, ui, !(shift || key(Key::Backtab)));
            return Some(true);
        }
        if key(Key::Escape) {
            if t.edit.placing_text {
                t.edit.placing_text = false;
                ui.global::<ViewState>().set_cursor(0);
                return Some(true);
            }
            if has {
                t.edit.sel = Selection::default();
                self.edit_selection_changed(s, ui, false);
                self.message(s, &tr!("Nothing selected."));
                return Some(true);
            }
            return None;
        }
        if t.edit.placing_text && key(Key::Return) {
            t.edit.placing_text = false;
            self.edit_action(s, ui, "add-text-centre", "");
            return Some(true);
        }
        if !has {
            return None;
        }
        if key(Key::Delete) || key(Key::Backspace) {
            self.edit_action(s, ui, "delete", "");
            return Some(true);
        }
        if key(Key::Return) {
            self.edit_action(s, ui, "edit-text", "");
            return Some(true);
        }
        let step = if shift { 10.0 } else { 1.0 };
        let d = match () {
            _ if key(Key::LeftArrow) => (-step, 0.0),
            _ if key(Key::RightArrow) => (step, 0.0),
            _ if key(Key::UpArrow) => (0.0, -step),
            _ if key(Key::DownArrow) => (0.0, step),
            _ => return None,
        };
        let (_, objs) = TabEdit::current(t)?;
        let m = oe::move_by_view(&objs.frame, d.0, d.1);
        let targets = t.edit.sel.targets();
        self.edit_apply(s, ui, vec![EditOp::Transform(targets, m)], m);
        Some(true)
    }

    /// Selects the next or previous object of the current page.
    fn edit_cycle(self: &Rc<Self>, s: &mut State, ui: &AppWindow, forward: bool) {
        let Some(t) = s.tab() else { return };
        let page = if t.edit.sel.is_empty() {
            t.view.current_page
        } else {
            t.edit.sel.page
        };
        let Some(objs) = TabEdit::objs(t, page) else {
            return;
        };
        let order = oe::tab_order(&objs);
        if order.is_empty() {
            return self.message(s, &tr!("There is nothing to edit on this page."));
        }
        let at = (t.edit.sel.page == page)
            .then(|| t.edit.sel.ids.last())
            .flatten()
            .and_then(|id| order.iter().position(|o| o == id));
        let n = order.len();
        let i = match (at, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        TabEdit::select(t, page, vec![order[i]]);
        self.edit_selection_changed(s, ui, true);
    }

    /// Applies `ops` to the selection's page as one step and selects the same objects
    /// after `m` moved them.
    pub fn edit_apply(self: &Rc<Self>, s: &mut State, ui: &AppWindow, ops: Vec<EditOp>, m: Matrix) {
        let label = match &ops[..] {
            [one] => one.label(),
            _ => tr!("Edit objects"),
        };
        self.edit_apply_as(s, ui, ops, &label, |old, new, ids| {
            oe::refind(old, new, ids, &m)
        });
    }

    /// Applies `ops` as one step named `label`; `reselect` finds the new selection from the
    /// objects before and after and the old selection.
    pub fn edit_apply_as(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        ops: Vec<EditOp>,
        label: &str,
        reselect: impl FnOnce(&PageObjects, &PageObjects, &[ObjectId]) -> Vec<ObjectId>,
    ) {
        if !self.may_edit_content(s, ui) {
            return;
        }
        let Some(t) = s.tab() else { return };
        let page = if t.edit.sel.is_empty() {
            t.view.current_page
        } else {
            t.edit.sel.page
        };
        let Some(old) = TabEdit::objs(t, page) else {
            return;
        };
        let ids = t.edit.sel.ids.clone();
        let result = t.edit.objects.apply_all(
            &mut t.session,
            page,
            &ops,
            label,
            &pagebara_edit::SimpleEncoder,
        );
        match result {
            Ok(changed) => {
                if let Some(new) = TabEdit::objs(t, page) {
                    let ids = reselect(&old, &new, &ids);
                    t.edit.sel = Selection::new(page, ids);
                    t.edit.sel_objs = Some(new);
                }
                self.after_edit(s, ui, changed.pages);
                self.message(s, &format!("{label}."));
            }
            Err(e) => {
                let text = match e {
                    pagebara_edit::Error::NotPermitted => {
                        return self.ask_owner_password(
                            s,
                            ui,
                            &tr!("This document's security settings do not allow changing it."),
                        );
                    }
                    pagebara_edit::Error::Encode(pagebara_edit::EncodeError::Missing(m)) => {
                        tr!("The font here cannot show “{}”.", m)
                    }
                    e => tr!("That did not work: {}.", e),
                };
                self.show_message(s, ui, &tr!("Could not change the page"), &text);
            }
        }
    }

    /// Whether page content may be changed; explains when not.
    pub fn may_edit_content(self: &Rc<Self>, s: &mut State, ui: &AppWindow) -> bool {
        let Some(t) = s.tab_ref() else { return false };
        if t.session.doc().permissions().modify {
            return true;
        }
        self.ask_owner_password(
            s,
            ui,
            &tr!("This document's security settings do not allow changing its pages."),
        );
        false
    }

    /// The tool changed: leaving Edit finishes text editing and clears the selection.
    pub fn edit_tool_changed(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let tool = ui.global::<AppState>().get_tool();
        if tool != TOOL {
            self.edit_text_done(s, ui, true);
            if let Some(t) = s.tab() {
                t.edit.sel = Selection::default();
                t.edit.crop = None;
                t.edit.placing_text = false;
                t.edit.hover = None;
            }
        } else {
            self.message(
                s,
                &tr!("Edit: click text, a picture or a drawing to select it, or press Tab."),
            );
        }
        ui.global::<ViewState>().set_ctx_edit(false);
        sync(s, ui);
    }
}

/// `r` moved by view-space matrix `m` (its bounds).
fn transformed(r: ViewRect, m: &Matrix) -> ViewRect {
    let a = m.apply(Point::new(r[0], r[1]));
    let b = m.apply(Point::new(r[0] + r[2], r[1] + r[3]));
    let x0 = a.x.min(b.x);
    let y0 = a.y.min(b.y);
    [x0, y0, (a.x - b.x).abs(), (a.y - b.y).abs()]
}
