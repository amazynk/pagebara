//! Spell checking on phones: page text being edited and form fields. Misspelled words get a
//! red wavy underline; with the caret in one (a tap on it), a bar above the keyboard offers
//! its corrections, Add to dictionary and Ignore. The language is the document's, else the
//! app's; a dictionary to download is offered once per session.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pagebara_app_core::spell::{
    self, Availability, Config, Fetch, Misspelling, SpellChecker, Spelling,
};
use pagebara_i18n::tr;
use slint::{ComponentHandle, ModelRc, SharedString, Timer, VecModel};

use super::{App, State, post_act};
use crate::{AppState, MobileWindow, SpellState, SpellWord};

pub const PAGE_TEXT: usize = 0;
pub const FIELD: usize = 1;

pub struct Spell {
    service: Arc<Spelling>,
    /// Checkers by language: `None` while loading, or when there is no dictionary.
    checkers: HashMap<String, Option<SpellChecker>>,
    declined: HashSet<String>,
    offered: Option<String>,
    cancel: Option<Arc<AtomicBool>>,
    /// The form field's text and its misspellings.
    field: String,
    found: Vec<Misspelling>,
    /// The word the bar is for: editor and byte range.
    shown: Option<(usize, Range<usize>)>,
    debounce: Timer,
}

impl Spell {
    pub fn new(data_dir: Option<std::path::PathBuf>) -> Spell {
        let mut config = Config::standard();
        config.data_dir = data_dir;
        Spell {
            service: Arc::new(Spelling::new(config)),
            checkers: HashMap::new(),
            declined: HashSet::new(),
            offered: None,
            cancel: None,
            field: String::new(),
            found: Vec::new(),
            shown: None,
            debounce: Timer::default(),
        }
    }

    /// The field's misspelled words (for scripts).
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub fn field_words(&self) -> Vec<String> {
        self.found.iter().map(|m| m.word.clone()).collect()
    }

    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub fn personal_words(&self) -> Vec<String> {
        self.service.personal_words()
    }
}

fn language(s: &State) -> String {
    let doc = s
        .doc
        .as_ref()
        .and_then(|d| spell::document_language(d.session.doc()));
    spell::language_for(None, doc.as_deref(), pagebara_i18n::language().code)
}

pub fn wire(app: &Rc<App>, ui: &MobileWindow) {
    let sp = ui.global::<SpellState>();
    let a = app.clone();
    sp.on_text_changed(move |editor, text| {
        if editor as usize != FIELD {
            return;
        }
        let text = text.to_string();
        a.act(move |app, s, ui| app.spell_field_changed(s, ui, text));
    });
    sp.on_open_menu(|_, _| false);
    sp.on_word_at(|_, _| -1);
    let a = app.clone();
    sp.on_action(move |what, value| {
        let (what, value) = (what.to_string(), value.to_string());
        a.act(move |app, s, ui| app.spell_action(s, ui, &what, &value));
    });
}

/// Shows the setting and the credits.
pub fn show(s: &State, ui: &MobileWindow) {
    ui.global::<SpellState>()
        .set_enabled(s.settings.check_spelling);
    let mut notices = vec![pagebara_ocr::model::ATTRIBUTION.to_owned()];
    notices.extend(s.spell.service.notices());
    ui.global::<AppState>()
        .set_licences(notices.join("\n\n").into());
}

fn fetch() -> Arc<dyn Fetch> {
    Arc::new(spell::OcrFetch(Arc::new(pagebara_ocr::HttpFetch)))
}

impl App {
    /// The checker for the open document, loading or offering its dictionary the first time.
    pub fn spell_checker(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
    ) -> Option<SpellChecker> {
        if !s.settings.check_spelling {
            return None;
        }
        let lang = language(s);
        if let Some(c) = s.spell.checkers.get(&lang) {
            return c.clone();
        }
        match s.spell.service.availability(&lang) {
            Availability::Ready { .. } => {
                s.spell.checkers.insert(lang.clone(), None);
                let service = s.spell.service.clone();
                pagebara_thread::spawn("spell-load", move || {
                    let c = service
                        .checker(&lang)
                        .inspect_err(|e| log::warn!("spelling dictionary for {lang}: {e}"))
                        .ok();
                    post_act(move |app, s, ui| {
                        s.spell.checkers.insert(lang, c);
                        app.spell_recheck(s, ui);
                    });
                });
            }
            Availability::Download(info) => {
                if !s.spell.declined.contains(&lang) && s.spell.cancel.is_none() {
                    s.spell.offered = Some(lang);
                    ui.global::<SpellState>().set_offer(
                        tr!(
                            "Check spelling in {}? Its dictionary is a {} download.",
                            info.name,
                            pagebara_i18n::bytes(info.download_size())
                        )
                        .into(),
                    );
                }
            }
            Availability::Unsupported => {
                s.spell.checkers.insert(lang, None);
            }
        }
        None
    }

    fn spell_field_changed(self: &Rc<Self>, s: &mut State, _ui: &MobileWindow, text: String) {
        if s.spell.field == text {
            return;
        }
        s.spell.field = text;
        let app = self.clone();
        s.spell.debounce.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(150),
            move || app.act(|app, s, ui| app.spell_recheck(s, ui)),
        );
    }

    /// Checks the field and the page text being edited again.
    pub fn spell_recheck(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let checker = self.spell_checker(s, ui);
        let editing = ui.global::<AppState>().get_field_editing();
        s.spell.found = match &checker {
            Some(c) if editing => c.check_text(&s.spell.field),
            _ => Vec::new(),
        };
        let words: Vec<SpellWord> = s
            .spell
            .found
            .iter()
            .map(|m| SpellWord {
                start: m.range.start as i32,
                end: m.range.end as i32,
                word: m.word.as_str().into(),
            })
            .collect();
        ui.global::<SpellState>()
            .set_field(ModelRc::new(VecModel::from(words)));
        if let Some(d) = &mut s.doc
            && let Some(e) = &mut d.edit.text
        {
            let had = e.te.spelling().is_some();
            if had || checker.is_some() {
                e.te.set_spelling(checker);
                super::text_edit::relayout_doc(d);
                d.marks_dirty = true;
                self.refresh(s, ui);
            }
        }
        self.spell_page_caret(s, ui);
    }

    /// Shows the bar for the misspelled word of the page text around the caret, if any.
    pub fn spell_page_caret(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let hit = s
            .doc
            .as_ref()
            .and_then(|d| d.edit.text.as_ref())
            .and_then(|e| {
                let caret = e.te.selection().1;
                let m = e.misspelled_around(caret)?;
                Some((m.range.clone(), e.te.text()))
            });
        match hit {
            Some((range, text)) => self.spell_show(s, ui, PAGE_TEXT, range, &text),
            None if s.spell.shown.as_ref().is_some_and(|(e, _)| *e == PAGE_TEXT) => {
                self.spell_hide(s, ui)
            }
            None => {}
        }
    }

    fn spell_show(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        editor: usize,
        range: Range<usize>,
        text: &str,
    ) {
        if s.spell.shown.as_ref() == Some(&(editor, range.clone())) {
            return;
        }
        let lang = language(s);
        let (Some(Some(c)), Some(word)) = (
            s.spell.checkers.get(&lang).cloned(),
            text.get(range.clone()),
        ) else {
            return self.spell_hide(s, ui);
        };
        let suggestions: Vec<SharedString> = c
            .suggest(word)
            .into_iter()
            .map(SharedString::from)
            .collect();
        let sp = ui.global::<SpellState>();
        sp.set_suggestions(ModelRc::new(VecModel::from(suggestions)));
        sp.set_word(word.into());
        s.spell.shown = Some((editor, range));
    }

    fn spell_hide(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        s.spell.shown = None;
        ui.global::<SpellState>().set_word("".into());
    }

    pub fn spell_action(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        what: &str,
        value: &str,
    ) {
        match what {
            "caret" => {
                let caret: usize = value.parse().unwrap_or(0);
                let hit = s
                    .spell
                    .found
                    .iter()
                    .find(|m| m.range.start <= caret && caret <= m.range.end)
                    .map(|m| m.range.clone());
                match hit {
                    Some(r) => {
                        let text = s.spell.field.clone();
                        self.spell_show(s, ui, FIELD, r, &text);
                    }
                    None => self.spell_hide(s, ui),
                }
            }
            "pick" => self.spell_replace(s, ui, value),
            "add" => {
                if let Err(e) = s.spell.service.add_to_dictionary(value) {
                    self.message(s, &tr!("Could not save your word list: {}", e));
                }
                self.spell_hide(s, ui);
                self.spell_recheck(s, ui);
            }
            "ignore" => {
                s.spell.service.ignore(value);
                self.spell_hide(s, ui);
                self.spell_recheck(s, ui);
            }
            "enabled" => {
                s.settings.check_spelling = value == "on";
                if let Some(path) = &s.settings_path
                    && let Err(e) = s.settings.save(path)
                {
                    log::warn!("could not save the settings: {e}");
                }
                show(s, ui);
                self.spell_hide(s, ui);
                self.spell_recheck(s, ui);
            }
            "download" => self.spell_download(s, ui),
            "dismiss" => {
                if let Some(c) = s.spell.cancel.take() {
                    c.store(true, Ordering::Relaxed);
                } else if let Some(lang) = s.spell.offered.take() {
                    s.spell.declined.insert(lang);
                }
                let sp = ui.global::<SpellState>();
                sp.set_offer("".into());
                sp.set_progress(-1.0);
            }
            _ => {}
        }
    }

    fn spell_replace(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, with: &str) {
        let Some((editor, range)) = s.spell.shown.take() else {
            return;
        };
        ui.global::<SpellState>().set_word("".into());
        if editor == PAGE_TEXT {
            return self.edit_text_replace(s, ui, range, with);
        }
        let text = &s.spell.field;
        if text.get(range.clone()).is_none() {
            return;
        }
        let new = format!("{}{with}{}", &text[..range.start], &text[range.end..]);
        ui.global::<AppState>().set_field_text(new.as_str().into());
        s.spell.field = new;
        self.spell_recheck(s, ui);
    }

    /// The field opened with `text`.
    pub fn spell_field_opened(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, text: &str) {
        s.spell.field = text.to_owned();
        self.spell_hide(s, ui);
        self.spell_recheck(s, ui);
    }

    fn spell_download(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(lang) = s.spell.offered.clone() else {
            return;
        };
        let cancel = Arc::new(AtomicBool::new(false));
        s.spell.cancel = Some(cancel.clone());
        ui.global::<SpellState>().set_progress(0.0);
        let service = s.spell.service.clone();
        pagebara_thread::spawn("spell-download", move || {
            let last = Mutex::new(0.0f32);
            let progress = |p: spell::Progress| {
                let f = p.done as f32 / p.total.max(1) as f32;
                let mut l = last.lock().unwrap_or_else(|e| e.into_inner());
                if f - *l >= 0.01 || f >= 1.0 {
                    *l = f;
                    post_act(move |_, _, ui| ui.global::<SpellState>().set_progress(f));
                }
            };
            let result = service.download(&lang, fetch().as_ref(), &progress, &cancel);
            post_act(move |app, s, ui| {
                s.spell.cancel = None;
                s.spell.offered = None;
                let sp = ui.global::<SpellState>();
                sp.set_offer("".into());
                sp.set_progress(-1.0);
                match result {
                    Ok(()) => {
                        s.spell.checkers.remove(&lang);
                        show(s, ui);
                        app.message(s, &tr!("The dictionary was downloaded."));
                        app.spell_recheck(s, ui);
                    }
                    Err(spell::Error::Cancelled) => {}
                    Err(e) => app.message(s, &tr!("Could not download the dictionary: {}", e)),
                }
            });
        });
    }
}

/// The misspelled words of the page text being edited, if it is (for scripts).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn page_words(s: &State) -> Option<Vec<String>> {
    let e = s.doc.as_ref()?.edit.text.as_ref()?;
    Some(e.misspelled().iter().map(|m| m.word.clone()).collect())
}

/// Page and view point of the middle of misspelled word `i` of the page text (scripts).
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn page_word(s: &State, i: usize) -> Option<(usize, f64, f64)> {
    let d = s.doc.as_ref()?;
    let e = d.edit.text.as_ref()?;
    let q = e.misspelled().get(i)?.quads.first()?;
    let r = pagebara_edit::Rect::from_points(q);
    let p = super::text_edit::view_point(
        d,
        pagebara_edit::Point::new((r.x0 + r.x1) / 2.0, (r.y0 + r.y1) / 2.0),
    )?;
    Some((e.page(), f64::from(p.x), f64::from(p.y)))
}
