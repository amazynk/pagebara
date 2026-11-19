//! What the apps' Edit mode needs on top of [`crate::edit`]: the selected objects of a
//! page and how they are described, selection handles and the page-space matrices that
//! dragging them makes (with snapping), finding the selection again after an edit, and
//! in-place text editing ([`text`]).
//!
//! Geometry is in view space (points from the top left of the displayed page, y down, see
//! [`PageFrame::to_view`]) for everything the user points at, and in page space for the
//! operations.

pub mod raster;
pub mod select;
pub mod text;
pub mod transform;

pub use pagebara_edit::PageFrame;
pub use raster::{DragImages, ImagePose, Raster, drag_images, preview_scale, preview_stale};
pub use select::{
    Selection, describe, describe_selection, kind_name, mirrored, object_center, refind, scale_of,
    tab_order,
};
pub use text::{
    CaretBox, DragUnit, Misspelled, StyleSummary, TextEdit, TextLook, TextView, block_len, byte_of,
    char_of, run_look, text_look,
};
pub use transform::{
    Drag, DragKind, DragResult, Guide, Handle, ViewRect, handle_at, handles, move_by_view, outset,
    to_page,
};
