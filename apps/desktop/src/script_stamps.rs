//! `--script` properties and commands for the output tools (watermark, header and footer,
//! Bates numbering, flatten) and the comparison view.

use std::path::PathBuf;
use std::rc::Rc;

use slint::{ComponentHandle, Model};

use crate::app::{App, State};
use crate::{AppWindow, CompareState, ProState};

/// The value of `key` for `assert`, or `None` when it is not one of these.
pub fn query(s: &mut State, ui: &AppWindow, key: &str) -> Option<Result<String, String>> {
    let g = ui.global::<ProState>();
    let c = ui.global::<CompareState>();
    let doc = || s.tab_ref().map(|t| t.session.doc().clone());
    Some(Ok(match key {
        "pro.dialog" => g.get_dialog().to_string(),
        "pro.found" => g.get_found().to_string(),
        "pro.error" => g.get_error().to_string(),
        "pro.status" => g.get_status().to_string(),
        "pro.busy" => g.get_busy().to_string(),
        "pro.preview" => {
            let p = g.get_preview().size();
            if p.width == 0 {
                "none".into()
            } else {
                format!("{}x{}", p.width, p.height)
            }
        }
        "pro.counts" => g.get_fl_counts().to_string(),
        "pro.warning" => g.get_fl_warning().to_string(),
        "pro.files" => g.get_bates_files().row_count().to_string(),
        "pro.last" => s.pro.last.clone(),
        // Our stamps in the active document: count, then the kinds.
        "stamps" => doc()
            .map_or(0, |d| pagebara_stamp::find(&d, false).len())
            .to_string(),
        "stamps.kinds" => {
            let mut k: Vec<String> = doc()
                .map(|d| pagebara_stamp::find(&d, false))
                .unwrap_or_default()
                .iter()
                .map(|f| format!("{:?}", f.kind))
                .collect();
            k.dedup();
            k.join(" ")
        }
        "flatten.contents" => doc()
            .map(|d| {
                let c = pagebara_flatten::contents(&d);
                format!(
                    "annotations={} links={} fields={} layers={}",
                    c.annotations, c.links, c.fields, c.layers
                )
            })
            .unwrap_or_default(),
        "compare.visible" => c.get_visible().to_string(),
        "compare.busy" => c.get_busy().to_string(),
        "compare.rows" => c.get_rows().row_count().to_string(),
        "compare.rows.text" => c
            .get_rows()
            .iter()
            .map(|r| format!("{} {}: {}", r.kind, r.at, r.text))
            .collect::<Vec<_>>()
            .join(" | "),
        "compare.current" => c.get_current().to_string(),
        "compare.pair" => format!("{}/{}", c.get_pair() + 1, c.get_pairs()),
        "compare.labels" => format!("{} | {}", c.get_left_label(), c.get_right_label()),
        "compare.summary" => c.get_summary().to_string(),
        "compare.marks" => format!(
            "{} {}",
            c.get_left_marks().row_count(),
            c.get_right_marks().row_count()
        ),
        "compare.images" => format!(
            "{} {}",
            c.get_left().size().width > 0,
            c.get_right().size().width > 0
        ),
        _ => return None,
    }))
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
    let g = ui.global::<ProState>();
    let c = ui.global::<CompareState>();
    match cmd {
        // `pro-open N`: 1 watermark, 2 header and footer, 3 Bates, 4 flatten, 5 compare.
        "pro-open" => app.pro_action(s, ui, "open", &arg(0)),
        // `pro-set NAME VALUE`: a field of the open dialog, then the preview follows.
        "pro-set" => {
            if let Err(e) = set(&g, &arg(0), &arg(1)) {
                return Some(Err(e));
            }
            app.pro_action(s, ui, "changed", "");
        }
        "pro-file" => {
            crate::app::pick::answer_next(PathBuf::from(arg(0)));
            let what = if g.get_dialog() == crate::app::stamps::COMPARE {
                "choose-compare"
            } else {
                "choose-file"
            };
            app.pro_action(s, ui, what, "");
        }
        "pro-add-file" => {
            crate::app::pick::answer_next(PathBuf::from(arg(0)));
            app.pro_action(s, ui, "add-files", "");
        }
        "pro-apply" => app.pro_action(s, ui, "apply", ""),
        "pro-remove" => app.pro_action(s, ui, "remove", ""),
        "pro-preset-save" => app.pro_action(s, ui, "save-preset", ""),
        "pro-preset-load" => app.pro_action(s, ui, "load-preset", &arg(0)),
        "pro-cancel" => app.pro_action(s, ui, "cancel", ""),
        "compare-select" => app.compare_action(s, ui, "select", &arg(0)),
        "compare-next" => app.compare_action(s, ui, "next", ""),
        "compare-pair" => app.compare_action(s, ui, "pair", &arg(0)),
        "compare-mode" => c.set_mode(arg(0).parse().unwrap_or(0)),
        // `compare-filter TEXT VISUAL ANNOTATIONS DOCUMENT` with true or false each.
        "compare-filter" => {
            c.set_show_text(arg(0) != "false");
            c.set_show_visual(arg(1) != "false");
            c.set_show_annots(arg(2) != "false");
            c.set_show_doc(arg(3) != "false");
            app.compare_action(s, ui, "filter", "");
        }
        "compare-report" => {
            crate::app::pick::answer_next(PathBuf::from(arg(0)));
            app.compare_action(s, ui, "report", "");
        }
        "compare-close" => app.compare_action(s, ui, "close", ""),
        _ => return None,
    }
    Some(Ok(()))
}

fn set(g: &ProState<'_>, name: &str, v: &str) -> Result<(), String> {
    let int = || {
        v.parse::<i32>()
            .map_err(|_| format!("{name}: not a number: {v}"))
    };
    let float = || {
        v.parse::<f32>()
            .map_err(|_| format!("{name}: not a number: {v}"))
    };
    let yes = v == "true";
    match name {
        "wm-source" => g.set_wm_source(int()?),
        "wm-text" => g.set_wm_text(v.into()),
        "wm-page" => g.set_wm_page(v.into()),
        "font" => g.set_font(int()?),
        "colour" => g.set_colour(int()?),
        "wm-size" => g.set_wm_size(float()?),
        "wm-opacity" => g.set_wm_opacity(float()?),
        "wm-rotation" => g.set_wm_rotation(int()?),
        "wm-anchor" => g.set_wm_anchor(int()?),
        "wm-tiled" => g.set_wm_tiled(yes),
        "wm-behind" => g.set_wm_behind(yes),
        "wm-visibility" => g.set_wm_visibility(int()?),
        "pages" => g.set_pages(v.into()),
        "parity" => g.set_parity(int()?),
        "hf-tl" => g.set_hf_tl(v.into()),
        "hf-tc" => g.set_hf_tc(v.into()),
        "hf-tr" => g.set_hf_tr(v.into()),
        "hf-bl" => g.set_hf_bl(v.into()),
        "hf-bc" => g.set_hf_bc(v.into()),
        "hf-br" => g.set_hf_br(v.into()),
        "hf-size" => g.set_hf_size(float()?),
        "hf-start" => g.set_hf_start(v.into()),
        "hf-margin" => g.set_hf_margin(v.into()),
        "hf-skip-first" => g.set_hf_skip_first(yes),
        "bates-prefix" => g.set_bates_prefix(v.into()),
        "bates-suffix" => g.set_bates_suffix(v.into()),
        "bates-digits" => g.set_bates_digits(v.into()),
        "bates-start" => g.set_bates_start(v.into()),
        "bates-position" => g.set_bates_position(int()?),
        "bates-box" => g.set_bates_box(yes),
        "fl-annots" => g.set_fl_annots(yes),
        "fl-links" => g.set_fl_links(yes),
        "fl-forms" => g.set_fl_forms(yes),
        "fl-signatures" => g.set_fl_signatures(yes),
        "fl-layers" => g.set_fl_layers(int()?),
        "fl-transparency" => g.set_fl_transparency(yes),
        "cmp-text" => g.set_cmp_text(yes),
        "cmp-visual" => g.set_cmp_visual(yes),
        "cmp-annots" => g.set_cmp_annots(yes),
        o => return Err(format!("unknown field {o}")),
    }
    Ok(())
}
