//! Preparing forms: creating fields and widgets, moving, renaming, duplicating and
//! removing them, and the tab order of a page. Every change writes appearance streams, so
//! no reader has to rely on `/NeedAppearances`.

use pagebara_annot::Rect;
use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::form::{Change, Form, acroform, update_acroform};
use crate::model::{FieldKind, Xfa};
use crate::props::{FieldProps, edit_dict, ensure_fonts};
use crate::text::{encode, text_of};
use crate::{Error, FieldFlags, Result};

/// A field to create.
#[derive(Clone, Debug, PartialEq)]
pub struct NewField {
    pub kind: FieldKind,
    /// The fully qualified name (periods make parent fields); empty picks one.
    pub name: String,
    pub page: usize,
    /// In user space.
    pub rect: Rect,
    pub props: FieldProps,
    /// The on-state of a check box or the first radio button (`Yes`, `Choice1` when
    /// `None`).
    pub export: Option<String>,
}

impl NewField {
    pub fn new(kind: FieldKind, page: usize, rect: Rect) -> NewField {
        NewField {
            kind,
            name: String::new(),
            page,
            rect,
            props: FieldProps::for_kind(kind),
            export: None,
        }
    }
}

/// How Tab moves through a page's fields (`/Tabs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TabOrder {
    /// In the order of the structure tree (`/S`).
    Structure,
    /// Row by row (`/R`).
    Rows,
    /// Column by column (`/C`).
    Columns,
    /// In this order of widgets (the page's annotation order; `/Tabs` removed). Reading a
    /// page gives the current order with an empty list.
    Manual(Vec<ObjRef>),
}

/// Keys that belong to the widget when a field and its widget are one dictionary.
const WIDGET_KEYS: [&[u8]; 18] = [
    b"Type",
    b"Subtype",
    b"Rect",
    b"P",
    b"F",
    b"AP",
    b"AS",
    b"MK",
    b"BS",
    b"Border",
    b"H",
    b"A",
    b"StructParent",
    b"NM",
    b"M",
    b"C",
    b"OC",
    b"Contents",
];
/// Triggers of a merged `/AA` that belong to the widget (the rest are the field's).
const WIDGET_TRIGGERS: [&[u8]; 10] = [
    b"E", b"X", b"D", b"U", b"Fo", b"Bl", b"PO", b"PC", b"PV", b"PI",
];

fn kids(doc: &Document, d: &Dict) -> Vec<Object> {
    doc.get_in(d, b"Kids")
        .as_array()
        .unwrap_or_default()
        .to_vec()
}

/// Edits an array that may be a direct entry of `owner` or an indirect object.
fn edit_array(doc: &mut Document, owner: ObjRef, key: &[u8], f: impl FnOnce(&mut Vec<Object>)) {
    let Some(d) = doc.get(owner).and_then(|o| o.as_dict().cloned()) else {
        return;
    };
    match d.get(key).and_then(Object::as_objref) {
        Some(r) => {
            let mut a = doc
                .get(r)
                .and_then(|o| o.as_array().map(<[_]>::to_vec))
                .unwrap_or_default();
            f(&mut a);
            doc.set(r, Object::Array(a));
        }
        None => edit_dict(doc, owner, |_, d| {
            let mut a = d
                .get(key)
                .and_then(Object::as_array)
                .map(<[_]>::to_vec)
                .unwrap_or_default();
            f(&mut a);
            d.set(key, Object::Array(a));
        }),
    }
}

/// Edits the form's `/Fields`.
fn edit_fields(doc: &mut Document, f: impl FnOnce(&mut Vec<Object>)) {
    let af_ref = doc
        .catalog()
        .and_then(|c| c.as_dict()?.get(b"AcroForm")?.as_objref());
    match af_ref {
        Some(r) => edit_array(doc, r, b"Fields", f),
        None => update_acroform(doc, |doc, af| {
            let mut a = doc
                .get_in(af, b"Fields")
                .as_array()
                .unwrap_or_default()
                .to_vec();
            f(&mut a);
            af.set(b"Fields", Object::Array(a));
        }),
    }
}

/// A partial name as `/T` holds it.
fn partial_of(doc: &Document, r: ObjRef) -> Option<String> {
    let d = doc.get(r)?;
    text_of(&doc.get_in(d.as_dict()?, b"T"))
}

fn kind_base(kind: FieldKind, date: bool) -> &'static str {
    match kind {
        FieldKind::Text if date => "Date",
        FieldKind::Text => "Text",
        FieldKind::CheckBox => "CheckBox",
        FieldKind::Radio => "Group",
        FieldKind::ComboBox => "Dropdown",
        FieldKind::ListBox => "ListBox",
        FieldKind::PushButton => "Button",
        FieldKind::Signature => "Signature",
        FieldKind::Unknown => "Field",
    }
}

fn placeholder_ap(on: &str) -> Dict {
    let mut n = Dict::new();
    n.set(on.as_bytes(), Object::Null);
    n.set(b"Off", Object::Null);
    let mut ap = Dict::new();
    ap.set(b"N", n);
    ap
}

impl Form {
    /// Re-reads the whole form after structural changes, keeping our fonts and streams.
    pub(crate) fn reload(&mut self, doc: &Document) {
        let font = self.font.take();
        let generated = std::mem::take(&mut self.generated);
        *self = Form::load(doc);
        self.font = font;
        self.generated = generated;
    }

    /// Fails unless fields may be created and changed; makes the form exist and drops
    /// XFA (as the first fill does).
    pub(crate) fn prepare_edit(&mut self, doc: &mut Document) -> Result<Change> {
        if self.xfa == Xfa::XfaOnly {
            return Err(Error::XfaOnly);
        }
        let p = doc.permissions();
        if !(p.modify && p.annotate) {
            return Err(Error::NotPermitted);
        }
        if acroform(doc).is_none() {
            let mut af = Dict::new();
            af.set(b"Fields", Object::Array(Vec::new()));
            let r = doc.add(af);
            if let Some(root) = doc.trailer().get(b"Root").and_then(Object::as_objref) {
                edit_dict(doc, root, |_, c| c.set(b"AcroForm", r));
            }
        }
        ensure_fonts(doc, "Helv");
        self.has_form = true;
        self.prepare(doc)
    }

    /// Whether `name` cannot be used for a new field: it is a field, a parent of one, or
    /// below one.
    pub fn name_taken(&self, name: &str) -> bool {
        self.fields.iter().any(|f| {
            f.name == name
                || f.name
                    .strip_prefix(name)
                    .is_some_and(|r| r.starts_with('.'))
                || name
                    .strip_prefix(f.name.as_str())
                    .is_some_and(|r| r.starts_with('.'))
        })
    }

    /// A free name for a new field: `base` itself, else `base` with the first free number
    /// (`Text1`, `Text2`, ...).
    pub fn unique_name(&self, base: &str) -> String {
        if !base.is_empty()
            && !self.name_taken(base)
            && !base.ends_with(|c: char| c.is_ascii_digit())
        {
            return base.to_owned();
        }
        let stem = base.trim_end_matches(|c: char| c.is_ascii_digit());
        let stem = if stem.is_empty() { "Field" } else { stem };
        (1..)
            .map(|n| format!("{stem}{n}"))
            .find(|n| !self.name_taken(n))
            .unwrap_or_default()
    }

    /// The name a new field of `kind` gets by default.
    pub fn default_name(&self, kind: FieldKind, date: bool) -> String {
        self.unique_name(&format!("{}1", kind_base(kind, date)))
    }

    fn check_name(name: &str) -> Result<Vec<&str>> {
        let parts: Vec<&str> = name.split('.').collect();
        if name.trim().is_empty() || parts.iter().any(|p| p.trim().is_empty()) {
            return Err(Error::BadName(name.to_owned()));
        }
        Ok(parts)
    }

    /// The parent field for the name parts before the last, created as needed.
    fn parent_for(doc: &mut Document, parts: &[&str]) -> Option<ObjRef> {
        let mut parent: Option<ObjRef> = None;
        for part in parts {
            let siblings: Vec<ObjRef> = match parent {
                None => acroform(doc)
                    .map(|af| {
                        doc.get_in(&af, b"Fields")
                            .as_array()
                            .unwrap_or_default()
                            .to_vec()
                    })
                    .unwrap_or_default(),
                Some(p) => doc
                    .get(p)
                    .and_then(|o| o.as_dict().map(|d| kids(doc, d)))
                    .unwrap_or_default(),
            }
            .iter()
            .filter_map(Object::as_objref)
            .collect();
            let found = siblings
                .into_iter()
                .find(|&k| partial_of(doc, k).as_deref() == Some(part));
            parent = Some(match found {
                Some(k) => k,
                None => {
                    let mut d = Dict::new();
                    d.set(b"T", encode(part));
                    d.set(b"Kids", Object::Array(Vec::new()));
                    if let Some(p) = parent {
                        d.set(b"Parent", p);
                    }
                    let r = doc.add(d);
                    Self::link(doc, parent, r);
                    r
                }
            });
        }
        parent
    }

    /// Adds field node `r` to `parent`'s `/Kids`, or to `/Fields`.
    fn link(doc: &mut Document, parent: Option<ObjRef>, r: ObjRef) {
        match parent {
            Some(p) => edit_array(doc, p, b"Kids", |k| k.push(Object::Ref(r))),
            None => edit_fields(doc, |f| f.push(Object::Ref(r))),
        }
    }

    fn page_ref(&self, page: usize) -> Result<ObjRef> {
        self.pages
            .get(page)
            .map(|p| p.r)
            .ok_or(Error::BadData("page"))
    }

    fn add_to_page(doc: &mut Document, page: ObjRef, widget: ObjRef) {
        edit_array(doc, page, b"Annots", |a| a.push(Object::Ref(widget)));
    }

    /// Creates a field with one widget. Returns its name.
    pub fn add_field(&mut self, doc: &mut Document, nf: &NewField) -> Result<(Change, String)> {
        self.prepare_edit(doc)?;
        let date = matches!(nf.props.format, crate::props::Format::Date(_));
        let name = if nf.name.trim().is_empty() {
            self.default_name(nf.kind, date)
        } else {
            nf.name.trim().to_owned()
        };
        let parts = Self::check_name(&name)?;
        if self.name_taken(&name) {
            return Err(Error::NameTaken(name));
        }
        if nf.kind == FieldKind::Unknown {
            return Err(Error::WrongKind {
                field: name,
                expected: "known",
            });
        }
        let page = self.page_ref(nf.page)?;
        let (last, path) = parts.split_last().ok_or(Error::BadName(name.clone()))?;
        let parent = Self::parent_for(doc, path);
        let ft: &[u8] = match nf.kind {
            FieldKind::Text => b"Tx",
            FieldKind::ComboBox | FieldKind::ListBox => b"Ch",
            FieldKind::Signature => b"Sig",
            _ => b"Btn",
        };
        let flags = match nf.kind {
            FieldKind::Radio => FieldFlags::RADIO,
            FieldKind::PushButton => FieldFlags::PUSHBUTTON,
            FieldKind::ComboBox => FieldFlags::COMBO,
            _ => FieldFlags(0),
        };
        let mut field = Dict::new();
        field.set(b"FT", Object::name(ft));
        field.set(b"T", encode(last));
        if flags.0 != 0 {
            field.set(b"Ff", Object::Int(i64::from(flags.0)));
        }
        if let Some(p) = parent {
            field.set(b"Parent", p);
        }
        let mut widget = Dict::new();
        widget.set(b"Type", Object::name(b"Annot"));
        widget.set(b"Subtype", Object::name(b"Widget"));
        widget.set(b"Rect", nf.rect.to_object());
        widget.set(b"P", page);
        widget.set(b"F", Object::Int(4));
        let on = nf.export.clone().unwrap_or_else(|| {
            if nf.kind == FieldKind::Radio {
                "Choice1".into()
            } else {
                "Yes".into()
            }
        });
        if matches!(nf.kind, FieldKind::CheckBox | FieldKind::Radio) {
            widget.set(b"AP", placeholder_ap(&on));
            widget.set(b"AS", Object::name(b"Off"));
        }
        let wref = if nf.kind == FieldKind::Radio {
            let fr = doc.add(field);
            widget.set(b"Parent", fr);
            let w = doc.add(widget);
            edit_dict(doc, fr, |_, d| {
                d.set(b"Kids", Object::Array(vec![Object::Ref(w)]))
            });
            Self::link(doc, parent, fr);
            w
        } else {
            for (k, v) in widget.iter() {
                field.set(k.as_bytes(), v.clone());
            }
            let r = doc.add(field);
            Self::link(doc, parent, r);
            r
        };
        Self::add_to_page(doc, page, wref);
        let tooltip = if nf.props.tooltip.is_empty() {
            name.clone()
        } else {
            nf.props.tooltip.clone()
        };
        crate::tag::tag_widget(doc, page, wref, &tooltip);
        self.reload(doc);
        let mut props = nf.props.clone();
        props.tooltip = tooltip;
        let change = self.set_props(doc, &name, &props)?;
        Ok((change, name))
    }

    /// Turns a field whose widget is the same dictionary into a field with one kid widget;
    /// returns the widget.
    fn split_merged(&mut self, doc: &mut Document, field: ObjRef) -> Option<ObjRef> {
        let mut d = doc.get(field)?.as_dict()?.clone();
        let mut w = Dict::new();
        for k in WIDGET_KEYS {
            if let Some(v) = d.remove(k) {
                w.set(k, v);
            }
        }
        if let Some(aa) = doc.get_in(&d, b"AA").as_dict().cloned() {
            let (mut fa, mut wa) = (Dict::new(), Dict::new());
            for (k, v) in aa.iter() {
                if WIDGET_TRIGGERS.contains(&k.as_bytes()) {
                    wa.set(k.as_bytes(), v.clone());
                } else {
                    fa.set(k.as_bytes(), v.clone());
                }
            }
            for (key, a) in [(&mut d, fa), (&mut w, wa)] {
                if a.is_empty() {
                    key.remove(b"AA");
                } else {
                    key.set(b"AA", a);
                }
            }
        }
        w.set(b"Parent", field);
        let key = w.int(b"StructParent");
        let page = w.get(b"P").and_then(Object::as_objref);
        let wr = doc.add(w);
        d.set(b"Kids", Object::Array(vec![Object::Ref(wr)]));
        doc.set(field, d);
        // Pages list the widget, not the field.
        for p in self.pages.iter().map(|p| p.r).chain(page) {
            edit_array(doc, p, b"Annots", |a| {
                for o in a.iter_mut() {
                    if o.as_objref() == Some(field) {
                        *o = Object::Ref(wr);
                    }
                }
            });
        }
        if let Some(k) = key {
            crate::tag::retarget(doc, k, wr);
        }
        Some(wr)
    }

    /// Adds a widget to field `name` on `page`: another radio button of a group (with its
    /// export value, else the next free `ChoiceN`), or another place showing the same
    /// field. Returns the widget.
    pub fn add_widget(
        &mut self,
        doc: &mut Document,
        name: &str,
        page: usize,
        rect: Rect,
        export: Option<&str>,
    ) -> Result<(Change, ObjRef)> {
        let mut change = self.prepare_edit(doc)?;
        let f = self
            .field(name)
            .cloned()
            .ok_or_else(|| Error::NoSuchField(name.to_owned()))?;
        let page_ref = self.page_ref(page)?;
        let first = f.widgets.first().cloned();
        let template = first
            .as_ref()
            .and_then(|w| doc.get(w.id)?.as_dict().cloned())
            .unwrap_or_default();
        if f.widgets.len() == 1 && f.widgets[0].id == f.id {
            self.split_merged(doc, f.id);
        }
        let mut w = Dict::new();
        w.set(b"Type", Object::name(b"Annot"));
        w.set(b"Subtype", Object::name(b"Widget"));
        for k in [&b"F"[..], b"MK", b"BS", b"Border", b"DA", b"Q", b"A", b"H"] {
            if let Some(v) = template.get(k) {
                w.set(k, v.clone());
            }
        }
        if !w.contains(b"F") {
            w.set(b"F", Object::Int(4));
        }
        w.set(b"Rect", rect.to_object());
        w.set(b"P", page_ref);
        w.set(b"Parent", f.id);
        if matches!(f.kind, FieldKind::CheckBox | FieldKind::Radio) {
            let on = match (export, f.kind) {
                (Some(e), _) if !e.trim().is_empty() && e != "Off" => e.trim().to_owned(),
                (_, FieldKind::Radio) => {
                    let used: Vec<String> = f
                        .widgets
                        .iter()
                        .filter_map(|w| w.on_state.clone())
                        .collect();
                    (1..)
                        .map(|n| format!("Choice{n}"))
                        .find(|c| !used.contains(c))
                        .unwrap_or_default()
                }
                _ => first
                    .as_ref()
                    .and_then(|w| w.on_state.clone())
                    .unwrap_or_else(|| "Yes".into()),
            };
            let state = if f.value == crate::FieldValue::State(on.clone())
                && f.kind == FieldKind::CheckBox
            {
                on.clone()
            } else {
                "Off".into()
            };
            w.set(b"AP", placeholder_ap(&on));
            w.set(b"AS", Object::name(state.as_bytes()));
        }
        let wr = doc.add(w);
        edit_array(doc, f.id, b"Kids", |k| k.push(Object::Ref(wr)));
        Self::add_to_page(doc, page_ref, wr);
        let tooltip = f.alternate_name.clone().unwrap_or_else(|| f.name.clone());
        crate::tag::tag_widget(doc, page_ref, wr, &tooltip);
        self.reload(doc);
        let i = self.index(name)?;
        change.merge(self.appearance_for(doc, i, true)?);
        change.widgets.push(wr);
        Ok((change, wr))
    }

    /// Puts copies of field `name`'s widgets on `pages` (at the same places), all showing
    /// the same value.
    pub fn copy_to_pages(
        &mut self,
        doc: &mut Document,
        name: &str,
        pages: &[usize],
    ) -> Result<Change> {
        let f = self
            .field(name)
            .cloned()
            .ok_or_else(|| Error::NoSuchField(name.to_owned()))?;
        let source = f.widgets.first().and_then(|w| w.page);
        let originals: Vec<_> = f
            .widgets
            .iter()
            .filter(|w| w.page == source)
            .cloned()
            .collect();
        let mut change = Change::default();
        for &p in pages {
            if Some(p) == source {
                continue;
            }
            for w in &originals {
                let (c, _) = self.add_widget(doc, name, p, w.rect, w.on_state.as_deref())?;
                change.merge(c);
            }
        }
        if f.kind == FieldKind::Radio && !change.widgets.is_empty() {
            let i = self.index(name)?;
            let id = self.fields[i].id;
            let flags = self.fields[i]
                .flags
                .with(FieldFlags::RADIOS_IN_UNISON, true);
            edit_dict(doc, id, |_, d| {
                d.set(b"Ff", Object::Int(i64::from(flags.0)))
            });
            self.reload(doc);
        }
        Ok(change)
    }

    /// Makes a copy of field `name` moved by `dx, dy` points, under a new name. Returns
    /// the new name.
    pub fn duplicate(
        &mut self,
        doc: &mut Document,
        name: &str,
        dx: f64,
        dy: f64,
    ) -> Result<(Change, String)> {
        let f = self
            .field(name)
            .cloned()
            .ok_or_else(|| Error::NoSuchField(name.to_owned()))?;
        let mut props = self
            .props(doc, name)
            .ok_or_else(|| Error::NoSuchField(name.to_owned()))?;
        if f.alternate_name.as_deref() == Some(name) {
            props.tooltip.clear();
        }
        let shift = |r: Rect| Rect::new(r.x0 + dx, r.y0 + dy, r.x1 + dx, r.y1 + dy);
        let base = f.name.trim_end_matches(|c: char| c.is_ascii_digit());
        let new_name = self.unique_name(&format!("{base}1"));
        let mut change = Change::default();
        for (n, w) in f.widgets.iter().enumerate() {
            let page = w.page.unwrap_or(0);
            if n == 0 {
                let nf = NewField {
                    kind: f.kind,
                    name: new_name.clone(),
                    page,
                    rect: shift(w.rect),
                    props: props.clone(),
                    export: w.on_state.clone(),
                };
                change.merge(self.add_field(doc, &nf)?.0);
            } else {
                let (c, _) =
                    self.add_widget(doc, &new_name, page, shift(w.rect), w.on_state.as_deref())?;
                change.merge(c);
            }
        }
        Ok((change, new_name))
    }

    /// Renames field `old` to `new` (periods move it under other parents).
    pub fn rename(&mut self, doc: &mut Document, old: &str, new: &str) -> Result<Change> {
        let mut change = self.prepare_edit(doc)?;
        let new = new.trim();
        if old == new {
            return Ok(change);
        }
        let parts = Self::check_name(new)?;
        if self.name_taken(new) {
            return Err(Error::NameTaken(new.to_owned()));
        }
        let f = self
            .field(old)
            .cloned()
            .ok_or_else(|| Error::NoSuchField(old.to_owned()))?;
        let (last, path) = parts.split_last().ok_or(Error::BadName(new.to_owned()))?;
        let old_path: Vec<&str> = old.split('.').collect();
        let same_parent = old_path.len() == parts.len() && old_path[..old_path.len() - 1] == *path;
        if !same_parent {
            self.unlink(doc, f.id);
            let parent = Self::parent_for(doc, path);
            edit_dict(doc, f.id, |_, d| match parent {
                Some(p) => d.set(b"Parent", p),
                None => {
                    d.remove(b"Parent");
                }
            });
            Self::link(doc, parent, f.id);
        }
        edit_dict(doc, f.id, |_, d| {
            d.set(b"T", encode(last));
            // A tooltip that was the name follows it.
            if d.get(b"TU").and_then(text_of).as_deref() == Some(old) {
                d.set(b"TU", encode(new));
            }
        });
        self.reload(doc);
        change.fields.push(new.to_owned());
        Ok(change)
    }

    /// Takes field node `r` out of its parent's `/Kids` (or `/Fields`); parents left empty
    /// go too.
    fn unlink(&self, doc: &mut Document, r: ObjRef) {
        let mut node = r;
        loop {
            let parent = doc
                .get(node)
                .and_then(|o| o.as_dict()?.get(b"Parent")?.as_objref());
            match parent {
                Some(p) => {
                    edit_array(doc, p, b"Kids", |k| {
                        k.retain(|o| o.as_objref() != Some(node))
                    });
                    let empty = doc
                        .get(p)
                        .and_then(|o| o.as_dict().map(|d| kids(doc, d).is_empty()))
                        .unwrap_or(false);
                    if !empty {
                        return;
                    }
                    node = p;
                }
                None => {
                    edit_fields(doc, |f| f.retain(|o| o.as_objref() != Some(node)));
                    return;
                }
            }
        }
    }

    fn drop_widget(&self, doc: &mut Document, w: &crate::Widget) {
        let key = doc
            .get(w.id)
            .and_then(|o| o.as_dict()?.int(b"StructParent"));
        for p in self.pages.iter().map(|p| p.r).chain(w.page_ref) {
            edit_array(doc, p, b"Annots", |a| {
                a.retain(|o| o.as_objref() != Some(w.id))
            });
        }
        if let Some(k) = key {
            crate::tag::untag(doc, k);
        }
    }

    /// Removes field `name` with all its widgets.
    pub fn remove_field(&mut self, doc: &mut Document, name: &str) -> Result<Change> {
        let mut change = self.prepare_edit(doc)?;
        let f = self
            .field(name)
            .cloned()
            .ok_or_else(|| Error::NoSuchField(name.to_owned()))?;
        for w in &f.widgets {
            self.drop_widget(doc, w);
            change.widgets.push(w.id);
        }
        self.unlink(doc, f.id);
        if self.calculation_order.contains(&f.id) {
            update_acroform(doc, |_, af| {
                if let Some(Object::Array(co)) = af.get_mut(b"CO") {
                    co.retain(|o| o.as_objref() != Some(f.id));
                }
            });
        }
        for w in &f.widgets {
            if w.id != f.id {
                doc.delete(w.id);
            }
        }
        doc.delete(f.id);
        self.reload(doc);
        change.fields.push(name.to_owned());
        Ok(change)
    }

    /// Removes one widget; the last one takes its field along.
    pub fn remove_widget(&mut self, doc: &mut Document, widget: ObjRef) -> Result<Change> {
        let f = self
            .field_of_widget(widget)
            .cloned()
            .ok_or_else(|| Error::NoSuchField(format!("{widget:?}")))?;
        if f.widgets.len() <= 1 || f.id == widget {
            return self.remove_field(doc, &f.name);
        }
        let mut change = self.prepare_edit(doc)?;
        let Some(w) = f.widgets.iter().find(|w| w.id == widget) else {
            return Err(Error::NoSuchField(f.name));
        };
        self.drop_widget(doc, w);
        edit_array(doc, f.id, b"Kids", |k| {
            k.retain(|o| o.as_objref() != Some(widget))
        });
        doc.delete(widget);
        self.reload(doc);
        change.widgets.push(widget);
        change.fields.push(f.name);
        Ok(change)
    }

    /// Moves or resizes widgets (rectangles in user space) and redraws them.
    pub fn set_rects(&mut self, doc: &mut Document, rects: &[(ObjRef, Rect)]) -> Result<Change> {
        let mut change = self.prepare_edit(doc)?;
        let mut names = Vec::new();
        for (w, r) in rects {
            let Some(f) = self.field_of_widget(*w) else {
                return Err(Error::NoSuchField(format!("{w:?}")));
            };
            if !names.contains(&f.name) {
                names.push(f.name.clone());
            }
            let r = Rect::new(r.x0, r.y0, r.x1.max(r.x0 + 1.0), r.y1.max(r.y0 + 1.0));
            edit_dict(doc, *w, |_, d| d.set(b"Rect", r.to_object()));
        }
        self.reload(doc);
        for n in names {
            let i = self.index(&n)?;
            change.merge(self.appearance_for(doc, i, true)?);
        }
        Ok(change)
    }

    /// The tab order of `page` (a manual order lists its widgets).
    pub fn tab_mode(&self, page: usize) -> TabOrder {
        match self.pages.get(page).and_then(|p| p.tabs.as_deref()) {
            Some("R") => TabOrder::Rows,
            Some("C") => TabOrder::Columns,
            Some("S") => TabOrder::Structure,
            _ => TabOrder::Manual(self.tab_order(page)),
        }
    }

    /// Sets how Tab moves through the fields of `page`.
    pub fn set_tab_order(
        &mut self,
        doc: &mut Document,
        page: usize,
        order: &TabOrder,
    ) -> Result<Change> {
        let change = self.prepare_edit(doc)?;
        let page_ref = self.page_ref(page)?;
        let tabs: Option<&[u8]> = match order {
            TabOrder::Structure => Some(b"S"),
            TabOrder::Rows => Some(b"R"),
            TabOrder::Columns => Some(b"C"),
            TabOrder::Manual(_) => None,
        };
        if let TabOrder::Manual(list) = order {
            let widgets: Vec<ObjRef> = self
                .fields
                .iter()
                .flat_map(|f| &f.widgets)
                .filter(|w| w.page == Some(page))
                .map(|w| w.id)
                .collect();
            let mut ordered: Vec<ObjRef> = list
                .iter()
                .copied()
                .filter(|w| widgets.contains(w))
                .collect();
            let annots = self.pages[page].annots.clone();
            let rest: Vec<ObjRef> = annots
                .iter()
                .copied()
                .filter(|a| widgets.contains(a) && !ordered.contains(a))
                .collect();
            ordered.extend(rest);
            // Widgets take the slots widgets had, in the new order; other annotations stay.
            let mut next = ordered.into_iter();
            edit_array(doc, page_ref, b"Annots", |a| {
                for o in a.iter_mut() {
                    if o.as_objref().is_some_and(|r| widgets.contains(&r))
                        && let Some(w) = next.next()
                    {
                        *o = Object::Ref(w);
                    }
                }
            });
        }
        edit_dict(doc, page_ref, |_, d| match tabs {
            Some(t) => d.set(b"Tabs", Object::name(t)),
            None => {
                d.remove(b"Tabs");
            }
        });
        self.reload(doc);
        Ok(change)
    }

    /// Gives push button `name` an icon: an image XObject (see
    /// [`pagebara_annot::embed_image`]) of `width` × `height` pixels, shown above the
    /// caption (`/TP` 2), or alone when the caption is empty.
    pub fn set_button_icon(
        &mut self,
        doc: &mut Document,
        name: &str,
        image: Option<(ObjRef, u32, u32)>,
    ) -> Result<Change> {
        let mut change = self.prepare_edit(doc)?;
        let f = self
            .field(name)
            .cloned()
            .ok_or_else(|| Error::NoSuchField(name.to_owned()))?;
        if f.kind != FieldKind::PushButton {
            return Err(Error::WrongKind {
                field: name.to_owned(),
                expected: "button",
            });
        }
        let icon = image.map(|(img, w, h)| {
            let (w, h) = (f64::from(w.max(1)), f64::from(h.max(1)));
            let mut d = Dict::new();
            d.set(b"Type", Object::name(b"XObject"));
            d.set(b"Subtype", Object::name(b"Form"));
            d.set(b"BBox", Rect::new(0.0, 0.0, w, h).to_object());
            let mut xo = Dict::new();
            xo.set(b"Im0", img);
            let mut res = Dict::new();
            res.set(b"XObject", xo);
            d.set(b"Resources", res);
            let content = format!(
                "q {} 0 0 {} 0 0 cm /Im0 Do Q",
                crate::content::fmt_num(w),
                crate::content::fmt_num(h)
            );
            doc.add(pagebara_syntax::Stream::new(d, content.into_bytes()))
        });
        for w in &f.widgets {
            edit_dict(doc, w.id, |doc, d| {
                let mut mk = doc.get_in(d, b"MK").as_dict().cloned().unwrap_or_default();
                match icon {
                    Some(i) => {
                        mk.set(b"I", i);
                        let empty = mk.get(b"CA").and_then(text_of).is_none_or(|c| c.is_empty());
                        mk.set(b"TP", Object::Int(if empty { 1 } else { 2 }));
                    }
                    None => {
                        mk.remove(b"I");
                        mk.remove(b"TP");
                    }
                }
                d.set(b"MK", mk);
            });
        }
        self.reload(doc);
        let i = self.index(name)?;
        change.merge(self.appearance_for(doc, i, true)?);
        Ok(change)
    }
}
