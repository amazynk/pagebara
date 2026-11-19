//! The signature library ("My signatures"): signatures imported from photos or scans (with
//! background removal) or drawn by hand, kept locally per user, and placed on pages as image
//! stamps that every PDF reader shows.

mod background;
mod draw;
mod import;
mod library;
mod place;
mod raster;

pub use background::{Ink, PREVIEW_SIDE, PROCESS_SIDE, Prepared, STORED_MAX, Settings, process};
pub use draw::{Drawing, Stroke, StrokePoint, render as render_drawing};
pub use import::decode as decode_image;
pub use library::{DEFAULT_WIDTH_MM, Library, Signature, Source};
pub use pagebara_annot::Placement;
pub use place::{POINTS_PER_MM, Target, embed as embed_signature, place, place_by};
pub use raster::Rgba;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("not a PNG or JPEG image")]
    UnsupportedImage,
    #[error("image: {0}")]
    Decode(String),
    #[error("library index: {0}")]
    Json(#[from] serde_json::Error),
    #[error("no such signature")]
    NotFound,
    #[error("no per-user data directory on this platform")]
    NoDataDir,
    #[error("invalid input: {0}")]
    Invalid(&'static str),
    #[error(transparent)]
    Annot(#[from] pagebara_annot::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
