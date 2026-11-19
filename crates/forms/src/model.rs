//! The field tree (ISO 32000-2 §12.7.4): terminal fields with inherited attributes, their
//! widgets, values and options.

use std::collections::{HashMap, HashSet};

use pagebara_annot::{Color, Rect};
use pagebara_syntax::{Dict, Document, ObjRef, Object};

use crate::flags::FieldFlags;
use crate::script::Scripts;
use crate::text::{decode, text_of};

/// Field trees deeper than this are treated as broken.
pub(crate) const MAX_DEPTH: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldKind {
    Text,
    CheckBox,
    Radio,
    PushButton,
    ComboBox,
    ListBox,
    Signature,
    /// No or an unknown `/FT`.
    Unknown,
}

impl FieldKind {
    pub fn as_str(self) -> &'static str {
        match self {
            FieldKind::Text => "text",
            FieldKind::CheckBox => "checkbox",
            FieldKind::Radio => "radio",
            FieldKind::PushButton => "button",
            FieldKind::ComboBox => "combo",
            FieldKind::ListBox => "list",
            FieldKind::Signature => "signature",
            FieldKind::Unknown => "unknown",
        }
    }

    pub fn is_button(self) -> bool {
        matches!(
            self,
            FieldKind::CheckBox | FieldKind::Radio | FieldKind::PushButton
        )
    }

    pub fn is_choice(self) -> bool {
        matches!(self, FieldKind::ComboBox | FieldKind::ListBox)
    }
}

/// A field's value (`/V`).
#[derive(Clone, Debug, PartialEq, Default)]
pub enum FieldValue {
    /// No value (or `/Off` for buttons).
    #[default]
    None,
    Text(String),
    /// The on-state name of a check box or radio button that is on.
    State(String),
    /// The selected export values of a choice field.
    Choices(Vec<String>),
}

impl FieldValue {
    /// The value as one string: choices joined by commas, empty for none.
    pub fn display(&self) -> String {
        match self {
            FieldValue::None => String::new(),
            FieldValue::Text(s) | FieldValue::State(s) => s.clone(),
            FieldValue::Choices(c) => c.join(", "),
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, FieldValue::None)
    }
}

/// One item of a choice field's `/Opt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChoiceOption {
    /// What `/V` holds when the item is selected.
    pub export: String,
    /// What the list shows.
    pub display: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BorderKind {
    #[default]
    Solid,
    Dashed,
    Beveled,
    Inset,
    Underline,
}

/// A widget's border (`/BS`, or the legacy `/Border`).
#[derive(Clone, Debug, PartialEq)]
pub struct Border {
    pub width: f64,
    pub kind: BorderKind,
    pub dash: Vec<f64>,
}

impl Default for Border {
    fn default() -> Self {
        Border {
            width: 1.0,
            kind: BorderKind::Solid,
            dash: vec![3.0],
        }
    }
}

/// The appearance characteristics (`/MK`, table 192).
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Mk {
    pub border: Option<Color>,
    pub background: Option<Color>,
    /// `/CA`: a button's caption, or the ZapfDingbats character of a check box's mark.
    pub caption: Option<String>,
    pub rollover_caption: Option<String>,
    pub down_caption: Option<String>,
    /// Counter-clockwise rotation of the widget's contents: 0, 90, 180 or 270.
    pub rotation: u16,
    /// The widget shows an icon (`/I`).
    pub has_icon: bool,
    /// The icon's form XObject.
    pub icon: Option<ObjRef>,
    /// `/TP`: where the caption goes relative to the icon (0 caption only, 1 icon only,
    /// 2 below, 3 above, 4 right, 5 left, 6 overlaid).
    pub text_position: u8,
}

/// One widget annotation of a field.
#[derive(Clone, Debug, PartialEq)]
pub struct Widget {
    pub id: ObjRef,
    /// Page index, from the page whose `/Annots` lists the widget (else `/P`).
    pub page: Option<usize>,
    pub page_ref: Option<ObjRef>,
    pub rect: Rect,
    pub mk: Mk,
    pub border: Border,
    /// Annotation flags (`/F`).
    pub annot_flags: u32,
    /// A check box's or radio button's on-state name.
    pub on_state: Option<String>,
    /// The value this button stands for: its `/Opt` entry, else its on-state name.
    pub export: Option<String>,
    /// The current appearance state (`/AS`).
    pub state: Option<String>,
    /// The widget's own `/DA` and `/Q`, overriding the field's.
    pub da: Option<String>,
    pub quadding: Option<u8>,
    pub has_appearance: bool,
}

impl Widget {
    pub fn is_hidden(&self) -> bool {
        // Hidden (2) or NoView (32).
        self.annot_flags & (2 | 32) != 0
    }

    /// Whether this button is shown on.
    pub fn is_on(&self) -> bool {
        self.state
            .as_deref()
            .is_some_and(|s| s != "Off" && Some(s) == self.on_state.as_deref())
    }
}

/// A terminal field: the node that holds the value, with its widgets.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    pub id: ObjRef,
    /// The fully qualified name: partial names of the ancestors joined by periods.
    pub name: String,
    pub partial_name: String,
    /// `/TU`, the name shown to users.
    pub alternate_name: Option<String>,
    /// `/TM`, the name used when exporting.
    pub mapping_name: Option<String>,
    pub kind: FieldKind,
    pub flags: FieldFlags,
    pub value: FieldValue,
    pub default_value: FieldValue,
    pub max_len: Option<usize>,
    pub options: Vec<ChoiceOption>,
    /// `/I`: selected option indices.
    pub selected_indices: Vec<usize>,
    /// `/TI`: the first visible item of a list box.
    pub top_index: usize,
    /// The effective `/DA` (inherited, else the form's).
    pub da: Option<String>,
    /// The effective `/Q`: 0 left, 1 centred, 2 right.
    pub quadding: u8,
    pub widgets: Vec<Widget>,
    pub scripts: Scripts,
    /// The field and its ancestors, nearest first.
    pub(crate) chain: Vec<ObjRef>,
}

impl Field {
    pub fn is_read_only(&self) -> bool {
        self.flags.read_only()
    }

    pub fn is_multiline(&self) -> bool {
        self.flags.contains(FieldFlags::MULTILINE)
    }

    pub fn is_password(&self) -> bool {
        self.flags.contains(FieldFlags::PASSWORD)
    }

    /// Comb fields need `/MaxLen` and exclude multiline, password and file select.
    pub fn is_comb(&self) -> bool {
        self.kind == FieldKind::Text
            && self.flags.contains(FieldFlags::COMB)
            && self.max_len.is_some()
            && !self.flags.contains(FieldFlags::MULTILINE)
            && !self.flags.contains(FieldFlags::PASSWORD)
            && !self.flags.contains(FieldFlags::FILE_SELECT)
    }

    pub fn is_multi_select(&self) -> bool {
        self.kind == FieldKind::ListBox && self.flags.contains(FieldFlags::MULTI_SELECT)
    }

    pub fn is_editable_combo(&self) -> bool {
        self.kind == FieldKind::ComboBox && self.flags.contains(FieldFlags::EDIT)
    }

    /// The text value, or empty.
    pub fn text(&self) -> String {
        self.value.display()
    }

    /// Whether a check box (or any of a radio group's buttons) is on.
    pub fn is_checked(&self) -> bool {
        matches!(self.value, FieldValue::State(_))
    }

    /// The export value of the button that is on, as data exchange reports it.
    pub fn export_value(&self) -> Option<String> {
        let FieldValue::State(s) = &self.value else {
            return None;
        };
        let w = self
            .widgets
            .iter()
            .find(|w| w.on_state.as_deref() == Some(s));
        Some(
            w.and_then(|w| w.export.clone())
                .unwrap_or_else(|| s.clone()),
        )
    }

    /// Distinct export values of a radio group or check box, in widget order.
    pub fn export_values(&self) -> Vec<String> {
        let mut v: Vec<String> = Vec::new();
        for w in &self.widgets {
            if let Some(e) = w.export.clone().or_else(|| w.on_state.clone())
                && !v.contains(&e)
            {
                v.push(e);
            }
        }
        v
    }

    /// Indices of the selected options of a choice field.
    pub fn selection(&self) -> Vec<usize> {
        let FieldValue::Choices(vals) = &self.value else {
            return Vec::new();
        };
        if !self.selected_indices.is_empty()
            && self.selected_indices.iter().all(|&i| {
                self.options
                    .get(i)
                    .is_some_and(|o| vals.contains(&o.export))
            })
        {
            return self.selected_indices.clone();
        }
        let mut out = Vec::new();
        for v in vals {
            if let Some(i) = self
                .options
                .iter()
                .enumerate()
                .position(|(i, o)| &o.export == v && !out.contains(&i))
            {
                out.push(i);
            }
        }
        out.sort_unstable();
        out
    }
}

/// How a form uses XFA (§12.7.9).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Xfa {
    #[default]
    None,
    /// XFA with AcroForm fields too: we fill the fields and drop the XFA on the first edit,
    /// so other readers show what we wrote.
    WithAcroForm,
    /// Only XFA, which is not supported.
    XfaOnly,
}

/// Where widgets are (object number to page index and page).
pub(crate) type PageMap = HashMap<u32, (usize, ObjRef)>;

/// A page as tab order and widget lookup need it.
#[derive(Clone, Debug)]
pub(crate) struct PageInfo {
    pub r: ObjRef,
    pub tabs: Option<String>,
    pub annots: Vec<ObjRef>,
}

pub(crate) fn page_map(doc: &Document) -> (PageMap, Vec<PageInfo>) {
    let tree = pagebara_pages::PageTree::load(doc);
    let mut map = PageMap::new();
    let mut pages = Vec::new();
    for (i, p) in tree.pages().iter().enumerate() {
        let Some(d) = doc.get(p.r).and_then(|d| d.as_dict().cloned()) else {
            pages.push(PageInfo {
                r: p.r,
                tabs: None,
                annots: Vec::new(),
            });
            continue;
        };
        let d = &d;
        let annots: Vec<ObjRef> = doc
            .get_in(d, b"Annots")
            .as_array()
            .unwrap_or_default()
            .iter()
            .filter_map(Object::as_objref)
            .collect();
        for a in &annots {
            map.entry(a.num).or_insert((i, p.r));
        }
        let tabs = d.name(b"Tabs").map(crate::text::name_text);
        pages.push(PageInfo {
            r: p.r,
            tabs,
            annots,
        });
    }
    (map, pages)
}

pub(crate) struct Reader<'a> {
    pub doc: &'a Document,
    pub pages: &'a PageMap,
    pub form: &'a Dict,
}

impl Reader<'_> {
    fn dict(&self, r: ObjRef) -> Option<Dict> {
        self.doc.get(r)?.as_dict().cloned()
    }

    /// `key` from the nearest node of `chain` that has it.
    fn inherited(&self, chain: &[ObjRef], key: &[u8]) -> Option<Object> {
        chain.iter().find_map(|&r| {
            let d = self.doc.get(r)?;
            let v = self.doc.get_in(d.as_dict()?, key);
            (!v.is_null()).then(|| (*v).clone())
        })
    }

    /// Walks the field tree from the form's `/Fields`, plus widgets found on pages whose
    /// fields the tree does not reach.
    pub fn fields(&self, extra_widgets: &[ObjRef]) -> Vec<Field> {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let roots: Vec<ObjRef> = self
            .doc
            .get_in(self.form, b"Fields")
            .as_array()
            .unwrap_or_default()
            .iter()
            .filter_map(Object::as_objref)
            .collect();
        for r in roots {
            self.walk(r, &mut Vec::new(), "", &mut out, &mut seen);
        }
        for &w in extra_widgets {
            if seen.contains(&w.num) {
                continue;
            }
            let Some(d) = self.dict(w) else { continue };
            if d.name(b"Subtype") != Some(b"Widget") {
                continue;
            }
            // Climb to the root field and read its whole tree.
            let mut root = w;
            let mut cur = d;
            for _ in 0..MAX_DEPTH {
                match cur.get(b"Parent").and_then(Object::as_objref) {
                    Some(p) if !seen.contains(&p.num) => match self.dict(p) {
                        Some(pd) => {
                            root = p;
                            cur = pd;
                        }
                        None => break,
                    },
                    _ => break,
                }
            }
            if self.inherited(&[root], b"FT").is_some() || cur.contains(b"T") {
                self.walk(root, &mut Vec::new(), "", &mut out, &mut seen);
            }
        }
        out
    }

    fn walk(
        &self,
        r: ObjRef,
        ancestors: &mut Vec<ObjRef>,
        prefix: &str,
        out: &mut Vec<Field>,
        seen: &mut HashSet<u32>,
    ) {
        if ancestors.len() > MAX_DEPTH || !seen.insert(r.num) {
            return;
        }
        let Some(d) = self.dict(r) else { return };
        let partial = d.get(b"T").and_then(Object::as_string).map(decode);
        let name = match (&partial, prefix.is_empty()) {
            (Some(p), true) => p.clone(),
            (Some(p), false) => format!("{prefix}.{p}"),
            (None, _) => prefix.to_owned(),
        };
        let kids: Vec<ObjRef> = self
            .doc
            .get_in(&d, b"Kids")
            .as_array()
            .unwrap_or_default()
            .iter()
            .filter_map(Object::as_objref)
            // Kids already read are cycles or shared nodes.
            .filter(|k| !seen.contains(&k.num))
            .collect();
        let (fields, widgets): (Vec<ObjRef>, Vec<ObjRef>) =
            kids.iter().partition(|&&k| self.is_field_node(k));
        ancestors.push(r);
        for k in fields {
            self.walk(k, ancestors, &name, out, seen);
        }
        ancestors.pop();
        let merged = kids.is_empty();
        if !widgets.is_empty() || merged {
            let widgets = if merged {
                if d.name(b"Subtype") == Some(b"Widget") || d.contains(b"Rect") {
                    vec![r]
                } else {
                    vec![]
                }
            } else {
                widgets
                    .into_iter()
                    .filter(|w| seen.insert(w.num) || *w == r)
                    .collect()
            };
            let mut chain = vec![r];
            chain.extend(ancestors.iter().rev());
            if let Some(f) = self.field(&chain, name, partial.unwrap_or_default(), &widgets) {
                out.push(f);
            }
        }
    }

    /// A kid is a field when it has a name or kids of its own, else it is a widget.
    fn is_field_node(&self, r: ObjRef) -> bool {
        let Some(d) = self.dict(r) else { return false };
        d.contains(b"T")
            || (d.name(b"Subtype") != Some(b"Widget")
                && self
                    .doc
                    .get_in(&d, b"Kids")
                    .as_array()
                    .is_some_and(|k| !k.is_empty()))
    }

    pub fn field(
        &self,
        chain: &[ObjRef],
        name: String,
        partial: String,
        widgets: &[ObjRef],
    ) -> Option<Field> {
        let d = self.dict(chain[0])?;
        let ft = self.inherited(chain, b"FT");
        let flags = FieldFlags(
            self.inherited(chain, b"Ff")
                .and_then(|o| o.as_int())
                .unwrap_or(0) as u32,
        );
        let kind = match ft.as_ref().and_then(Object::as_name) {
            Some(b"Tx") => FieldKind::Text,
            Some(b"Btn") if flags.contains(FieldFlags::PUSHBUTTON) => FieldKind::PushButton,
            Some(b"Btn") if flags.contains(FieldFlags::RADIO) => FieldKind::Radio,
            Some(b"Btn") => FieldKind::CheckBox,
            Some(b"Ch") if flags.contains(FieldFlags::COMBO) => FieldKind::ComboBox,
            Some(b"Ch") => FieldKind::ListBox,
            Some(b"Sig") => FieldKind::Signature,
            _ => FieldKind::Unknown,
        };
        let value = self.value(kind, self.inherited(chain, b"V"));
        let default_value = self.value(kind, self.inherited(chain, b"DV"));
        let opt: Vec<Object> = self
            .inherited(chain, b"Opt")
            .map(|o| {
                let o = self.doc.resolve(&o);
                o.as_array().map(<[_]>::to_vec).unwrap_or_default()
            })
            .unwrap_or_default();
        let options = if kind.is_choice() {
            opt.iter().filter_map(|o| self.option(o)).collect()
        } else {
            Vec::new()
        };
        let button_opts: Vec<Option<String>> = if kind.is_button() {
            opt.iter().map(|o| text_of(&self.doc.resolve(o))).collect()
        } else {
            Vec::new()
        };
        let form_da = self
            .doc
            .get_in(self.form, b"DA")
            .as_string()
            .map(<[u8]>::to_vec);
        let da = self
            .inherited(chain, b"DA")
            .and_then(|o| o.as_string().map(<[u8]>::to_vec))
            .or(form_da)
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        let quadding = self
            .inherited(chain, b"Q")
            .or_else(|| self.doc.get_in(self.form, b"Q").as_int().map(Object::Int))
            .and_then(|o| o.as_int())
            .unwrap_or(0)
            .clamp(0, 2) as u8;
        let max_len = self
            .inherited(chain, b"MaxLen")
            .and_then(|o| o.as_int())
            .and_then(|n| usize::try_from(n).ok());
        let widgets: Vec<Widget> = widgets
            .iter()
            .enumerate()
            .filter_map(|(i, &w)| {
                let export = button_opts.get(i).cloned().flatten();
                self.widget(w, export)
            })
            .collect();
        let int_list = |o: &Object| -> Vec<usize> {
            match &*self.doc.resolve(o) {
                Object::Array(a) => a
                    .iter()
                    .filter_map(|i| usize::try_from(i.as_int()?).ok())
                    .collect(),
                o => o
                    .as_int()
                    .and_then(|i| usize::try_from(i).ok())
                    .into_iter()
                    .collect(),
            }
        };
        let text = |k: &[u8]| self.doc.get_in(&d, k).as_string().map(decode);
        let mut scripts = Scripts::read(self.doc, &d);
        for w in &widgets {
            if w.id != chain[0]
                && let Some(wd) = self.dict(w.id)
            {
                scripts.add(self.doc, &wd, false);
            }
        }
        Some(Field {
            id: chain[0],
            name,
            partial_name: partial,
            alternate_name: text(b"TU"),
            mapping_name: text(b"TM"),
            kind,
            flags,
            value,
            default_value,
            max_len,
            options,
            selected_indices: d.get(b"I").map(int_list).unwrap_or_default(),
            top_index: self
                .doc
                .get_in(&d, b"TI")
                .as_int()
                .and_then(|i| usize::try_from(i).ok())
                .unwrap_or(0),
            da,
            quadding,
            widgets,
            scripts,
            chain: chain.to_vec(),
        })
    }

    fn value(&self, kind: FieldKind, v: Option<Object>) -> FieldValue {
        let Some(v) = v else {
            return FieldValue::None;
        };
        let v = self.doc.resolve(&v);
        match kind {
            FieldKind::CheckBox | FieldKind::Radio => match text_of(&v) {
                Some(s) if s != "Off" && !s.is_empty() => FieldValue::State(s),
                _ => FieldValue::None,
            },
            FieldKind::ComboBox | FieldKind::ListBox => {
                let list: Vec<String> = match &*v {
                    Object::Array(a) => a
                        .iter()
                        .filter_map(|o| text_of(&self.doc.resolve(o)))
                        .collect(),
                    o => text_of(o).into_iter().collect(),
                };
                if list.is_empty() {
                    FieldValue::None
                } else {
                    FieldValue::Choices(list)
                }
            }
            FieldKind::Signature | FieldKind::PushButton => FieldValue::None,
            _ => match &*v {
                Object::Stream(s) => self
                    .doc
                    .decode(s)
                    .map(|d| FieldValue::Text(decode(&d.data)))
                    .unwrap_or_default(),
                o => text_of(o).map_or(FieldValue::None, FieldValue::Text),
            },
        }
    }

    fn option(&self, o: &Object) -> Option<ChoiceOption> {
        let o = self.doc.resolve(o);
        if let Some(a) = o.as_array() {
            let t = |i: usize| a.get(i).and_then(|o| text_of(&self.doc.resolve(o)));
            let export = t(0)?;
            let display = t(1).unwrap_or_else(|| export.clone());
            return Some(ChoiceOption { export, display });
        }
        let s = text_of(&o)?;
        Some(ChoiceOption {
            export: s.clone(),
            display: s,
        })
    }

    fn widget(&self, r: ObjRef, opt_export: Option<String>) -> Option<Widget> {
        let d = self.dict(r)?;
        let doc = self.doc;
        let rect = Rect::from_object(&doc.get_in(&d, b"Rect")).unwrap_or_default();
        let (page, page_ref) = match self.pages.get(&r.num) {
            Some(&(i, p)) => (Some(i), Some(p)),
            None => (None, d.get(b"P").and_then(Object::as_objref)),
        };
        let mk = doc.get_in(&d, b"MK");
        let mk = mk.as_dict().cloned().unwrap_or_default();
        let color = |k: &[u8]| Color::from_object(&doc.get_in(&mk, k));
        let caption = |k: &[u8]| doc.get_in(&mk, k).as_string().map(decode);
        let rotation =
            doc.get_in(&mk, b"R")
                .as_int()
                .map_or(0, |r| if r % 90 == 0 { r.rem_euclid(360) } else { 0 }) as u16;
        let mk = Mk {
            border: color(b"BC"),
            background: color(b"BG"),
            caption: caption(b"CA"),
            rollover_caption: caption(b"RC"),
            down_caption: caption(b"AC"),
            rotation,
            has_icon: mk.contains(b"I"),
            icon: mk.get(b"I").and_then(Object::as_objref),
            text_position: doc.get_in(&mk, b"TP").as_int().unwrap_or(0).clamp(0, 6) as u8,
        };
        let ap = doc.get_in(&d, b"AP");
        let ap = ap.as_dict();
        let on_state = ap.and_then(|ap| {
            [&b"N"[..], b"D"].iter().find_map(|k| {
                let n = doc.get_in(ap, k);
                n.as_dict()
                    .filter(|_| n.as_stream().is_none())?
                    .iter()
                    .map(|(k, _)| crate::text::name_text(k.as_bytes()))
                    .find(|k| k != "Off")
            })
        });
        let has_appearance = ap.is_some_and(|ap| !doc.get_in(ap, b"N").is_null());
        Some(Widget {
            id: r,
            page,
            page_ref,
            rect,
            mk,
            border: border(doc, &d),
            annot_flags: doc.get_in(&d, b"F").as_int().unwrap_or(0) as u32,
            export: opt_export.or_else(|| on_state.clone()),
            on_state,
            state: d.name(b"AS").map(crate::text::name_text),
            da: d
                .get(b"DA")
                .and_then(Object::as_string)
                .map(|b| String::from_utf8_lossy(b).into_owned()),
            quadding: d.int(b"Q").map(|q| q.clamp(0, 2) as u8),
            has_appearance,
        })
    }
}

fn border(doc: &Document, d: &Dict) -> Border {
    let mut b = Border::default();
    let bs = doc.get_in(d, b"BS");
    if let Some(bs) = bs.as_dict() {
        if let Some(w) = doc.get_in(bs, b"W").as_f64() {
            b.width = w.max(0.0);
        }
        b.kind = match bs.name(b"S") {
            Some(b"D") => BorderKind::Dashed,
            Some(b"B") => BorderKind::Beveled,
            Some(b"I") => BorderKind::Inset,
            Some(b"U") => BorderKind::Underline,
            _ => BorderKind::Solid,
        };
        if let Some(a) = doc.get_in(bs, b"D").as_array() {
            let dash: Vec<f64> = a.iter().filter_map(Object::as_f64).collect();
            if !dash.is_empty() && dash.iter().any(|v| *v > 0.0) {
                b.dash = dash;
            }
        }
    } else if let Some(a) = doc.get_in(d, b"Border").as_array()
        && let Some(w) = a.get(2).and_then(Object::as_f64)
    {
        b.width = w.max(0.0);
    }
    b
}
