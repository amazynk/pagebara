//! Renders pages with our engine and with MuPDF at the same resolution and reports how
//! they differ, worst pages first, with thumbnails.

mod image;
mod report;

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use corpus_check::child::{self, Exit, Limits};
use corpus_check::{engine, walk};
use serde_json::{Value, json};

use crate::image::{Rgb, compare};

const USAGE: &str = "usage:
  pixel-diff run [--out DIR] [--dpi N] [--pages N] [--jobs N] [--timeout S]
                 [--known FILE] [--baseline OLD/results.jsonl] PATH...
      renders the first pages of every *.pdf under PATH with pagebara and with mutool draw
      (and pdfium_test when installed) and writes DIR/results.jsonl, DIR/report.md and
      DIR/report.html with thumbnails of the worst pages (default DIR: pixel-diff-out)
  pixel-diff summary RESULTS.jsonl [--known FILE] [--baseline OLD.jsonl]
  pixel-diff job FILE --dpi N --pages N --thumbs DIR     (internal: one file)";

/// Pages differing by less than this mean per channel get no thumbnails.
const THUMB_MAE: f64 = 2.0;
const THUMB_SIDE: u32 = 260;

fn main() {
    std::process::exit(match real_main() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("pixel-diff: {e}\n{USAGE}");
            2
        }
    });
}

struct Args {
    rest: Vec<String>,
    opts: std::collections::HashMap<String, String>,
}

impl Args {
    fn parse(mut it: impl Iterator<Item = String>) -> Args {
        let mut a = Args {
            rest: Vec::new(),
            opts: Default::default(),
        };
        while let Some(s) = it.next() {
            match s.strip_prefix("--") {
                Some(k) => {
                    let v = it.next().unwrap_or_default();
                    a.opts.insert(k.to_owned(), v);
                }
                None => a.rest.push(s),
            }
        }
        a
    }

    fn num<T: std::str::FromStr>(&self, k: &str, default: T) -> Result<T, String> {
        self.opts.get(k).map_or(Ok(default), |v| {
            v.parse().map_err(|_| format!("--{k}: bad number {v}"))
        })
    }

    fn path(&self, k: &str) -> Option<PathBuf> {
        self.opts.get(k).map(PathBuf::from)
    }
}

fn real_main() -> Result<i32, String> {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().ok_or("no command")?;
    let a = Args::parse(it);
    match cmd.as_str() {
        "job" => {
            let file = a.rest.first().ok_or("job needs a file")?;
            let thumbs = a.path("thumbs").ok_or("job needs --thumbs")?;
            job(
                Path::new(file),
                a.num("dpi", 50.0)?,
                a.num("pages", 3)?,
                &thumbs,
            );
            Ok(0)
        }
        "run" => {
            if a.rest.is_empty() {
                return Err("run needs a path".into());
            }
            let out = a.path("out").unwrap_or_else(|| "pixel-diff-out".into());
            let thumbs = out.join("thumbs");
            std::fs::create_dir_all(&thumbs).map_err(|e| e.to_string())?;
            let roots: Vec<PathBuf> = a.rest.iter().map(PathBuf::from).collect();
            let items = walk::pdfs(&roots);
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let jobs = a.num(
                "jobs",
                std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).max(1)),
            )?;
            let limits = Limits {
                timeout: Duration::from_secs(a.num("timeout", 120)?),
                memory_mb: 4096,
            };
            let (dpi, pages) = (a.num("dpi", 50.0f32)?, a.num("pages", 3usize)?);
            eprintln!(
                "{} files, {jobs} jobs, {dpi} dpi, {pages} pages each",
                items.len()
            );
            let mut sink = std::fs::File::create(out.join("results.jsonl"))
                .map(std::io::BufWriter::new)
                .map_err(|e| e.to_string())?;
            let mut results = Vec::new();
            let total = items.len();
            child::parallel(
                &items,
                jobs,
                |item| run_one(&exe, item, dpi, pages, &thumbs, limits),
                |i, recs| {
                    if i % 200 == 0 {
                        eprintln!("[{i}/{total}]");
                    }
                    for r in recs {
                        let _ = writeln!(sink, "{r}");
                        results.push(r);
                    }
                },
            );
            sink.flush().map_err(|e| e.to_string())?;
            let known = report::known(a.path("known").as_deref());
            let baseline = a.path("baseline").map(|p| report::load(&p)).transpose()?;
            let (md, html, keep) = report::render(&results, &known, baseline.as_deref());
            report::prune_thumbs(&thumbs, &keep);
            std::fs::write(out.join("report.md"), &md).map_err(|e| e.to_string())?;
            std::fs::write(out.join("report.html"), html).map_err(|e| e.to_string())?;
            println!("{}", md.lines().take(24).collect::<Vec<_>>().join("\n"));
            Ok(0)
        }
        "summary" => {
            let p = a.rest.first().ok_or("summary needs results.jsonl")?;
            let results = report::load(Path::new(p))?;
            let known = report::known(a.path("known").as_deref());
            let baseline = a.path("baseline").map(|p| report::load(&p)).transpose()?;
            print!(
                "{}",
                report::render(&results, &known, baseline.as_deref()).0
            );
            Ok(0)
        }
        _ => Err(format!("unknown command {cmd}")),
    }
}

/// Runs the job for one file in a child process; one record per page.
fn run_one(
    exe: &Path,
    item: &walk::Item,
    dpi: f32,
    pages: usize,
    thumbs: &Path,
    limits: Limits,
) -> Vec<Value> {
    let file = item.path.to_string_lossy().into_owned();
    let (d, p, t) = (dpi.to_string(), pages.to_string(), thumbs.to_string_lossy());
    let args = ["job", &file, "--dpi", &d, "--pages", &p, "--thumbs", &t];
    let base = json!({ "file": file, "corpus": item.corpus });
    let with = |mut v: Value| {
        if let (Value::Object(m), Value::Object(b)) = (&mut v, &base) {
            m.extend(b.clone());
        }
        v
    };
    match child::run(&exe.to_string_lossy(), &args, limits) {
        Ok(o) => {
            let mut recs: Vec<Value> = o
                .stdout
                .lines()
                .filter_map(|l| serde_json::from_str(l).ok())
                .map(with)
                .collect();
            if o.exit != Exit::Code(0) {
                let why = match o.exit {
                    Exit::Timeout => "timeout".to_owned(),
                    e => format!("{e:?}"),
                };
                recs.push(with(json!({ "ref": "mupdf", "error": why })));
            }
            recs
        }
        Err(e) => vec![with(json!({ "ref": "mupdf", "error": e.to_string() }))],
    }
}

/// Pages rendered no larger than this many pixels on a side, by both readers.
const MAX_SIDE: f32 = 4000.0;

/// The resolution page `i` is compared at: `dpi`, lowered for very large pages.
fn page_dpi(r: &pagebara_render::Renderer, i: usize, dpi: f32) -> f32 {
    let (w, h) = r.page_size(i).unwrap_or((0.0, 0.0));
    dpi.min(72.0 * MAX_SIDE / w.max(h).max(1.0))
}

/// Our rendering of page `i`, or why there is none.
fn ours(r: &pagebara_render::Renderer, i: usize, dpi: f32) -> Result<Rgb, String> {
    let img = engine::render_page(r, i, dpi, f32::MAX)?;
    Ok(Rgb::from_rgba(img.width, img.height, &img.data))
}

/// MuPDF's rendering of page `i` (0-based).
fn mupdf(file: &Path, dpi: f32, i: usize, tmp: &Path) -> Option<Rgb> {
    let out = tmp.join(format!("p{i}.pnm"));
    let args = [
        "draw",
        "-q",
        "-r",
        &dpi.to_string(),
        "-c",
        "rgb",
        "-o",
        &out.to_string_lossy(),
        &file.to_string_lossy(),
        &(i + 1).to_string(),
    ];
    let limits = Limits {
        timeout: Duration::from_secs(60),
        memory_mb: 4096,
    };
    let _ = child::run("mutool", &args, limits);
    let img = std::fs::read(&out).ok().and_then(|b| Rgb::from_pnm(&b));
    let _ = std::fs::remove_file(out);
    img
}

/// The rendering of page `i` the PDFium test suite expects (`NAME_expected.pdf.I.png`
/// next to `NAME.pdf`, rendered at 72 dpi), on white.
fn pdfium_expected(file: &Path, i: usize) -> Option<Rgb> {
    let stem = file.file_stem()?.to_string_lossy();
    let png = file.with_file_name(format!("{stem}_expected.pdf.{i}.png"));
    let img = pagebara_png::decode(&std::fs::read(png).ok()?, &Default::default()).ok()?;
    let rgba = img.to_rgba8();
    let data = rgba
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            let a = u32::from(p[3]);
            [0, 1, 2].map(|c| ((u32::from(p[c]) * a + 255 * (255 - a) + 127) / 255) as u8)
        })
        .collect();
    Some(Rgb {
        w: img.width,
        h: img.height,
        data,
    })
}

fn id(file: &Path) -> String {
    let h = file
        .to_string_lossy()
        .bytes()
        .fold(0xcbf29ce484222325u64, |h, c| {
            (h ^ u64::from(c)).wrapping_mul(0x100000001b3)
        });
    format!("{h:016x}")
}

/// The child process: compares the first `pages` pages of one file with MuPDF's (and
/// with PDFium's expected images where the file has them), one JSON line per comparison.
fn job(file: &Path, dpi: f32, pages: usize, thumbs: &Path) {
    corpus_check::guard::install();
    let mut out = std::io::stdout().lock();
    let doc = std::fs::read(file)
        .map_err(|e| e.to_string())
        .and_then(|b| engine::open(b).map_err(|e| format!("{e:?}")));
    let r = match doc {
        Ok(d) => pagebara_render::Renderer::new(d),
        Err(e) => {
            let _ = writeln!(out, "{}", json!({ "page": 0, "ref": "mupdf", "error": e }));
            return;
        }
    };
    let tmp = thumbs.join(format!(".tmp-{}", id(file)));
    let _ = std::fs::create_dir_all(&tmp);
    for i in 0..r.page_count().min(pages) {
        let d = page_dpi(&r, i, dpi);
        let refs = [
            ("mupdf", d, mupdf(file, d, i, &tmp)),
            ("pdfium-expected", 72.0, pdfium_expected(file, i)),
        ];
        for (name, d, theirs) in refs {
            if name != "mupdf" && theirs.is_none() {
                continue;
            }
            let mine = corpus_check::guard::catch(|| ours(&r, i, d)).and_then(|r| r);
            let mut rec = json!({ "page": i + 1, "ref": name });
            match (mine, theirs) {
                (Ok(a), Some(b)) => {
                    let (m, diff) = compare(&a, &b);
                    rec["ours"] = json!([a.w, a.h]);
                    rec["theirs"] = json!([b.w, b.h]);
                    rec["mae"] = ((m.mae * 1000.0).round() / 1000.0).into();
                    rec["over"] = ((m.over * 1000.0).round() / 1000.0).into();
                    rec["ssim"] = ((m.ssim * 10000.0).round() / 10000.0).into();
                    if m.mae >= THUMB_MAE || a.w.abs_diff(b.w) > 2 || a.h.abs_diff(b.h) > 2 {
                        let thumb = format!("{}-p{}-{name}", id(file), i + 1);
                        let side = Rgb::side_by_side(&[
                            a.thumbnail(THUMB_SIDE),
                            b.thumbnail(THUMB_SIDE),
                            diff.thumbnail(THUMB_SIDE),
                        ]);
                        let _ = std::fs::write(thumbs.join(format!("{thumb}.png")), side.png());
                        rec["thumb"] = thumb.into();
                    }
                }
                (Err(e), Some(_)) => rec["error"] = format!("ours: {e}").into(),
                (Ok(_), None) => rec["error"] = format!("{name} rendered nothing").into(),
                (Err(e), None) => rec["error"] = format!("both failed: {e}").into(),
            }
            let panics = corpus_check::guard::take();
            if !panics.is_empty() {
                rec["panics"] = panics.into();
            }
            let _ = writeln!(out, "{rec}");
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
}
