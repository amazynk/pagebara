//! Spell checking while typing: page text in Edit mode, form fields, free text boxes and
//! comments. The language is the document's (`/Lang`), else the app's. Dictionaries load on
//! a worker the first time; one that must be downloaded is offered in a banner. Misspelled
//! words are underlined (page text by the overlay, the fields by `SpellMarks`); a right
//! click or Shift+F10 on one opens its menu: corrections, Add to dictionary, Ignore.

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
use crate::{AppState, AppWindow, SpellState, SpellWord, ViewState};

/// The editors that check spelling, as `SpellState` numbers them.
pub const PAGE_TEXT: usize = 0;
pub const FIELD: usize = 1;
pub const FREE_TEXT: usize = 2;
pub const COMMENT: usize = 3;

/// Spell checking state of the window.
pub struct Spell {
    pub service: Arc<Spelling>,
    /// Checkers by language tag: `None` while loading, or when there is no dictionary.
    checkers: HashMap<String, Option<SpellChecker>>,
    /// Languages whose download the user declined this session.
    declined: HashSet<String>,
    /// The language offered for download, and a download's cancel flag.
    offered: Option<String>,
    cancel: Option<Arc<AtomicBool>>,
    /// Per editor: the text last reported and its misspellings.
    texts: [String; 4],
    found: [Vec<Misspelling>; 4],
    /// The word the menu is for: editor and byte range.
    menu: Option<(usize, Range<usize>)>,
    /// Checks a little after typing stops.
    debounce: Timer,
}

impl std::fmt::Debug for Spell {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Spell")
            .field("menu", &self.menu)
            .finish_non_exhaustive()
    }
}

impl Spell {
    /// Dictionaries from the app (or `--profile`) data folder and the system's.
    pub fn new(data_dir: Option<std::path::PathBuf>) -> Spell {
        let mut config = Config::standard();
        if data_dir.is_some() {
            config.data_dir = data_dir;
        }
        Spell {
            service: Arc::new(Spelling::new(config)),
            checkers: HashMap::new(),
            declined: HashSet::new(),
            offered: None,
            cancel: None,
            texts: Default::default(),
            found: Default::default(),
            menu: None,
            debounce: Timer::default(),
        }
    }
}

/// How dictionaries are downloaded: the app's HTTPS client, when built with it.
fn fetch() -> Option<Arc<dyn Fetch>> {
    #[cfg(feature = "network")]
    return Some(Arc::new(spell::OcrFetch(Arc::new(pagebara_ocr::HttpFetch))));
    #[cfg(not(feature = "network"))]
    None
}

/// The language text in the active document is checked in.
fn language(s: &State) -> String {
    let doc = s
        .tabs
        .get(s.active)
        .and_then(|t| spell::document_language(t.session.doc()));
    spell::language_for(None, doc.as_deref(), pagebara_i18n::language().code)
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let sp = ui.global::<SpellState>();
    let a = app.clone();
    sp.on_text_changed(move |editor, text| {
        let text = text.to_string();
        a.act(move |app, s, ui| app.spell_text_changed(s, ui, editor as usize, text));
    });
    let a = app.clone();
    sp.on_open_menu(move |editor, index| {
        let (Some(ui), Ok(mut s)) = (a.ui.upgrade(), a.state.try_borrow_mut()) else {
            return false;
        };
        let range = s
            .spell
            .found
            .get(editor as usize)
            .and_then(|f| f.get(index as usize))
            .map(|m| m.range.clone());
        range.is_some_and(|r| a.spell_open_menu(&mut s, &ui, editor as usize, r))
    });
    let a = app.clone();
    sp.on_word_at(move |editor, caret| {
        let Ok(s) = a.state.try_borrow() else {
            return -1;
        };
        let caret = caret as usize;
        s.spell
            .found
            .get(editor as usize)
            .and_then(|f| {
                f.iter()
                    .position(|m| m.range.start <= caret && caret <= m.range.end)
            })
            .map_or(-1, |i| i as i32)
    });
    let a = app.clone();
    sp.on_action(move |what, value| {
        let (what, value) = (what.to_string(), value.to_string());
        a.act(move |app, s, ui| app.spell_action(s, ui, &what, &value));
    });
}

/// Shows the setting and the dictionaries' credits.
pub fn show(s: &State, ui: &AppWindow) {
    ui.global::<SpellState>()
        .set_enabled(s.settings.check_spelling);
    ui.global::<AppState>().set_spelling_notices(
        s.spell
            .service
            .notices()
            .join(
                "
",
            )
            .into(),
    );
}

impl App {
    /// The checker for the active document, loading or offering its dictionary the first
    /// time; `None` while that happens, when there is none or spelling is off.
    pub fn spell_checker(self: &Rc<Self>, s: &mut State, ui: &AppWindow) -> Option<SpellChecker> {
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
                if fetch().is_some()
                    && !s.spell.declined.contains(&lang)
                    && s.spell.cancel.is_none()
                {
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

    /// An editor's text changed: checked again once typing pauses.
    fn spell_text_changed(
        self: &Rc<Self>,
        s: &mut State,
        _ui: &AppWindow,
        editor: usize,
        text: String,
    ) {
        if editor >= 4 || s.spell.texts[editor] == text {
            return;
        }
        s.spell.texts[editor] = text;
        let app = self.clone();
        s.spell.debounce.start(
            slint::TimerMode::SingleShot,
            Duration::from_millis(150),
            move || app.act(|app, s, ui| app.spell_recheck(s, ui)),
        );
    }

    /// The form field's text was set from Rust (opened, or a correction).
    pub fn spell_field_text(self: &Rc<Self>, s: &mut State, ui: &AppWindow, text: &str) {
        s.spell.texts[FIELD] = text.to_owned();
        self.spell_recheck(s, ui);
    }

    /// Checks every editor's text again (after typing, a dictionary change or a setting).
    pub fn spell_recheck(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let checker = self.spell_checker(s, ui);
        let vs = ui.global::<ViewState>();
        let open = [false, vs.get_field_editing(), vs.get_editing(), true];
        let sp = ui.global::<SpellState>();
        for editor in [FIELD, FREE_TEXT, COMMENT] {
            let found = match &checker {
                Some(c) if open[editor] => c.check_text(&s.spell.texts[editor]),
                _ => Vec::new(),
            };
            let words: Vec<SpellWord> = found
                .iter()
                .map(|m| SpellWord {
                    start: m.range.start as i32,
                    end: m.range.end as i32,
                    word: m.word.as_str().into(),
                })
                .collect();
            let model = ModelRc::new(VecModel::from(words));
            match editor {
                FIELD => sp.set_field(model),
                FREE_TEXT => sp.set_editor(model),
                _ => sp.set_comment(model),
            }
            s.spell.found[editor] = found;
        }
        if let Some(t) = s.tab()
            && let Some(te) = t.edit.text.as_mut()
        {
            let had = te.spelling().is_some();
            if had || checker.is_some() {
                te.set_spelling(checker);
                super::edit::refresh_text_view(t);
                self.overlay_changed(s, ui);
            }
        }
    }

    /// Opens the menu of the misspelled word at `range` of an editor's text: fills its
    /// corrections. False when there is no such word.
    pub fn spell_open_menu(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        editor: usize,
        range: Range<usize>,
    ) -> bool {
        let lang = language(s);
        let Some(Some(c)) = s.spell.checkers.get(&lang).cloned() else {
            return false;
        };
        let text = match editor {
            PAGE_TEXT => s
                .tab_ref()
                .and_then(|t| t.edit.text.as_ref())
                .map(|te| te.text())
                .unwrap_or_default(),
            e => s.spell.texts[e].clone(),
        };
        let Some(word) = text.get(range.clone()) else {
            return false;
        };
        let suggestions: Vec<SharedString> = c
            .suggest(word)
            .into_iter()
            .map(SharedString::from)
            .collect();
        let sp = ui.global::<SpellState>();
        sp.set_word(word.into());
        sp.set_suggestions(ModelRc::new(VecModel::from(suggestions)));
        s.spell.menu = Some((editor, range));
        true
    }

    /// An item of the spelling menu, the setting or the download offer.
    pub fn spell_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str, value: &str) {
        match what {
            "pick" => self.spell_replace(s, ui, value),
            "add" => {
                if let Err(e) = s.spell.service.add_to_dictionary(value) {
                    self.message(s, &tr!("Could not save your word list: {}", e));
                }
                self.message(s, &tr!("“{}” was added to your dictionary.", value));
                self.spell_recheck(s, ui);
            }
            "ignore" => {
                s.spell.service.ignore(value);
                self.spell_recheck(s, ui);
            }
            "enabled" => {
                s.settings.check_spelling = value == "on";
                self.save_settings(s);
                show(s, ui);
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

    /// Replaces the menu's word with a correction.
    fn spell_replace(self: &Rc<Self>, s: &mut State, ui: &AppWindow, with: &str) {
        let Some((editor, range)) = s.spell.menu.take() else {
            return;
        };
        if editor == PAGE_TEXT {
            return self.text_replace(s, ui, range, with);
        }
        let text = &s.spell.texts[editor];
        if text.get(range.clone()).is_none() {
            return;
        }
        let new = format!("{}{with}{}", &text[..range.start], &text[range.end..]);
        match editor {
            FIELD => {
                ui.global::<ViewState>().set_field_text(new.as_str().into());
                self.field_edited(s, ui, &new);
                self.spell_field_text(s, ui, &new);
            }
            FREE_TEXT => {
                ui.global::<ViewState>()
                    .set_editor_text(new.as_str().into());
                s.spell.texts[editor] = new;
                self.spell_recheck(s, ui);
            }
            _ => {
                ui.global::<AppState>()
                    .set_sel_contents(new.as_str().into());
                self.contents_edited(s, ui, &new);
                s.spell.texts[editor] = new;
                self.spell_recheck(s, ui);
            }
        }
    }

    /// Downloads the offered dictionary, with progress in the banner.
    fn spell_download(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let (Some(lang), Some(fetch)) = (s.spell.offered.clone(), fetch()) else {
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
            let result = service.download(&lang, fetch.as_ref(), &progress, &cancel);
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
                    Err(e) => app.show_message(
                        s,
                        ui,
                        &tr!("Could not download the dictionary"),
                        &e.to_string(),
                    ),
                }
            });
        });
    }

    /// The spelling menu of the page text at the caret (Shift+F10), placed under the word.
    pub fn spell_menu_at_caret(self: &Rc<Self>, s: &mut State, ui: &AppWindow) -> bool {
        let Some((range, (x, y))) = Self::text_misspelled_at_caret(s) else {
            return false;
        };
        if !self.spell_open_menu(s, ui, PAGE_TEXT, range) {
            return false;
        }
        let vs = ui.global::<ViewState>();
        vs.set_ctx_edit(true);
        vs.set_ctx_spell(true);
        vs.set_spell_menu_x(x as f32);
        vs.set_spell_menu_y(y as f32 + 4.0);
        vs.set_spell_menu_serial(vs.get_spell_menu_serial().wrapping_add(1));
        true
    }
}

/// The editor text is typed in: page text, a field, a free text box or else the comment.
fn active_editor(s: &State, ui: &AppWindow) -> usize {
    let vs = ui.global::<ViewState>();
    if s.tab_ref().is_some_and(|t| t.edit.text.is_some()) {
        PAGE_TEXT
    } else if vs.get_field_editing() {
        FIELD
    } else if vs.get_editing() {
        FREE_TEXT
    } else {
        COMMENT
    }
}

/// The underlines drawn for an editor's words, in the window (the page text's are overlay
/// items, not elements).
fn drawn(ui: &AppWindow, editor: usize) -> Vec<i_slint_backend_testing::ElementHandle> {
    use i_slint_backend_testing::ElementHandle;
    let id = match editor {
        FIELD => "PageView::field-marks",
        FREE_TEXT => "PageView::editor-marks",
        COMMENT => "TextEdit2::comment-marks",
        _ => return Vec::new(),
    };
    ElementHandle::find_by_element_id(ui, id)
        .flat_map(|m| m.query_descendants().match_id("Mark::word").find_all())
        .filter(|e| e.is_valid() && e.size().width > 0.0)
        .collect()
}

/// Where misspelled word `n` (1-based) of the active editor is in the window, for scripts.
pub fn script_target(
    s: &mut State,
    ui: &AppWindow,
    n: usize,
) -> Result<Option<(f64, f64)>, String> {
    let editor = active_editor(s, ui);
    if editor == PAGE_TEXT {
        let Some(at) = App::text_misspelled_centre(s, n.saturating_sub(1)) else {
            return Ok(None);
        };
        let origin =
            i_slint_backend_testing::ElementHandle::find_by_element_id(ui, "AppWindow::view")
                .next()
                .map(|e| e.absolute_position())
                .ok_or("the page view is not shown")?;
        return Ok(Some((
            at.0 + f64::from(origin.x),
            at.1 + f64::from(origin.y),
        )));
    }
    Ok(drawn(ui, editor).get(n.saturating_sub(1)).map(|e| {
        let p = e.absolute_position();
        let z = e.size();
        (
            f64::from(p.x + z.width / 2.0),
            f64::from(p.y + z.height / 2.0),
        )
    }))
}

/// Script queries: `spell.words`, `spell.marks` (words found), `spell.drawn` (underlines
/// shown), `spell.suggestions`, `spell.word` (the menu's), `spell.enabled`, `spell.offer`,
/// `spell.dictionary` (the user's word list) and `spell.language`.
pub fn query(s: &mut State, ui: &AppWindow, key: &str) -> Option<Result<String, String>> {
    let sp = ui.global::<SpellState>();
    let editor = active_editor(s, ui);
    let words = || -> Vec<String> {
        if editor == PAGE_TEXT {
            return s
                .tab_ref()
                .and_then(|t| t.edit.text_view.as_ref())
                .map(|v| v.misspelled.iter().map(|m| m.word.clone()).collect())
                .unwrap_or_default();
        }
        s.spell.found[editor]
            .iter()
            .map(|m| m.word.clone())
            .collect()
    };
    Some(Ok(match key {
        "spell.words" => words().join(" "),
        "spell.marks" => words().len().to_string(),
        "spell.drawn" => {
            if editor == PAGE_TEXT {
                words().len().to_string()
            } else {
                drawn(ui, editor).len().to_string()
            }
        }
        "spell.suggestions" => {
            use slint::Model;
            sp.get_suggestions().iter().collect::<Vec<_>>().join(", ")
        }
        "spell.word" => sp.get_word().to_string(),
        "spell.enabled" => sp.get_enabled().to_string(),
        "spell.offer" => sp.get_offer().to_string(),
        "spell.dictionary" => s.spell.service.personal_words().join(" "),
        "spell.language" => language(s),
        _ => return None,
    }))
}
