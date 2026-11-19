//! Find in document: pages are searched on a worker thread, nearest first, and hits arrive
//! in batches while the user keeps typing (a newer search cancels the older one). Pages
//! whose pictures live text has read are searched with that text too.

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use pagebara_content::TextPage;
use pagebara_i18n::{tr, trn};

pub use crate::text_index::FindOptions;
use crate::text_index::{Bbox, Query, TextIndex, boxes_of};

/// How often a running search hands over what it found.
const BATCH_EVERY: Duration = Duration::from_millis(60);

/// Pages searched at once: few at first, so the first hits come quickly.
const FIRST_CHUNK: usize = 8;
const MAX_CHUNK: usize = 512;

#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    pub page: usize,
    /// One box per line of the hit; worked out only for pages being looked at (`boxed`).
    pub boxes: Vec<Bbox>,
    pub boxed: bool,
}

/// A result preview and the byte range to emphasize in its single line of text.
#[derive(Debug)]
pub struct MatchContext {
    pub text: String,
    pub matched: Range<usize>,
}

/// Hits found on the pages searched so far.
#[derive(Debug)]
pub struct Batch {
    pub generation: u64,
    pub hits: Vec<Hit>,
    /// Boxes of the hits on a page, asked for with [`Search::want_boxes`].
    pub boxes: Vec<(usize, Vec<Vec<Bbox>>)>,
    /// Context lines for the hits on each requested page, in match order.
    pub contexts: Vec<(usize, Vec<MatchContext>)>,
    /// Pages searched in all.
    pub searched: usize,
    pub done: bool,
}

#[derive(Debug, Default)]
pub struct Search {
    pub query: String,
    pub options: FindOptions,
    generation: Arc<AtomicU64>,
    /// Hits in document order.
    pub hits: Vec<Hit>,
    pub current: Option<usize>,
    pub searched: usize,
    pub done: bool,
    job: Option<Job>,
}

/// The running search, for working out boxes later.
struct Job {
    index: Arc<TextIndex>,
    /// Pages' text with what live text recognised, searched in place of the index's.
    live: Arc<HashMap<usize, Arc<TextPage>>>,
    query: Arc<Query>,
    post: Arc<dyn Fn(Batch) + Send + Sync>,
    /// The occurrence selected before a filter changed, if the new search can keep it.
    preferred: Option<Hit>,
    /// Pages whose boxes were asked for.
    asked: HashSet<usize>,
    /// An empty slot marks a page whose context lines are on their way.
    contexts: HashMap<usize, Option<Vec<MatchContext>>>,
}

impl fmt::Debug for Job {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Job")
            .field("asked", &self.asked)
            .finish_non_exhaustive()
    }
}

impl Search {
    /// Stops a running search and forgets its hits.
    pub fn invalidate(&mut self) -> u64 {
        self.hits.clear();
        self.current = None;
        self.searched = 0;
        self.done = false;
        self.job = None;
        self.generation.fetch_add(1, Ordering::AcqRel) + 1
    }

    /// Searches `index` for the query on a worker thread, from page `from` on (wrapping
    /// around); `post` gets the hits in batches. An empty query searches nothing.
    pub fn start(
        &mut self,
        index: Arc<TextIndex>,
        from: usize,
        post: impl Fn(Batch) + Send + Sync + 'static,
    ) -> bool {
        self.start_with(index, HashMap::new(), from, post)
    }

    /// [`Search::start`] with pages' text that has more than the index has (what live
    /// text recognised in their pictures), searched in its place.
    pub fn start_with(
        &mut self,
        index: Arc<TextIndex>,
        live: HashMap<usize, Arc<TextPage>>,
        from: usize,
        post: impl Fn(Batch) + Send + Sync + 'static,
    ) -> bool {
        let live = Arc::new(live);
        let preferred = self
            .job
            .as_ref()
            .filter(|j| j.query.options != self.options)
            .and_then(|job| self.current_hit().or(job.preferred.as_ref()))
            .filter(|h| !h.boxes.is_empty())
            .cloned();
        let from = preferred.as_ref().map_or(from, |h| h.page);
        let generation = self.invalidate();
        let Some(query) = Query::with_options(&self.query, self.options) else {
            return false;
        };
        let query = Arc::new(query);
        let post: Arc<dyn Fn(Batch) + Send + Sync> = Arc::new(post);
        self.job = Some(Job {
            index: index.clone(),
            live: live.clone(),
            query: query.clone(),
            post: post.clone(),
            preferred,
            asked: HashSet::new(),
            contexts: HashMap::new(),
        });
        let flag = self.generation.clone();
        pagebara_thread::spawn("search", move || {
            let stop = || flag.load(Ordering::Acquire) != generation;
            let _searching = index.searching();
            index.load_cached();
            let n = index.len();
            let mut hits = Vec::new();
            let mut searched = 0;
            let mut found_any = false;
            let mut last_post = Instant::now();
            for (k, pages) in chunks(from.min(n), n).into_iter().enumerate() {
                // Boxes for the pages around `from`, where the first hits are shown.
                let mut found = index.search(pages.clone(), &query, k == 0, &stop);
                if stop() {
                    return;
                }
                if live.keys().any(|p| pages.contains(p)) {
                    found.retain(|(p, _)| !live.contains_key(p));
                    for (&p, text) in live.iter().filter(|(p, _)| pages.contains(p)) {
                        found.extend(boxes_of(text, &query).into_iter().map(|b| (p, Some(b))));
                    }
                    found.sort_by_key(|f| f.0);
                }
                searched += pages.len();
                hits.extend(found.into_iter().map(|(page, boxes)| Hit {
                    page,
                    boxed: boxes.is_some(),
                    boxes: boxes.unwrap_or_default(),
                }));
                let done = searched == n;
                // The first hits go out at once.
                let first = !found_any && !hits.is_empty();
                if done || first || last_post.elapsed() > BATCH_EVERY {
                    found_any |= first;
                    post(Batch {
                        generation,
                        hits: std::mem::take(&mut hits),
                        boxes: Vec::new(),
                        contexts: Vec::new(),
                        searched,
                        done,
                    });
                    last_post = Instant::now();
                }
            }
            if n == 0 {
                post(Batch {
                    generation,
                    hits: Vec::new(),
                    boxes: Vec::new(),
                    contexts: Vec::new(),
                    searched: 0,
                    done: true,
                });
            }
            index.save_cached();
        });
        true
    }

    /// Takes in a batch of the running search; returns whether it was the first with hits
    /// (the current hit is then the first at or after page `from`). Batches of older
    /// searches are ignored (`None`).
    pub fn add(&mut self, b: Batch, from: usize) -> Option<bool> {
        if self.generation.load(Ordering::Acquire) != b.generation {
            return None;
        }
        let first = self.current.is_none() && !b.hits.is_empty();
        // A batch covers pages not seen yet, ascending but for the wrap to page 0: each
        // ascending run goes in at its place, shifting the current hit when before it.
        let mut runs = Vec::new();
        let mut run = 0;
        for (k, h) in b.hits.iter().enumerate() {
            if k > 0 && h.page < b.hits[k - 1].page {
                runs.push(run);
                run = 0;
            }
            run += 1;
        }
        runs.push(run);
        let mut batch = b.hits.into_iter();
        for len in runs {
            let mut part = batch.by_ref().take(len).peekable();
            let Some(page) = part.peek().map(|h| h.page) else {
                continue;
            };
            let at = self.hits.partition_point(|h| h.page < page);
            if let Some(c) = &mut self.current
                && at <= *c
            {
                *c += len;
            }
            self.hits.splice(at..at, part);
        }
        if let Some(job) = &mut self.job {
            for (page, lines) in b.contexts {
                job.contexts.insert(page, Some(lines));
            }
        }
        for (page, boxes) in b.boxes {
            let at = self.hits.partition_point(|h| h.page < page);
            for (h, b) in self.hits[at..].iter_mut().zip(boxes) {
                if h.page != page {
                    break;
                }
                h.boxes = b;
                h.boxed = true;
            }
        }
        // Batches of boxes may overtake those of hits.
        self.searched = self.searched.max(b.searched);
        self.done |= b.done;
        if first {
            let preferred = self.job.as_mut().and_then(|j| j.preferred.take());
            let from = preferred.as_ref().map_or(from, |h| h.page);
            let first = self.hits.partition_point(|h| h.page < from);
            let same = preferred.and_then(|old| {
                self.hits[first..]
                    .iter()
                    .take_while(|h| h.page == from)
                    .position(|h| *h == old)
            });
            self.current = Some(if first < self.hits.len() {
                first + same.unwrap_or(0)
            } else {
                0
            });
            self.box_current();
        }
        Some(first)
    }

    /// Works out the boxes of the hits on these pages (those being drawn) on a worker; they
    /// arrive as a batch.
    pub fn want_boxes(&mut self, pages: impl IntoIterator<Item = usize>) {
        let Some(job) = &mut self.job else { return };
        let mut todo = Vec::new();
        for page in pages {
            let at = self.hits.partition_point(|h| h.page < page);
            let unboxed = self
                .hits
                .get(at)
                .is_some_and(|h| h.page == page && !h.boxed);
            if unboxed && job.asked.insert(page) {
                todo.push(page);
            }
        }
        if todo.is_empty() {
            return;
        }
        let (index, live) = (job.index.clone(), job.live.clone());
        let (query, post) = (job.query.clone(), job.post.clone());
        let generation = self.generation.load(Ordering::Acquire);
        let flag = self.generation.clone();
        pagebara_thread::spawn("search-boxes", move || {
            let boxes = todo
                .into_iter()
                .take_while(|_| flag.load(Ordering::Acquire) == generation)
                .map(|page| (page, hit_boxes(&index, &live, page, &query)))
                .collect();
            post(Batch {
                generation,
                hits: Vec::new(),
                boxes,
                contexts: Vec::new(),
                searched: 0,
                done: false,
            });
        });
    }

    /// Loads original-case context only for pages in the visible part of the results list.
    pub fn want_context(&mut self, rows: Range<usize>) {
        let Some(job) = &mut self.job else { return };
        let mut pages = Vec::new();
        for hit in self.hits.get(rows).unwrap_or_default() {
            if let std::collections::hash_map::Entry::Vacant(entry) = job.contexts.entry(hit.page) {
                entry.insert(None);
                pages.push(hit.page);
            }
        }
        if pages.is_empty() {
            return;
        }
        let (index, live, post) = (job.index.clone(), job.live.clone(), job.post.clone());
        let query = job.query.clone();
        let flag = self.generation.clone();
        let generation = flag.load(Ordering::Acquire);
        pagebara_thread::spawn("search-context", move || {
            let mut contexts = Vec::new();
            for page in pages {
                if flag.load(Ordering::Acquire) != generation {
                    return;
                }
                let text = live.get(&page).cloned().unwrap_or_else(|| index.page(page));
                let lines = query
                    .ranges(&text)
                    .into_iter()
                    .map(|r| context_line(&text.text, r))
                    .collect();
                contexts.push((page, lines));
            }
            post(Batch {
                generation,
                contexts,
                hits: Vec::new(),
                boxes: Vec::new(),
                searched: 0,
                done: false,
            });
        });
    }

    pub fn context(&self, row: usize) -> Option<&MatchContext> {
        let page = self.hits.get(row)?.page;
        let first = self.hits.partition_point(|h| h.page < page);
        let lines = self.job.as_ref()?.contexts.get(&page)?.as_ref()?;
        lines.get(row - first)
    }

    /// Works out the boxes of the current hit's page now, for scrolling to it.
    fn box_current(&mut self) {
        if let Some(page) = self.current_hit().map(|h| h.page) {
            self.box_page(page);
        }
    }

    /// Works out the boxes of the hits found on `page` now (where on the page they are).
    pub fn box_page(&mut self, page: usize) {
        let Some(job) = &self.job else { return };
        let at = self.hits.partition_point(|h| h.page < page);
        if !self
            .hits
            .get(at)
            .is_some_and(|h| h.page == page && !h.boxed)
        {
            return;
        }
        let boxes = hit_boxes(&job.index, &job.live, page, &job.query);
        for (h, b) in self.hits[at..].iter_mut().zip(boxes) {
            if h.page != page {
                break;
            }
            h.boxes = b;
            h.boxed = true;
        }
    }

    /// Makes hit `i` the current one.
    pub fn select(&mut self, i: usize) -> Option<&Hit> {
        if i >= self.hits.len() {
            return None;
        }
        self.current = Some(i);
        self.box_current();
        self.hits.get(i)
    }

    /// Moves to the next or previous hit, wrapping around.
    pub fn step(&mut self, forward: bool) -> Option<&Hit> {
        let n = self.hits.len();
        if n == 0 {
            return None;
        }
        let c = self.current.unwrap_or(0);
        let c = if forward {
            (c + 1) % n
        } else {
            (c + n - 1) % n
        };
        self.current = Some(c);
        self.box_current();
        self.hits.get(c)
    }

    pub fn current_hit(&self) -> Option<&Hit> {
        self.current.and_then(|c| self.hits.get(c))
    }

    /// The boxes of the hits on `page`, with whether each is the current one.
    pub fn boxes_on(&self, page: usize) -> impl Iterator<Item = (bool, &[Bbox])> {
        let start = self.hits.partition_point(|h| h.page < page);
        self.hits[start..]
            .iter()
            .enumerate()
            .take_while(move |(_, h)| h.page == page)
            .map(move |(j, h)| (self.current == Some(start + j), h.boxes.as_slice()))
    }

    /// "3 of 12", "No matches"...
    pub fn status(&self) -> String {
        if self.query.trim().is_empty() {
            return String::new();
        }
        let n = self.hits.len();
        match (n, self.current, self.done) {
            (0, _, true) => tr!("No matches"),
            (0, _, false) => tr!("Searching…"),
            // TRANSLATORS: the current search match and the number of matches
            (n, Some(c), true) => tr!("{} of {}", c + 1, n),
            // TRANSLATORS: the current search match and the matches found so far
            (n, Some(c), false) => tr!("{} of {}+", c + 1, n),
            (n, None, _) => trn!("{n} match", "{n} matches", n),
        }
    }
}

/// A single line with the match near the start so it survives a narrow results panel.
fn context_line(text: &str, hit: Range<usize>) -> MatchContext {
    let mut before = text[..hit.start]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| c != '\n');
    let start = before
        .by_ref()
        .take(16)
        .last()
        .map_or(hit.start, |(at, _)| at);
    let mut prefix = &text[start..hit.start];
    let mut line = String::with_capacity(96);
    if let Some((_, previous)) = before.next() {
        line.push('…');
        // Start at a word boundary when there is one close to the match.
        if !previous.is_whitespace()
            && let Some(at) = prefix.trim_end().find(char::is_whitespace)
        {
            prefix = &prefix[at..];
        }
    }
    append_context(&mut line, prefix.chars());
    let start = line.len();
    let mut matched = text[hit.clone()].chars();
    append_context(&mut line, matched.by_ref().take(96));
    let end = line.len();
    if matched.next().is_some() {
        line.push('…');
    } else {
        let mut after = text[hit.end..].chars().take_while(|&c| c != '\n');
        append_context(&mut line, after.by_ref().take(72));
        if after.next().is_some() {
            line.push('…');
        }
    }
    line.truncate(line.trim_end().len().max(end));
    MatchContext {
        text: line,
        matched: start..end,
    }
}

/// Preserve spaces at match boundaries while folding tabs and line breaks for display.
fn append_context(line: &mut String, chars: impl Iterator<Item = char>) {
    for c in chars {
        if !c.is_whitespace() {
            line.push(c);
        } else if !line.is_empty() && !line.ends_with(' ') {
            line.push(' ');
        }
    }
}

fn hit_boxes(
    index: &TextIndex,
    live: &HashMap<usize, Arc<TextPage>>,
    page: usize,
    query: &Query,
) -> Vec<Vec<Bbox>> {
    match live.get(&page) {
        Some(text) => boxes_of(text, query),
        None => index.hit_boxes(page, query),
    }
}

/// Pages `from..n` then `0..from`, in chunks growing from [`FIRST_CHUNK`].
fn chunks(from: usize, n: usize) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut size = FIRST_CHUNK;
    for part in [from..n, 0..from] {
        let mut at = part.start;
        while at < part.end {
            let end = (at + size).min(part.end);
            out.push(at..end);
            at = end;
            size = (size * 2).min(MAX_CHUNK);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text_index::find;

    fn hit(page: usize) -> Hit {
        Hit {
            page,
            boxes: Vec::new(),
            boxed: true,
        }
    }

    #[test]
    fn batches_merge_in_page_order() {
        let mut s = Search {
            query: "x".into(),
            ..Search::default()
        };
        let g = s.invalidate();
        // Searching from page 5: pages 5..9 first, then 0..4.
        let first = Batch {
            generation: g,
            hits: vec![hit(5), hit(7)],
            boxes: Vec::new(),
            contexts: Vec::new(),
            searched: 5,
            done: false,
        };
        assert_eq!(s.add(first, 5), Some(true));
        assert_eq!(s.current_hit().map(|h| h.page), Some(5));
        assert_eq!(s.status(), "1 of 2+");
        let rest = Batch {
            generation: g,
            hits: vec![hit(1)],
            boxes: Vec::new(),
            contexts: Vec::new(),
            searched: 10,
            done: true,
        };
        assert_eq!(s.add(rest, 5), Some(false));
        // Still on page 5's hit, now the second in document order.
        assert_eq!(s.current, Some(1));
        assert_eq!(s.status(), "2 of 3");
        assert_eq!(s.step(true).map(|h| h.page), Some(7));
        assert_eq!(s.step(true).map(|h| h.page), Some(1));
        assert_eq!(s.step(false).map(|h| h.page), Some(7));
        assert_eq!(s.boxes_on(7).map(|(c, _)| c).collect::<Vec<_>>(), [true]);
        // A batch of an older search is ignored.
        let stale = Batch {
            generation: g - 1,
            hits: vec![hit(0)],
            boxes: Vec::new(),
            contexts: Vec::new(),
            searched: 1,
            done: true,
        };
        assert_eq!(s.add(stale, 0), None);
        assert_eq!(s.hits.len(), 3);
    }

    #[test]
    fn a_batch_across_the_wrap_merges_in_order() {
        let mut s = Search {
            query: "x".into(),
            ..Search::default()
        };
        let g = s.invalidate();
        let pages = |b: &[usize]| b.iter().map(|&p| hit(p)).collect();
        let batch = |hits, done| Batch {
            generation: g,
            hits,
            boxes: Vec::new(),
            contexts: Vec::new(),
            searched: 0,
            done,
        };
        assert_eq!(s.add(batch(pages(&[4, 6, 6]), false), 4), Some(true));
        s.step(true);
        assert_eq!(s.add(batch(pages(&[8, 9, 0, 1, 1]), false), 4), Some(false));
        assert_eq!(s.add(batch(pages(&[2]), true), 4), Some(false));
        let order: Vec<usize> = s.hits.iter().map(|h| h.page).collect();
        assert_eq!(order, [0, 1, 1, 2, 4, 6, 6, 8, 9]);
        assert_eq!(s.current, Some(5));
        assert_eq!(chunks(5, 40), [5..13, 13..29, 29..40, 0..5]);
        assert!(chunks(0, 0).is_empty());
    }

    #[test]
    fn searches_on_a_worker() {
        let pdf = crate::test_pdf::text(&["hello world", "say hello again"]);
        let doc = pagebara_syntax::Document::open(pdf).unwrap();
        let refs = pagebara_pages::PageTree::open(&doc).refs();
        let index = Arc::new(TextIndex::new(doc, refs));
        let mut s = Search {
            query: "hello".into(),
            ..Search::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(s.start(index, 0, move |b| tx.send(b).unwrap()));
        loop {
            let b = rx.recv_timeout(Duration::from_secs(10)).unwrap();
            let done = b.done;
            s.add(b, 0);
            if done {
                break;
            }
        }
        assert!(s.done);
        assert_eq!(s.status(), "1 of 2");
    }

    #[test]
    fn context_uses_the_original_line_and_keeps_the_match_in_view() {
        let text = TextPage {
            text: "other line\nàéöäçñあいうえお1234567890 CAFÉ\n\tÉTÉ  today\nother line".into(),
            ..TextPage::default()
        };
        let hits = find(&text, "café été");
        assert_eq!(hits.len(), 1);
        let context = context_line(&text.text, hits[0].clone());
        assert_eq!(context.text, "…あいうえお1234567890 CAFÉ ÉTÉ today");
        assert_eq!(&context.text[context.matched], "CAFÉ ÉTÉ");
        let long = "界".repeat(200);
        assert_eq!(
            context_line(&long, 0..long.len()).text,
            format!("{}…", "界".repeat(96))
        );
    }

    #[test]
    fn repeated_matches_on_one_line_emphasize_their_own_occurrence() {
        let page = TextPage {
            text: "Yes, yes.".into(),
            ..TextPage::default()
        };
        let contexts: Vec<_> = find(&page, "YES")
            .into_iter()
            .map(|hit| context_line(&page.text, hit))
            .collect();
        assert_eq!(contexts.len(), 2);
        assert_eq!(contexts[0].text, page.text);
        assert_eq!(contexts[1].text, page.text);
        assert_eq!(contexts[0].matched, 0..3);
        assert_eq!(contexts[1].matched, 5..8);
    }

    #[test]
    fn live_text_is_searched_in_place_of_the_page() {
        let pdf = crate::test_pdf::text(&["hello world"]);
        let doc = pagebara_syntax::Document::open(pdf).unwrap();
        let refs = pagebara_pages::PageTree::open(&doc).refs();
        let index = Arc::new(TextIndex::new(doc.clone(), refs));
        // The page with another "hello" recognised in a picture.
        let word = pagebara_ocr::Word {
            text: "Hello".into(),
            quad: [
                pagebara_content::Point::new(72.0, 500.0),
                pagebara_content::Point::new(110.0, 500.0),
                pagebara_content::Point::new(110.0, 512.0),
                pagebara_content::Point::new(72.0, 512.0),
            ],
            confidence: 1.0,
            cuts: Vec::new(),
        };
        let lines = [pagebara_ocr::Line { words: vec![word] }];
        let live = HashMap::from([(
            0,
            Arc::new(pagebara_ocr::page_with_live(&doc, 0, &lines, None)),
        )]);
        let mut s = Search {
            query: "hello".into(),
            ..Search::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(s.start_with(index.clone(), live.clone(), 0, move |b| tx.send(b).unwrap()));
        loop {
            let b = rx.recv_timeout(Duration::from_secs(10)).unwrap();
            let done = b.done;
            s.add(b, 0);
            if done {
                break;
            }
        }
        let pages: Vec<usize> = s.hits.iter().map(|h| h.page).collect();
        assert_eq!(pages, [0, 0]);
        assert!(s.hits.iter().all(|h| h.boxed && !h.boxes.is_empty()));
        s.want_context(0..2);
        s.add(rx.recv_timeout(Duration::from_secs(10)).unwrap(), 0);
        assert_eq!(s.context(0).map(|c| c.text.as_str()), Some("hello world"));
        assert_eq!(s.context(1).map(|c| c.text.as_str()), Some("Hello"));

        s.query = "Hello".into();
        s.options = FindOptions {
            match_case: true,
            whole_words: true,
        };
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(s.start_with(index, live, 0, move |b| tx.send(b).unwrap()));
        while !s.done {
            s.add(rx.recv_timeout(Duration::from_secs(10)).unwrap(), 0);
        }
        assert_eq!(s.hits.len(), 1);
        assert!(!s.hits[0].boxes.is_empty());
        s.want_context(0..1);
        s.add(rx.recv_timeout(Duration::from_secs(10)).unwrap(), 0);
        let line = s.context(0).unwrap();
        assert_eq!(&line.text[line.matched.clone()], "Hello");
    }
    #[test]
    fn changing_filters_again_keeps_the_occurrence_and_drops_old_previews() {
        let doc = pagebara_syntax::Document::open(crate::test_pdf::text(&["x X x xx"])).unwrap();
        let refs = pagebara_pages::PageTree::open(&doc).refs();
        let index = Arc::new(TextIndex::new(doc, refs));
        let (tx, rx) = std::sync::mpsc::channel();
        let start = |search: &mut Search| {
            let tx = tx.clone();
            assert!(search.start(index.clone(), 0, move |b| {
                let _ = tx.send(b);
            }));
        };
        let next = || rx.recv_timeout(Duration::from_secs(10)).unwrap();
        let mut s = Search {
            query: "x".into(),
            ..Search::default()
        };
        start(&mut s);
        while !s.done {
            s.add(next(), 0);
        }
        assert_eq!(s.hits.len(), 5);
        let selected = s.select(2).unwrap().clone();
        s.want_context(0..3);
        let old_previews = next();
        s.options.match_case = true;
        start(&mut s);
        s.options.whole_words = true;
        start(&mut s);
        assert_eq!(s.add(old_previews, 0), None);
        while !s.done {
            s.add(next(), 0);
        }
        assert_eq!(s.hits.len(), 2);
        assert_eq!(s.current_hit(), Some(&selected));
        assert!(s.context(1).is_none());
        s.want_context(0..2);
        while s.context(1).is_none() {
            s.add(next(), 0);
        }
        let line = s.context(1).unwrap();
        assert_eq!(line.matched, 4..5);
    }

    #[test]
    fn boxes_only_for_pages_looked_at() {
        let text: Vec<&[&str]> = vec![&["hello there"]; 20];
        let doc = pagebara_syntax::Document::open(crate::test_pdf::pages(&text)).unwrap();
        let refs = pagebara_pages::PageTree::open(&doc).refs();
        let index = Arc::new(TextIndex::new(doc, refs));
        assert!(index.fill(0..20, &|| false));
        let mut s = Search {
            query: "HELLO".into(),
            ..Search::default()
        };
        let (tx, rx) = std::sync::mpsc::channel();
        assert!(s.start(index.clone(), 0, move |b| tx.send(b).unwrap()));
        let next = || rx.recv_timeout(Duration::from_secs(10)).unwrap();
        while !s.done {
            s.add(next(), 0);
        }
        assert_eq!(s.hits.len(), 20);
        // The first chunk of pages has boxes; the others wait until they are drawn.
        assert!(s.hits[..FIRST_CHUNK].iter().all(|h| h.boxed));
        assert!(
            s.hits[FIRST_CHUNK..]
                .iter()
                .all(|h| !h.boxed && h.boxes.is_empty())
        );
        s.want_context(14..16);
        s.add(next(), 0);
        assert_eq!(s.context(14).map(|c| c.text.as_str()), Some("hello there"));
        assert_eq!(s.context(15).map(|c| c.text.as_str()), Some("hello there"));
        assert!(s.context(13).is_none());
        assert!(!s.hits[15].boxed);
        s.want_context(15..16);
        let query = Query::new("hello").unwrap();
        s.want_boxes([15, 15, 3]);
        s.add(next(), 0);
        assert!(s.hits[15].boxed);
        assert_eq!(vec![s.hits[15].boxes.clone()], index.hit_boxes(15, &query));
        assert!(!s.hits[14].boxed);
        // Asked once: no second batch.
        s.want_boxes([15]);
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        // Stepping to a hit works out its boxes at once.
        s.current = Some(11);
        assert_eq!(s.step(true).map(|h| (h.page, h.boxed)), Some((12, true)));
        assert!(!s.hits[12].boxes.is_empty());
    }
}
