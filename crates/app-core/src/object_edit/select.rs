//! The selected objects of a page, what they are called, and finding them again in the
//! page's objects after an edit.

use pagebara_edit::{
    Kind, Matrix, ObjectData, ObjectId, PageObject, PageObjects, Point, Rect, Target,
};
use pagebara_i18n::{tr, trn};

/// Objects selected on one page, in the order they were picked.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    pub page: usize,
    pub ids: Vec<ObjectId>,
}

impl Selection {
    pub fn new(page: usize, ids: Vec<ObjectId>) -> Self {
        Selection { page, ids }
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    pub fn targets(&self) -> Vec<Target> {
        self.ids.iter().map(|&i| Target::Object(i)).collect()
    }

    /// Adds `id`, or removes it when it is selected already.
    pub fn toggle(&mut self, id: ObjectId) {
        match self.ids.iter().position(|&i| i == id) {
            Some(k) => {
                self.ids.remove(k);
            }
            None => self.ids.push(id),
        }
    }

    pub fn contains(&self, id: ObjectId) -> bool {
        self.ids.contains(&id)
    }

    /// The page-space bounds of everything selected.
    pub fn bbox(&self, objs: &PageObjects) -> Option<Rect> {
        self.ids
            .iter()
            .filter_map(|&i| objs.get(i))
            .map(|o| o.bbox)
            .reduce(|a, b| a.union(&b))
    }

    /// The selected objects that still exist.
    pub fn objects<'a>(&'a self, objs: &'a PageObjects) -> impl Iterator<Item = &'a PageObject> {
        self.ids.iter().filter_map(|&i| objs.get(i))
    }

    /// The one text block selected, if that is the selection.
    pub fn single_text<'a>(&self, objs: &'a PageObjects) -> Option<&'a PageObject> {
        match &self.ids[..] {
            [id] => objs.get(*id).filter(|o| o.kind == Kind::Text),
            _ => None,
        }
    }

    pub fn single_image<'a>(&self, objs: &'a PageObjects) -> Option<&'a PageObject> {
        match &self.ids[..] {
            [id] => objs.get(*id).filter(|o| o.kind == Kind::Image),
            _ => None,
        }
    }
}

/// A short name of an object's kind, for menus and screen readers.
pub fn kind_name(kind: Kind) -> String {
    match kind {
        Kind::Text => tr!("Text"),
        Kind::Image => tr!("Image"),
        Kind::Path => tr!("Drawing"),
        // TRANSLATORS: noun, a group of page objects
        Kind::Form => tr!("Group"),
        Kind::Shading => tr!("Shading"),
    }
}

/// [`kind_name`] in lower case, inside a sentence.
fn kind_word(kind: Kind) -> String {
    match kind {
        // TRANSLATORS: kinds of page objects, as in "3 objects selected (text, image)"
        Kind::Text => tr!("text"),
        Kind::Image => tr!("image"),
        Kind::Path => tr!("drawing"),
        Kind::Form => tr!("group"),
        Kind::Shading => tr!("shading"),
    }
}

/// An object as a screen reader says it: its kind, its text or pixel size, and its size on
/// the page.
pub fn describe(objs: &PageObjects, ob: &PageObject) -> String {
    let size = |r: &Rect| {
        let (w, h) = (format!("{:.0}", r.width()), format!("{:.0}", r.height()));
        tr!("{} by {} points", w, h)
    };
    match &ob.data {
        ObjectData::Text(t) => {
            let mut text: String = t.text.split_whitespace().collect::<Vec<_>>().join(" ");
            if text.chars().count() > 60 {
                text = text.chars().take(57).collect::<String>() + "…";
            }
            let font = t
                .runs()
                .next()
                .and_then(|r| objs.run(r))
                .map(|r| {
                    let pt = format!("{:.0}", r.size * scale_of(&r.matrix()));
                    format!(", {}", tr!("{} point", pt))
                })
                .unwrap_or_default();
            tr!("Text “{}”", text) + &font
        }
        ObjectData::Image(i) => tr!(
            "Image, {} by {} pixels, {}",
            i.width,
            i.height,
            size(&ob.bbox)
        ),
        ObjectData::Path(_) => tr!("Drawing, {}", size(&ob.bbox)),
        ObjectData::Form(f) => trn!(
            "Group of {n} object, {}",
            "Group of {n} objects, {}",
            f.children.len(),
            size(&ob.bbox)
        ),
        ObjectData::Shading(_) => tr!("Shading, {}", size(&ob.bbox)),
    }
}

/// The selection as a sentence: one object described, or how many of which kinds.
pub fn describe_selection(objs: &PageObjects, sel: &Selection) -> String {
    let obs: Vec<&PageObject> = sel.objects(objs).collect();
    match &obs[..] {
        [] => tr!("Nothing selected"),
        [one] => describe(objs, one),
        many => {
            let mut kinds: Vec<String> = many.iter().map(|o| kind_word(o.kind)).collect();
            kinds.sort_unstable();
            kinds.dedup();
            trn!(
                "{n} object selected ({})",
                "{n} objects selected ({})",
                many.len(),
                kinds.join(", ")
            )
        }
    }
}

/// How much a matrix scales lengths (the square root of its area scale).
pub fn scale_of(m: &Matrix) -> f32 {
    m.determinant().abs().sqrt()
}

/// The objects of `new` that are `ids` of `old` after `m` (a page-space matrix) moved
/// them: same kind, nearest bounds. Unmatched ones are left out.
pub fn refind(old: &PageObjects, new: &PageObjects, ids: &[ObjectId], m: &Matrix) -> Vec<ObjectId> {
    let mut taken: Vec<ObjectId> = Vec::new();
    for &id in ids {
        let Some(o) = old.get(id) else { continue };
        let want = o.bbox.transform(m);
        let tol = (want.width().max(want.height()) * 0.5).max(6.0);
        let best = new
            .top_level()
            .filter(|n| n.kind == o.kind && !taken.contains(&n.id))
            .map(|n| (distance(&n.bbox, &want), n.id))
            .filter(|(d, _)| *d <= tol)
            .min_by(|a, b| a.0.total_cmp(&b.0));
        if let Some((_, nid)) = best {
            taken.push(nid);
        }
    }
    taken
}

/// The largest distance between matching corners of two rectangles.
fn distance(a: &Rect, b: &Rect) -> f32 {
    [a.x0 - b.x0, a.y0 - b.y0, a.x1 - b.x1, a.y1 - b.y1]
        .iter()
        .fold(0.0f32, |m, v| m.max(v.abs()))
}

/// The objects on the page in Tab order: top to bottom, then left to right (in page
/// space, so as read on an unrotated page), hidden ones left out.
pub fn tab_order(objs: &PageObjects) -> Vec<ObjectId> {
    let mut v: Vec<&PageObject> = objs.top_level().filter(|o| !o.hidden).collect();
    let key = |o: &PageObject| {
        let p = objs.frame.view_point(Point::new(o.bbox.x0, o.bbox.y1));
        ((p.y / 4.0).round() as i64, (p.x * 10.0) as i64)
    };
    v.sort_by_key(|o| key(o));
    v.into_iter().map(|o| o.id).collect()
}

/// Whether an image or form is drawn mirrored (its own frame flipped on the page).
pub fn mirrored(ob: &PageObject) -> bool {
    let [a, b, _, d] = ob.quad;
    (b.x - a.x) * (d.y - a.y) - (b.y - a.y) * (d.x - a.x) < 0.0
}

/// The view-space centre of object `n` (1-based, painting order, top-level objects) of
/// page `page`, for scripts.
pub fn object_center(
    doc: &pagebara_syntax::Document,
    page: usize,
    n: usize,
) -> Result<(f64, f64), String> {
    let objs = pagebara_edit::parse(doc, page).map_err(|e| e.to_string())?;
    let ob = objs
        .top_level()
        .nth(n.saturating_sub(1))
        .ok_or_else(|| format!("page {} has no object {n}", page + 1))?;
    let r = objs.frame.view_rect(ob.bbox);
    Ok((f64::from(r.x0 + r.x1) / 2.0, f64::from(r.y0 + r.y1) / 2.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn distance_is_the_worst_corner() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(1.0, 0.0, 10.0, 13.0);
        assert_eq!(distance(&a, &b), 3.0);
    }
}
