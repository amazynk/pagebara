//! Page tiles: square pieces of a page rendered at a zoom bucket, cached and produced on a
//! background pool.

mod backend;
mod cache;
mod placeholder;
mod scheduler;

pub use backend::{CompleteBackend, EngineBackend, EngineRenderer, RenderBackend, RenderSource};
pub use cache::TileCache;
pub use placeholder::PlaceholderRenderer;
pub use scheduler::{Job, PageRenderer, Scheduler, TileBuffer, render_pool};

use crate::view::zoom;

/// Tile edge in device pixels (the default; see [`crate::view::Tuning`]).
pub const TILE: u32 = 256;
/// Longest edge of a page preview in device pixels.
pub const PREVIEW: u32 = 320;
/// The bucket of page previews (whole pages in one small image).
pub const PREVIEW_LEVEL: i16 = i16::MIN;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key {
    pub page: u32,
    /// Zoom bucket, or [`PREVIEW_LEVEL`].
    pub level: i16,
    pub x: u16,
    pub y: u16,
}

impl Key {
    pub fn preview(page: usize) -> Key {
        Key {
            page: page as u32,
            level: PREVIEW_LEVEL,
            x: 0,
            y: 0,
        }
    }

    pub fn is_preview(&self) -> bool {
        self.level == PREVIEW_LEVEL
    }
}

/// Device pixels of a page at some scale.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PixelRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

/// How a page of `w` × `h` points splits into tiles at one bucket.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub level: i16,
    /// Device pixels per point.
    pub scale: f64,
    pub w_px: u32,
    pub h_px: u32,
    /// Tile edge in device pixels (the whole preview for previews).
    tile: u32,
}

impl Grid {
    pub fn new(w_pt: f64, h_pt: f64, level: i16) -> Grid {
        Grid::with_tile(w_pt, h_pt, level, TILE)
    }

    /// A grid of `tile`-pixel tiles.
    pub fn with_tile(w_pt: f64, h_pt: f64, level: i16, tile: u32) -> Grid {
        let scale = zoom::bucket_scale(level);
        Grid::with_scale(w_pt, h_pt, level, scale, tile.max(16))
    }

    /// The grid of the page preview: one image of at most [`PREVIEW`] pixels.
    pub fn preview(w_pt: f64, h_pt: f64) -> Grid {
        let scale = f64::from(PREVIEW) / w_pt.max(h_pt);
        let mut g = Grid::with_scale(w_pt, h_pt, PREVIEW_LEVEL, scale, PREVIEW);
        g.tile = g.w_px.max(g.h_px);
        g
    }

    fn with_scale(w_pt: f64, h_pt: f64, level: i16, scale: f64, tile: u32) -> Grid {
        let px = |pt: f64| ((pt * scale).ceil() as u32).max(1);
        Grid {
            level,
            scale,
            w_px: px(w_pt),
            h_px: px(h_pt),
            tile,
        }
    }

    pub fn cols(&self) -> u32 {
        self.w_px.div_ceil(self.tile())
    }

    pub fn rows(&self) -> u32 {
        self.h_px.div_ceil(self.tile())
    }

    fn tile(&self) -> u32 {
        self.tile
    }

    pub fn tile_rect(&self, x: u32, y: u32) -> PixelRect {
        let t = self.tile();
        let (x0, y0) = (x * t, y * t);
        PixelRect {
            x: x0,
            y: y0,
            w: t.min(self.w_px - x0),
            h: t.min(self.h_px - y0),
        }
    }

    /// The tile's area in points: x, y, w, h from the page's top-left.
    pub fn tile_pt(&self, x: u32, y: u32) -> [f64; 4] {
        let r = self.tile_rect(x, y);
        let s = self.scale;
        [
            f64::from(r.x) / s,
            f64::from(r.y) / s,
            f64::from(r.w) / s,
            f64::from(r.h) / s,
        ]
    }

    /// Columns and rows of the tiles overlapping `[x, y, w, h]` (points, page top-left).
    pub fn tiles_in(&self, [x, y, w, h]: [f64; 4]) -> (std::ops::Range<u32>, std::ops::Range<u32>) {
        let t = f64::from(self.tile()) / self.scale;
        let span = |a: f64, len: f64, n: u32| {
            let lo = (a / t).floor().max(0.0) as u32;
            let hi = (((a + len) / t).ceil().max(0.0) as u32).min(n);
            lo.min(hi)..hi
        };
        (span(x, w, self.cols()), span(y, h, self.rows()))
    }

    pub fn key(&self, page: usize, x: u32, y: u32) -> Key {
        Key {
            page: page as u32,
            level: self.level,
            x: x as u16,
            y: y as u16,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests;
