//! Measures find-in-document on a large PDF: a search without an index, after background
//! indexing, a second query, after reopening with the index on disk, and after deleting a
//! page (the other pages keep their text).
//! `cargo run --release -p pagebara-app-core --example bench-search -- FILE QUERY…`
//! (`PAGEBARA_BENCH_CACHE=DIR` keeps the disk index there instead of a temporary folder).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use pagebara_app_core::find::Search;
use pagebara_app_core::session::Session;
use pagebara_app_core::text_index::TextIndex;
use pagebara_syntax::Document;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path, queries @ ..] = args.as_slice() else {
        eprintln!("usage: bench-search FILE QUERY…");
        return;
    };
    let bytes = std::fs::read(path).expect("read");
    let doc = Document::open(bytes.clone()).expect("open");
    let refs = pagebara_pages::PageTree::open(&doc).refs();
    let n = refs.len();
    println!("{n} pages, load average {}", load());
    let cache = std::env::var_os("PAGEBARA_BENCH_CACHE").map_or_else(
        || std::env::temp_dir().join(format!("pagebara-bench-search-{}", std::process::id())),
        PathBuf::from,
    );
    let _ = std::fs::remove_dir_all(&cache);

    // 1. No index yet: the first query extracts every page, the next scan the index.
    let index = Arc::new(TextIndex::new(doc.clone(), refs.clone()));
    let mut counts = Vec::new();
    for (k, q) in queries.iter().enumerate() {
        let r = search(&index, q);
        let what = if k == 0 { "cold" } else { "next query" };
        println!("{what} {q:?}: {r}");
        counts.push(r.hits);
    }
    let text: usize = (0..n).map(|i| index.folded(i).map_or(0, str::len)).sum();
    println!(
        "index: {:.1} MB of folded text",
        text as f64 / (1 << 20) as f64
    );
    drop(index);

    // 2. Indexed in the background first, then saved to disk.
    let index = Arc::new(TextIndex::new(doc.clone(), refs.clone()).with_cache_dir(cache.clone()));
    let t = Instant::now();
    index.index_in_background(Duration::ZERO);
    let saved = || std::fs::read_dir(&cache).is_ok_and(|mut d| d.next().is_some());
    while !index.is_indexed() {
        std::thread::sleep(Duration::from_millis(5));
    }
    let indexed = t.elapsed();
    while !saved() {
        std::thread::sleep(Duration::from_millis(5));
    }
    let disk: u64 = std::fs::read_dir(&cache)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok()?.metadata().ok())
        .map(|m| m.len())
        .sum();
    println!(
        "background indexing {indexed:.2?}, saved after {:.2?} ({:.1} MB on disk)",
        t.elapsed(),
        disk as f64 / (1 << 20) as f64
    );
    for (q, &c) in queries.iter().zip(&counts) {
        let r = search(&index, q);
        println!("after background indexing {q:?}: {r}");
        assert_eq!(r.hits, c, "hits differ from the cold search");
    }
    drop(index);

    // 3. Reopened: the index comes from disk.
    let t = Instant::now();
    let doc = Document::open(bytes).expect("open");
    let refs = pagebara_pages::PageTree::open(&doc).refs();
    let opened = t.elapsed();
    let index = Arc::new(TextIndex::new(doc, refs).with_cache_dir(cache.clone()));
    let t = Instant::now();
    index.load_cached();
    println!(
        "reopen: document {opened:.2?}, index from disk {:.2?} ({} of {n} pages)",
        t.elapsed(),
        index.indexed()
    );
    drop(index);
    for (q, &c) in queries.iter().zip(&counts) {
        let doc = Document::open(std::fs::read(path).expect("read")).expect("open");
        let refs = pagebara_pages::PageTree::open(&doc).refs();
        let index = Arc::new(TextIndex::new(doc, refs).with_cache_dir(cache.clone()));
        let r = search(&index, q);
        println!("reopened, first search {q:?} (loading the index included): {r}");
        assert_eq!(r.hits, c, "hits differ from the cold search");
    }
    if std::env::var_os("PAGEBARA_BENCH_CACHE").is_none() {
        let _ = std::fs::remove_dir_all(&cache);
    }

    // 4. A page deleted from an indexed document.
    let doc = Document::open(std::fs::read(path).expect("read")).expect("open");
    let mut s = Session::new(None, doc);
    s.text.fill(0..n, &|| false);
    let t = Instant::now();
    s.edit("Delete page", |d| pagebara_pages::delete(d, &[0]))
        .expect("delete");
    println!(
        "delete page 1: {:.2?}, {} of {} pages still indexed",
        t.elapsed(),
        s.text.indexed(),
        s.page_count()
    );
    let t = Instant::now();
    let again = TextIndex::open_after(s.doc().clone(), s.tree.refs(), Some(&s.text));
    println!(
        "  of which taking over the text: {:.2?} ({} pages)",
        t.elapsed(),
        again.indexed()
    );
    for q in queries {
        let r = search(&s.text, q);
        println!("after deleting a page {q:?}: {r}");
    }
}

struct Run {
    first: Duration,
    all: Duration,
    hits: usize,
}

impl std::fmt::Display for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "first hit {:.2?}, all {:.2?}, {} hits",
            self.first, self.all, self.hits
        )
    }
}

/// The app's search: time to the first hit and to the end.
fn search(index: &Arc<TextIndex>, q: &str) -> Run {
    let mut s = Search::default();
    s.query = q.into();
    let (tx, rx) = mpsc::channel();
    let t = Instant::now();
    s.start(index.clone(), 0, move |b| {
        let _ = tx.send(b);
    });
    let mut first = None;
    loop {
        let b = rx.recv().expect("search ended");
        let done = b.done;
        s.add(b, 0);
        if first.is_none() && !s.hits.is_empty() {
            first = Some(t.elapsed());
        }
        if done {
            break;
        }
    }
    let all = t.elapsed();
    Run {
        first: first.unwrap_or(all),
        all,
        hits: s.hits.len(),
    }
}

fn load() -> String {
    std::fs::read_to_string("/proc/loadavg")
        .map(|s| s.split_whitespace().take(3).collect::<Vec<_>>().join(" "))
        .unwrap_or_else(|_| "?".into())
}
