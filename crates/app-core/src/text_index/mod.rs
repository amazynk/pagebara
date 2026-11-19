//! The text of pages for search and selection: extracted on first use (from any thread)
//! and kept for the most recently used pages up to a memory budget, plus the searching,
//! hit testing and range helpers built on it.
//!
//! Every page's folded text (see [`fold_text`]) is kept too, a few dozen MB for the largest
//! books, so a search scans text in memory and extracts only the pages with hits. It fills
//! as pages are extracted, in parallel by searches and by [`TextIndex::index_in_background`],
//! and is kept on disk for unmodified documents (see `disk`).

mod carry;
mod disk;
mod fill;

use std::collections::HashMap;
use std::ops::Range;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use pagebara_content::{
    Interpreter, TextBlock, TextChar, TextLine, TextPage, TextWord, extract_text,
};
use pagebara_syntax::{Document, ObjRef};
use parking_lot::{Mutex, RwLock};

use crate::pages;

pub(crate) use fill::boxes_of;
pub use fill::{FindOptions, Query};

/// A box in user space: x0, y0, x1, y1.
pub type Bbox = [f32; 4];

/// Extracted text kept per document revision (a dense page of text takes about 250 KB).
const BUDGET: usize = 48 << 20;

/// How indexes made with [`TextIndex::open`] behave; the app sets it once at start.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Index every page in the background soon after a document opens.
    pub background: bool,
    /// Where the indexes of unmodified documents are kept between runs.
    pub cache_dir: Option<PathBuf>,
}

static CONFIG: RwLock<Config> = RwLock::new(Config {
    background: false,
    cache_dir: None,
});

pub fn configure(config: Config) {
    *CONFIG.write() = config;
}

/// Extracted text of one document revision, page by page.
pub struct TextIndex {
    doc: Document,
    refs: Vec<ObjRef>,
    pages: Mutex<Kept>,
    /// Each page's folded text, once extracted.
    folded: Vec<OnceLock<Box<str>>>,
    indexed: AtomicUsize,
    /// Searches running; background indexing waits for them.
    searches: AtomicUsize,
    /// Set when the index is no longer the document's current one.
    retired: AtomicBool,
    disk: disk::Cache,
}

/// Pages' text with their size and last use, under [`BUDGET`].
#[derive(Default)]
struct Kept {
    map: HashMap<usize, (Arc<TextPage>, usize, u64)>,
    bytes: usize,
    clock: u64,
}

impl std::fmt::Debug for TextIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TextIndex")
            .field("pages", &self.refs.len())
            .field("indexed", &self.indexed())
            .finish_non_exhaustive()
    }
}

impl TextIndex {
    /// An index of its own: nothing in the background, nothing on disk.
    pub fn new(doc: Document, refs: Vec<ObjRef>) -> Self {
        TextIndex {
            folded: refs.iter().map(|_| OnceLock::new()).collect(),
            doc,
            refs,
            pages: Mutex::default(),
            indexed: AtomicUsize::new(0),
            searches: AtomicUsize::new(0),
            retired: AtomicBool::new(false),
            disk: disk::Cache::default(),
        }
    }

    /// An index as the app's [`Config`] says: kept on disk, filled in the background.
    pub fn open(doc: Document, refs: Vec<ObjRef>) -> Arc<Self> {
        Self::open_after(doc, refs, None)
    }

    /// [`TextIndex::open`] for a new revision of `old`'s document: the pages whose text
    /// cannot have changed (the same page object with the same content and resources, as
    /// after rotating, cropping, moving, deleting or inserting pages) keep what `old` knows.
    pub fn open_after(doc: Document, refs: Vec<ObjRef>, old: Option<&TextIndex>) -> Arc<Self> {
        let config = CONFIG.read().clone();
        let mut index = TextIndex::new(doc, refs);
        if let Some(dir) = config.cache_dir {
            index = index.with_cache_dir(dir);
        }
        if let Some(old) = old {
            index.carry_over(old);
        }
        let index = Arc::new(index);
        if config.background && !index.is_empty() {
            index.index_in_background(fill::BACKGROUND_DELAY);
        }
        index
    }

    /// Keeps the index in `dir` between runs, unless the document was changed since it was
    /// read (the text may differ) or is protected (its text stays off the disk).
    pub fn with_cache_dir(mut self, dir: PathBuf) -> Self {
        if !self.doc.is_modified() && !self.doc.is_encrypted() {
            self.disk = disk::Cache::new(dir);
        }
        self
    }

    pub fn len(&self) -> usize {
        self.refs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    /// The text of page `i`, extracted now if needed. Empty for a missing page.
    pub fn page(&self, i: usize) -> Arc<TextPage> {
        if let Some(t) = self.cached(i) {
            return t;
        }
        self.page_with(&mut self.interpreter(), i)
    }

    pub fn cached(&self, i: usize) -> Option<Arc<TextPage>> {
        let mut k = self.pages.lock();
        k.clock += 1;
        let now = k.clock;
        k.map.get_mut(&i).map(|(t, _, used)| {
            *used = now;
            t.clone()
        })
    }

    /// Page `i`'s folded text, when it has been extracted.
    pub fn folded(&self, i: usize) -> Option<&str> {
        self.folded.get(i)?.get().map(|s| &**s)
    }

    /// How many pages' folded text is known.
    pub fn indexed(&self) -> usize {
        self.indexed.load(Ordering::Acquire)
    }

    /// Whether every page's folded text is known.
    pub fn is_indexed(&self) -> bool {
        self.indexed() == self.len()
    }

    /// Stops indexing in the background: the document moved on to another revision.
    pub fn retire(&self) {
        self.retired.store(true, Ordering::Release);
    }

    fn interpreter(&self) -> Interpreter<'_> {
        Interpreter::new(&self.doc)
    }

    /// Like [`TextIndex::page`], with an interpreter that may have read the fonts already.
    fn page_with(&self, interp: &mut Interpreter, i: usize) -> Arc<TextPage> {
        if let Some(t) = self.cached(i) {
            return t;
        }
        let t = self.extract(interp, i);
        self.keep(i, t.clone());
        t
    }

    /// Extracts page `i` (without keeping it), noting its folded text.
    fn extract(&self, interp: &mut Interpreter, i: usize) -> Arc<TextPage> {
        let t = self
            .refs
            .get(i)
            .map(|&r| extract(&self.doc, interp, r))
            .unwrap_or_default();
        self.set_folded(i, fold_text(&t.text));
        Arc::new(t)
    }

    fn set_folded(&self, i: usize, text: String) {
        if let Some(cell) = self.folded.get(i)
            && cell.set(text.into_boxed_str()).is_ok()
        {
            self.indexed.fetch_add(1, Ordering::AcqRel);
        }
    }

    fn keep(&self, i: usize, t: Arc<TextPage>) {
        let size = heap_bytes(&t);
        let mut k = self.pages.lock();
        k.clock += 1;
        let used = k.clock;
        if let Some((_, old, _)) = k.map.insert(i, (t, size, used)) {
            k.bytes -= old;
        }
        k.bytes += size;
        while k.bytes > BUDGET && k.map.len() > 1 {
            let Some(oldest) = k.map.iter().min_by_key(|e| e.1.2).map(|e| *e.0) else {
                break;
            };
            if let Some((_, b, _)) = k.map.remove(&oldest) {
                k.bytes -= b;
            }
        }
    }

    /// Counts a running search until dropped.
    pub(crate) fn searching(&self) -> Searching<'_> {
        self.searches.fetch_add(1, Ordering::AcqRel);
        Searching(&self.searches)
    }
}

pub(crate) struct Searching<'a>(&'a AtomicUsize);

impl Drop for Searching<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The memory a page's text takes, roughly.
fn heap_bytes(t: &TextPage) -> usize {
    use std::mem::size_of;
    256 + t.text.capacity()
        + t.chars.capacity() * size_of::<TextChar>()
        + t.words.capacity() * size_of::<TextWord>()
        + t.lines.capacity() * size_of::<TextLine>()
        + t.blocks.capacity() * size_of::<TextBlock>()
}

fn extract(doc: &Document, interp: &mut Interpreter, r: ObjRef) -> TextPage {
    let Some(o) = doc.get(r) else {
        return TextPage::default();
    };
    let Some(page) = o.as_dict() else {
        return TextPage::default();
    };
    let res = pages::resources(doc, page);
    let list = interp.page(page, &res);
    extract_text(doc, &list)
}

/// Normalized search text and a map from each byte to the source. Runs of whitespace
/// become a space. Case-insensitive searches also lowercase the text.
struct Folded {
    text: String,
    map: Vec<usize>,
}

fn fold(s: &str, match_case: bool) -> Folded {
    let mut text = String::with_capacity(s.len());
    let mut map = Vec::with_capacity(s.len() + 1);
    fold_into(s, match_case, &mut text, Some(&mut map));
    map.push(s.len());
    Folded { text, map }
}

/// `s` folded for matching: lower case, white space runs as one space.
pub fn fold_text(s: &str) -> String {
    let mut text = String::with_capacity(s.len());
    fold_into(s, false, &mut text, None);
    text
}

fn fold_into(s: &str, match_case: bool, text: &mut String, mut map: Option<&mut Vec<usize>>) {
    let mut space = false;
    for (at, c) in s.char_indices() {
        if c.is_whitespace() {
            if !space {
                text.push(' ');
                if let Some(m) = map.as_deref_mut() {
                    m.push(at);
                }
            }
            space = true;
            continue;
        }
        space = false;
        let before = text.len();
        if match_case {
            text.push(c);
        } else {
            text.extend(c.to_lowercase());
        }
        if let Some(m) = map.as_deref_mut() {
            m.extend(std::iter::repeat_n(at, text.len() - before));
        }
    }
}

/// Byte ranges of `page.text` matching `query`, ignoring case and differences in white space.
pub fn find(page: &TextPage, query: &str) -> Vec<Range<usize>> {
    Query::new(query).map_or_else(Vec::new, |q| q.ranges(page))
}

/// Characters covering a byte range of the text.
pub fn chars_of(page: &TextPage, bytes: Range<usize>) -> Range<usize> {
    let a = page
        .chars
        .partition_point(|c| (c.text.1 as usize) <= bytes.start);
    let b = page
        .chars
        .partition_point(|c| (c.text.0 as usize) < bytes.end);
    a..b.max(a)
}

fn bbox_of(q: &[pagebara_content::Point; 4]) -> Bbox {
    let xs = q.map(|p| p.x);
    let ys = q.map(|p| p.y);
    let min = |v: [f32; 4]| v.into_iter().fold(f32::INFINITY, f32::min);
    let max = |v: [f32; 4]| v.into_iter().fold(f32::NEG_INFINITY, f32::max);
    [min(xs), min(ys), max(xs), max(ys)]
}

fn union(a: Bbox, b: Bbox) -> Bbox {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// One box per line for a range of characters, for highlighting.
pub fn line_boxes(page: &TextPage, chars: Range<usize>) -> Vec<Bbox> {
    let mut out = Vec::new();
    for l in &page.lines {
        let (a, b) = (l.chars.0 as usize, l.chars.1 as usize);
        let (s, e) = (a.max(chars.start), b.min(chars.end));
        if s >= e {
            continue;
        }
        let bbox = page.chars[s..e]
            .iter()
            .filter(|c| c.glyph != pagebara_content::display::NONE || c.text.1 > c.text.0)
            .map(|c| bbox_of(&c.quad))
            .reduce(union);
        if let Some(b) = bbox {
            out.push(b);
        }
    }
    out
}

/// The text of a range of characters, lines joined as in the page text.
pub fn text_of(page: &TextPage, chars: Range<usize>) -> String {
    if chars.start >= chars.end || chars.end > page.chars.len() {
        return String::new();
    }
    let start = page.chars[chars.start].text.0 as usize;
    let end = page.chars[chars.end - 1].text.1 as usize;
    page.text[start.min(end)..end].trim_end().to_owned()
}

fn dist(b: &Bbox, x: f32, y: f32) -> f32 {
    let dx = (b[0] - x).max(x - b[2]).max(0.0);
    let dy = (b[1] - y).max(y - b[3]).max(0.0);
    dx.hypot(dy)
}

/// The caret position nearest to a point in user space: an index between characters
/// (0 ..= chars), or `None` on a page without text.
pub fn caret_at(page: &TextPage, x: f32, y: f32) -> Option<usize> {
    let line = page.lines.iter().min_by(|a, b| {
        let da = dist(&a.bbox_arr(), x, y);
        let db = dist(&b.bbox_arr(), x, y);
        da.total_cmp(&db)
    })?;
    let (a, b) = (line.chars.0 as usize, line.chars.1 as usize);
    let mut best = (f32::INFINITY, a);
    for (i, ch) in page.chars.iter().enumerate().take(b).skip(a) {
        let bb = bbox_of(&ch.quad);
        let d = dist(&bb, x, y);
        if d < best.0 {
            let after = if line.vertical {
                y < (bb[1] + bb[3]) / 2.0
            } else {
                x > (bb[0] + bb[2]) / 2.0
            };
            best = (d, if after { i + 1 } else { i });
        }
    }
    Some(best.1)
}

/// Whether a point is on a character (for the text cursor).
pub fn over_text(page: &TextPage, x: f32, y: f32) -> bool {
    page.lines.iter().any(|l| dist(&l.bbox_arr(), x, y) <= 0.0)
}

/// The word around character `i`.
pub fn word_at(page: &TextPage, i: usize) -> Option<Range<usize>> {
    page.words
        .iter()
        .map(|w| w.chars.0 as usize..w.chars.1 as usize)
        .find(|r| r.contains(&i))
}

/// The line around character `i`.
pub fn line_at(page: &TextPage, i: usize) -> Option<Range<usize>> {
    page.lines
        .iter()
        .map(|l| l.chars.0 as usize..l.chars.1 as usize)
        .find(|r| r.contains(&i))
}

trait BboxArr {
    fn bbox_arr(&self) -> Bbox;
}

impl BboxArr for pagebara_content::TextLine {
    fn bbox_arr(&self) -> Bbox {
        [self.bbox.x0, self.bbox.y0, self.bbox.x1, self.bbox.y1]
    }
}

#[cfg(test)]
mod tests;
