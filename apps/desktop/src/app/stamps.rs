//! The output tools' dialogs: watermark, header and footer, Bates numbering and flatten.
//! Each change re-renders a preview of the first page stamped on a worker; applying is
//! one undoable edit of the document (Bates numbering of more files writes numbered
//! copies of the others, with a list of the ranges).

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use pagebara_i18n::{tr, tr_noop, trn};
use pagebara_stamp::{
    Anchor, BatesNumber, DocInfo, HeaderFooter, Kind, Layer, Margins, PageSet, Preset, Rgb, Size,
    Slots, Source, TextStyle, Tiling, Visibility, Watermark,
};
use pagebara_syntax::Document;
use slint::{
    ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, Timer, TimerMode,
    VecModel,
};

use super::{App, State, pick, post_act};
use crate::{AppWindow, ProState};

const WATERMARK: i32 = 1;
const HEADER: i32 = 2;
const BATES: i32 = 3;
const FLATTEN: i32 = 4;
pub const COMPARE: i32 = 5;

/// Fonts offered: shown name and the standard font used.
const FONTS: [(&str, &str); 6] = [
    ("Helvetica", "Helvetica"),
    ("Helvetica Bold", "Helvetica-Bold"),
    ("Times", "Times-Roman"),
    ("Times Bold", "Times-Bold"),
    ("Courier", "Courier"),
    ("Courier Bold", "Courier-Bold"),
];

/// The colour row's colours (`Palette2` in the UI).
const COLOURS: [[f32; 3]; 7] = [
    [0.98, 0.8, 0.08],
    [0.29, 0.87, 0.5],
    [0.38, 0.65, 0.98],
    [0.96, 0.45, 0.71],
    [0.98, 0.57, 0.24],
    [0.86, 0.15, 0.15],
    [0.09, 0.09, 0.11],
];

#[derive(Default)]
pub struct Pro {
    preview_timer: Option<Timer>,
    /// Counts previews asked for; only the latest one is shown.
    preview_serial: u64,
    wm_file: Option<PathBuf>,
    pub cmp_file: Option<PathBuf>,
    /// More files to number after the open document.
    bates_files: Vec<PathBuf>,
    presets: Vec<Preset>,
    /// What the last run did, for scripts.
    pub last: String,
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let g = ui.global::<ProState>();
    let fonts: Vec<SharedString> = FONTS.iter().map(|f| f.0.into()).collect();
    g.set_fonts(ModelRc::new(VecModel::from(fonts)));
    let a = app.clone();
    g.on_action(move |what, value| {
        let (what, value) = (what.to_string(), value.to_string());
        a.act(move |app, s, ui| app.pro_action(s, ui, &what, &value))
    });
}

fn presets_path(s: &State) -> Option<PathBuf> {
    s.profile
        .dir
        .clone()
        .or_else(pagebara_identity::config_dir)
        .map(|d| d.join("stamp-presets.json"))
}

fn load_presets(s: &mut State, ui: &AppWindow) {
    s.pro.presets = presets_path(s)
        .and_then(|p| std::fs::read_to_string(p).ok())
        .map(|j| Preset::from_json(&j))
        .unwrap_or_default();
    let names: Vec<SharedString> = s
        .pro
        .presets
        .iter()
        .filter(|p| matches!(p, Preset::Watermark { .. }))
        .map(|p| p.name().into())
        .collect();
    let g = ui.global::<ProState>();
    g.set_presets(ModelRc::new(VecModel::from(names)));
    g.set_preset(-1);
}

fn colour(i: i32) -> Rgb {
    Rgb(COLOURS[i.clamp(0, 6) as usize])
}

fn font(g: &ProState<'_>) -> String {
    FONTS[g.get_font().clamp(0, FONTS.len() as i32 - 1) as usize]
        .1
        .to_owned()
}

fn pages(g: &ProState<'_>) -> PageSet {
    PageSet {
        list: g.get_pages().to_string(),
        odd: g.get_parity() == 1,
        even: g.get_parity() == 2,
    }
}

fn watermark_spec(s: &State, ui: &AppWindow) -> Result<Watermark, String> {
    let g = ui.global::<ProState>();
    let fit = g.get_wm_size() / 100.0;
    let tiled = g.get_wm_tiled();
    let source = match g.get_wm_source() {
        0 => {
            let text = g.get_wm_text().to_string();
            if text.trim().is_empty() {
                return Err(tr!("Type the watermark text."));
            }
            Source::Text {
                text,
                style: TextStyle {
                    font: font(&g),
                    size: 72.0,
                    color: colour(g.get_colour()),
                    background: None,
                },
            }
        }
        k => {
            let Some(path) = s.pro.wm_file.clone() else {
                return Err(tr!("Choose a file for the watermark."));
            };
            if k == 1 {
                Source::Image {
                    path,
                    data: Vec::new(),
                }
            } else {
                Source::Pdf {
                    path,
                    page: g.get_wm_page().trim().parse().unwrap_or(1).max(1),
                    data: Vec::new(),
                }
            }
        }
    };
    let vis = g.get_wm_visibility();
    Ok(Watermark {
        source,
        size: Size::Fit(if tiled { fit * 0.4 } else { fit }),
        opacity: g.get_wm_opacity() / 100.0,
        rotation: [0.0, 45.0, 90.0, -45.0][g.get_wm_rotation().clamp(0, 3) as usize],
        anchor: Anchor::ALL[g.get_wm_anchor().clamp(0, 8) as usize],
        tiling: tiled.then_some(Tiling { gap: [36.0, 36.0] }),
        layer: if g.get_wm_behind() {
            Layer::Behind
        } else {
            Layer::Front
        },
        pages: pages(&g),
        group: (vis != 3).then(|| "Watermark".to_owned()),
        visibility: match vis {
            1 => Visibility::ViewOnly,
            2 => Visibility::PrintOnly,
            _ => Visibility::Always,
        },
        ..Watermark::default()
    })
}

fn header_spec(ui: &AppWindow, bates: bool) -> Result<HeaderFooter, String> {
    let g = ui.global::<ProState>();
    let margin: f32 = g.get_hf_margin().trim().parse().unwrap_or(36.0);
    let mut hf = HeaderFooter {
        style: TextStyle {
            font: font(&g),
            size: g.get_hf_size().clamp(4.0, 72.0),
            color: colour(g.get_colour()),
            background: None,
        },
        margins: Margins {
            left: margin,
            right: margin,
            top: margin * 2.0 / 3.0,
            bottom: margin * 2.0 / 3.0,
        },
        ..HeaderFooter::default()
    };
    if bates {
        let start = g.get_bates_start().trim().parse::<u64>();
        let digits = g.get_bates_digits().trim().parse::<usize>();
        let (Ok(start), Ok(digits)) = (start, digits) else {
            return Err(tr!("The first number and the digits must be numbers."));
        };
        hf.bates = Some(BatesNumber {
            prefix: g.get_bates_prefix().to_string(),
            suffix: g.get_bates_suffix().to_string(),
            digits: digits.clamp(1, 20),
            start,
        });
        hf.slots = Slots::one(g.get_bates_position().clamp(0, 5) as usize, "{bates}");
        if g.get_bates_box() {
            hf.style.background = Some(Rgb([1.0, 1.0, 1.0]));
        }
        return Ok(hf);
    }
    hf.slots = Slots(
        [
            g.get_hf_tl(),
            g.get_hf_tc(),
            g.get_hf_tr(),
            g.get_hf_bl(),
            g.get_hf_bc(),
            g.get_hf_br(),
        ]
        .map(|s| s.to_string()),
    );
    if hf.slots.is_empty() {
        return Err(tr!("Type a text for at least one place."));
    }
    hf.start = g
        .get_hf_start()
        .trim()
        .parse()
        .map_err(|_| tr!("The first page number must be a number."))?;
    if g.get_hf_skip_first() {
        hf.first = Some(Slots::default());
    }
    hf.pages = pages(&g);
    Ok(hf)
}

fn flatten_options(ui: &AppWindow) -> pagebara_flatten::Options {
    let g = ui.global::<ProState>();
    pagebara_flatten::Options {
        annotations: g.get_fl_annots(),
        links: g.get_fl_annots() && g.get_fl_links(),
        forms: g.get_fl_forms(),
        signatures: g.get_fl_forms() && g.get_fl_signatures(),
        layers: match g.get_fl_layers() {
            0 => pagebara_flatten::Layers::Apply,
            1 => pagebara_flatten::Layers::Merge,
            _ => pagebara_flatten::Layers::Keep,
        },
        transparency: g.get_fl_transparency().then_some(200.0),
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn doc_info(s: &State) -> DocInfo {
    let Some(t) = s.tab_ref() else {
        return DocInfo::default();
    };
    let name = t
        .session
        .path
        .as_deref()
        .map(file_name)
        .unwrap_or_else(|| t.session.title());
    DocInfo::read(t.session.doc(), &name)
}

/// Our stamps of `kinds` and other tools' in the active document, as button text.
fn found_text(s: &State, dialog: i32) -> String {
    let Some(t) = s.tab_ref() else {
        return String::new();
    };
    let kinds: &[Kind] = match dialog {
        WATERMARK => &[Kind::Watermark],
        HEADER => &[Kind::HeaderFooter],
        BATES => &[Kind::Bates],
        _ => return String::new(),
    };
    let found = pagebara_stamp::find(t.session.doc(), true);
    let pages: std::collections::BTreeSet<usize> = found
        .iter()
        .filter(|f| kinds.contains(&f.kind))
        .map(|f| f.page)
        .collect();
    if pages.is_empty() {
        return String::new();
    }
    let n = pages.len();
    match dialog {
        WATERMARK => trn!("watermarks ({n} page)", "watermarks ({n} pages)", n),
        HEADER => trn!(
            "headers and footers ({n} page)",
            "headers and footers ({n} pages)",
            n
        ),
        _ => trn!("Bates numbers ({n} page)", "Bates numbers ({n} pages)", n),
    }
}

fn kinds_of(dialog: i32) -> &'static [Kind] {
    match dialog {
        WATERMARK => &[Kind::Watermark],
        HEADER => &[Kind::HeaderFooter],
        _ => &[Kind::Bates],
    }
}

impl App {
    pub fn pro_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str, value: &str) {
        let g = ui.global::<ProState>();
        let dialog = g.get_dialog();
        match what {
            "open" => {
                if s.tabs.is_empty() {
                    return;
                }
                let d = value.parse::<i32>().unwrap_or(0);
                g.set_error("".into());
                g.set_status("".into());
                g.set_preview(Image::default());
                g.set_found(found_text(s, d).into());
                // Watermarks start red, page text black.
                g.set_colour(if d == WATERMARK { 5 } else { 6 });
                if d == WATERMARK {
                    load_presets(s, ui);
                    if let Some(spec) = s
                        .tab_ref()
                        .and_then(|t| pagebara_stamp::existing(t.session.doc(), Kind::Watermark))
                        .and_then(|j| serde_json::from_str::<Watermark>(&j).ok())
                    {
                        show_watermark(s, ui, &spec);
                    }
                }
                if d == HEADER
                    && let Some(spec) = s
                        .tab_ref()
                        .and_then(|t| pagebara_stamp::existing(t.session.doc(), Kind::HeaderFooter))
                        .and_then(|j| serde_json::from_str::<HeaderFooter>(&j).ok())
                {
                    show_header(ui, &spec);
                }
                if d == BATES {
                    s.pro.bates_files.clear();
                    self.pro_bates_files(s, ui);
                }
                if d == FLATTEN {
                    let c = s
                        .tab_ref()
                        .map(|t| pagebara_flatten::contents(t.session.doc()))
                        .unwrap_or_default();
                    g.set_fl_counts(
                        tr!(
                            "This document has {}, {}, {} and {}.",
                            comments(c.annotations),
                            trn!("{n} link", "{n} links", c.links),
                            trn!("{n} form field", "{n} form fields", c.fields),
                            layers(c.layers)
                        )
                        .into(),
                    );
                    g.set_fl_has(
                        Rc::new(VecModel::from(
                            [c.annotations, c.links, c.fields, c.layers]
                                .map(|n| i32::try_from(n).unwrap_or(i32::MAX))
                                .to_vec(),
                        ))
                        .into(),
                    );
                    g.set_fl_warning(if c.signed.is_empty() {
                        "".into()
                    } else {
                        tr!("This document is signed: flattening changes it, so the signatures will show as modified.")
                            .into()
                    });
                }
                if d == COMPARE {
                    g.set_cmp_file(
                        s.pro
                            .cmp_file
                            .as_deref()
                            .map(file_name)
                            .unwrap_or_default()
                            .into(),
                    );
                }
                g.set_dialog(d);
                self.pro_preview_later(s, ui);
            }
            "cancel" => {
                close(ui);
                s.pro.preview_timer = None;
            }
            "changed" => self.pro_preview_later(s, ui),
            "choose-file" => {
                let (title, filter): (String, (&str, &[&str])) = if g.get_wm_source() == 1 {
                    (
                        tr!("Choose a picture"),
                        (tr_noop!("Pictures"), &["png", "jpg", "jpeg"]),
                    )
                } else {
                    (tr!("Choose a PDF"), (tr_noop!("PDF documents"), &["pdf"]))
                };
                pick::open(self, &title, filter, |app, s, ui, p| {
                    ui.global::<ProState>().set_wm_file(file_name(&p).into());
                    s.pro.wm_file = Some(p);
                    app.pro_preview_later(s, ui);
                });
            }
            "choose-compare" => {
                pick::open(
                    self,
                    &tr!("Compare with"),
                    (tr_noop!("PDF documents"), &["pdf"]),
                    |_app, s, ui, p| {
                        ui.global::<ProState>().set_cmp_file(file_name(&p).into());
                        s.pro.cmp_file = Some(p);
                    },
                );
            }
            "save-preset" => match watermark_spec(s, ui) {
                Ok(spec) => {
                    let name = match &spec.source {
                        Source::Text { text, .. } => text.chars().take(40).collect(),
                        Source::Image { path, .. } | Source::Pdf { path, .. } => file_name(path),
                    };
                    s.pro.presets.retain(|p| p.name() != name);
                    s.pro.presets.push(Preset::Watermark {
                        name: name.clone(),
                        spec: Box::new(spec),
                    });
                    if let Some(p) = presets_path(s) {
                        if let Some(d) = p.parent() {
                            let _ = std::fs::create_dir_all(d);
                        }
                        if let Err(e) = std::fs::write(&p, Preset::to_json(&s.pro.presets)) {
                            g.set_error(tr!("Could not save the settings: {}", e).into());
                        }
                    }
                    load_presets(s, ui);
                    g.set_status(tr!("Saved as “{}”.", name).into());
                }
                Err(e) => g.set_error(e.into()),
            },
            "load-preset" => {
                let i = value.parse::<usize>().unwrap_or(0);
                let spec = s
                    .pro
                    .presets
                    .iter()
                    .filter_map(|p| match p {
                        Preset::Watermark { spec, .. } => Some(spec.as_ref().clone()),
                        _ => None,
                    })
                    .nth(i);
                if let Some(spec) = spec {
                    show_watermark(s, ui, &spec);
                    self.pro_preview_later(s, ui);
                }
            }
            "add-files" => {
                pick::open(
                    self,
                    &tr!("Add a file to number"),
                    (tr_noop!("PDF documents"), &["pdf"]),
                    |app, s, ui, p| {
                        s.pro.bates_files.push(p);
                        app.pro_bates_files(s, ui);
                    },
                );
            }
            "remove-file" => {
                let i = value.parse::<usize>().unwrap_or(0);
                if i >= 1 && i <= s.pro.bates_files.len() {
                    s.pro.bates_files.remove(i - 1);
                }
                self.pro_bates_files(s, ui);
            }
            "remove" => self.pro_remove(s, ui, dialog),
            "apply" => self.pro_apply(s, ui, dialog),
            _ => {}
        }
    }

    fn pro_bates_files(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let g = ui.global::<ProState>();
        let mut names: Vec<SharedString> = Vec::new();
        if let Some(t) = s.tab_ref() {
            names.push(t.session.title().into());
        }
        names.extend(s.pro.bates_files.iter().map(|p| file_name(p).into()));
        g.set_bates_files(ModelRc::new(VecModel::from(names)));
        let folder = s
            .tab_ref()
            .and_then(|t| t.session.path.clone())
            .or_else(|| s.pro.bates_files.first().cloned())
            .and_then(|p| p.parent().map(|d| d.join("Bates numbered")))
            .map(|d| d.display().to_string())
            .unwrap_or_default();
        g.set_bates_folder(folder.into());
    }

    /// Renders the preview shortly after the last change.
    pub fn pro_preview_later(self: &Rc<Self>, s: &mut State, _ui: &AppWindow) {
        let app = self.clone();
        let t = Timer::default();
        t.start(
            TimerMode::SingleShot,
            Duration::from_millis(120),
            move || {
                app.act(|app, s, ui| app.pro_preview(s, ui));
            },
        );
        s.pro.preview_timer = Some(t);
    }

    fn pro_preview(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let g = ui.global::<ProState>();
        let dialog = g.get_dialog();
        if !(WATERMARK..=BATES).contains(&dialog) {
            return;
        }
        let Some(t) = s.tab_ref() else { return };
        let n = t.session.page_count();
        let spec = match dialog {
            WATERMARK => watermark_spec(s, ui).map(Ok),
            _ => header_spec(ui, dialog == BATES).map(Err),
        };
        let spec = match spec {
            Ok(spec) => spec,
            Err(e) => {
                g.set_error(e.into());
                return;
            }
        };
        let set = match &spec {
            Ok(w) => &w.pages,
            Err(h) => &h.pages,
        };
        let list = match set.resolve(n) {
            Ok(p) => p,
            Err(e) => {
                g.set_error(e.to_string().into());
                return;
            }
        };
        // A first page left blank: the second shows what the others get.
        let skip = matches!(&spec, Err(h) if h.first.is_some()) && list.len() > 1;
        let (first, page) = (list[0], if skip { list[1] } else { list[0] });
        g.set_error("".into());
        let doc = t.session.doc().clone();
        let info = doc_info(s);
        s.pro.preview_serial += 1;
        let serial = s.pro.preview_serial;
        g.set_busy(true);
        pagebara_thread::spawn("stamp-preview", move || {
            let r = render_preview(doc, first, page, spec, &info);
            post_act(move |_app, s, ui| {
                if s.pro.preview_serial != serial {
                    return;
                }
                let g = ui.global::<ProState>();
                g.set_busy(false);
                match r {
                    Ok((w, h, px)) => {
                        g.set_preview(Image::from_rgba8_premultiplied(SharedPixelBuffer::<
                            Rgba8Pixel,
                        >::clone_from_slice(
                            &px, w, h
                        )));
                        g.set_error("".into());
                    }
                    Err(e) => g.set_error(e.into()),
                }
            });
        });
    }

    fn pro_remove(self: &Rc<Self>, s: &mut State, ui: &AppWindow, dialog: i32) {
        let kinds = kinds_of(dialog);
        let Some(t) = s.tab() else { return };
        let label = match dialog {
            WATERMARK => tr!("Remove watermarks"),
            HEADER => tr!("Remove headers and footers"),
            _ => tr!("Remove Bates numbers"),
        };
        match t
            .session
            .edit_text(&label, |d| pagebara_stamp::remove(d, kinds, true))
        {
            Ok((r, changed)) => {
                s.pro.last = format!("removed {}", r.removed);
                close(ui);
                self.after_edit(s, ui, changed.pages);
                self.message(
                    s,
                    &trn!(
                        "Removed from {n} page.",
                        "Removed from {n} pages.",
                        r.pages.len()
                    ),
                );
            }
            Err(e) => ui.global::<ProState>().set_error(e.to_string().into()),
        }
    }

    fn pro_apply(self: &Rc<Self>, s: &mut State, ui: &AppWindow, dialog: i32) {
        let g = ui.global::<ProState>();
        if dialog == COMPARE {
            return self.compare_start(s, ui);
        }
        let info = doc_info(s);
        let result: Result<(String, bool), String> = match dialog {
            WATERMARK => watermark_spec(s, ui).and_then(|spec| {
                let t = s.tab().ok_or("no document")?;
                let (r, c) = t
                    .session
                    .edit_text(&tr!("Add watermark"), |d| {
                        pagebara_stamp::apply_watermark(d, &spec)
                    })
                    .map_err(|e| e.to_string())?;
                Ok((stamped(&r, true), c.pages))
            }),
            HEADER => header_spec(ui, false).and_then(|spec| {
                let t = s.tab().ok_or("no document")?;
                let (r, c) = t
                    .session
                    .edit_text(&tr!("Add header and footer"), |d| {
                        pagebara_stamp::apply_header_footer(d, &spec, &info)
                    })
                    .map_err(|e| e.to_string())?;
                Ok((stamped(&r, false), c.pages))
            }),
            BATES => header_spec(ui, true).and_then(|spec| self.pro_bates(s, ui, spec, &info)),
            FLATTEN => {
                let opts = flatten_options(ui);
                let Some(t) = s.tab() else { return };
                t.session
                    // TRANSLATORS: the undo step that flattens comments, fields and layers
                    .edit_text(&tr!("Flatten"), |d| pagebara_flatten::flatten(d, &opts))
                    .map_err(|e| e.to_string())
                    .map(|(r, c)| {
                        let mut m = tr!(
                            "Flattened {}, {} and {}.",
                            comments(r.annotations),
                            trn!("{n} field", "{n} fields", r.fields),
                            layers(r.layers)
                        );
                        if r.transparent_areas > 0 {
                            m += " ";
                            m += &trn!(
                                "{n} transparent area rendered.",
                                "{n} transparent areas rendered.",
                                r.transparent_areas
                            );
                        }
                        for w in &r.warnings {
                            m += " ";
                            m += w;
                        }
                        (m, c.pages)
                    })
            }
            _ => return,
        };
        match result {
            Ok((msg, pages)) => {
                close(ui);
                s.pro.last = msg.clone();
                if let Some(t) = s.tab() {
                    t.search.invalidate();
                }
                self.after_edit(s, ui, pages);
                self.message(s, &msg);
            }
            Err(e) => g.set_error(e.into()),
        }
    }

    /// Numbers the open document, then the other files continuing, into a folder.
    fn pro_bates(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        spec: HeaderFooter,
        info: &DocInfo,
    ) -> Result<(String, bool), String> {
        let files = s.pro.bates_files.clone();
        let t = s.tab().ok_or("no document")?;
        let (r, c) = t
            .session
            .edit_text(&tr!("Bates numbering"), |d| {
                pagebara_stamp::apply_header_footer(d, &spec, info)
            })
            .map_err(|e| e.to_string())?;
        let b = spec.bates.clone().unwrap_or_default();
        let next = b.start + r.pages.len() as u64;
        let mine = format!(
            "{} – {}",
            b.format(b.start),
            b.format(next.saturating_sub(1))
        );
        if files.is_empty() {
            return Ok((tr!("Numbered {}.", mine), c.pages));
        }
        let base = t
            .session
            .path
            .clone()
            .or_else(|| files.first().cloned())
            .and_then(|p| p.parent().map(Path::to_owned))
            .unwrap_or_default();
        let folder = unique_dir(&base.join("Bates numbered")).map_err(|e| e.to_string())?;
        let title = t.session.title();
        let mut rest = spec.clone();
        rest.bates = Some(BatesNumber {
            start: next,
            ..b.clone()
        });
        ui.global::<ProState>().set_busy(true);
        let f2 = folder.clone();
        let row = format!(
            "{title},(this document),{},{},{}\n",
            r.pages.len(),
            b.format(b.start),
            b.format(next.saturating_sub(1))
        );
        let done_msg = mine.clone();
        pagebara_thread::spawn("bates", move || {
            let r = pagebara_stamp::bates_batch(
                &files,
                &rest,
                None,
                &|_, input, data| {
                    let p = f2.join(format!(
                        "{} (Bates).pdf",
                        input.file_stem().unwrap_or_default().to_string_lossy()
                    ));
                    std::fs::write(&p, data).map(|()| p)
                },
                &|_, _| {},
            );
            post_act(move |app, s, ui| {
                ui.global::<ProState>().set_busy(false);
                match r {
                    Ok(done) => {
                        let csv = pagebara_stamp::bates_csv(&done) + &row;
                        let _ = std::fs::write(folder.join("Bates ranges.csv"), csv);
                        let msg = trn!(
                            "Numbered {} here and {n} more file into {}.",
                            "Numbered {} here and {n} more files into {}.",
                            done.len(),
                            done_msg,
                            folder.display()
                        );
                        s.pro.last = msg.clone();
                        app.message(s, &msg);
                    }
                    Err(e) => {
                        app.show_message(s, ui, &tr!("Bates numbering failed"), &e.to_string())
                    }
                }
            });
        });
        Ok((
            tr!("Numbered {}; numbering the other files…", mine),
            c.pages,
        ))
    }
}

/// Closes the dialog; the keyboard goes back to the pages.
fn close(ui: &AppWindow) {
    ui.global::<ProState>().set_dialog(0);
    ui.invoke_focus_view();
}

fn unique_dir(base: &Path) -> std::io::Result<PathBuf> {
    for n in 1..1000 {
        let p = if n == 1 {
            base.to_owned()
        } else {
            PathBuf::from(format!("{} ({n})", base.display()))
        };
        match std::fs::create_dir(&p) {
            Ok(()) => return Ok(p),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free folder name"))
}

fn comments(n: usize) -> String {
    trn!("{n} comment", "{n} comments", n)
}

fn layers(n: usize) -> String {
    trn!("{n} layer", "{n} layers", n)
}

/// What a watermark (else a header and footer) did.
fn stamped(r: &pagebara_stamp::Report, watermark: bool) -> String {
    let n = r.pages.len();
    let mut m = if watermark {
        trn!(
            "Watermark added on {n} page.",
            "Watermark added on {n} pages.",
            n
        )
    } else {
        trn!(
            "Header and footer added on {n} page.",
            "Header and footer added on {n} pages.",
            n
        )
    };
    if !r.missing.is_empty() {
        m += " ";
        m += &tr!(
            "No font has {}.",
            r.missing.iter().take(10).collect::<String>()
        );
    }
    m
}

/// Puts a saved watermark's settings into the dialog.
fn show_watermark(s: &mut State, ui: &AppWindow, w: &Watermark) {
    let g = ui.global::<ProState>();
    match &w.source {
        Source::Text { text, style } => {
            g.set_wm_source(0);
            g.set_wm_text(text.into());
            if let Some(i) = FONTS.iter().position(|f| f.1 == style.font) {
                g.set_font(i as i32);
            }
            if let Some(i) = COLOURS.iter().position(|c| {
                c.iter()
                    .zip(style.color.0)
                    .all(|(a, b)| (a - b).abs() < 0.02)
            }) {
                g.set_colour(i as i32);
            }
        }
        Source::Image { path, .. } | Source::Pdf { path, .. } => {
            g.set_wm_source(if matches!(w.source, Source::Image { .. }) {
                1
            } else {
                2
            });
            if let Source::Pdf { page, .. } = &w.source {
                g.set_wm_page(page.to_string().into());
            }
            g.set_wm_file(file_name(path).into());
            s.pro.wm_file = Some(path.clone());
        }
    }
    if let Size::Fit(f) = w.size {
        g.set_wm_size(if w.tiling.is_some() { f / 0.4 } else { f } * 100.0);
    }
    g.set_wm_opacity(w.opacity * 100.0);
    g.set_wm_rotation(match w.rotation.round() as i32 {
        45 => 1,
        90 => 2,
        -45 | 315 => 3,
        _ => 0,
    });
    g.set_wm_anchor(Anchor::ALL.iter().position(|a| *a == w.anchor).unwrap_or(4) as i32);
    g.set_wm_tiled(w.tiling.is_some());
    g.set_wm_behind(w.layer == Layer::Behind);
    g.set_wm_visibility(match (&w.group, w.visibility) {
        (None, _) => 3,
        (_, Visibility::ViewOnly) => 1,
        (_, Visibility::PrintOnly) => 2,
        _ => 0,
    });
}

fn show_header(ui: &AppWindow, h: &HeaderFooter) {
    let g = ui.global::<ProState>();
    let [tl, tc, tr, bl, bc, br] = h.slots.0.clone().map(SharedString::from);
    g.set_hf_tl(tl);
    g.set_hf_tc(tc);
    g.set_hf_tr(tr);
    g.set_hf_bl(bl);
    g.set_hf_bc(bc);
    g.set_hf_br(br);
    g.set_hf_size(h.style.size);
    g.set_hf_start(h.start.to_string().into());
    g.set_hf_skip_first(h.first.as_ref().is_some_and(Slots::is_empty));
    if let Some(i) = FONTS.iter().position(|f| f.1 == h.style.font) {
        g.set_font(i as i32);
    }
}

/// The page with the stamp applied to a copy of the document, at preview size.
fn render_preview(
    mut doc: Document,
    first: usize,
    page: usize,
    spec: Result<Watermark, HeaderFooter>,
    info: &DocInfo,
) -> Result<(u32, u32, Vec<u8>), String> {
    let only = PageSet::list(&(page + 1).to_string());
    match spec {
        Ok(mut w) => {
            w.pages = only;
            pagebara_stamp::apply_watermark(&mut doc, &w).map_err(|e| e.to_string())?;
        }
        Err(mut h) => {
            // Numbered as the page will be when all pages are stamped.
            let offset = page - first;
            h.start += offset as i64;
            if let Some(b) = h.bates.as_mut() {
                b.start += offset as u64;
            }
            if page != first {
                h.first = None;
            }
            h.pages = only;
            pagebara_stamp::apply_header_footer(&mut doc, &h, info).map_err(|e| e.to_string())?;
        }
    }
    let r = pagebara_render::Renderer::new(doc);
    let (w, h) = r.page_size(page).ok_or("no page")?;
    let scale = 420.0 / w.max(h);
    let rect = pagebara_render::PixelRect {
        x: 0,
        y: 0,
        w: (w * scale).round().max(1.0) as u32,
        h: (h * scale).round().max(1.0) as u32,
    };
    let req = pagebara_render::RenderRequest::new(scale, rect);
    let img = r.render(page, &req).map_err(|e| e.to_string())?;
    Ok((img.width, img.height, img.data))
}
