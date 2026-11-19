//! OCR for scanned pages: recognises the text of pages that have none and adds it as an
//! invisible text layer, so the pages become searchable, selectable and extractable.
//!
//! Pages scanned sideways or upside down are turned first (PP-LCNet orientation
//! classifiers), so their text layer lines up with the page as shown.
//!
//! Recognition uses the PP-OCRv5 mobile models (Apache-2.0, from PaddleOCR) run by the
//! pure-Rust [RTen](https://github.com/robertknight/rten) runtime. The weights are not in
//! this crate: see [`model`] for where they come from.
//!
//! ```no_run,standalone_crate
//! use std::sync::atomic::AtomicBool;
//! use pagebara_ocr::{Engine, ModelSource, Options, Script, ocr_document};
//!
//! let cancel = AtomicBool::new(false);
//! let sources = [ModelSource::Dir("models".into())];
//! let engine = Engine::load(&sources, Script::Latin, &|_| {}, &cancel).unwrap();
//! let mut doc = pagebara_syntax::Document::open(std::fs::read("scan.pdf").unwrap()).unwrap();
//! let summary = ocr_document(&mut doc, &engine, &Options::default(), &|_| {}, &cancel).unwrap();
//! println!("{} pages recognised", summary.recognized.len());
//! std::fs::write("out.pdf", doc.save_incremental().unwrap()).unwrap();
//! ```

mod detect;
mod engine;
mod image;
mod layer;
pub mod live;
pub mod model;
mod orient;
mod page;
mod recognize;
mod underscore;

#[cfg(feature = "download")]
mod download;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use pagebara_render::{DefaultGlyphs, Limits, Renderer};
use pagebara_syntax::Document;
use rayon::prelude::*;

pub use detect::DbParams;
#[cfg(feature = "download")]
pub use download::HttpFetch;
pub use engine::{Engine, EngineParams, ImageLine, ImageWord, LineLayout};
pub use image::{RgbImage, RotRect};
pub use layer::{add_text_layer, layer_font};
pub use live::{
    LiveParams, Region, page_with_live, recognize_region, recognize_region_live, regions,
};
pub use model::{DownloadProgress, Fetch, ModelFile, ModelSource, Script};
pub use page::{
    Line, PageText, Word, page_text, recognize_page, render_page, text_chars, to_page_space,
};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("model file {0} is not available")]
    ModelUnavailable(&'static str),
    #[error("downloading {0}: {1}")]
    Download(&'static str, String),
    #[error("{0}: {1}")]
    Io(&'static str, String),
    #[error("unusable model: {0}")]
    BadModel(String),
    #[error("inference failed: {0}")]
    Inference(String),
    #[error("no page {0}")]
    NoSuchPage(usize),
    #[error("rendering failed: {0}")]
    Render(String),
    #[error("the text layer font could not be made")]
    Font,
    #[error("too many distinct characters for one font")]
    TooManyGlyphs,
    #[error("cancelled")]
    Cancelled,
}

/// What [`ocr_document`] does.
#[derive(Clone, Debug)]
pub struct Options {
    /// Rendering resolution for recognition.
    pub dpi: f32,
    /// Recognise pages even when they already have text.
    pub force: bool,
    /// Pages with at least this many non-space characters of text count as having text.
    pub min_text_chars: usize,
    /// Pages processed at once (each holds a rendered image; inference itself is
    /// multi-threaded).
    pub parallel_pages: usize,
    /// Pages to process (indices from 0); `None` for all.
    pub pages: Option<Vec<usize>>,
    pub engine: EngineParams,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            dpi: 300.0,
            force: false,
            min_text_chars: 16,
            parallel_pages: 2,
            pages: None,
            engine: EngineParams::default(),
        }
    }
}

/// Progress of [`recognize_pages`]: `done` of `total` pages finished, the last being `page`.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub done: usize,
    pub total: usize,
    pub page: usize,
}

/// What [`ocr_document`] did.
#[derive(Clone, Debug, Default)]
pub struct Summary {
    /// Pages that got a text layer.
    pub recognized: Vec<usize>,
    /// Pages skipped because they already have text.
    pub skipped: Vec<usize>,
    /// Pages where nothing was recognised.
    pub empty: Vec<usize>,
    pub words: usize,
}

/// Recognises `pages` of a document (those with text are skipped unless `opts.force`),
/// `opts.parallel_pages` at a time. `None` entries are skipped pages.
pub fn recognize_pages(
    engine: &Engine,
    renderer: &Renderer,
    pages: &[usize],
    opts: &Options,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &AtomicBool,
) -> Result<Vec<Option<PageText>>, Error> {
    let done = AtomicUsize::new(0);
    let total = pages.len();
    let doc = renderer.document();
    let run = |&page: &usize| -> Result<Option<PageText>, Error> {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let result = if !opts.force && text_chars(doc, page) >= opts.min_text_chars {
            None
        } else {
            Some(recognize_page(
                engine,
                renderer,
                page,
                opts.dpi,
                &opts.engine,
                cancel,
            )?)
        };
        let done = done.fetch_add(1, Ordering::Relaxed) + 1;
        progress(Progress { done, total, page });
        Ok(result)
    };
    let threads = opts.parallel_pages.max(1);
    if threads == 1 {
        return pages.iter().map(run).collect();
    }
    let pool = pagebara_thread::rayon_pool("ocr")
        .num_threads(threads)
        .build()
        .map_err(|e| Error::Inference(e.to_string()))?;
    let first_error = Mutex::new(None);
    let out: Vec<Option<PageText>> = pool.install(|| {
        pages
            .par_iter()
            .with_max_len(1)
            .map(|p| match run(p) {
                Ok(t) => t,
                Err(e) => {
                    cancel_on_error(&first_error, e);
                    None
                }
            })
            .collect()
    });
    match first_error.into_inner().unwrap_or_else(|e| e.into_inner()) {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

fn cancel_on_error(slot: &Mutex<Option<Error>>, e: Error) {
    let mut s = slot.lock().unwrap_or_else(|e| e.into_inner());
    if s.is_none() || matches!(*s, Some(Error::Cancelled)) {
        *s = Some(e);
    }
}

/// Recognises the pages of `doc` that need it and adds their text layers. Save with
/// [`Document::save_incremental`] afterwards.
pub fn ocr_document(
    doc: &mut Document,
    engine: &Engine,
    opts: &Options,
    progress: &(dyn Fn(Progress) + Sync),
    cancel: &AtomicBool,
) -> Result<Summary, Error> {
    // Each page is rendered once: small caches keep memory to the pages in flight.
    let limits = Limits {
        display_lists: 16 << 20,
        images: 64 << 20,
        glyphs: 16 << 20,
        misc: 32 << 20,
    };
    let renderer = Renderer::with_options(doc.clone(), limits, Arc::new(DefaultGlyphs));
    let pages: Vec<usize> = match &opts.pages {
        Some(p) => p.clone(),
        None => (0..renderer.page_count()).collect(),
    };
    let results = recognize_pages(engine, &renderer, &pages, opts, progress, cancel)?;
    drop(renderer);
    let mut summary = Summary::default();
    let mut texts = Vec::new();
    for (&page, r) in pages.iter().zip(results) {
        match r {
            None => summary.skipped.push(page),
            Some(t) if t.word_count() == 0 => summary.empty.push(page),
            Some(t) => {
                summary.recognized.push(page);
                summary.words += t.word_count();
                texts.push(t);
            }
        }
    }
    add_text_layer(doc, &texts)?;
    Ok(summary)
}
