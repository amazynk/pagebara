//! Filling in forms: text typed in place over the field (multiline and comb fields too),
//! check boxes and radio buttons toggled and options picked on the page, Tab between
//! fields, reset, flattening on save, and the forms panel. Every change is one undoable
//! edit.

use std::rc::Rc;

use pagebara_annot::{Kind, Point};
use pagebara_app_core::annots;
use pagebara_app_core::forms::{
    self, ButtonAction, Field, FieldKind, FormChange, FormError, FormSession, Xfa,
};
use pagebara_i18n::tr;
use pagebara_syntax::{Document, ObjRef};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::document::EditError;
use super::{App, State, Tab};
use crate::{AppState, AppWindow, FieldRow, OverlayItem, ViewState};

/// Editor kinds, as the page view knows them.
const TEXT: i32 = 0;
const MULTILINE: i32 = 1;
const COMB: i32 = 2;
const COMBO: i32 = 3;
const LIST: i32 = 4;
const BUTTON: i32 = 5;

/// The field being filled in.
#[derive(Clone, Debug)]
pub struct Editing {
    pub name: String,
    pub widget: ObjRef,
    pub page: usize,
    /// The widget in user space.
    pub rect: [f64; 4],
    pub kind: i32,
    pub max_len: Option<usize>,
    pub font_size: f64,
    pub multi: bool,
    pub option_values: Vec<String>,
    pub selected: Vec<usize>,
}

/// Per document: the field being filled in and the save options.
#[derive(Debug, Default)]
pub struct FormUi {
    pub editing: Option<Editing>,
    pub flatten_on_save: bool,
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    let vs = ui.global::<ViewState>();
    let a = app.clone();
    st.on_field_clicked(move |i| a.act(move |app, s, ui| app.field_clicked(s, ui, i as usize)));
    let a = app.clone();
    st.on_form_action(move |what| {
        let what = what.to_string();
        a.act(move |app, s, ui| app.form_action(s, ui, &what))
    });
    let a = app.clone();
    vs.on_field_done(move |commit| a.act(move |app, s, ui| app.field_done(s, ui, commit)));
    let a = app.clone();
    vs.on_field_tab(move |back| a.act(move |app, s, ui| app.field_tab(s, ui, back)));
    let a = app.clone();
    vs.on_field_pick(move |i| a.act(move |app, s, ui| app.field_pick(s, ui, i as usize)));
    let a = app.clone();
    vs.on_field_edited(move |text| {
        let text = text.to_string();
        a.act(move |app, s, ui| app.field_edited(s, ui, &text))
    });
}

fn edit_error(e: FormError) -> EditError {
    EditError {
        text: e.text,
        permission: e.permission,
    }
}

fn kind_label(f: &Field) -> String {
    match f.kind {
        // TRANSLATORS: the kind of a form field, in the forms panel
        FieldKind::Text => tr!("Text"),
        FieldKind::CheckBox => tr!("Check box"),
        FieldKind::Radio => tr!("Radio buttons"),
        // TRANSLATORS: the kind of a form field, in the forms panel
        FieldKind::PushButton => tr!("Button"),
        FieldKind::ComboBox => tr!("Drop-down list"),
        // TRANSLATORS: the kind of a form field (a list box), in the forms panel
        FieldKind::ListBox => tr!("List"),
        // TRANSLATORS: the kind of a form field, in the forms panel
        FieldKind::Signature => tr!("Signature"),
        FieldKind::Unknown => tr!("Field"),
    }
}

/// The text size for typing into a field of height `h` points.
fn editor_font(f: &Field, h: f64) -> f64 {
    match f.font_size {
        s if s > 0.0 => s,
        _ if f.multiline => 10.0,
        _ => (h * 0.6).clamp(6.0, 14.0),
    }
}

/// The forms panel: fields, why the form cannot be filled, script warnings.
pub fn sync_fields(s: &mut State, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    if st.get_tool() != 5 {
        return;
    }
    let backend = s.forms.clone();
    let verified = super::signing::field_lines(s);
    let Some(t) = s.tab() else { return };
    let flatten = t.form_ui.flatten_on_save;
    let doc = t.session.doc().clone();
    let form = t.form(&*backend);
    let can_fill = form.can_fill(&doc);
    let fields = form.fields();
    let sigs = form.signatures(&doc);
    let notice = match form.xfa() {
        Xfa::XfaOnly => tr!(
            "This is an XFA form, which Pagebara cannot fill in yet: it shows the pages as they are, \
             without the form."
        ),
        Xfa::WithAcroForm => tr!(
            "This form also has an XFA version. Pagebara fills in the standard fields and removes \
             the XFA version on the first change, so every app shows the same values."
        ),
        Xfa::None if !fields.is_empty() && !doc.permissions().fill_forms => {
            tr!("This document's security settings do not allow filling in forms.")
        }
        Xfa::None => String::new(),
    };
    let warnings: Vec<SharedString> = form
        .script_warnings()
        .into_iter()
        .map(SharedString::from)
        .collect();
    let rows: Vec<FieldRow> = fields
        .iter()
        .map(|f| {
            let value = match f.kind {
                FieldKind::Signature => verified.get(&f.name).cloned().unwrap_or_else(|| {
                    sigs.iter()
                        .find(|s| s.field == f.name)
                        .map(forms::signature_status)
                        .unwrap_or_else(|| tr!("Signature: not signed yet"))
                }),
                _ if f.password && !f.value.is_empty() => "••••••".into(),
                _ => f.value.replace('\n', " "),
            };
            FieldRow {
                name: f.label.as_str().into(),
                kind: kind_label(f).into(),
                value: value.into(),
                page: f.widgets.first().map_or(0, |w| w.page as i32 + 1),
            }
        })
        .collect();
    st.set_can_fill(can_fill);
    st.set_form_notice(notice.into());
    st.set_form_warnings(ModelRc::new(VecModel::from(warnings)));
    st.set_flatten_forms(flatten);
    st.set_fields(ModelRc::new(VecModel::from(rows)));
}

/// Places the field editor over its widget (after scrolling or zooming).
pub fn place_editor(tab: &Tab, ui: &AppWindow) {
    let vs = ui.global::<ViewState>();
    let Some(e) = &tab.form_ui.editing else {
        return;
    };
    let (Some(pl), Some(g)) = (tab.view.placement(e.page), tab.session.geoms.get(e.page)) else {
        vs.set_field_shown(false);
        return;
    };
    let [x, y, w, h] = g.rect_to_view(e.rect);
    vs.set_field_box(OverlayItem {
        di: pl.di,
        by: pl.by + y as f32,
        bx: pl.bx + x as f32,
        w: w as f32,
        h: h as f32,
        font_size: e.font_size as f32,
        ..Default::default()
    });
    vs.set_field_shown(true);
}

impl App {
    /// Runs a form change as one undoable edit and keeps the form in step.
    pub(super) fn form_edit(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        label: &str,
        f: impl FnOnce(&mut dyn FormSession, &mut Document) -> forms::FormResult,
    ) -> Option<FormChange> {
        let backend = s.forms.clone();
        let mut form = s.tab()?.form(&*backend).boxed_clone();
        let change = self.edit(s, ui, label, |d| f(&mut *form, d).map_err(edit_error))?;
        if let Some(t) = s.tab() {
            t.set_form(form);
        }
        if !change.missing.is_empty() {
            let chars: String = change.missing.iter().take(8).collect();
            self.message(
                s,
                &tr!(
                    "The field's font cannot show “{}”; other apps may draw boxes.",
                    chars
                ),
            );
        } else if change.xfa_removed {
            self.message(s, &tr!("The XFA version of this form was removed."));
        }
        sync_fields(s, ui);
        Some(change)
    }

    /// A press on the page with the forms tool; true when it hit a field.
    pub fn field_at(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        page: usize,
        pt: Point,
    ) -> bool {
        let Some(t) = s.tab() else { return false };
        let list = t.annots(page);
        let hit = list
            .iter()
            .rev()
            .find(|a| matches!(a.kind, Kind::Widget { .. }) && annots::hits(a, pt, 0.0))
            .and_then(|a| a.id);
        match hit {
            Some(w) => {
                self.open_field(s, ui, w, true);
                true
            }
            None => false,
        }
    }

    /// Starts filling in the field of `widget`: text and choices open the editor over the
    /// widget, buttons get a focus frame (and with `toggle`, a click on them toggles).
    fn open_field(self: &Rc<Self>, s: &mut State, ui: &AppWindow, widget: ObjRef, toggle: bool) {
        self.field_close(s, ui, true);
        let backend = s.forms.clone();
        let Some(t) = s.tab() else { return };
        let doc = t.session.doc().clone();
        let form = t.form(&*backend);
        let Some(f) = form.field_of_widget(widget) else {
            return;
        };
        let can_fill = form.can_fill(&doc);
        if f.kind == FieldKind::Signature {
            let name = f.name.clone();
            if let Some(line) = super::signing::field_lines(s).remove(&name) {
                return self.message(s, &line);
            }
            let Some(t) = s.tab() else { return };
            let form = t.form(&*backend);
            let status = form
                .signatures(&doc)
                .iter()
                .find(|s| s.field == f.name)
                .map(forms::signature_status)
                .unwrap_or_else(|| tr!("Signature: not signed yet"));
            return self.message(s, &status);
        }
        if f.kind == FieldKind::PushButton {
            let name = f.name.clone();
            return self.press_button(s, ui, &name);
        }
        if !can_fill {
            return self.message(
                s,
                &tr!("This form cannot be filled in; the forms panel says why."),
            );
        }
        if f.read_only {
            return self.message(s, &tr!("“{}” is read-only.", f.label));
        }
        let Some(w) = f.widgets.iter().find(|w| w.id == widget).cloned() else {
            return;
        };
        let kind = match f.kind {
            FieldKind::Text if f.comb.is_some() => COMB,
            FieldKind::Text if f.multiline => MULTILINE,
            FieldKind::Text => TEXT,
            FieldKind::ComboBox => COMBO,
            FieldKind::ListBox => LIST,
            _ => BUTTON,
        };
        let h = (w.rect[3] - w.rect[1]).abs();
        let editing = Editing {
            name: f.name.clone(),
            widget,
            page: w.page,
            rect: w.rect,
            kind,
            max_len: f.max_len,
            font_size: editor_font(&f, h),
            multi: f.multi_select,
            option_values: f.option_values.clone(),
            selected: f.selected.clone(),
        };
        let vs = ui.global::<ViewState>();
        let text = if kind == BUTTON {
            String::new()
        } else {
            f.value.clone()
        };
        vs.set_field_kind(kind);
        vs.set_field_cells(f.comb.unwrap_or(0) as i32);
        vs.set_field_password(f.password);
        vs.set_field_typed(f.editable);
        vs.set_field_text(text.as_str().into());
        set_chars(&vs, &text);
        let options: Vec<SharedString> = f.options.iter().map(SharedString::from).collect();
        vs.set_field_options(ModelRc::new(VecModel::from(options)));
        set_selected(&vs, f.options.len(), &f.selected);
        t.form_ui.editing = Some(editing);
        place_editor(t, ui);
        vs.set_field_editing(true);
        vs.set_field_serial(vs.get_field_serial().wrapping_add(1));
        self.spell_field_text(s, ui, &text);
        self.scroll_to_field(s, ui);
        if toggle && kind == BUTTON {
            self.field_pick(s, ui, 0);
        }
        self.overlay_changed(s, ui);
    }

    /// Keeps the field being filled in on screen.
    fn scroll_to_field(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab() else { return };
        let Some(e) = &t.form_ui.editing else { return };
        let Some(g) = t.session.geoms.get(e.page) else {
            return;
        };
        let [x, y, w, h] = g.rect_to_view(e.rect);
        let (_, vh) = t.view.view.viewport.size();
        let shown = t
            .view
            .page_to_screen(e.page, x, y)
            .zip(t.view.page_to_screen(e.page, x + w, y + h))
            .is_some_and(|((_, top), (_, bottom))| top >= 0.0 && bottom <= vh);
        if !shown {
            let page = e.page;
            t.view.glide_to(page, (y - 80.0).max(0.0), 0.0);
            self.view_op(s, ui, |_| {});
        }
    }

    /// Closes the field editor; with `commit`, the typed text becomes the field's value.
    pub fn field_done(self: &Rc<Self>, s: &mut State, ui: &AppWindow, commit: bool) {
        let editing = ui.global::<ViewState>().get_field_editing();
        self.field_close(s, ui, commit);
        // Focus goes back to the pages only from the field, not from where the tool changed.
        if editing {
            ui.invoke_focus_view();
        }
    }

    /// As [`App::field_done`], leaving the keyboard focus where it is.
    fn field_close(self: &Rc<Self>, s: &mut State, ui: &AppWindow, commit: bool) {
        let vs = ui.global::<ViewState>();
        let Some(e) = s.tab().and_then(|t| t.form_ui.editing.take()) else {
            vs.set_field_editing(false);
            return;
        };
        vs.set_field_editing(false);
        let typed = matches!(e.kind, TEXT | MULTILINE | COMB) || vs.get_field_typed();
        if commit && typed {
            let text = vs.get_field_text().to_string();
            let backend = s.forms.clone();
            let current = s
                .tab()
                .and_then(|t| t.form(&*backend).field(&e.name))
                .map(|f| f.value);
            if current.as_deref() != Some(text.as_str()) {
                let name = e.name.clone();
                self.form_edit(s, ui, &tr!("Fill in “{}”", name), |f, d| {
                    f.set_text(d, &name, &text)
                });
            }
        }
        self.overlay_changed(s, ui);
    }

    /// Hides the field editor without changing anything (undo, other tab).
    pub fn field_cancel(&self, s: &mut State, ui: &AppWindow) {
        for t in &mut s.tabs {
            t.form_ui.editing = None;
        }
        ui.global::<ViewState>().set_field_editing(false);
    }

    /// Tab and Shift+Tab: finish this field and go to the next (previous) one.
    fn field_tab(self: &Rc<Self>, s: &mut State, ui: &AppWindow, back: bool) {
        let Some(from) = s
            .tab_ref()
            .and_then(|t| t.form_ui.editing.as_ref().map(|e| e.widget))
        else {
            return;
        };
        self.field_close(s, ui, true);
        let backend = s.forms.clone();
        let Some(t) = s.tab() else { return };
        let n = t.session.page_count();
        let form = t.form(&*backend);
        let fillable = |w: &ObjRef| {
            form.field_of_widget(*w).is_some_and(|f| {
                !f.read_only
                    && !matches!(
                        f.kind,
                        FieldKind::Signature | FieldKind::PushButton | FieldKind::Unknown
                    )
            })
        };
        let order: Vec<ObjRef> = (0..n)
            .flat_map(|p| form.tab_order(p))
            .filter(fillable)
            .collect();
        let Some(i) = order.iter().position(|w| *w == from) else {
            return;
        };
        let next = if back {
            (i + order.len() - 1) % order.len()
        } else {
            (i + 1) % order.len()
        };
        self.open_field(s, ui, order[next], false);
    }

    /// An option picked in a combo or list box, or Space on a check box or radio button.
    fn field_pick(self: &Rc<Self>, s: &mut State, ui: &AppWindow, i: usize) {
        let Some(e) = s.tab_ref().and_then(|t| t.form_ui.editing.clone()) else {
            return;
        };
        let name = e.name.clone();
        if e.kind == BUTTON {
            let w = e.widget;
            self.form_edit(s, ui, &tr!("Change “{}”", name), |f, d| f.click(d, w));
            return;
        }
        let Some(value) = e.option_values.get(i).cloned() else {
            return;
        };
        if e.multi {
            let mut selected = e.selected.clone();
            match selected.iter().position(|&j| j == i) {
                Some(at) => {
                    selected.remove(at);
                }
                None => selected.push(i),
            }
            selected.sort_unstable();
            let values: Vec<String> = selected
                .iter()
                .filter_map(|&j| e.option_values.get(j).cloned())
                .collect();
            let changed = self.form_edit(s, ui, &tr!("Choose in “{}”", name), |f, d| {
                let v: Vec<&str> = values.iter().map(String::as_str).collect();
                f.select_choices(d, &name, &v)
            });
            if changed.is_some() {
                let vs = ui.global::<ViewState>();
                set_selected(&vs, e.option_values.len(), &selected);
                if let Some(ed) = s.tab().and_then(|t| t.form_ui.editing.as_mut()) {
                    ed.selected = selected;
                }
            }
            return;
        }
        if let Some(t) = s.tab() {
            t.form_ui.editing = None;
        }
        ui.global::<ViewState>().set_field_editing(false);
        ui.invoke_focus_view();
        if e.selected != [i] {
            self.form_edit(s, ui, &tr!("Choose in “{}”", name), |f, d| {
                f.select_choice(d, &name, &value)
            });
        }
        self.overlay_changed(s, ui);
    }

    /// Typing: keeps text within the field's length and fills comb cells.
    pub(super) fn field_edited(self: &Rc<Self>, s: &mut State, ui: &AppWindow, text: &str) {
        let Some(e) = s.tab_ref().and_then(|t| t.form_ui.editing.as_ref()) else {
            return;
        };
        let vs = ui.global::<ViewState>();
        let mut text = text.to_owned();
        if let Some(max) = e.max_len
            && text.chars().count() > max
        {
            text = text.chars().take(max).collect();
            vs.set_field_text(text.as_str().into());
        }
        if e.kind == COMB {
            set_chars(&vs, &text);
        }
    }

    fn field_clicked(self: &Rc<Self>, s: &mut State, ui: &AppWindow, i: usize) {
        let backend = s.forms.clone();
        let Some(t) = s.tab() else { return };
        let fields = t.form(&*backend).fields();
        let Some(f) = fields.get(i) else { return };
        let Some(w) = f.widgets.first().cloned() else {
            return;
        };
        let g = t.session.geoms[w.page];
        let top = g.rect_to_view(w.rect)[1];
        self.jump(s, ui, w.page, Some((top - 60.0).max(0.0)));
        if !matches!(f.kind, FieldKind::Signature | FieldKind::PushButton) {
            self.open_field(s, ui, w.id, false);
        }
    }

    fn form_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str) {
        self.form_data(s, ui, what);
    }

    /// A push button clicked while filling in: its action, done natively.
    fn press_button(self: &Rc<Self>, s: &mut State, ui: &AppWindow, name: &str) {
        let backend = s.forms.clone();
        let Some(t) = s.tab() else { return };
        let doc = t.session.doc().clone();
        let (page, n) = (t.view.current_page, t.session.page_count());
        match t.form(&*backend).button_action(&doc, name) {
            ButtonAction::ResetForm => self.form_data(s, ui, "reset"),
            ButtonAction::Uri(u) => self.ask_open_link(s, ui, &u),
            ButtonAction::Named(a) => match a.as_str() {
                "NextPage" => self.jump(s, ui, (page + 1).min(n.saturating_sub(1)), None),
                "PrevPage" => self.jump(s, ui, page.saturating_sub(1), None),
                "FirstPage" => self.jump(s, ui, 0, None),
                "LastPage" => self.jump(s, ui, n.saturating_sub(1), None),
                "Print" => ui.global::<crate::PrintState>().invoke_open(),
                other => self.message(
                    s,
                    &tr!("This button runs “{}”, which Pagebara does not do.", other),
                ),
            },
            ButtonAction::None => self.message(s, &tr!("This button does nothing.")),
            ButtonAction::Other => self.message(
                s,
                &tr!("This button runs a script, which Pagebara does not run."),
            ),
        }
    }

    /// The "flatten fields when saving" option changed.
    pub fn flatten_forms_changed(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let on = ui.global::<AppState>().get_flatten_forms();
        if let Some(t) = s.tab() {
            t.form_ui.flatten_on_save = on;
        }
    }
}

fn set_chars(vs: &ViewState, text: &str) {
    let chars: Vec<SharedString> = text.chars().map(|c| c.to_string().into()).collect();
    vs.set_field_chars(ModelRc::new(VecModel::from(chars)));
}

fn set_selected(vs: &ViewState, n: usize, selected: &[usize]) {
    let flags: Vec<bool> = (0..n).map(|i| selected.contains(&i)).collect();
    vs.set_field_selected(ModelRc::new(VecModel::from(flags)));
}
