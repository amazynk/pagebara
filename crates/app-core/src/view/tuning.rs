//! How the page view spends memory and work: tile size, how far ahead it renders, and the
//! tile cache budget. Desktops and phones differ in screen density, scrolling speed and
//! memory.

use crate::tiles::TILE;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tuning {
    /// Tile edge in device pixels.
    pub tile: u32,
    /// Tiles this far beyond the view (in view heights or widths) are rendered ahead.
    pub prefetch: f64,
    /// Page previews rendered around the visible pages.
    pub preview_ahead: usize,
    /// Bytes of rendered tiles kept.
    pub cache_bytes: usize,
    /// While the view moves, at most this many tiles appear per frame (each is a texture
    /// upload on the UI thread); the rest follow in the next frames.
    pub uploads_per_frame: usize,
}

const MIB: u64 = 1 << 20;

/// `total / div`, clamped to `lo..=hi` MiB.
fn share(total: u64, div: u64, lo: u64, hi: u64) -> usize {
    usize::try_from((total / div).clamp(lo * MIB, hi * MIB)).unwrap_or(usize::MAX)
}

impl Tuning {
    /// For a machine with `total` bytes of memory: a sixty-fourth of it for tiles, between
    /// 48 and 256 MiB (at 1440×900, a screen of tiles takes about 5 MiB).
    pub fn desktop(total: u64) -> Tuning {
        Tuning {
            tile: TILE,
            prefetch: 0.5,
            preview_ahead: 2,
            cache_bytes: share(total, 64, 48, 256),
            uploads_per_frame: 4,
        }
    }

    /// Phones and tablets: at 2–3.5 device pixels per point, 256-pixel tiles would be
    /// small and many, and a fling crosses a screen in a fraction of a second. Apps using
    /// much memory get killed, so the cache is a ninety-sixth of the memory, between 32
    /// and 128 MiB (a 1080×2400 screen of tiles is about 10 MiB).
    pub fn mobile(total: u64) -> Tuning {
        Tuning {
            tile: 512,
            prefetch: 1.0,
            preview_ahead: 3,
            cache_bytes: share(total, 96, 32, 128),
            // Four times the pixels of a desktop tile.
            uploads_per_frame: 1,
        }
    }
}

impl Default for Tuning {
    fn default() -> Self {
        Tuning::desktop(8 << 30)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_scale_with_memory() {
        const GIB: u64 = 1 << 30;
        assert_eq!(Tuning::desktop(2 * GIB).cache_bytes, 48 << 20);
        assert_eq!(Tuning::desktop(8 * GIB).cache_bytes, 128 << 20);
        assert_eq!(Tuning::desktop(64 * GIB).cache_bytes, 256 << 20);
        assert_eq!(Tuning::mobile(2 * GIB).cache_bytes, 32 << 20);
        assert_eq!(Tuning::mobile(6 * GIB).cache_bytes, 64 << 20);
        assert_eq!(Tuning::mobile(16 * GIB).cache_bytes, 128 << 20);
        assert!(Tuning::mobile(GIB).tile > Tuning::desktop(GIB).tile);
        assert_eq!(Tuning::default().tile, TILE);
    }
}
