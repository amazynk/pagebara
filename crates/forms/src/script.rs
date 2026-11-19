//! Field scripts (`/AA`, §12.6.3) without a JavaScript engine: scripts are found and
//! reported, and the Acrobat helper calls most forms consist of (`AFNumber_Format`,
//! `AFDate_FormatEx`, `AFSimple_Calculate`, ...) are recognised and run natively.

use pagebara_annot::Color;
use pagebara_syntax::{Dict, Document, Object};

use crate::text::decode;

/// One script and, when it is a single call of a known helper, that call.
#[derive(Clone, Debug, PartialEq)]
pub struct Script {
    pub source: String,
    pub native: Option<Native>,
}

/// The helper calls we run ourselves.
#[derive(Clone, Debug, PartialEq)]
pub enum Native {
    /// `AFNumber_Format` / `AFNumber_Keystroke`.
    Number(NumberFormat),
    /// `AFPercent_Format` / `AFPercent_Keystroke`: decimals and separator style.
    Percent { decimals: u32, sep: u8 },
    /// `AFDate_FormatEx` / `AFDate_KeystrokeEx` (and the indexed `AFDate_Format`).
    Date(String),
    /// `AFSpecial_Format`: 0 ZIP, 1 ZIP+4, 2 phone, 3 social security number.
    Special(u8),
    /// `AFSimple_Calculate`.
    Calculate { op: CalcOp, fields: Vec<String> },
    /// `AFRange_Validate`.
    Range { min: Option<f64>, max: Option<f64> },
}

#[derive(Clone, Debug, PartialEq)]
pub struct NumberFormat {
    pub decimals: u32,
    /// 0 `1,234.56`, 1 `1234.56`, 2 `1.234,56`, 3 `1234,56`, 4 `1'234.56`.
    pub sep: u8,
    /// 0 minus, 1 red, 2 parentheses, 3 red parentheses.
    pub neg: u8,
    pub currency: String,
    pub currency_prepend: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CalcOp {
    Sum,
    Avg,
    Min,
    Max,
    Product,
}

/// The scripts of a field and its widgets.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Scripts {
    /// `K`: runs on each keystroke and when the value is committed.
    pub keystroke: Option<Script>,
    /// `F`: formats the value for display.
    pub format: Option<Script>,
    /// `V`: validates a new value.
    pub validate: Option<Script>,
    /// `C`: recalculates the value when others change.
    pub calculate: Option<Script>,
    /// Other triggers with JavaScript (mouse, focus, page, `/A`), by trigger name.
    pub other: Vec<(String, String)>,
}

impl Scripts {
    pub(crate) fn read(doc: &Document, d: &Dict) -> Scripts {
        let mut s = Scripts::default();
        s.add(doc, d, true);
        s
    }

    pub(crate) fn add(&mut self, doc: &Document, d: &Dict, field: bool) {
        let aa = doc.get_in(d, b"AA");
        if let Some(aa) = aa.as_dict() {
            for (k, v) in aa.iter() {
                let Some(src) = javascript(doc, v) else {
                    continue;
                };
                let script = || Script {
                    native: parse_native(&src),
                    source: src.clone(),
                };
                match (k.as_bytes(), field) {
                    (b"K", true) => self.keystroke = Some(script()),
                    (b"F", true) => self.format = Some(script()),
                    (b"V", true) => self.validate = Some(script()),
                    (b"C", true) => self.calculate = Some(script()),
                    (k, _) => self.other.push((crate::text::name_text(k), src.clone())),
                }
            }
        }
        if let Some(src) = d.get(b"A").and_then(|a| javascript(doc, a)) {
            self.other.push(("A".into(), src));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.keystroke.is_none()
            && self.format.is_none()
            && self.validate.is_none()
            && self.calculate.is_none()
            && self.other.is_empty()
    }

    /// Triggers whose scripts we cannot run (no JavaScript engine).
    pub fn unsupported(&self) -> Vec<String> {
        let mut v: Vec<String> = [
            ("K", &self.keystroke),
            ("F", &self.format),
            ("V", &self.validate),
            ("C", &self.calculate),
        ]
        .into_iter()
        .filter(|(_, s)| s.as_ref().is_some_and(|s| s.native.is_none()))
        .map(|(k, _)| k.to_owned())
        .collect();
        v.extend(self.other.iter().map(|(k, _)| k.clone()));
        v
    }
}

/// The JavaScript of an action and the actions it chains to with `/Next`.
pub(crate) fn javascript(doc: &Document, action: &Object) -> Option<String> {
    let mut out = String::new();
    let mut stack = vec![(*doc.resolve(action)).clone()];
    let mut n = 0;
    while let Some(a) = stack.pop() {
        n += 1;
        if n > 64 {
            break;
        }
        if let Object::Array(list) = &a {
            stack.extend(list.iter().rev().map(|o| (*doc.resolve(o)).clone()));
            continue;
        }
        let Some(d) = a.as_dict() else { continue };
        if d.name(b"S") == Some(b"JavaScript") {
            let js = doc.get_in(d, b"JS");
            let text = match &*js {
                Object::String(s) => Some(decode(&s.bytes)),
                Object::Stream(s) => doc.decode(s).ok().map(|d| decode(&d.data)),
                _ => None,
            };
            if let Some(t) = text {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&t);
            }
        }
        if let Some(next) = d.get(b"Next") {
            stack.push((*doc.resolve(next)).clone());
        }
    }
    (!out.trim().is_empty()).then_some(out)
}

/// Document-level scripts (the `/JavaScript` name tree and a JavaScript `/OpenAction`).
pub fn document_scripts(doc: &Document) -> Vec<String> {
    let Some(cat) = doc.catalog() else {
        return Vec::new();
    };
    let Some(cat) = cat.as_dict() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let names = doc.get_in(cat, b"Names");
    if let Some(tree) = names.as_dict().map(|n| doc.get_in(n, b"JavaScript"))
        && let Some(tree) = tree.as_dict()
    {
        let mut stack = vec![tree.clone()];
        let mut n = 0;
        while let Some(node) = stack.pop() {
            n += 1;
            if n > 10_000 {
                break;
            }
            if let Some(pairs) = doc.get_in(&node, b"Names").as_array() {
                for p in pairs.chunks(2) {
                    if let [_, v] = p
                        && let Some(js) = javascript(doc, v)
                    {
                        out.push(js);
                    }
                }
            }
            for k in doc.get_in(&node, b"Kids").as_array().unwrap_or_default() {
                if let Some(d) = doc.resolve(k).as_dict() {
                    stack.push(d.clone());
                }
            }
        }
    }
    if let Some(js) = cat.get(b"OpenAction").and_then(|a| javascript(doc, a)) {
        out.push(js);
    }
    out
}

// A tiny reader for the literal arguments of one call.

#[derive(Clone, Debug, PartialEq)]
enum Arg {
    Num(f64),
    Str(String),
    Bool(bool),
    List(Vec<Arg>),
}

struct Js<'a> {
    s: &'a [u8],
    i: usize,
}

impl Js<'_> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn ident(&mut self) -> &str {
        self.ws();
        let start = self.i;
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_alphanumeric() || matches!(self.s[self.i], b'_' | b'.'))
        {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i]).unwrap_or("")
    }

    fn args(&mut self, close: u8) -> Option<Vec<Arg>> {
        let mut out = Vec::new();
        if self.eat(close) {
            return Some(out);
        }
        loop {
            out.push(self.arg()?);
            if self.eat(close) {
                return Some(out);
            }
            if !self.eat(b',') {
                return None;
            }
        }
    }

    fn arg(&mut self) -> Option<Arg> {
        self.ws();
        match *self.s.get(self.i)? {
            q @ (b'"' | b'\'') => {
                self.i += 1;
                let mut out = Vec::new();
                while let Some(&c) = self.s.get(self.i) {
                    self.i += 1;
                    match c {
                        b'\\' => {
                            let e = *self.s.get(self.i)?;
                            self.i += 1;
                            out.push(match e {
                                b'n' => b'\n',
                                b't' => b'\t',
                                e => e,
                            });
                        }
                        c if c == q => return Some(Arg::Str(String::from_utf8(out).ok()?)),
                        c => out.push(c),
                    }
                }
                None
            }
            b'[' => {
                self.i += 1;
                self.args(b']').map(Arg::List)
            }
            b'-' | b'+' | b'.' | b'0'..=b'9' => {
                let start = self.i;
                self.i += 1;
                while self
                    .s
                    .get(self.i)
                    .is_some_and(|c| c.is_ascii_digit() || *c == b'.')
                {
                    self.i += 1;
                }
                let t = std::str::from_utf8(&self.s[start..self.i]).ok()?;
                t.parse().ok().map(Arg::Num)
            }
            _ => match self.ident() {
                "true" => Some(Arg::Bool(true)),
                "false" => Some(Arg::Bool(false)),
                "new" => {
                    if self.ident() != "Array" || !self.eat(b'(') {
                        return None;
                    }
                    self.args(b')').map(Arg::List)
                }
                _ => None,
            },
        }
    }
}

/// `name(args);` when that is the whole script.
fn single_call(src: &str) -> Option<(String, Vec<Arg>)> {
    let mut p = Js {
        s: src.as_bytes(),
        i: 0,
    };
    let name = p.ident().to_owned();
    if name.is_empty() || !p.eat(b'(') {
        return None;
    }
    let args = p.args(b')')?;
    p.eat(b';');
    p.ws();
    (p.i == p.s.len()).then_some((name, args))
}

const DATE_FORMATS: [&str; 14] = [
    "m/d",
    "m/d/yy",
    "mm/dd/yy",
    "mm/yy",
    "d-mmm",
    "d-mmm-yy",
    "dd-mmm-yy",
    "yy-mm-dd",
    "mmm-yy",
    "mmmm-yy",
    "mmm d, yyyy",
    "mmmm d, yyyy",
    "m/d/yy h:MM tt",
    "m/d/yy HH:MM",
];

pub(crate) fn parse_native(src: &str) -> Option<Native> {
    let (name, args) = single_call(src)?;
    let num = |i: usize| match args.get(i) {
        Some(Arg::Num(n)) => Some(*n),
        Some(Arg::Bool(b)) => Some(f64::from(u8::from(*b))),
        _ => None,
    };
    let s = |i: usize| match args.get(i) {
        Some(Arg::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let small = |i: usize| num(i).map_or(0, |n| n.clamp(0.0, 9.0) as u8);
    Some(match name.as_str() {
        "AFNumber_Format" | "AFNumber_Keystroke" => Native::Number(NumberFormat {
            decimals: num(0)?.clamp(0.0, 12.0) as u32,
            sep: small(1),
            neg: small(2),
            currency: s(4).unwrap_or_default(),
            currency_prepend: num(5).is_none_or(|b| b != 0.0),
        }),
        "AFPercent_Format" | "AFPercent_Keystroke" => Native::Percent {
            decimals: num(0)?.clamp(0.0, 12.0) as u32,
            sep: small(1),
        },
        "AFDate_FormatEx" | "AFDate_KeystrokeEx" => Native::Date(s(0)?),
        "AFDate_Format" | "AFDate_Keystroke" => {
            Native::Date(DATE_FORMATS.get(num(0)? as usize)?.to_string())
        }
        "AFSpecial_Format" | "AFSpecial_Keystroke" => Native::Special(small(0).min(3)),
        "AFSimple_Calculate" => {
            let op = match s(0)?.to_ascii_uppercase().as_str() {
                "SUM" => CalcOp::Sum,
                "AVG" => CalcOp::Avg,
                "MIN" => CalcOp::Min,
                "MAX" => CalcOp::Max,
                "PRD" => CalcOp::Product,
                _ => return None,
            };
            let fields = match args.get(1)? {
                Arg::List(l) => l
                    .iter()
                    .filter_map(|a| match a {
                        Arg::Str(s) => Some(s.trim().to_owned()),
                        _ => None,
                    })
                    .collect(),
                Arg::Str(s) => s
                    .split(',')
                    .map(|n| n.trim().to_owned())
                    .filter(|n| !n.is_empty())
                    .collect(),
                _ => return None,
            };
            Native::Calculate { op, fields }
        }
        "AFRange_Validate" => Native::Range {
            min: (num(0)? != 0.0).then(|| num(1)).flatten(),
            max: (num(2)? != 0.0).then(|| num(3)).flatten(),
        },
        _ => return None,
    })
}

/// A formatted value and, for red negative numbers, its colour.
#[derive(Clone, Debug, PartialEq)]
pub struct Formatted {
    pub text: String,
    pub color: Option<Color>,
}

/// Parses a number as `AFMakeNumber` does: separators by the style, currency and spaces
/// ignored. Empty input is `None`.
pub fn parse_number(s: &str, sep: u8) -> Option<f64> {
    let comma_decimal = matches!(sep, 2 | 3);
    let mut neg = false;
    let mut t = String::new();
    for c in s.trim().chars() {
        match c {
            '0'..='9' => t.push(c),
            '.' if !comma_decimal => t.push('.'),
            ',' if comma_decimal => t.push('.'),
            '-' | '(' => neg = true,
            ',' | '.' | '\'' | ' ' | ')' | '$' | '€' | '£' | '¥' | '+' | '%' => {}
            c if c.is_alphabetic() && !c.is_ascii_digit() && t.is_empty() => {}
            _ => return None,
        }
    }
    if t.is_empty() {
        return None;
    }
    let v: f64 = t.parse().ok()?;
    Some(if neg { -v } else { v })
}

fn group(int: &str, sep: char) -> String {
    let mut out = String::new();
    for (i, c) in int.chars().enumerate() {
        if i > 0 && (int.len() - i).is_multiple_of(3) {
            out.push(sep);
        }
        out.push(c);
    }
    out
}

fn number_text(v: f64, decimals: u32, sep: u8) -> String {
    // Halves round away from zero, as JavaScript's toFixed does for most inputs.
    let scale = 10f64.powi(decimals as i32);
    let s = format!(
        "{:.*}",
        decimals as usize,
        (v.abs() * scale).round() / scale
    );
    let (int, frac) = s.split_once('.').unwrap_or((&s, ""));
    let (thousands, point) = match sep {
        0 => (Some(','), '.'),
        1 => (None, '.'),
        2 => (Some('.'), ','),
        3 => (None, ','),
        _ => (Some('\''), '.'),
    };
    let mut out = thousands.map_or(int.to_owned(), |t| group(int, t));
    if !frac.is_empty() {
        out.push(point);
        out.push_str(frac);
    }
    out
}

impl NumberFormat {
    pub fn format(&self, value: &str) -> Option<Formatted> {
        let v = parse_number(value, self.sep)?;
        let mut text = number_text(v, self.decimals, self.sep);
        if !self.currency.is_empty() {
            text = if self.currency_prepend {
                format!("{}{text}", self.currency)
            } else {
                format!("{text}{}", self.currency)
            };
        }
        let negative = v < 0.0 && text.chars().any(|c| c.is_ascii_digit() && c != '0');
        let mut color = None;
        if negative {
            match self.neg {
                0 => text.insert(0, '-'),
                1 => color = Some(Color::Rgb(1.0, 0.0, 0.0)),
                2 => text = format!("({text})"),
                _ => {
                    text = format!("({text})");
                    color = Some(Color::Rgb(1.0, 0.0, 0.0));
                }
            }
        }
        Some(Formatted { text, color })
    }
}

const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];

/// A date (and time) as the date helpers see it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Date {
    pub year: i32,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

fn days_in(year: i32, month: u8) -> u8 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Splits a format into tokens: runs of one letter, or literal characters.
fn tokens(fmt: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for c in fmt.chars() {
        match out.last_mut() {
            Some(last) if c.is_ascii_alphabetic() && last.starts_with(c) => last.push(c),
            _ => out.push(c.to_string()),
        }
    }
    out
}

/// Parses `s` in format `fmt` (`d`, `dd`, `m`, `mm`, `mmm`, `mmmm`, `yy`, `yyyy`, `h`,
/// `HH`, `MM`, `ss`, `tt`); also accepts ISO `yyyy-mm-dd` and month names anywhere a month
/// is expected.
pub fn parse_date(s: &str, fmt: &str) -> Option<Date> {
    parse_with(s, fmt)
        .or_else(|| parse_with(s, "yyyy-mm-dd"))
        .or_else(|| {
            // Loose: numbers in the format's order of day, month and year.
            let nums: Vec<u32> = s
                .split(|c: char| !c.is_ascii_digit())
                .filter(|p| !p.is_empty())
                .filter_map(|p| p.parse().ok())
                .collect();
            let order: Vec<char> = tokens(fmt)
                .iter()
                .filter_map(|t| t.chars().next().filter(|c| matches!(c, 'd' | 'm' | 'y')))
                .collect();
            if nums.len() < 3 || order.len() < 3 {
                return None;
            }
            let mut d = Date {
                year: 0,
                month: 1,
                day: 1,
                hour: 0,
                minute: 0,
                second: 0,
            };
            for (c, n) in order.iter().zip(&nums) {
                match c {
                    'd' => d.day = u8::try_from(*n).ok()?,
                    'm' => d.month = u8::try_from(*n).ok()?,
                    _ => d.year = full_year(*n),
                }
            }
            valid(d)
        })
}

fn full_year(n: u32) -> i32 {
    match n {
        0..=49 => 2000 + n as i32,
        50..=99 => 1900 + n as i32,
        n => n as i32,
    }
}

fn valid(d: Date) -> Option<Date> {
    ((1..=12).contains(&d.month)
        && d.day >= 1
        && d.day <= days_in(d.year, d.month)
        && d.hour < 24
        && d.minute < 60
        && d.second < 60)
        .then_some(d)
}

fn parse_with(s: &str, fmt: &str) -> Option<Date> {
    let mut d = Date {
        year: 2000,
        month: 1,
        day: 1,
        hour: 0,
        minute: 0,
        second: 0,
    };
    let mut rest = s.trim();
    let mut pm = None;
    let take_num = |rest: &mut &str, max: usize| -> Option<u32> {
        let n = rest
            .chars()
            .take(max)
            .take_while(char::is_ascii_digit)
            .count();
        if n == 0 {
            return None;
        }
        let v = rest[..n].parse().ok()?;
        *rest = &rest[n..];
        Some(v)
    };
    let take_month = |rest: &mut &str| -> Option<u8> {
        let lower = rest.to_ascii_lowercase();
        let (i, len) = MONTHS.iter().enumerate().find_map(|(i, m)| {
            let m = m.to_ascii_lowercase();
            if lower.starts_with(&m) {
                Some((i, m.len()))
            } else if lower.starts_with(&m[..3]) {
                Some((i, 3))
            } else {
                None
            }
        })?;
        *rest = &rest[len..];
        Some(i as u8 + 1)
    };
    for t in tokens(fmt) {
        match t.as_str() {
            "d" | "dd" => d.day = u8::try_from(take_num(&mut rest, 2)?).ok()?,
            "m" | "mm" => {
                d.month = match take_num(&mut rest, 2) {
                    Some(m) => u8::try_from(m).ok()?,
                    None => take_month(&mut rest)?,
                }
            }
            "mmm" | "mmmm" => d.month = take_month(&mut rest)?,
            "yy" => d.year = full_year(take_num(&mut rest, 2)?),
            "yyyy" => {
                let n = rest.chars().take_while(char::is_ascii_digit).count();
                let v = take_num(&mut rest, 4)?;
                d.year = if n <= 2 { full_year(v) } else { v as i32 };
            }
            "h" | "hh" | "H" | "HH" => d.hour = u8::try_from(take_num(&mut rest, 2)?).ok()?,
            "M" | "MM" => d.minute = u8::try_from(take_num(&mut rest, 2)?).ok()?,
            "s" | "ss" => d.second = u8::try_from(take_num(&mut rest, 2)?).ok()?,
            "tt" | "t" => {
                let l = rest.to_ascii_lowercase();
                pm = Some(l.starts_with('p'));
                rest = rest
                    .get(rest.chars().take_while(char::is_ascii_alphabetic).count()..)
                    .unwrap_or("");
            }
            lit => {
                let lit = lit.trim();
                rest = rest.trim_start();
                if !lit.is_empty() {
                    rest = rest.strip_prefix(lit)?;
                }
            }
        }
    }
    if !rest.trim().is_empty() {
        return None;
    }
    if let Some(pm) = pm {
        d.hour = d.hour % 12 + if pm { 12 } else { 0 };
    }
    valid(d)
}

/// Formats `d` with the same tokens [`parse_date`] reads.
pub fn format_date(d: &Date, fmt: &str) -> String {
    let mut out = String::new();
    for t in tokens(fmt) {
        let h12 = if d.hour % 12 == 0 { 12 } else { d.hour % 12 };
        match t.as_str() {
            "d" => out.push_str(&d.day.to_string()),
            "dd" => out.push_str(&format!("{:02}", d.day)),
            "m" => out.push_str(&d.month.to_string()),
            "mm" => out.push_str(&format!("{:02}", d.month)),
            "mmm" => out.push_str(&MONTHS[usize::from(d.month - 1)][..3]),
            "mmmm" => out.push_str(MONTHS[usize::from(d.month - 1)]),
            "yy" => out.push_str(&format!("{:02}", d.year.rem_euclid(100))),
            "yyyy" => out.push_str(&format!("{:04}", d.year)),
            "h" => out.push_str(&h12.to_string()),
            "hh" => out.push_str(&format!("{h12:02}")),
            "H" => out.push_str(&d.hour.to_string()),
            "HH" => out.push_str(&format!("{:02}", d.hour)),
            "M" => out.push_str(&d.minute.to_string()),
            "MM" => out.push_str(&format!("{:02}", d.minute)),
            "s" => out.push_str(&d.second.to_string()),
            "ss" => out.push_str(&format!("{:02}", d.second)),
            "tt" => out.push_str(if d.hour < 12 { "am" } else { "pm" }),
            "t" => out.push(if d.hour < 12 { 'a' } else { 'p' }),
            lit => out.push_str(lit),
        }
    }
    out
}

fn special(value: &str, kind: u8) -> Option<String> {
    let digits: String = value.chars().filter(char::is_ascii_digit).collect();
    let d = |r: std::ops::Range<usize>| &digits[r];
    Some(match (kind, digits.len()) {
        (0, 5) => digits,
        (1, 9) => format!("{}-{}", d(0..5), d(5..9)),
        (2, 10) => format!("({}) {}-{}", d(0..3), d(3..6), d(6..10)),
        (2, 7) => format!("{}-{}", d(0..3), d(3..7)),
        (3, 9) => format!("{}-{}-{}", d(0..3), d(3..5), d(5..9)),
        _ => return None,
    })
}

impl Native {
    /// The display text of `value` for a format script; `None` when the value does not fit
    /// (it is then shown as is).
    pub fn format(&self, value: &str) -> Option<Formatted> {
        if value.trim().is_empty() {
            return None;
        }
        let plain = |text: String| Formatted { text, color: None };
        match self {
            Native::Number(f) => f.format(value),
            Native::Percent { decimals, sep } => {
                let v = parse_number(value, *sep)? * 100.0;
                let mut t = number_text(v, *decimals, *sep);
                if v < 0.0 && t.chars().any(|c| c.is_ascii_digit() && c != '0') {
                    t.insert(0, '-');
                }
                Some(plain(format!("{t}%")))
            }
            Native::Date(fmt) => parse_date(value, fmt).map(|d| plain(format_date(&d, fmt))),
            Native::Special(k) => special(value, *k).map(plain),
            _ => None,
        }
    }

    /// Whether `value` is acceptable to a keystroke or validate script.
    pub fn accepts(&self, value: &str) -> bool {
        if value.trim().is_empty() {
            return true;
        }
        match self {
            Native::Number(f) => parse_number(value, f.sep).is_some(),
            Native::Percent { sep, .. } => parse_number(value, *sep).is_some(),
            Native::Date(fmt) => parse_date(value, fmt).is_some(),
            Native::Special(k) => special(value, *k).is_some(),
            Native::Range { min, max } => parse_number(value, 0)
                .is_some_and(|v| min.is_none_or(|m| v >= m) && max.is_none_or(|m| v <= m)),
            Native::Calculate { .. } => true,
        }
    }
}

impl CalcOp {
    pub fn apply(self, values: &[f64]) -> f64 {
        match self {
            CalcOp::Sum => values.iter().sum(),
            CalcOp::Product => values.iter().product(),
            CalcOp::Avg if values.is_empty() => 0.0,
            CalcOp::Avg => values.iter().sum::<f64>() / values.len() as f64,
            CalcOp::Min => values.iter().copied().reduce(f64::min).unwrap_or(0.0),
            CalcOp::Max => values.iter().copied().reduce(f64::max).unwrap_or(0.0),
        }
    }
}

/// A number as JavaScript prints it (`3`, `0.5`, `-2.25`).
pub(crate) fn js_number(v: f64) -> String {
    if !v.is_finite() {
        return String::new();
    }
    // Rounding noise from summing decimals (0.1 + 0.2) is cut at 10 places, as forms
    // computing money expect.
    let r = (v * 1e10).round() / 1e10;
    let r = if r == 0.0 { 0.0 } else { r };
    format!("{r}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_helpers() {
        assert_eq!(
            parse_native("AFNumber_Format(2, 0, 0, 0, \"$\", true);"),
            Some(Native::Number(NumberFormat {
                decimals: 2,
                sep: 0,
                neg: 0,
                currency: "$".into(),
                currency_prepend: true,
            }))
        );
        assert_eq!(
            parse_native("AFSimple_Calculate(\"SUM\", new Array (\"a\", \"b.c\"));"),
            Some(Native::Calculate {
                op: CalcOp::Sum,
                fields: vec!["a".into(), "b.c".into()]
            })
        );
        assert_eq!(
            parse_native("AFSimple_Calculate('PRD', 'x, y')"),
            Some(Native::Calculate {
                op: CalcOp::Product,
                fields: vec!["x".into(), "y".into()]
            })
        );
        assert_eq!(
            parse_native("AFDate_FormatEx(\"dd.mm.yyyy\")"),
            Some(Native::Date("dd.mm.yyyy".into()))
        );
        assert_eq!(
            parse_native("AFDate_Format(2)"),
            Some(Native::Date("mm/dd/yy".into()))
        );
        assert_eq!(
            parse_native("AFNumber_Format(2,0,0,0,'',true); app.alert(1)"),
            None
        );
        assert_eq!(parse_native("event.value = 1"), None);
        assert_eq!(
            parse_native("AFRange_Validate(true, 0, true, 100)"),
            Some(Native::Range {
                min: Some(0.0),
                max: Some(100.0)
            })
        );
    }

    #[test]
    fn numbers() {
        let f = |d, sep, neg, cur: &str, pre| NumberFormat {
            decimals: d,
            sep,
            neg,
            currency: cur.into(),
            currency_prepend: pre,
        };
        let t = |nf: NumberFormat, v: &str| nf.format(v).map(|f| f.text);
        assert_eq!(
            t(f(2, 0, 0, "$", true), "1234567.891"),
            Some("$1,234,567.89".into())
        );
        assert_eq!(t(f(2, 0, 0, "$", true), "-12.5"), Some("-$12.50".into()));
        assert_eq!(t(f(0, 1, 2, "", true), "-12.5"), Some("(13)".into()));
        assert_eq!(
            t(f(2, 2, 0, " €", false), "1234,5"),
            Some("1.234,50 €".into())
        );
        assert_eq!(t(f(1, 4, 0, "", true), "1234.56"), Some("1'234.6".into()));
        assert_eq!(t(f(2, 0, 0, "", true), "abc"), None);
        let red = f(2, 0, 1, "", true).format("-3").unwrap();
        assert_eq!(red.text, "3.00");
        assert_eq!(red.color, Some(Color::Rgb(1.0, 0.0, 0.0)));
        assert_eq!(
            Native::Percent {
                decimals: 1,
                sep: 0
            }
            .format("0.256")
            .unwrap()
            .text,
            "25.6%"
        );
        assert_eq!(js_number(0.1 + 0.2), "0.3");
        assert_eq!(js_number(6.0), "6");
        assert_eq!(CalcOp::Avg.apply(&[1.0, 2.0]), 1.5);
        assert_eq!(CalcOp::Min.apply(&[3.0, -1.0]), -1.0);
    }

    #[test]
    fn dates() {
        let d = parse_date("05.03.2024", "dd.mm.yyyy").unwrap();
        assert_eq!((d.year, d.month, d.day), (2024, 3, 5));
        assert_eq!(format_date(&d, "mmmm d, yyyy"), "March 5, 2024");
        assert_eq!(format_date(&d, "d-mmm-yy"), "5-Mar-24");
        assert_eq!(
            parse_date("2024-03-05", "mm/dd/yyyy").map(|d| d.day),
            Some(5)
        );
        assert!(parse_date("31.02.2024", "dd.mm.yyyy").is_none());
        let t = parse_date("3/5/24 2:07 pm", "m/d/yy h:MM tt").unwrap();
        assert_eq!((t.hour, t.minute), (14, 7));
        assert_eq!(format_date(&t, "m/d/yy HH:MM"), "3/5/24 14:07");
        assert_eq!(
            Native::Special(2).format("5551234567").unwrap().text,
            "(555) 123-4567"
        );
        assert!(!Native::Special(0).accepts("1234"));
    }
}
