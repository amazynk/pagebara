//! Digital signatures (ISO 32000-2 §12.8, PAdES / ETSI EN 319 142): verifying every
//! signature of a document and signing one, in pure Rust.
//!
//! - [`verify`] checks each signature field: the CMS signature over its byte range, the
//!   certificate chain against a [`TrustStore`], timestamps, embedded revocation data,
//!   and what changed in later revisions (DocMDP / FieldMDP rules).
//! - [`sign`] adds a PAdES (`ETSI.CAdES.detached`) or `adbe.pkcs7.detached` signature as
//!   an incremental update, optionally certifying the document and timestamping the
//!   signature through a [`TimestampAuthority`].
//! - [`add_ltv`] and [`timestamp_document`] extend signatures to PAdES B-LT and B-LTA:
//!   a Document Security Store with certificates and revocation data, then a document
//!   timestamp.
//!
//! Network access (timestamp authorities, OCSP responders, CRL downloads) is the caller's:
//! the engine builds the requests and takes the answers through traits.
//!
//! Keys can live outside the process: [`system_identities`] lists those of the Windows
//! certificate store and the macOS keychain, which sign through a [`Signer`]. That FFI is
//! the crate's only unsafe code (Windows), kept in `keystore/win.rs`.

mod asn1;
mod cert;
mod chain;
mod cms;
mod crypto;
mod identity;
mod keystore;
mod ltv;
mod mdp;
mod oid;
mod pem;
mod pkcs12;
mod revocation;
mod sign;
mod time;
mod trust;
mod tsp;
mod verify;

pub use cert::{Certificate, CertificateRequest, KeyUsage};
pub use chain::Trust;
pub use crypto::{DigestAlgorithm, PrivateKey, PublicKey, SignatureScheme, Signer};
pub use identity::Identity;
pub use keystore::system_identities;
pub use ltv::{Dss, RevocationFetcher, Vri, add_dss, add_ltv, timestamp_document, vri_key};
pub use mdp::{Change, ChangeKind};
pub use revocation::{
    RevocationSource, RevocationStatus, issue_crl, issue_ocsp_response, ocsp_request,
};
pub use sign::{Appearance, MdpPermissions, SignOptions, SubFilter, sign};
pub use time::Time;
pub use trust::{TrustSource, TrustStore};
pub use tsp::{
    HttpTimestampAuthority, LocalTimestampAuthority, TimestampAuthority, TstInfo, timestamp_data,
};
pub use verify::{
    SignatureKind, SignatureReport, Status, TimeSource, TimestampReport, VerifyOptions, verify,
};

#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("damaged {0}")]
    Malformed(&'static str),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("key: {0}")]
    Key(String),
    #[error("wrong password")]
    WrongPassword,
    #[error("timestamp: {0}")]
    Timestamp(String),
    #[error("no signature field named {0:?}")]
    NoSuchField(String),
    #[error("field {0:?} is already signed")]
    AlreadySigned(String),
    #[error("field {0:?} is not a signature field")]
    NotASignatureField(String),
    #[error("no page {0}")]
    NoSuchPage(usize),
    #[error("the document's certification does not allow more signatures")]
    NotPermitted,
    #[error("PDF: {0}")]
    Pdf(String),
    #[error("{0}")]
    Io(String),
}

impl From<pagebara_syntax::Error> for Error {
    fn from(e: pagebara_syntax::Error) -> Self {
        Error::Pdf(e.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, Error>;
