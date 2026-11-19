//! Paragraph layout: styled spans broken into lines, aligned or justified, with glyph
//! positions ready for a content stream.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::ops::Range;

use unicode_bidi::BidiInfo;
use unicode_normalization::char::is_combining_mark;

use crate::encoded::{Encoded, EncodedGlyph, FontId, RunFont, fmt_num, tj_array};
use crate::hyphen;
use crate::linebreak::{self, Break};
use crate::session::FontSession;
use crate::style::Synthetic;

/// Encodes text for layout. [`FontSession`] implements it; tests and simple callers can
/// implement it over fixed metrics.
pub trait Shaper {
    /// Encodes `text` in `font` (see [`FontSession::encode`]).
    fn shape(&mut self, font: FontId, text: &str) -> Encoded;

    /// Ascent and descent in text space per unit size.
    fn metrics(&mut self, _font: FontId) -> (f32, f32) {
        (0.8, -0.2)
    }
}

impl Shaper for FontSession {
    fn shape(&mut self, font: FontId, text: &str) -> Encoded {
        self.encode(font, text)
    }

    fn metrics(&mut self, font: FontId) -> (f32, f32) {
        FontSession::metrics(self, font)
    }
}

/// A fill colour.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Color {
    Gray(f32),
    Rgb([f32; 3]),
    Cmyk([f32; 4]),
}

impl Default for Color {
    fn default() -> Self {
        Color::Gray(0.0)
    }
}

/// A run of text in one style.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub text: String,
    pub font: FontId,
    /// Font size in points.
    pub size: f32,
    pub color: Color,
    /// Character spacing (`Tc`) in points.
    pub char_spacing: f32,
    /// Synthetic bold/italic (see [`Synthetic`]).
    pub synthetic: Synthetic,
}

impl Span {
    pub fn new(text: impl Into<String>, font: FontId, size: f32) -> Span {
        Span {
            text: text.into(),
            font,
            size,
            color: Color::default(),
            char_spacing: 0.0,
            synthetic: Synthetic::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Right,
    Center,
    /// Both edges; the last line (and lines ending in a forced break) start-aligned.
    Justify,
}

/// Distance between baselines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeight {
    /// A multiple of each line's largest font size.
    Factor(f32),
    /// Points.
    Fixed(f32),
}

#[derive(Clone, Debug, PartialEq)]
pub struct ParagraphStyle {
    /// Box width in points.
    pub width: f32,
    pub align: Align,
    pub line_height: LineHeight,
    /// Indent of the first line in points (negative for a hanging indent).
    pub first_indent: f32,
    /// Hyphenate words in this language when that fills lines better (`None`: no
    /// hyphenation).
    pub hyphenate: Option<hyphen::Lang>,
    /// Justification: most character spacing to add between glyphs, in points (0: word
    /// spacing only).
    pub max_char_spacing: f32,
    /// Justification: word spaces stretch up to this multiple of their width before
    /// character spacing takes the rest (when `max_char_spacing` > 0).
    pub max_word_stretch: f32,
    /// Base direction: `Some(true)` right-to-left, `None` from the first strong character.
    pub rtl: Option<bool>,
}

impl Default for ParagraphStyle {
    fn default() -> Self {
        ParagraphStyle {
            width: 300.0,
            align: Align::Left,
            line_height: LineHeight::Factor(1.2),
            first_indent: 0.0,
            hyphenate: None,
            max_char_spacing: 0.0,
            max_word_stretch: 1.0,
            rtl: None,
        }
    }
}

/// A glyph placed on a line.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedGlyph {
    /// The glyph; its cluster indexes the paragraph text ([`Layout::text`]). An inserted
    /// hyphen has an empty cluster at the break.
    pub glyph: EncodedGlyph,
    /// Origin relative to the box's left edge, in points.
    pub x: f32,
    /// Distance to the next glyph's origin in points (everything included).
    pub advance: f32,
    /// Justification space after this glyph beyond kerning, in text space per unit size,
    /// written as a `TJ` adjustment (spaces of fonts where `Tw` does not apply).
    pub extra: f32,
}

/// Consecutive glyphs of a line in one span and font, shown with one `TJ`.
#[derive(Clone, Debug, PartialEq)]
pub struct Fragment {
    /// Index of the span.
    pub span: usize,
    pub font: RunFont,
    pub size: f32,
    /// Origin of the first glyph relative to the box's left edge.
    pub x: f32,
    /// `Tw` and `Tc` to set, in points.
    pub word_spacing: f32,
    pub char_spacing: f32,
    pub glyphs: Vec<PlacedGlyph>,
    /// Right-to-left run (glyphs are in visual order).
    pub rtl: bool,
}

impl Fragment {
    /// The `TJ` array operand.
    pub fn tj_operand(&self) -> String {
        let gs: Vec<EncodedGlyph> = self.glyphs.iter().map(|g| g.glyph.clone()).collect();
        tj_array(&gs, |i| self.glyphs[i].extra)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    /// Byte range of the paragraph text on this line (trailing spaces included).
    pub range: Range<usize>,
    /// Baseline below the box's top edge, in points.
    pub baseline: f32,
    /// Left edge of the line's first glyph, relative to the box.
    pub x: f32,
    /// From the first glyph's origin to the end of the last glyph's advance.
    pub width: f32,
    /// Largest ascent and descent (descent negative) of the line, in points.
    pub ascent: f32,
    pub descent: f32,
    /// Ends with an inserted hyphen.
    pub hyphenated: bool,
    /// Stretched to the full width.
    pub justified: bool,
    pub fragments: Vec<Fragment>,
}

/// Things a layout could not do as asked.
#[derive(Clone, Debug, PartialEq)]
pub enum LayoutWarning {
    /// A line is wider than the box (a word that could not be broken).
    Overfull { line: usize, excess: f32 },
    /// Characters no font could show (byte offset in the paragraph text).
    Missing(usize, char),
    /// Text that needs contextual shaping (Arabic, Indic scripts): shown with nominal
    /// glyphs, which is legible for extraction but not typographically right.
    NeedsShaping(usize),
}

#[derive(Clone, Debug, PartialEq)]
pub struct Layout {
    /// The paragraph text: the spans joined, soft hyphens removed.
    pub text: String,
    /// Byte range of each span in `text`.
    pub spans: Vec<Range<usize>>,
    pub lines: Vec<Line>,
    /// From the box top to the last line's descent.
    pub height: f32,
    pub warnings: Vec<LayoutWarning>,
}

/// A glyph in logical order.
#[derive(Clone, Debug)]
struct G {
    span: usize,
    font: RunFont,
    enc: EncodedGlyph,
    size: f32,
    /// Advance with character spacing, in points.
    w: f32,
    /// Kerning to the next glyph, in points.
    kern: f32,
    tc: f32,
    space: bool,
}

/// A hyphen inserted at a break: its font, glyph and span.
type HyphenGlyph = (RunFont, EncodedGlyph, usize);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Mandatory,
    Allowed,
    Hyphen,
}

fn needs_shaping(c: char) -> bool {
    matches!(c as u32, 0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF | 0x0900..=0x0DFF | 0x1000..=0x109F | 0x1780..=0x17FF)
}

/// Lays out a paragraph of `spans` in a box of `style.width`.
pub fn layout(shaper: &mut dyn Shaper, spans: &[Span], style: &ParagraphStyle) -> Layout {
    // Paragraph text, soft hyphens taken out as break points.
    let mut text = String::new();
    let mut span_ranges = Vec::new();
    let mut soft: Vec<usize> = Vec::new();
    let mut gs: Vec<G> = Vec::new();
    let mut warnings = Vec::new();
    for (si, s) in spans.iter().enumerate() {
        let start = text.len();
        let mut clean = String::new();
        for c in s.text.chars() {
            if c == '\u{AD}' {
                soft.push(start + clean.len());
            } else {
                clean.push(c);
            }
        }
        let enc = shaper.shape(s.font, &clean);
        // The encoder normalizes; clusters index its text.
        let base = text.len();
        text.push_str(&enc.text);
        span_ranges.push(start..text.len());
        for (off, c) in &enc.missing {
            if linebreak::class(*c) != linebreak::Class::Bk && *c != '\r' && *c != '\t' {
                warnings.push(LayoutWarning::Missing(base + off, *c));
            }
        }
        for run in &enc.runs {
            for g in &run.glyphs {
                let mut e = g.clone();
                e.cluster = base + g.cluster.start..base + g.cluster.end;
                let ch = &enc.text[g.cluster.clone()];
                gs.push(G {
                    span: si,
                    font: run.font,
                    size: s.size,
                    w: g.advance * s.size + s.char_spacing,
                    kern: g.kern * s.size,
                    tc: s.char_spacing,
                    space: ch == " " || ch == "\u{A0}",
                    enc: e,
                });
            }
        }
        // Kerning stops at span ends.
        if let Some(last) = gs.last_mut() {
            last.kern = 0.0;
        }
    }
    if let Some(i) = text.find(needs_shaping) {
        warnings.push(LayoutWarning::NeedsShaping(i));
    }

    // Break candidates.
    let mut cands: Vec<(usize, Kind)> = linebreak::breaks(&text)
        .into_iter()
        .map(|(o, b)| {
            (
                o,
                match b {
                    Break::Mandatory => Kind::Mandatory,
                    Break::Allowed => Kind::Allowed,
                },
            )
        })
        .collect();
    let mut hyph: Vec<usize> = soft.clone();
    if let Some(lang) = style.hyphenate {
        let h = lang.hyphenator();
        let mut start = None;
        for (i, c) in text
            .char_indices()
            .chain(std::iter::once((text.len(), ' ')))
        {
            // Whole words (with marks), so ones with letters the patterns lack stay whole.
            if c.is_alphabetic() || is_combining_mark(c) || h.is_letter(c) {
                start.get_or_insert(i);
            } else if let Some(s) = start.take() {
                // Only plain words (not inside identifiers like "e2e" or "x_y").
                let before = text[..s].chars().next_back();
                let ok = !before.is_some_and(|b| b.is_alphanumeric() || b == '_')
                    && !(c.is_alphanumeric() || c == '_');
                if ok {
                    hyph.extend(h.hyphenate(&text[s..i]).into_iter().map(|p| s + p));
                }
            }
        }
    }
    for h in hyph {
        if !cands.iter().any(|c| c.0 == h) {
            cands.push((h, Kind::Hyphen));
        }
    }
    cands.sort_by_key(|c| c.0);

    // Glyph index of each byte offset: the first glyph starting at or after it.
    let glyph_at = |off: usize| gs.partition_point(|g| g.enc.cluster.start < off);
    let mut pos = vec![0.0f32; gs.len() + 1];
    for (i, g) in gs.iter().enumerate() {
        pos[i + 1] = pos[i] + g.w + g.kern;
    }
    // Visible width of glyphs [a, b): trailing spaces, the last kerning and the last
    // character spacing left out.
    let width = |a: usize, b: usize| -> f32 {
        let mut e = b;
        while e > a && gs[e - 1].space {
            e -= 1;
        }
        if e == a {
            return 0.0;
        }
        pos[e] - pos[a] - gs[e - 1].kern - gs[e - 1].tc
    };
    let mut hyphens: HashMap<(usize, FontId), Option<(RunFont, EncodedGlyph)>> = HashMap::new();
    let mut hyphen_for = |shaper: &mut dyn Shaper, g: &G| -> Option<(RunFont, EncodedGlyph)> {
        let font = spans[g.span].font;
        hyphens
            .entry((g.span, font))
            .or_insert_with(|| {
                let e = shaper.shape(font, "-");
                let r = e.runs.first()?;
                Some((r.font, r.glyphs.first()?.clone()))
            })
            .clone()
    };

    // Greedy filling.
    struct Raw {
        range: Range<usize>,
        glyphs: Range<usize>,
        hyphen: Option<HyphenGlyph>,
        forced: bool,
    }
    let mut raws: Vec<Raw> = Vec::new();
    let mut start = 0usize;
    let mut ci = 0usize;
    while start < text.len() || (raws.is_empty() && text.is_empty()) {
        if text.is_empty() {
            raws.push(Raw {
                range: 0..0,
                glyphs: 0..0,
                hyphen: None,
                forced: false,
            });
            break;
        }
        let avail = style.width
            - if raws.is_empty() {
                style.first_indent
            } else {
                0.0
            };
        let a = glyph_at(start);
        while ci < cands.len() && cands[ci].0 <= start {
            ci += 1;
        }
        let mut best: Option<(usize, Kind, Option<HyphenGlyph>)> = None;
        let mut end_reached = true;
        let mut j = ci;
        while j < cands.len() {
            let (off, kind) = cands[j];
            let b = glyph_at(off);
            let mut w = width(a, b);
            let mut hy = None;
            if kind == Kind::Hyphen {
                if b == a || gs[b - 1].space {
                    j += 1;
                    continue;
                }
                match hyphen_for(shaper, &gs[b - 1]) {
                    Some((rf, eg)) => {
                        w += gs[b - 1].tc + eg.advance * gs[b - 1].size;
                        hy = Some((rf, eg, gs[b - 1].span));
                    }
                    None => {
                        j += 1;
                        continue;
                    }
                }
            }
            let fits = w <= avail + 1e-3;
            if !fits {
                end_reached = false;
                break;
            }
            best = Some((off, kind, hy));
            if kind == Kind::Mandatory {
                end_reached = false;
                break;
            }
            j += 1;
        }
        // The rest of the text fits on this line.
        if end_reached && width(a, gs.len()) <= avail + 1e-3 {
            raws.push(Raw {
                range: start..text.len(),
                glyphs: a..gs.len(),
                hyphen: None,
                forced: true,
            });
            break;
        }
        // The last opportunity that fits (a hyphen when it fills more than a word break).
        let (end, kind, hy) = match best {
            Some(b) => b,
            None => {
                // Nothing fits: break inside the word that is too wide by itself.
                let first = cands[ci..].iter().find(|c| c.1 != Kind::Hyphen).copied();
                let fb = first.map_or(gs.len(), |c| glyph_at(c.0));
                if fb <= a + 1 {
                    match first {
                        Some((off, k)) => (off, k, None),
                        None => (text.len(), Kind::Mandatory, None),
                    }
                } else {
                    let mut b = a + 1;
                    while b < fb && width(a, b + 1) <= avail + 1e-3 {
                        b += 1;
                    }
                    let off = gs.get(b).map_or(text.len(), |g| g.enc.cluster.start);
                    (off, Kind::Allowed, None)
                }
            }
        };
        raws.push(Raw {
            range: start..end,
            glyphs: a..glyph_at(end),
            hyphen: hy,
            forced: kind == Kind::Mandatory,
        });
        start = end;
    }

    // Bidi levels.
    let bidi = BidiInfo::new(
        &text,
        style.rtl.map(|r| {
            if r {
                unicode_bidi::Level::rtl()
            } else {
                unicode_bidi::Level::ltr()
            }
        }),
    );
    let base_rtl = bidi.paragraphs.first().is_some_and(|p| p.level.is_rtl());
    let has_rtl = bidi.has_rtl();

    let mut lines = Vec::new();
    let mut baseline = 0.0f32;
    let mut prev_size = spans.first().map_or(12.0, |s| s.size);
    let nraw = raws.len();
    for (li, raw) in raws.into_iter().enumerate() {
        // Glyphs without trailing spaces.
        let mut e = raw.glyphs.end;
        while e > raw.glyphs.start && gs[e - 1].space {
            e -= 1;
        }
        let mut line_gs: Vec<G> = gs[raw.glyphs.start..e].to_vec();
        if let Some(last) = line_gs.last_mut() {
            last.kern = 0.0;
        }
        let mut hyphenated = false;
        if let Some((rf, mut eg, span)) = raw.hyphen.clone() {
            let s = &spans[span];
            let at = raw.range.end;
            eg.cluster = at..at;
            line_gs.push(G {
                span,
                font: rf,
                size: s.size,
                w: eg.advance * s.size + s.char_spacing,
                kern: 0.0,
                tc: s.char_spacing,
                space: false,
                enc: eg,
            });
            hyphenated = true;
        }
        // Visual order.
        let mut rtl_flags = vec![false; line_gs.len()];
        if has_rtl && !line_gs.is_empty() {
            let para = bidi
                .paragraphs
                .iter()
                .find(|p| p.range.contains(&raw.range.start))
                .unwrap_or(&bidi.paragraphs[0]);
            let lr = raw.range.start..raw.range.end.max(raw.range.start);
            let (levels, runs) = bidi.visual_runs(para, lr);
            let mut order: Vec<(G, bool)> = Vec::with_capacity(line_gs.len());
            let hy_glyph = if hyphenated { line_gs.pop() } else { None };
            for run in runs {
                let rtl = levels.get(run.start).is_some_and(|l| l.is_rtl());
                let mut part: Vec<G> = line_gs
                    .iter()
                    .filter(|g| run.contains(&g.enc.cluster.start))
                    .cloned()
                    .collect();
                if rtl {
                    part.reverse();
                    for g in &mut part {
                        g.kern = 0.0;
                    }
                }
                order.extend(part.into_iter().map(|g| (g, rtl)));
            }
            if let Some(h) = hy_glyph {
                if base_rtl {
                    order.insert(0, (h, true));
                } else {
                    order.push((h, false));
                }
            }
            line_gs = order.iter().map(|o| o.0.clone()).collect();
            rtl_flags = order.iter().map(|o| o.1).collect();
            if let Some(last) = line_gs.last_mut() {
                last.kern = 0.0;
            }
        }

        let n = line_gs.len();
        let natural: f32 = line_gs.iter().map(|g| g.w + g.kern).sum::<f32>()
            - line_gs.last().map_or(0.0, |g| g.tc);
        let indent = if li == 0 { style.first_indent } else { 0.0 };
        let avail = style.width - indent;
        let last_line = li + 1 == nraw;
        let justify = style.align == Align::Justify && !last_line && !raw.forced && n > 1;
        // Justification: per-space and per-gap additions in points.
        let mut per_space = 0.0f32;
        let mut tc_add = 0.0f32;
        let mut justified = false;
        if justify && avail > natural {
            let extra = avail - natural;
            let spaces: Vec<usize> = (0..n.saturating_sub(1))
                .filter(|&i| line_gs[i].space)
                .collect();
            let ns = spaces.len() as f32;
            let gaps = (n - 1) as f32;
            if style.max_char_spacing > 0.0 {
                let space_w = if spaces.is_empty() {
                    0.0
                } else {
                    spaces.iter().map(|&i| line_gs[i].w).sum::<f32>() / ns
                };
                let word_cap = ns * style.max_word_stretch * space_w;
                if extra > word_cap {
                    tc_add = ((extra - word_cap) / gaps).min(style.max_char_spacing);
                }
            }
            let rest = extra - tc_add * gaps;
            if ns > 0.0 {
                per_space = rest / ns;
                justified = true;
            } else if (rest).abs() < 1e-3 {
                justified = true;
            }
        }
        let content_w = if justified {
            avail
        } else {
            natural + tc_add * (n.saturating_sub(1)) as f32
        };
        let x0 = indent
            + match style.align {
                Align::Left => 0.0,
                Align::Right => avail - content_w,
                Align::Center => (avail - content_w) / 2.0,
                Align::Justify => {
                    if base_rtl && !justified {
                        avail - content_w
                    } else {
                        0.0
                    }
                }
            };
        // Placement and fragments.
        let mut frags: Vec<Fragment> = Vec::new();
        let mut x = x0;
        let mut ascent = 0.0f32;
        let mut descent = 0.0f32;
        for (i, g) in line_gs.iter().enumerate() {
            let (a, d) = shaper.metrics(spans[g.span].font);
            ascent = ascent.max(a * g.size);
            descent = descent.min(d * g.size);
            let is_last = i + 1 == n;
            let space_extra = if g.space && !is_last { per_space } else { 0.0 };
            // The last glyph's advance is its visible extent (no spacing after it).
            let advance = if is_last {
                g.w - g.tc
            } else {
                g.w + g.kern + space_extra + tc_add
            };
            let mut enc = g.enc.clone();
            enc.kern = if g.size > 0.0 { g.kern / g.size } else { 0.0 };
            let new_frag = match frags.last() {
                Some(f) => f.span != g.span || f.font != g.font || f.rtl != rtl_flags[i],
                None => true,
            };
            if new_frag {
                frags.push(Fragment {
                    span: g.span,
                    font: g.font,
                    size: g.size,
                    x,
                    word_spacing: 0.0,
                    char_spacing: spans[g.span].char_spacing + tc_add,
                    glyphs: Vec::new(),
                    rtl: rtl_flags[i],
                });
            }
            let f = frags.last_mut().expect("fragment");
            f.glyphs.push(PlacedGlyph {
                glyph: enc,
                x,
                advance,
                extra: 0.0,
            });
            if space_extra != 0.0 {
                // Marked here; resolved to Tw or TJ per fragment below.
                f.word_spacing = space_extra;
            }
            x += advance;
        }
        for f in &mut frags {
            if f.word_spacing == 0.0 {
                continue;
            }
            let extra = f.word_spacing;
            let text_of = |g: &PlacedGlyph| &text[g.glyph.cluster.clone()];
            let tw_ok = f.glyphs.iter().all(|g| {
                (g.glyph.word_space && text_of(g) == " ")
                    || (!g.glyph.word_space && text_of(g) != " ")
            });
            if !tw_ok {
                f.word_spacing = 0.0;
                let size = f.size;
                let n = f.glyphs.len();
                for (k, g) in f.glyphs.iter_mut().enumerate() {
                    if text_of(g) == " " || text_of(g) == "\u{A0}" {
                        // The last glyph's adjustment is not written in its TJ; the next
                        // fragment's position covers it.
                        if k + 1 < n {
                            g.extra = extra / size;
                        }
                    }
                }
            }
        }
        if n == 0 {
            let si = span_ranges
                .iter()
                .position(|r| r.contains(&raw.range.start))
                .unwrap_or(spans.len().saturating_sub(1));
            if let Some(s) = spans.get(si) {
                let (a, d) = shaper.metrics(s.font);
                (ascent, descent) = (a * s.size, d * s.size);
                prev_size = s.size;
            }
        }
        let max_size = line_gs.iter().map(|g| g.size).fold(0.0f32, f32::max);
        let size = if max_size > 0.0 { max_size } else { prev_size };
        prev_size = size;
        baseline = if li == 0 {
            ascent
        } else {
            baseline
                + match style.line_height {
                    LineHeight::Fixed(l) => l,
                    LineHeight::Factor(f) => f * size,
                }
        };
        if natural > avail + 1e-3 {
            warnings.push(LayoutWarning::Overfull {
                line: li,
                excess: natural - avail,
            });
        }
        lines.push(Line {
            range: raw.range,
            baseline,
            x: x0,
            width: x - x0,
            ascent,
            descent,
            hyphenated,
            justified,
            fragments: frags,
        });
    }
    let height = lines.last().map_or(0.0, |l| l.baseline - l.descent);
    Layout {
        text,
        spans: span_ranges,
        lines,
        height,
        warnings,
    }
}

fn color_op(c: Color) -> String {
    match c {
        Color::Gray(g) => format!("{} g", fmt_num(g)),
        Color::Rgb([r, g, b]) => format!("{} {} {} rg", fmt_num(r), fmt_num(g), fmt_num(b)),
        Color::Cmyk([c, m, y, k]) => format!(
            "{} {} {} {} k",
            fmt_num(c),
            fmt_num(m),
            fmt_num(y),
            fmt_num(k)
        ),
    }
}

fn stroke_color_op(c: Color) -> String {
    let s = color_op(c);
    match s.rsplit_once(' ') {
        Some((v, op)) => format!("{v} {}", op.to_ascii_uppercase()),
        None => s,
    }
}

/// Content stream operators drawing `layout` with the box's top-left corner at
/// (`left`, `top`) in user space: one `BT`/`ET` block, a `Tm` per fragment. `name` gives
/// the font resource name for a run font. The graphics state (`Tc`, `Tw`, `Tr`, colours)
/// is set as needed; wrap the result in `q`/`Q` to keep it local.
pub fn write_content(
    layout: &Layout,
    spans: &[Span],
    left: f32,
    top: f32,
    name: &dyn Fn(RunFont) -> String,
) -> Vec<u8> {
    let mut s = String::from("BT\n");
    let mut font: Option<(RunFont, u32)> = None;
    let mut color: Option<String> = None;
    let mut tc = 0.0f32;
    let mut tw = 0.0f32;
    let mut tr = 0u8;
    for line in &layout.lines {
        for f in &line.fragments {
            let span = &spans[f.span];
            let key = (f.font, f.size.to_bits());
            if font != Some(key) {
                let _ = writeln!(s, "/{} {} Tf", name(f.font), fmt_num(f.size));
                font = Some(key);
            }
            let c = color_op(span.color);
            if color.as_ref() != Some(&c) {
                let _ = writeln!(s, "{c}");
                if span.synthetic.bold {
                    let _ = writeln!(s, "{}", stroke_color_op(span.color));
                }
                color = Some(c);
            }
            let mode = span.synthetic.render_mode();
            if mode != tr {
                let _ = writeln!(s, "{mode} Tr");
                tr = mode;
            }
            if span.synthetic.bold {
                let _ = writeln!(s, "{} w", fmt_num(span.synthetic.stroke_width(f.size)));
            }
            if (f.char_spacing - tc).abs() > 1e-6 {
                let _ = writeln!(s, "{} Tc", fmt_num(f.char_spacing));
                tc = f.char_spacing;
            }
            if (f.word_spacing - tw).abs() > 1e-6 {
                let _ = writeln!(s, "{} Tw", fmt_num(f.word_spacing));
                tw = f.word_spacing;
            }
            let m = span.synthetic.text_matrix(left + f.x, top - line.baseline);
            let _ = writeln!(
                s,
                "{} {} {} {} {} {} Tm",
                fmt_num(m[0]),
                fmt_num(m[1]),
                fmt_num(m[2]),
                fmt_num(m[3]),
                fmt_num(m[4]),
                fmt_num(m[5])
            );
            let _ = writeln!(s, "{} TJ", f.tj_operand());
        }
    }
    if tr != 0 {
        s.push_str("0 Tr\n");
    }
    s.push_str("ET\n");
    s.into_bytes()
}
