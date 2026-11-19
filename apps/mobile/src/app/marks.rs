//! What is drawn over the pages: search hits, the text selection, the stroke being drawn
//! (and just-drawn strokes until the page shows them), the selected signature's frame,
//! with the Forms tool the fields to fill in, with Edit the selected page objects, and with
//! Prepare form the fields and the selected one. Annotations themselves are in the tiles.

use pagebara_annot::Kind;
use pagebara_app_core::pages::PageGeom;
use pagebara_app_core::text_index;
use pagebara_app_core::view::Placement;
use slint::{Color, ComponentHandle, ModelRc, VecModel};

use super::State;
use crate::{AppState, Mark, MobileWindow, Tool, ViewState};

const SELECTION: Color = Color::from_argb_u8(0x40, 0x25, 0x63, 0xEB);
const HIT: Color = Color::from_argb_u8(0x55, 0xF5, 0x9E, 0x0B);
const HIT_CURRENT: Color = Color::from_argb_u8(0x99, 0xF9, 0x73, 0x16);

/// A drawn stroke in displayed page points.
#[derive(Clone, Debug)]
pub struct Stroke {
    pub page: usize,
    pub points: Vec<(f64, f64)>,
    pub color: Color,
    pub width: f64,
}

/// A mark at `[x, y, w, h]` (displayed points) of the page placed at `pl`.
pub(super) fn at(pl: &Placement, [x, y, w, h]: [f64; 4], kind: i32) -> Mark {
    Mark {
        di: pl.di,
        by: pl.by + y as f32,
        bx: pl.bx + x as f32,
        w: w as f32,
        h: h as f32,
        kind,
        ..Mark::default()
    }
}

pub(super) fn fill(pl: &Placement, r: [f64; 4], c: Color) -> Mark {
    Mark {
        fill: c,
        ..at(pl, r, 0)
    }
}

/// A PDF bounding box `[x0, y0, x1, y1]` as `[x, y, w, h]` in displayed points.
pub fn view_box(g: &PageGeom, b: [f64; 4]) -> [f64; 4] {
    g.rect_to_view(b)
}

fn stroke_mark(pl: &Placement, s: &Stroke) -> Option<Mark> {
    polyline(pl, &s.points, s.width, s.color)
}

/// A line through view points (page placement `pl`), `width` points wide.
pub(super) fn polyline(
    pl: &Placement,
    points: &[(f64, f64)],
    width: f64,
    color: Color,
) -> Option<Mark> {
    let first = points.first()?;
    let pad = width;
    let (mut x0, mut y0, mut x1, mut y1) = (first.0, first.1, first.0, first.1);
    for &(x, y) in points {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let (x0, y0) = (x0 - pad, y0 - pad);
    let (w, h) = (x1 + pad - x0, y1 + pad - y0);
    let mut cmd = String::new();
    for (i, &(x, y)) in points.iter().enumerate() {
        let op = if i == 0 { 'M' } else { 'L' };
        cmd.push_str(&format!("{op}{:.2} {:.2} ", x - x0, y - y0));
    }
    if points.len() == 1 {
        cmd.push_str(&format!("L{:.2} {:.2}", first.0 - x0 + 0.1, first.1 - y0));
    }
    Some(Mark {
        stroke: color,
        width: width as f32,
        commands: cmd.into(),
        ..at(pl, [x0, y0, w, h], 2)
    })
}

pub fn rebuild(s: &mut State, ui: &MobileWindow) {
    let tool = ui.global::<AppState>().get_tool();
    let forms = s.forms.clone();
    let live = s.input.live_stroke();
    let moving = s.input.moving_box();
    let prep = s.input.prep_drag().cloned();
    let backend = s.backend.clone();
    let theme = s.settings.page_theme;
    let Some(d) = &mut s.doc else { return };
    d.marks_dirty = false;
    d.marks_pages = d.view.last.pages.clone();
    d.search.want_boxes(d.marks_pages.iter().map(|(i, _)| *i));
    let mut out = Vec::new();
    let mut selection_at = None;
    let mut selected_at = None;
    let fields = if tool == Tool::Forms {
        d.form(&*forms).fields()
    } else {
        Vec::new()
    };
    for (page, pl) in d.marks_pages.clone() {
        let Some(g) = d.session.geoms.get(page).copied() else {
            continue;
        };
        for w in fields
            .iter()
            .filter(|f| !f.read_only)
            .flat_map(|f| &f.widgets)
        {
            if w.page == page {
                out.push(at(&pl, view_box(&g, w.rect), 3));
            }
        }
        for (current, boxes) in d.search.boxes_on(page) {
            let c = if current { HIT_CURRENT } else { HIT };
            for b in boxes {
                let r = view_box(&g, b.map(f64::from));
                out.push(fill(&pl, r, c));
            }
        }
        if let Some(sel) = d.text_sel.filter(|t| t.page == page && t.anchor != t.caret) {
            let text = d.session.sel_text(page);
            let boxes: Vec<[f64; 4]> = text_index::line_boxes(&text, sel.range())
                .into_iter()
                .map(|b| view_box(&g, b.map(f64::from)))
                .collect();
            for r in &boxes {
                if selection_at.is_none() {
                    selection_at = Some((page, r[0] + r[2] / 2.0, r[1]));
                }
                out.push(fill(&pl, *r, SELECTION));
            }
            // Handles to drag the ends.
            if let (Some(a), Some(b)) = (boxes.first(), boxes.last()) {
                out.push(at(&pl, [a[0], a[1], 0.01, a[3]], 4));
                out.push(at(&pl, [b[0] + b[2], b[1], 0.01, b[3]], 4));
            }
        }
        for stroke in d.pending_ink.iter().chain(&live).filter(|s| s.page == page) {
            out.extend(stroke_mark(&pl, stroke));
        }
        if tool == Tool::Edit {
            super::edit::marks(d, page, &pl, &mut out, &*backend, theme);
        }
        if tool == Tool::Prepare {
            super::prepare::marks(d, &*forms, page, &pl, prep.as_ref(), &mut out);
        }
        if let Some(sel) = d.selected.filter(|s| s.page == page) {
            let r = match moving {
                Some((p, r)) if p == sel => Some(r),
                _ => d
                    .annot(sel)
                    .map(|a| view_box(&g, [a.rect.x0, a.rect.y0, a.rect.x1, a.rect.y1])),
            };
            if let Some(r) = r {
                selected_at = Some((page, r[0] + r[2] / 2.0, r[1]));
                out.push(at(&pl, r, 1));
            }
        }
    }
    d.chips = Chips {
        selection: selection_at.filter(|_| !s.input.selecting()),
        selected: selected_at.filter(|_| moving.is_none()),
    };
    super::edit::show(d, ui);
    ui.global::<ViewState>()
        .set_marks(ModelRc::new(VecModel::from(out)));
}

/// Where the actions for the text selection and the selected signature go: page and
/// displayed point (the top centre of what they act on).
#[derive(Clone, Copy, Debug, Default)]
pub struct Chips {
    selection: Option<(usize, f64, f64)>,
    selected: Option<(usize, f64, f64)>,
}

/// Keeps the action chips over what they act on as the pages move.
pub fn place_chips(d: &super::Doc, ui: &MobileWindow) {
    let st = ui.global::<AppState>();
    let screen =
        |at: Option<(usize, f64, f64)>| at.and_then(|(p, u, v)| d.view.page_to_screen(p, u, v));
    let sel = screen(d.chips.selection);
    st.set_has_selection(sel.is_some());
    if let Some((x, y)) = sel {
        st.set_selection_x(x as f32);
        st.set_selection_y(y as f32);
    }
    let annot = screen(d.chips.selected);
    st.set_has_selected_annot(annot.is_some());
    if let Some((x, y)) = annot {
        st.set_selected_x(x as f32);
        st.set_selected_y(y as f32);
    }
}

/// Whether `a` is a signature placed with the app (an image stamp).
pub fn is_signature(a: &pagebara_annot::Annotation) -> bool {
    matches!(a.kind, Kind::Stamp { .. })
}
