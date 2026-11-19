//! What is drawn over the visible pages: annotations (the page renderer draws page content
//! only), search hits, the text selection, the selection frame and gestures in progress.

use pagebara_annot::{Annotation, Color, Kind, LineEnding, MarkupKind, Rect};
use pagebara_app_core::annots;
use pagebara_app_core::pages::PageGeom;
use pagebara_app_core::text_index;
use pagebara_app_core::view::Placement;
use pagebara_i18n::tr;
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use super::input::{COLOURS, Gesture, colour};
use super::{State, Tab};
use crate::{AppState, AppWindow, OverlayItem, ViewState};

/// The sentence being read aloud.
const READING: slint::Color = slint::Color::from_argb_u8(0x55, 0x22, 0xC5, 0x5E);
const SELECTION: slint::Color = slint::Color::from_argb_u8(0x55, 0x25, 0x63, 0xEB);
const HIT: slint::Color = slint::Color::from_argb_u8(0x66, 0xF5, 0x9E, 0x0B);
const HIT_CURRENT: slint::Color = slint::Color::from_argb_u8(0x99, 0xF9, 0x73, 0x16);
const ACCENT: slint::Color = slint::Color::from_rgb_u8(0x25, 0x63, 0xEB);
const FIELD: slint::Color = slint::Color::from_argb_u8(0x30, 0x60, 0xA5, 0xFA);

fn slint_color(c: Option<Color>, alpha: f64) -> slint::Color {
    match c {
        Some(c) => {
            let [r, g, b] = c
                .to_rgb()
                .map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
            slint::Color::from_argb_u8((alpha.clamp(0.0, 1.0) * 255.0) as u8, r, g, b)
        }
        None => slint::Color::from_argb_u8(0, 0, 0, 0),
    }
}

/// An item covering `[x, y, w, h]` (displayed points) of the page placed at `pl`.
pub(super) fn at(pl: &Placement, [x, y, w, h]: [f64; 4]) -> OverlayItem {
    OverlayItem {
        di: pl.di,
        by: pl.by + y as f32,
        bx: pl.bx + x as f32,
        w: w.max(0.01) as f32,
        h: h.max(0.01) as f32,
        ..Default::default()
    }
}

fn view_rect(g: &PageGeom, r: &Rect) -> [f64; 4] {
    g.rect_to_view([r.x0, r.y0, r.x1, r.y1])
}

pub(super) fn fill(pl: &Placement, r: [f64; 4], c: slint::Color) -> OverlayItem {
    OverlayItem {
        kind: 0,
        fill: c,
        ..at(pl, r)
    }
}

/// A path through points (displayed points) with stroke `width`.
/// A red wavy line under a word whose line box runs from `a` to `b` along its bottom, with
/// `top` above `a` (view points); `k` is the view's pixels per point.
pub(super) fn squiggle(
    pl: &Placement,
    a: (f32, f32),
    b: (f32, f32),
    top: (f32, f32),
    k: f64,
) -> Option<OverlayItem> {
    let f = |p: (f32, f32)| (f64::from(p.0), f64::from(p.1));
    let pts = pagebara_app_core::spell::squiggle(f(a), f(b), f(top), 1.0 / k);
    path(
        pl,
        &pts,
        1.0 / k,
        SPELLING_RED,
        slint::Color::default(),
        false,
    )
}

const SPELLING_RED: slint::Color = slint::Color::from_rgb_u8(0xE0, 0x20, 0x1B);

pub(super) fn path(
    pl: &Placement,
    pts: &[(f64, f64)],
    width: f64,
    stroke: slint::Color,
    fill_c: slint::Color,
    closed: bool,
) -> Option<OverlayItem> {
    let pad = width.max(0.5);
    let x0 = pts.iter().map(|p| p.0).fold(f64::INFINITY, f64::min) - pad;
    let y0 = pts.iter().map(|p| p.1).fold(f64::INFINITY, f64::min) - pad;
    let x1 = pts.iter().map(|p| p.0).fold(f64::NEG_INFINITY, f64::max) + pad;
    let y1 = pts.iter().map(|p| p.1).fold(f64::NEG_INFINITY, f64::max) + pad;
    if !x0.is_finite() {
        return None;
    }
    let mut cmd = String::new();
    for (i, (x, y)) in pts.iter().enumerate() {
        let op = if i == 0 { 'M' } else { 'L' };
        cmd.push_str(&format!("{op}{:.2} {:.2} ", x - x0, y - y0));
    }
    if pts.len() == 1 {
        cmd.push_str(&format!(
            "L{:.2} {:.2} ",
            pts[0].0 - x0 + 0.05,
            pts[0].1 - y0
        ));
    }
    if closed {
        cmd.push('Z');
    }
    Some(OverlayItem {
        kind: 2,
        commands: cmd.into(),
        stroke,
        fill: fill_c,
        width: width as f32,
        ..at(pl, [x0, y0, x1 - x0, y1 - y0])
    })
}

fn ellipse(
    pl: &Placement,
    r: [f64; 4],
    width: f64,
    stroke: slint::Color,
    fill_c: slint::Color,
) -> OverlayItem {
    let [x, y, w, h] = r;
    let (rx, ry) = (w / 2.0, h / 2.0);
    let cmd = format!(
        "M0 {ry:.2} A{rx:.2} {ry:.2} 0 1 0 {w:.2} {ry:.2} A{rx:.2} {ry:.2} 0 1 0 0 {ry:.2} Z"
    );
    OverlayItem {
        kind: 2,
        commands: cmd.into(),
        stroke,
        fill: fill_c,
        width: width as f32,
        ..at(pl, [x, y, w, h])
    }
}

/// An arrow head at `end` coming from `start`.
fn arrow_head(
    pl: &Placement,
    start: (f64, f64),
    end: (f64, f64),
    width: f64,
    c: slint::Color,
) -> Option<OverlayItem> {
    let (dx, dy) = (end.0 - start.0, end.1 - start.1);
    let len = dx.hypot(dy);
    if len < 0.01 {
        return None;
    }
    let (ux, uy) = (dx / len, dy / len);
    let size = (width * 4.0 + 4.0).min(len);
    let base = (end.0 - ux * size, end.1 - uy * size);
    let half = size * 0.45;
    path(
        pl,
        &[
            end,
            (base.0 - uy * half, base.1 + ux * half),
            (base.0 + uy * half, base.1 - ux * half),
        ],
        width.min(1.0),
        c,
        c,
        true,
    )
}

fn line_items(
    out: &mut Vec<OverlayItem>,
    pl: &Placement,
    g: &PageGeom,
    a: &Annotation,
    start: (f64, f64),
    end: (f64, f64),
    endings: &[LineEnding; 2],
) {
    let c = slint_color(a.color, a.opacity);
    let w = a.border.width;
    out.extend(path(
        pl,
        &[g.to_view(start.0, start.1), g.to_view(end.0, end.1)],
        w,
        c,
        slint::Color::default(),
        false,
    ));
    if endings[1] != LineEnding::None {
        out.extend(arrow_head(
            pl,
            g.to_view(start.0, start.1),
            g.to_view(end.0, end.1),
            w,
            c,
        ));
    }
    if endings[0] != LineEnding::None {
        out.extend(arrow_head(
            pl,
            g.to_view(end.0, end.1),
            g.to_view(start.0, start.1),
            w,
            c,
        ));
    }
}

fn image_of(tab: &mut Tab, a: &Annotation) -> Option<Image> {
    let id = a.id?;
    if let Some(i) = tab.images.get(&id) {
        return Some(i.clone());
    }
    let (w, h, px) = annots::stamp_image(tab.session.doc(), a)?;
    let buf = SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&px, w, h);
    let img = Image::from_rgba8(buf);
    tab.images.insert(id, img.clone());
    Some(img)
}

fn annot_items(
    out: &mut Vec<OverlayItem>,
    tab: &mut Tab,
    pl: &Placement,
    g: &PageGeom,
    a: &Annotation,
) {
    let stroke = slint_color(a.color, a.opacity);
    let r = view_rect(g, &a.rect);
    let w = a.border.width;
    match &a.kind {
        Kind::Markup { kind, quads } => {
            for q in quads {
                let Some(b) = Rect::bounding(q.0) else {
                    continue;
                };
                let [x, y, qw, qh] = view_rect(g, &b);
                let item = match kind {
                    MarkupKind::Highlight => {
                        fill(pl, [x, y, qw, qh], slint_color(a.color, 0.4 * a.opacity))
                    }
                    MarkupKind::StrikeOut => {
                        fill(pl, [x, y + qh * 0.52, qw, (qh * 0.07).max(0.8)], stroke)
                    }
                    _ => fill(pl, [x, y + qh * 0.92, qw, (qh * 0.07).max(0.8)], stroke),
                };
                out.push(item);
            }
        }
        Kind::Ink { strokes } => {
            for s in strokes {
                let pts: Vec<(f64, f64)> = s.iter().map(|p| g.to_view(p.x, p.y)).collect();
                out.extend(path(pl, &pts, w, stroke, slint::Color::default(), false));
            }
        }
        Kind::Square { interior } => {
            let inset = [r[0] + w / 2.0, r[1] + w / 2.0, r[2] - w, r[3] - w];
            out.push(OverlayItem {
                kind: 1,
                stroke,
                fill: slint_color(*interior, a.opacity),
                width: w as f32,
                ..at(pl, inset)
            });
        }
        Kind::Circle { interior } => {
            let inset = [
                r[0] + w / 2.0,
                r[1] + w / 2.0,
                (r[2] - w).max(0.1),
                (r[3] - w).max(0.1),
            ];
            out.push(ellipse(
                pl,
                inset,
                w,
                stroke,
                slint_color(*interior, a.opacity),
            ));
        }
        Kind::Line {
            start,
            end,
            endings,
            ..
        } => {
            line_items(out, pl, g, a, (start.x, start.y), (end.x, end.y), endings);
        }
        Kind::PolyLine { vertices, .. } | Kind::Polygon { vertices, .. } => {
            let pts: Vec<(f64, f64)> = vertices.iter().map(|p| g.to_view(p.x, p.y)).collect();
            let closed = matches!(a.kind, Kind::Polygon { .. });
            out.extend(path(pl, &pts, w, stroke, slint::Color::default(), closed));
        }
        Kind::FreeText {
            font_size,
            text_color,
            ..
        } => {
            out.push(OverlayItem {
                kind: 3,
                text: a.contents.clone().unwrap_or_default().into(),
                font_size: *font_size as f32,
                stroke: slint_color(Some(*text_color), a.opacity),
                fill: slint_color(a.color, a.opacity),
                width: w as f32,
                ..at(pl, r)
            });
        }
        Kind::Text { .. } => {
            out.push(OverlayItem {
                kind: 5,
                fill: slint_color(a.color.or(Some(Color::YELLOW)), 1.0),
                ..at(pl, [r[0], r[1], 22.0 / 1.0, 22.0])
            });
        }
        Kind::Stamp { icon } => match image_of(tab, a) {
            Some(image) => out.push(OverlayItem {
                kind: 4,
                image,
                ..at(pl, r)
            }),
            None => out.push(OverlayItem {
                kind: 3,
                // TRANSLATORS: the noun, shown on a stamp that has no picture
                text: icon.clone().unwrap_or_else(|| tr!("Stamp")).into(),
                font_size: (r[3] * 0.4).clamp(6.0, 24.0) as f32,
                stroke: slint::Color::from_rgb_u8(0xB9, 0x1C, 0x1C),
                width: 1.5,
                ..at(pl, r)
            }),
        },
        _ => {}
    }
}

/// Builds the overlay for the active tab.
pub fn rebuild(s: &mut State, ui: &AppWindow) {
    let tool = ui.global::<AppState>().get_tool();
    let shape = ui.global::<AppState>().get_shape();
    let st = ui.global::<AppState>();
    let pen = f64::from(st.get_pen_width());
    let pen_colour = slint_color(Some(colour(st.get_colour())), 1.0);
    let gesture = s.gesture.clone();
    let backend = s.backend.clone();
    let forms = s.forms.clone();
    let reading = &s.read_aloud;
    let Some(tab) = s.tabs.get_mut(s.active) else {
        return;
    };
    tab.overlay_dirty = false;
    tab.overlay_pages = tab.view.last.pages.clone();
    let pages = tab.overlay_pages.clone();
    let mut out = Vec::new();
    let preview = gesture.preview().map(|(sel, a)| (sel, a.clone()));
    tab.search.want_boxes(pages.iter().map(|(i, _)| *i));
    for (i, pl) in &pages {
        let (i, pl) = (*i, *pl);
        let Some(g) = tab.session.geoms.get(i).copied() else {
            continue;
        };
        let list = tab.annots(i);
        let doc = tab.session.doc().clone();
        for a in list.iter().filter(|a| annots::drawn_by_app(&doc, a)) {
            match &preview {
                Some((sel, now)) if sel.page == i && a.id == Some(sel.id) => {
                    annot_items(&mut out, tab, &pl, &g, now)
                }
                _ => annot_items(&mut out, tab, &pl, &g, a),
            }
        }
        super::redaction::overlay(&mut out, tab, &pl, &g, i);
        if tool == super::edit::TOOL {
            super::edit::overlay(&mut out, tab, &pl, i, &*backend);
        }
        if tool == super::prepare::TOOL {
            let fields = super::prepare::placed_in(tab, &*forms, i);
            super::prepare::overlay(&mut out, tab, &pl, i, &fields);
        }
        if tool == 5 {
            for a in list
                .iter()
                .filter(|a| matches!(a.kind, Kind::Widget { .. }))
            {
                out.push(OverlayItem {
                    kind: 1,
                    fill: FIELD,
                    stroke: ACCENT,
                    width: 0.75,
                    ..at(&pl, view_rect(&g, &a.rect))
                });
            }
        }
        // `:noh` hides the hits without ending the search.
        let hits = (!tab.vim.hits_hidden).then(|| tab.search.boxes_on(i));
        for (current, boxes) in hits.into_iter().flatten() {
            for b in boxes {
                let r = g.rect_to_view([
                    f64::from(b[0]),
                    f64::from(b[1]),
                    f64::from(b[2]),
                    f64::from(b[3]),
                ]);
                out.push(fill(&pl, r, if current { HIT_CURRENT } else { HIT }));
            }
        }
        for b in reading.highlight(tab.uid, i) {
            let r = g.rect_to_view(b.map(f64::from));
            out.push(fill(
                &pl,
                [r[0] - 1.0, r[1] - 1.0, r[2] + 2.0, r[3] + 2.0],
                READING,
            ));
        }
        if let Some(sel) = tab.text_sel.filter(|t| t.page == i)
            && let Some(text) = tab.session.sel_text_cached(i)
        {
            for b in text_index::line_boxes(&text, sel.range()) {
                let r = g.rect_to_view([
                    f64::from(b[0]),
                    f64::from(b[1]),
                    f64::from(b[2]),
                    f64::from(b[3]),
                ]);
                out.push(fill(&pl, r, SELECTION));
            }
        }
        if let Some(sel) = tab.selected.filter(|sel| sel.page == i) {
            let a = match &preview {
                Some((p, now)) if *p == sel => Some(now.clone()),
                _ => list.iter().find(|a| a.id == Some(sel.id)).cloned(),
            };
            if let Some(a) = a {
                let [x, y, w, h] = view_rect(&g, &a.rect);
                let handles = !matches!(a.kind, Kind::Markup { .. } | Kind::Text { .. });
                let r = if matches!(a.kind, Kind::Text { .. }) {
                    [x - 2.0, y - 2.0, 26.0, 26.0]
                } else {
                    [x - 1.5, y - 1.5, w + 3.0, h + 3.0]
                };
                out.push(OverlayItem {
                    kind: if handles { 6 } else { 1 },
                    stroke: ACCENT,
                    width: 1.0,
                    ..at(&pl, r)
                });
            }
        }
        match &gesture {
            Gesture::Ink { page, points } if *page == i => {
                out.extend(path(
                    &pl,
                    points,
                    pen,
                    pen_colour,
                    slint::Color::default(),
                    false,
                ));
            }
            Gesture::Shape { page, from, to } if *page == i => {
                let r = [
                    from.0.min(to.0),
                    from.1.min(to.1),
                    (to.0 - from.0).abs(),
                    (to.1 - from.1).abs(),
                ];
                match shape {
                    1 => out.push(OverlayItem {
                        kind: 1,
                        stroke: pen_colour,
                        width: pen as f32,
                        ..at(&pl, r)
                    }),
                    2 => out.push(ellipse(&pl, r, pen, pen_colour, slint::Color::default())),
                    _ => {
                        out.extend(path(
                            &pl,
                            &[*from, *to],
                            pen,
                            pen_colour,
                            slint::Color::default(),
                            false,
                        ));
                        if shape == 4 {
                            out.extend(arrow_head(&pl, *from, *to, pen, pen_colour));
                        }
                    }
                }
            }
            Gesture::TextBox { page, from, to }
            | Gesture::Place { page, from, to }
            | Gesture::CopyArea { page, from, to }
            | Gesture::Area { page, from, to }
                if *page == i =>
            {
                let r = [
                    from.0.min(to.0),
                    from.1.min(to.1),
                    (to.0 - from.0).abs(),
                    (to.1 - from.1).abs(),
                ];
                if r[2] > 1.0 && r[3] > 1.0 {
                    out.push(OverlayItem {
                        kind: 7,
                        ..at(&pl, r)
                    });
                }
            }
            _ => {}
        }
    }
    let _ = COLOURS;
    super::forms::place_editor(tab, ui);
    ui.global::<ViewState>()
        .set_overlay(ModelRc::new(VecModel::from(out)));
}
