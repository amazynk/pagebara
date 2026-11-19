//! PDF file structure (ISO 32000-2 §7): lexing, objects, cross-reference tables and
//! streams, object streams, repair of damaged files, and [`Document`], an editable object
//! store with cheap snapshots and incremental saving.

pub mod lexer;
pub mod parser;
pub mod text;
pub mod xref;

mod crypt;
mod document;
mod object;
mod objstm;
mod repair;
mod stream;
mod write;

pub use crypt::Permissions;
pub use document::{Document, Locked, Resolved};
pub use object::{Dict, Name, ObjRef, Object, PdfString, Stream, StreamData};
pub use stream::{DecodedStream, ImageFilter};
pub use write::{SaveOptions, write_object};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a PDF file")]
    NotPdf,
    #[error("syntax error at {offset}: {what}")]
    Syntax { offset: usize, what: &'static str },
    #[error("cross-reference: {0}")]
    Xref(&'static str),
    #[error("filter: {0}")]
    Filter(&'static str),
    #[error("unsupported filter {0:?}")]
    UnsupportedFilter(Name),
    #[error(transparent)]
    Decode(#[from] pagebara_filters::Error),
    #[error("a password is required")]
    PasswordRequired(Box<Locked>),
    #[error("this needs the owner password")]
    OwnerPasswordRequired,
    #[error("the password contains characters that are not allowed (SASLprep)")]
    InvalidPassword,
    #[error("unsupported encryption")]
    UnsupportedEncryption,
    #[error("the cross-reference was repaired or protection removed; save a full copy instead")]
    NeedsFullSave,
}

pub type Result<T> = std::result::Result<T, Error>;
