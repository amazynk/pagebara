//! Line break opportunities: the essentials of UAX #14 (mandatory breaks, spaces, hyphens
//! and dashes, no breaks before closing punctuation or after opening punctuation, quotes,
//! numbers with their prefixes and postfixes, non-breaking characters) and breaks between
//! ideographs, with Japanese kinsoku (small kana, prolonged sound mark and iteration marks
//! never start a line). Scripts that need a dictionary (Thai, Lao, Khmer, Myanmar) break
//! only at spaces.

/// Line breaking classes (a subset of UAX #14's).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    /// Mandatory break after (LF, VT, FF, NEL, LS, PS).
    Bk,
    Cr,
    Sp,
    /// Non-breaking (NBSP, NNBSP, figure space, word joiner, BOM).
    Gl,
    /// Zero width space.
    Zw,
    Zwj,
    /// Combining mark.
    Cm,
    /// Break after (dashes, soft hyphen, typographic spaces).
    Ba,
    /// Hyphen-minus.
    Hy,
    /// Em dash: break before and after.
    B2,
    Cl,
    Cp,
    Ex,
    Is,
    /// Nonstarter.
    Ns,
    Op,
    Qu,
    /// Ideographic (CJK, kana, Hangul, emoji).
    Id,
    Al,
    Nu,
    Pr,
    Po,
}

/// The class of a character.
pub fn class(c: char) -> Class {
    use Class::*;
    let u = c as u32;
    match c {
        '\n' | '\u{0B}' | '\u{0C}' | '\u{85}' | '\u{2028}' | '\u{2029}' => return Bk,
        '\r' => return Cr,
        ' ' | '\t' => return Sp,
        '\u{A0}' | '\u{202F}' | '\u{2007}' | '\u{2060}' | '\u{FEFF}' | '\u{034F}' => return Gl,
        '\u{200B}' => return Zw,
        '\u{200D}' => return Zwj,
        '-' => return Hy,
        '\u{2014}' => return B2,
        '\u{AD}'
        | '\u{2010}'
        | '\u{2012}'
        | '\u{2013}'
        | '|'
        | '\u{1680}'
        | '\u{2000}'..='\u{2006}'
        | '\u{2008}'..='\u{200A}'
        | '\u{205F}'
        | '\u{3000}' => return Ba,
        ')' | ']' | '\u{FF09}' | '\u{FF3D}' => return Cp,
        '}' | '\u{3001}' | '\u{3002}' | '\u{FF0C}' | '\u{FF0E}' | '\u{FE50}' | '\u{FE52}'
        | '\u{FF61}' | '\u{FF64}' | '\u{3009}' | '\u{300B}' | '\u{300D}' | '\u{300F}'
        | '\u{3011}' | '\u{3015}' | '\u{3017}' | '\u{3019}' | '\u{301B}' | '\u{301E}'
        | '\u{301F}' | '\u{FF5D}' | '\u{FF60}' | '\u{FF63}' | '\u{FE11}' | '\u{FE12}' => {
            return Cl;
        }
        '!' | '?' | '\u{FF01}' | '\u{FF1F}' | '\u{203C}' | '\u{2047}'..='\u{2049}' => return Ex,
        ',' | '.' | ':' | ';' | '\u{037E}' | '\u{0589}' | '\u{060C}' | '\u{060D}' | '\u{2044}'
        | '\u{FE10}' | '\u{FE13}' | '\u{FE14}' => return Is,
        '(' | '[' | '{' | '\u{FF08}' | '\u{FF3B}' | '\u{FF5B}' | '\u{3008}' | '\u{300A}'
        | '\u{300C}' | '\u{300E}' | '\u{3010}' | '\u{3014}' | '\u{3016}' | '\u{3018}'
        | '\u{301A}' | '\u{301D}' | '\u{FF5F}' | '\u{FF62}' | '\u{A1}' | '\u{BF}' => return Op,
        '"' | '\'' | '\u{AB}' | '\u{BB}' | '\u{2018}' | '\u{2019}' | '\u{201C}' | '\u{201D}'
        | '\u{2039}' | '\u{203A}' => return Qu,
        '$'
        | '+'
        | '\\'
        | '\u{A3}'
        | '\u{A5}'
        | '\u{B1}'
        | '\u{2116}'
        | '\u{20A0}'..='\u{20CF}' => {
            return Pr;
        }
        '%'
        | '\u{A2}'
        | '\u{B0}'
        | '\u{2030}'..='\u{2037}'
        | '\u{2103}'
        | '\u{2109}'
        | '\u{FF05}' => return Po,
        // Small kana, prolonged sound mark, iteration marks, middle dots, sound marks.
        '\u{3041}'
        | '\u{3043}'
        | '\u{3045}'
        | '\u{3047}'
        | '\u{3049}'
        | '\u{3063}'
        | '\u{3083}'
        | '\u{3085}'
        | '\u{3087}'
        | '\u{308E}'
        | '\u{3095}'
        | '\u{3096}'
        | '\u{30A1}'
        | '\u{30A3}'
        | '\u{30A5}'
        | '\u{30A7}'
        | '\u{30A9}'
        | '\u{30C3}'
        | '\u{30E3}'
        | '\u{30E5}'
        | '\u{30E7}'
        | '\u{30EE}'
        | '\u{30F5}'
        | '\u{30F6}'
        | '\u{31F0}'..='\u{31FF}'
        | '\u{30FC}'
        | '\u{3005}'
        | '\u{303B}'
        | '\u{309B}'..='\u{309E}'
        | '\u{30A0}'
        | '\u{30FB}'
        | '\u{30FD}'
        | '\u{30FE}'
        | '\u{FF1A}'
        | '\u{FF1B}'
        | '\u{FF65}'
        | '\u{FF67}'..='\u{FF70}'
        | '\u{FF9E}'
        | '\u{FF9F}' => return Ns,
        _ => {}
    }
    if c.is_ascii_digit()
        || matches!(u, 0x0660..=0x0669 | 0x06F0..=0x06F9 | 0x0966..=0x096F | 0xFF10..=0xFF19)
    {
        return Nu;
    }
    if is_mark(u) {
        return Cm;
    }
    if matches!(u,
        0x1100..=0x115F | 0x2E80..=0x2FFF | 0x3003..=0x3004 | 0x3006..=0x3007 | 0x3012..=0x3013
        | 0x3020..=0x303A | 0x303C..=0x303F | 0x3040..=0x30FF | 0x3100..=0x31EF
        | 0x3200..=0x4DBF | 0x4E00..=0x9FFF | 0xA000..=0xA4CF | 0xAC00..=0xD7A3
        | 0xF900..=0xFAFF | 0xFE30..=0xFE4F | 0xFF00..=0xFFEF | 0x1F000..=0x1FAFF
        | 0x20000..=0x3FFFD)
    {
        return Id;
    }
    Al
}

/// Combining marks and variation selectors (approximate ranges).
fn is_mark(u: u32) -> bool {
    matches!(u,
        0x0300..=0x036F | 0x0483..=0x0489 | 0x0591..=0x05BD | 0x05BF | 0x05C1..=0x05C2
        | 0x05C4..=0x05C5 | 0x05C7 | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670
        | 0x06D6..=0x06DC | 0x06DF..=0x06E4 | 0x06E7..=0x06E8 | 0x06EA..=0x06ED
        | 0x0900..=0x0903 | 0x093A..=0x094F | 0x0951..=0x0957 | 0x0962..=0x0963
        | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF
        | 0x20D0..=0x20FF | 0x3099..=0x309A | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F
        | 0xE0100..=0xE01EF)
}

/// A break opportunity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Break {
    /// The line must end here.
    Mandatory,
    /// The line may end here.
    Allowed,
}

/// Break opportunities in `text`: byte offsets where a new line may (or must) start, in
/// order. A break after spaces is placed after them. The end of the text is not included.
pub fn breaks(text: &str) -> Vec<(usize, Break)> {
    use Class::*;
    let mut out = Vec::new();
    // Class of the last non-space character, and whether spaces followed it.
    let mut prev: Option<Class> = None;
    let mut last: Option<Class> = None;
    let mut spaces = false;
    for (i, c) in text.char_indices() {
        let cls = class(c);
        // Mandatory breaks after BK and CR (but not between CR and LF).
        let mandatory = last == Some(Bk) || (last == Some(Cr) && c != '\n');
        if mandatory {
            out.push((i, Break::Mandatory));
            prev = None;
            spaces = false;
        }
        last = Some(cls);
        match cls {
            Bk | Cr => {
                // Never break before a line end; it ends the line itself.
                prev = Some(cls);
                spaces = false;
                continue;
            }
            Sp => {
                spaces = true;
                continue;
            }
            Zw => {
                prev = Some(Zw);
                spaces = false;
                continue;
            }
            Cm | Zwj => {
                if prev.is_none() {
                    prev = Some(Al);
                }
                continue;
            }
            _ => {}
        }
        if !mandatory
            && let Some(a) = prev
            && pair(a, cls, spaces)
        {
            out.push((i, Break::Allowed));
        }
        prev = Some(cls);
        spaces = false;
    }
    out
}

/// Whether a line may break before `b` when `a` precedes it (with spaces in between when
/// `sp`).
fn pair(a: Class, b: Class, sp: bool) -> bool {
    use Class::*;
    if a == Zw {
        return true;
    }
    if matches!(a, Bk | Cr) {
        return false;
    }
    if matches!(b, Cl | Cp | Ex | Is) || a == Op {
        return false;
    }
    if matches!(a, Cl | Cp) && b == Ns {
        return false;
    }
    if a == B2 && b == B2 {
        return false;
    }
    if sp {
        return true;
    }
    if a == Gl || b == Gl {
        return false;
    }
    if matches!(b, Ba | Hy | Ns) {
        return false;
    }
    if a == Qu || b == Qu {
        return false;
    }
    if a == B2 || b == B2 {
        return true;
    }
    !matches!(
        (a, b),
        (Al | Nu, Al | Nu | Op | Pr)
            | (Hy, Nu)
            | (Is, Al | Nu)
            | (Cp, Al | Nu)
            | (Pr, Nu | Al | Op | Id)
            | (Nu | Cp | Cl | Al | Id, Po)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(text: &str) -> Vec<&str> {
        let mut out = Vec::new();
        let mut start = 0;
        for (b, _) in breaks(text) {
            out.push(&text[start..b]);
            start = b;
        }
        out.push(&text[start..]);
        out
    }

    #[test]
    fn spaces_and_hyphens() {
        assert_eq!(lines("one two  three"), ["one ", "two  ", "three"]);
        assert_eq!(
            lines("e-mail well-known"),
            ["e-", "mail ", "well-", "known"]
        );
        assert_eq!(lines("a -5 b"), ["a ", "-5 ", "b"]);
        assert_eq!(lines("x\u{A0}y z"), ["x\u{A0}y ", "z"]);
        assert_eq!(lines("word\u{2014}dash"), ["word", "\u{2014}", "dash"]);
    }

    #[test]
    fn punctuation() {
        assert_eq!(lines("(see) this, then !"), ["(see) ", "this, ", "then !"]);
        assert_eq!(
            lines("f(x) = \"q\" 10% $5 1,000.5"),
            ["f(x) ", "= ", "\"q\" ", "10% ", "$5 ", "1,000.5"]
        );
    }

    #[test]
    fn mandatory() {
        let b = breaks("a\nb\r\nc\n\nd");
        assert_eq!(
            b,
            [
                (2, Break::Mandatory),
                (5, Break::Mandatory),
                (7, Break::Mandatory),
                (8, Break::Mandatory)
            ]
        );
    }

    #[test]
    fn cjk() {
        // Between ideographs, but never before closing punctuation or small kana.
        assert_eq!(lines("漢字。かなっ"), ["漢", "字。", "か", "なっ"]);
        assert_eq!(lines("「日本」語"), ["「日", "本」", "語"]);
        assert_eq!(lines("ABC漢字"), ["ABC", "漢", "字"]);
        assert_eq!(lines("コーヒー"), ["コー", "ヒー"]);
    }
}
