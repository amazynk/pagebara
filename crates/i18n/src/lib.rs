//! The UI language for Rust code: the same gettext catalogs (`po/*.po`) that the Slint UI
//! bundles, so both translate one string the same way.
//!
//! ```standalone_crate,no_run
//! use pagebara_i18n::{tr, trn};
//! let s = tr!("Saved as “{}”.", "Draft");
//! let p = trn!("{n} page", "{n} pages", 3);
//! assert_eq!((s.as_str(), p.as_str()), ("Saved as “Draft”.", "3 pages"));
//! ```
//!
//! Placeholders follow Slint's `@tr`: `{}` takes the next argument, `{0}` a numbered one,
//! `{n}` the count of a plural, `{{` and `}}` are braces.

use std::collections::HashMap;
use std::fmt::{Display, Write};
use std::sync::{Arc, RwLock};

mod format;
pub mod po;

pub use format::{bytes, date_time, decimal, integer};

mod catalogs {
    include!(concat!(env!("OUT_DIR"), "/catalogs.rs"));
}

/// A UI language.
#[derive(Debug, PartialEq, Eq)]
pub struct Language {
    /// The catalog's name: `po/<code>.po` (`en` has none).
    pub code: &'static str,
    /// The language's name in that language.
    pub name: &'static str,
    pub rtl: bool,
}

const fn lang(code: &'static str, name: &'static str, rtl: bool) -> Language {
    Language { code, name, rtl }
}

/// English first, then the translations by code.
pub static LANGUAGES: &[Language] = &[
    lang("en", "English", false),
    lang("ar", "العربية", true),
    lang("de", "Deutsch", false),
    lang("es", "Español", false),
    lang("fr", "Français", false),
    lang("he", "עברית", true),
    lang("it", "Italiano", false),
    lang("ja", "日本語", false),
    lang("ko", "한국어", false),
    lang("nl", "Nederlands", false),
    lang("pl", "Polski", false),
    lang("pt_BR", "Português (Brasil)", false),
    lang("ru", "Русский", false),
    lang("tr", "Türkçe", false),
    lang("uk", "Українська", false),
    lang("zh_CN", "简体中文", false),
];

/// The language for a locale or language tag ("de_DE.UTF-8", "pt-BR", "zh-Hans-CN",
/// "he-IL"), if it is one of [`LANGUAGES`].
pub fn find(tag: &str) -> Option<&'static Language> {
    let tag = tag
        .split(['.', '@'])
        .next()
        .unwrap_or(tag)
        .replace('-', "_");
    let mut parts = tag.split('_');
    let base = parts.next()?.to_ascii_lowercase();
    let base = match base.as_str() {
        "iw" => "he",
        "pt" => "pt_BR",
        "zh" => "zh_CN",
        "nb" | "nn" | "no" => return None,
        b => b,
    };
    LANGUAGES.iter().find(|l| l.code == base)
}

/// The system's language: `PAGEBARA_LANG`, else the locale (`LC_ALL`, `LC_MESSAGES`, `LANG`
/// on Unix; the platform's setting elsewhere), else English.
pub fn system_language() -> &'static Language {
    let env = std::env::var("PAGEBARA_LANG")
        .ok()
        .filter(|v| !v.is_empty());
    env.or_else(sys_locale::get_locale)
        .and_then(|l| find(&l))
        .unwrap_or(&LANGUAGES[0])
}

/// The language to show: the one chosen in the settings (a code of [`LANGUAGES`]), else
/// the system's (`PAGEBARA_LANG` first, as for UI tests).
pub fn choose(setting: Option<&str>) -> &'static Language {
    setting.and_then(find).unwrap_or_else(system_language)
}

/// For an app's build script: lays out the catalogs as Slint's bundled translations want
/// them (`<dir>/<lang>/LC_MESSAGES/<domain>.po`, the domain being the package name) and
/// returns the folder to pass to `with_bundled_translations`; `None` without catalogs.
pub fn slint_bundle(out_dir: &std::path::Path) -> Option<std::path::PathBuf> {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../po");
    let domain = std::env::var("CARGO_PKG_NAME").ok()?;
    let dir = out_dir.join("po");
    let mut any = false;
    println!("cargo:rerun-if-changed={}", src.display());
    for l in &LANGUAGES[1..] {
        let from = src.join(format!("{}.po", l.code));
        if !from.exists() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", from.display());
        let to = dir.join(l.code).join("LC_MESSAGES");
        std::fs::create_dir_all(&to).ok()?;
        std::fs::copy(&from, to.join(format!("{domain}.po"))).ok()?;
        any = true;
    }
    any.then_some(dir)
}

struct Current {
    lang: &'static Language,
    messages: HashMap<String, Vec<String>>,
}

static CURRENT: RwLock<Option<Arc<Current>>> = RwLock::new(None);

/// Makes `lang` the language of [`tr!`] and [`trn!`] (the Slint UI is switched separately).
pub fn set_language(lang: &'static Language) {
    let messages = catalogs::CATALOGS
        .iter()
        .find(|(code, _)| *code == lang.code)
        .map(|(_, blob)| unpack(blob))
        .unwrap_or_default();
    *CURRENT.write().unwrap_or_else(|e| e.into_inner()) =
        Some(Arc::new(Current { lang, messages }));
}

/// The current language (English until [`set_language`]).
pub fn language() -> &'static Language {
    current().map_or(&LANGUAGES[0], |c| c.lang)
}

pub fn is_rtl() -> bool {
    language().rtl
}

fn current() -> Option<Arc<Current>> {
    CURRENT.read().unwrap_or_else(|e| e.into_inner()).clone()
}

fn unpack(blob: &[u8]) -> HashMap<String, Vec<String>> {
    let Ok(raw) = miniz_oxide::inflate::decompress_to_vec(blob) else {
        return HashMap::new();
    };
    let text = String::from_utf8_lossy(&raw);
    text.split('\u{1e}')
        .filter_map(|rec| {
            let mut f = rec.split('\u{1f}');
            let id = f.next().filter(|id| !id.is_empty())?;
            Some((id.to_owned(), f.map(str::to_owned).collect()))
        })
        .collect()
}

/// The translation of `id`, formatted with `args`. Use [`tr!`].
pub fn translate(id: &str, args: &[&dyn Display]) -> String {
    let cur = current();
    let s = cur
        .as_ref()
        .and_then(|c| c.messages.get(id))
        .and_then(|v| v.first())
        .map_or(id, String::as_str);
    format(s, args, None)
}

/// The translation of a message with plural forms for `n`. Use [`trn!`].
pub fn translate_plural(id: &str, plural: &str, n: i64, args: &[&dyn Display]) -> String {
    let cur = current();
    let found = cur.as_ref().and_then(|c| {
        let forms = c.messages.get(id)?;
        forms.get(plural_form(c.lang.code, n)).or(forms.first())
    });
    let s = match found {
        Some(s) => s.as_str(),
        None if n == 1 => id,
        None => plural,
    };
    format(s, args, Some(n))
}

/// The plural form index for `n`, as the catalogs' `Plural-Forms` headers give it.
pub fn plural_form(code: &str, n: i64) -> usize {
    let n = n.unsigned_abs();
    let (m10, m100) = (n % 10, n % 100);
    let slavic_few = (2..=4).contains(&m10) && !(12..=14).contains(&m100);
    match code {
        "ja" | "ko" | "zh_CN" => 0,
        "fr" | "pt_BR" => usize::from(n > 1),
        "pl" => match n {
            1 => 0,
            _ if slavic_few => 1,
            _ => 2,
        },
        "ru" | "uk" => match () {
            _ if m10 == 1 && m100 != 11 => 0,
            _ if slavic_few => 1,
            _ => 2,
        },
        "ar" => match n {
            0 => 0,
            1 => 1,
            2 => 2,
            _ if (3..=10).contains(&m100) => 3,
            _ if m100 >= 11 => 4,
            _ => 5,
        },
        "he" => match n {
            1 => 0,
            2 => 1,
            _ => 2,
        },
        _ => usize::from(n != 1),
    }
}

/// The number of plural forms of a language.
pub fn plural_count(code: &str) -> usize {
    match code {
        "ja" | "ko" | "zh_CN" => 1,
        "pl" | "ru" | "uk" | "he" => 3,
        "ar" => 6,
        _ => 2,
    }
}

/// Fills Slint-style placeholders.
pub fn format(s: &str, args: &[&dyn Display], n: Option<i64>) -> String {
    let mut out = String::with_capacity(s.len() + 16);
    let mut next = 0;
    let mut rest = s;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        let Some(end) = tail.find('}').filter(|_| tail.starts_with('{')) else {
            out.push_str(&tail[..1]);
            rest = &tail[1..];
            continue;
        };
        let name = tail[1..end].trim();
        let arg = if name.is_empty() {
            next += 1;
            args.get(next - 1).map(|a| a.to_string())
        } else if name == "n" {
            n.map(|n| n.to_string())
        } else {
            name.parse::<usize>()
                .ok()
                .and_then(|i| args.get(i))
                .map(|a| a.to_string())
        };
        match arg {
            Some(a) => {
                let _ = write!(out, "{a}");
            }
            None => out.push_str(&tail[..=end]),
        }
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

/// A translated message: `tr!("Page {} of {}", page, count)`.
#[macro_export]
macro_rules! tr {
    ($id:literal $(,)?) => {
        $crate::translate($id, &[])
    };
    ($id:literal, $($arg:expr),+ $(,)?) => {
        $crate::translate($id, &[$(&$arg as &dyn ::std::fmt::Display),+])
    };
}

/// A translated message with plural forms: `trn!("{n} page", "{n} pages", count)`; further
/// arguments fill `{}` placeholders.
#[macro_export]
macro_rules! trn {
    ($id:literal, $plural:literal, $n:expr $(,)?) => {
        $crate::translate_plural($id, $plural, ($n) as i64, &[])
    };
    ($id:literal, $plural:literal, $n:expr, $($arg:expr),+ $(,)?) => {
        $crate::translate_plural($id, $plural, ($n) as i64, &[$(&$arg as &dyn ::std::fmt::Display),+])
    };
}

/// Marks a string for the catalog without translating it here (translated later with
/// [`translate`], e.g. a table of labels): `tr_noop!("Title")` is `"Title"`.
#[macro_export]
macro_rules! tr_noop {
    ($id:literal) => {
        $id
    };
}

#[cfg(test)]
mod tests;
