//! Saving: incremental when the file allows it, else a full rewrite; written to a temporary
//! file and renamed into place, then read back so the saved file becomes the new base.

use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};

use pagebara_annot::Kind;
use pagebara_pages::PageTree;
use pagebara_syntax::Document;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveMode {
    /// Appends the changes to the original file when possible.
    Incremental,
    /// Rewrites the whole file.
    Full,
    /// Rewrites the whole file without objects nothing uses any more.
    Compact,
}

/// A save to run off the UI thread.
#[derive(Debug)]
pub struct SaveJob {
    pub doc: Document,
    pub rev: u64,
    pub passwords: Vec<String>,
    pub path: PathBuf,
    pub mode: SaveMode,
    pub flatten_signatures: bool,
    /// Draws form fields into their pages and removes the form.
    pub flatten_forms: bool,
}

#[derive(Debug)]
pub struct SaveDone {
    pub rev: u64,
    pub path: PathBuf,
    /// The saved file, reopened; `None` when it could not be read back.
    pub doc: Option<Document>,
    /// Whether the whole file was rewritten.
    pub full: bool,
    pub bytes: usize,
}

#[derive(Debug)]
pub enum SaveError {
    Io(std::io::Error),
    Pdf(pagebara_syntax::Error),
}

impl fmt::Display for SaveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SaveError::Io(e) => {
                f.write_str(&pagebara_i18n::tr!("The file could not be written: {}", e))
            }
            SaveError::Pdf(e) => f.write_str(&pagebara_i18n::tr!(
                "The document could not be saved: {}",
                e
            )),
        }
    }
}

impl std::error::Error for SaveError {}

impl SaveJob {
    pub fn run(self) -> Result<SaveDone, SaveError> {
        let mut doc = self.doc;
        if self.flatten_signatures {
            flatten_signatures(&mut doc);
        }
        if self.flatten_forms {
            let mut form = pagebara_forms::Form::load(&doc);
            if !form.is_empty()
                && let Err(e) = form.flatten(&mut doc, None)
            {
                log::warn!("form not flattened: {e}");
            }
        }
        let (bytes, full) = match self.mode {
            SaveMode::Incremental => match doc.save_incremental() {
                Ok(b) => (b, false),
                Err(pagebara_syntax::Error::NeedsFullSave) => {
                    (doc.save_full().map_err(SaveError::Pdf)?, true)
                }
                Err(e) => return Err(SaveError::Pdf(e)),
            },
            SaveMode::Full => (doc.save_full().map_err(SaveError::Pdf)?, true),
            SaveMode::Compact => (
                pagebara_pages::save_compact(&doc).map_err(SaveError::Pdf)?,
                true,
            ),
        };
        write_atomic(&self.path, &bytes).map_err(SaveError::Io)?;
        let len = bytes.len();
        Ok(SaveDone {
            rev: self.rev,
            path: self.path,
            doc: reopen(bytes, &self.passwords),
            full,
            bytes: len,
        })
    }
}

/// Opens saved bytes again, with the first of `passwords` that works.
pub fn reopen(bytes: Vec<u8>, passwords: &[String]) -> Option<Document> {
    match Document::open(bytes) {
        Ok(d) => Some(d),
        Err(pagebara_syntax::Error::PasswordRequired(mut locked)) => {
            for p in passwords {
                match locked.unlock(p) {
                    Ok(mut d) => {
                        for p in passwords {
                            d.unlock_owner(p);
                        }
                        return Some(d);
                    }
                    Err(l) => locked = l,
                }
            }
            None
        }
        Err(_) => None,
    }
}

/// Writes `bytes` next to `path` under a temporary name, then renames it into place.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document.pdf".into());
    let tmp = path.with_file_name(format!(".{name}.pagebara-tmp"));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Draws placed signatures into their pages, so they can no longer be moved or removed.
/// Returns how many were flattened.
pub fn flatten_signatures(doc: &mut Document) -> usize {
    let tree = PageTree::load(doc);
    let mut n = 0;
    for page in tree.pages() {
        let Some(d) = doc.get(page.r).and_then(|o| o.as_dict().cloned()) else {
            continue;
        };
        for a in pagebara_annot::annotations(doc, &d) {
            if matches!(a.kind, Kind::Stamp { .. })
                && a.subject.as_deref() == Some("Signature")
                && let Some(id) = a.id
                && pagebara_annot::flatten(doc, page.r, id).is_ok()
            {
                n += 1;
            }
        }
    }
    n
}
