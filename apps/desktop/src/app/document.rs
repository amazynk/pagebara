//! Editing with undo, saving, closing documents.

use std::path::PathBuf;
use std::rc::Rc;

use pagebara_app_core::session::{SaveDone, SaveError, SaveMode};
use pagebara_i18n::tr;
use pagebara_syntax::Document;
use slint::ComponentHandle;

use super::{AfterUnsaved, App, Gesture, State, show_recent};
use crate::{AppState, AppWindow, DialogKind};

/// Why an edit failed, in words for the user.
#[derive(Debug)]
pub struct EditError {
    pub text: String,
    /// The document's security settings forbid it; the owner password would allow it.
    pub permission: bool,
}

impl From<pagebara_pages::Error> for EditError {
    fn from(e: pagebara_pages::Error) -> Self {
        let permission = matches!(e, pagebara_pages::Error::NotPermitted(_));
        let text = match &e {
            pagebara_pages::Error::NotPermitted(pagebara_pages::Access::Assemble) => {
                tr!("This document's security settings do not allow changing its pages.")
            }
            pagebara_pages::Error::NotPermitted(pagebara_pages::Access::Extract) => {
                tr!("This document's security settings do not allow copying its pages.")
            }
            pagebara_pages::Error::WouldBeEmpty => tr!("A document needs at least one page."),
            e => tr!("That did not work: {}.", e),
        };
        EditError { text, permission }
    }
}

impl From<pagebara_annot::Error> for EditError {
    fn from(e: pagebara_annot::Error) -> Self {
        EditError {
            text: match e {
                pagebara_annot::Error::ReadOnly => {
                    tr!("This kind of annotation cannot be changed.")
                }
                e => tr!("That did not work: {}.", e),
            },
            permission: false,
        }
    }
}

impl From<pagebara_signatures::Error> for EditError {
    fn from(e: pagebara_signatures::Error) -> Self {
        EditError {
            text: tr!("The signature could not be placed: {}.", e),
            permission: false,
        }
    }
}

impl From<pagebara_syntax::Error> for EditError {
    fn from(e: pagebara_syntax::Error) -> Self {
        let permission = matches!(e, pagebara_syntax::Error::OwnerPasswordRequired);
        EditError {
            text: match e {
                pagebara_syntax::Error::OwnerPasswordRequired => {
                    tr!("Changing the protection needs the owner password.")
                }
                pagebara_syntax::Error::InvalidPassword => {
                    tr!("That password contains characters that are not allowed.")
                }
                e => tr!("That did not work: {}.", e),
            },
            permission,
        }
    }
}

impl From<String> for EditError {
    fn from(text: String) -> Self {
        EditError {
            text,
            permission: false,
        }
    }
}

impl App {
    /// Runs `f` on the active document as one undoable step named `label`.
    pub fn edit<T>(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        label: &str,
        f: impl FnOnce(&mut Document) -> Result<T, EditError>,
    ) -> Option<T> {
        let t = s.tab()?;
        match t.session.edit(label, f) {
            Ok((v, changed)) => {
                self.after_edit(s, ui, changed.pages);
                Some(v)
            }
            Err(e) => {
                self.edit_failed(s, ui, e);
                None
            }
        }
    }

    pub fn edit_failed(self: &Rc<Self>, s: &mut State, ui: &AppWindow, e: EditError) {
        if e.permission {
            self.ask_owner_password(s, ui, &e.text);
        } else {
            self.show_message(s, ui, &tr!("Could not change the document"), &e.text);
        }
    }

    /// Whether annotations may be added or changed; explains when not.
    pub fn may_annotate(self: &Rc<Self>, s: &mut State, ui: &AppWindow) -> bool {
        let Some(t) = s.tab_ref() else { return false };
        let p = t.session.doc().permissions();
        if p.annotate || p.modify {
            return true;
        }
        self.ask_owner_password(
            s,
            ui,
            &tr!("This document's security settings do not allow adding or changing comments."),
        );
        false
    }

    pub fn undo(self: &Rc<Self>, s: &mut State, ui: &AppWindow, redo: bool) {
        s.gesture = Gesture::None;
        self.edit_text_done(s, ui, false);
        ui.global::<crate::ViewState>().set_editing(false);
        self.field_cancel(s, ui);
        let Some(t) = s.tab() else { return };
        let label = if redo {
            t.session.redo_label()
        } else {
            t.session.undo_label()
        }
        .map(str::to_owned);
        let changed = if redo {
            t.session.redo()
        } else {
            t.session.undo()
        };
        let Some(changed) = changed else { return };
        self.after_edit(s, ui, changed.pages);
        if let Some(l) = label {
            let text = if redo {
                tr!("Redid: {}", l)
            } else {
                tr!("Undid: {}", l)
            };
            self.message(s, &text);
        }
    }

    /// Commit the on-page editors while their document is still active.
    pub(super) fn finish_editors(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let st = ui.global::<AppState>();
        if st.get_has_selection() {
            self.contents_edited(s, ui, &st.get_sel_contents());
        }
        self.editor_done(s, ui, true);
        self.field_done(s, ui, true);
        self.edit_text_done(s, ui, true);
    }

    /// Saves the active document (to its own file when it has one).
    pub fn save(self: &Rc<Self>, s: &mut State, ui: &AppWindow, after: Option<AfterUnsaved>) {
        self.finish_editors(s, ui);
        let Some(t) = s.tab_ref() else { return };
        let Some(path) = t.session.path.clone().filter(|p| p.exists()) else {
            return self.save_as_then(s, ui, false, after);
        };
        if !t.session.is_dirty() && !s.settings.flatten_signatures && !t.form_ui.flatten_on_save {
            self.message(s, &tr!("No changes to save."));
            if let Some(a) = after {
                self.after_saved(s, ui, a);
            }
            return;
        }
        self.start_save(s, ui, path, SaveMode::Incremental, after);
    }

    pub fn save_as(self: &Rc<Self>, s: &mut State, ui: &AppWindow, compact: bool) {
        self.save_as_then(s, ui, compact, None);
    }

    fn save_as_then(
        self: &Rc<Self>,
        s: &mut State,
        _ui: &AppWindow,
        compact: bool,
        after: Option<AfterUnsaved>,
    ) {
        let Some(t) = s.tab_ref() else { return };
        let uid = t.uid;
        let name = t.session.title();
        let name = if name.to_lowercase().ends_with(".pdf") {
            name
        } else {
            format!("{name}.pdf")
        };
        let title = if compact {
            tr!("Save a smaller copy")
        } else {
            tr!("Save as")
        };
        super::pick::save_pdf(
            self,
            &title,
            t.session.path.clone(),
            &name,
            move |app, s, ui, path| {
                if let Some(i) = s.tab_index(uid) {
                    app.select_tab(s, ui, i);
                    let mode = if compact {
                        SaveMode::Compact
                    } else {
                        SaveMode::Full
                    };
                    app.start_save(s, ui, path, mode, after);
                }
            },
        );
    }

    /// Saves the active tab to `path` on a worker thread.
    pub fn start_save(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        path: PathBuf,
        mode: SaveMode,
        after: Option<AfterUnsaved>,
    ) {
        self.finish_editors(s, ui);
        let flatten = s.settings.flatten_signatures;
        let Some(t) = s.tab_ref() else { return };
        let uid = t.uid;
        let mut job = t.session.save_job(path, mode);
        job.flatten_signatures = flatten;
        job.flatten_forms = t.form_ui.flatten_on_save;
        ui.global::<AppState>().set_busy(true);
        self.message(s, &tr!("Saving…"));
        pagebara_thread::spawn("io-save", move || {
            let result = job.run();
            super::post_act(move |app, s, ui| app.saved(s, ui, uid, result, after));
        });
    }

    fn saved(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        uid: u64,
        result: Result<SaveDone, SaveError>,
        after: Option<AfterUnsaved>,
    ) {
        ui.global::<AppState>().set_busy(false);
        let Some(i) = s.tab_index(uid) else { return };
        match result {
            Ok(done) => {
                let path = done.path.clone();
                let size = pagebara_i18n::bytes(done.bytes as u64);
                let msg = if done.full {
                    tr!("Saved · {}", size)
                } else {
                    tr!("Saved (changes appended) · {}", size)
                };
                s.tabs[i].session.saved(done);
                s.tabs[i].forget_annots();
                s.recent.add(&path);
                show_recent(s, ui);
                if i == s.active {
                    if let Some(t) = s.tab() {
                        t.overlay_dirty = true;
                    }
                    self.doc_changed(s, ui);
                    self.refresh(s, ui);
                    self.message(s, &msg);
                } else {
                    Self::show_tabs(s, ui);
                }
                if let Some(a) = after {
                    self.after_saved(s, ui, a);
                }
            }
            Err(e) => {
                log::warn!("save failed: {e}");
                s.quitting = false;
                self.show_message(s, ui, &tr!("Could not save"), &e.to_string());
            }
        }
    }

    fn after_saved(self: &Rc<Self>, s: &mut State, ui: &AppWindow, after: AfterUnsaved) {
        match after {
            AfterUnsaved::CloseTab(uid) => {
                if let Some(i) = s.tab_index(uid) {
                    self.close_tab(s, ui, i, false);
                }
            }
            AfterUnsaved::Quit => self.continue_quit(s, ui),
        }
    }

    /// Asks about the next unsaved document, or quits.
    pub fn continue_quit(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        self.finish_editors(s, ui);
        if let Some(i) = s.tabs.iter().position(|t| t.session.is_dirty()) {
            self.select_tab(s, ui, i);
            self.ask_unsaved(s, ui, AfterUnsaved::Quit);
        } else {
            self.quit(s, ui);
        }
    }

    pub fn close_tab(self: &Rc<Self>, s: &mut State, ui: &AppWindow, i: usize, force: bool) {
        if i == s.active && !force {
            self.finish_editors(s, ui);
        }
        let Some(t) = s.tabs.get(i) else { return };
        if t.session.is_dirty() && !force {
            let uid = t.uid;
            self.select_tab(s, ui, i);
            self.ask_unsaved(s, ui, AfterUnsaved::CloseTab(uid));
            return;
        }
        let active = i == s.active;
        if active {
            if force {
                self.editor_done(s, ui, false);
                self.field_cancel(s, ui);
                self.edit_text_done(s, ui, false);
            }
            self.clear_message(s, ui);
            self.vim_reset(s, ui);
            s.gesture = Gesture::None;
        }
        let mut t = s.tabs.remove(i);
        super::remember_zoom(&mut s.settings, &t);
        super::reading::closed(s, &mut t);
        if i < s.active || s.active == s.tabs.len() {
            s.active = s.active.saturating_sub(1);
        }
        if active {
            self.restore_tab(s, ui);
        } else {
            Self::show_tabs(s, ui);
        }
    }

    pub fn ask_unsaved(self: &Rc<Self>, s: &mut State, ui: &AppWindow, after: AfterUnsaved) {
        let Some(t) = s.tab_ref() else { return };
        let st = ui.global::<AppState>();
        st.set_dialog_title(tr!("Save changes to “{}”?", t.session.title()).into());
        st.set_dialog(DialogKind::Unsaved);
        s.after_unsaved = Some(after);
    }

    /// The answer to the unsaved-changes question.
    pub fn unsaved_answer(self: &Rc<Self>, s: &mut State, ui: &AppWindow, action: &str) {
        let Some(after) = s.after_unsaved.take() else {
            return;
        };
        ui.global::<AppState>().set_dialog(DialogKind::None);
        match action {
            "save" => self.save(s, ui, Some(after)),
            "discard" => match after {
                AfterUnsaved::CloseTab(uid) => {
                    if let Some(i) = s.tab_index(uid) {
                        self.close_tab(s, ui, i, true);
                    }
                }
                AfterUnsaved::Quit => {
                    let i = s.active;
                    self.close_tab(s, ui, i, true);
                    self.continue_quit(s, ui);
                }
            },
            _ => s.quitting = false,
        }
    }
}
