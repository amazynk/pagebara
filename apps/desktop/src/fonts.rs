//! Inter, the app's typeface, kept once in the binary: the window's text (registered with
//! Slint here rather than imported in the .slint files, which would embed a second copy)
//! and the text of signature appearances.

use std::sync::Arc;

pub static INTER: &[u8] = include_bytes!("../ui/fonts/InterVariable.ttf");

/// Makes Inter known to Slint; before the first window, whose default font it is.
pub fn register() {
    use slint::fontique_011::{fontique::Blob, shared_collection};
    if shared_collection()
        .register_fonts(Blob::new(Arc::new(INTER)), None)
        .is_empty()
    {
        log::warn!("the Inter font could not be registered");
    }
}
