//! The permanent overlay drawn where content was removed: the fill and overlay text.

use pagebara_annot::{Helvetica, TextFont};
use pagebara_syntax::{Dict, Document, Object};

use crate::emit;
use crate::geom::{Matrix, Point, Rect, Region};
use crate::mark::{Align, Mark};
use crate::rewrite::Res;

/// Marked-content properties that tag the overlay, so verification can tell it apart from
/// page content.
pub const OVERLAY_TAG: &[u8] = b"PagebaraRedaction";

/// Content drawing `marks` on a page displayed rotated by `rotate` degrees (text reads
/// upright on screen). Adds a font to `res` when there is text.
pub(crate) fn draw(doc: &Document, marks: &[Mark], rotate: u16, res: &mut Res) -> Vec<u8> {
    let mut out = Vec::new();
    let mut font: Option<Vec<u8>> = None;
    for m in marks {
        let regions = m.regions();
        if regions.is_empty() || (m.fill.is_none() && m.overlay_text.is_none()) {
            continue;
        }
        out.extend_from_slice(b"/Artifact <</Type/Layout/");
        out.extend_from_slice(OVERLAY_TAG);
        out.extend_from_slice(b" true>> BDC\nq\n");
        if let Some(f) = m.fill {
            emit::nums(&mut out, &f);
            out.extend_from_slice(b"rg\n");
            for r in &regions {
                polygon(&mut out, r.points());
            }
            out.extend_from_slice(b"f\n");
        }
        if let Some(text) = m.overlay_text.as_deref().filter(|t| !t.trim().is_empty()) {
            let name = font
                .get_or_insert_with(|| res.add(doc, b"Font", helvetica()))
                .clone();
            // In the largest area, or every area when repeating.
            let mut targets: Vec<&Region> = regions.iter().collect();
            if !m.repeat {
                targets.sort_by(|a, b| b.area().total_cmp(&a.area()));
                targets.truncate(1);
            }
            for r in targets {
                text_in(&mut out, &name, m, text, r, rotate);
            }
        }
        out.extend_from_slice(b"Q\nEMC\n");
    }
    out
}

fn helvetica() -> Object {
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"Font"));
    d.set(b"Subtype", Object::name(b"Type1"));
    d.set(b"BaseFont", Object::name(b"Helvetica"));
    d.set(b"Encoding", Object::name(b"WinAnsiEncoding"));
    Object::Dict(d)
}

fn polygon(out: &mut Vec<u8>, pts: &[Point]) {
    for (k, p) in pts.iter().enumerate() {
        emit::nums(out, &[p.x, p.y]);
        out.extend_from_slice(if k == 0 { b"m\n" } else { b"l\n" });
    }
    out.extend_from_slice(b"h\n");
}

fn encode(text: &str) -> (Vec<u8>, f64) {
    let mut bytes = Vec::new();
    let mut width = 0.0;
    for c in text.chars().filter(|c| !c.is_control()) {
        let g = Helvetica.glyph(c).unwrap_or_else(|| Helvetica.fallback());
        bytes.push(g.code as u8);
        width += g.width;
    }
    (bytes, width)
}

/// Overlay text inside one region, clipped to it.
fn text_in(out: &mut Vec<u8>, font: &[u8], m: &Mark, text: &str, r: &Region, rotate: u16) {
    let angle = f64::from(rotate % 360).to_radians();
    let (sin, cos) = (angle.sin().round(), angle.cos().round());
    // Text space: horizontal, upright on the displayed page.
    let to_page = Matrix::new(cos, sin, -sin, cos, 0.0, 0.0);
    let Some(to_text) = to_page.invert() else {
        return;
    };
    let pts: Vec<Point> = r.points().iter().map(|&p| to_text.apply(p)).collect();
    let b = Rect::from_points(&pts);
    let (unit, w1) = encode(text);
    if w1 <= 0.0 || b.width() <= 0.0 || b.height() <= 0.0 {
        return;
    }
    let size = if m.font_size > 0.0 {
        m.font_size
    } else {
        (b.height() * 0.7)
            .min(12.0)
            .min(b.width() / w1 * 0.95)
            .max(1.0)
    };
    let gap = Helvetica.glyph(' ').map_or(0.278, |g| g.width) * 2.0;
    let line_h = size * 1.2;
    let mut lines: Vec<(Vec<u8>, f64, f64)> = Vec::new();
    if m.repeat {
        let per = ((b.width() + gap * size) / ((w1 + gap) * size))
            .floor()
            .max(1.0) as usize;
        let mut row = Vec::new();
        for k in 0..per {
            if k > 0 {
                row.extend_from_slice(b"  ");
            }
            row.extend_from_slice(&unit);
        }
        let width = (w1 * per as f64 + gap * (per as f64 - 1.0)) * size;
        let rows = (b.height() / line_h).floor().max(1.0) as usize;
        let top = b.y1 - (b.height() - rows as f64 * line_h) / 2.0;
        for k in 0..rows {
            lines.push((
                row.clone(),
                width,
                top - line_h * (k as f64 + 1.0) + size * 0.25,
            ));
        }
    } else {
        let y = b.y0 + (b.height() - size * 0.718) / 2.0;
        lines.push((unit, w1 * size, y));
    }
    out.extend_from_slice(b"q\n");
    polygon(out, r.points());
    out.extend_from_slice(b"W n\nBT\n/");
    out.extend_from_slice(font);
    out.push(b' ');
    emit::num(out, size);
    out.extend_from_slice(b" Tf\n");
    emit::nums(out, &m.text_color);
    out.extend_from_slice(b"rg\n");
    for (bytes, width, y) in lines {
        let x = match m.align {
            Align::Left => b.x0 + size * 0.2,
            Align::Center => b.x0 + (b.width() - width) / 2.0,
            Align::Right => b.x1 - width - size * 0.2,
        };
        let o = to_page.apply(Point::new(x, y));
        emit::nums(out, &[cos, sin, -sin, cos, o.x, o.y]);
        out.extend_from_slice(b"Tm ");
        emit::hex_string(out, &bytes);
        out.extend_from_slice(b" Tj\n");
    }
    out.extend_from_slice(b"ET\nQ\n");
}

/// The overlay's properties dictionary marker, for readers of the tagged content.
pub(crate) fn is_overlay(props: &Dict) -> bool {
    matches!(props.get(OVERLAY_TAG), Some(Object::Bool(true)))
}
