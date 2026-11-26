//! The page tree: pages found on demand through `/Count`s, or by walking the whole tree.

use std::collections::HashSet;
use std::sync::{Arc, OnceLock};

use pagebara_syntax::xref::NumMap;
use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::rect::Rect;

/// Attributes a page may inherit from its ancestors (ISO 32000-2 table 31).
pub(crate) const INHERITABLE: [&[u8]; 4] = [b"Resources", b"MediaBox", b"CropBox", b"Rotate"];

/// Deeper trees are treated as damaged; real ones have a handful of levels.
const MAX_DEPTH: usize = 64;

/// Inheritable values accumulated along the path to a page, as written (possibly
/// references). Shared by all pages under the same node.
#[derive(Debug, Default)]
pub(crate) struct Inherited(pub [Option<Object>; 4]);

impl Inherited {
    fn extend(parent: &Arc<Inherited>, node: &Dict) -> Arc<Inherited> {
        if !INHERITABLE.iter().any(|k| node.contains(k)) {
            return parent.clone();
        }
        let mut v = parent.0.clone();
        for (slot, key) in v.iter_mut().zip(INHERITABLE) {
            if let Some(o) = node.get(key) {
                *slot = Some(o.clone());
            }
        }
        Arc::new(Inherited(v))
    }

    pub(crate) fn get(&self, key: &[u8]) -> Option<&Object> {
        let i = INHERITABLE.iter().position(|k| *k == key)?;
        self.0[i].as_ref()
    }
}

/// One page: its object and the attributes the app needs for layout.
#[derive(Clone, Debug)]
pub struct Page {
    pub r: ObjRef,
    pub media_box: Rect,
    /// The visible region: `/CropBox` clipped to the media box.
    pub crop_box: Rect,
    /// Clockwise display rotation: 0, 90, 180 or 270.
    pub rotate: u16,
    /// Size of a default user space unit in points (`/UserUnit`).
    pub user_unit: f64,
    pub(crate) inherited: Arc<Inherited>,
}

impl Page {
    /// Displayed width and height in points: crop box, user unit and rotation applied.
    pub fn size(&self) -> (f64, f64) {
        let (w, h) = (
            self.crop_box.width() * self.user_unit,
            self.crop_box.height() * self.user_unit,
        );
        if self.rotate % 180 == 90 {
            (h, w)
        } else {
            (w, h)
        }
    }

    fn new(doc: &Document, r: ObjRef, d: &Dict, inherited: Arc<Inherited>) -> Page {
        let attr = |k: &[u8]| d.get(k).or_else(|| inherited.get(k));
        let media_box = attr(b"MediaBox")
            .and_then(|o| Rect::from_object(doc, o))
            .unwrap_or(Rect::LETTER);
        let crop_box = attr(b"CropBox")
            .and_then(|o| Rect::from_object(doc, o))
            .and_then(|c| c.intersect(&media_box))
            .unwrap_or(media_box);
        let rotate = attr(b"Rotate")
            .and_then(|o| doc.resolve(o).as_int())
            .map_or(0, normalize_rotation);
        let user_unit = d
            .get(b"UserUnit")
            .and_then(|o| doc.resolve(o).as_f64())
            .filter(|u| u.is_finite() && *u > 0.0)
            .unwrap_or(1.0);
        Page {
            r,
            media_box,
            crop_box,
            rotate,
            user_unit,
            inherited,
        }
    }
}

/// `/Rotate` reduced to 0, 90, 180 or 270; values that are not multiples of 90 count as 0.
pub(crate) fn normalize_rotation(r: i64) -> u16 {
    if r % 90 == 0 {
        r.rem_euclid(360) as u16
    } else {
        0
    }
}

/// The pages of a document in order.
///
/// [`PageTree::open`] reads only the root: [`PageTree::len`] is its `/Count`, and
/// [`PageTree::page`] descends by the `/Count`s of the nodes on the way, checking that each
/// of those nodes' kids add up to it. Where they do not, the tree is walked in full, once,
/// as [`PageTree::load`] and [`PageTree::pages`] do. Counts are only checked on the path
/// to the page asked for, so a damaged subtree elsewhere goes unnoticed until the walk.
/// The tree reads a snapshot of the document taken when it was opened.
#[derive(Clone, Debug)]
pub struct PageTree {
    doc: Document,
    root: Option<ObjRef>,
    /// The root's `/Count`, trusted until the tree is walked.
    count: usize,
    full: OnceLock<Arc<Walk>>,
}

#[derive(Debug, Default)]
struct Walk {
    pages: Vec<Page>,
    /// Interior `/Pages` nodes below the root, in walk order.
    nodes: Vec<ObjRef>,
    repaired: bool,
    index: OnceLock<NumMap<usize>>,
}

impl PageTree {
    /// Reads the root of the tree for access on demand. Only a missing or broken root, or
    /// one whose kids disagree with its `/Count`, makes this walk the tree.
    pub fn open(doc: &Document) -> PageTree {
        let mut t = PageTree {
            doc: doc.clone(),
            root: None,
            count: 0,
            full: OnceLock::new(),
        };
        if let Some(r) = pages_ref(doc)
            && let Some(o) = doc.get(r)
            && let Some(d) = o.as_dict()
            && is_node(doc, d)
        {
            t.root = Some(r);
            if let Some(n) = root_count(doc, r, d) {
                t.count = n;
                return t;
            }
        }
        t.count = t.walk().pages.len();
        t
    }

    /// Walks the whole tree from the catalog. Cycles, shared kids, wrong `/Count`s,
    /// missing `/Type`s and broken kids are tolerated; when no tree can be found, the pages
    /// are recovered by scanning every object for `/Type /Page`.
    pub fn load(doc: &Document) -> PageTree {
        let t = PageTree::open(doc);
        t.walk();
        t
    }

    /// The number of pages: the root's `/Count` until the tree has been walked (by
    /// [`PageTree::load`], [`PageTree::pages`] or a lookup finding wrong counts), exact after.
    pub fn len(&self) -> usize {
        self.full.get().map_or(self.count, |w| w.pages.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Page `i`, found through the `/Count`s without walking the tree when they add up.
    pub fn page(&self, i: usize) -> Option<Page> {
        if let Some(w) = self.full.get() {
            return w.pages.get(i).cloned();
        }
        if i >= self.count {
            return None;
        }
        self.descend(i)
            .or_else(|| self.walk().pages.get(i).cloned())
    }

    /// The index of page object `r` (such as a link's target), found by climbing its
    /// `/Parent`s when the tree has not been walked.
    pub fn index_of(&self, r: ObjRef) -> Option<usize> {
        if self.full.get().is_none()
            && let Some(i) = self.climb(r)
            && self.descend(i).is_some_and(|p| p.r == r)
        {
            return Some(i);
        }
        let w = self.walk();
        w.index.get_or_init(|| index(&w.pages)).get(&r.num).copied()
    }

    /// Every page, walking the tree on first use.
    pub fn pages(&self) -> &[Page] {
        &self.walk().pages
    }

    /// The root `/Pages` node, if the catalog has a usable one.
    pub fn root(&self) -> Option<ObjRef> {
        self.root
    }

    /// The tree was unusable and pages were found by scanning all objects.
    pub fn repaired(&self) -> bool {
        self.full.get().is_some_and(|w| w.repaired)
    }

    pub fn refs(&self) -> Vec<ObjRef> {
        self.pages().iter().map(|p| p.r).collect()
    }

    /// Page index by object number.
    pub fn index(&self) -> NumMap<usize> {
        let w = self.walk();
        w.index.get_or_init(|| index(&w.pages)).clone()
    }

    pub(crate) fn nodes(&self) -> &[ObjRef] {
        &self.walk().nodes
    }

    fn walk(&self) -> &Walk {
        self.full.get_or_init(|| {
            let doc = &self.doc;
            let mut w = Walk::default();
            match self.root {
                Some(root) => w.walk(doc, root),
                None => {
                    // A catalog pointing straight at a single page.
                    if let Some(r) = pages_ref(doc)
                        && let Some(o) = doc.get(r)
                        && let Some(d) = page_dict(r, &o)
                    {
                        w.pages.push(Page::new(doc, r, d, Arc::default()));
                    }
                }
            }
            if w.pages.is_empty() && !self.root.is_some_and(|r| claims_empty(doc, r)) {
                w.scan(doc);
            }
            Arc::new(w)
        })
    }

    /// Page `i` by `/Count`s, or `None` where they do not add up.
    fn descend(&self, mut i: usize) -> Option<Page> {
        let doc = &self.doc;
        let root = self.root?;
        let mut node = doc.get(root)?;
        let mut inh = Inherited::extend(&Arc::default(), node.as_dict()?);
        let mut seen = HashSet::from([root.num]);
        for _ in 0..MAX_DEPTH {
            let d = node.as_dict()?;
            let total = count(d)?;
            let kids = doc.get_in(d, b"Kids");
            let kids = kids.as_array().unwrap_or_default();
            // As many kids as pages: taken to be all pages, so only kid `i` is looked at.
            if kids.len() == total
                && let Some(r) = kids.get(i).and_then(Object::as_objref)
                && !seen.contains(&r.num)
                && let Some(o) = doc.get(r)
                && let Some(kd) = page_dict(r, &o)
                && !is_node(doc, kd)
            {
                return Some(Page::new(doc, r, kd, inh));
            }
            let mut hit = None;
            let mut sum = 0usize;
            for k in kids.iter() {
                let r = k.as_objref()?;
                if !seen.insert(r.num) {
                    return None;
                }
                let o = doc.get(r)?;
                let kd = o.as_dict()?;
                let leaf = !is_node(doc, kd);
                let c = if leaf { 1 } else { count(kd)? };
                if hit.is_none() && i < sum + c {
                    hit = Some((r, o.clone(), leaf, i - sum));
                }
                sum = sum.checked_add(c)?;
            }
            if sum != total {
                return None;
            }
            let (r, o, leaf, rest) = hit?;
            let kd = o.as_dict()?;
            if leaf {
                return Some(Page::new(doc, r, page_dict(r, &o)?, inh));
            }
            inh = Inherited::extend(&inh, kd);
            i = rest;
            node = o;
        }
        None
    }

    /// The index of page `r` from the `/Count`s of the kids before it on the way up.
    fn climb(&self, r: ObjRef) -> Option<usize> {
        let doc = &self.doc;
        let root = self.root?;
        let mut cur = r;
        let mut i = 0usize;
        for _ in 0..MAX_DEPTH {
            if cur == root {
                return Some(i);
            }
            let parent = doc.get(cur)?.as_dict()?.get(b"Parent")?.as_objref()?;
            let p = doc.get(parent)?;
            let pd = p.as_dict()?;
            let kids = doc.get_in(pd, b"Kids");
            let kids = kids.as_array().unwrap_or_default();
            // As in `descend`, as many kids as pages are taken to be pages.
            if Some(kids.len()) == count(pd) {
                i = i.checked_add(kids.iter().position(|k| *k == Object::Ref(cur))?)?;
                cur = parent;
                continue;
            }
            for k in kids.iter() {
                let k = k.as_objref()?;
                if k == cur {
                    break;
                }
                let o = doc.get(k)?;
                let kd = o.as_dict()?;
                i = i.checked_add(if is_node(doc, kd) { count(kd)? } else { 1 })?;
            }
            cur = parent;
        }
        None
    }
}

impl Walk {
    fn walk(&mut self, doc: &Document, root: ObjRef) {
        let Some(obj) = doc.get(root) else { return };
        let Some(d) = obj.as_dict() else { return };
        let mut seen: HashSet<u32> = HashSet::from([root.num]);
        let mut stack = vec![(kids(doc, d), 0usize, Inherited::extend(&Arc::default(), d))];
        while let Some((list, i, inh)) = stack.last_mut() {
            let Some(r) = list.get(*i).and_then(Object::as_objref) else {
                if *i >= list.len() {
                    stack.pop();
                } else {
                    *i += 1;
                }
                continue;
            };
            *i += 1;
            let inh = inh.clone();
            if !seen.insert(r.num) {
                continue;
            }
            let Some(obj) = doc.get(r) else { continue };
            let Some(d) = obj.as_dict() else { continue };
            if is_node(doc, d) {
                self.nodes.push(r);
                let inh = Inherited::extend(&inh, d);
                stack.push((kids(doc, d), 0, inh));
            } else if let Some(d) = page_dict(r, &obj) {
                self.pages.push(Page::new(doc, r, d, inh));
            }
        }
    }

    fn scan(&mut self, doc: &Document) {
        self.repaired = true;
        // Pages the file's table leaves out count too, whether or not a lookup of some
        // other missing object has made the document scan the file yet.
        doc.scan_for_objects();
        let none = Arc::new(Inherited::default());
        for num in doc.object_numbers() {
            let Some(r) = doc.reference(num) else {
                continue;
            };
            let Some(obj) = doc.get(r) else { continue };
            if let Some(d) = page_dict(r, &obj)
                && d.name(b"Type") == Some(b"Page")
            {
                self.pages.push(Page::new(doc, r, d, none.clone()));
            }
        }
    }
}

fn pages_ref(doc: &Document) -> Option<ObjRef> {
    doc.catalog()?.as_dict()?.get(b"Pages")?.as_objref()
}

fn index(pages: &[Page]) -> NumMap<usize> {
    pages
        .iter()
        .enumerate()
        .map(|(i, p)| (p.r.num, i))
        .collect()
}

fn count(d: &Dict) -> Option<usize> {
    usize::try_from(d.int(b"Count")?).ok()
}

/// The root's `/Count` if its kids agree with it. A root with as many kids as pages is
/// taken to hold pages only, which [`PageTree::page`] checks for each page it returns.
fn root_count(doc: &Document, root: ObjRef, d: &Dict) -> Option<usize> {
    let total = count(d)?;
    let kids = doc.get_in(d, b"Kids");
    let kids = kids.as_array().unwrap_or_default();
    if kids.len() == total {
        return Some(total);
    }
    let mut seen = HashSet::from([root.num]);
    let mut sum = 0usize;
    for k in kids.iter() {
        let r = k.as_objref()?;
        if !seen.insert(r.num) {
            return None;
        }
        let o = doc.get(r)?;
        let kd = o.as_dict()?;
        sum = sum.checked_add(if is_node(doc, kd) { count(kd)? } else { 1 })?;
    }
    (sum == total).then_some(total)
}

/// The dictionary of page `r`: a page is a dictionary, never a stream (a damaged tree may
/// lead to an object stream or a picture instead), and never object 0, the head of the
/// free list, which repair may find in a damaged file but no file can save.
fn page_dict(r: ObjRef, o: &Object) -> Option<&Dict> {
    match o {
        Object::Dict(d) if r.num > 0 => Some(d),
        _ => None,
    }
}

fn is_node(doc: &Document, d: &Dict) -> bool {
    match d.name(b"Type") {
        Some(b"Pages") => true,
        Some(b"Page") => false,
        _ => doc.get_in(d, b"Kids").as_array().is_some(),
    }
}

fn kids(doc: &Document, d: &Dict) -> Arc<[Object]> {
    doc.get_in(d, b"Kids")
        .as_array()
        .map_or_else(|| Arc::from([]), Arc::from)
}

/// A root that is a proper, deliberately empty `/Pages` node.
fn claims_empty(doc: &Document, root: ObjRef) -> bool {
    doc.get(root).is_some_and(|o| {
        o.as_dict().is_some_and(|d| {
            d.int(b"Count") == Some(0) && doc.get_in(d, b"Kids").as_array().is_some()
        })
    })
}
