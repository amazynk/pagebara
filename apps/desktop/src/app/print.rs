//! The print dialog: choices, a live preview of the first sheets, and the print job,
//! rendered into a PDF in the temp folder and handed to the system (see
//! `pagebara_app_core::print` for why sheets are rendered as images).

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pagebara_app_core::print::system::{Backend, Job, Printer};
use pagebara_app_core::print::{
    self, Duplex, PageRange, Paper, PrintOptions, Renderer, Scale, place, prepare_document,
    render_sheet,
};
use pagebara_i18n::{tr, trn};
use slint::{
    ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, SharedString, VecModel,
};

use super::{App, State, post_act};
use crate::{AppWindow, PrintState};

/// Sheets in the preview.
const PREVIEW_SHEETS: usize = 3;
/// The preview's sheet height in device pixels.
const PREVIEW_PX: f64 = 360.0;
const QUALITY_DPI: [u32; 3] = [150, 300, 600];

#[derive(Debug, Default)]
pub struct Print {
    backend: Option<Backend>,
    pub printers: Vec<Printer>,
    preview_serial: u64,
    /// The running job's cancel flag.
    job: Option<Arc<AtomicBool>>,
    /// The last print file, removed when the next is made.
    last_file: Option<PathBuf>,
    /// What the last job did, for tests: the file and the system's answer.
    pub last_result: Option<(PathBuf, usize, String)>,
    paper_set: bool,
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let ps = ui.global::<PrintState>();
    macro_rules! on {
        ($cb:ident, |$app:ident, $s:ident, $ui:ident| $body:expr) => {{
            let a = app.clone();
            ps.$cb(move || {
                a.act(move |$app, $s, $ui| {
                    let _ = (&$app, &$ui);
                    $body
                })
            });
        }};
    }
    on!(on_open, |app, s, ui| open(app, s, ui));
    on!(on_changed, |app, s, ui| preview(app, s, ui));
    on!(on_print, |app, s, ui| start(app, s, ui, None));
    on!(on_save_pdf, |app, s, ui| pick_save(app, s, ui));
    on!(on_close, |app, s, ui| close(app, s, ui));
}

/// Ctrl+P opens the dialog; while it is open, Esc closes it and other keys stay in it.
pub fn shortcut(app: &Rc<App>, s: &mut State, ui: &AppWindow, c: char, ctrl: bool) -> Option<bool> {
    let visible = ui.global::<PrintState>().get_visible();
    if visible {
        if c == char::from(slint::platform::Key::Escape) {
            close(app, s, ui);
            return Some(true);
        }
        return Some(false);
    }
    if ctrl && c.eq_ignore_ascii_case(&'p') && !s.tabs.is_empty() {
        open(app, s, ui);
        return Some(true);
    }
    None
}

pub fn open(app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    let ps = ui.global::<PrintState>();
    let backend = s.print.backend.get_or_insert_with(Backend::detect).clone();
    if !s.print.paper_set {
        s.print.paper_set = true;
        let locale = ["LC_ALL", "LC_PAPER", "LANG"]
            .iter()
            .find_map(|v| std::env::var(v).ok().filter(|s| !s.is_empty()))
            .unwrap_or_default();
        let paper = Paper::for_locale(&locale);
        ps.set_paper(Paper::ALL.iter().position(|p| *p == paper).unwrap_or(0) as i32);
        let labels: Vec<SharedString> = Paper::ALL.iter().map(|p| p.label().into()).collect();
        ps.set_papers(ModelRc::new(VecModel::from(labels)));
    }
    let Some(t) = s.tab_ref() else { return };
    let n = t.session.page_count();
    let selected = !t.grid.selected.is_empty();
    if !t.session.doc().permissions().print {
        app.message(
            s,
            &tr!("The author of this document does not allow printing it."),
        );
        return;
    }
    ps.set_range_text(format!("1-{n}").into());
    ps.set_has_selection(selected);
    if ps.get_range() == 3 && !selected {
        ps.set_range(0);
    }
    ps.set_error(SharedString::default());
    ps.set_status(SharedString::default());
    ps.set_busy(false);
    ps.set_duplex_available(backend.takes_options());
    // Printers are listed off the UI thread: `lpstat` can be slow to answer.
    ps.set_note(tr!("Looking for printers…").into());
    ps.set_can_print(false);
    pagebara_thread::spawn("printers", move || {
        let found = backend.printers();
        post_act(move |_app, s, ui| {
            let ps = ui.global::<PrintState>();
            let (printers, note) = match found {
                Ok(p) if p.is_empty() => (
                    p,
                    match s.print.backend {
                        Some(Backend::None) => {
                            tr!("No print system found. You can save a PDF for printing.")
                        }
                        _ => tr!("No printers are set up. You can save a PDF for printing."),
                    },
                ),
                Ok(p) => (p, String::new()),
                Err(e) => (Vec::new(), e),
            };
            let names: Vec<SharedString> = printers
                .iter()
                .map(|p| {
                    let name = if p.description.is_empty() {
                        &p.name
                    } else {
                        &p.description
                    };
                    if p.enabled {
                        name.as_str().into()
                    } else {
                        tr!("{} (paused)", name).into()
                    }
                })
                .collect();
            ps.set_can_print(!names.is_empty());
            ps.set_printers(ModelRc::new(VecModel::from(names)));
            ps.set_printer(0);
            ps.set_note(note.into());
            s.print.printers = printers;
        });
    });
    ps.set_visible(true);
    preview(app, s, ui);
}

fn close(_app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    if let Some(c) = s.print.job.take() {
        c.store(true, Ordering::Relaxed);
    }
    s.print.preview_serial += 1;
    let ps = ui.global::<PrintState>();
    ps.set_visible(false);
    ps.set_previews(ModelRc::default());
    ui.invoke_focus_view();
}

/// The options chosen in the dialog and the pages they select.
fn options(s: &State, ui: &AppWindow) -> Result<(PrintOptions, Vec<usize>), String> {
    let t = s.tab_ref().ok_or_else(|| tr!("No document is open."))?;
    let ps = ui.global::<PrintState>();
    let range = match ps.get_range() {
        1 => PageRange::Current(t.view.current_page),
        2 => PageRange::Ranges(ps.get_range_text().to_string()),
        3 => PageRange::Pages(t.grid.selected.iter().copied().collect()),
        _ => PageRange::All,
    };
    let opts = PrintOptions {
        copies: ps.get_copies().clamp(1, 999) as u32,
        range,
        scale: match ps.get_scale() {
            1 => Scale::Actual,
            2 => Scale::Custom(f64::from(ps.get_custom_scale())),
            _ => Scale::Fit,
        },
        paper: Paper::ALL
            .get(ps.get_paper().max(0) as usize)
            .copied()
            .unwrap_or_default(),
        auto_rotate: ps.get_auto_rotate(),
        centre: ps.get_centre(),
        annotations: ps.get_annotations(),
        forms: ps.get_forms(),
        greyscale: ps.get_greyscale(),
        duplex: match ps.get_duplex() {
            1 => Duplex::LongEdge,
            2 => Duplex::ShortEdge,
            _ => Duplex::Off,
        },
        dpi: QUALITY_DPI[ps.get_quality().clamp(0, 2) as usize],
    };
    let pages = print::select_pages(&opts.range, t.session.page_count())?;
    Ok((opts, pages))
}

/// What a print thread needs: the document as printed, by its page refs.
fn renderer(s: &State, opts: &PrintOptions) -> Option<Arc<Renderer>> {
    let t = s.tab_ref()?;
    let doc = prepare_document(
        t.session.doc(),
        &t.session.tree.refs(),
        opts.annotations,
        opts.forms,
    );
    Some(Arc::new(Renderer::new(doc)))
}

fn page_size(r: &Renderer, page: usize) -> (f64, f64) {
    r.page_size(page)
        .map_or((612.0, 792.0), |(w, h)| (f64::from(w), f64::from(h)))
}

/// Renders the first sheets small, in the background.
fn preview(_app: &Rc<App>, s: &mut State, ui: &AppWindow) {
    s.print.preview_serial += 1;
    let serial = s.print.preview_serial;
    let ps = ui.global::<PrintState>();
    let (opts, pages) = match options(s, ui) {
        Ok(v) => v,
        Err(e) => {
            ps.set_error(e.into());
            ps.set_previews(ModelRc::default());
            ps.set_summary(SharedString::default());
            return;
        }
    };
    ps.set_error(SharedString::default());
    let sheets = pages.len();
    let copies = opts.copies;
    ps.set_summary(
        if copies > 1 {
            trn!(
                "{n} sheet × {} copies",
                "{n} sheets × {} copies",
                sheets,
                copies
            )
        } else {
            trn!("{n} sheet", "{n} sheets", sheets)
        }
        .into(),
    );
    // The document is prepared on the worker as well; only clone what it needs here.
    let Some(t) = s.tab_ref() else { return };
    let doc = t.session.doc().clone();
    let refs = t.session.tree.refs();
    pagebara_thread::spawn("print-preview", move || {
        let r = Renderer::new(prepare_document(&doc, &refs, opts.annotations, opts.forms));
        let mut out = Vec::new();
        for &page in pages.iter().take(PREVIEW_SHEETS) {
            let placed = place(page_size(&r, page), &opts);
            let dpi = (PREVIEW_PX / (placed.sheet.1 / 72.0)).round() as u32;
            match render_sheet(&r, page, &placed, dpi, opts.greyscale, None) {
                Ok(sheet) => out.push((sheet.width, sheet.height, sheet.to_rgba())),
                Err(e) => log::warn!("print preview: {e}"),
            }
        }
        post_act(move |_app, s, ui| {
            if s.print.preview_serial != serial {
                return;
            }
            let images: Vec<Image> = out
                .into_iter()
                .map(|(w, h, px)| {
                    Image::from_rgba8_premultiplied(
                        SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&px, w, h),
                    )
                })
                .collect();
            ui.global::<PrintState>()
                .set_previews(ModelRc::new(VecModel::from(images)));
        });
    });
}

fn pick_save(app: &Rc<App>, s: &mut State, _ui: &AppWindow) {
    let name = s
        .tab_ref()
        .map(|t| format!("{} (print).pdf", t.session.title()))
        .unwrap_or_else(|| "print.pdf".into());
    let app = app.clone();
    let task = slint::spawn_local(async move {
        let picked = rfd::AsyncFileDialog::new()
            .set_title(tr!("Save as PDF for printing"))
            .set_file_name(name)
            .add_filter(tr!("PDF documents"), &["pdf"])
            .save_file()
            .await;
        if let Some(f) = picked {
            let path = f.path().to_owned();
            app.act(move |app, s, ui| start(app, s, ui, Some(path)));
        }
    });
    if let Err(e) = task {
        log::warn!("cannot show the file dialog: {e}");
    }
}

/// Makes the print file and prints it, or (with `save_to`) saves it there.
pub fn start(_app: &Rc<App>, s: &mut State, ui: &AppWindow, save_to: Option<PathBuf>) {
    let ps = ui.global::<PrintState>();
    if s.print.job.is_some() {
        return;
    }
    let (opts, pages) = match options(s, ui) {
        Ok(v) => v,
        Err(e) => {
            ps.set_error(e.into());
            return;
        }
    };
    let backend = s.print.backend.clone().unwrap_or(Backend::None);
    let printer = if save_to.is_none() {
        match s.print.printers.get(ps.get_printer().max(0) as usize) {
            Some(p) => Some(p.name.clone()),
            None => {
                ps.set_error(tr!("Choose a printer first.").into());
                return;
            }
        }
    } else {
        None
    };
    let Some(r) = renderer(s, &opts) else { return };
    let title = s.tab_ref().map(|t| t.session.title()).unwrap_or_default();
    // Copies the system cannot make go into the file.
    let in_file = if printer.is_some() && backend.takes_options() {
        1
    } else {
        opts.copies.max(1) as usize
    };
    let sheets: Vec<usize> = (0..in_file).flat_map(|_| pages.iter().copied()).collect();
    let file = match &save_to {
        Some(p) => p.clone(),
        None => {
            if let Some(old) = s.print.last_file.take() {
                let _ = std::fs::remove_file(old);
            }
            let f = std::env::temp_dir().join(format!(
                "pagebara-print-{}-{}.pdf",
                std::process::id(),
                s.print.preview_serial
            ));
            s.print.last_file = Some(f.clone());
            f
        }
    };
    let cancel = Arc::new(AtomicBool::new(false));
    s.print.job = Some(cancel.clone());
    ps.set_busy(true);
    ps.set_error(SharedString::default());
    ps.set_status(tr!("Preparing…").into());
    pagebara_thread::spawn("print", move || {
        let total = sheets.len();
        let written = print::write_pdf(&file, total, |i| {
            if cancel.load(Ordering::Relaxed) {
                return Err("Cancelled".into());
            }
            post_act(move |_app, _s, ui| {
                ui.global::<PrintState>()
                    .set_status(tr!("Preparing page {} of {}…", i + 1, total).into());
            });
            let page = sheets[i];
            let placed = place(page_size(&r, page), &opts);
            render_sheet(&r, page, &placed, opts.dpi, opts.greyscale, Some(&cancel))
        });
        let result = written.and_then(|doc| match &printer {
            None => Ok((doc, tr!("Saved {}", doc_name(&file)))),
            Some(name) => {
                let job = Job {
                    printer: name.clone(),
                    file: doc.path.clone(),
                    title,
                    copies: if in_file > 1 { 1 } else { opts.copies },
                    duplex: opts.duplex,
                    paper: opts.paper,
                    collate: true,
                };
                backend.submit(&job).map(|said| {
                    let pages = doc.pages;
                    let msg = if said.is_empty() {
                        trn!("Sent {n} page to {}", "Sent {n} pages to {}", pages, name)
                    } else {
                        trn!(
                            "Sent {n} page to {} ({})",
                            "Sent {n} pages to {} ({})",
                            pages,
                            name,
                            said
                        )
                    };
                    (doc, msg)
                })
            }
        });
        post_act(move |app, s, ui| {
            s.print.job = None;
            let ps = ui.global::<PrintState>();
            ps.set_busy(false);
            match result {
                Ok((doc, msg)) => {
                    log::info!("print: {msg}");
                    s.print.last_result = Some((doc.path, doc.pages, msg.clone()));
                    ps.set_status(SharedString::default());
                    close(app, s, ui);
                    app.message(s, &format!("{msg}."));
                }
                Err(e) => {
                    log::warn!("print: {e}");
                    ps.set_status(SharedString::default());
                    if e != "Cancelled" {
                        ps.set_error(e.into());
                    }
                }
            }
        });
    });
}

fn doc_name(p: &std::path::Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}
