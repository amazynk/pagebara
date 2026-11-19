//! Stamping pages: watermarks, headers and footers, and Bates numbers.
//!
//! Every stamp is a form XObject drawn by a content stream of its own, before the page's
//! content (behind) or after it (in front, the original wrapped in `q … Q`). That stream
//! carries a private `/PagebaraStamp` key naming its kind, and marks what it draws as a
//! pagination artifact (`/Artifact <</Type /Pagination /Subtype /Watermark>> BDC`), so
//! tagged-PDF readers and reading order skip it. Optionally it is in an optional content
//! group (a "Watermark" layer by default), which viewers can hide, or show only on screen
//! or only in print.
//!
//! [`remove`] takes our stamps off again; it also finds other tools' watermarks and
//! headers by their artifact marks or their layer name. The settings of a stamp are kept in
//! its XObject (`/PagebaraSpec`, JSON), so [`existing`] can offer them for editing, and
//! [`Preset`]s save them for reuse.

#![forbid(unsafe_code)]

mod bates;
mod date;
mod draw;
mod header;
mod oc;
mod range;
mod remove;
mod text;
mod watermark;

pub use bates::{BatesFile, BatesNumber, bates_batch, bates_csv};
pub use date::{Clock, format_date};
pub use header::{DocInfo, HeaderFooter, Margins, Slots, apply_header_footer};
pub use range::PageSet;
pub use remove::{Found, Removed, existing, find, remove};
pub use text::{Line, Typesetter, standard_fonts};
pub use watermark::{Anchor, Size, Source, Tiling, Watermark, apply_watermark, page_as_form};

use serde::{Deserialize, Serialize};

/// What kind of stamp: the value of the `/PagebaraStamp` marker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    Watermark,
    HeaderFooter,
    Bates,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Watermark, Kind::HeaderFooter, Kind::Bates];

    pub(crate) fn marker(self) -> &'static [u8] {
        match self {
            Kind::Watermark => b"Watermark",
            Kind::HeaderFooter => b"HeaderFooter",
            Kind::Bates => b"Bates",
        }
    }

    pub(crate) fn from_marker(b: &[u8]) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.marker() == b)
    }

    /// The prefix of the resource names we give its XObjects.
    pub(crate) fn prefix(self) -> &'static str {
        match self {
            Kind::Watermark => "PagebaraWm",
            Kind::HeaderFooter => "PagebaraHf",
            Kind::Bates => "PagebaraBn",
        }
    }
}

/// An sRGB colour, components 0–1.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Rgb(pub [f32; 3]);

impl Rgb {
    pub const BLACK: Rgb = Rgb([0.0; 3]);
    pub const RED: Rgb = Rgb([0.85, 0.1, 0.1]);

    /// `#rrggbb` or `rrggbb`.
    pub fn parse(s: &str) -> Option<Rgb> {
        let h = s.trim().trim_start_matches('#');
        if h.len() != 6 || !h.is_ascii() {
            return None;
        }
        let c = |i: usize| u8::from_str_radix(&h[i..i + 2], 16).ok();
        Some(Rgb([c(0)?, c(2)?, c(4)?].map(|v| f32::from(v) / 255.0)))
    }

    pub fn hex(self) -> String {
        let [r, g, b] = self.0.map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
        format!("#{r:02x}{g:02x}{b:02x}")
    }
}

/// Whether a stamp is drawn under the page content or over it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Layer {
    Behind,
    #[default]
    Front,
}

/// When an optional content group shows (its `/Usage` and the default configuration).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Visibility {
    #[default]
    Always,
    ViewOnly,
    PrintOnly,
}

/// Text appearance.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TextStyle {
    /// A standard font (`Helvetica`, `Times-Bold`, `Courier`, …) or an installed family.
    pub font: String,
    /// Points.
    pub size: f32,
    pub color: Rgb,
    /// A box painted behind the text.
    pub background: Option<Rgb>,
}

impl Default for TextStyle {
    fn default() -> Self {
        TextStyle {
            font: "Helvetica".into(),
            size: 10.0,
            color: Rgb::BLACK,
            background: None,
        }
    }
}

/// Saved settings of one stamp, by name (stored by the app as JSON).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum Preset {
    Watermark {
        name: String,
        spec: Box<Watermark>,
    },
    HeaderFooter {
        name: String,
        spec: Box<HeaderFooter>,
    },
}

impl Preset {
    pub fn name(&self) -> &str {
        match self {
            Preset::Watermark { name, .. } | Preset::HeaderFooter { name, .. } => name,
        }
    }

    pub fn to_json(list: &[Preset]) -> String {
        serde_json::to_string_pretty(list).unwrap_or_default()
    }

    /// Presets from JSON; unreadable input gives none.
    pub fn from_json(s: &str) -> Vec<Preset> {
        serde_json::from_str(s).unwrap_or_default()
    }
}

/// What applying a stamp did.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Report {
    /// Pages stamped (0-based).
    pub pages: Vec<usize>,
    /// Characters no font could show (left out).
    pub missing: Vec<char>,
    /// Stamps of the same kind removed first (when replacing).
    pub replaced: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the document's security settings do not allow changes")]
    NotPermitted,
    #[error("pages: {0}")]
    Pages(String),
    #[error("no pages selected")]
    NoPages,
    #[error("image: {0}")]
    Image(String),
    #[error("source PDF: {0}")]
    Source(String),
    #[error("nothing to stamp: {0}")]
    Empty(&'static str),
    #[error("{0}")]
    Io(String),
}

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) fn check_editable(doc: &pagebara_syntax::Document) -> Result<()> {
    if doc.permissions().modify {
        Ok(())
    } else {
        Err(Error::NotPermitted)
    }
}
