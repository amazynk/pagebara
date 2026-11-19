//! Dialogs over the window: unsaved changes, protection, properties, split, crop,
//! settings, drawing a signature, links, owner password, messages.

use std::rc::Rc;

use pagebara_i18n::tr;
use pagebara_syntax::Permissions;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::{App, State};
use crate::{AppState, AppWindow, DialogKind, InfoRow};

#[derive(Debug, Default)]
pub struct DialogState {
    /// The address waiting for confirmation.
    pub link: Option<String>,
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_dialog_action(move |action, value| {
        let (action, value) = (action.to_string(), value.to_string());
        a.act(move |app, s, ui| app.dialog_action(s, ui, &action, &value))
    });
    let a = app.clone();
    st.on_show_dialog(move |kind| a.act(move |app, s, ui| app.show_dialog(s, ui, kind)));
}

impl App {
    pub fn show_message(self: &Rc<Self>, _s: &mut State, ui: &AppWindow, title: &str, text: &str) {
        let st = ui.global::<AppState>();
        st.set_dialog_title(title.into());
        st.set_dialog_text(text.into());
        st.set_dialog(DialogKind::Message);
    }

    pub fn ask_owner_password(self: &Rc<Self>, _s: &mut State, ui: &AppWindow, why: &str) {
        let st = ui.global::<AppState>();
        st.set_dialog_title(tr!("Owner password needed").into());
        st.set_dialog_text(
            tr!(
                "{} If you know the owner password, enter it to allow all changes.",
                why
            )
            .into(),
        );
        st.set_dialog_error("".into());
        st.set_dialog(DialogKind::OwnerPassword);
    }

    pub fn ask_open_link(self: &Rc<Self>, s: &mut State, ui: &AppWindow, url: &str) {
        s.dialog.link = Some(url.to_owned());
        let st = ui.global::<AppState>();
        st.set_dialog_text(url.into());
        st.set_dialog(DialogKind::OpenLink);
    }

    pub(super) fn show_dialog(self: &Rc<Self>, s: &mut State, ui: &AppWindow, kind: DialogKind) {
        let st = ui.global::<AppState>();
        st.set_dialog_error("".into());
        match kind {
            DialogKind::Properties => {
                let Some(t) = s.tab_ref() else { return };
                let rows: Vec<InfoRow> = t
                    .session
                    .info()
                    .rows
                    .into_iter()
                    .map(|(label, value)| InfoRow {
                        label: label.into(),
                        value: value.into(),
                    })
                    .collect();
                st.set_info(ModelRc::new(VecModel::from(rows)));
            }
            DialogKind::Protect => {
                let Some(t) = s.tab_ref() else { return };
                if !t.session.doc().is_owner() {
                    return self.ask_owner_password(
                        s,
                        ui,
                        &tr!("Changing the protection of this document needs its owner password."),
                    );
                }
                let p = if t.session.encrypted() {
                    t.session.doc().permissions()
                } else {
                    Permissions::ALL
                };
                st.set_permissions(ModelRc::new(VecModel::from(vec![
                    p.print,
                    p.copy,
                    p.modify,
                    p.annotate,
                    p.fill_forms,
                    p.assemble,
                ])));
            }
            DialogKind::Optimize | DialogKind::Pdfa | DialogKind::AccessCheck => {
                if s.tab_ref().is_none() {
                    return;
                }
                st.set_dialog(kind);
                return match kind {
                    DialogKind::Optimize => self.optimize_open(s, ui),
                    DialogKind::Pdfa => self.pdfa_open(s, ui),
                    _ => self.access_check(s, ui),
                };
            }
            DialogKind::Settings => {
                st.set_settings_section(0);
                st.set_author(s.settings.author.as_str().into());
            }
            _ => {}
        }
        st.set_dialog(kind);
    }

    pub(super) fn show_about(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let st = ui.global::<AppState>();
        if st.get_dialog() != DialogKind::Settings {
            self.show_dialog(s, ui, DialogKind::Settings);
        }
        st.set_settings_section(3);
    }

    pub fn dialog_action(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        action: &str,
        value: &str,
    ) {
        let st = ui.global::<AppState>();
        let kind = st.get_dialog();
        if kind == DialogKind::Unsaved {
            return self.unsaved_answer(s, ui, action);
        }
        if st.get_password_visible() && action == "cancel" {
            s.pending = None;
            st.set_password_visible(false);
            return self.show_active(s, ui);
        }
        match action {
            "cancel" => {
                if kind == DialogKind::DrawSignature {
                    s.sign.strokes.clear();
                }
                if kind == DialogKind::Ocr {
                    self.ocr_cancel(s);
                }
                st.set_dialog(DialogKind::None);
            }
            "protect" => self.protect(s, ui, value),
            "unprotect" => {
                st.set_dialog(DialogKind::None);
                if self
                    .edit(s, ui, &tr!("Remove protection"), |d| {
                        Ok(d.remove_security()?)
                    })
                    .is_some()
                {
                    self.message(
                        s,
                        &tr!("Protection removed. Save to write the file without it."),
                    );
                }
            }
            "owner" => {
                let Some(t) = s.tab() else { return };
                if t.session.unlock_owner(value) {
                    st.set_dialog(DialogKind::None);
                    self.doc_changed(s, ui);
                    self.message(
                        s,
                        &tr!("Owner password accepted: all changes are allowed now."),
                    );
                } else {
                    st.set_dialog_error(tr!("That is not the owner password.").into());
                }
            }
            "split" => self.split(s, ui),
            "crop" => self.crop(s, ui, false),
            "uncrop" => self.crop(s, ui, true),
            "settings" => {
                s.settings.author = value.trim().to_owned();
                st.set_author(s.settings.author.as_str().into());
                st.set_dialog(DialogKind::None);
                self.save_settings(s);
            }
            "save-drawing" => self.save_drawing(s, ui, value),
            "rename" => self.rename_signature(s, ui, value),
            "open-link" => {
                st.set_dialog(DialogKind::None);
                if let Some(url) = s.dialog.link.take() {
                    crate::links::open(&url);
                }
            }
            _ => st.set_dialog(DialogKind::None),
        }
    }

    fn protect(self: &Rc<Self>, s: &mut State, ui: &AppWindow, value: &str) {
        let st = ui.global::<AppState>();
        let (user, owner) = value.split_once('\u{1f}').unwrap_or((value, ""));
        let (user, owner) = (user.to_owned(), owner.to_owned());
        let p: Vec<bool> = st.get_permissions().iter().collect();
        let get = |i: usize| p.get(i).copied().unwrap_or(true);
        let perms = Permissions {
            print: get(0),
            print_high_quality: get(0),
            copy: get(1),
            extract_for_accessibility: true,
            modify: get(2),
            annotate: get(3),
            fill_forms: get(4),
            assemble: get(5),
        };
        // Without an owner password, the user password unlocks everything.
        let owner_pw = if owner.is_empty() {
            user.clone()
        } else {
            owner.clone()
        };
        let result = s.tab().map(|t| {
            t.session
                .edit(&tr!("Protect"), |d| d.set_password(&user, &owner_pw, perms))
        });
        match result {
            Some(Ok((_, changed))) => {
                if let Some(t) = s.tab() {
                    t.session.remember_password(&user);
                    t.session.remember_password(&owner_pw);
                }
                st.set_dialog(DialogKind::None);
                self.after_edit(s, ui, changed.pages);
                self.message(s, &tr!("Protection set. Save to apply it to the file."));
            }
            Some(Err(e)) => st.set_dialog_error(super::document::EditError::from(e).text.into()),
            None => {}
        }
    }
}
