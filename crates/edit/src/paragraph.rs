//! Paragraph editing: a text block read as styled paragraphs ([`block_text`]), laid out
//! again in its box ([`layout_block`]) and written back in place of its runs.

use std::ops::Range;
use std::sync::{PoisonError, RwLock};

use pagebara_content::{Matrix, Point, Rect};
use pagebara_syntax::{Document, Name, Object};
use pagebara_typeset::hyphen::Lang;
use pagebara_typeset::{Align, LineHeight, ParagraphStyle, RunFont, Synthetic};

use crate::fonts::{Fonts, GlyphPath, SpanFont};
use crate::model::{Color, ObjectId, PageObjects, Paint, RunId, TextRun};
use crate::{EncodeError, Error, Result, emit};

/// How a stretch of text looks.
#[derive(Clone, Debug, PartialEq)]
pub struct SpanStyle {
    pub font: SpanFont,
    /// Font size in page units (the size the text appears at).
    pub size: f32,
    pub fill: Paint,
    /// `Tr` as the original text had it (3: invisible, as in recognised text layers).
    pub render_mode: u8,
    /// Emulated bold and italic, for fonts without such a face.
    pub synthetic: Synthetic,
    /// Character spacing in page units.
    pub char_spacing: f32,
}

impl SpanStyle {
    /// Black text in `font` at `size`.
    pub fn new(font: SpanFont, size: f32) -> SpanStyle {
        SpanStyle {
            font,
            size,
            fill: Paint::black(),
            render_mode: 0,
            synthetic: Synthetic::default(),
            char_spacing: 0.0,
        }
    }
}

/// Text in one style.
#[derive(Clone, Debug, PartialEq)]
pub struct StyledText {
    pub text: String,
    pub style: SpanStyle,
}

/// The box a paragraph is set in. Paragraph space has x along the baseline and y up, in
/// page units; [`TextFrame::matrix`] maps it to page space.
#[derive(Clone, Debug, PartialEq)]
pub struct TextFrame {
    pub matrix: Matrix,
    /// Left edge and width of the box.
    pub left: f32,
    pub width: f32,
    /// Top edge of the box: the first baseline is one ascent below it, unless
    /// `first_baseline` places it.
    pub top: f32,
    pub first_baseline: Option<f32>,
    pub align: Align,
    pub first_indent: f32,
    /// The indent starts every paragraph, not only the first.
    pub indent_all: bool,
    /// Baseline distance as a multiple of the largest font size on a line.
    pub line_factor: f32,
    /// Extra space between paragraphs.
    pub paragraph_gap: f32,
    /// Where the original text ended (paragraph space y), for reporting overflow.
    pub bottom: f32,
    pub hyphenate: bool,
    /// The text's language (a BCP 47 tag from the PDF's `/Lang`), for hyphenation; `None`:
    /// the default language ([`set_default_language`]).
    pub lang: Option<String>,
}

impl TextFrame {
    /// A box for new text: `rect` in page space, upright as the page is displayed.
    pub fn new_box(objs: &PageObjects, rect: Rect) -> TextFrame {
        let from = objs.frame.from_view();
        let x = unit(from.apply_vector(Point::new(1.0, 0.0)));
        let y = unit(from.apply_vector(Point::new(0.0, -1.0)));
        let matrix = Matrix::new(x.x, x.y, y.x, y.y, 0.0, 0.0);
        let inv = matrix.invert().unwrap_or_default();
        let r = rect.transform(&inv);
        TextFrame {
            matrix,
            left: r.x0,
            width: r.width().max(1.0),
            top: r.y1,
            first_baseline: None,
            align: Align::Left,
            first_indent: 0.0,
            indent_all: true,
            line_factor: 1.2,
            paragraph_gap: 0.0,
            bottom: r.y0,
            hyphenate: false,
            lang: objs.lang.clone(),
        }
    }
}

fn unit(p: Point) -> Point {
    let l = p.x.hypot(p.y);
    if l > 0.0 {
        Point::new(p.x / l, p.y / l)
    } else {
        Point::new(1.0, 0.0)
    }
}

/// A text block as paragraphs to edit.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockText {
    pub block: ObjectId,
    /// The text blocks the paragraphs span, top to bottom: `block` and blocks continuing
    /// its paragraph (text extraction can split off a short last line, say).
    pub blocks: Vec<ObjectId>,
    pub frame: TextFrame,
    /// The text with its styles; paragraphs are separated by `\n`.
    pub spans: Vec<StyledText>,
    /// The block's runs in reading order.
    pub runs: Vec<RunId>,
}

impl BlockText {
    pub fn text(&self) -> String {
        self.spans.iter().map(|s| s.text.as_str()).collect()
    }
}

/// The scale of a matrix along a text space direction.
fn scale_along(m: &Matrix, x: f32, y: f32) -> f32 {
    let v = m.apply_vector(Point::new(x, y));
    v.x.hypot(v.y)
}

/// The style of a run, in page units.
pub fn run_style(r: &TextRun) -> Option<SpanStyle> {
    let f = r.font.as_ref()?;
    let m = r.matrix();
    let size = r.size * scale_along(&m, 0.0, 1.0);
    Some(SpanStyle {
        font: SpanFont::Pdf(f.object.clone()),
        size,
        fill: r.fill.clone(),
        render_mode: if r.render_mode == 2 && !r.fill.components.is_empty() {
            0
        } else {
            r.render_mode
        },
        synthetic: Synthetic {
            bold: r.render_mode == 2,
            italic: m.c.abs() > 1e-3 && (m.a * m.c + m.b * m.d).abs() > 0.05 * size,
        },
        // Tracking below a twentieth of a point is noise from the producer.
        char_spacing: (r.char_spacing * scale_along(&m, r.hscale, 0.0) * 20.0).round() / 20.0,
    })
}

#[derive(Debug)]
struct LineInfo {
    x0: f32,
    x1: f32,
    baseline: f32,
    size: f32,
    descent: f32,
    first_word: f32,
    stretched: bool,
    spans: Vec<StyledText>,
}

fn push_text(spans: &mut Vec<StyledText>, text: &str, style: &SpanStyle) {
    if text.is_empty() {
        return;
    }
    match spans.last_mut() {
        Some(s) if s.style == *style => s.text.push_str(text),
        _ => spans.push(StyledText {
            text: text.to_owned(),
            style: style.clone(),
        }),
    }
}

/// Whether a line's text ends a sentence: `.`, `!`, `?` or `:`, maybe before closing
/// quotes or brackets.
fn ends_sentence(spans: &[StyledText]) -> bool {
    let tail = spans.iter().rev().flat_map(|s| s.text.chars().rev());
    tail.filter(|c| !c.is_whitespace())
        .find(|c| !matches!(c, '"' | '\'' | ')' | ']' | '»' | '”' | '’'))
        .is_some_and(|c| matches!(c, '.' | '!' | '?' | ':' | '…' | '。' | '！' | '？'))
}

/// Reads a text block as styled paragraphs in an inferred box: alignment, width, indent and
/// leading from its lines, paragraph breaks at larger gaps or where the next line's first
/// word would have fitted and the lines say so (justified, indented, or a sentence ending
/// short), hyphens at line ends dropped where they split a word.
pub fn block_text(objs: &PageObjects, block: ObjectId) -> Result<BlockText> {
    let none = || Error::NoSuchObject(block.into());
    let tb = objs.get(block).and_then(|o| o.text()).ok_or_else(none)?;
    let first = tb
        .runs()
        .filter_map(|r| objs.run(r))
        .find(|r| r.font.is_some() && !r.glyphs.is_empty())
        .ok_or_else(none)?;
    if first.vertical {
        return Err(Error::Unsupported("vertical text"));
    }
    let m = first.matrix();
    let x = unit(m.apply_vector(Point::new(1.0, 0.0)));
    let matrix = Matrix::new(x.x, x.y, -x.y, x.x, 0.0, 0.0);
    let inv = matrix.invert().unwrap_or_default();
    let blocks = paragraph_blocks(objs, block, &inv);
    let src_lines: Vec<&crate::model::TextLine> = blocks
        .iter()
        .filter_map(|b| objs.get(*b).and_then(|o| o.text()))
        .flat_map(|t| t.lines.iter())
        .collect();
    let runs: Vec<RunId> = src_lines
        .iter()
        .flat_map(|l| l.runs.iter().copied())
        .collect();

    let mut lines: Vec<LineInfo> = Vec::new();
    for l in src_lines {
        let mut info = LineInfo {
            x0: f32::INFINITY,
            x1: f32::NEG_INFINITY,
            baseline: 0.0,
            size: 0.0,
            descent: 0.0,
            first_word: 0.0,
            stretched: false,
            spans: Vec::new(),
        };
        let mut prev_end: Option<(f32, bool)> = None;
        let mut baseline = None;
        let mut word_end: Option<f32> = None;
        let mut space_w = 0.0f32;
        let mut prev_style: Option<SpanStyle> = None;
        for &rid in &l.runs {
            let Some(r) = objs.run(rid) else { continue };
            let Some(style) = run_style(r) else { continue };
            let size = style.size;
            info.size = info.size.max(size);
            if let Some(f) = &r.font {
                info.descent = info.descent.min(f.font.descent * size);
            }
            for g in &r.glyphs {
                let p0 = inv.apply(g.quad[0]);
                let p1 = inv.apply(g.quad[1]);
                let blank = g.text.chars().all(char::is_whitespace);
                if let Some((end, was_blank)) = prev_end {
                    let gap = p0.x - end;
                    if gap > 0.2 * size && !blank && !was_blank {
                        push_text(&mut info.spans, " ", prev_style.as_ref().unwrap_or(&style));
                        if word_end.is_none() && info.x0.is_finite() {
                            word_end = Some(end);
                        }
                        space_w = space_w.max(gap);
                        if gap > 0.5 * size {
                            info.stretched = true;
                        }
                    }
                }
                if blank && !g.text.is_empty() {
                    if word_end.is_none() && info.x0.is_finite() {
                        word_end = prev_end.map(|e| e.0);
                    }
                    let w = p1.x - p0.x;
                    space_w = space_w.max(w);
                    if let Some(f) = &r.font {
                        let base = f.font.width(g.code) * r.size * r.hscale;
                        if g.advance - base > 0.05 * r.size {
                            info.stretched = true;
                        }
                    }
                } else if !g.text.is_empty() {
                    info.x0 = info.x0.min(p0.x);
                    info.x1 = info.x1.max(p1.x);
                    baseline.get_or_insert(inv.apply(g.origin).y);
                }
                push_text(&mut info.spans, &g.text, &style);
                prev_end = Some((p1.x, blank));
            }
            prev_style = Some(style);
        }
        if !info.x0.is_finite() {
            continue;
        }
        info.baseline = baseline.unwrap_or(0.0);
        let we = word_end.unwrap_or(info.x1);
        info.first_word = we - info.x0
            + if space_w > 0.0 {
                space_w
            } else {
                0.25 * info.size
            };
        lines.push(info);
    }
    if lines.is_empty() {
        return Err(none());
    }
    let samples = |ls: &[&LineInfo]| -> Vec<pagebara_typeset::LineSample> {
        ls.iter()
            .map(|l| pagebara_typeset::LineSample {
                x0: l.x0,
                x1: l.x1,
                baseline: l.baseline,
                size: l.size,
                first_word: l.first_word,
                stretched: l.stretched,
            })
            .collect()
    };
    let all: Vec<&LineInfo> = lines.iter().collect();
    let p1 = pagebara_typeset::infer_paragraph(&samples(&all)).ok_or_else(none)?;
    let n = lines.len();
    let median_size = {
        let mut v: Vec<f32> = lines.iter().map(|l| l.size).collect();
        v.sort_by(f32::total_cmp);
        v[v.len() / 2]
    };
    let tol = (median_size * 0.1).max(0.5);
    // Paragraph breaks after line i: a larger gap, or room left for the next line's first
    // word. Ragged lines often end early anyway (a box narrower once, a manual break), so
    // unless the text is justified that room counts only with more evidence: the next line
    // indented, or this one ending a sentence well short of the edge.
    let mut breaks = vec![false; n];
    for i in 0..n.saturating_sub(1) {
        let (a, b) = (&lines[i], &lines[i + 1]);
        let gap = a.baseline - b.baseline;
        let slack = match p1.align {
            Align::Left | Align::Justify => p1.left + p1.width - a.x1,
            _ => p1.width - (a.x1 - a.x0),
        };
        let fits = slack - b.first_word > tol;
        let paragraph_end = match p1.align {
            Align::Justify => true,
            _ => {
                let indented =
                    matches!(p1.align, Align::Left) && b.x0 - p1.left > tol.max(0.4 * b.size);
                let short = slack - b.first_word > 2.0 * a.size;
                indented || short && ends_sentence(&a.spans)
            }
        };
        breaks[i] = gap > 1.4 * p1.line_height || gap < 0.0 || (n > 2 && fits && paragraph_end);
    }
    // The paragraph with the most lines gives the box.
    let mut paras: Vec<Range<usize>> = Vec::new();
    let mut s = 0;
    for (i, &b) in breaks.iter().enumerate() {
        if b || i + 1 == n {
            paras.push(s..i + 1);
            s = i + 1;
        }
    }
    let rep = paras
        .iter()
        .max_by_key(|r| (r.len(), std::cmp::Reverse(r.start)))
        .cloned()
        .unwrap_or(0..n);
    let wraps = rep.len() >= 2;
    let rep_lines: Vec<&LineInfo> = lines[rep.clone()].iter().collect();
    let inferred = pagebara_typeset::infer_paragraph(&samples(&rep_lines)).ok_or_else(none)?;
    let line_height = if n >= 2 {
        p1.line_height
    } else {
        1.2 * median_size
    };

    // Page bounds in paragraph space, for boxes that do not wrap.
    let page = objs.frame.crop_box.transform(&inv);
    let (left, width, align) = if wraps {
        (inferred.left, inferred.width, inferred.align)
    } else {
        let x0 = lines.iter().map(|l| l.x0).fold(f32::INFINITY, f32::min);
        let x1 = lines.iter().map(|l| l.x1).fold(f32::NEG_INFINITY, f32::max);
        let centre = (lines[0].x0 + lines[0].x1) / 2.0;
        let near = |a: f32, b: f32| (a - b).abs() <= tol;
        let align = if n >= 2 && lines.iter().all(|l| near((l.x0 + l.x1) / 2.0, centre)) {
            if lines.iter().all(|l| near(l.x0, x0)) {
                Align::Left
            } else {
                Align::Center
            }
        } else if n >= 2
            && lines.iter().all(|l| near(l.x1, x1))
            && !lines.iter().all(|l| near(l.x0, x0))
        {
            Align::Right
        } else if n == 1 && near(centre, (page.x0 + page.x1) / 2.0) && x0 > page.x0 + 72.0 {
            Align::Center
        } else {
            Align::Left
        };
        match align {
            Align::Center => {
                let half = (centre - page.x0)
                    .min(page.x1 - centre)
                    .max((x1 - x0) / 2.0);
                (centre - half, 2.0 * half, align)
            }
            Align::Right => {
                let l = page.x0.min(x0);
                (l, x1 - l, align)
            }
            _ => (x0, (page.x1 - x0).max(x1 - x0), Align::Left),
        }
    };
    let first_indent = if wraps && matches!(align, Align::Left | Align::Justify) {
        lines[0].x0 - left
    } else {
        0.0
    };
    let indent_all = (1..n)
        .filter(|&i| breaks[i - 1])
        .all(|i| (lines[i].x0 - left - first_indent).abs() <= tol);
    let gaps: Vec<f32> = (0..n.saturating_sub(1))
        .filter(|&i| breaks[i])
        .map(|i| lines[i].baseline - lines[i + 1].baseline - line_height)
        .filter(|g| *g > 0.2 * line_height)
        .collect();
    let paragraph_gap = if gaps.is_empty() {
        0.0
    } else {
        let mut g = gaps;
        g.sort_by(f32::total_cmp);
        g[g.len() / 2]
    };

    // The text: lines joined by spaces, paragraphs by newlines.
    let lang = text_lang(objs, &runs);
    let hy = hyphen_lang(lang.as_deref()).map(Lang::hyphenator);
    let mut spans: Vec<StyledText> = Vec::new();
    let mut hyphenated = false;
    for i in 0..n {
        let mut ls = lines[i].spans.clone();
        // Trailing blanks go: the join decides what separates lines.
        while let Some(last) = ls.last_mut() {
            let t = last.text.trim_end().len();
            last.text.truncate(t);
            if last.text.is_empty() {
                ls.pop();
            } else {
                break;
            }
        }
        if let Some(f) = ls.first_mut() {
            let t = f.text.trim_start().to_owned();
            f.text = t;
        }
        if i + 1 < n && !breaks[i] {
            let next: String = lines[i + 1]
                .spans
                .iter()
                .map(|s| s.text.as_str())
                .collect::<String>()
                .trim_start()
                .to_owned();
            let cur: String = ls.iter().map(|s| s.text.as_str()).collect();
            let joined_word = hy.is_some_and(|h| soft_hyphen(&cur, &next, h));
            let last_style = ls.last().map(|s| s.style.clone());
            if joined_word {
                if let Some(l) = ls.last_mut() {
                    l.text.pop();
                }
                hyphenated = true;
            } else if let Some(st) = last_style {
                let cjk = |c: Option<char>| c.is_some_and(is_cjk);
                let hard_hyphen = cur.ends_with('-')
                    && cur[..cur.len() - 1]
                        .chars()
                        .next_back()
                        .is_some_and(char::is_alphabetic)
                    && next.chars().next().is_some_and(char::is_alphabetic);
                if !(hard_hyphen || cjk(cur.chars().next_back()) || cjk(next.chars().next())) {
                    push_text(&mut ls, " ", &st);
                }
            }
        } else if i + 1 < n
            && let Some(st) = ls.last().map(|s| s.style.clone())
        {
            push_text(&mut ls, "\n", &st);
        }
        for s in ls {
            push_text(&mut spans, &s.text, &s.style);
        }
    }
    let last = &lines[n - 1];
    let frame = TextFrame {
        matrix,
        left,
        width: width.max(1.0),
        top: lines[0].baseline + lines[0].size * 0.8,
        first_baseline: Some(lines[0].baseline),
        align,
        first_indent,
        indent_all,
        line_factor: (line_height / median_size).clamp(0.5, 5.0),
        paragraph_gap,
        bottom: last.baseline + last.descent.min(-0.2 * last.size),
        hyphenate: hyphenated,
        lang,
    };
    Ok(BlockText {
        block,
        blocks,
        frame,
        spans,
        runs,
    })
}

/// Baseline, left and right edge (paragraph space) and size of a line's first glyph.
fn line_geom(
    objs: &PageObjects,
    l: &crate::model::TextLine,
    inv: &Matrix,
) -> Option<(f32, f32, f32, f32)> {
    let r = l
        .runs
        .iter()
        .filter_map(|r| objs.run(*r))
        .find(|r| !r.glyphs.is_empty())?;
    let size = run_style(r)?.size;
    let b = inv.apply(r.glyphs[0].origin).y;
    let bb = l.bbox.transform(inv);
    Some((b, bb.x0, bb.x1, size))
}

/// `block` with the text blocks above and below that continue its paragraph: the next
/// line one line height away, in the same size, overlapping horizontally.
fn paragraph_blocks(objs: &PageObjects, block: ObjectId, inv: &Matrix) -> Vec<ObjectId> {
    let geom = |b: ObjectId| -> Option<Vec<(f32, f32, f32, f32)>> {
        let o = objs.get(b)?;
        if o.hidden {
            return None;
        }
        let g: Vec<_> = o
            .text()?
            .lines
            .iter()
            .filter_map(|l| line_geom(objs, l, inv))
            .collect();
        (!g.is_empty()).then_some(g)
    };
    let Some(me) = geom(block) else {
        return vec![block];
    };
    let parent = objs.get(block).and_then(|o| o.parent);
    let gap_of = |g: &[(f32, f32, f32, f32)]| (g.len() >= 2).then(|| (g[0].0 - g[1].0).abs());
    let mut out = vec![block];
    let (mut top, mut bottom) = (me.clone(), me);
    let mut step = gap_of(&top);
    for down in [true, false] {
        loop {
            let edge = if down {
                *bottom.last().expect("line")
            } else {
                top[0]
            };
            let lh = step.unwrap_or(1.2 * edge.3);
            let next = objs.objects.iter().find(|o| {
                o.kind == crate::Kind::Text
                    && o.parent == parent
                    && !out.contains(&o.id)
                    && geom(o.id).is_some_and(|g| {
                        let n = if down { g[0] } else { *g.last().expect("line") };
                        let d = if down { edge.0 - n.0 } else { n.0 - edge.0 };
                        let ok_gap = match step {
                            Some(s) => (d - s).abs() < 0.15 * edge.3,
                            None => d > 0.9 * edge.3 && d < 1.6 * edge.3,
                        };
                        ok_gap
                            && (n.3 - edge.3).abs() < 0.05 * edge.3
                            && n.1 < edge.2
                            && n.2 > edge.1
                            && gap_of(&g).is_none_or(|s| (s - lh).abs() < 0.15 * edge.3)
                    })
            });
            let Some(n) = next else { break };
            let g = geom(n.id).expect("geometry");
            step = step.or_else(|| {
                let e = if down {
                    bottom.last().expect("line").0
                } else {
                    top[0].0
                };
                let f = if down {
                    g[0].0
                } else {
                    g.last().expect("line").0
                };
                Some((e - f).abs())
            });
            if down {
                bottom = g;
                out.push(n.id);
            } else {
                top = g;
                out.insert(0, n.id);
            }
        }
    }
    out
}

static DEFAULT_LANG: RwLock<Option<String>> = RwLock::new(None);

/// Sets the language assumed for text the document gives none for (a BCP 47 tag, such as
/// the app's UI language); until set, English.
pub fn set_default_language(tag: Option<&str>) {
    *DEFAULT_LANG.write().unwrap_or_else(PoisonError::into_inner) = tag.map(str::to_owned);
}

/// The patterns for text in `lang`, or in the default language; `None` for languages
/// without them.
fn hyphen_lang(lang: Option<&str>) -> Option<Lang> {
    if let Some(t) = lang {
        return Lang::from_tag(t);
    }
    match &*DEFAULT_LANG.read().unwrap_or_else(PoisonError::into_inner) {
        Some(t) => Lang::from_tag(t),
        None => Some(Lang::EnUs),
    }
}

/// The language of a block's text: the innermost `/Lang` its first run's marked content
/// gives (a property, or the structure element's), else the document's.
fn text_lang(objs: &PageObjects, runs: &[RunId]) -> Option<String> {
    let run = runs
        .iter()
        .filter_map(|&r| objs.run(r))
        .find(|r| !r.glyphs.is_empty());
    run.and_then(|r| {
        r.marked.iter().rev().find_map(|m| {
            let prop = m.properties.as_ref().and_then(|p| p.get(b"Lang"));
            prop.and_then(Object::as_string)
                .and_then(crate::tagged::lang_string)
                .or_else(|| m.mcid.and_then(|id| objs.mcid_lang.get(&id).cloned()))
        })
    })
    .or_else(|| objs.lang.clone())
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32, 0x2E80..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0xFF00..=0xFFEF)
}

/// Whether a line ending in `-` splits a word the hyphenation patterns would split there.
fn soft_hyphen(cur: &str, next: &str, hy: &pagebara_typeset::hyphen::Hyphenator) -> bool {
    let Some(stem) = cur.strip_suffix('-') else {
        return false;
    };
    let a: String = stem
        .chars()
        .rev()
        .take_while(|c| c.is_alphabetic())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    let b: String = next.chars().take_while(|c| c.is_alphabetic()).collect();
    if a.chars().count() < 2 || b.is_empty() || !b.chars().next().is_some_and(char::is_lowercase) {
        return false;
    }
    let word = format!("{a}{b}");
    hy.hyphenate(&word).contains(&a.len())
}

/// A laid-out glyph.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutGlyph {
    /// Byte range of the text it shows (empty for an inserted hyphen).
    pub cluster: Range<usize>,
    /// Origin along the line and advance, in paragraph space.
    pub x: f32,
    pub advance: f32,
    /// Index of its span in the laid-out spans.
    pub span: usize,
    pub font: RunFont,
    pub code: u32,
    pub size: f32,
    /// Its box in page space: origin, along the advance, up to the ascent.
    pub quad: [Point; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct LayoutLine {
    /// Byte range of the text on the line (without the paragraph's newline).
    pub range: Range<usize>,
    /// Baseline in paragraph space.
    pub baseline: f32,
    /// Ascent and descent (negative) in page units.
    pub ascent: f32,
    pub descent: f32,
    /// Where the line's text starts and ends along it.
    pub x0: f32,
    pub x1: f32,
    /// The width of a space at the line's end, for carets after trailing spaces.
    pub space: f32,
    pub glyphs: Vec<LayoutGlyph>,
}

/// A caret: the line from the descent to the ascent at a text position, in page space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caret {
    pub bottom: Point,
    pub top: Point,
}

/// A paragraph's text laid out in its frame.
#[derive(Clone, Debug)]
pub struct BlockLayout {
    pub text: String,
    pub spans: Vec<StyledText>,
    pub frame: TextFrame,
    pub lines: Vec<LayoutLine>,
    /// Bounds of the lines in page space.
    pub bbox: Rect,
    /// How far the text runs below the original text (page units, 0 when it does not).
    pub overflow: f32,
    /// A word was too wide for the box.
    pub overfull: bool,
    /// Characters no font could show (byte offset, character).
    pub missing: Vec<(usize, char)>,
    paras: Vec<Para>,
}

#[derive(Clone, Debug)]
struct Para {
    layout: pagebara_typeset::Layout,
    /// Index of its first span in the block's spans, and per typeset span, the block span.
    span_map: Vec<usize>,
    /// Baseline of its first line in paragraph space.
    y0: f32,
}

/// Lays out `spans` (paragraphs separated by `\n`) in `frame`.
pub fn layout_block(
    doc: &Document,
    fonts: &mut Fonts,
    frame: &TextFrame,
    spans: &[StyledText],
) -> Result<BlockLayout> {
    let spans = merge(spans);
    let text: String = spans.iter().map(|s| s.text.as_str()).collect();
    // Split into paragraphs: per paragraph, (start offset, [(block span, text)]).
    let mut paras_src: Vec<(usize, Vec<(usize, String)>)> = vec![(0, Vec::new())];
    let mut off = 0;
    for (si, s) in spans.iter().enumerate() {
        for (k, part) in s.text.split('\n').enumerate() {
            if k > 0 {
                off += 1;
                paras_src.push((off, Vec::new()));
            }
            let cur = paras_src.last_mut().expect("paragraph");
            if !part.is_empty() || cur.1.is_empty() {
                cur.1.push((si, part.to_owned()));
            }
            off += part.len();
        }
    }
    let mut ids = Vec::with_capacity(spans.len());
    for s in &spans {
        ids.push(fonts.open(doc, &s.style.font)?);
    }
    if spans.is_empty() {
        return Err(Error::Unsupported("no text style"));
    }
    let mut style = ParagraphStyle {
        width: frame.width,
        align: frame.align,
        line_height: LineHeight::Factor(frame.line_factor),
        first_indent: frame.first_indent,
        hyphenate: frame
            .hyphenate
            .then(|| hyphen_lang(frame.lang.as_deref()))
            .flatten(),
        max_char_spacing: 0.0,
        max_word_stretch: 1.0,
        rtl: None,
    };
    let mut paras = Vec::new();
    let mut lines = Vec::new();
    let mut missing = Vec::new();
    let mut overfull = false;
    let mut y = 0.0f32;
    for (pi, (start, parts)) in paras_src.into_iter().enumerate() {
        let parts = if parts.is_empty() {
            // An empty paragraph after a newline: the newline's style.
            let si = spans
                .iter()
                .scan(0usize, |o, s| {
                    *o += s.text.len();
                    Some(*o)
                })
                .position(|end| end >= start)
                .unwrap_or(spans.len() - 1);
            vec![(si, String::new())]
        } else {
            parts
        };
        let tspans: Vec<pagebara_typeset::Span> = parts
            .iter()
            .map(|(si, t)| {
                let st = &spans[*si].style;
                pagebara_typeset::Span {
                    text: t.clone(),
                    font: ids[*si],
                    size: st.size,
                    color: pagebara_typeset::Color::default(),
                    char_spacing: st.char_spacing,
                    synthetic: st.synthetic,
                }
            })
            .collect();
        let span_map: Vec<usize> = parts.iter().map(|p| p.0).collect();
        style.first_indent = if pi == 0 || frame.indent_all {
            frame.first_indent
        } else {
            0.0
        };
        let lay = pagebara_typeset::layout(fonts.session(), &tspans, &style);
        for w in &lay.warnings {
            match w {
                pagebara_typeset::LayoutWarning::Missing(o, c) => missing.push((start + o, *c)),
                pagebara_typeset::LayoutWarning::Overfull { .. } => overfull = true,
                _ => {}
            }
        }
        let size_of = |l: &pagebara_typeset::Line| {
            l.fragments
                .iter()
                .map(|f| f.size)
                .fold(0.0f32, f32::max)
                .max(if l.fragments.is_empty() {
                    tspans[0].size
                } else {
                    0.0
                })
        };
        let first_line = lay.lines.first();
        let y0 = if pi == 0 {
            match frame.first_baseline {
                Some(b) => b,
                None => frame.top - first_line.map_or(0.0, |l| l.ascent),
            }
        } else {
            y - frame.line_factor * first_line.map_or(tspans[0].size, size_of) - frame.paragraph_gap
        };
        let base0 = first_line.map_or(0.0, |l| l.baseline);
        for l in &lay.lines {
            let baseline = y0 - (l.baseline - base0);
            let mut glyphs = Vec::new();
            for f in &l.fragments {
                let si = span_map[f.span];
                let (asc, desc) = fonts.session().metrics(ids[si]);
                for g in &f.glyphs {
                    let x = frame.left + g.x;
                    let q = [
                        Point::new(x, baseline + desc * f.size),
                        Point::new(x + g.advance, baseline + desc * f.size),
                        Point::new(x + g.advance, baseline + asc * f.size),
                        Point::new(x, baseline + asc * f.size),
                    ];
                    glyphs.push(LayoutGlyph {
                        cluster: start + g.glyph.cluster.start..start + g.glyph.cluster.end,
                        x,
                        advance: g.advance,
                        span: si,
                        font: f.font,
                        code: g.glyph.code,
                        size: f.size,
                        quad: q.map(|p| frame.matrix.apply(p)),
                    });
                }
            }
            let last_span = l
                .fragments
                .last()
                .map(|f| span_map[f.span])
                .unwrap_or(span_map[0]);
            let space = fonts.space_width(ids[last_span]) * spans[last_span].style.size;
            lines.push(LayoutLine {
                range: start + l.range.start..start + l.range.end,
                baseline,
                ascent: l.ascent,
                descent: l.descent,
                x0: frame.left + l.x,
                x1: frame.left + l.x + l.width,
                space,
                glyphs,
            });
            y = baseline;
        }
        paras.push(Para {
            layout: lay,

            span_map,
            y0,
        });
    }
    let mut corners = Vec::new();
    for l in &lines {
        for (x, yy) in [
            (l.x0, l.baseline + l.descent),
            (l.x1, l.baseline + l.ascent),
        ] {
            corners.push(frame.matrix.apply(Point::new(x, yy)));
        }
    }
    let bbox = if corners.is_empty() {
        Rect::default()
    } else {
        Rect::from_points(&corners)
    };
    let end = lines
        .last()
        .map_or(frame.bottom, |l| l.baseline + l.descent);
    Ok(BlockLayout {
        text,
        spans,
        frame: frame.clone(),
        lines,
        bbox,
        overflow: (frame.bottom - end).max(0.0),
        overfull,
        missing,
        paras,
    })
}

/// Adjacent spans of the same style joined, empty ones dropped (one kept for its style).
pub fn merge(spans: &[StyledText]) -> Vec<StyledText> {
    let mut out: Vec<StyledText> = Vec::new();
    for s in spans {
        push_text(&mut out, &s.text, &s.style);
    }
    if out.is_empty()
        && let Some(s) = spans.first()
    {
        out.push(StyledText {
            text: String::new(),
            style: s.style.clone(),
        });
    }
    out
}

impl BlockLayout {
    /// The line showing a text offset (at a wrap, the line it starts).
    pub fn line_of(&self, offset: usize) -> usize {
        let n = self.lines.len();
        for (i, l) in self.lines.iter().enumerate() {
            let next_starts = self.lines.get(i + 1).map(|x| x.range.start);
            let ends_here = offset < l.range.end
                || (offset == l.range.end && next_starts != Some(offset))
                || i + 1 == n;
            if ends_here && (offset >= l.range.start || i == 0) {
                return i;
            }
        }
        n.saturating_sub(1)
    }

    /// The position along its line of a text offset, in paragraph space.
    pub fn x_of(&self, offset: usize) -> f32 {
        let Some(l) = self.lines.get(self.line_of(offset)) else {
            return self.frame.left;
        };
        if let Some(g) = l.glyphs.iter().find(|g| g.cluster.start >= offset) {
            // Offsets inside a cluster go to its start.
            return g.x;
        }
        let last_end = l
            .glyphs
            .iter()
            .map(|g| g.cluster.end)
            .max()
            .unwrap_or(l.range.start);
        let trailing = self.text[last_end.min(offset)..offset]
            .chars()
            .filter(|c| *c != '\n')
            .count();
        l.x1 + trailing as f32 * l.space
    }

    /// The caret at a text offset.
    pub fn caret(&self, offset: usize) -> Caret {
        let Some(l) = self.lines.get(self.line_of(offset)) else {
            let p = self
                .frame
                .matrix
                .apply(Point::new(self.frame.left, self.frame.top));
            return Caret { bottom: p, top: p };
        };
        let x = self.x_of(offset);
        let m = &self.frame.matrix;
        Caret {
            bottom: m.apply(Point::new(x, l.baseline + l.descent)),
            top: m.apply(Point::new(x, l.baseline + l.ascent)),
        }
    }

    /// The text offset nearest to a page-space point.
    pub fn offset_at(&self, p: Point) -> usize {
        let inv = self.frame.matrix.invert().unwrap_or_default();
        let q = inv.apply(p);
        let Some(li) = self
            .lines
            .iter()
            .enumerate()
            .min_by(|a, b| {
                let d = |l: &LayoutLine| {
                    if q.y > l.baseline + l.ascent {
                        q.y - l.baseline - l.ascent
                    } else if q.y < l.baseline + l.descent {
                        l.baseline + l.descent - q.y
                    } else {
                        0.0
                    }
                };
                d(a.1).total_cmp(&d(b.1))
            })
            .map(|x| x.0)
        else {
            return 0;
        };
        self.offset_in_line(li, q.x)
    }

    /// The offset on line `li` nearest to `x` (paragraph space).
    pub fn offset_in_line(&self, li: usize, x: f32) -> usize {
        let l = &self.lines[li];
        let mut best = (f32::INFINITY, l.range.start);
        let mut consider = |o: usize, bx: f32| {
            let d = (bx - x).abs();
            if d < best.0 {
                best = (d, o);
            }
        };
        consider(l.range.start, self.x_of(l.range.start));
        for g in &l.glyphs {
            if !g.cluster.is_empty() {
                consider(g.cluster.start, g.x);
                consider(g.cluster.end, g.x + g.advance);
            }
        }
        // After trailing spaces, up to the line's end.
        let mut end = l.range.end;
        if self.lines.get(li + 1).is_some_and(|n| n.range.start == end) && end > l.range.start {
            // A wrapped line: its end belongs to the next line.
            end = self.text[..end]
                .char_indices()
                .next_back()
                .map_or(end, |(i, _)| i.max(l.range.start));
        }
        let last = l
            .glyphs
            .iter()
            .map(|g| g.cluster.end)
            .max()
            .unwrap_or(l.range.start);
        let mut o = last;
        while o < end {
            let c = self.text[o..].chars().next().map_or(1, char::len_utf8);
            o += c;
            consider(o, self.x_of(o));
        }
        best.1
    }

    /// Rectangles (page-space quads) covering a text range, one per line.
    pub fn range_quads(&self, r: Range<usize>) -> Vec<[Point; 4]> {
        let mut out = Vec::new();
        if r.is_empty() {
            return out;
        }
        let m = &self.frame.matrix;
        for (i, l) in self.lines.iter().enumerate() {
            let s = r.start.max(l.range.start);
            let e = r.end.min(l.range.end);
            if s >= e && !(s == e && r.contains(&l.range.start) && l.range.is_empty()) {
                continue;
            }
            let x0 = if s == l.range.start {
                l.x0.min(self.x_of(s))
            } else {
                self.x_of(s)
            };
            let x1 =
                if e >= l.range.end && self.lines.get(i + 1).is_some_and(|n| n.range.start == e) {
                    l.x1.max(x0 + l.space * 0.5)
                } else {
                    self.x_of(e)
                        .max(x0 + l.space * 0.5 * f32::from(u8::from(l.range.is_empty())))
                };
            let (y0, y1) = (l.baseline + l.descent, l.baseline + l.ascent);
            out.push([
                m.apply(Point::new(x0, y0)),
                m.apply(Point::new(x1, y0)),
                m.apply(Point::new(x1, y1)),
                m.apply(Point::new(x0, y1)),
            ]);
        }
        out
    }

    /// The glyph outlines in page space, with the fill of their span: for drawing the
    /// paragraph while it is edited.
    pub fn outlines(&self, fonts: &mut Fonts) -> Vec<PreviewGlyph> {
        let mut out = Vec::new();
        for l in &self.lines {
            for g in &l.glyphs {
                let Some(path) = fonts.outline(g.font, g.code) else {
                    continue;
                };
                let st = &self.spans[g.span].style;
                let skew = if st.synthetic.italic {
                    Synthetic::SKEW
                } else {
                    0.0
                };
                let m = Matrix::new(g.size, 0.0, skew * g.size, g.size, g.x, l.baseline)
                    .then(&self.frame.matrix);
                out.push(PreviewGlyph {
                    path,
                    matrix: m,
                    fill: st.fill.rgb().unwrap_or([0.0; 3]),
                    bold: st.synthetic.bold,
                    invisible: matches!(st.render_mode, 3 | 7),
                });
            }
        }
        out
    }

    /// Content operators drawing the layout inside a text object where the CTM is `ctm`,
    /// starting from the text state `from` and leaving `from` as it was. `names` gives the
    /// resource name of each run font.
    pub(crate) fn write_ops(
        &self,
        ctm: &Matrix,
        from: &TextState,
        names: &dyn Fn(RunFont) -> Vec<u8>,
    ) -> Result<Vec<u8>> {
        let inv = ctm.invert().ok_or(Error::Unsupported("singular matrix"))?;
        let mut out = Vec::new();
        let mut st = from.clone();
        let mut fill: Option<Paint> = None;
        let mut stroke: Option<Paint> = None;
        let mut width: Option<f32> = None;
        if (from.th - 1.0).abs() > 1e-6 {
            out.extend_from_slice(b"100 Tz\n");
            st.th = 1.0;
        }
        if from.rise != 0.0 {
            out.extend_from_slice(b"0 Ts\n");
            st.rise = 0.0;
        }
        let mut font: Option<(RunFont, u32)> = None;
        for p in &self.paras {
            let base0 = p.layout.lines.first().map_or(0.0, |l| l.baseline);
            for line in &p.layout.lines {
                let baseline = p.y0 - (line.baseline - base0);
                for f in &line.fragments {
                    let style = &self.spans[p.span_map[f.span]].style;
                    let key = (f.font, f.size.to_bits());
                    if font != Some(key) {
                        emit::name(&mut out, &names(f.font));
                        out.push(b' ');
                        emit::nums(&mut out, &[f.size.into()]);
                        out.extend_from_slice(b"Tf\n");
                        font = Some(key);
                    }
                    if fill.as_ref() != Some(&style.fill) {
                        paint_ops(&mut out, &style.fill, false);
                        fill = Some(style.fill.clone());
                    }
                    let mode = if matches!(style.render_mode, 3 | 7) {
                        style.render_mode
                    } else if style.synthetic.bold {
                        2
                    } else {
                        style.render_mode
                    };
                    if mode == 2 && style.synthetic.bold {
                        if stroke.as_ref() != Some(&style.fill) {
                            paint_ops(&mut out, &style.fill, true);
                            stroke = Some(style.fill.clone());
                        }
                        let w = style.synthetic.stroke_width(f.size);
                        if width != Some(w) {
                            emit::nums(&mut out, &[w.into()]);
                            out.extend_from_slice(b"w\n");
                            width = Some(w);
                        }
                    }
                    if mode != st.mode {
                        emit::nums(&mut out, &[mode.into()]);
                        out.extend_from_slice(b"Tr\n");
                        st.mode = mode;
                    }
                    if (f.char_spacing - st.tc).abs() > 1e-6 {
                        emit::nums(&mut out, &[f.char_spacing.into()]);
                        out.extend_from_slice(b"Tc\n");
                        st.tc = f.char_spacing;
                    }
                    if (f.word_spacing - st.tw).abs() > 1e-6 {
                        emit::nums(&mut out, &[f.word_spacing.into()]);
                        out.extend_from_slice(b"Tw\n");
                        st.tw = f.word_spacing;
                    }
                    let tm = style.synthetic.text_matrix(self.frame.left + f.x, baseline);
                    let m = Matrix::from_array(tm).then(&self.frame.matrix).then(&inv);
                    emit::tm(&mut out, &m);
                    out.extend_from_slice(f.tj_operand().as_bytes());
                    out.extend_from_slice(b" TJ\n");
                }
            }
        }
        // Back to the state as it was.
        if font.is_some() {
            out.extend_from_slice(&from.font_op);
        }
        if fill.is_some() {
            out.extend_from_slice(&from.fill_ops);
        }
        if stroke.is_some() {
            out.extend_from_slice(&from.stroke_ops);
        }
        if width.is_some() {
            emit::nums(&mut out, &[from.line_width.into()]);
            out.extend_from_slice(b"w\n");
        }
        let restore = [
            (st.tc != from.tc, from.tc, &b"Tc\n"[..]),
            (st.tw != from.tw, from.tw, b"Tw\n"),
            (st.th != from.th, from.th * 100.0, b"Tz\n"),
            (st.rise != from.rise, from.rise, b"Ts\n"),
            (st.mode != from.mode, f32::from(from.mode), b"Tr\n"),
        ];
        for (changed, v, op) in restore {
            if changed {
                emit::nums(&mut out, &[v.into()]);
                out.extend_from_slice(op);
            }
        }
        Ok(out)
    }
}

/// A glyph to draw for a preview: its outline (text space per unit size), the matrix to
/// page space, and how it is painted.
#[derive(Clone, Debug)]
pub struct PreviewGlyph {
    pub path: std::sync::Arc<GlyphPath>,
    pub matrix: Matrix,
    pub fill: [f32; 3],
    /// Emulated bold: stroke the outline too.
    pub bold: bool,
    /// Invisible text (a recognised text layer).
    pub invisible: bool,
}

/// The text state around written text, to come back to.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct TextState {
    pub tc: f32,
    pub tw: f32,
    pub th: f32,
    pub rise: f32,
    pub mode: u8,
    pub line_width: f32,
    /// Operators selecting the font and size again (empty for none).
    pub font_op: Vec<u8>,
    pub fill_ops: Vec<u8>,
    pub stroke_ops: Vec<u8>,
}

impl Default for TextState {
    fn default() -> Self {
        TextState {
            tc: 0.0,
            tw: 0.0,
            th: 1.0,
            rise: 0.0,
            mode: 0,
            line_width: 1.0,
            font_op: Vec::new(),
            fill_ops: b"0 g\n".to_vec(),
            stroke_ops: b"0 G\n".to_vec(),
        }
    }
}

/// Operators setting a colour as a [`Paint`] describes it.
pub(crate) fn paint_ops(out: &mut Vec<u8>, p: &Paint, stroke: bool) {
    let c = &p.components;
    let dev = |out: &mut Vec<u8>, ops: [&[u8]; 2]| {
        let v: Vec<f64> = c.iter().map(|&x| f64::from(x)).collect();
        emit::nums(out, &v);
        out.extend_from_slice(ops[usize::from(stroke)]);
        out.push(b'\n');
    };
    match (p.space.as_bytes(), c.len(), &p.pattern) {
        (b"DeviceGray", 1, None) => dev(out, [b"g", b"G"]),
        (b"DeviceRGB", 3, None) => dev(out, [b"rg", b"RG"]),
        (b"DeviceCMYK", 4, None) => dev(out, [b"k", b"K"]),
        (space, _, pattern) => {
            emit::name(out, space);
            out.extend_from_slice(if stroke { b" CS\n" } else { b" cs\n" });
            let v: Vec<f64> = c.iter().map(|&x| f64::from(x)).collect();
            emit::nums(out, &v);
            if let Some(n) = pattern {
                emit::name(out, n.as_bytes());
                out.push(b' ');
            }
            out.extend_from_slice(if stroke { b"SCN\n" } else { b"scn\n" });
        }
    }
}

/// A device colour as a [`Paint`].
pub fn paint_of(c: Color) -> Paint {
    let (space, components): (&[u8], Vec<f32>) = match c {
        Color::Gray(g) => (b"DeviceGray", vec![g]),
        Color::Rgb(r, g, b) => (b"DeviceRGB", vec![r, g, b]),
        Color::Cmyk(c, m, y, k) => (b"DeviceCMYK", vec![c, m, y, k]),
    };
    Paint {
        space: Name::new(space),
        components,
        pattern: None,
    }
}

/// What a laid-out block edit did.
#[derive(Clone, Debug, PartialEq)]
pub struct BlockReport {
    /// Bounds of the new text in page space.
    pub bbox: Rect,
    pub lines: usize,
    /// How far the text now runs below where the original ended (page units).
    pub overflow: f32,
    /// A word did not fit the box's width.
    pub overfull: bool,
    /// Objects drawn where the text grew (overlapping it now).
    pub overlaps: Vec<ObjectId>,
}

impl From<&EncodeError> for Error {
    fn from(e: &EncodeError) -> Self {
        Error::Encode(e.clone())
    }
}
