//! Letter case: the shape of a word's capitalisation and case mapping (with the Turkish
//! dotted and dotless i).

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Cap {
    /// No capitals.
    Lower,
    /// Only the first letter is a capital.
    Init,
    /// Every letter is a capital.
    All,
    /// Mixed, starting with a small letter ("iPhone").
    Mixed,
    /// Mixed, starting with a capital ("McDonald").
    MixedInit,
}

pub(crate) fn cap(word: &str) -> Cap {
    let (mut upper, mut lower) = (0, 0);
    let mut first_upper = None;
    for c in word.chars() {
        if c.is_uppercase() {
            upper += 1;
        } else if c.is_lowercase() {
            lower += 1;
        }
        if first_upper.is_none() && (c.is_uppercase() || c.is_lowercase()) {
            first_upper = Some(c.is_uppercase());
        }
    }
    let first_upper = first_upper == Some(true);
    match (upper, lower) {
        (0, _) => Cap::Lower,
        (_, 0) if upper > 1 => Cap::All,
        (1, _) if first_upper => Cap::Init,
        (_, 0) => Cap::Init,
        _ if first_upper => Cap::MixedInit,
        _ => Cap::Mixed,
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Casing {
    pub turkic: bool,
}

impl Casing {
    pub(crate) fn lower(self, s: &str) -> String {
        if self.turkic {
            s.chars()
                .map(|c| match c {
                    'I' => 'ı',
                    'İ' => 'i',
                    c => c,
                })
                .collect::<String>()
                .to_lowercase()
        } else {
            s.to_lowercase()
        }
    }

    pub(crate) fn upper(self, s: &str) -> String {
        if self.turkic {
            s.chars()
                .map(|c| match c {
                    'i' => 'İ',
                    'ı' => 'I',
                    c => c,
                })
                .collect::<String>()
                .to_uppercase()
        } else {
            s.to_uppercase()
        }
    }

    /// The first letter as a capital, the rest unchanged.
    pub(crate) fn capitalize(self, s: &str) -> String {
        let mut cs = s.chars();
        match cs.next() {
            Some(c) => {
                let mut out = self.upper(c.encode_utf8(&mut [0; 4]));
                out.push_str(cs.as_str());
                out
            }
            None => String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shapes_and_mapping() {
        assert_eq!(cap("word"), Cap::Lower);
        assert_eq!(cap("Word"), Cap::Init);
        assert_eq!(cap("WORD"), Cap::All);
        assert_eq!(cap("iPhone"), Cap::Mixed);
        assert_eq!(cap("McDonald"), Cap::MixedInit);
        assert_eq!(cap("A"), Cap::Init);
        assert_eq!(cap("don't"), Cap::Lower);
        let tr = Casing { turkic: true };
        assert_eq!(tr.lower("IİSTANBUL"), "ıistanbul");
        assert_eq!(tr.capitalize("istanbul"), "İstanbul");
        assert_eq!(Casing::default().capitalize("paris"), "Paris");
        assert_eq!(Casing::default().upper("straße"), "STRASSE");
    }
}
