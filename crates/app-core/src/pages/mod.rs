//! Pages as the viewer needs them: the page tree of `pagebara-pages` and the mapping between a
//! page's PDF user space and its displayed points (top-left origin, y down, rotation and
//! user unit applied).

use pagebara_pages::{Page, PageTree};
use pagebara_syntax::{Dict, Document, ObjRef};

/// Finds pages by object, for outline and link destinations.
pub trait PageSource {
    fn index_of(&self, r: ObjRef) -> Option<usize>;
}

impl PageSource for PageTree {
    fn index_of(&self, r: ObjRef) -> Option<usize> {
        PageTree::index_of(self, r)
    }
}

/// Displayed sizes of all pages, in points.
pub fn sizes(tree: &PageTree) -> Vec<(f64, f64)> {
    tree.pages().iter().map(Page::size).collect()
}

/// The transform between PDF user space and displayed page points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageGeom {
    /// The crop box: x0, y0, x1, y1.
    pub crop: [f64; 4],
    pub rotate: u16,
    pub unit: f64,
}

impl Default for PageGeom {
    fn default() -> Self {
        PageGeom {
            crop: [0.0, 0.0, 612.0, 792.0],
            rotate: 0,
            unit: 1.0,
        }
    }
}

impl PageGeom {
    pub fn of(p: &Page) -> Self {
        let c = p.crop_box;
        PageGeom {
            crop: [c.x0, c.y0, c.x1, c.y1],
            rotate: p.rotate,
            unit: p.user_unit,
        }
    }

    /// Unrotated width and height in points.
    fn raw(&self) -> (f64, f64) {
        let [x0, y0, x1, y1] = self.crop;
        ((x1 - x0) * self.unit, (y1 - y0) * self.unit)
    }

    /// Displayed width and height in points.
    pub fn size(&self) -> (f64, f64) {
        let (w, h) = self.raw();
        if self.rotate % 180 == 90 {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// User space to displayed points.
    pub fn to_view(&self, x: f64, y: f64) -> (f64, f64) {
        let (w, h) = self.raw();
        let dx = (x - self.crop[0]) * self.unit;
        let dy = (self.crop[3] - y) * self.unit;
        match self.rotate {
            90 => (h - dy, dx),
            180 => (w - dx, h - dy),
            270 => (dy, w - dx),
            _ => (dx, dy),
        }
    }

    /// Displayed points to user space.
    pub fn to_pdf(&self, u: f64, v: f64) -> (f64, f64) {
        let (w, h) = self.raw();
        let (dx, dy) = match self.rotate {
            90 => (v, h - u),
            180 => (w - u, h - v),
            270 => (w - v, u),
            _ => (u, v),
        };
        (self.crop[0] + dx / self.unit, self.crop[3] - dy / self.unit)
    }

    /// A user-space rectangle as a displayed one: x, y, w, h.
    pub fn rect_to_view(&self, [x0, y0, x1, y1]: [f64; 4]) -> [f64; 4] {
        let (a, b) = self.to_view(x0, y0);
        let (c, d) = self.to_view(x1, y1);
        [a.min(c), b.min(d), (a - c).abs(), (b - d).abs()]
    }

    /// A displayed rectangle (x, y, w, h) in user space: x0, y0, x1, y1.
    pub fn rect_to_pdf(&self, [x, y, w, h]: [f64; 4]) -> [f64; 4] {
        let (a, b) = self.to_pdf(x, y);
        let (c, d) = self.to_pdf(x + w, y + h);
        [a.min(c), b.min(d), a.max(c), b.max(d)]
    }
}

/// A page's resources: its own or inherited from the tree.
pub fn resources(doc: &Document, page: &Dict) -> Dict {
    let mut cur = page.clone();
    for _ in 0..64 {
        if let Some(r) = doc.get_in(&cur, b"Resources").as_dict() {
            return r.clone();
        }
        let parent = doc.get_in(&cur, b"Parent");
        let Some(p) = parent.as_dict() else { break };
        cur = p.clone();
    }
    Dict::new()
}

#[cfg(test)]
mod tests;
