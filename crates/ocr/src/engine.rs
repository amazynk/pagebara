//! The OCR engine: PP-OCRv5 detection and recognition models run with RTen.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rten::{Model, RunOptions, ThreadPool};
use rten_tensor::NdTensor;
use rten_tensor::prelude::*;

use crate::Error;
use crate::detect::{DbParams, detections};
use crate::image::{RgbImage, RotRect, warp};
use crate::model::{DownloadProgress, ModelSource, Script, read_file};
use crate::orient;
use crate::recognize::{Charset, DecodedWord, decode};

/// The page classifier's probability above which its answer stands without the lines.
const PAGE_SURE: f32 = 0.9;
/// Tall lines read both ways to tell which way they run.
const SAMPLE_COLUMNS: usize = 24;

/// Recognition input height in pixels.
const REC_HEIGHT: usize = 48;
/// Widest recognition input (the model's tested range).
const REC_MAX_WIDTH: usize = 3200;
/// Pixels per recognition batch (bounds memory; crops are batched by similar width).
const REC_BATCH_PIXELS: usize = REC_HEIGHT * 8 * 1024;

/// How images are analysed.
#[derive(Clone, Debug)]
pub struct EngineParams {
    /// Images are scaled down for detection so their longer side is at most this.
    pub detection_max_side: usize,
    pub db: DbParams,
    /// Lines recognised with a lower mean confidence are dropped.
    pub min_confidence: f32,
    /// Turn pages whose text does not read upright (scanned sideways or upside down).
    pub orientation: bool,
}

impl Default for EngineParams {
    fn default() -> Self {
        EngineParams {
            detection_max_side: 2400,
            db: DbParams::default(),
            min_confidence: 0.5,
            orientation: true,
        }
    }
}

/// A recognised word in image pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageWord {
    pub text: String,
    /// `u` runs along the baseline, `v` from the top of the line down.
    pub rect: RotRect,
    pub confidence: f32,
    /// Where each character starts: byte offset in `text`, fraction along `rect`.
    pub cuts: Vec<(usize, f32)>,
}

/// A line of words, in reading order.
#[derive(Clone, Debug, PartialEq)]
pub struct ImageLine {
    pub rect: RotRect,
    pub words: Vec<ImageWord>,
}

/// Loaded detection and recognition models; shareable across threads.
pub struct Engine {
    det: Model,
    rec: Model,
    /// The orientation classifiers, when their models are available.
    page_ori: Option<Model>,
    line_ori: Option<Model>,
    charset: Charset,
    pool: Option<Arc<ThreadPool>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("classes", &self.charset.classes())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Loads (downloading if a source allows and needs it) the models for `script`.
    pub fn load(
        sources: &[ModelSource],
        script: Script,
        progress: &dyn Fn(DownloadProgress<'_>),
        cancel: &AtomicBool,
    ) -> Result<Engine, Error> {
        let [det, page, line, rec, dict] = script.files();
        let det = read_file(sources, &det, progress, cancel)?;
        // Without the classifiers pages are read as they are shown.
        let optional = |f| match read_file(sources, f, progress, cancel) {
            Err(Error::ModelUnavailable(_)) => Ok(None),
            r => r.map(Some),
        };
        let page = optional(&page)?;
        let line = optional(&line)?;
        let rec = read_file(sources, &rec, progress, cancel)?;
        let dict = read_file(sources, &dict, progress, cancel)?;
        let dict = String::from_utf8(dict).map_err(|_| Error::BadModel("dictionary".into()))?;
        let e = Engine::from_bytes(det, rec, &dict)?;
        e.with_orientation(page, line)
    }

    /// Adds the page and text line orientation classifiers (ONNX bytes).
    pub fn with_orientation(
        mut self,
        page: Option<Vec<u8>>,
        line: Option<Vec<u8>>,
    ) -> Result<Engine, Error> {
        let load = |b: Option<Vec<u8>>| {
            b.map(|b| Model::load(b).map_err(|e| Error::BadModel(e.to_string())))
                .transpose()
        };
        self.page_ori = load(page)?;
        self.line_ori = load(line)?;
        Ok(self)
    }

    /// From ONNX model bytes and the recognition model's `inference.yml`.
    pub fn from_bytes(det: Vec<u8>, rec: Vec<u8>, rec_yml: &str) -> Result<Engine, Error> {
        let charset = Charset::from_yaml(rec_yml)
            .ok_or_else(|| Error::BadModel("no character list".into()))?;
        let det = Model::load(det).map_err(|e| Error::BadModel(e.to_string()))?;
        let rec = Model::load(rec).map_err(|e| Error::BadModel(e.to_string()))?;
        Ok(Engine {
            det,
            rec,
            page_ori: None,
            line_ori: None,
            charset,
            pool: None,
        })
    }

    /// Runs inference on a pool of `threads` threads instead of RTen's global one (sized to
    /// the physical cores).
    pub fn with_threads(mut self, threads: usize) -> Engine {
        self.pool = Some(Arc::new(ThreadPool::with_num_threads(threads.max(1))));
        self
    }

    fn opts(&self) -> Option<RunOptions> {
        self.pool
            .as_ref()
            .map(|p| RunOptions::default().with_thread_pool(Some(p.clone())))
    }

    /// Finds and reads the text lines of an image, turned upright first when its text is
    /// sideways or upside down. Boxes are in `img`'s pixels.
    pub fn recognize(
        &self,
        img: &RgbImage,
        params: &EngineParams,
        cancel: &AtomicBool,
    ) -> Result<Vec<ImageLine>, Error> {
        let boxes = self.detect(img, params)?;
        self.recognize_detected(img, boxes, params, cancel)
    }

    /// [`Engine::recognize`] with the text line boxes [`Engine::detect`] found in `img`.
    pub fn recognize_detected(
        &self,
        img: &RgbImage,
        boxes: Vec<RotRect>,
        params: &EngineParams,
        cancel: &AtomicBool,
    ) -> Result<Vec<ImageLine>, Error> {
        let layout = self.layout(img, boxes, params, cancel)?;
        let all: Vec<usize> = (0..layout.len()).collect();
        let lines = self.read_layout(&layout, &all, params, cancel)?;
        Ok(lines.into_iter().flatten().collect())
    }

    /// The text lines of `img` (boxes [`Engine::detect`] found) laid out for reading:
    /// turned upright and in reading order, to be read in any order with
    /// [`Engine::read_layout`].
    pub fn layout<'a>(
        &self,
        img: &'a RgbImage,
        boxes: Vec<RotRect>,
        params: &EngineParams,
        cancel: &AtomicBool,
    ) -> Result<LineLayout<'a>, Error> {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let layout = self.orient(img, boxes, params, cancel)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let boxes = if layout.columns {
            column_reading_order(layout.boxes)
        } else {
            let mut b = layout.boxes;
            sort_reading_order(&mut b);
            b
        };
        Ok(LineLayout {
            image: img,
            upright: layout.upright,
            turn: layout.turn,
            boxes,
        })
    }

    /// Reads lines `which` (indices in reading order) of a layout; `None` for lines too
    /// unsure to keep. Boxes are in the pixels of the image as given.
    pub fn read_layout(
        &self,
        layout: &LineLayout<'_>,
        which: &[usize],
        params: &EngineParams,
        cancel: &AtomicBool,
    ) -> Result<Vec<Option<ImageLine>>, Error> {
        let img = layout.upright();
        let boxes: Vec<RotRect> = which.iter().map(|&i| layout.boxes[i]).collect();
        let texts = self.read_lines(img, &boxes, cancel)?;
        let mut lines = Vec::with_capacity(boxes.len());
        for (rect, words) in boxes.into_iter().zip(texts) {
            let words: Vec<ImageWord> = words
                .into_iter()
                .map(|w| {
                    let span = (w.end - w.start).max(1e-6);
                    ImageWord {
                        rect: rect.slice(w.start, w.end),
                        cuts: w
                            .cuts
                            .iter()
                            .map(|&(b, f)| (b, ((f - w.start) / span).clamp(0.0, 1.0)))
                            .collect(),
                        text: w.text,
                        confidence: w.confidence,
                    }
                })
                .collect();
            let chars: usize = words.iter().map(|w| w.text.chars().count()).sum();
            let conf = words
                .iter()
                .map(|w| w.confidence * w.text.chars().count() as f32)
                .sum::<f32>()
                / chars.max(1) as f32;
            let keep = chars > 0 && conf >= params.min_confidence;
            lines.push(keep.then(|| {
                let mut l = ImageLine { rect, words };
                layout.unturn(&mut l);
                l
            }));
        }
        Ok(lines)
    }

    /// How the text of `img` lies: the quarter turns clockwise that make it read upright
    /// (with the turned image when not 0) and its text lines there, or upright vertical
    /// writing.
    fn orient(
        &self,
        img: &RgbImage,
        boxes: Vec<RotRect>,
        params: &EngineParams,
        cancel: &AtomicBool,
    ) -> Result<TextLayout, Error> {
        let as_shown = |boxes, columns| TextLayout {
            turn: 0,
            upright: None,
            boxes,
            columns,
        };
        if !params.orientation || boxes.is_empty() {
            return Ok(as_shown(boxes, false));
        }
        let vertical = orient::mostly_vertical(&boxes);
        // The page classifier's answer, when it is sure. Class k: the content is turned k
        // quarters clockwise.
        let sure = match &self.page_ori {
            Some(m) => {
                let p = orient::page_classes(m, img, |m, t| self.classify(m, t))?;
                p.iter()
                    .enumerate()
                    .max_by(|a, b| a.1.total_cmp(b.1))
                    .filter(|(_, p)| **p >= PAGE_SURE)
                    .map(|(k, _)| k as u8)
            }
            None => None,
        };
        let turn = match sure {
            // Turned a quarter, and the lines agree: it needs as many turns back.
            Some(k) if (k % 2 == 1) == vertical => (4 - k) % 4,
            // Upright with tall lines: vertical writing, unless its characters lie on
            // their sides.
            Some(0) if vertical => match self.character_turn(img, &boxes, true, cancel)? {
                0 => return Ok(as_shown(boxes, true)),
                t => t,
            },
            _ if vertical => match self.column_reading(img, &boxes, cancel)? {
                // CJK read downwards: vertical writing when its characters stand upright,
                // else horizontal lines turned a quarter clockwise.
                Columns::Down { cjk: true } => {
                    match self.character_turn(img, &boxes, false, cancel)? {
                        0 => return Ok(as_shown(boxes, true)),
                        t => t,
                    }
                }
                Columns::Down { cjk: false } => 3,
                Columns::Up => 1,
            },
            // Horizontal lines: the line classifier says whether they are upside down.
            _ => {
                let flip = match &self.line_ori {
                    Some(m) => orient::upside_down(m, img, &boxes, |m, t| self.classify(m, t))?,
                    None => None,
                };
                if !flip.is_some_and(|p| p > 0.5) {
                    return Ok(as_shown(boxes, false));
                }
                let (w, h) = (img.width as f32, img.height as f32);
                return Ok(TextLayout {
                    turn: 2,
                    upright: Some(img.turned(2)),
                    boxes: boxes.iter().map(|r| r.reversed_in(w, h)).collect(),
                    columns: false,
                });
            }
        };
        if turn == 0 {
            return Ok(as_shown(boxes, false));
        }
        let t = img.turned(turn);
        let boxes = self.detect(&t, params)?;
        Ok(TextLayout {
            turn,
            upright: Some(t),
            boxes,
            columns: false,
        })
    }

    /// Which way the tall lines of `boxes` read best: down (vertical writing, or a page
    /// turned a quarter clockwise) or up (a page turned a quarter counter-clockwise). The
    /// recogniser reads the longest lines both ways and the more confident reading wins.
    fn column_reading(
        &self,
        img: &RgbImage,
        boxes: &[RotRect],
        cancel: &AtomicBool,
    ) -> Result<Columns, Error> {
        let mut tall: Vec<&RotRect> = boxes.iter().filter(|r| is_column(r)).collect();
        tall.sort_by(|a, b| b.height().total_cmp(&a.height()));
        tall.truncate(SAMPLE_COLUMNS);
        let down: Vec<RotRect> = tall.iter().map(|r| column(r)).collect();
        // Read from the bottom: the far corner first, axes reversed.
        let up: Vec<RotRect> = down
            .iter()
            .map(|r| RotRect {
                origin: r.at(1.0, 1.0),
                u: [-r.u[0], -r.u[1]],
                v: [-r.v[0], -r.v[1]],
            })
            .collect();
        let read_down = self.read_lines(img, &down, cancel)?;
        let read_up = self.read_lines(img, &up, cancel)?;
        if confidence(&read_up) > confidence(&read_down) {
            return Ok(Columns::Up);
        }
        let (mut cjk, mut all) = (0, 0);
        for c in read_down.iter().flatten().flat_map(|w| w.text.chars()) {
            all += 1;
            cjk += usize::from(is_cjk(c));
        }
        Ok(Columns::Down { cjk: cjk * 2 > all })
    }

    /// Which way the characters of tall lines stand, read one by one: upright (0, vertical
    /// writing), or lying on their sides as on a page that needs 3 quarter turns (turned
    /// clockwise) or, when `both`, 1. Samples are the top square of the tallest columns
    /// and the square boxes (lines of one character); the most confident reading wins.
    fn character_turn(
        &self,
        img: &RgbImage,
        boxes: &[RotRect],
        both: bool,
        cancel: &AtomicBool,
    ) -> Result<u8, Error> {
        let mut tall: Vec<&RotRect> = boxes.iter().filter(|r| is_column(r)).collect();
        tall.sort_by(|a, b| b.height().total_cmp(&a.height()));
        let mut squares: Vec<RotRect> = tall
            .iter()
            .take(SAMPLE_COLUMNS)
            .map(|r| {
                let k = r.width() / r.height().max(1.0);
                RotRect {
                    origin: r.origin,
                    u: r.u,
                    v: [r.v[0] * k, r.v[1] * k],
                }
            })
            .collect();
        squares.extend(
            boxes
                .iter()
                .filter(|r| !is_column(r) && r.width() <= r.height() * 1.2)
                .take(SAMPLE_COLUMNS)
                .copied(),
        );
        if squares.is_empty() {
            return Ok(0);
        }
        // Read as they stand, turned a quarter counter-clockwise (what a page turned
        // clockwise needs) and clockwise.
        let ccw: Vec<RotRect> = squares.iter().map(column).collect();
        let cw: Vec<RotRect> = squares
            .iter()
            .map(|r| RotRect {
                origin: r.at(0.0, 1.0),
                u: [-r.v[0], -r.v[1]],
                v: r.u,
            })
            .collect();
        let upright = confidence(&self.read_lines(img, &squares, cancel)?);
        let mut best = (0, upright);
        let turned = confidence(&self.read_lines(img, &ccw, cancel)?);
        if turned > best.1 {
            best = (3, turned);
        }
        if both {
            let turned = confidence(&self.read_lines(img, &cw, cancel)?);
            if turned > best.1 {
                best = (1, turned);
            }
        }
        Ok(best.0)
    }

    /// Runs a classifier: `[batch, classes]` scores.
    fn classify(&self, m: &Model, input: NdTensor<f32, 4>) -> Result<NdTensor<f32, 2>, Error> {
        let out = m
            .run_one(input.into(), self.opts())
            .map_err(|e| Error::Inference(e.to_string()))?;
        out.try_into()
            .map_err(|_| Error::Inference("classifier output".into()))
    }

    /// Text line boxes in image pixels, unordered.
    pub fn detect(&self, img: &RgbImage, params: &EngineParams) -> Result<Vec<RotRect>, Error> {
        let (w, h) = (img.width, img.height);
        if w == 0 || h == 0 {
            return Ok(Vec::new());
        }
        let scale = (params.detection_max_side as f32 / w.max(h) as f32).min(1.0);
        let round = |v: usize| ((v as f32 * scale / 32.0).round() as usize).max(1) * 32;
        let (dw, dh) = (round(w), round(h));
        let small = img.resize(dw, dh);
        // BGR, ImageNet mean and deviation.
        const MEAN: [f32; 3] = [0.485, 0.456, 0.406];
        const STD: [f32; 3] = [0.229, 0.224, 0.225];
        let mut input = vec![0.0f32; 3 * dw * dh];
        for (i, p) in small.data.as_chunks::<3>().0.iter().enumerate() {
            for c in 0..3 {
                let v = f32::from(p[2 - c]) / 255.0;
                input[c * dw * dh + i] = (v - MEAN[c]) / STD[c];
            }
        }
        drop(small);
        let input = NdTensor::from_data([1, 3, dh, dw], input);
        let out = self
            .det
            .run_one(input.into(), self.opts())
            .map_err(|e| Error::Inference(e.to_string()))?;
        let out: NdTensor<f32, 4> = out
            .try_into()
            .map_err(|_| Error::Inference("detection output".into()))?;
        let [_, _, oh, ow] = out.shape();
        let prob = out.to_vec();
        let (sx, sy) = (w as f32 / ow as f32, h as f32 / oh as f32);
        Ok(detections(&prob, ow, oh, &params.db)
            .into_iter()
            .map(|r| RotRect {
                origin: [r.origin[0] * sx, r.origin[1] * sy],
                u: [r.u[0] * sx, r.u[1] * sy],
                v: [r.v[0] * sx, r.v[1] * sy],
            })
            .collect())
    }

    /// Recognises each line box, batching crops of similar width.
    pub(crate) fn read_lines(
        &self,
        img: &RgbImage,
        boxes: &[RotRect],
        cancel: &AtomicBool,
    ) -> Result<Vec<Vec<DecodedWord>>, Error> {
        let widths: Vec<usize> = boxes
            .iter()
            .map(|r| {
                let ratio = r.width() / r.height().max(1.0);
                ((REC_HEIGHT as f32 * ratio).ceil() as usize).clamp(REC_HEIGHT / 4, REC_MAX_WIDTH)
            })
            .collect();
        let mut order: Vec<usize> = (0..boxes.len()).collect();
        order.sort_by_key(|&i| widths[i]);
        let mut out = vec![Vec::new(); boxes.len()];
        let mut start = 0;
        while start < order.len() {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let mut end = start + 1;
            while end < order.len()
                && widths[order[end]] * (end + 1 - start) <= REC_BATCH_PIXELS / REC_HEIGHT
            {
                end += 1;
            }
            let batch = &order[start..end];
            let wmax = widths[*batch.last().unwrap_or(&0)];
            let plane = REC_HEIGHT * wmax;
            let mut input = vec![0.0f32; batch.len() * 3 * plane];
            for (b, &i) in batch.iter().enumerate() {
                let crop = warp(img, &boxes[i], widths[i], REC_HEIGHT);
                let base = b * 3 * plane;
                for y in 0..REC_HEIGHT {
                    for x in 0..widths[i] {
                        let p = &crop.data[(y * widths[i] + x) * 3..][..3];
                        for c in 0..3 {
                            input[base + c * plane + y * wmax + x] =
                                f32::from(p[2 - c]) / 127.5 - 1.0;
                        }
                    }
                }
            }
            let input = NdTensor::from_data([batch.len(), 3, REC_HEIGHT, wmax], input);
            let res = self
                .rec
                .run_one(input.into(), self.opts())
                .map_err(|e| Error::Inference(e.to_string()))?;
            let res: NdTensor<f32, 3> = res
                .try_into()
                .map_err(|_| Error::Inference("recognition output".into()))?;
            let [_, frames, classes] = res.shape();
            if classes != self.charset.classes() {
                return Err(Error::BadModel(format!(
                    "{classes} classes, dictionary has {}",
                    self.charset.classes()
                )));
            }
            let all = res.to_vec();
            for (b, &i) in batch.iter().enumerate() {
                let probs = &all[b * frames * classes..(b + 1) * frames * classes];
                let share = widths[i] as f32 / wmax as f32;
                out[i] = decode(probs, frames, classes, &self.charset, share);
            }
            start = end;
        }
        Ok(out)
    }
}

/// How the text of an image lies, from [`Engine::orient`].
struct TextLayout {
    /// Clockwise quarter turns that make the image read upright.
    turn: u8,
    /// The turned image, when `turn` is not 0.
    upright: Option<RgbImage>,
    /// Text lines of the upright image.
    boxes: Vec<RotRect>,
    /// Vertical writing: tall lines are columns read top to bottom, right to left.
    columns: bool,
}

/// Text lines of an image ready to be read, from [`Engine::layout`].
#[derive(Debug)]
pub struct LineLayout<'a> {
    image: &'a RgbImage,
    upright: Option<RgbImage>,
    /// Clockwise quarter turns from `image` to `upright`.
    turn: u8,
    /// In `upright`'s pixels, in reading order.
    boxes: Vec<RotRect>,
}

impl LineLayout<'_> {
    /// The number of lines.
    pub fn len(&self) -> usize {
        self.boxes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.boxes.is_empty()
    }

    fn upright(&self) -> &RgbImage {
        self.upright.as_ref().unwrap_or(self.image)
    }

    /// Line `i`'s box in the pixels of the image as given.
    pub fn line_box(&self, i: usize) -> RotRect {
        let (w, h) = self.upright_size();
        let r = self.boxes[i];
        if self.turn == 0 {
            r
        } else {
            r.unturned(self.turn, w, h)
        }
    }

    /// The size `unturned` wants: the upright image's, turned back.
    fn upright_size(&self) -> (f32, f32) {
        let img = self.upright();
        let (w, h) = (img.height as f32, img.width as f32);
        if self.turn == 2 { (h, w) } else { (w, h) }
    }

    fn unturn(&self, l: &mut ImageLine) {
        if self.turn == 0 {
            return;
        }
        let (w, h) = self.upright_size();
        l.rect = l.rect.unturned(self.turn, w, h);
        for wd in &mut l.words {
            wd.rect = wd.rect.unturned(self.turn, w, h);
        }
    }
}

/// Which way tall text lines read.
enum Columns {
    /// Top to bottom; `cjk` when most of what they say is Chinese, Japanese or Korean, so
    /// they are vertical writing rather than a page turned a quarter clockwise.
    Down { cjk: bool },
    /// Bottom to top: a page turned a quarter counter-clockwise.
    Up,
}

/// The summed mean confidence of each line read.
fn confidence(lines: &[Vec<DecodedWord>]) -> f32 {
    lines
        .iter()
        .map(|words| {
            let n: usize = words.iter().map(|w| w.text.chars().count()).sum();
            let sum: f32 = words
                .iter()
                .map(|w| w.confidence * w.text.chars().count() as f32)
                .sum();
            sum / n.max(1) as f32
        })
        .sum()
}

/// Whether a line box is a column of vertical writing rather than a horizontal line.
fn is_column(r: &RotRect) -> bool {
    r.height() > r.width() * 1.2
}

/// A column box as the recogniser reads it: from its top-right corner down, so the crop is
/// the column turned a quarter counter-clockwise (as PP-OCR reads vertical text).
fn column(r: &RotRect) -> RotRect {
    RotRect {
        origin: r.at(1.0, 0.0),
        u: r.v,
        v: [-r.u[0], -r.u[1]],
    }
}

/// Han, kana, Hangul and their punctuation and full-width forms.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF
        | 0x3000..=0x30FF
        | 0x3130..=0x318F
        | 0x31F0..=0x31FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF
        | 0x20000..=0x2FFFF)
}

/// Vertical writing in reading order: horizontal lines above the columns (headings), the
/// columns right to left (top to bottom within one), then the other horizontal lines.
/// Columns become boxes read downwards (see [`column`]).
fn column_reading_order(boxes: Vec<RotRect>) -> Vec<RotRect> {
    let (mut cols, rows): (Vec<RotRect>, Vec<RotRect>) = boxes.into_iter().partition(is_column);
    let top = cols
        .iter()
        .map(|r| r.origin[1])
        .fold(f32::INFINITY, f32::min);
    let (mut above, mut below): (Vec<RotRect>, Vec<RotRect>) =
        rows.into_iter().partition(|r| r.at(0.5, 0.5)[1] < top);
    sort_reading_order(&mut above);
    sort_reading_order(&mut below);
    let centre = |r: &RotRect| r.at(0.5, 0.5);
    cols.sort_by(|a, b| {
        let (ca, cb) = (centre(a), centre(b));
        cb[0].total_cmp(&ca[0]).then(ca[1].total_cmp(&cb[1]))
    });
    // Boxes of one column (centres within half a width) top to bottom.
    for i in 1..cols.len() {
        let mut j = i;
        while j > 0 {
            let (a, b) = (cols[j - 1], cols[j]);
            let same = (centre(&a)[0] - centre(&b)[0]).abs() < a.width().min(b.width()) * 0.5;
            if same && centre(&b)[1] < centre(&a)[1] {
                cols.swap(j - 1, j);
                j -= 1;
            } else {
                break;
            }
        }
    }
    above
        .into_iter()
        .chain(cols.iter().map(column))
        .chain(below)
        .collect()
}

/// Top to bottom, then left to right among boxes on the same line (PaddleOCR's order).
fn sort_reading_order(boxes: &mut [RotRect]) {
    boxes.sort_by(|a, b| {
        let (pa, pb) = (a.origin, b.origin);
        pa[1].total_cmp(&pb[1]).then(pa[0].total_cmp(&pb[0]))
    });
    for i in 1..boxes.len() {
        let mut j = i;
        while j > 0 {
            let (a, b) = (boxes[j - 1], boxes[j]);
            let ca = a.at(0.5, 0.5)[1];
            let cb = b.at(0.5, 0.5)[1];
            let same_line = (ca - cb).abs() < a.height().min(b.height()) * 0.5;
            if same_line && b.origin[0] < a.origin[0] {
                boxes.swap(j - 1, j);
                j -= 1;
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: f32, y: f32, w: f32, h: f32) -> RotRect {
        RotRect {
            origin: [x, y],
            u: [w, 0.0],
            v: [0.0, h],
        }
    }

    #[test]
    fn columns_read_down_from_the_right() {
        let c = column(&rect(10.0, 20.0, 30.0, 200.0));
        // From the top-right corner down, the line's top towards the right edge.
        assert_eq!(
            (c.origin, c.u, c.v),
            ([40.0, 20.0], [0.0, 200.0], [-30.0, 0.0])
        );
        assert_eq!((c.width(), c.height()), (200.0, 30.0));
        assert!(is_column(&rect(0.0, 0.0, 30.0, 60.0)));
        assert!(!is_column(&rect(0.0, 0.0, 30.0, 30.0)));
    }

    #[test]
    fn vertical_writing_order() {
        let heading = rect(100.0, 10.0, 300.0, 30.0);
        let folio = rect(250.0, 900.0, 40.0, 20.0);
        let right = rect(400.0, 60.0, 30.0, 400.0);
        let right_lower = rect(402.0, 500.0, 30.0, 300.0);
        let left = rect(300.0, 60.0, 30.0, 700.0);
        let order = column_reading_order(vec![left, folio, right_lower, heading, right]);
        let want = [
            heading,
            column(&right),
            column(&right_lower),
            column(&left),
            folio,
        ];
        assert_eq!(order, want);
    }
}
