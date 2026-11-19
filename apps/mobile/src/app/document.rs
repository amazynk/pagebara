//! Undo, saving (back to where the document came from), saving a copy, sharing, and the
//! password of protected documents.

use std::path::PathBuf;
use std::rc::Rc;

use pagebara_app_core::session::{SaveDone, SaveError, SaveMode};
use pagebara_i18n::{tr, trn};
use slint::ComponentHandle;

use super::{App, State, post_act};
use crate::platform::WriteBack;
use crate::{AppState, MobileWindow};

/// What a save is for.
#[derive(Clone, Debug, PartialEq)]
enum Purpose {
    /// The document itself.
    Save,
    /// A copy handed to the platform's "save as".
    Copy(String),
    Share(String),
}

pub fn wire(app: &Rc<App>, ui: &MobileWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_undo(move || a.act(|app, s, ui| app.undo(s, ui, false)));
    let a = app.clone();
    st.on_redo(move || a.act(|app, s, ui| app.undo(s, ui, true)));
    let a = app.clone();
    st.on_set_page_theme(move |i| {
        let theme = pagebara_app_core::page_theme::PageTheme::from_index(i.max(0) as usize);
        a.act(move |app, s, ui| app.set_page_theme(s, ui, theme))
    });
    let a = app.clone();
    st.on_save(move || a.act(|app, s, ui| app.save(s, ui)));
    let a = app.clone();
    st.on_save_copy(move || a.act(|app, s, ui| app.save_copy(s, ui, false)));
    let a = app.clone();
    st.on_share(move || a.act(|app, s, ui| app.save_copy(s, ui, true)));
    let a = app.clone();
    st.on_output_tool(move |name| {
        let name = name.to_string();
        a.act(move |app, s, ui| app.run_tool(s, ui, &name))
    });
    let a = app.clone();
    st.on_unlock(move |pw| {
        let pw = pw.to_string();
        a.act(move |app, s, ui| app.unlock(s, ui, &pw))
    });
    let a = app.clone();
    st.on_unlock_cancel(move || {
        a.act(|_, s, ui| {
            s.pending = None;
            ui.global::<AppState>().set_password_needed(false);
        })
    });
}

impl App {
    /// The output tools offered on phones, each one undoable edit with sensible settings:
    /// page numbers in the footer, a "DRAFT" watermark, flattening comments and forms.
    pub fn run_tool(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, name: &str) {
        let Some(d) = &mut s.doc else { return };
        let title = d.session.title();
        let result = match name {
            "page-numbers" => d
                .session
                .edit_text(&tr!("Add page numbers"), |doc| {
                    let info = pagebara_stamp::DocInfo::read(doc, &title);
                    pagebara_stamp::apply_header_footer(
                        doc,
                        &pagebara_stamp::HeaderFooter::default(),
                        &info,
                    )
                })
                .map(|(r, _)| {
                    let n = r.pages.len();
                    trn!(
                        "Page numbers added to {n} page.",
                        "Page numbers added to {n} pages.",
                        n
                    )
                })
                .map_err(|e| e.to_string()),
            "watermark" => {
                let wm = pagebara_stamp::Watermark {
                    source: pagebara_stamp::Source::Text {
                        // TRANSLATORS: the watermark stamped across the pages
                        text: tr!("DRAFT"),
                        style: pagebara_stamp::TextStyle {
                            font: "Helvetica-Bold".into(),
                            size: 72.0,
                            color: pagebara_stamp::Rgb::RED,
                            background: None,
                        },
                    },
                    replace: true,
                    ..pagebara_stamp::Watermark::default()
                };
                d.session
                    .edit_text(&tr!("Mark as draft"), |doc| {
                        pagebara_stamp::apply_watermark(doc, &wm)
                    })
                    .map(|(r, _)| {
                        let n = r.pages.len();
                        trn!("Marked {n} page as draft.", "Marked {n} pages as draft.", n)
                    })
                    .map_err(|e| e.to_string())
            }
            "flatten" => d
                .session
                // TRANSLATORS: undo label: comments and form fields made part of the pages
                .edit_text(&tr!("Flatten"), |doc| {
                    pagebara_flatten::flatten(doc, &pagebara_flatten::Options::all())
                })
                .map(|(r, _)| {
                    trn!(
                        "Flattened {n} comment and {} form fields.",
                        "Flattened {n} comments and {} form fields.",
                        r.annotations,
                        r.fields
                    )
                })
                .map_err(|e| e.to_string()),
            _ => return,
        };
        let msg = match result {
            Ok(m) => m,
            Err(e) => tr!("That did not work: {}.", e),
        };
        self.after_edit(s, ui);
        self.message(s, &msg);
    }

    fn undo(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, redo: bool) {
        // While editing text, undo takes back the typing.
        if s.doc.as_ref().is_some_and(|d| d.edit.editing_text()) {
            return self.edit_text_done(s, ui, false);
        }
        let Some(d) = &mut s.doc else { return };
        let label = if redo {
            d.session.redo_label()
        } else {
            d.session.undo_label()
        }
        .map(str::to_owned);
        let changed = if redo {
            d.session.redo()
        } else {
            d.session.undo()
        };
        if changed.is_none() {
            return;
        }
        d.pending_ink.clear();
        self.after_edit(s, ui);
        if let Some(l) = label {
            let text = if redo {
                tr!("Redid: {}", l)
            } else {
                tr!("Undid: {}", l)
            };
            self.message(s, &text);
        }
    }

    fn save(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(d) = &s.doc else { return };
        let Some(path) = d.session.path.clone().filter(|p| p.exists()) else {
            return self.save_copy(s, ui, false);
        };
        if !d.session.is_dirty() {
            return self.message(s, &tr!("No changes to save."));
        }
        self.start_save(s, ui, path, SaveMode::Incremental, Purpose::Save);
    }

    /// Saves a copy with the changes for the platform to share or put somewhere.
    fn save_copy(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, share: bool) {
        let Some(d) = &s.doc else { return };
        let mut name = d.session.title();
        if !name.to_lowercase().ends_with(".pdf") {
            name.push_str(".pdf");
        }
        let dir = self.platform.cache_dir().join("share");
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return self.message(s, &tr!("Could not prepare the copy: {}", e));
        }
        let purpose = if share {
            Purpose::Share(name.clone())
        } else {
            Purpose::Copy(name.clone())
        };
        self.start_save(s, ui, dir.join(&name), SaveMode::Full, purpose);
    }

    fn start_save(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        path: PathBuf,
        mode: SaveMode,
        purpose: Purpose,
    ) {
        self.field_done(s, ui, true);
        self.edit_text_done(s, ui, true);
        let Some(d) = &s.doc else { return };
        let job = d.session.save_job(path, mode);
        ui.global::<AppState>().set_busy(true);
        pagebara_thread::spawn("io-save", move || {
            let result = job.run();
            post_act(move |app, s, ui| app.saved(s, ui, result, purpose));
        });
    }

    fn saved(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        result: Result<SaveDone, SaveError>,
        purpose: Purpose,
    ) {
        ui.global::<AppState>().set_busy(false);
        let done = match result {
            Ok(done) => done,
            Err(e) => {
                log::warn!("save failed: {e}");
                return self.message(s, &e.to_string());
            }
        };
        match purpose {
            Purpose::Save => {
                let path = done.path.clone();
                if let Some(d) = &mut s.doc {
                    d.session.saved(done);
                }
                let text = match self.platform.write_back(&path) {
                    WriteBack::Local | WriteBack::Done => tr!("Saved."),
                    WriteBack::Failed => tr!(
                        "Saved in the app, but the original could not be changed. Use Save a copy."
                    ),
                };
                self.message(s, &text);
                self.after_edit(s, ui);
            }
            Purpose::Copy(name) => self.platform.save_copy(&done.path, &name),
            Purpose::Share(name) => self.platform.share(&done.path, &name),
        }
    }

    pub fn ask_password(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, retry: bool) {
        let Some(p) = &s.pending else { return };
        let st = ui.global::<AppState>();
        st.set_password_title(p.title().into());
        st.set_password_error(
            if retry {
                tr!("That password is not right.")
            } else {
                String::new()
            }
            .into(),
        );
        st.set_password_needed(true);
    }

    fn unlock(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, password: &str) {
        let Some(p) = s.pending.take() else { return };
        match p.unlock(password) {
            Ok(session) => {
                ui.global::<AppState>().set_password_needed(false);
                if let Some(path) = &session.path {
                    s.recent.add(path);
                }
                self.show(s, ui, session);
            }
            Err(p) => {
                s.pending = Some(p);
                self.ask_password(s, ui, true);
            }
        }
    }
}
