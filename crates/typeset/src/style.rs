//! Style of a PDF font (family, weight, slant, class) and bold/italic variants: the
//! family's real face (in the document or installed) or a synthetic one.

use pagebara_font::Font;
use pagebara_font::fallback::{self, FaceInfo, FontDb, normalize, split_style, weight_from_name};
use pagebara_font::pdf::{self, strip_subset_tag};
use pagebara_syntax::{Dict, Document, ObjRef, Object};

/// Broad design class.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontClass {
    Serif,
    Sans,
    Mono,
    /// Symbol and dingbat fonts.
    Symbol,
}

/// What a font looks like, from its dictionary, descriptor and program.
#[derive(Clone, Debug, PartialEq)]
pub struct FontStyle {
    /// `/BaseFont` without subset tag (`TimesNewRomanPS-BoldMT`).
    pub base_font: String,
    /// The family part of the name (`TimesNewRomanPS`), or the program's family name.
    pub family: String,
    /// CSS weight, 100–900.
    pub weight: u16,
    pub italic: bool,
    pub class: FontClass,
    /// Italic angle in degrees (negative leans right).
    pub italic_angle: f32,
    /// x-height and cap height in thousandths of the font size, when known.
    pub x_height: Option<f32>,
    pub cap_height: Option<f32>,
    /// Embedded font program present.
    pub embedded: bool,
    /// The embedded program is a subset (tagged name).
    pub subset: bool,
}

impl FontStyle {
    pub fn is_bold(&self) -> bool {
        self.weight >= 600
    }
}

/// Reads the style of a font dictionary.
pub fn font_style(doc: &Document, dict: &Dict) -> FontStyle {
    let raw_name = |d: &Dict| {
        doc.get_in(d, b"BaseFont")
            .as_name()
            .map(|n| String::from_utf8_lossy(n).into_owned())
    };
    let descendant = match doc.get_in(dict, b"DescendantFonts").as_array() {
        Some([first, ..]) => doc.resolve(first).as_dict().cloned(),
        _ => None,
    };
    let raw = descendant
        .as_ref()
        .and_then(raw_name)
        .or_else(|| raw_name(dict))
        .unwrap_or_default();
    let subset = strip_subset_tag(&raw).len() != raw.len();
    let base_font = strip_subset_tag(&raw).to_owned();
    let fd = pdf::font_descriptor(doc, dict).unwrap_or_default();
    let num = |k: &[u8]| doc.get_in(&fd, k).as_f64().map(|v| v as f32);
    let flags = doc.get_in(&fd, b"Flags").as_int().unwrap_or(0) as u32;
    let program = pdf::embedded_program(doc, dict);
    let (fam, style) = split_style(&base_font);
    let mut family = fam.to_owned();
    if family.is_empty()
        && let Some(n) = program.as_ref().and_then(program_family)
    {
        family = n;
    }
    let os2 = program.as_ref().and_then(|p| p.as_sfnt()?.os2());
    let weight = num(b"FontWeight")
        .map(|w| w.clamp(100.0, 900.0) as u16)
        .or_else(|| weight_from_name(style).filter(|_| !style.is_empty()))
        .or_else(|| os2.map(|o| o.weight_class.clamp(100, 900)))
        .unwrap_or(if flags & (1 << 18) != 0 { 700 } else { 400 });
    let weight = if flags & (1 << 18) != 0 {
        weight.max(700)
    } else {
        weight
    };
    let style_l = style.to_ascii_lowercase();
    let italic_angle = num(b"ItalicAngle").unwrap_or(0.0);
    let italic = flags & 64 != 0
        || italic_angle != 0.0
        || style_l.contains("italic")
        || style_l.contains("oblique")
        || os2.is_some_and(|o| o.is_italic());
    let fam_l = normalize(&family);
    let class = if flags & 4 != 0
        && (fam_l.starts_with("symbol")
            || fam_l.contains("dingbat")
            || fam_l.starts_with("wingding"))
    {
        FontClass::Symbol
    } else if flags & 1 != 0
        || os2.is_some_and(|o| o.is_monospaced())
        || program
            .as_ref()
            .and_then(Font::as_sfnt)
            .is_some_and(|s| s.is_fixed_pitch())
        || ["courier", "mono", "consol", "typewriter", "code"]
            .iter()
            .any(|k| fam_l.contains(k))
    {
        FontClass::Mono
    } else if flags & 2 != 0
        || os2.and_then(|o| o.is_serif()) == Some(true)
        || [
            "times", "serif", "roman", "georgia", "garamond", "minion", "cambria", "book",
        ]
        .iter()
        .any(|k| fam_l.contains(k) && !fam_l.contains("sans"))
    {
        FontClass::Serif
    } else {
        FontClass::Sans
    };
    let upem_scale = program
        .as_ref()
        .and_then(Font::as_sfnt)
        .map_or(1.0, |s| 1000.0 / f32::from(s.units_per_em().max(1)));
    let x_height = num(b"XHeight").filter(|v| *v > 0.0).or_else(|| {
        os2.map(|o| f32::from(o.x_height) * upem_scale)
            .filter(|v| *v > 0.0)
    });
    let cap_height = num(b"CapHeight").filter(|v| *v > 0.0).or_else(|| {
        os2.map(|o| f32::from(o.cap_height) * upem_scale)
            .filter(|v| *v > 0.0)
    });
    FontStyle {
        base_font,
        family,
        weight,
        italic,
        class,
        italic_angle,
        x_height,
        cap_height,
        embedded: program.is_some(),
        subset,
    }
}

fn program_family(p: &Font) -> Option<String> {
    match p.as_sfnt() {
        Some(s) => s.family_name(),
        None => p.postscript_name().map(|n| split_style(&n).0.to_owned()),
    }
}

/// Synthetic emboldening and slanting, for when the family has no such face.
///
/// Bold: fill and stroke the glyphs (text render mode 2) with a line width of
/// [`Synthetic::stroke_width`] in the fill colour. Italic: shear the text matrix by
/// [`Synthetic::SKEW`]. Both are approximations and are marked as such so the UI can say
/// so.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Synthetic {
    pub bold: bool,
    pub italic: bool,
}

impl Synthetic {
    /// tan 12°, the usual oblique angle.
    pub const SKEW: f32 = 0.212_556_56;

    pub fn is_none(&self) -> bool {
        !self.bold && !self.italic
    }

    /// Text render mode: 2 (fill, then stroke) when bold, else 0.
    pub fn render_mode(&self) -> u8 {
        if self.bold { 2 } else { 0 }
    }

    /// Stroke width in text space units for font size `size` (3% of the size).
    pub fn stroke_width(&self, size: f32) -> f32 {
        if self.bold { size * 0.03 } else { 0.0 }
    }

    /// The text matrix placing the baseline origin at (x, y).
    pub fn text_matrix(&self, x: f32, y: f32) -> [f32; 6] {
        [
            1.0,
            0.0,
            if self.italic { Self::SKEW } else { 0.0 },
            1.0,
            x,
            y,
        ]
    }

    /// Extra advance a bold stroke adds to each glyph's ink (half the stroke on each
    /// side); layouts may add it as character spacing.
    pub fn extra_advance(&self, size: f32) -> f32 {
        self.stroke_width(size)
    }
}

/// Where a bold or italic variant comes from.
#[derive(Clone, Debug, PartialEq)]
pub enum VariantSource {
    /// The font already has the requested style.
    Same,
    /// Another font resource of the same family in the document.
    Document(ObjRef),
    /// An installed face of the family (open it with
    /// [`FontSession::open_face`](crate::FontSession::open_face)).
    System(FaceInfo),
    /// A standard 14 font (the base is a non-embedded standard font): add a font resource
    /// made by [`standard_font_dict`].
    Standard(&'static str),
    /// The base font itself, with synthetic styling only.
    Base,
}

/// The standard 14 font of a family (`Helvetica`, `Times`, `Courier`) in a style.
pub fn standard_name(family: &str, bold: bool, italic: bool) -> Option<&'static str> {
    let f = normalize(family);
    let names: [&'static str; 4] = if f.starts_with("helvetica") || f.starts_with("arial") {
        [
            "Helvetica",
            "Helvetica-Bold",
            "Helvetica-Oblique",
            "Helvetica-BoldOblique",
        ]
    } else if f.starts_with("times") {
        [
            "Times-Roman",
            "Times-Bold",
            "Times-Italic",
            "Times-BoldItalic",
        ]
    } else if f.starts_with("courier") {
        [
            "Courier",
            "Courier-Bold",
            "Courier-Oblique",
            "Courier-BoldOblique",
        ]
    } else {
        return None;
    };
    Some(names[usize::from(bold) | usize::from(italic) << 1])
}

/// A non-embedded standard font dictionary with `WinAnsiEncoding`.
pub fn standard_font_dict(name: &str) -> Dict {
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"Font"));
    d.set(b"Subtype", Object::name(b"Type1"));
    d.set(b"BaseFont", Object::name(name.as_bytes()));
    d.set(b"Encoding", Object::name(b"WinAnsiEncoding"));
    d
}

/// A variant: a real face, plus whatever synthetic styling it still needs.
#[derive(Clone, Debug, PartialEq)]
pub struct Variant {
    pub source: VariantSource,
    pub synthetic: Synthetic,
}

/// Finds the best way to show text of `base` in bold and/or italic. Prefers a font of the
/// same family already in `doc`, then the matching standard 14 font (for non-embedded
/// standard fonts), then an installed face of the family, then synthetic styling on
/// whichever real face is closest.
pub fn find_variant(
    doc: &Document,
    db: Option<&FontDb>,
    base: &FontStyle,
    bold: bool,
    italic: bool,
) -> Variant {
    if base.is_bold() == bold && base.italic == italic {
        return Variant {
            source: VariantSource::Same,
            synthetic: Synthetic::default(),
        };
    }
    let fam = family_key(&base.family);
    // Real faces: (source, has bold, has italic).
    let mut options: Vec<(VariantSource, bool, bool)> = Vec::new();
    for n in doc.object_numbers() {
        let Some(r) = doc.reference(n) else { continue };
        let Some(o) = doc.get(r) else { continue };
        let Object::Dict(d) = &*o else { continue };
        if d.name(b"Type") != Some(b"Font")
            || !matches!(d.name(b"Subtype"), Some(b"Type1" | b"TrueType" | b"Type0"))
        {
            continue;
        }
        let s = font_style(doc, d);
        if s.embedded == base.embedded && family_key(&s.family) == fam && !fam.is_empty() {
            options.push((VariantSource::Document(r), s.is_bold(), s.italic));
        }
    }
    if !base.embedded
        && let Some(n) = standard_name(&base.family, bold, italic)
    {
        options.push((VariantSource::Standard(n), bold, italic));
    }
    if let Some(db) = db {
        for f in db.faces() {
            if f.sfnt && f.embeddable() && family_key(&f.family) == fam && !fam.is_empty() {
                options.push((VariantSource::System(f.clone()), f.weight >= 600, f.italic));
            }
        }
    }
    let score = |b: bool, i: bool| {
        // Exact first; a wrong real style is worse than a missing one (cannot be undone).
        let wrong = (b && !bold) as u8 * 4 + (i && !italic) as u8 * 4;
        let missing = (bold && !b) as u8 + (italic && !i) as u8;
        wrong + missing
    };
    let base_score = score(base.is_bold(), base.italic);
    let best = options.into_iter().min_by_key(|(src, b, i)| {
        let rank = match src {
            VariantSource::Document(_) => 0,
            VariantSource::Standard(_) => 1,
            _ => 2,
        };
        (score(*b, *i), rank)
    });
    match best {
        Some((src, b, i)) if score(b, i) < base_score => Variant {
            source: src,
            synthetic: Synthetic {
                bold: bold && !b,
                italic: italic && !i,
            },
        },
        _ => Variant {
            source: VariantSource::Base,
            synthetic: Synthetic {
                bold: bold && !base.is_bold(),
                italic: italic && !base.italic,
            },
        },
    }
}

/// Whether two family names are the same family (`ArialMT`, `Arial`, `Arial-BoldMT`).
pub fn same_family(a: &str, b: &str) -> bool {
    let k = family_key(a);
    !k.is_empty() && k == family_key(b)
}

/// Family names compared without vendor suffixes (`ArialMT` = `Arial`).
fn family_key(f: &str) -> String {
    let n = normalize(split_style(f).0);
    for s in ["psmt", "mt", "ps"] {
        if let Some(b) = n.strip_suffix(s)
            && !b.is_empty()
        {
            return b.to_owned();
        }
    }
    n
}

/// The faces of `db` in the family of `style`, if installed.
pub fn installed_family<'a>(db: &'a FontDb, style: &FontStyle) -> Vec<&'a FaceInfo> {
    let fam = family_key(&style.family);
    db.faces()
        .iter()
        .filter(|f| !fam.is_empty() && family_key(&f.family) == fam)
        .collect()
}

/// A substitute request for a style (used to pick fallback faces).
pub(crate) fn request(style: &FontStyle) -> fallback::FontRequest {
    fallback::FontRequest {
        name: style.base_font.clone(),
        bold: style.is_bold(),
        italic: style.italic,
        fixed: style.class == FontClass::Mono,
        serif: style.class == FontClass::Serif,
        symbolic: style.class == FontClass::Symbol,
        ordering: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn family_keys() {
        assert_eq!(family_key("ArialMT"), "arial");
        assert_eq!(family_key("TimesNewRomanPS-BoldMT"), "timesnewroman");
        assert_eq!(family_key("Liberation Sans"), "liberationsans");
    }

    #[test]
    fn synthetic_ops() {
        let s = Synthetic {
            bold: true,
            italic: true,
        };
        assert_eq!(s.render_mode(), 2);
        assert!((s.stroke_width(10.0) - 0.3).abs() < 1e-6);
        assert!((s.text_matrix(1.0, 2.0)[2] - 0.2126).abs() < 1e-3);
    }
}
