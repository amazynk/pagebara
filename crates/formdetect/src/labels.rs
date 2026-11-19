//! Labels: the text a field belongs to. Left of text fields on the same line (else above,
//! else below a signature line), right of check boxes.

use pagebara_annot::Rect;

use crate::geometry::{Area, Geometry};

/// Words further apart than this are not one label.
const GAP: f64 = 12.0;

fn clean(s: &str) -> Option<String> {
    let t = s
        .trim()
        .trim_end_matches(|c: char| c == ':' || c == '_' || c == '*' || c.is_whitespace())
        .trim_start_matches(|c: char| c == '_' || c.is_whitespace())
        .to_owned();
    (!t.is_empty() && t.chars().any(char::is_alphanumeric)).then_some(t)
}

fn centre_y(r: &Rect) -> f64 {
    (r.y0 + r.y1) / 2.0
}

/// Joins words left to right while they are close.
fn join(mut words: Vec<(Rect, String)>) -> String {
    words.sort_by(|a, b| a.0.x0.total_cmp(&b.0.x0));
    words
        .iter()
        .map(|(_, t)| t.as_str())
        .collect::<Vec<_>>()
        .join(" ")
}

pub fn label_for(g: &Geometry, area: &Area) -> Option<String> {
    let r = area.rect();
    let words: Vec<(Rect, String, usize)> = g
        .words_near()
        .map(|(b, t, l)| (b, t.to_owned(), l))
        .filter(|(_, t, _)| !t.trim().is_empty())
        .collect();
    if let Area::Check(_) = area {
        let mut right: Vec<&(Rect, String, usize)> = words
            .iter()
            .filter(|(b, _, _)| {
                let c = centre_y(b);
                c >= r.y0 - 3.0 && c <= r.y1 + 3.0 && b.x0 >= r.x1 - 1.0
            })
            .collect();
        right.sort_by(|a, b| a.0.x0.total_cmp(&b.0.x0));
        let first = right.first().filter(|w| w.0.x0 <= r.x1 + 25.0)?;
        let mut picked = vec![(first.0, first.1.clone())];
        let mut x = first.0.x1;
        for w in right.iter().skip(1) {
            if w.0.x0 - x > GAP {
                break;
            }
            picked.push((w.0, w.1.clone()));
            x = w.0.x1;
        }
        return clean(&join(picked));
    }
    let band = (r.y0 - 4.0, r.y0 + r.height().min(14.0));
    // Same line, to the left (a word running into underscores counts up to them).
    let mut left: Vec<(Rect, String)> = words
        .iter()
        .filter(|(b, _, _)| {
            let c = centre_y(b);
            c >= band.0 && c <= band.1 && b.x0 < r.x0 - 1.0 && b.x0 >= r.x0 - 300.0
        })
        .filter_map(|(b, t, _)| {
            if b.x1 <= r.x0 + 2.0 {
                Some((*b, t.clone()))
            } else {
                let before = t.split('_').next().unwrap_or_default();
                (!before.trim().is_empty()).then(|| (*b, before.to_owned()))
            }
        })
        .collect();
    left.sort_by(|a, b| b.0.x1.total_cmp(&a.0.x1));
    if let Some(first) = left.first().filter(|w| r.x0 - w.0.x1 <= 60.0) {
        let mut picked = vec![first.clone()];
        let mut x = first.0.x0;
        for w in left.iter().skip(1) {
            if x - w.0.x1 > GAP || w.1.ends_with(':') {
                break;
            }
            picked.push(w.clone());
            x = w.0.x0;
        }
        if let Some(l) = clean(&join(picked)) {
            return Some(l);
        }
    }
    let overlaps = |b: &Rect| b.x1 > r.x0 && b.x0 < r.x1;
    let nearest_line = |pick: &dyn Fn(&Rect) -> Option<f64>| -> Option<String> {
        let (_, line) = words
            .iter()
            .filter(|(b, _, _)| overlaps(b))
            .filter_map(|(b, _, l)| pick(b).map(|d| (d, *l)))
            .min_by(|a, b| a.0.total_cmp(&b.0))?;
        let picked: Vec<(Rect, String)> = words
            .iter()
            .filter(|(b, _, l)| *l == line && overlaps(b))
            .map(|(b, t, _)| (*b, t.clone()))
            .collect();
        clean(&join(picked))
    };
    if let Area::Line(_) = area {
        // Below a line first: "Signature" printed under it.
        if let Some(l) = nearest_line(&|b: &Rect| {
            let d = r.y0 - b.y1;
            (0.0..=16.0).contains(&d).then_some(d)
        }) {
            return Some(l);
        }
    }
    nearest_line(&|b: &Rect| {
        let d = b.y0 - r.y1;
        (-2.0..=16.0).contains(&d).then_some(d)
    })
}
