//! `/Predictor` of LZW and Flate streams: TIFF predictor 2 and the PNG row filters.

mod png;
mod tiff;

/// Rows at least this large overall are unfiltered by the wavefront on the rayon pool.
const PARALLEL_MIN: usize = 1 << 20;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Kind {
    #[default]
    None,
    /// Predictor 2.
    Tiff,
    /// Predictors 10..=15: the filter type is per row, so they are all the same.
    Png,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Predictor {
    pub kind: Kind,
    pub colors: u8,
    pub bits_per_component: u8,
    pub columns: u32,
}

impl Default for Predictor {
    fn default() -> Self {
        Predictor {
            kind: Kind::None,
            colors: 1,
            bits_per_component: 8,
            columns: 1,
        }
    }
}

impl Predictor {
    /// From `/Predictor`; unknown values mean none.
    pub fn new(predictor: i64, colors: u8, bits_per_component: u8, columns: u32) -> Self {
        let kind = match predictor {
            2 => Kind::Tiff,
            10..=15 => Kind::Png,
            _ => Kind::None,
        };
        Predictor {
            kind,
            colors: colors.max(1),
            bits_per_component,
            columns: columns.max(1),
        }
    }

    fn row_bytes(&self) -> usize {
        (usize::from(self.colors) * usize::from(self.bits_per_component))
            .saturating_mul(self.columns as usize)
            .div_ceil(8)
    }

    /// Undoes the predictor in place. Returns whether rows were damaged.
    pub(crate) fn apply(&self, buf: &mut Vec<u8>) -> bool {
        if !matches!(self.bits_per_component, 1 | 2 | 4 | 8 | 16) && self.kind != Kind::None {
            return true;
        }
        match self.kind {
            Kind::None => false,
            Kind::Tiff => {
                let rb = self.row_bytes();
                for row in buf.chunks_mut(rb) {
                    tiff::unpredict_horizontal(
                        row,
                        u16::from(self.bits_per_component),
                        usize::from(self.colors),
                        false,
                    );
                }
                false
            }
            Kind::Png => {
                let bpp = (usize::from(self.colors) * usize::from(self.bits_per_component))
                    .div_ceil(8)
                    .max(1);
                // A row longer than the data is one partial row: never pad up to `/Columns`.
                let rb = self.row_bytes().min(buf.len().max(1));
                let (out, damaged) = unfilter_png(buf, rb, bpp);
                *buf = out;
                damaged
            }
        }
    }
}

/// `src`: rows of a filter type byte plus `rb` bytes. A partial last row is unfiltered as far
/// as it goes; an unknown filter type is taken as None and reported as damage.
fn unfilter_png(src: &mut Vec<u8>, rb: usize, bpp: usize) -> (Vec<u8>, bool) {
    let stride = rb + 1;
    let tail = src.len() % stride;
    let out_len = src.len() / stride * rb + tail.saturating_sub(1);
    if tail > 0 {
        src.resize(src.len() + stride - tail, 0);
    }
    let mut damaged = false;
    for row in src.chunks_exact_mut(stride) {
        if row[0] > 4 {
            row[0] = 0;
            damaged = true;
        }
    }
    let rows = src.len() / stride;
    let mut out = vec![0u8; rows * rb];
    let fast = matches!(bpp, 1 | 2 | 3 | 4 | 6 | 8);
    if fast && out.len() >= PARALLEL_MIN {
        let segs = png::segments(rb, bpp, rayon::current_num_threads());
        png::unfilter_rows(bpp, rb, &segs, &vec![0; rb], src, &mut out);
    } else {
        let zero = vec![0u8; rb];
        for (y, s) in src.chunks_exact(stride).enumerate() {
            let (done, rest) = out.split_at_mut(y * rb);
            let prev = if y == 0 {
                &zero[..]
            } else {
                &done[(y - 1) * rb..]
            };
            let dst = &mut rest[..rb];
            if fast {
                png::unfilter(s[0], bpp, prev, &s[1..], dst);
            } else {
                unfilter_scalar(s[0], bpp, prev, &s[1..], dst);
            }
        }
    }
    out.truncate(out_len);
    (out, damaged)
}

/// Any bytes per pixel (PDF allows more colors than PNG).
fn unfilter_scalar(ft: u8, bpp: usize, prev: &[u8], src: &[u8], dst: &mut [u8]) {
    for i in 0..src.len() {
        let a = if i >= bpp { i16::from(dst[i - bpp]) } else { 0 };
        let b = i16::from(prev[i]);
        let c = if i >= bpp {
            i16::from(prev[i - bpp])
        } else {
            0
        };
        let p = match ft {
            1 => a,
            2 => b,
            3 => (a + b) / 2,
            4 => {
                let base = a + b - c;
                let (pa, pb, pc) = ((base - a).abs(), (base - b).abs(), (base - c).abs());
                if pa <= pb && pa <= pc {
                    a
                } else if pb <= pc {
                    b
                } else {
                    c
                }
            }
            _ => 0,
        };
        dst[i] = src[i].wrapping_add(p as u8);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filter_row(ft: u8, bpp: usize, prev: &[u8], row: &[u8]) -> Vec<u8> {
        let mut out = vec![ft];
        for i in 0..row.len() {
            let a = if i >= bpp { i16::from(row[i - bpp]) } else { 0 };
            let b = i16::from(prev[i]);
            let c = if i >= bpp {
                i16::from(prev[i - bpp])
            } else {
                0
            };
            let p = match ft {
                1 => a,
                2 => b,
                3 => (a + b) / 2,
                4 => {
                    let base = a + b - c;
                    let (pa, pb, pc) = ((base - a).abs(), (base - b).abs(), (base - c).abs());
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => 0,
            };
            out.push(row[i].wrapping_sub(p as u8));
        }
        out
    }

    fn roundtrip(colors: u8, columns: u32, rows: usize) {
        let p = Predictor::new(12, colors, 8, columns);
        let (rb, bpp) = (p.row_bytes(), usize::from(colors));
        let mut x = 99u32;
        let img: Vec<Vec<u8>> = (0..rows)
            .map(|_| {
                (0..rb)
                    .map(|_| {
                        x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                        (x >> 20) as u8
                    })
                    .collect()
            })
            .collect();
        let mut enc = Vec::new();
        let zero = vec![0; rb];
        for (y, r) in img.iter().enumerate() {
            let prev = if y == 0 { &zero } else { &img[y - 1] };
            enc.extend(filter_row((y % 5) as u8, bpp, prev, r));
        }
        assert!(!p.apply(&mut enc));
        assert_eq!(enc, img.concat(), "colors {colors}");
    }

    #[test]
    fn png_all_filter_types_any_bpp() {
        for colors in [1, 3, 4, 5, 7, 12] {
            roundtrip(colors, 33, 17);
        }
    }

    #[test]
    fn png_parallel_path() {
        roundtrip(3, 2000, 200);
    }

    #[test]
    fn png_partial_row_and_bad_type() {
        let p = Predictor::new(10, 1, 8, 4);
        let mut d = vec![1, 1, 1, 1, 1, 9, 5, 6];
        assert!(p.apply(&mut d));
        assert_eq!(d, [1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn huge_columns_do_not_allocate_rows() {
        let p = Predictor::new(15, 255, 16, u32::MAX);
        let mut d = vec![1, 7, 7, 7];
        assert!(!p.apply(&mut d));
        assert_eq!(d, [7, 7, 7]);
        let mut d = vec![1, 7, 7, 7];
        assert!(!Predictor::new(2, 255, 16, u32::MAX).apply(&mut d));
    }

    #[test]
    fn tiff_8bit_rgb() {
        let p = Predictor::new(2, 3, 8, 2);
        let mut d = vec![10, 20, 30, 1, 2, 3];
        assert!(!p.apply(&mut d));
        assert_eq!(d, [10, 20, 30, 11, 22, 33]);
    }
}
