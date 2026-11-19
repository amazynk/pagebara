//! App logic shared by the desktop and mobile shells, free of any UI toolkit: open
//! documents with their edit history, recent files and settings, the page view's geometry
//! and its tile rendering pipeline, text search and selection, annotation editing helpers
//! and the automation scripts used for UI tests.

pub mod annots;
pub mod doc_state;
pub mod edit;
pub mod find;
pub mod form_prep;
pub mod forms;
pub mod library;
pub mod live_text;
pub mod object_edit;
pub mod ocr;
pub mod os_prefs;
pub mod outline;
pub mod page_ops;
pub mod page_theme;
pub mod pages;
pub mod print;
pub mod reading_order;
pub mod recent;
pub mod redaction;
pub mod reflow;
pub mod script;
pub mod session;
pub mod settings;
pub mod signing;
pub mod speech;
pub mod spell;
pub mod stats;
pub mod text;
pub mod text_export;
pub mod text_index;
pub mod theme;
pub mod tiles;
pub mod utterances;
pub mod view;
pub mod vim;

#[cfg(test)]
mod test_pdf;
