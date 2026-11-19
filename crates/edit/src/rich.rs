//! Styled text: plain-text changes that keep the styles around them, and style changes of
//! ranges (bold, italic, size, colour, font family).

use std::ops::Range;

use pagebara_syntax::{Document, Object};
use pagebara_typeset::{FontStyle, VariantSource, find_variant, font_style, same_family};

use crate::fonts::{Fonts, SpanFont};
use crate::model::Color;
use crate::paragraph::{SpanStyle, StyledText, merge, paint_of};
use crate::{Error, Result};

/// A change of style.
#[derive(Clone, Debug, PartialEq)]
pub enum StyleChange {
    /// The family's bold (or regular) face, else emulated bold.
    Bold(bool),
    /// The family's italic (or upright) face, else a slanted one.
    Italic(bool),
    /// Font size in page units.
    Size(f32),
    Fill(Color),
    /// A font family by name: a font of the document in that family, else an installed
    /// face; bold and italic are kept.
    Family(String),
    Font(SpanFont),
}

/// The spans of `spans` within a byte range of their text.
pub fn slice(spans: &[StyledText], r: Range<usize>) -> Vec<StyledText> {
    let mut out = Vec::new();
    let mut off = 0;
    for s in spans {
        let (a, b) = (off, off + s.text.len());
        off = b;
        let (x, y) = (r.start.max(a), r.end.min(b));
        if x < y {
            out.push(StyledText {
                text: s.text[x - a..y - a].to_owned(),
                style: s.style.clone(),
            });
        }
    }
    out
}

/// The style at a byte offset (of the character there, or the last one).
pub fn style_at(spans: &[StyledText], at: usize) -> Option<&SpanStyle> {
    let mut off = 0;
    for s in spans {
        off += s.text.len();
        if at < off {
            return Some(&s.style);
        }
    }
    spans.last().map(|s| &s.style)
}

/// `spans` with their text replaced by `new`: the common start and end keep their styles,
/// the changed middle takes the style of the character before it (or after, at the start).
pub fn restyle_plain(spans: &[StyledText], new: &str) -> Vec<StyledText> {
    let old: String = spans.iter().map(|s| s.text.as_str()).collect();
    let mut a = 0;
    for ((i, x), y) in old.char_indices().zip(new.chars()) {
        if x != y {
            break;
        }
        a = i + x.len_utf8();
    }
    let mut b = 0;
    for (x, y) in old[a..].chars().rev().zip(new[a..].chars().rev()) {
        if x != y {
            break;
        }
        b += x.len_utf8();
    }
    let Some(st) = style_at(spans, a.saturating_sub(1)).cloned() else {
        return Vec::new();
    };
    let mut out = slice(spans, 0..a);
    out.push(StyledText {
        text: new[a..new.len() - b].to_owned(),
        style: st,
    });
    out.extend(slice(spans, old.len() - b..old.len()));
    merge(&out)
}

/// `spans` with `change` applied to a byte range of their text.
pub fn restyle_range(
    doc: &Document,
    fonts: &mut Fonts,
    spans: &[StyledText],
    r: Range<usize>,
    change: &StyleChange,
) -> Result<Vec<StyledText>> {
    let len: usize = spans.iter().map(|s| s.text.len()).sum();
    let r = r.start.min(len)..r.end.min(len);
    let mut out = slice(spans, 0..r.start);
    for s in slice(spans, r.clone()) {
        out.push(StyledText {
            style: restyle(doc, fonts, &s.style, change)?,
            text: s.text,
        });
    }
    out.extend(slice(spans, r.end..len));
    Ok(merge(&out))
}

/// A style with `change` applied.
pub fn restyle(
    doc: &Document,
    fonts: &mut Fonts,
    style: &SpanStyle,
    change: &StyleChange,
) -> Result<SpanStyle> {
    let mut out = style.clone();
    match change {
        StyleChange::Size(s) if s.is_finite() && *s > 0.0 => out.size = *s,
        StyleChange::Size(_) => {}
        StyleChange::Fill(c) => out.fill = paint_of(*c),
        StyleChange::Font(f) => {
            out.font = f.clone();
            out.synthetic = Default::default();
        }
        StyleChange::Bold(_) | StyleChange::Italic(_) => {
            let id = fonts.open(doc, &style.font)?;
            let Some(fs) = fonts.session().style(id).cloned() else {
                return Ok(out);
            };
            let bold = fs.is_bold() || style.synthetic.bold;
            let italic = fs.italic || style.synthetic.italic;
            let (b, i) = match change {
                StyleChange::Bold(b) => (*b, italic),
                StyleChange::Italic(i) => (bold, *i),
                _ => (bold, italic),
            };
            let db = fonts.db().cloned();
            let v = find_variant(doc, db.as_deref(), &fs, b, i);
            out.font = variant_font(&style.font, v.source);
            out.synthetic = v.synthetic;
        }
        StyleChange::Family(name) => {
            let id = fonts.open(doc, &style.font)?;
            let fs = fonts.session().style(id).cloned();
            let bold = fs.as_ref().is_some_and(FontStyle::is_bold) || style.synthetic.bold;
            let italic = fs.as_ref().is_some_and(|f| f.italic) || style.synthetic.italic;
            let font = family_font(doc, fonts, name, bold, italic)
                .ok_or(Error::Unsupported("font family not found"))?;
            let id = fonts.open(doc, &font)?;
            let fs = fonts.session().style(id).cloned();
            out.synthetic = pagebara_typeset::Synthetic {
                bold: bold && !fs.as_ref().is_some_and(FontStyle::is_bold),
                italic: italic && !fs.as_ref().is_some_and(|f| f.italic),
            };
            out.font = font;
        }
    }
    Ok(out)
}

fn variant_font(base: &SpanFont, v: VariantSource) -> SpanFont {
    match v {
        VariantSource::Same | VariantSource::Base => base.clone(),
        VariantSource::Document(r) => SpanFont::Pdf(Object::Ref(r)),
        VariantSource::System(f) => SpanFont::Face(f),
        VariantSource::Standard(n) => {
            SpanFont::Pdf(Object::Dict(pagebara_typeset::standard_font_dict(n)))
        }
    }
}

/// Font dictionaries of the document: (reference, style).
fn document_fonts(doc: &Document) -> Vec<(pagebara_syntax::ObjRef, FontStyle)> {
    let mut out = Vec::new();
    for n in doc.object_numbers() {
        let Some(r) = doc.reference(n) else { continue };
        let Some(o) = doc.get(r) else { continue };
        let Object::Dict(d) = &*o else { continue };
        if d.name(b"Type") != Some(b"Font")
            || !matches!(d.name(b"Subtype"), Some(b"Type1" | b"TrueType" | b"Type0"))
        {
            continue;
        }
        out.push((r, font_style(doc, d)));
    }
    out
}

/// The best font of a family for a style: the document's, else an installed face, else a
/// standard 14 font.
fn family_font(
    doc: &Document,
    fonts: &Fonts,
    name: &str,
    bold: bool,
    italic: bool,
) -> Option<SpanFont> {
    let score = |b: bool, i: bool| u8::from(b != bold) + u8::from(i != italic);
    let in_doc = document_fonts(doc)
        .into_iter()
        .filter(|(_, s)| same_family(&s.family, name))
        .min_by_key(|(_, s)| (score(s.is_bold(), s.italic), u8::from(s.subset)));
    if let Some((r, s)) = &in_doc
        && score(s.is_bold(), s.italic) == 0
    {
        return Some(SpanFont::Pdf(Object::Ref(*r)));
    }
    let installed = fonts.db().and_then(|db| {
        db.faces()
            .iter()
            .filter(|f| f.sfnt && f.embeddable() && same_family(&f.family, name))
            .min_by_key(|f| {
                (
                    score(f.weight >= 600, f.italic),
                    f.weight.abs_diff(if bold { 700 } else { 400 }),
                )
            })
            .cloned()
    });
    match (in_doc, installed) {
        (Some((r, s)), Some(f)) => {
            if score(s.is_bold(), s.italic) <= score(f.weight >= 600, f.italic) {
                Some(SpanFont::Pdf(Object::Ref(r)))
            } else {
                Some(SpanFont::Face(f))
            }
        }
        (Some((r, _)), None) => Some(SpanFont::Pdf(Object::Ref(r))),
        (None, Some(f)) => Some(SpanFont::Face(f)),
        (None, None) => pagebara_typeset::standard_name(name, bold, italic)
            .map(|n| SpanFont::Pdf(Object::Dict(pagebara_typeset::standard_font_dict(n)))),
    }
}

/// Font families to offer: the document's first (in order of appearance), then the
/// installed ones that can be embedded, by name.
pub fn font_families(doc: &Document, fonts: &Fonts) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let push = |out: &mut Vec<String>, f: &str| {
        if !f.is_empty() && !out.iter().any(|x| same_family(x, f)) {
            out.push(f.to_owned());
        }
    };
    for (_, s) in document_fonts(doc) {
        push(&mut out, &s.family);
    }
    let n = out.len();
    if let Some(db) = fonts.db() {
        let mut inst: Vec<&str> = db
            .faces()
            .iter()
            .filter(|f| f.sfnt && f.embeddable())
            .map(|f| f.family.as_str())
            .collect();
        inst.sort_unstable_by_key(|f| f.to_lowercase());
        for f in inst {
            push(&mut out, f);
        }
    }
    out[n..].sort_by_key(|f| f.to_lowercase());
    out
}
