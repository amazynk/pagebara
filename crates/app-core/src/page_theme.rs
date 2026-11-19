//! Page colour themes for reading: applied to rendered pixels, so they change the view
//! only, never the document. Dark and high contrast invert lightness but keep hues, so
//! photos and charts stay recognisable.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::{Deserialize, Serialize};

use crate::tiles::{Job, PageRenderer};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PageTheme {
    #[default]
    Normal,
    Dark,
    Sepia,
    HighContrast,
}

/// Paper and ink of the sepia theme.
const SEPIA_PAPER: [u8; 3] = [0xF4, 0xEC, 0xD8];
const SEPIA_INK: [u8; 3] = [0x5B, 0x46, 0x36];
/// Dark pages are dark grey with light grey text rather than black and white.
const DARK_LO: f64 = 28.0;
const DARK_HI: f64 = 222.0;

impl PageTheme {
    pub const ALL: [PageTheme; 4] = [
        PageTheme::Normal,
        PageTheme::Dark,
        PageTheme::Sepia,
        PageTheme::HighContrast,
    ];

    pub fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(0)
    }

    pub fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or_default()
    }

    pub fn name(self) -> &'static str {
        match self {
            PageTheme::Normal => "normal",
            PageTheme::Dark => "dark",
            PageTheme::Sepia => "sepia",
            PageTheme::HighContrast => "high-contrast",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == s)
    }

    /// The colour of blank paper in this theme.
    pub fn paper(self) -> [u8; 3] {
        let mut px = [255, 255, 255, 255];
        self.apply(&mut px);
        [px[0], px[1], px[2]]
    }

    /// Recolours premultiplied RGBA8 pixels in place.
    pub fn apply(self, px: &mut [u8]) {
        if self == PageTheme::Normal {
            return;
        }
        let lut = self.lut();
        for p in px.as_chunks_mut::<4>().0 {
            let a = p[3];
            if a == 0 {
                continue;
            }
            let mut rgb = [p[0], p[1], p[2]];
            if a < 255 {
                // Work on straight colour; tiles are opaque, this is for the odd edge.
                for c in &mut rgb {
                    *c = (u16::from(*c) * 255 / u16::from(a)).min(255) as u8;
                }
            }
            if self != PageTheme::Sepia {
                rgb = lift_ink(rgb, invert_lightness(rgb));
            }
            for (i, c) in rgb.iter_mut().enumerate() {
                *c = lut[i][usize::from(*c)];
            }
            if a < 255 {
                for c in &mut rgb {
                    *c = (u16::from(*c) * u16::from(a) / 255) as u8;
                }
            }
            p[..3].copy_from_slice(&rgb);
        }
    }

    /// Per channel, the mapping applied after the lightness inversion.
    fn lut(self) -> [[u8; 256]; 3] {
        let mut lut = [[0u8; 256]; 3];
        for (ch, table) in lut.iter_mut().enumerate() {
            for (v, out) in table.iter_mut().enumerate() {
                let x = v as f64;
                let y = match self {
                    PageTheme::Normal => x,
                    PageTheme::Dark => DARK_LO + x * (DARK_HI - DARK_LO) / 255.0,
                    // Steeper around the middle: black paper, white ink.
                    PageTheme::HighContrast => (x - 128.0) * 1.6 + 128.0,
                    PageTheme::Sepia => {
                        let (ink, paper) = (f64::from(SEPIA_INK[ch]), f64::from(SEPIA_PAPER[ch]));
                        ink + x * (paper - ink) / 255.0
                    }
                };
                *out = y.round().clamp(0.0, 255.0) as u8;
            }
        }
        lut
    }
}

/// Inverts HSL lightness, keeping hue and saturation: every channel moves by the same
/// amount, so `max + min` becomes `510 - (max + min)`.
fn invert_lightness([r, g, b]: [u8; 3]) -> [u8; 3] {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let shift = 255 - i16::from(max) - i16::from(min);
    let f = |c: u8| (i16::from(c) + shift).clamp(0, 255) as u8;
    [f(r), f(g), f(b)]
}

/// Perceived brightness (Rec. 601 luma), 0-255.
fn luma([r, g, b]: [u8; 3]) -> u32 {
    (77 * u32::from(r) + 150 * u32::from(g) + 29 * u32::from(b)) >> 8
}

/// HSL lightness treats saturated colours as mid-light, so dark-looking ink such as link
/// blue (`[0, 0, 204]`) stays dark after [`invert_lightness`] and is hard to read on a dark
/// page. This mixes the inverted colour toward white, keeping its hue, until it is at least
/// nine tenths as bright as the inverse of the original's perceived brightness (link blue
/// then reads at about 5:1 on dark paper). Greys, black and white are already there and stay
/// as they are.
fn lift_ink(original: [u8; 3], inverted: [u8; 3]) -> [u8; 3] {
    let target = (255 - luma(original)) * 9 / 10;
    let y = luma(inverted);
    if y >= target {
        return inverted;
    }
    // Mixing by t moves luma from y to y + (255 - y) * t.
    let (num, den) = (target - y, 255 - y);
    inverted.map(|c| (u32::from(c) + (255 - u32::from(c)) * num / den).min(255) as u8)
}

/// Draws pages with another renderer and recolours them.
#[allow(missing_debug_implementations)]
pub struct ThemedRenderer {
    pub inner: Arc<dyn PageRenderer>,
    pub theme: PageTheme,
}

impl ThemedRenderer {
    /// `inner` recoloured with `theme`; `inner` itself for the normal theme.
    pub fn wrap(inner: Arc<dyn PageRenderer>, theme: PageTheme) -> Arc<dyn PageRenderer> {
        if theme == PageTheme::Normal {
            inner
        } else {
            Arc::new(ThemedRenderer { inner, theme })
        }
    }
}

impl PageRenderer for ThemedRenderer {
    fn render(&self, job: &Job, cancel: &AtomicBool, out: &mut [u8]) -> bool {
        let done = self.inner.render(job, cancel, out);
        if done {
            self.theme.apply(out);
        }
        done
    }

    fn page_size(&self, page: usize) -> Option<(f64, f64)> {
        self.inner.page_size(page)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(t: PageTheme, rgb: [u8; 3]) -> [u8; 3] {
        let mut p = [rgb[0], rgb[1], rgb[2], 255];
        t.apply(&mut p);
        [p[0], p[1], p[2]]
    }

    fn lum([r, g, b]: [u8; 3]) -> u32 {
        (u32::from(r) * 3 + u32::from(g) * 6 + u32::from(b)) / 10
    }

    #[test]
    fn dark_inverts_lightness_and_keeps_hue() {
        assert_eq!(invert_lightness([255, 255, 255]), [0, 0, 0]);
        assert_eq!(invert_lightness([0, 0, 0]), [255, 255, 255]);
        // Pure red stays pure red (lightness 0.5); dark red becomes light red.
        assert_eq!(invert_lightness([255, 0, 0]), [255, 0, 0]);
        assert_eq!(invert_lightness([128, 0, 0]), [255, 127, 127]);
        let paper = PageTheme::Dark.paper();
        assert!(lum(paper) < 40, "{paper:?}");
        assert!(lum(one(PageTheme::Dark, [0, 0, 0])) > 200);
        let blue = one(PageTheme::Dark, [30, 60, 200]);
        assert!(blue[2] > blue[0] && blue[2] > blue[1], "{blue:?}");
        // Saturated dark ink (link blue) comes out light enough to read on dark paper.
        let link = one(PageTheme::Dark, [0, 0, 204]);
        assert!(luma(link) > 140 && link[2] > link[0], "{link:?}");
        // Greys only invert.
        assert_eq!(lift_ink([100, 100, 100], [155, 155, 155]), [155, 155, 155]);
        assert_eq!(PageTheme::HighContrast.paper(), [0, 0, 0]);
        assert_eq!(one(PageTheme::HighContrast, [0, 0, 0]), [255, 255, 255]);
    }

    #[test]
    fn sepia_and_normal() {
        assert_eq!(PageTheme::Sepia.paper(), SEPIA_PAPER);
        assert_eq!(one(PageTheme::Sepia, [0, 0, 0]), SEPIA_INK);
        assert_eq!(one(PageTheme::Normal, [12, 34, 56]), [12, 34, 56]);
        for t in PageTheme::ALL {
            assert_eq!(PageTheme::parse(t.name()), Some(t));
            assert_eq!(PageTheme::from_index(t.index()), t);
        }
        // Transparent pixels stay untouched.
        let mut p = [0u8; 4];
        PageTheme::Dark.apply(&mut p);
        assert_eq!(p, [0; 4]);
    }
}
