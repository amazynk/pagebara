//! Headers and footers: six text slots with tokens, set inside the page's margins as the
//! page is shown (crop box and rotation).

use std::fmt::Write as _;

use pagebara_syntax::{Dict, Document};
use serde::{Deserialize, Serialize};

use crate::bates::BatesNumber;
use crate::date::{Clock, format_date};
use crate::draw::{self, PageStamp, num};
use crate::text::{Line, Typesetter};
use crate::{Kind, PageSet, Report, Result, TextStyle, Visibility, oc};

/// Slot texts: top left, top centre, top right, bottom left, bottom centre, bottom right.
///
/// Tokens: `{page}`, `{pages}`, `{label}` (the page label), `{date}`, `{date:FORMAT}`,
/// `{time}`, `{time:FORMAT}` (`strftime`: `%Y-%m-%d`), `{file}`, `{title}`, `{author}`,
/// `{bates}`; `{{` for a brace. Lines break at `\n`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Slots(pub [String; 6]);

impl Slots {
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|s| s.trim().is_empty())
    }

    /// One slot set, the others empty.
    pub fn one(slot: usize, text: &str) -> Slots {
        let mut s = Slots::default();
        if let Some(t) = s.0.get_mut(slot) {
            *t = text.into();
        }
        s
    }
}

/// Distances from the shown page's edges, in points.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Margins {
    pub left: f32,
    pub right: f32,
    pub top: f32,
    pub bottom: f32,
}

impl Default for Margins {
    fn default() -> Self {
        Margins {
            left: 36.0,
            right: 36.0,
            top: 24.0,
            bottom: 24.0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HeaderFooter {
    pub slots: Slots,
    /// Different slots on the first page stamped.
    pub first: Option<Slots>,
    /// Different slots on even pages (counted as shown, from 1).
    pub even: Option<Slots>,
    pub style: TextStyle,
    pub margins: Margins,
    pub pages: PageSet,
    /// The `{page}` number of the first page stamped.
    pub start: i64,
    pub date_format: String,
    pub time_format: String,
    /// The moment `{date}` and `{time}` show (`None`: now, in UTC).
    pub clock: Option<Clock>,
    /// Bates numbering: `{bates}`, and the stamps are marked as Bates numbers.
    pub bates: Option<BatesNumber>,
    /// An optional content group to draw them in.
    pub group: Option<String>,
    pub visibility: Visibility,
    /// Remove our earlier headers and footers (or Bates numbers) first.
    pub replace: bool,
}

impl Default for HeaderFooter {
    fn default() -> Self {
        HeaderFooter {
            slots: Slots::one(4, "Page {page} of {pages}"),
            first: None,
            even: None,
            style: TextStyle::default(),
            margins: Margins::default(),
            pages: PageSet::all(),
            start: 1,
            date_format: "%Y-%m-%d".into(),
            time_format: "%H:%M".into(),
            clock: None,
            bates: None,
            group: None,
            visibility: Visibility::Always,
            replace: true,
        }
    }
}

/// What the document-wide tokens stand for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocInfo {
    pub file_name: String,
    pub title: String,
    pub author: String,
}

impl DocInfo {
    /// Title and author from the document information dictionary.
    pub fn read(doc: &Document, file_name: &str) -> DocInfo {
        let info = doc
            .trailer()
            .get(b"Info")
            .map(|o| doc.resolve(o).as_dict().cloned().unwrap_or_default())
            .unwrap_or_default();
        let get = |k: &[u8]| {
            doc.get_in(&info, k)
                .as_string()
                .map(oc::pdf_text)
                .unwrap_or_default()
        };
        DocInfo {
            file_name: file_name.into(),
            title: get(b"Title"),
            author: get(b"Author"),
        }
    }
}

/// Per-page values of the tokens.
pub(crate) struct Values<'a> {
    pub page: i64,
    pub pages: i64,
    pub label: &'a str,
    pub bates: &'a str,
    pub clock: Clock,
    pub info: &'a DocInfo,
}

/// `text` with its tokens replaced.
pub(crate) fn expand(text: &str, hf: &HeaderFooter, v: &Values<'_>) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(i) = rest.find(['{', '}']) {
        out.push_str(&rest[..i]);
        let tail = &rest[i..];
        if tail.starts_with("{{") || tail.starts_with("}}") {
            out.push_str(&tail[..1]);
            rest = &tail[2..];
            continue;
        }
        if let Some(t) = tail.strip_prefix('}') {
            out.push('}');
            rest = t;
            continue;
        }
        let Some(end) = tail.find('}') else {
            out.push_str(tail);
            rest = "";
            break;
        };
        let token = &tail[1..end];
        let (name, arg) = token.split_once(':').unwrap_or((token, ""));
        let s = match name.trim() {
            "page" => v.page.to_string(),
            "pages" => v.pages.to_string(),
            "label" => v.label.to_owned(),
            "bates" => v.bates.to_owned(),
            "date" => format_date(v.clock, if arg.is_empty() { &hf.date_format } else { arg }),
            "time" => format_date(v.clock, if arg.is_empty() { &hf.time_format } else { arg }),
            "file" => v.info.file_name.clone(),
            "title" => v.info.title.clone(),
            "author" => v.info.author.clone(),
            _ => tail[..=end].to_owned(),
        };
        out.push_str(&s);
        rest = &tail[end + 1..];
    }
    out.push_str(rest);
    out
}

fn colour(c: crate::Rgb, op: &str) -> String {
    let [r, g, b] = c.0;
    format!("{} {} {} {op}", num(r.into()), num(g.into()), num(b.into()))
}

/// Applies headers and footers; `bates_start` overrides the first Bates number.
pub fn apply_header_footer(
    doc: &mut Document,
    hf: &HeaderFooter,
    info: &DocInfo,
) -> Result<Report> {
    apply(doc, hf, info).map(|(r, _)| r)
}

/// [`apply_header_footer`], also returning the last Bates number used.
pub(crate) fn apply(
    doc: &mut Document,
    hf: &HeaderFooter,
    info: &DocInfo,
) -> Result<(Report, Option<u64>)> {
    crate::check_editable(doc)?;
    let kind = if hf.bates.is_some() {
        Kind::Bates
    } else {
        Kind::HeaderFooter
    };
    let frames = draw::frames(doc);
    let pages = hf.pages.resolve(frames.len())?;
    if hf.slots.is_empty()
        && hf.first.as_ref().is_none_or(Slots::is_empty)
        && hf.even.as_ref().is_none_or(Slots::is_empty)
    {
        return Err(crate::Error::Empty("all slots are empty"));
    }
    let mut report = Report::default();
    if hf.replace {
        report.replaced = crate::remove(doc, &[kind], false)?.removed;
    }
    let labels = pagebara_pages::page_labels(doc, frames.len());
    let clock = hf.clock.unwrap_or_else(Clock::local);
    let first = pages[0] as i64;
    let total = frames.len() as i64 - first + hf.start - 1;
    let spec = serde_json::to_string(hf).unwrap_or_default();
    let mut ts = Typesetter::new(doc, Some(pagebara_typeset::system_fonts()));
    let group = hf
        .group
        .as_deref()
        .filter(|g| !g.trim().is_empty())
        .map(|g| oc::group(doc, g.trim(), kind.marker(), hf.visibility));
    let size = f64::from(hf.style.size.max(1.0));
    let m = hf.margins;
    let mut last_bates = None;
    for (n, &i) in pages.iter().enumerate() {
        let f = &frames[i];
        let number = i as i64 - first + hf.start;
        let bates = hf.bates.as_ref().map(|b| {
            let v = b.start + n as u64;
            last_bates = Some(v);
            b.format(v)
        });
        let label = labels
            .as_ref()
            .and_then(|l| l.get(i).cloned())
            .unwrap_or_else(|| (i + 1).to_string());
        let v = Values {
            page: number,
            pages: total,
            label: &label,
            bates: bates.as_deref().unwrap_or(""),
            clock,
            info,
        };
        let slots = match (&hf.first, &hf.even) {
            (Some(s), _) if n == 0 => s,
            (_, Some(s)) if (i + 1) % 2 == 0 => s,
            _ => &hf.slots,
        };
        let mut c = String::new();
        for (half, subtype) in [(0..3, "Header"), (3..6, "Footer")] {
            let mut part = String::new();
            for slot in half {
                let text = expand(&slots.0[slot], hf, &v);
                if text.trim().is_empty() {
                    continue;
                }
                let lines: Vec<Line> = text
                    .lines()
                    .map(|l| ts.line(doc, &hf.style.font, size, l))
                    .collect();
                let lh = size * 1.2;
                let count = lines.len();
                for (k, l) in lines.iter().enumerate() {
                    let x = match slot % 3 {
                        0 => f64::from(m.left),
                        1 => (f.w - l.width) / 2.0,
                        _ => f.w - f64::from(m.right) - l.width,
                    };
                    let y = if slot < 3 {
                        f.h - f64::from(m.top) - l.ascent - k as f64 * lh
                    } else {
                        f64::from(m.bottom) - l.descent + (count - 1 - k) as f64 * lh
                    };
                    if let Some(bg) = hf.style.background {
                        let pad = size * 0.25;
                        let _ = writeln!(
                            part,
                            "{} {} {} {} {} re f",
                            colour(bg, "rg"),
                            num(x - pad),
                            num(y + l.descent - pad),
                            num(l.width + 2.0 * pad),
                            num(l.ascent - l.descent + 2.0 * pad)
                        );
                    }
                    let _ = writeln!(
                        part,
                        "BT {} 1 0 0 1 {} {} Tm {}ET",
                        colour(hf.style.color, "rg"),
                        num(x),
                        num(y),
                        l.ops
                    );
                }
            }
            if !part.is_empty() {
                c.push_str(&draw::artifact(subtype));
                c.push_str(&part);
                c.push_str("EMC\n");
            }
        }
        if c.is_empty() {
            continue;
        }
        let mut res = Dict::new();
        res.set(b"Font", ts.resources());
        let form = draw::form(
            kind,
            [0.0, 0.0, f.w, f.h],
            res,
            c.as_bytes(),
            (n == 0).then_some(spec.as_str()),
        );
        let form = doc.add(form);
        let name = format!("{}{}", kind.prefix(), form.num);
        let body = format!("q {} /{name} Do Q\n", draw::cm(f.m));
        draw::add(
            doc,
            f.r,
            &PageStamp {
                kind,
                behind: false,
                xobjects: &[(name, form)],
                group,
                body: &body,
            },
        );
        report.pages.push(i);
    }
    report.missing = ts.finish(doc);
    Ok((report, last_bates))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        let hf = HeaderFooter::default();
        let info = DocInfo {
            file_name: "a.pdf".into(),
            title: "T".into(),
            author: "Ann".into(),
        };
        let v = Values {
            page: 3,
            pages: 9,
            label: "iii",
            bates: "X-0001",
            clock: Clock {
                secs: 0,
                offset_minutes: 0,
            },
            info: &info,
        };
        assert_eq!(
            expand("{page}/{pages} {label} {file} {title} {author}", &hf, &v),
            "3/9 iii a.pdf T Ann"
        );
        assert_eq!(
            expand("{date} {time:%H.%M} {{x}}", &hf, &v),
            "1970-01-01 00.00 {x}"
        );
        assert_eq!(expand("{bates} {nope} {", &hf, &v), "X-0001 {nope} {");
    }
}
