//! Just enough shaping for generated text without a shaping engine: Arabic letters take
//! their contextual presentation forms (Unicode Arabic Presentation Forms-A/B, which most
//! Arabic fonts map), and lines are drawn in visual order by the Unicode bidirectional
//! algorithm. Ligatures (lam-alef) and mark positioning are not done.

use unicode_bidi::BidiInfo;

use crate::font::Glyph;

/// How a letter joins its neighbours (Unicode `Joining_Type`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Joining {
    /// Joins on both sides; forms isolated, final, initial, medial.
    Dual,
    /// Joins only the preceding letter; forms isolated, final.
    Right,
    /// Tatweel: joins both sides, has no forms.
    Causing,
    /// Marks, skipped when looking for neighbours.
    Transparent,
    None,
}

/// Letters with presentation forms: (letter, isolated form, dual-joining).
const LETTERS: &[(u16, u16, bool)] = &[
    (0x0621, 0xFE80, false),
    (0x0622, 0xFE81, false),
    (0x0623, 0xFE83, false),
    (0x0624, 0xFE85, false),
    (0x0625, 0xFE87, false),
    (0x0626, 0xFE89, true),
    (0x0627, 0xFE8D, false),
    (0x0628, 0xFE8F, true),
    (0x0629, 0xFE93, false),
    (0x062A, 0xFE95, true),
    (0x062B, 0xFE99, true),
    (0x062C, 0xFE9D, true),
    (0x062D, 0xFEA1, true),
    (0x062E, 0xFEA5, true),
    (0x062F, 0xFEA9, false),
    (0x0630, 0xFEAB, false),
    (0x0631, 0xFEAD, false),
    (0x0632, 0xFEAF, false),
    (0x0633, 0xFEB1, true),
    (0x0634, 0xFEB5, true),
    (0x0635, 0xFEB9, true),
    (0x0636, 0xFEBD, true),
    (0x0637, 0xFEC1, true),
    (0x0638, 0xFEC5, true),
    (0x0639, 0xFEC9, true),
    (0x063A, 0xFECD, true),
    (0x0641, 0xFED1, true),
    (0x0642, 0xFED5, true),
    (0x0643, 0xFED9, true),
    (0x0644, 0xFEDD, true),
    (0x0645, 0xFEE1, true),
    (0x0646, 0xFEE5, true),
    (0x0647, 0xFEE9, true),
    (0x0648, 0xFEED, false),
    (0x0649, 0xFEEF, false),
    (0x064A, 0xFEF1, true),
    (0x067E, 0xFB56, true),
    (0x0686, 0xFB7A, true),
    (0x0698, 0xFB8A, false),
    (0x06A9, 0xFB8E, true),
    (0x06AF, 0xFB92, true),
    (0x06CC, 0xFBFC, true),
];

fn letter(c: char) -> Option<(u16, bool)> {
    let u = u16::try_from(u32::from(c)).ok()?;
    let i = LETTERS.binary_search_by_key(&u, |l| l.0).ok()?;
    Some((LETTERS[i].1, LETTERS[i].2))
}

fn joining(c: char) -> Joining {
    match c {
        '\u{0621}' => Joining::None,
        '\u{0640}' => Joining::Causing,
        '\u{064B}'..='\u{065F}' | '\u{0670}' | '\u{06D6}'..='\u{06ED}' => Joining::Transparent,
        _ => match letter(c) {
            Some((_, true)) => Joining::Dual,
            Some((_, false)) => Joining::Right,
            None => Joining::None,
        },
    }
}

/// The presentation form of each character in logical order (`None`: no contextual form).
pub(crate) fn arabic_forms(chars: &[char]) -> Vec<Option<char>> {
    let mut out = vec![None; chars.len()];
    if !chars.iter().any(|c| ('\u{0600}'..='\u{06FF}').contains(c)) {
        return out;
    }
    let types: Vec<Joining> = chars.iter().map(|&c| joining(c)).collect();
    let neighbour = |range: &mut dyn Iterator<Item = usize>| {
        range.map(|j| types[j]).find(|t| *t != Joining::Transparent)
    };
    for (i, &c) in chars.iter().enumerate() {
        let Some((iso, _)) = letter(c) else {
            continue;
        };
        let prev = neighbour(&mut (0..i).rev());
        let next = neighbour(&mut (i + 1..chars.len()));
        let joins_prev =
            types[i] != Joining::None && matches!(prev, Some(Joining::Dual | Joining::Causing));
        let joins_next = types[i] == Joining::Dual
            && matches!(
                next,
                Some(Joining::Dual | Joining::Right | Joining::Causing)
            );
        // Forms follow the isolated one: final, initial, medial.
        let offset = match (joins_prev, joins_next) {
            (false, false) => 0,
            (true, false) => 1,
            (false, true) => 2,
            (true, true) => 3,
        };
        out[i] = char::from_u32(u32::from(iso) + offset);
    }
    out
}

/// The glyphs of a line in visual (left-to-right drawing) order.
pub(crate) fn visual_order(line: &[(char, Glyph)]) -> Vec<(char, Glyph)> {
    let text: String = line.iter().map(|g| g.0).collect();
    let info = BidiInfo::new(&text, None);
    if !info.has_rtl() {
        return line.to_vec();
    }
    // Byte offset of each character → its index in `line`.
    let mut index = vec![0; text.len()];
    for (i, (b, _)) in text.char_indices().enumerate() {
        index[b] = i;
    }
    let mut out = Vec::with_capacity(line.len());
    for para in &info.paragraphs {
        let (levels, runs) = info.visual_runs(para, para.range.clone());
        for run in runs {
            let mut chars: Vec<usize> = text[run.clone()]
                .char_indices()
                .map(|(b, _)| index[run.start + b])
                .collect();
            if levels[run.start].is_rtl() {
                chars.reverse();
            }
            out.extend(chars.into_iter().map(|i| line[i]));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn forms(s: &str) -> String {
        let chars: Vec<char> = s.chars().collect();
        arabic_forms(&chars)
            .iter()
            .zip(&chars)
            .map(|(f, c)| f.unwrap_or(*c))
            .collect()
    }

    #[test]
    fn arabic_joining() {
        // مرحبا: meem initial, reh final, hah initial, beh medial, alef final.
        assert_eq!(forms("مرحبا"), "\u{FEE3}\u{FEAE}\u{FEA3}\u{FE92}\u{FE8E}");
        // A lone letter and letters around a space stay isolated or end words.
        assert_eq!(forms("ب ب"), "\u{FE8F} \u{FE8F}");
        // Marks are transparent: بَب joins across the fatha.
        assert_eq!(forms("بَب"), "\u{FE91}\u{064E}\u{FE90}");
        // Hamza never joins.
        assert_eq!(forms("بءب"), "\u{FE8F}\u{FE80}\u{FE8F}");
        assert_eq!(forms("abc"), "abc");
    }

    #[test]
    fn visual_runs() {
        let g = |c: char| (c, Glyph::default());
        let line: Vec<_> = "ab אבג cd".chars().map(g).collect();
        let v: String = visual_order(&line).iter().map(|g| g.0).collect();
        assert_eq!(v, "ab גבא cd");
    }
}
