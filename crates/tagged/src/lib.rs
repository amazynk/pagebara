//! Tagged PDF (ISO 32000-2 §14.7–14.8): the logical structure tree, and the content of a
//! page as block-level items in the tree's order, for screen readers, reading aloud and
//! the reading view.
//!
//! ```standalone_crate,no_run
//! # fn f(doc: &pagebara_syntax::Document, page: pagebara_syntax::ObjRef, list: &pagebara_content::DisplayList) {
//! if let Some(tree) = pagebara_tagged::StructTree::load(doc) {
//!     for block in tree.page_content(doc, page, list).blocks {
//!         println!("{:?}: {}", block.kind, block.text);
//!     }
//! }
//! # }
//! ```

mod page;
mod tree;

pub use page::{Block, PageContent};
pub use tree::{Element, Kid, Role, StructTree};

/// What a block-level item of a page is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ItemKind {
    /// Level 1 to 6.
    Heading(u8),
    Paragraph,
    ListItem,
    /// A figure or formula: its text is its alternate description.
    Figure,
    Caption,
    /// A table cell, header or data.
    Cell,
    Quote,
    Note,
    Code,
}

#[cfg(test)]
mod tests;
