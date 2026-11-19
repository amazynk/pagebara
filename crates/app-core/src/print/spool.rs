//! The parts of printing through a spooler that draws bitmaps (Windows GDI) that do not
//! depend on the platform: reading the sheets back from the print file, converting them to
//! device-independent bitmaps and placing them on the device's page.

use pagebara_pages::PageTree;
use pagebara_syntax::Document;

use super::{Duplex, Paper, Sheet};

/// The print file written by [`super::write_pdf`]: one image per page.
#[derive(Debug)]
pub struct PrintFile {
    doc: Document,
    pages: PageTree,
}

impl PrintFile {
    pub fn open(path: &std::path::Path) -> Result<PrintFile, String> {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let doc = Document::open(bytes).map_err(|e| format!("{}: {e:?}", path.display()))?;
        let pages = PageTree::open(&doc);
        Ok(PrintFile { doc, pages })
    }

    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Sheet `i`, decoded.
    pub fn sheet(&self, i: usize) -> Result<Sheet, String> {
        let bad = || format!("print file: page {} is not a printed sheet", i + 1);
        let page = self.pages.page(i).ok_or_else(bad)?;
        let d = &self.doc;
        let o = d.get(page.r).ok_or_else(bad)?;
        let dict = o.as_dict().ok_or_else(bad)?;
        let res = d.get_in(dict, b"Resources");
        let xobj = res.as_dict().map(|r| d.get_in(r, b"XObject"));
        let im = xobj
            .as_ref()
            .and_then(|x| x.as_dict())
            .map(|x| d.get_in(x, b"Im0"))
            .ok_or_else(bad)?;
        let s = im.as_stream().ok_or_else(bad)?;
        let width = s.dict.int(b"Width").ok_or_else(bad)? as u32;
        let height = s.dict.int(b"Height").ok_or_else(bad)? as u32;
        let grey = s.dict.name(b"ColorSpace") == Some(b"DeviceGray");
        let data = d.decode(s).map_err(|e| format!("print file: {e:?}"))?.data;
        if data.len() != width as usize * height as usize * if grey { 1 } else { 3 } {
            return Err(bad());
        }
        Ok(Sheet {
            width,
            height,
            grey,
            data,
            size_pt: page.size(),
        })
    }
}

/// A bottom-up 24-bit BGR bitmap with rows padded to four bytes, as GDI takes them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dib {
    pub width: i32,
    pub height: i32,
    pub bits: Vec<u8>,
}

pub fn to_dib(s: &Sheet) -> Dib {
    let (w, h) = (s.width as usize, s.height as usize);
    let stride = (w * 3).div_ceil(4) * 4;
    let mut bits = vec![0u8; stride * h];
    let channels = if s.grey { 1 } else { 3 };
    for y in 0..h {
        let src = &s.data[y * w * channels..(y + 1) * w * channels];
        let row = &mut bits[(h - 1 - y) * stride..][..w * 3];
        let px = row.as_chunks_mut::<3>().0.iter_mut();
        if s.grey {
            for (d, &v) in px.zip(src) {
                *d = [v; 3];
            }
        } else {
            for (d, p) in px.zip(src.as_chunks::<3>().0) {
                *d = [p[2], p[1], p[0]];
            }
        }
    }
    Dib {
        width: w as i32,
        height: h as i32,
        bits,
    }
}

/// What the device says about its page, in device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DevicePage {
    /// Pixels per inch.
    pub dpi: (f64, f64),
    /// The whole sheet of paper.
    pub physical: (i32, i32),
    /// Where the printable area starts on it; drawing is relative to that.
    pub offset: (i32, i32),
}

/// Where a sheet of `size_pt` goes on the device: x, y, width, height relative to the
/// printable area. A sheet the size of the paper lands on it exactly; another (a page
/// printed at its own size on other paper) is scaled to fit and centred.
pub fn place_on_device(size_pt: (f64, f64), dev: &DevicePage) -> (i32, i32, i32, i32) {
    let w = size_pt.0 / 72.0 * dev.dpi.0;
    let h = size_pt.1 / 72.0 * dev.dpi.1;
    let (pw, ph) = (f64::from(dev.physical.0), f64::from(dev.physical.1));
    let k = if w <= pw * 1.02 && h <= ph * 1.02 {
        1.0
    } else {
        (pw / w).min(ph / h)
    };
    let (w, h) = (w * k, h * k);
    let x = ((pw - w) / 2.0).max(0.0) - f64::from(dev.offset.0);
    let y = ((ph - h) / 2.0).max(0.0) - f64::from(dev.offset.1);
    (
        x.round() as i32,
        y.round() as i32,
        w.round() as i32,
        h.round() as i32,
    )
}

/// The `DEVMODE` paper size (`DMPAPER_*`); `None` leaves the printer's.
pub fn paper_code(p: Paper) -> Option<i16> {
    match p {
        Paper::Letter => Some(1),
        Paper::Legal => Some(5),
        Paper::A3 => Some(8),
        Paper::A4 => Some(9),
        Paper::A5 => Some(11),
        Paper::Document => None,
    }
}

/// The `DEVMODE` duplex mode (`DMDUP_*`): long-edge binding is "vertical".
pub fn duplex_code(d: Duplex) -> i16 {
    match d {
        Duplex::Off => 1,
        Duplex::LongEdge => 2,
        Duplex::ShortEdge => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dib_rows_are_bgr_bottom_up_and_padded() {
        let s = Sheet {
            width: 2,
            height: 2,
            grey: false,
            data: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
            size_pt: (2.0, 2.0),
        };
        let d = to_dib(&s);
        assert_eq!((d.width, d.height), (2, 2));
        // Rows of 6 bytes padded to 8; the last row of the sheet comes first.
        assert_eq!(d.bits, [9, 8, 7, 12, 11, 10, 0, 0, 3, 2, 1, 6, 5, 4, 0, 0]);
        let g = Sheet {
            width: 1,
            height: 1,
            grey: true,
            data: vec![200],
            size_pt: (1.0, 1.0),
        };
        assert_eq!(to_dib(&g).bits, [200, 200, 200, 0]);
    }

    #[test]
    fn sheets_land_on_the_paper() {
        // A4 at 600 dpi with a 1/6 inch unprintable margin.
        let dev = DevicePage {
            dpi: (600.0, 600.0),
            physical: (4961, 7016),
            offset: (100, 100),
        };
        let a4 = Paper::A4.size().unwrap();
        assert_eq!(place_on_device(a4, &dev), (-100, -100, 4961, 7016));
        // A landscape page at its own size is scaled down to fit and centred.
        let (x, y, w, h) = place_on_device((842.0, 595.0), &dev);
        assert_eq!(w, 4961);
        assert!(
            (h - 3505).abs() <= 1 && y > 1000 && x == -100,
            "{x} {y} {w} {h}"
        );
        assert_eq!(paper_code(Paper::Letter), Some(1));
        assert_eq!(paper_code(Paper::Document), None);
        assert_eq!(duplex_code(Duplex::LongEdge), 2);
    }
}
