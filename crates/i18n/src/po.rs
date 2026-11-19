//! A small reader for gettext `.po` files (also used by the build script).

/// One message of a catalog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub context: Option<String>,
    pub id: String,
    pub plural: Option<String>,
    /// One string, or one per plural form.
    pub strs: Vec<String>,
    /// `#:` source references.
    pub refs: Vec<String>,
    pub fuzzy: bool,
}

impl Entry {
    /// Translated, not fuzzy, and not the header.
    pub fn translated(&self) -> bool {
        !self.id.is_empty()
            && !self.fuzzy
            && !self.strs.is_empty()
            && self.strs.iter().all(|s| !s.is_empty())
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Field {
    None,
    Context,
    Id,
    Plural,
    Str(usize),
}

/// The entries of a `.po` file, the header (empty id) included; obsolete (`#~`) ones are skipped.
pub fn parse(src: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut e = Entry::default();
    let mut field = Field::None;
    let mut started = false;
    for line in src.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('#') {
            if started && field != Field::None {
                out.push(std::mem::take(&mut e));
                started = false;
                field = Field::None;
            }
            if let Some(r) = rest.strip_prefix(':') {
                e.refs.extend(r.split_whitespace().map(str::to_owned));
            } else if let Some(f) = rest.strip_prefix(',') {
                e.fuzzy |= f.split(',').any(|f| f.trim() == "fuzzy");
            }
            continue;
        }
        let (key, value) = match line.find(' ') {
            Some(i) if !line.starts_with('"') => (&line[..i], line[i..].trim()),
            _ => ("", line),
        };
        let next = match key {
            "msgctxt" => Field::Context,
            "msgid" => Field::Id,
            "msgid_plural" => Field::Plural,
            "msgstr" => Field::Str(0),
            // No language has more than six plural forms.
            k if k.starts_with("msgstr[") => k[7..]
                .strip_suffix(']')
                .and_then(|n| n.parse().ok())
                .filter(|&n| n < 16)
                .map_or(Field::None, Field::Str),
            _ => field,
        };
        if key == "msgctxt" || (key == "msgid" && field != Field::Context) {
            if started {
                out.push(std::mem::take(&mut e));
            }
            started = true;
        }
        field = next;
        let s = unquote(value);
        match field {
            Field::None => {}
            Field::Context => e.context.get_or_insert_with(String::new).push_str(&s),
            Field::Id => e.id.push_str(&s),
            Field::Plural => e.plural.get_or_insert_with(String::new).push_str(&s),
            Field::Str(i) => {
                if e.strs.len() <= i {
                    e.strs.resize(i + 1, String::new());
                }
                e.strs[i].push_str(&s);
            }
        }
    }
    if started {
        out.push(e);
    }
    out
}

fn unquote(v: &str) -> String {
    let v = v.strip_prefix('"').unwrap_or(v);
    let v = v.strip_suffix('"').unwrap_or(v);
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some(c) => out.push(c),
            None => {}
        }
    }
    out
}

/// The `Plural-Forms` header's form count, if any.
pub fn plural_count(entries: &[Entry]) -> Option<usize> {
    let header = entries
        .iter()
        .find(|e| e.id.is_empty() && e.context.is_none())?;
    let line = header
        .strs
        .first()?
        .lines()
        .find(|l| l.starts_with("Plural-Forms:"))?;
    let n = line.split(';').find_map(|p| {
        p.trim()
            .strip_prefix("Plural-Forms:")
            .unwrap_or(p)
            .trim()
            .strip_prefix("nplurals=")
    })?;
    n.trim().parse().ok()
}
