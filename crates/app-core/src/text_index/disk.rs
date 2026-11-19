//! The folded text of every page kept on disk between runs, in a file named after the
//! SHA-256 of the document's bytes. Layout: [`MAGIC`], the format version and the app's
//! version, the hash, the page count, then compressed: each page's length (u32 LE) and text.
//! Files that do not match are ignored (and replaced once the index is full again); the
//! oldest go when the directory passes [`CAP`].

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Once, OnceLock};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use super::TextIndex;

const MAGIC: &[u8; 8] = b"artfxtxi";
/// Bump when the file layout or the text extracted from pages changes.
const VERSION: u32 = 1;
const EXT: &str = "txi";
/// All index files together.
const CAP: u64 = 256 << 20;

#[derive(Debug, Default)]
pub(super) struct Cache {
    dir: Option<PathBuf>,
    /// SHA-256 of the document's bytes.
    hash: OnceLock<[u8; 32]>,
    loaded: Once,
    saved: AtomicBool,
}

impl Cache {
    pub(super) fn new(dir: PathBuf) -> Self {
        Cache {
            dir: Some(dir),
            ..Cache::default()
        }
    }
}

impl TextIndex {
    fn hash(&self) -> &[u8; 32] {
        self.disk
            .hash
            .get_or_init(|| Sha256::digest(self.doc.source()).into())
    }

    fn cache_path(&self) -> Option<PathBuf> {
        let dir = self.disk.dir.as_ref()?;
        let name: String = self.hash().iter().map(|b| format!("{b:02x}")).collect();
        Some(dir.join(name).with_extension(EXT))
    }

    /// Reads the index from disk, once; does nothing without a cache directory or file.
    pub fn load_cached(&self) {
        self.disk.loaded.call_once(|| {
            let Some(path) = self.cache_path() else {
                return;
            };
            let Ok(bytes) = std::fs::read(&path) else {
                return;
            };
            match self.parse(&bytes) {
                Some(pages) => {
                    for (i, text) in pages.into_iter().enumerate() {
                        self.set_folded(i, text);
                    }
                    self.disk.saved.store(true, Ordering::Release);
                    // Marks it recently used, for the eviction of the oldest.
                    let touched = std::fs::File::options()
                        .append(true)
                        .open(&path)
                        .and_then(|f| f.set_modified(SystemTime::now()));
                    if let Err(e) = touched {
                        log::debug!("text index {}: {e}", path.display());
                    }
                }
                None => log::info!("ignoring text index {}", path.display()),
            }
        });
    }

    /// Writes the index to disk once every page is indexed (once per index).
    pub fn save_cached(&self) {
        if !self.is_indexed() || self.disk.saved.load(Ordering::Acquire) {
            return;
        }
        let Some(path) = self.cache_path() else {
            return;
        };
        if self.disk.saved.swap(true, Ordering::AcqRel) {
            return;
        }
        let mut body = Vec::new();
        for i in 0..self.len() {
            let text = self.folded(i).unwrap_or_default();
            body.extend_from_slice(&(text.len() as u32).to_le_bytes());
            body.extend_from_slice(text.as_bytes());
        }
        let mut out = self.header();
        out.extend(pagebara_filters::encode_flate(&body, 3));
        let written = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| crate::session::write_atomic(&path, &out));
        match written {
            Ok(()) => evict(&path),
            Err(e) => log::warn!("text index {}: {e}", path.display()),
        }
    }

    fn header(&self) -> Vec<u8> {
        let mut h = MAGIC.to_vec();
        h.extend_from_slice(&VERSION.to_le_bytes());
        let app = env!("CARGO_PKG_VERSION").as_bytes();
        h.push(app.len() as u8);
        h.extend_from_slice(app);
        h.extend_from_slice(self.hash());
        h.extend_from_slice(&(self.len() as u32).to_le_bytes());
        h
    }

    /// The pages' folded text in a file, if it is this document's index.
    fn parse(&self, bytes: &[u8]) -> Option<Vec<String>> {
        let header = self.header();
        let body = bytes.strip_prefix(header.as_slice())?;
        let filter = pagebara_filters::Filter::Flate {
            predictor: pagebara_filters::Predictor::default(),
        };
        let d =
            pagebara_filters::decode(body, &[filter], pagebara_filters::Limits::default()).ok()?;
        if d.damaged {
            return None;
        }
        let mut rest = d.data.as_slice();
        let mut pages = Vec::with_capacity(self.len());
        while let Some((len, tail)) = rest.split_first_chunk::<4>() {
            let len = u32::from_le_bytes(*len) as usize;
            let text = tail.get(..len)?;
            pages.push(String::from_utf8(text.to_vec()).ok()?);
            rest = &tail[len..];
        }
        (rest.is_empty() && pages.len() == self.len()).then_some(pages)
    }
}

/// Removes the least recently used index files until the directory fits [`CAP`]; never
/// `keep`.
fn evict(keep: &Path) {
    let Some(dir) = keep.parent() else { return };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = entries
        .filter_map(|e| {
            let e = e.ok()?;
            let path = e.path();
            if path.extension()? != EXT {
                return None;
            }
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), path))
        })
        .collect();
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    files.sort();
    for (_, len, path) in files {
        if total <= CAP {
            break;
        }
        if path != keep && std::fs::remove_file(&path).is_ok() {
            total -= len;
        }
    }
}
