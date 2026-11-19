//! `--script` properties and commands for Optimize, Save as PDF/A and the accessibility
//! check.

use std::path::PathBuf;
use std::rc::Rc;

use slint::{ComponentHandle, Model};

use crate::app::{App, State, pick};
use crate::{AccessState, AppState, AppWindow, DialogKind, OptimizeState, PdfaState};

/// The value of `key` for `assert`, or `None` when it is not one of these.
pub fn query(s: &mut State, ui: &AppWindow, key: &str) -> Option<Result<String, String>> {
    let og = ui.global::<OptimizeState>();
    let pg = ui.global::<PdfaState>();
    let ag = ui.global::<AccessState>();
    Some(Ok(match key {
        "optimize.busy" => og.get_busy().to_string(),
        "optimize.preset" => og.get_preset().to_string(),
        "optimize.after" => og.get_after().to_string(),
        "optimize.saved" => og.get_saved().to_string(),
        "optimize.signed" => og.get_signed().to_string(),
        "optimize.rows" => og.get_rows().row_count().to_string(),
        "optimize.smaller" => s
            .optimize
            .last
            .as_ref()
            .is_some_and(|l| l.2.after < l.2.before)
            .to_string(),
        "pdfa.phase" => pg.get_phase().to_string(),
        "pdfa.passed" => pg.get_passed().to_string(),
        "pdfa.summary" => pg.get_summary().to_string(),
        "pdfa.issues" => pg.get_issues().row_count().to_string(),
        "pdfa.changes" => pg.get_changes().row_count().to_string(),
        "access.busy" => ag.get_busy().to_string(),
        "access.passed" => ag.get_passed().to_string(),
        "access.summary" => ag.get_summary().to_string(),
        "access.issues" => ag.get_issues().row_count().to_string(),
        "access.automatic" => ag.get_automatic().to_string(),
        "access.elements" => ag.get_elements().row_count().to_string(),
        "access.rules" => ag
            .get_issues()
            .iter()
            .map(|i| i.rule.to_string())
            .collect::<Vec<_>>()
            .join(" "),
        "optimize.file" => s
            .optimize
            .saved
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        "pdfa.file" => s
            .conformance
            .saved
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        _ => return None,
    }))
}

pub fn call(
    _app: &Rc<App>,
    _s: &mut State,
    ui: &AppWindow,
    cmd: &str,
    args: &[String],
) -> Option<Result<(), String>> {
    let arg = |i: usize| args.get(i).cloned().unwrap_or_default();
    let num = |i: usize| arg(i).parse::<i32>().unwrap_or(0);
    let og = ui.global::<OptimizeState>();
    let pg = ui.global::<PdfaState>();
    let st = ui.global::<AppState>();
    match cmd {
        "optimize" => st.invoke_show_dialog(DialogKind::Optimize),
        "optimize-preset" => {
            og.set_preset(num(0));
            og.invoke_action("preset".into());
        }
        "optimize-resolution" => {
            og.set_resolution(num(0));
            og.invoke_action("changed".into());
        }
        "optimize-option" => {
            let on = arg(1) != "false";
            match arg(0).as_str() {
                "metadata" => og.set_strip_metadata(on),
                "thumbnails" => og.set_strip_thumbnails(on),
                "private" => og.set_strip_private(on),
                "scripts" => og.set_strip_scripts(on),
                "attachments" => og.set_strip_attachments(on),
                "comments" => og.set_strip_comments(on),
                "grey" => og.set_grayscale(on),
                "linearize" => og.set_linearize(on),
                "maximum" => og.set_maximum(on),
                o => return Some(Err(format!("unknown option {o}"))),
            }
            og.invoke_action("changed".into());
        }
        "optimize-save" => {
            pick::answer_next(PathBuf::from(arg(0)));
            og.invoke_action("save".into());
        }
        "pdfa" => st.invoke_show_dialog(DialogKind::Pdfa),
        "pdfa-part" => pg.set_part(num(0)),
        "pdfa-convert" => pg.invoke_action("convert".into()),
        "pdfa-check" => pg.invoke_action("check".into()),
        "pdfa-save" => {
            pick::answer_next(PathBuf::from(arg(0)));
            pg.invoke_action("save".into());
        }
        "access" => st.invoke_show_dialog(DialogKind::AccessCheck),
        "access-fix" => ui
            .global::<AccessState>()
            .invoke_action("fix".into(), 0, "".into()),
        "access-alt" => {
            ui.global::<AccessState>()
                .invoke_action("alt".into(), num(0), arg(1).into())
        }
        "access-level" => {
            ui.global::<AccessState>()
                .invoke_action("level".into(), num(0), arg(1).into())
        }
        "access-jump" => ui
            .global::<AccessState>()
            .invoke_action("jump".into(), num(0), "".into()),
        _ => return None,
    }
    Some(Ok(()))
}
