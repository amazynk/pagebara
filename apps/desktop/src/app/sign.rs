//! Signing: the signature library ("My signatures") with live background-removal previews
//! computed off the UI thread, drawing new signatures, and placing them on pages.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use pagebara_annot::{Point, Rect};
use pagebara_i18n::{decimal, tr};
use pagebara_signatures::{
    Drawing, Ink, Library, PREVIEW_SIDE, Placement, Prepared, Rgba, Settings, Signature, Source,
    StrokePoint, Target,
};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use super::{App, Sel, State};
use crate::{AppState, AppWindow, DialogKind, SignatureRow};

#[derive(Default)]
pub struct SignState {
    pub library: Option<Library>,
    /// The library is away on a worker thread (import, re-processing).
    pub busy: bool,
    pub selected: Option<String>,
    /// The analysed original of the selected imported signature, for previews.
    prepared: Option<(String, Arc<Prepared>)>,
    preview_running: bool,
    preview_again: bool,
    images: HashMap<String, (u64, Arc<Rgba>)>,
    thumbs: HashMap<String, (u64, Image)>,
    pub strokes: Vec<Vec<StrokePoint>>,
}

fn image(rgba: &Rgba) -> Image {
    let buf =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba.pixels, rgba.width, rgba.height);
    Image::from_rgba8(buf)
}

pub fn load_library(_app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    match s.profile.library() {
        Ok(lib) => {
            s.sign.selected = lib.signatures().first().map(|g| g.id.clone());
            s.sign.library = Some(lib);
            show_library(s, ui);
        }
        Err(e) => {
            log::warn!("signature library: {e}");
            ui.global::<AppState>()
                .set_sig_error(tr!("The signature library could not be opened: {}", e).into());
        }
    }
}

fn meta(g: &Signature) -> String {
    let mm = decimal(g.default_width_mm, 0);
    match g.source {
        Source::Drawn => tr!("Drawn · {} mm", mm),
        Source::Imported { .. } => tr!("Imported image · {} mm", mm),
    }
}

/// The list and the selected signature's settings.
pub fn show_library(s: &mut State, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    let Some(lib) = &s.sign.library else { return };
    let selected = s.sign.selected.clone();
    let mut rows = Vec::new();
    for g in lib.signatures() {
        let thumb = match s.sign.thumbs.get(&g.id) {
            Some((m, img)) if *m == g.modified => img.clone(),
            _ => {
                let img = lib
                    .image(&g.id)
                    .map(|r| image(&r.fit_within(240, 96)))
                    .unwrap_or_default();
                s.sign
                    .thumbs
                    .insert(g.id.clone(), (g.modified, img.clone()));
                img
            }
        };
        rows.push(SignatureRow {
            id: g.id.as_str().into(),
            name: g.name.as_str().into(),
            meta: meta(g).into(),
            image: thumb,
            selected: selected.as_deref() == Some(g.id.as_str()),
        });
    }
    let index = lib
        .signatures()
        .iter()
        .position(|g| Some(g.id.as_str()) == selected.as_deref());
    st.set_signatures(ModelRc::new(VecModel::from(rows)));
    st.set_sig_selected(index.map_or(-1, |i| i as i32));
    let Some(g) = index.map(|i| lib.signatures()[i].clone()) else {
        return;
    };
    st.set_sig_name(g.name.as_str().into());
    st.set_sig_imported(matches!(g.source, Source::Imported { .. }));
    st.set_sig_remove_bg(g.settings.remove_background);
    st.set_sig_threshold(f32::from(g.settings.threshold));
    st.set_sig_ink(match g.settings.ink {
        Ink::Original => 0,
        Ink::Color([0x0b, 0x24, 0x7a]) => 2,
        Ink::Color(_) => 1,
    });
    st.set_sig_width_mm(g.default_width_mm as f32);
    if let Ok(img) = lib.image(&g.id) {
        st.set_sig_preview(image(&img));
    }
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_sig_select(move |i| a.act(move |app, s, ui| app.sig_select(s, ui, i as usize)));
    let a = app.clone();
    st.on_sig_action(move |what| {
        let what = what.to_string();
        a.act(move |app, s, ui| app.sig_action(s, ui, &what))
    });
    let a = app.clone();
    st.on_sig_move(move |i, to| {
        a.act(move |app, s, ui| {
            let Some(lib) = &mut s.sign.library else {
                return;
            };
            let Some(id) = lib.signatures().get(i as usize).map(|g| g.id.clone()) else {
                return;
            };
            if let Err(e) = lib.move_to(&id, to.max(0) as usize) {
                app.message(s, &tr!("Could not move the signature: {}", e));
            }
            show_library(s, ui);
        })
    });
    let a = app.clone();
    st.on_sig_draw(move |kind, x, y| {
        a.act(move |_app, s, ui| {
            match kind {
                0 => s.sign.strokes.push(vec![StrokePoint::new(x, y)]),
                1 | 2 => {
                    if let Some(st) = s.sign.strokes.last_mut() {
                        let last = st.last().copied();
                        if last.is_none_or(|p| (p.x - x).hypot(p.y - y) >= 1.0) {
                            st.push(StrokePoint::new(x, y));
                        }
                    }
                }
                _ => s.sign.strokes.clear(),
            }
            ui.global::<AppState>()
                .set_dialog_text(strokes_path(&s.sign.strokes).into());
        })
    });
}

fn strokes_path(strokes: &[Vec<StrokePoint>]) -> String {
    let mut out = String::new();
    for st in strokes {
        for (i, p) in st.iter().enumerate() {
            let op = if i == 0 { 'M' } else { 'L' };
            out.push_str(&format!("{op}{:.1} {:.1} ", p.x, p.y));
        }
        if st.len() == 1 {
            out.push_str(&format!("L{:.1} {:.1} ", st[0].x + 0.5, st[0].y));
        }
    }
    out
}

impl App {
    pub fn sign_panel_refresh(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        show_library(s, ui);
        self.prepare_preview(s);
    }

    fn sig_select(self: &Rc<Self>, s: &mut State, ui: &AppWindow, i: usize) {
        let Some(lib) = &s.sign.library else { return };
        let Some(g) = lib.signatures().get(i) else {
            return;
        };
        s.sign.selected = Some(g.id.clone());
        show_library(s, ui);
        self.prepare_preview(s);
    }

    /// Analyses the selected imported signature's original for live previews.
    fn prepare_preview(self: &Rc<Self>, s: &mut State) {
        let Some(lib) = &s.sign.library else { return };
        let Some(id) = s.sign.selected.clone() else {
            return;
        };
        if s.sign.prepared.as_ref().is_some_and(|(p, _)| *p == id) {
            return;
        }
        let Some(g) = lib.get(&id) else { return };
        if !matches!(g.source, Source::Imported { .. }) {
            return;
        }
        let Ok(original) = lib.original(&id) else {
            return;
        };
        pagebara_thread::spawn("sig-prepare", move || {
            let p = Arc::new(Prepared::new(&original, PREVIEW_SIDE));
            super::post_act(move |_app, s, _ui| {
                if s.sign.selected.as_deref() == Some(id.as_str()) {
                    s.sign.prepared = Some((id, p));
                }
            });
        });
    }

    fn settings_from_ui(ui: &AppWindow, imported: bool) -> Settings {
        let st = ui.global::<AppState>();
        let ink = match st.get_sig_ink() {
            0 if imported => Ink::Original,
            2 => Ink::DARK_BLUE,
            _ => Ink::BLACK,
        };
        Settings {
            remove_background: !imported || st.get_sig_remove_bg(),
            threshold: st.get_sig_threshold().round().clamp(0.0, 100.0) as u8,
            ink,
            despeckle: true,
        }
    }

    /// Shows the selected signature with the slider's settings, computed off the UI thread.
    fn live_preview(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        if s.sign.preview_running {
            s.sign.preview_again = true;
            return;
        }
        let Some((id, prepared)) = s.sign.prepared.clone() else {
            return;
        };
        if s.sign.selected.as_deref() != Some(id.as_str()) {
            return;
        }
        let settings = Self::settings_from_ui(ui, true);
        s.sign.preview_running = true;
        ui.global::<AppState>().set_sig_preview_busy(true);
        pagebara_thread::spawn("sig-preview", move || {
            let out = prepared.apply(&settings);
            super::post_act(move |app, s, ui| {
                s.sign.preview_running = false;
                ui.global::<AppState>().set_sig_preview_busy(false);
                if s.sign.selected.as_deref() == Some(id.as_str()) {
                    ui.global::<AppState>().set_sig_preview(image(&out));
                }
                if std::mem::take(&mut s.sign.preview_again) {
                    app.live_preview(s, ui);
                }
            });
        });
    }

    /// Runs `f` on the library on a worker thread, then shows the result.
    fn with_library_off_thread(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        busy_text: &str,
        f: impl FnOnce(&mut Library) -> Result<Option<String>, String> + Send + 'static,
    ) {
        if s.sign.busy {
            return;
        }
        let Some(mut lib) = s.sign.library.take() else {
            return;
        };
        s.sign.busy = true;
        ui.global::<AppState>().set_sig_hint(busy_text.into());
        ui.global::<AppState>().set_sig_preview_busy(true);
        pagebara_thread::spawn("sig-library", move || {
            let result = f(&mut lib);
            super::post_act(move |app, s, ui| {
                s.sign.library = Some(lib);
                s.sign.busy = false;
                let st = ui.global::<AppState>();
                st.set_sig_hint("".into());
                st.set_sig_preview_busy(false);
                match result {
                    Ok(Some(id)) => {
                        s.sign.selected = Some(id);
                        s.sign.prepared = None;
                        st.set_sig_error("".into());
                    }
                    Ok(None) => st.set_sig_error("".into()),
                    Err(e) => st.set_sig_error(e.into()),
                }
                show_library(s, ui);
                app.prepare_preview(s);
            });
        });
    }

    fn sig_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str) {
        let st = ui.global::<AppState>();
        let id = s.sign.selected.clone();
        match what {
            "import" => self.import_signature(s),
            "draw" => {
                s.sign.strokes.clear();
                st.set_dialog_text("".into());
                st.set_dialog_error("".into());
                if st.get_sig_ink() == 0 {
                    st.set_sig_ink(1);
                }
                st.set_dialog(DialogKind::DrawSignature);
            }
            "preview" => self.live_preview(s, ui),
            "settings" => {
                let Some(id) = id else { return };
                let imported = st.get_sig_imported();
                let settings = Self::settings_from_ui(ui, imported);
                if imported {
                    self.live_preview(s, ui);
                }
                self.with_library_off_thread(s, ui, &tr!("Applying…"), move |lib| {
                    lib.set_settings(&id, settings).map_err(|e| e.to_string())?;
                    Ok(None)
                });
            }
            "width" => {
                let Some(id) = id else { return };
                let mm = f64::from(st.get_sig_width_mm());
                if let Some(lib) = &mut s.sign.library
                    && let Err(e) = lib.set_default_width(&id, mm)
                {
                    st.set_sig_error(e.to_string().into());
                }
                show_library(s, ui);
            }
            "delete" => {
                let Some(id) = id else { return };
                let Some(lib) = &mut s.sign.library else {
                    return;
                };
                let name = lib.get(&id).map(|g| g.name.clone()).unwrap_or_default();
                if let Err(e) = lib.delete(&id) {
                    st.set_sig_error(e.to_string().into());
                }
                s.sign.selected = lib.signatures().first().map(|g| g.id.clone());
                s.sign.prepared = None;
                show_library(s, ui);
                self.message(s, &tr!("Deleted “{}” from your signatures.", name));
            }
            _ => {}
        }
    }

    fn import_signature(self: &Rc<Self>, _s: &mut State) {
        let app = self.clone();
        let task = slint::spawn_local(async move {
            let Some(f) = rfd::AsyncFileDialog::new()
                .set_title(tr!("Import a signature image"))
                .add_filter(tr!("Images"), &["png", "jpg", "jpeg", "PNG", "JPG", "JPEG"])
                .pick_file()
                .await
            else {
                return;
            };
            let path = f.path().to_owned();
            app.act(move |app, s, ui| app.import_signature_file(s, ui, path));
        });
        if let Err(e) = task {
            log::warn!("cannot show the file dialog: {e}");
        }
    }

    pub fn import_signature_file(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        path: std::path::PathBuf,
    ) {
        let name = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            // TRANSLATORS: the name of a signature imported from a file without a name
            .unwrap_or_else(|| tr!("Signature"));
        self.with_library_off_thread(s, ui, &tr!("Removing the background…"), move |lib| {
            let bytes =
                std::fs::read(&path).map_err(|e| tr!("The image could not be read: {}", e))?;
            let g = lib.import(&bytes, &name).map_err(|e| e.to_string())?;
            Ok(Some(g.id))
        });
    }

    /// Saves the drawing from the draw dialog as a new signature.
    pub fn save_drawing(self: &Rc<Self>, s: &mut State, ui: &AppWindow, name: &str) {
        let strokes: Vec<Vec<StrokePoint>> = s
            .sign
            .strokes
            .iter()
            .filter(|st| !st.is_empty())
            .cloned()
            .collect();
        let st = ui.global::<AppState>();
        if strokes.is_empty() {
            st.set_dialog_error(tr!("Draw your signature first.").into());
            return;
        }
        let color = if st.get_sig_ink() == 2 {
            [0x0b, 0x24, 0x7a]
        } else {
            [0x10, 0x10, 0x10]
        };
        let drawing = Drawing {
            strokes,
            pen_width: 2.5,
        };
        let Some(lib) = &mut s.sign.library else {
            return;
        };
        let name = if name.trim().is_empty() {
            tr!("My signature")
        } else {
            name.trim().to_owned()
        };
        match lib.add_drawn(&drawing, &name, color) {
            Ok(g) => {
                s.sign.selected = Some(g.id);
                st.set_dialog(DialogKind::None);
                show_library(s, ui);
                self.message(s, &tr!("Signature saved. Click on the page to place it."));
            }
            Err(e) => st.set_dialog_error(e.to_string().into()),
        }
    }

    pub fn rename_signature(self: &Rc<Self>, s: &mut State, ui: &AppWindow, name: &str) {
        let Some(id) = s.sign.selected.clone() else {
            return;
        };
        let Some(lib) = &mut s.sign.library else {
            return;
        };
        if let Err(e) = lib.rename(&id, name.trim()) {
            ui.global::<AppState>().set_sig_error(e.to_string().into());
        }
        ui.global::<AppState>().set_dialog(DialogKind::None);
        show_library(s, ui);
    }

    /// The processed image of signature `id`, decoded once per change.
    fn sig_image(s: &mut State, id: &str) -> Option<(Signature, Arc<Rgba>)> {
        let lib = s.sign.library.as_ref()?;
        let g = lib.get(id)?.clone();
        if let Some((m, img)) = s.sign.images.get(id)
            && *m == g.modified
        {
            return Some((g, img.clone()));
        }
        let img = Arc::new(lib.image(id).ok()?);
        s.sign
            .images
            .insert(id.to_owned(), (g.modified, img.clone()));
        Some((g, img))
    }

    /// Places the selected signature: centred on a click, or into a dragged rectangle.
    pub fn place_signature(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        page: usize,
        from: (f64, f64),
        to: (f64, f64),
    ) {
        if s.sign.library.is_none() && s.sign.busy {
            return self.message(s, &tr!("One moment, the signature library is busy."));
        }
        let Some(id) = s.sign.selected.clone() else {
            self.overlay_changed(s, ui);
            return self.message(s, &tr!("Import or draw a signature first."));
        };
        let Some((sig, img)) = Self::sig_image(s, &id) else {
            return self.message(s, &tr!("That signature could not be read."));
        };
        if !self.may_annotate(s, ui) {
            return self.overlay_changed(s, ui);
        }
        let Some(t) = s.tab_ref() else { return };
        let Some(r) = t.session.page_ref(page) else {
            return;
        };
        let g = t.session.geoms[page];
        let (w, h) = ((to.0 - from.0).abs(), (to.1 - from.1).abs());
        let target = if w < 8.0 || h < 8.0 {
            let (x, y) = g.to_pdf(from.0, from.1);
            Target::At {
                center: Point::new(x, y),
                width: None,
            }
        } else {
            // Keep the signature's proportions inside the dragged box.
            let aspect = f64::from(img.height.max(1)) / f64::from(img.width.max(1));
            let (bw, bh) = if h / w > aspect {
                (w, w * aspect)
            } else {
                (h / aspect, h)
            };
            let x0 = from.0.min(to.0);
            let y0 = from.1.min(to.1);
            let [a, b, c, d] = g.rect_to_pdf([x0, y0, bw, bh]);
            Target::Rect(Rect::new(a, b, c, d))
        };
        let author = ui.global::<AppState>().get_author().to_string();
        let placed = self.edit(s, ui, &tr!("Place signature"), |d| {
            Ok(pagebara_signatures::place_by(
                d,
                r,
                &sig,
                &img,
                target,
                Placement::Annotation,
                Some(&author),
            )?)
        });
        if let Some(Some(id)) = placed
            && let Some(t) = s.tab()
        {
            t.selected = Some(Sel { page, id });
            self.selection_changed(s, ui);
            self.message(s, &tr!("Placed. Drag to move it, drag a corner to resize."));
        }
    }
}
