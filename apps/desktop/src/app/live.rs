//! Live text: pictures of text become selectable when the pointer comes over them (see
//! `pagebara_app_core::live_text`), with nothing written to the document. The page's context
//! menu copies the text of a picture or of an area dragged out (also Alt+drag).

use std::rc::Rc;

use pagebara_annot::Point;
use pagebara_app_core::live_text::{self, Job, State as Spot};
use pagebara_content::Rect;
use pagebara_i18n::{tr, trn};
use slint::ComponentHandle;

use super::input::{Gesture, Pointer};
use super::{App, State, TextSel, post_act};
use crate::{AppWindow, ViewState};

/// Where the models come from (never downloaded for live text) and which language the
/// system uses.
pub fn configure(models_dir: Option<std::path::PathBuf>) {
    let mut sources = vec![pagebara_ocr::ModelSource::Bundled];
    if let Some(d) = std::env::var_os("PAGEBARA_OCR_MODELS") {
        sources.push(pagebara_ocr::ModelSource::Dir(d.into()));
    }
    sources.extend(models_dir.map(pagebara_ocr::ModelSource::Dir));
    let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
        .iter()
        .find_map(|v| std::env::var(v).ok().filter(|l| !l.is_empty()));
    live_text::configure(live_text::Config {
        sources,
        script: None,
        locale,
    });
}

/// Runs a job on a worker and hands its result back to the tab.
pub fn spawn(uid: u64, job: Job) {
    pagebara_thread::spawn("live-text", move || {
        let done = job.run_with(|part| post_act(move |app, s, ui| app.live_done(s, ui, uid, part)));
        post_act(move |app, s, ui| app.live_done(s, ui, uid, done));
    });
}

impl App {
    /// The pointer rests over `pt` of `page`, where the page has no text: recognises the
    /// picture there, if there is one.
    pub fn live_point(&self, s: &mut State, page: usize, pt: Point) {
        if !live_text::available() {
            return;
        }
        let Some(t) = s.tab() else { return };
        if t.held {
            return;
        }
        let uid = t.uid;
        let sample = t.session.text.cached(page).map(|p| p.text.clone());
        t.session.live().script(|| sample.unwrap_or_default());
        if let Some(job) = t.session.live().point(page, pt.x as f32, pt.y as f32) {
            spawn(uid, job);
        }
    }

    /// Recognises the scanned pages on screen ahead of the pointer, one picture at a time
    /// while nothing else is being recognised.
    pub fn live_prefetch(&self, s: &mut State) {
        if !live_text::available() {
            return;
        }
        let scanned = super::ocr::scanned(s).to_vec();
        let Some(t) = s.tab() else { return };
        if t.held || scanned.is_empty() {
            return;
        }
        let visible: Vec<usize> = t
            .view
            .last
            .pages
            .iter()
            .map(|(i, _)| *i)
            .filter(|i| scanned.contains(i))
            .collect();
        let uid = t.uid;
        let live = t.session.live();
        live.script(String::new);
        if let Some(job) = live.prefetch(&visible) {
            spawn(uid, job);
        }
    }

    /// Whether the pointer is over a picture whose text is being recognised (a busy cursor).
    pub fn live_busy_at(s: &State, page: usize, pt: Point) -> bool {
        s.tab_ref().is_some_and(|t| {
            t.session
                .live_ref()
                .is_some_and(|l| l.state_at(page, pt.x as f32, pt.y as f32) == Some(Spot::Running))
        })
    }

    fn live_done(self: &Rc<Self>, s: &mut State, ui: &AppWindow, uid: u64, done: live_text::Done) {
        let Some(i) = s.tab_index(uid) else { return };
        let t = &mut s.tabs[i];
        let sel = t.text_sel;
        let before = sel.map(|sel| t.session.sel_text(sel.page));
        let f = t.session.live().finish(done);
        if let Some(page) = f.page {
            // The selection keeps to its characters in the page's new text.
            if let (Some(sel), Some(old)) = (sel, before)
                && sel.page == page
            {
                let new = t.session.sel_text(page);
                let (anchor, caret) = live_text::remap(&old, &new, sel.anchor, sel.caret);
                t.text_sel = Some(TextSel {
                    page,
                    anchor,
                    caret,
                });
            }
            t.overlay_dirty = true;
        }
        if let Some(script) = f.warm {
            pagebara_thread::spawn("live-text-load", move || live_text::warm_up(script));
        }
        if let Some(job) = f.next {
            spawn(uid, job);
        } else if s.active == i && !f.partial {
            self.live_prefetch(s);
        }
        if let Some(e) = &f.error {
            log::warn!("live text: {e}");
        }
        if let Some(text) = f.copy {
            self.put_text(s, &text, false);
        }
        if f.page.is_some() && s.active == i {
            // The cursor shows the text is there now.
            if let Some(p) = s.input.hover {
                self.pointer(s, ui, p);
            }
            self.overlay_changed(s, ui);
        }
    }

    /// Puts recognised text (from a marked area or else a picture) on the clipboard.
    fn put_text(self: &Rc<Self>, s: &mut State, text: &str, area: bool) {
        let text = text.trim();
        if text.is_empty() {
            return self.message(s, &tr!("No text was found there."));
        }
        if s.tab_ref()
            .is_some_and(|t| !t.session.doc().permissions().copy)
        {
            return self.message(
                s,
                &tr!("This document's security settings do not allow copying text."),
            );
        }
        let n = text.chars().count();
        match crate::clipboard::set_text(text) {
            Ok(()) if area => self.message(
                s,
                &trn!(
                    "Copied {n} character from the area.",
                    "Copied {n} characters from the area.",
                    n
                ),
            ),
            Ok(()) => self.message(
                s,
                &trn!(
                    "Copied {n} character from the picture.",
                    "Copied {n} characters from the picture.",
                    n
                ),
            ),
            Err(e) => self.message(s, &tr!("Could not copy: {}", e)),
        }
        s.input.copied = Some(text.to_owned());
    }

    /// A right press on the pages: what the context menu offers there.
    pub fn context_menu(self: &Rc<Self>, s: &mut State, ui: &AppWindow, p: Pointer) {
        let vs = ui.global::<ViewState>();
        let picture = Self::hit_point(s, p.x, p.y).and_then(|(page, pt)| {
            let t = s.tab_ref()?;
            t.session
                .live_ref()?
                .picture_at(page, pt.x as f32, pt.y as f32)
                .map(|_| (page, pt))
        });
        s.input.menu_at = Self::hit_point(s, p.x, p.y);
        vs.set_ctx_edit(false);
        vs.set_ctx_spell(false);
        vs.set_ctx_picture(picture.is_some() && live_text::available());
        vs.set_ctx_copy(
            s.tab_ref()
                .and_then(|t| t.text_sel)
                .is_some_and(|sel| sel.anchor != sel.caret),
        );
        vs.set_ctx_area(live_text::available());
    }

    /// An item of the page's context menu.
    pub fn context_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str) {
        if let Some(action) = what.strip_prefix("edit:") {
            return self.edit_action(s, ui, action, "");
        }
        match what {
            "copy" => self.copy_selection(s),
            "select-all" => self.select_all_text(s, ui),
            "highlight" => self.markup_selection(s, ui, Some(0)),
            "underline" => self.markup_selection(s, ui, Some(1)),
            "strike" => self.markup_selection(s, ui, Some(2)),
            "copy-picture" => {
                let Some((page, pt)) = s.input.menu_at else {
                    return;
                };
                self.copy_picture(s, page, pt);
            }
            "copy-area" => {
                s.input.area_copy = true;
                ui.global::<ViewState>().set_cursor(3);
                self.message(s, &tr!("Drag over the text to copy."));
            }
            _ => {}
        }
    }

    /// Copies the text of the picture at a point.
    pub fn copy_picture(self: &Rc<Self>, s: &mut State, page: usize, pt: Point) {
        let Some(t) = s.tab() else { return };
        let uid = t.uid;
        let sample = t.session.text.cached(page).map(|p| p.text.clone());
        let live = t.session.live();
        live.script(|| sample.unwrap_or_default());
        match live.copy_picture(page, pt.x as f32, pt.y as f32) {
            Ok(job) => {
                self.message(s, &tr!("Recognizing the text in the picture…"));
                spawn(uid, job);
            }
            Err(Some(text)) => self.put_text(s, &text, false),
            Err(None) => self.message(s, &tr!("There is no picture there.")),
        }
    }

    /// Copies the text in a rectangle dragged out on a page (view coordinates).
    pub fn copy_area(
        self: &Rc<Self>,
        s: &mut State,
        page: usize,
        from: (f64, f64),
        to: (f64, f64),
    ) {
        let Some(t) = s.tab() else { return };
        let g = t.session.geoms[page];
        let (x0, y0) = g.to_pdf(from.0, from.1);
        let (x1, y1) = g.to_pdf(to.0, to.1);
        let rect = Rect::new(
            x0.min(x1) as f32,
            y0.min(y1) as f32,
            x0.max(x1) as f32,
            y0.max(y1) as f32,
        );
        if rect.width() < 4.0 || rect.height() < 4.0 {
            return;
        }
        if !live_text::available() {
            // Without the models: the page's own text in the area.
            let text = t.session.text.page(page);
            let chars: Vec<usize> = text
                .chars
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    let b = c.bbox();
                    let (x, y) = ((b.x0 + b.x1) / 2.0, (b.y0 + b.y1) / 2.0);
                    rect.x0 <= x && x <= rect.x1 && rect.y0 <= y && y <= rect.y1
                })
                .map(|(i, _)| i)
                .collect();
            let copied = match (chars.first(), chars.last()) {
                (Some(&a), Some(&b)) => pagebara_content::copy_text(&text, a..b + 1),
                _ => String::new(),
            };
            return self.put_text(s, &copied, true);
        }
        let uid = t.uid;
        let sample = t.session.text.cached(page).map(|p| p.text.clone());
        let live = t.session.live();
        live.script(|| sample.unwrap_or_default());
        let job = live.copy_area(page, rect);
        self.message(s, &tr!("Recognizing the text in the area…"));
        spawn(uid, job);
    }

    /// The page and user-space point under a window position.
    fn hit_point(s: &State, x: f64, y: f64) -> Option<(usize, Point)> {
        let t = s.tab_ref()?;
        let (page, u, v) = t.view.screen_to_page(x, y)?;
        let g = *t.session.geoms.get(page)?;
        let (px, py) = g.to_pdf(u, v);
        Some((page, Point::new(px, py)))
    }

    /// Starts dragging out an area to copy (after the menu item, or with Alt held).
    pub fn start_copy_area(&self, s: &mut State, page: usize, uv: (f64, f64)) {
        s.input.area_copy = false;
        s.gesture = Gesture::CopyArea {
            page,
            from: uv,
            to: uv,
        };
    }
}
