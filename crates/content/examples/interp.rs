//! Interprets every page of the given PDFs and prints throughput and command counts (no
//! text is printed). `cargo run --release -p pagebara-content --example interp -- a.pdf b.pdf`
//!
//! `--text PAGE FILE` prints a page's extracted text. `--mupdf N FILE…` compares open +
//! interpret of all pages (no rasterisation), and that plus text extraction, with MuPDF's
//! `mutool draw -F trace` and `-F stext` (best of N wall times, `mutool` startup included),
//! printing the load average before and after each file. `--unmapped FILE…` counts
//! extracted characters without a Unicode mapping (U+FFFD) with and without reading the
//! embedded font programs; `--unmapped-strip FILE…` does the same with every `/ToUnicode`
//! removed, also counting characters that differ from the text extracted with them.
//! `--hash FILE…` prints a hash of each page's full extracted [`TextPage`] (and of its
//! marked-content texts), one line per page, to check that layout changes keep the output.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use pagebara_content::{
    ContentParser, Interpreter, Matrix, TextPage, extract_marked, extract_text, page_contents,
};
use pagebara_syntax::{Dict, Document, Object};

const ROUNDS: usize = 3;

fn main() {
    let mut paths: Vec<String> = std::env::args().skip(1).collect();
    if paths.is_empty() {
        eprintln!("usage: interp FILE.pdf… | interp --text PAGE FILE.pdf");
        return;
    }
    if paths[0] == "--text" && paths.len() == 3 {
        print_text(paths[1].parse().unwrap_or(1), &paths[2]);
        return;
    }
    if paths[0] == "--unmapped" || paths[0] == "--unmapped-strip" {
        for p in &paths[1..] {
            unmapped(p, paths[0] == "--unmapped-strip");
        }
        return;
    }
    if paths[0] == "--hash" {
        for p in &paths[1..] {
            hash_pages(p);
        }
        return;
    }
    if paths[0] == "--mupdf" && paths.len() > 2 {
        let n = paths[1].parse().unwrap_or(5);
        for p in &paths[2..] {
            vs_mupdf(n, p);
        }
        return;
    }

    paths.retain(|p| !p.starts_with("--"));
    let mut total = Totals::default();
    for path in &paths {
        let Ok(bytes) = std::fs::read(path) else {
            eprintln!("{path}: cannot read");
            continue;
        };
        let doc = match Document::open(bytes) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("{path}: {e}");
                continue;
            }
        };
        let pages = pages(&doc);
        let t = Instant::now();
        let contents: Vec<Vec<u8>> = pages.iter().map(|(p, _)| page_contents(&doc, p)).collect();
        let decode = t.elapsed();
        let bytes: usize = contents.iter().map(Vec::len).sum();

        let t = Instant::now();
        let mut ops = 0u64;
        for c in &contents {
            let mut p = ContentParser::new(c);
            while p.next().is_some() {
                ops += 1;
            }
        }
        let parse = t.elapsed();

        let mut interp_best = Duration::MAX;
        let mut lists = Vec::new();
        for _ in 0..ROUNDS {
            // A fresh interpreter each round so font loading is measured too.
            let mut interp = Interpreter::new(&doc);
            let t = Instant::now();
            lists = pages
                .iter()
                .zip(&contents)
                .map(|((_, res), c)| interp.content(c, res, Matrix::IDENTITY))
                .collect();
            interp_best = interp_best.min(t.elapsed());
        }

        let t = Instant::now();
        let mut chars = 0;
        for l in &lists {
            chars += extract_text(&doc, l).chars.len();
        }
        let extract = t.elapsed();

        let mut counts = BTreeMap::new();
        let (mut glyphs, mut heap) = (0, 0);
        for l in &lists {
            for c in &l.cmds {
                *counts.entry(c.kind()).or_insert(0usize) += 1;
            }
            glyphs += l.glyphs.len();
            heap += l.heap_size();
        }
        println!(
            "{}: {} pages, {:.1} MB content, {} ops",
            short(path),
            pages.len(),
            mb(bytes),
            ops
        );
        println!("  decode    {decode:>10.2?}");
        println!(
            "  parse     {parse:>10.2?}  {:>7.1} Mops/s {:>8.1} MB/s",
            rate(ops as f64, parse) / 1e6,
            rate(mb(bytes), parse)
        );
        println!(
            "  interpret {interp_best:>10.2?}  {:>7.1} Mops/s {:>8.1} MB/s  (best of {ROUNDS})",
            rate(ops as f64, interp_best) / 1e6,
            rate(mb(bytes), interp_best)
        );
        println!(
            "  extract   {extract:>10.2?}  {chars} chars  ({:.1} Mchars/s)",
            rate(chars as f64, extract) / 1e6
        );
        println!("  display lists: {glyphs} glyphs, {:.1} MB", mb(heap));
        println!("  commands: {counts:?}");
        total.add(bytes, ops, parse, interp_best, &counts);
    }
    if paths.len() > 1 {
        println!(
            "TOTAL: {:.1} MB, {} ops, parse {:.1} Mops/s {:.1} MB/s, interpret {:.1} Mops/s {:.1} MB/s",
            mb(total.bytes),
            total.ops,
            rate(total.ops as f64, total.parse) / 1e6,
            rate(mb(total.bytes), total.parse),
            rate(total.ops as f64, total.interp) / 1e6,
            rate(mb(total.bytes), total.interp),
        );
        println!("  commands: {:?}", total.counts);
    }
}

fn vs_mupdf(n: usize, path: &str) {
    let Ok(bytes) = std::fs::read(path) else {
        eprintln!("{path}: cannot read");
        return;
    };
    let load = || {
        std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next().map(str::to_owned))
            .unwrap_or_default()
    };
    let before = load();
    let ours = |extract: bool| {
        best(n, || {
            let Ok(doc) = Document::open(bytes.clone()) else {
                return;
            };
            let mut interp = Interpreter::new(&doc);
            for (p, res) in pages(&doc) {
                let list = interp.page(&p, &res);
                if extract {
                    std::hint::black_box(extract_text(&doc, &list));
                }
                std::hint::black_box(list);
            }
        })
    };
    let mutool = |format: &str| {
        best(n, || {
            let ok = std::process::Command::new("mutool")
                .args(["draw", "-q", "-F", format, "-o", "/dev/null", path])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            assert!(ok, "mutool failed on {path}");
        })
    };
    let (interp, trace) = (ours(false), mutool("trace"));
    let (extract, stext) = (ours(true), mutool("stext"));
    println!(
        "{}: interpret {interp:.1?} vs trace {trace:.1?} ({:.2}x); +extract {extract:.1?} vs \
         stext {stext:.1?} ({:.2}x); load {before} -> {}",
        short(path),
        trace.as_secs_f64() / interp.as_secs_f64(),
        stext.as_secs_f64() / extract.as_secs_f64(),
        load()
    );
}

fn hash_pages(path: &str) {
    let Ok(doc) = std::fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|b| Document::open(b).map_err(|e| e.to_string()))
    else {
        eprintln!("{path}: cannot open");
        return;
    };
    let mut interp = Interpreter::new(&doc);
    for (i, (p, res)) in pages(&doc).iter().enumerate() {
        let list = interp.page(p, res);
        let mut h = std::hash::DefaultHasher::new();
        hash_text(&extract_text(&doc, &list), &mut h);
        for (mcid, t) in extract_marked(&doc, &list) {
            std::hash::Hash::hash(&mcid, &mut h);
            hash_text(&t, &mut h);
        }
        println!(
            "{} {} {:016x}",
            short(path),
            i + 1,
            std::hash::Hasher::finish(&h)
        );
    }
}

/// Hashes every field of a text page; floats by their bits.
fn hash_text(t: &TextPage, h: &mut impl std::hash::Hasher) {
    use std::hash::Hash;
    let rect = |r: &pagebara_content::Rect, h: &mut _| {
        [r.x0, r.y0, r.x1, r.y1].map(f32::to_bits).hash(h);
    };
    t.text.hash(h);
    t.chars.len().hash(h);
    for c in &t.chars {
        (c.text, c.glyph).hash(h);
        c.quad.map(|p| [p.x.to_bits(), p.y.to_bits()]).hash(h);
    }
    t.words.len().hash(h);
    for w in &t.words {
        w.chars.hash(h);
        rect(&w.bbox, h);
    }
    t.lines.len().hash(h);
    for l in &t.lines {
        (l.chars, l.words, l.vertical, l.rtl).hash(h);
        rect(&l.bbox, h);
    }
    t.blocks.len().hash(h);
    for b in &t.blocks {
        b.lines.hash(h);
        rect(&b.bbox, h);
    }
}

fn best(n: usize, mut f: impl FnMut()) -> Duration {
    (0..n.max(1))
        .map(|_| {
            let t = Instant::now();
            f();
            t.elapsed()
        })
        .min()
        .unwrap_or_default()
}

fn unmapped(path: &str, strip: bool) {
    let Ok(mut doc) = std::fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|b| Document::open(b).map_err(|e| e.to_string()))
    else {
        eprintln!("{path}: cannot open");
        return;
    };
    let texts = |doc: &Document, mut interp: Interpreter| -> Vec<Vec<char>> {
        pages(doc)
            .iter()
            .map(|(p, res)| {
                extract_text(doc, &interp.page(p, res))
                    .text
                    .chars()
                    .collect()
            })
            .collect()
    };
    let original = texts(&doc, Interpreter::new(&doc));
    if strip {
        for n in doc.object_numbers() {
            let Some(r) = doc.reference(n) else { continue };
            let Some(o) = doc.get(r) else { continue };
            if let Object::Dict(d) = &*o
                && d.name(b"Type") == Some(b"Font")
                && d.contains(b"ToUnicode")
            {
                let mut d = d.clone();
                d.remove(b"ToUnicode");
                doc.set(r, d);
            }
        }
    }
    let (mut without, mut with) = (Vec::new(), Vec::new());
    let t0 = best(ROUNDS, || {
        without = texts(&doc, Interpreter::new(&doc).without_font_programs())
    });
    let t1 = best(ROUNDS, || with = texts(&doc, Interpreter::new(&doc)));
    let bad = |t: &[Vec<char>]| t.iter().flatten().filter(|&&c| c == '\u{FFFD}').count();
    let edits = |t: &[Vec<char>]| -> usize {
        t.iter()
            .zip(&original)
            .map(|(a, b)| edit_distance(a, b))
            .sum()
    };
    println!(
        "{}: {} chars, unmapped {} -> {}, edits from the original text {} -> {} \
         (interpret + extract {t0:.1?} -> {t1:.1?})",
        short(path),
        original.iter().map(Vec::len).sum::<usize>(),
        bad(&without),
        bad(&with),
        edits(&without),
        edits(&with),
    );
}

/// Levenshtein distance.
fn edit_distance(a: &[char], b: &[char]) -> usize {
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut diag = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (diag + usize::from(ca != cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            diag = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// Prints the extracted text of a page (1-based), to inspect reading order.
fn print_text(page: usize, path: &str) {
    let Ok(doc) = std::fs::read(path)
        .map_err(|e| e.to_string())
        .and_then(|b| Document::open(b).map_err(|e| e.to_string()))
    else {
        eprintln!("{path}: cannot open");
        return;
    };
    let pages = pages(&doc);
    let Some((p, res)) = pages.get(page.saturating_sub(1)) else {
        eprintln!("{path}: no page {page}");
        return;
    };
    let list = Interpreter::new(&doc).page(p, res);
    println!("{}", extract_text(&doc, &list).text);
}

#[derive(Default)]
struct Totals {
    bytes: usize,
    ops: u64,
    parse: Duration,
    interp: Duration,
    counts: BTreeMap<&'static str, usize>,
}

impl Totals {
    fn add(
        &mut self,
        b: usize,
        o: u64,
        p: Duration,
        i: Duration,
        c: &BTreeMap<&'static str, usize>,
    ) {
        self.bytes += b;
        self.ops += o;
        self.parse += p;
        self.interp += i;
        for (k, v) in c {
            *self.counts.entry(k).or_insert(0) += v;
        }
    }
}

fn mb(b: usize) -> f64 {
    b as f64 / 1e6
}

fn rate(x: f64, d: Duration) -> f64 {
    x / d.as_secs_f64().max(1e-9)
}

fn short(p: &str) -> &str {
    p.rsplit('/').next().unwrap_or(p)
}

/// Pages in order with their inherited resources (a minimal page tree walk).
fn pages(doc: &Document) -> Vec<(Dict, Dict)> {
    let mut out = Vec::new();
    let Some(cat) = doc.catalog() else { return out };
    let Some(root) = cat.as_dict().and_then(|c| c.get(b"Pages")).cloned() else {
        return out;
    };
    let mut stack = vec![(root, Dict::new(), 0)];
    let mut seen = std::collections::HashSet::new();
    while let Some((node, inherited, depth)) = stack.pop() {
        if depth > 64 || node.as_objref().is_some_and(|r| !seen.insert(r)) {
            continue;
        }
        let n = doc.resolve(&node);
        let Some(d) = n.as_dict() else { continue };
        let res = match &*doc.get_in(d, b"Resources") {
            Object::Dict(r) => r.clone(),
            _ => inherited,
        };
        match doc.get_in(d, b"Kids").as_array() {
            Some(kids) => {
                for k in kids.iter().rev() {
                    stack.push((k.clone(), res.clone(), depth + 1));
                }
            }
            None => out.push((d.clone(), res)),
        }
    }
    out
}
