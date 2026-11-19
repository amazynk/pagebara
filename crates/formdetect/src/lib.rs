//! Automatic field detection: where a printed form wants to be filled in, from its page
//! content. Lines and runs of underscores become text fields (signature and date fields
//! when their label says so), boxes become text fields (multiline when tall), rows of
//! small equal boxes comb fields, and small squares check boxes; each is named after the
//! text beside it. The results are suggestions for the user to accept.

mod geometry;
mod labels;

use pagebara_annot::Rect;
use pagebara_forms::{FieldKind, Form, Format, NewField};
use pagebara_syntax::Document;

use geometry::{Area, Geometry};

/// A field the page seems to ask for.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub page: usize,
    pub kind: FieldKind,
    /// In user space.
    pub rect: Rect,
    /// A field name from the label (not yet checked against the form's names).
    pub name: String,
    /// The text next to the field, for the tooltip.
    pub label: Option<String>,
    /// Cells of a comb field.
    pub comb: Option<usize>,
    pub multiline: bool,
    /// The label asks for a date.
    pub date: bool,
}

/// Date format of detected date fields.
pub const DATE_FORMAT: &str = "yyyy-mm-dd";

impl Suggestion {
    /// The field to create for this suggestion.
    pub fn to_new_field(&self) -> NewField {
        let mut nf = NewField::new(self.kind, self.page, self.rect);
        nf.name = self.name.clone();
        if let Some(l) = &self.label {
            nf.props.tooltip = l.clone();
        }
        nf.props.multiline = self.multiline;
        if let Some(n) = self.comb {
            nf.props.comb = true;
            nf.props.max_len = Some(n);
        }
        if self.date {
            nf.props.format = Format::Date(DATE_FORMAT.into());
        }
        if self.kind == FieldKind::Text && !self.multiline {
            // Detected lines are thin: no box around the typed text.
            nf.props.border_color = None;
            nf.props.border_width = 0.0;
        }
        nf
    }
}

/// Suggestions for every page.
pub fn detect_all(doc: &Document) -> Vec<Suggestion> {
    let n = pagebara_pages::PageTree::load(doc).pages().len();
    (0..n).flat_map(|p| detect(doc, p)).collect()
}

/// Suggestions for page `page` (0-based), top to bottom, left to right; places that
/// already have a field are left out.
pub fn detect(doc: &Document, page: usize) -> Vec<Suggestion> {
    let Some(g) = Geometry::read(doc, page) else {
        return Vec::new();
    };
    let form = Form::load(doc);
    let taken: Vec<Rect> = form
        .fields_on_page(page)
        .iter()
        .flat_map(|f| &f.widgets)
        .filter(|w| w.page == Some(page))
        .map(|w| w.rect)
        .collect();
    let mut out: Vec<Suggestion> = Vec::new();
    for area in g.areas() {
        let rect = area.rect();
        if taken.iter().any(|t| overlap(t, &rect) > 0.5)
            || out.iter().any(|s| overlap(&s.rect, &rect) > 0.3)
        {
            continue;
        }
        let label = labels::label_for(&g, &area);
        let lower = label.as_deref().unwrap_or_default().to_lowercase();
        let (kind, multiline, comb) = match area {
            Area::Check(_) => (FieldKind::CheckBox, false, None),
            Area::Comb(_, n) => (FieldKind::Text, false, Some(n)),
            Area::Box(r) if r.height() > 40.0 => (FieldKind::Text, true, None),
            _ if lower.contains("signature") || lower.starts_with("sign") => {
                (FieldKind::Signature, false, None)
            }
            _ => (FieldKind::Text, false, None),
        };
        let rect = match (kind, area) {
            // A signature on a line gets room to sign in, as far as the text above allows.
            (FieldKind::Signature, Area::Line(r)) => g.room_above(r, 36.0),
            _ => rect,
        };
        let date = kind == FieldKind::Text && comb.is_none() && !multiline && is_date(&lower);
        let base = match kind {
            FieldKind::CheckBox => "CheckBox",
            FieldKind::Signature => "Signature",
            _ if date => "Date",
            _ => "Text",
        };
        out.push(Suggestion {
            page,
            kind,
            rect,
            name: label
                .as_deref()
                .map(name_from)
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| format!("{base}1")),
            label,
            comb,
            multiline,
            date,
        });
    }
    out.sort_by(|a, b| {
        ((b.rect.y1 / 6.0).round() as i64)
            .cmp(&((a.rect.y1 / 6.0).round() as i64))
            .then(a.rect.x0.total_cmp(&b.rect.x0))
    });
    out
}

/// Creates fields for `list`, with names made unique. Returns the names given.
pub fn accept(
    form: &mut Form,
    doc: &mut Document,
    list: &[Suggestion],
) -> pagebara_forms::Result<Vec<String>> {
    let mut names = Vec::new();
    for s in list {
        let mut nf = s.to_new_field();
        nf.name = form.unique_name(&nf.name);
        names.push(form.add_field(doc, &nf)?.1);
    }
    Ok(names)
}

fn is_date(label: &str) -> bool {
    label.split(|c: char| !c.is_alphanumeric()).any(|w| {
        matches!(
            w,
            "date" | "dob" | "datum" | "fecha" | "data" | "birthday" | "born"
        )
    })
}

/// A field name from a label: letters, digits and single spaces, at most 40 characters.
pub fn name_from(label: &str) -> String {
    let cleaned: String = label
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                ' '
            }
        })
        .collect();
    let mut name = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    if let Some((i, _)) = name.char_indices().nth(40) {
        name.truncate(i);
    }
    name.trim().to_owned()
}

/// The part of the smaller rectangle covered by the other.
fn overlap(a: &Rect, b: &Rect) -> f64 {
    let w = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let h = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    let small = (a.width() * a.height())
        .min(b.width() * b.height())
        .max(1e-6);
    w * h / small
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_from_labels() {
        assert_eq!(name_from("First name:"), "First name");
        assert_eq!(
            name_from("  E-mail / phone (optional) "),
            "E-mail phone optional"
        );
        assert_eq!(name_from("…"), "");
        assert!(is_date("date of birth"));
        assert!(!is_date("update"));
    }
}
