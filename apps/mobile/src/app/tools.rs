//! Output tools on the phone: a smaller copy (lossless, balanced or small preset), a
//! PDF/A copy, and an accessibility check with its automatic fixes. Copies are written to
//! the cache and handed to the platform's "save as".

use std::path::PathBuf;
use std::rc::Rc;

use pagebara_conform::{Fix, Profile, pdfa, pdfua};
use pagebara_i18n::{decimal, tr, trn};
use pagebara_optimize::Preset;
use slint::ComponentHandle;

use super::{App, State, post_act};
use crate::{AppState, MobileWindow, Sheet};

pub fn wire(app: &Rc<App>, ui: &MobileWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_optimize(move |p| a.act(move |app, s, ui| app.make_smaller(s, ui, p)));
    let a = app.clone();
    st.on_save_pdfa(move || a.act(|app, s, ui| app.save_pdfa(s, ui)));
    let a = app.clone();
    st.on_check_access(move || a.act(|app, s, ui| app.check_access(s, ui)));
    let a = app.clone();
    st.on_fix_access(move || a.act(|app, s, ui| app.fix_access(s, ui)));
}

fn size(b: u64) -> String {
    match b {
        b if b >= 1 << 20 => tr!("{} MB", decimal(b as f64 / (1 << 20) as f64, 1)),
        b if b >= 1 << 10 => tr!("{} KB", decimal(b as f64 / 1024.0, 0)),
        b => trn!("{n} byte", "{n} bytes", b),
    }
}

impl App {
    /// Writes `bytes` to the cache as `name` and offers to save it.
    pub(super) fn hand_out(
        self: &Rc<Self>,
        s: &mut State,
        name: &str,
        bytes: &[u8],
    ) -> Option<PathBuf> {
        let dir = self.platform.cache_dir().join("share");
        let path = dir.join(name);
        let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, bytes));
        if let Err(e) = written {
            self.message(s, &tr!("Could not prepare the copy: {}", e));
            return None;
        }
        self.platform.save_copy(&path, name);
        s.tools_output = Some(path.clone());
        Some(path)
    }

    pub(super) fn stem(s: &State) -> Option<String> {
        let d = s.doc.as_ref()?;
        let t = d.session.title();
        Some(
            t.strip_suffix(".pdf")
                .or_else(|| t.strip_suffix(".PDF"))
                .unwrap_or(&t)
                .to_owned(),
        )
    }

    fn make_smaller(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, preset: i32) {
        let Some(d) = &s.doc else { return };
        let doc = d.session.doc().clone();
        let Some(stem) = Self::stem(s) else { return };
        let preset = Preset::ALL[preset.clamp(0, 2) as usize];
        let st = ui.global::<AppState>();
        st.set_sheet(Sheet::None);
        st.set_busy(true);
        pagebara_thread::spawn("optimize", move || {
            let r = pagebara_optimize::optimize(&doc, &preset.options(), None);
            post_act(move |app, s, ui| {
                ui.global::<AppState>().set_busy(false);
                match r {
                    Ok(out) => {
                        let rep = &out.report;
                        if rep.after >= rep.before {
                            return app
                                .message(s, &tr!("This file is already as small as it gets."));
                        }
                        if app
                            .hand_out(s, &format!("{stem} (smaller).pdf"), &out.bytes)
                            .is_some()
                        {
                            let mut m = tr!(
                                "Smaller copy: {} → {} ({}% smaller).",
                                size(rep.before),
                                size(rep.after),
                                decimal(rep.saved_percent(), 0)
                            );
                            if rep.signed {
                                m.push(' ');
                                m.push_str(&tr!("Its signatures are no longer valid."));
                            }
                            app.message(s, &m);
                        }
                    }
                    Err(e) => app.message(s, &e.to_string()),
                }
            });
        });
    }

    fn save_pdfa(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(d) = &s.doc else { return };
        let doc = d.session.doc().clone();
        let Some(stem) = Self::stem(s) else { return };
        let st = ui.global::<AppState>();
        st.set_sheet(Sheet::None);
        st.set_busy(true);
        pagebara_thread::spawn("pdfa", move || {
            let r = pdfa::convert(&doc, Profile::PdfA2b);
            post_act(move |app, s, ui| {
                ui.global::<AppState>().set_busy(false);
                match r {
                    Ok(c) => {
                        if app
                            .hand_out(s, &format!("{stem} (PDF-A).pdf"), &c.bytes)
                            .is_some()
                        {
                            let m = if c.report.passed() {
                                tr!("PDF/A-2b copy ready: it conforms.")
                            } else {
                                trn!(
                                    "PDF/A copy ready, but {n} problem remains (see the desktop app for details).",
                                    "PDF/A copy ready, but {n} problems remain (see the desktop app for details).",
                                    c.report.errors()
                                )
                            };
                            app.message(s, &m);
                        }
                    }
                    Err(e) => app.message(s, &e.to_string()),
                }
            });
        });
    }

    fn check_access(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(d) = &s.doc else { return };
        let r = pagebara_conform::validate(d.session.doc(), Profile::PdfUa1);
        let auto = r
            .issues
            .iter()
            .filter(|i| i.fix == Some(Fix::Automatic))
            .count();
        let st = ui.global::<AppState>();
        st.set_sheet(Sheet::None);
        st.set_access_fixable(auto > 0);
        s.tools_access = Some((r.errors(), auto));
        let m = if r.passed() {
            tr!("No accessibility problems found.")
        } else {
            let first = r
                .issues
                .first()
                .map(|i| i.message.clone())
                .unwrap_or_default();
            trn!(
                "{n} accessibility problem ({} fixable here), e.g. {}.",
                "{n} accessibility problems ({} fixable here), e.g. {}.",
                r.errors(),
                auto,
                first
            )
        };
        self.message(s, &m);
    }

    fn fix_access(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let title = Self::stem(s);
        let Some(d) = &mut s.doc else { return };
        let opts = pdfua::FixOptions {
            lang: Some(
                std::env::var("LANG")
                    .ok()
                    .and_then(|l| l.split(['.', '@']).next().map(|l| l.replace('_', "-")))
                    .filter(|l| l.len() >= 2 && l != "C")
                    .unwrap_or_else(|| "en-US".into()),
            ),
            title,
        };
        let r = d.session.edit(&tr!("Accessibility fixes"), |doc| {
            Ok::<_, String>(pdfua::fix_safe(doc, &opts))
        });
        ui.global::<AppState>().set_sheet(Sheet::None);
        match r {
            Ok((done, _)) => {
                self.after_edit(s, ui);
                self.message(
                    s,
                    &trn!(
                        "{n} accessibility fix applied. Save to keep them.",
                        "{n} accessibility fixes applied. Save to keep them.",
                        done.len()
                    ),
                );
                self.check_access(s, ui);
            }
            Err(e) => self.message(s, &e),
        }
    }
}
