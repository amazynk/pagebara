//! Windows Explorer extensions, as an in-process COM server (`pagebara_identity::SHELL_DLL`):
//! thumbnails (`IThumbnailProvider`), the preview pane (`IPreviewHandler`) and document
//! text for Windows Search (`IFilter`), all drawn and read with pagebara's own engine.
//!
//! The DLL registers nothing itself: `pagebara-integration` writes the registry keys when the
//! user turns each item on. The class IDs are `pagebara_identity::windows_clsid("thumbnail")`,
//! `("preview")` and `("filter")`.
//!
//! What does not depend on Windows (opening, fitting, pixel conversion, text chunks) lives
//! in the safe modules below and is tested everywhere; `win` is the COM glue.

pub mod chunks;
pub mod image;
pub mod pdf;

#[cfg(windows)]
mod win;

/// Largest file read (bigger files get no thumbnail, preview or index text).
pub const MAX_FILE: u64 = 512 << 20;
