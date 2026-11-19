//! Rendering speed against `mutool draw`, same pages and resolution.
//!
//! ```text
//! cargo run --release -p pagebara-render --example bench-render -- [--dpi 150] [--pages 3]
//!     [--runs 5] [--threads 8] [--spread] file.pdf...
//! ```
//!
//! `--spread` takes the pages evenly across each document instead of the first ones.
//!
//! For each page, best of `--runs`:
//! - `cold`: a fresh renderer (interpretation, decoding and font loading included), one
//!   thread, the whole page in one image;
//! - `warm`: the same renderer again (display list, images and glyphs cached);
//! - `tiles`: a fresh renderer, the page as 256×256 tiles on `--threads` threads;
//! - MuPDF: `mutool draw -s t` page time, one thread, and banded with `-T threads -B 256`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use pagebara_render::{PixelRect, RenderRequest, Renderer};
use rayon::prelude::*;

struct Args {
    dpi: f32,
    pages: usize,
    runs: usize,
    threads: usize,
    spread: bool,
    files: Vec<PathBuf>,
}

fn args() -> Args {
    let mut a = Args {
        dpi: 150.0,
        pages: 3,
        runs: 5,
        threads: 8,
        spread: false,
        files: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        let mut num = || it.next().and_then(|v| v.parse::<f32>().ok());
        match s.as_str() {
            "--dpi" => a.dpi = num().unwrap_or(a.dpi),
            "--pages" => a.pages = num().map_or(a.pages, |v| v as usize),
            "--runs" => a.runs = num().map_or(a.runs, |v| v as usize),
            "--threads" => a.threads = num().map_or(a.threads, |v| v as usize),
            "--spread" => a.spread = true,
            _ => a.files.push(PathBuf::from(s)),
        }
    }
    a
}

/// MuPDF's own page time in ms (`-s t`), best of `runs`.
fn mutool(file: &Path, page: usize, dpi: f32, runs: usize, threads: usize) -> Option<f64> {
    let mut best = f64::MAX;
    for _ in 0..runs {
        let mut cmd = Command::new("mutool");
        cmd.args(["draw", "-q", "-s", "t", "-r", &dpi.to_string(), "-c", "rgb"]);
        if threads > 1 {
            cmd.args(["-T", &threads.to_string(), "-B", "256"]);
        }
        let out = cmd
            .args(["-o", "/dev/null", "-F", "pnm"])
            .arg(file)
            .arg((page + 1).to_string())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        // "total 7ms (0ms layout) / 1 pages for an average of 7ms"
        let ms = text.lines().find_map(|l| {
            let rest = l.strip_prefix("total ")?;
            rest.split_whitespace()
                .next()?
                .trim_end_matches("ms")
                .parse::<f64>()
                .ok()
        })?;
        best = best.min(ms);
    }
    Some(best)
}

fn load(f: &Path) -> Option<Renderer> {
    let doc = pagebara_syntax::Document::open(std::fs::read(f).ok()?).ok()?;
    Some(Renderer::new(doc))
}

fn main() {
    let a = args();
    let pool = pagebara_thread::rayon_pool("ren-bench")
        .num_threads(a.threads)
        .build()
        .expect("pool");
    let load_avg = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    println!(
        "dpi {} runs {} threads {} loadavg {}",
        a.dpi,
        a.runs,
        a.threads,
        load_avg
            .split_whitespace()
            .take(3)
            .collect::<Vec<_>>()
            .join(" ")
    );
    println!(
        "{:<28} {:>4} {:>10} {:>9} {:>9} {:>9} {:>10} {:>10}",
        "file", "page", "size", "cold ms", "warm ms", "tiles ms", "mupdf 1t", "mupdf Nt"
    );
    let (mut sum_cold, mut sum_warm, mut sum_tiles, mut sum_mu1, mut sum_mun) =
        (0.0, 0.0, 0.0, 0.0, 0.0);
    for f in &a.files {
        let Some(r0) = load(f) else {
            eprintln!("cannot open {}", f.display());
            continue;
        };
        let name: String = f
            .file_stem()
            .map_or("?".into(), |s| s.to_string_lossy().into_owned())
            .chars()
            .take(28)
            .collect();
        let count = r0.page_count();
        let n = count.min(a.pages);
        let pages: Vec<usize> = if a.spread {
            (0..n).map(|i| i * count / n.max(1)).collect()
        } else {
            (0..n).collect()
        };
        for page in pages {
            let Some((pw, ph)) = r0.page_size(page) else {
                continue;
            };
            let s = a.dpi / 72.0;
            let (w, h) = ((pw * s).ceil() as u32, (ph * s).ceil() as u32);
            let full = RenderRequest::new(s, PixelRect { x: 0, y: 0, w, h });
            let mut cold = f64::MAX;
            let mut warm = f64::MAX;
            let mut tiles = f64::MAX;
            for _ in 0..a.runs {
                let r = load(f).expect("reload");
                let t = Instant::now();
                let _ = r.render(page, &full);
                cold = cold.min(t.elapsed().as_secs_f64() * 1e3);
                let t = Instant::now();
                let _ = r.render(page, &full);
                warm = warm.min(t.elapsed().as_secs_f64() * 1e3);
                let r = load(f).expect("reload");
                let rects: Vec<PixelRect> = (0..h.div_ceil(256))
                    .flat_map(|ty| {
                        (0..w.div_ceil(256)).map(move |tx| PixelRect {
                            x: tx as i32 * 256,
                            y: ty as i32 * 256,
                            w: 256.min(w - tx * 256),
                            h: 256.min(h - ty * 256),
                        })
                    })
                    .collect();
                let t = Instant::now();
                pool.install(|| {
                    rects.par_iter().for_each(|rect| {
                        let req = RenderRequest::new(s, *rect);
                        let _ = r.render(page, &req);
                    })
                });
                tiles = tiles.min(t.elapsed().as_secs_f64() * 1e3);
            }
            let mu1 = mutool(f, page, a.dpi, a.runs, 1).unwrap_or(f64::NAN);
            let mun = mutool(f, page, a.dpi, a.runs, a.threads).unwrap_or(f64::NAN);
            println!(
                "{name:<28} {page:>4} {:>10} {cold:>9.1} {warm:>9.1} {tiles:>9.1} {mu1:>10.1} {mun:>10.1}",
                format!("{w}x{h}")
            );
            sum_cold += cold;
            sum_warm += warm;
            sum_tiles += tiles;
            sum_mu1 += mu1;
            sum_mun += mun;
        }
    }
    println!(
        "total ms: cold {sum_cold:.0}  warm {sum_warm:.0}  tiles {sum_tiles:.0}  mupdf 1t {sum_mu1:.0}  mupdf Nt {sum_mun:.0}"
    );
}
