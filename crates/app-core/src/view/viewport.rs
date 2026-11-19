//! Scroll position and zoom over a [`Layout`].

use std::ops::Range;

use super::layout::{GAP, Layout, LayoutMode, PX_PER_PT, Rect};
use super::zoom;

#[derive(Clone, Debug)]
pub struct Viewport {
    layout: Layout,
    /// 1.0 is 100 %.
    zoom: f64,
    /// Top-left of the visible area in content pixels.
    sx: f64,
    sy: f64,
    vw: f64,
    vh: f64,
    /// Device pixels per logical pixel.
    dpr: f64,
    /// The row shown in the single page layout.
    row: usize,
}

impl Viewport {
    pub fn new(layout: Layout) -> Self {
        Viewport {
            layout,
            zoom: 1.0,
            sx: 0.0,
            sy: 0.0,
            vw: 800.0,
            vh: 600.0,
            dpr: 1.0,
            row: 0,
        }
    }

    pub fn layout(&self) -> &Layout {
        &self.layout
    }

    /// Switches to another layout of the same pages, showing page `page` from its top.
    pub fn set_layout(&mut self, layout: Layout, page: usize) {
        self.layout = layout;
        self.row = 0;
        self.go_to_page(page);
    }

    fn paged(&self) -> bool {
        self.layout.mode() == LayoutMode::Single && !self.layout.is_empty()
    }

    pub fn zoom(&self) -> f64 {
        self.zoom
    }

    /// Logical pixels per point.
    pub fn k(&self) -> f64 {
        self.zoom * PX_PER_PT
    }

    pub fn dpr(&self) -> f64 {
        self.dpr
    }

    pub fn scroll(&self) -> (f64, f64) {
        (self.sx, self.sy)
    }

    pub fn size(&self) -> (f64, f64) {
        (self.vw, self.vh)
    }

    pub fn content_size(&self) -> (f64, f64) {
        let k = self.k();
        (
            self.layout.content_width(k, self.vw),
            self.layout.content_height(k),
        )
    }

    /// The part of the content that can be scrolled through: all of it, or the current
    /// page in the single page layout.
    pub fn bounds(&self) -> Rect {
        let (cw, ch) = self.content_size();
        if !self.paged() {
            return Rect {
                x: 0.0,
                y: 0.0,
                w: cw,
                h: ch,
            };
        }
        let k = self.k();
        let top = self.layout.row_top(self.row, k) - GAP;
        Rect {
            x: 0.0,
            y: top,
            w: cw,
            h: self.layout.row_bottom(self.row, k) + GAP - top,
        }
    }

    /// The visible area in content pixels.
    pub fn visible(&self) -> Rect {
        Rect {
            x: self.sx,
            y: self.sy,
            w: self.vw,
            h: self.vh,
        }
    }

    /// The pages to show: those in view, only the current one in the single page layout.
    pub fn visible_pages(&self) -> Range<usize> {
        if self.paged() {
            return self.layout.row_pages(self.row);
        }
        let k = self.k();
        let cw = self.content_size().0;
        self.layout.pages_in(&self.visible(), k, cw)
    }

    pub fn set_dpr(&mut self, dpr: f64) {
        self.dpr = dpr.max(0.25);
    }

    pub fn set_size(&mut self, w: f64, h: f64) {
        self.vw = w.max(1.0);
        self.vh = h.max(1.0);
        self.clamp();
    }

    pub fn scroll_to(&mut self, x: f64, y: f64) {
        self.sx = x;
        self.sy = y;
        self.clamp();
    }

    pub fn scroll_by(&mut self, dx: f64, dy: f64) {
        self.scroll_to(self.sx + dx, self.sy + dy);
    }

    /// Sets the zoom keeping the point under `(ax, ay)` (viewport pixels) in place.
    pub fn zoom_at(&mut self, zoom: f64, ax: f64, ay: f64) {
        let zoom = zoom::clamp(zoom);
        if self.layout.is_empty() {
            self.zoom = zoom;
            return;
        }
        let k = self.k();
        let (cw, _) = self.content_size();
        let u = (self.sx + ax - cw / 2.0) / k;
        let i = if self.paged() {
            self.layout.row_pages(self.row).start
        } else {
            self.layout.page_at(self.sx + ax, self.sy + ay, k, cw)
        };
        let r = self.layout.row_of(i) as f64;
        let v = (self.sy + ay - GAP * (r + 1.0)) / k;
        self.zoom = zoom;
        let k = self.k();
        let (cw, _) = self.content_size();
        self.sx = cw / 2.0 + k * u - ax;
        self.sy = GAP * (r + 1.0) + k * v - ay;
        self.clamp();
    }

    /// Zooms around the centre of the view.
    pub fn zoom_to(&mut self, zoom: f64) {
        self.zoom_at(zoom, self.vw / 2.0, self.vh / 2.0);
    }

    /// Fits the widest row (in the horizontal layout, the current page) to the view width.
    pub fn fit_width(&mut self) {
        if self.layout.is_empty() {
            return;
        }
        let i = self.current_page();
        let w = self.layout.fit_width_of(i);
        if w > 0.0 {
            let at_top = self.sy <= 0.0;
            self.zoom_at((self.vw - 2.0 * GAP) / (w * PX_PER_PT), self.vw / 2.0, 0.0);
            if self.layout.mode() == LayoutMode::Horizontal {
                self.go_to_page(i);
            } else if at_top {
                self.sy = 0.0;
                self.clamp();
            }
        }
    }

    /// Fits the current page (its spread, in the two-page layouts) into the view and shows
    /// it whole.
    pub fn fit_page(&mut self) {
        if self.layout.is_empty() {
            return;
        }
        let i = self.current_page();
        let (w, h) = self.layout.fit_size(i);
        let z = ((self.vw - 2.0 * GAP) / w).min((self.vh - 2.0 * GAP) / h) / PX_PER_PT;
        self.zoom = zoom::clamp(z);
        self.go_to_page(i);
        // Centre the row vertically when the view is taller than it.
        let k = self.k();
        let r = self.layout.row_of(i);
        let top = self.layout.row_top(r, k);
        let rh = self.layout.row_bottom(r, k) - top;
        self.scroll_to(self.sx, top - (self.vh - rh) / 2.0);
    }

    /// Shows page `i` from its top (and its left edge, in the horizontal layout).
    pub fn go_to_page(&mut self, i: usize) {
        if i >= self.layout.len() {
            return;
        }
        let k = self.k();
        let r = self.layout.row_of(i);
        self.row = r;
        let y = self.layout.row_top(r, k) - GAP;
        let x = if self.layout.mode() == LayoutMode::Horizontal {
            let cw = self.content_size().0;
            let p = self.layout.page_rect(i, k, cw);
            // Centred when the page fits, else from its left edge.
            if p.w + 2.0 * GAP <= self.vw {
                p.x + p.w / 2.0 - self.vw / 2.0
            } else {
                p.x - GAP
            }
        } else {
            self.sx
        };
        self.scroll_to(x, y);
    }

    /// In the single page layout, makes page `i`'s row the one shown (before scrolling to
    /// a place on it).
    pub fn show_row_of(&mut self, i: usize) {
        if self.paged() && i < self.layout.len() {
            self.row = self.layout.row_of(i);
            self.clamp();
        }
    }

    /// The page covering most of the view.
    pub fn current_page(&self) -> usize {
        if self.paged() {
            return self.layout.row_pages(self.row).start;
        }
        let k = self.k();
        let view = self.visible();
        let cw = self.content_size().0;
        let range = self.layout.pages_in(&view, k, cw);
        range
            .clone()
            .max_by(|&a, &b| {
                let area = |i| {
                    let r = self.layout.page_rect(i, k, cw);
                    r.intersection(&view).map_or(0.0, |r| r.w * r.h)
                };
                // Ties go to the earlier page.
                area(a).total_cmp(&area(b)).then(b.cmp(&a))
            })
            .unwrap_or_else(|| self.layout.page_at(view.x, view.y, k, cw))
    }

    /// Whether the view is at the start (`end` false) or the end of what it can scroll to.
    pub fn at_edge(&self, end: bool) -> bool {
        let b = self.bounds();
        if end {
            self.sy + self.vh >= b.bottom() - 0.5
        } else {
            self.sy <= b.y + 0.5
        }
    }

    fn clamp(&mut self) {
        let b = self.bounds();
        // The single page and the horizontal strip are centred when shorter than the view.
        let centre = matches!(
            self.layout.mode(),
            LayoutMode::Single | LayoutMode::Horizontal
        );
        let fit = |v: f64, lo: f64, len: f64, view: f64, centre: bool| {
            if len <= view {
                if centre { lo - (view - len) / 2.0 } else { lo }
            } else {
                v.clamp(lo, lo + len - view)
            }
        };
        self.sx = fit(self.sx, b.x, b.w, self.vw, true);
        self.sy = fit(self.sy, b.y, b.h, self.vh, centre);
    }
}
