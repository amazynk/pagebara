//! Printing: which pages, how they sit on the paper, and the print-ready file.
//!
//! Every sheet is rasterised with our own renderer at the printer's resolution and written
//! as one image per page of a PDF. That is what the preview shows and what the printer
//! gets, pixel for pixel: annotations, form fields, page themes aside, look exactly as in
//! the app, and the printer's own PDF interpreter (often a different, older engine) never
//! sees fonts, transparency or broken syntax it could get wrong. The cost is a larger file
//! and text that is only as sharp as the chosen resolution (300 dpi by default).

mod pdf;
mod sheet;
pub mod spool;
pub mod system;
#[cfg(windows)]
pub mod win32;

use pagebara_i18n::tr;
use std::ops::Range;

pub use pagebara_render::Renderer;
pub use pdf::{PrintDoc, write_pdf};
pub use sheet::{Sheet, prepare_document, render_sheet};

/// Points per inch.
const PT: f64 = 72.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PageRange {
    All,
    /// 0-based.
    Current(usize),
    /// Like "1-3, 5" (1-based).
    Ranges(String),
    /// Pages picked elsewhere (thumbnails, the organize view), 0-based.
    Pages(Vec<usize>),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Scale {
    /// The page as large as the paper allows.
    Fit,
    /// 100 %.
    Actual,
    /// A percentage.
    Custom(f64),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Paper {
    #[default]
    A4,
    Letter,
    Legal,
    A3,
    A5,
    /// Each sheet the size of its page.
    Document,
}

impl Paper {
    pub const ALL: [Paper; 6] = [
        Paper::A4,
        Paper::Letter,
        Paper::Legal,
        Paper::A3,
        Paper::A5,
        Paper::Document,
    ];

    /// Portrait size in points; `None` for [`Paper::Document`].
    pub fn size(self) -> Option<(f64, f64)> {
        let mm = |w: f64, h: f64| Some((w / 25.4 * PT, h / 25.4 * PT));
        match self {
            Paper::A4 => mm(210.0, 297.0),
            Paper::Letter => Some((612.0, 792.0)),
            Paper::Legal => Some((612.0, 1008.0)),
            Paper::A3 => mm(297.0, 420.0),
            Paper::A5 => mm(148.0, 210.0),
            Paper::Document => None,
        }
    }

    /// The CUPS media name.
    pub fn media(self) -> Option<&'static str> {
        match self {
            Paper::A4 => Some("A4"),
            Paper::Letter => Some("Letter"),
            Paper::Legal => Some("Legal"),
            Paper::A3 => Some("A3"),
            Paper::A5 => Some("A5"),
            Paper::Document => None,
        }
    }

    pub fn label(self) -> String {
        match self {
            Paper::A4 => "A4".into(),
            // TRANSLATORS: the US paper size
            Paper::Letter => tr!("Letter"),
            // TRANSLATORS: the US paper size
            Paper::Legal => tr!("Legal"),
            Paper::A3 => "A3".into(),
            Paper::A5 => "A5".into(),
            Paper::Document => tr!("Same as page"),
        }
    }

    /// Letter where it is the norm (by the locale), else A4.
    pub fn for_locale(locale: &str) -> Paper {
        let region = locale
            .split(['.', '@'])
            .next()
            .and_then(|l| l.split(['_', '-']).nth(1))
            .unwrap_or("");
        match region {
            "US" | "CA" | "MX" | "PH" | "CL" | "CO" | "VE" => Paper::Letter,
            _ => Paper::A4,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Duplex {
    #[default]
    Off,
    LongEdge,
    ShortEdge,
}

impl Duplex {
    /// The CUPS `sides` option.
    pub fn sides(self) -> &'static str {
        match self {
            Duplex::Off => "one-sided",
            Duplex::LongEdge => "two-sided-long-edge",
            Duplex::ShortEdge => "two-sided-short-edge",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PrintOptions {
    pub copies: u32,
    pub range: PageRange,
    pub scale: Scale,
    pub paper: Paper,
    /// Turn pages whose orientation differs from the paper's.
    pub auto_rotate: bool,
    /// Centre on the paper, else at the top left.
    pub centre: bool,
    pub annotations: bool,
    pub forms: bool,
    pub greyscale: bool,
    pub duplex: Duplex,
    pub dpi: u32,
}

impl Default for PrintOptions {
    fn default() -> Self {
        PrintOptions {
            copies: 1,
            range: PageRange::All,
            scale: Scale::Fit,
            paper: Paper::A4,
            auto_rotate: true,
            centre: true,
            annotations: true,
            forms: true,
            greyscale: false,
            duplex: Duplex::Off,
            dpi: 300,
        }
    }
}

/// The 0-based pages to print, in order.
pub fn select_pages(range: &PageRange, count: usize) -> Result<Vec<usize>, String> {
    let pages: Vec<usize> = match range {
        PageRange::All => (0..count).collect(),
        PageRange::Current(i) if *i < count => vec![*i],
        PageRange::Current(_) => Vec::new(),
        PageRange::Ranges(s) => crate::page_ops::parse_ranges(s, count)?
            .into_iter()
            .flat_map(Range::into_iter)
            .collect(),
        PageRange::Pages(p) => p.iter().copied().filter(|&i| i < count).collect(),
    };
    if pages.is_empty() {
        return Err(tr!("No pages to print."));
    }
    Ok(pages)
}

/// Where a page goes on a sheet, in points from the sheet's top left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    /// The sheet, portrait or landscape as used.
    pub sheet: (f64, f64),
    /// The page is turned a quarter clockwise.
    pub rotate: bool,
    /// Sheet points per page point.
    pub scale: f64,
    pub x: f64,
    pub y: f64,
    /// The page's size on the sheet (after turning).
    pub w: f64,
    pub h: f64,
}

/// Places a page of `page` points (as displayed) on `opts.paper`.
pub fn place(page: (f64, f64), opts: &PrintOptions) -> Placement {
    let Some(paper) = opts.paper.size() else {
        return Placement {
            sheet: page,
            rotate: false,
            scale: 1.0,
            x: 0.0,
            y: 0.0,
            w: page.0,
            h: page.1,
        };
    };
    let landscape = |(w, h): (f64, f64)| w > h;
    let rotate = opts.auto_rotate && landscape(page) != landscape(paper);
    let (pw, ph) = if rotate { (page.1, page.0) } else { page };
    let sheet = paper;
    let scale = match opts.scale {
        Scale::Fit => (sheet.0 / pw).min(sheet.1 / ph),
        Scale::Actual => 1.0,
        Scale::Custom(p) => (p / 100.0).clamp(0.01, 10.0),
    };
    let (w, h) = (pw * scale, ph * scale);
    let (x, y) = if opts.centre {
        ((sheet.0 - w) / 2.0, (sheet.1 - h) / 2.0)
    } else {
        (0.0, 0.0)
    };
    Placement {
        sheet,
        rotate,
        scale,
        x,
        y,
        w,
        h,
    }
}

#[cfg(test)]
mod tests;
