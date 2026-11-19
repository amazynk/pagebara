//! Numbers, sizes and dates in the current language's style.

use crate::{language, tr};

struct Style {
    decimal: char,
    group: char,
    /// Digits before grouping starts: 4 groups 1,000; 5 groups only 10 000 and up.
    min_grouped: usize,
}

fn style(code: &str) -> Style {
    let s = |decimal, group, min_grouped| Style {
        decimal,
        group,
        min_grouped,
    };
    match code {
        "de" | "it" | "nl" | "pt_BR" | "tr" => s(',', '.', 4),
        "es" => s(',', '.', 5),
        "fr" => s(',', '\u{202f}', 4),
        "pl" => s(',', '\u{a0}', 5),
        "ru" | "uk" => s(',', '\u{a0}', 4),
        _ => s('.', ',', 4),
    }
}

/// `1234567` as "1,234,567" (English), "1.234.567" (German), "1 234 567" (French).
pub fn integer(n: i64) -> String {
    group(
        &n.unsigned_abs().to_string(),
        n < 0,
        &style(language().code),
    )
}

/// `x` with `places` decimals: "1.5" (English), "1,5" (German).
pub fn decimal(x: f64, places: usize) -> String {
    let st = style(language().code);
    let s = format!("{:.*}", places, x.abs());
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let negative = x < 0.0 && s.bytes().any(|b| b.is_ascii_digit() && b != b'0');
    let mut out = group(int, negative, &st);
    if !frac.is_empty() {
        out.push(st.decimal);
        out.push_str(frac);
    }
    out
}

fn group(digits: &str, negative: bool, st: &Style) -> String {
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    let len = digits.len();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && len >= st.min_grouped && (len - i) % 3 == 0 {
            out.push(st.group);
        }
        out.push(c);
    }
    out
}

/// A file size: "820 bytes", "12.4 KB", "3.1 MB", "1.2 GB" (binary units).
pub fn bytes(n: u64) -> String {
    const K: f64 = 1024.0;
    let x = n as f64;
    if n < 1024 {
        crate::trn!("{n} byte", "{n} bytes", n)
    } else if x < K * K {
        tr!("{} KB", decimal(x / K, if x < 10.0 * K { 1 } else { 0 }))
    } else if x < K * K * K {
        tr!("{} MB", decimal(x / (K * K), 1))
    } else {
        tr!("{} GB", decimal(x / (K * K * K), 1))
    }
}

/// A date and time, numeric, in the language's order: "2026-11-11 09:30" (English),
/// "11.11.2026, 09:30" (German).
pub fn date_time(year: u32, month: u32, day: u32, hour: u32, minute: u32) -> String {
    let (y, m, d, h, mi) = (year, month, day, hour, minute);
    match language().code {
        "de" | "pl" | "ru" | "uk" | "he" => format!("{d:02}.{m:02}.{y}, {h:02}:{mi:02}"),
        "tr" => format!("{d:02}.{m:02}.{y} {h:02}:{mi:02}"),
        "fr" | "es" | "it" | "pt_BR" | "ar" => format!("{d:02}/{m:02}/{y} {h:02}:{mi:02}"),
        "nl" => format!("{d:02}-{m:02}-{y} {h:02}:{mi:02}"),
        "ja" | "zh_CN" => format!("{y}/{m:02}/{d:02} {h:02}:{mi:02}"),
        "ko" => format!("{y}. {m}. {d}. {h:02}:{mi:02}"),
        _ => format!("{y}-{m:02}-{d:02} {h:02}:{mi:02}"),
    }
}
