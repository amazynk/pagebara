//! Fonts for text in any script: a coarse script of each character, preferred families per
//! script, and a search of the [`FontDb`] for an embeddable face that maps a character.

use super::{FaceInfo, FontDb, FontRequest, best_style, normalize};

/// The writing system of a character, as far as choosing a font goes. Punctuation, digits
/// and other common characters count as Latin.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Script {
    Latin,
    Greek,
    Cyrillic,
    Armenian,
    Hebrew,
    Arabic,
    Syriac,
    Thaana,
    Devanagari,
    Bengali,
    Gurmukhi,
    Gujarati,
    Oriya,
    Tamil,
    Telugu,
    Kannada,
    Malayalam,
    Sinhala,
    Thai,
    Lao,
    Tibetan,
    Myanmar,
    Georgian,
    Ethiopic,
    Khmer,
    Hangul,
    /// Hiragana and Katakana.
    Kana,
    /// Ideographs, CJK punctuation and full-width forms.
    Han,
    /// Arrows, mathematical operators, technical and miscellaneous symbols, dingbats.
    Symbols,
    Other,
}

/// Sorted, non-overlapping ranges; characters outside them are Latin below U+0250 and Other
/// above.
const RANGES: &[(u32, u32, Script)] = &[
    (0x0250, 0x036F, Script::Latin),
    (0x0370, 0x03FF, Script::Greek),
    (0x0400, 0x052F, Script::Cyrillic),
    (0x0530, 0x058F, Script::Armenian),
    (0x0590, 0x05FF, Script::Hebrew),
    (0x0600, 0x06FF, Script::Arabic),
    (0x0700, 0x074F, Script::Syriac),
    (0x0750, 0x077F, Script::Arabic),
    (0x0780, 0x07BF, Script::Thaana),
    (0x08A0, 0x08FF, Script::Arabic),
    (0x0900, 0x097F, Script::Devanagari),
    (0x0980, 0x09FF, Script::Bengali),
    (0x0A00, 0x0A7F, Script::Gurmukhi),
    (0x0A80, 0x0AFF, Script::Gujarati),
    (0x0B00, 0x0B7F, Script::Oriya),
    (0x0B80, 0x0BFF, Script::Tamil),
    (0x0C00, 0x0C7F, Script::Telugu),
    (0x0C80, 0x0CFF, Script::Kannada),
    (0x0D00, 0x0D7F, Script::Malayalam),
    (0x0D80, 0x0DFF, Script::Sinhala),
    (0x0E00, 0x0E7F, Script::Thai),
    (0x0E80, 0x0EFF, Script::Lao),
    (0x0F00, 0x0FFF, Script::Tibetan),
    (0x1000, 0x109F, Script::Myanmar),
    (0x10A0, 0x10FF, Script::Georgian),
    (0x1100, 0x11FF, Script::Hangul),
    (0x1200, 0x139F, Script::Ethiopic),
    (0x1780, 0x17FF, Script::Khmer),
    (0x1C80, 0x1C8F, Script::Cyrillic),
    (0x1C90, 0x1CBF, Script::Georgian),
    (0x1E00, 0x1EFF, Script::Latin),
    (0x1F00, 0x1FFF, Script::Greek),
    (0x2000, 0x206F, Script::Latin),
    (0x2070, 0x20CF, Script::Latin),
    (0x20D0, 0x2BFF, Script::Symbols),
    (0x2C60, 0x2C7F, Script::Latin),
    (0x2D00, 0x2D2F, Script::Georgian),
    (0x2DE0, 0x2DFF, Script::Cyrillic),
    (0x2E80, 0x303F, Script::Han),
    (0x3040, 0x30FF, Script::Kana),
    (0x3100, 0x312F, Script::Han),
    (0x3130, 0x318F, Script::Hangul),
    (0x3190, 0x31EF, Script::Han),
    (0x31F0, 0x31FF, Script::Kana),
    (0x3200, 0x9FFF, Script::Han),
    (0xA640, 0xA69F, Script::Cyrillic),
    (0xA720, 0xA7FF, Script::Latin),
    (0xA8E0, 0xA8FF, Script::Devanagari),
    (0xAC00, 0xD7FF, Script::Hangul),
    (0xF900, 0xFAFF, Script::Han),
    (0xFB00, 0xFB1C, Script::Latin),
    (0xFB1D, 0xFB4F, Script::Hebrew),
    (0xFB50, 0xFDFF, Script::Arabic),
    (0xFE30, 0xFE4F, Script::Han),
    (0xFE70, 0xFEFF, Script::Arabic),
    (0xFF00, 0xFF65, Script::Han),
    (0xFF66, 0xFF9F, Script::Kana),
    (0xFFA0, 0xFFDF, Script::Hangul),
    (0xFFE0, 0xFFEF, Script::Han),
    (0x1F000, 0x1FAFF, Script::Symbols),
    (0x20000, 0x3FFFF, Script::Han),
];

/// Families of last resort for any script.
const GENERIC: &[&str] = &[
    "Noto Sans",
    "DejaVu Sans",
    "FreeSans",
    "FreeSerif",
    "Arial Unicode MS",
];

const SANS: &[&str] = &[
    "Noto Sans",
    "DejaVu Sans",
    "Liberation Sans",
    "Arimo",
    "Arial",
    "Helvetica",
    "Segoe UI",
    "Roboto",
    "Open Sans",
    "Ubuntu",
    "Verdana",
    "FreeSans",
];

const HAN: &[&str] = &[
    "Noto Sans CJK SC",
    "Noto Sans SC",
    "Source Han Sans SC",
    "Source Han Sans CN",
    "Microsoft YaHei",
    "PingFang SC",
    "Hiragino Sans GB",
    "WenQuanYi Zen Hei",
    "WenQuanYi Micro Hei",
    "Droid Sans Fallback",
    "SimHei",
    "SimSun",
    "Noto Sans CJK JP",
    "Noto Sans CJK TC",
    "Noto Sans CJK KR",
];

const KANA: &[&str] = &[
    "Noto Sans CJK JP",
    "Noto Sans JP",
    "Source Han Sans JP",
    "Yu Gothic",
    "Meiryo",
    "MS Gothic",
    "Hiragino Sans",
    "Hiragino Kaku Gothic ProN",
    "TakaoGothic",
    "IPAGothic",
    "VL Gothic",
    "Droid Sans Fallback",
];

const HANGUL: &[&str] = &[
    "Noto Sans CJK KR",
    "Noto Sans KR",
    "Source Han Sans KR",
    "Malgun Gothic",
    "Apple SD Gothic Neo",
    "NanumGothic",
    "UnDotum",
    "Baekmuk Gulim",
];

impl Script {
    pub fn of(c: char) -> Script {
        let u = u32::from(c);
        let i = RANGES.partition_point(|r| r.0 <= u);
        match i.checked_sub(1).map(|i| RANGES[i]) {
            Some((_, end, s)) if u <= end => s,
            _ if u < 0x0250 => Script::Latin,
            _ => Script::Other,
        }
    }

    /// Whether the script is written right to left.
    pub fn is_rtl(self) -> bool {
        matches!(
            self,
            Script::Hebrew | Script::Arabic | Script::Syriac | Script::Thaana
        )
    }

    /// Preferred families (sans serif first), best first.
    pub fn families(self) -> &'static [&'static str] {
        match self {
            Script::Latin | Script::Greek | Script::Cyrillic => SANS,
            Script::Armenian => &["Noto Sans Armenian", "DejaVu Sans", "Sylfaen"],
            Script::Hebrew => &[
                "Noto Sans Hebrew",
                "DejaVu Sans",
                "Arial",
                "Arial Hebrew",
                "Liberation Sans",
                "Tahoma",
            ],
            Script::Arabic => &[
                "Noto Sans Arabic",
                "Noto Naskh Arabic",
                "DejaVu Sans",
                "Arial",
                "Segoe UI",
                "Tahoma",
                "Geeza Pro",
                "Amiri",
            ],
            Script::Syriac => &["Noto Sans Syriac", "Estrangelo Edessa"],
            Script::Thaana => &["Noto Sans Thaana", "MV Boli"],
            Script::Devanagari => &[
                "Noto Sans Devanagari",
                "Lohit Devanagari",
                "Nirmala UI",
                "Mangal",
                "Kohinoor Devanagari",
                "Devanagari Sangam MN",
            ],
            Script::Bengali => &["Noto Sans Bengali", "Lohit Bengali", "Nirmala UI", "Vrinda"],
            Script::Gurmukhi => &["Noto Sans Gurmukhi", "Lohit Gurmukhi", "Nirmala UI"],
            Script::Gujarati => &["Noto Sans Gujarati", "Lohit Gujarati", "Nirmala UI"],
            Script::Oriya => &["Noto Sans Oriya", "Lohit Odia", "Nirmala UI"],
            Script::Tamil => &["Noto Sans Tamil", "Lohit Tamil", "Nirmala UI", "Latha"],
            Script::Telugu => &["Noto Sans Telugu", "Lohit Telugu", "Nirmala UI"],
            Script::Kannada => &["Noto Sans Kannada", "Lohit Kannada", "Nirmala UI"],
            Script::Malayalam => &["Noto Sans Malayalam", "Lohit Malayalam", "Nirmala UI"],
            Script::Sinhala => &["Noto Sans Sinhala", "LKLUG", "Iskoola Pota"],
            Script::Thai => &[
                "Noto Sans Thai",
                "Loma",
                "Garuda",
                "Leelawadee UI",
                "Tahoma",
                "Thonburi",
            ],
            Script::Lao => &["Noto Sans Lao", "Phetsarath OT", "Lao UI"],
            Script::Tibetan => &["Noto Sans Tibetan", "Tibetan Machine Uni"],
            Script::Myanmar => &["Noto Sans Myanmar", "Padauk", "Myanmar Text"],
            Script::Georgian => &["Noto Sans Georgian", "DejaVu Sans", "Sylfaen"],
            Script::Ethiopic => &["Noto Sans Ethiopic", "Abyssinica SIL", "Nyala"],
            Script::Khmer => &["Noto Sans Khmer", "Khmer OS", "Khmer UI"],
            Script::Hangul => HANGUL,
            Script::Kana => KANA,
            Script::Han => HAN,
            Script::Symbols => &[
                "DejaVu Sans",
                "Noto Sans Symbols",
                "Noto Sans Symbols 2",
                "Noto Sans Math",
                "Segoe UI Symbol",
                "Apple Symbols",
                "Symbola",
            ],
            Script::Other => &[],
        }
    }

    /// `OS/2` `ulUnicodeRange` bits of the script's blocks.
    pub fn range_bits(self) -> &'static [u32] {
        match self {
            Script::Latin => &[0, 1, 2, 3, 29],
            Script::Greek => &[7, 30],
            Script::Cyrillic => &[9],
            Script::Armenian => &[10],
            Script::Hebrew => &[11],
            Script::Arabic => &[13, 63, 67],
            Script::Syriac => &[71],
            Script::Thaana => &[72],
            Script::Devanagari => &[15],
            Script::Bengali => &[16],
            Script::Gurmukhi => &[17],
            Script::Gujarati => &[18],
            Script::Oriya => &[19],
            Script::Tamil => &[20],
            Script::Telugu => &[21],
            Script::Kannada => &[22],
            Script::Malayalam => &[23],
            Script::Sinhala => &[73],
            Script::Thai => &[24],
            Script::Lao => &[25],
            Script::Tibetan => &[70],
            Script::Myanmar => &[74],
            Script::Georgian => &[26],
            Script::Ethiopic => &[75],
            Script::Khmer => &[80],
            Script::Hangul => &[28, 56],
            Script::Kana => &[49, 50],
            Script::Han => &[59, 48, 61],
            Script::Symbols => &[37, 38, 39, 46, 47],
            Script::Other => &[],
        }
    }
}

/// Faces loaded to test coverage in the search of faces by `OS/2` ranges, at most.
const MAX_RANGE_CANDIDATES: usize = 24;

/// An embeddable TrueType/OpenType face that maps `c`, in regular style when there is a
/// choice: the script's preferred families, the generic ones, then faces whose `OS/2`
/// ranges claim the script.
pub fn find_for_char(db: &FontDb, c: char) -> Option<&FaceInfo> {
    let script = Script::of(c);
    let usable = |f: &&FaceInfo| f.sfnt && f.embeddable();
    let covers = |f: &FaceInfo| {
        db.load(f)
            .and_then(|font| font.glyph_for_char(c))
            .is_some_and(|g| g != 0)
    };
    let req = FontRequest::default();
    let mut tried = Vec::new();
    for fam in script.families().iter().chain(GENERIC) {
        let n = normalize(fam);
        let face = best_style(
            db.faces()
                .iter()
                .filter(usable)
                .filter(|f| normalize(&f.family) == n),
            &req,
        );
        if let Some(f) = face
            && !tried.contains(&f)
        {
            if covers(f) {
                return Some(f);
            }
            tried.push(f);
        }
    }
    let bits = script.range_bits();
    db.faces()
        .iter()
        .filter(usable)
        .filter(|f| !f.italic && bits.iter().any(|&b| f.claims_range(b)))
        .filter(|f| !tried.contains(f))
        .take(MAX_RANGE_CANDIDATES)
        .find(|f| covers(f))
}
