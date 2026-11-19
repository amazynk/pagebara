//! `--script` properties and commands for digital signatures, redaction and text
//! recognition (kept apart from `script.rs`).

use std::path::PathBuf;
use std::rc::Rc;

use slint::{ComponentHandle, Model};

use crate::app::{App, State, signing};
use crate::{AppState, AppWindow, OcrState, RedactState, SigningState};

/// The value of `key` for `assert`, or `None` when it is not one of these.
pub fn query(s: &mut State, ui: &AppWindow, key: &str) -> Option<Result<String, String>> {
    let sg = ui.global::<SigningState>();
    let rg = ui.global::<RedactState>();
    let og = ui.global::<OcrState>();
    if let Some(v) = signature_query(s, key) {
        return Some(v);
    }
    if let Some(v) = live_query(s, ui, key) {
        return Some(v);
    }
    Some(Ok(match key {
        "sigs.count" => signing::views(s).map_or(0, |v| v.len()).to_string(),
        "sigs.verdicts" => signing::views(s)
            .unwrap_or_default()
            .iter()
            .map(|v| v.verdict.word())
            .collect::<Vec<_>>()
            .join(" "),
        "sigs.verdict" => signing::verdict_word(s),
        "sigs.banner" => sg.get_banner().to_string(),
        "sigs.text" => signing::panel_text(ui),
        "sigs.panel" => sg.get_panel_open().to_string(),
        "sign.busy" => sg.get_busy().to_string(),
        "sign.ids" => signing::id_choices(ui),
        "sign.with" => sg.get_id_source().to_string(),
        "sign.store-loading" => sg.get_store_loading().to_string(),
        "trust.count" => sg.get_trusted().row_count().to_string(),
        "redact.marks" => crate::app::redaction::count_marks(s, ui).to_string(),
        "redact.hits" => rg.get_hits().row_count().to_string(),
        "redact.hits.text" => crate::app::redaction::hit_texts(s),
        "redact.status" => rg.get_find_status().to_string(),
        "redact.clean" => rg.get_clean().to_string(),
        "redact.findings" => rg.get_findings().iter().collect::<Vec<_>>().join(" | "),
        "redact.busy" => rg.get_busy().to_string(),
        "redact.has-mark" => rg.get_has_mark().to_string(),
        "ocr.offer" => og.get_offer().to_string(),
        "ocr.phase" => og.get_phase().to_string(),
        "ocr.running" => s.ocr.running.to_string(),
        "ocr.scanned" => crate::app::ocr::scanned_pages(s),
        "ocr.words" => s.ocr.last.map_or(0, |l| l.1).to_string(),
        "busy" => ui.global::<AppState>().get_busy().to_string(),
        "dialog.error" => ui.global::<AppState>().get_dialog_error().to_string(),
        _ => return None,
    }))
}

/// Live text: `live.words:P` (words recognised in the pictures of page P), `live.busy`,
/// `live.text:P` (the text selection works on, recognised words included) and what the
/// context menu offers (`menu.picture`, `menu.copy`).
fn live_query(s: &mut State, ui: &AppWindow, key: &str) -> Option<Result<String, String>> {
    let vs = ui.global::<crate::ViewState>();
    let page = |a: &str| a.parse::<usize>().unwrap_or(1).saturating_sub(1);
    Some(Ok(match key.split_once(':') {
        Some(("live.words", p)) => s
            .tab_ref()
            .and_then(|t| t.session.live_ref())
            .map_or(0, |l| l.words(page(p)))
            .to_string(),
        Some(("live.text", p)) => match s.tab() {
            Some(t) => t.session.sel_text(page(p)).text.clone(),
            None => String::new(),
        },
        _ => match key {
            "live.busy" => s
                .tab_ref()
                .and_then(|t| t.session.live_ref())
                .is_some_and(|l| l.busy())
                .to_string(),
            "live.available" => pagebara_app_core::live_text::available().to_string(),
            "menu.picture" => vs.get_ctx_picture().to_string(),
            "menu.copy" => vs.get_ctx_copy().to_string(),
            _ => return None,
        },
    }))
}

/// `sigs.N.PROPERTY`: one signature of the panel (1-based), e.g. `sigs.2.revision`.
fn signature_query(s: &State, key: &str) -> Option<Result<String, String>> {
    let (n, prop) = key.strip_prefix("sigs.")?.split_once('.')?;
    let n: usize = n.parse().ok()?;
    let Some(v) = signing::views(s).and_then(|v| v.get(n.wrapping_sub(1))) else {
        return Some(Err(format!("there is no signature {n}")));
    };
    let lines = |l: &[String]| l.join(" | ");
    Some(Ok(match prop {
        "verdict" => v.verdict.word().into(),
        "summary" => v.summary.clone(),
        "kind" => v.kind.clone(),
        "field" => v.field.clone(),
        "signer" => v.signer.clone(),
        "revision" => v.revision.clone(),
        "ltv" => v.ltv.clone(),
        "timestamp" => v.timestamp.clone(),
        "revocation" => v.revocation.clone(),
        "changes" => v
            .changes
            .iter()
            .map(|(t, ok)| format!("{t} allowed={ok}"))
            .collect::<Vec<_>>()
            .join(" | "),
        "problems" => lines(&v.problems),
        "warnings" => lines(&v.warnings),
        p => return Some(Err(format!("unknown signature property {p}"))),
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
    let sg = ui.global::<SigningState>();
    let rg = ui.global::<RedactState>();
    let og = ui.global::<OcrState>();
    match cmd {
        // Signatures.
        "signatures-panel" => sg.set_panel_open(arg(0) != "false"),
        "sign-dialog" => app.signing_action(s, ui, "open-sign", ""),
        "sign-id" => signing::use_id_file(ui, &PathBuf::from(arg(0)), Some(&arg(1))),
        // The key store's identity `N` (from 1), 0 for the ID file.
        "sign-with" => sg.set_id_source(arg(0).parse().unwrap_or(0)),
        "create-id" => {
            let value = format!("{}\u{1f}{}\u{1f}{}", arg(0), arg(2), arg(1));
            app.signing_action(s, ui, "create-id", &value);
        }
        "sign-place" => sg.set_field_choice(arg(0).parse().unwrap_or(1)),
        "sign-options" => {
            sg.set_reason(arg(0).into());
            sg.set_location(arg(1).into());
            sg.set_certify(arg(2).parse().unwrap_or(0));
            sg.set_lock(arg(3).parse().unwrap_or(0));
        }
        "sign-ink" => sg.set_show_ink(arg(0) == "true"),
        "sign-ltv" => sg.set_ltv(arg(0) != "false"),
        "sign-timestamp" => {
            sg.set_timestamp(!arg(0).is_empty());
            sg.set_tsa_url(arg(0).into());
        }
        "sign" => {
            crate::app::pick::answer_next(PathBuf::from(arg(0)));
            app.signing_action(s, ui, "sign", "");
        }
        "ltv" => {
            crate::app::pick::answer_next(PathBuf::from(arg(0)));
            app.signing_action(s, ui, "ltv", "");
        }
        "trust-signature" => app.signing_action(s, ui, "trust-card", &arg(0)),
        "trust-dialog" => app.signing_action(s, ui, "open-trust", ""),
        "trust-file" => app.add_trust_file(s, ui, &PathBuf::from(arg(0))),
        "untrust-all" => {
            let fps: Vec<String> = sg
                .get_trusted()
                .iter()
                .map(|r| r.fingerprint.to_string())
                .collect();
            for fp in fps {
                app.signing_action(s, ui, "untrust", &fp);
            }
        }
        "signature-card" => sg.invoke_card_clicked(arg(0).parse().unwrap_or(0)),
        // Redaction.
        "redact-mode" => {
            rg.set_mode(arg(0).parse().unwrap_or(0));
            app.redact_action(s, ui, "mode", "");
        }
        "redact-find" => {
            let kind = match arg(0).as_str() {
                "text" => 0,
                "regex" => 1,
                "email" => 2,
                "phone" => 3,
                "iban" => 4,
                "card" => 5,
                k => return Some(Err(format!("unknown kind {k}"))),
            };
            rg.set_find_kind(kind);
            rg.set_find_text(arg(1).into());
            app.redact_action(s, ui, "find", "");
        }
        "redact-style" => {
            rg.set_fill(arg(0).parse().unwrap_or(0));
            rg.set_overlay(arg(1).into());
        }
        "redact-mark" => app.redact_action(s, ui, &format!("mark-{}", arg(0)), ""),
        "redact-mark-style" => {
            rg.set_mark_fill(arg(0).parse().unwrap_or(0));
            rg.set_mark_overlay(arg(1).into());
            app.redact_action(s, ui, "mark-style", "");
        }
        "redact-hit" => rg.invoke_hit_toggled(arg(0).parse().unwrap_or(0)),
        "redact-confirm" => app.redact_action(s, ui, "confirm", ""),
        "redact-apply" => {
            crate::app::pick::answer_next(PathBuf::from(arg(0)));
            app.redact_action(s, ui, "apply", "");
        }
        // Text recognition.
        "ocr-dialog" => app.ocr_action(s, ui, "open"),
        "ocr-options" => {
            og.set_script(arg(0).parse().unwrap_or(0));
            og.set_pages(arg(1).into());
            og.set_force(arg(2) == "true");
        }
        "ocr-start" => app.ocr_action(s, ui, "start"),
        "ocr-cancel" => app.ocr_action(s, ui, "cancel"),
        "about" => ui.global::<AppState>().invoke_about(),
        // Live text: the context menu as a right click at a point of a page (points from
        // its top left as shown) opens it, and one of its items.
        "context-menu" => {
            let page = arg(0).parse::<usize>().unwrap_or(1).saturating_sub(1);
            let (x, y) = (arg(1).parse().unwrap_or(0.0), arg(2).parse().unwrap_or(0.0));
            let Some(at) = s.tab_ref().and_then(|t| t.view.page_to_screen(page, x, y)) else {
                return Some(Err(format!("page {} is not shown", page + 1)));
            };
            app.context_menu(
                s,
                ui,
                crate::app::Pointer {
                    kind: 0,
                    x: at.0,
                    y: at.1,
                    button: 1,
                    ctrl: false,
                    shift: false,
                    alt: false,
                },
            );
        }
        "context-action" => app.context_action(s, ui, &arg(0)),
        _ => return None,
    }
    Some(Ok(()))
}
