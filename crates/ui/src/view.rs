//! Drives a Slint page view from a [`PageView`]: keeps widget slots stable (so tile
//! textures are uploaded once), moves everything by setting three numbers per frame, and
//! turns wheel, drag, fling, pinch and keys into scrolling and zooming.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use pagebara_app_core::tiles::{Key, PageRenderer};
use pagebara_app_core::view::gesture::Fling;
use pagebara_app_core::view::{
    Frame, GAP, Layout, LayoutMode, PageView, Placement, SlotChange, SlotPool, Tuning, zoom,
};
use slint::{Image, Model, ModelRc, VecModel};

use crate::SlintTile;

/// Rendered tiles kept per document on a desktop (see [`Tuning::desktop`]).
pub fn cache_bytes() -> usize {
    Tuning::desktop(total_memory()).cache_bytes
}

/// This machine's memory, or 8 GiB when unknown.
pub fn total_memory() -> u64 {
    pagebara_budget::total_memory().unwrap_or(8 << 30)
}

/// The models of a page view: page slots and whether tiles cover them, tile slots and
/// their images, and the fallback tiles standing in for missing ones.
pub struct Models<P: 'static, T: 'static> {
    pub pages: ModelRc<P>,
    pub covered: ModelRc<bool>,
    pub tiles: ModelRc<T>,
    pub tile_images: ModelRc<Image>,
    pub fallback: ModelRc<T>,
    pub fallback_images: ModelRc<Image>,
}

impl<P, T> std::fmt::Debug for Models<P, T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Models({} pages, {} tiles)",
            self.pages.row_count(),
            self.tiles.row_count()
        )
    }
}

impl<P, T> Default for Models<P, T> {
    fn default() -> Self {
        Models {
            pages: ModelRc::default(),
            covered: ModelRc::default(),
            tiles: ModelRc::default(),
            tile_images: ModelRc::default(),
            fallback: ModelRc::default(),
            fallback_images: ModelRc::default(),
        }
    }
}

/// The widgets of an app's page view: Slint generates the slot types and the global
/// holding the view's properties per app.
pub trait ViewSink {
    type Page: Clone + Default + 'static;
    type Tile: Clone + Default + 'static;

    fn page_slot(p: &Placement, number: i32, preview: Image) -> Self::Page;
    fn tile_slot(p: &Placement) -> Self::Tile;
    fn scale_factor(&self) -> f64;
    fn show_models(&self, models: Models<Self::Page, Self::Tile>);
    /// Logical pixels per point, the pages' centre line, the anchor page's top, and the
    /// device pixel ratio (see `pagebara_app_core::view`).
    fn show_transform(&self, k: f32, cx: f32, oy: f32, dpr: f32);
    /// Scrollbar thumbs as fractions of the track.
    fn show_scrollbars(&self, v_start: f32, v_size: f32, h_start: f32, h_size: f32);
}
/// Mouse wheels report whole lines of this many pixels; those scroll smoothly.
const WHEEL_LINE: f64 = 60.0;
const SMOOTH_SCROLL: Duration = Duration::from_millis(140);
const ARROW_STEP: f64 = 48.0;

/// Scrolls jump instead of gliding (the "reduce motion" preference).
pub static REDUCE_MOTION: AtomicBool = AtomicBool::new(false);

/// A smooth scroll in progress.
#[derive(Clone, Copy, Debug)]
struct Glide {
    from: (f64, f64),
    to: (f64, f64),
    start: Instant,
    duration: Duration,
}

impl Glide {
    fn at(&self, now: Instant) -> ((f64, f64), bool) {
        let t = (now - self.start).as_secs_f64() / self.duration.as_secs_f64();
        let t = t.clamp(0.0, 1.0);
        // Ease-out cubic.
        let e = 1.0 - (1.0 - t).powi(3);
        let lerp = |a: f64, b: f64| a + (b - a) * e;
        (
            (lerp(self.from.0, self.to.0), lerp(self.from.1, self.to.1)),
            t >= 1.0,
        )
    }
}

/// Slots of one layer of tiles: geometry rows and, separately, their images, so moving a
/// tile never touches its image (which would re-upload the texture).
struct TileLayer<T: 'static> {
    pool: SlotPool<Key>,
    rows: Rc<VecModel<T>>,
    images: Rc<VecModel<Image>>,
    placed: Vec<Option<Placement>>,
}

impl<T: Clone + Default + 'static> TileLayer<T> {
    fn new() -> Self {
        TileLayer {
            pool: SlotPool::default(),
            rows: Rc::new(VecModel::default()),
            images: Rc::new(VecModel::default()),
            placed: Vec::new(),
        }
    }

    /// Shows `items`; tiles in `arrived` were rendered again and get their new image.
    fn sync(
        &mut self,
        items: &[(Key, Placement)],
        arrived: &[Key],
        image: impl Fn(&Key) -> Image,
        slot: impl Fn(&Placement) -> T,
    ) {
        let mut assigned = Vec::new();
        for change in self.pool.sync(items.iter().map(|(k, _)| *k)) {
            match change {
                SlotChange::Assign(i, k) => {
                    assigned.push(i);
                    if i >= self.rows.row_count() {
                        self.rows.push(T::default());
                        self.images.push(Image::default());
                        self.placed.push(None);
                    }
                    self.images.set_row_data(i, image(&k));
                    self.placed[i] = None;
                }
                SlotChange::Clear(i) => {
                    self.rows.set_row_data(i, T::default());
                    self.images.set_row_data(i, Image::default());
                    self.placed[i] = None;
                }
            }
        }
        for k in arrived {
            if let Some(i) = self.pool.slot_of(k)
                && !assigned.contains(&i)
            {
                self.images.set_row_data(i, image(k));
            }
        }
        for (k, p) in items {
            let Some(i) = self.pool.slot_of(k) else {
                continue;
            };
            if self.placed[i] != Some(*p) {
                self.placed[i] = Some(*p);
                self.rows.set_row_data(i, slot(p));
            }
        }
    }
}

/// A zoom that follows the view size until the user zooms.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Fit {
    /// Fit the width, but not beyond this zoom.
    Width(f64),
    Page,
}

pub struct ViewBridge<S: ViewSink> {
    pub view: PageView<SlintTile>,
    fit: Option<Fit>,
    pages: SlotPool<usize>,
    page_rows: Rc<VecModel<S::Page>>,
    /// Per page slot: whether tiles cover the page (a separate model, so that changing it
    /// leaves the slot and its preview alone).
    page_covered: Rc<VecModel<bool>>,
    /// Per page slot: its placement and whether the preview is set.
    page_placed: Vec<Option<(Placement, bool)>>,
    tiles: TileLayer<S::Tile>,
    fallback: TileLayer<S::Tile>,
    glide: Option<Glide>,
    fling: Option<Fling>,
    drag_from: (f64, f64),
    pinch_from: f64,
    /// The last values given to the UI, to skip unchanged ones.
    shown: (f32, f32, f32, f32),
    pub current_page: usize,
    pub last: Frame,
}

impl<S: ViewSink> std::fmt::Debug for ViewBridge<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewBridge")
            .field("view", &self.view)
            .field("current_page", &self.current_page)
            .finish_non_exhaustive()
    }
}

impl<S: ViewSink> ViewBridge<S> {
    /// A view tuned for desktops.
    pub fn new(
        ui: &S,
        sizes: &[(f64, f64)],
        renderer: Arc<dyn PageRenderer>,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        Self::with_tuning(ui, sizes, renderer, Tuning::desktop(total_memory()), notify)
    }

    pub fn with_tuning(
        ui: &S,
        sizes: &[(f64, f64)],
        renderer: Arc<dyn PageRenderer>,
        tuning: Tuning,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let layout = Layout::new(sizes.iter().map(|&(w, h)| (w as f32, h as f32)));
        let mut view = PageView::with_tuning(layout, renderer, tuning, notify);
        view.viewport.set_dpr(ui.scale_factor());
        let bridge = ViewBridge {
            view,
            fit: None,
            pages: SlotPool::default(),
            page_rows: Rc::new(VecModel::default()),
            page_covered: Rc::new(VecModel::default()),
            page_placed: Vec::new(),
            tiles: TileLayer::new(),
            fallback: TileLayer::new(),
            glide: None,
            fling: None,
            drag_from: (0.0, 0.0),
            pinch_from: 1.0,
            shown: (f32::NAN, f32::NAN, f32::NAN, f32::NAN),
            current_page: 0,
            last: Frame::default(),
        };
        bridge.attach(ui);
        bridge
    }

    /// Shows this view's models in the page view (after switching tabs).
    pub fn attach(&self, ui: &S) {
        ui.show_models(Models {
            pages: ModelRc::from(self.page_rows.clone()),
            covered: ModelRc::from(self.page_covered.clone()),
            tiles: ModelRc::from(self.tiles.rows.clone()),
            tile_images: ModelRc::from(self.tiles.images.clone()),
            fallback: ModelRc::from(self.fallback.rows.clone()),
            fallback_images: ModelRc::from(self.fallback.images.clone()),
        });
        if !self.shown.0.is_nan() {
            let (k, cx, oy, dpr) = self.shown;
            ui.show_transform(k, cx, oy, dpr);
        }
    }

    /// The page under a point of the view and the point in displayed page points.
    pub fn screen_to_page(&self, x: f64, y: f64) -> Option<(usize, f64, f64)> {
        let f = &self.last;
        f.pages.iter().find_map(|&(i, p)| {
            let (x0, y0) = self.corner(&p);
            let (u, v) = ((x - x0) / f.k, (y - y0) / f.k);
            (u >= 0.0 && v >= 0.0 && u <= f64::from(p.w) && v <= f64::from(p.h))
                .then_some((i, u, v))
        })
    }

    /// A point of the view in page `i`'s displayed points, also outside the page; `None`
    /// when the page is not on screen.
    pub fn screen_to_page_of(&self, i: usize, x: f64, y: f64) -> Option<(f64, f64)> {
        let p = self.placement(i)?;
        let (x0, y0) = self.corner(&p);
        Some(((x - x0) / self.last.k, (y - y0) / self.last.k))
    }

    /// Where a point of page `i` (displayed points) is in the view.
    pub fn page_to_screen(&self, i: usize, u: f64, v: f64) -> Option<(f64, f64)> {
        let p = self.placement(i)?;
        let (x0, y0) = self.corner(&p);
        Some((x0 + u * self.last.k, y0 + v * self.last.k))
    }

    /// Page `i`'s placement in the last frame.
    pub fn placement(&self, i: usize) -> Option<Placement> {
        self.last
            .pages
            .iter()
            .find(|(p, _)| *p == i)
            .map(|(_, pl)| *pl)
    }

    /// The top left corner of a placed item in the view.
    fn corner(&self, p: &Placement) -> (f64, f64) {
        let f = &self.last;
        (
            f.cx + f.k * f64::from(p.bx),
            f.oy + GAP * f64::from(p.di) + f.k * f64::from(p.by),
        )
    }

    /// Scrolls so that point `v` (displayed points from the top) of page `i` is `margin`
    /// pixels below the top of the view.
    pub fn glide_to(&mut self, i: usize, v: f64, margin: f64) {
        if i >= self.view.viewport.layout().len() {
            return;
        }
        let paged = self.mode() == LayoutMode::Single;
        if paged && self.view.viewport.current_page() != i {
            self.glide = None;
            self.view.viewport.go_to_page(i);
        }
        let vp = &self.view.viewport;
        let k = vp.k();
        let r = vp.layout().page_rect(i, k, vp.content_size().0);
        let (sx, sy) = vp.scroll();
        let (vw, _) = vp.size();
        // Sideways only when the page is not in view across.
        let dx = if r.x >= sx && r.right() <= sx + vw {
            0.0
        } else {
            r.x - GAP - sx
        };
        self.glide_by(dx, r.y + v * k - margin - sy);
    }

    /// Scrolls to the top (`part` 0), middle (0.5) or bottom (1) of the current page.
    pub fn glide_to_page_part(&mut self, part: f64) {
        let part = part.clamp(0.0, 1.0);
        let vp = &self.view.viewport;
        let i = vp.current_page();
        if i >= vp.layout().len() {
            return;
        }
        let k = vp.k();
        let h = vp.layout().page_rect(i, k, vp.content_size().0).h / k;
        let (_, vh) = vp.size();
        // That point of the page at the same fraction of the view, a gap from its edges.
        self.glide_to(i, h * part, GAP + (vh - 2.0 * GAP).max(0.0) * part);
    }

    pub fn mode(&self) -> LayoutMode {
        self.view.viewport.layout().mode()
    }

    /// Shows the pages in another layout, keeping the current page and the fit.
    pub fn set_mode(&mut self, mode: LayoutMode) {
        if mode == self.mode() {
            return;
        }
        self.relayout(mode, self.view.viewport.layout().r2l());
    }

    /// Spreads and the horizontal strip run right to left (the document's `/Direction`).
    pub fn set_r2l(&mut self, r2l: bool) {
        if r2l != self.view.viewport.layout().r2l() {
            self.relayout(self.mode(), r2l);
        }
    }

    fn relayout(&mut self, mode: LayoutMode, r2l: bool) {
        let page = self.view.viewport.current_page();
        let sizes = self.view.viewport.layout().sizes().to_vec();
        let layout =
            Layout::with_direction(sizes.iter().map(|&(w, h)| (w as f32, h as f32)), mode, r2l);
        self.glide = None;
        self.view.viewport.set_layout(layout, page);
        self.apply_fit();
        self.view.viewport.go_to_page(page);
        if self.fit == Some(Fit::Page) {
            self.apply_fit();
        }
    }

    /// The next or previous page (spread) from the current one.
    pub fn turn_page(&mut self, forward: bool) {
        let vp = &self.view.viewport;
        let l = vp.layout();
        if l.is_empty() {
            return;
        }
        if self.mode() == LayoutMode::Horizontal {
            let i = vp.current_page();
            let target = if forward { i + 1 } else { i.saturating_sub(1) };
            self.glide_to_page(target.min(l.len() - 1));
            return;
        }
        let r = l.row_of(vp.current_page());
        let target = if forward {
            if r + 1 >= l.row_count() {
                return;
            }
            l.row_pages(r + 1).start
        } else {
            match r.checked_sub(1) {
                Some(p) => l.row_pages(p).start,
                None => return,
            }
        };
        self.glide = None;
        self.view.viewport.go_to_page(target);
        if self.fit == Some(Fit::Page) {
            self.apply_fit();
        } else if !forward && self.mode() == LayoutMode::Single {
            // Back to the end of the previous page, as when scrolling up.
            self.view.viewport.scroll_by(0.0, f64::MAX / 4.0);
        }
    }

    /// Ends a smooth scroll at its destination, so the position is where it is going.
    pub fn finish_glide(&mut self) {
        if let Some(g) = self.glide.take() {
            self.view.viewport.scroll_to(g.to.0, g.to.1);
        }
        self.fling = None;
    }

    /// Where point (`u`, `v`) of page `i` (displayed points) is in the view now, in view
    /// pixels; unlike [`ViewBridge::page_to_screen`] this needs no frame drawn since the
    /// view last moved.
    pub fn view_point(&self, i: usize, u: f64, v: f64) -> Option<(f64, f64)> {
        let vp = &self.view.viewport;
        if i >= vp.layout().len() {
            return None;
        }
        let k = vp.k();
        let r = vp.layout().page_rect(i, k, vp.content_size().0);
        let (sx, sy) = vp.scroll();
        Some((r.x + u * k - sx, r.y + v * k - sy))
    }

    /// `by` pages forward (back when negative) from the current one, by spreads in the
    /// two-page layouts, to the top of the page: [`ViewBridge::turn_page`] counted.
    pub fn turn_pages(&mut self, by: i64) {
        let vp = &self.view.viewport;
        let l = vp.layout();
        if l.is_empty() || by == 0 {
            return;
        }
        if self.mode() == LayoutMode::Horizontal {
            let last = l.len() as i64 - 1;
            let target = (vp.current_page() as i64 + by).clamp(0, last) as usize;
            self.glide_to_page(target);
            return;
        }
        let last = l.row_count() as i64 - 1;
        let row = (l.row_of(vp.current_page()) as i64 + by).clamp(0, last) as usize;
        let target = l.row_pages(row).start;
        self.glide = None;
        self.view.viewport.go_to_page(target);
        if self.fit == Some(Fit::Page) {
            self.apply_fit();
        }
    }

    /// Scrolls `by` half screens down (up when negative); a single page turns at its end,
    /// as with Page Down.
    pub fn scroll_half(&mut self, by: i32) {
        if by == 0 {
            return;
        }
        let down = by > 0;
        if self.mode() == LayoutMode::Single && self.view.viewport.at_edge(down) {
            self.turn_page(down);
            return;
        }
        let (_, vh) = self.view.viewport.size();
        self.glide_by(0.0, vh / 2.0 * f64::from(by));
    }

    /// The scroll position as a page and the displayed point at the top of the view.
    pub fn position(&self) -> (usize, f64) {
        let vp = &self.view.viewport;
        let (_, sy) = vp.scroll();
        let k = vp.k();
        let i = vp.current_page();
        let top = vp.layout().page_rect(i, k, vp.content_size().0).y;
        (i, (sy - top) / k)
    }

    pub fn is_fit_width(&self) -> bool {
        matches!(self.fit, Some(Fit::Width(_)))
    }

    /// Clears the page view's models (no document shown).
    pub fn detach(ui: &S) {
        ui.show_models(Models::default());
    }

    pub fn image(&self, k: &Key) -> Option<Image> {
        self.view.tile(k).map(SlintTile::image)
    }

    /// Whether a smooth scroll or a fling needs more frames.
    pub fn animating(&self) -> bool {
        self.glide.is_some() || self.fling.is_some()
    }

    /// Advances a smooth scroll or fling to `now`.
    pub fn step(&mut self, now: Instant) {
        if let Some(g) = self.glide {
            let ((x, y), done) = g.at(now);
            self.view.viewport.scroll_to(x, y);
            if done {
                self.glide = None;
            }
        }
        if let Some(f) = &mut self.fling {
            match f.step(now) {
                Some((dx, dy)) => {
                    let before = self.view.viewport.scroll();
                    self.view.viewport.scroll_by(-dx, -dy);
                    // Stops at the ends of the document.
                    if self.view.viewport.scroll() == before {
                        self.fling = None;
                    }
                }
                None => self.fling = None,
            }
        }
    }

    /// Lets go of a drag moving at `v` pixels per second: the pages glide on and slow down
    /// (or jump the whole way, with reduced motion).
    pub fn fling(&mut self, v: (f64, f64)) {
        self.glide = None;
        let Some(f) = Fling::start(v, Instant::now()) else {
            return;
        };
        if REDUCE_MOTION.load(Ordering::Relaxed) {
            let (dx, dy) = f.distance();
            self.view.viewport.scroll_by(-dx, -dy);
            return;
        }
        self.fling = Some(f);
    }

    /// Recomputes what is visible and updates the UI; returns the previews that arrived.
    pub fn update(&mut self, ui: &S, now: Instant) -> Vec<usize> {
        self.update_keys(ui, now)
            .iter()
            .filter(|k| k.is_preview())
            .map(|k| k.page as usize)
            .collect()
    }

    /// As [`Self::update`], returning every tile that arrived.
    pub fn update_keys(&mut self, ui: &S, now: Instant) -> Vec<Key> {
        let (frame, arrived) = self.view.update(now);
        let dpr = ui.scale_factor();
        if dpr != self.view.viewport.dpr() {
            self.view.viewport.set_dpr(dpr);
        }
        let shown = (frame.k as f32, frame.cx as f32, frame.oy as f32, dpr as f32);
        if shown != self.shown {
            ui.show_transform(shown.0, shown.1, shown.2, shown.3);
            self.shown = shown;
        }
        self.sync_pages(&frame, &arrived);
        let view = &self.view;
        let image = |k: &Key| view.tile(k).map(SlintTile::image).unwrap_or_default();
        self.fallback
            .sync(&frame.fallback, &arrived, image, S::tile_slot);
        self.tiles.sync(&frame.tiles, &arrived, image, S::tile_slot);
        self.update_scrollbars(ui);
        self.current_page = self.view.viewport.current_page();
        self.last = frame;
        arrived
    }

    fn sync_pages(&mut self, frame: &Frame, arrived: &[Key]) {
        for change in self.pages.sync(frame.pages.iter().map(|(i, _)| *i)) {
            match change {
                SlotChange::Assign(slot, _) if slot >= self.page_rows.row_count() => {
                    self.page_rows.push(S::Page::default());
                    self.page_covered.push(false);
                    self.page_placed.push(None);
                }
                SlotChange::Assign(slot, _) => self.page_placed[slot] = None,
                SlotChange::Clear(slot) => {
                    self.page_rows.set_row_data(slot, S::Page::default());
                    self.page_placed[slot] = None;
                }
            }
        }
        for (&(page, p), &covered) in frame.pages.iter().zip(&frame.covered) {
            let Some(slot) = self.pages.slot_of(&page) else {
                continue;
            };
            if self.page_covered.row_data(slot) != Some(covered) {
                self.page_covered.set_row_data(slot, covered);
            }
            let preview = self.image(&Key::preview(page));
            let has_preview = preview.is_some();
            let fresh = arrived.contains(&Key::preview(page));
            if self.page_placed[slot] == Some((p, has_preview)) && !fresh {
                continue;
            }
            self.page_placed[slot] = Some((p, has_preview));
            self.page_rows.set_row_data(
                slot,
                S::page_slot(&p, page as i32 + 1, preview.unwrap_or_default()),
            );
        }
    }

    fn update_scrollbars(&self, ui: &S) {
        let vp = &self.view.viewport;
        let b = vp.bounds();
        let (vw, vh) = vp.size();
        let (sx, sy) = vp.scroll();
        ui.show_scrollbars(
            ((sy - b.y) / b.h) as f32,
            (vh / b.h).min(1.0) as f32,
            ((sx - b.x) / b.w) as f32,
            (vw / b.w).min(1.0) as f32,
        );
    }

    pub fn resized(&mut self, w: f64, h: f64) {
        self.view.viewport.set_size(w, h);
        self.apply_fit();
    }

    fn apply_fit(&mut self) {
        let vp = &mut self.view.viewport;
        match self.fit {
            Some(Fit::Width(max)) => {
                vp.fit_width();
                if vp.zoom() > max {
                    let (w, _) = vp.size();
                    let (_, y) = vp.scroll();
                    vp.zoom_at(max, w / 2.0, 0.0);
                    if y <= 0.0 {
                        vp.scroll_to(vp.scroll().0, 0.0);
                    }
                }
            }
            Some(Fit::Page) => vp.fit_page(),
            None => {}
        }
    }

    /// A wheel or trackpad scroll; Ctrl zooms around the pointer.
    pub fn wheel(&mut self, dx: f64, dy: f64, x: f64, y: f64, ctrl: bool, shift: bool) {
        self.fling = None;
        if ctrl {
            self.fit = None;
            let z = self.view.viewport.zoom() * 1.1f64.powf(dy / WHEEL_LINE);
            self.glide = None;
            self.view.zoom_gesture(Instant::now());
            self.view.viewport.zoom_at(z, x, y);
            return;
        }
        // Slint reports scrolling content up as positive; we move the view down. The
        // horizontal strip scrolls sideways with a plain wheel.
        let sideways = self.mode() == LayoutMode::Horizontal;
        let (dx, dy) = if (shift || sideways) && dx == 0.0 {
            (-dy, 0.0)
        } else {
            (-dx, -dy)
        };
        if self.mode() == LayoutMode::Single && dy != 0.0 && self.glide.is_none() {
            let vp = &self.view.viewport;
            if vp.at_edge(dy > 0.0) {
                self.turn_page(dy > 0.0);
                return;
            }
        }
        let line = |d: f64| d != 0.0 && (d / WHEEL_LINE).fract() == 0.0;
        if line(dx) || line(dy) {
            self.glide_by(dx, dy);
        } else {
            self.glide = None;
            self.view.viewport.scroll_by(dx, dy);
        }
    }

    /// Scrolls smoothly, adding to a scroll in progress.
    pub fn glide_by(&mut self, dx: f64, dy: f64) {
        self.fling = None;
        let now = Instant::now();
        let from = self.view.viewport.scroll();
        let base = self.glide.map_or(from, |g| g.to);
        // Where the viewport would clamp to.
        let mut probe = self.view.viewport.clone();
        probe.scroll_to(base.0 + dx, base.1 + dy);
        let to = probe.scroll();
        if REDUCE_MOTION.load(Ordering::Relaxed) {
            self.glide = None;
            self.view.viewport.scroll_to(to.0, to.1);
            return;
        }
        self.glide = Some(Glide {
            from,
            to,
            start: now,
            duration: SMOOTH_SCROLL,
        });
    }

    pub fn glide_to_page(&mut self, page: usize) {
        if self.mode() == LayoutMode::Single {
            self.glide = None;
            self.view.viewport.go_to_page(page);
            if self.fit == Some(Fit::Page) {
                self.apply_fit();
            }
            return;
        }
        let mut probe = self.view.viewport.clone();
        probe.go_to_page(page);
        let (x, y) = probe.scroll();
        let (sx, sy) = self.view.viewport.scroll();
        self.glide_by(x - sx, y - sy);
    }

    pub fn drag_start(&mut self) {
        self.glide = None;
        self.fling = None;
        self.drag_from = self.view.viewport.scroll();
    }

    pub fn drag(&mut self, dx: f64, dy: f64) {
        let (x, y) = self.drag_from;
        self.view.viewport.scroll_to(x - dx, y - dy);
    }

    pub fn pinch_start(&mut self) {
        self.glide = None;
        self.fling = None;
        self.fit = None;
        self.pinch_from = self.view.viewport.zoom();
    }

    pub fn pinch(&mut self, scale: f64, x: f64, y: f64) {
        self.view.zoom_gesture(Instant::now());
        self.view.viewport.zoom_at(self.pinch_from * scale, x, y);
    }

    pub fn scrollbar(&mut self, vertical: bool, start: f64) {
        let b = self.view.viewport.bounds();
        let (sx, sy) = self.view.viewport.scroll();
        self.glide = None;
        if vertical {
            self.view.viewport.scroll_to(sx, b.y + start * b.h);
        } else {
            self.view.viewport.scroll_to(b.x + start * b.w, sy);
        }
    }

    /// A discrete zoom step (buttons, keys): tiles of the new bucket are wanted at once.
    pub fn zoom_to(&mut self, z: f64) {
        self.glide = None;
        self.fling = None;
        self.fit = None;
        self.view.viewport.zoom_to(z);
    }

    pub fn zoom_in(&mut self) {
        self.zoom_to(zoom::step_in(self.view.viewport.zoom()));
    }

    pub fn zoom_out(&mut self) {
        self.zoom_to(zoom::step_out(self.view.viewport.zoom()));
    }

    /// Fits the width now and whenever the view is resized, up to zoom `max`.
    pub fn fit_width(&mut self, max: f64) {
        self.glide = None;
        self.fit = Some(Fit::Width(max));
        self.apply_fit();
    }

    pub fn fit_page(&mut self) {
        self.glide = None;
        self.fit = Some(Fit::Page);
        self.apply_fit();
    }

    /// Scrolling keys; returns whether the key was one.
    pub fn key(&mut self, key: char) -> bool {
        use slint::platform::Key;
        let is = |k: Key| key == char::from(k);
        let mode = self.mode();
        let fits_across = {
            let b = self.view.viewport.bounds();
            b.w <= self.view.viewport.size().0 + 0.5
        };
        // Page turns: the strip pages sideways; the single page turns at its ends, and
        // sideways when the page fits across.
        let turn = match mode {
            LayoutMode::Horizontal if is(Key::RightArrow) || is(Key::PageDown) || key == ' ' => {
                Some(true)
            }
            LayoutMode::Horizontal if is(Key::LeftArrow) || is(Key::PageUp) => Some(false),
            LayoutMode::Single | LayoutMode::TwoPage | LayoutMode::Book
                if fits_across && is(Key::RightArrow) =>
            {
                Some(true)
            }
            LayoutMode::Single | LayoutMode::TwoPage | LayoutMode::Book
                if fits_across && is(Key::LeftArrow) =>
            {
                Some(false)
            }
            LayoutMode::Single
                if (is(Key::PageDown) || is(Key::DownArrow) || key == ' ')
                    && self.view.viewport.at_edge(true) =>
            {
                Some(true)
            }
            LayoutMode::Single
                if (is(Key::PageUp) || is(Key::UpArrow)) && self.view.viewport.at_edge(false) =>
            {
                Some(false)
            }
            _ => None,
        };
        if let Some(forward) = turn {
            self.turn_page(forward);
            return true;
        }
        if mode == LayoutMode::Single && (is(Key::Home) || is(Key::End)) {
            let n = self.view.viewport.layout().len();
            self.glide = None;
            self.view.viewport.go_to_page(if is(Key::Home) {
                0
            } else {
                n.saturating_sub(1)
            });
            return true;
        }
        let (_, vh) = self.view.viewport.size();
        let page = (vh - 48.0).max(vh * 0.5);
        let (cw, ch) = self.view.viewport.content_size();
        if mode == LayoutMode::Horizontal && (is(Key::Home) || is(Key::End)) {
            self.glide_by(if is(Key::Home) { -cw } else { cw }, 0.0);
            return true;
        }
        let (dx, dy) = match key {
            k if k == char::from(Key::DownArrow) => (0.0, ARROW_STEP),
            k if k == char::from(Key::UpArrow) => (0.0, -ARROW_STEP),
            k if k == char::from(Key::RightArrow) => (ARROW_STEP, 0.0),
            k if k == char::from(Key::LeftArrow) => (-ARROW_STEP, 0.0),
            k if k == char::from(Key::PageDown) || k == ' ' => (0.0, page),
            k if k == char::from(Key::PageUp) => (0.0, -page),
            k if k == char::from(Key::Home) => (0.0, -ch),
            k if k == char::from(Key::End) => (0.0, ch),
            _ => return false,
        };
        self.glide_by(dx, dy);
        true
    }
}
