//! Offline spell checking with Hunspell dictionaries, in pure Rust.
//!
//! [`Dictionary`] reads a Hunspell `.aff`/`.dic` pair (8-bit character sets or UTF-8; flag
//! aliases; prefixes and suffixes with conditions, cross products and twofold suffixes;
//! compounding by flags and by rules; the usual special flags) and checks words with
//! Hunspell's case rules. [`Dictionary::suggest`] proposes corrections and [`tokens`] finds
//! the words of a text with their byte ranges.
//!
//! American English is built in ([`en_us`]); the [`catalog`] lists the dictionaries that
//! can be downloaded, pinned by SHA-256, with their licences ([`download`]).

mod aff;
mod casing;
pub mod catalog;
mod charsets;
mod dictionary;
pub mod download;
mod encoding;
#[cfg(feature = "download")]
mod http;
mod suggest;
mod table;
mod tokenize;

use std::sync::{Arc, OnceLock};

pub use catalog::{DictFile, DictInfo};
pub use dictionary::Dictionary;
pub use download::{Fetch, Progress};
#[cfg(feature = "download")]
pub use http::HttpFetch;
pub use suggest::MAX_SUGGESTIONS;
pub use tokenize::{Token, Tokens, tokens};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("not a Hunspell dictionary: {0}")]
    Invalid(&'static str),
    #[error("{0}: {1}")]
    Io(String, String),
    #[error("downloading {0} failed: {1}")]
    Download(String, String),
    #[error("{0} is damaged (checksum mismatch)")]
    Checksum(String),
    #[error("cancelled")]
    Cancelled,
    #[error("no dictionary for {0}")]
    Unavailable(String),
}

static EN_US_AFF: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/en_US.aff.deflate"));
static EN_US_DIC: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/en_US.dic.deflate"));

/// The built-in American English dictionary, inflated and parsed on first use.
pub fn en_us() -> Arc<Dictionary> {
    static DICT: OnceLock<Arc<Dictionary>> = OnceLock::new();
    DICT.get_or_init(|| {
        let inflate = |d: &[u8]| {
            miniz_oxide::inflate::decompress_to_vec_with_limit(d, 16 << 20).unwrap_or_default()
        };
        let dict = Dictionary::from_bytes(&inflate(EN_US_AFF), &inflate(EN_US_DIC))
            .expect("the bundled en_US dictionary is valid");
        Arc::new(dict)
    })
    .clone()
}

/// Bytes the built-in dictionary adds to the program (compressed).
pub fn bundled_size() -> usize {
    EN_US_AFF.len() + EN_US_DIC.len()
}
