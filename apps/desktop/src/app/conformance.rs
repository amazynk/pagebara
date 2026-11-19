//! Save as PDF/A (part, check or conversion with its validation report, then a file to
//! save) and the accessibility check (PDF/UA report, jump to an issue, automatic fixes and
//! guided edits as undoable changes).

use std::rc::Rc;
use std::sync::Arc;

use pagebara_app_core::session::write_atomic;
use pagebara_conform::{Fix, Profile, Report, pdfa, pdfua};
use pagebara_i18n::{tr, trn};
use pagebara_syntax::{Document, ObjRef};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{App, State, pick};
use crate::{AccessState, AppState, AppWindow, DialogKind, ElementRow, IssueRow, PdfaState};

#[derive(Default)]
pub struct Conformance {
    /// The converted PDF/A file waiting to be saved, and its report.
    pub pdfa: Option<(Arc<Vec<u8>>, Report)>,
    pub access: Option<Report>,
    /// Structure elements behind the guided-fix rows, by row id.
    elements: Vec<ObjRef>,
    /// The file written last, for scripts.
    pub saved: Option<std::path::PathBuf>,
}

const PARTS: [Profile; 5] = [
    Profile::PdfA1b,
    Profile::PdfA2b,
    Profile::PdfA2u,
    Profile::PdfA3b,
    Profile::PdfA3u,
];

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let a = app.clone();
    ui.global::<PdfaState>().on_action(move |what| {
        let what = what.to_string();
        a.act(move |app, s, ui| app.pdfa_action(s, ui, &what))
    });
    let a = app.clone();
    ui.global::<AccessState>()
        .on_action(move |what, id, value| {
            let (what, value) = (what.to_string(), value.to_string());
            a.act(move |app, s, ui| app.access_action(s, ui, &what, id, &value))
        });
}

/// The report's problems, one row each: the same problem on several pages is one row
/// with the pages listed.
fn issue_rows(r: &Report) -> ModelRc<IssueRow> {
    let mut groups: Vec<(&pagebara_conform::Issue, Vec<usize>)> = Vec::new();
    for i in &r.issues {
        let same = groups
            .iter_mut()
            .find(|(g, _)| g.rule == i.rule && g.message == i.message && g.fix == i.fix);
        match (same, i.page) {
            (Some((_, pages)), Some(p)) => pages.push(p + 1),
            (Some(_), None) => {}
            (None, p) => groups.push((i, p.map(|p| p + 1).into_iter().collect())),
        }
    }
    let rows: Vec<IssueRow> = groups
        .into_iter()
        .map(|(i, pages)| IssueRow {
            rule: i.rule.as_str().into(),
            message: i.message.as_str().into(),
            page: pages.first().map_or(0, |&p| p as i32),
            pages: page_list(&pages).into(),
            fix: match i.fix {
                Some(Fix::Automatic) => 1,
                Some(Fix::Guided) => 2,
                None => 0,
            },
        })
        .collect();
    ModelRc::new(VecModel::from(rows))
}

/// "p. 3", "pp. 1–4, 7": page numbers with runs joined, the first few runs only.
fn page_list(pages: &[usize]) -> String {
    let mut sorted = pages.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for p in sorted {
        match runs.last_mut() {
            Some((_, end)) if *end + 1 == p => *end = p,
            _ => runs.push((p, p)),
        }
    }
    let shown: Vec<String> = runs
        .iter()
        .take(4)
        .map(|&(a, b)| {
            if a == b {
                a.to_string()
            } else {
                format!("{a}–{b}")
            }
        })
        .collect();
    let more = if runs.len() > 4 { " …" } else { "" };
    match runs.as_slice() {
        [] => String::new(),
        [(a, b)] if a == b => tr!("p. {}", a),
        _ => tr!("pp. {}", format!("{}{more}", shown.join(", "))),
    }
}

fn strings(v: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        v.iter()
            .map(|s| SharedString::from(s.as_str()))
            .collect::<Vec<_>>(),
    ))
}

fn summary(r: &Report) -> String {
    if r.passed() {
        tr!("Conforms to {}", r.profile.title())
    } else {
        trn!(
            "{n} problem with {}",
            "{n} problems with {}",
            r.errors(),
            r.profile.title()
        )
    }
}

impl App {
    pub fn pdfa_open(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        ui.global::<PdfaState>().set_phase(0);
        s.conformance.pdfa = None;
    }

    fn pdfa_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str) {
        let g = ui.global::<PdfaState>();
        let profile = PARTS[g.get_part().clamp(0, 4) as usize];
        match what {
            "convert" | "check" => {
                let Some(t) = s.tab_ref() else { return };
                let doc = t.session.doc().clone();
                let convert = what == "convert";
                g.set_phase(1);
                ui.global::<AppState>().set_dialog_error("".into());
                pagebara_thread::spawn("pdfa", move || {
                    let r = if convert {
                        pdfa::convert(&doc, profile).map(|c| (Some(c.bytes), c.changes, c.report))
                    } else {
                        Ok((None, Vec::new(), pagebara_conform::validate(&doc, profile)))
                    };
                    super::post_act(move |_app, s, ui| {
                        let g = ui.global::<PdfaState>();
                        match r {
                            Ok((bytes, changes, report)) => {
                                g.set_passed(report.passed());
                                let mut sum = summary(&report);
                                if bytes.is_some() && report.passed() {
                                    sum = tr!("Converted: conforms to {}", profile.title());
                                }
                                g.set_summary(sum.into());
                                let mut changes = changes;
                                if bytes.is_some() && changes.is_empty() {
                                    changes.push(tr!("Nothing needed to change."));
                                }
                                g.set_changes(strings(&changes));
                                g.set_issues(issue_rows(&report));
                                g.set_phase(2);
                                s.conformance.pdfa = bytes.map(|b| (Arc::new(b), report));
                            }
                            Err(e) => {
                                g.set_phase(0);
                                ui.global::<AppState>()
                                    .set_dialog_error(e.to_string().into());
                            }
                        }
                    });
                });
            }
            "back" => g.set_phase(0),
            "save" => {
                let Some((bytes, _)) = s.conformance.pdfa.clone() else {
                    return;
                };
                let Some(t) = s.tab_ref() else { return };
                let name =
                    pick::derived_name(&t.session.title(), &profile.title().replace('/', "-"));
                pick::save_pdf(
                    self,
                    &tr!("Save as PDF/A"),
                    t.session.path.clone(),
                    &name,
                    move |app, s, ui, out| match write_atomic(&out, &bytes) {
                        Ok(()) => {
                            ui.global::<AppState>().set_dialog(DialogKind::None);
                            s.conformance.saved = Some(out.clone());
                            app.open_fresh(s, ui, out);
                        }
                        Err(e) => ui
                            .global::<AppState>()
                            .set_dialog_error(tr!("The file could not be written: {}", e).into()),
                    },
                );
            }
            _ => {}
        }
    }

    /// Checks the document against PDF/UA on a worker.
    pub fn access_check(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab_ref() else { return };
        let doc = t.session.doc().clone();
        let g = ui.global::<AccessState>();
        g.set_busy(true);
        pagebara_thread::spawn("pdfua", move || {
            let report = pagebara_conform::validate(&doc, Profile::PdfUa1);
            let els = pdfua::elements(&doc);
            super::post_act(move |_app, s, ui| {
                let g = ui.global::<AccessState>();
                g.set_busy(false);
                g.set_passed(report.passed());
                g.set_summary(if report.passed() {
                    tr!("No accessibility problems found").into()
                } else {
                    summary(&report).into()
                });
                g.set_automatic(
                    report
                        .issues
                        .iter()
                        .filter(|i| i.fix == Some(Fix::Automatic))
                        .count() as i32,
                );
                g.set_issues(issue_rows(&report));
                let mut rows = Vec::new();
                s.conformance.elements.clear();
                for e in els.iter() {
                    let kind = match e.role.as_str() {
                        "Figure" | "Formula" => 0,
                        r if r.len() == 2 && r.starts_with('H') => 1,
                        "Table" => 2,
                        _ => continue,
                    };
                    let id = s.conformance.elements.len() as i32;
                    s.conformance.elements.push(e.obj);
                    let title = match kind {
                        0 => tr!("Figure"),
                        1 => tr!("Heading {}", &e.role[1..]),
                        _ => tr!("Table"),
                    };
                    rows.push(ElementRow {
                        id,
                        kind,
                        title: title.into(),
                        page: e.page.map_or(0, |p| p as i32 + 1),
                        alt: e.alt.clone().unwrap_or_default().into(),
                        level: e.role[1..].parse().unwrap_or(1),
                    });
                    if rows.len() >= 200 {
                        break;
                    }
                }
                g.set_elements(ModelRc::new(VecModel::from(rows)));
                s.conformance.access = Some(report);
            });
        });
    }

    fn access_action(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        what: &str,
        id: i32,
        value: &str,
    ) {
        let element = usize::try_from(id)
            .ok()
            .and_then(|i| s.conformance.elements.get(i).copied());
        match what {
            "check" => self.access_check(s, ui),
            "jump" => {
                let page = (id.max(1) - 1) as usize;
                self.jump(s, ui, page, None);
            }
            "fix" => {
                let title = s.tab_ref().map(|t| t.session.title()).map(|t| {
                    t.trim_end_matches(".pdf")
                        .trim_end_matches(".PDF")
                        .to_owned()
                });
                let lang = system_language();
                let opts = pdfua::FixOptions {
                    lang: Some(lang),
                    title,
                };
                if let Some(done) = self.edit(s, ui, &tr!("Accessibility fixes"), |d| {
                    Ok(pdfua::fix_safe(d, &opts))
                }) {
                    self.message(
                        s,
                        &trn!(
                            "{n} fix applied. Save to keep it.",
                            "{n} fixes applied. Save to keep them.",
                            done.len()
                        ),
                    );
                }
                self.access_check(s, ui);
            }
            "alt" => {
                if let Some(e) = element {
                    let text = value.to_owned();
                    self.edit(s, ui, &tr!("Alternative text"), move |d| {
                        Ok(pdfua::set_alt(d, e, &text))
                    });
                    self.access_check(s, ui);
                }
            }
            "level" => {
                if let Some(e) = element {
                    let tag = format!("H{}", value.parse::<u8>().unwrap_or(1).clamp(1, 6));
                    self.edit(s, ui, &tr!("Heading level"), move |d| {
                        Ok(pdfua::set_tag(d, e, &tag))
                    });
                    self.access_check(s, ui);
                }
            }
            "table" => {
                if let Some(e) = element {
                    self.edit(s, ui, &tr!("Table header cells"), move |d| {
                        Ok(table_header_row(d, e))
                    });
                    self.access_check(s, ui);
                }
            }
            _ => {}
        }
    }
}

/// The document language to set: the system's (`en-US` from `en_US.UTF-8`).
fn system_language() -> String {
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .filter_map(|v| std::env::var(v).ok())
        .find(|v| !v.is_empty() && v != "C" && v != "POSIX");
    let tag = locale
        .as_deref()
        .and_then(|l| l.split(['.', '@']).next())
        .map(|l| l.replace('_', "-"))
        .filter(|l| l.len() >= 2 && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'));
    tag.unwrap_or_else(|| "en-US".into())
}

/// Makes the cells of a table's first row header cells; returns whether any changed.
fn table_header_row(doc: &mut Document, table: ObjRef) -> bool {
    let els = pdfua::elements(doc);
    let Some(ti) = els.iter().position(|e| e.obj == table) else {
        return false;
    };
    let depth = els[ti].depth;
    // The first TR below the table, then its cells.
    let mut cells = Vec::new();
    let mut tr_depth = None;
    for e in &els[ti + 1..] {
        if e.depth <= depth {
            break;
        }
        match tr_depth {
            None if e.role == "TR" => tr_depth = Some(e.depth),
            Some(d) if e.depth <= d => break,
            Some(d) if e.depth == d + 1 && (e.role == "TD" || e.role == "TH") => cells.push(e.obj),
            _ => {}
        }
    }
    let mut changed = false;
    for c in cells {
        changed |= pdfua::set_tag(doc, c, "TH");
    }
    changed
}
