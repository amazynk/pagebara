//! Live text: text in pictures (images, scans, text drawn as outlines) recognised when the
//! pointer comes near it and then selectable and copyable like the page's own text. Nothing
//! is written to the document. Results are kept per page in memory (a few pages' worth),
//! across zooming and scrolling, until the document's pages change.
//!
//! [`LiveText`] is the state of one document, driven from the UI thread: it hands out
//! [`Job`]s to run on a worker and takes their [`Done`] back. One recognition runs at a
//! time; pointing at another picture cancels it. Within a picture the lines nearest the
//! pointer are read first and handed back as they are read, so text under the pointer is
//! selectable before the whole picture is.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock, Weak};

use pagebara_content::{Interpreter, Point, Rect, TextPage, copy_text, extract_text};
use pagebara_ocr::{Engine, LiveParams, ModelSource, Region, Script, page_with_live};
use pagebara_render::{DefaultGlyphs, Limits, Renderer};
use pagebara_syntax::{Document, ObjRef};
use parking_lot::{Mutex, RwLock};

use crate::pages;
use crate::text_index::TextIndex;

/// Memory kept for the recognised text of pages, per document.
const BUDGET: usize = 24 << 20;
/// A region whose area is covered this much by the page's own characters has its text.
const COVERED: f32 = 0.12;
/// Prefetching recognises regions covering at least this share of the page (scans).
const PREFETCH_SHARE: f32 = 0.5;

/// Where the models come from. Live text never downloads on its own; [`download`] fetches
/// another script's models when the user agrees.
#[derive(Clone, Debug, Default)]
pub struct Config {
    pub sources: Vec<ModelSource>,
    /// The writing system the user last chose for recognition, if any.
    pub script: Option<Script>,
    /// The system's language (`LANG`-style, e.g. `el_GR.UTF-8`).
    pub locale: Option<String>,
}

static CONFIG: RwLock<Option<Config>> = RwLock::new(None);

pub fn configure(config: Config) {
    *CONFIG.write() = Some(config);
}

/// Remembers the script the user chose (in the recognition dialog).
pub fn set_script(script: Script) {
    if let Some(c) = CONFIG.write().as_mut() {
        c.script = Some(script);
    }
}

fn config() -> Config {
    CONFIG.read().clone().unwrap_or_default()
}

/// Whether live text can run: the Latin models are at hand.
pub fn available() -> bool {
    let c = config();
    Script::Latin
        .files()
        .iter()
        .take(1)
        .chain(Script::Latin.recognition().iter())
        .all(|f| c.sources.iter().any(|s| s.has(f)))
}

fn script_available(sources: &[ModelSource], script: Script) -> bool {
    let [det, ..] = script.files();
    std::iter::once(det)
        .chain(script.recognition())
        .all(|f| sources.iter().any(|s| s.has(&f)))
}

/// The loaded engine, shared by all documents and kept for the process.
fn engine(script: Script) -> Result<Arc<Engine>, pagebara_ocr::Error> {
    static ENGINE: Mutex<Option<(Script, Arc<Engine>)>> = Mutex::new(None);
    let mut e = ENGINE.lock();
    if let Some((s, engine)) = e.as_ref()
        && *s == script
    {
        return Ok(engine.clone());
    }
    let sources = config().sources;
    let never = AtomicBool::new(false);
    // Only files already at hand: the orientation classifiers are optional.
    let engine = Arc::new(Engine::load(&sources, script, &|_| {}, &never)?);
    *e = Some((script, engine.clone()));
    Ok(engine)
}

/// Loads the engine for the document's script ahead of first use (on a worker).
pub fn warm_up(script: Script) {
    let _ = engine(script);
}

/// The writing system to recognise: the user's choice, the document's language, the
/// script of its text, the system's language; one whose models are at hand, else Latin.
pub fn choose_script(doc: &Document, sample: &str) -> Script {
    match wanted_script(doc, sample) {
        Some(s) if script_available(&config().sources, s) => s,
        _ => Script::Latin,
    }
}

/// A writing system the document seems to be in whose models are not at hand, so live
/// text reads it as Latin: one to offer to download.
pub fn script_to_offer(doc: &Document, sample: &str) -> Option<Script> {
    wanted_script(doc, sample)
        .filter(|&s| s != Script::Latin && !script_available(&config().sources, s))
}

/// Downloads the models for `script` (the user agreed) into `dir` through `fetch`, where
/// live text then finds them (`dir` should be one of the configured sources).
pub fn download(
    script: Script,
    dir: std::path::PathBuf,
    fetch: Arc<dyn pagebara_ocr::Fetch>,
    progress: &dyn Fn(pagebara_ocr::DownloadProgress<'_>),
    cancel: &AtomicBool,
) -> Result<(), pagebara_ocr::Error> {
    let mut sources = config().sources;
    sources.push(ModelSource::Download { dir, fetch });
    Engine::load(&sources, script, progress, cancel).map(drop)
}

/// Bytes [`download`] would fetch for `script`.
pub fn download_size(script: Script) -> u64 {
    crate::ocr::missing_bytes(&config().sources, script)
}

/// The script the user chose, the document's language, its text's or the system's.
fn wanted_script(doc: &Document, sample: &str) -> Option<Script> {
    let c = config();
    let lang = doc
        .catalog()
        .and_then(|cat| {
            let d = cat.as_dict()?.clone();
            let l = doc.get_in(&d, b"Lang");
            l.as_string().map(crate::text::decode)
        })
        .and_then(|l| script_of_language(&l));
    c.script
        .or(lang)
        .or_else(|| script_of_text(sample))
        .or_else(|| c.locale.as_deref().and_then(script_of_language))
}

/// The script of a BCP 47 or POSIX language tag (`ru`, `el-GR`, `zh_TW.UTF-8`).
pub fn script_of_language(tag: &str) -> Option<Script> {
    let lang = tag.split(['-', '_', '.', '@']).next()?.to_ascii_lowercase();
    Some(match lang.as_str() {
        "zh" | "ja" => Script::Chinese,
        "ko" => Script::Korean,
        "ar" | "fa" | "ur" | "ps" => Script::Arabic,
        "ru" | "uk" | "be" | "bg" | "sr" | "mk" | "kk" | "ky" | "mn" | "tg" => Script::Cyrillic,
        "hi" | "mr" | "ne" | "sa" => Script::Devanagari,
        "el" => Script::Greek,
        "th" => Script::Thai,
        "" | "c" | "posix" => return None,
        _ => Script::Latin,
    })
}

/// The script most of the letters of `text` are in, if there are enough.
pub fn script_of_text(text: &str) -> Option<Script> {
    let mut counts: HashMap<Script, usize> = HashMap::new();
    let mut letters = 0;
    for c in text.chars().filter(|c| c.is_alphabetic()).take(4000) {
        letters += 1;
        let s = match c as u32 {
            0x0370..=0x03FF | 0x1F00..=0x1FFF => Script::Greek,
            0x0400..=0x052F => Script::Cyrillic,
            0x0600..=0x06FF | 0x0750..=0x077F | 0xFB50..=0xFDFF | 0xFE70..=0xFEFF => Script::Arabic,
            0x0900..=0x097F => Script::Devanagari,
            0x0E00..=0x0E7F => Script::Thai,
            0x1100..=0x11FF | 0x3130..=0x318F | 0xAC00..=0xD7AF => Script::Korean,
            _ if pagebara_content::is_cjk(c) => Script::Chinese,
            _ => Script::Latin,
        };
        *counts.entry(s).or_default() += 1;
    }
    if letters < 20 {
        return None;
    }
    counts.into_iter().max_by_key(|e| e.1).map(|e| e.0)
}

/// What is known of a region of a page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Waiting,
    Running,
    Done,
    /// The page has text there already.
    Covered,
}

#[derive(Clone, Debug)]
struct Spot {
    region: Region,
    state: State,
}

#[derive(Default)]
struct LivePage {
    /// `None` until the page has been looked at.
    spots: Option<Vec<Spot>>,
    lines: Vec<pagebara_ocr::Line>,
    /// Lines read so far of the region being recognised.
    partial: Vec<pagebara_ocr::Line>,
    /// The page's text with what was recognised, once something was.
    merged: Option<Arc<TextPage>>,
    used: u64,
    bytes: usize,
}

/// Live text of one document revision.
pub struct LiveText {
    /// The revision's text index: identifies it (held weakly, so its address is not
    /// reused while this lives).
    text: Weak<TextIndex>,
    key: usize,
    doc: Document,
    refs: Vec<ObjRef>,
    renderer: OnceLock<Arc<Renderer>>,
    script: OnceLock<Script>,
    pages: HashMap<usize, LivePage>,
    clock: u64,
    /// The recognition running: page, region and its cancel flag.
    running: Option<(usize, usize, Arc<AtomicBool>)>,
    /// Where the pointer last asked for text: page and point.
    pointer: Option<(usize, f32, f32)>,
    /// Where on the page being recognised the pointer is, for the running job.
    focus: Arc<Mutex<Option<Point>>>,
    /// The engine was asked to load ahead of use.
    warmed: bool,
}

impl std::fmt::Debug for LiveText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveText")
            .field("pages", &self.pages.len())
            .field("running", &self.running.as_ref().map(|r| (r.0, r.1)))
            .finish_non_exhaustive()
    }
}

/// Work for a worker thread.
pub struct Job {
    key: usize,
    page: usize,
    kind: JobKind,
    doc: Document,
    refs: Vec<ObjRef>,
    renderer: Arc<Renderer>,
    script: Script,
    cancel: Arc<AtomicBool>,
    focus: Arc<Mutex<Option<Point>>>,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("page", &self.page)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

#[derive(Debug)]
enum JobKind {
    /// Find the page's regions.
    Look,
    /// Recognise one region, with the page's lines so far; `copy` when its text is wanted
    /// on the clipboard.
    Read {
        spot: usize,
        region: Region,
        lines: Vec<pagebara_ocr::Line>,
        copy: bool,
    },
    /// Recognise an area for copying.
    Area { rect: Rect, px_per_pt: Option<f32> },
}

/// A job's result, for [`LiveText::finish`].
#[derive(Debug)]
pub struct Done {
    key: usize,
    page: usize,
    /// The region recognised, for a recognition.
    spot: Option<usize>,
    result: Result<Outcome, pagebara_ocr::Error>,
}

#[derive(Debug)]
enum Outcome {
    Looked(Vec<Spot>),
    /// Lines of a region read so far (all of them), while the rest is read.
    Partial {
        spot: usize,
        lines: Vec<pagebara_ocr::Line>,
        merged: Arc<TextPage>,
    },
    Read {
        spot: usize,
        lines: Vec<pagebara_ocr::Line>,
        merged: Arc<TextPage>,
        copy: Option<String>,
    },
    Area(String),
}

/// What finishing a job changed.
#[derive(Debug, Default)]
pub struct Finished {
    /// A page's selectable text changed.
    pub page: Option<usize>,
    /// Only part of a region's text: more is coming.
    pub partial: bool,
    /// Text to put on the clipboard (a copy of an area or a picture).
    pub copy: Option<String>,
    /// Recognition failed (models missing or broken).
    pub error: Option<String>,
    /// Work to start next.
    pub next: Option<Job>,
    /// The engine for this script should be loaded now (see [`warm_up`]): a page has
    /// pictures to recognise.
    pub warm: Option<Script>,
}

impl Job {
    pub fn page(&self) -> usize {
        self.page
    }

    /// Does the work (on a worker thread).
    pub fn run(self) -> Done {
        self.run_with(|_| {})
    }

    /// [`Job::run`], handing `partial` the lines of a picture read so far as they come.
    pub fn run_with(self, mut partial: impl FnMut(Done)) -> Done {
        let result = self.work(&mut partial);
        let spot = match &self.kind {
            JobKind::Read { spot, .. } => Some(*spot),
            _ => None,
        };
        Done {
            key: self.key,
            page: self.page,
            spot,
            result,
        }
    }

    fn work(&self, partial: &mut dyn FnMut(Done)) -> Result<Outcome, pagebara_ocr::Error> {
        let params = LiveParams::default();
        match &self.kind {
            JobKind::Look => Ok(Outcome::Looked(look(&self.doc, &self.refs, self.page))),
            JobKind::Read {
                spot,
                region,
                lines,
                copy,
            } => {
                let engine = engine(self.script)?;
                let focus = || *self.focus.lock();
                let mut publish = |so_far: &[pagebara_ocr::Line]| {
                    let mut all = lines.clone();
                    all.extend(so_far.iter().cloned());
                    let merged = Arc::new(page_with_live(&self.doc, self.page, &all, None));
                    partial(Done {
                        key: self.key,
                        page: self.page,
                        spot: Some(*spot),
                        result: Ok(Outcome::Partial {
                            spot: *spot,
                            lines: so_far.to_vec(),
                            merged,
                        }),
                    });
                };
                let found = pagebara_ocr::recognize_region_live(
                    &engine,
                    &self.renderer,
                    self.page,
                    region,
                    &params,
                    &focus,
                    &mut publish,
                    &self.cancel,
                )?;
                let mut all = lines.clone();
                all.extend(found.iter().cloned());
                let merged = Arc::new(page_with_live(&self.doc, self.page, &all, None));
                let copy = copy.then(|| {
                    let t = page_with_live(&self.doc, self.page, &all, Some(region.rect));
                    copy_text(&t, 0..t.chars.len())
                });
                Ok(Outcome::Read {
                    spot: *spot,
                    lines: found,
                    merged,
                    copy,
                })
            }
            JobKind::Area { rect, px_per_pt } => {
                let engine = engine(self.script)?;
                let region = Region {
                    rect: *rect,
                    px_per_pt: *px_per_pt,
                };
                let found = pagebara_ocr::recognize_region(
                    &engine,
                    &self.renderer,
                    self.page,
                    &region,
                    &params,
                    &self.cancel,
                )?;
                let t = page_with_live(&self.doc, self.page, &found, Some(*rect));
                Ok(Outcome::Area(copy_text(&t, 0..t.chars.len())))
            }
        }
    }
}

/// The regions of a page and whether its own text covers them.
fn look(doc: &Document, refs: &[ObjRef], page: usize) -> Vec<Spot> {
    let Some(d) = refs
        .get(page)
        .and_then(|&r| doc.get(r))
        .and_then(|o| o.as_dict().cloned())
    else {
        return Vec::new();
    };
    let res = pages::resources(doc, &d);
    let list = Interpreter::new(doc).page(&d, &res);
    let tree = pagebara_pages::PageTree::open(doc);
    let Some(p) = tree.page(page) else {
        return Vec::new();
    };
    let c = p.crop_box;
    let crop = Rect::new(c.x0 as f32, c.y0 as f32, c.x1 as f32, c.y1 as f32);
    let regions = pagebara_ocr::regions(&list, crop);
    if regions.is_empty() {
        return Vec::new();
    }
    let text = extract_text(doc, &list);
    regions
        .into_iter()
        .map(|region| {
            let r = region.rect;
            let covered: f32 = text
                .chars
                .iter()
                .filter(|c| c.glyph != pagebara_content::display::NONE)
                .map(|c| c.bbox())
                .filter(|b| {
                    let (x, y) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
                    r.x0 <= x && x <= r.x1 && r.y0 <= y && y <= r.y1
                })
                .map(|b| b.width() * b.height())
                .sum();
            let area = (r.width() * r.height()).max(1.0);
            Spot {
                region,
                state: if covered / area >= COVERED {
                    State::Covered
                } else {
                    State::Waiting
                },
            }
        })
        .collect()
}

fn contains(r: &Rect, x: f32, y: f32) -> bool {
    r.x0 <= x && x <= r.x1 && r.y0 <= y && y <= r.y1
}

fn heap(lines: &[pagebara_ocr::Line], merged: Option<&TextPage>) -> usize {
    let words: usize = lines
        .iter()
        .flat_map(|l| &l.words)
        .map(|w| 96 + w.text.len() + w.cuts.len() * 16)
        .sum();
    words
        + merged.map_or(0, |t| {
            t.text.len()
                + t.chars.len() * std::mem::size_of::<pagebara_content::TextChar>()
                + t.words.len() * 24
                + t.lines.len() * 48
        })
}

impl LiveText {
    /// Live text for the document revision whose pages' text is `text`.
    pub fn new(text: &Arc<TextIndex>, doc: Document, refs: Vec<ObjRef>) -> Self {
        LiveText {
            text: Arc::downgrade(text),
            key: Arc::as_ptr(text) as usize,
            doc,
            refs,
            renderer: OnceLock::new(),
            script: OnceLock::new(),
            pages: HashMap::new(),
            clock: 0,
            running: None,
            pointer: None,
            focus: Arc::new(Mutex::new(None)),
            warmed: false,
        }
    }

    /// Whether this is the live text of the revision with the page text `text`.
    pub fn is_for(&self, text: &Arc<TextIndex>) -> bool {
        std::ptr::eq(self.text.as_ptr(), Arc::as_ptr(text))
    }

    /// The pages whose pictures were read, with their text (for searching).
    pub fn texts(&self) -> HashMap<usize, Arc<TextPage>> {
        self.pages
            .iter()
            .filter_map(|(&i, p)| Some((i, p.merged.clone()?)))
            .collect()
    }

    /// [`LiveText::text`] without counting it as a use.
    pub fn peek(&self, page: usize) -> Option<Arc<TextPage>> {
        self.pages.get(&page)?.merged.clone()
    }

    /// Forgets what was recognised and the script chosen (models for another script
    /// arrived), so pictures are read again.
    pub fn forget(&mut self) {
        self.cancel();
        self.pages.clear();
        self.script = OnceLock::new();
        self.warmed = false;
    }

    /// Stops the work in progress (the document is closing or changed).
    pub fn cancel(&mut self) {
        if let Some((_, _, c)) = self.running.take() {
            c.store(true, Ordering::Relaxed);
        }
    }

    /// The page's text with what was recognised in its pictures, if anything was.
    pub fn text(&mut self, page: usize) -> Option<Arc<TextPage>> {
        self.clock += 1;
        let now = self.clock;
        let p = self.pages.get_mut(&page)?;
        p.used = now;
        p.merged.clone()
    }

    /// Whether a recognition is running.
    pub fn busy(&self) -> bool {
        self.running.is_some()
    }

    /// Recognised words on page `page`.
    pub fn words(&self, page: usize) -> usize {
        self.pages.get(&page).map_or(0, |p| {
            p.lines
                .iter()
                .chain(&p.partial)
                .map(|l| l.words.len())
                .sum()
        })
    }

    /// The state of the region at a point, if there is one.
    pub fn state_at(&self, page: usize, x: f32, y: f32) -> Option<State> {
        let spots = self.pages.get(&page)?.spots.as_ref()?;
        spot_at(spots, x, y).map(|i| spots[i].state)
    }

    /// The rectangle of the picture at a point (pictures only, not drawings).
    pub fn picture_at(&self, page: usize, x: f32, y: f32) -> Option<Rect> {
        let spots = self.pages.get(&page)?.spots.as_ref()?;
        spots
            .iter()
            .filter(|s| s.region.px_per_pt.is_some() && contains(&s.region.rect, x, y))
            .min_by(|a, b| area(&a.region.rect).total_cmp(&area(&b.region.rect)))
            .map(|s| s.region.rect)
    }

    fn renderer(&self) -> Arc<Renderer> {
        self.renderer
            .get_or_init(|| {
                let limits = Limits {
                    display_lists: 16 << 20,
                    images: 96 << 20,
                    glyphs: 16 << 20,
                    misc: 16 << 20,
                };
                Arc::new(Renderer::with_options(
                    self.doc.clone(),
                    limits,
                    Arc::new(DefaultGlyphs),
                ))
            })
            .clone()
    }

    /// The script to read this document in (worked out once, from page `sample`'s text).
    pub fn script(&self, sample: impl FnOnce() -> String) -> Script {
        *self
            .script
            .get_or_init(|| choose_script(&self.doc, &sample()))
    }

    fn job(&mut self, page: usize, kind: JobKind) -> Job {
        let cancel = Arc::new(AtomicBool::new(false));
        let focus = Arc::new(Mutex::new(None));
        if let JobKind::Read { spot, region, .. } = &kind {
            self.running = Some((page, *spot, cancel.clone()));
            *focus.lock() = self
                .pointer
                .filter(|p| p.0 == page && contains(&region.rect, p.1, p.2))
                .map(|p| Point::new(p.1, p.2));
            self.focus = focus.clone();
        }
        Job {
            key: self.key,
            page,
            kind,
            doc: self.doc.clone(),
            refs: self.refs.clone(),
            renderer: self.renderer(),
            script: self.script.get().copied().unwrap_or_default(),
            cancel,
            focus,
        }
    }

    /// The pointer is at `(x, y)` (user space) on `page`, not over text the page has: the
    /// work that would make text there selectable, if any is needed.
    pub fn point(&mut self, page: usize, x: f32, y: f32) -> Option<Job> {
        self.pointer = Some((page, x, y));
        if self.running.as_ref().is_some_and(|r| r.0 == page) {
            // The picture being read: the lines here next.
            *self.focus.lock() = Some(Point::new(x, y));
        }
        self.touch(page);
        let p = self.pages.entry(page).or_default();
        let Some(spots) = p.spots.as_ref() else {
            // Looked at once; a missing entry means not yet.
            p.spots = Some(Vec::new());
            return Some(self.job(page, JobKind::Look));
        };
        let i = spot_at(spots, x, y)?;
        match spots[i].state {
            State::Waiting => self.read(page, i, false),
            State::Running | State::Done | State::Covered => None,
        }
    }

    /// Recognises the picture at a point and copies its text (from a context menu): the
    /// text now when it is known, else a job that finishes with it.
    pub fn copy_picture(&mut self, page: usize, x: f32, y: f32) -> Result<Job, Option<String>> {
        let p = self.pages.get(&page).ok_or(None)?;
        let spots = p.spots.as_ref().ok_or(None)?;
        let i = spot_at(spots, x, y).ok_or(None)?;
        match spots[i].state {
            State::Done | State::Covered => {
                let rect = spots[i].region.rect;
                let t = page_with_live(&self.doc, page, &p.lines, Some(rect));
                Err(Some(copy_text(&t, 0..t.chars.len())))
            }
            State::Waiting | State::Running => self.read(page, i, true).ok_or(None),
        }
    }

    /// Recognises everything in `rect` (user space) of `page` for copying; the page's own
    /// text is used where it has some.
    pub fn copy_area(&mut self, page: usize, rect: Rect) -> Job {
        // The sharpest picture in the area sets the resolution.
        let px_per_pt = self
            .pages
            .get(&page)
            .and_then(|p| p.spots.as_ref())
            .and_then(|spots| {
                spots
                    .iter()
                    .filter(|s| {
                        let r = s.region.rect;
                        r.x0 < rect.x1 && rect.x0 < r.x1 && r.y0 < rect.y1 && rect.y0 < r.y1
                    })
                    .filter_map(|s| s.region.px_per_pt)
                    .reduce(f32::max)
            });
        self.cancel();
        self.job(page, JobKind::Area { rect, px_per_pt })
    }

    /// Work for when the view is idle: the next large picture (a scan) of the pages on
    /// screen not yet recognised, or a page not yet looked at.
    pub fn prefetch(&mut self, visible: &[usize]) -> Option<Job> {
        if self.running.is_some() {
            return None;
        }
        for &page in visible {
            let Some(p) = self.pages.get(&page) else {
                self.pages.entry(page).or_default().spots = Some(Vec::new());
                return Some(self.job(page, JobKind::Look));
            };
            let Some(spots) = &p.spots else { continue };
            let Some(size) = self.page_area(page) else {
                continue;
            };
            let next = spots.iter().position(|s| {
                s.state == State::Waiting && area(&s.region.rect) >= PREFETCH_SHARE * size
            });
            if let Some(i) = next {
                return self.read(page, i, false);
            }
        }
        None
    }

    fn page_area(&self, page: usize) -> Option<f32> {
        let tree = pagebara_pages::PageTree::open(&self.doc);
        let p = tree.page(page)?;
        Some((p.crop_box.width() * p.crop_box.height()) as f32)
    }

    fn read(&mut self, page: usize, spot: usize, copy: bool) -> Option<Job> {
        // A recognition of another picture is stale now.
        if let Some((rp, rs, c)) = &self.running {
            if (*rp, *rs) == (page, spot) && !copy {
                return None;
            }
            c.store(true, Ordering::Relaxed);
            let (rp, rs) = (*rp, *rs);
            if let Some(p) = self.pages.get_mut(&rp) {
                p.partial.clear();
            }
            if let Some(s) = self
                .pages
                .get_mut(&rp)
                .and_then(|p| p.spots.as_mut())
                .and_then(|s| s.get_mut(rs))
                && s.state == State::Running
            {
                s.state = State::Waiting;
            }
            self.running = None;
        }
        let p = self.pages.get_mut(&page)?;
        let s = p.spots.as_mut()?.get_mut(spot)?;
        s.state = State::Running;
        let region = s.region;
        let lines = p.lines.clone();
        Some(self.job(
            page,
            JobKind::Read {
                spot,
                region,
                lines,
                copy,
            },
        ))
    }

    fn touch(&mut self, page: usize) {
        self.clock += 1;
        let now = self.clock;
        if let Some(p) = self.pages.get_mut(&page) {
            p.used = now;
        }
    }

    /// Takes a job's result back (on the UI thread).
    pub fn finish(&mut self, done: Done) -> Finished {
        let mut out = Finished::default();
        if done.key != self.key {
            return out;
        }
        let page = done.page;
        match done.result {
            Ok(Outcome::Looked(spots)) => {
                if !self.warmed && spots.iter().any(|s| s.state == State::Waiting) {
                    self.warmed = true;
                    out.warm = Some(self.script.get().copied().unwrap_or_default());
                }
                self.pages.entry(page).or_default().spots = Some(spots);
                // The pointer may be waiting over one of them.
                if let Some((pp, x, y)) = self.pointer
                    && pp == page
                {
                    out.next = self.point(page, x, y);
                }
            }
            Ok(Outcome::Partial {
                spot,
                lines,
                merged,
            }) => {
                // Only while that region is still being read.
                if self
                    .running
                    .as_ref()
                    .is_some_and(|r| (r.0, r.1) == (page, spot))
                {
                    let p = self.pages.entry(page).or_default();
                    p.partial = lines;
                    p.merged = Some(merged);
                    out.page = Some(page);
                    out.partial = true;
                }
            }
            Ok(Outcome::Read {
                spot,
                lines,
                merged,
                copy,
            }) => {
                if self
                    .running
                    .as_ref()
                    .is_some_and(|r| (r.0, r.1) == (page, spot))
                {
                    self.running = None;
                }
                let p = self.pages.entry(page).or_default();
                if let Some(s) = p.spots.as_mut().and_then(|s| s.get_mut(spot)) {
                    s.state = State::Done;
                }
                p.partial.clear();
                p.lines.extend(lines);
                p.merged = Some(merged);
                p.bytes = heap(&p.lines, p.merged.as_deref());
                out.page = Some(page);
                out.copy = copy;
                self.touch(page);
                self.trim(page);
            }
            Ok(Outcome::Area(text)) => out.copy = Some(text),
            // Whoever cancelled the work has put its region back to waiting.
            Err(pagebara_ocr::Error::Cancelled) => {}
            Err(e) => {
                if let Some(spot) = done.spot
                    && self
                        .running
                        .as_ref()
                        .is_some_and(|r| (r.0, r.1) == (page, spot))
                {
                    self.running = None;
                    // Failed work is not asked for again.
                    if let Some(s) = self
                        .pages
                        .get_mut(&page)
                        .and_then(|p| p.spots.as_mut())
                        .and_then(|s| s.get_mut(spot))
                    {
                        s.state = State::Done;
                    }
                }
                out.error = Some(e.to_string());
            }
        }
        out
    }

    /// Keeps the recognised text within [`BUDGET`], dropping the least recently used
    /// pages other than `keep`.
    fn trim(&mut self, keep: usize) {
        loop {
            let total: usize = self.pages.values().map(|p| p.bytes).sum();
            if total <= BUDGET {
                return;
            }
            let Some(old) = self
                .pages
                .iter()
                .filter(|(k, p)| **k != keep && p.bytes > 0)
                .min_by_key(|(_, p)| p.used)
                .map(|(k, _)| *k)
            else {
                return;
            };
            self.pages.remove(&old);
        }
    }
}

fn area(r: &Rect) -> f32 {
    r.width() * r.height()
}

/// The smallest region at a point.
fn spot_at(spots: &[Spot], x: f32, y: f32) -> Option<usize> {
    spots
        .iter()
        .enumerate()
        .filter(|(_, s)| contains(&s.region.rect, x, y))
        .min_by(|a, b| area(&a.1.region.rect).total_cmp(&area(&b.1.region.rect)))
        .map(|(i, _)| i)
}

/// A selection `anchor..caret` of `old` in `new`, for a selection made before the page's
/// text gained recognised words: characters keep their boxes. Its start goes by its first
/// character, its end by its last (what follows may be other words now).
pub fn remap(old: &TextPage, new: &TextPage, anchor: usize, caret: usize) -> (usize, usize) {
    let find = |k: usize| {
        let c = old.chars.get(k)?;
        let text = old.char_text(c);
        new.chars
            .iter()
            .position(|n| n.quad == c.quad && new.char_text(n) == text)
    };
    let (lo, hi) = (anchor.min(caret), anchor.max(caret));
    let start = find(lo).unwrap_or(0);
    let end = match hi {
        0 => 0,
        _ if hi == lo => start,
        _ => find(hi - 1).map_or(start, |j| j + 1),
    };
    if anchor <= caret {
        (start, end)
    } else {
        (end, start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scripts_from_languages_and_text() {
        assert_eq!(script_of_language("el-GR"), Some(Script::Greek));
        assert_eq!(script_of_language("ru_RU.UTF-8"), Some(Script::Cyrillic));
        assert_eq!(script_of_language("zh-Hant"), Some(Script::Chinese));
        assert_eq!(script_of_language("de"), Some(Script::Latin));
        assert_eq!(script_of_language("C"), None);
        assert_eq!(
            script_of_text("Η γρήγορη καφέ αλεπού πηδάει πάνω από τον τεμπέλη σκύλο"),
            Some(Script::Greek)
        );
        assert_eq!(script_of_text("short"), None);
    }

    fn line(text: &str, y: f32) -> pagebara_ocr::Line {
        let p = pagebara_content::Point::new;
        pagebara_ocr::Line {
            words: vec![pagebara_ocr::Word {
                text: text.into(),
                quad: [p(10.0, y), p(60.0, y), p(60.0, y + 12.0), p(10.0, y + 12.0)],
                confidence: 1.0,
                cuts: Vec::new(),
            }],
        }
    }

    #[test]
    fn lines_read_so_far_are_selectable_and_follow_the_pointer() {
        let doc = Document::open(crate::test_pdf::text(&["Hello"])).unwrap();
        let refs = pagebara_pages::PageTree::load(&doc).refs();
        let index = Arc::new(TextIndex::new(doc.clone(), refs.clone()));
        let mut live = LiveText::new(&index, doc.clone(), refs);
        let region = Region {
            rect: Rect::new(0.0, 0.0, 600.0, 600.0),
            px_per_pt: Some(4.0),
        };
        live.pages.entry(0).or_default().spots = Some(vec![Spot {
            region,
            state: State::Waiting,
        }]);
        let job = live.point(0, 300.0, 400.0).expect("a recognition");
        assert_eq!(*job.focus.lock(), Some(Point::new(300.0, 400.0)));
        let merged = |lines: &[pagebara_ocr::Line]| Arc::new(page_with_live(&doc, 0, lines, None));
        let first = vec![line("first", 500.0)];
        let f = live.finish(Done {
            key: live.key,
            page: 0,
            spot: Some(0),
            result: Ok(Outcome::Partial {
                spot: 0,
                lines: first.clone(),
                merged: merged(&first),
            }),
        });
        assert!(f.partial && f.page == Some(0));
        assert_eq!(live.words(0), 1);
        assert!(live.text(0).unwrap().text.contains("first"));
        assert_eq!(live.state_at(0, 300.0, 400.0), Some(State::Running));
        // The pointer moves on within the picture: no new work, the lines there next.
        assert!(live.point(0, 100.0, 100.0).is_none());
        assert_eq!(*job.focus.lock(), Some(Point::new(100.0, 100.0)));
        let all = vec![line("first", 500.0), line("second", 300.0)];
        let f = live.finish(Done {
            key: live.key,
            page: 0,
            spot: Some(0),
            result: Ok(Outcome::Read {
                spot: 0,
                lines: all.clone(),
                merged: merged(&all),
                copy: None,
            }),
        });
        assert!(!f.partial && f.page == Some(0));
        assert_eq!(live.words(0), 2);
        assert_eq!(live.state_at(0, 300.0, 400.0), Some(State::Done));
        assert!(!live.busy());
        // A partial result of finished work changes nothing.
        let f = live.finish(Done {
            key: live.key,
            page: 0,
            spot: Some(0),
            result: Ok(Outcome::Partial {
                spot: 0,
                lines: first.clone(),
                merged: merged(&first),
            }),
        });
        assert!(f.page.is_none());
        assert_eq!(live.words(0), 2);
    }

    #[test]
    fn remap_follows_characters() {
        let doc = Document::open(crate::test_pdf::text(&["Hello world"])).unwrap();
        let tree = pagebara_pages::PageTree::load(&doc);
        let refs = tree.refs();
        let old = crate::text_index::TextIndex::new(doc.clone(), refs.clone()).page(0);
        // The same page laid out again with a recognised word before it.
        let lines = vec![pagebara_ocr::Line {
            words: vec![pagebara_ocr::Word {
                text: "Before".into(),
                quad: [
                    pagebara_content::Point::new(10.0, 780.0),
                    pagebara_content::Point::new(60.0, 780.0),
                    pagebara_content::Point::new(60.0, 792.0),
                    pagebara_content::Point::new(10.0, 792.0),
                ],
                confidence: 1.0,
                cuts: Vec::new(),
            }],
        }];
        let new = page_with_live(&doc, 0, &lines, None);
        assert!(new.text.starts_with("Before"), "{}", new.text);
        let w = old.text.find("world").unwrap();
        let i = old
            .chars
            .iter()
            .position(|c| c.text.0 as usize == w)
            .unwrap();
        // "world" selected, either way round.
        let (a, c) = remap(&old, &new, i, i + 5);
        assert_eq!(new.char_text(&new.chars[a]), "w");
        assert_eq!(new.char_text(&new.chars[c - 1]), "d");
        assert_eq!(remap(&old, &new, i + 5, i), (c, a));
        // Up to the end of the page.
        let n = old.chars.len();
        assert_eq!(remap(&old, &new, i, n).1, new.chars.len());
    }
}
