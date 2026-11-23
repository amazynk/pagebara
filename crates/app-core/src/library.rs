//! The library: documents the user opened or keeps in chosen folders, with a full-text
//! index for searching across them. Local only: the index is a file in the app's cache
//! directory, brought up to date incrementally (files whose size and modification time
//! changed are read again) on a background thread that can be cancelled.

use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::UNIX_EPOCH;

use pagebara_syntax::Document;
use rayon::prelude::*;
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

/// The indexed files. Each file's entry is shared, so a copy of the index to update in the
/// background costs little.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Index {
    pub files: BTreeMap<PathBuf, Arc<IndexedFile>>,
}

/// What an update did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpdateStats {
    pub read: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub cancelled: bool,
}

/// A document that matches a search.
#[derive(Clone, Debug, PartialEq)]
pub struct Found {
    pub path: PathBuf,
    /// Its file name or title has every word of the query.
    pub named: bool,
    /// The first page whose text has the query (0-based), for a document not `named`.
    pub page: Option<usize>,
    /// How many pages have the query.
    pub pages: usize,
    /// The text around the first match, and where in it the match is.
    pub snippet: String,
    pub mark: std::ops::Range<usize>,
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
    /// ones dropped. `progress(done, total, index so far)` is called after each file.
    pub fn update(
        &mut self,
        files: &[PathBuf],
        cancel: &AtomicBool,
        mut progress: impl FnMut(usize, usize, &Index),
    ) -> UpdateStats {
        let mut stats = UpdateStats::default();
        let before = self.files.len();
        let wanted: HashSet<&PathBuf> = files.iter().collect();
        self.files.retain(|p, _| wanted.contains(p));
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
                self.files.insert(path.clone(), Arc::new(f));
                stats.read += 1;
            }
            progress(n + 1, files.len(), self);
        }
        stats
    }

    /// The documents matching `query`, case and white space ignored: first those whose file
    /// name or title has each of its words, then those whose text has it. Each group lists
    /// the documents in `recent` first (in that order), then those with more matching
    /// pages, then by name, at most `limit` of them. `None` when `stop` was set on the way.
    pub fn find(
        &self,
        query: &str,
        recent: &[PathBuf],
        limit: usize,
        stop: &AtomicBool,
    ) -> Option<Vec<Found>> {
        let q = fold(query);
        let q = q.trim();
        if q.is_empty() {
            return Some(Vec::new());
        }
        let words: Vec<&str> = q.split(' ').collect();
        let needle = Needle::new(q);
        let files: Vec<(&PathBuf, &Arc<IndexedFile>)> = self.files.iter().collect();
        let mut found: Vec<Found> = files
            .par_iter()
            .filter_map(|&(path, f)| {
                if stop.load(Ordering::Relaxed) {
                    return None;
                }
                let name = fold(&format!("{} {}", stem(path), f.title));
                if words.iter().all(|w| name.contains(w)) {
                    return Some(Found {
                        path: path.clone(),
                        named: true,
                        page: None,
                        pages: 0,
                        snippet: String::new(),
                        mark: 0..0,
                    });
                }
                let mut first = None;
                let mut pages = 0;
                for (i, text) in f.pages.iter().enumerate() {
                    if let Some(at) = needle.find(text) {
                        pages += 1;
                        first.get_or_insert((i, at));
                    }
                }
                let (page, (start, end)) = first?;
                let (snippet, mark) = snippet(&f.pages[page], start, end);
                Some(Found {
                    path: path.clone(),
                    named: false,
                    page: Some(page),
                    pages,
                    snippet,
                    mark,
                })
            })
            .collect();
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let order: BTreeMap<&PathBuf, usize> =
            recent.iter().enumerate().map(|(i, p)| (p, i)).collect();
        let rank = |f: &Found| order.get(&f.path).copied().unwrap_or(usize::MAX);
        found.sort_by_cached_key(|f| {
            (
                !f.named,
                rank(f),
                std::cmp::Reverse(f.pages),
                stem(&f.path).to_lowercase(),
                f.path.clone(),
            )
        });
        let named = found.iter().filter(|f| f.named).count();
        let mut text = found.split_off(named);
        found.truncate(limit);
        text.truncate(limit);
        found.append(&mut text);
        Some(found)
    }
}

/// A file's name without its extension.
pub fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
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

/// Folded text to find in page text: letters in any case, any run of white space for a
/// space. Without allocating, as a search reads every page of every document.
struct Needle {
    chars: Vec<char>,
    /// The bytes, when they are all ASCII (most searches): then the text is compared byte
    /// by byte.
    ascii: Option<Vec<u8>>,
}

impl Needle {
    /// `q` is folded and trimmed.
    fn new(q: &str) -> Self {
        Needle {
            chars: q.chars().collect(),
            ascii: q.is_ascii().then(|| q.as_bytes().to_vec()),
        }
    }

    /// The byte range of the first match in `text` that starts a word.
    fn find(&self, text: &str) -> Option<(usize, usize)> {
        let first = self.chars[0];
        if !first.is_ascii() {
            return text.char_indices().find_map(|(i, c)| {
                let starts = c.to_lowercase().next() == Some(first) && word_start(text, i, first);
                starts
                    .then(|| self.chars_at(text, i))
                    .flatten()
                    .map(|end| (i, end))
            });
        }
        // Candidates are where the first letter is, in either case.
        let t = text.as_bytes();
        let b = first as u8;
        let mut from = 0;
        while from < t.len() {
            let i = from
                + if b.is_ascii_alphabetic() {
                    memchr::memchr2(b, b.to_ascii_uppercase(), &t[from..])?
                } else {
                    memchr::memchr(b, &t[from..])?
                };
            let end = match &self.ascii {
                Some(q) => Self::bytes_at(text, i, q),
                None => self.chars_at(text, i),
            };
            if let Some(end) = end
                && word_start(text, i, first)
            {
                return Some((i, end));
            }
            from = i + 1;
        }
        None
    }

    /// Where an ASCII match starting at `i` ends.
    fn bytes_at(text: &str, mut i: usize, q: &[u8]) -> Option<usize> {
        let t = text.as_bytes();
        for &b in q {
            if b == b' ' {
                let mut n = space_at(text, i);
                if n == 0 {
                    return None;
                }
                while n > 0 {
                    i += n;
                    n = space_at(text, i);
                }
            } else if t.get(i).is_some_and(|c| c.to_ascii_lowercase() == b) {
                i += 1;
            } else {
                return None;
            }
        }
        Some(i)
    }

    /// Where a match starting at `i` ends, comparing characters.
    fn chars_at(&self, text: &str, start: usize) -> Option<usize> {
        let q = &self.chars;
        let mut it = text[start..].char_indices().peekable();
        let mut k = 0;
        while k < q.len() {
            let (_, c) = it.next()?;
            if q[k] == ' ' {
                if !c.is_whitespace() {
                    return None;
                }
                while it.next_if(|&(_, c)| c.is_whitespace()).is_some() {}
                k += 1;
                continue;
            }
            for l in c.to_lowercase() {
                if q.get(k) != Some(&l) {
                    return None;
                }
                k += 1;
            }
        }
        Some(it.peek().map_or(text.len(), |&(o, _)| start + o))
    }
}

/// Whether a match of a query starting with `first` at byte `i` starts a word, so that
/// "lease" does not find "please". Scripts written without spaces between words (Chinese,
/// Japanese, Thai and others) match anywhere.
fn word_start(text: &str, i: usize, first: char) -> bool {
    let Some(prev) = text[..i].chars().next_back() else {
        return true;
    };
    let unspaced = |c: char| {
        matches!(u32::from(c),
            0x0E00..=0x0EFF | 0x1000..=0x109F | 0x1780..=0x17FF | 0x3040..=0x30FF
            | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF | 0x20000..=0x3FFFF)
    };
    !(prev.is_alphanumeric() && first.is_alphanumeric()) || unspaced(prev) || unspaced(first)
}

/// The length in bytes of the white space character at byte `i`, or 0.
fn space_at(text: &str, i: usize) -> usize {
    match text.as_bytes().get(i) {
        Some(&b) if b < 0x80 => usize::from((b as char).is_whitespace()),
        Some(_) => text
            .get(i..)
            .and_then(|s| s.chars().next())
            .filter(|c| c.is_whitespace())
            .map_or(0, char::len_utf8),
        None => 0,
    }
}

/// The text around bytes `start..end` on one line, and where those are in it.
fn snippet(text: &str, start: usize, end: usize) -> (String, std::ops::Range<usize>) {
    let mut from = text[..start]
        .char_indices()
        .rev()
        .nth(SNIPPET - 1)
        .map_or(0, |(i, _)| i);
    let mut to = text[end..]
        .char_indices()
        .nth(SNIPPET)
        .map_or(text.len(), |(i, _)| end + i);
    // Whole words only, where there is a space to cut at.
    if from > 0
        && let Some(i) = text[from..start].find(char::is_whitespace)
    {
        from += i;
    }
    if to < text.len()
        && let Some(i) = text[end..to].rfind(char::is_whitespace)
    {
        to = end + i;
    }
    let one_line = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut before = one_line(&text[from..start]);
    if from > 0 {
        before.insert(0, '…');
    }
    if !before.is_empty() && text[..start].ends_with(char::is_whitespace) {
        before.push(' ');
    }
    let matched = one_line(&text[start..end]);
    let mut after = one_line(&text[end..to]);
    if !after.is_empty() && text[end..].starts_with(char::is_whitespace) {
        after.insert(0, ' ');
    }
    if to < text.len() {
        after.push('…');
    }
    let mark = before.len()..before.len() + matched.len();
    (before + &matched + &after, mark)
}

/// A small picture of a document's first page, as straight RGBA.
#[derive(Clone, Debug)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// The first page of `path`, `size` pixels on its longer side. Kept as a PNG in `cache`,
/// named for the file's path, size and modification time, so a changed file gets a new
/// one. `None` for a file that cannot be read (damaged, or needing a password).
pub fn thumbnail(path: &Path, size: u32, cache: Option<&Path>) -> Option<Thumbnail> {
    use sha2::{Digest, Sha256};
    let (len, mtime) = stamp(path)?;
    let key = cache.map(|dir| {
        let mut h = Sha256::new();
        h.update(path.as_os_str().as_encoded_bytes());
        h.update(len.to_le_bytes());
        h.update(mtime.to_le_bytes());
        h.update(size.to_le_bytes());
        let hex: String = h.finalize().iter().map(|b| format!("{b:02x}")).collect();
        dir.join(format!("{hex}.png"))
    });
    if let Some(bytes) = key.as_ref().and_then(|k| std::fs::read(k).ok()) {
        let opts = pagebara_png::DecodeOptions {
            output: pagebara_png::Output::Rgba8,
            ..Default::default()
        };
        if let Ok(img) = pagebara_png::decode(&bytes, &opts) {
            return Some(Thumbnail {
                width: img.width,
                height: img.height,
                rgba: img.to_rgba8(),
            });
        }
    }
    let doc = Document::open(std::fs::read(path).ok()?).ok()?;
    let r = pagebara_render::Renderer::new(doc);
    let (w, h) = r.page_size(0)?;
    let scale = size as f32 / w.max(h).max(1.0);
    let (pw, ph) = (
        ((w * scale).round() as u32).clamp(1, size),
        ((h * scale).round() as u32).clamp(1, size),
    );
    let req = pagebara_render::RenderRequest::new(
        scale,
        pagebara_render::PixelRect {
            x: 0,
            y: 0,
            w: pw,
            h: ph,
        },
    );
    let img = r.render(0, &req).ok()?;
    let t = Thumbnail {
        width: img.width,
        height: img.height,
        rgba: img.data,
    };
    if let Some(k) = &key
        && let Ok(png) =
            pagebara_png::encode(t.width, t.height, pagebara_png::ColorType::Rgba, &t.rgba)
    {
        let saved = k
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| crate::session::write_atomic(k, &png));
        if let Err(e) = saved {
            log::warn!("could not keep a library thumbnail: {e}");
        }
    }
    Some(t)
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
        let st = idx.update(&files, &cancel, |_, _, _| calls += 1);
        assert_eq!((st.read, st.unchanged, calls), (3, 0, 3));
        assert!(idx.files[&docs.join("broken.pdf")].error.is_some());
        idx.save(&dir).unwrap();

        let mut idx = Index::load(&dir);
        let none = AtomicBool::new(false);
        let hits = idx.find("QUICK   brown", &[], 10, &none).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].path, docs.join("a.pdf"));
        assert_eq!(hits[0].page, Some(0));
        assert!(hits[0].snippet.contains("quick brown fox"), "{:?}", hits[0]);
        assert!(idx.find("", &[], 10, &none).unwrap().is_empty());

        // Only the changed file is read again; a removed one is dropped.
        write(&docs.join("sub/b.PDF"), &["Now with a quick mention"]);
        std::fs::remove_file(docs.join("broken.pdf")).unwrap();
        let files = collect(&cfg.folders, &[]);
        let st = idx.update(&files, &cancel, |_, _, _| {});
        assert_eq!((st.read, st.unchanged, st.removed), (1, 1, 1));
        assert_eq!(idx.find("quick", &[], 10, &none).unwrap().len(), 2);

        cancel.store(true, Ordering::Relaxed);
        let st = Index::default().update(&files, &cancel, |_, _, _| {});
        assert!(st.cancelled);
        std::fs::remove_dir_all(dir).unwrap();
    }

    fn file(title: &str, pages: &[&str]) -> Arc<IndexedFile> {
        Arc::new(IndexedFile {
            title: title.into(),
            pages: pages.iter().map(|p| p.to_string()).collect(),
            ..IndexedFile::default()
        })
    }

    #[test]
    fn finds_names_then_text_ranked() {
        let mut idx = Index::default();
        let p = |n: &str| PathBuf::from(format!("/docs/{n}.pdf"));
        idx.files
            .insert(p("annual_report_2023"), file("", &["Nothing here"]));
        idx.files
            .insert(p("scan001"), file("Board minutes", &["Revenue grew"]));
        idx.files.insert(
            p("a-one"),
            file("", &["revenue  grew", "x", "Revenue\ngrew"]),
        );
        idx.files
            .insert(p("b-two"), file("", &["it said: REVENUE GREW."]));
        idx.files
            .insert(p("c-three"), file("", &["revenue fell", "revenue grew"]));
        let stop = AtomicBool::new(false);
        let names = |found: &[Found]| -> Vec<String> {
            found
                .iter()
                .map(|f| {
                    format!(
                        "{}{}",
                        stem(&f.path),
                        f.page.map_or(String::new(), |p| format!(" p{}", p + 1))
                    )
                })
                .collect()
        };

        // Each word of a name, in any order; a title counts as a name.
        let found = idx.find("report annual", &[], 10, &stop).unwrap();
        assert_eq!(names(&found), ["annual_report_2023"]);
        assert!(found[0].named);
        let found = idx.find("minutes", &[], 10, &stop).unwrap();
        assert_eq!(names(&found), ["scan001"]);

        // Text: more matching pages first, then by name; a recent document before them.
        let found = idx.find("revenue grew", &[], 10, &stop).unwrap();
        assert_eq!(
            names(&found),
            ["a-one p1", "b-two p1", "c-three p2", "scan001 p1"]
        );
        assert_eq!(found[0].pages, 2);
        let found = idx
            .find("revenue grew", &[p("c-three")], 10, &stop)
            .unwrap();
        assert_eq!(
            names(&found),
            ["c-three p2", "a-one p1", "b-two p1", "scan001 p1"]
        );
        let f = &found[1];
        assert_eq!(&f.snippet[f.mark.clone()], "revenue grew");

        // Names come before text, and each group has its own limit.
        let found = idx.find("two", &[], 10, &stop).unwrap();
        assert_eq!(names(&found), ["b-two"]);
        assert_eq!(idx.find("revenue", &[], 2, &stop).unwrap().len(), 2);

        stop.store(true, Ordering::Relaxed);
        assert!(idx.find("revenue", &[], 10, &stop).is_none());
    }

    #[test]
    fn needles_ignore_case_and_white_space() {
        fn find<'a>(text: &'a str, q: &str) -> Option<&'a str> {
            let q = fold(q);
            Needle::new(q.trim()).find(text).map(|(a, b)| &text[a..b])
        }
        assert_eq!(
            find("Alpha  beta\nGamma", "BETA gamma"),
            Some("beta\nGamma")
        );
        assert_eq!(
            find("non\u{a0}breaking", "non breaking"),
            Some("non\u{a0}breaking")
        );
        assert_eq!(find("Größe und Maße", "MASSE"), None);
        assert_eq!(find("Größe und Maße", "GRÖSSE"), None);
        assert_eq!(find("Die GRÖẞE", "größe"), Some("GRÖẞE"));
        assert_eq!(find("ÀÉÎ café", "Café"), Some("café"));
        assert_eq!(find("über Ärger", "ärger"), Some("Ärger"));
        assert_eq!(find("bee be", " be "), Some("be"));
        // Matches start words, except in scripts without spaces.
        assert_eq!(find("aab", "ab"), None);
        assert_eq!(find("Please sign the lease", "lease"), Some("lease"));
        assert_eq!(find("a-ab", "ab"), Some("ab"));
        assert_eq!(find("(ab)", "ab"), Some("ab"));
        assert_eq!(find("café", "fé"), None);
        assert_eq!(find("東京都の地図", "地図"), Some("地図"));
        assert_eq!(find("พจนานุกรม", "นานุ"), Some("นานุ"));
        assert_eq!(find("a b", "ab"), None);
        assert_eq!(find("end", "end"), Some("end"));
        assert_eq!(find("ends", "endss"), None);
    }

    #[test]
    fn snippets_stay_on_one_line() {
        let t = "Alpha  beta\nGamma delta";
        let (s, m) = snippet(t, 7, 17);
        assert_eq!(s, "Alpha beta Gamma delta");
        assert_eq!(&s[m], "beta Gamma");
        let long = "x ".repeat(100) + "needle" + &" y".repeat(100);
        let at = long.find("needle").unwrap();
        let (s, m) = snippet(&long, at, at + 6);
        assert!(s.starts_with('…') && s.ends_with('…'), "{s}");
        assert_eq!(&s[m], "needle");
        let (s, m) = snippet("ééé needle", 7, 13);
        assert_eq!((s.as_str(), &s[m]), ("ééé needle", "needle"));
        // Cut at whole words.
        let t = "subscriptions doubled. The outlook for the fourth quarter is cautious: \
                 winter is our quiet season, so revenue will fall";
        let at = t.find("fourth").unwrap();
        let (s, _) = snippet(t, at, at + 14);
        assert_eq!(
            s,
            "…doubled. The outlook for the fourth quarter is cautious: winter is our quiet…"
        );
    }

    #[test]
    fn thumbnails_are_kept() {
        let dir = temp("thumbs");
        let pdf = dir.join("a.pdf");
        std::fs::write(&pdf, crate::test_pdf::text(&["Hello"])).unwrap();
        let cache = dir.join("cache");
        let t = thumbnail(&pdf, 64, Some(&cache)).unwrap();
        assert_eq!(t.width.max(t.height), 64);
        assert_eq!(t.rgba.len(), (t.width * t.height * 4) as usize);
        assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
        let again = thumbnail(&pdf, 64, Some(&cache)).unwrap();
        assert_eq!(
            (again.width, again.height, again.rgba),
            (t.width, t.height, t.rgba)
        );
        std::fs::write(dir.join("b.pdf"), "not a pdf").unwrap();
        assert!(thumbnail(&dir.join("b.pdf"), 64, Some(&cache)).is_none());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
