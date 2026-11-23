//! Substitution on the system's fonts. Assertions adapt to what is installed: each checks
//! the expected family only when that family exists.

use std::sync::OnceLock;
use std::time::Instant;

use pagebara_font::fallback::{self, FaceInfo, FontDb, FontRequest};
use pagebara_font::pdf::{FontSpec, PdfFont, PdfFontType};
use pagebara_font::{BaseEncoding, Path, SimpleEncoding};

fn db() -> &'static FontDb {
    static DB: OnceLock<FontDb> = OnceLock::new();
    DB.get_or_init(|| {
        let t = Instant::now();
        let db = FontDb::system();
        eprintln!("scanned {} faces in {:?}", db.faces().len(), t.elapsed());
        db
    })
}

fn request(name: &str, flags: u32) -> FontRequest {
    let mut s = FontSpec::new(PdfFontType::Type1);
    s.base_font = Some(name.into());
    s.flags = flags;
    FontRequest::from_spec(&s)
}

fn has(family: &str) -> bool {
    db().faces().iter().any(|f| f.family == family)
}

fn find(name: &str, flags: u32) -> Option<&'static FaceInfo> {
    fallback::find(db(), &request(name, flags))
}

#[test]
fn scan_finds_faces() {
    if fallback::system_font_dirs().is_empty() {
        return;
    }
    let faces = db().faces();
    assert!(!faces.is_empty());
    // Collections contribute one face per font. Not every collection gives all of its
    // fonts (faces that are bitmap only or have no name are left out), so this asks only
    // that the fonts after the first are read where there are several.
    let several = |p: &std::path::Path| {
        let mut head = [0; 12];
        std::fs::File::open(p)
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head))
            .is_ok_and(|()| {
                &head[..4] == b"ttcf"
                    && u32::from_be_bytes([head[8], head[9], head[10], head[11]]) > 1
            })
    };
    if faces.iter().any(|f| f.index == 0 && several(&f.path)) {
        assert!(faces.iter().any(|f| f.index > 0));
    }
    for f in faces {
        assert!(
            !f.family.is_empty() && (1..=1000).contains(&f.weight),
            "{f:?}"
        );
    }
}

/// The first of `families` that is installed (the font itself, then its clones).
fn first_installed(families: &[&'static str]) -> Option<&'static str> {
    families.iter().copied().find(|f| has(f))
}

fn check(name: &str, flags: u32, families: &[&'static str], weight: u16, italic: bool) {
    let Some(want) = first_installed(families) else {
        return;
    };
    let f = find(name, flags).unwrap();
    assert_eq!(
        (f.family.as_str(), f.weight, f.italic),
        (want, weight, italic),
        "{name}"
    );
}

#[test]
fn standard_14() {
    let sans = ["Helvetica", "Liberation Sans"];
    check("Helvetica", 32, &sans, 400, false);
    check("Helvetica-BoldOblique", 32, &sans, 700, true);
    check(
        "ABCDEF+Arial,Bold",
        32,
        &["Arial", "Liberation Sans"],
        700,
        false,
    );
    let serif = ["Times", "Liberation Serif"];
    check("Times-Italic", 34, &serif, 400, true);
    check(
        "TimesNewRomanPS-BoldMT",
        34,
        &["Times New Roman", "Liberation Serif"],
        700,
        false,
    );
    let mono = ["Courier", "Liberation Mono"];
    check("Courier", 33, &mono, 400, false);
    check("Courier-Bold", 33, &mono, 700, false);
    check(
        "CourierNewPS-BoldMT",
        33,
        &["Courier New", "Liberation Mono"],
        700,
        false,
    );
    check("Symbol", 4, &["Symbol", "Standard Symbols PS"], 400, false);
    check("ZapfDingbats", 4, &["D050000L"], 400, false);
    check("Calibri-Bold", 32, &["Calibri", "Carlito"], 700, false);
    // Unknown names: by flags.
    check("SomeUnknownFont", 1, &["Liberation Mono"], 400, false);
    check("OtherUnknown,Italic", 2, &["Liberation Serif"], 400, true);
    // An installed font by its PostScript name.
    if has("DejaVu Sans") {
        assert_eq!(
            find("DejaVuSans-Bold", 0).unwrap().postscript_name,
            "DejaVuSans-Bold"
        );
    }
}

#[test]
fn cjk_by_ordering() {
    if !has("Noto Sans CJK JP") {
        return;
    }
    let mut s = FontSpec::new(PdfFontType::CidType0);
    s.base_font = Some("MS-Gothic".into());
    for (ord, fam) in [
        ("Japan1", "Noto Sans CJK JP"),
        ("GB1", "Noto Sans CJK SC"),
        ("CNS1", "Noto Sans CJK TC"),
        ("Korea1", "Noto Sans CJK KR"),
    ] {
        s.ordering = Some(ord.into());
        let f = fallback::find(db(), &FontRequest::from_spec(&s)).unwrap();
        assert_eq!(f.family, fam);
        let font = db().load(f).unwrap();
        assert!(font.glyph_for_char('中').is_some());
    }
}

#[test]
fn substitute_pdf_font() {
    let mut s = FontSpec::new(PdfFontType::Type1);
    s.base_font = Some("Helvetica-Bold".into());
    s.flags = 32;
    s.encoding = SimpleEncoding {
        base: Some(BaseEncoding::WinAnsi),
        differences: vec![(0x41, b"bullet"[..].into())],
    };
    let Some(face) = fallback::find(db(), &FontRequest::from_spec(&s)) else {
        return;
    };
    let pf = PdfFont::substitute(db().load(face).unwrap(), s.clone());
    assert!(pf.is_substitute());
    let mut p = Path::new();
    assert!(pf.outline(0x42, &mut p), "{face:?}");
    assert_eq!(pf.unicode(0x42).as_deref(), Some("B"));
    assert_eq!(pf.unicode(0x80).as_deref(), Some("€"));
    assert_eq!(pf.unicode(0x41).as_deref(), Some("•"));
    assert_ne!(pf.glyph(0x41), 0);
    let w = pf.advance(0x42).unwrap();
    assert!((0.6..0.8).contains(&w), "{w}");
    // Symbol uses its built-in encoding.
    let mut s = FontSpec::new(PdfFontType::Type1);
    s.base_font = Some("Symbol".into());
    s.flags = 4;
    let Some(face) = fallback::find(db(), &FontRequest::from_spec(&s)) else {
        return;
    };
    let pf = PdfFont::substitute(db().load(face).unwrap(), s);
    if face.family.contains("Symbol") {
        assert_eq!(pf.unicode(0x61).as_deref(), Some("α"));
        assert_ne!(pf.glyph(0x61), 0);
    }
}

#[test]
fn scripts_of_characters() {
    use fallback::Script;
    for (c, s) in [
        ('a', Script::Latin),
        ('é', Script::Latin),
        ('€', Script::Latin),
        ('Ж', Script::Cyrillic),
        ('λ', Script::Greek),
        ('א', Script::Hebrew),
        ('م', Script::Arabic),
        ('\u{FEE3}', Script::Arabic),
        ('क', Script::Devanagari),
        ('ก', Script::Thai),
        ('密', Script::Han),
        ('。', Script::Han),
        ('ひ', Script::Kana),
        ('한', Script::Hangul),
        ('→', Script::Symbols),
        ('\u{10FFFF}', Script::Other),
    ] {
        assert_eq!(Script::of(c), s, "{c}");
    }
    assert!(Script::Arabic.is_rtl() && !Script::Han.is_rtl());
}

/// Each script finds an embeddable face mapping the character, from its own families when
/// installed.
#[test]
fn faces_for_characters() {
    use fallback::{Script, find_for_char};
    if db().faces().is_empty() {
        return;
    }
    for c in "aЖλאمकก密ひ한→".chars() {
        let Some(f) = find_for_char(db(), c) else {
            eprintln!("no face for {c}");
            continue;
        };
        assert!(f.sfnt && f.embeddable(), "{f:?}");
        let font = db().load(f).unwrap();
        assert!(font.glyph_for_char(c).is_some_and(|g| g != 0), "{c}: {f:?}");
        // A face of the script's own families when one is installed and covers it.
        if let Some(fam) = first_installed(Script::of(c).families())
            && c != '→'
        {
            assert_eq!(f.family, fam, "{c}");
        }
    }
    // Nothing maps an unassigned code point.
    assert!(find_for_char(db(), '\u{10FFFD}').is_none());
}

/// Font data held in memory (a font bundled with an app) is found and loaded like a file.
#[test]
fn faces_from_memory() {
    let Some(face) = db().faces().iter().find(|f| f.sfnt) else {
        return;
    };
    let data: std::sync::Arc<[u8]> = std::fs::read(&face.path).unwrap().into();
    let key = std::path::Path::new("bundled/in-memory.ttf");
    let mut mem = FontDb::new();
    mem.add_data(key, data.clone());
    let found: Vec<FaceInfo> = mem
        .faces()
        .iter()
        .filter(|f| f.path == key)
        .cloned()
        .collect();
    assert!(!found.is_empty());
    let same = found.iter().find(|f| f.index == face.index).unwrap();
    assert_eq!(same.family, face.family);
    assert_eq!(same.unicode_ranges, face.unicode_ranges);
    assert!(mem.load(same).is_some());
    // Data that is not a font adds nothing.
    mem.add_data(
        std::path::Path::new("bundled/not-a-font"),
        (*b"nothing").into(),
    );
    assert_eq!(mem.faces().len(), found.len());
}
