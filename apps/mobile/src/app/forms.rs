//! Filling in forms with the Forms tool: the fields are outlined; a tap on a check box or
//! radio button toggles it, on a text field opens an editor above the keyboard, on a choice
//! field a list of its options.

use std::rc::Rc;

use pagebara_annot::{Kind, Point};
use pagebara_app_core::annots;
use pagebara_app_core::forms::{FieldKind, FormChange, FormResult, FormSession};
use pagebara_i18n::tr;
use pagebara_syntax::{Document, ObjRef};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{App, State};
use crate::{AppState, MobileWindow, Sheet};

/// The field being filled in.
#[derive(Debug, Default)]
pub struct FieldEdit {
    name: String,
    choice: bool,
    option_values: Vec<String>,
}

pub fn wire(app: &Rc<App>, ui: &MobileWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_field_done(move |commit| a.act(move |app, s, ui| app.field_done(s, ui, commit)));
    let a = app.clone();
    st.on_field_pick(move |i| a.act(move |app, s, ui| app.field_pick(s, ui, i.max(0) as usize)));
}

impl App {
    /// Says what the Forms tool can do here.
    pub fn forms_tool(self: &Rc<Self>, s: &mut State, _ui: &MobileWindow) {
        let backend = s.forms.clone();
        let Some(d) = &mut s.doc else { return };
        let doc = d.session.doc().clone();
        let form = d.form(&*backend);
        let n = form.fields().iter().filter(|f| !f.read_only).count();
        let text = if n == 0 {
            tr!("This document has no fields to fill in.")
        } else if !form.can_fill(&doc) {
            tr!("This form cannot be filled in here.")
        } else {
            tr!("Tap a field to fill it in.")
        };
        self.message(s, &text);
    }

    /// Runs a form change as one undoable edit and keeps the form in step.
    fn form_edit(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        label: &str,
        f: impl FnOnce(&mut dyn FormSession, &mut Document) -> FormResult,
    ) -> Option<FormChange> {
        let backend = s.forms.clone();
        let d = s.doc.as_mut()?;
        let mut form = d.form(&*backend).boxed_clone();
        let result = d.session.edit(label, |doc| f(&mut *form, doc));
        match result {
            Ok((change, _)) => {
                d.set_form(form);
                if !change.missing.is_empty() {
                    let chars: String = change.missing.iter().take(8).collect();
                    self.message(s, &tr!("The field's font cannot show “{}”.", chars));
                }
                self.after_edit(s, ui);
                Some(change)
            }
            Err(e) => {
                self.message(s, &e.text);
                None
            }
        }
    }

    /// A tap with the Forms tool at a point of a page.
    pub fn field_at(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, page: usize, pt: Point) {
        let Some(d) = &mut s.doc else { return };
        let list = d.annots(page);
        let hit = list
            .iter()
            .rev()
            .find(|a| matches!(a.kind, Kind::Widget { .. }) && annots::hits(a, pt, 4.0))
            .and_then(|a| a.id);
        match hit {
            Some(w) => self.open_field(s, ui, w),
            None => self.refresh(s, ui),
        }
    }

    fn open_field(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, widget: ObjRef) {
        let backend = s.forms.clone();
        let Some(d) = &mut s.doc else { return };
        let doc = d.session.doc().clone();
        let form = d.form(&*backend);
        let Some(f) = form.field_of_widget(widget) else {
            return;
        };
        if !form.can_fill(&doc) {
            return self.message(s, &tr!("This form cannot be filled in here."));
        }
        if f.read_only {
            return self.message(s, &tr!("“{}” cannot be changed.", f.label));
        }
        let st = ui.global::<AppState>();
        match f.kind {
            FieldKind::CheckBox | FieldKind::Radio => {
                self.form_edit(s, ui, &tr!("Fill in “{}”", f.label), |form, doc| {
                    form.click(doc, widget)
                });
            }
            FieldKind::Text => {
                s.fields = FieldEdit {
                    name: f.name.clone(),
                    ..FieldEdit::default()
                };
                st.set_field_label(f.label.as_str().into());
                st.set_field_text(f.value.as_str().into());
                st.set_field_multiline(f.multiline);
                st.set_field_editing(true);
                let value = f.value.clone();
                self.spell_field_opened(s, ui, &value);
            }
            FieldKind::ComboBox | FieldKind::ListBox => {
                s.fields = FieldEdit {
                    name: f.name.clone(),
                    choice: true,
                    option_values: f.option_values.clone(),
                };
                let options: Vec<SharedString> = f.options.iter().map(SharedString::from).collect();
                st.set_field_label(f.label.as_str().into());
                st.set_field_options(ModelRc::new(VecModel::from(options)));
                st.set_field_selected(f.selected.first().map_or(-1, |&i| i as i32));
                st.set_sheet(Sheet::Choices);
            }
            FieldKind::Signature => self.message(
                s,
                &tr!("Signature fields need a digital ID, which the desktop app can add."),
            ),
            _ => self.message(s, &tr!("Buttons run scripts, which Pagebara does not run.")),
        }
    }

    pub fn field_done(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, commit: bool) {
        let st = ui.global::<AppState>();
        let editing = st.get_field_editing();
        st.set_field_editing(false);
        self.spell_field_opened(s, ui, "");
        if st.get_sheet() == Sheet::Choices {
            st.set_sheet(Sheet::None);
        }
        let edit = std::mem::take(&mut s.fields);
        if !commit || !editing || edit.choice || edit.name.is_empty() {
            return;
        }
        let text = st.get_field_text().to_string();
        self.form_edit(s, ui, &tr!("Fill in field"), |form, doc| {
            form.set_text(doc, &edit.name, &text)
        });
    }

    fn field_pick(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, i: usize) {
        let st = ui.global::<AppState>();
        st.set_sheet(Sheet::None);
        let edit = std::mem::take(&mut s.fields);
        let Some(value) = edit.option_values.get(i).cloned() else {
            return;
        };
        self.form_edit(s, ui, &tr!("Fill in field"), |form, doc| {
            form.select_choice(doc, &edit.name, &value)
        });
    }
}
