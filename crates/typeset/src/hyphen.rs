//! Liang's hyphenation (TeX's algorithm) with TeX patterns: US English from plain TeX
//! (`data/hyphen-en-us.tex`, Knuth's `hyphen.tex`, unmodified) and other languages from
//! hyph-utf8 (`data/hyph`, compressed at build time and decoded on first use). See
//! THIRD-PARTY.md and LICENSE-hyphenation.
//!
//! Languages without patterns here: Chinese, Japanese, Korean, Arabic and Hebrew are not
//! hyphenated; Russian and Turkish patterns are LPPL-only, so not included.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

/// A set of hyphenation patterns and exceptions.
#[derive(Debug, Default)]
pub struct Hyphenator {
    /// Letters → priorities between them (one more than the letters).
    patterns: HashMap<Box<str>, Box<[u8]>>,
    /// Lowercase words → character positions of their hyphens.
    exceptions: HashMap<String, Vec<usize>>,
    /// Longest pattern in characters.
    max_len: usize,
    /// The lowercase letters the patterns know; words with others are not hyphenated.
    alphabet: HashSet<char>,
    /// Letters kept before the first and after the last hyphen.
    pub left_min: usize,
    pub right_min: usize,
}

/// A language with hyphenation patterns.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Lang {
    EnUs,
    EnGb,
    De,
    Fr,
    Es,
    It,
    Pt,
    Nl,
    Pl,
    Uk,
}

/// The hyphenator for a BCP 47 language tag (also PDF `/Lang` values and POSIX locales:
/// `de-DE`, `pt-BR`, `en-GB`, `de-1996`, `fr_CA.UTF-8`); `None` when there are no
/// patterns for the language.
pub fn for_language(tag: &str) -> Option<&'static Hyphenator> {
    Lang::from_tag(tag).map(Lang::hyphenator)
}

/// US English (plain TeX patterns, `\lefthyphenmin=2`, `\righthyphenmin=3`).
pub fn english() -> &'static Hyphenator {
    Lang::EnUs.hyphenator()
}

const EN_US: &str = include_str!("../data/hyphen-en-us.tex");

macro_rules! patterns {
    ($name:literal) => {
        include_bytes!(concat!(env!("OUT_DIR"), "/hyph-", $name, ".deflate"))
    };
}

impl Lang {
    pub const ALL: [Lang; 10] = [
        Lang::EnUs,
        Lang::EnGb,
        Lang::De,
        Lang::Fr,
        Lang::Es,
        Lang::It,
        Lang::Pt,
        Lang::Nl,
        Lang::Pl,
        Lang::Uk,
    ];

    /// The language of a tag (see [`for_language`]): its primary subtag, and for English
    /// the region (British spelling for the UK, Ireland, Australia, New Zealand, South
    /// Africa and India).
    pub fn from_tag(tag: &str) -> Option<Lang> {
        let mut parts = tag.split(['-', '_', '.', '@']).map(str::to_ascii_lowercase);
        Some(match parts.next()?.as_str() {
            "en" | "eng" => {
                let gb = |p: String| {
                    matches!(p.as_str(), "gb" | "uk" | "ie" | "au" | "nz" | "za" | "in")
                };
                if parts.any(gb) {
                    Lang::EnGb
                } else {
                    Lang::EnUs
                }
            }
            "de" | "deu" | "ger" | "gsw" => Lang::De,
            "fr" | "fra" | "fre" => Lang::Fr,
            "es" | "spa" => Lang::Es,
            "it" | "ita" => Lang::It,
            "pt" | "por" => Lang::Pt,
            "nl" | "nld" | "dut" => Lang::Nl,
            "pl" | "pol" => Lang::Pl,
            "uk" | "ukr" => Lang::Uk,
            _ => return None,
        })
    }

    /// The tag of the patterns.
    pub fn tag(self) -> &'static str {
        match self {
            Lang::EnUs => "en-US",
            Lang::EnGb => "en-GB",
            Lang::De => "de",
            Lang::Fr => "fr",
            Lang::Es => "es",
            Lang::It => "it",
            Lang::Pt => "pt",
            Lang::Nl => "nl",
            Lang::Pl => "pl",
            Lang::Uk => "uk",
        }
    }

    /// The language's patterns, read on first use.
    pub fn hyphenator(self) -> &'static Hyphenator {
        static LOADED: [OnceLock<Hyphenator>; Lang::ALL.len()] =
            [const { OnceLock::new() }; Lang::ALL.len()];
        LOADED[self as usize].get_or_init(|| {
            // Left and right hyphen mins from the hyph-utf8 files ("typesetting").
            let (data, left, right): (&[u8], _, _) = match self {
                Lang::EnUs => return Hyphenator::from_tex(EN_US, 2, 3),
                Lang::EnGb => (patterns!("en-gb"), 2, 3),
                Lang::De => (patterns!("de-1996"), 2, 2),
                Lang::Fr => (patterns!("fr"), 2, 2),
                Lang::Es => (patterns!("es"), 2, 2),
                Lang::It => (patterns!("it"), 2, 2),
                Lang::Pt => (patterns!("pt"), 2, 3),
                Lang::Nl => (patterns!("nl"), 2, 2),
                Lang::Pl => (patterns!("pl"), 2, 2),
                Lang::Uk => (patterns!("uk"), 2, 2),
            };
            let text = miniz_oxide::inflate::decompress_to_vec(data).unwrap_or_default();
            let text = String::from_utf8_lossy(&text);
            let (pat, hyp) = text.split_once('\0').unwrap_or((&text, ""));
            Hyphenator::from_lists(pat.split_whitespace(), hyp.split_whitespace(), left, right)
        })
    }
}

/// The body of `\name{...}` in a TeX file, without comments.
fn tex_group<'a>(src: &'a str, name: &str) -> Vec<&'a str> {
    let Some(at) = src.find(name) else {
        return Vec::new();
    };
    let rest = &src[at + name.len()..];
    let Some(open) = rest.find('{') else {
        return Vec::new();
    };
    let body = &rest[open + 1..];
    let end = body.find('}').unwrap_or(body.len());
    body[..end]
        .lines()
        .map(|l| l.split('%').next().unwrap_or(""))
        .flat_map(str::split_whitespace)
        .collect()
}

/// A character as the patterns have it: lowercase (`İ` is `i`, not `i` and a dot).
fn lower(c: char) -> char {
    if c == 'İ' {
        'i'
    } else {
        c.to_lowercase().next().unwrap_or(c)
    }
}

impl Hyphenator {
    /// Reads `\patterns{...}` and `\hyphenation{...}` from a TeX pattern file.
    pub fn from_tex(src: &str, left_min: usize, right_min: usize) -> Hyphenator {
        Hyphenator::from_lists(
            tex_group(src, "\\patterns"),
            tex_group(src, "\\hyphenation"),
            left_min,
            right_min,
        )
    }

    /// Builds a hyphenator from patterns (`.hy3ph`) and exceptions (`hy-phen-ation`).
    pub fn from_lists<'a>(
        patterns: impl IntoIterator<Item = &'a str>,
        exceptions: impl IntoIterator<Item = &'a str>,
        left_min: usize,
        right_min: usize,
    ) -> Hyphenator {
        let mut h = Hyphenator {
            left_min,
            right_min,
            ..Hyphenator::default()
        };
        for p in patterns {
            let mut letters = String::new();
            let mut prio = vec![0u8];
            for c in p.chars() {
                if let Some(d) = c.to_digit(10) {
                    *prio.last_mut().unwrap_or(&mut 0) = d as u8;
                } else {
                    letters.push(c);
                    prio.push(0);
                    if c != '.' {
                        h.alphabet.insert(c);
                    }
                }
            }
            h.max_len = h.max_len.max(prio.len() - 1);
            h.patterns.insert(letters.into(), prio.into());
        }
        for w in exceptions {
            let mut word = String::new();
            let mut points = Vec::new();
            for c in w.chars() {
                if c == '-' {
                    points.push(word.chars().count());
                } else {
                    word.push(lower(c));
                }
            }
            h.exceptions.insert(word, points);
        }
        h
    }

    /// Whether `c` can be part of a word the patterns hyphenate (letters, and apostrophes
    /// for languages whose patterns include them).
    pub fn is_letter(&self, c: char) -> bool {
        self.alphabet.contains(&lower(c))
    }

    /// Byte offsets in `word` where it may be hyphenated (a hyphen goes before the
    /// character at the offset). Only words of letters the patterns know are hyphenated.
    pub fn hyphenate(&self, word: &str) -> Vec<usize> {
        // The word lowercase between dots, and the byte offset in `word` of each letter.
        let mut w = String::with_capacity(word.len() + 2);
        let mut offsets = Vec::with_capacity(word.len());
        w.push('.');
        for (i, c) in word.char_indices() {
            let l = lower(c);
            if !self.alphabet.contains(&l) {
                return Vec::new();
            }
            w.push(l);
            offsets.push(i);
        }
        w.push('.');
        let n = offsets.len();
        if n < self.left_min + self.right_min {
            return Vec::new();
        }
        let ok = |p: usize| p >= self.left_min && n - p >= self.right_min;
        if let Some(e) = self.exceptions.get(&w[1..w.len() - 1]) {
            return e
                .iter()
                .filter(|&&p| p < n && ok(p))
                .map(|&p| offsets[p])
                .collect();
        }
        let bounds: Vec<usize> = w
            .char_indices()
            .map(|(i, _)| i)
            .chain(std::iter::once(w.len()))
            .collect();
        let len = n + 2;
        let mut points = vec![0u8; len + 1];
        for i in 0..len {
            for j in i + 1..=(i + self.max_len).min(len) {
                if let Some(p) = self.patterns.get(&w[bounds[i]..bounds[j]]) {
                    for (k, &v) in p.iter().enumerate() {
                        points[i + k] = points[i + k].max(v);
                    }
                }
            }
        }
        // points[k] is before letter k of `w`; letter p of the word is letter p + 1 of `w`.
        (1..n)
            .filter(|&p| points[p + 1] % 2 == 1 && ok(p))
            .map(|p| offsets[p])
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hy_in(h: &Hyphenator, w: &str) -> String {
        let pts = h.hyphenate(w);
        let mut s = String::new();
        for (i, c) in w.char_indices() {
            if pts.contains(&i) {
                s.push('-');
            }
            s.push(c);
        }
        s
    }

    fn hy(w: &str) -> String {
        hy_in(english(), w)
    }

    fn lang(tag: &str, w: &str) -> String {
        hy_in(for_language(tag).expect(tag), w)
    }

    #[test]
    fn english_words() {
        assert_eq!(hy("hyphenation"), "hy-phen-ation");
        assert_eq!(hy("Computer"), "Com-puter");
        assert_eq!(hy("algorithm"), "al-go-rithm");
        assert_eq!(hy("typesetting"), "type-set-ting");
        assert_eq!(hy("table"), "ta-ble");
        assert_eq!(hy("project"), "project");
        assert_eq!(hy("cat"), "cat");
        assert_eq!(hy("naïve"), "naïve");
    }

    #[test]
    fn languages() {
        assert_eq!(lang("en-GB", "hyphenation"), "hy-phen-a-tion");
        assert_eq!(lang("en-GB", "university"), "uni-ver-sity");
        assert_eq!(lang("de-DE", "Silbentrennung"), "Sil-ben-tren-nung");
        assert_eq!(lang("de", "Größenänderung"), "Grö-ßen-än-de-rung");
        assert_eq!(lang("fr", "hyphénation"), "hy-phé-na-tion");
        assert_eq!(lang("es", "silabeo"), "si-la-beo");
        assert_eq!(lang("es-MX", "murciélago"), "mur-cié-la-go");
        assert_eq!(lang("it", "sillabazione"), "sil-la-ba-zio-ne");
        assert_eq!(lang("pt-BR", "hifenização"), "hi-fe-ni-za-ção");
        assert_eq!(lang("nl", "lettergreep"), "let-ter-greep");
        assert_eq!(lang("pl", "przenoszenie"), "prze-no-sze-nie");
        assert_eq!(lang("uk", "м'ята"), "м'я-та");
        assert_eq!(lang("uk-UA", "Україна"), "Укра-ї-на");
    }

    #[test]
    fn mins_exceptions_and_other_scripts() {
        // German keeps two letters on each side; English three after the last hyphen.
        assert_eq!(lang("de", "Ofen"), "Ofen");
        assert_eq!(lang("en-GB", "however"), "how-ever");
        assert_eq!(lang("de", "перенос"), "перенос");
        assert_eq!(lang("uk", "word"), "word");
        // French patterns know apostrophes: elided words stay whole.
        assert_eq!(lang("fr", "l’hyphénation"), "l’hy-phé-na-tion");
    }

    #[test]
    fn tags() {
        assert_eq!(Lang::from_tag("de-1996"), Some(Lang::De));
        assert_eq!(Lang::from_tag("de-CH-1901"), Some(Lang::De));
        assert_eq!(Lang::from_tag("pt-BR"), Some(Lang::Pt));
        assert_eq!(Lang::from_tag("EN-gb"), Some(Lang::EnGb));
        assert_eq!(Lang::from_tag("en-Latn-AU"), Some(Lang::EnGb));
        assert_eq!(Lang::from_tag("en"), Some(Lang::EnUs));
        assert_eq!(Lang::from_tag("en_US.UTF-8"), Some(Lang::EnUs));
        assert_eq!(Lang::from_tag("fr_CA.UTF-8"), Some(Lang::Fr));
        assert_eq!(Lang::from_tag("uk"), Some(Lang::Uk));
        for t in ["ja", "zh-Hans", "ko", "ar", "he", "ru", "tr", "", "x"] {
            assert_eq!(Lang::from_tag(t), None, "{t}");
        }
        for l in Lang::ALL {
            assert_eq!(Lang::from_tag(l.tag()), Some(l));
            assert!(l.hyphenator().patterns.len() > 100, "{l:?}");
        }
    }
}
