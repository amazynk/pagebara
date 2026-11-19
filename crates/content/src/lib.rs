//! Content streams (ISO 32000-2 §8–9): operators, the graphics and text state, and a
//! [`DisplayList`] for rendering, plus text extraction from it.
//!
//! ```standalone_crate,no_run
//! # fn f(doc: &pagebara_syntax::Document, page: &pagebara_syntax::Dict, res: &pagebara_syntax::Dict) {
//! let mut interp = pagebara_content::Interpreter::new(doc);
//! let list = interp.page(page, res);
//! let text = pagebara_content::extract_text(doc, &list);
//! # }
//! ```

mod copy;
pub mod display;
pub mod extract;
pub mod font;
pub mod geom;
pub mod lex;
pub mod oc;
pub mod ops;
pub mod parse;

mod color;
mod extgstate;
mod interp;
mod state;
mod textops;
mod xobject;

pub use copy::{copy_text, is_cjk};
pub use display::{Cmd, DisplayList};
pub use extract::{
    ExtraChar, TextBlock, TextChar, TextLine, TextPage, TextWord, extract_marked, extract_text,
    extract_text_with, marked_bounds,
};
pub use font::Font;
pub use geom::{Matrix, Point, Rect};
pub use interp::{Interpreter, Options, page_contents};
pub use ops::Op;
pub use parse::{ContentParser, Instr};
