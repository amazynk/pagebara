//! PDF glyph selection: synthetic font dictionaries over system fonts, and the fonts of the
//! public sample PDFs checked against their `/Widths` (a wrong glyph has a wrong width).

mod common;

use common::system;
use pagebara_font::pdf::{self, CidToGid, FontSpec, PdfFont, PdfFontType, SimpleEncoding};
use pagebara_font::sfnt::Sfnt;
use pagebara_font::subset::truetype;
use pagebara_font::{BaseEncoding, Font, Path};
use pagebara_syntax::{Document, Object};

const LIBERATION: &str = "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf";
const NIMBUS_PFB: &str = "/usr/share/fonts/X11/Type1/NimbusRoman-Regular.pfb";
const NIMBUS_OTF: &str = "/usr/share/fonts/opentype/urw-base35/NimbusSans-Regular.otf";

fn spec(
    kind: PdfFontType,
    flags: u32,
    base: Option<BaseEncoding>,
    diffs: &[(u8, &str)],
) -> FontSpec {
    let mut s = FontSpec::new(kind);
    s.flags = flags;
    s.encoding = SimpleEncoding {
        base,
        differences: diffs
            .iter()
            .map(|(c, n)| (*c, n.as_bytes().into()))
            .collect(),
    };
    s.has_encoding = base.is_some() || !diffs.is_empty();
    s
}

#[test]
fn truetype_nonsymbolic() {
    let Some(d) = system(LIBERATION) else { return };
    let f = Font::parse(d.clone()).unwrap();
    let euro = f.glyph_for_char('€').unwrap();
    let b = f.glyph_for_char('B').unwrap();
    let pf = PdfFont::new(
        f,
        spec(
            PdfFontType::TrueType,
            32,
            Some(BaseEncoding::WinAnsi),
            &[(65, "B"), (66, "uni00E9")],
        ),
    );
    assert_eq!(pf.glyph(0x80), euro);
    assert_eq!(pf.unicode(0x80).as_deref(), Some("€"));
    assert_eq!(pf.glyph(65), b);
    assert_eq!(pf.unicode(65).as_deref(), Some("B"));
    assert_eq!(pf.unicode(66).as_deref(), Some("é"));
    assert_ne!(pf.glyph(66), 0);
    let mut p = Path::new();
    assert!(pf.outline(65, &mut p) && !p.is_empty());
    // No encoding at all: WinAnsi for a non-symbolic font.
    let pf = PdfFont::new(
        Font::parse(d).unwrap(),
        spec(PdfFontType::TrueType, 32, None, &[]),
    );
    assert_eq!(pf.glyph(0x80), euro);
}

#[test]
fn truetype_symbolic_30_cmap() {
    let Some(d) = system(LIBERATION) else { return };
    let s = Sfnt::parse(d, 0).unwrap();
    let a = s.glyph_for_char('A').unwrap();
    let z = s.glyph_for_char('Z').unwrap();
    // A symbol font: (3,0) cmap at U+F041/U+F05A.
    let sub = truetype::subset(&s, [a, z], &[('\u{F041}', a), ('\u{F05A}', z)]).unwrap();
    let mut data = sub.data;
    let cmap = Sfnt::parse(data.clone(), 0).unwrap();
    let off = cmap.table(b"cmap").unwrap().as_ptr() as usize - cmap.data().as_ptr() as usize;
    data[off + 6..off + 8].copy_from_slice(&0u16.to_be_bytes());
    let f = Font::parse(data).unwrap();
    let (na, nz) = (sub_id(&sub.glyphs, a), sub_id(&sub.glyphs, z));
    assert_eq!(f.builtin_glyph(0x41), Some(na));
    let pf = PdfFont::new(f, spec(PdfFontType::TrueType, 4, None, &[]));
    assert_eq!(pf.glyph(0x41), na);
    assert_eq!(pf.glyph(0x5A), nz);
    assert_eq!(pf.glyph(0x42), 0);
}

fn sub_id(glyphs: &[u16], old: u16) -> u16 {
    glyphs.iter().position(|&g| g == old).unwrap() as u16
}

#[test]
fn type1_builtin_and_named_encodings() {
    let Some(d) = system(NIMBUS_PFB) else { return };
    let f = Font::parse(d.clone()).unwrap();
    let qr = f.glyph_by_name(b"quoteright").unwrap();
    let qs = f.glyph_by_name(b"quotesingle").unwrap();
    let pf = PdfFont::new(f, spec(PdfFontType::Type1, 32, None, &[]));
    assert_eq!(pf.glyph(0x27), qr);
    assert_eq!(pf.unicode(0x27).as_deref(), Some("\u{2019}"));
    let f = Font::parse(d.clone()).unwrap();
    let pf = PdfFont::new(
        f,
        spec(
            PdfFontType::Type1,
            32,
            Some(BaseEncoding::WinAnsi),
            &[(0x41, "Aacute")],
        ),
    );
    assert_eq!(pf.glyph(0x27), qs);
    let aacute = pf.font().glyph_by_name(b"Aacute").unwrap();
    assert_eq!(pf.glyph(0x41), aacute);
    assert_eq!(pf.unicode(0x41).as_deref(), Some("Á"));
    // Unknown names fall back to nothing rather than the built-in encoding.
    let f = Font::parse(d).unwrap();
    let pf = PdfFont::new(
        f,
        spec(PdfFontType::Type1, 32, None, &[(0x41, "nonexistent")]),
    );
    assert_eq!(pf.glyph(0x41), 0);
}

#[test]
fn bare_cff_type1c() {
    let Some(d) = system(NIMBUS_OTF) else { return };
    let s = Sfnt::parse(d, 0).unwrap();
    let f = Font::parse(s.table(b"CFF ").unwrap().to_vec()).unwrap();
    let a = f.glyph_by_name(b"A").unwrap();
    let pf = PdfFont::new(f, spec(PdfFontType::Type1, 32, None, &[]));
    assert_eq!(pf.glyph(b'A'.into()), a);
    assert_eq!(pf.simple_glyph(b'A').unwrap().name.as_deref(), Some("A"));
}

#[test]
fn cid_fonts() {
    let Some(d) = system(LIBERATION) else { return };
    let f = Font::parse(d.clone()).unwrap();
    let a = f.glyph_for_char('A').unwrap();
    let mut s = FontSpec::new(PdfFontType::CidType2);
    s.cid_to_gid = CidToGid::from_bytes(&[0, 0, 0, 0, (a >> 8) as u8, a as u8]);
    let pf = PdfFont::new(f, s);
    assert_eq!(pf.glyph(2), a);
    assert_eq!(pf.glyph(1), 0);
    assert_eq!(pf.glyph(7), 0);
    assert_eq!(pf.unicode(2).as_deref(), Some("A"));
    let pf = PdfFont::new(
        Font::parse(d).unwrap(),
        FontSpec::new(PdfFontType::CidType2),
    );
    assert_eq!(pf.glyph(u32::from(a)), a);
    // CID-keyed CFF: CIDs through the charset.
    let Some(d) = system("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc") else {
        return;
    };
    let f = Font::parse(d).unwrap();
    let g = f.glyph_for_char('一').unwrap();
    let cid = f.as_cff().unwrap().charset_id(g).unwrap();
    let pf = PdfFont::new(f, FontSpec::new(PdfFontType::CidType0));
    assert_eq!(pf.glyph(u32::from(cid)), g);
    assert_eq!(pf.unicode(u32::from(cid)).as_deref(), Some("一"));
}

/// Width checks of every embedded font in the sample PDFs.
#[test]
fn sample_pdfs_widths() {
    let (mut checked, mut matched, mut fonts) = (0usize, 0usize, 0usize);
    for path in common::PDFS {
        let Some(doc) = common::open(path) else {
            continue;
        };
        for dict in common::font_dicts(&doc) {
            let Some(pf) = pdf::load(&doc, &dict, None) else {
                continue;
            };
            fonts += 1;
            // Type 1 and CFF programs always have a built-in encoding.
            if !pf.spec().kind.is_composite()
                && matches!(
                    pf.font().format(),
                    pagebara_font::Format::Type1 | pagebara_font::Format::Cff
                )
            {
                let enc = pdf::builtin_encoding(&doc, &dict).unwrap();
                assert_eq!(enc.len(), 256);
                assert!(enc.iter().any(|n| n.is_some()));
            }
            let (c, m) = check_widths(&doc, &dict, &pf);
            if c > 0 && m * 10 < c * 9 {
                eprintln!(
                    "{path}: {:?} {:?}: {m}/{c}",
                    dict.get(b"BaseFont"),
                    pf.font().format()
                );
            }
            checked += c;
            matched += m;
            // Every code draws without trouble.
            let mut p = Path::new();
            for code in 0..256 {
                p.clear();
                pf.outline(code, &mut p);
                pf.unicode(code);
            }
        }
    }
    eprintln!("{fonts} fonts, {matched}/{checked} glyph widths match");
    if checked > 0 {
        assert!(matched * 100 >= checked * 97, "{matched}/{checked}");
    }
}

fn check_widths(doc: &Document, dict: &pagebara_syntax::Dict, pf: &PdfFont) -> (usize, usize) {
    let mut pairs: Vec<(u32, f64)> = Vec::new();
    if pf.spec().kind.is_composite() {
        let df = doc.get_in(dict, b"DescendantFonts");
        let Some(first) = df.as_array().and_then(|a| a.first()) else {
            return (0, 0);
        };
        let d = doc.resolve(first);
        let Some(d) = d.as_dict() else { return (0, 0) };
        let w = doc.get_in(d, b"W");
        let items = w.as_array().unwrap_or_default();
        let mut i = 0;
        while i + 1 < items.len() {
            let start = doc.resolve(&items[i]).as_int().unwrap_or(0) as u32;
            match &*doc.resolve(&items[i + 1]) {
                Object::Array(ws) => {
                    for (k, w) in ws.iter().enumerate() {
                        pairs.push((start + k as u32, doc.resolve(w).as_f64().unwrap_or(0.0)));
                    }
                    i += 2;
                }
                // `c1 c2 w` ranges are often written loosely over unused CIDs.
                _ => i += 3,
            }
        }
    } else {
        let first = doc.get_in(dict, b"FirstChar").as_int().unwrap_or(0) as u32;
        let ws = doc.get_in(dict, b"Widths");
        for (k, w) in ws.as_array().unwrap_or_default().iter().enumerate() {
            pairs.push((first + k as u32, doc.resolve(w).as_f64().unwrap_or(0.0)));
        }
    }
    let (mut c, mut m) = (0, 0);
    for (code, w) in pairs {
        if w <= 0.0 || pf.glyph(code) == 0 {
            continue;
        }
        let Some(adv) = pf.advance(code) else {
            continue;
        };
        c += 1;
        if (f64::from(adv) * 1000.0 - w).abs() <= 2.0 {
            m += 1;
        }
    }
    (c, m)
}
