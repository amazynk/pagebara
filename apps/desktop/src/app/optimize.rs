//! Optimize: the options of the dialog, the result worked out live on a worker (sizes
//! per category, a warning for signed documents), and the smaller file saved.

use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use pagebara_app_core::session::write_atomic;
use pagebara_i18n::{bytes as bytes_text, decimal, tr};
use pagebara_optimize::{Effort, Options, Preset};
use slint::{ComponentHandle, ModelRc, VecModel};

use super::{App, State, pick};
use crate::{AppState, AppWindow, DialogKind, OptimizeState, SizeRow};

#[derive(Default)]
pub struct Optimize {
    /// Stops the estimate that is running.
    cancel: Arc<AtomicBool>,
    /// Counts estimates, so a late result of an old one is dropped.
    generation: u64,
    /// The last estimate: options, file and report.
    pub last: Option<(Options, Arc<Vec<u8>>, pagebara_optimize::Report)>,
    /// The file written last, for scripts.
    pub saved: Option<std::path::PathBuf>,
}

const RESOLUTIONS: [Option<f32>; 5] = [None, Some(300.0), Some(150.0), Some(100.0), Some(72.0)];
const QUALITIES: [u8; 4] = [90, 80, 65, 50];

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let a = app.clone();
    ui.global::<OptimizeState>().on_action(move |what| {
        let what = what.to_string();
        a.act(move |app, s, ui| app.optimize_action(s, ui, &what))
    });
}

/// The options the dialog shows.
fn options_of(ui: &AppWindow) -> Options {
    let g = ui.global::<OptimizeState>();
    let mut o = Preset::Lossless.options();
    o.images.target_ppi = RESOLUTIONS[g.get_resolution().clamp(0, 4) as usize];
    o.images.jpeg_quality = QUALITIES[g.get_quality().clamp(0, 3) as usize];
    o.images.recompress_jpeg = g.get_preset() == 2;
    o.strip.metadata = g.get_strip_metadata();
    o.strip.thumbnails = g.get_strip_thumbnails();
    o.strip.piece_info = g.get_strip_private();
    o.strip.javascript = g.get_strip_scripts();
    o.strip.embedded_files = g.get_strip_attachments();
    o.strip.annotations = g.get_strip_comments();
    o.grayscale = g.get_grayscale();
    o.linearize = g.get_linearize();
    o.effort = if g.get_maximum() {
        Effort::Maximum
    } else {
        Effort::Best
    };
    o
}

/// Shows the options of a preset in the dialog.
fn show_preset(ui: &AppWindow, p: Preset) {
    let g = ui.global::<OptimizeState>();
    let o = p.options();
    let res = RESOLUTIONS
        .iter()
        .position(|r| *r == o.images.target_ppi)
        .unwrap_or(0);
    g.set_resolution(res as i32);
    let q = QUALITIES
        .iter()
        .position(|q| *q == o.images.jpeg_quality)
        .unwrap_or(1);
    g.set_quality(q as i32);
    g.set_strip_metadata(o.strip.metadata);
    g.set_strip_thumbnails(o.strip.thumbnails);
    g.set_strip_private(o.strip.piece_info);
    g.set_strip_scripts(o.strip.javascript);
    g.set_strip_attachments(o.strip.embedded_files);
    g.set_strip_comments(o.strip.annotations);
    g.set_grayscale(false);
}

impl App {
    pub fn optimize_open(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let g = ui.global::<OptimizeState>();
        g.set_preset(0);
        show_preset(ui, Preset::Lossless);
        g.set_linearize(false);
        g.set_maximum(false);
        let signed = s
            .tab_ref()
            .is_some_and(|t| pagebara_optimize::is_signed(t.session.doc()));
        g.set_signed(signed);
        self.optimize_estimate(s, ui);
    }

    fn optimize_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str) {
        let g = ui.global::<OptimizeState>();
        match what {
            "preset" => {
                let p = Preset::ALL[g.get_preset().clamp(0, 3) as usize];
                if p != Preset::Custom {
                    show_preset(ui, p);
                }
                self.optimize_estimate(s, ui);
            }
            "changed" => {
                g.set_preset(3);
                self.optimize_estimate(s, ui);
            }
            "save" => self.optimize_save(s, ui),
            _ => {}
        }
    }

    /// Works out the result of the dialog's options on a worker.
    pub fn optimize_estimate(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab_ref() else { return };
        let doc = t.session.doc().clone();
        let opts = options_of(ui);
        s.optimize.cancel.store(true, Ordering::Relaxed);
        let cancel = Arc::new(AtomicBool::new(false));
        s.optimize.cancel = cancel.clone();
        s.optimize.generation += 1;
        let generation = s.optimize.generation;
        let g = ui.global::<OptimizeState>();
        g.set_busy(true);
        g.set_before(bytes_text(doc.source().len() as u64).into());
        pagebara_thread::spawn("optimize", move || {
            let r = pagebara_optimize::optimize(&doc, &opts, Some(&cancel));
            super::post_act(move |_app, s, ui| {
                if s.optimize.generation != generation {
                    return;
                }
                let g = ui.global::<OptimizeState>();
                g.set_busy(false);
                match r {
                    Ok(out) => {
                        let rep = &out.report;
                        g.set_after(bytes_text(rep.after).into());
                        let pct = rep.saved_percent();
                        g.set_saved(
                            if pct >= 0.5 {
                                tr!("{}% smaller", decimal(pct, 0))
                            } else if pct <= -0.5 {
                                tr!("{}% larger", decimal(-pct, 0))
                            } else {
                                tr!("about the same")
                            }
                            .into(),
                        );
                        let rows: Vec<SizeRow> = rep
                            .categories
                            .iter()
                            .map(|c| SizeRow {
                                label: c.category.title().into(),
                                before: bytes_text(c.before).into(),
                                after: bytes_text(c.after).into(),
                            })
                            .collect();
                        g.set_rows(ModelRc::new(VecModel::from(rows)));
                        let mut note = Vec::new();
                        if !opts.changes_appearance() {
                            note.push(tr!("Pages will look exactly as they do now."));
                        }
                        note.extend(
                            rep.warnings
                                .iter()
                                .filter(|w| !w.contains("signed"))
                                .cloned(),
                        );
                        g.set_note(note.join(" ").into());
                        s.optimize.last = Some((opts, Arc::new(out.bytes), out.report));
                    }
                    Err(pagebara_optimize::Error::Cancelled) => {}
                    Err(e) => {
                        ui.global::<AppState>()
                            .set_dialog_error(e.to_string().into());
                    }
                }
            });
        });
    }

    fn optimize_save(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab_ref() else { return };
        let opts = options_of(ui);
        let ready = s
            .optimize
            .last
            .as_ref()
            .filter(|l| l.0 == opts)
            .map(|l| l.1.clone());
        let Some(bytes) = ready else {
            return self.optimize_estimate(s, ui);
        };
        let before = t.session.doc().source().len() as u64;
        let name = pick::derived_name(&t.session.title(), "optimized");
        pick::save_pdf(
            self,
            &tr!("Save the optimized file"),
            t.session.path.clone(),
            &name,
            move |app, s, ui, out| match write_atomic(&out, &bytes) {
                Ok(()) => {
                    ui.global::<AppState>().set_dialog(DialogKind::None);
                    s.optimize.saved = Some(out.clone());
                    let file = out
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    app.message(
                        s,
                        &tr!(
                            "Saved {}: {} → {}",
                            file,
                            bytes_text(before),
                            bytes_text(bytes.len() as u64)
                        ),
                    );
                }
                Err(e) => ui
                    .global::<AppState>()
                    .set_dialog_error(tr!("The file could not be written: {}", e).into()),
            },
        );
    }
}
