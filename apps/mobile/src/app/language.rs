//! The UI language: the system's or the one chosen on the home screen, applied at once.

use std::rc::Rc;

use pagebara_i18n::{LANGUAGES, Language};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{App, State};
use crate::{AppState, MobileWindow, Sheet, Theme};

pub fn wire(app: &Rc<App>, ui: &MobileWindow) {
    let a = app.clone();
    ui.global::<AppState>().on_set_language(move |i| {
        a.act(move |app, s, ui| {
            // Row 0 follows the system; then LANGUAGES in order.
            s.settings.language = usize::try_from(i - 1)
                .ok()
                .and_then(|i| LANGUAGES.get(i))
                .map(|l| l.code.to_owned());
            if let Some(path) = &s.settings_path
                && let Err(e) = s.settings.save(path)
            {
                log::warn!("could not save the settings: {e}");
            }
            apply(s, ui);
            ui.global::<AppState>().set_sheet(Sheet::None);
            app.show_home(s, ui);
            // The page label is made again in the new language.
            if let Some(d) = &mut s.doc {
                d.shown_page = None;
            }
            app.refresh(s, ui);
        })
    });
}

/// Shows the UI in the chosen language (or the system's), mirrored for right-to-left ones.
pub fn apply(s: &State, ui: &MobileWindow) {
    let lang = pagebara_i18n::choose(s.settings.language.as_deref());
    select(lang);
    ui.global::<Theme>().set_rtl(lang.rtl);
    let st = ui.global::<AppState>();
    let mut rows: Vec<SharedString> = vec![pagebara_i18n::tr!("Follow the system").into()];
    rows.extend(LANGUAGES.iter().map(|l| SharedString::from(l.name)));
    st.set_languages(ModelRc::new(VecModel::from(rows)));
    let index = s
        .settings
        .language
        .as_deref()
        .and_then(|c| LANGUAGES.iter().position(|l| l.code == c))
        .map_or(0, |i| i as i32 + 1);
    st.set_language_index(index);
    st.set_language_name(lang.name.into());
}

/// Switches the Rust catalogs and the Slint UI's bundled translations.
pub fn select(lang: &'static Language) {
    pagebara_i18n::set_language(lang);
    // Text without a /Lang of its own is hyphenated in the UI language.
    pagebara_app_core::edit::set_default_language(Some(&lang.code.replace('_', "-")));
    let code = if lang.code == "en" { "" } else { lang.code };
    if let Err(e) = slint::select_bundled_translation(code) {
        log::warn!("UI language {}: {e}", lang.code);
    }
}
