//! Splitting text into words to check, with their byte ranges. Words may hold apostrophes
//! and hyphens between letters; words with digits, web addresses, e-mail addresses, file
//! names and paths are skipped, as is text in scripts written without spaces (Chinese,
//! Japanese, Thai, ...) and Korean, which Hunspell does not check.

use std::ops::Range;

/// A word in a text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Token<'a> {
    /// Byte range in the text.
    pub range: Range<usize>,
    pub word: &'a str,
}

/// The words of a text; see [`tokens`].
#[derive(Clone, Debug)]
pub struct Tokens<'a> {
    text: &'a str,
    pos: usize,
    /// End of the whitespace-delimited run already looked at for addresses.
    run_end: usize,
    joiners: &'a str,
}

/// The words of `text` to spell check.
pub fn tokens(text: &str) -> Tokens<'_> {
    Tokens::with_joiners(text, "")
}

fn unspaced(c: char) -> bool {
    matches!(c as u32,
        0x0E00..=0x0EFF | 0x1000..=0x109F | 0x1100..=0x11FF | 0x1780..=0x17FF
        | 0x2E80..=0xA4CF | 0xA960..=0xA97F | 0xAC00..=0xD7FF | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF | 0x20000..=0x3FFFF)
}

fn is_mark(c: char) -> bool {
    matches!(c as u32,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2
        | 0x05C4..=0x05C5 | 0x05C7 | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670
        | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 | 0x06EA..=0x06ED
        | 0x0900..=0x0903 | 0x093A..=0x094F | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF
        | 0x200C..=0x200D | 0x20D0..=0x20FF | 0xFE20..=0xFE2F)
}

fn is_letter(c: char) -> bool {
    (c.is_alphabetic() || is_mark(c)) && !unspaced(c)
}

fn is_hebrew(c: Option<char>) -> bool {
    c.is_some_and(|c| ('\u{05D0}'..='\u{05EA}').contains(&c))
}

const FILE_TYPES: &[&str] = &[
    "pdf", "doc", "docx", "odt", "rtf", "xls", "xlsx", "ods", "csv", "ppt", "pptx", "txt", "md",
    "png", "jpg", "jpeg", "gif", "svg", "tif", "tiff", "html", "htm", "xml", "json", "zip", "exe",
    "js", "ts", "rs", "py", "com", "org", "net", "edu", "gov", "info", "biz", "io", "dev", "app",
];

/// Whether a whitespace-delimited run is an address, a file name or a path.
fn is_address(run: &str) -> bool {
    let run = run.trim_start_matches(['(', '[', '<', '"', '\'', '«', '“', '‘']);
    let run = run.trim_end_matches([
        '.', ',', ';', ':', '!', '?', ')', ']', '>', '"', '\'', '»', '”', '’',
    ]);
    let lower = run.to_ascii_lowercase();
    if lower.contains("://") || lower.starts_with("www.") || lower.starts_with("mailto:") {
        return true;
    }
    if run.starts_with(['/', '~', '@', '#', '\\']) || run.contains('\\') {
        return run.chars().count() > 1;
    }
    if let Some((user, host)) = run.split_once('@') {
        return !user.is_empty() && host.contains('.');
    }
    // A host or file name: labels joined by dots, ending in a known suffix or a country code.
    let name = run.split('/').next().unwrap_or(run);
    let labels: Vec<&str> = name.split('.').collect();
    if labels.len() < 2 || labels.iter().any(|l| l.is_empty()) {
        return false;
    }
    let last = labels[labels.len() - 1];
    let first = labels[0];
    first
        .chars()
        .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
        && (FILE_TYPES.contains(&last)
            || (last.len() == 2 && last.bytes().all(|b| b.is_ascii_lowercase()) && first.len() > 1))
}

impl<'a> Tokens<'a> {
    /// Like [`tokens`], with `joiners` also allowed between letters (a dictionary's
    /// `WORDCHARS`).
    pub fn with_joiners(text: &'a str, joiners: &'a str) -> Self {
        Tokens {
            text,
            pos: 0,
            run_end: 0,
            joiners,
        }
    }

    fn joins(&self, c: char, prev: Option<char>, next: Option<char>) -> bool {
        match c {
            '\'' | '’' | 'ʼ' | '-' | '\u{2010}' | '\u{2011}' | '\u{05F3}' | '\u{05F4}' => true,
            '"' => is_hebrew(prev) && is_hebrew(next),
            c => self.joiners.contains(c),
        }
    }
}

impl<'a> Iterator for Tokens<'a> {
    type Item = Token<'a>;

    fn next(&mut self) -> Option<Token<'a>> {
        let t = self.text;
        loop {
            let (off, _) = t[self.pos..]
                .char_indices()
                .find(|&(_, c)| is_letter(c) || c.is_numeric())?;
            let start = self.pos + off;
            if start >= self.run_end {
                let run_start = t[..start]
                    .char_indices()
                    .rev()
                    .find(|(_, c)| c.is_whitespace())
                    .map_or(0, |(i, c)| i + c.len_utf8());
                let run_end = t[start..]
                    .find(char::is_whitespace)
                    .map_or(t.len(), |i| start + i);
                self.run_end = run_end;
                if is_address(&t[run_start..run_end]) {
                    self.pos = run_end;
                    continue;
                }
            }
            let mut end = start;
            let mut digits = false;
            let mut prev = None;
            let mut it = t[start..].char_indices().peekable();
            while let Some((i, c)) = it.next() {
                let next = it.peek().map(|&(_, n)| n);
                if is_letter(c) {
                } else if c.is_numeric() {
                    digits = true;
                } else if !(prev.is_some()
                    && next.is_some_and(|n| is_letter(n) || n.is_numeric())
                    && self.joins(c, prev, next))
                {
                    break;
                }
                end = start + i + c.len_utf8();
                prev = Some(c);
            }
            self.pos = end;
            if !digits {
                return Some(Token {
                    range: start..end,
                    word: &t[start..end],
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<&str> {
        tokens(text).map(|t| t.word).collect()
    }

    #[test]
    fn words_and_ranges() {
        let t: Vec<_> = tokens("Hi, don't  stop—well-known.").collect();
        assert_eq!(
            t[0],
            Token {
                range: 0..2,
                word: "Hi"
            }
        );
        assert_eq!(t[1].word, "don't");
        assert_eq!(t[2].word, "stop");
        assert_eq!(t[3].word, "well-known");
        assert_eq!(
            &"Hi, don't  stop—well-known."[t[3].range.clone()],
            "well-known"
        );
        assert_eq!(
            words("'quoted' l’homme -dash- end'"),
            ["quoted", "l’homme", "dash", "end"]
        );
        assert_eq!(
            words("Grüße, Ελλάδα! Привет"),
            ["Grüße", "Ελλάδα", "Привет"]
        );
    }

    #[test]
    fn skipped() {
        assert_eq!(words("MP3 3rd A4 1,5 x2 ok"), ["ok"]);
        assert_eq!(
            words("see https://example.com/a?b=c or www.rust-lang.org now"),
            ["see", "or", "now"]
        );
        assert_eq!(
            words("mail me@example.org (report.pdf) or /usr/share and C:\\x"),
            ["mail", "or", "and"]
        );
        assert_eq!(words("e.g. this.That"), ["e", "g", "this", "That"]);
        assert_eq!(words("日本語のテキスト and 한국어 text"), ["and", "text"]);
        assert_eq!(words("שלום צה\"ל"), ["שלום", "צה\"ל"]);
    }
}
