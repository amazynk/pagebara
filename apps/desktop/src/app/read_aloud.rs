//! Reading aloud: the selection, or the document from the current page on, a sentence at
//! a time in reading order (the structure tree of tagged PDFs, else the layout), through
//! the system's speech engine. The sentence being read is highlighted on the page.
//!
//! The voice is the one chosen in the bar's voice menu, else the most natural one
//! installed for the text's language (see `speech::pick`), else the engine's own.

use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pagebara_app_core::speech::{self, Logged, Recorder, Speaker, Voice};
use pagebara_app_core::utterances::{Utterance, page_utterances, selection};
use pagebara_i18n::tr;
use slint::{ComponentHandle, ModelRc, VecModel};

use super::{App, State, post_act};
use crate::{AppState, AppWindow, ReadAloudState, VoiceRow};

/// The engine's voices, listed once (and again when the voice menu opens).
type Voices = Arc<Mutex<Option<Arc<Vec<Voice>>>>>;

pub struct ReadAloud {
    speaker: Option<Arc<dyn Speaker>>,
    /// What the test engine was asked to say.
    pub spoken: Option<Arc<Mutex<Vec<String>>>>,
    queue: Vec<Utterance>,
    at: usize,
    /// The page whose sentences were queued last, to go on with the next one.
    last_page: Option<usize>,
    /// Bumped by every start, skip and stop: a sentence finishing late is then ignored.
    run: u64,
    playing: bool,
    paused: bool,
    tab: u64,
    rate: f32,
    voices: Voices,
    /// The voice ids of the voice menu's rows; the first, `None`, picks one.
    menu: Vec<Option<String>>,
    /// The voice the last sentence was read with; `None` for the engine's own.
    pub voice_used: Option<String>,
    /// The language to pick a voice for when the text does not say: the system's.
    default_lang: Option<String>,
}

impl std::fmt::Debug for ReadAloud {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReadAloud")
            .field("queued", &self.queue.len())
            .field("at", &self.at)
            .field("playing", &self.playing)
            .finish()
    }
}

impl ReadAloud {
    /// The engine: `PAGEBARA_SPEECH=stub` records instead of speaking (tests), `none` has none.
    /// `log` keeps what the system's engine is asked to say, for scripted runs.
    pub fn new(log: bool) -> Self {
        let (speaker, spoken): (Option<Arc<dyn Speaker>>, _) =
            match std::env::var("PAGEBARA_SPEECH").as_deref() {
                Ok("stub") => {
                    let r = Recorder::new(Duration::from_millis(120));
                    let spoken = r.spoken.clone();
                    (Some(Arc::new(r)), Some(spoken))
                }
                Ok("none") => (None, None),
                _ => match speech::detect() {
                    Some(s) if log => {
                        let l = Logged::new(s);
                        let spoken = l.spoken.clone();
                        (Some(Arc::new(l)), Some(spoken))
                    }
                    s => (s, None),
                },
            };
        ReadAloud {
            speaker,
            spoken,
            queue: Vec::new(),
            at: 0,
            last_page: None,
            run: 0,
            playing: false,
            paused: false,
            tab: 0,
            rate: 1.0,
            voices: Voices::default(),
            menu: vec![None],
            voice_used: None,
            default_lang: pagebara_i18n::system_locale().and_then(|l| speech::locale_tag(&l)),
        }
    }

    /// The sentence being read on `page` of tab `uid`: boxes in page space.
    pub fn highlight(&self, uid: u64, page: usize) -> &[[f32; 4]] {
        match self.queue.get(self.at) {
            Some(u) if (self.playing || self.paused) && self.tab == uid && u.page == page => {
                &u.boxes
            }
            _ => &[],
        }
    }
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let rs = ui.global::<ReadAloudState>();
    {
        let s = app.state.borrow();
        rs.set_available(s.read_aloud.speaker.is_some());
        rs.set_engine(
            s.read_aloud
                .speaker
                .as_ref()
                .map_or(String::new(), |e| e.name().to_owned())
                .into(),
        );
        rs.set_rate(s.settings.speech_rate);
    }
    show_voices(&mut app.state.borrow_mut(), ui);
    macro_rules! on {
        ($cb:ident, |$app:ident, $s:ident, $ui:ident $(, $arg:ident)*| $body:expr) => {{
            let a = app.clone();
            rs.$cb(move |$($arg),*| a.act(move |$app, $s, $ui| $body));
        }};
    }
    on!(on_show, |app, s, ui, visible| {
        ui.global::<ReadAloudState>().set_visible(visible);
        if visible {
            // Ready for the first sentence, which would otherwise wait for the list.
            prefetch_voices(s);
            s.focus_region = 7;
            ui.global::<crate::FocusNav>().invoke_request(7);
        } else {
            stop(app, s, ui);
            ui.invoke_focus_view();
        }
    });
    on!(on_play, |app, s, ui| play(app, s, ui));
    on!(on_pause, |app, s, ui| pause(app, s, ui));
    on!(on_stop, |app, s, ui| stop(app, s, ui));
    on!(on_skip, |app, s, ui, forward| skip(app, s, ui, forward));
    on!(on_set_rate, |app, s, ui, rate| {
        let rate = rate.clamp(0.5, 2.5);
        s.settings.speech_rate = rate;
        s.read_aloud.rate = rate;
        ui.global::<ReadAloudState>().set_rate(rate);
        app.save_settings(s);
    });
    on!(on_load_voices, |app, s, ui| load_voices(app, s, ui));
    on!(on_set_voice, |app, s, ui, i| set_voice(app, s, ui, i));
}

/// The engine's voices: those listed already, else listed now (which can take a moment,
/// and then the voice menu is shown again).
fn voice_list(engine: &dyn Speaker, voices: &Voices) -> Arc<Vec<Voice>> {
    let mut guard = voices.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(list) = &*guard {
        return list.clone();
    }
    let list = Arc::new(engine.voices());
    *guard = Some(list.clone());
    post_act(|_app, s, ui| show_voices(s, ui));
    list
}

fn prefetch_voices(s: &State) {
    let r = &s.read_aloud;
    let Some(engine) = r.speaker.clone() else {
        return;
    };
    if r.voices.lock().unwrap_or_else(|e| e.into_inner()).is_some() {
        return;
    }
    let voices = r.voices.clone();
    pagebara_thread::spawn("speech-voices", move || {
        voice_list(&*engine, &voices);
    });
}

/// Lists the voices afresh for the voice menu.
fn load_voices(_app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let Some(engine) = s.read_aloud.speaker.clone() else {
        return;
    };
    let rs = ui.global::<ReadAloudState>();
    rs.set_voices_loading(true);
    let voices = s.read_aloud.voices.clone();
    pagebara_thread::spawn("speech-voices", move || {
        let list = Arc::new(engine.voices());
        *voices.lock().unwrap_or_else(|e| e.into_inner()) = Some(list);
        post_act(|_app, s, ui| {
            ui.global::<ReadAloudState>().set_voices_loading(false);
            show_voices(s, ui);
        });
    });
}

/// The voice menu and the name on its button.
fn show_voices(s: &mut State, ui: &AppWindow) {
    let r = &s.read_aloud;
    let rs = ui.global::<ReadAloudState>();
    let Some(engine) = &r.speaker else {
        return;
    };
    let list = r
        .voices
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
        .unwrap_or_default();
    let mut voices: Vec<Voice> = (*list).clone();
    speech::sort_voices(&mut voices, r.default_lang.as_deref());
    let chosen = s.settings.speech_voice.as_deref();
    let auto = speech::pick(&voices, None, r.default_lang.as_deref())
        .and_then(|id| voices.iter().find(|v| v.id == id));
    let mut rows = vec![VoiceRow {
        name: tr!("Automatic").into(),
        detail: auto
            .map_or_else(|| engine.name().to_owned(), |v| v.name.clone())
            .into(),
    }];
    let mut ids = vec![None];
    for v in &voices {
        rows.push(VoiceRow {
            name: v.name.as_str().into(),
            detail: v.lang.clone().unwrap_or_default().into(),
        });
        ids.push(Some(v.id.clone()));
    }
    // A voice chosen before that is not listed now (removed, or the list is not in yet).
    if let Some(c) = chosen
        && !ids.iter().any(|i| i.as_deref() == Some(c))
    {
        rows.push(VoiceRow {
            name: c.into(),
            detail: Default::default(),
        });
        ids.push(Some(c.to_owned()));
    }
    let index = ids.iter().position(|i| i.as_deref() == chosen).unwrap_or(0);
    let name = match chosen {
        Some(c) => voices
            .iter()
            .find(|v| v.id == c)
            .map_or_else(|| c.to_owned(), |v| v.name.clone()),
        None => auto.map_or_else(|| tr!("Automatic"), |v| v.name.clone()),
    };
    rs.set_voice_name(name.into());
    rs.set_voice_index(index as i32);
    // Only robotic voices for the reader's language: say where better ones come from.
    let natural = voices.iter().any(|v| {
        v.quality >= speech::GOOD
            && match (v.lang.as_deref(), r.default_lang.as_deref()) {
                (Some(l), Some(d)) => speech::same_language(l, d),
                _ => true,
            }
    });
    let hint = if natural || list.is_empty() {
        String::new()
    } else {
        engine.more_voices().unwrap_or_default()
    };
    rs.set_voices_hint(hint.into());
    rs.set_voices(ModelRc::new(VecModel::from(rows)));
    s.read_aloud.menu = ids;
}

/// The reader chose row `i` of the voice menu: kept in the settings, and the sentence
/// being read is read again in that voice.
fn set_voice(app: &Rc<App>, s: &mut State, ui: &AppWindow, i: i32) {
    let Some(id) = s.read_aloud.menu.get(i.max(0) as usize).cloned() else {
        return;
    };
    s.settings.speech_voice = id;
    app.save_settings(s);
    show_voices(s, ui);
    let r = &mut s.read_aloud;
    if r.playing && !r.paused {
        r.run += 1;
        if let Some(e) = &r.speaker {
            e.stop();
        }
        speak_current(app, s, ui);
    }
}

/// Ctrl+Shift+V reads, Ctrl+Shift+C pauses or resumes, Ctrl+Shift+E stops.
pub fn shortcut(
    app: &Rc<App>,
    s: &mut State,
    ui: &AppWindow,
    c: char,
    ctrl: bool,
    shift: bool,
) -> Option<bool> {
    if !(ctrl && shift) || s.tabs.is_empty() || s.read_aloud.speaker.is_none() {
        return None;
    }
    match c.to_ascii_lowercase() {
        'v' => {
            ui.global::<ReadAloudState>().set_visible(true);
            stop(app, s, ui);
            play(app, s, ui);
        }
        'c' if s.read_aloud.paused => play(app, s, ui),
        'c' if s.read_aloud.playing => pause(app, s, ui),
        'e' => stop(app, s, ui),
        _ => return None,
    }
    Some(true)
}

/// Starts reading (from the selection, else the current page), or resumes.
pub fn play(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    s.read_aloud.rate = s.settings.speech_rate;
    if s.read_aloud.paused && !s.read_aloud.queue.is_empty() {
        s.read_aloud.paused = false;
        return speak_current(app, s, ui);
    }
    let Some(t) = s.tab() else { return };
    let uid = t.uid;
    let (queue, last_page) = match t.text_sel.filter(|sel| sel.anchor != sel.caret) {
        Some(sel) => {
            let text = t.session.text.page(sel.page);
            (selection(&text, sel.page, sel.range()), None)
        }
        None => {
            let page = t.view.current_page;
            (page_utterances(&t.session.reading(page), page), Some(page))
        }
    };
    let r = &mut s.read_aloud;
    r.run += 1;
    r.queue = queue;
    r.at = 0;
    r.last_page = last_page;
    r.tab = uid;
    r.playing = true;
    r.paused = false;
    speak_current(app, s, ui);
}

pub fn pause(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let r = &mut s.read_aloud;
    if !r.playing {
        return;
    }
    r.paused = true;
    r.run += 1;
    if let Some(e) = &r.speaker {
        e.stop();
    }
    show(app, s, ui);
}

pub fn stop(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let r = &mut s.read_aloud;
    let was = r.playing || r.paused;
    r.run += 1;
    r.playing = false;
    r.paused = false;
    if let Some(e) = &r.speaker {
        e.stop();
    }
    if was {
        show(app, s, ui);
    }
}

fn skip(app: &Rc<App>, s: &mut State, ui: &AppWindow, forward: bool) {
    let r = &mut s.read_aloud;
    if !r.playing && !r.paused {
        return;
    }
    if forward {
        r.at += 1;
    } else {
        r.at = r.at.saturating_sub(1);
    }
    r.run += 1;
    if let Some(e) = &r.speaker {
        e.stop();
    }
    if r.paused {
        fill(s);
        return show(app, s, ui);
    }
    speak_current(app, s, ui);
}

/// Queues the next page's sentences once the queue has run out.
fn fill(s: &mut State) {
    let Some(t) = s.tabs.iter().find(|t| t.uid == s.read_aloud.tab) else {
        return;
    };
    let r = &mut s.read_aloud;
    while r.at >= r.queue.len() {
        let Some(last) = r.last_page else { return };
        let next = last + 1;
        if next >= t.session.page_count() {
            return;
        }
        r.last_page = Some(next);
        r.queue
            .extend(page_utterances(&t.session.reading(next), next));
    }
}

/// Says the sentence at `at` on a worker; the next one follows when it is done.
fn speak_current(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    fill(s);
    let r = &mut s.read_aloud;
    let Some(u) = r.queue.get(r.at).cloned() else {
        r.playing = false;
        r.paused = false;
        show(app, s, ui);
        return app.message(s, &tr!("Finished reading."));
    };
    let Some(engine) = r.speaker.clone() else {
        return;
    };
    r.run += 1;
    let run = r.run;
    let rate = r.rate;
    let voices = r.voices.clone();
    let default_lang = r.default_lang.clone();
    let chosen = s.settings.speech_voice.clone();
    // Follow the reading through the document.
    let page = u.page;
    let off_page = s
        .tab_ref()
        .is_some_and(|t| t.uid == s.read_aloud.tab && t.view.current_page != page);
    if off_page {
        app.jump(s, ui, page, None);
    }
    show(app, s, ui);
    pagebara_thread::spawn("read-aloud", move || {
        let list = voice_list(&*engine, &voices);
        let lang = u.lang.as_deref().or(default_lang.as_deref());
        let voice = speech::pick(&list, chosen.as_deref(), lang).map(str::to_owned);
        let spoken = engine.speak(&u.text, rate, u.lang.as_deref(), voice.as_deref());
        post_act(move |app, s, ui| {
            let r = &mut s.read_aloud;
            if r.run != run || !r.playing || r.paused {
                return;
            }
            r.voice_used = voice;
            // The engine is missing or broken: stop rather than skip through the text.
            if let Err(e) = spoken {
                log::warn!("speech: {e}");
                stop(app, s, ui);
                return app.message(
                    s,
                    &tr!("Reading aloud stopped: the speech engine is not working."),
                );
            }
            r.at += 1;
            speak_current(app, s, ui);
        });
    });
}

/// Shows where reading is, and the highlight.
fn show(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let r = &s.read_aloud;
    let rs = ui.global::<ReadAloudState>();
    rs.set_playing(r.playing);
    rs.set_paused(r.paused);
    let current = r.queue.get(r.at).filter(|_| r.playing || r.paused);
    rs.set_sentence(current.map_or(String::new(), |u| u.text.clone()).into());
    let position = match (current, s.tab_ref()) {
        (Some(u), Some(t)) => {
            // As the tool row has it: the page's label, and its number when they differ.
            let (label, n) = (t.session.page_label(u.page), t.session.page_count());
            if label == (u.page + 1).to_string() {
                tr!("Page {} of {}", u.page + 1, n)
            } else {
                tr!("Page {} ({} of {})", label, u.page + 1, n)
            }
        }
        _ => String::new(),
    };
    rs.set_position(position.into());
    if let Some(t) = s.tab() {
        t.overlay_dirty = true;
    }
    app.overlay_changed(s, ui);
    let _ = ui.global::<AppState>();
}
