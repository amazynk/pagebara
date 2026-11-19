//! Fitting a page into a box and turning rendered RGBA into the BGRA Windows bitmaps use.

/// The pixel size of a `w` × `h` page fitted into `max_w` × `max_h` (aspect kept, at least
/// one pixel each way), and the scale from points to pixels.
pub fn fit(w: f32, h: f32, max_w: u32, max_h: u32) -> (u32, u32, f32) {
    let (w, h) = (w.max(1.0), h.max(1.0));
    let scale = (max_w.max(1) as f32 / w).min(max_h.max(1) as f32 / h);
    let px = |v: f32, max: u32| ((v * scale).round() as u32).clamp(1, max.max(1));
    (px(w, max_w), px(h, max_h), scale)
}

/// RGBA (premultiplied, as the renderer gives it) to BGRA in place: what a 32-bit DIB with
/// premultiplied alpha (`WTSAT_ARGB`, `AlphaBlend`) expects.
pub fn rgba_to_bgra(px: &mut [u8]) {
    for p in px.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
}

/// Where to draw a `w` × `h` image centred in a `box_w` × `box_h` area.
pub fn centre(w: u32, h: u32, box_w: u32, box_h: u32) -> (i32, i32) {
    (
        (box_w.saturating_sub(w) / 2) as i32,
        (box_h.saturating_sub(h) / 2) as i32,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fits_portrait_and_landscape() {
        assert_eq!(fit(595.0, 842.0, 256, 256).0, 181);
        assert_eq!(fit(595.0, 842.0, 256, 256).1, 256);
        let (w, h, s) = fit(842.0, 595.0, 256, 256);
        assert_eq!((w, h), (256, 181));
        assert!((s - 256.0 / 842.0).abs() < 1e-6);
        // A box wider than the page is tall: the height decides.
        assert_eq!(fit(100.0, 100.0, 800, 200).0, 200);
    }

    #[test]
    fn degenerate_sizes_stay_positive() {
        assert_eq!(fit(0.0, 0.0, 0, 0), (1, 1, 1.0));
        let (w, h, _) = fit(10_000.0, 1.0, 64, 64);
        assert_eq!((w, h), (64, 1));
    }

    #[test]
    fn swaps_red_and_blue() {
        let mut px = [1, 2, 3, 4, 10, 20, 30, 40];
        rgba_to_bgra(&mut px);
        assert_eq!(px, [3, 2, 1, 4, 30, 20, 10, 40]);
    }

    #[test]
    fn centres() {
        assert_eq!(centre(100, 50, 300, 50), (100, 0));
        assert_eq!(centre(400, 50, 300, 10), (0, 0));
    }
}
