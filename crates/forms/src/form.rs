//! [`Form`]: the interactive form of a document.

use std::collections::HashSet;

use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::fonts::SharedFont;
use crate::model::{Field, FieldKind, PageInfo, PageMap, Reader, Xfa, page_map};
use crate::{Error, Result};

/// What an edit changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Change {
    /// Fully qualified names of fields whose value changed, recalculated ones included.
    pub fields: Vec<String>,
    /// Widgets whose appearance was regenerated (to redraw).
    pub widgets: Vec<ObjRef>,
    /// Characters no available font could show (drawn as a fallback glyph).
    pub missing: Vec<char>,
    /// The XFA form was removed so readers show the AcroForm we filled.
    pub xfa_removed: bool,
}

impl Change {
    /// Adds what `o` changed.
    pub fn merge(&mut self, o: Change) {
        for f in o.fields {
            if !self.fields.contains(&f) {
                self.fields.push(f);
            }
        }
        for w in o.widgets {
            if !self.widgets.contains(&w) {
                self.widgets.push(w);
            }
        }
        for c in o.missing {
            if !self.missing.contains(&c) {
                self.missing.push(c);
            }
        }
        self.xfa_removed |= o.xfa_removed;
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty() && self.widgets.is_empty()
    }
}

/// A field whose scripts we cannot run, for the app to warn about.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptWarning {
    pub field: String,
    /// Triggers (`K`, `F`, `V`, `C`, `A`, `E`, ...) with scripts we did not run.
    pub triggers: Vec<String>,
}

/// A document's interactive form.
#[derive(Clone, Debug)]
pub struct Form {
    /// Terminal fields in tree order.
    pub fields: Vec<Field>,
    /// `/NeedAppearances`: the file asks readers to build appearances.
    pub need_appearances: bool,
    pub xfa: Xfa,
    /// `/SigFlags`: 1 signatures exist, 2 append only.
    pub sig_flags: i64,
    /// `/CO`: fields recalculated, in this order, when any value changes.
    pub calculation_order: Vec<ObjRef>,
    /// Scripts at document level (name tree, open action).
    pub document_scripts: usize,
    pub(crate) has_form: bool,
    pub(crate) pages: Vec<PageInfo>,
    pub(crate) page_map: PageMap,
    pub(crate) font: Option<SharedFont>,
    /// Appearance streams we created, which later edits overwrite in place.
    pub(crate) generated: HashSet<u32>,
}

impl Form {
    /// Reads the form; a document without one gives an empty form.
    pub fn load(doc: &Document) -> Form {
        let (page_map, pages) = page_map(doc);
        let af = acroform(doc);
        let mut form = Form {
            fields: Vec::new(),
            need_appearances: false,
            xfa: Xfa::None,
            sig_flags: 0,
            calculation_order: Vec::new(),
            document_scripts: crate::script::document_scripts(doc).len(),
            has_form: af.is_some(),
            pages,
            page_map,
            font: None,
            generated: HashSet::new(),
        };
        let af = af.unwrap_or_default();
        let all_annots: Vec<ObjRef> = form
            .pages
            .iter()
            .flat_map(|p| p.annots.iter().copied())
            .collect();
        let reader = Reader {
            doc,
            pages: &form.page_map,
            form: &af,
        };
        form.fields = reader.fields(&all_annots);
        form.need_appearances = doc.get_in(&af, b"NeedAppearances").as_bool() == Some(true);
        form.sig_flags = doc.get_in(&af, b"SigFlags").as_int().unwrap_or(0);
        form.calculation_order = doc
            .get_in(&af, b"CO")
            .as_array()
            .unwrap_or_default()
            .iter()
            .filter_map(Object::as_objref)
            .collect();
        if !doc.get_in(&af, b"XFA").is_null() {
            form.xfa = if form.fields.is_empty() {
                Xfa::XfaOnly
            } else {
                Xfa::WithAcroForm
            };
        }
        form.has_form |= !form.fields.is_empty();
        form
    }

    /// Whether the document has an interactive form.
    pub fn has_form(&self) -> bool {
        self.has_form
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// The font for text the form's own fonts cannot show (an embedded Unicode font).
    /// Without one, Helvetica (WinAnsi) is the last resort.
    pub fn set_font(&mut self, font: SharedFont) {
        self.font = Some(font);
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name == name)
    }

    pub(crate) fn index(&self, name: &str) -> Result<usize> {
        self.fields
            .iter()
            .position(|f| f.name == name)
            .ok_or_else(|| Error::NoSuchField(name.to_owned()))
    }

    /// The field a widget belongs to.
    pub fn field_of_widget(&self, widget: ObjRef) -> Option<&Field> {
        self.fields
            .iter()
            .find(|f| f.widgets.iter().any(|w| w.id == widget))
    }

    /// Fields that have widgets on page `page`.
    pub fn fields_on_page(&self, page: usize) -> Vec<&Field> {
        self.fields
            .iter()
            .filter(|f| f.widgets.iter().any(|w| w.page == Some(page)))
            .collect()
    }

    /// The widgets of page `page` in tab order: by `/Tabs` (`R` rows, `C` columns), else
    /// in `/Annots` order (also for `S`, structure order, which we approximate so).
    pub fn tab_order(&self, page: usize) -> Vec<ObjRef> {
        let Some(info) = self.pages.get(page) else {
            return Vec::new();
        };
        let widgets: Vec<&crate::Widget> = self
            .fields
            .iter()
            .flat_map(|f| &f.widgets)
            .filter(|w| w.page == Some(page) && !w.is_hidden())
            .collect();
        let pos = |w: &crate::Widget| info.annots.iter().position(|a| *a == w.id);
        let mut v = widgets;
        v.sort_by_key(|w| pos(w).unwrap_or(usize::MAX));
        // Rows (columns) are taken as equal within a few points.
        let key = |a: f64| (a / 4.0).round() as i64;
        match info.tabs.as_deref() {
            Some("R") => v.sort_by(|a, b| {
                key(b.rect.y1)
                    .cmp(&key(a.rect.y1))
                    .then(a.rect.x0.total_cmp(&b.rect.x0))
            }),
            Some("C") => v.sort_by(|a, b| {
                key(a.rect.x0)
                    .cmp(&key(b.rect.x0))
                    .then(b.rect.y1.total_cmp(&a.rect.y1))
            }),
            _ => {}
        }
        v.into_iter().map(|w| w.id).collect()
    }

    /// Whether any field or the document has scripts.
    pub fn has_scripts(&self) -> bool {
        self.document_scripts > 0 || self.fields.iter().any(|f| !f.scripts.is_empty())
    }

    /// Fields with scripts we do not run, for a warning that the form may not behave as
    /// its author intended.
    pub fn script_warnings(&self) -> Vec<ScriptWarning> {
        self.fields
            .iter()
            .filter_map(|f| {
                let triggers = f.scripts.unsupported();
                (!triggers.is_empty()).then(|| ScriptWarning {
                    field: f.name.clone(),
                    triggers,
                })
            })
            .collect()
    }

    /// Signature fields.
    pub fn signature_fields(&self) -> Vec<&Field> {
        self.fields
            .iter()
            .filter(|f| f.kind == FieldKind::Signature)
            .collect()
    }

    /// Fails unless the form can be edited.
    pub fn check_editable(&self, doc: &Document) -> Result<()> {
        if self.xfa == Xfa::XfaOnly {
            return Err(Error::XfaOnly);
        }
        if !self.has_form {
            return Err(Error::NoForm);
        }
        if !doc.permissions().fill_forms {
            return Err(Error::NotPermitted);
        }
        Ok(())
    }

    /// Re-reads field `i` from the document.
    pub(crate) fn reload_field(&mut self, doc: &Document, i: usize) {
        let af = acroform(doc).unwrap_or_default();
        let reader = Reader {
            doc,
            pages: &self.page_map,
            form: &af,
        };
        let f = &self.fields[i];
        let widgets: Vec<ObjRef> = f.widgets.iter().map(|w| w.id).collect();
        if let Some(n) = reader.field(&f.chain, f.name.clone(), f.partial_name.clone(), &widgets) {
            self.fields[i] = n;
        }
    }

    /// Before the first edit: drops XFA (so other readers show our values) and builds the
    /// appearances a `/NeedAppearances` form lacks.
    pub(crate) fn prepare(&mut self, doc: &mut Document) -> Result<Change> {
        self.check_editable(doc)?;
        let mut change = Change::default();
        if self.xfa == Xfa::WithAcroForm {
            update_acroform(doc, |_, af| {
                af.remove(b"XFA");
            });
            if let Some(cat) = doc.catalog()
                && let Some(mut c) = cat.as_dict().cloned()
                && c.remove(b"NeedsRendering").is_some()
                && let Some(r) = doc.trailer().get(b"Root").and_then(Object::as_objref)
            {
                doc.set(r, c);
            }
            self.xfa = Xfa::None;
            change.xfa_removed = true;
        }
        if self.need_appearances {
            change.merge(self.generate_appearances(doc, false)?);
        }
        Ok(change)
    }
}

/// The catalog's `/AcroForm` dictionary.
pub(crate) fn acroform(doc: &Document) -> Option<Dict> {
    let cat = doc.catalog()?;
    doc.get_in(cat.as_dict()?, b"AcroForm").as_dict().cloned()
}

/// Edits `/AcroForm` where it lives (its own object or inside the catalog).
pub(crate) fn update_acroform(doc: &mut Document, f: impl FnOnce(&Document, &mut Dict)) {
    let Some(root) = doc.trailer().get(b"Root").and_then(Object::as_objref) else {
        return;
    };
    let Some(mut cat) = doc.get(root).and_then(|c| c.as_dict().cloned()) else {
        return;
    };
    match cat.get(b"AcroForm").and_then(Object::as_objref) {
        Some(r) => {
            let mut af = doc
                .get(r)
                .and_then(|o| o.as_dict().cloned())
                .unwrap_or_default();
            f(doc, &mut af);
            doc.set(r, af);
        }
        None => {
            let mut af = cat
                .get(b"AcroForm")
                .and_then(Object::as_dict)
                .cloned()
                .unwrap_or_default();
            f(doc, &mut af);
            cat.set(b"AcroForm", af);
            doc.set(root, cat);
        }
    }
}
