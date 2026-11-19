//! Preparing forms on the phone: a field type chosen in the bar is placed where the page is
//! tapped; a tap on a field selects it, a drag moves it and its lower right handle resizes
//! it; the bar duplicates, deletes and adds radio buttons, the properties sheet renames
//! and changes it; fields found on the page are added at once. The Tools sheet imports and
//! exports form data, resets and clears the form. Every change is one undoable step.

use std::path::Path;
use std::rc::Rc;

use pagebara_app_core::form_prep::{self, Suggestion, Tool as FieldTool};
use pagebara_app_core::forms::{ChoiceOption, FieldKind, FormError, FormSession};
use pagebara_i18n::{tr, trn};
use pagebara_syntax::{Document, ObjRef};
use slint::ComponentHandle;

use super::marks::{at, view_box};
use super::{App, Doc, State};
use crate::{AppState, Mark, MobileWindow, Sheet, Tool};

/// How far (screen pixels) from the resize handle a touch takes it.
const HANDLE_REACH: f64 = 26.0;
/// The smallest a field gets, in points.
const MIN_SIZE: f64 = 8.0;

/// The selected field: its name and the widget tapped.
#[derive(Clone, Debug, PartialEq)]
pub struct PrepSel {
    pub name: String,
    pub widget: ObjRef,
    pub page: usize,
}

#[derive(Debug, Default)]
pub struct PrepState {
    pub sel: Option<PrepSel>,
    /// Fields found on a page, waiting to be added.
    pub found: Vec<Suggestion>,
}

/// A drag of the selected field: its box (displayed points) at the start and now.
#[derive(Clone, Debug)]
pub struct PrepDrag {
    pub sel: PrepSel,
    pub resize: bool,
    pub from: [f64; 4],
    pub now: [f64; 4],
}

pub fn wire(app: &Rc<App>, ui: &MobileWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_form_tool(move |name| {
        let name = name.to_string();
        a.act(move |app, s, ui| app.form_tool(s, ui, &name))
    });
    let a = app.clone();
    st.on_prep_action(move |what| {
        let what = what.to_string();
        a.act(move |app, s, ui| app.prep_action(s, ui, &what))
    });
    let a = app.clone();
    st.on_prep_type_changed(move || {
        a.act(|app, s, ui| {
            let t = ui.global::<AppState>().get_prep_type();
            if let Some(tool) = usize::try_from(t).ok().and_then(|i| FieldTool::ALL.get(i)) {
                app.message(s, &tap_hint(*tool));
            }
        })
    });
    let a = app.clone();
    st.on_prop_apply(move || a.act(|app, s, ui| app.prop_apply(s, ui)));
}

fn kind_label(k: FieldKind) -> String {
    match k {
        FieldKind::Text => tr!("Text field"),
        FieldKind::CheckBox => tr!("Check box"),
        FieldKind::Radio => tr!("Radio buttons"),
        FieldKind::ComboBox => tr!("Drop-down list"),
        FieldKind::ListBox => tr!("List box"),
        FieldKind::PushButton => tr!("Button"),
        FieldKind::Signature => tr!("Signature field"),
        FieldKind::Unknown => tr!("Field"),
    }
}

fn tap_hint(t: FieldTool) -> String {
    match t {
        FieldTool::Text => tr!("Tap the page where the text field goes."),
        FieldTool::Date => tr!("Tap the page where the date field goes."),
        FieldTool::CheckBox => tr!("Tap the page where the check box goes."),
        FieldTool::Radio => tr!("Tap the page where the radio button goes."),
        FieldTool::ComboBox => tr!("Tap the page where the drop-down list goes."),
        FieldTool::ListBox => tr!("Tap the page where the list box goes."),
        FieldTool::Button => tr!("Tap the page where the button goes."),
        FieldTool::Signature => tr!("Tap the page where the signature field goes."),
    }
}

/// The undo label of placing a field.
fn add_label(t: FieldTool) -> String {
    match t {
        FieldTool::Text => tr!("Add text field"),
        FieldTool::Date => tr!("Add date field"),
        FieldTool::CheckBox => tr!("Add check box"),
        FieldTool::Radio => tr!("Add radio button"),
        FieldTool::ComboBox => tr!("Add drop-down list"),
        FieldTool::ListBox => tr!("Add list box"),
        FieldTool::Button => tr!("Add button"),
        FieldTool::Signature => tr!("Add signature field"),
    }
}

/// The selected field's box in displayed points.
fn sel_box(
    d: &mut Doc,
    backend: &dyn pagebara_app_core::forms::FormBackend,
    sel: &PrepSel,
) -> Option<[f64; 4]> {
    let g = *d.session.geoms.get(sel.page)?;
    let f = d.form(backend).field(&sel.name)?;
    let w = f.widgets.iter().find(|w| w.id == sel.widget)?;
    Some(view_box(&g, w.rect))
}

/// Marks for Prepare form: every field outlined, the selected one framed with handles,
/// fields found shown haloed.
pub fn marks(
    d: &mut Doc,
    backend: &dyn pagebara_app_core::forms::FormBackend,
    page: usize,
    pl: &pagebara_app_core::view::Placement,
    moving: Option<&PrepDrag>,
    out: &mut Vec<Mark>,
) {
    let Some(g) = d.session.geoms.get(page).copied() else {
        return;
    };
    let sel = d.prep.sel.clone();
    for f in d.form(backend).fields() {
        for w in f.widgets.iter().filter(|w| w.page == page) {
            if sel.as_ref().is_some_and(|s| s.widget == w.id) {
                continue;
            }
            out.push(at(pl, view_box(&g, w.rect), 3));
        }
    }
    for s in d.prep.found.iter().filter(|s| s.page == page) {
        let r = [s.rect.x0, s.rect.y0, s.rect.x1, s.rect.y1];
        out.push(Mark {
            halo: true,
            ..at(pl, view_box(&g, r), 3)
        });
    }
    if let Some(sel) = sel.filter(|s| s.page == page) {
        let r = match moving {
            Some(m) if m.sel.widget == sel.widget => Some(m.now),
            _ => sel_box(d, backend, &sel),
        };
        if let Some(r) = r {
            out.push(at(pl, r, 1));
        }
    }
}

/// Shows the selection in the bar.
pub fn show(s: &mut State, ui: &MobileWindow) {
    let backend = s.forms.clone();
    let st = ui.global::<AppState>();
    let Some(d) = &mut s.doc else { return };
    st.set_prep_found(d.prep.found.len() as i32);
    let sel = d.prep.sel.clone();
    let field = sel
        .as_ref()
        .and_then(|sel| d.form(&*backend).field(&sel.name));
    match field {
        Some(f) => {
            st.set_prep_selected(true);
            st.set_prep_radio(f.kind == FieldKind::Radio);
            // TRANSLATORS: a field's kind and name, e.g. Text field “Name”
            st.set_prep_description(tr!("{} “{}”", kind_label(f.kind), f.name).into());
        }
        None => {
            d.prep.sel = None;
            st.set_prep_selected(false);
            st.set_prep_radio(false);
        }
    }
}

impl App {
    /// Runs a form change as one undoable step and keeps the form in step.
    fn prep_edit<T>(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        label: &str,
        f: impl FnOnce(&mut dyn FormSession, &mut Document) -> Result<T, FormError>,
    ) -> Option<T> {
        let backend = s.forms.clone();
        let d = s.doc.as_mut()?;
        let mut form = d.form(&*backend).boxed_clone();
        let result = d.session.edit(label, |doc| f(&mut *form, doc));
        match result {
            Ok((v, _)) => {
                d.set_form(form);
                self.after_edit(s, ui);
                show(s, ui);
                Some(v)
            }
            Err(e) => {
                self.message(s, &e.text);
                None
            }
        }
    }

    fn form_tool(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, name: &str) {
        let st = ui.global::<AppState>();
        match name {
            "prepare" => {
                st.set_tool(Tool::Prepare);
                self.tool_changed(s, ui);
            }
            "import" => self.platform.pick_form_data(),
            "export-fdf" | "export-xfdf" => self.export_form_data(s, name == "export-xfdf"),
            "reset" => {
                self.prep_edit(s, ui, &tr!("Reset form"), |f, d| f.reset(d));
            }
            "clear" => {
                self.prep_edit(s, ui, &tr!("Clear form"), |f, d| f.clear_all(d));
            }
            _ => {}
        }
    }

    fn export_form_data(self: &Rc<Self>, s: &mut State, xml: bool) {
        let backend = s.forms.clone();
        let stem = Self::stem(s);
        let Some(d) = &mut s.doc else { return };
        let title = d.session.title();
        let doc = d.session.doc().clone();
        let form = d.form(&*backend);
        if form.fields().is_empty() {
            return self.message(s, &tr!("This document has no form fields."));
        }
        let (bytes, ext) = if xml {
            (
                form.export_xfdf(&doc, Some(&title), true).into_bytes(),
                "xfdf",
            )
        } else {
            (form.export_fdf(Some(&title)), "fdf")
        };
        let name = format!("{}.{ext}", stem.unwrap_or_else(|| "form".into()));
        if self.hand_out(s, &name, &bytes).is_some() {
            self.message(s, &tr!("Form data ready: {}.", name));
        }
    }

    /// Fills the form with an FDF or XFDF file.
    pub(crate) fn import_form_data(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, path: &Path) {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => return self.message(s, &tr!("The file could not be read: {}", e)),
        };
        let Some(r) = self.prep_edit(s, ui, &tr!("Import form data"), |f, d| {
            f.import_data(d, &data)
        }) else {
            return;
        };
        let mut m = match r.change.fields.len() {
            0 if r.annotations == 0 => tr!("The data has nothing for this form."),
            n => trn!("Filled in {n} field.", "Filled in {n} fields.", n),
        };
        if r.annotations > 0 {
            m.push(' ');
            m.push_str(&trn!(
                "Added {n} comment.",
                "Added {n} comments.",
                r.annotations
            ));
        }
        if !r.unknown.is_empty() {
            m.push(' ');
            m.push_str(&trn!(
                "{n} value is for a field this form does not have.",
                "{n} values are for fields this form does not have.",
                r.unknown.len()
            ));
        }
        self.message(s, &m);
    }

    /// The field type the next tap places.
    fn placing(ui: &MobileWindow) -> Option<FieldTool> {
        let t = ui.global::<AppState>().get_prep_type();
        usize::try_from(t)
            .ok()
            .and_then(|i| FieldTool::ALL.get(i).copied())
    }

    /// A finger down with Prepare form: on the selected field's handle it resizes, on the
    /// field it moves.
    pub(crate) fn prep_down(s: &mut State, page: usize, u: f64, v: f64) -> Option<PrepDrag> {
        let backend = s.forms.clone();
        let d = s.doc.as_mut()?;
        let sel = d.prep.sel.clone().filter(|s| s.page == page)?;
        let r = sel_box(d, &*backend, &sel)?;
        let reach = HANDLE_REACH / d.view.last.k.max(0.01);
        let (hx, hy) = (r[0] + r[2], r[1] + r[3]);
        let resize = (u - hx).abs() <= reach && (v - hy).abs() <= reach;
        let slop = 8.0 / d.view.last.k.max(0.01);
        let inside = u >= r[0] - slop
            && v >= r[1] - slop
            && u <= r[0] + r[2] + slop
            && v <= r[1] + r[3] + slop;
        (resize || inside).then_some(PrepDrag {
            sel,
            resize,
            from: r,
            now: r,
        })
    }

    /// The field's box for the finger moved by `dx, dy` displayed points.
    pub(crate) fn prep_drag(g: &mut PrepDrag, dx: f64, dy: f64) {
        let f = g.from;
        g.now = if g.resize {
            [
                f[0],
                f[1],
                (f[2] + dx).max(MIN_SIZE),
                (f[3] + dy).max(MIN_SIZE),
            ]
        } else {
            [f[0] + dx, f[1] + dy, f[2], f[3]]
        };
    }

    pub(crate) fn prep_drop(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, g: PrepDrag) {
        if g.from == g.now {
            return self.refresh(s, ui);
        }
        let Some(d) = &s.doc else { return };
        let r = d.session.geoms[g.sel.page].rect_to_pdf(g.now);
        let label = if g.resize {
            tr!("Resize field")
        } else {
            tr!("Move field")
        };
        let w = g.sel.widget;
        self.prep_edit(s, ui, &label, |f, d| f.set_rects(d, &[(w, r)]));
    }

    /// A tap with Prepare form: places the chosen field type, else selects the field there.
    pub(crate) fn prep_tap(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        page: usize,
        u: f64,
        v: f64,
    ) {
        let backend = s.forms.clone();
        if let Some(tool) = Self::placing(ui) {
            ui.global::<AppState>().set_prep_type(-1);
            let Some(d) = &s.doc else { return };
            let (x, y) = d.session.geoms[page].to_pdf(u, v);
            let nf = tool.new_field(page, tool.rect_at(x, y));
            let Some((_, name)) =
                self.prep_edit(s, ui, &add_label(tool), |f, d| f.add_field(d, &nf))
            else {
                return;
            };
            self.prep_select_name(s, ui, &name, page);
            return self.message(s, &tr!("Added “{}”.", name));
        }
        let Some(d) = &mut s.doc else { return };
        let g = d.session.geoms[page];
        let (x, y) = g.to_pdf(u, v);
        let tol = 4.0 / d.view.last.k.max(0.01);
        let hit = d.form(&*backend).fields().into_iter().rev().find_map(|f| {
            let w = f.widgets.iter().find(|w| {
                w.page == page
                    && x >= w.rect[0] - tol
                    && x <= w.rect[2] + tol
                    && y >= w.rect[1] - tol
                    && y <= w.rect[3] + tol
            })?;
            Some(PrepSel {
                name: f.name.clone(),
                widget: w.id,
                page,
            })
        });
        d.prep.sel = hit;
        d.marks_dirty = true;
        show(s, ui);
        self.refresh(s, ui);
    }

    fn prep_select_name(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        name: &str,
        page: usize,
    ) {
        let backend = s.forms.clone();
        let Some(d) = &mut s.doc else { return };
        let w = d.form(&*backend).field(name).and_then(|f| {
            f.widgets
                .iter()
                .rev()
                .find(|w| w.page == page)
                .or(f.widgets.first())
                .map(|w| (w.id, w.page))
        });
        d.prep.sel = w.map(|(widget, page)| PrepSel {
            name: name.to_owned(),
            widget,
            page,
        });
        d.marks_dirty = true;
        show(s, ui);
        self.refresh(s, ui);
    }

    /// Selects field `name` (for scripts).
    pub(crate) fn prep_select(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        name: &str,
    ) -> Result<(), String> {
        let backend = s.forms.clone();
        let d = s.doc.as_mut().ok_or("no document is open")?;
        let page = d
            .form(&*backend)
            .field(name)
            .and_then(|f| f.widgets.first().map(|w| w.page))
            .ok_or_else(|| format!("no field {name}"))?;
        self.prep_select_name(s, ui, name, page);
        Ok(())
    }

    fn prep_action(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, what: &str) {
        let backend = s.forms.clone();
        let Some(d) = &mut s.doc else { return };
        let sel = d.prep.sel.clone();
        let field = sel
            .as_ref()
            .and_then(|sel| d.form(&*backend).field(&sel.name));
        let rect = sel.as_ref().and_then(|sel| {
            field
                .as_ref()?
                .widgets
                .iter()
                .find(|w| w.id == sel.widget)
                .map(|w| w.rect)
        });
        match (what, sel, field, rect) {
            ("detect", ..) => {
                let page = d.view.current_page;
                d.prep.found = form_prep::detect(d.session.doc(), page);
                d.prep.sel = None;
                d.marks_dirty = true;
                let n = d.prep.found.len();
                show(s, ui);
                self.refresh(s, ui);
                if n == 0 {
                    self.message(s, &tr!("No places for fields found on this page."));
                }
            }
            ("accept", ..) => {
                let found = std::mem::take(&mut d.prep.found);
                let n = found.len();
                self.prep_edit(s, ui, &tr!("Add fields found"), |f, d| {
                    for sg in &found {
                        let mut nf = sg.to_new_field();
                        nf.name = f.unique_name(&nf.name);
                        f.add_field(d, &nf)?;
                    }
                    Ok(())
                });
                self.message(s, &trn!("Added {n} field.", "Added {n} fields.", n));
            }
            ("dismiss", ..) => {
                d.prep.found.clear();
                d.marks_dirty = true;
                show(s, ui);
                self.refresh(s, ui);
            }
            ("deselect", ..) => {
                d.prep.sel = None;
                d.marks_dirty = true;
                show(s, ui);
                self.refresh(s, ui);
            }
            ("props", Some(sel), Some(f), _) => {
                let doc = d.session.doc().clone();
                let Some(p) = d.form(&*backend).props(&doc, &f.name) else {
                    return;
                };
                let st = ui.global::<AppState>();
                st.set_prop_kind(match f.kind {
                    FieldKind::Text => 0,
                    FieldKind::ComboBox | FieldKind::ListBox => 1,
                    FieldKind::CheckBox | FieldKind::Radio => 2,
                    _ => 3,
                });
                st.set_prop_name(f.name.as_str().into());
                st.set_prop_tooltip(p.tooltip.as_str().into());
                st.set_prop_required(p.required);
                st.set_prop_read_only(p.read_only);
                st.set_prop_multiline(p.multiline);
                st.set_prop_font_size(format!("{}", p.font_size).into());
                st.set_prop_options(
                    p.options
                        .iter()
                        .map(|o| o.display.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                        .into(),
                );
                let export = f
                    .widgets
                    .iter()
                    .find(|w| w.id == sel.widget)
                    .and_then(|w| w.export.clone())
                    .unwrap_or_default();
                st.set_prop_export(export.into());
                st.set_sheet(Sheet::FieldProps);
            }
            ("duplicate", Some(sel), Some(f), Some(_)) => {
                // Below all of the field's widgets on the page (a whole radio group).
                let on_page = f.widgets.iter().filter(|w| w.page == sel.page);
                let (lo, hi) = on_page.fold((f64::MAX, f64::MIN), |(lo, hi), w| {
                    (lo.min(w.rect[1]), hi.max(w.rect[3]))
                });
                let dy = -(hi - lo + 8.0);
                if let Some((_, name)) =
                    self.prep_edit(s, ui, &tr!("Duplicate field"), |form, d| {
                        form.duplicate(d, &f.name, 0.0, dy)
                    })
                {
                    self.prep_select_name(s, ui, &name, sel.page);
                }
            }
            ("delete", Some(sel), Some(_), _) => {
                let w = sel.widget;
                self.prep_edit(s, ui, &tr!("Delete field"), |f, d| f.remove_widget(d, w));
                if let Some(d) = &mut s.doc {
                    d.prep.sel = None;
                }
                show(s, ui);
            }
            ("add-radio", Some(sel), Some(f), Some(r)) => {
                let h = r[3] - r[1] + 8.0;
                let rect = [r[0], r[1] - h, r[2], r[3] - h];
                if let Some((_, w)) = self.prep_edit(s, ui, &tr!("Add radio button"), |form, d| {
                    form.add_widget(d, &f.name, sel.page, rect, None)
                }) && let Some(d) = &mut s.doc
                {
                    d.prep.sel = Some(PrepSel { widget: w, ..sel });
                    d.marks_dirty = true;
                    self.refresh(s, ui);
                }
            }
            _ => {}
        }
    }

    /// Applies the properties sheet as one step.
    fn prop_apply(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let backend = s.forms.clone();
        let st = ui.global::<AppState>();
        let Some(d) = &mut s.doc else { return };
        let Some(sel) = d.prep.sel.clone() else {
            return;
        };
        let doc = d.session.doc().clone();
        let form = d.form(&*backend);
        let (Some(f), Some(mut p)) = (form.field(&sel.name), form.props(&doc, &sel.name)) else {
            return;
        };
        let new_name = st.get_prop_name().trim().to_owned();
        p.tooltip = st.get_prop_tooltip().trim().to_owned();
        p.required = st.get_prop_required();
        p.read_only = st.get_prop_read_only();
        if f.kind == FieldKind::Text {
            p.multiline = st.get_prop_multiline();
        }
        if let Ok(size) = st.get_prop_font_size().trim().parse::<f64>() {
            p.font_size = size.clamp(0.0, 200.0);
        }
        if f.kind.is_choice() {
            p.options = st
                .get_prop_options()
                .split(',')
                .map(str::trim)
                .filter(|o| !o.is_empty())
                .map(|o| ChoiceOption {
                    export: o.to_owned(),
                    display: o.to_owned(),
                })
                .collect();
        }
        let export = st.get_prop_export().trim().to_owned();
        let old_export = f
            .widgets
            .iter()
            .find(|w| w.id == sel.widget)
            .and_then(|w| w.export.clone());
        let button = matches!(f.kind, FieldKind::CheckBox | FieldKind::Radio);
        let old = sel.name.clone();
        let name = if new_name.is_empty() {
            old.clone()
        } else {
            new_name
        };
        let w = sel.widget;
        let done = self.prep_edit(s, ui, &tr!("Change field properties"), |form, d| {
            if name != old {
                form.rename(d, &old, &name)?;
            }
            form.set_props(d, &name, &p)?;
            if button && !export.is_empty() && old_export.as_deref() != Some(export.as_str()) {
                form.set_export(d, w, &export)?;
            }
            Ok(())
        });
        if done.is_some() {
            st.set_sheet(Sheet::None);
            if let Some(d) = &mut s.doc {
                d.prep.sel = Some(PrepSel {
                    name: name.clone(),
                    ..sel
                });
            }
            show(s, ui);
            self.message(s, &tr!("Changed “{}”.", name));
        }
    }
}
