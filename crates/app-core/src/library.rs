//! The library: documents the user opened or keeps in chosen folders, with a full-text
//! index for searching across them. Local only: the index is a file in the app's cache
//! directory, brought up to date incrementally (files whose size and modification time
//! changed are read again) on a background thread that can be cancelled.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use pagebara_syntax::Document;
use serde::{Deserialize, Serialize};

/// Folders are searched this deep for PDF files.
const MAX_DEPTH: usize = 8;
/// At most this many files are indexed.
const MAX_FILES: usize = 20_000;
/// Text kept per page, in bytes.
const MAX_PAGE_TEXT: usize = 64 << 10;
/// Characters of context on each side of a hit.
const SNIPPET: usize = 40;

const CONFIG: &str = "library.json";
const INDEX: &str = "library-index.json";

/// The folders the user added.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LibraryConfig {
    pub folders: Vec<PathBuf>,
}

impl LibraryConfig {
    pub fn load(dir: &Path) -> Self {
        std::fs::read(dir.join(CONFIG))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_vec_pretty(self).map_err(std::io::Error::other)?;
        crate::session::write_atomic(&dir.join(CONFIG), &json)
    }

    /// Adds a folder; false when it is already there.
    pub fn add(&mut self, folder: &Path) -> bool {
        let f = std::path::absolute(folder).unwrap_or_else(|_| folder.to_owned());
        if self.folders.contains(&f) {
            return false;
        }
        self.folders.push(f);
        true
    }

    pub fn remove(&mut self, folder: &Path) {
        self.folders.retain(|f| f != folder);
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct IndexedFile {
    pub size: u64,
    /// Modification time in nanoseconds since the epoch.
    pub mtime: u128,
    pub title: String,
    /// The text of each page.
    pub pages: Vec<String>,
    /// Why the text could not be read (a password, a damaged file).
    pub error: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Index {
    pub files: BTreeMap<PathBuf, IndexedFile>,
}

/// What an update did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateStats {
    pub read: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub cancelled: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub path: PathBuf,
    pub title: String,
    /// 0-based.
    pub page: usize,
    pub snippet: String,
}

impl Index {
    pub fn load(dir: &Path) -> Self {
        std::fs::read(dir.join(INDEX))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let json = serde_json::to_vec(self).map_err(std::io::Error::other)?;
        crate::session::write_atomic(&dir.join(INDEX), &json)
    }

    /// Brings the index up to date with `files`: new and changed files are read, missing
    /// ones dropped. `progress(done, total)` is called after each file.
    pub fn update(
        &mut self,
        files: &[PathBuf],
        cancel: &AtomicBool,
        mut progress: impl FnMut(usize, usize),
    ) -> UpdateStats {
        let mut stats = UpdateStats::default();
        let before = self.files.len();
        self.files.retain(|p, _| files.contains(p));
        stats.removed = before - self.files.len();
        for (n, path) in files.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                stats.cancelled = true;
                break;
            }
            let Some((size, mtime)) = stamp(path) else {
                self.files.remove(path);
                continue;
            };
            if self
                .files
                .get(path)
                .is_some_and(|f| f.size == size && f.mtime == mtime)
            {
                stats.unchanged += 1;
            } else {
                let mut f = read(path);
                f.size = size;
                f.mtime = mtime;
                self.files.insert(path.clone(), f);
                stats.read += 1;
            }
            progress(n + 1, files.len());
        }
        stats
    }

    /// Pages matching `query` (case and white space ignored), at most `limit`.
    pub fn search(&self, query: &str, limit: usize) -> Vec<Hit> {
        let q = fold(query);
        let mut out = Vec::new();
        if q.trim().is_empty() {
            return out;
        }
        for (path, f) in &self.files {
            for (page, text) in f.pages.iter().enumerate() {
                let Some(at) = find_folded(text, &q) else {
                    continue;
                };
                out.push(Hit {
                    path: path.clone(),
                    title: f.title.clone(),
                    page,
                    snippet: snippet(text, at, q.chars().count()),
                });
                if out.len() >= limit {
                    return out;
                }
            }
        }
        out
    }
}

fn stamp(path: &Path) -> Option<(u64, u128)> {
    let m = std::fs::metadata(path).ok()?;
    let mtime = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    Some((m.len(), mtime))
}

/// Reads a file's title and page text.
fn read(path: &Path) -> IndexedFile {
    let name = crate::session::file_name(Some(path)).unwrap_or_default();
    let fail = |e: String| IndexedFile {
        title: name.clone(),
        error: Some(e),
        ..IndexedFile::default()
    };
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => return fail(e.to_string()),
    };
    let doc = match Document::open(bytes) {
        Ok(d) => d,
        Err(pagebara_syntax::Error::PasswordRequired(_)) => {
            return fail(pagebara_i18n::tr!("needs a password"));
        }
        Err(e) => return fail(e.to_string()),
    };
    let s = crate::session::Session::new(Some(path.to_owned()), doc);
    let pages = (0..s.page_count())
        .map(|i| {
            let mut t = s.text.page(i).text.clone();
            if t.len() > MAX_PAGE_TEXT {
                let mut end = MAX_PAGE_TEXT;
                while !t.is_char_boundary(end) {
                    end -= 1;
                }
                t.truncate(end);
            }
            t
        })
        .collect();
    IndexedFile {
        title: s.title(),
        pages,
        ..IndexedFile::default()
    }
}

/// The PDF files in `folders` (recursively, hidden entries skipped) and `files`, each once.
pub fn collect(folders: &[PathBuf], files: &[PathBuf]) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    for f in folders {
        walk(f, 0, &mut out);
    }
    for f in files {
        if f.is_file() && !out.contains(f) {
            out.push(f.clone());
        }
    }
    out.truncate(MAX_FILES);
    out
}

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        let hidden = p
            .file_name()
            .is_some_and(|n| n.to_string_lossy().starts_with('.'));
        if hidden {
            continue;
        }
        // Symbolic links to folders are not followed, so loops cannot happen.
        let Ok(meta) = std::fs::symlink_metadata(&p) else {
            continue;
        };
        if meta.is_dir() {
            walk(&p, depth + 1, out);
        } else if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))
            && std::fs::metadata(&p).is_ok_and(|m| m.is_file())
        {
            out.push(p);
        }
    }
}

/// Lower case, white space runs as one space.
fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c.is_whitespace() {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            space = false;
            out.extend(c.to_lowercase());
        }
    }
    out
}

/// The character index in `text` where folded `q` starts.
fn find_folded(text: &str, q: &str) -> Option<usize> {
    let q = q.trim();
    let f = fold(text);
    let byte = f.find(q)?;
    // Folding keeps one character per character except for some case mappings and white
    // space runs; map back by counting characters of the folded text.
    let folded_chars = f[..byte].chars().count();
    let mut seen = 0;
    let mut space = false;
    for (i, c) in text.chars().enumerate() {
        if seen >= folded_chars && !(space && c.is_whitespace()) {
            return Some(i);
        }
        if c.is_whitespace() {
            if !space {
                seen += 1;
            }
            space = true;
        } else {
            space = false;
            seen += c.to_lowercase().count();
        }
    }
    Some(text.chars().count())
}

fn snippet(text: &str, at: usize, len: usize) -> String {
    let chars: Vec<char> = text.chars().collect();
    let start = at.saturating_sub(SNIPPET);
    let end = (at + len + SNIPPET).min(chars.len());
    let mut s: String = chars[start..end]
        .iter()
        .map(|&c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if start > 0 {
        s.insert(0, '…');
    }
    if end < chars.len() {
        s.push('…');
    }
    s
}

/// Where the library keeps its files: `profile` when given, else the per-user cache dir.
pub fn default_dir() -> Option<PathBuf> {
    pagebara_identity::cache_dir()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("pagebara-lib-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn indexes_folders_incrementally_and_searches() {
        let dir = temp("idx");
        let docs = dir.join("docs");
        std::fs::create_dir_all(docs.join("sub")).unwrap();
        std::fs::create_dir_all(docs.join(".hidden")).unwrap();
        let write = |p: &Path, lines: &[&str]| {
            std::fs::write(p, crate::test_pdf::text(lines)).unwrap();
        };
        write(&docs.join("a.pdf"), &["The quick brown fox", "jumps over"]);
        write(&docs.join("sub/b.PDF"), &["Nothing to see"]);
        write(&docs.join(".hidden/c.pdf"), &["quick"]);
        std::fs::write(docs.join("notes.txt"), "quick").unwrap();
        std::fs::write(docs.join("broken.pdf"), "not a pdf").unwrap();

        let mut cfg = LibraryConfig::default();
        assert!(cfg.add(&docs));
        assert!(!cfg.add(&docs));
        cfg.save(&dir).unwrap();
        assert_eq!(LibraryConfig::load(&dir), cfg);

        let files = collect(&cfg.folders, &[]);
        let names: Vec<String> = files
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.pdf", "broken.pdf", "b.PDF"]);

        let mut idx = Index::default();
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let st = idx.update(&files, &cancel, |_, _| calls += 1);
        assert_eq!((st.read, st.unchanged, calls), (3, 0, 3));
        assert!(idx.files[&docs.join("broken.pdf")].error.is_some());
        idx.save(&dir).unwrap();

        let mut idx = Index::load(&dir);
        let hits = idx.search("QUICK   brown", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, docs.join("a.pdf"));
        assert_eq!(hits[0].page, 0);
        assert!(hits[0].snippet.contains("quick brown fox"), "{:?}", hits[0]);
        assert!(idx.search("", 10).is_empty());

        // Only the changed file is read again; a removed one is dropped.
        write(&docs.join("sub/b.PDF"), &["Now with a quick mention"]);
        std::fs::remove_file(docs.join("broken.pdf")).unwrap();
        let files = collect(&cfg.folders, &[]);
        let st = idx.update(&files, &cancel, |_, _| {});
        assert_eq!((st.read, st.unchanged, st.removed), (1, 1, 1));
        assert_eq!(idx.search("quick", 10).len(), 2);

        cancel.store(true, Ordering::Relaxed);
        let st = Index::default().update(&files, &cancel, |_, _| {});
        assert!(st.cancelled);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn snippets_and_folding() {
        let t = "Alpha  beta\nGamma delta";
        assert_eq!(find_folded(t, &fold("beta gamma")), Some(7));
        assert_eq!(snippet(t, 7, 10), "Alpha beta Gamma delta");
        let long = "x ".repeat(100) + "needle" + &" y".repeat(100);
        let at = find_folded(&long, "needle").unwrap();
        let s = snippet(&long, at, 6);
        assert!(s.starts_with('…') && s.ends_with('…') && s.contains("needle"));
    }
}
