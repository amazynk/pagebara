//! What a page draws, as detection needs it: straight horizontal and vertical segments,
//! axis-aligned rectangles, runs of underscores, and the text.

use pagebara_annot::Rect;
use pagebara_content::display::Verb;
use pagebara_content::{Cmd, DisplayList, Interpreter, Point, TextPage};
use pagebara_syntax::{Dict, Document};

/// Shorter lines are not fields.
const MIN_LINE: f64 = 30.0;
/// More drawing than this is not a form page (a map, a chart).
const MAX_SHAPES: usize = 4000;

/// A place for a field.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Area {
    /// The field's rectangle above a line or underscores.
    Line(Rect),
    Box(Rect),
    Check(Rect),
    Comb(Rect, usize),
}

impl Area {
    pub fn rect(&self) -> Rect {
        match *self {
            Area::Line(r) | Area::Box(r) | Area::Check(r) | Area::Comb(r, _) => r,
        }
    }
}

/// A horizontal (`y`, `x0..x1`) or vertical (`x`, `y0..y1`) segment.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Seg {
    at: f64,
    from: f64,
    to: f64,
}

#[derive(Debug, Default)]
pub struct Geometry {
    pub text: TextPage,
    horizontal: Vec<Seg>,
    vertical: Vec<Seg>,
    rects: Vec<Rect>,
    /// Underscore runs: their boxes.
    underscores: Vec<Rect>,
    pub page: Rect,
}

fn resources(doc: &Document, page: &Dict) -> Dict {
    let mut cur = page.clone();
    for _ in 0..64 {
        if let Some(d) = doc.get_in(&cur, b"Resources").as_dict() {
            return d.clone();
        }
        let parent = doc.get_in(&cur, b"Parent");
        match parent.as_dict() {
            Some(p) => cur = p.clone(),
            None => break,
        }
    }
    Dict::new()
}

fn near(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

impl Geometry {
    pub fn read(doc: &Document, page: usize) -> Option<Geometry> {
        let tree = pagebara_pages::PageTree::load(doc);
        let p = tree.pages().get(page)?;
        let d = doc.get(p.r)?.as_dict()?.clone();
        let list = Interpreter::new(doc).page(&d, &resources(doc, &d));
        let mut g = Geometry {
            text: pagebara_content::extract_text(doc, &list),
            page: Rect::new(p.crop_box.x0, p.crop_box.y0, p.crop_box.x1, p.crop_box.y1),
            ..Geometry::default()
        };
        g.shapes(&list);
        g.underscore_runs();
        Some(g)
    }

    /// Collects segments and rectangles from strokes, and thin filled rectangles as lines.
    fn shapes(&mut self, list: &DisplayList) {
        let mut count = 0;
        for cmd in &list.cmds {
            let (path, ctm, filled) = match *cmd {
                Cmd::Stroke { path, ctm, .. } => (path, ctm, false),
                Cmd::Fill { path, ctm, .. } => (path, ctm, true),
                _ => continue,
            };
            count += 1;
            if count > MAX_SHAPES {
                self.horizontal.clear();
                self.vertical.clear();
                self.rects.clear();
                return;
            }
            let m = list.matrix(ctm);
            let p = list.path(path);
            let mut pts = p.points.iter();
            let mut sub: Vec<Point> = Vec::new();
            let mut curved = false;
            let mut subs: Vec<(Vec<Point>, bool)> = Vec::new();
            for v in p.verbs {
                match v {
                    Verb::Move => {
                        if sub.len() > 1 {
                            subs.push((std::mem::take(&mut sub), curved));
                        }
                        sub.clear();
                        curved = false;
                        sub.extend(pts.next().map(|q| m.apply(*q)));
                    }
                    Verb::Line => sub.extend(pts.next().map(|q| m.apply(*q))),
                    Verb::Cubic => {
                        curved = true;
                        let last = pts.by_ref().take(3).last();
                        sub.extend(last.map(|q| m.apply(*q)));
                    }
                    Verb::Close => {
                        if let Some(&f) = sub.first() {
                            sub.push(f);
                        }
                    }
                }
            }
            if sub.len() > 1 {
                subs.push((sub, curved));
            }
            for (s, curved) in subs {
                if !curved {
                    self.add_subpath(&s, filled);
                }
            }
        }
    }

    fn add_subpath(&mut self, s: &[Point], filled: bool) {
        let pts: Vec<(f64, f64)> = s.iter().map(|p| (f64::from(p.x), f64::from(p.y))).collect();
        let closed = pts.len() >= 4 && {
            let (a, b) = (pts[0], pts[pts.len() - 1]);
            near(a.0, b.0, 0.5) && near(a.1, b.1, 0.5)
        };
        if closed && pts.len() <= 6 {
            let xs: Vec<f64> = pts.iter().map(|p| p.0).collect();
            let ys: Vec<f64> = pts.iter().map(|p| p.1).collect();
            let (x0, x1) = (
                xs.iter().copied().fold(f64::MAX, f64::min),
                xs.iter().copied().fold(f64::MIN, f64::max),
            );
            let (y0, y1) = (
                ys.iter().copied().fold(f64::MAX, f64::min),
                ys.iter().copied().fold(f64::MIN, f64::max),
            );
            let axis = pts.iter().all(|p| {
                (near(p.0, x0, 0.5) || near(p.0, x1, 0.5))
                    && (near(p.1, y0, 0.5) || near(p.1, y1, 0.5))
            });
            if axis {
                let (w, h) = (x1 - x0, y1 - y0);
                if filled && h <= 2.0 && w >= MIN_LINE {
                    self.horizontal.push(Seg {
                        at: (y0 + y1) / 2.0,
                        from: x0,
                        to: x1,
                    });
                } else if filled && w <= 2.0 && h >= 6.0 {
                    self.vertical.push(Seg {
                        at: (x0 + x1) / 2.0,
                        from: y0,
                        to: y1,
                    });
                } else if !filled && w > 2.0 && h > 2.0 {
                    self.rects.push(Rect::new(x0, y0, x1, y1));
                }
                return;
            }
        }
        if filled {
            return;
        }
        for w in pts.windows(2) {
            let ((ax, ay), (bx, by)) = (w[0], w[1]);
            if near(ay, by, 0.5) && (bx - ax).abs() > 1.0 {
                self.horizontal.push(Seg {
                    at: (ay + by) / 2.0,
                    from: ax.min(bx),
                    to: ax.max(bx),
                });
            } else if near(ax, bx, 0.5) && (by - ay).abs() > 1.0 {
                self.vertical.push(Seg {
                    at: (ax + bx) / 2.0,
                    from: ay.min(by),
                    to: ay.max(by),
                });
            }
        }
    }

    fn underscore_runs(&mut self) {
        for line in &self.text.lines {
            let chars = &self.text.chars[line.chars.0 as usize..line.chars.1 as usize];
            let mut run: Vec<Rect> = Vec::new();
            let flush = |run: &mut Vec<Rect>, out: &mut Vec<Rect>| {
                if run.len() >= 3 {
                    let x0 = run.iter().map(|r| r.x0).fold(f64::MAX, f64::min);
                    let x1 = run.iter().map(|r| r.x1).fold(f64::MIN, f64::max);
                    let y0 = run.iter().map(|r| r.y0).fold(f64::MAX, f64::min);
                    let y1 = run.iter().map(|r| r.y1).fold(f64::MIN, f64::max);
                    out.push(Rect::new(x0, y0, x1, y1));
                }
                run.clear();
            };
            for c in chars {
                let b = c.bbox();
                let r = Rect::new(
                    f64::from(b.x0),
                    f64::from(b.y0),
                    f64::from(b.x1),
                    f64::from(b.y1),
                );
                if self.text.char_text(c) == "_" {
                    run.push(r);
                } else {
                    flush(&mut run, &mut self.underscores);
                }
            }
            flush(&mut run, &mut self.underscores);
        }
    }

    /// Rectangles made of four separate segments (table cells, drawn boxes).
    fn segment_boxes(&self) -> Vec<Rect> {
        let mut out = Vec::new();
        let hs: Vec<&Seg> = self
            .horizontal
            .iter()
            .filter(|s| s.to - s.from > 4.0)
            .take(600)
            .collect();
        for (i, a) in hs.iter().enumerate() {
            for b in &hs[i + 1..] {
                let (top, bottom) = if a.at > b.at { (a, b) } else { (b, a) };
                let h = top.at - bottom.at;
                if !(5.0..=200.0).contains(&h)
                    || !near(top.from, bottom.from, 2.0)
                    || !near(top.to, bottom.to, 2.0)
                {
                    continue;
                }
                let side = |x: f64| {
                    self.vertical.iter().any(|v| {
                        near(v.at, x, 2.0) && v.from <= bottom.at + 2.0 && v.to >= top.at - 2.0
                    })
                };
                if side(top.from) && side(top.to) {
                    out.push(Rect::new(top.from, bottom.at, top.to, top.at));
                }
            }
        }
        out
    }

    /// Text characters (not spaces or underscores) inside `r`.
    pub fn text_in(&self, r: &Rect) -> Vec<Rect> {
        self.text
            .chars
            .iter()
            .filter(|c| {
                let t = self.text.char_text(c);
                !t.trim().is_empty() && t != "_"
            })
            .map(|c| {
                let b = c.bbox();
                Rect::new(
                    f64::from(b.x0),
                    f64::from(b.y0),
                    f64::from(b.x1),
                    f64::from(b.y1),
                )
            })
            .filter(|b| {
                let (cx, cy) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
                cx > r.x0 && cx < r.x1 && cy > r.y0 && cy < r.y1
            })
            .collect()
    }

    /// The tallest rectangle up to `want` points high on line `r`, below any text above.
    pub fn room_above(&self, r: Rect, want: f64) -> Rect {
        let probe = Rect::new(r.x0, r.y0, r.x1, r.y0 + want);
        let top = self
            .text_in(&probe)
            .iter()
            .map(|b| b.y0 - 1.0)
            .fold(r.y0 + want, f64::min);
        Rect::new(r.x0, r.y0, r.x1, top.max(r.y1))
    }

    /// Every place for a field: boxes, combs and check boxes first, then underscores and
    /// lines that are not inside a box.
    pub fn areas(&self) -> Vec<Area> {
        let mut rects = self.rects.clone();
        for b in self.segment_boxes() {
            if !rects.iter().any(|r| {
                near(r.x0, b.x0, 2.0)
                    && near(r.y0, b.y0, 2.0)
                    && near(r.x1, b.x1, 2.0)
                    && near(r.y1, b.y1, 2.0)
            }) {
                rects.push(b);
            }
        }
        let page_w = self.page.width();
        let contains = |outer: &Rect, inner: &Rect| {
            inner.x0 >= outer.x0 - 0.5
                && inner.x1 <= outer.x1 + 0.5
                && inner.y0 >= outer.y0 - 0.5
                && inner.y1 <= outer.y1 + 0.5
                && outer != inner
        };
        let squares: Vec<Rect> = rects
            .iter()
            .copied()
            .filter(|r| (6.0..=24.0).contains(&r.width()) && near(r.width(), r.height(), 2.5))
            .collect();
        let mut out = Vec::new();
        // Rows of equal adjacent squares are combs.
        let mut in_comb: Vec<Rect> = Vec::new();
        let mut sorted = squares.clone();
        sorted.sort_by(|a, b| b.y0.total_cmp(&a.y0).then(a.x0.total_cmp(&b.x0)));
        let mut i = 0;
        while i < sorted.len() {
            let mut j = i + 1;
            while j < sorted.len()
                && near(sorted[j].y0, sorted[i].y0, 1.5)
                && near(sorted[j].width(), sorted[i].width(), 1.5)
                && sorted[j].x0 - sorted[j - 1].x1 <= 3.0
                && sorted[j].x0 >= sorted[j - 1].x1 - 1.0
            {
                j += 1;
            }
            if j - i >= 3 {
                let run = &sorted[i..j];
                out.push(Area::Comb(
                    Rect::new(run[0].x0, run[0].y0, run[j - i - 1].x1, run[0].y1),
                    j - i,
                ));
                in_comb.extend_from_slice(run);
            }
            i = j;
        }
        for r in &rects {
            if in_comb.contains(r) {
                continue;
            }
            let has_inner = rects.iter().any(|o| contains(r, o));
            if squares.contains(r) {
                if self.text_in(r).is_empty() {
                    out.push(Area::Check(r.inset(0.5)));
                }
                continue;
            }
            let (w, h) = (r.width(), r.height());
            if has_inner
                || w < MIN_LINE
                || h < 10.0
                || h > 300.0
                || (w > page_w * 0.85 && h > 150.0)
            {
                continue;
            }
            let text = self.text_in(r);
            if text.is_empty() {
                out.push(Area::Box(r.inset(1.0)));
            } else {
                // A label printed in the top of the box: the field fills the rest.
                let lowest = text.iter().map(|b| b.y0).fold(f64::MAX, f64::min);
                if lowest > r.y0 + h * 0.5 && lowest - r.y0 >= 10.0 {
                    out.push(Area::Box(Rect::new(
                        r.x0 + 1.0,
                        r.y0 + 1.0,
                        r.x1 - 1.0,
                        lowest - 1.0,
                    )));
                }
            }
        }
        let in_rect = |x0: f64, x1: f64, y: f64| {
            rects
                .iter()
                .any(|r| x0 >= r.x0 - 1.0 && x1 <= r.x1 + 1.0 && y >= r.y0 - 1.0 && y <= r.y1 + 1.0)
        };
        for u in &self.underscores {
            if u.width() >= MIN_LINE * 0.6 && !in_rect(u.x0, u.x1, u.y0) {
                let base = Rect::new(u.x0, u.y0 + 1.0, u.x1, u.y0 + 1.0);
                out.push(Area::Line(self.room_above(base, 18.0).max_height(18.0)));
            }
        }
        for s in &self.horizontal {
            let len = s.to - s.from;
            if len < MIN_LINE || in_rect(s.from, s.to, s.at) {
                continue;
            }
            // Text standing on the line: an underline or a table rule, not a field.
            let probe = Rect::new(s.from, s.at, s.to, s.at + 10.0);
            let covered: f64 = self.text_in(&probe).iter().map(Rect::width).sum();
            if covered > len * 0.3 {
                continue;
            }
            let base = Rect::new(s.from, s.at + 1.0, s.to, s.at + 1.0);
            out.push(Area::Line(self.room_above(base, 18.0).max_height(18.0)));
        }
        out.retain(|a| a.rect().height() >= 6.0);
        out
    }
}

trait MaxHeight {
    fn max_height(self, h: f64) -> Rect;
}

impl MaxHeight for Rect {
    fn max_height(self, h: f64) -> Rect {
        Rect::new(self.x0, self.y0, self.x1, self.y1.min(self.y0 + h))
    }
}

impl Geometry {
    /// Every word with its box and line.
    pub fn words_near(&self) -> impl Iterator<Item = (Rect, &str, usize)> {
        self.text.lines.iter().enumerate().flat_map(move |(li, l)| {
            self.text.words[l.words.0 as usize..l.words.1 as usize]
                .iter()
                .map(move |w| {
                    let b = w.bbox;
                    (
                        Rect::new(
                            f64::from(b.x0),
                            f64::from(b.y0),
                            f64::from(b.x1),
                            f64::from(b.y1),
                        ),
                        self.text.word_text(w),
                        li,
                    )
                })
        })
    }
}
