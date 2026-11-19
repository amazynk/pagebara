//! Optional content (§8.11): which groups the default configuration turns off, and the
//! visibility of groups and membership dictionaries.

use std::collections::HashSet;

use pagebara_syntax::{Document, ObjRef, Object};

/// Visibility expressions nested deeper than this count as visible.
const MAX_DEPTH: u32 = 16;

#[derive(Clone, Debug, Default)]
pub struct OptionalContent {
    /// `/BaseState /OFF`: only groups in `on` are visible.
    base_off: bool,
    on: HashSet<ObjRef>,
    off: HashSet<ObjRef>,
}

impl OptionalContent {
    /// The default configuration (`/OCProperties /D`) of a document.
    pub fn new(doc: &Document) -> Self {
        let mut oc = OptionalContent::default();
        let Some(cat) = doc.catalog() else { return oc };
        let Some(cat) = cat.as_dict() else { return oc };
        let props = doc.get_in(cat, b"OCProperties");
        let Some(props) = props.as_dict() else {
            return oc;
        };
        let d = doc.get_in(props, b"D");
        let Some(d) = d.as_dict() else { return oc };
        oc.base_off = doc.get_in(d, b"BaseState").as_name() == Some(b"OFF");
        let refs = |key: &[u8]| -> HashSet<ObjRef> {
            doc.get_in(d, key)
                .as_array()
                .unwrap_or_default()
                .iter()
                .filter_map(Object::as_objref)
                .collect()
        };
        oc.on = refs(b"ON");
        oc.off = refs(b"OFF");
        oc
    }

    /// Hides a group (or shows it with `visible`), as a viewer's layer panel would.
    pub fn set_group(&mut self, group: ObjRef, visible: bool) {
        if visible {
            self.off.remove(&group);
            self.on.insert(group);
        } else {
            self.on.remove(&group);
            self.off.insert(group);
        }
    }

    pub fn group_visible(&self, r: ObjRef) -> bool {
        if self.base_off {
            self.on.contains(&r) && !self.off.contains(&r)
        } else {
            !self.off.contains(&r)
        }
    }

    /// Visibility of an optional content group or membership dictionary (or a reference
    /// to one). Anything malformed is visible.
    pub fn visible(&self, doc: &Document, o: &Object) -> bool {
        self.visible_depth(doc, o, 0)
    }

    fn visible_depth(&self, doc: &Document, o: &Object, depth: u32) -> bool {
        if depth > MAX_DEPTH {
            return true;
        }
        let d = doc.resolve(o);
        let Some(d) = d.as_dict() else { return true };
        match d.name(b"Type") {
            Some(b"OCMD") => {}
            _ => return o.as_objref().is_none_or(|r| self.group_visible(r)),
        }
        if let Some(ve) = d.get(b"VE")
            && let Some(v) = self.expression(doc, ve, depth + 1)
        {
            return v;
        }
        let groups = doc.get_in(d, b"OCGs");
        let groups: Vec<bool> = match &*groups {
            Object::Array(a) => a
                .iter()
                .filter_map(Object::as_objref)
                .map(|r| self.group_visible(r))
                .collect(),
            _ => match d.get(b"OCGs").and_then(Object::as_objref) {
                Some(r) => vec![self.group_visible(r)],
                None => return true,
            },
        };
        if groups.is_empty() {
            return true;
        }
        match d.name(b"P") {
            Some(b"AllOn") => groups.iter().all(|&v| v),
            Some(b"AnyOff") => groups.iter().any(|&v| !v),
            Some(b"AllOff") => groups.iter().all(|&v| !v),
            _ => groups.iter().any(|&v| v),
        }
    }

    /// A visibility expression `[/And|/Or|/Not operands…]`; `None` if malformed.
    fn expression(&self, doc: &Document, e: &Object, depth: u32) -> Option<bool> {
        if depth > MAX_DEPTH {
            return None;
        }
        let a = doc.resolve(e);
        let a = a.as_array()?;
        let (op, args) = a.split_first()?;
        let mut vals = args.iter().map(|x| match x {
            Object::Ref(r) if doc.resolve(x).as_array().is_none() => Some(self.group_visible(*r)),
            _ => self.expression(doc, x, depth + 1),
        });
        match op.as_name()? {
            b"And" => vals.try_fold(true, |acc, v| Some(acc & v?)),
            b"Or" => vals.try_fold(false, |acc, v| Some(acc | v?)),
            b"Not" => vals.next()?.map(|v| !v),
            _ => None,
        }
    }
}
