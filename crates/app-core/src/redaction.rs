//! Redaction as the app offers it: what to look for, how marks look, the text a mark
//! covers (to check afterwards that it is gone) and the verification report in words.

use pagebara_content::TextPage;
use pagebara_i18n::{tr, tr_noop, trn};
use pagebara_redact::{Kind, Mark, Pattern, Point, Rect, Sanitize, VerifyReport};

/// What "Find & mark" looks for, in the order the panel lists them (shown through
/// [`pagebara_i18n::translate`]).
pub const FIND_KINDS: [&str; 6] = [
    // TRANSLATORS: what redaction searches for: plain text
    tr_noop!("Text"),
    // TRANSLATORS: what redaction searches for: a regular expression
    tr_noop!("Pattern"),
    tr_noop!("E-mail addresses"),
    tr_noop!("Phone numbers"),
    tr_noop!("IBANs"),
    tr_noop!("Card numbers"),
];

/// The search for the panel's choice `kind` (an index into [`FIND_KINDS`]).
pub fn pattern(kind: usize, text: &str, case: bool, whole_word: bool) -> Result<Pattern, String> {
    Ok(match kind {
        0 => {
            if text.trim().is_empty() {
                return Err(tr!("Enter the text to find."));
            }
            Pattern::Text {
                text: text.to_owned(),
                case_sensitive: case,
                whole_word,
            }
        }
        1 => {
            if text.is_empty() {
                return Err(tr!("Enter a regular expression."));
            }
            let re = if case {
                text.to_owned()
            } else {
                format!("(?i){text}")
            };
            Pattern::regex(&re).map_err(|e| tr!("That pattern is not valid: {}", e))?
        }
        2 => Pattern::Kind(Kind::Email),
        3 => Pattern::Kind(Kind::Phone),
        4 => Pattern::Kind(Kind::Iban),
        _ => Pattern::Kind(Kind::CreditCard),
    })
}

/// Fills a mark can have once applied: black, white, none.
pub const FILLS: [Option<[f64; 3]>; 3] = [Some([0.0; 3]), Some([1.0; 3]), None];

/// The look of new marks: fill (an index into [`FILLS`]) and overlay text.
pub fn styled(mut m: Mark, fill: usize, overlay: &str) -> Mark {
    m.fill = FILLS.get(fill).copied().unwrap_or(FILLS[0]);
    let overlay = overlay.trim();
    m.overlay_text = (!overlay.is_empty()).then(|| overlay.to_owned());
    // Overlay text in the colour that shows on the fill.
    m.text_color = if m.fill == Some([1.0; 3]) || m.fill.is_none() {
        [0.0; 3]
    } else {
        [1.0; 3]
    };
    m
}

/// The fill index of a mark read back.
pub fn fill_index(m: &Mark) -> usize {
    FILLS.iter().position(|f| *f == m.fill).unwrap_or(0)
}

/// A mark over text boxes `[x0, y0, x1, y1]` in user space.
pub fn mark_boxes(boxes: &[[f32; 4]]) -> Mark {
    Mark::quads(
        boxes
            .iter()
            .map(|b| {
                Rect::new(
                    f64::from(b[0]),
                    f64::from(b[1]),
                    f64::from(b[2]),
                    f64::from(b[3]),
                )
                .corners()
            })
            .collect(),
    )
}

/// The mark with the upright boxes of neighbouring glyphs on a line joined into one box
/// (search hits come a box per glyph); slanted quads stay as they are.
pub fn joined(mut m: Mark) -> Mark {
    let mut out: Vec<[Point; 4]> = Vec::new();
    let upright = |q: &[Point; 4]| {
        let r = Rect::from_points(q);
        q.iter()
            .all(|p| (p.x == r.x0 || p.x == r.x1) && (p.y == r.y0 || p.y == r.y1))
    };
    for q in m.quads {
        let r = Rect::from_points(&q);
        if let Some(last) = out.last_mut()
            && upright(&q)
            && upright(last)
        {
            let l = Rect::from_points(last);
            let same_line = (l.y0 - r.y0).abs() < 0.3 * l.height().max(1.0)
                && (l.y1 - r.y1).abs() < 0.3 * l.height().max(1.0);
            let near = r.x0 <= l.x1 + 0.5 * l.height() && r.x1 >= l.x0;
            if same_line && near {
                *last = Rect::new(
                    l.x0.min(r.x0),
                    l.y0.min(r.y0),
                    l.x1.max(r.x1),
                    l.y1.max(r.y1),
                )
                .corners();
                continue;
            }
        }
        out.push(q);
    }
    m.quads = out;
    m
}

/// A mark over a rectangle `[x0, y0, x1, y1]` in user space.
pub fn mark_rect(r: [f64; 4]) -> Mark {
    Mark::rect(Rect::new(
        r[0].min(r[2]),
        r[1].min(r[3]),
        r[0].max(r[2]),
        r[1].max(r[3]),
    ))
}

/// The text a mark covers: characters whose centre lies in one of its areas.
pub fn text_under(page: &TextPage, m: &Mark) -> String {
    let boxes: Vec<Rect> = m.quads.iter().map(|q| Rect::from_points(q)).collect();
    let mut out = String::new();
    for c in &page.chars {
        let b = c.bbox();
        let (x, y) = (f64::from(b.x0 + b.x1) / 2.0, f64::from(b.y0 + b.y1) / 2.0);
        if boxes
            .iter()
            .any(|r| x >= r.x0 && x <= r.x1 && y >= r.y0 && y <= r.y1)
        {
            out.push_str(page.char_text(c));
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Secrets worth checking for after applying: the covered texts long enough not to match
/// by chance elsewhere.
pub fn secrets(texts: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = texts
        .into_iter()
        .map(|t| t.trim().to_owned())
        .filter(|t| t.chars().filter(|c| c.is_alphanumeric()).count() >= 4)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The sanitising choices in the order the confirmation lists them (shown through
/// [`pagebara_i18n::translate`]).
pub const SANITIZE_LABELS: [&str; 8] = [
    tr_noop!("Metadata and document information"),
    tr_noop!("Scripts and actions"),
    tr_noop!("Attached files"),
    tr_noop!("Hidden layers"),
    tr_noop!("All comments"),
    tr_noop!("Page thumbnails"),
    tr_noop!("XFA form data"),
    tr_noop!("Unused images and resources"),
];

pub fn sanitize_flags(s: &Sanitize) -> [bool; 8] {
    [
        s.metadata,
        s.javascript,
        s.embedded_files,
        s.hidden_layers,
        s.comments,
        s.thumbnails,
        s.xfa,
        s.unused_resources,
    ]
}

pub fn sanitize_from(f: &[bool]) -> Sanitize {
    let g = |i: usize| f.get(i).copied().unwrap_or(false);
    Sanitize {
        metadata: g(0),
        javascript: g(1),
        embedded_files: g(2),
        hidden_layers: g(3),
        comments: g(4),
        thumbnails: g(5),
        xfa: g(6),
        unused_resources: g(7),
    }
}

/// The verification in words: whether it is clean, and one line per finding.
pub fn report_lines(v: &VerifyReport) -> (bool, Vec<String>) {
    let mut lines = Vec::new();
    for t in &v.text_in_regions {
        lines.push(tr!(
            "Page {}: text is still under a redaction: “{}”",
            t.page + 1,
            t.text
        ));
    }
    for i in &v.images_in_regions {
        lines.push(tr!(
            "Page {}: an image under a redaction {}",
            i.page + 1,
            i.reason
        ));
    }
    for a in &v.annotations_in_regions {
        // TRANSLATORS: the annotation's PDF type (like Highlight) fills the second {}
        lines.push(tr!(
            "Page {}: a {} annotation is still over a redaction",
            a.page + 1,
            a.subtype
        ));
    }
    for s in &v.secrets_found {
        let at = match s.location {
            pagebara_redact::Location::PageText { page } => {
                tr!(
                    "“{}” can still be found in the text of page {}",
                    s.secret,
                    page + 1
                )
            }
            pagebara_redact::Location::File { offset } => {
                tr!(
                    "“{}” can still be found in the file at byte {}",
                    s.secret,
                    offset
                )
            }
            pagebara_redact::Location::Object { num } => {
                tr!("“{}” can still be found in object {}", s.secret, num)
            }
            pagebara_redact::Location::Stream { num } => {
                tr!(
                    "“{}” can still be found in the data of object {}",
                    s.secret,
                    num
                )
            }
            pagebara_redact::Location::ContentText { num } => {
                tr!(
                    "“{}” can still be found in the content of object {}",
                    s.secret,
                    num
                )
            }
            pagebara_redact::Location::LooseStream { offset } => {
                tr!(
                    "“{}” can still be found in leftover data at byte {}",
                    s.secret,
                    offset
                )
            }
        };
        lines.push(at);
    }
    if v.revisions > 1 {
        lines.push(trn!(
            "The file has {n} revision; earlier ones may hold the original",
            "The file has {n} revisions; earlier ones may hold the original",
            v.revisions
        ));
    }
    lines.extend(v.errors.iter().map(|e| tr!("Not checked: {}", e)));
    (v.is_clean(), lines)
}

/// Corners of a box for a mark from points of a quad (handy for tests and overlays).
pub fn quad_bbox(q: &[Point; 4]) -> [f64; 4] {
    let r = Rect::from_points(q);
    [r.x0, r.y0, r.x1, r.y1]
}

#[cfg(test)]
mod tests;
