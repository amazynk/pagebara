//! The comparison view: the active document (old) against a chosen file (new), compared
//! on a worker; the change list with filters, and the paired pages rendered side by side
//! with the changes marked. Choosing a change shows its pages; both sides always show a
//! pair, so they scroll together.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use pagebara_compare::{Category, Change, ChangeKind, Comparison, Options, Stage};
use pagebara_i18n::{decimal, tr, trn};
use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;
use slint::{ComponentHandle, Image, ModelRc, Rgba8Pixel, SharedPixelBuffer, VecModel};

use super::{App, State, pick, post_act};
use crate::{AppWindow, CompareMark, CompareRow, CompareState, ProState};

/// Rendered pages are this tall.
const PAGE_PX: f32 = 1100.0;

pub struct Compared {
    pub cmp: Comparison,
    old: Arc<Renderer>,
    new: Arc<Renderer>,
    old_doc: Document,
    new_doc: Document,
    old_name: String,
    new_name: String,
    /// Indices into `cmp.changes` of the rows shown.
    pub rows: Vec<usize>,
    pub pair: usize,
    /// Counts renders; only the latest is shown.
    serial: u64,
}

#[derive(Default)]
pub struct CompareUi {
    pub data: Option<Compared>,
    cancel: Option<Arc<AtomicBool>>,
}

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let a = app.clone();
    ui.global::<CompareState>().on_action(move |what, value| {
        let (what, value) = (what.to_string(), value.to_string());
        a.act(move |app, s, ui| app.compare_action(s, ui, &what, &value))
    });
}

fn category_index(c: Category) -> i32 {
    match c {
        Category::Text => 0,
        Category::Visual => 1,
        Category::Annotations => 2,
        Category::Document => 3,
    }
}

fn at(c: &Change) -> String {
    match (c.page_a, c.page_b) {
        (Some(x), Some(y)) if x == y => tr!("p. {}", x + 1),
        (Some(x), Some(y)) => tr!("p. {} → {}", x + 1, y + 1),
        (Some(x), None) => tr!("old p. {}", x + 1),
        (None, Some(y)) => tr!("new p. {}", y + 1),
        // TRANSLATORS: where a change is, for changes to the whole document
        _ => tr!("document"),
    }
}

fn mark_kind(k: ChangeKind, new_side: bool) -> i32 {
    match k {
        ChangeKind::Deleted | ChangeKind::PageDeleted => 0,
        ChangeKind::Inserted | ChangeKind::PageInserted => 1,
        ChangeKind::Replaced => i32::from(new_side),
        ChangeKind::Moved => 2,
        ChangeKind::Visual => 3,
        _ => 4,
    }
}

fn render(r: &Renderer, page: usize) -> Option<(u32, u32, Vec<u8>)> {
    let (w, h) = r.page_size(page)?;
    let scale = PAGE_PX / h.max(w);
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: (w * scale).round().max(1.0) as u32,
        h: (h * scale).round().max(1.0) as u32,
    };
    let img = r.render(page, &RenderRequest::new(scale, rect)).ok()?;
    Some((img.width, img.height, img.data))
}

fn image(p: Option<(u32, u32, Vec<u8>)>) -> Image {
    match p {
        Some((w, h, px)) => Image::from_rgba8_premultiplied(
            SharedPixelBuffer::<Rgba8Pixel>::clone_from_slice(&px, w, h),
        ),
        None => Image::default(),
    }
}

impl Compared {
    fn shown(&self, ui: &AppWindow) -> Vec<usize> {
        let g = ui.global::<CompareState>();
        let on = [
            g.get_show_text(),
            g.get_show_visual(),
            g.get_show_annots(),
            g.get_show_doc(),
        ];
        (0..self.cmp.changes.len())
            .filter(|&i| on[category_index(self.cmp.changes[i].kind.category()) as usize])
            .collect()
    }

    fn marks(
        &self,
        ui: &AppWindow,
        side: usize,
        page: usize,
        current: Option<usize>,
    ) -> Vec<CompareMark> {
        let r = if side == 0 { &self.old } else { &self.new };
        let Some((w, h)) = r.page_size(page) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for &i in &self.shown(ui) {
            let c = &self.cmp.changes[i];
            let rects = if side == 0 { &c.rects_a } else { &c.rects_b };
            for (p, q) in rects {
                if *p != page {
                    continue;
                }
                out.push(CompareMark {
                    x: q[0] / w,
                    y: q[1] / h,
                    w: (q[2] - q[0]) / w,
                    h: (q[3] - q[1]) / h,
                    kind: mark_kind(c.kind, side == 1),
                    current: current == Some(i),
                });
            }
        }
        out
    }
}

impl App {
    /// Starts comparing the active document with the chosen file.
    pub fn compare_start(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let pg = ui.global::<ProState>();
        let Some(path) = s.pro.cmp_file.clone() else {
            pg.set_error(tr!("Choose the file to compare with.").into());
            return;
        };
        let Some(t) = s.tab_ref() else { return };
        let old_doc = t.session.doc().clone();
        let old_name = t.session.title();
        let opts = Options {
            text: pg.get_cmp_text(),
            visual: pg.get_cmp_visual(),
            annotations: pg.get_cmp_annots(),
            metadata: pg.get_cmp_annots(),
            forms: pg.get_cmp_annots(),
            ..Options::default()
        };
        pg.set_dialog(0);
        let g = ui.global::<CompareState>();
        g.set_visible(true);
        g.set_busy(true);
        g.set_progress(0.0);
        g.set_status(tr!("Reading the text…").into());
        g.set_old_name(old_name.clone().into());
        g.set_rows(ModelRc::new(VecModel::<CompareRow>::default()));
        g.set_left(Image::default());
        g.set_right(Image::default());
        let new_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        g.set_new_name(new_name.clone().into());
        let cancel = Arc::new(AtomicBool::new(false));
        s.compare.cancel = Some(cancel.clone());
        s.compare.data = None;
        pagebara_thread::spawn("compare", move || {
            let result = std::fs::read(&path)
                .map_err(|e| e.to_string())
                .and_then(|b| Document::open(b).map_err(|e| e.to_string()))
                .map(|new_doc| {
                    let progress = |stage: Stage, done: usize, total: usize| {
                        let (f, text) = match stage {
                            Stage::Text => (0.1, tr!("Comparing the text…")),
                            Stage::Visual => (
                                0.2 + 0.8 * done as f32 / total.max(1) as f32,
                                tr!("Comparing how pages look: {} of {}", done, total),
                            ),
                        };
                        post_act(move |_app, _s, ui| {
                            let g = ui.global::<CompareState>();
                            g.set_progress(f);
                            g.set_status(text.into());
                        });
                    };
                    let cmp = pagebara_compare::compare(
                        &old_doc,
                        &new_doc,
                        &opts,
                        &progress,
                        Some(&cancel),
                    );
                    (cmp, old_doc, new_doc)
                });
            post_act(move |app, s, ui| app.compare_done(s, ui, result, old_name, new_name));
        });
    }

    fn compare_done(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        result: Result<(Comparison, Document, Document), String>,
        old_name: String,
        new_name: String,
    ) {
        let g = ui.global::<CompareState>();
        g.set_busy(false);
        s.compare.cancel = None;
        if !g.get_visible() {
            return;
        }
        let (cmp, old_doc, new_doc) = match result {
            Ok(r) => r,
            Err(e) => {
                g.set_visible(false);
                return self.show_message(s, ui, &tr!("Could not compare"), &e);
            }
        };
        let sm = &cmp.summary;
        g.set_summary(
            trn!(
                "{n} change: {} words inserted, {} deleted, {} moved · {} pages inserted, {} removed, {} looking different · {} s",
                "{n} changes: {} words inserted, {} deleted, {} moved · {} pages inserted, {} removed, {} looking different · {} s",
                sm.changes,
                sm.words_inserted,
                sm.words_deleted,
                sm.words_moved,
                sm.pages_inserted,
                sm.pages_deleted,
                sm.pages_looking_different,
                decimal(cmp.timing.total_ms as f64 / 1000.0, 1)
            )
            .into(),
        );
        s.pro.last = format!("{} changes", sm.changes);
        s.compare.data = Some(Compared {
            old: Arc::new(Renderer::new(old_doc.clone())),
            new: Arc::new(Renderer::new(new_doc.clone())),
            old_doc,
            new_doc,
            cmp,
            old_name,
            new_name,
            rows: Vec::new(),
            pair: 0,
            serial: 0,
        });
        self.compare_rows(s, ui);
        let first = s
            .compare
            .data
            .as_ref()
            .and_then(|d| d.rows.first().copied());
        match first {
            Some(_) => self.compare_select(s, ui, 0),
            None => self.compare_show_pair(s, ui, 0),
        }
    }

    fn compare_rows(&self, s: &mut State, ui: &AppWindow) {
        let Some(d) = s.compare.data.as_mut() else {
            return;
        };
        d.rows = d.shown(ui);
        let rows: Vec<CompareRow> = d
            .rows
            .iter()
            .map(|&i| {
                let c = &d.cmp.changes[i];
                CompareRow {
                    kind: c.kind.label().into(),
                    category: category_index(c.kind.category()),
                    at: at(c).into(),
                    text: c.description.clone().into(),
                }
            })
            .collect();
        let g = ui.global::<CompareState>();
        g.set_rows(ModelRc::new(VecModel::from(rows)));
        g.set_current(-1);
    }

    /// Shows row `i` of the change list and its pages.
    pub fn compare_select(self: &Rc<Self>, s: &mut State, ui: &AppWindow, i: usize) {
        let Some(d) = s.compare.data.as_ref() else {
            return;
        };
        let Some(&ci) = d.rows.get(i) else { return };
        let c = &d.cmp.changes[ci];
        let pair = c
            .page_b
            .and_then(|p| d.cmp.pair_of(1, p))
            .or_else(|| c.page_a.and_then(|p| d.cmp.pair_of(0, p)))
            .unwrap_or(d.pair);
        ui.global::<CompareState>().set_current(i as i32);
        self.compare_show_pair(s, ui, pair);
    }

    fn compare_show_pair(self: &Rc<Self>, s: &mut State, ui: &AppWindow, pair: usize) {
        let g = ui.global::<CompareState>();
        let Some(d) = s.compare.data.as_mut() else {
            return;
        };
        let n = d.cmp.pairs.len();
        if n == 0 {
            return;
        }
        let pair = pair.min(n - 1);
        d.pair = pair;
        let (x, y) = d.cmp.pairs[pair];
        let current = usize::try_from(g.get_current())
            .ok()
            .and_then(|r| d.rows.get(r).copied());
        let label = |p: Option<usize>, gone: &str| match p {
            Some(p) => tr!("page {}", p + 1),
            None => gone.to_owned(),
        };
        let left = label(x, &tr!("no page (inserted)"));
        let right = label(y, &tr!("no page (removed)"));
        g.set_left_label(format!("{}, {}", d.old_name, left).into());
        g.set_right_label(format!("{}, {}", d.new_name, right).into());
        g.set_pair(pair as i32);
        g.set_pairs(n as i32);
        let lm = x.map(|p| d.marks(ui, 0, p, current)).unwrap_or_default();
        let rm = y.map(|p| d.marks(ui, 1, p, current)).unwrap_or_default();
        g.set_left_marks(ModelRc::new(VecModel::from(lm)));
        g.set_right_marks(ModelRc::new(VecModel::from(rm)));
        d.serial += 1;
        let serial = d.serial;
        let (old, new) = (d.old.clone(), d.new.clone());
        pagebara_thread::spawn("compare-pages", move || {
            let (l, r) = rayon::join(
                || x.and_then(|p| render(&old, p)),
                || y.and_then(|p| render(&new, p)),
            );
            post_act(move |_app, s, ui| {
                if s.compare.data.as_ref().is_none_or(|d| d.serial != serial) {
                    return;
                }
                let g = ui.global::<CompareState>();
                g.set_left(image(l));
                g.set_right(image(r));
            });
        });
    }

    pub fn compare_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str, value: &str) {
        let g = ui.global::<CompareState>();
        let current = g.get_current();
        let rows = s.compare.data.as_ref().map_or(0, |d| d.rows.len());
        let pair = s.compare.data.as_ref().map_or(0, |d| d.pair);
        match what {
            "select" => self.compare_select(s, ui, value.parse().unwrap_or(0)),
            "next" if rows > 0 => {
                self.compare_select(s, ui, ((current + 1).max(0) as usize).min(rows - 1))
            }
            "previous" if rows > 0 => self.compare_select(s, ui, (current - 1).max(0) as usize),
            "next-pair" => self.compare_show_pair(s, ui, pair + 1),
            "previous-pair" => self.compare_show_pair(s, ui, pair.saturating_sub(1)),
            "pair" => self.compare_show_pair(s, ui, value.parse().unwrap_or(0)),
            "filter" => {
                self.compare_rows(s, ui);
                self.compare_show_pair(s, ui, pair);
            }
            "report" => {
                let Some(d) = s.compare.data.as_ref() else {
                    return;
                };
                let name = format!(
                    "{} vs {}.pdf",
                    d.old_name.trim_end_matches(".pdf"),
                    d.new_name.trim_end_matches(".pdf")
                );
                let near = s.tab_ref().and_then(|t| t.session.path.clone());
                pick::save_pdf(
                    self,
                    &tr!("Save the comparison report"),
                    near,
                    &name,
                    |app, s, _ui, p| {
                        app.compare_report(s, p);
                    },
                );
            }
            "close" => {
                if let Some(c) = s.compare.cancel.take() {
                    c.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                s.compare.data = None;
                g.set_visible(false);
                ui.invoke_focus_view();
                g.set_left(Image::default());
                g.set_right(Image::default());
            }
            _ => {}
        }
    }

    fn compare_report(self: &Rc<Self>, s: &mut State, path: PathBuf) {
        let Some(d) = s.compare.data.as_ref() else {
            return;
        };
        let (a, b, cmp) = (d.old_doc.clone(), d.new_doc.clone(), d.cmp.clone());
        let opts = pagebara_compare::ReportOptions {
            old_name: d.old_name.clone(),
            new_name: d.new_name.clone(),
            only_changed: true,
        };
        self.message(s, &tr!("Writing the report…"));
        pagebara_thread::spawn("compare-report", move || {
            let r = pagebara_compare::report(&a, &b, &cmp, &opts)
                .and_then(|pdf| std::fs::write(&path, pdf).map_err(|e| e.to_string()));
            post_act(move |app, s, ui| match r {
                Ok(()) => {
                    s.pro.last = format!("report {}", path.display());
                    app.message(s, &tr!("Report saved to {}.", path.display()));
                }
                Err(e) => app.show_message(s, ui, &tr!("Could not save the report"), &e),
            });
        });
    }
}
