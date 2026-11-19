//! Preparing forms in the apps: field types as the toolbars offer them, the size a new
//! field gets, widgets moved by the Edit tool's drag matrices, aligning, distributing and
//! sizing several widgets, and suggested fields from page content.
//!
//! Rectangles are x0, y0, x1, y1 in user space.

use pagebara_edit::{Matrix, Point};
use pagebara_i18n::tr;
use pagebara_syntax::Document;

pub use pagebara_formdetect::{DATE_FORMAT, Suggestion};

use crate::forms::FieldKind;

/// What the Prepare form toolbar creates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tool {
    Text,
    Date,
    CheckBox,
    Radio,
    ComboBox,
    ListBox,
    Button,
    Signature,
}

impl Tool {
    pub const ALL: [Tool; 8] = [
        Tool::Text,
        Tool::Date,
        Tool::CheckBox,
        Tool::Radio,
        Tool::ComboBox,
        Tool::ListBox,
        Tool::Button,
        Tool::Signature,
    ];

    pub fn kind(self) -> FieldKind {
        match self {
            Tool::Text | Tool::Date => FieldKind::Text,
            Tool::CheckBox => FieldKind::CheckBox,
            Tool::Radio => FieldKind::Radio,
            Tool::ComboBox => FieldKind::ComboBox,
            Tool::ListBox => FieldKind::ListBox,
            Tool::Button => FieldKind::PushButton,
            Tool::Signature => FieldKind::Signature,
        }
    }

    pub fn label(self) -> String {
        match self {
            Tool::Text => tr!("Text field"),
            Tool::Date => tr!("Date field"),
            Tool::CheckBox => tr!("Check box"),
            Tool::Radio => tr!("Radio button"),
            Tool::ComboBox => tr!("Drop-down list"),
            Tool::ListBox => tr!("List box"),
            Tool::Button => tr!("Button"),
            Tool::Signature => tr!("Signature field"),
        }
    }

    /// Width and height of a field placed with a click.
    pub fn size(self) -> (f64, f64) {
        match self {
            Tool::Text => (160.0, 22.0),
            Tool::Date => (100.0, 22.0),
            Tool::CheckBox | Tool::Radio => (14.0, 14.0),
            Tool::ComboBox => (140.0, 22.0),
            Tool::ListBox => (140.0, 60.0),
            Tool::Button => (90.0, 24.0),
            Tool::Signature => (180.0, 44.0),
        }
    }

    /// A rectangle of the default size with its top left corner at `x, y`.
    pub fn rect_at(self, x: f64, y: f64) -> [f64; 4] {
        let (w, h) = self.size();
        [x, y - h, x + w, y]
    }

    /// The new field's description, with options a list needs.
    pub fn new_field(self, page: usize, rect: [f64; 4]) -> crate::forms::NewField {
        let r = pagebara_annot::Rect::new(rect[0], rect[1], rect[2], rect[3]);
        let mut f = crate::forms::NewField::new(self.kind(), page, r);
        match self {
            Tool::Date => f.props.format = crate::forms::Format::Date(DATE_FORMAT.into()),
            Tool::ComboBox | Tool::ListBox => {
                f.props.options = (1..=3)
                    .map(|i| {
                        // TRANSLATORS: a placeholder entry of a new drop-down or list
                        let o = tr!("Option {}", i);
                        crate::forms::ChoiceOption {
                            export: o.clone(),
                            display: o,
                        }
                    })
                    .collect();
            }
            _ => {}
        }
        f
    }
}

/// How several widgets line up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Left,
    Centre,
    Right,
    Top,
    Middle,
    Bottom,
}

impl Align {
    pub fn parse(s: &str) -> Option<Align> {
        Some(match s {
            "left" => Align::Left,
            "centre" | "center" => Align::Centre,
            "right" => Align::Right,
            "top" => Align::Top,
            "middle" => Align::Middle,
            "bottom" => Align::Bottom,
            _ => return None,
        })
    }
}

/// `rects` lined up with the first one.
pub fn align(rects: &[[f64; 4]], how: Align) -> Vec<[f64; 4]> {
    let Some(&a) = rects.first() else {
        return Vec::new();
    };
    rects
        .iter()
        .map(|r| {
            let (w, h) = (r[2] - r[0], r[3] - r[1]);
            match how {
                Align::Left => [a[0], r[1], a[0] + w, r[3]],
                Align::Right => [a[2] - w, r[1], a[2], r[3]],
                Align::Centre => {
                    let c = (a[0] + a[2]) / 2.0;
                    [c - w / 2.0, r[1], c + w / 2.0, r[3]]
                }
                Align::Top => [r[0], a[3] - h, r[2], a[3]],
                Align::Bottom => [r[0], a[1], r[2], a[1] + h],
                Align::Middle => {
                    let c = (a[1] + a[3]) / 2.0;
                    [r[0], c - h / 2.0, r[2], c + h / 2.0]
                }
            }
        })
        .collect()
}

/// `rects` spread so the gaps between them are equal, the outermost staying put (in
/// the order given back).
pub fn distribute(rects: &[[f64; 4]], horizontal: bool) -> Vec<[f64; 4]> {
    let n = rects.len();
    if n < 3 {
        return rects.to_vec();
    }
    let (lo, hi) = if horizontal { (0, 2) } else { (1, 3) };
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| rects[a][lo].total_cmp(&rects[b][lo]));
    let first = rects[order[0]][lo];
    let last = order.iter().map(|&i| rects[i][hi]).fold(f64::MIN, f64::max);
    let total: f64 = rects.iter().map(|r| r[hi] - r[lo]).sum();
    let gap = (last - first - total) / (n - 1) as f64;
    let mut out = rects.to_vec();
    let mut at = first;
    for i in order {
        let size = rects[i][hi] - rects[i][lo];
        out[i][lo] = at;
        out[i][hi] = at + size;
        at += size + gap;
    }
    out
}

/// `rects` given the first one's width and/or height (keeping their top left corners).
pub fn same_size(rects: &[[f64; 4]], width: bool, height: bool) -> Vec<[f64; 4]> {
    let Some(&a) = rects.first() else {
        return Vec::new();
    };
    rects
        .iter()
        .map(|r| {
            let x1 = if width { r[0] + (a[2] - a[0]) } else { r[2] };
            let y0 = if height { r[3] - (a[3] - a[1]) } else { r[1] };
            [r[0], y0, x1, r[3]]
        })
        .collect()
}

/// A widget rectangle after a page-space move or resize matrix (its bounds; rotation is
/// not something widgets do).
pub fn transformed(r: [f64; 4], m: &Matrix) -> [f64; 4] {
    let a = m.apply(Point::new(r[0] as f32, r[1] as f32));
    let b = m.apply(Point::new(r[2] as f32, r[3] as f32));
    let (x0, x1) = (a.x.min(b.x), a.x.max(b.x));
    let (y0, y1) = (a.y.min(b.y), a.y.max(b.y));
    [
        f64::from(x0),
        f64::from(y0),
        f64::from(x1).max(f64::from(x0) + 4.0),
        f64::from(y1).max(f64::from(y0) + 4.0),
    ]
}

/// Fields the page content suggests, where the form has none yet.
pub fn detect(doc: &Document, page: usize) -> Vec<Suggestion> {
    pagebara_formdetect::detect(doc, page)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aligns_distributes_and_sizes() {
        let r = [
            [10.0, 10.0, 30.0, 20.0],
            [50.0, 40.0, 60.0, 60.0],
            [100.0, 0.0, 140.0, 5.0],
        ];
        assert_eq!(align(&r, Align::Left)[1], [10.0, 40.0, 20.0, 60.0]);
        assert_eq!(align(&r, Align::Top)[2], [100.0, 15.0, 140.0, 20.0]);
        assert_eq!(align(&r, Align::Middle)[1], [50.0, 5.0, 60.0, 25.0]);
        let d = distribute(&r, true);
        // Widths 20, 10, 40 between 10 and 140: gaps of 30.
        assert_eq!(d[1], [60.0, 40.0, 70.0, 60.0]);
        assert_eq!(d[2], r[2]);
        let s = same_size(&r, true, true);
        assert_eq!(s[1], [50.0, 50.0, 70.0, 60.0]);
        let m = Matrix::translate(5.0, -5.0);
        assert_eq!(transformed(r[0], &m), [15.0, 5.0, 35.0, 15.0]);
        assert_eq!(Tool::Date.rect_at(10.0, 100.0), [10.0, 78.0, 110.0, 100.0]);
    }
}
