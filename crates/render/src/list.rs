//! Display lists prepared for repeated rendering: per-command bounds for culling and lazily
//! resolved colour spaces.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

use pagebara_content::display::{Cmd, FontRef, LineJoin, TextRun};
use pagebara_content::font::FontKind;
use pagebara_content::{DisplayList, Matrix, Rect};

use crate::color::Cs;

/// Bounds of commands that are not bounded by geometry (shadings fill the clip).
pub const UNBOUNDED: Rect = Rect {
    x0: f32::MIN,
    y0: f32::MIN,
    x1: f32::MAX,
    y1: f32::MAX,
};

#[derive(Debug)]
pub struct Prepared {
    pub list: DisplayList,
    /// Bounds in the list's space, per command; `UNBOUNDED` when unknown.
    pub bounds: Vec<Rect>,
    /// Stroke commands: half the widest pen extent in user space, applied after the CTM.
    pub spaces: Vec<OnceLock<Option<Arc<Cs>>>>,
    /// A process-unique id for keying caches.
    pub id: u64,
    pub heap: usize,
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

impl Prepared {
    pub fn new(list: DisplayList) -> Arc<Prepared> {
        let bounds = list.cmds.iter().map(|c| cmd_bounds(&list, c)).collect();
        let spaces = (0..list.color_spaces.len())
            .map(|_| OnceLock::new())
            .collect();
        let heap = list.heap_size() + list.cmds.len() * 16;
        Arc::new(Prepared {
            list,
            bounds,
            spaces,
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            heap,
        })
    }
}

fn transform(r: Rect, m: &Matrix) -> Rect {
    r.transform(m)
}

fn cmd_bounds(l: &DisplayList, c: &Cmd) -> Rect {
    match *c {
        Cmd::Fill { path, ctm, .. } | Cmd::Clip { path, ctm, .. } => l
            .path(path)
            .bounds()
            .map_or(Rect::default(), |b| transform(b, &l.matrix(ctm))),
        Cmd::Stroke {
            path, ctm, style, ..
        } => {
            let Some(b) = l.path(path).bounds() else {
                return Rect::default();
            };
            let s = l.strokes[style as usize];
            let mut hw = s.width * 0.5;
            if s.join == LineJoin::Miter {
                hw *= s.miter_limit.max(1.0);
            }
            // Square caps reach √2 further.
            hw *= std::f32::consts::SQRT_2;
            let b = Rect::new(b.x0 - hw, b.y0 - hw, b.x1 + hw, b.y1 + hw);
            transform(b, &l.matrix(ctm))
        }
        Cmd::Text { run } => run_bounds(l, &l.runs[run as usize]),
        Cmd::Image { ctm, .. } => transform(Rect::new(0.0, 0.0, 1.0, 1.0), &l.matrix(ctm)),
        Cmd::BeginGroup { group } => {
            let g = &l.groups[group as usize];
            if g.bbox.width() > 0.0 && g.bbox.height() > 0.0 {
                transform(g.bbox, &l.matrix(g.ctm))
            } else {
                UNBOUNDED
            }
        }
        _ => UNBOUNDED,
    }
}

/// Bounds of a text run from the font bounding box (generous when unknown).
pub fn run_bounds(l: &DisplayList, r: &TextRun) -> Rect {
    let Some(f) = l.fonts.get(r.font as usize) else {
        return Rect::default();
    };
    let gb = glyph_box(f);
    let mut out: Option<Rect> = None;
    let glyphs = l.run_glyphs(r);
    if glyphs.is_empty() {
        return Rect::default();
    }
    // The glyph box at the corners of the extent of the glyph origins.
    let (mut minx, mut maxx, mut miny, mut maxy) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for g in glyphs {
        minx = minx.min(g.x);
        maxx = maxx.max(g.x);
        miny = miny.min(g.y);
        maxy = maxy.max(g.y);
    }
    for (x, y) in [(minx, miny), (maxx, maxy), (minx, maxy), (maxx, miny)] {
        let g = pagebara_content::display::Glyph {
            code: 0,
            x,
            y,
            width: 0.0,
        };
        let b = gb.transform(&r.glyph_matrix(&g));
        out = Some(out.map_or(b, |o| o.union(&b)));
    }
    let mut b = out.unwrap_or_default();
    if r.mode.strokes() {
        let s = l.strokes.get(r.style as usize).map_or(1.0, |s| s.width);
        let m = l.matrix(r.ctm);
        let e = crate::raster::stroke::expansion(&m) * s * 2.0;
        b = Rect::new(b.x0 - e, b.y0 - e, b.x1 + e, b.y1 + e);
    }
    b
}

/// A glyph's box in text space units per unit font size.
pub fn glyph_box(f: &FontRef) -> Rect {
    let font = &f.font;
    let generous = Rect::new(-0.5, -0.5, 1.5, 1.5);
    if font.kind == FontKind::Type3 {
        let b = font.bbox;
        if b.width() > 0.0 && b.height() > 0.0 {
            let t = b.transform(&font.font_matrix);
            return t.union(&Rect::new(0.0, 0.0, 0.0, 0.0));
        }
        return Rect::new(-2.0, -2.0, 3.0, 3.0);
    }
    let b = font.bbox;
    if b.width() > 0.0 && b.height() > 0.0 && b.width() < 1e5 {
        let t = b.transform(&font.font_matrix);
        // Font boxes are often wrong; never assume less than a generous em box.
        generous.union(&Rect::new(t.x0 - 0.1, t.y0 - 0.1, t.x1 + 0.1, t.y1 + 0.1))
    } else {
        generous
    }
}

/// Whether the commands between a `BeginGroup` at `start` and its `EndGroup` use blend
/// modes other than Normal or soft masks (which make isolation matter).
pub fn group_blends(l: &DisplayList, start: usize) -> bool {
    let mut depth = 0;
    for c in &l.cmds[start + 1..] {
        let paint = match *c {
            Cmd::BeginGroup { group } => {
                depth += 1;
                Some(l.groups[group as usize].paint)
            }
            Cmd::EndGroup => {
                if depth == 0 {
                    return false;
                }
                depth -= 1;
                None
            }
            Cmd::Fill { paint, .. }
            | Cmd::Stroke { paint, .. }
            | Cmd::Image { paint, .. }
            | Cmd::Shading { paint, .. } => Some(paint),
            Cmd::Text { run } => Some(l.runs[run as usize].fill).filter(|&p| p != u32::MAX),
            _ => None,
        };
        if let Some(p) = paint
            && let Some(p) = l.paints.get(p as usize)
            && p.blend != pagebara_content::display::BlendMode::Normal
        {
            return true;
        }
    }
    false
}

/// The index of the `EndGroup` matching a `BeginGroup` at `start`.
pub fn group_end(l: &DisplayList, start: usize) -> usize {
    let mut depth = 0;
    for (i, c) in l.cmds.iter().enumerate().skip(start + 1) {
        match c {
            Cmd::BeginGroup { .. } => depth += 1,
            Cmd::EndGroup => {
                if depth == 0 {
                    return i;
                }
                depth -= 1;
            }
            _ => {}
        }
    }
    l.cmds.len()
}
