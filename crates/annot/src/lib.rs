//! Annotations (ISO 32000-2 §12.5): a typed model read from a page's `/Annots`, and edits
//! on a [`Document`](pagebara_syntax::Document) that always write appearance streams, so other
//! readers show exactly what we show.
//!
//! Pages are passed as the page object's reference; this crate finds what it needs in the
//! page dictionary itself (boxes, `/Annots`, resources, contents).

mod appearance;
mod content;
mod deflate;
mod edit;
mod embedded;
mod font;
mod geom;
mod image;
mod link;
mod model;
mod page;
mod read;
mod shape;
mod smooth;
mod text;

pub use deflate::zlib;
pub use edit::{
    Written, add, add_flattened, add_flattened_with, add_popup, add_with, draw_popup, flatten,
    flatten_with, remove, reply, update, update_with,
};
pub use embedded::{EmbeddedFont, default_font};
pub use font::{Glyph, Helvetica, Line, Shaper, TextFont, unsupported_chars};
pub use geom::{Color, Point, Quad, Rect};
pub use image::{Placement, RgbaImage, StampOptions, add_image_stamp, embed_image, place_image};
pub use link::{Dest, DestPage, Destination, LinkAction, View, named_destination};
pub use model::{
    Align, Annotation, Border, BorderStyle, Flags, Kind, LineEnding, MarkupKind, ReplyType,
};
pub use page::{page_box, page_rotation};
pub use pagebara_font::fallback::{FaceInfo, FontDb};
pub use read::{annotations, read, replies};
pub use text::{date as pdf_date, now as pdf_now};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a page object")]
    NotAPage,
    #[error("annotation not found")]
    NotFound,
    #[error("this kind of annotation cannot be edited here")]
    ReadOnly,
    #[error("invalid input: {0}")]
    Invalid(&'static str),
}

pub type Result<T> = std::result::Result<T, Error>;
