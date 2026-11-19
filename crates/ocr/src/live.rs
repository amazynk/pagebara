//! Live text: pictures of text recognised on demand, without changing the document. The
//! places a page may show text without having it ([`regions`]: images, and vector drawings
//! of glyph-sized shapes) are recognised one at a time ([`recognize_region`]) at a
//! resolution suited to the size of their text, and the words found are laid out together
//! with the page's own text ([`page_with_live`]), so selecting and copying treat both alike.

use std::sync::Mutex;
use std::sync::atomic::AtomicBool;

use pagebara_content::{
    Cmd, DisplayList, ExtraChar, Interpreter, Matrix, Point, Rect, TextPage, extract_text_with,
    is_cjk,
};
use pagebara_pages::PageTree;
use pagebara_render::{Annotations, PixelRect, RenderError, RenderRequest, Renderer, page_matrix};
use pagebara_syntax::Document;

use crate::Error;
use crate::engine::{Engine, EngineParams};
use crate::image::{RgbImage, RotRect};
use crate::page::{Line, Word, page_dict, to_page_space};
use crate::underscore::take_in_underscores;

/// Regions smaller than this (points) on either side hold no readable text.
const MIN_SIDE: f32 = 12.0;
/// Pictures with fewer pixels hold no readable text.
const MIN_PIXELS: u32 = 1200;
/// Glyph-sized shapes: heights in points.
const GLYPH_HEIGHT: std::ops::RangeInclusive<f32> = 2.5..=72.0;
/// Shapes in a cluster before it counts as text drawn as outlines.
const MIN_SHAPES: usize = 6;
/// Regions kept per page (the largest).
const MAX_REGIONS: usize = 48;
/// Rendered regions are scaled down to stay within this many pixels.
const MAX_PIXELS: f32 = 24e6;

/// A place on a page that may show text it does not have.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    /// In the page's default user space.
    pub rect: Rect,
    /// Pixels per point of the picture there; `None` for vector drawings.
    pub px_per_pt: Option<f32>,
}

/// The pictures and drawings of glyph-sized shapes of a display list, within `crop`.
pub fn regions(list: &DisplayList, crop: Rect) -> Vec<Region> {
    let mut out = Vec::new();
    let mut shapes: Vec<Rect> = Vec::new();
    for cmd in &list.cmds {
        match *cmd {
            Cmd::Image { image, ctm, .. } => {
                let m = list.matrix(ctm);
                let Some(img) = list.images.get(image as usize) else {
                    continue;
                };
                let Some(rect) = clip(Rect::new(0.0, 0.0, 1.0, 1.0).transform(&m), crop) else {
                    continue;
                };
                let (w, h) = (m.a.hypot(m.b), m.c.hypot(m.d));
                if img.width.min(img.height) < 8
                    || img.width.saturating_mul(img.height) < MIN_PIXELS
                    || w * h <= 0.0
                {
                    continue;
                }
                let ppp = ((img.width as f32 / w) * (img.height as f32 / h)).sqrt();
                if ppp.is_finite() {
                    out.push(Region {
                        rect,
                        px_per_pt: Some(ppp),
                    });
                }
            }
            Cmd::Fill { path, ctm, .. } => {
                let Some(b) = list.path(path).bounds() else {
                    continue;
                };
                let r = b.transform(&list.matrix(ctm));
                let (w, h) = (r.width(), r.height());
                if GLYPH_HEIGHT.contains(&h.min(w).max(h.min(w * 4.0))) && w < h * 6.0 + 8.0 {
                    shapes.push(r);
                }
            }
            _ => {}
        }
    }
    for c in clusters(&shapes) {
        if let Some(rect) = clip(c, crop) {
            out.push(Region {
                rect,
                px_per_pt: None,
            });
        }
    }
    out.sort_by(|a, b| area(&b.rect).total_cmp(&area(&a.rect)));
    out.truncate(MAX_REGIONS);
    out
}

fn area(r: &Rect) -> f32 {
    r.width() * r.height()
}

fn clip(r: Rect, crop: Rect) -> Option<Rect> {
    let c = Rect::new(
        r.x0.max(crop.x0),
        r.y0.max(crop.y0),
        r.x1.min(crop.x1),
        r.y1.min(crop.y1),
    );
    (c.width() >= MIN_SIDE && c.height() >= MIN_SIDE && c.x0.is_finite() && c.y0.is_finite())
        .then_some(c)
}

/// Boxes of shapes close to one another (within half their height), as clusters of at
/// least [`MIN_SHAPES`].
fn clusters(shapes: &[Rect]) -> Vec<Rect> {
    let n = shapes.len();
    if n < MIN_SHAPES {
        return Vec::new();
    }
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(p: &mut [usize], mut i: usize) -> usize {
        while p[i] != i {
            p[i] = p[p[i]];
            i = p[i];
        }
        i
    }
    let grown: Vec<Rect> = shapes
        .iter()
        .map(|r| {
            let d = r.height().min(24.0) * 0.6;
            Rect::new(r.x0 - d, r.y0 - d * 0.8, r.x1 + d, r.y1 + d * 0.8)
        })
        .collect();
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| grown[a].x0.total_cmp(&grown[b].x0));
    for (k, &a) in order.iter().enumerate() {
        for &b in &order[k + 1..] {
            if grown[b].x0 > grown[a].x1 {
                break;
            }
            let (ra, rb) = (&grown[a], &grown[b]);
            if ra.y0 <= rb.y1 && rb.y0 <= ra.y1 {
                let (x, y) = (find(&mut parent, a), find(&mut parent, b));
                parent[x.max(y)] = x.min(y);
            }
        }
    }
    let mut groups: std::collections::HashMap<usize, (Rect, usize)> = Default::default();
    for (i, shape) in shapes.iter().enumerate() {
        let root = find(&mut parent, i);
        groups
            .entry(root)
            .and_modify(|g| {
                g.0 = g.0.union(shape);
                g.1 += 1;
            })
            .or_insert((*shape, 1));
    }
    groups
        .into_values()
        .filter(|g| g.1 >= MIN_SHAPES)
        .map(|g| {
            let d = 2.0;
            Rect::new(g.0.x0 - d, g.0.y0 - d, g.0.x1 + d, g.0.y1 + d)
        })
        .collect()
}

/// How regions are recognised.
#[derive(Clone, Debug)]
pub struct LiveParams {
    /// Pictures with text lines lower than this many pixels are enlarged (drawings are
    /// drawn larger) before reading.
    pub small_text: f32,
    /// Text lines are read at about this height in pixels, when enlarged.
    pub line_height: f32,
    /// Pictures are enlarged at most this much for small text.
    pub max_upscale: f32,
    /// Lines are first looked for in a copy at most this large (pixels on the longer
    /// side), which is enough when their text is large.
    pub probe_side: usize,
    /// Lines nearest the pointer read first, on their own.
    pub first_lines: usize,
    /// Threads reading the other lines, and lines each reads at a time.
    pub threads: usize,
    pub chunk_lines: usize,
    pub engine: EngineParams,
}

impl Default for LiveParams {
    fn default() -> Self {
        LiveParams {
            small_text: 20.0,
            line_height: 32.0,
            max_upscale: 3.0,
            probe_side: 1280,
            first_lines: 1,
            threads: std::thread::available_parallelism()
                .map_or(1, |n| n.get() / 4)
                .clamp(1, 3),
            chunk_lines: 6,
            engine: EngineParams {
                detection_max_side: 2400,
                ..EngineParams::default()
            },
        }
    }
}

/// Renders `rect` (user space) of a page at `scale` pixels per point: the image and the
/// matrix from its pixels to the page's default user space.
pub fn render_region(
    renderer: &Renderer,
    page: usize,
    rect: Rect,
    scale: f32,
    cancel: &AtomicBool,
) -> Result<(RgbImage, Matrix), Error> {
    let tree = PageTree::open(renderer.document());
    let p = tree.page(page).ok_or(Error::NoSuchPage(page))?;
    let c = p.crop_box;
    let crop = Rect::new(c.x0 as f32, c.y0 as f32, c.x1 as f32, c.y1 as f32);
    let uu = p.user_unit as f32;
    let px = area(&rect) * scale * scale * uu * uu;
    let scale = if px > MAX_PIXELS {
        scale * (MAX_PIXELS / px).sqrt()
    } else {
        scale
    };
    let m = page_matrix(crop, p.rotate, scale * uu, 0, 0);
    let r = rect.transform(&m);
    let (x0, y0) = (r.x0.floor().max(0.0), r.y0.floor().max(0.0));
    let (x1, y1) = (r.x1.ceil(), r.y1.ceil());
    if x1 - x0 < 2.0 || y1 - y0 < 2.0 {
        return Err(Error::Render("empty region".into()));
    }
    let px = PixelRect {
        x: x0 as i32,
        y: y0 as i32,
        w: (x1 - x0) as u32,
        h: (y1 - y0) as u32,
    };
    let mut req = RenderRequest::new(scale, px);
    req.annotations = Annotations::None;
    req.cancel = Some(cancel);
    let img = renderer.render(page, &req).map_err(|e| match e {
        RenderError::Cancelled => Error::Cancelled,
        RenderError::NoSuchPage(p) => Error::NoSuchPage(p),
        e => Error::Render(e.to_string()),
    })?;
    let rgb = RgbImage::from_rgba(img.width as usize, img.height as usize, &img.data);
    let inv = m.invert().ok_or(Error::Render("degenerate page".into()))?;
    Ok((rgb, Matrix::translate(x0, y0).then(&inv)))
}

/// Recognises the text in `region` of a page. Pictures are rendered at their own
/// resolution (drawings at 150 dpi) and lines are looked for in a copy no larger than
/// [`LiveParams::probe_side`]; small text is rendered again larger, and large text is
/// read where the copy found it. Lines are in the page's default user space.
pub fn recognize_region(
    engine: &Engine,
    renderer: &Renderer,
    page: usize,
    region: &Region,
    params: &LiveParams,
    cancel: &AtomicBool,
) -> Result<Vec<Line>, Error> {
    recognize_region_live(
        engine,
        renderer,
        page,
        region,
        params,
        &|| None,
        &mut |_| {},
        cancel,
    )
}

/// [`recognize_region`] reading the lines nearest `focus` (where the pointer is, in user
/// space, asked again as the work goes on) first, and handing each batch of lines read to
/// `publish` (all lines so far, in reading order) so they can be selected while the rest
/// is read. Lines are read on [`LiveParams::threads`] threads.
#[allow(clippy::too_many_arguments)]
pub fn recognize_region_live(
    engine: &Engine,
    renderer: &Renderer,
    page: usize,
    region: &Region,
    params: &LiveParams,
    focus: &(dyn Fn() -> Option<Point> + Sync),
    publish: &mut dyn FnMut(&[Line]),
    cancel: &AtomicBool,
) -> Result<Vec<Line>, Error> {
    let Some((img, to_page, boxes)) = find_lines(engine, renderer, page, region, params, cancel)?
    else {
        return Ok(Vec::new());
    };
    let layout = engine.layout(&img, boxes, &params.engine, cancel)?;
    let n = layout.len();
    let from_page = to_page.invert();
    let rects: Vec<Rect> = (0..n)
        .map(|i| {
            let r = layout.line_box(i);
            let corners = [
                r.at(0.0, 0.0),
                r.at(1.0, 0.0),
                r.at(1.0, 1.0),
                r.at(0.0, 1.0),
            ];
            Rect::from_points(&corners.map(|[x, y]| Point::new(x, y)))
        })
        .collect();
    let pending = Mutex::new((0..n).collect::<Vec<usize>>());
    // The `k` lines left nearest the pointer (in reading order without one).
    let take = |k: usize| -> Vec<usize> {
        let at = focus().zip(from_page).map(|(p, m)| m.apply(p));
        let mut left = pending.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(p) = at {
            let d = |i: &usize| distance(&rects[*i], p);
            left.sort_by(|a, b| d(b).total_cmp(&d(a)).then(b.cmp(a)));
        } else {
            left.sort_by(|a, b| b.cmp(a));
        }
        let keep = left.len().saturating_sub(k);
        left.split_off(keep)
    };
    let mut read: Vec<Option<Line>> = vec![None; n];
    let mut store = |ids: &[usize], lines: Vec<Option<crate::ImageLine>>| {
        for (&i, l) in ids.iter().zip(lines) {
            read[i] = l.and_then(|l| to_page_space(page, vec![l], &to_page).lines.pop());
        }
        let so_far: Vec<Line> = read.iter().flatten().cloned().collect();
        publish(&so_far);
    };
    // The lines nearest the pointer on their own, to be selectable soonest.
    if focus().is_some() {
        let ids = take(params.first_lines);
        let lines = engine.read_layout(&layout, &ids, &params.engine, cancel)?;
        store(&ids, lines);
    }
    let threads = params.threads.clamp(1, 8);
    let chunk = params.chunk_lines.max(1);
    let (tx, rx) = std::sync::mpsc::channel();
    let failed = std::thread::scope(|scope| {
        for t in 0..threads {
            let (tx, take, layout) = (tx.clone(), &take, &layout);
            pagebara_thread::spawn_scoped(scope, format!("ai-infer-{t}"), move || {
                loop {
                    let ids = take(chunk);
                    if ids.is_empty() {
                        return;
                    }
                    let r = engine.read_layout(layout, &ids, &params.engine, cancel);
                    let stop = r.is_err();
                    if tx.send((ids, r)).is_err() || stop {
                        return;
                    }
                }
            });
        }
        drop(tx);
        let mut failed = None;
        for (ids, r) in rx {
            match r {
                Ok(lines) if failed.is_none() => store(&ids, lines),
                Ok(_) => {}
                Err(e) => failed = failed.or(Some(e)),
            }
        }
        failed
    });
    if let Some(e) = failed {
        return Err(e);
    }
    Ok(read.into_iter().flatten().collect())
}

/// Distance from a point to a rectangle (0 inside).
fn distance(r: &Rect, p: Point) -> f32 {
    let dx = (r.x0 - p.x).max(p.x - r.x1).max(0.0);
    let dy = (r.y0 - p.y).max(p.y - r.y1).max(0.0);
    dx.hypot(dy)
}

/// The picture of a region to read, the matrix from its pixels to user space, and its
/// text line boxes; `None` when it has no text.
fn find_lines(
    engine: &Engine,
    renderer: &Renderer,
    page: usize,
    region: &Region,
    params: &LiveParams,
    cancel: &AtomicBool,
) -> Result<Option<(RgbImage, Matrix, Vec<RotRect>)>, Error> {
    let picture = region.px_per_pt.is_some();
    let s0 = match region.px_per_pt {
        Some(p) => p.clamp(1.0, 6.0),
        None => 150.0 / 72.0,
    };
    let (img, to_page, dark, probe) = probe(engine, renderer, page, region, s0, params, cancel)?;
    if probe.boxes.is_empty() {
        return Ok(None);
    }
    let k = probe.scale;
    // Line height in pixels of `img`.
    let h = probe.height / k;
    if h < params.small_text || (!picture && h > 3.0 * params.line_height) {
        let max = if picture { params.max_upscale } else { 4.0 };
        let f = (params.line_height / h).clamp(0.25, max);
        let (mut big, to_page) = render_region(renderer, page, region.rect, s0 * f, cancel)?;
        if dark {
            invert(&mut big);
        }
        let boxes = take_in_underscores(&big, engine.detect(&big, &params.engine)?);
        return Ok(Some((big, to_page, boxes)));
    }
    let boxes = if k == 1.0 || probe.height >= params.small_text * 0.7 {
        probe.boxes.iter().map(|b| b.scaled(1.0 / k)).collect()
    } else {
        engine.detect(&img, &params.engine)?
    };
    let boxes = take_in_underscores(&img, boxes);
    Ok(Some((img, to_page, boxes)))
}

/// Renders a region at `s0` pixels per point (made dark on light) and finds its lines in
/// a copy no larger than [`LiveParams::probe_side`]. A large picture (a scan) is rendered
/// at that size for the probe while it is rendered in full on another thread.
#[allow(clippy::type_complexity)]
fn probe(
    engine: &Engine,
    renderer: &Renderer,
    page: usize,
    region: &Region,
    s0: f32,
    params: &LiveParams,
    cancel: &AtomicBool,
) -> Result<(RgbImage, Matrix, bool, DbProbe), Error> {
    let r = region.rect;
    let k = params.probe_side as f32 / (r.width().max(r.height()) * s0);
    if k >= 0.9 {
        let (mut img, to_page) = render_region(renderer, page, r, s0, cancel)?;
        let dark = light_on_dark(&img);
        if dark {
            invert(&mut img);
        }
        let k = (params.probe_side as f32 / img.width.max(img.height) as f32).min(1.0);
        let probe = if k < 1.0 {
            let small = img.resize(
                ((img.width as f32 * k).round() as usize).max(1),
                ((img.height as f32 * k).round() as usize).max(1),
            );
            DbProbe::run(engine, &small, k, params)?
        } else {
            DbProbe::run(engine, &img, 1.0, params)?
        };
        return Ok((img, to_page, dark, probe));
    }
    std::thread::scope(|scope| {
        let full = pagebara_thread::spawn_scoped(scope, "ai-render", || {
            render_region(renderer, page, r, s0, cancel)
        });
        let found = render_region(renderer, page, r, s0 * k, cancel).and_then(|(mut small, _)| {
            let dark = light_on_dark(&small);
            if dark {
                invert(&mut small);
            }
            let probe = DbProbe::run(engine, &small, 0.0, params)?;
            Ok((small.width, dark, probe))
        });
        let full = full
            .join()
            .map_err(|_| Error::Render("render failed".into()))?;
        let ((width, dark, mut probe), (mut img, to_page)) = (found?, full?);
        if dark {
            invert(&mut img);
        }
        probe.scale = width as f32 / img.width.max(1) as f32;
        Ok((img, to_page, dark, probe))
    })
}

/// Text lines found in a copy of an image scaled by `scale`, and their median height
/// there.
struct DbProbe {
    boxes: Vec<RotRect>,
    height: f32,
    scale: f32,
}

impl DbProbe {
    fn run(
        engine: &Engine,
        small: &RgbImage,
        scale: f32,
        params: &LiveParams,
    ) -> Result<Self, Error> {
        let boxes = engine.detect(small, &params.engine)?;
        let mut heights: Vec<f32> = boxes.iter().map(|b| b.width().min(b.height())).collect();
        heights.sort_by(f32::total_cmp);
        let height = heights
            .get(heights.len() / 2)
            .copied()
            .unwrap_or(0.0)
            .max(1.0);
        Ok(DbProbe {
            boxes,
            height,
            scale,
        })
    }
}

/// Whether an image is mostly dark (light text on a dark background, as in dark themes):
/// the models read dark text on light best.
fn light_on_dark(img: &RgbImage) -> bool {
    let px = img.data.as_chunks::<3>().0;
    if px.is_empty() {
        return false;
    }
    let step = (px.len() / 20_000).max(1);
    let (mut dark, mut n) = (0usize, 0usize);
    for p in px.iter().step_by(step) {
        let l = (u32::from(p[0]) * 3 + u32::from(p[1]) * 6 + u32::from(p[2])) / 10;
        dark += usize::from(l < 100);
        n += 1;
    }
    dark * 10 > n * 6
}

fn invert(img: &mut RgbImage) {
    for v in &mut img.data {
        *v = 255 - *v;
    }
}

/// The text of page `page` with the recognised `lines` laid out among its own text (whose
/// words win where both are), only what lies in `clip` when given.
pub fn page_with_live(doc: &Document, page: usize, lines: &[Line], clip: Option<Rect>) -> TextPage {
    let tree = PageTree::open(doc);
    let Some((d, res)) = tree.page(page).and_then(|p| page_dict(doc, &p)) else {
        return TextPage::default();
    };
    let list = Interpreter::new(doc).page(&d, &res);
    let inside = |q: &[Point; 4]| {
        clip.is_none_or(|c| {
            let r = Rect::from_points(q);
            let (x, y) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
            c.x0 <= x && x <= c.x1 && c.y0 <= y && y <= c.y1
        })
    };
    let mut extra = live_chars(lines);
    extra.retain(|e| inside(&e.quad));
    extract_text_with(doc, &list, &extra, &inside)
}

/// The recognised text alone, laid out as a page (for pictures outside a document).
pub fn lines_text(lines: &[Line]) -> TextPage {
    let Ok(doc) =
        Document::open(b"%PDF-1.4\n1 0 obj\n<<>>\nendobj\ntrailer\n<</Root 1 0 R>>\n".to_vec())
    else {
        return TextPage::default();
    };
    extract_text_with(&doc, &DisplayList::default(), &live_chars(lines), &|_| true)
}

/// The characters of recognised lines, with spaces between words; code in a monospaced
/// font keeps its indentation and the spaces between words.
pub fn live_chars(lines: &[Line]) -> Vec<ExtraChar> {
    let grid = Grid::of(lines);
    let size = font_sizes(lines, grid.is_some());
    let mut out = Vec::new();
    let mut word_id = 0u32;
    for (line_id, row) in rows(lines, grid.as_ref()).into_iter().enumerate() {
        let first = out.len();
        let cjk_line = row.iter().filter(|w| mostly_cjk_word(w.1)).count() * 2 > row.len();
        let mut prev: Option<(&Word, i32)> = None;
        for &(starts, w) in &row {
            let cell = grid.as_ref().map(|g| match prev {
                Some((p, at)) if !starts => g.next_cell(p, at, w),
                _ => g.first_cell(w),
            });
            match (prev, &grid, cell) {
                (None, Some(g), Some(c)) if c > 0 => {
                    // Indentation: spaces one cell each, back from the first word.
                    let n = c.min(200);
                    for k in 0..n {
                        let back = (n - k) as f32 * g.pitch;
                        let q = shifted(&w.quad, g.dir, -back, g.pitch);
                        out.push(space(q, word_id));
                    }
                    // The first character starts where the spaces end.
                    if let Some(last) = out.last_mut() {
                        last.quad[1] = w.quad[0];
                        last.quad[2] = w.quad[3];
                    }
                }
                (Some((p, pc)), Some(g), Some(c)) => {
                    let len = p.text.chars().count() as i32;
                    let gap = gap_quad(&p.quad, &w.quad);
                    // At least as many spaces as fill the gap: none is wider than a cell.
                    let wide =
                        ((gap[1].x - gap[0].x).hypot(gap[1].y - gap[0].y) / g.pitch).round() as i32;
                    let n = (c - pc - len).max(wide).clamp(1, 200);
                    for k in 0..n {
                        let (a, b) = (k as f32 / n as f32, (k + 1) as f32 / n as f32);
                        out.push(space(slice(&gap, a, b), word_id));
                    }
                }
                (Some((p, _)), None, _) => {
                    let joined = cjk_line
                        && p.text.chars().last().is_some_and(is_cjk)
                        && w.text.chars().next().is_some_and(is_cjk);
                    if joined {
                        // Characters run on: the last one reaches the next word.
                        if let Some(last) = out.last_mut() {
                            last.quad[1] = w.quad[0];
                            last.quad[2] = w.quad[3];
                        }
                    } else {
                        out.push(space(gap_quad(&p.quad, &w.quad), word_id));
                    }
                }
                _ => {}
            }
            word_id += 1;
            push_word(&mut out, w, word_id, cjk_line, grid.is_some());
            prev = Some((w, cell.unwrap_or(0)));
        }
        let h = row.first().map_or(0.0, |w| size(w.1));
        for c in &mut out[first..] {
            c.line = line_id as u32;
            c.size = h;
        }
        word_id += 1;
    }
    out
}

/// The words of each line, the first of a line marked. In monospaced text, lines the
/// detector broke apart (at underscores, say) on one baseline are one row again.
fn rows<'a>(lines: &'a [Line], grid: Option<&Grid>) -> Vec<Vec<(bool, &'a Word)>> {
    let mut out: Vec<Vec<(bool, &Word)>> = lines
        .iter()
        .filter(|l| !l.words.is_empty())
        .map(|l| {
            l.words
                .iter()
                .enumerate()
                .map(|(i, w)| (i == 0, w))
                .collect()
        })
        .collect();
    let Some(g) = grid else {
        return out;
    };
    let n = Point::new(-g.dir.y, g.dir.x);
    let across = |w: &Word| w.quad[0].x * n.x + w.quad[0].y * n.y;
    let along = |w: &Word| w.quad[0].x * g.dir.x + w.quad[0].y * g.dir.y;
    out.sort_by(|a, b| across(b[0].1).total_cmp(&across(a[0].1)));
    let mut merged: Vec<Vec<(bool, &Word)>> = Vec::new();
    for row in out {
        match merged.last_mut() {
            Some(last) if last.iter().any(|w| same_row(w.1, row[0].1, g.dir)) => {
                last.extend(row);
                last.sort_by(|a, b| along(a.1).total_cmp(&along(b.1)));
                last[0].0 = true;
            }
            _ => merged.push(row),
        }
    }
    merged
}

/// The font size of each word's line: three quarters of the height of the box it was
/// found in, the same for lines of about the same height (boxes differ with the letters
/// in them), so lines of one paragraph read as one.
fn font_sizes(lines: &[Line], one: bool) -> impl Fn(&Word) -> f32 {
    let height = |w: &Word| (w.quad[3].x - w.quad[0].x).hypot(w.quad[3].y - w.quad[0].y);
    let mut hs: Vec<f32> = lines
        .iter()
        .filter_map(|l| l.words.first())
        .map(height)
        .collect();
    hs.sort_by(f32::total_cmp);
    let median = hs.get(hs.len() / 2).copied().unwrap_or(0.0);
    move |w: &Word| {
        let h = height(w);
        if one || (h - median).abs() <= 0.35 * median {
            median * 0.75
        } else {
            h * 0.75
        }
    }
}

/// Whether two words lie on one row: the middle of each, across the reading direction
/// `dir`, within the other.
fn same_row(a: &Word, b: &Word, dir: Point) -> bool {
    let n = Point::new(-dir.y, dir.x);
    let span = |w: &Word| {
        let (p, q) = (
            w.quad[0].x * n.x + w.quad[0].y * n.y,
            w.quad[3].x * n.x + w.quad[3].y * n.y,
        );
        (p.min(q), p.max(q))
    };
    let (a, b) = (span(a), span(b));
    let within = |m: f32, r: (f32, f32)| r.0 <= m && m <= r.1;
    within((a.0 + a.1) / 2.0, b) && within((b.0 + b.1) / 2.0, a)
}

fn space(quad: [Point; 4], word: u32) -> ExtraChar {
    ExtraChar {
        text: " ".into(),
        quad,
        space: true,
        word,
        line: 0,
        size: 0.0,
    }
}

fn lerp(a: Point, b: Point, t: f32) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// The part of a quad between fractions `a` and `b` along it.
fn slice(q: &[Point; 4], a: f32, b: f32) -> [Point; 4] {
    [
        lerp(q[0], q[1], a),
        lerp(q[0], q[1], b),
        lerp(q[3], q[2], b),
        lerp(q[3], q[2], a),
    ]
}

/// The space between two words of a line: from the end of `a` along its direction to
/// where `b` starts.
fn gap_quad(a: &[Point; 4], b: &[Point; 4]) -> [Point; 4] {
    let (dx, dy) = (a[1].x - a[0].x, a[1].y - a[0].y);
    let len = dx.hypot(dy);
    if len <= 0.0 {
        return [a[1], b[0], b[3], a[2]];
    }
    let d = Point::new(dx / len, dy / len);
    let reach = ((b[0].x - a[1].x) * d.x + (b[0].y - a[1].y) * d.y).max(0.0);
    let off = |p: Point| Point::new(p.x + d.x * reach, p.y + d.y * reach);
    [a[1], off(a[1]), off(a[2]), a[2]]
}

/// A cell of `width` along `dir`, `by` from the start of `q`.
fn shifted(q: &[Point; 4], dir: Point, by: f32, width: f32) -> [Point; 4] {
    let off = |p: Point, d: f32| Point::new(p.x + dir.x * d, p.y + dir.y * d);
    [
        off(q[0], by),
        off(q[0], by + width),
        off(q[3], by + width),
        off(q[3], by),
    ]
}

fn push_word(out: &mut Vec<ExtraChar>, w: &Word, id: u32, cjk_line: bool, code: bool) {
    let cuts: Vec<(usize, f32)> = if w.cuts.is_empty() {
        // One character per char, evenly spread.
        let n = w.text.chars().count().max(1) as f32;
        w.text
            .char_indices()
            .enumerate()
            .map(|(k, (b, _))| (b, k as f32 / n))
            .collect()
    } else {
        w.cuts.clone()
    };
    for (k, &(b, f)) in cuts.iter().enumerate() {
        let (end, g) = cuts.get(k + 1).map_or((w.text.len(), 1.0), |c| (c.0, c.1));
        let Some(t) = w.text.get(b..end) else {
            continue;
        };
        let text = normalize(t, cjk_line, code);
        if text.is_empty() {
            continue;
        }
        out.push(ExtraChar {
            text,
            quad: slice(&w.quad, f, g.max(f)),
            space: false,
            word: id,
            line: 0,
            size: 0.0,
        });
    }
}

/// Recognised characters as text extraction gives them: ligatures as their letters,
/// full-width Latin as ASCII outside CJK text, doubled quote marks as one; in code, the
/// typographic dashes and quotes the recogniser sometimes sees as the ASCII ones.
fn normalize(t: &str, cjk_line: bool, code: bool) -> String {
    let mut out = String::with_capacity(t.len());
    for c in t.chars() {
        match c {
            '\u{FB00}' => out.push_str("ff"),
            '\u{FB01}' => out.push_str("fi"),
            '\u{FB02}' => out.push_str("fl"),
            '\u{FB03}' => out.push_str("ffi"),
            '\u{FB04}' => out.push_str("ffl"),
            '\u{FB05}' | '\u{FB06}' => out.push_str("st"),
            '\u{FF01}'..='\u{FF5E}' if !cjk_line => {
                out.push(char::from_u32(c as u32 - 0xFEE0).unwrap_or(c))
            }
            '\u{3000}' if !cjk_line => out.push(' '),
            '\u{2212}' | '\u{2010}' | '\u{2011}' | '\u{2013}' if code => out.push('-'),
            '\u{2018}' | '\u{2019}' if code => out.push('\''),
            '\u{201C}' | '\u{201D}' if code => out.push('"'),
            c => out.push(c),
        }
    }
    out.replace("''", "\"").replace("``", "\"")
}

fn mostly_cjk_word(w: &Word) -> bool {
    let (mut cjk, mut all) = (0, 0);
    for c in w.text.chars() {
        all += 1;
        cjk += usize::from(is_cjk(c));
    }
    cjk * 2 > all
}

/// The character grid of monospaced text: lines start on whole cells from the leftmost and
/// are about a whole number of cells long.
struct Grid {
    /// Reading direction (unit vector) and cell width.
    dir: Point,
    pitch: f32,
    /// Start of the leftmost line along `dir`.
    origin: f32,
}

impl Grid {
    fn of(lines: &[Line]) -> Option<Grid> {
        let first = lines.iter().find_map(|l| l.words.first())?;
        let (dx, dy) = (
            first.quad[1].x - first.quad[0].x,
            first.quad[1].y - first.quad[0].y,
        );
        let len = dx.hypot(dy);
        if len <= 0.0 {
            return None;
        }
        let dir = Point::new(dx / len, dy / len);
        let along = |p: Point| p.x * dir.x + p.y * dir.y;
        // Of lines on one row (one the detector broke apart), the leftmost.
        let lines: Vec<&Line> = lines
            .iter()
            .filter(|l| {
                let Some(w) = l.words.first() else {
                    return false;
                };
                !lines.iter().any(|o| {
                    o.words
                        .first()
                        .is_some_and(|v| same_row(v, w, dir) && along(v.quad[0]) < along(w.quad[0]))
                })
            })
            .collect();
        // (start, end, characters with single spaces) of each line.
        let spans: Vec<(f32, f32, usize)> = lines
            .iter()
            .filter_map(|l| {
                let (a, b) = (l.words.first()?, l.words.last()?);
                let n = l
                    .words
                    .iter()
                    .map(|w| w.text.chars().count())
                    .sum::<usize>()
                    + l.words.len()
                    - 1;
                Some((along(a.quad[0]), along(b.quad[1]), n))
            })
            .collect();
        let mut pitches: Vec<f32> = spans
            .iter()
            .filter(|s| s.2 >= 6)
            .map(|s| (s.1 - s.0) / (s.2 as f32 + 0.2))
            .collect();
        if pitches.len() < 3 {
            return None;
        }
        pitches.sort_by(f32::total_cmp);
        let pitch = pitches[pitches.len() / 2];
        if pitch.is_nan() || pitch <= 0.0 {
            return None;
        }
        let even = pitches
            .iter()
            .filter(|p| (**p - pitch).abs() <= 0.06 * pitch)
            .count();
        if (even as f32) < 0.75 * pitches.len() as f32 {
            return None;
        }
        // The advance within words, where the characters are; lines with more than one
        // space between words make the line estimate a little wide.
        let mut advances: Vec<f32> = lines
            .iter()
            .flat_map(|l| &l.words)
            .filter_map(|w| {
                let n = w.cuts.len();
                (n >= 4).then(|| {
                    let wl = (w.quad[1].x - w.quad[0].x).hypot(w.quad[1].y - w.quad[0].y);
                    (w.cuts[n - 1].1 - w.cuts[1].1) * wl / (n - 2) as f32
                })
            })
            .collect();
        advances.sort_by(f32::total_cmp);
        let pitch = match advances.get(advances.len() / 2) {
            Some(&a) if advances.len() >= 5 && (a - pitch).abs() < 0.15 * pitch => a,
            _ => pitch,
        };
        let mut grid = Grid {
            dir,
            pitch,
            origin: 0.0,
        };
        let starts: Vec<f32> = lines
            .iter()
            .filter(|l| {
                l.words
                    .iter()
                    .map(|w| w.text.chars().count())
                    .sum::<usize>()
                    >= 2
            })
            .filter_map(|l| l.words.first())
            .map(|w| along(w.quad[0]))
            .collect();
        // The grid's phase: the mean of the line starts' positions within a cell.
        let tau = std::f32::consts::TAU;
        let (sin, cos) = starts.iter().fold((0.0, 0.0), |(s, c), x| {
            let a = x / pitch * tau;
            (s + a.sin(), c + a.cos())
        });
        let phase = sin.atan2(cos) / tau * pitch;
        let min = starts.iter().copied().fold(f32::INFINITY, f32::min);
        grid.origin = phase + ((min - phase) / pitch).round() * pitch;
        let indents: Vec<f32> = starts.iter().map(|s| (s - grid.origin) / pitch).collect();
        let indented = indents.iter().filter(|i| **i >= 1.5).count();
        let on_grid = indents
            .iter()
            .filter(|i| (**i - i.round()).abs() <= 0.35)
            .count();
        (indented >= 1 && on_grid as f32 >= 0.75 * indents.len() as f32).then_some(grid)
    }

    /// Where a word's first cell starts along the reading direction, fitted to where its
    /// characters start one cell apart (for distances between words of a line; lines
    /// start where their boxes do).
    fn start(&self, w: &Word) -> f32 {
        let along = |p: Point| p.x * self.dir.x + p.y * self.dir.y;
        let (a, b) = (along(w.quad[0]), along(w.quad[1]));
        let n = w.cuts.len();
        if n < 2 {
            return a;
        }
        let sum: f32 = w.cuts[1..]
            .iter()
            .enumerate()
            .map(|(k, c)| a + c.1 * (b - a) - (k + 1) as f32 * self.pitch)
            .sum();
        sum / (n - 1) as f32
    }

    /// The cell a line's first word starts, counted from the leftmost line.
    fn first_cell(&self, w: &Word) -> i32 {
        let s = w.quad[0].x * self.dir.x + w.quad[0].y * self.dir.y;
        ((s - self.origin) / self.pitch).round().max(0.0) as i32
    }

    /// The cell `w` starts in, after `prev` starting in cell `at`.
    fn next_cell(&self, prev: &Word, at: i32, w: &Word) -> i32 {
        at + ((self.start(w) - self.start(prev)) / self.pitch).round() as i32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of_lines(lines: &[Line]) -> String {
        lines_text(lines).text
    }

    fn word(text: &str, x: f32, y: f32, w: f32, h: f32) -> Word {
        let n = text.chars().count() as f32;
        Word {
            text: text.into(),
            quad: [
                Point::new(x, y),
                Point::new(x + w, y),
                Point::new(x + w, y + h),
                Point::new(x, y + h),
            ],
            confidence: 1.0,
            cuts: text
                .char_indices()
                .enumerate()
                .map(|(k, (b, _))| (b, k as f32 / n))
                .collect(),
        }
    }

    /// A line of monospaced words at cells `cells`, 10 points a cell.
    fn mono(y: f32, words: &[(usize, &str)]) -> Line {
        Line {
            words: words
                .iter()
                .map(|(c, t)| word(t, 20.0 + *c as f32 * 10.0, y, t.len() as f32 * 10.0, 14.0))
                .collect(),
        }
    }

    #[test]
    fn code_keeps_indentation_and_spacing() {
        let lines = [
            mono(500.0, &[(0, "fn"), (3, "main()"), (10, "{")]),
            mono(480.0, &[(4, "let"), (8, "x"), (10, "="), (12, "1;")]),
            mono(
                460.0,
                &[(4, "if"), (7, "x"), (9, ">"), (11, "0"), (13, "{")],
            ),
            mono(440.0, &[(8, "print(x);"), (20, "//"), (23, "done")]),
            mono(420.0, &[(4, "}")]),
            mono(400.0, &[(0, "}")]),
        ];
        let t = text_of_lines(&lines);
        assert_eq!(
            t,
            "fn main() {\n    let x = 1;\n    if x > 0 {\n        print(x);   // done\n    }\n}"
        );
    }

    #[test]
    fn prose_reads_as_lines_and_cjk_has_no_spaces() {
        let lines = [
            Line {
                words: vec![
                    word("Hello", 20.0, 500.0, 50.0, 14.0),
                    word("ﬁne", 76.0, 500.0, 30.0, 14.0),
                    word("world", 112.0, 500.0, 48.0, 14.0),
                ],
            },
            Line {
                words: vec![
                    word("日本", 20.0, 480.0, 28.0, 14.0),
                    word("語です", 52.0, 480.0, 42.0, 14.0),
                ],
            },
        ];
        assert_eq!(text_of_lines(&lines), "Hello fine world\n日本語です");
    }

    #[test]
    fn regions_of_pictures_and_outlines() {
        let page = Rect::new(0.0, 0.0, 600.0, 800.0);
        assert!(regions(&DisplayList::default(), page).is_empty());
        assert!(clusters(&[Rect::new(0.0, 0.0, 5.0, 8.0); 3]).is_empty());
        let glyphs: Vec<Rect> = (0..8)
            .map(|i| Rect::new(100.0 + i as f32 * 7.0, 300.0, 105.0 + i as f32 * 7.0, 310.0))
            .chain([Rect::new(400.0, 50.0, 405.0, 60.0)])
            .collect();
        let c = clusters(&glyphs);
        assert_eq!(c.len(), 1);
        assert!(c[0].x0 < 100.0 && c[0].x1 > 154.0 && c[0].x1 < 200.0);
    }
}
