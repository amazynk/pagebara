//! Form filling: a [`FormBackend`] loads a [`FormSession`] per document revision, whose
//! editing methods change the document in place (the caller runs them inside an undoable
//! edit) and report a [`FormChange`]. [`EngineForms`] is `pagebara_forms::Form`.

use pagebara_forms::{FieldValue, Form};
use pagebara_i18n::{tr, trn};
use pagebara_syntax::{Document, ObjRef};

pub use pagebara_forms::{
    BorderKind, ButtonAction, ByteRangeCheck, CHECK_STYLES, ChoiceOption, FieldKind, FieldProps,
    Format, NewField, STANDARD_FONTS, SignatureInfo, TabOrder, Xfa,
};

/// What importing form data did.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ImportSummary {
    pub change: FormChange,
    /// Field names the form does not have.
    pub unknown: Vec<String>,
    /// Refused values, in words.
    pub rejected: Vec<String>,
    pub annotations: usize,
}

/// A terminal field as the app shows it.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// Fully qualified name.
    pub name: String,
    /// What users are shown: `/TU`, else the name.
    pub label: String,
    pub kind: FieldKind,
    /// The value as text (the selected options' display texts for choices).
    pub value: String,
    pub read_only: bool,
    pub multiline: bool,
    pub password: bool,
    pub max_len: Option<usize>,
    /// Cells of a comb field.
    pub comb: Option<usize>,
    /// Choice options as shown.
    pub options: Vec<String>,
    /// What each option stands for (its export value).
    pub option_values: Vec<String>,
    /// Selected option indices.
    pub selected: Vec<usize>,
    pub multi_select: bool,
    /// A combo box that also takes typed text.
    pub editable: bool,
    /// A check box or radio group is on.
    pub checked: bool,
    /// The text size; 0 sizes text to fit.
    pub font_size: f64,
    pub widgets: Vec<Widget>,
}

impl Field {
    /// The first widget on `page`, else the first one.
    pub fn widget_on(&self, page: usize) -> Option<&Widget> {
        self.widgets
            .iter()
            .find(|w| w.page == page)
            .or(self.widgets.first())
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Widget {
    pub id: ObjRef,
    pub page: usize,
    /// In user space: x0, y0, x1, y1.
    pub rect: [f64; 4],
    /// A button widget is on.
    pub on: bool,
    /// The value a button widget stands for.
    pub export: Option<String>,
}

/// What a fill changed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FormChange {
    /// Fields whose value changed, recalculated ones included.
    pub fields: Vec<String>,
    /// Widgets whose appearance was regenerated.
    pub widgets: Vec<ObjRef>,
    /// Characters no available font could show.
    pub missing: Vec<char>,
    /// The XFA form was removed so other readers show the filled fields.
    pub xfa_removed: bool,
}

impl From<pagebara_forms::Change> for FormChange {
    fn from(c: pagebara_forms::Change) -> Self {
        FormChange {
            fields: c.fields,
            widgets: c.widgets,
            missing: c.missing,
            xfa_removed: c.xfa_removed,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct FormError {
    pub text: String,
    /// The document's permissions forbid it (an owner password would help).
    pub permission: bool,
}

impl From<pagebara_forms::Error> for FormError {
    fn from(e: pagebara_forms::Error) -> Self {
        let permission = e == pagebara_forms::Error::NotPermitted;
        let text = error_text(&e);
        FormError { text, permission }
    }
}

/// The message for a form engine error, as a sentence.
fn error_text(e: &pagebara_forms::Error) -> String {
    use pagebara_forms::Error as E;
    let q = |s: &str| format!("{s:?}");
    match e {
        E::NoForm => tr!("The document has no interactive form."),
        E::XfaOnly => tr!("This is an XFA form, which Pagebara cannot fill in yet."),
        E::NotPermitted => {
            tr!("This document's security settings do not allow filling in forms.")
        }
        E::NoSuchField(f) => tr!("No field named {}.", q(f)),
        E::ReadOnly(f) => tr!("Field {} is read-only.", q(f)),
        E::WrongKind { field, expected } => {
            tr!(
                "Field {} is not a {} field.",
                q(field),
                field_kind_name(expected)
            )
        }
        E::TooLong { field, max } => trn!(
            "Field {} takes at most {n} character.",
            "Field {} takes at most {n} characters.",
            *max,
            q(field)
        ),
        E::NotAnOption { field, value } => {
            tr!("{} is not an option of field {}.", q(value), q(field))
        }
        E::NotMultiSelect(f) => tr!("Field {} allows only one selection.", q(f)),
        E::Rejected {
            field,
            value,
            reason,
        } => tr!(
            "Field {} rejects {}: {}.",
            q(field),
            q(value),
            rejection(reason)
        ),
        E::BadData(what) => tr!("Not a valid {} file.", what),
        E::NameTaken(n) => tr!("There is already a field named {}.", q(n)),
        E::BadName(n) => tr!("{} is not a valid field name.", q(n)),
    }
}

/// The form engine's names of field kinds, as they appear in its errors.
fn field_kind_name(kind: &str) -> String {
    match kind {
        // TRANSLATORS: a kind of form field, in "Field … is not a {} field."
        "text" => tr!("text"),
        "check box" => tr!("check box"),
        "radio button" => tr!("radio button"),
        "check box or radio button" => tr!("check box or radio button"),
        "choice" => tr!("choice"),
        "fillable" => tr!("fillable"),
        k => k.to_owned(),
    }
}

fn rejection(reason: &str) -> String {
    match reason {
        "a comb field needs a maximum length" => tr!("a comb field needs a maximum length"),
        "an export value must not be empty or Off" => {
            tr!("an export value must not be empty or Off")
        }
        r => r.to_owned(),
    }
}

pub type FormResult = Result<FormChange, FormError>;

/// The form of one document revision.
pub trait FormSession: std::fmt::Debug {
    fn fields(&self) -> Vec<Field>;
    fn field(&self, name: &str) -> Option<Field> {
        self.fields().into_iter().find(|f| f.name == name)
    }
    fn field_of_widget(&self, widget: ObjRef) -> Option<Field> {
        self.fields()
            .into_iter()
            .find(|f| f.widgets.iter().any(|w| w.id == widget))
    }
    /// The widgets of a page in tab order.
    fn tab_order(&self, page: usize) -> Vec<ObjRef>;
    fn set_text(&mut self, doc: &mut Document, name: &str, value: &str) -> FormResult;
    fn set_checked(&mut self, doc: &mut Document, name: &str, on: bool) -> FormResult;
    /// Turns on the radio button standing for `value`; `None` turns the group off.
    fn select_radio(&mut self, doc: &mut Document, name: &str, value: Option<&str>) -> FormResult;
    /// Selects options by export value or display text.
    fn select_choices(&mut self, doc: &mut Document, name: &str, values: &[&str]) -> FormResult;
    fn select_choice(&mut self, doc: &mut Document, name: &str, value: &str) -> FormResult {
        self.select_choices(doc, name, &[value])
    }
    /// A click on a check box or radio button widget.
    fn click(&mut self, doc: &mut Document, widget: ObjRef) -> FormResult;
    /// Every field back to its default value.
    fn reset(&mut self, doc: &mut Document) -> FormResult;
    fn recalculate(&mut self, doc: &mut Document) -> FormResult;
    fn generate_appearances(&mut self, doc: &mut Document, force: bool) -> FormResult;
    /// Draws every field into its page and removes the form.
    fn flatten(&mut self, doc: &mut Document) -> FormResult;
    /// Fields whose scripts are not run, in words.
    fn script_warnings(&self) -> Vec<String>;
    fn xfa(&self) -> Xfa;
    fn signatures(&self, doc: &Document) -> Vec<SignatureInfo>;
    /// Whether the document has fields that can be filled in.
    fn can_fill(&self, doc: &Document) -> bool;
    fn boxed_clone(&self) -> Box<dyn FormSession>;

    // Preparing forms.
    /// The properties a form designer edits.
    fn props(&self, doc: &Document, name: &str) -> Option<FieldProps>;
    fn set_props(&mut self, doc: &mut Document, name: &str, props: &FieldProps) -> FormResult;
    /// Creates a field; returns its name.
    fn add_field(
        &mut self,
        doc: &mut Document,
        field: &NewField,
    ) -> Result<(FormChange, String), FormError>;
    /// Another widget of a field (a radio button, or a copy elsewhere).
    fn add_widget(
        &mut self,
        doc: &mut Document,
        name: &str,
        page: usize,
        rect: [f64; 4],
        export: Option<&str>,
    ) -> Result<(FormChange, ObjRef), FormError>;
    fn remove_field(&mut self, doc: &mut Document, name: &str) -> FormResult;
    fn remove_widget(&mut self, doc: &mut Document, widget: ObjRef) -> FormResult;
    fn rename(&mut self, doc: &mut Document, old: &str, new: &str) -> FormResult;
    /// A copy under a new name, moved by `dx, dy` points; returns the name.
    fn duplicate(
        &mut self,
        doc: &mut Document,
        name: &str,
        dx: f64,
        dy: f64,
    ) -> Result<(FormChange, String), FormError>;
    /// Copies with the same name (and value) on other pages.
    fn copy_to_pages(&mut self, doc: &mut Document, name: &str, pages: &[usize]) -> FormResult;
    /// Moves or resizes widgets (x0, y0, x1, y1 in user space).
    fn set_rects(&mut self, doc: &mut Document, rects: &[(ObjRef, [f64; 4])]) -> FormResult;
    fn set_export(&mut self, doc: &mut Document, widget: ObjRef, value: &str) -> FormResult;
    /// An embedded image XObject (and its pixel size) as a button's icon; `None` removes it.
    fn set_button_icon(
        &mut self,
        doc: &mut Document,
        name: &str,
        icon: Option<(ObjRef, u32, u32)>,
    ) -> FormResult;
    fn tab_mode(&self, page: usize) -> TabOrder;
    fn set_tab_order(&mut self, doc: &mut Document, page: usize, order: &TabOrder) -> FormResult;
    fn name_taken(&self, name: &str) -> bool;
    /// A free name for a new field of `kind` (`date` for date fields).
    fn default_name(&self, kind: FieldKind, date: bool) -> String;
    fn unique_name(&self, base: &str) -> String;
    fn button_action(&self, doc: &Document, name: &str) -> ButtonAction;
    /// Every field (or those named) back to its default value.
    fn reset_fields(&mut self, doc: &mut Document, names: &[&str]) -> FormResult;
    /// Every field emptied, whatever its default.
    fn clear_all(&mut self, doc: &mut Document) -> FormResult;

    // Form data.
    fn export_fdf(&self, file: Option<&str>) -> Vec<u8>;
    fn export_xfdf(&self, doc: &Document, file: Option<&str>, annotations: bool) -> String;
    /// Imports FDF or XFDF (told apart by their contents).
    fn import_data(&mut self, doc: &mut Document, data: &[u8]) -> Result<ImportSummary, FormError>;
}

pub trait FormBackend: Send + Sync + std::fmt::Debug {
    fn load(&self, doc: &Document) -> Box<dyn FormSession>;
}

#[derive(Debug, Default)]
pub struct EngineForms;

impl FormBackend for EngineForms {
    fn load(&self, doc: &Document) -> Box<dyn FormSession> {
        Box::new(Form::load(doc))
    }
}

fn field(form: &Form, f: &pagebara_forms::Field) -> Field {
    let options: Vec<String> = f.options.iter().map(|o| o.display.clone()).collect();
    let selected = f.selection();
    let value = match &f.value {
        FieldValue::Choices(_) if !selected.is_empty() => selected
            .iter()
            .filter_map(|&i| options.get(i).cloned())
            .collect::<Vec<_>>()
            .join(", "),
        FieldValue::State(_) => f.export_value().unwrap_or_default(),
        _ if f.kind == FieldKind::Text => form.display_text(f).text,
        v => v.display(),
    };
    Field {
        name: f.name.clone(),
        label: f.alternate_name.clone().unwrap_or_else(|| f.name.clone()),
        kind: f.kind,
        value,
        read_only: f.is_read_only(),
        multiline: f.is_multiline(),
        password: f.is_password(),
        max_len: f.max_len,
        comb: f.is_comb().then_some(f.max_len).flatten(),
        options,
        option_values: f.options.iter().map(|o| o.export.clone()).collect(),
        selected,
        multi_select: f.is_multi_select(),
        editable: f.is_editable_combo(),
        checked: f.is_checked(),
        font_size: f.da.as_deref().map_or(0.0, |da| {
            pagebara_forms::DefaultAppearance::parse(da.as_bytes()).size
        }),
        widgets: f
            .widgets
            .iter()
            .filter_map(|w| {
                Some(Widget {
                    id: w.id,
                    page: w.page?,
                    rect: [w.rect.x0, w.rect.y0, w.rect.x1, w.rect.y1],
                    on: w.is_on(),
                    export: w.export.clone().or_else(|| w.on_state.clone()),
                })
            })
            .collect(),
    }
}

fn done(r: pagebara_forms::Result<pagebara_forms::Change>) -> FormResult {
    r.map(FormChange::from).map_err(FormError::from)
}

impl FormSession for Form {
    fn fields(&self) -> Vec<Field> {
        self.fields.iter().map(|f| field(self, f)).collect()
    }

    fn field(&self, name: &str) -> Option<Field> {
        Form::field(self, name).map(|f| field(self, f))
    }

    fn field_of_widget(&self, widget: ObjRef) -> Option<Field> {
        Form::field_of_widget(self, widget).map(|f| field(self, f))
    }

    fn tab_order(&self, page: usize) -> Vec<ObjRef> {
        Form::tab_order(self, page)
    }

    fn set_text(&mut self, doc: &mut Document, name: &str, value: &str) -> FormResult {
        done(Form::set_text(self, doc, name, value))
    }

    fn set_checked(&mut self, doc: &mut Document, name: &str, on: bool) -> FormResult {
        done(Form::set_checked(self, doc, name, on))
    }

    fn select_radio(&mut self, doc: &mut Document, name: &str, value: Option<&str>) -> FormResult {
        done(Form::select_radio(self, doc, name, value))
    }

    fn select_choices(&mut self, doc: &mut Document, name: &str, values: &[&str]) -> FormResult {
        done(Form::select_choices(self, doc, name, values))
    }

    fn click(&mut self, doc: &mut Document, widget: ObjRef) -> FormResult {
        done(Form::click(self, doc, widget))
    }

    fn reset(&mut self, doc: &mut Document) -> FormResult {
        done(Form::reset(self, doc, None))
    }

    fn recalculate(&mut self, doc: &mut Document) -> FormResult {
        done(Form::recalculate(self, doc))
    }

    fn generate_appearances(&mut self, doc: &mut Document, force: bool) -> FormResult {
        done(Form::generate_appearances(self, doc, force))
    }

    fn flatten(&mut self, doc: &mut Document) -> FormResult {
        done(Form::flatten(self, doc, None))
    }

    fn script_warnings(&self) -> Vec<String> {
        let mut out: Vec<String> = Form::script_warnings(self)
            .into_iter()
            .map(|w| {
                let what = w
                    .triggers
                    .iter()
                    .map(|t| trigger_name(t))
                    .collect::<Vec<_>>()
                    .join(", ");
                tr!(
                    "“{}” has scripts ({}) that Pagebara does not run: what you type there is not checked or formatted for you.",
                    w.field,
                    what
                )
            })
            .collect();
        if self.document_scripts > 0 {
            out.push(tr!(
                "The document has scripts, which Pagebara does not run."
            ));
        }
        out
    }

    fn xfa(&self) -> Xfa {
        self.xfa
    }

    fn signatures(&self, doc: &Document) -> Vec<SignatureInfo> {
        Form::signatures(self, doc)
    }

    fn can_fill(&self, doc: &Document) -> bool {
        self.has_form()
            && self.xfa != Xfa::XfaOnly
            && doc.permissions().fill_forms
            && self.fields.iter().any(|f| {
                !f.is_read_only() && !matches!(f.kind, FieldKind::Signature | FieldKind::Unknown)
            })
    }

    fn boxed_clone(&self) -> Box<dyn FormSession> {
        Box::new(self.clone())
    }

    fn props(&self, doc: &Document, name: &str) -> Option<FieldProps> {
        Form::props(self, doc, name)
    }

    fn set_props(&mut self, doc: &mut Document, name: &str, props: &FieldProps) -> FormResult {
        done(Form::set_props(self, doc, name, props))
    }

    fn add_field(
        &mut self,
        doc: &mut Document,
        field: &NewField,
    ) -> Result<(FormChange, String), FormError> {
        Form::add_field(self, doc, field)
            .map(|(c, n)| (c.into(), n))
            .map_err(FormError::from)
    }

    fn add_widget(
        &mut self,
        doc: &mut Document,
        name: &str,
        page: usize,
        rect: [f64; 4],
        export: Option<&str>,
    ) -> Result<(FormChange, ObjRef), FormError> {
        Form::add_widget(self, doc, name, page, rect_of(rect), export)
            .map(|(c, w)| (c.into(), w))
            .map_err(FormError::from)
    }

    fn remove_field(&mut self, doc: &mut Document, name: &str) -> FormResult {
        done(Form::remove_field(self, doc, name))
    }

    fn remove_widget(&mut self, doc: &mut Document, widget: ObjRef) -> FormResult {
        done(Form::remove_widget(self, doc, widget))
    }

    fn rename(&mut self, doc: &mut Document, old: &str, new: &str) -> FormResult {
        done(Form::rename(self, doc, old, new))
    }

    fn duplicate(
        &mut self,
        doc: &mut Document,
        name: &str,
        dx: f64,
        dy: f64,
    ) -> Result<(FormChange, String), FormError> {
        Form::duplicate(self, doc, name, dx, dy)
            .map(|(c, n)| (c.into(), n))
            .map_err(FormError::from)
    }

    fn copy_to_pages(&mut self, doc: &mut Document, name: &str, pages: &[usize]) -> FormResult {
        done(Form::copy_to_pages(self, doc, name, pages))
    }

    fn set_rects(&mut self, doc: &mut Document, rects: &[(ObjRef, [f64; 4])]) -> FormResult {
        let r: Vec<(ObjRef, pagebara_annot::Rect)> =
            rects.iter().map(|(w, r)| (*w, rect_of(*r))).collect();
        done(Form::set_rects(self, doc, &r))
    }

    fn set_export(&mut self, doc: &mut Document, widget: ObjRef, value: &str) -> FormResult {
        done(Form::set_export(self, doc, widget, value))
    }

    fn set_button_icon(
        &mut self,
        doc: &mut Document,
        name: &str,
        icon: Option<(ObjRef, u32, u32)>,
    ) -> FormResult {
        done(Form::set_button_icon(self, doc, name, icon))
    }

    fn tab_mode(&self, page: usize) -> TabOrder {
        Form::tab_mode(self, page)
    }

    fn set_tab_order(&mut self, doc: &mut Document, page: usize, order: &TabOrder) -> FormResult {
        done(Form::set_tab_order(self, doc, page, order))
    }

    fn name_taken(&self, name: &str) -> bool {
        Form::name_taken(self, name)
    }

    fn default_name(&self, kind: FieldKind, date: bool) -> String {
        Form::default_name(self, kind, date)
    }

    fn unique_name(&self, base: &str) -> String {
        Form::unique_name(self, base)
    }

    fn button_action(&self, doc: &Document, name: &str) -> ButtonAction {
        Form::button_action(self, doc, name)
    }

    fn reset_fields(&mut self, doc: &mut Document, names: &[&str]) -> FormResult {
        let names = (!names.is_empty()).then_some(names);
        done(Form::reset(self, doc, names))
    }

    fn clear_all(&mut self, doc: &mut Document) -> FormResult {
        let names: Vec<String> = self
            .fields
            .iter()
            .filter(|f| !f.is_read_only() && !f.value.is_none())
            .map(|f| f.name.clone())
            .collect();
        let mut change = pagebara_forms::Change::default();
        for n in names {
            change.merge(Form::clear(self, doc, &n).map_err(FormError::from)?);
        }
        Ok(change.into())
    }

    fn export_fdf(&self, file: Option<&str>) -> Vec<u8> {
        pagebara_fdf::export_fdf(self, file)
    }

    fn export_xfdf(&self, doc: &Document, file: Option<&str>, annotations: bool) -> String {
        pagebara_fdf::export_xfdf(self, doc, file, annotations)
    }

    fn import_data(&mut self, doc: &mut Document, data: &[u8]) -> Result<ImportSummary, FormError> {
        if data.trim_ascii_start().starts_with(b"<") {
            imported(pagebara_fdf::import_xfdf(
                self,
                doc,
                &String::from_utf8_lossy(data),
            ))
        } else {
            imported(pagebara_fdf::import_fdf(self, doc, data))
        }
    }
}

fn rect_of(r: [f64; 4]) -> pagebara_annot::Rect {
    pagebara_annot::Rect::new(r[0], r[1], r[2], r[3])
}

fn imported(
    r: pagebara_fdf::Result<pagebara_fdf::ImportReport>,
) -> Result<ImportSummary, FormError> {
    let r = r.map_err(|e| match e {
        pagebara_fdf::Error::Form(f) => FormError::from(f),
        pagebara_fdf::Error::BadData(what) => FormError {
            text: tr!("Not a valid {} file.", what),
            permission: false,
        },
    })?;
    Ok(ImportSummary {
        change: r.change.into(),
        unknown: r.unknown,
        rejected: r
            .rejected
            .into_iter()
            .map(|(n, e)| format!("{n}: {e}"))
            .collect(),
        annotations: r.annotations,
    })
}

fn trigger_name(t: &str) -> String {
    match t {
        "K" => tr!("keystroke"),
        // TRANSLATORS: a kind of form field script (it formats the value)
        "F" => tr!("format"),
        "V" => tr!("validate"),
        "C" => tr!("calculate"),
        // TRANSLATORS: a kind of form field script (run on a mouse action)
        "A" | "U" | "D" => tr!("action"),
        "E" | "X" | "Fo" | "Bl" => tr!("focus"),
        t => t.to_owned(),
    }
}

/// The document as it should look: when the form asks readers to build field appearances
/// (or some widgets have none), a copy with them built. The document itself is unchanged.
pub fn for_display(doc: &Document) -> Document {
    let catalog = doc.get_in(doc.trailer(), b"Root");
    let has_form = catalog
        .as_dict()
        .is_some_and(|c| c.get(b"AcroForm").is_some());
    if !has_form {
        return doc.clone();
    }
    let mut form = Form::load(doc);
    let missing = form
        .fields
        .iter()
        .flat_map(|f| &f.widgets)
        .any(|w| !w.has_appearance);
    if !form.need_appearances && !missing {
        return doc.clone();
    }
    let mut shown = doc.clone();
    match form.generate_appearances(&mut shown, false) {
        Ok(_) => shown,
        Err(_) => doc.clone(),
    }
}

/// One line about a signature field for the forms panel before its signature is verified
/// (see [`crate::signing::field_status`] for after).
pub fn signature_status(s: &SignatureInfo) -> String {
    if !s.signed {
        return tr!("Signature: not signed yet");
    }
    let who = s
        .signer
        .clone()
        .or_else(|| s.certificate_names.first().cloned())
        .map(|n| format!(" · {}", tr!("by {}", n)))
        .unwrap_or_default();
    let changed = match &s.byte_range {
        Some(b) if b.later_revisions > 0 => format!(" · {}", tr!("changed after signing")),
        Some(b) if !b.well_formed => format!(" · {}", tr!("byte range is malformed")),
        _ => String::new(),
    };
    format!("{}{who}{changed}", tr!("Signature: checking…"))
}

#[cfg(test)]
mod tests;
