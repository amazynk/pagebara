//! Page layouts: pages in rows, rows stacked top to bottom with a fixed gap.
//!
//! Units: points (pt) on the page, logical pixels (px) on screen. At zoom `z` a point is
//! `k = z * PX_PER_PT` pixels. Row `r` starts at `GAP * (r + 1) + k * tops[r]`; within a
//! row a page sits `yoff` points down and `xoff` points right of the content's centre line,
//! so pages of a spread or a horizontal strip scale with the zoom, gaps between rows do not.

use std::ops::{Deref, Range};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Pixels per point at 100 % (96 dpi screens, 72 pt per inch).
pub const PX_PER_PT: f64 = 96.0 / 72.0;
/// Space around and between rows of pages, in pixels at every zoom.
pub const GAP: f64 = 16.0;
/// Between the two pages of a two-page spread, in points.
const SPREAD_GAP: f64 = 8.0;
/// Between pages of the horizontal strip, in points.
const STRIP_GAP: f64 = 12.0;

/// How pages are arranged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutMode {
    /// One column, scrolling through all pages.
    #[default]
    Continuous,
    /// One page at a time; scrolling past its end turns the page.
    Single,
    /// Two pages side by side, odd pages on the left.
    TwoPage,
    /// Like a book: the cover alone on the right, then spreads.
    Book,
    /// All pages in one row, scrolling sideways.
    Horizontal,
}

impl LayoutMode {
    pub const ALL: [LayoutMode; 5] = [
        LayoutMode::Continuous,
        LayoutMode::Single,
        LayoutMode::TwoPage,
        LayoutMode::Book,
        LayoutMode::Horizontal,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|m| *m == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }

    pub fn name(self) -> &'static str {
        match self {
            LayoutMode::Continuous => "continuous",
            LayoutMode::Single => "single",
            LayoutMode::TwoPage => "two-page",
            LayoutMode::Book => "book",
            LayoutMode::Horizontal => "horizontal",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.name() == s)
    }
}

/// Shared on clone: views copy the layout every frame, and long documents have many pages.
#[derive(Clone, Debug, Default)]
pub struct Layout(Arc<LayoutData>);

/// The data of a [`Layout`].
#[derive(Debug, Default)]
pub struct LayoutData {
    mode: LayoutMode,
    /// Displayed page sizes in points.
    sizes: Vec<(f64, f64)>,
    /// The row of each page.
    row: Vec<usize>,
    /// The pages of each row.
    rows: Vec<Range<usize>>,
    /// `tops[r]`: summed heights of the rows before `r`, in points; one more entry than
    /// rows, the last being the total.
    tops: Vec<f64>,
    /// A page's left edge from the centre line, in points.
    xoff: Vec<f64>,
    /// A page's top from the top of its row, in points.
    yoff: Vec<f64>,
    /// The widest row, in points (symmetric about the centre line).
    max_w: f64,
    /// Pages run right to left in a row (the document's `/Direction /R2L`).
    r2l: bool,
}

/// A rectangle in pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Rect {
    pub fn right(&self) -> f64 {
        self.x + self.w
    }

    pub fn bottom(&self) -> f64 {
        self.y + self.h
    }

    pub fn intersects(&self, o: &Rect) -> bool {
        self.x < o.right() && o.x < self.right() && self.y < o.bottom() && o.y < self.bottom()
    }

    pub fn intersection(&self, o: &Rect) -> Option<Rect> {
        let x = self.x.max(o.x);
        let y = self.y.max(o.y);
        let r = self.right().min(o.right());
        let b = self.bottom().min(o.bottom());
        (r > x && b > y).then_some(Rect {
            x,
            y,
            w: r - x,
            h: b - y,
        })
    }

    pub fn inflate(&self, dx: f64, dy: f64) -> Rect {
        Rect {
            x: self.x - dx,
            y: self.y - dy,
            w: self.w + 2.0 * dx,
            h: self.h + 2.0 * dy,
        }
    }
}

/// The pages of each row in `mode`.
fn rows_of(mode: LayoutMode, n: usize) -> Vec<Range<usize>> {
    match mode {
        LayoutMode::Continuous | LayoutMode::Single => (0..n).map(|i| i..i + 1).collect(),
        LayoutMode::TwoPage => (0..n).step_by(2).map(|i| i..(i + 2).min(n)).collect(),
        LayoutMode::Book => {
            let mut rows: Vec<Range<usize>> = (0..n.min(1)).map(|_| 0..1).collect();
            rows.extend((1..n).step_by(2).map(|i| i..(i + 2).min(n)));
            rows
        }
        LayoutMode::Horizontal => (n > 0).then_some(0..n).into_iter().collect(),
    }
}

impl Layout {
    /// The continuous layout.
    pub fn new(sizes: impl IntoIterator<Item = (f32, f32)>) -> Self {
        Self::with_mode(sizes, LayoutMode::Continuous)
    }

    pub fn with_mode(sizes: impl IntoIterator<Item = (f32, f32)>, mode: LayoutMode) -> Self {
        Self::with_direction(sizes, mode, false)
    }

    /// `r2l`: the pages of a spread or strip run right to left, as in a document for a
    /// right-to-left script (`/ViewerPreferences` `/Direction /R2L`).
    pub fn with_direction(
        sizes: impl IntoIterator<Item = (f32, f32)>,
        mode: LayoutMode,
        r2l: bool,
    ) -> Self {
        let sizes: Vec<(f64, f64)> = sizes
            .into_iter()
            .map(|(w, h)| (f64::from(w), f64::from(h)))
            .collect();
        let n = sizes.len();
        let rows = rows_of(mode, n);
        let mut l = LayoutData {
            mode,
            row: vec![0; n],
            tops: Vec::with_capacity(rows.len() + 1),
            xoff: vec![0.0; n],
            yoff: vec![0.0; n],
            max_w: 0.0,
            sizes,
            rows: Vec::new(),
            r2l,
        };
        let mut acc = 0.0;
        for (r, pages) in rows.iter().enumerate() {
            l.tops.push(acc);
            let h = pages.clone().map(|i| l.sizes[i].1).fold(0.0, f64::max);
            for i in pages.clone() {
                l.row[i] = r;
                l.yoff[i] = (h - l.sizes[i].1) / 2.0;
            }
            l.place_row(pages.clone());
            if r2l {
                for i in pages.clone() {
                    l.xoff[i] = -(l.xoff[i] + l.sizes[i].0);
                }
            }
            acc += h;
        }
        l.tops.push(acc);
        l.max_w = (0..n)
            .map(|i| 2.0 * (-l.xoff[i]).max(l.xoff[i] + l.sizes[i].0))
            .fold(0.0, f64::max);
        l.rows = rows;
        Layout(Arc::new(l))
    }
}

impl Deref for Layout {
    type Target = LayoutData;

    fn deref(&self) -> &LayoutData {
        &self.0
    }
}

impl LayoutData {
    /// Sets `xoff` for the pages of one row.
    fn place_row(&mut self, pages: Range<usize>) {
        let w = |i: usize| self.sizes[i].0;
        match self.mode {
            LayoutMode::Continuous | LayoutMode::Single => {
                for i in pages {
                    self.xoff[i] = -w(i) / 2.0;
                }
            }
            LayoutMode::TwoPage | LayoutMode::Book => {
                let gap = if self.mode == LayoutMode::Book {
                    0.0
                } else {
                    SPREAD_GAP
                };
                let (a, len) = (pages.start, pages.len());
                // The book's cover is a right-hand page; a lone last page is a left one.
                let cover = self.mode == LayoutMode::Book && a == 0;
                if len == 2 || !cover {
                    self.xoff[a] = -gap / 2.0 - w(a);
                }
                if len == 2 {
                    self.xoff[a + 1] = gap / 2.0;
                } else if cover {
                    self.xoff[a] = gap / 2.0;
                }
            }
            LayoutMode::Horizontal => {
                let total: f64 =
                    pages.clone().map(w).sum::<f64>() + STRIP_GAP * (pages.len() as f64 - 1.0);
                let mut x = -total / 2.0;
                for i in pages {
                    self.xoff[i] = x;
                    x += w(i) + STRIP_GAP;
                }
            }
        }
    }
}

impl Layout {
    pub fn mode(&self) -> LayoutMode {
        self.mode
    }

    pub fn r2l(&self) -> bool {
        self.r2l
    }

    pub fn len(&self) -> usize {
        self.sizes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sizes.is_empty()
    }

    pub fn size(&self, i: usize) -> (f64, f64) {
        self.sizes[i]
    }

    pub fn sizes(&self) -> &[(f64, f64)] {
        &self.sizes
    }

    pub fn row_of(&self, i: usize) -> usize {
        self.row[i]
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn row_pages(&self, r: usize) -> Range<usize> {
        self.rows[r].clone()
    }

    /// The width of the widest row in points.
    pub fn max_width(&self) -> f64 {
        self.max_w
    }

    /// The size in points of what "fit page" shows for page `i`: its spread, or the page.
    pub fn fit_size(&self, i: usize) -> (f64, f64) {
        if self.mode == LayoutMode::Horizontal {
            return self.sizes[i];
        }
        let r = self.row[i];
        let w = self.rows[r]
            .clone()
            .map(|p| 2.0 * (-self.xoff[p]).max(self.xoff[p] + self.sizes[p].0))
            .fold(0.0, f64::max);
        (w, self.tops[r + 1] - self.tops[r])
    }

    /// The width "fit width" fills: the widest row, or the page in the horizontal strip.
    pub fn fit_width_of(&self, i: usize) -> f64 {
        if self.mode == LayoutMode::Horizontal {
            self.sizes.get(i).map_or(0.0, |s| s.0)
        } else {
            self.max_w
        }
    }

    /// Where page `i` is relative to page `a`: rows between them, and points down and
    /// right from `a`'s top left corner (see [`super::Placement`]).
    pub fn offset(&self, a: usize, i: usize) -> (i32, f64, f64) {
        let (ra, ri) = (self.row[a], self.row[i]);
        (
            ri as i32 - ra as i32,
            self.tops[ri] + self.yoff[i] - self.tops[ra] - self.yoff[a],
            self.xoff[i] - self.xoff[a],
        )
    }

    /// The left edge of page `i` from the centre line, in points.
    pub fn x_offset(&self, i: usize) -> f64 {
        self.xoff[i]
    }

    pub fn content_width(&self, k: f64, viewport_w: f64) -> f64 {
        (self.max_w * k + 2.0 * GAP).max(viewport_w)
    }

    pub fn content_height(&self, k: f64) -> f64 {
        let rows = self.rows.len();
        GAP * (rows as f64 + 1.0) + k * self.tops[rows]
    }

    /// The top of row `r` in content pixels.
    pub fn row_top(&self, r: usize, k: f64) -> f64 {
        GAP * (r as f64 + 1.0) + k * self.tops[r]
    }

    /// The bottom of row `r` in content pixels.
    pub fn row_bottom(&self, r: usize, k: f64) -> f64 {
        GAP * (r as f64 + 1.0) + k * self.tops[r + 1]
    }

    pub fn page_top(&self, i: usize, k: f64) -> f64 {
        self.row_top(self.row[i], k) + k * self.yoff[i]
    }

    /// Page `i` in content pixels, for a content area `content_w` wide.
    pub fn page_rect(&self, i: usize, k: f64, content_w: f64) -> Rect {
        let (w, h) = self.sizes[i];
        Rect {
            x: content_w / 2.0 + k * self.xoff[i],
            y: self.page_top(i, k),
            w: w * k,
            h: h * k,
        }
    }

    /// Rows that overlap content rows `y0..y1`.
    fn rows_in(&self, y0: f64, y1: f64, k: f64) -> Range<usize> {
        let n = self.rows.len();
        let first = partition(n, |r| self.row_bottom(r, k) <= y0);
        let end = partition(n, |r| self.row_top(r, k) < y1);
        first..end.max(first)
    }

    /// Pages that overlap `view` (content pixels), for a content area `content_w` wide.
    pub fn pages_in(&self, view: &Rect, k: f64, content_w: f64) -> Range<usize> {
        let rows = self.rows_in(view.y, view.bottom(), k);
        if rows.is_empty() {
            let at = self.rows.get(rows.start).map_or(self.len(), |r| r.start);
            return at..at;
        }
        let pages = self.rows[rows.start].start..self.rows[rows.end - 1].end;
        if self.mode != LayoutMode::Horizontal {
            return pages;
        }
        // One row: narrow down along it.
        let a = pages.start;
        let x = |j: usize| content_w / 2.0 + k * self.xoff[a + j];
        let n = pages.len();
        let first = partition(n, |j| x(j) + k * self.sizes[a + j].0 <= view.x);
        let end = partition(n, |j| x(j) < view.right());
        a + first..a + end.max(first)
    }

    /// The page at content point `(x, y)`, or the nearest one.
    pub fn page_at(&self, x: f64, y: f64, k: f64, content_w: f64) -> usize {
        let n = self.rows.len();
        if n == 0 {
            return 0;
        }
        let r = partition(n, |r| self.row_bottom(r, k) + GAP / 2.0 <= y).min(n - 1);
        let pages = self.rows[r].clone();
        let dist = |i: usize| {
            let p = self.page_rect(i, k, content_w);
            (p.x - x).max(x - p.right()).max(0.0)
        };
        pages
            .clone()
            .min_by(|&a, &b| dist(a).total_cmp(&dist(b)))
            .unwrap_or(pages.start)
    }
}

/// The first index in `0..n` where `pred` is false; `pred` must be true then false.
fn partition(n: usize, pred: impl Fn(usize) -> bool) -> usize {
    let (mut lo, mut hi) = (0, n);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if pred(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}
