//! A readable name for the input language, as a hint in the password dialog.
//!
//! Approximated from the locale environment; reading the active keyboard layout is
//! platform-specific and not done yet.

use pagebara_i18n::{tr, tr_noop};

const NAMES: [(&str, &str); 24] = [
    ("ar", tr_noop!("Arabic")),
    ("cs", tr_noop!("Czech")),
    ("da", tr_noop!("Danish")),
    ("de", tr_noop!("German")),
    ("el", tr_noop!("Greek")),
    ("en", tr_noop!("English")),
    ("es", tr_noop!("Spanish")),
    ("fi", tr_noop!("Finnish")),
    ("fr", tr_noop!("French")),
    ("he", tr_noop!("Hebrew")),
    ("hi", tr_noop!("Hindi")),
    ("hu", tr_noop!("Hungarian")),
    ("it", tr_noop!("Italian")),
    ("ja", tr_noop!("Japanese")),
    ("ko", tr_noop!("Korean")),
    ("nb", tr_noop!("Norwegian")),
    ("nl", tr_noop!("Dutch")),
    ("pl", tr_noop!("Polish")),
    ("pt", tr_noop!("Portuguese")),
    ("ru", tr_noop!("Russian")),
    ("sv", tr_noop!("Swedish")),
    ("tr", tr_noop!("Turkish")),
    ("uk", tr_noop!("Ukrainian")),
    ("zh", tr_noop!("Chinese")),
];

pub fn input_language() -> String {
    let locale = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX");
    let Some(locale) = locale else {
        // TRANSLATORS: the input language, when it is not known
        return tr!("unknown");
    };
    describe(&locale)
}

/// "en_US.UTF-8" → "English (US)".
fn describe(locale: &str) -> String {
    let tag = locale.split(['.', '@']).next().unwrap_or(locale);
    let mut parts = tag.split(['_', '-']);
    let lang = parts.next().unwrap_or_default();
    let region = parts.next();
    let name = NAMES
        .iter()
        .find(|(code, _)| *code == lang)
        .map_or(lang.to_owned(), |(_, n)| pagebara_i18n::translate(n, &[]));
    match region {
        Some(r) => format!("{name} ({r})"),
        None => name,
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn describes_locales() {
        assert_eq!(super::describe("en_US.UTF-8"), "English (US)");
        assert_eq!(super::describe("de_DE@euro"), "German (DE)");
        assert_eq!(super::describe("xx"), "xx");
    }
}
