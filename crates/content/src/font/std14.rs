//! The standard 14 fonts and the common names that stand for them.

use super::data::{STD14, Std14};

/// The standard font a base font name refers to: exact names, subset tags, `Arial,Bold`
/// style and the usual TrueType equivalents.
pub fn lookup(base_font: &[u8]) -> Option<&'static Std14> {
    let name = strip_subset(base_font);
    let s: String = String::from_utf8_lossy(name)
        .chars()
        .filter(|c| !matches!(c, ' ' | '-' | ',' | '_'))
        .collect::<String>()
        .to_ascii_lowercase();
    let family = if s.starts_with("courier") {
        0
    } else if s.starts_with("helvetica") || s.starts_with("arial") {
        4
    } else if s.starts_with("times") {
        8
    } else if s.starts_with("symbol") {
        return Some(&STD14[12]);
    } else if s.starts_with("zapfdingbats") || s.starts_with("itczapfdingbats") {
        return Some(&STD14[13]);
    } else {
        return None;
    };
    Some(&STD14[family + style(&s)])
}

/// A stand-in for a non-embedded font without widths, chosen from its descriptor flags.
pub fn fallback(base_font: &[u8], flags: u32) -> &'static Std14 {
    let s = String::from_utf8_lossy(strip_subset(base_font)).to_ascii_lowercase();
    let family = if flags & 1 != 0 || s.contains("mono") || s.contains("courier") {
        0
    } else if flags & 2 != 0 || s.contains("serif") && !s.contains("sans") || s.contains("roman") {
        8
    } else {
        4
    };
    let bold = flags & (1 << 18) != 0;
    let italic = flags & (1 << 6) != 0;
    &STD14[family + style(&s).max(usize::from(bold) | usize::from(italic) << 1)]
}

/// 0 regular, 1 bold, 2 italic, 3 bold italic.
fn style(s: &str) -> usize {
    let bold = s.contains("bold") || s.contains("black") || s.contains("heavy");
    let italic = s.contains("italic") || s.contains("oblique");
    match (bold, italic) {
        (false, false) => 0,
        (true, false) => 1,
        (false, true) => 2,
        (true, true) => 3,
    }
}

fn strip_subset(n: &[u8]) -> &[u8] {
    match n.get(6) {
        Some(b'+') if n[..6].iter().all(u8::is_ascii_uppercase) => &n[7..],
        _ => n,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> Option<&'static str> {
        lookup(n.as_bytes()).map(|s| s.name)
    }

    #[test]
    fn aliases() {
        assert_eq!(name("Helvetica"), Some("Helvetica"));
        assert_eq!(
            name("ABCDEF+Arial,BoldItalic"),
            Some("Helvetica-BoldOblique")
        );
        assert_eq!(name("TimesNewRomanPS-ItalicMT"), Some("Times-Italic"));
        assert_eq!(name("Times-Roman"), Some("Times-Roman"));
        assert_eq!(name("CourierNew,Bold"), Some("Courier-Bold"));
        assert_eq!(name("Symbol"), Some("Symbol"));
        assert_eq!(name("Verdana"), None);
        assert_eq!(fallback(b"Garamond", 2).name, "Times-Roman");
        assert_eq!(fallback(b"Foo-Bold", 0).name, "Helvetica-Bold");
    }
}
