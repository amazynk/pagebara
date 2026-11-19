//! Text of a display list in reading order with per-glyph quads, for search and selection.
//!
//! Layout analysis: glyphs are grouped by writing direction (rotation, and vertical writing),
//! then within each group into lines along the baseline, split where a gap is too wide to
//! be within one line of text. Lines stack into blocks, and recursive XY-cuts on the gaps
//! between blocks order them: columns left to right (right to left for right-to-left
//! text), bands top to bottom. Lines of right-to-left scripts are reordered from visual to
//! logical order with the Unicode bidirectional algorithm. Blocks split further into
//! paragraphs at indented first lines.

use std::collections::BTreeMap;

use pagebara_syntax::{Document, Object};
use unicode_bidi::{BidiClass, Level, ParagraphBidiInfo, bidi_class};

use crate::display::{Cmd, DisplayList, Id, NONE};
use crate::geom::{Point, Rect};

/// A gap wider than this share of the font size between glyphs reads as a space.
const SPACE_GAP: f32 = 0.2;
/// Baselines closer than this share of the font size are the same line.
const SAME_LINE: f32 = 0.5;
/// A gap wider than this share of the font size along a baseline separates two lines.
const LINE_GAP: f32 = 1.5;
/// Lines further apart than this share of the font size never share a block.
const MAX_LEADING: f32 = 2.5;
/// Gaps between blocks narrower than this share of the font size are not cut.
const CUT_GAP: f32 = 0.3;
/// Columns narrower than this many font sizes beside lines on their baselines are cells.
const CELL_WIDTH: f32 = 10.0;
/// A first line indented by this share of the font size starts a paragraph.
const INDENT: f32 = 0.8;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextChar {
    /// Byte range of the character's text in [`TextPage::text`]; empty for glyphs whose text
    /// an `/ActualText` span gave to its first glyph.
    pub text: (u32, u32),
    /// The glyph's box (origin, along the advance, up to the ascent) in page space.
    pub quad: [Point; 4],
    /// Index in [`DisplayList::glyphs`], or [`NONE`] for a space inserted for a gap.
    pub glyph: Id,
}

impl TextChar {
    pub fn bbox(&self) -> Rect {
        Rect::from_points(&self.quad)
    }
}

/// A run of characters between spaces.
#[derive(Clone, Debug, PartialEq)]
pub struct TextWord {
    /// Range of [`TextPage::chars`].
    pub chars: (u32, u32),
    pub bbox: Rect,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    /// Range of [`TextPage::chars`].
    pub chars: (u32, u32),
    /// Range of [`TextPage::words`].
    pub words: (u32, u32),
    pub bbox: Rect,
    /// Written top to bottom.
    pub vertical: bool,
    /// Its paragraph direction is right to left.
    pub rtl: bool,
}

/// A paragraph: lines of a layout block up to the next indented first line.
#[derive(Clone, Debug, PartialEq)]
pub struct TextBlock {
    /// Range of [`TextPage::lines`].
    pub lines: (u32, u32),
    pub bbox: Rect,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextPage {
    /// Lines joined by `\n`, paragraphs separated by an empty line.
    pub text: String,
    pub chars: Vec<TextChar>,
    pub words: Vec<TextWord>,
    pub lines: Vec<TextLine>,
    pub blocks: Vec<TextBlock>,
}

impl TextPage {
    pub fn char_text(&self, c: &TextChar) -> &str {
        &self.text[c.text.0 as usize..c.text.1 as usize]
    }

    /// The character whose text contains byte `offset` of [`TextPage::text`].
    pub fn char_at(&self, offset: usize) -> Option<&TextChar> {
        let i = self
            .chars
            .partition_point(|c| (c.text.1 as usize) <= offset);
        self.chars.get(i).filter(|c| c.text.0 as usize <= offset)
    }

    /// Boxes of the characters covering a byte range of the text (a search hit).
    pub fn quads(&self, range: std::ops::Range<usize>) -> Vec<[Point; 4]> {
        let i = self
            .chars
            .partition_point(|c| (c.text.1 as usize) <= range.start);
        self.chars[i..]
            .iter()
            .take_while(|c| (c.text.0 as usize) < range.end)
            .filter(|c| c.text.1 > c.text.0)
            .map(|c| c.quad)
            .collect()
    }

    /// The text of a line.
    pub fn line_text(&self, l: &TextLine) -> &str {
        self.span_text(l.chars)
    }

    /// The text of a word.
    pub fn word_text(&self, w: &TextWord) -> &str {
        self.span_text(w.chars)
    }

    fn span_text(&self, (a, b): (u32, u32)) -> &str {
        if a >= b {
            return "";
        }
        let start = self.chars[a as usize].text.0 as usize;
        let end = self.chars[b as usize - 1].text.1 as usize;
        &self.text[start.min(end)..end]
    }
}

/// A glyph placed on the page.
struct Placed {
    text: (u32, u32),
    quad: [Point; 4],
    glyph: Id,
    origin: Point,
    /// The angle of the advance (down the column for vertical writing) in whole degrees,
    /// 0 to 359.
    angle: u16,
    vertical: bool,
    size: f32,
    space: bool,
    /// The direction of its text: see [`text_direction`].
    bidi: i8,
}

/// Extracts the text of a display list. Fonts' ToUnicode maps are read on first use.
pub fn extract_text(doc: &Document, list: &DisplayList) -> TextPage {
    let (scratch, placed, _) = place_all(doc, list);
    layout(&placed, &scratch)
}

/// A character the content does not draw as text (recognised in a picture), laid out with
/// the page's own text by [`extract_text_with`]. Its [`TextChar::glyph`] is [`NONE`].
#[derive(Clone, Debug, PartialEq)]
pub struct ExtraChar {
    pub text: String,
    /// Bottom-left, bottom-right, top-right and top-left in page space, along the reading
    /// direction, from below descenders to above capitals.
    pub quad: [Point; 4],
    /// A space between words: no other space is added beside it.
    pub space: bool,
    /// Characters of one word share it.
    pub word: u32,
    /// Characters of one line share it: they keep to one baseline.
    pub line: u32,
    /// The font size it is set in, as far as it can be told.
    pub size: f32,
}

/// Recognised lines this close (degrees) to the page's text direction, or to most of the
/// recognised lines, read in that direction.
const SNAP_ANGLE: i32 = 6;

/// [`extract_text`] of the glyphs `keep` accepts (by their quad) and the `extra`
/// characters, laid out together. Extra words over the page's own text are left out: the
/// text the page has wins.
pub fn extract_text_with(
    doc: &Document,
    list: &DisplayList,
    extra: &[ExtraChar],
    keep: &dyn Fn(&[Point; 4]) -> bool,
) -> TextPage {
    let (mut scratch, mut placed, _) = place_all(doc, list);
    placed.retain(|p| keep(&p.quad));
    let covered = covered_words(&placed, extra);
    let extra: Vec<&ExtraChar> = extra
        .iter()
        .filter(|e| !covered.contains(&e.word) && !e.text.is_empty())
        .filter(|e| {
            let [bl, _, _, tl] = e.quad;
            let h = (tl.x - bl.x).hypot(tl.y - bl.y);
            h > 0.0 && h.is_finite() && e.size > 0.0 && e.size.is_finite()
        })
        .collect();
    let angle_of = |e: &ExtraChar| {
        let [bl, br, _, _] = e.quad;
        let (dx, dy) = (br.x - bl.x, br.y - bl.y);
        if dx == 0.0 && dy == 0.0 {
            0
        } else {
            (dy.atan2(dx).to_degrees().round() as i32).rem_euclid(360)
        }
    };
    // Lines a degree or two apart read as one direction: the page's, else their own.
    let real = mode(
        placed
            .iter()
            .filter(|p| !p.vertical)
            .map(|p| i32::from(p.angle)),
    );
    let own = mode(extra.iter().map(|e| angle_of(e)));
    let near = |a: i32, b: Option<i32>| {
        b.filter(|b| {
            let d = (a - b).rem_euclid(360);
            d.min(360 - d) <= SNAP_ANGLE
        })
    };
    // The baseline of each line: its first character's.
    let mut anchors: std::collections::HashMap<u32, Point> = Default::default();
    for e in extra {
        let [bl, _, _, tl] = e.quad;
        let raw = angle_of(e);
        let angle = near(raw, real).or(near(raw, own)).unwrap_or(raw);
        let start = scratch.len() as u32;
        scratch.push_str(&e.text);
        // Boxes of recognised text reach below descenders: the baseline is a fifth up.
        let origin = Point::new(bl.x + (tl.x - bl.x) * 0.2, bl.y + (tl.y - bl.y) * 0.2);
        let anchor = *anchors.entry(e.line).or_insert(origin);
        let (s, c) = (angle as f32).to_radians().sin_cos();
        let along = (origin.x - anchor.x) * c + (origin.y - anchor.y) * s;
        placed.push(Placed {
            text: (start, scratch.len() as u32),
            quad: e.quad,
            glyph: NONE,
            origin: Point::new(anchor.x + c * along, anchor.y + s * along),
            angle: angle as u16,
            vertical: false,
            size: e.size,
            space: e.space,
            bidi: text_direction(&e.text),
        });
    }
    layout(&placed, &scratch)
}

/// The most common value.
fn mode(values: impl Iterator<Item = i32>) -> Option<i32> {
    let mut counts = [0u32; 360];
    let mut any = false;
    for v in values {
        counts[v.rem_euclid(360) as usize] += 1;
        any = true;
    }
    any.then(|| {
        (0..360)
            .max_by_key(|&i| (counts[i as usize], -i))
            .unwrap_or(0)
    })
}

/// The text of each marked-content sequence with an `/MCID` (the content items of tagged
/// PDF), laid out like [`extract_text`]. Glyphs belong to the innermost enclosing sequence
/// with an MCID; glyphs outside any are left out. Form XObjects' sequences are not told
/// apart from the page's: their MCIDs read as the page's.
pub fn extract_marked(doc: &Document, list: &DisplayList) -> BTreeMap<i64, TextPage> {
    let (scratch, placed, mcids) = place_all(doc, list);
    let mut groups: BTreeMap<i64, Vec<Placed>> = BTreeMap::new();
    for (p, mcid) in placed.into_iter().zip(mcids) {
        if let Some(m) = mcid {
            groups.entry(m).or_default().push(p);
        }
    }
    groups
        .into_iter()
        .map(|(m, placed)| (m, layout(&placed, &scratch)))
        .collect()
}

/// The page-space bounds of what each marked-content sequence with an `/MCID` paints
/// besides text: images, fills, strokes and shadings (the latter as their clip's path).
/// Gives figures, which have no text, a place on the page.
pub fn marked_bounds(list: &DisplayList) -> BTreeMap<i64, Rect> {
    let mut out: BTreeMap<i64, Rect> = BTreeMap::new();
    let mut marks: Vec<Option<i64>> = Vec::new();
    let mut clips: Vec<Option<Rect>> = Vec::new();
    let path_rect = |path: Id, ctm: Id| {
        list.path(path)
            .bounds()
            .map(|r| r.transform(&list.matrix(ctm)))
    };
    for cmd in &list.cmds {
        let drawn = match *cmd {
            Cmd::BeginMarked { marked } => {
                marks.push(list.marked[marked as usize].mcid());
                None
            }
            Cmd::EndMarked => {
                marks.pop();
                None
            }
            Cmd::Clip { path, ctm, .. } => {
                clips.push(path_rect(path, ctm));
                None
            }
            Cmd::ClipText { .. } => {
                clips.push(None);
                None
            }
            Cmd::PopClip { count } => {
                clips.truncate(clips.len().saturating_sub(count as usize));
                None
            }
            Cmd::Image { ctm, .. } => {
                Some(Rect::new(0.0, 0.0, 1.0, 1.0).transform(&list.matrix(ctm)))
            }
            Cmd::Fill { path, ctm, .. } | Cmd::Stroke { path, ctm, .. } => path_rect(path, ctm),
            Cmd::Shading { .. } => clips.iter().rev().find_map(|c| *c),
            _ => None,
        };
        if let (Some(r), Some(m)) = (drawn, marks.iter().rev().find_map(|m| *m))
            && r.x0.is_finite()
            && r.y0.is_finite()
            && r.x1.is_finite()
            && r.y1.is_finite()
        {
            out.entry(m).and_modify(|o| *o = o.union(&r)).or_insert(r);
        }
    }
    out
}

/// Every glyph placed on the page, the text they index, and the MCID each is marked with.
fn place_all(doc: &Document, list: &DisplayList) -> (String, Vec<Placed>, Vec<Option<i64>>) {
    let mut scratch = String::with_capacity(list.glyphs.len());
    let mut placed = Vec::with_capacity(list.glyphs.len());
    let mut mcids = Vec::with_capacity(list.glyphs.len());
    // Per open marked-content sequence: its `/ActualText` and its `/MCID`.
    let mut actual: Vec<Option<String>> = Vec::new();
    let mut marks: Vec<Option<i64>> = Vec::new();
    for cmd in &list.cmds {
        match *cmd {
            Cmd::BeginMarked { marked } => {
                let m = &list.marked[marked as usize];
                let at = m
                    .properties
                    .as_ref()
                    .and_then(|p| p.get(b"ActualText"))
                    .map(|o| text_string(&doc.resolve(o)));
                actual.push(at);
                marks.push(m.mcid());
            }
            Cmd::EndMarked => {
                actual.pop();
                marks.pop();
            }
            Cmd::Text { run } => {
                let current = actual.iter_mut().rev().find(|a| a.is_some());
                place_run(doc, list, run, &mut scratch, &mut placed, current);
                mcids.resize(placed.len(), marks.iter().rev().find_map(|m| *m));
            }
            _ => {}
        }
    }
    (scratch, placed, mcids)
}

/// The words of `extra` with a character whose centre lies on a glyph of `placed`.
fn covered_words(placed: &[Placed], extra: &[ExtraChar]) -> std::collections::HashSet<u32> {
    const CELL: f32 = 24.0;
    let key = |x: f32, y: f32| ((x / CELL).floor() as i32, (y / CELL).floor() as i32);
    let mut grid: std::collections::HashMap<(i32, i32), Vec<Rect>> = Default::default();
    for p in placed.iter().filter(|p| !p.space) {
        let r = Rect::from_points(&p.quad);
        if !(r.width() < 4.0 * CELL && r.height() < 4.0 * CELL) {
            continue;
        }
        let (a, b) = (key(r.x0, r.y0), key(r.x1, r.y1));
        for x in a.0..=b.0 {
            for y in a.1..=b.1 {
                grid.entry((x, y)).or_default().push(r);
            }
        }
    }
    let mut out = std::collections::HashSet::new();
    if grid.is_empty() {
        return out;
    }
    for e in extra.iter().filter(|e| !e.space) {
        let r = Rect::from_points(&e.quad);
        let (cx, cy) = ((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0);
        let on = grid.get(&key(cx, cy)).is_some_and(|v| {
            v.iter()
                .any(|g| g.x0 <= cx && cx <= g.x1 && g.y0 <= cy && cy <= g.y1)
        });
        if on {
            out.insert(e.word);
        }
    }
    out
}

/// Layout analysis of placed glyphs whose text is in `src`.
fn layout(placed: &[Placed], src: &str) -> TextPage {
    let mut page = TextPage::default();
    // Gaps add a few spaces; overprinted duplicates drop a few glyphs.
    page.text.reserve(src.len() + src.len() / 8);
    page.chars.reserve(placed.len() + placed.len() / 8);
    for group in direction_groups(placed) {
        Group::new(placed, src, group).emit(&mut page);
    }
    page
}

fn place_run(
    doc: &Document,
    list: &DisplayList,
    run: Id,
    scratch: &mut String,
    out: &mut Vec<Placed>,
    mut actual: Option<&mut Option<String>>,
) {
    let r = &list.runs[run as usize];
    let font = &list.fonts[r.font as usize].font;
    let m = r.matrix;
    let dir = if r.vertical {
        m.apply_vector(Point::new(0.0, -1.0))
    } else {
        m.apply_vector(Point::new(1.0, 0.0))
    };
    let len = dir.x.hypot(dir.y);
    if len == 0.0 || !len.is_finite() {
        return;
    }
    let dir = Point::new(dir.x / len, dir.y / len);
    let angle = (dir.y.atan2(dir.x).to_degrees().round() as i32).rem_euclid(360) as u16;
    let up = m.apply_vector(Point::new(0.0, r.size));
    let size = up.x.hypot(up.y).max(r.size.abs() * len).max(1e-3);
    let (asc, desc) = (font.ascent * r.size, font.descent * r.size);
    for (k, g) in list.run_glyphs(r).iter().enumerate() {
        let start = scratch.len() as u32;
        match actual.as_deref_mut() {
            Some(a) => {
                // The span's text goes to its first glyph.
                if let Some(t) = a.take() {
                    scratch.push_str(&t);
                }
                *a = Some(String::new());
            }
            None => {
                if !font.unicode(doc, g.code, scratch) {
                    scratch.push('\u{FFFD}');
                }
                expand_ligature(scratch, start as usize);
            }
        }
        let text = (start, scratch.len() as u32);
        let s = &scratch[start as usize..];
        let space = !s.is_empty() && s.chars().all(|c| c.is_whitespace() || c.is_control());
        let bidi = text_direction(s);
        let w = if g.width > 0.0 {
            g.width
        } else {
            0.25 * r.size
        };
        let y = g.y + r.rise;
        let (x0, x1) = (g.x, g.x + w);
        let quad = [
            Point::new(x0, y + desc),
            Point::new(x1, y + desc),
            Point::new(x1, y + asc),
            Point::new(x0, y + asc),
        ]
        .map(|p| m.apply(p));
        out.push(Placed {
            text,
            quad,
            glyph: r.glyphs.0 + k as u32,
            origin: m.apply(Point::new(g.x, y)),
            angle,
            vertical: r.vertical,
            size,
            space,
            bidi,
        });
    }
}

/// Glyph indices by writing direction: angles within a few degrees share a group. The
/// group with most glyphs comes first.
fn direction_groups(placed: &[Placed]) -> Vec<(bool, i32, Vec<usize>)> {
    let mut counts = [[0u32; 360]; 2];
    for p in placed {
        counts[usize::from(p.vertical)][usize::from(p.angle)] += 1;
    }
    let mut groups: Vec<(bool, i32, Vec<usize>)> = Vec::new();
    // The group of each (vertical, angle) seen so far; there are seldom more than a few.
    let mut seen: Vec<(bool, u16, usize)> = Vec::new();
    for (i, p) in placed.iter().enumerate() {
        let key = (p.vertical, p.angle);
        let g = match seen.iter().find(|s| (s.0, s.1) == key) {
            Some(s) => s.2,
            None => {
                let c = &counts[usize::from(p.vertical)];
                let b = i32::from(p.angle);
                let rep = (-2..=2)
                    .map(|d| (b + d).rem_euclid(360))
                    .max_by_key(|&k| (c[k as usize], -(k - b).abs()))
                    .unwrap_or(b);
                let g = match groups.iter().position(|g| g.0 == p.vertical && g.1 == rep) {
                    Some(g) => g,
                    None => {
                        groups.push((p.vertical, rep, Vec::new()));
                        groups.len() - 1
                    }
                };
                seen.push((key.0, key.1, g));
                g
            }
        };
        groups[g].2.push(i);
    }
    groups.sort_by_key(|g| (std::cmp::Reverse(g.2.len()), g.0, g.1));
    groups
}

/// A glyph in the frame of its direction group: `u` along the reading direction, `v`
/// across it, increasing towards the line before.
#[derive(Clone, Copy, Default)]
struct Geo {
    u0: f32,
    u1: f32,
    v0: f32,
    v1: f32,
    base: f32,
}

struct Line {
    items: Vec<usize>,
    u0: f32,
    u1: f32,
    v0: f32,
    v1: f32,
    base: f32,
    size: f32,
}

struct Block {
    lines: Vec<usize>,
    u0: f32,
    u1: f32,
    v0: f32,
    v1: f32,
    size: f32,
}

struct Group<'a> {
    placed: &'a [Placed],
    src: &'a str,
    vertical: bool,
    geo: Vec<Geo>,
    lines: Vec<Line>,
    blocks: Vec<Block>,
}

impl<'a> Group<'a> {
    fn new(
        placed: &'a [Placed],
        src: &'a str,
        (vertical, angle, idx): (bool, i32, Vec<usize>),
    ) -> Self {
        let a = (angle as f32).to_radians();
        let d = Point::new(a.cos(), a.sin());
        let n = Point::new(-d.y, d.x);
        let dot = |p: Point, a: Point| p.x * a.x + p.y * a.y;
        let mut geo = vec![Geo::default(); placed.len()];
        for &i in &idx {
            let p = &placed[i];
            let us = p.quad.map(|q| dot(q, d));
            let vs = p.quad.map(|q| dot(q, n));
            let (v0, v1) = (min4(vs), max4(vs));
            geo[i] = Geo {
                u0: min4(us),
                u1: max4(us),
                v0,
                v1,
                base: if vertical {
                    (v0 + v1) / 2.0
                } else {
                    dot(p.origin, n)
                },
            };
        }
        let mut g = Group {
            placed,
            src,
            vertical,
            geo,
            lines: Vec::new(),
            blocks: Vec::new(),
        };
        g.build_lines(idx);
        g.build_blocks();
        g
    }

    fn text(&self, i: usize) -> &'a str {
        let t = self.placed[i].text;
        &self.src[t.0 as usize..t.1 as usize]
    }

    /// Clusters glyphs by baseline, then splits clusters at wide gaps.
    fn build_lines(&mut self, mut idx: Vec<usize>) {
        let (geo, placed) = (&self.geo, self.placed);
        // Baselines from the top; `idx` is ascending, so ties keep that order as a stable
        // sort would.
        let mut keyed: Vec<u64> = idx
            .iter()
            .map(|&i| u64::from(!total_order(geo[i].base)) << 32 | i as u64)
            .collect();
        keyed.sort_unstable();
        for (d, k) in idx.iter_mut().zip(keyed) {
            *d = k as u32 as usize;
        }
        let mut i = 0;
        while i < idx.len() {
            let first = idx[i];
            let mut j = i + 1;
            while j < idx.len() {
                let k = idx[j];
                let size = placed[first].size.max(placed[k].size);
                if (geo[first].base - geo[k].base).abs() > SAME_LINE * size {
                    break;
                }
                j += 1;
            }
            let cluster = &mut idx[i..j];
            cluster.sort_by(|&a, &b| geo[a].u0.total_cmp(&geo[b].u0));
            let mut start = 0;
            let mut end = f32::NEG_INFINITY;
            for (k, &g) in cluster.iter().enumerate() {
                if k > start && geo[g].u0 - end > LINE_GAP * placed[g].size {
                    self.lines.push(self.line(&cluster[start..k]));
                    start = k;
                }
                end = end.max(geo[g].u1);
            }
            self.lines.push(self.line(&cluster[start..]));
            i = j;
        }
    }

    fn line(&self, items: &[usize]) -> Line {
        let g = |i: usize| self.geo[i];
        let mut l = Line {
            items: items.to_vec(),
            u0: f32::INFINITY,
            u1: f32::NEG_INFINITY,
            v0: f32::INFINITY,
            v1: f32::NEG_INFINITY,
            base: 0.0,
            size: 0.0,
        };
        for &i in items {
            let (q, p) = (g(i), &self.placed[i]);
            l.u0 = l.u0.min(q.u0);
            l.u1 = l.u1.max(q.u1);
            l.v0 = l.v0.min(q.v0);
            l.v1 = l.v1.max(q.v1);
            l.base += q.base;
            l.size = l.size.max(p.size);
        }
        l.base /= items.len().max(1) as f32;
        l
    }

    /// Stacks lines into blocks: a line joins the one before when each is the other's only
    /// overlapping neighbour across the gap, at a similar size and the usual line spacing.
    fn build_blocks(&mut self) {
        let lines = &self.lines;
        let n = lines.len();
        let mut order: Vec<usize> = (0..n).collect();
        order.sort_by(|&a, &b| lines[b].base.total_cmp(&lines[a].base));
        let overlap = |a: &Line, b: &Line| a.u1.min(b.u1) - a.u0.max(b.u0) > 0.0;
        // The nearest overlapping lines below and above each line, if unique.
        let mut below = vec![None; n];
        let mut above = vec![None; n];
        let mut seen_above = vec![false; n];
        for (k, &a) in order.iter().enumerate() {
            let la = &lines[a];
            let mut near: Option<(usize, f32)> = None;
            let mut unique = true;
            for &b in &order[k + 1..] {
                let lb = &lines[b];
                let dist = la.base - lb.base;
                if dist > MAX_LEADING * la.size.max(lb.size) {
                    break;
                }
                if !overlap(la, lb) || dist <= 0.0 {
                    continue;
                }
                match near {
                    None => near = Some((b, dist)),
                    Some((_, d)) if dist - d < 0.2 * la.size => unique = false,
                    _ => break,
                }
            }
            if let Some((b, d)) = near {
                if unique {
                    below[a] = Some((b, d));
                }
                // A line with two nearest lines above joins neither.
                if seen_above[b] {
                    above[b] = None;
                } else {
                    seen_above[b] = true;
                    above[b] = unique.then_some(a);
                }
            }
        }
        let pairs: Vec<(usize, usize, f32)> = (0..n)
            .filter_map(|a| {
                let (b, d) = below[a]?;
                let (sa, sb) = (lines[a].size, lines[b].size);
                (above[b] == Some(a) && sa.max(sb) <= 1.3 * sa.min(sb)).then_some((a, b, d / sa))
            })
            .collect();
        let mut spacing: Vec<f32> = pairs.iter().map(|p| p.2).collect();
        spacing.sort_by(f32::total_cmp);
        let typical = spacing.get(spacing.len() / 2).copied().unwrap_or(1.2);
        let mut next = vec![None; n];
        let mut has_prev = vec![false; n];
        for (a, b, s) in pairs {
            if s <= typical * 1.3 + 0.1 {
                next[a] = Some(b);
                has_prev[b] = true;
            }
        }
        for &a in &order {
            if has_prev[a] {
                continue;
            }
            let mut b = Block {
                lines: Vec::new(),
                u0: f32::INFINITY,
                u1: f32::NEG_INFINITY,
                v0: f32::INFINITY,
                v1: f32::NEG_INFINITY,
                size: 0.0,
            };
            let mut cur = Some(a);
            while let Some(i) = cur {
                let l = &lines[i];
                b.lines.push(i);
                b.u0 = b.u0.min(l.u0);
                b.u1 = b.u1.max(l.u1);
                b.v0 = b.v0.min(l.v0);
                b.v1 = b.v1.max(l.v1);
                b.size = b.size.max(l.size);
                cur = next[i];
            }
            self.blocks.push(b);
        }
    }

    /// Orders blocks by recursive XY-cuts: columns first, then bands.
    fn xy_cut(&self, mut ids: Vec<usize>, rtl: bool, out: &mut Vec<Unit>) {
        if ids.len() <= 1 {
            out.extend(ids.into_iter().map(Unit::Block));
            return;
        }
        let blocks = &self.blocks;
        let gap = CUT_GAP * self.median_size(&ids);
        let mut by_u = ids.clone();
        by_u.sort_by(|&a, &b| blocks[a].u0.total_cmp(&blocks[b].u0));
        let (mut columns, column_gap) = split(&by_u, gap, |i| (blocks[i].u0, blocks[i].u1));
        ids.sort_by(|&a, &b| blocks[b].v1.total_cmp(&blocks[a].v1));
        let (bands, band_gap) = split(&ids, gap, |i| (-blocks[i].v1, -blocks[i].v0));
        let table = columns.len() > 1 && self.table(&columns);
        if table && bands.len() == 1 {
            out.push(Unit::Rows(ids));
            return;
        }
        // Columns first, unless bands are separated much more clearly or the columns are
        // cells of table rows.
        let parts =
            if columns.len() > 1 && !table && (bands.len() == 1 || band_gap <= 1.5 * column_gap) {
                if rtl {
                    columns.reverse();
                }
                columns
            } else {
                bands
            };
        if parts.len() > 1 {
            for p in parts {
                self.xy_cut(p, rtl, out);
            }
            return;
        }
        ids.sort_by(|&a, &b| {
            let (a, b) = (&blocks[a], &blocks[b]);
            b.v1.total_cmp(&a.v1).then(a.u0.total_cmp(&b.u0))
        });
        // One-line blocks side by side on a baseline still read as one row.
        let mut i = 0;
        while i < ids.len() {
            let mut j = i + 1;
            if let [l] = blocks[ids[i]].lines[..] {
                while j < ids.len()
                    && matches!(blocks[ids[j]].lines[..], [m] if self.same_row(l, m))
                {
                    j += 1;
                }
            }
            out.push(match j - i {
                1 => Unit::Block(ids[i]),
                _ => Unit::Rows(ids[i..j].to_vec()),
            });
            i = j;
        }
    }

    /// Lines side by side on one baseline.
    fn same_row(&self, l: usize, m: usize) -> bool {
        let (a, b) = (&self.lines[l], &self.lines[m]);
        (a.base - b.base).abs() <= SAME_LINE * a.size.max(b.size) && (b.u0 >= a.u1 || a.u0 >= b.u1)
    }

    fn median_size(&self, ids: &[usize]) -> f32 {
        let mut sizes: Vec<f32> = ids.iter().map(|&i| self.blocks[i].size).collect();
        sizes.sort_by(f32::total_cmp);
        sizes.get(sizes.len() / 2).copied().unwrap_or(1.0)
    }

    /// Whether some column is cells of table rows rather than text: each of its lines has
    /// a line of another column on its baseline, and it is narrow or all one-line blocks.
    fn table(&self, columns: &[Vec<usize>]) -> bool {
        // (baseline, size, column) of every line.
        let mut rows: Vec<(f32, f32, usize)> = Vec::new();
        for (c, col) in columns.iter().enumerate() {
            for &b in col {
                for &l in &self.blocks[b].lines {
                    rows.push((self.lines[l].base, self.lines[l].size, c));
                }
            }
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        let max_size = rows.iter().map(|r| r.1).fold(0.0, f32::max);
        let reach = SAME_LINE * max_size;
        let mut unpaired = vec![false; columns.len()];
        for &(base, size, c) in &rows {
            let lo = rows.partition_point(|r| r.0 < base - reach);
            unpaired[c] |= !rows[lo..]
                .iter()
                .take_while(|r| r.0 <= base + reach)
                .any(|r| r.2 != c && (r.0 - base).abs() <= SAME_LINE * size.max(r.1));
        }
        columns.iter().zip(unpaired).any(|(col, unpaired)| {
            let b = |i: &usize| &self.blocks[*i];
            let width = col.iter().map(|i| b(i).u1).fold(f32::MIN, f32::max)
                - col.iter().map(|i| b(i).u0).fold(f32::MAX, f32::min);
            !unpaired
                && (col.iter().all(|i| b(i).lines.len() == 1)
                    || width < CELL_WIDTH * self.median_size(col))
        })
    }

    fn emit(self, page: &mut TextPage) {
        let rtl = self.lines.iter().map(|l| self.direction(l)).sum::<i32>() < 0;
        let mut order = Vec::with_capacity(self.blocks.len());
        self.xy_cut((0..self.blocks.len()).collect(), rtl, &mut order);
        let mut items = Vec::new();
        for unit in order {
            match unit {
                Unit::Block(b) => {
                    let block = &self.blocks[b];
                    let lines = block.lines.iter().map(|&l| &self.lines[l]);
                    self.emit_block(lines, block.u0, block.u1, &mut items, page);
                }
                Unit::Rows(ids) => {
                    let rows = self.rows(&ids);
                    let (u0, u1) = rows
                        .iter()
                        .fold((f32::MAX, f32::MIN), |(a, b), l| (a.min(l.u0), b.max(l.u1)));
                    self.emit_block(&rows, u0, u1, &mut items, page);
                }
            }
        }
    }

    /// The lines of table blocks joined into rows by baseline.
    fn rows(&self, ids: &[usize]) -> Vec<Line> {
        let mut lines: Vec<&Line> = ids
            .iter()
            .flat_map(|&b| &self.blocks[b].lines)
            .map(|&l| &self.lines[l])
            .collect();
        lines.sort_by(|a, b| b.base.total_cmp(&a.base));
        let mut rows = Vec::new();
        let mut i = 0;
        while i < lines.len() {
            let first = lines[i];
            let mut items = Vec::new();
            while let Some(l) = lines.get(i)
                && (first.base - l.base).abs() <= SAME_LINE * first.size.max(l.size)
            {
                items.extend_from_slice(&l.items);
                i += 1;
            }
            items.sort_by(|&a, &b| self.geo[a].u0.total_cmp(&self.geo[b].u0));
            rows.push(self.line(&items));
        }
        rows
    }

    /// Emits a block's lines, starting a paragraph at each indented line after one that is
    /// not, unless the paragraph hangs (its first line out, the rest indented alike); a
    /// hanging paragraph ends at the next line out.
    fn emit_block<'l>(
        &self,
        lines: impl IntoIterator<Item = &'l Line>,
        u0: f32,
        u1: f32,
        items: &mut Vec<Item>,
        page: &mut TextPage,
    ) {
        let lines: Vec<&Line> = lines.into_iter().collect();
        let indents: Vec<f32> = lines
            .iter()
            .map(|l| {
                if self.line_rtl(l) {
                    u1 - l.u1
                } else {
                    l.u0 - u0
                }
            })
            .collect();
        // Whether line k, indented after one that is not, continues a hanging paragraph:
        // the next line is indented as far, or k is the last and the one before it full.
        let hangs = |k: usize| {
            let size = lines[k].size;
            match lines.get(k + 1) {
                Some(n) => {
                    indents[k + 1] > INDENT * n.size
                        && (indents[k + 1] - indents[k]).abs() < 0.3 * size
                }
                None => {
                    let prev = lines[k - 1];
                    let end = if self.line_rtl(prev) {
                        u0.max(prev.u0) - u0
                    } else {
                        u1 - prev.u1
                    };
                    end < 2.0 * size
                }
            }
        };
        let mut first = page.lines.len() as u32;
        let mut prev_indent = 0.0;
        let mut hanging = false;
        for (k, &line) in lines.iter().enumerate() {
            let rtl = self.line_rtl(line);
            let indent = indents[k];
            let out = indent < 0.3 * line.size;
            let starts = if k == 0 {
                false
            } else if indent > INDENT * line.size && prev_indent < 0.3 * line.size {
                hanging = hangs(k);
                !hanging
            } else if hanging && out && prev_indent > INDENT * line.size {
                hanging = false;
                true
            } else {
                false
            };
            if starts {
                close_block(page, first);
                first = page.lines.len() as u32;
            }
            prev_indent = indent;
            if !page.text.is_empty() {
                page.text.push('\n');
                if page.lines.len() as u32 == first {
                    page.text.push('\n');
                }
            }
            self.emit_line(line, rtl, items, page);
        }
        close_block(page, first);
    }

    /// Left-to-right glyphs less right-to-left ones.
    fn direction(&self, line: &Line) -> i32 {
        line.items
            .iter()
            .map(|&i| i32::from(self.placed[i].bidi))
            .sum()
    }

    fn line_rtl(&self, line: &Line) -> bool {
        self.direction(line) < 0
    }

    fn emit_line(&self, line: &Line, rtl: bool, items: &mut Vec<Item>, page: &mut TextPage) {
        // Visual order with spaces for gaps; overprinted duplicates (fake bold) read once.
        items.clear();
        let mut prev: Option<usize> = None;
        for &i in &line.items {
            let p = &self.placed[i];
            let t = self.text(i);
            if let Some(q) = prev {
                let (gp, gq) = (self.geo[i], self.geo[q]);
                let size = p.size.min(self.placed[q].size);
                if (gp.u0 - gq.u0).abs() < 0.1 * size
                    && (gp.base - gq.base).abs() < 0.1 * size
                    && !t.is_empty()
                    && t == self.text(q)
                {
                    continue;
                }
                if !p.space && !self.placed[q].space && gp.u0 - gq.u1 > SPACE_GAP * size {
                    items.push(Item::Gap(q, i));
                }
            }
            items.push(Item::Glyph(i));
            prev = Some(i);
        }
        if rtl
            || items
                .iter()
                .any(|it| matches!(it, Item::Glyph(i) if self.placed[*i].bidi < 0))
        {
            self.reorder(items, rtl);
        }
        let first = page.chars.len() as u32;
        let first_word = page.words.len() as u32;
        // The open word's first character and box, and the line's box.
        let mut word: Option<(u32, Rect)> = None;
        let mut line_box: Option<Rect> = None;
        for &it in items.iter() {
            let s = page.text.len() as u32;
            let (c, space) = match it {
                Item::Glyph(i) => {
                    let p = &self.placed[i];
                    page.text.push_str(self.text(i));
                    let c = TextChar {
                        text: (s, page.text.len() as u32),
                        quad: p.quad,
                        glyph: p.glyph,
                    };
                    (c, p.space)
                }
                Item::Gap(a, b) => {
                    let (qa, qb) = (self.placed[a].quad, self.placed[b].quad);
                    page.text.push(' ');
                    let quad = if self.geo[a].u0 <= self.geo[b].u0 {
                        [qa[1], qb[0], qb[3], qa[2]]
                    } else {
                        [qb[1], qa[0], qa[3], qb[2]]
                    };
                    let c = TextChar {
                        text: (s, s + 1),
                        quad,
                        glyph: NONE,
                    };
                    (c, true)
                }
            };
            let k = page.chars.len() as u32;
            let b = c.bbox();
            page.chars.push(c);
            line_box = Some(line_box.map_or(b, |l| l.union(&b)));
            match (space, &mut word) {
                (true, Some((w, wb))) => {
                    page.words.push(TextWord {
                        chars: (*w, k),
                        bbox: *wb,
                    });
                    word = None;
                }
                (false, None) => word = Some((k, b)),
                (false, Some((_, wb))) => *wb = wb.union(&b),
                _ => {}
            }
        }
        let last = page.chars.len() as u32;
        if let Some((w, bbox)) = word {
            page.words.push(TextWord {
                chars: (w, last),
                bbox,
            });
        }
        if let Some(bbox) = line_box {
            page.lines.push(TextLine {
                chars: (first, last),
                words: (first_word, page.words.len() as u32),
                bbox,
                vertical: self.vertical,
                rtl,
            });
        }
    }

    /// Visual to logical order: the bidi algorithm's reordering, applied to text laid out
    /// visually, restores the logical order of runs.
    fn reorder(&self, items: &mut [Item], rtl: bool) {
        let mut line = String::new();
        let mut starts = Vec::with_capacity(items.len());
        for it in items.iter() {
            starts.push(line.len());
            match *it {
                Item::Glyph(i) => line.push_str(self.text(i)),
                Item::Gap(..) => line.push(' '),
            }
        }
        if line.is_empty() {
            return;
        }
        let level = if rtl { Level::rtl() } else { Level::ltr() };
        let info = ParagraphBidiInfo::new(&line, Some(level));
        let levels = info.reordered_levels(0..line.len());
        let mut lv: Vec<u8> = starts
            .iter()
            .map(|&s| levels.get(s).map_or(0, |l| l.number()))
            .collect();
        // Rule L2: from the highest level to the lowest odd one, reverse every run at or above it.
        let max = lv.iter().copied().max().unwrap_or(0);
        let min_odd = lv
            .iter()
            .copied()
            .filter(|l| l % 2 == 1)
            .min()
            .unwrap_or(max + 1);
        for level in (min_odd..=max).rev() {
            let mut i = 0;
            while i < items.len() {
                if lv[i] < level {
                    i += 1;
                    continue;
                }
                let j = (i..items.len())
                    .find(|&j| lv[j] < level)
                    .unwrap_or(items.len());
                items[i..j].reverse();
                lv[i..j].reverse();
                i = j;
            }
        }
    }
}

/// +1 for text whose first strong character is left to right, -1 for right to left, 0
/// for text with none.
fn text_direction(t: &str) -> i8 {
    t.chars()
        .find_map(|c| match c {
            // The only strong ASCII characters are letters, all left to right.
            _ if c.is_ascii() => c.is_ascii_alphabetic().then_some(1),
            _ => match bidi_class(c) {
                BidiClass::L => Some(1),
                BidiClass::R | BidiClass::AL => Some(-1),
                _ => None,
            },
        })
        .unwrap_or(0)
}

/// What the reading order emits next.
enum Unit {
    Block(usize),
    /// Blocks that are cells of table rows, read row by row.
    Rows(Vec<usize>),
}

#[derive(Clone, Copy)]
enum Item {
    Glyph(usize),
    /// A space for the gap between two glyphs.
    Gap(usize, usize),
}

fn close_block(page: &mut TextPage, first: u32) {
    let last = page.lines.len() as u32;
    if last > first {
        let bbox = page.lines[first as usize..last as usize]
            .iter()
            .map(|l| l.bbox)
            .reduce(|a, b| a.union(&b))
            .unwrap_or_default();
        page.blocks.push(TextBlock {
            lines: (first, last),
            bbox,
        });
    }
}

/// Splits ids sorted by the start of their extent where the next starts more than `gap`
/// past the furthest end so far; also returns the widest such gap.
fn split(ids: &[usize], gap: f32, extent: impl Fn(usize) -> (f32, f32)) -> (Vec<Vec<usize>>, f32) {
    let mut parts: Vec<Vec<usize>> = Vec::new();
    let (mut end, mut widest) = (f32::NEG_INFINITY, 0.0f32);
    for &i in ids {
        let (a, b) = extent(i);
        match parts.last_mut() {
            Some(p) if a - end <= gap => p.push(i),
            last => {
                if last.is_some() {
                    widest = widest.max(a - end);
                }
                parts.push(vec![i]);
            }
        }
        end = end.max(b);
    }
    (parts, widest)
}

/// A key whose unsigned order is the [`f32::total_cmp`] order.
fn total_order(f: f32) -> u32 {
    let b = f.to_bits();
    if b >> 31 == 1 { !b } else { b | 1 << 31 }
}

fn min4(v: [f32; 4]) -> f32 {
    v.into_iter().fold(f32::INFINITY, f32::min)
}

fn max4(v: [f32; 4]) -> f32 {
    v.into_iter().fold(f32::NEG_INFINITY, f32::max)
}

/// Latin ligatures read as their letters, so search finds them.
fn expand_ligature(s: &mut String, start: usize) {
    let mut it = s[start..].chars();
    let (Some(c), None) = (it.next(), it.next()) else {
        return;
    };
    let letters = match c {
        '\u{FB00}' => "ff",
        '\u{FB01}' => "fi",
        '\u{FB02}' => "fl",
        '\u{FB03}' => "ffi",
        '\u{FB04}' => "ffl",
        '\u{FB05}' | '\u{FB06}' => "st",
        _ => return,
    };
    s.truncate(start);
    s.push_str(letters);
}

/// A PDF text string: UTF-16BE or UTF-8 with a byte order mark, else PDFDocEncoding.
pub fn text_string(o: &Object) -> String {
    let Some(b) = o.as_string() else {
        return String::new();
    };
    if let Some(u) = b.strip_prefix(&[0xFE, 0xFF]) {
        let units = u
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], *c.get(1).unwrap_or(&0)]));
        return char::decode_utf16(units)
            .map(|c| c.unwrap_or('\u{FFFD}'))
            .collect();
    }
    if let Some(u) = b.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(u).into_owned();
    }
    b.iter()
        .map(|&c| pagebara_syntax::text::pdfdoc_char(c))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_directions_match_bidi_classes() {
        for c in (0u8..128).map(char::from) {
            let strong = match bidi_class(c) {
                BidiClass::L => 1,
                BidiClass::R | BidiClass::AL => -1,
                _ => 0,
            };
            assert_eq!(text_direction(c.encode_utf8(&mut [0; 4])), strong, "{c:?}");
        }
        assert_eq!(text_direction(". \u{5D0}a"), -1);
        assert_eq!(text_direction("1\u{627}"), -1);
        assert_eq!(text_direction("12 ,"), 0);
    }

    #[test]
    fn total_order_keys_sort_like_total_cmp() {
        let v = [
            f32::NAN,
            -f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            0.0,
            -0.0,
            1.5,
            -1.5,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            f32::MAX,
            f32::MIN,
        ];
        for a in v {
            for b in v {
                assert_eq!(
                    total_order(a).cmp(&total_order(b)),
                    a.total_cmp(&b),
                    "{a} {b}"
                );
            }
        }
    }
}
