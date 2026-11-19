//! Comparing two PDFs.
//!
//! - **Text**: the words of both documents (in reading order, page after page) are
//!   diffed as one sequence, so text that flows onto another page is not a change.
//!   Deleted runs that come back elsewhere unchanged are moves.
//! - **Pages** are paired by aligning their text (inserted and deleted pages stand out),
//!   for side-by-side views and the visual comparison.
//! - **Visual**: paired pages are rendered and compared pixel by pixel with a tolerance;
//!   pages drawn from identical objects are skipped. Differences explained by text changes
//!   are not listed again.
//! - **Annotations, document information and form values** are compared too.
//!
//! Places are rectangles on the pages as shown, in points from the top left.
//! [`report`] writes the comparison as a PDF.

#![forbid(unsafe_code)]

mod align;
mod diff;
mod meta;
mod report;
mod text;
mod visual;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Instant;

use pagebara_i18n::{tr, trn};
use pagebara_pages::PageTree;
use pagebara_render::Renderer;
use pagebara_syntax::Document;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

pub use align::align;
pub use diff::{Edit, diff};
pub use report::{ReportOptions, report};
pub use text::{Box4, DocText, Word};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub text: bool,
    pub visual: bool,
    pub annotations: bool,
    pub metadata: bool,
    pub forms: bool,
    /// Resolution of the visual comparison.
    pub dpi: f32,
    /// Channel difference (0–255) below which pixels count as the same.
    pub tolerance: u8,
    /// Deleted runs of at least this many words found again elsewhere are moves.
    pub min_moved_words: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            text: true,
            visual: true,
            annotations: true,
            metadata: true,
            forms: true,
            dpi: 50.0,
            tolerance: 48,
            min_moved_words: 3,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeKind {
    Inserted,
    Deleted,
    Replaced,
    Moved,
    PageInserted,
    PageDeleted,
    Visual,
    Annotation,
    Metadata,
    Form,
}

/// The filters of the change list.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    Text,
    Visual,
    Annotations,
    Document,
}

impl ChangeKind {
    pub fn category(self) -> Category {
        match self {
            ChangeKind::Inserted
            | ChangeKind::Deleted
            | ChangeKind::Replaced
            | ChangeKind::Moved
            | ChangeKind::PageInserted
            | ChangeKind::PageDeleted => Category::Text,
            ChangeKind::Visual => Category::Visual,
            ChangeKind::Annotation => Category::Annotations,
            ChangeKind::Metadata | ChangeKind::Form => Category::Document,
        }
    }

    pub fn label(self) -> String {
        match self {
            // TRANSLATORS: a kind of difference between two documents
            ChangeKind::Inserted => tr!("Inserted"),
            ChangeKind::Deleted => tr!("Deleted"),
            ChangeKind::Replaced => tr!("Changed"),
            ChangeKind::Moved => tr!("Moved"),
            ChangeKind::PageInserted => tr!("Page inserted"),
            ChangeKind::PageDeleted => tr!("Page deleted"),
            ChangeKind::Visual => tr!("Looks different"),
            ChangeKind::Annotation => tr!("Annotation"),
            // TRANSLATORS: a kind of difference between two documents: document properties
            ChangeKind::Metadata => tr!("Properties"),
            ChangeKind::Form => tr!("Form field"),
        }
    }
}

/// One difference. Pages are 0-based; for an insertion, `page_a` is where it goes in the
/// old document (and the other way round for a deletion).
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Change {
    pub kind: ChangeKind,
    pub page_a: Option<usize>,
    pub page_b: Option<usize>,
    /// Places in the old document: (page, rectangle).
    pub rects_a: Vec<(usize, Box4)>,
    /// Places in the new document.
    pub rects_b: Vec<(usize, Box4)>,
    pub old: String,
    pub new: String,
    pub description: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Summary {
    pub words_inserted: usize,
    pub words_deleted: usize,
    pub words_moved: usize,
    pub pages_inserted: usize,
    pub pages_deleted: usize,
    pub pages_looking_different: usize,
    pub annotations: usize,
    pub metadata: usize,
    pub forms: usize,
    pub changes: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Timing {
    pub text_ms: u64,
    pub align_ms: u64,
    pub diff_ms: u64,
    pub visual_ms: u64,
    pub total_ms: u64,
    pub pages_rendered: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct Comparison {
    pub pages_a: usize,
    pub pages_b: usize,
    pub words_a: usize,
    pub words_b: usize,
    /// Paired pages in the order of both documents.
    pub pairs: Vec<(Option<usize>, Option<usize>)>,
    pub changes: Vec<Change>,
    pub summary: Summary,
    pub timing: Timing,
    pub cancelled: bool,
}

impl Comparison {
    /// The pair index showing page `p` of the old (`side` 0) or new (1) document.
    pub fn pair_of(&self, side: usize, p: usize) -> Option<usize> {
        self.pairs.iter().position(|&(a, b)| {
            if side == 0 {
                a == Some(p)
            } else {
                b == Some(p)
            }
        })
    }
}

/// Stages reported to the progress callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Text,
    Visual,
}

pub(crate) fn shorten(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut t: String = s.chars().take(max).collect();
    t.push('…');
    t
}

/// Compares `a` (old) with `b` (new), extracting their text.
pub fn compare(
    a: &Document,
    b: &Document,
    opts: &Options,
    progress: &(dyn Fn(Stage, usize, usize) + Sync),
    cancel: Option<&AtomicBool>,
) -> Comparison {
    let t0 = Instant::now();
    let (ta, tb) = rayon::join(|| DocText::extract(a), || DocText::extract(b));
    let mut c = compare_texts(a, b, &ta, &tb, opts, progress, cancel);
    c.timing.text_ms = t0.elapsed().as_millis() as u64 - c.timing.total_ms;
    c.timing.total_ms = t0.elapsed().as_millis() as u64;
    c
}

fn page_of(t: &DocText, i: usize) -> Option<usize> {
    if t.words.is_empty() {
        return None;
    }
    Some(t.words[i.min(t.words.len() - 1)].page as usize)
}

/// Rectangles of words, one per line of consecutive words.
fn word_rects(words: &[Word]) -> Vec<(usize, Box4)> {
    let mut out: Vec<(usize, Box4)> = Vec::new();
    for w in words {
        let r = w.rect;
        if let Some((p, last)) = out.last_mut()
            && *p == w.page as usize
        {
            let h = (r[3] - r[1]).max(last[3] - last[1]).max(1.0);
            let overlap = r[3].min(last[3]) - r[1].max(last[1]);
            let gap = r[0] - last[2];
            if overlap > 0.5 * h && gap < 2.0 * h && gap > -h {
                *last = [
                    last[0].min(r[0]),
                    last[1].min(r[1]),
                    last[2].max(r[2]),
                    last[3].max(r[3]),
                ];
                continue;
            }
        }
        out.push((w.page as usize, r));
    }
    out
}

fn join(words: &[Word]) -> String {
    let mut s = String::new();
    for w in words {
        if !s.is_empty() {
            s.push(' ');
        }
        s.push_str(&w.text);
        if s.len() > 2000 {
            break;
        }
    }
    s
}

fn overlaps(a: &Box4, b: &Box4) -> bool {
    a[0] <= b[2] + 2.0 && b[0] <= a[2] + 2.0 && a[1] <= b[3] + 2.0 && b[1] <= a[3] + 2.0
}

/// Compares with text already extracted (the app passes its text index's pages).
pub fn compare_texts(
    a: &Document,
    b: &Document,
    ta: &DocText,
    tb: &DocText,
    opts: &Options,
    progress: &(dyn Fn(Stage, usize, usize) + Sync),
    cancel: Option<&AtomicBool>,
) -> Comparison {
    let t0 = Instant::now();
    let stop = || cancel.is_some_and(|c| c.load(Ordering::Relaxed));
    let mut out = Comparison {
        pages_a: ta.pages(),
        pages_b: tb.pages(),
        words_a: ta.words.len(),
        words_b: tb.words.len(),
        ..Comparison::default()
    };
    progress(Stage::Text, 0, 1);
    out.pairs = align(ta, tb);
    out.timing.align_ms = t0.elapsed().as_millis() as u64;
    let deleted_pages: Vec<bool> = {
        let mut v = vec![false; ta.pages()];
        for &(x, y) in &out.pairs {
            if let (Some(x), None) = (x, y) {
                v[x] = true;
            }
        }
        v
    };
    let inserted_pages: Vec<bool> = {
        let mut v = vec![false; tb.pages()];
        for &(x, y) in &out.pairs {
            if let (None, Some(y)) = (x, y) {
                v[y] = true;
            }
        }
        v
    };
    // Where each page of one side sits in the other, for jumping.
    let pairs = out.pairs.clone();
    let partner = |side: usize, p: usize| -> Option<usize> {
        let k = pairs.iter().position(|q| {
            if side == 0 {
                q.0 == Some(p)
            } else {
                q.1 == Some(p)
            }
        })?;
        let pick = |q: &(Option<usize>, Option<usize>)| if side == 0 { q.1 } else { q.0 };
        pairs[k..]
            .iter()
            .find_map(pick)
            .or_else(|| pairs[..k].iter().rev().find_map(pick))
    };
    let mut changes = Vec::new();
    for &(x, y) in &pairs {
        match (x, y) {
            (Some(p), None) => {
                let words = ta.page_words(p);
                changes.push(Change {
                    kind: ChangeKind::PageDeleted,
                    page_a: Some(p),
                    page_b: partner(0, p),
                    rects_a: vec![],
                    rects_b: vec![],
                    old: shorten(&join(words), 200),
                    new: String::new(),
                    description: tr!("Page {} was removed", p + 1),
                });
                out.summary.pages_deleted += 1;
            }
            (None, Some(p)) => {
                let words = tb.page_words(p);
                changes.push(Change {
                    kind: ChangeKind::PageInserted,
                    page_a: partner(1, p),
                    page_b: Some(p),
                    rects_a: vec![],
                    rects_b: vec![],
                    old: String::new(),
                    new: shorten(&join(words), 200),
                    description: tr!("Page {} was added", p + 1),
                });
                out.summary.pages_inserted += 1;
            }
            _ => {}
        }
    }

    let td = Instant::now();
    if opts.text && !stop() {
        text_changes(
            ta,
            tb,
            opts,
            &deleted_pages,
            &inserted_pages,
            &mut out.summary,
            &mut changes,
        );
        // The other side of an insertion or deletion: the page paired with it.
        for c in &mut changes {
            match c.kind {
                ChangeKind::Inserted => {
                    if let Some(&(Some(x), _)) =
                        pairs.iter().find(|q| q.1.is_some() && q.1 == c.page_b)
                    {
                        c.page_a = Some(x);
                    }
                }
                ChangeKind::Deleted => {
                    if let Some(&(_, Some(y))) =
                        pairs.iter().find(|q| q.0.is_some() && q.0 == c.page_a)
                    {
                        c.page_b = Some(y);
                    }
                }
                _ => {}
            }
        }
    }
    out.timing.diff_ms = td.elapsed().as_millis() as u64;
    progress(Stage::Text, 1, 1);

    if opts.annotations {
        let (pa, pb) = (PageTree::load(a), PageTree::load(b));
        for &(x, y) in &out.pairs {
            let (Some(x), Some(y)) = (x, y) else { continue };
            let (Some(px), Some(py)) = (pa.page(x), pb.page(y)) else {
                continue;
            };
            for (_, ra, rb, what) in meta::note_changes(&meta::notes(a, &px), &meta::notes(b, &py))
            {
                out.summary.annotations += 1;
                changes.push(Change {
                    kind: ChangeKind::Annotation,
                    page_a: Some(x),
                    page_b: Some(y),
                    rects_a: ra.map(|r| (x, r)).into_iter().collect(),
                    rects_b: rb.map(|r| (y, r)).into_iter().collect(),
                    old: String::new(),
                    new: String::new(),
                    description: what,
                });
            }
        }
    }
    if opts.metadata {
        for (k, va, vb) in meta::metadata(a, b) {
            out.summary.metadata += 1;
            changes.push(Change {
                kind: ChangeKind::Metadata,
                page_a: None,
                page_b: None,
                rects_a: vec![],
                rects_b: vec![],
                description: format!(
                    "{}: \u{201c}{va}\u{201d} → \u{201c}{vb}\u{201d}",
                    pagebara_i18n::translate(&k, &[])
                ),
                old: va,
                new: vb,
            });
        }
    }
    if opts.forms {
        for (name, va, vb) in meta::forms(a, b) {
            out.summary.forms += 1;
            let description = match (&va, &vb) {
                (Some(x), Some(y)) => format!("{name}: \u{201c}{x}\u{201d} → \u{201c}{y}\u{201d}"),
                (Some(_), None) => tr!("{}: field removed", name),
                _ => tr!("{}: field added", name),
            };
            changes.push(Change {
                kind: ChangeKind::Form,
                page_a: None,
                page_b: None,
                rects_a: vec![],
                rects_b: vec![],
                old: va.unwrap_or_default(),
                new: vb.unwrap_or_default(),
                description,
            });
        }
    }

    let tv = Instant::now();
    if opts.visual && !stop() {
        let visual = visual_changes(a, b, &out.pairs, &changes, opts, progress, cancel);
        out.timing.pages_rendered = visual.1;
        out.summary.pages_looking_different = visual.0.len();
        changes.extend(visual.0);
    }
    out.timing.visual_ms = tv.elapsed().as_millis() as u64;
    out.cancelled = stop();

    // In page order (new document first), top to bottom.
    changes.sort_by_key(|c| {
        (
            c.page_b.or(c.page_a).unwrap_or(usize::MAX),
            c.rects_b
                .first()
                .or(c.rects_a.first())
                .map_or(0, |r| (r.1[1] * 10.0) as i64),
        )
    });
    out.summary.changes = changes.len();
    out.changes = changes;
    out.timing.total_ms = t0.elapsed().as_millis() as u64;
    out
}

fn text_changes(
    ta: &DocText,
    tb: &DocText,
    opts: &Options,
    deleted_pages: &[bool],
    inserted_pages: &[bool],
    summary: &mut Summary,
    changes: &mut Vec<Change>,
) {
    let ka: Vec<u64> = ta.words.iter().map(|w| w.key).collect();
    let kb: Vec<u64> = tb.words.iter().map(|w| w.key).collect();
    let edits = diff(&ka, &kb);
    // Hunks: a deleted and/or an inserted range at positions (i in a, j in b).
    struct Hunk {
        del: std::ops::Range<usize>,
        ins: std::ops::Range<usize>,
        moved: bool,
    }
    let mut hunks: Vec<Hunk> = Vec::new();
    let (mut i, mut j) = (0, 0);
    let mut open = false;
    for e in &edits {
        match e {
            Edit::Equal(x, y) => {
                (i, j) = (x.end, y.end);
                open = false;
            }
            Edit::Delete(x) => {
                if !open {
                    hunks.push(Hunk {
                        del: x.start..x.start,
                        ins: j..j,
                        moved: false,
                    });
                    open = true;
                }
                let h = hunks.last_mut().expect("open hunk");
                h.del = h.del.start.min(x.start)..x.end;
                i = x.end;
            }
            Edit::Insert(y) => {
                if !open {
                    hunks.push(Hunk {
                        del: i..i,
                        ins: y.start..y.start,
                        moved: false,
                    });
                    open = true;
                }
                let h = hunks.last_mut().expect("open hunk");
                h.ins = h.ins.start.min(y.start)..y.end;
                j = y.end;
            }
        }
    }
    // Moves: a deleted run found again, word for word, inside inserted text (or an
    // inserted run inside deleted text); the hunk it is found in is split around it.
    let mut moves: Vec<(std::ops::Range<usize>, std::ops::Range<usize>)> = Vec::new();
    let min = opts.min_moved_words.max(1);
    for pass in 0..2 {
        let mut d = 0;
        while d < hunks.len() {
            let (pure, run) = if pass == 0 {
                (hunks[d].ins.is_empty(), hunks[d].del.clone())
            } else {
                (hunks[d].del.is_empty(), hunks[d].ins.clone())
            };
            if !pure || hunks[d].moved || run.len() < min {
                d += 1;
                continue;
            }
            let needle = if pass == 0 {
                &ka[run.clone()]
            } else {
                &kb[run.clone()]
            };
            let found = (0..hunks.len()).find_map(|h| {
                if h == d || hunks[h].moved {
                    return None;
                }
                let r = if pass == 0 {
                    hunks[h].ins.clone()
                } else {
                    hunks[h].del.clone()
                };
                let side = if pass == 0 {
                    &kb[r.clone()]
                } else {
                    &ka[r.clone()]
                };
                side.windows(needle.len())
                    .position(|w| w == needle)
                    .map(|k| (h, r.start + k))
            });
            let Some((h, at)) = found else {
                d += 1;
                continue;
            };
            let moved = at..at + run.len();
            moves.push(if pass == 0 {
                (run.clone(), moved.clone())
            } else {
                (moved.clone(), run.clone())
            });
            hunks[d].moved = true;
            let old = Hunk {
                del: hunks[h].del.clone(),
                ins: hunks[h].ins.clone(),
                moved: false,
            };
            let (before, after) = if pass == 0 {
                (
                    Hunk {
                        del: old.del.clone(),
                        ins: old.ins.start..moved.start,
                        moved: false,
                    },
                    Hunk {
                        del: old.del.end..old.del.end,
                        ins: moved.end..old.ins.end,
                        moved: false,
                    },
                )
            } else {
                (
                    Hunk {
                        del: old.del.start..moved.start,
                        ins: old.ins.clone(),
                        moved: false,
                    },
                    Hunk {
                        del: moved.end..old.del.end,
                        ins: old.ins.end..old.ins.end,
                        moved: false,
                    },
                )
            };
            hunks[h] = before;
            hunks.insert(h + 1, after);
            if h < d {
                d += 1;
            }
            d += 1;
        }
    }
    for (from, to) in moves {
        let (from, to) = (&ta.words[from], &tb.words[to]);
        summary.words_moved += from.len();
        changes.push(Change {
            kind: ChangeKind::Moved,
            page_a: Some(from[0].page as usize),
            page_b: Some(to[0].page as usize),
            rects_a: word_rects(from),
            rects_b: word_rects(to),
            old: shorten(&join(from), 200),
            new: String::new(),
            description: tr!(
                "Moved from page {} to page {}: “{}”",
                from[0].page + 1,
                to[0].page + 1,
                shorten(&join(from), 80)
            ),
        });
    }
    // Pure insertions and deletions are listed per page.
    let split = |r: std::ops::Range<usize>, t: &DocText| -> Vec<std::ops::Range<usize>> {
        let mut out = Vec::new();
        let mut start = r.start;
        for i in r.start + 1..r.end {
            if t.words[i].page != t.words[i - 1].page {
                out.push(start..i);
                start = i;
            }
        }
        out.push(start..r.end);
        out
    };
    let hunks: Vec<Hunk> = hunks
        .into_iter()
        .filter(|h| !h.moved)
        .flat_map(|h| match (h.del.is_empty(), h.ins.is_empty()) {
            (true, false) => split(h.ins.clone(), tb)
                .into_iter()
                .map(|ins| Hunk {
                    del: h.del.clone(),
                    ins,
                    moved: false,
                })
                .collect(),
            (false, true) => split(h.del.clone(), ta)
                .into_iter()
                .map(|del| Hunk {
                    del,
                    ins: h.ins.clone(),
                    moved: false,
                })
                .collect(),
            _ => vec![h],
        })
        .collect();
    for h in &hunks {
        let del = &ta.words[h.del.clone()];
        let ins = &tb.words[h.ins.clone()];
        // Words of whole pages added or removed are listed as those pages.
        let del: Vec<Word> = del
            .iter()
            .filter(|w| !deleted_pages[w.page as usize])
            .cloned()
            .collect();
        let ins: Vec<Word> = ins
            .iter()
            .filter(|w| !inserted_pages[w.page as usize])
            .cloned()
            .collect();
        let (del, ins) = (&del[..], &ins[..]);
        summary.words_deleted += del.len();
        summary.words_inserted += ins.len();
        let (old, new) = (join(del), join(ins));
        let (kind, description) = match (del.is_empty(), ins.is_empty()) {
            (false, false) => (
                ChangeKind::Replaced,
                format!(
                    "\u{201c}{}\u{201d} → \u{201c}{}\u{201d}",
                    shorten(&old, 80),
                    shorten(&new, 80)
                ),
            ),
            (false, true) => (ChangeKind::Deleted, tr!("Deleted “{}”", shorten(&old, 120))),
            (true, false) => (
                ChangeKind::Inserted,
                tr!("Inserted “{}”", shorten(&new, 120)),
            ),
            (true, true) => continue,
        };
        changes.push(Change {
            kind,
            page_a: del
                .first()
                .map(|w| w.page as usize)
                .or(page_of(ta, h.del.start)),
            page_b: ins
                .first()
                .map(|w| w.page as usize)
                .or(page_of(tb, h.ins.start)),
            rects_a: word_rects(del),
            rects_b: word_rects(ins),
            old: shorten(&old, 500),
            new: shorten(&new, 500),
            description,
        });
    }
}

#[allow(clippy::type_complexity)]
fn visual_changes(
    a: &Document,
    b: &Document,
    pairs: &[(Option<usize>, Option<usize>)],
    known: &[Change],
    opts: &Options,
    progress: &(dyn Fn(Stage, usize, usize) + Sync),
    cancel: Option<&AtomicBool>,
) -> (Vec<Change>, usize) {
    let (pa, pb) = (PageTree::load(a), PageTree::load(b));
    let todo: Vec<(usize, usize)> = pairs
        .iter()
        .filter_map(|&(x, y)| Some((x?, y?)))
        .filter(|&(x, y)| match (pa.page(x), pb.page(y)) {
            (Some(p), Some(q)) => !visual::same_page(a, p.r, b, q.r),
            _ => false,
        })
        .collect();
    let (ra, rb) = (Renderer::new(a.clone()), Renderer::new(b.clone()));
    let done = AtomicUsize::new(0);
    let total = todo.len();
    progress(Stage::Visual, 0, total);
    let found: Vec<Option<Change>> = todo
        .par_iter()
        .map(|&(x, y)| {
            if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return None;
            }
            let ia = visual::render(&ra, x, opts.dpi)?;
            let ib = visual::render(&rb, y, opts.dpi)?;
            let (boxes, share) = visual::differences(&ia, &ib, opts.tolerance, opts.dpi);
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            progress(Stage::Visual, n, total);
            // Boxes a listed change on this page explains are left out.
            let explained = |r: &Box4| {
                known.iter().any(|c| {
                    c.rects_a.iter().any(|(p, q)| *p == x && overlaps(q, r))
                        || c.rects_b.iter().any(|(p, q)| *p == y && overlaps(q, r))
                })
            };
            let rest: Vec<Box4> = boxes.into_iter().filter(|r| !explained(r)).collect();
            if rest.is_empty() {
                return None;
            }
            Some(Change {
                kind: ChangeKind::Visual,
                page_a: Some(x),
                page_b: Some(y),
                rects_a: rest.iter().map(|r| (x, *r)).collect(),
                rects_b: rest.iter().map(|r| (y, *r)).collect(),
                old: String::new(),
                new: String::new(),
                description: trn!(
                    "Old page {} and new page {} look different ({n} area, {}% of the page)",
                    "Old page {} and new page {} look different ({n} areas, {}% of the page)",
                    rest.len(),
                    x + 1,
                    y + 1,
                    format!("{:.1}", share * 100.0)
                ),
            })
        })
        .collect();
    (found.into_iter().flatten().collect(), 2 * total)
}
