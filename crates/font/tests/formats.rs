//! Parsing and outlines of every supported format, on system fonts (skipped if missing).
//! Expected values come from fontTools.

mod common;

use common::system;
use pagebara_font::cff::Cff;
use pagebara_font::sfnt::Sfnt;
use pagebara_font::{Font, Format, Path, Verb};

const LIBERATION: &str = "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf";
const DEJAVU: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";
const NIMBUS_OTF: &str = "/usr/share/fonts/opentype/urw-base35/NimbusSans-Regular.otf";
const NIMBUS_PFB: &str = "/usr/share/fonts/X11/Type1/NimbusRoman-Regular.pfb";
const BOOKMAN_T1: &str = "/usr/share/fonts/type1/urw-base35/URWBookman-Demi.t1";
const BITSTREAM_PFB: &str = "/usr/share/fonts/X11/Type1/c0419bt_.pfb";
const NOTO_CJK: &str = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
const WQY: &str = "/usr/share/fonts/truetype/wqy/wqy-microhei.ttc";

fn outline(f: &Font, gid: u16) -> Path {
    let mut p = Path::new();
    assert!(f.outline(gid, &mut p), "glyph {gid}");
    p
}

/// Subpaths start with a move and end with a close.
fn well_formed(p: &Path) -> bool {
    let mut open = false;
    for v in &p.verbs {
        match v {
            Verb::Move if open => return false,
            Verb::Move => open = true,
            Verb::Close if !open => return false,
            Verb::Close => open = false,
            _ if !open => return false,
            _ => {}
        }
    }
    !open
}

#[test]
fn truetype() {
    let Some(d) = system(LIBERATION) else { return };
    let f = Font::parse(d).unwrap();
    assert_eq!(f.format(), Format::TrueType);
    let s = f.as_sfnt().unwrap();
    assert_eq!(s.units_per_em(), 2048);
    let a = s.glyph_for_char('A').unwrap();
    assert_eq!(a, 36);
    assert_eq!(s.advance(a), Some(1366));
    assert_eq!(s.left_side_bearing(a), Some(4));
    assert_eq!(s.glyph_name(a), Some("A"));
    assert_eq!(s.glyph_by_name(b"A"), Some(36));
    assert_eq!(s.kerning(36, 57), -152);
    assert_eq!(s.family_name().as_deref(), Some("Liberation Sans"));
    assert_eq!(s.postscript_name().as_deref(), Some("LiberationSans"));
    assert_eq!(s.os2().unwrap().weight_class, 400);
    let p = outline(&f, a);
    assert!(well_formed(&p));
    let b = p.bounds().unwrap();
    assert!(b[0] >= 0.0 && b[2] <= 1366.0 && b[3] > 1300.0, "{b:?}");
    // (1,0) Macintosh cmap and the built-in encoding.
    let mac = s.find_subtable(1, 0).unwrap();
    assert_eq!(s.cmap_lookup(mac, 0x41), Some(36));
    assert_eq!(f.builtin_glyph(0x41), Some(36));
    for g in 0..f.num_glyphs() {
        let mut p = Path::new();
        assert!(f.outline(g, &mut p));
        assert!(well_formed(&p), "glyph {g}");
    }
}

#[test]
fn truetype_cmap_format12_and_composites() {
    let Some(d) = system(DEJAVU) else { return };
    let s = Sfnt::parse(d, 0).unwrap();
    assert_eq!(s.glyph_for_char('\u{10300}'), Some(5373));
    // A composite (Aacute) draws more contours than its base.
    let a = s.glyph_for_char('A').unwrap();
    let aa = s.glyph_for_char('Á').unwrap();
    let (mut p, mut q) = (Path::new(), Path::new());
    s.outline(a, &mut p);
    s.outline(aa, &mut q);
    let closes = |p: &Path| p.verbs.iter().filter(|v| **v == Verb::Close).count();
    assert!(closes(&q) > closes(&p));
}

#[test]
fn opentype_cff() {
    let Some(d) = system(NIMBUS_OTF) else { return };
    let f = Font::parse(d).unwrap();
    assert_eq!(f.format(), Format::OpenTypeCff);
    let a = f.glyph_for_char('A').unwrap();
    assert_eq!(a, 34);
    assert_eq!(f.glyph_by_name(b"A"), Some(34));
    assert_eq!(f.advance(a), Some(667.0));
    assert_eq!(f.as_cff().unwrap().advance(a), Some(667.0));
    assert_eq!(f.font_matrix().0[0], 0.001);
    let p = outline(&f, a);
    assert!(well_formed(&p) && p.verbs.contains(&Verb::Line));
    // The CFF built-in encoding is StandardEncoding.
    assert_eq!(f.builtin_glyph(0x41), Some(34));
    assert_eq!(f.builtin_encoding().unwrap()[0x27], Some("quoteright"));
}

#[test]
fn bare_cff_and_cff_matches_opentype() {
    let Some(d) = system(NIMBUS_OTF) else { return };
    let otf = Sfnt::parse(d, 0).unwrap();
    let table = otf.table(b"CFF ").unwrap().to_vec();
    let bare = Font::parse(table).unwrap();
    assert_eq!(bare.format(), Format::Cff);
    let cff: &Cff = bare.as_cff().unwrap();
    assert!(!cff.is_cid());
    assert_eq!(cff.name(), Some("NimbusSans-Regular"));
    for g in 0..otf.num_glyphs() {
        let (mut p, mut q) = (Path::new(), Path::new());
        otf.outline(g, &mut p);
        bare.outline(g, &mut q);
        assert_eq!(p, q);
        assert!(well_formed(&q));
    }
}

#[test]
fn type1_pfb_and_pfa() {
    for path in [NIMBUS_PFB, BOOKMAN_T1, BITSTREAM_PFB] {
        let Some(d) = system(path) else { continue };
        let f = Font::parse(d).unwrap();
        assert_eq!(f.format(), Format::Type1);
        assert_eq!(f.glyph_name(0), Some(".notdef"));
        let a = f.glyph_by_name(b"A").unwrap();
        assert_eq!(f.builtin_glyph(b'A'), Some(a));
        let p = outline(&f, a);
        assert!(well_formed(&p) && !p.is_empty());
        assert!(f.advance(a).unwrap() > 500.0);
        for g in 0..f.num_glyphs() {
            let mut p = Path::new();
            assert!(f.outline(g, &mut p), "{path} glyph {g}");
            assert!(well_formed(&p), "{path} glyph {g}");
        }
    }
    // seac: Bitstream Courier composes accented letters; the accent sits above the base.
    let Some(d) = system(BITSTREAM_PFB) else {
        return;
    };
    let f = Font::parse(d).unwrap();
    let e = outline(&f, f.glyph_by_name(b"e").unwrap())
        .bounds()
        .unwrap();
    let ea = outline(&f, f.glyph_by_name(b"eacute").unwrap())
        .bounds()
        .unwrap();
    assert!(ea[3] > e[3] + 100.0 && (ea[1] - e[1]).abs() < 1.0);
}

#[test]
fn collections_cid_keyed_cff_vmtx_and_variations() {
    let Some(d) = system(NOTO_CJK) else { return };
    assert!(pagebara_font::sfnt::collection_count(&d) >= 4);
    let s = Sfnt::parse(d.clone(), 0).unwrap();
    assert_eq!(s.family_name().as_deref(), Some("Noto Sans CJK JP"));
    let g = s.glyph_for_char('一').unwrap();
    assert_eq!(g, 9481);
    assert_eq!(s.vertical_advance(g), Some((1000, 449)));
    assert_eq!(s.glyph_for_variation('\u{4FAE}', '\u{FE00}'), Some(58912));
    assert_eq!(
        s.glyph_for_variation('\u{3001}', '\u{FE00}'),
        s.glyph_for_char('\u{3001}')
    );
    let cff = s.cff().unwrap();
    let ros = cff.ros().unwrap();
    assert_eq!(
        (ros.registry.as_str(), ros.ordering.as_str()),
        ("Adobe", "Identity")
    );
    assert_eq!(cff.charset_id(g), Some(9481));
    assert_eq!(cff.glyph_by_cid(9481), Some(g));
    let f = Font::parse_index(d.clone(), 0).unwrap();
    assert!(well_formed(&outline(&f, g)));
    // Another face of the collection.
    let kr = Sfnt::parse(d, 1).unwrap();
    assert_ne!(kr.family_name(), s.family_name());
    let Some(d) = system(WQY) else { return };
    let w = Font::parse_index(d, 0).unwrap();
    assert_eq!(w.format(), Format::TrueType);
    assert!(!outline(&w, w.glyph_for_char('中').unwrap()).is_empty());
}

#[test]
fn not_fonts() {
    for d in [
        &b""[..],
        b"\0\x01\0\0",
        b"ttcf",
        b"OTTO\0\0",
        b"%!PS-AdobeFont-1.0",
        b"\x01\0\x04\x01",
    ] {
        assert!(Font::parse(d.to_vec()).is_none());
    }
}
