//! Text recognition: scanned documents get an offer to recognise their text; the dialog
//! asks for the script and pages, downloads the models on first use (with the size, progress
//! and cancel), and recognises page by page on a worker. The text layer is added as one
//! undoable edit, after which search and selection see the text.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use pagebara_app_core::ocr as core;
use pagebara_i18n::{tr, trn};
use pagebara_ocr::{Engine, ModelSource, Options, PageText, Progress};
use pagebara_render::{DefaultGlyphs, Limits, Renderer};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use super::{App, State};
use crate::{AppState, AppWindow, DialogKind, OcrState};

#[derive(Default)]
pub struct Ocr {
    cancel: Arc<AtomicBool>,
    pub running: bool,
    /// Pages that look scanned, by tab, for the text index they were found with.
    scanned: HashMap<u64, (usize, Vec<usize>)>,
    dismissed: HashSet<u64>,
    /// What the last run did, for scripts: pages and words recognised.
    pub last: Option<(usize, usize)>,
}

const PHASE_CHOOSE: i32 = 0;
const PHASE_ASK: i32 = 1;
const PHASE_DOWNLOAD: i32 = 2;
const PHASE_RUN: i32 = 3;

/// Pages looked at for the scanned-document offer.
const SCAN_PAGES: usize = 200;

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let g = ui.global::<OcrState>();
    g.set_attribution(pagebara_ocr::model::ATTRIBUTION.into());
    g.set_scripts(ModelRc::new(VecModel::from(
        core::script_labels()
            .into_iter()
            .map(SharedString::from)
            .collect::<Vec<_>>(),
    )));
    let a = app.clone();
    g.on_action(move |what| {
        let what = what.to_string();
        a.act(move |app, s, ui| app.ocr_action(s, ui, &what))
    });
}

fn text_key(t: &super::Tab) -> usize {
    Arc::as_ptr(&t.session.text) as usize
}

/// Looks for scanned pages in the active document when its text changed.
pub fn sync(_app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let Some(t) = s.tab_ref().filter(|t| !t.held) else {
        return show(s, ui);
    };
    let (uid, key) = (t.uid, text_key(t));
    if s.ocr.scanned.get(&uid).is_none_or(|(k, _)| *k != key) {
        let text = t.session.text.clone();
        let doc = t.session.doc().clone();
        let refs: Vec<_> = (0..t.session.page_count().min(SCAN_PAGES))
            .filter_map(|i| t.session.page_ref(i))
            .collect();
        s.ocr.scanned.insert(uid, (key, Vec::new()));
        pagebara_thread::spawn("ocr-scan", move || {
            let pages: Vec<usize> = refs
                .iter()
                .enumerate()
                .filter(|(i, r)| core::looks_scanned(&doc, **r, core::chars(&text.page(*i).text)))
                .map(|(i, _)| i)
                .collect();
            super::post_act(move |app, s, ui| {
                if let Some(e) = s.ocr.scanned.get_mut(&uid)
                    && e.0 == key
                {
                    e.1 = pages;
                }
                show(s, ui);
                app.live_prefetch(s);
            });
        });
    }
    show(s, ui);
}

fn show(s: &State, ui: &AppWindow) {
    let g = ui.global::<OcrState>();
    let offer = s
        .tab_ref()
        .filter(|t| !s.ocr.dismissed.contains(&t.uid) && !s.ocr.running)
        .and_then(|t| {
            let pages = &s.ocr.scanned.get(&t.uid)?.1;
            Some(match pages.len() {
                0 => return None,
                n if n == t.session.page_count() => {
                    tr!("This document looks scanned: its text cannot be searched or selected yet.")
                }
                1 => tr!(
                    "Page {} looks scanned: its text cannot be searched or selected yet.",
                    pages[0] + 1
                ),
                n => trn!(
                    "{n} page looks scanned: its text cannot be searched yet.",
                    "{n} pages look scanned: their text cannot be searched yet.",
                    n
                ),
            })
        });
    g.set_offer(offer.unwrap_or_default().into());
}

/// The pages of the active tab that look scanned.
pub fn scanned(s: &State) -> &[usize] {
    s.tab_ref()
        .and_then(|t| s.ocr.scanned.get(&t.uid))
        .map_or(&[], |(_, p)| p.as_slice())
}

/// For scripts: the pages that look scanned (1-based, comma separated).
pub fn scanned_pages(s: &State) -> String {
    s.tab_ref()
        .and_then(|t| s.ocr.scanned.get(&t.uid))
        .map(|(_, p)| {
            p.iter()
                .map(|i| (i + 1).to_string())
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}

/// The directories models are read from (or downloaded into).
fn sources(s: &State) -> Vec<ModelSource> {
    crate::net::model_sources(s.profile.models_dir.clone())
}

impl App {
    pub fn ocr_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str) {
        let g = ui.global::<OcrState>();
        let st = ui.global::<AppState>();
        match what {
            "open" => {
                if s.tabs.is_empty() {
                    return;
                }
                if s.ocr.running {
                    st.set_dialog(DialogKind::Ocr);
                    return;
                }
                g.set_phase(PHASE_CHOOSE);
                st.set_dialog_error("".into());
                st.set_dialog(DialogKind::Ocr);
            }
            "dismiss" => {
                if let Some(t) = s.tab_ref() {
                    s.ocr.dismissed.insert(t.uid);
                }
                show(s, ui);
            }
            "start" => {
                let Some(t) = s.tab_ref() else { return };
                if let Err(e) = core::pages(&g.get_pages(), t.session.page_count()) {
                    return st.set_dialog_error(e.into());
                }
                let script = core::script(g.get_script().max(0) as usize);
                let missing = core::missing_bytes(&sources(s), script);
                if missing > 0 {
                    g.set_download_size(pagebara_i18n::bytes(missing).into());
                    g.set_phase(PHASE_ASK);
                } else {
                    self.ocr_run(s, ui, false);
                }
            }
            "download" => self.ocr_run(s, ui, true),
            "cancel" => {
                self.ocr_cancel(s);
                st.set_dialog(DialogKind::None);
            }
            _ => {}
        }
    }

    /// Stops recognition or a download in progress.
    pub fn ocr_cancel(&self, s: &mut State) {
        if s.ocr.running {
            s.ocr.cancel.store(true, Ordering::Relaxed);
        }
    }

    fn ocr_run(self: &Rc<Self>, s: &mut State, ui: &AppWindow, download: bool) {
        let g = ui.global::<OcrState>();
        let Some(t) = s.tab_ref() else { return };
        let n = t.session.page_count();
        let Ok(pages) = core::pages(&g.get_pages(), n) else {
            return;
        };
        let pages = pages.unwrap_or_else(|| (0..n).collect());
        let (uid, doc, layout) = (t.uid, t.session.doc().clone(), t.session.layout_rev);
        let script = core::script(g.get_script().max(0) as usize);
        pagebara_app_core::live_text::set_script(script);
        let opts = Options {
            force: g.get_force(),
            min_text_chars: core::MIN_TEXT_CHARS,
            ..Options::default()
        };
        let sources = sources(s);
        let cancel = Arc::new(AtomicBool::new(false));
        s.ocr.cancel = cancel.clone();
        s.ocr.running = true;
        show(s, ui);
        g.set_phase(if download { PHASE_DOWNLOAD } else { PHASE_RUN });
        g.set_progress(0.0);
        g.set_progress_text(
            if download {
                tr!("Downloading the models…")
            } else {
                tr!("Loading the models…")
            }
            .into(),
        );
        pagebara_thread::spawn("ocr", move || {
            let last = AtomicU64::new(u64::MAX);
            let fetched = |p: pagebara_ocr::DownloadProgress<'_>| {
                let pct = p.done * 100 / p.total.max(1);
                if last.swap(pct, Ordering::Relaxed) != pct {
                    let text = tr!(
                        "Downloading {}: {} of {}",
                        p.file,
                        pagebara_i18n::bytes(p.done),
                        pagebara_i18n::bytes(p.total)
                    );
                    let f = pct as f32 / 100.0;
                    super::post(move |app| {
                        app.act(move |_app, _s, ui| {
                            let g = ui.global::<OcrState>();
                            g.set_phase(PHASE_DOWNLOAD);
                            g.set_progress(f);
                            g.set_progress_text(text.into());
                        })
                    });
                }
            };
            let result = (|| -> Result<Vec<PageText>, pagebara_ocr::Error> {
                let engine = Engine::load(&sources, script, &fetched, &cancel)?;
                let total = pages.len();
                super::post_act(move |_app, _s, ui| {
                    let g = ui.global::<OcrState>();
                    g.set_phase(PHASE_RUN);
                    g.set_progress(0.0);
                    g.set_progress_text(tr!("Recognizing page {} of {}…", 1, total).into());
                });
                let limits = Limits {
                    display_lists: 16 << 20,
                    images: 64 << 20,
                    glyphs: 16 << 20,
                    misc: 32 << 20,
                };
                let renderer = Renderer::with_options(doc, limits, Arc::new(DefaultGlyphs));
                let progress = |p: Progress| {
                    let f = p.done as f32 / p.total.max(1) as f32;
                    let text = if p.done < p.total {
                        tr!("Recognizing page {} of {}…", p.done + 1, p.total)
                    } else {
                        tr!("Adding the text…")
                    };
                    super::post_act(move |app, s, ui| {
                        let g = ui.global::<OcrState>();
                        g.set_progress(f);
                        g.set_progress_text(text.into());
                        app.job_progress(s, ui, Some(f64::from(f)));
                    });
                };
                let out = pagebara_ocr::recognize_pages(
                    &engine, &renderer, &pages, &opts, &progress, &cancel,
                )?;
                Ok(out
                    .into_iter()
                    .flatten()
                    .filter(|t| t.word_count() > 0)
                    .collect())
            })();
            super::post_act(move |app, s, ui| app.ocr_done(s, ui, uid, layout, result));
        });
    }

    fn ocr_done(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        uid: u64,
        layout: u64,
        result: Result<Vec<PageText>, pagebara_ocr::Error>,
    ) {
        s.ocr.running = false;
        let st = ui.global::<AppState>();
        let g = ui.global::<OcrState>();
        g.set_phase(PHASE_CHOOSE);
        let (title, body) = match &result {
            Ok(t) if t.is_empty() => (tr!("Text recognition finished"), tr!("No text was found.")),
            Ok(t) => (
                tr!("Text recognition finished"),
                trn!("{n} page recognised.", "{n} pages recognised.", t.len()),
            ),
            Err(pagebara_ocr::Error::Cancelled) => {
                (tr!("Text recognition cancelled"), String::new())
            }
            Err(e) => (tr!("Text recognition failed"), e.to_string()),
        };
        self.job_done(s, ui, &title, &body);
        let texts = match result {
            Ok(t) => t,
            Err(pagebara_ocr::Error::Cancelled) => {
                st.set_dialog(DialogKind::None);
                show(s, ui);
                return self.message(s, &tr!("Text recognition cancelled."));
            }
            Err(e) => {
                let text = match &e {
                    pagebara_ocr::Error::Download(..)
                    | pagebara_ocr::Error::ModelUnavailable(_) => {
                        tr!(
                            "The models could not be downloaded: {}. Check the connection and try again.",
                            e
                        )
                    }
                    e => tr!("Text recognition failed: {}.", e),
                };
                st.set_dialog(DialogKind::Ocr);
                st.set_dialog_error(text.into());
                return show(s, ui);
            }
        };
        st.set_dialog(DialogKind::None);
        let Some(i) = s.tab_index(uid) else { return };
        if s.tabs[i].session.layout_rev != layout {
            return self.message(
                s,
                &tr!("The pages changed meanwhile; run Recognize text again."),
            );
        }
        if texts.is_empty() {
            show(s, ui);
            return self.message(s, &tr!("No text was found on those pages."));
        }
        let words: usize = texts.iter().map(PageText::word_count).sum();
        let pages = texts.len();
        let edit = s.tabs[i].session.edit_text(&tr!("Recognize text"), |d| {
            pagebara_ocr::add_text_layer(d, &texts)
        });
        match edit {
            Ok((_, changed)) => {
                s.ocr.last = Some((pages, words));
                self.select_tab(s, ui, i);
                if let Some(t) = s.tab() {
                    t.search.invalidate();
                }
                self.after_edit(s, ui, changed.pages);
                self.restart_search(s, ui);
                self.message(
                    s,
                    &tr!(
                        "Recognized {} on {}. Search and select the text now.",
                        trn!("{n} word", "{n} words", words),
                        trn!("{n} page", "{n} pages", pages)
                    ),
                );
            }
            Err(e) => self.show_message(s, ui, &tr!("Could not add the text"), &e.to_string()),
        }
    }
}
