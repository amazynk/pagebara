//! Substitutes for fonts that are not embedded: a model from the PDF font name, flags and
//! CID ordering to candidate families (metric-compatible free fonts for the standard 14
//! and common Office fonts), and a database of the system's fonts to pick from.

pub mod scan;
pub mod script;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::font::Font;
use crate::pdf::{FontSpec, strip_subset_tag};

pub use scan::system_font_dirs;
pub use script::{Script, find_for_char};

/// A face found on the system.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceInfo {
    pub path: PathBuf,
    /// The face index in a collection.
    pub index: u32,
    pub family: String,
    pub full_name: String,
    pub postscript_name: String,
    /// CSS-style weight, 100–900.
    pub weight: u16,
    pub italic: bool,
    pub monospace: bool,
    pub serif: Option<bool>,
    /// A TrueType or OpenType face (not Type 1).
    pub sfnt: bool,
    /// `OS/2` `fsType` embedding permissions (0 when unknown).
    pub fs_type: u16,
    /// `OS/2` `ulUnicodeRange1..4`: the Unicode blocks the face claims to cover.
    pub unicode_ranges: [u32; 4],
}

impl FaceInfo {
    /// Whether the license allows embedding (`fsType` is not "restricted license").
    pub fn embeddable(&self) -> bool {
        self.fs_type & 0x000F != 0x0002
    }

    /// Whether the `OS/2` table claims Unicode range `bit`.
    pub fn claims_range(&self, bit: u32) -> bool {
        self.unicode_ranges
            .get(bit as usize / 32)
            .is_some_and(|r| r & (1 << (bit % 32)) != 0)
    }
}

/// The fonts available for substitution.
#[derive(Debug, Default)]
pub struct FontDb {
    faces: Vec<FaceInfo>,
    cache: Mutex<HashMap<PathBuf, Arc<[u8]>>>,
}

impl FontDb {
    pub fn new() -> FontDb {
        FontDb::default()
    }

    /// Scans the OS's standard font directories (see [`system_font_dirs`]).
    pub fn system() -> FontDb {
        let mut db = FontDb::new();
        for d in system_font_dirs() {
            db.add_dir(&d);
        }
        db
    }

    /// Adds every font below `dir` (scanned in parallel).
    pub fn add_dir(&mut self, dir: &Path) {
        let files = scan::font_files(dir);
        let threads = std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .clamp(1, 8);
        let chunk = files.len().div_ceil(threads).max(1);
        let found: Vec<Vec<FaceInfo>> = std::thread::scope(|s| {
            let handles: Vec<_> = files
                .chunks(chunk)
                .enumerate()
                .map(|(i, c)| {
                    pagebara_thread::spawn_scoped(s, format!("font-scan-{i}"), move || {
                        c.iter().flat_map(|p| scan::faces(p)).collect()
                    })
                })
                .collect();
            handles
                .into_iter()
                .map(|h| h.join().unwrap_or_default())
                .collect()
        });
        self.faces.extend(found.into_iter().flatten());
    }

    pub fn add_file(&mut self, path: &Path) {
        self.faces.extend(scan::faces(path));
    }

    /// Adds the faces of font data held in memory (e.g. a font bundled with the app),
    /// known by `path`, which need not exist.
    pub fn add_data(&mut self, path: &Path, data: Arc<[u8]>) {
        let faces = scan::faces_of(&data, path);
        if !faces.is_empty() {
            let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            c.insert(path.to_path_buf(), data);
        }
        self.faces.extend(faces);
    }

    pub fn add_face(&mut self, face: FaceInfo) {
        self.faces.push(face);
    }

    pub fn faces(&self) -> &[FaceInfo] {
        &self.faces
    }

    /// Reads and parses a face (file contents are cached).
    pub fn load(&self, face: &FaceInfo) -> Option<Font> {
        let data = {
            let mut c = self.cache.lock().unwrap_or_else(|e| e.into_inner());
            match c.get(&face.path) {
                Some(d) => d.clone(),
                None => {
                    let d: Arc<[u8]> = std::fs::read(&face.path).ok()?.into();
                    c.insert(face.path.clone(), d.clone());
                    d
                }
            }
        };
        Font::parse_index(data, face.index)
    }
}

/// Broad classes of substitutes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Sans,
    SansNarrow,
    Serif,
    Mono,
    Symbol,
    Dingbats,
    /// CJK by CIDSystemInfo ordering, sans or serif (Mincho/Song/Ming).
    Japanese(bool),
    Chinese(bool),
    ChineseTraditional(bool),
    Korean(bool),
}

/// What to look for.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FontRequest {
    /// The PostScript name without subset tag (`Helvetica-Bold`, `ArialMT`).
    pub name: String,
    pub bold: bool,
    pub italic: bool,
    pub fixed: bool,
    pub serif: bool,
    pub symbolic: bool,
    pub ordering: Option<String>,
}

impl FontRequest {
    pub fn from_spec(spec: &FontSpec) -> FontRequest {
        let name = strip_subset_tag(spec.base_font.as_deref().unwrap_or("")).to_owned();
        let (_, style) = split_style(&name);
        let style = style.to_ascii_lowercase();
        FontRequest {
            bold: weight_from_name(&style).is_some_and(|w| w >= 600)
                || spec.weight.is_some_and(|w| w >= 600.0)
                || spec.flags & (1 << 18) != 0,
            italic: style.contains("italic")
                || style.contains("oblique")
                || spec.flags & 64 != 0
                || spec.italic_angle != 0.0,
            fixed: spec.flags & 1 != 0,
            serif: spec.flags & 2 != 0,
            symbolic: spec.flags & 4 != 0,
            ordering: spec.ordering.clone(),
            name,
        }
    }

    pub fn class(&self) -> Class {
        let fam = normalize(split_style(&self.name).0);
        if let Some(o) = &self.ordering {
            let serif = self.serif || is_cjk_serif_name(&fam);
            match o.as_str() {
                "Japan1" | "Japan2" => return Class::Japanese(serif),
                "GB1" => return Class::Chinese(serif),
                "CNS1" => return Class::ChineseTraditional(serif),
                "Korea1" | "KR" => return Class::Korean(serif),
                _ => {}
            }
        }
        if fam.contains("narrow") || fam.contains("condensed") {
            return Class::SansNarrow;
        }
        if let Some(c) = NAME_CLASSES
            .iter()
            .find(|(n, _)| fam.starts_with(n))
            .map(|c| c.1)
        {
            return c;
        }
        if fam.contains("mono") || fam.contains("courier") || fam.contains("console") {
            return Class::Mono;
        }
        if self.fixed {
            Class::Mono
        } else if self.serif || fam.contains("serif") || fam.contains("roman") {
            Class::Serif
        } else {
            Class::Sans
        }
    }
}

/// Families to try for a request, best first: specific replacements for well-known fonts,
/// then the class's metric-compatible and common families.
pub fn candidates(req: &FontRequest) -> Vec<&'static str> {
    let fam = normalize(split_style(&req.name).0);
    let mut out: Vec<&'static str> = REPLACEMENTS
        .iter()
        .filter(|(n, _)| fam == *n)
        .flat_map(|r| r.1.iter().copied())
        .collect();
    out.extend(class_families(req.class()));
    out
}

fn class_families(c: Class) -> Vec<&'static str> {
    let (sans, serif) = (&JAPANESE_SANS[..], &JAPANESE_SERIF[..]);
    let pick = |s: bool, a: &[&'static str], b: &[&'static str]| -> Vec<&'static str> {
        if s {
            a.iter().chain(b).copied().collect()
        } else {
            b.iter().chain(a).copied().collect()
        }
    };
    match c {
        Class::Sans => SANS.to_vec(),
        Class::SansNarrow => NARROW.iter().chain(SANS).copied().collect(),
        Class::Serif => SERIF.to_vec(),
        Class::Mono => MONO.to_vec(),
        Class::Symbol => SYMBOL.to_vec(),
        Class::Dingbats => DINGBATS.to_vec(),
        Class::Japanese(s) => pick(s, serif, sans),
        Class::Chinese(s) => pick(s, &CHINESE_SERIF, &CHINESE_SANS),
        Class::ChineseTraditional(s) => pick(s, &TRAD_SERIF, &TRAD_SANS),
        Class::Korean(s) => pick(s, &KOREAN_SERIF, &KOREAN_SANS),
    }
}

/// Picks the best face for a request.
pub fn find<'a>(db: &'a FontDb, req: &FontRequest) -> Option<&'a FaceInfo> {
    let faces = db.faces();
    if faces.is_empty() {
        return None;
    }
    let want = normalize(&req.name);
    // The exact font, when installed.
    if !want.is_empty()
        && let Some(f) = faces
            .iter()
            .find(|f| normalize(&f.postscript_name) == want || normalize(&f.full_name) == want)
    {
        return Some(f);
    }
    let base = normalize(split_style(&req.name).0);
    let base_stripped = strip_vendor(&base);
    let family_match = |f: &FaceInfo, fam: &str| {
        let n = normalize(&f.family);
        n == fam || strip_vendor(&n) == fam
    };
    if !base.is_empty() && faces.iter().any(|f| family_match(f, base_stripped)) {
        return best_style(faces.iter().filter(|f| family_match(f, base_stripped)), req);
    }
    for fam in candidates(req) {
        let n = normalize(fam);
        if faces.iter().any(|f| normalize(&f.family) == n) {
            return best_style(faces.iter().filter(|f| normalize(&f.family) == n), req);
        }
    }
    // Anything with the right shape.
    let class = req.class();
    best_style(
        faces.iter().filter(|f| match class {
            Class::Mono => f.monospace,
            Class::Serif => f.serif == Some(true),
            _ => !f.monospace && f.serif != Some(true),
        }),
        req,
    )
    .or_else(|| best_style(faces.iter(), req))
}

fn best_style<'a>(
    faces: impl Iterator<Item = &'a FaceInfo>,
    req: &FontRequest,
) -> Option<&'a FaceInfo> {
    let want = if req.bold { 700 } else { 400 };
    faces.min_by_key(|f| {
        let w = i32::from(f.weight).abs_diff(want);
        let i = if f.italic == req.italic { 0 } else { 1000 };
        // Prefer TrueType/OpenType over Type 1 for the same family.
        let t1 = f
            .path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("pfb") || e.eq_ignore_ascii_case("t1"));
        w + i + u32::from(t1)
    })
}

/// Lowercase without spaces and punctuation.
pub fn normalize(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn strip_vendor(n: &str) -> &str {
    n.strip_suffix("psmt")
        .or_else(|| n.strip_suffix("mt"))
        .or_else(|| n.strip_suffix("ps"))
        .unwrap_or(n)
}

/// Splits `Arial,BoldItalic` / `Helvetica-BoldOblique` / `TimesNewRomanPS-BoldMT` into
/// family and style.
pub fn split_style(name: &str) -> (&str, &str) {
    let name = strip_subset_tag(name);
    if let Some(p) = name.find([',', '-']) {
        return (&name[..p], &name[p + 1..]);
    }
    for suffix in [
        "BoldItalic",
        "BoldOblique",
        "Bold",
        "Italic",
        "Oblique",
        "Black",
        "Semibold",
        "Light",
    ] {
        if let Some(f) = name.strip_suffix(suffix)
            && !f.is_empty()
        {
            return (f, &name[f.len()..]);
        }
    }
    (name, "")
}

/// Weight from a style or weight name.
pub fn weight_from_name(s: &str) -> Option<u16> {
    let s = s.to_ascii_lowercase();
    [
        ("thin", 100),
        ("extralight", 200),
        ("ultralight", 200),
        ("light", 300),
        ("semibold", 600),
        ("demibold", 600),
        ("demi", 600),
        ("extrabold", 800),
        ("ultrabold", 800),
        ("heavy", 900),
        ("black", 900),
        ("bold", 700),
        ("medium", 500),
        ("book", 400),
        ("regular", 400),
        ("roman", 400),
        ("normal", 400),
    ]
    .iter()
    .find(|(n, _)| s.contains(n))
    .map(|w| w.1)
}

fn is_cjk_serif_name(fam: &str) -> bool {
    [
        "mincho", "ming", "song", "batang", "myeongjo", "serif", "kaiti", "fangsong",
    ]
    .iter()
    .any(|k| fam.contains(k))
}

/// Normalized family names with a fixed class.
const NAME_CLASSES: &[(&str, Class)] = &[
    ("helvetica", Class::Sans),
    ("arial", Class::Sans),
    ("times", Class::Serif),
    ("courier", Class::Mono),
    ("symbol", Class::Symbol),
    ("zapfdingbats", Class::Dingbats),
    ("dingbats", Class::Dingbats),
    ("wingdings", Class::Dingbats),
    ("verdana", Class::Sans),
    ("tahoma", Class::Sans),
    ("calibri", Class::Sans),
    ("segoeui", Class::Sans),
    ("cambria", Class::Serif),
    ("georgia", Class::Serif),
    ("garamond", Class::Serif),
    ("bookantiqua", Class::Serif),
    ("palatino", Class::Serif),
    ("consolas", Class::Mono),
    ("lucidaconsole", Class::Mono),
    ("msgothic", Class::Japanese(false)),
    ("msmincho", Class::Japanese(true)),
    ("simsun", Class::Chinese(true)),
    ("simhei", Class::Chinese(false)),
    ("mingliu", Class::ChineseTraditional(true)),
    ("batang", Class::Korean(true)),
    ("gulim", Class::Korean(false)),
    ("dotum", Class::Korean(false)),
];

/// Metric-compatible or close replacements of specific fonts (normalized family names).
const REPLACEMENTS: &[(&str, &[&str])] = &[
    (
        "helvetica",
        &[
            "Liberation Sans",
            "Arimo",
            "Nimbus Sans",
            "Nimbus Sans L",
            "TeX Gyre Heros",
        ],
    ),
    (
        "arial",
        &[
            "Arial",
            "Liberation Sans",
            "Arimo",
            "Nimbus Sans",
            "Nimbus Sans L",
        ],
    ),
    (
        "arialnarrow",
        &["Liberation Sans Narrow", "Nimbus Sans Narrow"],
    ),
    (
        "times",
        &[
            "Liberation Serif",
            "Tinos",
            "Nimbus Roman",
            "Nimbus Roman No9 L",
            "TeX Gyre Termes",
        ],
    ),
    (
        "timesnewroman",
        &[
            "Times New Roman",
            "Liberation Serif",
            "Tinos",
            "Nimbus Roman",
        ],
    ),
    (
        "timesnewromanps",
        &[
            "Times New Roman",
            "Liberation Serif",
            "Tinos",
            "Nimbus Roman",
        ],
    ),
    (
        "courier",
        &[
            "Liberation Mono",
            "Cousine",
            "Nimbus Mono PS",
            "Nimbus Mono L",
            "TeX Gyre Cursor",
        ],
    ),
    (
        "couriernew",
        &[
            "Courier New",
            "Liberation Mono",
            "Cousine",
            "Nimbus Mono PS",
        ],
    ),
    (
        "couriernewps",
        &[
            "Courier New",
            "Liberation Mono",
            "Cousine",
            "Nimbus Mono PS",
        ],
    ),
    (
        "symbol",
        &["Standard Symbols PS", "Standard Symbols L", "Symbol"],
    ),
    (
        "zapfdingbats",
        &["D050000L", "Dingbats", "Zapf Dingbats", "ITC Zapf Dingbats"],
    ),
    ("calibri", &["Carlito"]),
    ("cambria", &["Caladea"]),
    ("georgia", &["Gelasio"]),
    (
        "palatino",
        &[
            "P052",
            "TeX Gyre Pagella",
            "URW Palladio L",
            "Palatino Linotype",
        ],
    ),
    (
        "bookantiqua",
        &["P052", "TeX Gyre Pagella", "URW Palladio L"],
    ),
    (
        "bookman",
        &["URW Bookman", "URW Bookman L", "TeX Gyre Bonum"],
    ),
    (
        "newcenturyschlbk",
        &["C059", "Century Schoolbook L", "TeX Gyre Schola"],
    ),
    (
        "centuryschoolbook",
        &["C059", "Century Schoolbook L", "TeX Gyre Schola"],
    ),
    (
        "avantgarde",
        &["URW Gothic", "URW Gothic L", "TeX Gyre Adventor"],
    ),
    (
        "zapfchancery",
        &["Z003", "URW Chancery L", "TeX Gyre Chorus"],
    ),
    ("segoeui", &["Selawik", "Open Sans", "Noto Sans"]),
];

const SANS: &[&str] = &[
    "Liberation Sans",
    "Arimo",
    "Helvetica",
    "Arial",
    "Nimbus Sans",
    "Nimbus Sans L",
    "TeX Gyre Heros",
    "FreeSans",
    "DejaVu Sans",
    "Noto Sans",
    "Roboto",
    "Droid Sans",
    "Open Sans",
    "Segoe UI",
    "Verdana",
];
const NARROW: &[&str] = &[
    "Liberation Sans Narrow",
    "Nimbus Sans Narrow",
    "Arial Narrow",
];
const SERIF: &[&str] = &[
    "Liberation Serif",
    "Tinos",
    "Times New Roman",
    "Times",
    "Nimbus Roman",
    "Nimbus Roman No9 L",
    "TeX Gyre Termes",
    "FreeSerif",
    "DejaVu Serif",
    "Noto Serif",
    "Droid Serif",
    "Georgia",
];
const MONO: &[&str] = &[
    "Liberation Mono",
    "Cousine",
    "Courier New",
    "Courier",
    "Nimbus Mono PS",
    "Nimbus Mono L",
    "TeX Gyre Cursor",
    "FreeMono",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Droid Sans Mono",
    "Menlo",
    "Consolas",
];
const SYMBOL: &[&str] = &[
    "Standard Symbols PS",
    "Standard Symbols L",
    "Symbol",
    "OpenSymbol",
];
const DINGBATS: &[&str] = &[
    "D050000L",
    "Dingbats",
    "Zapf Dingbats",
    "ITC Zapf Dingbats",
    "Wingdings",
];
const JAPANESE_SANS: [&str; 14] = [
    "Noto Sans CJK JP",
    "Source Han Sans JP",
    "Source Han Sans",
    "Hiragino Sans",
    "Hiragino Kaku Gothic ProN",
    "Yu Gothic",
    "Meiryo",
    "MS Gothic",
    "IPAexGothic",
    "IPAGothic",
    "TakaoGothic",
    "VL Gothic",
    "Droid Sans Fallback",
    "WenQuanYi Micro Hei",
];
const JAPANESE_SERIF: [&str; 8] = [
    "Noto Serif CJK JP",
    "Source Han Serif JP",
    "Hiragino Mincho ProN",
    "Yu Mincho",
    "MS Mincho",
    "IPAexMincho",
    "IPAMincho",
    "TakaoMincho",
];
const CHINESE_SANS: [&str; 9] = [
    "Noto Sans CJK SC",
    "Source Han Sans SC",
    "PingFang SC",
    "Microsoft YaHei",
    "SimHei",
    "WenQuanYi Micro Hei",
    "WenQuanYi Zen Hei",
    "Droid Sans Fallback",
    "Noto Sans CJK JP",
];
const CHINESE_SERIF: [&str; 6] = [
    "Noto Serif CJK SC",
    "Source Han Serif SC",
    "Songti SC",
    "SimSun",
    "AR PL UMing CN",
    "AR PL SungtiL GB",
];
const TRAD_SANS: [&str; 7] = [
    "Noto Sans CJK TC",
    "Source Han Sans TC",
    "PingFang TC",
    "Microsoft JhengHei",
    "WenQuanYi Micro Hei",
    "Droid Sans Fallback",
    "Noto Sans CJK JP",
];
const TRAD_SERIF: [&str; 5] = [
    "Noto Serif CJK TC",
    "Source Han Serif TC",
    "MingLiU",
    "PMingLiU",
    "AR PL UMing TW",
];
const KOREAN_SANS: [&str; 8] = [
    "Noto Sans CJK KR",
    "Source Han Sans KR",
    "Apple SD Gothic Neo",
    "Malgun Gothic",
    "NanumGothic",
    "Gulim",
    "UnDotum",
    "Noto Sans CJK JP",
];
const KOREAN_SERIF: [&str; 5] = [
    "Noto Serif CJK KR",
    "Source Han Serif KR",
    "Batang",
    "NanumMyeongjo",
    "UnBatang",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn req(name: &str, flags: u32) -> FontRequest {
        let mut s = FontSpec::new(crate::pdf::PdfFontType::Type1);
        s.base_font = Some(name.into());
        s.flags = flags;
        FontRequest::from_spec(&s)
    }

    #[test]
    fn model() {
        let r = req("Helvetica-BoldOblique", 0);
        assert!(r.bold && r.italic);
        assert_eq!(r.class(), Class::Sans);
        assert_eq!(candidates(&r)[0], "Liberation Sans");
        let r = req("ABCDEF+TimesNewRomanPS-ItalicMT", 2);
        assert!(!r.bold && r.italic);
        assert_eq!(r.class(), Class::Serif);
        assert_eq!(candidates(&r)[0], "Times New Roman");
        assert_eq!(req("Courier", 0).class(), Class::Mono);
        assert_eq!(req("Foo", 1).class(), Class::Mono);
        assert_eq!(req("Foo", 2).class(), Class::Serif);
        assert_eq!(req("Foo,Bold", 0).class(), Class::Sans);
        assert!(req("Foo,Bold", 0).bold);
        assert!(req("Foo", 1 << 18).bold);
        assert_eq!(req("Symbol", 4).class(), Class::Symbol);
        assert_eq!(req("ZapfDingbats", 4).class(), Class::Dingbats);
        assert_eq!(candidates(&req("Calibri-Bold", 0))[0], "Carlito");
        let mut s = FontSpec::new(crate::pdf::PdfFontType::CidType0);
        s.base_font = Some("KozMinPr6N-Regular".into());
        s.ordering = Some("Japan1".into());
        s.flags = 2;
        let r = FontRequest::from_spec(&s);
        assert_eq!(r.class(), Class::Japanese(true));
        assert_eq!(candidates(&r)[0], "Noto Serif CJK JP");
        assert_eq!(split_style("Arial,BoldItalic"), ("Arial", "BoldItalic"));
        assert_eq!(split_style("ArialBold"), ("Arial", "Bold"));
        assert_eq!(weight_from_name("SemiBold"), Some(600));
    }
}
