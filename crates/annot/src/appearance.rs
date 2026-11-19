//! Appearance streams (`/AP /N` form XObjects), so every reader draws our annotations the
//! same way. Forms use page coordinates (`/BBox` is the annotation's `/Rect`, identity
//! matrix), except free text and stamps on rotated pages: those are drawn in an upright frame
//! that `/Matrix` turns against the page's `/Rotate`, as Acrobat does.

use pagebara_syntax::{Dict, Document, Object, Stream};

use crate::content::Content;
use crate::font::{Glyph, Line, Shaper, TextFont};
use crate::geom::{Color, Point, Quad, Rect};
use crate::model::*;
use crate::smooth;

/// A form XObject's parts.
#[derive(Debug)]
pub(crate) struct Appearance {
    pub bbox: Rect,
    pub matrix: Option<[f64; 6]>,
    pub content: Vec<u8>,
    pub resources: Dict,
    /// Every character drawn in the text font; `None` when there is no text.
    pub glyphs: Option<Vec<(char, Glyph)>>,
    /// Characters the font could not show.
    pub missing: Vec<char>,
}

impl Appearance {
    /// Draws nothing; for replies, and links without a border.
    pub fn empty(bbox: Rect) -> Self {
        Appearance {
            bbox,
            matrix: None,
            content: Vec::new(),
            resources: Dict::new(),
            glyphs: None,
            missing: Vec::new(),
        }
    }

    pub fn into_stream(self, doc: &mut Document, font: &dyn TextFont) -> Stream {
        let mut res = self.resources;
        if let Some(used) = &self.glyphs {
            let mut f = Dict::new();
            for (name, font) in font.resources(doc, used) {
                f.set(name.as_bytes(), font);
            }
            res.set(b"Font", f);
        }
        let mut d = Dict::new();
        d.set(b"Type", Object::name(b"XObject"));
        d.set(b"Subtype", Object::name(b"Form"));
        d.set(b"BBox", self.bbox.to_object());
        if let Some(m) = self.matrix {
            d.set(b"Matrix", crate::geom::nums(m));
        }
        d.set(b"Resources", res);
        crate::deflate::stream(d, &self.content)
    }
}

/// Length of arrow heads and size of other line endings for a line `w` wide.
pub(crate) fn ending_size(w: f64) -> f64 {
    6.0 + 3.0 * w
}

/// `/Rotate` as 0, 90, 180 or 270.
pub(crate) fn normalize_rotation(r: i64) -> u16 {
    (r.rem_euclid(360) / 90 * 90) as u16
}

/// The upright frame for `rect` on a page rotated by `rot`: a rectangle at the origin as
/// the viewer sees it, and the matrix that turns it to the page (counter to `/Rotate`).
pub(crate) fn upright_frame(rect: Rect, rot: u16) -> (Rect, Option<[f64; 6]>) {
    let (w, h) = (rect.width(), rect.height());
    match rot {
        90 => (
            Rect::new(0.0, 0.0, h, w),
            Some([0.0, 1.0, -1.0, 0.0, 0.0, 0.0]),
        ),
        180 => (
            Rect::new(0.0, 0.0, w, h),
            Some([-1.0, 0.0, 0.0, -1.0, 0.0, 0.0]),
        ),
        270 => (
            Rect::new(0.0, 0.0, h, w),
            Some([0.0, -1.0, 1.0, 0.0, 0.0, 0.0]),
        ),
        _ => (rect, None),
    }
}

/// The normal appearance of `a`, or `None` for kinds drawn by the viewer (popups) or not
/// generated here (widgets, image stamps).
pub(crate) fn generate(a: &Annotation, font: &dyn TextFont) -> Option<Appearance> {
    let mut c = Content::new();
    let mut res = Resources::new(font);
    let w = a.border.width;
    let stroke = a.color;
    let opaque = a.opacity >= 1.0;
    let (frame, matrix) = match a.kind {
        Kind::FreeText { .. } | Kind::Stamp { .. } => {
            upright_frame(a.rect, normalize_rotation(a.rotate.unwrap_or(0).into()))
        }
        _ => (a.rect, None),
    };
    if !opaque && !matches!(a.kind, Kind::Markup { .. }) {
        let gs = res.gstate(a.opacity, false);
        c.gs(&gs);
    }
    match &a.kind {
        Kind::Markup { kind, quads } => markup(&mut c, &mut res, a, *kind, quads),
        Kind::Ink { strokes } => {
            let col = stroke?;
            c.stroke_color(col).line_width(w).round();
            for s in strokes {
                smooth::path(&mut c, s);
            }
            c.stroke();
        }
        Kind::Square { interior } | Kind::Circle { interior } => {
            let r = a.rect.inset(w / 2.0);
            border_setup(&mut c, a);
            if let Kind::Square { .. } = a.kind {
                c.rect(r);
            } else {
                c.ellipse(r);
            }
            paint(&mut c, stroke.filter(|_| w > 0.0), *interior);
        }
        Kind::Polygon { vertices, interior } => {
            border_setup(&mut c, a);
            c.polygon(vertices);
            paint(&mut c, stroke.filter(|_| w > 0.0), *interior);
        }
        Kind::Line {
            start,
            end,
            endings,
            interior,
        } => {
            border_setup(&mut c, a);
            c.move_to(*start).line_to(*end).stroke();
            line_endings(&mut c, &[*start, *end], endings, w, stroke, *interior);
        }
        Kind::PolyLine {
            vertices,
            endings,
            interior,
        } => {
            border_setup(&mut c, a);
            if let Some((&first, rest)) = vertices.split_first() {
                c.move_to(first);
                for &p in rest {
                    c.line_to(p);
                }
                c.stroke();
            }
            line_endings(&mut c, vertices, endings, w, stroke, *interior);
        }
        Kind::FreeText {
            font_size,
            text_color,
            align,
        } => free_text(&mut c, &mut res, a, frame, *font_size, *text_color, *align),
        Kind::Text { .. } => note_icon(&mut c, a.rect, stroke.unwrap_or(Color::YELLOW)),
        Kind::Stamp { icon: Some(icon) } => {
            text_stamp(&mut c, &mut res, frame, icon, stroke.unwrap_or(Color::RED))
        }
        Kind::Link { .. } => {
            if let Some(col) = stroke
                && w > 0.0
            {
                border_setup(&mut c, a);
                c.stroke_color(col).rect(a.rect.inset(w / 2.0)).stroke();
            }
        }
        _ => return None,
    }
    Some(res.finish(frame, matrix, c))
}

/// An appearance for a popup window: a title bar in the parent's colour with the author,
/// and the contents below it.
pub(crate) fn popup(
    rect: Rect,
    title: &str,
    contents: &str,
    color: Option<Color>,
    font: &dyn TextFont,
) -> Appearance {
    let mut c = Content::new();
    let mut res = Resources::new(font);
    let bar_color = color.unwrap_or(Color::YELLOW);
    let size = 10.0f64;
    let bar = (size * 1.6).min(rect.height());
    c.fill_color(Color::Rgb(1.0, 1.0, 0.94))
        .stroke_color(Color::Gray(0.45))
        .line_width(1.0)
        .rect(rect.inset(0.5))
        .fill_stroke();
    c.fill_color(bar_color)
        .rect(Rect::new(rect.x0, rect.y1 - bar, rect.x1, rect.y1).inset(0.5))
        .fill();
    let inner = rect.inset(4.0);
    let title_line = res.shaper.line(font, title);
    c.save()
        .rect(Rect::new(inner.x0, rect.y1 - bar, inner.x1, rect.y1))
        .clip()
        .fill_color(Color::BLACK);
    let baseline = rect.y1 - bar + (bar - size * font.ascent()) / 2.0;
    res.text(&mut c, size, Point::new(inner.x0, baseline), &title_line);
    c.restore();
    let body = Rect::new(inner.x0, inner.y0, inner.x1, rect.y1 - bar - 2.0);
    if body.height() > 0.0 {
        c.save().rect(body).clip().fill_color(Color::BLACK);
        let lines = res.shaper.wrap(font, contents, size, body.width());
        let mut y = body.y1 - size * font.ascent();
        for line in lines {
            if y + size * font.ascent() < body.y0 {
                break;
            }
            res.text(&mut c, size, Point::new(body.x0, y), &line);
            y -= size * 1.2;
        }
        c.restore();
    }
    res.finish(rect, None, c)
}

struct Resources<'f> {
    font: &'f dyn TextFont,
    gstates: Vec<(String, Dict)>,
    glyphs: Option<Vec<(char, Glyph)>>,
    shaper: Shaper,
}

impl<'f> Resources<'f> {
    fn new(font: &'f dyn TextFont) -> Self {
        Resources {
            font,
            gstates: Vec::new(),
            glyphs: None,
            shaper: Shaper::default(),
        }
    }

    fn gstate(&mut self, opacity: f64, multiply: bool) -> String {
        let mut d = Dict::new();
        d.set(b"Type", Object::name(b"ExtGState"));
        d.set(b"CA", crate::geom::num(opacity.clamp(0.0, 1.0)));
        d.set(b"ca", crate::geom::num(opacity.clamp(0.0, 1.0)));
        if multiply {
            d.set(b"BM", Object::name(b"Multiply"));
        }
        let name = format!("GS{}", self.gstates.len());
        self.gstates.push((name.clone(), d));
        name
    }

    /// Shows `line` at `at`, recording its glyphs for the font resource.
    fn text(&mut self, c: &mut Content, size: f64, at: Point, line: &Line) {
        let used = self.glyphs.get_or_insert_default();
        if line.is_empty() {
            return;
        }
        for g in &line.0 {
            if !used.contains(g) {
                used.push(*g);
            }
        }
        let runs: Vec<(String, Vec<u8>)> = line
            .runs()
            .into_iter()
            .map(|(face, b)| (self.font.face_name(face), b))
            .collect();
        c.text(size, at, &runs);
    }

    fn finish(self, bbox: Rect, matrix: Option<[f64; 6]>, c: Content) -> Appearance {
        let mut r = Dict::new();
        if !self.gstates.is_empty() {
            let mut g = Dict::new();
            for (n, d) in self.gstates {
                g.set(n.as_bytes(), d);
            }
            r.set(b"ExtGState", g);
        }
        Appearance {
            bbox,
            matrix,
            content: c.0,
            resources: r,
            glyphs: self.glyphs,
            missing: self.shaper.missing,
        }
    }
}

fn border_setup(c: &mut Content, a: &Annotation) {
    c.line_width(a.border.width);
    if let BorderStyle::Dashed(d) = &a.border.style
        && !d.is_empty()
    {
        c.dash(d);
    }
    if let Some(col) = a.color {
        c.stroke_color(col);
    }
}

fn paint(c: &mut Content, stroke: Option<Color>, fill: Option<Color>) {
    if let Some(f) = fill {
        c.fill_color(f);
    }
    paint_op(c, stroke.is_some(), fill.is_some());
}

fn paint_op(c: &mut Content, stroke: bool, fill: bool) {
    match (stroke, fill) {
        (true, true) => c.fill_stroke(),
        (true, false) => c.stroke(),
        (false, true) => c.fill(),
        (false, false) => c.end_path(),
    };
}

fn markup(c: &mut Content, res: &mut Resources, a: &Annotation, kind: MarkupKind, quads: &[Quad]) {
    let Some(col) = a.color else { return };
    let highlight = kind == MarkupKind::Highlight;
    if highlight || a.opacity < 1.0 {
        let gs = res.gstate(a.opacity, highlight);
        c.gs(&gs);
    }
    let lerp =
        |p: Point, q: Point, t: f64| Point::new(p.x + (q.x - p.x) * t, p.y + (q.y - p.y) * t);
    for q in quads {
        let (ul, ur, ll, lr) = (
            q.upper_left(),
            q.upper_right(),
            q.lower_left(),
            q.lower_right(),
        );
        let h = q.height();
        match kind {
            MarkupKind::Highlight => {
                c.fill_color(col).polygon(&[ul, ur, lr, ll]).fill();
            }
            MarkupKind::Underline | MarkupKind::StrikeOut => {
                let t = if kind == MarkupKind::Underline {
                    0.07
                } else {
                    0.45
                };
                let lw = (h / 14.0).max(0.5);
                c.stroke_color(col)
                    .line_width(lw)
                    .move_to(lerp(ll, ul, t))
                    .line_to(lerp(lr, ur, t))
                    .stroke();
            }
            MarkupKind::Squiggly => {
                let (a0, a1) = (lerp(ll, ul, 0.05), lerp(lr, ur, 0.05));
                let len = a0.dist(a1);
                let period = (h / 3.0).max(2.0);
                let amp = h / 12.0;
                let n = (len / period * 2.0).ceil().max(1.0) as usize;
                let (ux, uy) = ((a1.x - a0.x) / len.max(1e-9), (a1.y - a0.y) / len.max(1e-9));
                let (nx, ny) = (-uy, ux);
                c.stroke_color(col).line_width((h / 18.0).max(0.5)).round();
                for i in 0..=n {
                    let t = (i as f64 / n as f64).min(1.0);
                    let s = if i % 2 == 0 { -amp } else { amp };
                    let p = lerp(a0, a1, t);
                    let p = Point::new(p.x + nx * s, p.y + ny * s);
                    if i == 0 {
                        c.move_to(p);
                    } else {
                        c.line_to(p);
                    }
                }
                c.stroke();
            }
        }
    }
}

/// Endings at the first and last of `pts`.
fn line_endings(
    c: &mut Content,
    pts: &[Point],
    endings: &[LineEnding; 2],
    w: f64,
    stroke: Option<Color>,
    fill: Option<Color>,
) {
    let (Some(&first), Some(&last)) = (pts.first(), pts.last()) else {
        return;
    };
    if pts.len() < 2 {
        return;
    }
    // Each tip, with the neighbouring point that gives the line's direction there.
    let tips = [
        (first, pts[1], endings[0]),
        (last, pts[pts.len() - 2], endings[1]),
    ];
    c.raw(b"[] 0 d\n");
    if let Some(f) = fill {
        c.fill_color(f);
    }
    for (tip, from, e) in tips {
        let len = tip.dist(from);
        if e == LineEnding::None || len == 0.0 {
            continue;
        }
        let d = ((tip.x - from.x) / len, (tip.y - from.y) / len);
        ending(
            c,
            tip,
            d,
            e,
            ending_size(w),
            stroke.is_some(),
            fill.is_some(),
        );
    }
}

/// One line ending at `tip`; `d` points out of the line.
fn ending(
    c: &mut Content,
    tip: Point,
    d: (f64, f64),
    e: LineEnding,
    size: f64,
    stroke: bool,
    fill: bool,
) {
    let n = (-d.1, d.0);
    // A point `along` the line from the tip and `across` it.
    let at = |along: f64, across: f64| {
        Point::new(
            tip.x + d.0 * along + n.0 * across,
            tip.y + d.1 * along + n.1 * across,
        )
    };
    let half = size / 2.0;
    let arrow_w = size * 0.5;
    let finish = |c: &mut Content, closed: bool| {
        if closed {
            paint_op(c, stroke, fill);
        } else {
            c.stroke();
        }
    };
    match e {
        LineEnding::None => {}
        LineEnding::OpenArrow | LineEnding::ClosedArrow => {
            c.move_to(at(-size, arrow_w))
                .line_to(tip)
                .line_to(at(-size, -arrow_w));
            let closed = e == LineEnding::ClosedArrow;
            if closed {
                c.close();
            }
            finish(c, closed);
        }
        LineEnding::ROpenArrow | LineEnding::RClosedArrow => {
            c.move_to(at(size, arrow_w))
                .line_to(tip)
                .line_to(at(size, -arrow_w));
            let closed = e == LineEnding::RClosedArrow;
            if closed {
                c.close();
            }
            finish(c, closed);
        }
        LineEnding::Square => {
            c.polygon(&[
                at(-half, -half),
                at(-half, half),
                at(half, half),
                at(half, -half),
            ]);
            finish(c, true);
        }
        LineEnding::Diamond => {
            c.polygon(&[at(-half, 0.0), at(0.0, half), at(half, 0.0), at(0.0, -half)]);
            finish(c, true);
        }
        LineEnding::Circle => {
            c.ellipse(Rect::new(
                tip.x - half,
                tip.y - half,
                tip.x + half,
                tip.y + half,
            ));
            finish(c, true);
        }
        LineEnding::Butt => {
            c.move_to(at(0.0, half)).line_to(at(0.0, -half));
            finish(c, false);
        }
        LineEnding::Slash => {
            let (s, k) = (half * 0.5, half * 0.866);
            c.move_to(at(-s, -k)).line_to(at(s, k));
            finish(c, false);
        }
    }
}

const PAD: f64 = 2.0;

fn free_text(
    c: &mut Content,
    res: &mut Resources,
    a: &Annotation,
    r: Rect,
    size: f64,
    color: Color,
    align: Align,
) {
    let w = a.border.width;
    if let Some(bg) = a.color {
        c.fill_color(bg).rect(r).fill();
    }
    if w > 0.0 {
        c.save();
        border_setup(c, a);
        c.stroke_color(color)
            .rect(r.inset(w / 2.0))
            .stroke()
            .restore();
    }
    let inner = r.inset(w + PAD);
    let font = res.font;
    c.save().rect(inner).clip().fill_color(color);
    let leading = size * 1.15;
    let mut y = inner.y1 - size * font.ascent();
    let text = a.contents.as_deref().unwrap_or("");
    for line in res.shaper.wrap(font, text, size, inner.width()) {
        if y + size * font.ascent() < inner.y0 {
            break;
        }
        let lw = line.width() * size;
        let x = match align {
            Align::Left => inner.x0,
            Align::Center => inner.x0 + (inner.width() - lw) / 2.0,
            Align::Right => inner.x1 - lw,
        };
        res.text(c, size, Point::new(x, y), &line);
        y -= leading;
    }
    c.restore();
}

/// A sheet of paper with a folded corner and ruled lines, filling `r`.
fn note_icon(c: &mut Content, r: Rect, color: Color) {
    let (w, h) = (r.width(), r.height());
    let p = |x: f64, y: f64| Point::new(r.x0 + x * w, r.y0 + y * h);
    let fold = 0.3;
    c.line_width((w.min(h) / 20.0).max(0.5))
        .fill_color(color)
        .stroke_color(Color::Gray(0.25))
        .raw(b"1 j\n")
        .polygon(&[
            p(0.05, 0.05),
            p(0.05, 0.95),
            p(0.95, 0.95),
            p(0.95, fold),
            p(0.95 - (fold - 0.05), 0.05),
        ])
        .fill_stroke()
        .move_to(p(0.95, fold))
        .line_to(p(0.95 - (fold - 0.05), fold))
        .line_to(p(0.95 - (fold - 0.05), 0.05))
        .stroke();
    for y in [0.75, 0.6, 0.45] {
        c.move_to(p(0.2, y)).line_to(p(0.8, y));
    }
    c.move_to(p(0.2, 0.3)).line_to(p(0.55, 0.3)).stroke();
}

fn text_stamp(c: &mut Content, res: &mut Resources, r: Rect, label: &str, color: Color) {
    let lw = (r.height() / 15.0).clamp(1.0, 4.0);
    c.stroke_color(color).line_width(lw).round();
    rounded_rect(c, r.inset(lw), (r.height() / 6.0).min(r.width() / 6.0));
    c.stroke();
    let font = res.font;
    let text = res.shaper.line(font, &label.to_uppercase());
    let tw = text.width().max(0.1);
    let size = (r.height() * 0.55).min((r.width() - 4.0 * lw) * 0.9 / tw);
    if size > 0.0 {
        let x = r.x0 + (r.width() - tw * size) / 2.0;
        let y = r.y0 + (r.height() - size * font.ascent()) / 2.0;
        c.fill_color(color);
        res.text(c, size, Point::new(x, y), &text);
    }
}

fn rounded_rect(c: &mut Content, r: Rect, rad: f64) {
    let k = rad * (1.0 - crate::content::KAPPA);
    let p = Point::new;
    c.move_to(p(r.x0 + rad, r.y0))
        .line_to(p(r.x1 - rad, r.y0))
        .curve_to(p(r.x1 - k, r.y0), p(r.x1, r.y0 + k), p(r.x1, r.y0 + rad))
        .line_to(p(r.x1, r.y1 - rad))
        .curve_to(p(r.x1, r.y1 - k), p(r.x1 - k, r.y1), p(r.x1 - rad, r.y1))
        .line_to(p(r.x0 + rad, r.y1))
        .curve_to(p(r.x0 + k, r.y1), p(r.x0, r.y1 - k), p(r.x0, r.y1 - rad))
        .line_to(p(r.x0, r.y0 + rad))
        .curve_to(p(r.x0, r.y0 + k), p(r.x0 + k, r.y0), p(r.x0 + rad, r.y0))
        .close();
}
