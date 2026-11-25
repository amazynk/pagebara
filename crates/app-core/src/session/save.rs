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
        drop(f);
        rename_over(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Renames `from` over `to`. On Windows a file that another program has open cannot be
/// replaced, and programs open a PDF for a moment all the time (a virus scanner, the
/// search indexer, Explorer's preview pane, a sync client), so there the rename is tried
/// again for up to two seconds before its error is reported.
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
    let start = std::time::Instant::now();
    let mut pause = std::time::Duration::from_millis(20);
    loop {
        match std::fs::rename(from, to) {
            Err(e)
                if cfg!(windows)
                    && held_by_another_program(&e)
                    && !read_only(to)
                    && start.elapsed() < std::time::Duration::from_secs(2) =>
            {
                std::thread::sleep(pause);
                pause = (pause * 2).min(std::time::Duration::from_millis(250));
            }
            r => return r,
        }
    }
}

/// Whether the file is marked read-only: replacing it is refused however long one waits.
fn read_only(path: &Path) -> bool {
    path.metadata().is_ok_and(|m| m.permissions().readonly())
}

/// Windows' "access denied", "sharing violation" and "lock violation": what replacing a
/// file answers while another program has it open.
fn held_by_another_program(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(5 | 32 | 33))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_file_replaces_the_old_one_and_leaves_no_temporary() {
        let dir = std::env::temp_dir().join(format!("pagebara-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("a.pdf");
        std::fs::write(&path, b"old").unwrap();
        write_atomic(&path, b"new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new");
        let names: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["a.pdf"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn only_a_file_held_by_another_program_is_tried_again() {
        let os = std::io::Error::from_raw_os_error;
        assert!(held_by_another_program(&os(32)));
        assert!(held_by_another_program(&os(5)));
        // "File not found" will not get better by waiting.
        assert!(!held_by_another_program(&os(2)));
        assert!(!held_by_another_program(&std::io::Error::other("x")));
    }
}
