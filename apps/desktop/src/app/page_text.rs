//! The text of the pages on screen for screen readers: an invisible element per heading,
//! paragraph, list item, figure and link, in reading order and over where it is on the
//! page, so assistive technology reads the page and shows where it is.
//!
//! Slint (1.18.1, the newest release) has no heading, link or custom accessible roles, so
//! items are text whose description says what they are ("Heading level 2", "List item")
//! and links are buttons described as "Link to …" whose default action follows them.

use std::rc::Rc;

use pagebara_annot::{Kind, Point};
use pagebara_app_core::annots;
use pagebara_app_core::reading_order::{Item, ItemKind};
use pagebara_i18n::tr;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{App, State, post_act};
use crate::{AppWindow, PageTextItem, ViewState};

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let model = app.state.borrow().page_text.clone();
    ui.global::<ViewState>().set_page_text(ModelRc::from(model));
    let a = app.clone();
    ui.global::<ViewState>().on_page_link(move |page, i| {
        a.act(move |app, s, ui| follow(app, s, ui, page as usize, i as usize))
    });
}

/// Updates the elements for the pages placed now. Pages whose reading order is not known
/// yet are worked out on a worker (text extraction would hold up scrolling), and the
/// elements updated again when it is done.
pub fn rebuild(s: &mut State) {
    let Some(t) = s.tabs.get(s.active) else {
        return;
    };
    let readings = t.session.readings();
    let held = t.held;
    let mut missing = Vec::new();
    let mut out = Vec::new();
    for (i, pl) in &t.overlay_pages {
        let Some(g) = t.session.geoms.get(*i).copied() else {
            continue;
        };
        let Some(reading) = readings.cached(*i) else {
            missing.push(*i);
            continue;
        };
        let place = |r: [f32; 4]| {
            let [x, y, w, h] = g.rect_to_view(r.map(f64::from));
            (
                pl.bx + x as f32,
                pl.by + y as f32,
                w.max(1.0) as f32,
                h.max(1.0) as f32,
            )
        };
        let boxes: Vec<[f32; 4]> = reading.items.iter().map(bbox).collect();
        let mut page_items: Vec<PageTextItem> = reading
            .items
            .iter()
            .zip(&boxes)
            .map(|(item, b)| {
                let (bx, by, w, h) = place(*b);
                let (label, description) = describe(item);
                PageTextItem {
                    di: pl.di,
                    by,
                    bx,
                    w,
                    h,
                    page: *i as i32,
                    link: -1,
                    label: label.into(),
                    description: description.into(),
                }
            })
            .collect();
        let links: Vec<_> = reading
            .links
            .iter()
            .map(|l| (l.rect, l.text.as_str()))
            .collect();
        // Back to front, so the indices of the items still to place hold.
        let mut placed = link_places(&reading.items, &boxes, &links);
        placed.sort_by_key(|&(n, at, replaces)| std::cmp::Reverse((at, replaces, n)));
        for (n, at, replaces) in placed {
            let l = &reading.links[n];
            let (bx, by, w, h) = place(l.rect);
            let item = PageTextItem {
                di: pl.di,
                by,
                bx,
                w,
                h,
                page: *i as i32,
                link: n as i32,
                label: if l.text.is_empty() {
                    tr!("Link").into()
                } else {
                    l.text.as_str().into()
                },
                description: tr!("Link to {}", l.target).into(),
            };
            if replaces {
                page_items[at] = item;
            } else {
                page_items.insert(at.min(page_items.len()), item);
            }
        }
        out.extend(page_items);
    }
    if !missing.is_empty() && !s.page_text_busy && !held {
        s.page_text_busy = true;
        pagebara_thread::spawn("a11y-text", move || {
            for i in missing {
                readings.page(i);
            }
            post_act(|_app, s, _ui| {
                s.page_text_busy = false;
                rebuild(s);
            });
        });
    }
    replace_rows(&s.page_text, out);
}

/// Gives `model` the rows `rows` in place: the elements of rows that stay the same are
/// kept, and changed ones updated rather than made again (a page has hundreds).
fn replace_rows<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    let n = rows.len();
    for (i, row) in rows.into_iter().enumerate() {
        match model.row_data(i) {
            Some(old) if old == row => {}
            Some(_) => model.set_row_data(i, row),
            None => model.push(row),
        }
    }
    while model.row_count() > n {
        model.remove(model.row_count() - 1);
    }
}

/// Where each link goes among the items read before it: `(link, index, replaces)`. A link
/// whose text is a whole item takes its place, so it is read once, as a link; another goes
/// after the item it overlaps most; one overlapping none goes last.
fn link_places(
    items: &[Item],
    boxes: &[[f32; 4]],
    links: &[([f32; 4], &str)],
) -> Vec<(usize, usize, bool)> {
    let overlap = |a: [f32; 4], b: [f32; 4]| {
        let w = a[2].min(b[2]) - a[0].max(b[0]);
        let h = a[3].min(b[3]) - a[1].max(b[1]);
        if w > 0.0 && h > 0.0 { w * h } else { 0.0 }
    };
    let mut taken = vec![false; items.len()];
    links
        .iter()
        .enumerate()
        .map(|(n, &(rect, text))| {
            let best = boxes
                .iter()
                .enumerate()
                .map(|(k, b)| (k, overlap(rect, *b)))
                .filter(|&(_, a)| a > 0.0)
                .max_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(k, _)| k);
            match best {
                Some(k) if !taken[k] && !text.is_empty() && items[k].text.trim() == text.trim() => {
                    taken[k] = true;
                    (n, k, true)
                }
                Some(k) => (n, k + 1, false),
                None => (n, items.len(), false),
            }
        })
        .collect()
}

fn bbox(item: &Item) -> [f32; 4] {
    let mut b = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for (_, q) in &item.chars {
        for p in q {
            b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
        }
    }
    if b[0].is_finite() {
        b
    } else {
        [0.0, 0.0, 1.0, 1.0]
    }
}

/// What a screen reader says for an item, and what kind of item it is.
fn describe(item: &Item) -> (String, String) {
    let kind = match item.kind {
        ItemKind::Heading(n) => tr!("Heading level {}", n),
        ItemKind::Paragraph => String::new(),
        ItemKind::ListItem => tr!("List item"),
        ItemKind::Figure => tr!("Figure"),
        ItemKind::Caption => tr!("Caption"),
        ItemKind::Cell => tr!("Table cell"),
        ItemKind::Quote => tr!("Quotation"),
        // TRANSLATORS: a footnote or endnote in the document's structure, read by screen readers
        ItemKind::Note => tr!("Note"),
        // TRANSLATORS: computer code in the document's structure, read by screen readers
        ItemKind::Code => tr!("Code"),
    };
    let label = match item.kind {
        ItemKind::Figure if item.text.is_empty() => tr!("Figure without a description"),
        _ => item.text.clone(),
    };
    let kind = match &item.lang {
        Some(l) if !kind.is_empty() => tr!("{}, language {}", kind, l),
        _ => kind,
    };
    (label, kind)
}

/// Follows link `i` of `page` (from a screen reader's default action).
fn follow(app: &Rc<App>, s: &mut State, ui: &AppWindow, page: usize, i: usize) {
    let Some(t) = s.tab() else { return };
    let Some(l) = t.session.reading(page).links.get(i).cloned() else {
        return;
    };
    let c = Point::new(
        f64::from(l.rect[0] + l.rect[2]) / 2.0,
        f64::from(l.rect[1] + l.rect[3]) / 2.0,
    );
    let list = t.annots(page);
    let action = annots::link_at(&list, c).and_then(|a| match &a.kind {
        Kind::Link { action, .. } => action.clone(),
        _ => None,
    });
    if let Some(action) = action {
        app.follow_link(s, ui, page, action);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagebara_app_core::reading_order::ItemKind;

    fn item(text: &str) -> Item {
        Item {
            kind: ItemKind::Paragraph,
            text: text.into(),
            lang: None,
            chars: Vec::new(),
        }
    }

    #[test]
    fn links_go_where_they_are_read() {
        let items = [
            item("Intro"),
            item("Go on"),
            item("See the appendix for more"),
        ];
        let boxes = [
            [0.0, 90.0, 100.0, 100.0],
            [0.0, 70.0, 100.0, 80.0],
            [0.0, 50.0, 100.0, 60.0],
        ];
        let links = [
            ([10.0, 52.0, 40.0, 58.0], "appendix"),
            ([0.0, 70.0, 50.0, 80.0], "Go on"),
            ([200.0, 0.0, 210.0, 10.0], ""),
        ];
        assert_eq!(
            link_places(&items, &boxes, &links),
            [(0, 3, false), (1, 1, true), (2, 3, false)]
        );
    }
}
