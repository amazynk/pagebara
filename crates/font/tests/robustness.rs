//! Truncated and corrupted fonts must not panic, loop or blow up: every glyph is drawn and
//! every lookup run on each damaged copy.

mod common;

use common::{Lcg, system};
use pagebara_font::pdf::{FontSpec, PdfFont, PdfFontType};
use pagebara_font::{Font, Path};

/// Exercises a (possibly broken) font program.
fn exercise(data: Vec<u8>) {
    for index in 0..2 {
        let Some(f) = Font::parse_index(data.clone(), index) else {
            continue;
        };
        let mut p = Path::new();
        for g in 0..f.num_glyphs().min(400) {
            p.clear();
            f.outline(g, &mut p);
            f.advance(g);
            f.glyph_name(g);
        }
        f.builtin_encoding();
        f.glyph_for_char('A');
        f.glyph_by_cid(1);
        f.postscript_name();
        if let Some(s) = f.as_sfnt() {
            s.os2();
            s.kerning(1, 2);
            s.glyph_for_variation('A', '\u{FE00}');
            s.vertical_advance(3);
            for t in s.cmap_subtables().to_vec() {
                s.cmap_lookup(&t, 0x41);
                if t.format != 2 {
                    let mut n = 0;
                    s.cmap_for_each(&t, &mut |_, _| n += 1);
                }
            }
        }
        let kind = match f.format() {
            pagebara_font::Format::TrueType => PdfFontType::TrueType,
            _ => PdfFontType::Type1,
        };
        let pf = PdfFont::new(f, FontSpec::new(kind));
        for c in [0u32, 65, 200] {
            pf.outline(c, &mut p);
            pf.unicode(c);
        }
    }
}

fn damage(name: &str, data: &[u8], rounds: u32) {
    let mut rng = Lcg(data.len() as u64 ^ 0x9E37_79B9);
    // Truncations: every byte of the head, then evenly spread cut points.
    let n = data.len();
    let cuts: Vec<usize> = (0..n.min(600))
        .step_by(7)
        .chain((0..60).map(|i| n * i / 60))
        .collect();
    for cut in cuts {
        exercise(data[..cut].to_vec());
    }
    // Corruptions: a few random bytes, biased to the start where the tables are.
    // PAGEBARA_FUZZ_ROUNDS multiplies the corruption rounds for longer local runs.
    let mult: u32 = std::env::var("PAGEBARA_FUZZ_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1);
    for _ in 0..rounds * mult {
        let mut d = data.to_vec();
        for _ in 0..1 + rng.next() % 8 {
            let span = if rng.next() % 2 == 0 { n.min(4096) } else { n };
            let at = rng.next() as usize % span.max(1);
            d[at] = match rng.next() % 4 {
                0 => 0,
                1 => 0xFF,
                _ => rng.next() as u8,
            };
        }
        exercise(d);
    }
    eprintln!("{name}: ok");
}

#[test]
fn truetype() {
    let path = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf";
    let Some(d) = system(path) else { return };
    damage(path, &d, 150);
}

#[test]
fn opentype_cff() {
    let path = "/usr/share/fonts/opentype/urw-base35/NimbusSans-Regular.otf";
    let Some(d) = system(path) else { return };
    damage(path, &d, 150);
    // The bare CFF table.
    let s = pagebara_font::sfnt::Sfnt::parse(d, 0).unwrap();
    damage("CFF table", s.table(b"CFF ").unwrap(), 300);
}

#[test]
fn type1() {
    let path = "/usr/share/fonts/X11/Type1/c0419bt_.pfb";
    let Some(d) = system(path) else { return };
    damage(path, &d, 200);
    // Damage after decryption too: re-encrypting is not needed to reach the parser, the
    // eexec part decrypts to garbage that must be survived.
    let path = "/usr/share/fonts/type1/urw-base35/URWBookman-Light.t1";
    let Some(d) = system(path) else { return };
    damage(path, &d, 60);
}

#[test]
fn collection() {
    // A synthetic collection header pointing at a real font.
    let path = "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf";
    let Some(d) = system(path) else { return };
    let mut ttc = b"ttcf\0\x01\0\0\0\0\0\x02\0\0\0\x14\0\0\0\x14".to_vec();
    let base = ttc.len() as u32;
    // Rebase the table offsets of the inner font.
    let mut inner = d.clone();
    let n = u16::from_be_bytes([inner[4], inner[5]]) as usize;
    for i in 0..n {
        let at = 12 + i * 16 + 8;
        let off = u32::from_be_bytes(inner[at..at + 4].try_into().unwrap()) + base;
        inner[at..at + 4].copy_from_slice(&off.to_be_bytes());
    }
    ttc.extend(inner);
    let f = Font::parse_index(ttc.clone(), 1).unwrap();
    assert_eq!(f.num_glyphs(), Font::parse(d).unwrap().num_glyphs());
    assert!(Font::parse_index(ttc.clone(), 2).is_none());
    damage("collection", &ttc[..ttc.len().min(200_000)], 40);
}

#[test]
fn embedded_programs() {
    let progs = common::embedded_programs();
    eprintln!("{} embedded programs", progs.len());
    // Small ones get the full treatment, the rest a few rounds.
    for (name, d) in progs.iter().filter(|p| p.1.len() < 60_000).take(12) {
        damage(name, d, 40);
    }
}

#[test]
fn hostile_charstrings() {
    // Deep recursion and huge loops in subroutines are cut off.
    use pagebara_font::cff::Cff;
    let mut cff = vec![1u8, 0, 4, 1];
    // Name INDEX
    cff.extend([0, 1, 1, 1, 2, b'X']);
    // Top DICT INDEX: CharStrings at a fixed offset, Private (size 2, offset)
    let top: Vec<u8> = vec![29, 0, 0, 0, 60, 17, 139 + 2, 29, 0, 0, 0, 80, 18];
    cff.extend([0, 1, 1, 1, top.len() as u8 + 1]);
    cff.extend(&top);
    // String INDEX (empty), GSubr INDEX with one subr calling itself: 0-107 callgsubr
    cff.extend([0, 0]);
    cff.extend([0, 1, 1, 1, 4, 32, 29, 11]);
    while cff.len() < 60 {
        cff.push(0);
    }
    // CharStrings: two glyphs, the second calls the recursive subr.
    cff.extend([0, 2, 1, 1, 2, 4, 14, 32, 29, 14]);
    while cff.len() < 80 {
        cff.push(0);
    }
    cff.extend([139, 20]);
    let f = Cff::parse(cff).unwrap();
    let mut p = Path::new();
    assert!(f.outline(0, &mut p));
    assert!(!f.outline(1, &mut p));
}

#[test]
fn huge_cff2_offsets() {
    // A CFF2 font whose variation store offset is too large to add to (found by fuzzing).
    let data: &[u8] = &[
        0x02, 0x00, 0x02, 0x00, 0x7c, 0x11, 0x1e, 0x00, 0x00, 0x00, 0x00, 0x00, 0x86, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x02, 0x01, 0x00, 0x00, 0x11, 0x00, 0x28, 0xff, 0x18,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x95, 0xff, 0xff, 0xff, 0x9c, 0x11, 0x00, 0x00, 0x00,
        0x2f, 0xff, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f,
        0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x4f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f,
        0x8f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x6f, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    assert!(Font::parse(data.to_vec()).is_some());
    exercise(data.to_vec());
}
