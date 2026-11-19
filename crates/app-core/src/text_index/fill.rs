//! Extracting many pages at once: on a pool of its own (the global one renders tiles), one
//! interpreter per worker so fonts are read once per run of pages, not once per page.

use std::ops::Range;
use std::sync::atomic::Ordering;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use memchr::memmem::Finder;
use pagebara_content::TextPage;
use rayon::ThreadPool;
use rayon::prelude::*;
use unicode_segmentation::UnicodeSegmentation;

use super::{Bbox, TextIndex, chars_of, fold, fold_text, line_boxes};

/// How long after a document opens background indexing starts: its first pages render first.
pub(super) const BACKGROUND_DELAY: Duration = Duration::from_millis(1500);

/// Pages indexed in the background between checks for a search or a closed document.
const BACKGROUND_CHUNK: usize = 512;

/// How often paused background indexing looks whether the search is over.
const PAUSE: Duration = Duration::from_millis(50);

/// About half the cores, leaving the rest to rendering and the UI.
fn pool() -> &'static ThreadPool {
    static POOL: OnceLock<ThreadPool> = OnceLock::new();
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism().map_or(2, |n| n.get());
        pagebara_thread::rayon_pool("text")
            .num_threads((n / 2).clamp(1, 16))
            .build()
            .expect("text pool")
    })
}

/// How a document Find query matches the original page text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindOptions {
    pub match_case: bool,
    pub whole_words: bool,
}

#[derive(Debug)]
pub struct Query {
    /// The index stores lowercase text, also useful for rejecting pages in a case-sensitive search.
    folded: Finder<'static>,
    exact: Option<Finder<'static>>,
    pub(crate) options: FindOptions,
}

impl Query {
    pub fn new(query: &str) -> Option<Query> {
        Self::with_options(query, FindOptions::default())
    }

    /// An empty or whitespace-only query searches nothing.
    pub fn with_options(query: &str, options: FindOptions) -> Option<Query> {
        let query = query.trim();
        if query.is_empty() {
            return None;
        }
        let folded = Finder::new(fold_text(query).as_bytes()).into_owned();
        let exact = options.match_case.then(|| {
            let text = query.split_whitespace().collect::<Vec<_>>().join(" ");
            Finder::new(text.as_bytes()).into_owned()
        });
        Some(Query {
            folded,
            exact,
            options,
        })
    }

    fn in_text(&self, folded: &str) -> bool {
        self.folded.find(folded.as_bytes()).is_some()
    }

    /// Counts without loading the original page when the folded index contains enough information.
    fn count_folded(&self, folded: &str) -> usize {
        if !self.options.whole_words {
            return self.folded.find_iter(folded.as_bytes()).count();
        }
        if self.in_text(folded) {
            self.matches(folded).count()
        } else {
            0
        }
    }

    /// Non-overlapping matches in normalized text. A rejected partial word can overlap
    /// the start of a valid phrase, so retry at the next word boundary in that case.
    fn matches<'a>(&'a self, text: &'a str) -> impl Iterator<Item = Range<usize>> + 'a {
        let finder = self.exact.as_ref().unwrap_or(&self.folded);
        let boundaries = self.options.whole_words.then(|| {
            text.split_word_bound_indices()
                .map(|(at, _)| at)
                .chain(std::iter::once(text.len()))
                .collect::<Vec<_>>()
        });
        let mut next = 0;
        std::iter::from_fn(move || {
            loop {
                let start = next + finder.find(&text.as_bytes()[next..])?;
                let end = start + finder.needle().len();
                if let Some(bounds) = &boundaries
                    && (bounds.binary_search(&start).is_err()
                        || bounds.binary_search(&end).is_err())
                {
                    next = bounds[bounds.partition_point(|&b| b <= start)];
                    continue;
                }
                next = end;
                return Some(start..end);
            }
        })
    }

    /// Byte ranges in the original page, used for counts, highlights and result previews.
    pub(crate) fn ranges(&self, page: &TextPage) -> Vec<Range<usize>> {
        let normalized = fold(&page.text, self.exact.is_some());
        self.matches(&normalized.text)
            .map(|r| {
                let start = normalized.map[r.start];
                let last = normalized.map[r.end - 1];
                let end = normalized.map[r.end];
                // A lowercase expansion can match only part of one source character (e.g. İ).
                let end = if end == last {
                    last + page.text[last..].chars().next().unwrap().len_utf8()
                } else {
                    end
                };
                start..end
            })
            .collect()
    }
}

/// The boxes of each hit of `query` on a page, one per line of the hit.
pub(crate) fn boxes_of(page: &TextPage, query: &Query) -> Vec<Vec<Bbox>> {
    query
        .ranges(page)
        .into_iter()
        .map(|r| line_boxes(page, chars_of(page, r)))
        .collect()
}

impl TextIndex {
    /// Finds hits in page order, indexing new pages along the way. Case-sensitive queries
    /// check the original text of candidate pages. Other queries can count hits directly
    /// in the folded index. Ask for boxes here or load them later with [`TextIndex::hit_boxes`].
    pub fn search(
        &self,
        pages: Range<usize>,
        query: &Query,
        with_boxes: bool,
        stop: &(dyn Fn() -> bool + Sync),
    ) -> Vec<(usize, Option<Vec<Bbox>>)> {
        let found: Vec<Vec<(usize, Option<Vec<Bbox>>)>> = pool().install(|| {
            pages
                .into_par_iter()
                .map_init(
                    || self.interpreter(),
                    |interp, i| {
                        if stop() {
                            return Vec::new();
                        }
                        let page = match self.folded(i) {
                            Some(f) if !with_boxes && !query.options.match_case => {
                                return vec![(i, None); query.count_folded(f)];
                            }
                            Some(f) if !query.in_text(f) => return Vec::new(),
                            Some(_) => self.page_with(interp, i),
                            None => {
                                let t = self.extract(interp, i);
                                if !self.folded(i).is_some_and(|f| query.in_text(f)) {
                                    return Vec::new();
                                }
                                self.keep(i, t.clone());
                                t
                            }
                        };
                        let ranges = query.ranges(&page);
                        if !with_boxes && query.exact.is_some() {
                            return vec![(i, None); ranges.len()];
                        }
                        ranges
                            .into_iter()
                            .map(|r| (i, Some(line_boxes(&page, chars_of(&page, r)))))
                            .collect()
                    },
                )
                .collect()
        });
        found.into_iter().flatten().collect()
    }

    /// The boxes of each hit of `query` on page `i`, in order.
    pub fn hit_boxes(&self, i: usize, query: &Query) -> Vec<Vec<Bbox>> {
        boxes_of(&self.page(i), query)
    }

    /// Indexes the pages of `pages` not indexed yet, until `stop` says so; returns whether
    /// all of them are.
    pub fn fill(&self, pages: Range<usize>, stop: &(dyn Fn() -> bool + Sync)) -> bool {
        pool().install(|| {
            pages
                .clone()
                .into_par_iter()
                .filter(|&i| self.folded(i).is_none())
                .for_each_init(
                    || self.interpreter(),
                    |interp, i| {
                        if !stop() {
                            self.extract(interp, i);
                        }
                    },
                )
        });
        pages.into_iter().all(|i| self.folded(i).is_some())
    }

    /// Indexes every page on a background thread after `delay`, then keeps the index on disk
    /// (when it has a cache directory). Waits while a search runs, and stops once the index
    /// is dropped elsewhere or retired.
    pub fn index_in_background(self: &Arc<Self>, delay: Duration) {
        let weak = Arc::downgrade(self);
        pagebara_thread::spawn("text-index", move || {
            std::thread::sleep(delay);
            let mut next = 0;
            loop {
                let Some(index) = weak.upgrade() else { return };
                let gone =
                    || Arc::strong_count(&index) <= 1 || index.retired.load(Ordering::Acquire);
                if gone() {
                    return;
                }
                if next == 0 {
                    index.load_cached();
                }
                if index.searches.load(Ordering::Acquire) > 0 {
                    drop(index);
                    std::thread::sleep(PAUSE);
                    continue;
                }
                if next >= index.len() {
                    index.save_cached();
                    return;
                }
                let pages = next..(next + BACKGROUND_CHUNK).min(index.len());
                let stop = || gone() || index.searches.load(Ordering::Acquire) > 0;
                if index.fill(pages.clone(), &stop) {
                    next = pages.end;
                }
            }
        });
    }
}
