//! Finding what to redact: text, regular expressions and common personal data, with the
//! areas of each match ready to become marks.

use pagebara_content::{Interpreter, TextPage, extract_text};
use pagebara_syntax::Document;
use regex_lite::Regex;

use crate::geom::Point;
use crate::mark::Mark;

/// Personal data with a recognisable shape. Matches are validated (checksums, digit
/// counts), so most look-alikes are skipped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Email,
    /// International or national phone numbers: 7 to 15 digits with the usual separators.
    Phone,
    /// IBANs with a valid mod-97 checksum.
    Iban,
    /// Payment card numbers (13–19 digits) with a valid Luhn checksum.
    CreditCard,
}

/// What to look for.
#[derive(Clone, Debug)]
pub enum Pattern {
    Text {
        text: String,
        case_sensitive: bool,
        whole_word: bool,
    },
    Regex(Regex),
    Kind(Kind),
}

impl Pattern {
    /// Plain text, ignoring case.
    pub fn text(s: &str) -> Pattern {
        Pattern::Text {
            text: s.to_string(),
            case_sensitive: false,
            whole_word: false,
        }
    }

    /// A regular expression (`regex-lite` syntax).
    pub fn regex(re: &str) -> Result<Pattern, regex_lite::Error> {
        Ok(Pattern::Regex(Regex::new(re)?))
    }
}

/// One match.
#[derive(Clone, Debug, PartialEq)]
pub struct Hit {
    /// Page index.
    pub page: usize,
    pub text: String,
    /// Boxes of the matched characters in default user space.
    pub quads: Vec<[Point; 4]>,
}

impl Hit {
    /// A black box mark over the match.
    pub fn mark(&self) -> Mark {
        Mark::quads(self.quads.clone())
    }
}

/// Matches of `pattern` on the given pages (all pages when `pages` is `None`).
pub fn find(doc: &Document, pattern: &Pattern, pages: Option<&[usize]>) -> Vec<Hit> {
    let tree = pagebara_pages::PageTree::load(doc);
    let mut interp = Interpreter::new(doc);
    let mut out = Vec::new();
    for (i, page) in tree.pages().iter().enumerate() {
        if pages.is_some_and(|p| !p.contains(&i)) {
            continue;
        }
        let Some(po) = doc.get(page.r) else { continue };
        let Some(pd) = po.as_dict() else { continue };
        let res = crate::apply::inherited(doc, pd, b"Resources")
            .and_then(|o| doc.resolve(&o).as_dict().cloned())
            .unwrap_or_default();
        let text = extract_text(doc, &interp.page(pd, &res));
        for range in matches(&text.text, pattern) {
            let quads = quads(&text, range.clone());
            if !quads.is_empty() {
                out.push(Hit {
                    page: i,
                    text: text.text[range].to_string(),
                    quads,
                });
            }
        }
    }
    out
}

fn quads(text: &TextPage, range: std::ops::Range<usize>) -> Vec<[Point; 4]> {
    text.quads(range)
        .into_iter()
        .map(|q| q.map(|p| Point::new(f64::from(p.x), f64::from(p.y))))
        .collect()
}

/// Byte ranges of matches in `text`.
pub fn matches(text: &str, pattern: &Pattern) -> Vec<std::ops::Range<usize>> {
    match pattern {
        Pattern::Text {
            text: needle,
            case_sensitive,
            whole_word,
        } => {
            if needle.is_empty() {
                return Vec::new();
            }
            let mut re = String::new();
            if !case_sensitive {
                re.push_str("(?i)");
            }
            let quoted = regex_lite::escape(needle);
            if *whole_word {
                re.push_str(&format!(r"\b{quoted}\b"));
            } else {
                re.push_str(&quoted);
            }
            match Regex::new(&re) {
                Ok(r) => r.find_iter(text).map(|m| m.range()).collect(),
                Err(_) => Vec::new(),
            }
        }
        Pattern::Regex(r) => r
            .find_iter(text)
            .filter(|m| !m.is_empty())
            .map(|m| m.range())
            .collect(),
        Pattern::Kind(k) => kind_matches(text, *k),
    }
}

fn kind_matches(text: &str, k: Kind) -> Vec<std::ops::Range<usize>> {
    let (re, valid): (&str, fn(&str) -> bool) = match k {
        Kind::Email => (
            r"[A-Za-z0-9._%+\-]+@[A-Za-z0-9\-]+(\.[A-Za-z0-9\-]+)*\.[A-Za-z]{2,}",
            |_| true,
        ),
        Kind::Phone => (r"\+?\(?\d[\d \-./()]{5,}\d", |s| {
            let digits = s.chars().filter(char::is_ascii_digit).count();
            (7..=15).contains(&digits)
        }),
        Kind::Iban => (r"\b[A-Z]{2}\d{2}(?: ?[A-Z0-9]){11,30}\b", iban_valid),
        Kind::CreditCard => (r"\b\d(?:[ \-]?\d){12,18}\b", luhn_valid),
    };
    let Ok(r) = Regex::new(re) else {
        return Vec::new();
    };
    r.find_iter(text)
        .filter(|m| valid(m.as_str()))
        .map(|m| {
            // Trailing separators are not part of the number.
            let s = m.as_str().trim_end_matches([' ', '-', '.', '/']);
            m.start()..m.start() + s.len()
        })
        .collect()
}

/// The Luhn checksum of the digits in `s`, for 13 to 19 digits.
pub fn luhn_valid(s: &str) -> bool {
    let digits: Vec<u32> = s.chars().filter_map(|c| c.to_digit(10)).collect();
    if !(13..=19).contains(&digits.len()) {
        return false;
    }
    let sum: u32 = digits
        .iter()
        .rev()
        .enumerate()
        .map(|(i, &d)| {
            if i % 2 == 1 {
                let d = d * 2;
                if d > 9 { d - 9 } else { d }
            } else {
                d
            }
        })
        .sum();
    sum % 10 == 0
}

/// The ISO 13616 mod-97 check of an IBAN (spaces allowed).
pub fn iban_valid(s: &str) -> bool {
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    if !(15..=34).contains(&compact.len()) || !compact.is_ascii() {
        return false;
    }
    let (head, tail) = compact.split_at(4);
    let mut rem = 0u32;
    for c in tail.chars().chain(head.chars()) {
        let v = match c {
            '0'..='9' => c as u32 - '0' as u32,
            'A'..='Z' => c as u32 - 'A' as u32 + 10,
            _ => return false,
        };
        rem = if v >= 10 {
            (rem * 100 + v) % 97
        } else {
            (rem * 10 + v) % 97
        };
    }
    rem == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(text: &str, k: Kind) -> Vec<&str> {
        kind_matches(text, k)
            .into_iter()
            .map(|r| &text[r])
            .collect()
    }

    #[test]
    fn personal_data() {
        assert_eq!(
            found("mail jane.doe@example.co.uk or x@y", Kind::Email),
            ["jane.doe@example.co.uk"]
        );
        assert_eq!(
            found("call +44 20 7946 0958 now, room 12", Kind::Phone),
            ["+44 20 7946 0958"]
        );
        assert_eq!(
            found(
                "IBAN GB82 WEST 1234 5698 7654 32 and GB82 WEST 1234 5698 7654 33",
                Kind::Iban
            ),
            ["GB82 WEST 1234 5698 7654 32"]
        );
        assert_eq!(
            found(
                "card 4111 1111 1111 1111; not 4111 1111 1111 1112",
                Kind::CreditCard
            ),
            ["4111 1111 1111 1111"]
        );
        assert!(iban_valid("DE89370400440532013000"));
        assert!(luhn_valid("378282246310005"));
    }

    #[test]
    fn text_patterns() {
        let p = Pattern::Text {
            text: "cat".into(),
            case_sensitive: false,
            whole_word: true,
        };
        assert_eq!(matches("Cat concat cat.", &p), [0..3, 11..14]);
        assert_eq!(matches("a.b", &Pattern::text(".")), vec![1..2]);
    }
}
