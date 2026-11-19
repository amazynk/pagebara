//! The signature library ("My signatures"): signatures drawn with a finger or taken from a
//! photo (its background removed), kept on the device, placed with a tap and then moved by
//! dragging or resized with a pinch.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use pagebara_annot::Point;
use pagebara_i18n::tr;
use pagebara_signatures::{Drawing, Library, Placement, Rgba, Signature, StrokePoint, Target};
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use super::{App, Sel, State, post_act};
use crate::{AppState, MobileWindow, Sheet, SigCard};

#[derive(Default)]
pub(crate) struct SignState {
    pub library: Option<Library>,
    /// Why there is no library.
    error: Option<String>,
    /// The library is away on a worker thread (importing a photo).
    busy: bool,
    pub selected: Option<String>,
    images: HashMap<String, (u64, Arc<Rgba>)>,
    /// The drawing pad's strokes, in pad pixels.
    strokes: Vec<Vec<StrokePoint>>,
}

impl SignState {
    pub fn open(data_dir: Option<&Path>) -> SignState {
        let opened = data_dir
            .ok_or_else(|| "no place for app data".to_owned())
            .and_then(|d| Library::open(d.join("signatures")).map_err(|e| e.to_string()));
        match opened {
            Ok(lib) => SignState {
                selected: lib.signatures().first().map(|g| g.id.clone()),
                library: Some(lib),
                ..SignState::default()
            },
            Err(e) => {
                log::warn!("signature library: {e}");
                SignState {
                    error: Some(e),
                    ..SignState::default()
                }
            }
        }
    }

    /// The processed image of signature `id`, decoded once per change.
    fn image(&mut self, id: &str) -> Option<(Signature, Arc<Rgba>)> {
        let lib = self.library.as_ref()?;
        let g = lib.get(id)?.clone();
        if let Some((m, img)) = self.images.get(id)
            && *m == g.modified
        {
            return Some((g, img.clone()));
        }
        let img = Arc::new(lib.image(id).ok()?);
        self.images.insert(id.to_owned(), (g.modified, img.clone()));
        Some((g, img))
    }
}

fn image(rgba: &Rgba) -> Image {
    let buf =
        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&rgba.pixels, rgba.width, rgba.height);
    Image::from_rgba8(buf)
}

pub fn show_library(s: &mut State, ui: &MobileWindow) {
    let ids: Vec<(String, String)> = s
        .sign
        .library
        .as_ref()
        .map(|l| {
            l.signatures()
                .iter()
                .map(|g| (g.id.clone(), g.name.clone()))
                .collect()
        })
        .unwrap_or_default();
    let cards: Vec<SigCard> = ids
        .into_iter()
        .map(|(id, name)| SigCard {
            selected: s.sign.selected.as_deref() == Some(&id),
            image: s
                .sign
                .image(&id)
                .map(|(_, img)| image(&img.fit_within(480, 160)))
                .unwrap_or_default(),
            name: name.into(),
        })
        .collect();
    let st = ui.global::<AppState>();
    st.set_signatures(ModelRc::new(VecModel::from(cards)));
    if let Some(e) = &s.sign.error {
        st.set_sig_hint(tr!("The signature library could not be opened: {}", e).into());
    }
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

pub fn wire(app: &Rc<App>, ui: &MobileWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_sig_select(move |i| {
        a.act(move |_, s, ui| {
            let id = s
                .sign
                .library
                .as_ref()
                .and_then(|l| l.signatures().get(i as usize))
                .map(|g| g.id.clone());
            s.sign.selected = id;
            show_library(s, ui);
        })
    });
    let a = app.clone();
    st.on_sig_delete(move |i| {
        a.act(move |app, s, ui| {
            let Some(lib) = &mut s.sign.library else {
                return;
            };
            let Some(id) = lib.signatures().get(i as usize).map(|g| g.id.clone()) else {
                return;
            };
            let deleted = lib.delete(&id);
            s.sign.selected = lib.signatures().first().map(|g| g.id.clone());
            if let Err(e) = deleted {
                app.message(s, &tr!("Could not delete the signature: {}", e));
            }
            show_library(s, ui);
        })
    });
    let a = app.clone();
    st.on_sig_from_photo(move || a.platform.pick_image());
    let a = app.clone();
    st.on_sig_draw(move |kind, x, y| {
        a.act(move |_, s, ui| {
            let strokes = &mut s.sign.strokes;
            match kind {
                0 => strokes.push(vec![StrokePoint::new(x, y)]),
                1 | 2 => {
                    if let Some(st) = strokes.last_mut() {
                        let last = st.last().copied();
                        if last.is_none_or(|p| (p.x - x).hypot(p.y - y) >= 1.0) {
                            st.push(StrokePoint::new(x, y));
                        }
                    }
                }
                _ => strokes.clear(),
            }
            ui.global::<AppState>()
                .set_draw_path(strokes_path(strokes).into());
        })
    });
    let a = app.clone();
    st.on_sig_draw_save(move || a.act(|app, s, ui| app.save_drawing(s, ui)));
}

impl App {
    fn save_drawing(self: &Rc<Self>, s: &mut State, ui: &MobileWindow) {
        let strokes: Vec<Vec<StrokePoint>> = std::mem::take(&mut s.sign.strokes)
            .into_iter()
            .filter(|st| !st.is_empty())
            .collect();
        let st = ui.global::<AppState>();
        st.set_draw_path("".into());
        st.set_sheet(Sheet::Signatures);
        if strokes.is_empty() {
            return;
        }
        let Some(lib) = &mut s.sign.library else {
            return self.message(s, &tr!("The signature library is not available."));
        };
        let drawing = Drawing {
            strokes,
            pen_width: 3.0,
        };
        match lib.add_drawn(&drawing, &tr!("My signature"), [0x10, 0x10, 0x10]) {
            Ok(g) => {
                s.sign.selected = Some(g.id);
                show_library(s, ui);
                self.message(s, &tr!("Saved. Tap the page to place it."));
            }
            Err(e) => self.message(s, &tr!("The signature could not be saved: {}", e)),
        }
    }

    /// Adds a photo of a signature to the library, its background removed, off the UI
    /// thread.
    pub fn import_signature(self: &Rc<Self>, s: &mut State, ui: &MobileWindow, path: PathBuf) {
        if s.sign.busy {
            return self.message(s, &tr!("One moment, still working on the last photo."));
        }
        let Some(mut lib) = s.sign.library.take() else {
            return self.message(s, &tr!("The signature library is not available."));
        };
        s.sign.busy = true;
        ui.global::<AppState>().set_busy(true);
        self.message(s, &tr!("Removing the background…"));
        pagebara_thread::spawn("sig-import", move || {
            let result = std::fs::read(&path)
                .map_err(|e| tr!("The photo could not be read: {}", e))
                .and_then(|bytes| {
                    lib.import(&bytes, &tr!("Signature"))
                        .map_err(|e| e.to_string())
                });
            post_act(move |app, s, ui| {
                s.sign.library = Some(lib);
                s.sign.busy = false;
                ui.global::<AppState>().set_busy(false);
                match result {
                    Ok(g) => {
                        s.sign.selected = Some(g.id);
                        app.message(s, &tr!("Added. Tap the page to place it."));
                    }
                    Err(e) => app.message(s, &e),
                }
                show_library(s, ui);
            });
        });
    }

    /// Places the selected signature centred on a point of a page.
    pub fn place_signature(
        self: &Rc<Self>,
        s: &mut State,
        ui: &MobileWindow,
        page: usize,
        u: f64,
        v: f64,
    ) {
        let Some(id) = s.sign.selected.clone() else {
            return self.message(s, &tr!("Draw a signature or add one from a photo first."));
        };
        let Some((sig, img)) = s.sign.image(&id) else {
            return self.message(s, &tr!("That signature could not be read."));
        };
        let Some(d) = &mut s.doc else { return };
        if !super::input::may_annotate(d) {
            return self.message(
                s,
                &tr!("This document's security settings do not allow signing."),
            );
        }
        let Some(r) = d.session.page_ref(page) else {
            return;
        };
        let (x, y) = d.session.geoms[page].to_pdf(u, v);
        let target = Target::At {
            center: Point::new(x, y),
            width: None,
        };
        let placed = d.session.edit(&tr!("Place signature"), |doc| {
            pagebara_signatures::place(doc, r, &sig, &img, target, Placement::Annotation)
        });
        match placed {
            Ok((Some(id), _)) => {
                d.selected = Some(Sel { page, id });
                self.message(s, &tr!("Placed. Drag to move it, pinch to resize."));
            }
            Ok((None, _)) => {}
            Err(e) => self.message(s, &tr!("The signature could not be placed: {}.", e)),
        }
        self.after_edit(s, ui);
    }
}
