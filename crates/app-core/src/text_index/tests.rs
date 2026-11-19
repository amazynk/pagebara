use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;
use crate::test_pdf;

fn page() -> Arc<TextPage> {
    let doc = Document::open(test_pdf::text(&["Hello brave World", "second line here"])).unwrap();
    let tree = pagebara_pages::PageTree::load(&doc);
    TextIndex::new(doc, tree.refs()).page(0)
}

#[test]
fn finds_ignoring_case_and_line_breaks() {
    let p = page();
    assert!(p.text.contains("Hello"), "{:?}", p.text);
    let hits = find(&p, "world second");
    assert_eq!(hits.len(), 1);
    let chars = chars_of(&p, hits[0].clone());
    assert_eq!(text_of(&p, chars.clone()), "World\nsecond");
    assert_eq!(line_boxes(&p, chars).len(), 2);
    assert_eq!(find(&p, "HELLO").len(), 1);
    assert!(find(&p, "  ").is_empty());
}

#[test]
fn whole_words_use_unicode_boundaries_and_keep_source_ranges() {
    let p = TextPage {
        text: "bobcat cat cat, CAT cat_2 cat2 cat’s écat caté cafe\u{301} 猫犬 İ".into(),
        ..TextPage::default()
    };
    let matching = |needle, match_case, whole_words| {
        Query::with_options(
            needle,
            FindOptions {
                match_case,
                whole_words,
            },
        )
        .unwrap()
        .ranges(&p)
        .into_iter()
        .map(|r| &p.text[r])
        .collect::<Vec<_>>()
    };
    assert_eq!(matching("cat", false, true), ["cat", "cat", "CAT"]);
    assert_eq!(matching("cat", true, true), ["cat", "cat"]);
    assert_eq!(matching("cat cat", false, true), ["cat cat"]);
    assert!(matching("cafe", false, true).is_empty());
    assert_eq!(matching("猫", true, true), ["猫"]);
    assert_eq!(matching("i", false, false), ["İ"]);

    let p = page();
    let options = FindOptions {
        match_case: true,
        whole_words: true,
    };
    let query = Query::with_options(" World   second ", options).unwrap();
    let hits = query.ranges(&p);
    assert_eq!(hits.len(), 1);
    assert_eq!(&p.text[hits[0].clone()], "World\nsecond");
    assert_eq!(boxes_of(&p, &query)[0].len(), 2);
}

#[test]
fn filtered_counts_and_highlights_agree_on_cold_and_warm_pages() {
    let doc = Document::open(test_pdf::pages(&[&["pin In in in_2"], &["Inside in!"]])).unwrap();
    for (match_case, whole_words, counts) in [
        (false, false, [4, 2]),
        (true, false, [3, 1]),
        (false, true, [2, 1]),
        (true, true, [1, 1]),
    ] {
        let ix = index(&doc);
        let q = Query::with_options(
            "in",
            FindOptions {
                match_case,
                whole_words,
            },
        )
        .unwrap();
        let cold = ix.search(0..2, &q, true, &|| false);
        let warm = ix.search(0..2, &q, false, &|| false);
        let pages: Vec<_> = counts
            .into_iter()
            .enumerate()
            .flat_map(|(p, n)| std::iter::repeat_n(p, n))
            .collect();
        assert_eq!(cold.iter().map(|h| h.0).collect::<Vec<_>>(), pages);
        assert_eq!(
            warm,
            pages.into_iter().map(|p| (p, None)).collect::<Vec<_>>()
        );
        for page in 0..2 {
            let boxes: Vec<_> = cold
                .iter()
                .filter(|h| h.0 == page)
                .map(|h| h.1.clone().unwrap())
                .collect();
            assert!(boxes.iter().all(|b| !b.is_empty()));
            assert_eq!(ix.hit_boxes(page, &q), boxes);
        }
    }
}

#[test]
fn carets_words_and_lines() {
    let p = page();
    let first = p.lines[0].bbox;
    let c = caret_at(&p, first.x0 - 5.0, (first.y0 + first.y1) / 2.0).unwrap();
    assert_eq!(c, 0);
    let end = caret_at(&p, first.x1 + 50.0, (first.y0 + first.y1) / 2.0).unwrap();
    assert_eq!(text_of(&p, 0..end), "Hello brave World");
    let w = word_at(&p, 7).unwrap();
    assert_eq!(text_of(&p, w), "brave");
    let l = line_at(&p, 7).unwrap();
    assert_eq!(text_of(&p, l), "Hello brave World");
}

#[test]
fn folded_text_is_the_matching_fold() {
    for s in [
        "",
        "  Hello\n\n World\t",
        "İstanbul ǅemal ΣΑΣ straße",
        "a\u{a0}\u{2003}b ﬁ ẞ",
    ] {
        assert_eq!(fold_text(s), fold(s, false).text, "{s:?}");
    }
}

/// `n` pages, with "needle" on some of them (twice on every third, across two lines on
/// every fifth).
fn book(n: usize) -> Document {
    let lines: Vec<Vec<String>> = (0..n)
        .map(|i| {
            let mut l = vec![format!("Page {i} of the book"), "nothing here".to_owned()];
            if i % 3 == 0 {
                l.push(format!("the Needle on page {i}, NEEDLE again"));
            }
            if i % 5 == 0 {
                l.push("a needle".to_owned());
                l.push("split across lines".to_owned());
            }
            l
        })
        .collect();
    let pages: Vec<Vec<&str>> = lines
        .iter()
        .map(|p| p.iter().map(String::as_str).collect())
        .collect();
    let pages: Vec<&[&str]> = pages.iter().map(Vec::as_slice).collect();
    Document::open(test_pdf::pages(&pages)).unwrap()
}

fn index(doc: &Document) -> TextIndex {
    let refs = pagebara_pages::PageTree::open(doc).refs();
    TextIndex::new(doc.clone(), refs)
}

/// Hits as searched before the index: every page extracted and matched.
fn page_by_page(index: &TextIndex, query: &str) -> Vec<(usize, Vec<Bbox>)> {
    (0..index.len())
        .flat_map(|i| {
            let p = index.page(i);
            find(&p, query)
                .into_iter()
                .map(|r| (i, line_boxes(&p, chars_of(&p, r))))
                .collect::<Vec<_>>()
        })
        .collect()
}

#[test]
fn indexed_search_matches_page_by_page() {
    let doc = book(40);
    let never = || false;
    for q in [
        "needle",
        "NEEDLE AGAIN",
        "needle split",
        "page 3",
        "absent",
        "of",
    ] {
        let expected: Vec<_> = page_by_page(&index(&doc), q)
            .into_iter()
            .map(|(p, b)| (p, Some(b)))
            .collect();
        let query = Query::new(q).unwrap();
        // Cold, then on the filled index, then in two parts.
        let ix = index(&doc);
        assert_eq!(ix.search(0..40, &query, true, &never), expected, "{q}");
        assert!(ix.is_indexed());
        assert_eq!(ix.search(0..40, &query, true, &never), expected, "{q}");
        let ix = index(&doc);
        assert!(ix.fill(0..40, &never));
        let mut parts = ix.search(25..40, &query, true, &never);
        parts.splice(0..0, ix.search(0..25, &query, true, &never));
        assert_eq!(parts, expected, "{q}");
        // Without boxes: the same hits, counted on the folded text.
        let counted: Vec<_> = expected.iter().map(|(p, _)| (*p, None)).collect();
        assert_eq!(ix.search(0..40, &query, false, &never), counted, "{q}");
    }
    assert!(!page_by_page(&index(&doc), "needle split").is_empty());
    assert!(Query::new(" \n ").is_none());
}

#[test]
fn search_stops_when_told() {
    let doc = book(60);
    let ix = index(&doc);
    let checks = AtomicUsize::new(0);
    let stop = || checks.fetch_add(1, Ordering::AcqRel) >= 3;
    let hits = ix.search(0..60, &Query::new("page").unwrap(), true, &stop);
    assert_eq!(ix.indexed(), 3);
    let mut pages: Vec<usize> = hits.iter().map(|h| h.0).collect();
    pages.dedup();
    assert!(pages.len() <= 3);
    let checks = AtomicUsize::new(0);
    let stop = || checks.fetch_add(1, Ordering::AcqRel) >= 5;
    assert!(!ix.fill(0..60, &stop));
    assert_eq!(ix.indexed(), 8);
}

fn temp_dir(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("pagebara-text-index-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn files_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|d| d.filter_map(|e| Some(e.ok()?.path())).collect())
        .unwrap_or_default()
}

#[test]
fn index_is_kept_on_disk() {
    let dir = temp_dir("disk");
    let doc = book(12);
    let never = || false;
    let ix = index(&doc).with_cache_dir(dir.clone());
    ix.load_cached();
    ix.save_cached();
    assert!(files_in(&dir).is_empty(), "saved before indexed");
    assert!(ix.fill(0..12, &never));
    ix.save_cached();
    let files = files_in(&dir);
    assert_eq!(files.len(), 1);

    // Reopened: every page is known without extracting any.
    let reopened = Document::open(doc.source().to_vec()).unwrap();
    let ix2 = index(&reopened).with_cache_dir(dir.clone());
    ix2.load_cached();
    assert!(ix2.is_indexed());
    assert!((0..12).all(|i| ix2.folded(i) == ix.folded(i)));
    assert!(ix2.cached(0).is_none());

    // A damaged file is ignored, then replaced.
    let good = std::fs::read(&files[0]).unwrap();
    let mut header = good.clone();
    header[9] ^= 1;
    for damaged in [
        header,
        good[..good.len() - 8].to_vec(),
        b"junk".to_vec(),
        Vec::new(),
    ] {
        std::fs::write(&files[0], damaged).unwrap();
        let ix3 = index(&reopened).with_cache_dir(dir.clone());
        ix3.load_cached();
        assert_eq!(ix3.indexed(), 0);
        assert!(ix3.fill(0..12, &never));
        ix3.save_cached();
        assert_eq!(std::fs::read(&files[0]).unwrap(), good);
    }

    // A changed document is not kept: its text may differ from the file's.
    std::fs::remove_file(&files[0]).unwrap();
    let mut changed = reopened.clone();
    changed.add(pagebara_syntax::Object::Null);
    let ix4 = index(&changed).with_cache_dir(dir.clone());
    assert!(ix4.fill(0..12, &never));
    ix4.save_cached();
    assert!(files_in(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

fn wait_for(done: impl Fn() -> bool) {
    let t = Instant::now();
    while !done() && t.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn background_indexing_fills_and_stops() {
    let doc = book(30);
    let dir = temp_dir("background");
    let ix = Arc::new(index(&doc).with_cache_dir(dir.clone()));
    ix.index_in_background(Duration::ZERO);
    wait_for(|| !files_in(&dir).is_empty());
    assert!(ix.is_indexed());
    assert_eq!(files_in(&dir).len(), 1);
    let _ = std::fs::remove_dir_all(&dir);

    // Dropped or retired before it starts: nothing is indexed, nothing kept alive.
    let delay = Duration::from_millis(100);
    let ix = Arc::new(index(&doc));
    ix.index_in_background(delay);
    let weak = Arc::downgrade(&ix);
    drop(ix);
    let retired = Arc::new(index(&doc));
    retired.index_in_background(delay);
    retired.retire();
    std::thread::sleep(delay * 4);
    assert_eq!(weak.strong_count(), 0);
    assert_eq!(retired.indexed(), 0);
}
