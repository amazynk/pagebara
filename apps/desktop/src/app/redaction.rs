//! Redaction: the Redact tool marks text (by dragging, or by search for text, patterns and
//! personal data) and areas as `/Redact` annotations, each mark an undoable edit. "Apply"
//! removes what is under the marks for good into a new file (applied, sanitised and fully
//! rewritten off the UI thread), opens it, and shows the check of that file.

use std::path::PathBuf;
use std::rc::Rc;

use pagebara_annot::Kind;
use pagebara_app_core::pages::PageGeom;
use pagebara_app_core::redaction as core;
use pagebara_app_core::session::{SaveMode, write_atomic};
use pagebara_app_core::text_index;
use pagebara_app_core::view::Placement;
use pagebara_i18n::{tr, trn};
use pagebara_redact::{ApplyOptions, Hit, Mark, Sanitize};
use pagebara_syntax::{Document, ObjRef};
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};

use super::document::EditError;
use super::{App, Sel, State, Tab, pick};
use crate::{AppState, AppWindow, DialogKind, HitRow, OverlayItem, RedactState};

#[derive(Default)]
pub struct Redaction {
    /// Search results of "Find & mark" for the tab `uid`, each with its check box.
    hits: Vec<(Hit, bool)>,
    hits_uid: u64,
    /// The last check of a redacted file, for scripts.
    pub clean: Option<bool>,
    /// The tab and document generation `RedactState.marks` was counted for.
    counted: Option<(u64, u64)>,
}

impl From<pagebara_redact::Error> for EditError {
    fn from(e: pagebara_redact::Error) -> Self {
        match e {
            pagebara_redact::Error::NotPermitted => EditError {
                text: tr!("This document's security settings do not allow redacting it."),
                permission: true,
            },
            e => EditError {
                text: tr!("That did not work: {}.", e),
                permission: false,
            },
        }
    }
}

pub fn is_mark(kind: &Kind) -> bool {
    matches!(kind, Kind::Other(s) if s == "Redact")
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let g = ui.global::<RedactState>();
    g.set_find_kinds(ModelRc::new(VecModel::from(
        core::FIND_KINDS
            .iter()
            .map(|k| SharedString::from(pagebara_i18n::translate(k, &[])))
            .collect::<Vec<_>>(),
    )));
    g.set_sanitize_labels(ModelRc::new(VecModel::from(
        core::SANITIZE_LABELS
            .iter()
            .map(|k| SharedString::from(pagebara_i18n::translate(k, &[])))
            .collect::<Vec<_>>(),
    )));
    g.set_sanitize(ModelRc::new(VecModel::from(
        core::sanitize_flags(&Sanitize::default()).to_vec(),
    )));
    let a = app.clone();
    g.on_action(move |what, value| {
        let (what, value) = (what.to_string(), value.to_string());
        a.act(move |app, s, ui| app.redact_action(s, ui, &what, &value))
    });
    let a = app.clone();
    g.on_hit_toggled(move |i| {
        a.act(move |_app, s, ui| {
            if let Some(h) = s.redaction.hits.get_mut(i as usize) {
                h.1 = !h.1;
            }
            show_hits(s, ui);
        })
    });
}

/// The number of marks and the selected mark's style, after any change.
pub fn sync(s: &mut State, ui: &AppWindow) {
    let g = ui.global::<RedactState>();
    // The panel of the Redact tool shows the count.
    if ui.global::<AppState>().get_tool() == 6 {
        count_marks(s, ui);
    }
    let uid = s.tab_ref().map(|t| t.uid);
    if let Some(uid) = uid
        && uid != s.redaction.hits_uid
    {
        s.redaction.hits_uid = uid;
        s.redaction.hits.clear();
        g.set_find_status("".into());
        show_hits(s, ui);
    }
    sync_selection(s, ui);
}

/// The number of marks in the active document. Reads the annotations of every page, so
/// it is counted only for the redaction panel (or a script), once per revision.
pub fn count_marks(s: &mut State, ui: &AppWindow) -> i32 {
    let g = ui.global::<RedactState>();
    let key = s.tab_ref().map(|t| (t.uid, t.session.generation()));
    if key != s.redaction.counted {
        s.redaction.counted = key;
        let n = s.tab_ref().map_or(0, |t| {
            (0..t.session.page_count())
                .filter_map(|i| t.session.page_ref(i))
                .map(|r| pagebara_redact::marks(t.session.doc(), r).len())
                .sum::<usize>()
        });
        g.set_marks(n as i32);
    }
    g.get_marks()
}

/// Whether the selected annotation is a mark, and its style.
pub fn sync_selection(s: &mut State, ui: &AppWindow) {
    let g = ui.global::<RedactState>();
    let mark = selected_mark(s).map(|(_, m)| m);
    g.set_has_mark(mark.is_some());
    if let Some(m) = mark {
        g.set_mark_overlay(m.overlay_text.clone().unwrap_or_default().into());
        g.set_mark_fill(core::fill_index(&m) as i32);
    }
}

fn selected_mark(s: &mut State) -> Option<(Sel, Mark)> {
    let t = s.tab()?;
    let sel = t.selected?;
    let a = t.annot(sel)?;
    if !is_mark(&a.kind) {
        return None;
    }
    let d = t.session.doc().get(sel.id)?;
    Some((
        sel,
        pagebara_redact::read_mark(t.session.doc(), d.as_dict()?),
    ))
}

fn show_hits(s: &State, ui: &AppWindow) {
    let rows: Vec<HitRow> = s
        .redaction
        .hits
        .iter()
        .map(|(h, on)| HitRow {
            page: h.page as i32 + 1,
            text: h.text.as_str().into(),
            checked: *on,
        })
        .collect();
    ui.global::<RedactState>()
        .set_hits(ModelRc::new(VecModel::from(rows)));
}

/// The marks on page `page`, drawn over it so they stand out from other annotations.
pub fn overlay(out: &mut Vec<OverlayItem>, tab: &Tab, pl: &Placement, g: &PageGeom, page: usize) {
    let Some(r) = tab.session.page_ref(page) else {
        return;
    };
    let red = slint::Color::from_rgb_u8(0xDC, 0x26, 0x26);
    for (_, m) in pagebara_redact::marks(tab.session.doc(), r) {
        let fill = match m.fill {
            Some([v, ..]) if v > 0.5 => slint::Color::from_argb_u8(0x55, 0xD4, 0xD4, 0xD8),
            Some(_) => slint::Color::from_argb_u8(0x66, 0x18, 0x18, 0x1B),
            None => slint::Color::from_argb_u8(0x22, 0xDC, 0x26, 0x26),
        };
        for q in &m.quads {
            let [x0, y0, x1, y1] = core::quad_bbox(q);
            let [x, y, w, h] = g.rect_to_view([x0, y0, x1, y1]);
            out.push(OverlayItem {
                di: pl.di,
                by: pl.by + y as f32,
                bx: pl.bx + x as f32,
                w: w.max(0.5) as f32,
                h: h.max(0.5) as f32,
                kind: 1,
                fill,
                stroke: red,
                width: 1.0,
                ..Default::default()
            });
        }
        if let (Some(text), Some(b)) = (&m.overlay_text, m.bbox()) {
            let [x, y, w, h] = g.rect_to_view([b.x0, b.y0, b.x1, b.y1]);
            out.push(OverlayItem {
                di: pl.di,
                by: pl.by + y as f32,
                bx: pl.bx + x as f32,
                w: w as f32,
                h: h as f32,
                kind: 3,
                text: text.as_str().into(),
                font_size: (h * 0.6).clamp(5.0, 14.0) as f32,
                stroke: red,
                ..Default::default()
            });
        }
    }
}

impl App {
    pub fn redact_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str, value: &str) {
        let g = ui.global::<RedactState>();
        match what {
            "mode" => {
                ui.global::<crate::ViewState>()
                    .set_cursor(if g.get_mode() == 1 { 3 } else { 1 });
            }
            "find" => self.redact_find(s, ui),
            "show-hit" => {
                let hit = s.redaction.hits.get(value.parse().unwrap_or(usize::MAX));
                if let Some((h, _)) = hit
                    && let Some(t) = s.tab_ref()
                    && let Some(q) = h.quads.first()
                {
                    let [x0, y0, x1, y1] = core::quad_bbox(q);
                    let [_, y, _, _] = t.session.geoms[h.page].rect_to_view([x0, y0, x1, y1]);
                    let page = h.page;
                    self.jump(s, ui, page, Some(y));
                }
            }
            "mark-all" | "mark-selected" => {
                let all = what == "mark-all";
                let chosen: Vec<Hit> = s
                    .redaction
                    .hits
                    .iter()
                    .filter(|(_, on)| all || *on)
                    .map(|(h, _)| h.clone())
                    .collect();
                if chosen.is_empty() {
                    return self.message(s, &tr!("Select the matches to mark first."));
                }
                let marks: Vec<(usize, Mark)> = chosen
                    .iter()
                    .map(|h| (h.page, self.styled(ui, core::joined(h.mark()))))
                    .collect();
                let n = marks.len();
                if self.add_marks(s, ui, marks).is_some() {
                    s.redaction.hits.clear();
                    show_hits(s, ui);
                    g.set_find_status(tr!("Marked {} for redaction.", n).into());
                }
            }
            "mark-style" => {
                let Some((sel, mut m)) = selected_mark(s) else {
                    return;
                };
                m = core::styled(m, g.get_mark_fill().max(0) as usize, &g.get_mark_overlay());
                let Some(page) = s.tab_ref().and_then(|t| t.session.page_ref(sel.page)) else {
                    return;
                };
                let new = self.edit(s, ui, &tr!("Change redaction mark"), |d| {
                    pagebara_redact::remove_mark(d, page, sel.id)?;
                    Ok(pagebara_redact::add_mark(d, page, &m)?)
                });
                if let Some(id) = new
                    && let Some(t) = s.tab()
                {
                    t.selected = Some(Sel { page: sel.page, id });
                    self.selection_changed(s, ui);
                }
            }
            "confirm" => {
                ui.global::<AppState>().set_dialog_error("".into());
                ui.global::<AppState>().set_dialog(DialogKind::RedactApply);
            }
            "apply" => self.redact_apply(s, ui),
            _ => {}
        }
    }

    fn styled(&self, ui: &AppWindow, m: Mark) -> Mark {
        let g = ui.global::<RedactState>();
        core::styled(m, g.get_fill().max(0) as usize, &g.get_overlay())
    }

    /// Adds marks as one undoable step.
    fn add_marks(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        marks: Vec<(usize, Mark)>,
    ) -> Option<Vec<ObjRef>> {
        let t = s.tab_ref()?;
        let refs: Vec<Option<ObjRef>> = marks.iter().map(|(p, _)| t.session.page_ref(*p)).collect();
        let label = if marks.len() == 1 {
            tr!("Mark for redaction")
        } else {
            tr!("Mark {} for redaction", marks.len())
        };
        self.edit(s, ui, &label, |d| {
            let mut out = Vec::new();
            for ((_, m), r) in marks.iter().zip(&refs) {
                let r = r.ok_or_else(|| EditError::from(tr!("That page is gone.")))?;
                out.push(pagebara_redact::add_mark(d, r, m)?);
            }
            Ok(out)
        })
    }

    /// The text selection becomes a mark (the Redact tool's "Mark text" mode).
    pub fn redact_selection(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab() else { return };
        let Some(sel) = t.text_sel.take() else { return };
        let text = t.session.text.page(sel.page);
        let boxes = text_index::line_boxes(&text, sel.range());
        if boxes.is_empty() {
            return self.overlay_changed(s, ui);
        }
        let m = self.styled(ui, core::mark_boxes(&boxes));
        self.add_marks(s, ui, vec![(sel.page, m)]);
        self.overlay_changed(s, ui);
    }

    /// A box was drawn with the Redact tool (displayed points on `page`).
    pub fn redact_area(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        page: usize,
        from: (f64, f64),
        to: (f64, f64),
    ) {
        let Some(t) = s.tab_ref() else { return };
        let (w, h) = ((to.0 - from.0).abs(), (to.1 - from.1).abs());
        if w < 3.0 || h < 3.0 {
            return self.overlay_changed(s, ui);
        }
        let g = t.session.geoms[page];
        let r = g.rect_to_pdf([from.0.min(to.0), from.1.min(to.1), w, h]);
        let m = self.styled(ui, core::mark_rect(r));
        self.add_marks(s, ui, vec![(page, m)]);
        self.overlay_changed(s, ui);
    }

    fn redact_find(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let g = ui.global::<RedactState>();
        let pattern = match core::pattern(
            g.get_find_kind().max(0) as usize,
            &g.get_find_text(),
            g.get_match_case(),
            g.get_whole_word(),
        ) {
            Ok(p) => p,
            Err(e) => return g.set_find_status(e.into()),
        };
        let Some(t) = s.tab_ref() else { return };
        let (uid, doc) = (t.uid, t.session.doc().clone());
        g.set_finding(true);
        g.set_find_status(tr!("Searching…").into());
        pagebara_thread::spawn("redact-find", move || {
            let hits = pagebara_redact::find(&doc, &pattern, None);
            super::post_act(move |_app, s, ui| {
                let g = ui.global::<RedactState>();
                g.set_finding(false);
                if s.tab_ref().map(|t| t.uid) != Some(uid) {
                    return;
                }
                g.set_find_status(match hits.len() {
                    0 => tr!("No matches.").into(),
                    n => trn!("{n} match.", "{n} matches.", n).into(),
                });
                s.redaction.hits_uid = uid;
                s.redaction.hits = hits.into_iter().map(|h| (h, true)).collect();
                show_hits(s, ui);
            });
        });
    }

    /// Applies every mark into a new file, opens it and shows the check.
    fn redact_apply(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab_ref() else { return };
        let doc = t.session.doc().clone();
        if !doc.permissions().modify && !doc.is_owner() {
            return self.ask_owner_password(
                s,
                ui,
                &tr!("This document's security settings do not allow redacting it."),
            );
        }
        // What the marks cover, to look for once it is applied.
        let mut covered = Vec::new();
        for i in 0..t.session.page_count() {
            let Some(r) = t.session.page_ref(i) else {
                continue;
            };
            let marks = pagebara_redact::marks(&doc, r);
            if marks.is_empty() {
                continue;
            }
            let text = t.session.text.page(i);
            covered.extend(marks.iter().map(|(_, m)| core::text_under(&text, m)));
        }
        let secrets = core::secrets(covered);
        let password = t
            .session
            .save_job(PathBuf::new(), SaveMode::Compact)
            .passwords
            .into_iter()
            .next();
        let flags: Vec<bool> = ui.global::<RedactState>().get_sanitize().iter().collect();
        let clean = core::sanitize_from(&flags);
        let name = pick::derived_name(&t.session.title(), "redacted");
        let near = t.session.path.clone();
        pick::save_pdf(
            self,
            &tr!("Save the redacted document"),
            near,
            &name,
            move |app, s, ui, out| {
                app.run_redact(s, ui, doc, clean, secrets, password, out);
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn run_redact(
        self: &Rc<Self>,
        _s: &mut State,
        ui: &AppWindow,
        doc: Document,
        clean: Sanitize,
        secrets: Vec<String>,
        password: Option<String>,
        out: PathBuf,
    ) {
        ui.global::<RedactState>().set_busy(true);
        ui.global::<AppState>().set_busy(true);
        pagebara_thread::spawn("redact", move || {
            let result = (|| -> Result<(bool, Vec<String>, Vec<String>), Fail> {
                let mut d = doc;
                let report = pagebara_redact::apply(&mut d, &ApplyOptions::default())?;
                let cleaned = pagebara_redact::sanitize(&mut d, &clean);
                // The covered words also leave bookmarks, destinations and metadata.
                let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
                let scrubbed = pagebara_redact::scrub(&mut d, &refs);
                let bytes = pagebara_redact::save(&d)?;
                write_atomic(&out, &bytes)
                    .map_err(|e| Fail::Text(tr!("The file could not be written: {}", e)))?;
                let check =
                    pagebara_redact::verify(&bytes, password.as_deref(), &report.regions(), &refs);
                let (ok, findings) = core::report_lines(&check);
                let mut notes = report.warnings.clone();
                if report.signatures_invalidated > 0 {
                    notes.push(trn!(
                        "{n} signature no longer applies to the redacted document.",
                        "{n} signatures no longer apply to the redacted document.",
                        report.signatures_invalidated
                    ));
                }
                if cleaned.metadata + cleaned.scripts + cleaned.embedded_files > 0 {
                    notes.push(tr!(
                        "Also removed: {}, {}, {}.",
                        trn!(
                            "{n} metadata entry",
                            "{n} metadata entries",
                            cleaned.metadata
                        ),
                        trn!("{n} script", "{n} scripts", cleaned.scripts),
                        trn!(
                            "{n} attached file",
                            "{n} attached files",
                            cleaned.embedded_files
                        )
                    ));
                }
                if report.glyphs_blanked > 0 {
                    notes.push(trn!(
                        "The shape of {n} removed character was taken out of the embedded fonts.",
                        "The shapes of {n} removed characters were taken out of the embedded fonts.",
                        report.glyphs_blanked
                    ));
                }
                if scrubbed > 0 {
                    notes.push(trn!(
                        "The redacted words were also replaced in {n} bookmark, link, metadata or \
                         other entry.",
                        "The redacted words were also replaced in {n} bookmark, link, metadata or \
                         other entries.",
                        scrubbed
                    ));
                }
                Ok((ok, findings, notes))
            })();
            super::post_act(move |app, s, ui| {
                ui.global::<RedactState>().set_busy(false);
                ui.global::<AppState>().set_busy(false);
                match result {
                    Ok((ok, findings, notes)) => {
                        s.redaction.clean = Some(ok);
                        let g = ui.global::<RedactState>();
                        g.set_clean(ok);
                        g.set_report_file(
                            out.file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default()
                                .into(),
                        );
                        g.set_findings(lines(&findings));
                        g.set_warnings(lines(&notes));
                        app.open_fresh(s, ui, out);
                        ui.global::<AppState>().set_dialog(DialogKind::RedactReport);
                    }
                    Err(Fail::Permission) => {
                        ui.global::<AppState>().set_dialog(DialogKind::None);
                        app.ask_owner_password(
                            s,
                            ui,
                            &tr!("This document's security settings do not allow redacting it."),
                        );
                    }
                    Err(Fail::Text(e)) => {
                        ui.global::<AppState>()
                            .set_dialog_error(tr!("Redacting failed: {}.", e).into());
                    }
                }
            });
        });
    }
}

enum Fail {
    Permission,
    Text(String),
}

impl From<pagebara_redact::Error> for Fail {
    fn from(e: pagebara_redact::Error) -> Self {
        match e {
            pagebara_redact::Error::NotPermitted => Fail::Permission,
            e => Fail::Text(e.to_string()),
        }
    }
}

fn lines(v: &[String]) -> ModelRc<SharedString> {
    ModelRc::new(VecModel::from(
        v.iter().map(SharedString::from).collect::<Vec<_>>(),
    ))
}

/// For scripts: the hit texts.
pub fn hit_texts(s: &State) -> String {
    s.redaction
        .hits
        .iter()
        .map(|(h, _)| h.text.as_str())
        .collect::<Vec<_>>()
        .join(", ")
}
