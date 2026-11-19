//! The text of a selection as it is copied: lines broken where the page breaks them, cells
//! of a table row apart by tabs, words hyphenated at the end of a line joined when that is
//! safe, and no spaces between Chinese, Japanese or Korean characters.

use std::ops::Range;

use crate::display::NONE;
use crate::extract::{TextChar, TextPage};

/// A space wider than this share of its line's height separates table cells.
const TAB_GAP: f32 = 1.2;

/// The text of characters `chars` of `page`, for the clipboard.
pub fn copy_text(page: &TextPage, chars: Range<usize>) -> String {
    let end = chars.end.min(page.chars.len());
    if chars.start >= end {
        return String::new();
    }
    let mut out = String::new();
    let mut prev_end: Option<usize> = None;
    let mut line = 0;
    for i in chars.start..end {
        let c = &page.chars[i];
        let (a, b) = (c.text.0 as usize, c.text.1 as usize);
        if let Some(p) = prev_end
            && a > p
        {
            // Line and paragraph breaks between characters.
            out.push_str(&page.text[p..a]);
        }
        while page
            .lines
            .get(line)
            .is_some_and(|l| (l.chars.1 as usize) <= i)
        {
            line += 1;
        }
        let t = &page.text[a..b];
        if t == " " && c.glyph == NONE && wide_gap(page, c, line) {
            out.push('\t');
        } else {
            out.push_str(t);
        }
        prev_end = Some(b.max(prev_end.unwrap_or(0)));
    }
    let out = join_hyphens(&out);
    let out = drop_cjk_spaces(&out);
    out.trim_end().to_owned()
}

/// A gap between words much wider than a word space: a cell boundary.
fn wide_gap(page: &TextPage, c: &TextChar, line: usize) -> bool {
    let q = c.quad;
    let width = (q[1].x - q[0].x).hypot(q[1].y - q[0].y);
    let Some(l) = page.lines.get(line) else {
        return false;
    };
    // The height of the line's glyphs across its direction.
    let mut heights: Vec<f32> = page.chars[l.chars.0 as usize..l.chars.1 as usize]
        .iter()
        .filter(|c| !page.char_text(c).trim().is_empty())
        .map(|c| (c.quad[3].x - c.quad[0].x).hypot(c.quad[3].y - c.quad[0].y))
        .collect();
    if heights.is_empty() {
        return false;
    }
    heights.sort_by(f32::total_cmp);
    width > TAB_GAP * heights[heights.len() / 2]
}

/// `exam-\nple` as `example\n`: a word broken by a hyphen at a line end within a paragraph
/// is joined when both parts are lower-case letters (a soft hyphen always joins), keeping
/// the hyphen when the text has the word with one elsewhere; the break moves to after the
/// word.
fn join_hyphens(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let breaks = matches!(c, '-' | '\u{AD}' | '\u{2010}')
            && chars.get(i + 1) == Some(&'\n')
            && chars.get(i + 2) != Some(&'\n');
        if breaks && safe_join(&chars, i) {
            let mut j = i + 2;
            while j < chars.len() && !chars[j].is_whitespace() {
                j += 1;
            }
            let rest: String = chars[i + 2..j].iter().collect();
            let mut k = i;
            while k > 0 && !chars[k - 1].is_whitespace() {
                k -= 1;
            }
            let head: String = chars[k..i].iter().collect();
            if c == '-' && s.contains(&format!("{head}-{rest}")) {
                out.push('-');
            }
            // The rest of the word, then the line break in place of the space after it.
            out.push_str(&rest);
            if chars.get(j) == Some(&' ') {
                out.push('\n');
                j += 1;
            }
            i = j;
            continue;
        }
        if c == '\u{AD}' && chars.get(i + 1) != Some(&'\n') {
            // A soft hyphen inside a line is invisible.
            i += 1;
            continue;
        }
        out.push(c);
        i += 1;
    }
    out
}

fn safe_join(chars: &[char], hyphen: usize) -> bool {
    let next = chars.get(hyphen + 2).copied();
    if chars[hyphen] == '\u{AD}' {
        return next.is_some_and(char::is_alphabetic);
    }
    // At least two lower-case letters before the hyphen, back to the start of the word.
    let mut k = hyphen;
    let mut letters = 0;
    while k > 0 && !chars[k - 1].is_whitespace() {
        k -= 1;
        if !chars[k].is_lowercase() {
            return false;
        }
        letters += 1;
    }
    letters >= 2 && next.is_some_and(char::is_lowercase)
}

/// Spaces between two CJK characters are layout, not text.
fn drop_cjk_spaces(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len());
    for (i, &c) in chars.iter().enumerate() {
        if c == ' ' && i > 0 && chars.get(i + 1).is_some_and(|n| is_cjk(*n)) && is_cjk(chars[i - 1])
        {
            continue;
        }
        out.push(c);
    }
    out
}

/// Han, kana, Hangul and their punctuation and full-width forms.
pub fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x1100..=0x11FF
        | 0x3000..=0x30FF
        | 0x3130..=0x318F
        | 0x31F0..=0x31FF
        | 0x3400..=0x4DBF
        | 0x4E00..=0x9FFF
        | 0xAC00..=0xD7AF
        | 0xF900..=0xFAFF
        | 0xFF00..=0xFFEF
        | 0x20000..=0x2FFFF)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hyphens_join_only_when_safe() {
        assert_eq!(join_hyphens("an exam-\nple of text"), "an example\nof text");
        assert_eq!(join_hyphens("well-\nknown"), "wellknown");
        assert_eq!(
            join_hyphens("a well-\nknown and well-known fact"),
            "a well-known\nand well-known fact"
        );
        // Capitals, numbers, one letter, a paragraph break: kept.
        assert_eq!(join_hyphens("Anglo-\nSaxon"), "Anglo-\nSaxon");
        assert_eq!(join_hyphens("pages 10-\n12"), "pages 10-\n12");
        assert_eq!(join_hyphens("e-\nmail"), "e-\nmail");
        assert_eq!(join_hyphens("end-\n\nnext"), "end-\n\nnext");
        assert_eq!(join_hyphens("soft\u{AD}\nHyphen x"), "softHyphen\nx");
        assert_eq!(join_hyphens("in\u{AD}visible"), "invisible");
    }

    #[test]
    fn cjk_has_no_spaces() {
        assert_eq!(drop_cjk_spaces("日本 語 です a b"), "日本語です a b");
        assert_eq!(drop_cjk_spaces("漢字 abc 漢字"), "漢字 abc 漢字");
    }
}
