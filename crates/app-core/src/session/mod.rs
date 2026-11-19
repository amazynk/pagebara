//! An open document and the steps to get there (reading the file and, for protected files,
//! asking for the password), its edit history and saving.
//!
//! Every edit runs on the current [`Document`] after keeping a snapshot of it (a clone,
//! O(1)), so undo and redo swap whole snapshots.

mod info;
mod readings;
mod save;

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pagebara_i18n::{tr, tr_noop};
use pagebara_pages::PageTree;
use pagebara_syntax::{Document, Locked, ObjRef, Permissions};
use pagebara_tagged::StructTree;

pub use info::{DocInfo, format_pdf_date};
pub use readings::Readings;
pub use save::{SaveDone, SaveError, SaveJob, SaveMode, flatten_signatures, reopen, write_atomic};

use crate::live_text::LiveText;
use crate::outline::{self, OutlineItem};
use crate::pages::PageGeom;
use crate::reading_order::PageReading;
use crate::text;
use crate::text_index::TextIndex;
use pagebara_content::TextPage;

/// Snapshots kept for undo.
const HISTORY: usize = 200;

struct Snapshot {
    doc: Document,
    /// The text index of `doc`, kept so undoing a change of content restores its text.
    text: Arc<TextIndex>,
    rev: u64,
    label: String,
}

/// One open document. The UI keeps a list of these, one per tab.
pub struct Session {
    pub path: Option<PathBuf>,
    doc: Document,
    pub tree: PageTree,
    /// Geometry of every page, in order.
    pub geoms: Vec<PageGeom>,
    pub outline: Vec<OutlineItem>,
    /// Page labels, when the document defines them.
    pub labels: Option<Vec<String>>,
    /// Text of the current pages; replaced when pages change.
    pub text: Arc<TextIndex>,
    refs: Vec<ObjRef>,
    /// Passwords that opened this document, newest first, for reopening after a save.
    passwords: Vec<String>,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    rev: u64,
    next_rev: u64,
    saved_rev: u64,
    /// Changes whenever pages are added, removed, moved, rotated or cropped.
    pub layout_rev: u64,
    /// Changes whenever the document is replaced (edits, undo, reading back a save).
    generation: u64,
    /// The structure tree and the pages' reading order; reset with the document.
    readings: Arc<Readings>,
    /// Text recognised in the pictures of the current pages (never written to the file).
    live: LiveText,
}

impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("path", &self.path)
            .field("pages", &self.geoms.len())
            .field("rev", &self.rev)
            .finish_non_exhaustive()
    }
}

/// What an edit, undo or redo changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Changed {
    /// The pages themselves (count, order, size, rotation), not just their annotations.
    pub pages: bool,
}

impl Session {
    pub fn new(path: Option<PathBuf>, doc: Document) -> Self {
        let text = Arc::new(TextIndex::new(doc.clone(), Vec::new()));
        let mut s = Session {
            path,
            tree: PageTree::open(&doc),
            readings: Arc::new(Readings::new(doc.clone(), Vec::new(), None, text.clone())),
            live: LiveText::new(&text, doc.clone(), Vec::new()),
            text,
            doc,
            geoms: Vec::new(),
            outline: Vec::new(),
            labels: None,
            refs: Vec::new(),
            passwords: Vec::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            rev: 0,
            next_rev: 1,
            saved_rev: 0,
            layout_rev: 0,
            generation: 0,
        };
        s.refresh(true);
        s
    }

    pub fn doc(&self) -> &Document {
        &self.doc
    }

    /// Rebuilds what is derived from the document; with `pages`, also the page list.
    fn refresh(&mut self, pages: bool) -> Changed {
        self.generation += 1;
        self.tree = PageTree::load(&self.doc);
        let geoms: Vec<PageGeom> = self.tree.pages().iter().map(PageGeom::of).collect();
        let refs = self.tree.refs();
        let pages = pages || geoms != self.geoms || refs != self.refs;
        if pages {
            self.geoms = geoms;
            self.refs = refs;
            self.new_text();
            self.layout_rev += 1;
        }
        self.outline = outline::load(&self.doc, &self.tree);
        self.labels = pagebara_pages::page_labels(&self.doc, self.geoms.len());
        self.readings = Arc::new(Readings::new(
            self.doc.clone(),
            self.refs.clone(),
            self.labels.clone(),
            self.text.clone(),
        ));
        Changed { pages }
    }

    /// The file name, or the document's own title for files without a path.
    pub fn title(&self) -> String {
        file_name(self.path.as_deref()).unwrap_or_else(|| {
            let t = self.info_title();
            if t.is_empty() { tr!("Untitled") } else { t }
        })
    }

    /// `/Info /Title`, or empty.
    pub fn info_title(&self) -> String {
        let info = self.doc.get_in(self.doc.trailer(), b"Info");
        info.as_dict()
            .map(|d| self.doc.get_in(d, b"Title"))
            .and_then(|t| t.as_string().map(text::decode))
            .unwrap_or_default()
    }

    pub fn page_count(&self) -> usize {
        self.geoms.len()
    }

    /// The document reads right to left (`/ViewerPreferences` `/Direction /R2L`): spreads
    /// put the first page on the right, whatever the UI language.
    pub fn reads_right_to_left(&self) -> bool {
        let Some(cat) = self.doc.catalog() else {
            return false;
        };
        let Some(cat) = cat.as_dict() else {
            return false;
        };
        let vp = self.doc.get_in(cat, b"ViewerPreferences");
        vp.as_dict()
            .map(|d| self.doc.get_in(d, b"Direction"))
            .is_some_and(|d| d.as_name() == Some(b"R2L"))
    }

    pub fn page_ref(&self, i: usize) -> Option<ObjRef> {
        self.tree.page(i).map(|p| p.r)
    }

    /// The live text of the current pages (made again when they change).
    pub fn live(&mut self) -> &mut LiveText {
        if !self.live.is_for(&self.text) {
            self.live.cancel();
            self.live = LiveText::new(&self.text, self.doc.clone(), self.refs.clone());
        }
        &mut self.live
    }

    /// The live text of the current pages, if it was made for them.
    pub fn live_ref(&self) -> Option<&LiveText> {
        self.live.is_for(&self.text).then_some(&self.live)
    }

    /// The text that selection works on: the page's own text with what was recognised in
    /// its pictures.
    pub fn sel_text(&mut self, page: usize) -> Arc<TextPage> {
        match self.live().text(page) {
            Some(t) => t,
            None => self.text.page(page),
        }
    }

    /// [`Session::sel_text`] when it is known without extracting text.
    pub fn sel_text_cached(&self, page: usize) -> Option<Arc<TextPage>> {
        if self.live.is_for(&self.text)
            && let Some(t) = self.live.peek(page)
        {
            return Some(t);
        }
        self.text.cached(page)
    }

    /// The label of page `i` ("iv", "A-3"), or its number.
    pub fn page_label(&self, i: usize) -> String {
        self.labels
            .as_ref()
            .and_then(|l| l.get(i).cloned())
            .unwrap_or_else(|| (i + 1).to_string())
    }

    pub fn protection(&self) -> String {
        protection_label(self.encrypted(), self.doc.permissions())
    }

    /// Whether the document is (or, once saved, will be) password protected.
    pub fn encrypted(&self) -> bool {
        self.doc.trailer().contains(b"Encrypt")
    }

    pub fn is_dirty(&self) -> bool {
        self.rev != self.saved_rev
    }

    pub fn rev(&self) -> u64 {
        self.rev
    }

    /// Identifies the current document: changes with every edit, undo, redo and with
    /// reading back a save.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|s| s.label.as_str())
    }

    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|s| s.label.as_str())
    }

    /// Runs `f` on the document as one undoable step. On error nothing changes.
    pub fn edit<T, E>(
        &mut self,
        label: &str,
        f: impl FnOnce(&mut Document) -> Result<T, E>,
    ) -> Result<(T, Changed), E> {
        let before = self.doc.clone();
        match f(&mut self.doc) {
            Ok(v) => {
                self.undo.push(Snapshot {
                    doc: before,
                    text: self.text.clone(),
                    rev: self.rev,
                    label: label.into(),
                });
                if self.undo.len() > HISTORY {
                    self.undo.remove(0);
                }
                self.redo.clear();
                self.rev = self.next_rev;
                self.next_rev += 1;
                Ok((v, self.refresh(false)))
            }
            Err(e) => {
                self.doc = before;
                Err(e)
            }
        }
    }

    /// Like [`Session::edit`] for changes to the text of pages (a recognised text layer):
    /// search and selection see the new text.
    pub fn edit_text<T, E>(
        &mut self,
        label: &str,
        f: impl FnOnce(&mut Document) -> Result<T, E>,
    ) -> Result<(T, Changed), E> {
        let out = self.edit(label, f)?;
        self.new_text();
        Ok(out)
    }

    /// A text index for the current document, keeping the text of unchanged pages; the old
    /// one stops indexing.
    fn new_text(&mut self) {
        self.text.retire();
        self.text = TextIndex::open_after(self.doc.clone(), self.refs.clone(), Some(&self.text));
    }

    pub fn undo(&mut self) -> Option<Changed> {
        let s = self.undo.pop()?;
        let cur = Snapshot {
            doc: std::mem::replace(&mut self.doc, s.doc),
            text: std::mem::replace(&mut self.text, s.text),
            rev: self.rev,
            label: s.label,
        };
        cur.text.retire();
        self.rev = s.rev;
        self.redo.push(cur);
        Some(self.refresh(false))
    }

    pub fn redo(&mut self) -> Option<Changed> {
        let s = self.redo.pop()?;
        let cur = Snapshot {
            doc: std::mem::replace(&mut self.doc, s.doc),
            text: std::mem::replace(&mut self.text, s.text),
            rev: self.rev,
            label: s.label,
        };
        cur.text.retire();
        self.rev = s.rev;
        self.undo.push(cur);
        Some(self.refresh(false))
    }

    /// Confirms the owner password, granting full rights in every snapshot.
    pub fn unlock_owner(&mut self, password: &str) -> bool {
        if !self.doc.unlock_owner(password) {
            return false;
        }
        for s in self.undo.iter_mut().chain(self.redo.iter_mut()) {
            s.doc.unlock_owner(password);
        }
        self.remember_password(password);
        true
    }

    pub fn remember_password(&mut self, password: &str) {
        self.passwords.retain(|p| p != password);
        self.passwords.insert(0, password.to_owned());
    }

    /// What a save needs, to run on another thread.
    pub fn save_job(&self, path: PathBuf, mode: SaveMode) -> SaveJob {
        SaveJob {
            doc: self.doc.clone(),
            rev: self.rev,
            passwords: self.passwords.clone(),
            path,
            mode,
            flatten_signatures: false,
            flatten_forms: false,
        }
    }

    /// Takes a finished save: the document now matches the file at `done.path` as of
    /// `done.rev`. When nothing changed since, the reopened file becomes the base.
    pub fn saved(&mut self, done: SaveDone) {
        self.path = Some(done.path);
        self.saved_rev = done.rev;
        if self.rev == done.rev
            && let Some(doc) = done.doc
        {
            self.doc = doc;
            self.refresh(false);
        }
    }

    pub fn info(&self) -> DocInfo {
        info::read(self)
    }

    /// The document's logical structure (tagged PDF), read on first use.
    pub fn struct_tree(&self) -> Option<Arc<StructTree>> {
        self.readings.struct_tree()
    }

    /// Page `i` in reading order, worked out on first use (see
    /// [`crate::reading_order::page_reading`] for the cost) and kept for recent pages until
    /// the document changes.
    pub fn reading(&self, i: usize) -> Arc<PageReading> {
        self.readings.page(i)
    }

    /// The reading order of the current revision, for working it out on another thread.
    pub fn readings(&self) -> Arc<Readings> {
        self.readings.clone()
    }
}

/// The title of a new document made from pages (extract, split).
pub fn file_name(path: Option<&Path>) -> Option<String> {
    Some(path?.file_name()?.to_string_lossy().into_owned())
}

#[derive(Debug)]
pub enum Opened {
    Ready(Box<Session>),
    Locked(PendingUnlock),
}

/// A protected file waiting for its password.
#[derive(Debug)]
pub struct PendingUnlock {
    pub path: Option<PathBuf>,
    locked: Box<Locked>,
}

impl PendingUnlock {
    /// Opens the document, or hands this back for another try.
    pub fn unlock(self, password: &str) -> Result<Session, PendingUnlock> {
        match self.locked.unlock(password) {
            Ok(doc) => {
                let mut s = Session::new(self.path, doc);
                s.remember_password(password);
                Ok(s)
            }
            Err(locked) => Err(PendingUnlock {
                path: self.path,
                locked,
            }),
        }
    }

    pub fn title(&self) -> String {
        file_name(self.path.as_deref()).unwrap_or_else(|| tr!("Untitled"))
    }
}

#[derive(Debug)]
pub enum OpenError {
    Io(std::io::Error),
    Pdf(pagebara_syntax::Error),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::Io(e) if e.kind() == std::io::ErrorKind::NotFound => f.write_str(&tr!(
                "The file is not there any more: it may have been moved, renamed or deleted."
            )),
            OpenError::Io(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                f.write_str(&tr!("You do not have permission to read this file."))
            }
            OpenError::Io(e) => f.write_str(&tr!("The file could not be read: {}", e)),
            OpenError::Pdf(pagebara_syntax::Error::NotPdf) => {
                f.write_str(&tr!("This is not a PDF file."))
            }
            OpenError::Pdf(pagebara_syntax::Error::UnsupportedEncryption) => f.write_str(&tr!(
                "This file uses a kind of protection that Pagebara cannot open yet."
            )),
            OpenError::Pdf(e) => f.write_str(&tr!("This file could not be opened: {}", e)),
        }
    }
}

impl std::error::Error for OpenError {}

pub fn open_file(path: &Path) -> Result<Opened, OpenError> {
    let bytes = std::fs::read(path).map_err(OpenError::Io)?;
    open_bytes(Some(path.to_owned()), bytes)
}

pub fn open_bytes(path: Option<PathBuf>, bytes: Vec<u8>) -> Result<Opened, OpenError> {
    match Document::open(bytes) {
        Ok(doc) => Ok(Opened::Ready(Box::new(Session::new(path, doc)))),
        Err(pagebara_syntax::Error::PasswordRequired(locked)) => {
            Ok(Opened::Locked(PendingUnlock { path, locked }))
        }
        Err(e) => Err(OpenError::Pdf(e)),
    }
}

/// "Protected: printing not allowed", "Protected" or, for unprotected files, empty.
pub fn protection_label(encrypted: bool, p: Permissions) -> String {
    if !encrypted {
        return String::new();
    }
    let denied = denied(p);
    match denied.as_slice() {
        [] => tr!("Protected"),
        // TRANSLATORS: what a protected document forbids, like "printing"
        [one] => tr!("Protected: {} not allowed", one),
        // TRANSLATORS: a list of what a protected document forbids, then the last one
        [init @ .., last] => tr!("Protected: {} and {} not allowed", init.join(", "), last),
    }
}

/// What `p` does not allow, in words.
pub fn denied(p: Permissions) -> Vec<String> {
    [
        // TRANSLATORS: things a protected document may forbid, as in "Protected: printing not allowed"
        (p.print, tr_noop!("printing")),
        (p.copy, tr_noop!("copying")),
        (p.modify, tr_noop!("editing")),
        (p.annotate, tr_noop!("commenting")),
        (p.fill_forms, tr_noop!("filling forms")),
        (p.assemble, tr_noop!("changing pages")),
    ]
    .into_iter()
    .filter(|(allowed, _)| !allowed)
    .map(|(_, what)| pagebara_i18n::translate(what, &[]))
    .collect()
}

const PAPER: [(&str, f64, f64); 9] = [
    ("A3", 297.0, 420.0),
    ("A4", 210.0, 297.0),
    ("A5", 148.0, 210.0),
    ("A6", 105.0, 148.0),
    ("B5", 176.0, 250.0),
    // TRANSLATORS: the US paper size
    (tr_noop!("Letter"), 215.9, 279.4),
    // TRANSLATORS: the US paper size
    (tr_noop!("Legal"), 215.9, 355.6),
    // TRANSLATORS: the US paper size
    (tr_noop!("Tabloid"), 279.4, 431.8),
    // TRANSLATORS: the US paper size
    (tr_noop!("Executive"), 184.15, 266.7),
];

/// "A4 · 210 × 297 mm" for a page of `w` × `h` points.
pub fn page_size_label(w: f64, h: f64) -> String {
    let mm = |pt: f64| pt * 25.4 / 72.0;
    let (wm, hm) = (mm(w), mm(h));
    let (short, long) = (wm.min(hm), wm.max(hm));
    let name = PAPER
        .iter()
        .find(|(_, a, b)| (short - a).abs() < 1.5 && (long - b).abs() < 1.5)
        .map(|(n, _, _)| pagebara_i18n::translate(n, &[]));
    let dims = tr!("{} × {} mm", wm.round(), hm.round());
    match name {
        Some(n) => format!("{n} · {dims}"),
        None => dims,
    }
}

#[cfg(test)]
mod tests;
