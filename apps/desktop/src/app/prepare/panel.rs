//! Prepare form's panel: the selected field's properties, aligning several fields, tab
//! order, suggested fields, and form data (also used by Fill forms).

use std::path::PathBuf;
use std::rc::Rc;

use pagebara_annot::Color;
use pagebara_app_core::form_prep::{self, Align, Tool};
use pagebara_app_core::forms::{
    BorderKind, ButtonAction, CHECK_STYLES, ChoiceOption, FieldKind, FieldProps, FormChange,
    FormError, Format, STANDARD_FONTS, TabOrder,
};
use pagebara_i18n::{tr, tr_noop, trn};
use pagebara_syntax::ObjRef;
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::super::{App, State, pick};
use super::{TOOL, placed};
use crate::{AppState, AppWindow, PrepRow, PrepState, ViewState};

/// The panel's colours (index 0 is none).
const COLOURS: [Option<Color>; 11] = [
    None,
    Some(Color::Gray(0.0)),
    Some(Color::Gray(0.25)),
    Some(Color::Gray(0.5)),
    Some(Color::Gray(0.75)),
    Some(Color::Gray(1.0)),
    Some(Color::Rgb(0.15, 0.39, 0.92)),
    Some(Color::Rgb(0.86, 0.92, 1.0)),
    Some(Color::Rgb(0.86, 0.15, 0.15)),
    Some(Color::Rgb(0.09, 0.64, 0.29)),
    Some(Color::Rgb(1.0, 0.95, 0.6)),
];
const SIZES: [f64; 10] = [0.0, 8.0, 9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 24.0];
const WIDTHS: [f64; 3] = [1.0, 2.0, 3.0];
const STYLES: [BorderKind; 5] = [
    BorderKind::Solid,
    BorderKind::Dashed,
    BorderKind::Beveled,
    BorderKind::Inset,
    BorderKind::Underline,
];
const NAMED: [&str; 5] = ["NextPage", "PrevPage", "FirstPage", "LastPage", "Print"];

fn colour_index(c: Option<Color>) -> i32 {
    let Some(c) = c else { return 0 };
    let [r, g, b] = c.to_rgb();
    (1..COLOURS.len())
        .min_by(|&i, &j| {
            let d = |k: usize| {
                let [x, y, z] = COLOURS[k].map_or([0.0; 3], Color::to_rgb);
                (x - r).powi(2) + (y - g).powi(2) + (z - b).powi(2)
            };
            d(i).total_cmp(&d(j))
        })
        .unwrap_or(1) as i32
}

fn nearest(list: &[f64], v: f64) -> i32 {
    (0..list.len())
        .min_by(|&i, &j| (list[i] - v).abs().total_cmp(&(list[j] - v).abs()))
        .unwrap_or(0) as i32
}

pub fn kind_name(k: FieldKind) -> &'static str {
    match k {
        FieldKind::Text => "text",
        FieldKind::CheckBox => "check",
        FieldKind::Radio => "radio",
        FieldKind::ComboBox => "combo",
        FieldKind::ListBox => "list",
        FieldKind::PushButton => "button",
        FieldKind::Signature => "signature",
        FieldKind::Unknown => "unknown",
    }
}

pub fn kind_label(k: FieldKind, date: bool) -> String {
    match k {
        FieldKind::Text if date => tr!("Date field"),
        FieldKind::Text => tr!("Text field"),
        FieldKind::CheckBox => tr!("Check box"),
        FieldKind::Radio => tr!("Radio button"),
        FieldKind::ComboBox => tr!("Drop-down list"),
        FieldKind::ListBox => tr!("List box"),
        // TRANSLATORS: the kind of a form field
        FieldKind::PushButton => tr!("Button"),
        FieldKind::Signature => tr!("Signature field"),
        FieldKind::Unknown => tr!("Field"),
    }
}

/// [`kind_label`] inside a sentence.
fn kind_noun(k: FieldKind, date: bool) -> String {
    match k {
        FieldKind::Text if date => tr!("date field"),
        FieldKind::Text => tr!("text field"),
        FieldKind::CheckBox => tr!("check box"),
        FieldKind::Radio => tr!("radio button"),
        FieldKind::ComboBox => tr!("drop-down list"),
        FieldKind::ListBox => tr!("list box"),
        // TRANSLATORS: the kind of a form field, inside a sentence
        FieldKind::PushButton => tr!("button"),
        FieldKind::Signature => tr!("signature field"),
        FieldKind::Unknown => tr!("field"),
    }
}

/// The name of a check box mark (`CHECK_STYLES`).
fn check_style_label(c: char, english: &str) -> String {
    match c {
        // TRANSLATORS: a check mark, a style of check box
        '4' => tr!("Check"),
        // TRANSLATORS: a style of check box mark
        'l' => tr!("Circle"),
        // TRANSLATORS: a style of check box mark
        '8' => tr!("Cross"),
        // TRANSLATORS: a style of check box mark
        'u' => tr!("Diamond"),
        // TRANSLATORS: a style of check box mark
        'n' => tr!("Square"),
        // TRANSLATORS: a style of check box mark
        'H' => tr!("Star"),
        _ => english.to_owned(),
    }
}

fn rows(list: Vec<PrepRow>) -> ModelRc<PrepRow> {
    ModelRc::new(VecModel::from(list))
}

fn options_text(o: &[ChoiceOption]) -> String {
    o.iter()
        .map(|o| {
            if o.export == o.display {
                o.display.clone()
            } else {
                format!("{}|{}", o.display, o.export)
            }
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn parse_options(s: &str) -> Vec<ChoiceOption> {
    s.split(';')
        .map(str::trim)
        .filter(|o| !o.is_empty())
        .map(|o| match o.split_once('|') {
            Some((d, e)) => ChoiceOption {
                display: d.trim().into(),
                export: e.trim().into(),
            },
            None => ChoiceOption {
                display: o.into(),
                export: o.into(),
            },
        })
        .collect()
}

/// The page whose tab order the panel shows.
fn tab_page(s: &State) -> usize {
    s.tab_ref().map_or(0, |t| {
        if t.prep.sel.is_empty() {
            t.view.current_page
        } else {
            t.prep.page
        }
    })
}

/// Shows the selection, tab order and suggestions in the panel.
pub fn sync(s: &mut State, ui: &AppWindow) {
    let ps = ui.global::<PrepState>();
    if ui.global::<AppState>().get_tool() != TOOL || s.tab_ref().is_none() {
        ps.set_count(0);
        ps.set_placing(-1);
        return;
    }
    let page = s.tab_ref().map_or(0, |t| t.prep.page);
    let widgets = placed(s, page);
    let backend = s.forms.clone();
    let Some(t) = s.tab() else { return };
    t.prep.sel.retain(|id| widgets.iter().any(|w| w.id == *id));
    ps.set_placing(
        t.prep
            .placing
            .and_then(|p| Tool::ALL.iter().position(|x| *x == p))
            .map_or(-1, |i| i as i32),
    );
    let doc = t.session.doc().clone();
    let sel = t.prep.sel.clone();
    let (tab_row, current, suggestion) = (t.prep.tab_row, t.view.current_page, t.prep.suggestion);
    let suggestions = t.prep.suggestions.clone();
    let form = t.form(&*backend);
    let mut names: Vec<String> = Vec::new();
    for id in &sel {
        if let Some(f) = form.field_of_widget(*id)
            && !names.contains(&f.name)
        {
            names.push(f.name);
        }
    }
    ps.set_count(sel.len() as i32);
    if sel.len() == 1
        && let Some(f) = form.field_of_widget(sel[0])
        && let Some(p) = form.props(&doc, &f.name)
    {
        let date = matches!(p.format, Format::Date(_));
        ps.set_summary(tr!("{} “{}”", kind_label(f.kind, date), f.name).into());
        ps.set_kind(kind_name(f.kind).into());
        ps.set_name(f.name.as_str().into());
        ps.set_tooltip(p.tooltip.as_str().into());
        ps.set_required(p.required);
        ps.set_read_only(p.read_only);
        ps.set_default_value(p.default_value.as_str().into());
        let mut fonts: Vec<SharedString> =
            STANDARD_FONTS.iter().map(|(_, _, l)| (*l).into()).collect();
        let font = match STANDARD_FONTS.iter().position(|(r, _, _)| *r == p.font) {
            Some(i) => i,
            None => {
                fonts.push(tr!("Form font ({})", p.font).into());
                fonts.len() - 1
            }
        };
        ps.set_fonts(ModelRc::new(VecModel::from(fonts)));
        ps.set_font(font as i32);
        ps.set_size(nearest(&SIZES, p.font_size));
        ps.set_text_colour(colour_index(Some(p.text_color)).max(1));
        ps.set_border_colour(if p.border_width > 0.0 {
            colour_index(p.border_color)
        } else {
            0
        });
        ps.set_background(colour_index(p.background));
        ps.set_border_width(nearest(&WIDTHS, p.border_width.max(1.0)));
        ps.set_border_style(
            STYLES
                .iter()
                .position(|k| *k == p.border_style)
                .unwrap_or(0) as i32,
        );
        ps.set_align(i32::from(p.align.min(2)));
        ps.set_multiline(p.multiline);
        ps.set_password(p.password);
        ps.set_comb(p.comb);
        ps.set_max_len(p.max_len.map(|n| n.to_string()).unwrap_or_default().into());
        let mut formats: Vec<SharedString> = vec![
            // TRANSLATORS: no format, for a form field's value
            tr!("None").into(),
            // TRANSLATORS: a format for a form field's value
            tr!("Number").into(),
            // TRANSLATORS: a format for a form field's value
            tr!("Date").into(),
        ];
        let format = match &p.format {
            Format::None => 0,
            Format::Number { .. } => 1,
            Format::Date(d) => {
                ps.set_date_format(d.as_str().into());
                2
            }
            Format::Other => {
                formats.push(tr!("Script (kept)").into());
                3
            }
        };
        if !date {
            ps.set_date_format(form_prep::DATE_FORMAT.into());
        }
        ps.set_formats(ModelRc::new(VecModel::from(formats)));
        ps.set_format(format);
        ps.set_options(options_text(&p.options).into());
        ps.set_editable(p.editable);
        ps.set_multi_select(p.multi_select);
        ps.set_caption(p.caption.as_str().into());
        let mut actions: Vec<SharedString> = [
            // TRANSLATORS: no action, for what a form button does
            tr!("None"),
            tr!("Reset form"),
            tr!("Open a web address"),
            tr!("Next page"),
            tr!("Previous page"),
            tr!("First page"),
            tr!("Last page"),
            // TRANSLATORS: the verb, an action of a form button
            tr!("Print"),
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        let action = match &p.action {
            ButtonAction::None => 0,
            ButtonAction::ResetForm => 1,
            ButtonAction::Uri(u) => {
                ps.set_uri(u.as_str().into());
                2
            }
            ButtonAction::Named(n) => NAMED.iter().position(|x| x == n).map_or_else(
                || {
                    actions.push(tr!("{} (kept)", n).into());
                    actions.len() - 1
                },
                |i| i + 3,
            ),
            ButtonAction::Other => {
                actions.push(tr!("Script (kept)").into());
                actions.len() - 1
            }
        };
        ps.set_actions(ModelRc::new(VecModel::from(actions)));
        ps.set_click_action(action as i32);
        let styles: Vec<SharedString> = CHECK_STYLES
            .iter()
            .map(|(c, l)| check_style_label(*c, l).into())
            .collect();
        ps.set_check_styles(ModelRc::new(VecModel::from(styles)));
        ps.set_check_style(
            CHECK_STYLES
                .iter()
                .position(|(c, _)| *c == p.check_style)
                .unwrap_or(0) as i32,
        );
        let w = f.widgets.iter().find(|w| w.id == sel[0]);
        ps.set_export_value(w.and_then(|w| w.export.clone()).unwrap_or_default().into());
        ps.set_has_icon(false);
        if f.kind == FieldKind::PushButton {
            let icon = doc
                .get(sel[0])
                .and_then(|o| {
                    let d = o.as_dict()?.clone();
                    let mk = doc.get_in(&d, b"MK");
                    Some(mk.as_dict()?.contains(b"I"))
                })
                .unwrap_or(false);
            ps.set_has_icon(icon);
        }
    } else if sel.len() > 1 {
        ps.set_summary(trn!("{n} field selected", "{n} fields selected", sel.len()).into());
    } else {
        ps.set_summary("".into());
    }
    // Tab order of the page.
    let tp = if sel.is_empty() { current } else { page };
    let order = form.tab_order(tp);
    let mode = form.tab_mode(tp);
    let tab_rows: Vec<PrepRow> = order
        .iter()
        .enumerate()
        .filter_map(|(i, w)| {
            let f = form.field_of_widget(*w)?;
            Some(PrepRow {
                text: format!("{}. {}", i + 1, f.name).into(),
                detail: kind_label(f.kind, false).into(),
                selected: tab_row == Some(i),
            })
        })
        .collect();
    ps.set_tab_mode(match mode {
        TabOrder::Structure => 0,
        TabOrder::Rows => 1,
        TabOrder::Columns => 2,
        TabOrder::Manual(_) => 3,
    });
    ps.set_tab_rows(rows(tab_rows));
    ps.set_tab_page(tp as i32 + 1);
    let sugg: Vec<PrepRow> = suggestions
        .iter()
        .enumerate()
        .map(|(i, sg)| PrepRow {
            text: sg.name.as_str().into(),
            detail: tr!("{}, page {}", kind_label(sg.kind, sg.date), sg.page + 1).into(),
            selected: suggestion == Some(i),
        })
        .collect();
    ps.set_suggestions(rows(sugg));
}

/// The panel's values over `old`.
fn read_props(ps: &PrepState, old: &FieldProps) -> FieldProps {
    let mut p = old.clone();
    p.tooltip = ps.get_tooltip().trim().to_owned();
    p.required = ps.get_required();
    p.read_only = ps.get_read_only();
    p.default_value = ps.get_default_value().trim().to_owned();
    if let Some((r, _, _)) = STANDARD_FONTS.get(ps.get_font() as usize) {
        p.font = (*r).to_owned();
    }
    p.font_size = SIZES[(ps.get_size().max(0) as usize).min(SIZES.len() - 1)];
    p.text_color = COLOURS
        .get(ps.get_text_colour() as usize)
        .copied()
        .flatten()
        .unwrap_or(Color::BLACK);
    let border = COLOURS
        .get(ps.get_border_colour() as usize)
        .copied()
        .flatten();
    p.border_color = border;
    p.border_width = match border {
        Some(_) => WIDTHS[(ps.get_border_width().max(0) as usize).min(2)],
        None => 0.0,
    };
    p.background = COLOURS.get(ps.get_background() as usize).copied().flatten();
    p.border_style = STYLES[(ps.get_border_style().max(0) as usize).min(4)];
    p.align = ps.get_align().clamp(0, 2) as u8;
    p.multiline = ps.get_multiline();
    p.password = ps.get_password();
    p.comb = ps.get_comb();
    p.max_len = ps.get_max_len().trim().parse().ok().filter(|n| *n > 0);
    if p.comb && p.max_len.is_none() {
        p.max_len = Some(10);
    }
    p.format = match ps.get_format() {
        0 => Format::None,
        1 => match &old.format {
            f @ Format::Number { .. } => f.clone(),
            _ => Format::Number {
                decimals: 2,
                sep: 0,
                currency: String::new(),
            },
        },
        2 => {
            let f = ps.get_date_format().trim().to_owned();
            Format::Date(if f.is_empty() {
                form_prep::DATE_FORMAT.into()
            } else {
                f
            })
        }
        _ => old.format.clone(),
    };
    if !old.options.is_empty() || matches!(ps.get_kind().as_str(), "combo" | "list") {
        p.options = parse_options(&ps.get_options());
    }
    p.editable = ps.get_editable();
    p.multi_select = ps.get_multi_select();
    p.caption = ps.get_caption().to_string();
    p.action = match ps.get_click_action() {
        0 => ButtonAction::None,
        1 => ButtonAction::ResetForm,
        2 => ButtonAction::Uri(ps.get_uri().trim().to_owned()),
        i @ 3..=7 => ButtonAction::Named(NAMED[i as usize - 3].into()),
        _ => old.action.clone(),
    };
    if let Some((c, _)) = CHECK_STYLES.get(ps.get_check_style() as usize) {
        p.check_style = *c;
    }
    p
}

impl App {
    /// The selected field's name and its widgets on the page.
    fn prep_field(self: &Rc<Self>, s: &mut State) -> Option<(String, Vec<super::Placed>)> {
        let t = s.tab_ref()?;
        let (page, first) = (t.prep.page, *t.prep.sel.first()?);
        let all = placed(s, page);
        let name = all.iter().find(|w| w.id == first)?.name.clone();
        Some((name, all))
    }

    /// The selected widgets in selection order.
    fn prep_selected(s: &mut State) -> Vec<super::Placed> {
        let Some(t) = s.tab_ref() else {
            return Vec::new();
        };
        let (page, sel) = (t.prep.page, t.prep.sel.clone());
        let all = placed(s, page);
        sel.iter()
            .filter_map(|id| all.iter().find(|w| w.id == *id).cloned())
            .collect()
    }

    pub fn prep_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str, value: &str) {
        let ps = ui.global::<PrepState>();
        let backend = s.forms.clone();
        match what {
            "tool" => {
                let Some(tool) = value.parse::<usize>().ok().and_then(|i| Tool::ALL.get(i)) else {
                    return;
                };
                let Some(t) = s.tab() else { return };
                if t.prep.placing == Some(*tool) {
                    t.prep.placing = None;
                    ui.global::<ViewState>().set_cursor(0);
                } else {
                    t.prep.placing = Some(*tool);
                    t.prep.sel.clear();
                    ui.global::<ViewState>().set_cursor(3);
                    self.message(s, &super::place_message(*tool));
                }
                sync(s, ui);
                self.overlay_changed(s, ui);
            }
            "prop" => {
                let Some((name, _)) = self.prep_field(s) else {
                    return;
                };
                let Some(t) = s.tab() else { return };
                let doc = t.session.doc().clone();
                let Some(old) = t.form(&*backend).props(&doc, &name) else {
                    return;
                };
                let new = read_props(&ps, &old);
                if new == old {
                    return;
                }
                if self
                    .prep_edit(s, ui, &tr!("Change field properties"), |f, d| {
                        f.set_props(d, &name, &new)
                    })
                    .is_some()
                {
                    self.message(s, &tr!("Changed “{}”.", name));
                }
            }
            "rename" => {
                let Some((name, _)) = self.prep_field(s) else {
                    return;
                };
                let new = value.trim().to_owned();
                if new == name || new.is_empty() {
                    return sync(s, ui);
                }
                let taken = s.tab().is_some_and(|t| t.form(&*backend).name_taken(&new));
                if taken {
                    self.message(s, &tr!("There is already a field named “{}”.", new));
                    return sync(s, ui);
                }
                if self
                    .prep_edit(s, ui, &tr!("Rename field"), |f, d| f.rename(d, &name, &new))
                    .is_some()
                {
                    self.message(s, &tr!("Renamed “{}” to “{}”.", name, new));
                }
            }
            "export" => {
                let Some(w) = Self::prep_selected(s).first().map(|w| w.id) else {
                    return;
                };
                let v = value.trim().to_owned();
                let same = s.tab().is_some_and(|t| {
                    t.form(&*backend)
                        .field_of_widget(w)
                        .and_then(|f| f.widgets.iter().find(|x| x.id == w)?.export.clone())
                        .as_deref()
                        == Some(v.as_str())
                });
                if !same && !v.is_empty() {
                    self.prep_edit(s, ui, &tr!("Change export value"), |f, d| {
                        f.set_export(d, w, &v)
                    });
                }
            }
            "add-radio" => {
                let Some((name, all)) = self.prep_field(s) else {
                    return;
                };
                let page = s.tab_ref().map_or(0, |t| t.prep.page);
                let Some(last) = all.iter().rfind(|w| w.name == name).cloned() else {
                    return;
                };
                let h = last.rect[3] - last.rect[1];
                let r = [
                    last.rect[0],
                    last.rect[1] - h - 8.0,
                    last.rect[2],
                    last.rect[1] - 8.0,
                ];
                let mut made = None;
                self.prep_edit(s, ui, &tr!("Add radio button"), |f, d| {
                    let (c, w) = f.add_widget(d, &name, page, r, None)?;
                    made = Some(w);
                    Ok(c)
                });
                if let Some(w) = made
                    && let Some(t) = s.tab()
                {
                    t.prep.sel = vec![w];
                    self.prep_selection_changed(s, ui, false);
                    self.message(s, &tr!("Added a radio button to “{}”.", name));
                }
            }
            "duplicate" => {
                let sel = Self::prep_selected(s);
                let mut names: Vec<(String, f64)> = Vec::new();
                for w in &sel {
                    if !names.iter().any(|(n, _)| *n == w.name) {
                        names.push((w.name.clone(), w.rect[3] - w.rect[1]));
                    }
                }
                if names.is_empty() {
                    return;
                }
                let mut made = Vec::new();
                let label = if names.len() > 1 {
                    tr!("Duplicate fields")
                } else {
                    tr!("Duplicate field")
                };
                self.prep_edit(s, ui, &label, |f, d| {
                    let mut change = FormChange::default();
                    for (n, h) in &names {
                        let (c, new) = f.duplicate(d, n, 0.0, -(h + 10.0))?;
                        change.fields.extend(c.fields);
                        made.push(new);
                    }
                    Ok(change)
                });
                if !made.is_empty() {
                    let page = s.tab_ref().map_or(0, |t| t.prep.page);
                    let ids: Vec<ObjRef> = placed(s, page)
                        .into_iter()
                        .filter(|w| made.contains(&w.name))
                        .map(|w| w.id)
                        .collect();
                    if let Some(t) = s.tab() {
                        t.prep.sel = ids;
                    }
                    self.prep_selection_changed(s, ui, false);
                    self.message(s, &tr!("Duplicated as “{}”.", made.join("”, “")));
                }
            }
            "delete" => {
                let sel: Vec<ObjRef> = Self::prep_selected(s).iter().map(|w| w.id).collect();
                if sel.is_empty() {
                    return;
                }
                let label = if sel.len() > 1 {
                    tr!("Delete fields")
                } else {
                    tr!("Delete field")
                };
                if self
                    .prep_edit(s, ui, &label, |f, d| {
                        let mut change = FormChange::default();
                        for w in &sel {
                            if f.field_of_widget(*w).is_some() {
                                change.widgets.extend(f.remove_widget(d, *w)?.widgets);
                            }
                        }
                        Ok(change)
                    })
                    .is_some()
                {
                    if let Some(t) = s.tab() {
                        t.prep.sel.clear();
                    }
                    sync(s, ui);
                    self.message(s, &tr!("{}: done.", label));
                }
            }
            "copy-pages" => {
                let Some((name, _)) = self.prep_field(s) else {
                    return;
                };
                let n = s.tab_ref().map_or(0, |t| t.session.page_count());
                let pages: Vec<usize> = (0..n).collect();
                if self
                    .prep_edit(s, ui, &tr!("Copy field to all pages"), |f, d| {
                        f.copy_to_pages(d, &name, &pages)
                    })
                    .is_some()
                {
                    // The widget may be a new object now (a field and its widget split).
                    let page = s.tab_ref().map_or(0, |t| t.prep.page);
                    self.prep_select_field(s, ui, &name, page);
                    self.message(
                        s,
                        &trn!(
                            "“{}” is on all {n} page.",
                            "“{}” is on all {n} pages.",
                            n,
                            name
                        ),
                    );
                }
            }
            "align" | "distribute" | "size" => {
                let sel = Self::prep_selected(s);
                if sel.len() < 2 {
                    return self.message(s, &tr!("Select two or more fields first."));
                }
                let rects: Vec<[f64; 4]> = sel.iter().map(|w| w.rect).collect();
                let (new, label) = match what {
                    "align" => match Align::parse(value) {
                        Some(a) => (form_prep::align(&rects, a), tr!("Align fields")),
                        None => return,
                    },
                    "distribute" => (
                        form_prep::distribute(&rects, value != "v"),
                        tr!("Distribute fields"),
                    ),
                    _ => (
                        form_prep::same_size(&rects, value != "h", value != "w"),
                        tr!("Size fields"),
                    ),
                };
                let changes: Vec<(ObjRef, [f64; 4])> = sel
                    .iter()
                    .zip(new)
                    .filter(|(w, r)| w.rect != *r)
                    .map(|(w, r)| (w.id, r))
                    .collect();
                if changes.is_empty() {
                    return self.message(s, &tr!("The fields are already like that."));
                }
                self.prep_set_rects(s, ui, &label, changes);
            }
            "select-all" => {
                let page = s.tab_ref().map_or(0, |t| t.view.current_page);
                let ids: Vec<ObjRef> = placed(s, page).into_iter().map(|w| w.id).collect();
                if let Some(t) = s.tab() {
                    t.prep.page = page;
                    t.prep.sel = ids;
                }
                self.prep_selection_changed(s, ui, true);
            }
            "select" => {
                // `NAME` or `NAME,N`: a field's widgets (or its Nth) on any page.
                let (name, n) = match value.rsplit_once(',') {
                    Some((a, b)) if b.trim().parse::<usize>().is_ok() => {
                        (a.to_owned(), b.trim().parse::<usize>().ok())
                    }
                    _ => (value.to_owned(), None),
                };
                let Some(t) = s.tab() else { return };
                let Some(f) = t.form(&*backend).field(&name) else {
                    return self.message(s, &tr!("There is no field “{}”.", name));
                };
                let ws: Vec<_> = match n {
                    Some(i) => f
                        .widgets
                        .get(i.saturating_sub(1))
                        .into_iter()
                        .cloned()
                        .collect(),
                    None => {
                        let page = f.widgets.first().map_or(0, |w| w.page);
                        f.widgets
                            .iter()
                            .filter(|w| w.page == page)
                            .cloned()
                            .collect()
                    }
                };
                let Some(page) = ws.first().map(|w| w.page) else {
                    return;
                };
                t.prep.page = page;
                t.prep.sel = ws.iter().map(|w| w.id).collect();
                t.prep.placing = None;
                self.prep_selection_changed(s, ui, true);
            }
            "tab-mode" => {
                let page = tab_page(s);
                let order = match value {
                    "0" => TabOrder::Structure,
                    "1" => TabOrder::Rows,
                    "2" => TabOrder::Columns,
                    _ => TabOrder::Manual(Vec::new()),
                };
                if self
                    .prep_edit(s, ui, &tr!("Change tab order"), |f, d| {
                        f.set_tab_order(d, page, &order)
                    })
                    .is_some()
                {
                    self.message(s, &tr!("Tab order changed."));
                }
            }
            "tab-select" => {
                if let Some(t) = s.tab() {
                    t.prep.tab_row = value.parse().ok();
                }
                sync(s, ui);
            }
            "tab-up" | "tab-down" => {
                let page = tab_page(s);
                let Some(t) = s.tab() else { return };
                let Some(i) = t.prep.tab_row else {
                    return self.message(s, &tr!("Pick a field in the tab order list first."));
                };
                let mut order = t.form(&*backend).tab_order(page);
                let j = if what == "tab-up" {
                    i.checked_sub(1)
                } else {
                    Some(i + 1).filter(|j| *j < order.len())
                };
                let Some(j) = j.filter(|_| i < order.len()) else {
                    return;
                };
                order.swap(i, j);
                t.prep.tab_row = Some(j);
                let mode = TabOrder::Manual(order);
                if self
                    .prep_edit(s, ui, &tr!("Change tab order"), |f, d| {
                        f.set_tab_order(d, page, &mode)
                    })
                    .is_some()
                {
                    self.message(s, &tr!("Moved to place {} in the tab order.", j + 1));
                }
            }
            "detect" => {
                let Some(t) = s.tab() else { return };
                let doc = t.session.doc().clone();
                let n = t.session.page_count();
                let found: Vec<_> = (0..n).flat_map(|p| form_prep::detect(&doc, p)).collect();
                let text = match found.len() {
                    0 => tr!(
                        "No fields found: the pages have no lines, boxes or check boxes to fill in."
                    ),
                    k => trn!(
                        "Found {n} field. Accept it, or click it on the page.",
                        "Found {n} fields. Accept them all, or pick some.",
                        k
                    ),
                };
                t.prep.suggestions = found;
                t.prep.suggestion = None;
                ps.set_detect_status(text.as_str().into());
                sync(s, ui);
                self.overlay_changed(s, ui);
                self.message(s, &text);
            }
            "suggestion" => {
                let Some(t) = s.tab() else { return };
                let Some(sg) = value
                    .parse::<usize>()
                    .ok()
                    .and_then(|i| t.prep.suggestions.get(i))
                else {
                    return;
                };
                let text = tr!(
                    "Suggested {} “{}”, page {}.",
                    kind_noun(sg.kind, sg.date),
                    sg.name,
                    sg.page + 1
                );
                t.prep.suggestion = value.parse().ok();
                sync(s, ui);
                self.overlay_changed(s, ui);
                self.message(s, &text);
            }
            "accept" | "accept-all" => {
                let Some(t) = s.tab() else { return };
                let picked: Vec<usize> = if what == "accept-all" {
                    (0..t.prep.suggestions.len()).collect()
                } else {
                    t.prep.suggestion.into_iter().collect()
                };
                if picked.is_empty() {
                    return self.message(s, &tr!("Pick a suggested field first."));
                }
                let list: Vec<_> = picked
                    .iter()
                    .filter_map(|&i| t.prep.suggestions.get(i).cloned())
                    .collect();
                let mut made = 0;
                let ok = self
                    .prep_edit(s, ui, &tr!("Add detected fields"), |f, d| {
                        let mut change = FormChange::default();
                        for sg in &list {
                            let mut nf = sg.to_new_field();
                            nf.name = f.unique_name(&nf.name);
                            change.fields.extend(f.add_field(d, &nf)?.0.fields);
                            made += 1;
                        }
                        Ok(change)
                    })
                    .is_some();
                if ok && let Some(t) = s.tab() {
                    let mut i = 0;
                    t.prep.suggestions.retain(|_| {
                        i += 1;
                        !picked.contains(&(i - 1))
                    });
                    t.prep.suggestion = None;
                    if t.prep.suggestions.is_empty() {
                        ps.set_detect_status("".into());
                    }
                    sync(s, ui);
                    self.overlay_changed(s, ui);
                    self.message(
                        s,
                        &trn!(
                            "Added {n} detected field.",
                            "Added {n} detected fields.",
                            made
                        ),
                    );
                }
            }
            "dismiss" | "dismiss-all" => {
                let Some(t) = s.tab() else { return };
                if what == "dismiss-all" {
                    t.prep.suggestions.clear();
                } else if let Some(i) = t.prep.suggestion.take()
                    && i < t.prep.suggestions.len()
                {
                    t.prep.suggestions.remove(i);
                }
                if t.prep.suggestions.is_empty() {
                    ps.set_detect_status("".into());
                }
                sync(s, ui);
                self.overlay_changed(s, ui);
            }
            "icon" => {
                pick::open(
                    self,
                    &tr!("Button icon"),
                    (tr_noop!("Pictures"), &["png", "jpg", "jpeg"]),
                    |app, s, ui, path| app.prep_icon(s, ui, Some(path)),
                );
            }
            "icon-remove" => self.prep_icon(s, ui, None),
            "done" => {
                let st = ui.global::<AppState>();
                st.set_tool(5);
                self.tool_changed(s, ui);
            }
            other => self.form_data(s, ui, other),
        }
    }

    fn prep_icon(self: &Rc<Self>, s: &mut State, ui: &AppWindow, path: Option<PathBuf>) {
        let Some((name, _)) = self.prep_field(s) else {
            return;
        };
        let data = match &path {
            Some(p) => match std::fs::read(p) {
                Ok(d) => Some(d),
                Err(e) => return self.message(s, &tr!("Cannot read the picture: {}.", e)),
            },
            None => None,
        };
        let label = if data.is_some() {
            tr!("Set button icon")
        } else {
            tr!("Remove button icon")
        };
        self.prep_edit(s, ui, &label, |f, d| {
            let icon = match &data {
                Some(bytes) => {
                    let img = if bytes.starts_with(&[0xFF, 0xD8]) {
                        pagebara_edit::NewImage::Jpeg(bytes)
                    } else {
                        pagebara_edit::NewImage::Png(bytes)
                    };
                    Some(pagebara_edit::embed_image(d, &img).map_err(|e| FormError {
                        text: tr!("That picture cannot be used: {}.", e),
                        permission: false,
                    })?)
                }
                None => None,
            };
            f.set_button_icon(d, &name, icon)
        });
    }

    /// Form data and reset, from Prepare form and Fill forms: `import`, `export-fdf`,
    /// `export-xfdf`, `reset`, `clear`.
    pub fn form_data(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str) {
        let title = s.tab_ref().map(|t| t.session.title()).unwrap_or_default();
        let stem = title
            .strip_suffix(".pdf")
            .or_else(|| title.strip_suffix(".PDF"))
            .unwrap_or(&title)
            .to_owned();
        match what {
            "reset" => {
                self.field_done(s, ui, false);
                self.form_edit(s, ui, &tr!("Reset form"), |f, d| f.reset(d));
                self.message(s, &tr!("The form is back to its default values."));
            }
            "clear" => {
                self.field_done(s, ui, false);
                self.form_edit(s, ui, &tr!("Clear form"), |f, d| f.clear_all(d));
                self.message(s, &tr!("Every field is empty."));
            }
            "import" => pick::open(
                self,
                &tr!("Import form data"),
                (tr_noop!("Form data"), &["fdf", "xfdf"]),
                |app, s, ui, path| app.import_form_data(s, ui, &path),
            ),
            "export-fdf" | "export-xfdf" => {
                let xml = what == "export-xfdf";
                let ext = if xml { "xfdf" } else { "fdf" };
                let filter: (&str, &[&str]) = if xml {
                    (tr_noop!("XFDF form data"), &["xfdf"])
                } else {
                    (tr_noop!("FDF form data"), &["fdf"])
                };
                let annots = ui.global::<PrepState>().get_xfdf_annots();
                pick::save_file(
                    self,
                    &tr!("Export form data"),
                    filter,
                    &format!("{stem}.{ext}"),
                    move |app, s, ui, path| {
                        let backend = s.forms.clone();
                        let Some(t) = s.tab() else { return };
                        let doc = t.session.doc().clone();
                        let file = format!("{}.pdf", stem);
                        let form = t.form(&*backend);
                        let bytes = if xml {
                            form.export_xfdf(&doc, Some(&file), annots).into_bytes()
                        } else {
                            form.export_fdf(Some(&file))
                        };
                        match pagebara_app_core::session::write_atomic(&path, &bytes) {
                            Ok(()) => app
                                .message(s, &tr!("Exported the form data to {}.", path.display())),
                            Err(e) => {
                                app.show_message(s, ui, &tr!("Could not export"), &e.to_string())
                            }
                        }
                    },
                );
            }
            _ => {}
        }
    }

    fn import_form_data(self: &Rc<Self>, s: &mut State, ui: &AppWindow, path: &std::path::Path) {
        let data = match std::fs::read(path) {
            Ok(d) => d,
            Err(e) => return self.show_message(s, ui, &tr!("Could not import"), &e.to_string()),
        };
        self.field_done(s, ui, false);
        let mut summary = None;
        self.form_edit(s, ui, &tr!("Import form data"), |f, d| {
            let r = f.import_data(d, &data)?;
            let c = r.change.clone();
            summary = Some(r);
            Ok(c)
        });
        if let Some(r) = summary {
            let file = path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default();
            let n = r.change.fields.len();
            let mut text = trn!(
                "Imported {n} value from {}",
                "Imported {n} values from {}",
                n,
                file
            );
            if r.annotations > 0 {
                text = trn!(
                    "{} and {n} comment",
                    "{} and {n} comments",
                    r.annotations,
                    text
                );
            }
            if !r.unknown.is_empty() {
                text = tr!("{}; not in this form: {}", text, r.unknown.join(", "));
            }
            if !r.rejected.is_empty() {
                text = tr!("{}; refused: {}", text, r.rejected.join("; "));
            }
            text.push('.');
            self.message(s, &text);
        }
        super::sync(s, ui);
    }
}
