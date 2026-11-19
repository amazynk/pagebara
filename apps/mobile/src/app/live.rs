//! Live text on phones: a long press on text in a picture (a screenshot, a scan, a title
//! drawn as outlines) recognises the picture and then selects the word there, as on real
//! text; nothing is written to the document. Copy puts the selection on the clipboard.
//!
//! The Latin models come with the app. A document that seems to be in another script
//! (its language, its text, the phone's language) gets a quiet offer to download that
//! script's models, once per script unless accepted.

use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pagebara_app_core::live_text::{self, Job};
use pagebara_app_core::ocr::{human_size, script_name};
use pagebara_i18n::{tr, trn};
use pagebara_ocr::Script;
use slint::ComponentHandle;

use super::{App, State, TextSel, post_act};
use crate::{AppState, MobileWindow};

/// Downloads of other scripts' models: where they go, and what the user said.
#[derive(Default)]
pub struct Offers {
    /// The app's model folder (downloads land there).
    dir: Option<PathBuf>,
    /// The script offered now.
    shown: Option<Script>,
    /// Scripts the user said no to (kept in the model folder).
    declined: HashSet<Script>,
    /// A download running: its script and cancel flag.
    running: Option<(Script, Arc<AtomicBool>)>,
}

impl Offers {
    fn declined_file(&self) -> Option<PathBuf> {
        Some(self.dir.as_ref()?.join("declined.txt"))
    }

    fn save(&self) {
        let (Some(dir), Some(f)) = (&self.dir, self.declined_file()) else {
            return;
        };
        let names: Vec<&str> = self.declined.iter().map(|&s| script_name(s)).collect();
        let r = std::fs::create_dir_all(dir).and_then(|()| std::fs::write(&f, names.join("\n")));
        if let Err(e) = r {
            log::warn!("cannot save {}: {e}", f.display());
        }
    }

    /// Whether a script is being offered or downloaded (for scripts).
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    pub fn state(&self) -> &'static str {
        match (self.running.is_some(), self.shown.is_some()) {
            (true, _) => "downloading",
            (false, true) => "offered",
            _ => "none",
        }
    }
}

/// The bundled models, those downloaded into `data_dir` (and, for tests,
/// `PAGEBARA_OCR_MODELS`).
pub fn configure(data_dir: Option<&std::path::Path>) -> Offers {
    let mut sources = vec![pagebara_ocr::ModelSource::Bundled];
    if let Some(d) = std::env::var_os("PAGEBARA_OCR_MODELS").filter(|d| !d.is_empty()) {
        sources.push(pagebara_ocr::ModelSource::Dir(d.into()));
    }
    let dir = data_dir.map(|d| d.join("ocr-models"));
    sources.extend(dir.clone().map(pagebara_ocr::ModelSource::Dir));
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|l| !l.is_empty()));
    live_text::configure(live_text::Config {
        sources,
        script: None,
        locale,
    });
    let mut offers = Offers {
        dir,
        ..Offers::default()
    };
    if let Some(text) = offers
        .declined_file()
        .and_then(|f| std::fs::read_to_string(f).ok())
    {
        offers.declined = Script::ALL
            .into_iter()
            .filter(|&s| text.lines().any(|l| l == script_name(s)))
            .collect();
    }
    offers
}

/// Where downloads come from: the network, or for tests a folder (`PAGEBARA_OCR_FETCH_DIR`).
fn fetch() -> Arc<dyn pagebara_ocr::Fetch> {
    match std::env::var_os("PAGEBARA_OCR_FETCH_DIR").filter(|d| !d.is_empty()) {
        Some(d) => Arc::new(pagebara_app_core::ocr::DirFetch(d.into())),
        None => Arc::new(pagebara_ocr::HttpFetch),
    }
}

fn spawn(job: Job) {
    pagebara_thread::spawn("live-text", move || {
        let done = job.run_with(|part| post_act(move |app, s, ui| app.live_done(s, ui, part)));
        post_act(move |app, s, ui| app.live_done(s, ui, done));
    });
}

impl App {
    /// Offers the models of the script the document seems to be in, when they are not at
    /// hand and the user has not said no.
    fn live_offer(&self, s: &mut State, sample: &str) {
        let o = &s.live;
        if o.dir.is_none() || o.shown.is_some() || o.running.is_some() {
            return;
        }
        let Some(d) = &s.doc else { return };
        let Some(script) = live_text::script_to_offer(d.session.doc(), sample) else {
            return;
        };
        if o.declined.contains(&script) {
            return;
        }
        s.live.shown = Some(script);
        let Some(ui) = self.ui.upgrade() else { return };
        let st = ui.global::<AppState>();
        st.set_offer(
            tr!(
                "Text in pictures here may be {}. Download its text recognition ({})?",
                pagebara_i18n::translate(script_name(script), &[]),
                human_size(live_text::download_size(script))
            )
            .into(),
        );
        st.set_offer_action(tr!("Download").into());
    }

    /// The offer's button: downloads the models.
    pub fn offer_accept(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let o = &mut s.live;
        let (Some(script), Some(dir)) = (o.shown, o.dir.clone()) else {
            return;
        };
        if o.running.is_some() {
            return;
        }
        let cancel = Arc::new(AtomicBool::new(false));
        o.running = Some((script, cancel.clone()));
        let st = ui.global::<AppState>();
        let what = pagebara_i18n::translate(script_name(script), &[]);
        st.set_offer(tr!("Downloading {} text recognition…", what).into());
        st.set_offer_action("".into());
        pagebara_thread::spawn("live-text-load", move || {
            let last = AtomicU64::new(u64::MAX);
            let progress = |p: pagebara_ocr::DownloadProgress<'_>| {
                let pct = p.done * 100 / p.total.max(1);
                if last.swap(pct, Ordering::Relaxed) != pct {
                    let text = tr!("Downloading {} text recognition… {}%", what, pct);
                    post_act(move |_app, s, ui| {
                        if s.live.running.is_some() {
                            ui.global::<AppState>().set_offer(text.into());
                        }
                    });
                }
            };
            let r = live_text::download(script, dir, fetch(), &progress, &cancel);
            post_act(move |app, s, ui| app.offer_done(s, ui, script, r));
        });
    }

    /// "Not now": no more offers of this script; a download in progress stops.
    pub fn offer_dismiss(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let o = &mut s.live;
        if let Some((_, c)) = o.running.take() {
            c.store(true, Ordering::Relaxed);
        } else if let Some(script) = o.shown {
            o.declined.insert(script);
            o.save();
        }
        o.shown = None;
        ui.global::<AppState>().set_offer("".into());
    }

    fn offer_done(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        script: Script,
        r: Result<(), pagebara_ocr::Error>,
    ) {
        let o = &mut s.live;
        if o.running.take().is_none() {
            // Dismissed meanwhile.
            return;
        }
        o.shown = None;
        ui.global::<AppState>().set_offer("".into());
        match r {
            Ok(()) => {
                if let Some(d) = &mut s.doc {
                    d.session.live().forget();
                    d.text_sel = None;
                    d.marks_dirty = true;
                }
                let what = pagebara_i18n::translate(script_name(script), &[]);
                self.message(
                    s,
                    &tr!(
                        "{} text recognition is ready: long-press the text again.",
                        what
                    ),
                );
                self.refresh(s, ui);
            }
            Err(pagebara_ocr::Error::Cancelled) => {}
            Err(e) => {
                log::warn!("live text download: {e}");
                self.message(
                    s,
                    &tr!(
                        "The text recognition could not be downloaded. Check the connection and try again."
                    ),
                );
            }
        }
    }

    /// A long press at `(px, py)` (user space) of `page` found no text: recognises the
    /// picture there, then selects the word under the finger.
    pub(super) fn live_long_press(&self, s: &mut State, page: usize, px: f64, py: f64) -> bool {
        if !live_text::available() {
            return false;
        }
        let Some(d) = &mut s.doc else { return false };
        let sample = d
            .session
            .text
            .cached(page)
            .map(|p| p.text.clone())
            .unwrap_or_default();
        let live = d.session.live();
        live.script(|| sample.clone());
        let job = live.point(page, px as f32, py as f32);
        let waiting = job.is_some()
            || live.state_at(page, px as f32, py as f32) == Some(live_text::State::Running);
        if waiting {
            s.input.live_word = Some((page, px, py));
            self.live_offer(s, &sample);
        }
        if let Some(job) = job {
            spawn(job);
        }
        waiting
    }

    fn live_done(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, done: live_text::Done) {
        let Some(d) = &mut s.doc else { return };
        let sel = d.text_sel;
        let before = sel.map(|sel| d.session.sel_text(sel.page));
        let f = d.session.live().finish(done);
        if let Some(script) = f.warm {
            pagebara_thread::spawn("live-text-load", move || live_text::warm_up(script));
        }
        if let Some(job) = f.next {
            spawn(job);
        }
        if let Some(e) = &f.error {
            log::warn!("live text: {e}");
        }
        let Some(page) = f.page else { return };
        // A selection keeps to its characters in the page's new text.
        if let (Some(sel), Some(old)) = (sel, before)
            && sel.page == page
        {
            let new = d.session.sel_text(page);
            let (anchor, caret) = live_text::remap(&old, &new, sel.anchor, sel.caret);
            d.text_sel = Some(TextSel {
                page,
                anchor,
                caret,
            });
        }
        if let Some((p, px, py)) = s.input.live_word
            && p == page
        {
            s.input.live_word = None;
            let text = d.session.sel_text(page);
            let (x, y) = (px as f32, py as f32);
            if pagebara_app_core::text_index::over_text(&text, x, y)
                && let Some(c) = pagebara_app_core::text_index::caret_at(&text, x, y)
                && let Some(r) = pagebara_app_core::text_index::word_at(
                    &text,
                    c.min(text.chars.len().saturating_sub(1)),
                )
            {
                d.text_sel = Some(TextSel {
                    page,
                    anchor: r.start,
                    caret: r.end,
                });
                d.selected = None;
            } else if f.partial {
                // Not read yet there: wait for more.
                s.input.live_word = Some((p, px, py));
            } else {
                self.message(s, &tr!("No text was found there."));
            }
        }
        if let Some(d) = &mut s.doc {
            d.marks_dirty = true;
        }
        self.refresh(s, ui);
    }

    /// Puts the selected text on the clipboard.
    pub fn copy_selection(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let Some(d) = &mut s.doc else { return };
        let Some(sel) = d.text_sel else { return };
        if !d.session.doc().permissions().copy {
            return self.message(
                s,
                &tr!("This document's security settings do not allow copying text."),
            );
        }
        let text = pagebara_content::copy_text(&d.session.sel_text(sel.page), sel.range());
        if text.is_empty() {
            return;
        }
        ui.invoke_copy_text(text.as_str().into());
        let n = text.chars().count();
        s.input.copied = Some(text);
        if let Some(d) = &mut s.doc {
            d.text_sel = None;
            d.marks_dirty = true;
        }
        self.message(
            s,
            &trn!("Copied {n} character.", "Copied {n} characters.", n),
        );
        ui.global::<AppState>().set_has_selection(false);
        self.refresh(s, ui);
    }
}
