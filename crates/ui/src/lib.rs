//! Slint code shared by the desktop and mobile apps. Each app compiles its own `.slint`
//! files, so the types Slint generates differ per app; the page view reaches its widgets
//! through [`ViewSink`], which each app implements for its window.

pub mod snapshot;
mod tile;
mod view;

pub use tile::SlintTile;
pub use view::{Models, REDUCE_MOTION, ViewBridge, ViewSink, cache_bytes, total_memory};
