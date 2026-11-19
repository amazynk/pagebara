//! The actions themselves.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use pagebara_pages::PageTree;
use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_syntax::{Dict, Document, Error, Object, Stream};
use rayon::prelude::*;

use crate::out::{self, parent, stem};
use crate::{Options, Report};

/// Pages wider or taller than this many pixels are rendered smaller.
const MAX_SIDE: f32 = 16384.0;

pub fn open(path: &Path, password: Option<&str>) -> Result<Document, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read: {e}"))?;
    match Document::open(bytes) {
        Ok(d) => Ok(d),
        Err(Error::PasswordRequired(locked)) => match password {
            Some(p) => locked.unlock(p).map_err(|_| "wrong password".to_owned()),
            None => Err("protected with a password; open it in the app first".into()),
        },
        Err(e) => Err(format!("not a readable PDF: {e}")),
    }
}

/// The incremental save when the file allows one (signatures stay valid), else a full one.
pub fn save(doc: &Document) -> Result<Vec<u8>, String> {
    match doc.save_incremental() {
        Ok(b) => Ok(b),
        Err(_) => doc.save_full().map_err(|e| e.to_string()),
    }
}

/// Where one output of several inputs goes: `opts.output` names a file only when there is
/// one input; with several it is a folder.
fn target(opts: &Options, inputs: usize) -> Option<&Path> {
    opts.output.as_deref().filter(|p| inputs == 1 || p.is_dir())
}

fn folder(opts: &Options, input: &Path) -> PathBuf {
    opts.output.clone().unwrap_or_else(|| parent(input))
}

fn write(
    report: &mut Report,
    opts: &Options,
    inputs: usize,
    first: &Path,
    base: &str,
    bytes: &[u8],
) -> Result<(), String> {
    let p = out::write_output(target(opts, inputs), &parent(first), base, bytes)
        .map_err(|e| format!("cannot write: {e}"))?;
    report.outputs.push(p);
    Ok(())
}

pub fn merge(
    files: &[PathBuf],
    opts: &Options,
    report: &mut Report,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> usize {
    let mut docs = Vec::new();
    let mut errors = 0;
    for (i, f) in files.iter().enumerate() {
        match open(f, opts.password.as_deref()) {
            Ok(d) => docs.push(d),
            Err(e) => {
                report.messages.push(format!("{}: {e}", f.display()));
                errors += 1;
            }
        }
        progress(i + 1, files.len() + 1);
    }
    if errors > 0 {
        report
            .messages
            .push("Nothing was merged: every file must open.".into());
        return errors;
    }
    let refs: Vec<&Document> = docs.iter().collect();
    let r = pagebara_pages::merge(&refs)
        .map_err(|e| e.to_string())
        .and_then(|d| d.save_full().map_err(|e| e.to_string()))
        .and_then(|b| {
            let base = format!("{} (merged)", stem(&files[0]));
            write(report, opts, 1, &files[0], &base, &b)
        });
    progress(files.len() + 1, files.len() + 1);
    match r {
        Ok(()) => 0,
        Err(e) => {
            report.messages.push(e);
            1
        }
    }
}

pub fn split(
    file: &Path,
    _inputs: usize,
    opts: &Options,
    report: &mut Report,
) -> Result<(), String> {
    let doc = open(file, opts.password.as_deref())?;
    let n = PageTree::open(&doc).len();
    if n < 2 {
        return Err("has only one page".into());
    }
    let ranges: Vec<_> = (0..n).map(|i| i..i + 1).collect();
    let parts = pagebara_pages::split(&doc, &ranges).map_err(|e| e.to_string())?;
    let dir = folder(opts, file);
    let base = stem(file);
    let digits = n.to_string().len();
    for (i, d) in parts.iter().enumerate() {
        let bytes = d.save_full().map_err(|e| e.to_string())?;
        let name = format!("{base} - page {:0digits$}", i + 1);
        let p = out::write_unique(&dir, &name, "pdf", &bytes)
            .map_err(|e| format!("cannot write: {e}"))?;
        report.outputs.push(p);
    }
    Ok(())
}

pub fn rotate(
    file: &Path,
    turns: i32,
    inputs: usize,
    opts: &Options,
    report: &mut Report,
) -> Result<(), String> {
    let mut doc = open(file, opts.password.as_deref())?;
    let n = PageTree::open(&doc).len();
    let all: Vec<usize> = (0..n).collect();
    pagebara_pages::rotate(&mut doc, &all, turns).map_err(|e| e.to_string())?;
    let bytes = save(&doc)?;
    match target(opts, inputs) {
        Some(_) => write(report, opts, inputs, file, &stem(file), &bytes),
        None => {
            out::replace_atomic(file, &bytes).map_err(|e| format!("cannot write: {e}"))?;
            report.outputs.push(file.to_owned());
            Ok(())
        }
    }
}

pub fn compress(
    file: &Path,
    inputs: usize,
    opts: &Options,
    report: &mut Report,
) -> Result<(), String> {
    let doc = open(file, opts.password.as_deref())?;
    let o = opts.optimize.unwrap_or_default();
    let out = pagebara_optimize::optimize(&doc, &o, None).map_err(|e| e.to_string())?;
    let r = &out.report;
    for w in &r.warnings {
        report.messages.push(format!("{}: {w}", file.display()));
    }
    let before = std::fs::metadata(file).map(|m| m.len()).unwrap_or(r.before);
    let after = out.bytes.len() as u64;
    if after >= before && !o.linearize {
        report.messages.push(format!(
            "{}: already compact ({}); left as it is",
            file.display(),
            size(before)
        ));
        return Ok(());
    }
    write(
        report,
        opts,
        inputs,
        file,
        &format!("{} (compressed)", stem(file)),
        &out.bytes,
    )?;
    let parts: Vec<String> = r
        .categories
        .iter()
        .filter(|c| c.before != c.after)
        .map(|c| {
            format!(
                "{} {} → {}",
                c.category.title().to_lowercase(),
                size(c.before),
                size(c.after)
            )
        })
        .collect();
    report.messages.push(format!(
        "{}: {} → {} ({}% smaller{}{})",
        file.display(),
        size(before),
        size(after),
        100 - after * 100 / before.max(1),
        if parts.is_empty() { "" } else { "; " },
        parts.join(", ")
    ));
    Ok(())
}

/// The profile asked for, PDF/A-2b by default.
fn profile(opts: &Options) -> pagebara_conform::Profile {
    opts.conformance
        .unwrap_or(pagebara_conform::Profile::PdfA2b)
}

pub fn pdfa(file: &Path, inputs: usize, opts: &Options, report: &mut Report) -> Result<(), String> {
    let p = profile(opts);
    if p.part().is_none() {
        return Err(format!("{} is not a PDF/A part", p.title()));
    }
    let doc = open(file, opts.password.as_deref())?;
    let c = pagebara_conform::pdfa::convert(&doc, p).map_err(|e| e.to_string())?;
    let tag = p.title().replace('/', "-");
    write(
        report,
        opts,
        inputs,
        file,
        &format!("{} ({tag})", stem(file)),
        &c.bytes,
    )?;
    for ch in &c.changes {
        report.messages.push(format!("{}: {ch}", file.display()));
    }
    if c.report.passed() {
        report
            .messages
            .push(format!("{}: conforms to {}", file.display(), p.title()));
    } else {
        report.failed = true;
        report.messages.push(format!(
            "{}: written, but {} problem{} remain:\n{}",
            file.display(),
            c.report.errors(),
            if c.report.errors() == 1 { "" } else { "s" },
            c.report.to_text()
        ));
    }
    Ok(())
}

pub fn validate(file: &Path, opts: &Options, report: &mut Report) -> Result<(), String> {
    let doc = open(file, opts.password.as_deref())?;
    let r = pagebara_conform::validate(&doc, profile(opts));
    report.failed |= !r.passed();
    report.printed.push(if opts.json {
        r.to_json()
    } else {
        format!("{}: {}", file.display(), r.to_text().trim_end())
    });
    Ok(())
}

fn size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 20 => format!("{:.1} MB", b as f64 / (1 << 20) as f64),
        b if b >= 1 << 10 => format!("{:.0} KB", b as f64 / 1024.0),
        b => format!("{b} bytes"),
    }
}

pub fn images(
    file: &Path,
    _inputs: usize,
    opts: &Options,
    report: &mut Report,
) -> Result<(), String> {
    let r = Renderer::new(open(file, opts.password.as_deref())?);
    let count = r.page_count();
    if count == 0 {
        return Err("has no pages".into());
    }
    let dir = out::create_dir_unique(&folder(opts, file), &format!("{} pages", stem(file)))
        .map_err(|e| format!("cannot make the folder: {e}"))?;
    let dpi = opts.dpi.unwrap_or(150.0).clamp(1.0, 2400.0);
    let digits = count.to_string().len();
    let written: Vec<Result<PathBuf, String>> = (0..count)
        .into_par_iter()
        .map(|page| {
            let png = render_png(&r, page, dpi)?;
            let p = dir.join(format!("page-{:0digits$}.png", page + 1));
            std::fs::write(&p, png).map_err(|e| format!("cannot write: {e}"))?;
            Ok(p)
        })
        .collect();
    for w in written {
        report.outputs.push(w?);
    }
    Ok(())
}

fn render_png(r: &Renderer, page: usize, dpi: f32) -> Result<Vec<u8>, String> {
    let (w, h) = r
        .page_size(page)
        .ok_or_else(|| format!("no page {}", page + 1))?;
    let scale = (dpi / 72.0).min(MAX_SIDE / w.max(h).max(1.0));
    let (pw, ph) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
    let req = RenderRequest::new(
        scale,
        PixelRect {
            x: 0,
            y: 0,
            w: pw.max(1),
            h: ph.max(1),
        },
    );
    let img = r
        .render(page, &req)
        .map_err(|e| format!("page {}: {e:?}", page + 1))?;
    // On white paper every pixel is opaque: RGB is a third smaller.
    let rgb: Vec<u8> = img
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    pagebara_png::encode(img.width, img.height, pagebara_png::ColorType::Rgb, &rgb)
        .map_err(|e| e.to_string())
}

/// Images to one PDF, a page per image (96 dpi, shrunk to fit A4 when larger), in the
/// order given.
pub fn convert(
    files: &[PathBuf],
    opts: &Options,
    report: &mut Report,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> usize {
    let mut doc = pagebara_pages::blank((1, 7));
    let mut pages = Vec::new();
    let mut errors = 0;
    for (i, f) in files.iter().enumerate() {
        match add_image_page(&mut doc, f) {
            Ok(p) => pages.push(p),
            Err(e) => {
                report.messages.push(format!("{}: {e}", f.display()));
                errors += 1;
            }
        }
        progress(i + 1, files.len() + 1);
    }
    if pages.is_empty() {
        return errors.max(1);
    }
    let r = finish_pages(&mut doc, pages)
        .and_then(|()| doc.save_full().map_err(|e| e.to_string()))
        .and_then(|b| write(report, opts, 1, &files[0], &stem(&files[0]), &b));
    progress(files.len() + 1, files.len() + 1);
    match r {
        Ok(()) => errors,
        Err(e) => {
            report.messages.push(e);
            errors + 1
        }
    }
}

fn pages_root(doc: &Document) -> Result<pagebara_syntax::ObjRef, String> {
    doc.catalog()
        .and_then(|c| c.as_dict()?.get(b"Pages")?.as_objref())
        .ok_or_else(|| "no page tree".to_owned())
}

fn add_image_page(doc: &mut Document, path: &Path) -> Result<pagebara_syntax::ObjRef, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read: {e}"))?;
    let img = pagebara_signatures::decode_image(&bytes).map_err(|e| e.to_string())?;
    let image = pagebara_annot::embed_image(
        doc,
        pagebara_annot::RgbaImage {
            width: img.width,
            height: img.height,
            pixels: &img.pixels,
        },
    )
    .map_err(|e| e.to_string())?;
    let (mut w, mut h) = (f64::from(img.width) * 0.75, f64::from(img.height) * 0.75);
    let fit = (595.0 / w).min(842.0 / h).min(1.0);
    w *= fit;
    h *= fit;
    let content = format!("q {w:.3} 0 0 {h:.3} 0 0 cm /Im0 Do Q");
    let contents = doc.add(Stream::new(Dict::new(), content.into_bytes()));
    let mut xobjects = Dict::new();
    xobjects.set(b"Im0", image);
    let mut res = Dict::new();
    res.set(b"XObject", xobjects);
    let mut page = Dict::new();
    page.set(b"Type", Object::name(b"Page"));
    page.set(b"Parent", pages_root(doc)?);
    page.set(
        b"MediaBox",
        vec![
            Object::Int(0),
            Object::Int(0),
            Object::Real(w),
            Object::Real(h),
        ],
    );
    page.set(b"Resources", res);
    page.set(b"Contents", contents);
    Ok(doc.add(page))
}

fn finish_pages(doc: &mut Document, pages: Vec<pagebara_syntax::ObjRef>) -> Result<(), String> {
    let root = pages_root(doc)?;
    let mut tree = Dict::new();
    tree.set(b"Type", Object::name(b"Pages"));
    tree.set(b"Count", pages.len() as i64);
    tree.set(
        b"Kids",
        pages.into_iter().map(Object::Ref).collect::<Vec<_>>(),
    );
    doc.set(root, tree);
    Ok(())
}

pub fn ocr(
    files: &[PathBuf],
    opts: &Options,
    report: &mut Report,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> usize {
    let engine = match load_engine(opts) {
        Ok(e) => e,
        Err(e) => {
            report.messages.push(e);
            return files.len();
        }
    };
    let never = AtomicBool::new(false);
    let mut errors = 0;
    for (i, f) in files.iter().enumerate() {
        let r = (|| {
            let mut doc = open(f, opts.password.as_deref())?;
            let each = |p: pagebara_ocr::Progress| {
                progress(i * 100 + p.done * 100 / p.total.max(1), files.len() * 100)
            };
            let summary = pagebara_ocr::ocr_document(
                &mut doc,
                &engine,
                &pagebara_ocr::Options::default(),
                &each,
                &never,
            )
            .map_err(|e| e.to_string())?;
            if summary.recognized.is_empty() {
                report.messages.push(format!(
                    "{}: no page needed recognition (all have text, or none was found); nothing written",
                    f.display()
                ));
                return Ok(());
            }
            let bytes = save(&doc)?;
            write(
                report,
                opts,
                files.len(),
                f,
                &format!("{} (searchable)", stem(f)),
                &bytes,
            )?;
            report.messages.push(format!(
                "{}: {} page(s) recognised, {} words",
                f.display(),
                summary.recognized.len(),
                summary.words
            ));
            Ok::<(), String>(())
        })();
        if let Err(e) = r {
            report.messages.push(format!("{}: {e}", f.display()));
            errors += 1;
        }
        progress((i + 1) * 100, files.len() * 100);
    }
    errors
}

/// The Latin models, built in or from `opts.models_dir`; never downloaded.
fn load_engine(opts: &Options) -> Result<pagebara_ocr::Engine, String> {
    let mut sources = vec![pagebara_ocr::ModelSource::Bundled];
    if let Some(d) = &opts.models_dir {
        sources.push(pagebara_ocr::ModelSource::Dir(d.clone()));
    }
    let never = AtomicBool::new(false);
    pagebara_ocr::Engine::load(&sources, pagebara_ocr::Script::Latin, &|_| {}, &never).map_err(|e| match e {
        pagebara_ocr::Error::ModelUnavailable(_) => {
            "The text recognition models are not installed. Open a scanned document in the app once to download them.".to_owned()
        }
        e => format!("text recognition: {e}"),
    })
}
