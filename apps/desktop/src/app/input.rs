//! Pointer input on the pages: what a press, drag and release do with each tool.

use std::rc::Rc;
use std::time::{Duration, Instant};

use pagebara_annot::{Annotation, Color, Kind, LinkAction, MarkupKind, Point, Quad, Rect};
use pagebara_app_core::annots;
use pagebara_app_core::pages::PageGeom;
use pagebara_app_core::text_index;
use pagebara_i18n::{tr, trn};
use slint::ComponentHandle;

use super::{App, Sel, State, TextSel};
use crate::{AppState, AppWindow, OverlayItem, ViewState};

/// Palette shared with the colour swatches (`Palette2` in the UI).
pub const COLOURS: [[u8; 3]; 7] = [
    [0xFA, 0xCC, 0x15],
    [0x4A, 0xDE, 0x80],
    [0x60, 0xA5, 0xFA],
    [0xF4, 0x72, 0xB6],
    [0xFB, 0x92, 0x3C],
    [0xDC, 0x26, 0x26],
    [0x18, 0x18, 0x1B],
];

pub fn colour(i: i32) -> Color {
    let [r, g, b] = COLOURS[(i.max(0) as usize).min(COLOURS.len() - 1)];
    Color::Rgb(
        f32::from(r) / 255.0,
        f32::from(g) / 255.0,
        f32::from(b) / 255.0,
    )
}

/// The swatch closest to `c`.
pub fn colour_index(c: Option<Color>) -> i32 {
    let Some(c) = c else { return -1 };
    let [r, g, b] = c.to_rgb().map(|v| v * 255.0);
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

#[derive(Clone, Copy, Debug)]
pub struct Pointer {
    pub kind: i32,
    pub x: f64,
    pub y: f64,
    pub button: i32,
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
}

/// Counts quick successive presses at one place: 1, 2 (word), 3 (line).
#[derive(Debug, Default)]
pub struct Clicks {
    last: Option<(Instant, f64, f64)>,
    count: u8,
}

impl Clicks {
    fn press(&mut self, x: f64, y: f64) -> u8 {
        let now = Instant::now();
        let near = self.last.is_some_and(|(t, lx, ly)| {
            now - t < Duration::from_millis(450) && (lx - x).abs() < 5.0 && (ly - y).abs() < 5.0
        });
        self.count = if near { self.count % 3 + 1 } else { 1 };
        self.last = Some((now, x, y));
        self.count
    }
}

/// A press in progress and what it is doing.
#[derive(Clone, Debug, Default)]
pub enum Gesture {
    #[default]
    None,
    Pan,
    /// Selecting text; with `markup`, the selection becomes a highlight on release.
    Select {
        markup: bool,
    },
    Ink {
        page: usize,
        points: Vec<(f64, f64)>,
    },
    Shape {
        page: usize,
        from: (f64, f64),
        to: (f64, f64),
    },
    TextBox {
        page: usize,
        from: (f64, f64),
        to: (f64, f64),
    },
    Note {
        page: usize,
        at: (f64, f64),
    },
    Place {
        page: usize,
        from: (f64, f64),
        to: (f64, f64),
    },
    /// A box whose text is copied (live text).
    CopyArea {
        page: usize,
        from: (f64, f64),
        to: (f64, f64),
    },
    /// A box for a redaction or a signature with a certificate.
    Area {
        page: usize,
        from: (f64, f64),
        to: (f64, f64),
    },
    Move {
        sel: Sel,
        orig: Annotation,
        from: (f64, f64),
        now: Annotation,
        moved: bool,
    },
    Resize {
        sel: Sel,
        orig: Annotation,
        corner: usize,
        now: Annotation,
    },
    Link {
        page: usize,
        action: LinkAction,
        at: (f64, f64),
    },
}

impl Gesture {
    /// The annotation being moved or resized, as it is drawn now.
    pub fn preview(&self) -> Option<(Sel, &Annotation)> {
        match self {
            Gesture::Move { sel, now, .. } | Gesture::Resize { sel, now, .. } => Some((*sel, now)),
            _ => None,
        }
    }
}

fn pdf_rect(r: [f64; 4]) -> Rect {
    Rect::new(r[0], r[1], r[2], r[3])
}

impl App {
    pub fn pointer(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        if p.kind == 0 && ui.global::<ViewState>().get_field_editing() {
            self.field_done(s, ui, true);
        }
        if s.tabs.is_empty() || ui.global::<ViewState>().get_editing() && p.kind == 0 {
            if p.kind == 0 {
                self.editor_done(s, ui, true);
            }
            if s.tabs.is_empty() {
                return;
            }
        }
        if Self::tool(ui) == super::edit::TOOL
            && p.button != 2
            && !matches!(s.gesture, Gesture::Pan)
        {
            return self.edit_pointer(s, ui, p);
        }
        if Self::tool(ui) == super::prepare::TOOL
            && p.button != 2
            && !matches!(s.gesture, Gesture::Pan)
        {
            return self.prep_pointer(s, ui, p);
        }
        match p.kind {
            0 => self.press(s, ui, p),
            2 => self.release(s, ui, p),
            4 => {
                s.gesture = super::Gesture::None;
                self.overlay_changed(s, ui);
            }
            _ if matches!(s.gesture, Gesture::None) => {
                s.input.hover = Some(p);
                self.hover(s, ui, p)
            }
            _ => self.drag(s, ui, p),
        }
    }

    fn tool(ui: &AppWindow) -> i32 {
        ui.global::<AppState>().get_tool()
    }

    /// The page and its point (displayed points, then user space) under the pointer.
    fn hit(s: &State, x: f64, y: f64) -> Option<(usize, (f64, f64), Point, PageGeom)> {
        let t = s.tab_ref()?;
        let (page, u, v) = t.view.screen_to_page(x, y)?;
        let g = *t.session.geoms.get(page)?;
        let (px, py) = g.to_pdf(u, v);
        Some((page, (u, v), Point::new(px, py), g))
    }

    /// Points per screen pixel at the current zoom.
    fn px(s: &State) -> f64 {
        s.tab_ref().map_or(1.0, |t| 1.0 / t.view.last.k.max(0.01))
    }

    fn hover(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let tool = Self::tool(ui);
        let mut live_at = None;
        let cursor = (|| {
            let (page, uv, pt, _) = Self::hit(s, p.x, p.y)?;
            if s.input.area_copy && matches!(tool, 0 | 1) {
                return Some(3);
            }
            if let Some(c) = self.handle_cursor(s, page, uv) {
                return Some(c);
            }
            let tol = 4.0 * Self::px(s);
            let t = s.tab()?;
            let list = t.annots(page);
            match tool {
                0 | 4 => {
                    if annots::hit_test(&list, pt, tol)
                        .is_some_and(|i| tool == 0 || is_signature(&list[i]))
                    {
                        return Some(4);
                    }
                    if tool == 0 && annots::link_at(&list, pt).is_some() {
                        return Some(2);
                    }
                    if tool == 4 {
                        return Some(3);
                    }
                    let text = t.session.sel_text_cached(page)?;
                    if text_index::over_text(&text, pt.x as f32, pt.y as f32) {
                        return Some(1);
                    }
                    live_at = Some((page, pt));
                    Some(if Self::live_busy_at(s, page, pt) {
                        9
                    } else {
                        0
                    })
                }
                1 => {
                    if let Some(text) = t.session.sel_text_cached(page)
                        && !text_index::over_text(&text, pt.x as f32, pt.y as f32)
                    {
                        live_at = Some((page, pt));
                    }
                    Some(1)
                }
                2 | 3 => Some(3),
                _ => Some(0),
            }
        })()
        .unwrap_or(0);
        let vs = ui.global::<ViewState>();
        if vs.get_cursor() != cursor {
            vs.set_cursor(cursor);
        }
        if let Some((page, pt)) = live_at {
            self.live_point(s, page, pt);
        }
        // Text for the cursor is extracted ahead, off the UI thread.
        if matches!(tool, 0 | 1)
            && let Some((page, ..)) = Self::hit(s, p.x, p.y)
            && let Some(t) = s.tab_ref()
            && t.session.text.cached(page).is_none()
        {
            let text = t.session.text.clone();
            pagebara_thread::spawn("text-extract", move || {
                text.page(page);
            });
        }
    }

    /// A resize cursor over a handle of the selected annotation.
    fn handle_cursor(&self, s: &mut State, page: usize, uv: (f64, f64)) -> Option<i32> {
        let corner = self.handle_at(s, page, uv)?;
        Some(if corner == 0 || corner == 3 { 7 } else { 8 })
    }

    /// Which corner handle of the selected annotation is at `uv` (0 top left, 1 top right,
    /// 2 bottom left, 3 bottom right).
    fn handle_at(&self, s: &mut State, page: usize, (u, v): (f64, f64)) -> Option<usize> {
        let tol = 7.0 * Self::px(s);
        let t = s.tab()?;
        let sel = t.selected.filter(|sel| sel.page == page)?;
        let a = t.annot(sel)?;
        if matches!(a.kind, Kind::Markup { .. } | Kind::Text { .. }) {
            return None;
        }
        let g = t.session.geoms[page];
        let [x, y, w, h] = g.rect_to_view([a.rect.x0, a.rect.y0, a.rect.x1, a.rect.y1]);
        [(x, y), (x + w, y), (x, y + h), (x + w, y + h)]
            .iter()
            .position(|&(cx, cy)| (cx - u).abs() <= tol && (cy - v).abs() <= tol)
    }

    fn press(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        if p.button == 2 || (p.button == 0 && p.ctrl && Self::tool(ui) != 0) {
            s.gesture = Gesture::Pan;
            if let Some(t) = s.tab() {
                t.view.drag_start();
            }
            s.input.pan_from = (p.x, p.y);
            ui.global::<ViewState>().set_cursor(6);
            return;
        }
        if p.button == 1 {
            return self.context_menu(s, ui, p);
        }
        if p.button != 0 {
            return;
        }
        let clicks = s.input.press(p.x, p.y);
        let Some((page, uv, pt, _)) = Self::hit(s, p.x, p.y) else {
            self.start_pan(s, ui, p);
            return;
        };
        let tool = Self::tool(ui);
        s.input.last_press = Some((page, pt));
        if matches!(tool, 0 | 1) && (s.input.area_copy || p.alt) {
            self.deselect(s, ui);
            self.start_copy_area(s, page, uv);
            return self.overlay_changed(s, ui);
        }
        let tol = 4.0 * Self::px(s);
        let area = s.signing.awaiting_box
            || (tool == 6 && ui.global::<crate::RedactState>().get_mode() == 1);
        // The selected annotation can be moved and resized with any tool.
        if let Some(corner) = self.handle_at(s, page, uv)
            && let Some(t) = s.tab()
            && let Some(sel) = t.selected
            && let Some(a) = t.annot(sel)
        {
            s.gesture = Gesture::Resize {
                sel,
                now: a.clone(),
                orig: a,
                corner,
            };
            return;
        }
        let Some(t) = s.tab() else { return };
        let list = t.annots(page);
        let hit = annots::hit_test(&list, pt, tol).filter(|&i| {
            tool == 0
                || is_signature(&list[i])
                || (tool == 6 && super::redaction::is_mark(&list[i].kind))
                || t.selected.is_some_and(|sel| list[i].id == Some(sel.id))
        });
        if let Some(i) = hit
            && let Some(id) = list[i].id
            && tool != 1
        {
            let sel = Sel { page, id };
            let a = list[i].clone();
            t.selected = Some(sel);
            t.text_sel = None;
            s.gesture = Gesture::Move {
                sel,
                now: a.clone(),
                orig: a,
                from: (pt.x, pt.y),
                moved: false,
            };
            self.selection_changed(s, ui);
            return;
        }
        match tool {
            _ if area => {
                self.deselect(s, ui);
                s.gesture = Gesture::Area {
                    page,
                    from: uv,
                    to: uv,
                };
            }
            6 => self.start_text_select(s, ui, page, pt, clicks, true),
            0 => {
                if let Some(link) = annots::link_at(&list, pt)
                    && let Kind::Link {
                        action: Some(action),
                        ..
                    } = &link.kind
                {
                    s.gesture = Gesture::Link {
                        page,
                        action: action.clone(),
                        at: (p.x, p.y),
                    };
                    return;
                }
                let text = t.session.sel_text(page);
                if !text_index::over_text(&text, pt.x as f32, pt.y as f32) && clicks == 1 {
                    let had = t.selected.take().is_some() | t.text_sel.take().is_some();
                    if had {
                        self.selection_changed(s, ui);
                    }
                    self.start_pan(s, ui, p);
                    return;
                }
                self.start_text_select(s, ui, page, pt, clicks, false);
            }
            1 => self.start_text_select(s, ui, page, pt, clicks, true),
            2 => {
                let shape = ui.global::<AppState>().get_shape();
                s.gesture = if shape == 0 {
                    Gesture::Ink {
                        page,
                        points: vec![uv],
                    }
                } else {
                    Gesture::Shape {
                        page,
                        from: uv,
                        to: uv,
                    }
                };
                self.deselect(s, ui);
            }
            3 => {
                self.deselect(s, ui);
                s.gesture = if ui.global::<AppState>().get_text_mode() == 1 {
                    Gesture::Note { page, at: uv }
                } else {
                    Gesture::TextBox {
                        page,
                        from: uv,
                        to: uv,
                    }
                };
            }
            4 => {
                self.deselect(s, ui);
                s.gesture = Gesture::Place {
                    page,
                    from: uv,
                    to: uv,
                };
            }
            _ => {
                if !self.field_at(s, ui, page, pt) {
                    self.start_pan(s, ui, p);
                }
            }
        }
        self.overlay_changed(s, ui);
    }

    fn start_pan(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        s.gesture = Gesture::Pan;
        s.input.pan_from = (p.x, p.y);
        if let Some(t) = s.tab() {
            t.view.drag_start();
        }
        ui.global::<ViewState>().set_cursor(6);
    }

    fn deselect(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        if let Some(t) = s.tab()
            && (t.selected.take().is_some() | t.text_sel.take().is_some())
        {
            self.selection_changed(s, ui);
        }
    }

    fn start_text_select(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        page: usize,
        pt: Point,
        clicks: u8,
        markup: bool,
    ) {
        let redacting = Self::tool(ui) == 6;
        let Some(t) = s.tab() else { return };
        let text = if redacting {
            t.session.text.page(page)
        } else {
            t.session.sel_text(page)
        };
        let Some(caret) = text_index::caret_at(&text, pt.x as f32, pt.y as f32) else {
            if markup {
                self.message(s, &tr!("There is no text to mark on this page."));
            }
            return;
        };
        let had_annot = t.selected.take().is_some();
        let unit = match clicks {
            2 => text_index::word_at(&text, caret.min(text.chars.len().saturating_sub(1))),
            3 => text_index::line_at(&text, caret.min(text.chars.len().saturating_sub(1))),
            _ => None,
        };
        t.text_sel = Some(match unit {
            Some(r) => TextSel {
                page,
                anchor: r.start,
                caret: r.end,
            },
            None => TextSel {
                page,
                anchor: caret,
                caret,
            },
        });
        s.gesture = Gesture::Select { markup };
        if had_annot {
            self.selection_changed(s, ui);
        }
    }

    fn drag(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let from = s.input.pan_from;
        let Some(t) = s.tabs.get_mut(s.active) else {
            return;
        };
        let k = t.view.last.k.max(0.01);
        match &mut s.gesture {
            Gesture::Pan => {
                t.view.drag(p.x - from.0, p.y - from.1);
                self.refresh(s, ui);
                return;
            }
            Gesture::Select { .. } => {
                let Some(sel) = t.text_sel else { return };
                let Some((u, v)) = t.view.screen_to_page_of(sel.page, p.x, p.y) else {
                    return;
                };
                let g = t.session.geoms[sel.page];
                let (x, y) = g.to_pdf(u, v);
                let text = if Self::tool(ui) == 6 {
                    t.session.text.page(sel.page)
                } else {
                    t.session.sel_text(sel.page)
                };
                if let Some(c) = text_index::caret_at(&text, x as f32, y as f32) {
                    let new = TextSel { caret: c, ..sel };
                    if Some(new) != t.text_sel {
                        t.text_sel = Some(new);
                        t.overlay_dirty = true;
                    }
                }
            }
            Gesture::Ink { page, points } => {
                if let Some(uv) = t.view.screen_to_page_of(*page, p.x, p.y) {
                    let last = points.last().copied().unwrap_or(uv);
                    if (last.0 - uv.0).hypot(last.1 - uv.1) * k >= 1.5 {
                        points.push(uv);
                        t.overlay_dirty = true;
                    }
                }
            }
            Gesture::Shape { page, to, .. }
            | Gesture::CopyArea { page, to, .. }
            | Gesture::TextBox { page, to, .. }
            | Gesture::Place { page, to, .. }
            | Gesture::Area { page, to, .. } => {
                if let Some(uv) = t.view.screen_to_page_of(*page, p.x, p.y) {
                    *to = uv;
                    t.overlay_dirty = true;
                }
            }
            Gesture::Move {
                sel,
                orig,
                from,
                now,
                moved,
            } => {
                if let Some(uv) = t.view.screen_to_page_of(sel.page, p.x, p.y) {
                    let g = t.session.geoms[sel.page];
                    let (x, y) = g.to_pdf(uv.0, uv.1);
                    let (dx, dy) = (x - from.0, y - from.1);
                    if !*moved && dx.hypot(dy) * k < 3.0 {
                        return;
                    }
                    *moved = true;
                    *now = annots::translated(orig, dx, dy);
                    t.overlay_dirty = true;
                    ui.global::<ViewState>().set_cursor(4);
                }
            }
            Gesture::Resize {
                sel,
                orig,
                corner,
                now,
            } => {
                if let Some(uv) = t.view.screen_to_page_of(sel.page, p.x, p.y) {
                    let g = t.session.geoms[sel.page];
                    let r = orig.rect;
                    let [x, y, w, h] = g.rect_to_view([r.x0, r.y0, r.x1, r.y1]);
                    let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + h);
                    match corner {
                        0 => (x0, y0) = uv,
                        1 => (x1, y0) = uv,
                        2 => (x0, y1) = uv,
                        _ => (x1, y1) = uv,
                    }
                    // Stamps (signatures) keep their proportions.
                    if matches!(orig.kind, Kind::Stamp { .. }) || p.shift {
                        let aspect = h / w.max(0.01);
                        let nw = (x1 - x0).abs();
                        let nh = nw * aspect;
                        if *corner < 2 {
                            y0 = y1 - nh * (y1 - y0).signum();
                        } else {
                            y1 = y0 + nh * (y1 - y0).signum();
                        }
                    }
                    let min = 6.0;
                    if (x1 - x0).abs() < min || (y1 - y0).abs() < min {
                        return;
                    }
                    let to =
                        g.rect_to_pdf([x0.min(x1), y0.min(y1), (x1 - x0).abs(), (y1 - y0).abs()]);
                    *now = annots::resized(orig, pdf_rect(to));
                    t.overlay_dirty = true;
                }
            }
            Gesture::Link { .. } | Gesture::Note { .. } | Gesture::None => return,
        }
        self.refresh(s, ui);
    }

    /// Ends the gesture in progress as if the pointer was let go (keyboard placement).
    pub fn finish_gesture(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let p = Pointer {
            kind: 2,
            x: f64::NAN,
            y: f64::NAN,
            button: 0,
            ctrl: false,
            shift: false,
            alt: false,
        };
        self.release(s, ui, p);
    }

    fn release(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let gesture = std::mem::take(&mut s.gesture);
        let author = ui.global::<AppState>().get_author().to_string();
        let st = ui.global::<AppState>();
        let colour_i = st.get_colour();
        match gesture {
            Gesture::None => {}
            Gesture::Pan => self.hover(s, ui, p),
            Gesture::Select { markup } => {
                let Some(t) = s.tab() else { return };
                let Some(sel) = t.text_sel else { return };
                if sel.anchor == sel.caret {
                    t.text_sel = None;
                    t.overlay_dirty = true;
                } else if markup {
                    self.markup_selection(s, ui, None);
                } else {
                    self.selection_changed(s, ui);
                }
            }
            Gesture::Ink { page, points } => {
                if !self.may_annotate(s, ui) {
                    return self.overlay_changed(s, ui);
                }
                let Some(t) = s.tab_ref() else { return };
                let g = t.session.geoms[page];
                let mut pts: Vec<Point> = points
                    .iter()
                    .map(|&(u, v)| {
                        let (x, y) = g.to_pdf(u, v);
                        Point::new(x, y)
                    })
                    .collect();
                if st.get_smoothing() {
                    pts = annots::smooth(&annots::smooth(&pts));
                }
                let mut a = Annotation::new(Kind::Ink { strokes: vec![pts] }, Rect::default());
                a.color = Some(colour(colour_i));
                a.border.width = f64::from(st.get_pen_width());
                self.add_annotation(s, ui, page, a, &author, &tr!("Add drawing"));
            }
            Gesture::Shape { page, from, to } => {
                let Some(t) = s.tab_ref() else { return };
                let g = t.session.geoms[page];
                let shape = st.get_shape();
                let (mut tu, mut tv) = to;
                if p.shift {
                    let (du, dv) = (tu - from.0, tv - from.1);
                    if shape <= 2 {
                        let d = du.abs().max(dv.abs());
                        (tu, tv) = (from.0 + d * du.signum(), from.1 + d * dv.signum());
                    } else if du.abs() > dv.abs() * 2.0 {
                        tv = from.1;
                    } else if dv.abs() > du.abs() * 2.0 {
                        tu = from.0;
                    }
                }
                if (tu - from.0).hypot(tv - from.1) < 3.0 {
                    return self.overlay_changed(s, ui);
                }
                if !self.may_annotate(s, ui) {
                    return self.overlay_changed(s, ui);
                }
                let width = f64::from(st.get_pen_width());
                let c = colour(colour_i);
                let (x0, y0) = g.to_pdf(from.0, from.1);
                let (x1, y1) = g.to_pdf(tu, tv);
                let rect = Rect::new(x0, y0, x1, y1);
                let (a, label) = match shape {
                    1 => (
                        Annotation::new(Kind::Square { interior: None }, rect),
                        tr!("Add box"),
                    ),
                    2 => (
                        Annotation::new(Kind::Circle { interior: None }, rect),
                        tr!("Add ellipse"),
                    ),
                    3 => (
                        Annotation::new(
                            Kind::Line {
                                start: Point::new(x0, y0),
                                end: Point::new(x1, y1),
                                endings: Default::default(),
                                interior: None,
                            },
                            rect,
                        ),
                        tr!("Add line"),
                    ),
                    _ => (
                        Annotation::arrow(Point::new(x0, y0), Point::new(x1, y1), c, width),
                        tr!("Add arrow"),
                    ),
                };
                let mut a = a;
                a.color = Some(c);
                a.border.width = width;
                self.add_annotation(s, ui, page, a, &author, &label);
            }
            Gesture::TextBox { page, from, to } => self.open_editor(s, ui, page, from, to),
            Gesture::Note { page, at } => {
                if !self.may_annotate(s, ui) {
                    return;
                }
                let Some(t) = s.tab_ref() else { return };
                let g = t.session.geoms[page];
                let [x0, y0, x1, y1] = g.rect_to_pdf([at.0, at.1, 22.0, 22.0]);
                let mut a = Annotation::new(
                    Kind::Text {
                        icon: "Comment".into(),
                        open: false,
                    },
                    Rect::new(x0, y0, x1, y1),
                );
                // Notes stay light so their icon shows.
                a.color = Some(colour(if colour_i >= 5 { 0 } else { colour_i.max(0) }));
                if let Some(id) = self.add_annotation(s, ui, page, a, &author, &tr!("Add note"))
                    && let Some(t) = s.tab()
                {
                    t.selected = Some(Sel { page, id });
                    self.selection_changed(s, ui);
                    ui.global::<AppState>().set_tool(0);
                    self.tool_changed(s, ui);
                    self.message(s, &tr!("Write the comment in the panel on the right."));
                }
            }
            Gesture::Place { page, from, to } => self.place_signature(s, ui, page, from, to),
            Gesture::Area { page, from, to } if s.signing.awaiting_box => {
                self.sign_box_drawn(s, ui, page, from, to)
            }
            Gesture::Area { page, from, to } => self.redact_area(s, ui, page, from, to),
            Gesture::CopyArea { page, from, to } => {
                self.overlay_changed(s, ui);
                self.copy_area(s, page, from, to);
            }
            Gesture::Move {
                sel, now, moved, ..
            } => {
                if moved {
                    self.update_annotation(s, ui, sel, now, &tr!("Move"));
                } else {
                    self.overlay_changed(s, ui);
                }
            }
            Gesture::Resize { sel, now, .. } => {
                self.update_annotation(s, ui, sel, now, &tr!("Resize"))
            }
            Gesture::Link { page, action, at } => {
                if (at.0 - p.x).abs() < 4.0 && (at.1 - p.y).abs() < 4.0 {
                    self.follow_link(s, ui, page, action);
                }
            }
        }
    }

    /// Adds `a` to page `page` as one undoable step named `label`; returns its reference.
    pub fn add_annotation(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        page: usize,
        mut a: Annotation,
        author: &str,
        label: &str,
    ) -> Option<pagebara_syntax::ObjRef> {
        if !author.is_empty() {
            a.author = Some(author.to_owned());
        }
        let r = s.tab_ref()?.session.page_ref(page)?;
        let w = self.edit(s, ui, label, |d| {
            Ok(pagebara_annot::add_with(
                d,
                r,
                &a,
                &pagebara_annot::Helvetica,
            )?)
        })?;
        if !w.missing.is_empty() {
            let chars: String = w.missing.iter().take(8).collect();
            let warn = tr!(
                "The built-in font cannot show “{}”: those characters are drawn as boxes in other apps, the text itself is kept.",
                chars
            );
            ui.global::<AppState>()
                .set_text_warning(warn.as_str().into());
            self.message(s, &warn);
        }
        Some(w.id)
    }

    pub fn update_annotation(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        sel: Sel,
        a: Annotation,
        label: &str,
    ) {
        if !self.may_annotate(s, ui) {
            return self.overlay_changed(s, ui);
        }
        self.edit(s, ui, label, |d| {
            Ok(pagebara_annot::update_with(
                d,
                sel.id,
                &a,
                &pagebara_annot::Helvetica,
            )?)
        });
        if let Some(t) = s.tab() {
            t.selected = Some(sel);
        }
        self.selection_changed(s, ui);
    }

    /// Turns the text selection into a highlight, underline or strike-out: `kind` (0, 1, 2:
    /// the context menu's), else the Highlight tool's kind and colour.
    pub fn markup_selection(self: &Rc<Self>, s: &mut State, ui: &AppWindow, kind: Option<i32>) {
        let st = ui.global::<AppState>();
        if kind.is_none() && st.get_tool() == 6 {
            return self.redact_selection(s, ui);
        }
        let kind = match kind.unwrap_or_else(|| st.get_markup_kind()) {
            1 => MarkupKind::Underline,
            2 => MarkupKind::StrikeOut,
            _ => MarkupKind::Highlight,
        };
        let author = st.get_author().to_string();
        // From the context menu in another tool: the Highlight tool's colour.
        let colour_i = if st.get_tool() == 1 {
            st.get_colour()
        } else {
            s.tool_colours[1]
        };
        let Some(t) = s.tab() else { return };
        let Some(sel) = t.text_sel.take() else { return };
        let text = t.session.sel_text(sel.page);
        let range = sel.range();
        let quads: Vec<Quad> = text_index::line_boxes(&text, range.clone())
            .into_iter()
            .map(|b| {
                Quad::from_rect(Rect::new(
                    f64::from(b[0]),
                    f64::from(b[1]),
                    f64::from(b[2]),
                    f64::from(b[3]),
                ))
            })
            .collect();
        if quads.is_empty() || !self.may_annotate(s, ui) {
            return self.overlay_changed(s, ui);
        }
        let words = text_index::text_of(&text, range);
        let mut a = Annotation::new(Kind::Markup { kind, quads }, Rect::default());
        a.color = Some(colour(colour_i));
        a.subject = Some(words.chars().take(200).collect());
        let label = match kind {
            MarkupKind::Highlight => tr!("Add highlight"),
            MarkupKind::Underline => tr!("Add underline"),
            _ => tr!("Add strikethrough"),
        };
        self.add_annotation(s, ui, sel.page, a, &author, &label);
    }

    fn open_editor(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        page: usize,
        from: (f64, f64),
        to: (f64, f64),
    ) {
        if !self.may_annotate(s, ui) {
            return;
        }
        let st = ui.global::<AppState>();
        let size = f64::from(st.get_font_size());
        let (mut x, mut y) = (from.0.min(to.0), from.1.min(to.1));
        let (mut w, mut h) = ((to.0 - from.0).abs(), (to.1 - from.1).abs());
        if w < 12.0 || h < 8.0 {
            w = (size * 16.0).max(120.0);
            h = size * 1.6 + 6.0;
            x = from.0;
            y = from.1 - h / 2.0;
        }
        let Some(t) = s.tab_ref() else { return };
        let Some(pl) = t.view.placement(page) else {
            return;
        };
        let [r, g, b] = COLOURS[(st.get_colour().max(0) as usize).min(6)];
        let text_colour = slint::Color::from_rgb_u8(r, g, b);
        s.input.editor = Some((page, [x, y, w, h]));
        let vs = ui.global::<ViewState>();
        vs.set_editor(OverlayItem {
            di: pl.di,
            by: pl.by + y as f32,
            bx: pl.bx + x as f32,
            w: w as f32,
            h: h as f32,
            font_size: size as f32,
            stroke: text_colour,
            ..Default::default()
        });
        vs.set_editor_text("".into());
        vs.set_editing(true);
        self.overlay_changed(s, ui);
    }

    /// Finishes the free text editor, adding the text box when there is text.
    pub fn editor_done(self: &Rc<Self>, s: &mut State, ui: &AppWindow, commit: bool) {
        let vs = ui.global::<ViewState>();
        if !vs.get_editing() {
            return;
        }
        vs.set_editing(false);
        let text = vs.get_editor_text().to_string();
        let ed = vs.get_editor();
        let Some((page, [x, y, w, h])) = s.input.editor.take() else {
            return;
        };
        ui.invoke_focus_view();
        if !commit || text.trim().is_empty() {
            return self.overlay_changed(s, ui);
        }
        let Some(t) = s.tab_ref() else { return };
        let g = t.session.geoms[page];
        let lines = text.lines().count().max(1) as f64;
        let size = f64::from(ed.font_size);
        let h = h.max(lines * size * 1.25 + 6.0);
        let r = g.rect_to_pdf([x, y, w, h]);
        let c = ed.stroke;
        let text_color = Color::Rgb(
            f32::from(c.red()) / 255.0,
            f32::from(c.green()) / 255.0,
            f32::from(c.blue()) / 255.0,
        );
        let mut a = Annotation::new(
            Kind::FreeText {
                font_size: size,
                text_color,
                align: pagebara_annot::Align::Left,
            },
            pdf_rect(r),
        );
        a.contents = Some(text);
        let author = ui.global::<AppState>().get_author().to_string();
        ui.global::<AppState>().set_text_warning("".into());
        self.add_annotation(s, ui, page, a, &author, &tr!("Add text box"));
    }

    /// Goes where a link points: pages of this document, or (after asking) a web page.
    pub fn follow_link(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        _page: usize,
        action: LinkAction,
    ) {
        let Some(t) = s.tab_ref() else { return };
        let doc = t.session.doc();
        let dest = match &action {
            LinkAction::GoTo(pagebara_annot::Dest::Explicit(d)) => Some(*d),
            LinkAction::GoTo(pagebara_annot::Dest::Named(n)) => {
                pagebara_annot::named_destination(doc, n)
            }
            LinkAction::Uri(u) => {
                let u = u.clone();
                return self.ask_open_link(s, ui, &u);
            }
            LinkAction::GoToR { file, .. } | LinkAction::Launch { file, .. } => {
                let f = file.clone();
                return self.message(
                    s,
                    &tr!(
                        "This link opens another file ({}); Pagebara does not follow it.",
                        f
                    ),
                );
            }
            LinkAction::Other(kind) => {
                let k = kind.clone();
                return self.message(
                    s,
                    &tr!(
                        "This link runs a “{}” action, which Pagebara does not do.",
                        k
                    ),
                );
            }
        };
        let Some(dest) = dest else {
            return self.message(s, &tr!("This link leads nowhere in this document."));
        };
        let page = match dest.page {
            pagebara_annot::DestPage::Ref(r) => t.session.tree.index_of(r),
            pagebara_annot::DestPage::Index(i) => Some(i as usize),
        };
        let Some(page) = page.filter(|&p| p < t.session.page_count()) else {
            return self.message(s, &tr!("This link leads to a page that does not exist."));
        };
        let g = t.session.geoms[page];
        let top = match dest.view {
            pagebara_annot::View::Xyz {
                top: Some(top),
                left,
                ..
            } => Some(g.to_view(left.unwrap_or(g.crop[0]), top).1),
            pagebara_annot::View::FitH { top: Some(top) }
            | pagebara_annot::View::FitBH { top: Some(top) } => Some(g.to_view(g.crop[0], top).1),
            pagebara_annot::View::FitR(r) => Some(g.to_view(r.x0, r.y1).1),
            _ => None,
        };
        self.jump(s, ui, page, top.map(|v| v.max(0.0)));
    }

    /// Copies the selected text.
    pub fn copy_selection(self: &Rc<Self>, s: &mut State) {
        let Some(t) = s.tab() else { return };
        let Some(sel) = t.text_sel else { return };
        if !t.session.doc().permissions().copy {
            return self.message(
                s,
                &tr!("This document's security settings do not allow copying text."),
            );
        }
        let text = pagebara_content::copy_text(&t.session.sel_text(sel.page), sel.range());
        if text.is_empty() {
            return;
        }
        let n = text.chars().count();
        match crate::clipboard::set_text(&text) {
            Ok(()) => self.message(
                s,
                &trn!("Copied {n} character.", "Copied {n} characters.", n),
            ),
            Err(e) => self.message(s, &tr!("Could not copy: {}", e)),
        }
        s.input.copied = Some(text);
    }

    /// Selects all text on the current page, or of the picture last clicked when its text
    /// was recognised.
    pub fn select_all_text(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let last = s.input.last_press;
        let Some(t) = s.tab() else { return };
        let page = t.view.current_page;
        let picture = last.and_then(|(p, pt)| {
            let rect = t
                .session
                .live_ref()?
                .picture_at(p, pt.x as f32, pt.y as f32)?;
            Some((p, rect))
        });
        if let Some((p, rect)) = picture {
            let text = t.session.sel_text(p);
            let inside: Vec<usize> = text
                .chars
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    let b = c.bbox();
                    let (x, y) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
                    rect.x0 <= x && x <= rect.x1 && rect.y0 <= y && y <= rect.y1
                })
                .map(|(i, _)| i)
                .collect();
            if let (Some(&a), Some(&b)) = (inside.first(), inside.last()) {
                t.selected = None;
                t.text_sel = Some(TextSel {
                    page: p,
                    anchor: a,
                    caret: b + 1,
                });
                return self.selection_changed(s, ui);
            }
        }
        let text = t.session.sel_text(page);
        if text.chars.is_empty() {
            return self.message(s, &tr!("There is no text on this page."));
        }
        t.selected = None;
        t.text_sel = Some(TextSel {
            page,
            anchor: 0,
            caret: text.chars.len(),
        });
        self.selection_changed(s, ui);
    }
}

pub fn is_signature(a: &Annotation) -> bool {
    matches!(a.kind, Kind::Stamp { .. }) && a.subject.as_deref() == Some("Signature")
}

/// Pointer bookkeeping kept between events.
#[derive(Debug, Default)]
pub struct InputState {
    pub clicks: Clicks,
    pub pan_from: (f64, f64),
    pub editor: Option<(usize, [f64; 4])>,
    pub copied: Option<String>,
    /// The pointer's last position over the pages without a button down.
    pub hover: Option<Pointer>,
    /// Where the context menu was opened: page and user-space point.
    pub menu_at: Option<(usize, Point)>,
    /// The next drag copies the text of an area (from the context menu).
    pub area_copy: bool,
    /// Where the last press was: page and user-space point.
    pub last_press: Option<(usize, Point)>,
}

impl InputState {
    pub fn press(&mut self, x: f64, y: f64) -> u8 {
        self.clicks.press(x, y)
    }
}
