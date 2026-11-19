//! The page tree (ISO 32000-2 §7.7.3) and page operations.
//!
//! [`PageTree::open`] gives page access on demand and [`PageTree::load`] lists all pages
//! with their inherited attributes and sizes. The operations in [`ops`] edit a
//! [`Document`](pagebara_syntax::Document) in place, so they are undone by keeping a snapshot
//! (a clone) and saved like any other edit. They keep outlines, links, named destinations,
//! form fields, page labels, the structure tree and article threads consistent.

mod assemble;
mod dest;
mod edit;
mod fixup;
mod gc;
mod import;
mod labels;
mod names;
pub mod ops;
mod outline;
mod rebuild;
mod rect;
mod structure;
mod threads;
mod tree;
mod xfer;

pub use gc::{compact, remove_unreachable, save_compact};
pub use labels::page_labels;
pub use ops::{
    blank, crop, delete, duplicate, extract, insert, merge, move_pages, reorder, repair_tree,
    rotate, split,
};
pub use rect::Rect;
pub use tree::{Page, PageTree};

use pagebara_syntax::Document;

/// What an operation does with a document's pages, for its permissions (ISO 32000-2
/// table 22).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Delete, move, rotate, crop, duplicate or insert pages: "assemble the document".
    Assemble,
    /// Copy pages out into another document, which the copy-content bit governs.
    Extract,
}

/// Whether the document's security settings allow `access`. Unprotected documents and
/// those unlocked with the owner password allow everything; a document opened with the
/// user password gets full rights once [`Document::unlock_owner`] accepts the owner's.
pub fn permitted(doc: &Document, access: Access) -> bool {
    let p = doc.permissions();
    match access {
        Access::Assemble => p.assemble,
        Access::Extract => p.copy,
    }
}

fn check(doc: &Document, access: Access) -> Result<()> {
    if permitted(doc, access) {
        Ok(())
    } else {
        Err(Error::NotPermitted(access))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("page {index} does not exist (the document has {count})")]
    OutOfRange { index: usize, count: usize },
    #[error("the new order is not a permutation of the pages")]
    NotPermutation,
    #[error("a document needs at least one page")]
    WouldBeEmpty,
    #[error("the document has no catalog")]
    NoCatalog,
    #[error(
        "the document's security settings do not allow this ({0:?}); the owner password grants it"
    )]
    NotPermitted(Access),
    #[error(transparent)]
    Syntax(#[from] pagebara_syntax::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
