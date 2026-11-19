//! The invisible text layer: recognised words drawn in text render mode 3 with an
//! embedded "glyphless" Type 0 font, so extraction, search and selection see them.
//!
//! The font has one empty glyph per distinct character (all 500 units wide); the
//! [`Embedder`] writes it as an Identity-H subset with `/ToUnicode`. Each word gets its own
//! text matrix along its box, with horizontal scaling (`Tz`) stretching it to the box width.

use std::collections::BTreeMap;
use std::f32::consts::FRAC_PI_2;
use std::fmt::Write as _;

use pagebara_content::Point;
use pagebara_font::subset::Embedder;
use pagebara_pages::PageTree;
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

use crate::Error;
use crate::page::PageText;

/// Glyph advance in font units (1000 per em).
const ADVANCE: u16 = 500;
const ASCENT: i16 = 800;
const DESCENT: i16 = -200;
/// Lines within this angle of a page axis are written along it.
const SNAP_DEGREES: f32 = 3.0;

/// Adds the words of `pages` to their pages as invisible text. Pages without words are
/// left alone.
pub fn add_text_layer(doc: &mut Document, pages: &[PageText]) -> Result<(), Error> {
    let pages: Vec<&PageText> = pages.iter().filter(|p| p.word_count() > 0).collect();
    if pages.is_empty() {
        return Ok(());
    }
    // One glyph per distinct character over all pages, the space included.
    let mut gids: BTreeMap<char, u16> = BTreeMap::new();
    gids.insert(' ', 0);
    for p in &pages {
        for l in &p.lines {
            for w in &l.words {
                for c in w.text.chars() {
                    gids.insert(c, 0);
                }
            }
        }
    }
    if gids.len() >= 0xFFFF {
        return Err(Error::TooManyGlyphs);
    }
    for (i, g) in gids.values_mut().enumerate() {
        *g = i as u16 + 1;
    }
    let font = glyphless_font(gids.len() as u16 + 1);
    let mut emb = Embedder::new(font, 0).ok_or(Error::Font)?;
    let mut buf = [0; 4];
    for (&c, &g) in &gids {
        emb.use_glyph(g, c.encode_utf8(&mut buf));
    }
    let font = emb.write(doc).ok_or(Error::Font)?;

    let tree = PageTree::open(doc);
    let save = doc.add(Object::Stream(Stream::new(Dict::new(), b"q\n".to_vec())));
    for p in pages {
        let page = tree.page(p.page).ok_or(Error::NoSuchPage(p.page))?;
        let (mut d, mut res) =
            crate::page::page_dict(doc, &page).ok_or(Error::NoSuchPage(p.page))?;
        let mut fonts = doc
            .get_in(&res, b"Font")
            .as_dict()
            .cloned()
            .unwrap_or_default();
        let name = (0..)
            .map(|i| format!("OCR{i}"))
            .find(|n| !fonts.contains(n.as_bytes()))
            .unwrap_or_default();
        fonts.set(name.as_bytes(), Object::Ref(font));
        res.set(b"Font", Object::Dict(fonts));
        d.set(b"Resources", Object::Dict(res));

        let text = content(p, &name, &gids);
        let z = miniz_oxide::deflate::compress_to_vec_zlib(text.as_bytes(), 6);
        let mut sd = Dict::new();
        sd.set(b"Filter", Object::name(b"FlateDecode"));
        let ours = doc.add(Object::Stream(Stream::new(sd, z)));

        let mut list = vec![Object::Ref(save)];
        list.extend(existing_contents(doc, &d));
        list.push(Object::Ref(ours));
        d.set(b"Contents", Object::Array(list));
        doc.set(page.r, Object::Dict(d));
    }
    Ok(())
}

fn existing_contents(doc: &Document, d: &Dict) -> Vec<Object> {
    match d.get(b"Contents") {
        Some(Object::Array(a)) => a.clone(),
        Some(o @ Object::Ref(r)) => match doc.get(*r).as_deref() {
            Some(Object::Array(a)) => a.clone(),
            _ => vec![o.clone()],
        },
        _ => Vec::new(),
    }
}

fn num(out: &mut String, v: f32) {
    let v = if v.is_finite() { v } else { 0.0 };
    let s = format!("{:.3}", v);
    let s = s.trim_end_matches('0').trim_end_matches('.');
    out.push_str(if s == "-0" || s.is_empty() { "0" } else { s });
}

/// A word's box: start of its bottom edge, unit vectors along and up, and size.
struct Placed {
    origin: Point,
    e: (f32, f32),
    f: (f32, f32),
    width: f32,
    height: f32,
}

impl Placed {
    fn centre(&self) -> Point {
        Point::new(
            self.origin.x + (self.e.0 * self.width + self.f.0 * self.height) / 2.0,
            self.origin.y + (self.e.1 * self.width + self.f.1 * self.height) / 2.0,
        )
    }
}

fn dot(p: Point, d: (f32, f32)) -> f32 {
    p.x * d.0 + p.y * d.1
}

/// Where the words of a line go. A line within [`SNAP_DEGREES`] of a page axis (a slightly
/// skewed scan) is written straight along that axis at its mean height, since extractors
/// split slanted lines; other lines keep their angle.
fn place(words: &[crate::page::Word]) -> Vec<Option<Placed>> {
    let mut out: Vec<Option<Placed>> = words
        .iter()
        .map(|w| {
            let [bl, br, _, tl] = w.quad;
            let (ux, uy) = (br.x - bl.x, br.y - bl.y);
            let (vx, vy) = (tl.x - bl.x, tl.y - bl.y);
            let (width, height) = (ux.hypot(uy), vx.hypot(vy));
            let ok = width > 0.0 && height > 0.0 && width.is_finite() && height.is_finite();
            ok.then_some(Placed {
                origin: bl,
                e: (ux / width, uy / width),
                f: (vx / height, vy / height),
                width,
                height,
            })
        })
        .collect();
    let Some(first) = out.iter().flatten().next() else {
        return out;
    };
    let angle = first.e.1.atan2(first.e.0);
    let quarter = (angle / FRAC_PI_2).round() * FRAC_PI_2;
    if (angle - quarter).abs() > SNAP_DEGREES.to_radians() {
        return out;
    }
    let (sin, cos) = quarter.sin_cos();
    let e = (cos.round(), sin.round());
    let f = (-e.1, e.0);
    let n = out.iter().flatten().count() as f32;
    let across = out
        .iter()
        .flatten()
        .map(|p| dot(p.centre(), f))
        .sum::<f32>()
        / n;
    let height = out.iter().flatten().map(|p| p.height).sum::<f32>() / n;
    for p in out.iter_mut().flatten() {
        let along = dot(p.centre(), e) - p.width / 2.0;
        let up = across - height / 2.0;
        p.origin = Point::new(e.0 * along + f.0 * up, e.1 * along + f.1 * up);
        p.e = e;
        p.f = f;
        p.height = height;
    }
    out
}

/// The page's text layer content (after the original content, restored by `Q`).
fn content(p: &PageText, font: &str, gids: &BTreeMap<char, u16>) -> String {
    let mut s = String::from("Q\nq\nBT\n3 Tr\n");
    for l in &p.lines {
        for (i, (w, pl)) in l.words.iter().zip(place(&l.words)).enumerate() {
            let Some(pl) = pl else { continue };
            let mut text: Vec<char> = w.text.chars().collect();
            let n = text.len() as f32;
            if i + 1 < l.words.len() {
                text.push(' ');
            }
            let size = pl.height;
            let tz = 100.0 * pl.width / (n * f32::from(ADVANCE) / 1000.0 * size);
            let base = -f32::from(DESCENT) / 1000.0 * size;
            let (ox, oy) = (pl.origin.x + pl.f.0 * base, pl.origin.y + pl.f.1 * base);
            let _ = write!(s, "/{font} ");
            num(&mut s, size);
            s.push_str(" Tf ");
            num(&mut s, tz);
            s.push_str(" Tz ");
            for v in [pl.e.0, pl.e.1, pl.f.0, pl.f.1, ox, oy] {
                num(&mut s, v);
                s.push(' ');
            }
            s.push_str("Tm <");
            for c in text {
                let g = gids.get(&c).copied().unwrap_or(0);
                let _ = write!(s, "{g:04X}");
            }
            s.push_str("> Tj\n");
        }
    }
    s.push_str("ET\nQ\n");
    s
}

/// A TrueType font of `n` glyphs: `.notdef` a box, the rest empty, all [`ADVANCE`] wide.
fn glyphless_font(n: u16) -> Vec<u8> {
    let mut head = Vec::new();
    for v in [0x0001_0000u32, 0x0001_0000, 0, 0x5F0F_3CF5] {
        head.extend(v.to_be_bytes());
    }
    for v in [0x000Bu16, 1000] {
        head.extend(v.to_be_bytes());
    }
    head.extend([0; 16]); // created, modified
    for v in [0i16, DESCENT, ADVANCE as i16, ASCENT] {
        head.extend(v.to_be_bytes());
    }
    for v in [0u16, 6, 2, 0, 0] {
        // macStyle, lowestRecPPEM, fontDirectionHint, indexToLocFormat, glyphDataFormat
        head.extend(v.to_be_bytes());
    }

    let mut hhea = Vec::new();
    hhea.extend(0x0001_0000u32.to_be_bytes());
    for v in [ASCENT, DESCENT, 0] {
        hhea.extend(v.to_be_bytes());
    }
    for v in [ADVANCE, 0, 0, ADVANCE, 1, 0, 0, 0, 0, 0, 0, 0, n] {
        hhea.extend(v.to_be_bytes());
    }

    let mut maxp = Vec::new();
    maxp.extend(0x0001_0000u32.to_be_bytes());
    for v in [n, 4, 1, 0, 0, 2, 0, 0, 0, 0, 0, 0, 0, 0] {
        maxp.extend(v.to_be_bytes());
    }

    let mut hmtx = Vec::with_capacity(usize::from(n) * 4);
    for _ in 0..n {
        hmtx.extend(ADVANCE.to_be_bytes());
        hmtx.extend(0u16.to_be_bytes());
    }

    // .notdef: one closed contour, four on-curve points with word deltas.
    let mut glyf = Vec::new();
    for v in [1i16, 0, DESCENT, ADVANCE as i16, ASCENT, 3, 0] {
        glyf.extend(v.to_be_bytes());
    }
    glyf.extend([1u8; 4]);
    for v in [0i16, ADVANCE as i16, 0, -(ADVANCE as i16)] {
        glyf.extend(v.to_be_bytes());
    }
    for v in [DESCENT, 0, ASCENT - DESCENT, 0] {
        glyf.extend(v.to_be_bytes());
    }
    let len = glyf.len().div_ceil(4) * 4;
    glyf.resize(len, 0);
    let mut loca = Vec::with_capacity((usize::from(n) + 1) * 2);
    loca.extend(0u16.to_be_bytes());
    for _ in 0..n {
        loca.extend(((len / 2) as u16).to_be_bytes());
    }

    // A (3,1) format 4 cmap with only the closing segment.
    let mut cmap = Vec::new();
    for v in [0u16, 1, 3, 1] {
        cmap.extend(v.to_be_bytes());
    }
    cmap.extend(12u32.to_be_bytes());
    for v in [4u16, 24, 0, 2, 2, 0, 0, 0xFFFF, 0, 0xFFFF, 1, 0] {
        cmap.extend(v.to_be_bytes());
    }

    let mut post = Vec::new();
    post.extend(0x0003_0000u32.to_be_bytes());
    post.extend(0u32.to_be_bytes());
    for v in [-100i16, 50] {
        post.extend(v.to_be_bytes());
    }
    post.extend(1u32.to_be_bytes());
    post.extend([0; 16]);

    sfnt(vec![
        (*b"cmap", cmap),
        (*b"glyf", glyf),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"loca", loca),
        (*b"maxp", maxp),
        (*b"post", post),
    ])
}

/// An sfnt from tables sorted by tag (checksums are not needed by readers here).
fn sfnt(tables: Vec<([u8; 4], Vec<u8>)>) -> Vec<u8> {
    let n = tables.len() as u16;
    let mut out = Vec::new();
    out.extend(0x0001_0000u32.to_be_bytes());
    let mut sr = 1u16;
    let mut es = 0u16;
    while sr * 2 <= n {
        sr *= 2;
        es += 1;
    }
    for v in [n, sr * 16, es, n * 16 - sr * 16] {
        out.extend(v.to_be_bytes());
    }
    let mut off = 12 + 16 * tables.len();
    for (tag, d) in &tables {
        out.extend(tag);
        out.extend(checksum(d).to_be_bytes());
        out.extend((off as u32).to_be_bytes());
        out.extend((d.len() as u32).to_be_bytes());
        off += d.len().div_ceil(4) * 4;
    }
    for (_, d) in &tables {
        out.extend(d);
        out.resize(out.len().div_ceil(4) * 4, 0);
    }
    out
}

fn checksum(d: &[u8]) -> u32 {
    d.chunks(4).fold(0u32, |s, c| {
        let mut b = [0u8; 4];
        b[..c.len()].copy_from_slice(c);
        s.wrapping_add(u32::from_be_bytes(b))
    })
}

/// The reference of the font a layer added to a page, if any (for tests and tools).
pub fn layer_font(doc: &Document, page: usize) -> Option<ObjRef> {
    let tree = PageTree::open(doc);
    let (_, res) = crate::page::page_dict(doc, &tree.page(page)?)?;
    let fonts = doc.get_in(&res, b"Font");
    let fonts = fonts.as_dict()?;
    fonts
        .iter()
        .find(|(k, _)| k.as_bytes().starts_with(b"OCR"))
        .and_then(|(_, v)| v.as_objref())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagebara_font::Font;

    #[test]
    fn font_parses() {
        let f = glyphless_font(5);
        let Some(Font::Sfnt(s)) = Font::parse_index(f, 0) else {
            panic!("not a font");
        };
        assert_eq!(s.num_glyphs(), 5);
        assert_eq!(s.advance(3), Some(ADVANCE));
        assert_eq!(s.units_per_em(), 1000);
    }

    #[test]
    fn numbers() {
        let mut s = String::new();
        for v in [1.0, -0.0001, 12.3456, f32::NAN] {
            num(&mut s, v);
            s.push(' ');
        }
        assert_eq!(s, "1 0 12.346 0 ");
    }
}
