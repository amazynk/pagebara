//! The UI language: the system's or the one chosen in Settings, applied without a restart.

use std::rc::Rc;

use pagebara_i18n::{LANGUAGES, Language};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{App, State};
use crate::{AppState, AppWindow, Theme};

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let a = app.clone();
    ui.global::<AppState>().on_set_language(move |i| {
        a.act(move |app, s, ui| {
            // Row 0 follows the system; then LANGUAGES in order.
            s.settings.language = usize::try_from(i - 1)
                .ok()
                .and_then(|i| LANGUAGES.get(i))
                .map(|l| l.code.to_owned());
            apply(s, ui);
            app.save_settings(s);
            // Texts made in Rust (window title, panels, lists) are made again.
            #[cfg(target_os = "macos")]
            super::macos_menu::retitle();
            app.show_active(s, ui);
            super::show_recent(s, ui);
        })
    });
}

/// Shows the UI in the chosen language (or the system's), mirrored for right-to-left ones.
pub fn apply(s: &State, ui: &AppWindow) {
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
