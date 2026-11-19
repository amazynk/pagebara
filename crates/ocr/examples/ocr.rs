//! Makes the scanned pages of a PDF searchable.
//!
//! ```text
//! ocr <in.pdf> <out.pdf> [--models DIR] [--script latin|chinese|korean|arabic|cyrillic|
//!     devanagari|greek|thai] [--dpi N] [--force] [--pages 1,3-5] [--threads N]
//!     [--parallel N] [--print] [--download]
//! ```
//!
//! Models are read from `--models` (default `$PAGEBARA_OCR_MODELS`); with the `download`
//! feature and `--download`, missing ones are fetched into that directory.

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

use pagebara_ocr::{Engine, ModelSource, Options, Script, ocr_document};
use pagebara_syntax::Document;

fn script(s: &str) -> Option<Script> {
    Some(match s {
        "latin" => Script::Latin,
        "chinese" => Script::Chinese,
        "korean" => Script::Korean,
        "arabic" => Script::Arabic,
        "cyrillic" => Script::Cyrillic,
        "devanagari" => Script::Devanagari,
        "greek" => Script::Greek,
        "thai" => Script::Thai,
        _ => return None,
    })
}

fn pages(s: &str) -> Option<Vec<usize>> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let (a, b) = part.split_once('-').unwrap_or((part, part));
        let (a, b): (usize, usize) = (a.trim().parse().ok()?, b.trim().parse().ok()?);
        out.extend(a.checked_sub(1)?..b);
    }
    Some(out)
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut files = Vec::new();
    let mut opts = Options::default();
    let mut dir = std::env::var_os("PAGEBARA_OCR_MODELS").map(std::path::PathBuf::from);
    let mut sc = Script::Latin;
    let mut threads = None;
    let mut print = false;
    let mut download = false;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = || it.next().cloned().unwrap_or_default();
        match a.as_str() {
            "--models" => dir = Some(value().into()),
            "--script" => match script(&value()) {
                Some(s) => sc = s,
                None => return usage(),
            },
            "--dpi" => opts.dpi = value().parse().unwrap_or(300.0),
            "--force" => opts.force = true,
            "--pages" => opts.pages = pages(&value()),
            "--threads" => threads = value().parse().ok(),
            "--parallel" => opts.parallel_pages = value().parse().unwrap_or(1),
            "--print" => print = true,
            "--download" => download = true,
            f if !f.starts_with("--") => files.push(f.to_owned()),
            _ => return usage(),
        }
    }
    let ([input, output], Some(dir)) = (<[String; 2]>::try_from(files).unwrap_or_default(), dir)
    else {
        return usage();
    };
    if input.is_empty() {
        return usage();
    }
    let source = if download {
        #[cfg(feature = "download")]
        {
            ModelSource::Download {
                dir,
                fetch: std::sync::Arc::new(pagebara_ocr::HttpFetch),
            }
        }
        #[cfg(not(feature = "download"))]
        {
            eprintln!("built without the `download` feature");
            return ExitCode::FAILURE;
        }
    } else {
        ModelSource::Dir(dir)
    };

    let cancel = AtomicBool::new(false);
    let t = Instant::now();
    let engine = match Engine::load(
        &[source],
        sc,
        &|p| eprint!("\r{}: {} / {} bytes", p.file, p.done, p.total),
        &cancel,
    ) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("loading models: {e}");
            return ExitCode::FAILURE;
        }
    };
    let engine = match threads {
        Some(n) => engine.with_threads(n),
        None => engine,
    };
    eprintln!("models loaded in {:.2?}", t.elapsed());

    let data = match std::fs::read(&input) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut doc = match Document::open(data) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{input}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let t = Instant::now();
    let progress =
        |p: pagebara_ocr::Progress| eprintln!("page {} ({}/{})", p.page + 1, p.done, p.total);
    let summary = match ocr_document(&mut doc, &engine, &opts, &progress, &cancel) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ocr: {e}");
            return ExitCode::FAILURE;
        }
    };
    let secs = t.elapsed().as_secs_f32();
    let n = summary.recognized.len() + summary.empty.len();
    eprintln!(
        "{} pages recognised ({} words), {} skipped (have text), {} empty; {:.2}s, {:.2}s/page",
        summary.recognized.len(),
        summary.words,
        summary.skipped.len(),
        summary.empty.len(),
        secs,
        secs / n.max(1) as f32
    );
    if print {
        for &p in &summary.recognized {
            println!("--- page {}\n{}", p + 1, pagebara_ocr::page_text(&doc, p));
        }
    }
    let saved = doc.save_incremental().or_else(|_| doc.save_full());
    match saved.map(|b| std::fs::write(&output, b)) {
        Ok(Ok(())) => ExitCode::SUCCESS,
        Ok(Err(e)) => {
            eprintln!("{output}: {e}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("saving: {e}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: ocr <in.pdf> <out.pdf> [--models DIR] [--script NAME] [--dpi N] [--force] \
         [--pages 1,3-5] [--threads N] [--parallel N] [--print] [--download]"
    );
    ExitCode::FAILURE
}
