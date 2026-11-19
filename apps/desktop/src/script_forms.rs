//! `--script` properties and commands of Prepare form and form data.
//!
//! Queries: `prep.count`, `prep.summary`, `prep.placing` (tool index, -1), `prep.kind`,
//! `prep.name`, `prep.rect` (the first selected widget, user space, rounded),
//! `prep.suggestions`, `prep.suggestions.text`, `prep.tab-mode`, `prep.tab-order` (field
//! names of the panel's page in tab order), `form.names` (every field), `field.kind:NAME`,
//! `field.widgets:NAME`, `field.rect:NAME[,N]`, `field.prop:NAME,KEY` (tooltip, required,
//! read-only, default, font, size, align, multiline, password, comb, max-len, format,
//! options, editable, multi-select, caption, action, check, border, background, export).
//!
//! Commands: `prep ACTION [VALUE]` (the panel's actions), `prep-tool NAME` (text, date,
//! check, radio, combo, list, button, signature), `prep-set NAME VALUE` (a panel value,
//! then applied), `prep-add TOOL PAGE X0 Y0 X1 Y1`, `prep-import PATH`,
//! `prep-export-fdf PATH`, `prep-export-xfdf PATH`, `prep-icon PATH`.

use std::path::PathBuf;
use std::rc::Rc;

use pagebara_app_core::form_prep::Tool;
use pagebara_app_core::forms::{ButtonAction, FieldKind, Format};
use slint::{ComponentHandle, Model};

use crate::app::{App, State, pick};
use crate::{AppWindow, PrepState};

const TOOLS: [&str; 8] = [
    "text",
    "date",
    "check",
    "radio",
    "combo",
    "list",
    "button",
    "signature",
];

fn rect(r: [f64; 4]) -> String {
    format!("{:.0},{:.0},{:.0},{:.0}", r[0], r[1], r[2], r[3])
}

fn kind(k: FieldKind) -> &'static str {
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

pub fn query(s: &mut State, ui: &AppWindow, key: &str) -> Option<Result<String, String>> {
    let (k, arg) = key.split_once(':').unwrap_or((key, ""));
    let ps = ui.global::<PrepState>();
    let simple = match k {
        "prep.count" => Some(ps.get_count().to_string()),
        "prep.summary" => Some(ps.get_summary().to_string()),
        "prep.placing" => Some(ps.get_placing().to_string()),
        "prep.kind" => Some(ps.get_kind().to_string()),
        "prep.name" => Some(ps.get_name().to_string()),
        "prep.suggestions" => Some(ps.get_suggestions().row_count().to_string()),
        "prep.suggestions.text" => Some(
            ps.get_suggestions()
                .iter()
                .map(|r| format!("{} ({})", r.text, r.detail))
                .collect::<Vec<_>>()
                .join(" | "),
        ),
        "prep.tab-mode" => Some(ps.get_tab_mode().to_string()),
        "prep.tab-order" => Some(
            ps.get_tab_rows()
                .iter()
                .map(|r| {
                    r.text
                        .split_once(". ")
                        .map_or(r.text.to_string(), |(_, n)| n.to_owned())
                })
                .collect::<Vec<_>>()
                .join(","),
        ),
        _ => None,
    };
    if let Some(v) = simple {
        return Some(Ok(v));
    }
    if !matches!(
        k,
        "prep.rect" | "form.names" | "field.kind" | "field.widgets" | "field.rect" | "field.prop"
    ) {
        return None;
    }
    let backend = s.forms.clone();
    let sel = s.tab_ref().and_then(|t| t.prep.sel.first().copied());
    let Some(t) = s.tab() else {
        return Some(Err("no document is open".into()));
    };
    let doc = t.session.doc().clone();
    let form = t.form(&*backend);
    let field = |name: &str| form.field(name).ok_or_else(|| format!("no field {name}"));
    Some(match k {
        "prep.rect" => sel
            .and_then(|w| {
                let f = form.field_of_widget(w)?;
                let r = f.widgets.iter().find(|x| x.id == w)?.rect;
                Some(rect(r))
            })
            .ok_or_else(|| "nothing is selected".to_owned()),
        "form.names" => Ok(form
            .fields()
            .iter()
            .map(|f| f.name.clone())
            .collect::<Vec<_>>()
            .join(",")),
        "field.kind" => field(arg).map(|f| kind(f.kind).to_owned()),
        "field.widgets" => field(arg).map(|f| f.widgets.len().to_string()),
        "field.rect" => {
            let (name, n): (&str, usize) = match arg.rsplit_once(',') {
                Some((a, b)) if b.parse::<usize>().is_ok() => (a, b.parse().unwrap_or(1)),
                _ => (arg, 1),
            };
            field(name).and_then(|f| {
                f.widgets
                    .get(n.saturating_sub(1))
                    .map(|w| rect(w.rect))
                    .ok_or_else(|| format!("{name} has no widget {n}"))
            })
        }
        _ => {
            let (name, what) = arg.rsplit_once(',').unwrap_or((arg, ""));
            let f = match field(name) {
                Ok(f) => f,
                Err(e) => return Some(Err(e)),
            };
            let Some(p) = form.props(&doc, name) else {
                return Some(Err(format!("no field {name}")));
            };
            let first = f.widgets.first();
            Ok(match what {
                "tooltip" => p.tooltip,
                "required" => p.required.to_string(),
                "read-only" => p.read_only.to_string(),
                "default" => p.default_value,
                "font" => p.font,
                "size" => format!("{}", p.font_size),
                "align" => p.align.to_string(),
                "multiline" => p.multiline.to_string(),
                "password" => p.password.to_string(),
                "comb" => p.comb.to_string(),
                "max-len" => p.max_len.map(|n| n.to_string()).unwrap_or_default(),
                "format" => match p.format {
                    Format::None => "none".into(),
                    Format::Number { decimals, .. } => format!("number {decimals}"),
                    Format::Date(d) => format!("date {d}"),
                    Format::Other => "other".into(),
                },
                "options" => p
                    .options
                    .iter()
                    .map(|o| o.display.clone())
                    .collect::<Vec<_>>()
                    .join(","),
                "editable" => p.editable.to_string(),
                "multi-select" => p.multi_select.to_string(),
                "caption" => p.caption,
                "action" => match p.action {
                    ButtonAction::None => "none".into(),
                    ButtonAction::ResetForm => "reset".into(),
                    ButtonAction::Uri(u) => format!("uri {u}"),
                    ButtonAction::Named(n) => format!("named {n}"),
                    ButtonAction::Other => "other".into(),
                },
                "check" => p.check_style.to_string(),
                "border" => format!("{:?} {}", p.border_color, p.border_width),
                "background" => format!("{:?}", p.background),
                "export" => first.and_then(|w| w.export.clone()).unwrap_or_default(),
                o => return Some(Err(format!("unknown field property {o}"))),
            })
        }
    })
}

/// Sets panel value `name` (as the panel's controls do), then applies it.
fn set(app: &Rc<App>, s: &mut State, ui: &AppWindow, name: &str, v: &str) -> Result<(), String> {
    let g = ui.global::<PrepState>();
    let int = || {
        v.parse::<i32>()
            .map_err(|_| format!("{name}: not a number: {v}"))
    };
    let yes = v == "true";
    match name {
        "name" => {
            g.set_name(v.into());
            app.prep_action(s, ui, "rename", v);
            return Ok(());
        }
        "export" => {
            g.set_export_value(v.into());
            app.prep_action(s, ui, "export", v);
            return Ok(());
        }
        "tooltip" => g.set_tooltip(v.into()),
        "required" => g.set_required(yes),
        "read-only" => g.set_read_only(yes),
        "default" => g.set_default_value(v.into()),
        "font" => g.set_font(int()?),
        "size" => g.set_size(int()?),
        "text-colour" => g.set_text_colour(int()?),
        "border-colour" => g.set_border_colour(int()?),
        "background" => g.set_background(int()?),
        "border-width" => g.set_border_width(int()?),
        "border-style" => g.set_border_style(int()?),
        "align" => g.set_align(int()?),
        "multiline" => g.set_multiline(yes),
        "password" => g.set_password(yes),
        "comb" => g.set_comb(yes),
        "max-len" => g.set_max_len(v.into()),
        "format" => g.set_format(int()?),
        "date-format" => g.set_date_format(v.into()),
        "options" => g.set_options(v.into()),
        "editable" => g.set_editable(yes),
        "multi-select" => g.set_multi_select(yes),
        "caption" => g.set_caption(v.into()),
        "action" => g.set_click_action(int()?),
        "uri" => g.set_uri(v.into()),
        "check" => g.set_check_style(int()?),
        "xfdf-annots" => {
            g.set_xfdf_annots(yes);
            return Ok(());
        }
        o => return Err(format!("unknown panel value {o}")),
    }
    app.prep_action(s, ui, "prop", "");
    Ok(())
}

/// Runs `cmd`, or returns `None` when it is not one of these.
pub fn call(
    app: &Rc<App>,
    s: &mut State,
    ui: &AppWindow,
    cmd: &str,
    args: &[String],
) -> Option<Result<(), String>> {
    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
    match cmd {
        "prep" => app.prep_action(s, ui, &arg(0), &args.get(1..).unwrap_or_default().join(" ")),
        "prep-tool" => {
            let Some(i) = TOOLS.iter().position(|t| *t == arg(0)) else {
                return Some(Err(format!("unknown field type {}", arg(0))));
            };
            app.prep_action(s, ui, "tool", &i.to_string());
        }
        "prep-set" => return Some(set(app, s, ui, &arg(0), &arg(1))),
        "prep-add" => {
            let Some(i) = TOOLS.iter().position(|t| *t == arg(0)) else {
                return Some(Err(format!("unknown field type {}", arg(0))));
            };
            let n: Vec<f64> = args[1..].iter().filter_map(|a| a.parse().ok()).collect();
            let [page, x0, y0, x1, y1] = n[..] else {
                return Some(Err("prep-add TOOL PAGE X0 Y0 X1 Y1".into()));
            };
            app.prep_add(
                s,
                ui,
                Tool::ALL[i],
                (page as usize).saturating_sub(1),
                [x0, y0, x1, y1],
            );
        }
        "prep-import" | "prep-export-fdf" | "prep-export-xfdf" | "prep-icon" => {
            pick::answer_next(PathBuf::from(arg(0)));
            let what = match cmd {
                "prep-import" => "import",
                "prep-export-fdf" => "export-fdf",
                "prep-export-xfdf" => "export-xfdf",
                _ => "icon",
            };
            app.prep_action(s, ui, what, "");
        }
        "prep-tab-order" => {
            // `prep-tab-order MODE`: structure, rows, columns or manual.
            let i = match arg(0).as_str() {
                "structure" => 0,
                "rows" => 1,
                "columns" => 2,
                _ => 3,
            };
            app.prep_action(s, ui, "tab-mode", &i.to_string());
        }
        _ => return None,
    }
    Some(Ok(()))
}
