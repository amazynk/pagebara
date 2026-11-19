//! Command-line output for scripts and AI tools, without opening a window:
//! `--text FILE` prints the text page by page (`--json`: one JSON object per page, with
//! text blocks and their boxes); `--images FILE` writes pages as PNG files.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use pagebara_app_core::text_export;
use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_syntax::{Document, Error};
use rayon::prelude::*;

/// What to export, from the command line.
#[derive(Debug)]
pub struct Export<'a> {
    pub file: &'a Path,
    pub pages: Option<&'a str>,
    pub password: Option<&'a str>,
    /// `--text` as JSON lines.
    pub json: bool,
}

/// Pages wider or taller than this many pixels are rendered smaller.
const MAX_SIDE: f32 = 16384.0;

/// Prints the text; returns the exit code.
pub fn text(e: &Export) -> i32 {
    exit_code(e.file, print_text(e))
}

/// Writes `page-N.png` files into `dir`, printing each path; returns the exit code.
pub fn images(e: &Export, dir: &Path, dpi: f32) -> i32 {
    exit_code(e.file, write_images(e, dir, dpi))
}

fn exit_code(file: &Path, r: io::Result<()>) -> i32 {
    match r {
        Ok(()) => 0,
        // `pagebara --text big.pdf | head` closes the pipe early: not an error.
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => 0,
        Err(e) => {
            eprintln!("pagebara: {}: {e}", file.display());
            1
        }
    }
}

fn print_text(e: &Export) -> io::Result<()> {
    let doc = open(e)?;
    let tree = pagebara_pages::PageTree::open(&doc);
    let pages = tree.pages();
    let list = page_list(e.pages, pages.len())?;
    let mut out = io::BufWriter::with_capacity(1 << 16, io::stdout().lock());
    if e.json {
        text_export::write_json(&doc, pages, &list, &mut out)?;
    } else {
        let refs: Vec<_> = pages.iter().map(|p| p.r).collect();
        text_export::write_text(&doc, &refs, &list, &mut out)?;
    }
    out.flush()
}

fn write_images(e: &Export, dir: &Path, dpi: f32) -> io::Result<()> {
    let r = Renderer::new(open(e)?);
    let count = r.page_count();
    let list = page_list(e.pages, count)?;
    std::fs::create_dir_all(dir)?;
    let digits = count.to_string().len();
    // Pages render in parallel; paths are printed in page order as each chunk finishes.
    let threads = rayon::current_num_threads().max(1);
    let mut out = io::stdout().lock();
    for chunk in list.chunks(threads) {
        let paths: Vec<io::Result<PathBuf>> = chunk
            .par_iter()
            .map(|&page| {
                let path = dir.join(format!("page-{:0digits$}.png", page + 1));
                std::fs::write(&path, render_png(&r, page, dpi)?)?;
                Ok(path)
            })
            .collect();
        for p in paths {
            writeln!(out, "{}", p?.display())?;
        }
    }
    out.flush()
}

fn render_png(r: &Renderer, page: usize, dpi: f32) -> io::Result<Vec<u8>> {
    let (w, h) = r
        .page_size(page)
        .ok_or_else(|| io::Error::other(format!("no page {}", page + 1)))?;
    let scale = (dpi / 72.0).min(MAX_SIDE / w.max(h).max(1.0));
    let (pw, ph) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
    let req = RenderRequest::new(
        scale,
        PixelRect {
            x: 0,
            y: 0,
            w: pw,
            h: ph,
        },
    );
    let img = r
        .render(page, &req)
        .map_err(|e| io::Error::other(format!("page {}: {e:?}", page + 1)))?;
    // On white paper every pixel is opaque: RGB is a third smaller.
    let rgb: Vec<u8> = img
        .data
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| [p[0], p[1], p[2]])
        .collect();
    pagebara_png::encode(img.width, img.height, pagebara_png::ColorType::Rgb, &rgb)
        .map_err(io::Error::other)
}

fn page_list(spec: Option<&str>, count: usize) -> io::Result<Vec<usize>> {
    match spec {
        Some(s) => text_export::parse_pages(s, count).map_err(io::Error::other),
        None => Ok((0..count).collect()),
    }
}

fn open(e: &Export) -> io::Result<Document> {
    match Document::open(std::fs::read(e.file)?) {
        Ok(doc) => Ok(doc),
        Err(Error::PasswordRequired(locked)) => match e.password {
            Some(p) => locked
                .unlock(p)
                .map_err(|_| io::Error::other("wrong password")),
            None => Err(io::Error::other("password protected; pass --password")),
        },
        Err(err) => Err(io::Error::other(err.to_string())),
    }
}
