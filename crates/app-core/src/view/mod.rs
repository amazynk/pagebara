//! The continuous page view: which pages and tiles are on screen, where, and which tiles
//! to render next. UI-agnostic; the shell turns a [`Frame`] into widgets.
//!
//! Positions are relative to an anchor page so they stay small (and exact in `f32`) in
//! long documents: on screen, an item is at
//! `x = cx + k * bx`, `y = oy + GAP * di + k * by`, sized `k * w` × `k * h`, where
//! `(cx, oy)` is the anchor page's top left corner and `di` counts rows.

pub mod form_factor;
pub mod gesture;
pub mod layout;
mod slots;
mod tuning;
pub mod viewport;
pub mod zoom;

use std::collections::HashSet;
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant};

pub use layout::{GAP, Layout, LayoutMode, PX_PER_PT, Rect};
pub use slots::{SlotChange, SlotPool};
pub use tuning::Tuning;
pub use viewport::Viewport;

use crate::tiles::{Grid, Job, Key, PREVIEW_LEVEL, PageRenderer, Scheduler, TileBuffer, TileCache};

/// How long after the last zoom step tiles of the new bucket are requested.
const SETTLE: Duration = Duration::from_millis(150);
/// Fallback tiles may be at most this many buckets finer than the current one.
const FALLBACK_FINER: i16 = 32;
/// The frame budget at 120 Hz: uploads are spread over frames this long.
const FRAME: Duration = Duration::from_micros(8_333);
/// The view counts as moving this long after it last scrolled or zoomed.
const MOVING_FOR: Duration = Duration::from_millis(120);

/// Where an item sits relative to the anchor page (see the module docs).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Placement {
    pub di: i32,
    pub by: f32,
    pub bx: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Default)]
pub struct Frame {
    /// Logical pixels per point.
    pub k: f64,
    /// Screen x of the pages' centre line.
    pub cx: f64,
    /// Screen y of the anchor page's top.
    pub oy: f64,
    pub pages: Vec<(usize, Placement)>,
    /// Per page of `pages`: whether tiles (of the current bucket or standing in) cover its
    /// visible part, so its paper and preview need no drawing.
    pub covered: Vec<bool>,
    /// Cached tiles at the current bucket.
    pub tiles: Vec<(Key, Placement)>,
    /// Cached tiles of other buckets standing in for missing ones, coarsest first.
    pub fallback: Vec<(Key, Placement)>,
    /// Tiles at the current bucket not shown yet (not cached, or deferred).
    pub missing: usize,
    /// Cached tiles held back to the next frames, as too many appeared at once.
    pub deferred: usize,
    /// Update again at this time (when a zoom gesture settles, or for deferred tiles).
    pub wake_at: Option<Instant>,
}

pub struct PageView<B> {
    pub viewport: Viewport,
    cache: TileCache<B>,
    sched: Scheduler<B>,
    tuning: Tuning,
    anchor: usize,
    settle_at: Option<Instant>,
    /// Extra previews wanted, e.g. for the visible thumbnails.
    previews: Range<usize>,
    /// Tiles in the last frame: their textures are already on the GPU.
    shown: HashSet<Key>,
    /// New tiles that may still appear while moving, topped up every [`FRAME`].
    uploads: f64,
    uploads_at: Option<Instant>,
    /// Scroll position and scale at the last update, and when they last changed.
    last_pos: (f64, f64, f64),
    last_pages: Range<usize>,
    moved_at: Option<Instant>,
    /// A whole screen of tiles was shown once: until then nothing is held back.
    filled: bool,
}

impl<B> std::fmt::Debug for PageView<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PageView")
            .field("viewport", &self.viewport)
            .field("cached", &self.cache.len())
            .finish_non_exhaustive()
    }
}

impl<B: TileBuffer> PageView<B> {
    /// `notify` is called from a render thread when finished tiles are waiting.
    pub fn new(
        layout: Layout,
        renderer: Arc<dyn PageRenderer>,
        cache_bytes: usize,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        let tuning = Tuning {
            cache_bytes,
            ..Tuning::default()
        };
        Self::with_tuning(layout, renderer, tuning, notify)
    }

    pub fn with_tuning(
        layout: Layout,
        renderer: Arc<dyn PageRenderer>,
        tuning: Tuning,
        notify: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        PageView {
            viewport: Viewport::new(layout),
            cache: TileCache::new(tuning.cache_bytes),
            tuning,
            sched: Scheduler::new(crate::tiles::render_pool(), renderer, notify),
            anchor: 0,
            settle_at: None,
            previews: 0..0,
            shown: HashSet::new(),
            uploads: 0.0,
            uploads_at: None,
            last_pos: (f64::NAN, 0.0, 0.0),
            last_pages: 0..0,
            moved_at: None,
            filled: false,
        }
    }

    pub fn tile(&self, k: &Key) -> Option<&B> {
        self.cache.get(k)
    }

    /// Defers tiles of a new zoom bucket during continuous zooming (wheel, pinch).
    pub fn zoom_gesture(&mut self, now: Instant) {
        self.settle_at = Some(now + SETTLE);
    }

    /// Renders with `renderer` from now on (the document changed). Tiles of `pages` (all
    /// when `None`) are rendered again; the old ones stay shown until then.
    pub fn set_renderer(&mut self, renderer: Arc<dyn PageRenderer>, pages: Option<&[usize]>) {
        self.sched.set_renderer(renderer);
        self.cache.mark_stale(pages);
    }

    pub fn renderer(&self) -> &Arc<dyn PageRenderer> {
        self.sched.renderer()
    }

    pub fn want_previews(&mut self, pages: Range<usize>) {
        self.previews = pages;
    }

    /// Tiles rendered, mean render time, jobs running, tiles cached and their bytes.
    pub fn stats(&self) -> (u64, Duration, usize, usize, usize) {
        let (n, mean) = self.sched.rendered();
        (
            n,
            mean,
            self.sched.in_flight(),
            self.cache.len(),
            self.cache.bytes(),
        )
    }

    /// Takes finished tiles and computes what to show; returns the keys that arrived.
    pub fn update(&mut self, now: Instant) -> (Frame, Vec<Key>) {
        let done = self.sched.drain();
        let mut arrived = Vec::with_capacity(done.len());
        for (key, buf) in done {
            let bytes = self.grid(&key).map_or(0, |g| {
                let r = g.tile_rect(key.x.into(), key.y.into());
                (r.w * r.h * 4) as usize
            });
            self.cache.insert(key, buf, bytes);
            arrived.push(key);
        }
        self.cache.next_round();
        let settled = self.settle_at.is_none_or(|t| now >= t);
        if settled {
            self.settle_at = None;
        }
        let budget = self.upload_budget(now);
        let frame = self.compute(settled, budget, now);
        self.cache.evict();
        (frame, arrived)
    }

    /// How many tiles not shown before may appear now: unlimited at rest or after a jump, a
    /// few per frame while scrolling or zooming, so a frame does not stall on texture uploads.
    fn upload_budget(&mut self, now: Instant) -> Option<usize> {
        let (sx, sy) = self.viewport.scroll();
        let (vw, vh) = self.viewport.size();
        let pos = (sx, sy, self.viewport.k());
        let last = self.last_pos;
        // A jump (page change, go to) leaves nothing of the last screen to look at, so
        // holding tiles back would only show the edges late.
        let pages = self.viewport.visible_pages();
        let jumped = pos.2 == last.2
            && ((sx - last.0).abs() >= vw / 2.0
                || (sy - last.1).abs() >= vh / 2.0
                || pages.end <= self.last_pages.start
                || pages.start >= self.last_pages.end);
        self.last_pages = pages;
        if pos != last {
            self.last_pos = pos;
            self.moved_at = Some(now);
        }
        if jumped {
            return None;
        }
        let cap = self.tuning.uploads_per_frame as f64;
        let elapsed = self
            .uploads_at
            .map_or(FRAME, |t| now.saturating_duration_since(t));
        self.uploads = (self.uploads + cap * elapsed.as_secs_f64() / FRAME.as_secs_f64()).min(cap);
        self.uploads_at = Some(now);
        let moving = self.moved_at.is_some_and(|t| now < t + MOVING_FOR);
        // The first screen appears at once, however the view settles in.
        (self.filled && moving && cap > 0.0).then_some(self.uploads.max(0.0) as usize)
    }

    fn grid(&self, k: &Key) -> Option<Grid> {
        let layout = self.viewport.layout();
        let i = k.page as usize;
        (i < layout.len()).then(|| {
            let (w, h) = layout.size(i);
            if k.is_preview() {
                Grid::preview(w, h)
            } else {
                Grid::with_tile(w, h, k.level, self.tuning.tile)
            }
        })
    }

    fn compute(&mut self, settled: bool, mut budget: Option<usize>, now: Instant) -> Frame {
        let vp = &self.viewport;
        let layout = vp.layout().clone();
        let k = vp.k();
        let level = zoom::bucket(k * vp.dpr());
        let view = vp.visible();
        let (cw, _) = vp.content_size();
        let pages = vp.visible_pages();
        if !pages.contains(&self.anchor) {
            self.anchor = pages.start.min(layout.len().saturating_sub(1));
        }
        let anchor = self.anchor;
        let place = |i: usize, [x, y, w, h]: [f64; 4]| {
            let (di, dy, dx) = layout.offset(anchor, i);
            Placement {
                di,
                by: (dy + y) as f32,
                bx: (dx + x) as f32,
                w: w as f32,
                h: h as f32,
            }
        };
        let (cx, oy) = if layout.is_empty() {
            (cw / 2.0 - view.x, 0.0)
        } else {
            let r = layout.page_rect(anchor, k, cw);
            (r.x - view.x, r.y - view.y)
        };
        let mut frame = Frame {
            k,
            cx,
            oy,
            wake_at: self.settle_at,
            ..Frame::default()
        };
        let center = (view.x + view.w / 2.0, view.y + view.h / 2.0);
        let mut visible_jobs: Vec<(f64, Job)> = Vec::new();
        let mut preview_jobs = Vec::new();
        let mut fallback = HashSet::new();
        // Per visible page, the areas without a tile in this frame.
        let mut holes: Vec<(usize, Vec<[f64; 4]>)> = Vec::new();
        // Cached tiles not shown before, while their number is limited: distance from the
        // centre, page, key, tile and its visible part.
        type Fresh = (f64, usize, Key, [f64; 4], [f64; 4]);
        let mut fresh: Vec<Fresh> = Vec::new();
        for i in pages.clone() {
            let (pw, ph) = layout.size(i);
            frame.pages.push((i, place(i, [0.0, 0.0, pw, ph])));
            let preview = Key::preview(i);
            if !self.cache.touch(&preview) || self.cache.is_stale(&preview) {
                preview_jobs.push(self.job(preview));
            }
            let r = layout.page_rect(i, k, cw);
            let Some(vis) = r.intersection(&view) else {
                // Nothing of it to draw.
                holes.push((i, Vec::new()));
                continue;
            };
            let to_pt = |v: Rect| [(v.x - r.x) / k, (v.y - r.y) / k, v.w / k, v.h / k];
            let grid = Grid::with_tile(pw, ph, level, self.tuning.tile);
            let shown = to_pt(vis);
            let (xs, ys) = grid.tiles_in(shown);
            let mut missing = Vec::new();
            for ty in ys {
                for tx in xs.clone() {
                    let key = grid.key(i, tx, ty);
                    let pt = grid.tile_pt(tx, ty);
                    let cached = self.cache.touch(&key);
                    let c = (
                        r.x + (pt[0] + pt[2] / 2.0) * k,
                        r.y + (pt[1] + pt[3] / 2.0) * k,
                    );
                    let d = (c.0 - center.0).hypot(c.1 - center.1);
                    if !cached {
                        missing.push(clip(pt, shown));
                    } else if budget.is_none() || self.shown.contains(&key) {
                        frame.tiles.push((key, place(i, pt)));
                    } else {
                        fresh.push((d, i, key, pt, clip(pt, shown)));
                    }
                    if !cached || self.cache.is_stale(&key) {
                        visible_jobs.push((d, self.job(key)));
                    }
                }
            }
            holes.push((i, missing));
        }
        // The nearest new tiles appear now, the rest in the next frames; meanwhile coarser
        // tiles or the page preview stand in for them.
        if let Some(b) = &mut budget {
            fresh.sort_by(|a, b| a.0.total_cmp(&b.0));
            let admitted = fresh.len().min(*b);
            for (n, (_, i, key, pt, area)) in fresh.into_iter().enumerate() {
                if n < admitted {
                    frame.tiles.push((key, place(i, pt)));
                } else if let Some((_, m)) = holes.iter_mut().find(|(p, _)| *p == i) {
                    m.push(area);
                    frame.deferred += 1;
                }
            }
            *b -= admitted;
        }
        let mut covered = HashSet::new();
        for (i, missing) in &holes {
            frame.missing += missing.len();
            if self.fallbacks(*i, level, missing, !settled, &mut fallback) {
                covered.insert(*i);
            }
        }
        let mut fallback: Vec<Key> = fallback.into_iter().collect();
        fallback.sort_unstable_by_key(|k| (k.level, k.page, k.y, k.x));
        // New fallbacks are uploads too: coarsest first, within what is left.
        if let Some(b) = &mut budget {
            fallback.retain(|k| {
                if self.shown.contains(k) {
                    return true;
                }
                let keep = *b > 0;
                *b = b.saturating_sub(1);
                if !keep {
                    covered.remove(&(k.page as usize));
                }
                keep
            });
        }
        frame.covered = frame
            .pages
            .iter()
            .map(|(i, _)| covered.contains(i))
            .collect();
        if frame.deferred > 0 {
            let next = now + FRAME;
            frame.wake_at = Some(frame.wake_at.map_or(next, |t| t.min(next)));
        }
        frame.fallback = fallback
            .into_iter()
            .filter_map(|key| {
                let g = self.grid(&key)?;
                Some((
                    key,
                    place(key.page as usize, g.tile_pt(key.x.into(), key.y.into())),
                ))
            })
            .collect();

        let mut jobs = preview_jobs;
        if settled {
            visible_jobs.sort_by(|a, b| a.0.total_cmp(&b.0));
            jobs.extend(visible_jobs.into_iter().map(|(_, j)| j));
            jobs.extend(self.prefetch(level, &view, cw));
        }
        let ahead = self.tuning.preview_ahead;
        let near = pages.start.saturating_sub(ahead)..(pages.end + ahead).min(layout.len());
        let extra = self.previews.start.min(layout.len())..self.previews.end.min(layout.len());
        for i in near.chain(extra) {
            let key = Key::preview(i);
            self.cache.touch(&key);
            jobs.push(self.job(key));
        }
        let cache = &self.cache;
        self.sched.want(&jobs, |k| cache.is_fresh(k));
        if budget.is_some() {
            let used = frame
                .tiles
                .iter()
                .chain(&frame.fallback)
                .filter(|(k, _)| !self.shown.contains(k))
                .count();
            self.uploads -= used as f64;
        }
        self.filled |= frame.missing == 0 && !frame.tiles.is_empty();
        self.shown.clear();
        self.shown
            .extend(frame.tiles.iter().chain(&frame.fallback).map(|(k, _)| *k));
        frame
    }

    /// Cached tiles of other buckets standing in for the `missing` areas of page `i`, up to
    /// the nearest bucket covering an area whole, with whatever nearer buckets have drawn
    /// over it (sharper while the current tiles load). During a zoom `gesture` only the
    /// covering bucket is drawn: one layer, as a software renderer is fill bound. Returns
    /// whether every area is covered.
    fn fallbacks(
        &mut self,
        i: usize,
        level: i16,
        missing: &[[f64; 4]],
        gesture: bool,
        out: &mut HashSet<Key>,
    ) -> bool {
        if missing.is_empty() {
            return true;
        }
        let mut levels: Vec<i16> = self
            .cache
            .levels(i as u32)
            .filter(|&l| l != level && l != PREVIEW_LEVEL && l <= level + FALLBACK_FINER)
            .collect();
        // Nearest first; on a tie the sharper one.
        levels.sort_by_key(|&l| ((l - level).abs(), l < level));
        let (w, h) = self.viewport.layout().size(i);
        let tile = self.tuning.tile;
        let grids: Vec<Grid> = levels
            .iter()
            .map(|&l| Grid::with_tile(w, h, l, tile))
            .collect();
        let keys = |g: Grid, pt: [f64; 4]| {
            let (xs, ys) = g.tiles_in(pt);
            ys.flat_map(move |ty| xs.clone().map(move |tx| g.key(i, tx, ty)))
        };
        let mut all = true;
        for &pt in missing {
            let whole = grids
                .iter()
                .position(|&g| keys(g, pt).all(|k| self.cache.get(&k).is_some()));
            let layers = match whole {
                Some(w) if gesture => &grids[w..=w],
                Some(w) => &grids[..=w],
                None => {
                    all = false;
                    &grids[..]
                }
            };
            for &g in layers {
                for key in keys(g, pt) {
                    if self.cache.touch(&key) {
                        out.insert(key);
                    }
                }
            }
        }
        all
    }

    /// Tiles just outside the view, above and below.
    fn prefetch(&mut self, level: i16, view: &Rect, cw: f64) -> Vec<Job> {
        let layout = self.viewport.layout().clone();
        let k = self.viewport.k();
        let ahead = if layout.mode() == LayoutMode::Horizontal {
            view.inflate(view.w * self.tuning.prefetch, 0.0)
        } else {
            view.inflate(0.0, view.h * self.tuning.prefetch)
        };
        let mut jobs = Vec::new();
        for i in layout.pages_in(&ahead, k, cw) {
            let (pw, ph) = layout.size(i);
            let r = layout.page_rect(i, k, cw);
            let Some(area) = r.intersection(&ahead) else {
                continue;
            };
            let grid = Grid::with_tile(pw, ph, level, self.tuning.tile);
            let to_pt = |v: Rect| [(v.x - r.x) / k, (v.y - r.y) / k, v.w / k, v.h / k];
            let (xs, ys) = grid.tiles_in(to_pt(area));
            for ty in ys {
                for tx in xs.clone() {
                    let key = grid.key(i, tx, ty);
                    if !self.cache.touch(&key) || self.cache.is_stale(&key) {
                        jobs.push(self.job(key));
                    }
                }
            }
        }
        jobs
    }

    fn job(&self, key: Key) -> Job {
        let grid = self.grid(&key).expect("key of a page in the layout");
        let page = key.page as usize;
        Job {
            key,
            page,
            page_size: self.viewport.layout().size(page),
            scale: grid.scale,
            rect: grid.tile_rect(key.x.into(), key.y.into()),
        }
    }
}

/// The part of rectangle `a` inside `b` (both `[x, y, w, h]`).
fn clip(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let x = a[0].max(b[0]);
    let y = a[1].max(b[1]);
    let w = (a[0] + a[2]).min(b[0] + b[2]) - x;
    let h = (a[1] + a[3]).min(b[1] + b[3]) - y;
    [x, y, w.max(0.0), h.max(0.0)]
}

#[cfg(test)]
mod tests;
