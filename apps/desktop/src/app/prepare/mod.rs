//! The Prepare form tool: placing fields of each type (a click places one of the default
//! size, a drag draws it), selecting widgets (click, Shift or Ctrl adds, a box, Tab),
//! moving and resizing them with the Edit tool's handles and snapping, arrows to nudge,
//! Delete and Ctrl+D; the panel (see `panel`) edits properties, aligns several fields,
//! sets the tab order, offers detected fields and exchanges form data. Every change is
//! one undoable step; the status line says what happened, for screen readers.

mod panel;

use std::rc::Rc;

use pagebara_app_core::form_prep::{self, Suggestion, Tool};
use pagebara_app_core::forms::FormChange;
use pagebara_app_core::object_edit::{self as oe, Drag, DragKind, DragResult, Handle, ViewRect};
use pagebara_app_core::pages::PageGeom;
use pagebara_app_core::view::Placement;
use pagebara_edit::{PageFrame, Rect};
use pagebara_i18n::tr;
use pagebara_syntax::ObjRef;
use slint::ComponentHandle;

pub use panel::sync;

use super::input::Pointer;
use super::overlay::{at, fill};
use super::{App, State, Tab};
use crate::{AppState, AppWindow, OverlayItem, PrepState, ViewState};

/// `AppState.tool` of Prepare form.
pub const TOOL: i32 = 8;
const HANDLE_PX: f32 = 6.0;
const PAD_PX: f32 = 4.0;
const FIELD: slint::Color = slint::Color::from_argb_u8(0x26, 0x60, 0xA5, 0xFA);
const ACCENT: slint::Color = slint::Color::from_rgb_u8(0x25, 0x63, 0xEB);
const NAME: slint::Color = slint::Color::from_rgb_u8(0x1E, 0x3A, 0x8A);
const SUGGESTED: slint::Color = slint::Color::from_argb_u8(0x30, 0xF5, 0x9E, 0x0B);
const SUGGESTED_EDGE: slint::Color = slint::Color::from_rgb_u8(0xD9, 0x77, 0x06);
const GUIDE: slint::Color = slint::Color::from_rgb_u8(0xDB, 0x27, 0x77);
const LABEL_BG: slint::Color = slint::Color::from_argb_u8(0xE6, 0x18, 0x18, 0x1B);

/// Prepare form's state in one tab.
#[derive(Debug, Default)]
pub struct TabPrep {
    /// Selected widgets, all on `page`.
    pub sel: Vec<ObjRef>,
    pub page: usize,
    pub placing: Option<Tool>,
    press: Option<Press>,
    preview: Option<(usize, DragResult, (f32, f32))>,
    pub suggestions: Vec<Suggestion>,
    pub suggestion: Option<usize>,
    /// The highlighted row of the tab order list.
    pub tab_row: Option<usize>,
}

#[derive(Debug)]
enum Press {
    Drag {
        page: usize,
        drag: Drag,
        moved: bool,
    },
    Draw {
        page: usize,
        tool: Tool,
        from: (f32, f32),
        to: (f32, f32),
    },
    Marquee {
        page: usize,
        from: (f32, f32),
        to: (f32, f32),
        add: bool,
    },
}

/// A widget of the page: id, field name, user-space and view-space rectangles.
#[derive(Clone, Debug)]
pub struct Placed {
    pub id: ObjRef,
    pub name: String,
    pub rect: [f64; 4],
    pub view: ViewRect,
}

pub fn frame_of(g: &PageGeom) -> PageFrame {
    let c = Rect::new(
        g.crop[0] as f32,
        g.crop[1] as f32,
        g.crop[2] as f32,
        g.crop[3] as f32,
    );
    PageFrame {
        media_box: c,
        crop_box: c,
        rotate: g.rotate,
    }
}

fn to_view(g: &PageGeom, r: [f64; 4]) -> ViewRect {
    g.rect_to_view(r).map(|v| v as f32)
}

fn union(rs: impl Iterator<Item = ViewRect>) -> Option<ViewRect> {
    rs.reduce(|a, b| {
        let x0 = a[0].min(b[0]);
        let y0 = a[1].min(b[1]);
        let x1 = (a[0] + a[2]).max(b[0] + b[2]);
        let y1 = (a[1] + a[3]).max(b[1] + b[3]);
        [x0, y0, x1 - x0, y1 - y0]
    })
}

fn inside(r: ViewRect, p: (f32, f32), tol: f32) -> bool {
    p.0 >= r[0] - tol && p.0 <= r[0] + r[2] + tol && p.1 >= r[1] - tol && p.1 <= r[1] + r[3] + tol
}

/// The widgets of `page` in field order.
pub fn placed(s: &mut State, page: usize) -> Vec<Placed> {
    let backend = s.forms.clone();
    match s.tab() {
        Some(t) => placed_in(t, &*backend, page),
        None => Vec::new(),
    }
}

pub fn placed_in(
    t: &mut Tab,
    backend: &dyn pagebara_app_core::forms::FormBackend,
    page: usize,
) -> Vec<Placed> {
    let Some(g) = t.session.geoms.get(page).copied() else {
        return Vec::new();
    };
    t.form(backend)
        .fields()
        .into_iter()
        .flat_map(|f| {
            f.widgets
                .into_iter()
                .filter(|w| w.page == page)
                .map(move |w| Placed {
                    id: w.id,
                    name: f.name.clone(),
                    rect: w.rect,
                    view: to_view(&g, w.rect),
                })
        })
        .collect()
}

/// The selection's frame (view space) on its page.
pub fn selection_frame(s: &mut State) -> Option<(usize, ViewRect)> {
    let t = s.tab_ref()?;
    if t.prep.sel.is_empty() {
        return None;
    }
    let (page, sel) = (t.prep.page, t.prep.sel.clone());
    let r = union(
        placed(s, page)
            .into_iter()
            .filter(|p| sel.contains(&p.id))
            .map(|p| p.view),
    )?;
    Some((page, r))
}

/// Where handle `h` of the selection frame is (page and view point).
pub fn handle_pos(s: &mut State, h: Handle) -> Option<(usize, f64, f64)> {
    let (page, r) = selection_frame(s)?;
    let k = s.tab_ref()?.view.last.k.max(0.01) as f32;
    let (_, (x, y)) = oe::handles(oe::outset(r, PAD_PX / k), 0.0)
        .into_iter()
        .find(|(hh, _)| *hh == h)?;
    Some((page, f64::from(x), f64::from(y)))
}

impl App {
    fn prep_hit(s: &State, x: f64, y: f64) -> Option<(usize, (f32, f32), f32)> {
        let t = s.tab_ref()?;
        let (page, u, v) = t.view.screen_to_page(x, y)?;
        Some((page, (u as f32, v as f32), t.view.last.k.max(0.01) as f32))
    }

    pub fn prep_pointer(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let pressed = s.tab_ref().is_some_and(|t| t.prep.press.is_some());
        match p.kind {
            0 if p.button == 0 => self.prep_press(s, ui, p),
            2 => self.prep_release(s, ui),
            4 => {
                if let Some(t) = s.tab() {
                    t.prep.press = None;
                    t.prep.preview = None;
                }
                self.overlay_changed(s, ui);
            }
            1 | 3 if pressed => self.prep_drag(s, ui, p),
            3 => self.prep_hover(s, ui, p),
            _ => {}
        }
    }

    fn prep_hover(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let cursor = (|| {
            let (page, at, k) = Self::prep_hit(s, p.x, p.y)?;
            if s.tab_ref()?.prep.placing.is_some() {
                return Some(3);
            }
            if let Some(h) = self.prep_handle_at(s, page, at, k) {
                return Some(h.cursor());
            }
            placed(s, page)
                .iter()
                .any(|w| inside(w.view, at, 1.0))
                .then_some(4)
        })()
        .unwrap_or(0);
        let vs = ui.global::<ViewState>();
        if vs.get_cursor() != cursor {
            vs.set_cursor(cursor);
        }
    }

    /// A handle of a single selected widget's frame.
    fn prep_handle_at(&self, s: &mut State, page: usize, at: (f32, f32), k: f32) -> Option<Handle> {
        if s.tab_ref()?.prep.sel.len() != 1 {
            return None;
        }
        let (sp, r) = selection_frame(s)?;
        if sp != page {
            return None;
        }
        oe::handle_at(oe::outset(r, PAD_PX / k), at, HANDLE_PX / k, -1e6)
            .filter(|h| *h != Handle::Rotate)
    }

    fn prep_press(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let Some((page, at, k)) = Self::prep_hit(s, p.x, p.y) else {
            return;
        };
        let Some(t) = s.tab() else { return };
        if let Some(tool) = t.prep.placing {
            t.prep.press = Some(Press::Draw {
                page,
                tool,
                from: at,
                to: at,
            });
            return;
        }
        let Some(g) = t.session.geoms.get(page).copied() else {
            return;
        };
        // A suggested field: selected for Accept or Dismiss.
        if let Some(i) = t
            .prep
            .suggestions
            .iter()
            .rposition(|sg| sg.page == page && inside(to_view(&g, rect4(sg)), at, 0.0))
        {
            return self.prep_action(s, ui, "suggestion", &i.to_string());
        }
        if let Some(h) = self.prep_handle_at(s, page, at, k)
            && let Some((_, r)) = selection_frame(s)
            && let Some(t) = s.tab()
        {
            t.prep.press = Some(Press::Drag {
                page,
                drag: Drag::new(DragKind::Scale(h), frame_of(&g), at, r),
                moved: false,
            });
            return;
        }
        let widgets = placed(s, page);
        let hit = widgets
            .iter()
            .rev()
            .find(|w| inside(w.view, at, 0.0))
            .cloned();
        let Some(t) = s.tab() else { return };
        match hit {
            Some(w) => {
                let add = p.shift || p.ctrl;
                if t.prep.page != page {
                    t.prep.sel.clear();
                    t.prep.page = page;
                }
                if add {
                    match t.prep.sel.iter().position(|x| *x == w.id) {
                        Some(i) => {
                            t.prep.sel.remove(i);
                        }
                        None => t.prep.sel.push(w.id),
                    }
                    return self.prep_selection_changed(s, ui, true);
                }
                let changed = !t.prep.sel.contains(&w.id);
                if changed {
                    t.prep.sel = vec![w.id];
                }
                let sel = t.prep.sel.clone();
                let r = union(
                    widgets
                        .iter()
                        .filter(|x| sel.contains(&x.id))
                        .map(|x| x.view),
                )
                .unwrap_or(w.view);
                let mut drag = Drag::new(DragKind::Move, frame_of(&g), at, r);
                drag.others = widgets
                    .iter()
                    .filter(|x| !sel.contains(&x.id))
                    .map(|x| x.view)
                    .collect();
                t.prep.press = Some(Press::Drag {
                    page,
                    drag,
                    moved: false,
                });
                if changed {
                    self.prep_selection_changed(s, ui, true);
                }
            }
            None => {
                let add = p.shift || p.ctrl;
                if !add && !t.prep.sel.is_empty() {
                    t.prep.sel.clear();
                    self.prep_selection_changed(s, ui, false);
                }
                if let Some(t) = s.tab() {
                    t.prep.press = Some(Press::Marquee {
                        page,
                        from: at,
                        to: at,
                        add,
                    });
                }
            }
        }
    }

    fn prep_drag(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let Some(t) = s.tab() else { return };
        let k = t.view.last.k.max(0.01) as f32;
        let page = match &t.prep.press {
            Some(
                Press::Drag { page, .. } | Press::Draw { page, .. } | Press::Marquee { page, .. },
            ) => *page,
            None => return,
        };
        let Some((u, v)) = t.view.screen_to_page_of(page, p.x, p.y) else {
            return;
        };
        let at = (u as f32, v as f32);
        match &mut t.prep.press {
            Some(Press::Drag { drag, moved, .. }) => {
                let d = (at.0 - drag.start.0).hypot(at.1 - drag.start.1) * k;
                if !*moved && d < 3.0 {
                    return;
                }
                *moved = true;
                let snap = if p.alt { 0.0 } else { 5.0 / k };
                let result = drag.update(at, p.shift, snap);
                t.prep.preview = Some((page, result, at));
            }
            Some(Press::Draw { to, .. } | Press::Marquee { to, .. }) => *to = at,
            None => return,
        }
        self.overlay_changed(s, ui);
    }

    fn prep_release(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab() else { return };
        let press = t.prep.press.take();
        let preview = t.prep.preview.take();
        let k = t.view.last.k.max(0.01) as f32;
        match press {
            Some(Press::Drag {
                page,
                drag,
                moved: true,
            }) => {
                let Some((_, r, _)) = preview else {
                    return self.overlay_changed(s, ui);
                };
                let sel = t.prep.sel.clone();
                let rects: Vec<(ObjRef, [f64; 4])> = placed(s, page)
                    .into_iter()
                    .filter(|w| sel.contains(&w.id))
                    .map(|w| (w.id, form_prep::transformed(w.rect, &r.matrix)))
                    .collect();
                let label = match drag.kind {
                    DragKind::Move if sel.len() > 1 => tr!("Move fields"),
                    DragKind::Move => tr!("Move field"),
                    _ => tr!("Resize field"),
                };
                self.prep_set_rects(s, ui, &label, rects);
            }
            Some(Press::Draw {
                page,
                tool,
                from,
                to,
            }) => {
                let Some(g) = t.session.geoms.get(page).copied() else {
                    return;
                };
                let small = (to.0 - from.0).abs() * k < 4.0 && (to.1 - from.1).abs() * k < 4.0;
                let rect = if small {
                    let r = g.rect_to_pdf([f64::from(from.0), f64::from(from.1), 0.0, 0.0]);
                    tool.rect_at(r[0], r[3])
                } else {
                    let (x, y) = (from.0.min(to.0), from.1.min(to.1));
                    g.rect_to_pdf([
                        f64::from(x),
                        f64::from(y),
                        f64::from((to.0 - from.0).abs().max(8.0)),
                        f64::from((to.1 - from.1).abs().max(8.0)),
                    ])
                };
                t.prep.placing = None;
                ui.global::<ViewState>().set_cursor(0);
                self.prep_add(s, ui, tool, page, rect);
            }
            Some(Press::Marquee {
                page,
                from,
                to,
                add,
            }) => {
                if (to.0 - from.0).abs() * k < 3.0 && (to.1 - from.1).abs() * k < 3.0 {
                    return self.overlay_changed(s, ui);
                }
                let m = [
                    from.0.min(to.0),
                    from.1.min(to.1),
                    (to.0 - from.0).abs(),
                    (to.1 - from.1).abs(),
                ];
                let found: Vec<ObjRef> = placed(s, page)
                    .into_iter()
                    .filter(|w| {
                        w.view[0] >= m[0]
                            && w.view[1] >= m[1]
                            && w.view[0] + w.view[2] <= m[0] + m[2]
                            && w.view[1] + w.view[3] <= m[1] + m[3]
                    })
                    .map(|w| w.id)
                    .collect();
                let Some(t) = s.tab() else { return };
                if !add || t.prep.page != page {
                    t.prep.sel.clear();
                }
                t.prep.page = page;
                for id in found {
                    if !t.prep.sel.contains(&id) {
                        t.prep.sel.push(id);
                    }
                }
                self.prep_selection_changed(s, ui, true);
            }
            _ => self.overlay_changed(s, ui),
        }
    }

    /// Runs a form change as one step; `Some` when it worked.
    fn prep_edit(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        label: &str,
        f: impl FnOnce(
            &mut dyn pagebara_app_core::forms::FormSession,
            &mut pagebara_syntax::Document,
        ) -> pagebara_app_core::forms::FormResult,
    ) -> Option<FormChange> {
        let r = self.form_edit(s, ui, label, f);
        sync(s, ui);
        self.overlay_changed(s, ui);
        r
    }

    fn prep_set_rects(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        label: &str,
        rects: Vec<(ObjRef, [f64; 4])>,
    ) {
        if rects.is_empty() {
            return self.overlay_changed(s, ui);
        }
        if self
            .prep_edit(s, ui, label, |f, d| f.set_rects(d, &rects))
            .is_some()
        {
            self.message(s, &format!("{label}."));
        }
    }

    /// Creates a field of `tool` at `rect` (user space) and selects it.
    pub fn prep_add(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        tool: Tool,
        page: usize,
        rect: [f64; 4],
    ) {
        let nf = tool.new_field(page, rect);
        let mut name = String::new();
        let made = self.prep_edit(s, ui, &add_label(tool), |f, d| {
            let (c, n) = f.add_field(d, &nf)?;
            name = n;
            Ok(c)
        });
        if made.is_some() {
            self.prep_select_field(s, ui, &name, page);
            self.message(s, &added_message(tool, &name));
        }
    }

    /// Selects the widgets of field `name` on `page`.
    pub fn prep_select_field(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        name: &str,
        page: usize,
    ) {
        let ids: Vec<ObjRef> = placed(s, page)
            .into_iter()
            .filter(|w| w.name == name)
            .map(|w| w.id)
            .collect();
        if let Some(t) = s.tab() {
            t.prep.page = page;
            t.prep.sel = ids;
        }
        self.prep_selection_changed(s, ui, false);
    }

    pub fn prep_selection_changed(self: &Rc<Self>, s: &mut State, ui: &AppWindow, announce: bool) {
        sync(s, ui);
        if announce {
            let text = ui.global::<PrepState>().get_summary().to_string();
            if !text.is_empty() {
                self.message(s, &tr!("Selected: {}.", text));
            }
        }
        self.overlay_changed(s, ui);
    }

    /// Keys with Prepare form; `None` when they are not the tool's.
    pub fn prep_keys(
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
        let has = !t.prep.sel.is_empty();
        if ctrl {
            match c.to_ascii_lowercase() {
                'd' if has => self.prep_action(s, ui, "duplicate", ""),
                'a' => self.prep_action(s, ui, "select-all", ""),
                _ => return None,
            }
            return Some(true);
        }
        if key(Key::Escape) {
            if t.prep.placing.take().is_some() {
                ui.global::<ViewState>().set_cursor(0);
                sync(s, ui);
                return Some(true);
            }
            if has {
                t.prep.sel.clear();
                self.prep_selection_changed(s, ui, false);
                self.message(s, &tr!("Nothing selected."));
                return Some(true);
            }
            return None;
        }
        if key(Key::Tab) || key(Key::Backtab) {
            self.prep_cycle(s, ui, !(shift || key(Key::Backtab)));
            return Some(true);
        }
        if !has {
            return None;
        }
        if key(Key::Delete) || key(Key::Backspace) {
            self.prep_action(s, ui, "delete", "");
            return Some(true);
        }
        let step = if shift { 10.0 } else { 1.0 };
        let (dx, dy) = match () {
            _ if key(Key::LeftArrow) => (-step, 0.0),
            _ if key(Key::RightArrow) => (step, 0.0),
            _ if key(Key::UpArrow) => (0.0, -step),
            _ if key(Key::DownArrow) => (0.0, step),
            _ => return None,
        };
        let page = t.prep.page;
        let g = t.session.geoms.get(page).copied()?;
        let m = oe::move_by_view(&frame_of(&g), dx, dy);
        let sel = t.prep.sel.clone();
        let rects: Vec<(ObjRef, [f64; 4])> = placed(s, page)
            .into_iter()
            .filter(|w| sel.contains(&w.id))
            .map(|w| (w.id, form_prep::transformed(w.rect, &m)))
            .collect();
        self.prep_set_rects(
            s,
            ui,
            &if sel.len() > 1 {
                tr!("Move fields")
            } else {
                tr!("Move field")
            },
            rects,
        );
        Some(true)
    }

    /// Selects the next or previous widget of the page in tab order.
    fn prep_cycle(self: &Rc<Self>, s: &mut State, ui: &AppWindow, forward: bool) {
        let backend = s.forms.clone();
        let Some(t) = s.tab() else { return };
        let page = if t.prep.sel.is_empty() {
            t.view.current_page
        } else {
            t.prep.page
        };
        let order = t.form(&*backend).tab_order(page);
        if order.is_empty() {
            return self.message(s, &tr!("There are no fields on this page."));
        }
        let at = t
            .prep
            .sel
            .last()
            .and_then(|id| order.iter().position(|o| o == id));
        let n = order.len();
        let i = match (at, forward) {
            (None, true) => 0,
            (None, false) => n - 1,
            (Some(i), true) => (i + 1) % n,
            (Some(i), false) => (i + n - 1) % n,
        };
        t.prep.page = page;
        t.prep.sel = vec![order[i]];
        self.prep_selection_changed(s, ui, true);
    }

    /// The tool changed: leaving Prepare form clears its state.
    pub fn prep_tool_changed(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let tool = ui.global::<AppState>().get_tool();
        if let Some(t) = s.tab() {
            t.prep.press = None;
            t.prep.preview = None;
            if tool != TOOL {
                t.prep.sel.clear();
                t.prep.placing = None;
                t.prep.suggestions.clear();
                t.prep.suggestion = None;
            }
        }
        if tool == TOOL {
            self.message(
                s,
                &tr!("Prepare form: pick a field type and click the page, or click a field to change it."),
            );
        }
        sync(s, ui);
    }
}

fn rect4(sg: &Suggestion) -> [f64; 4] {
    [sg.rect.x0, sg.rect.y0, sg.rect.x1, sg.rect.y1]
}

fn r64(r: ViewRect) -> [f64; 4] {
    r.map(f64::from)
}

/// What Prepare form draws over page `page`: every field with its name, suggested
/// fields, the selection with its handles, a drag in progress, the box being drawn.
pub fn overlay(
    out: &mut Vec<OverlayItem>,
    t: &mut Tab,
    pl: &Placement,
    page: usize,
    fields: &[Placed],
) {
    let Some(g) = t.session.geoms.get(page).copied() else {
        return;
    };
    let k = t.view.last.k.max(0.01) as f32;
    let selected = |id: ObjRef| t.prep.page == page && t.prep.sel.contains(&id);
    for w in fields {
        out.push(OverlayItem {
            kind: 1,
            fill: FIELD,
            stroke: ACCENT,
            width: 0.75,
            ..at(pl, r64(w.view))
        });
        // The name inside the field's top left corner, or above a field too small for it.
        let size = 7.0;
        let text_w = w.name.chars().count() as f32 * size * 0.62 + 6.0;
        let h = size + 5.0;
        let y = if text_w > w.view[2] || h > w.view[3] {
            w.view[1] - h - 1.0
        } else {
            w.view[1]
        };
        out.push(OverlayItem {
            kind: 3,
            fill: slint::Color::from_argb_u8(0xD9, 0xEF, 0xF6, 0xFF),
            stroke: NAME,
            text: w.name.as_str().into(),
            font_size: size,
            ..at(pl, r64([w.view[0], y, text_w, h]))
        });
    }
    for (i, sg) in t.prep.suggestions.iter().enumerate() {
        if sg.page != page {
            continue;
        }
        let r = to_view(&g, rect4(sg));
        let current = t.prep.suggestion == Some(i);
        out.push(OverlayItem {
            kind: 1,
            fill: SUGGESTED,
            stroke: SUGGESTED_EDGE,
            width: if current { 2.0 } else { 1.0 },
            ..at(pl, r64(r))
        });
    }
    let preview = t.prep.preview.as_ref().filter(|p| p.0 == page);
    match preview {
        Some((_, result, at_pt)) => {
            for w in fields.iter().filter(|w| selected(w.id)) {
                let m = &result.view;
                let a = m.apply(pagebara_edit::Point::new(w.view[0], w.view[1]));
                let b = m.apply(pagebara_edit::Point::new(
                    w.view[0] + w.view[2],
                    w.view[1] + w.view[3],
                ));
                let r = [
                    a.x.min(b.x),
                    a.y.min(b.y),
                    (a.x - b.x).abs(),
                    (a.y - b.y).abs(),
                ];
                out.push(OverlayItem {
                    kind: 1,
                    halo: true,
                    ..at(pl, r64(r))
                });
            }
            for gd in &result.guides {
                let r = if gd.vertical {
                    [gd.at - 0.4, gd.from, 0.8, gd.to - gd.from]
                } else {
                    [gd.from, gd.at - 0.4, gd.to - gd.from, 0.8]
                };
                out.push(fill(pl, r64(r), GUIDE));
            }
            let w = result.label.chars().count() as f32 * 5.6 + 10.0;
            out.push(OverlayItem {
                kind: 3,
                fill: LABEL_BG,
                stroke: slint::Color::from_rgb_u8(0xFF, 0xFF, 0xFF),
                text: result.label.as_str().into(),
                font_size: 9.0,
                ..at(pl, r64([at_pt.0 + 14.0, at_pt.1 + 14.0, w, 15.0]))
            });
        }
        None => {
            let sel: Vec<&Placed> = fields.iter().filter(|w| selected(w.id)).collect();
            if sel.len() > 1 {
                for w in &sel {
                    out.push(OverlayItem {
                        kind: 1,
                        halo: true,
                        ..at(pl, r64(oe::outset(w.view, 1.0)))
                    });
                }
            }
            if let Some(r) = union(sel.iter().map(|w| w.view)) {
                out.push(OverlayItem {
                    kind: if sel.len() == 1 { 9 } else { 1 },
                    width: 0.0,
                    halo: sel.len() > 1,
                    ..at(pl, r64(oe::outset(r, PAD_PX / k)))
                });
            }
        }
    }
    match &t.prep.press {
        Some(
            Press::Draw {
                page: p, from, to, ..
            }
            | Press::Marquee {
                page: p, from, to, ..
            },
        ) if *p == page => {
            let r = [
                from.0.min(to.0),
                from.1.min(to.1),
                (to.0 - from.0).abs(),
                (to.1 - from.1).abs(),
            ];
            if r[2] > 1.0 || r[3] > 1.0 {
                out.push(OverlayItem {
                    kind: 7,
                    ..at(pl, r64(r))
                });
            }
        }
        _ => {}
    }
}

/// The undo step of adding a field with `tool`.
fn add_label(tool: Tool) -> String {
    match tool {
        Tool::Text => tr!("Add text field"),
        Tool::Date => tr!("Add date field"),
        Tool::CheckBox => tr!("Add check box"),
        Tool::Radio => tr!("Add radio button"),
        Tool::ComboBox => tr!("Add drop-down list"),
        Tool::ListBox => tr!("Add list box"),
        Tool::Button => tr!("Add button"),
        Tool::Signature => tr!("Add signature field"),
    }
}

/// The status after adding the field `name` with `tool`.
fn added_message(tool: Tool, name: &str) -> String {
    match tool {
        Tool::Text => tr!("Added text field “{}”.", name),
        Tool::Date => tr!("Added date field “{}”.", name),
        Tool::CheckBox => tr!("Added check box “{}”.", name),
        Tool::Radio => tr!("Added radio button “{}”.", name),
        Tool::ComboBox => tr!("Added drop-down list “{}”.", name),
        Tool::ListBox => tr!("Added list box “{}”.", name),
        Tool::Button => tr!("Added button “{}”.", name),
        Tool::Signature => tr!("Added signature field “{}”.", name),
    }
}

/// How to place a field with `tool`.
fn place_message(tool: Tool) -> String {
    match tool {
        Tool::Text => tr!("Click the page to place a text field, or drag to draw it."),
        Tool::Date => tr!("Click the page to place a date field, or drag to draw it."),
        Tool::CheckBox => tr!("Click the page to place a check box, or drag to draw it."),
        Tool::Radio => tr!("Click the page to place a radio button, or drag to draw it."),
        Tool::ComboBox => tr!("Click the page to place a drop-down list, or drag to draw it."),
        Tool::ListBox => tr!("Click the page to place a list box, or drag to draw it."),
        Tool::Button => tr!("Click the page to place a button, or drag to draw it."),
        Tool::Signature => tr!("Click the page to place a signature field, or drag to draw it."),
    }
}
