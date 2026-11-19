//! Organizing pages: a virtualised grid of thumbnails, selection, drag to move, and the
//! page operations (rotate, duplicate, extract, remove, crop, split, insert, merge).

use std::path::{Path, PathBuf};
use std::rc::Rc;

use pagebara_app_core::page_ops;
use pagebara_app_core::session::write_atomic;
use pagebara_app_core::tiles::Key;
use pagebara_i18n::{tr, trn};
use pagebara_syntax::Document;
use slint::{ComponentHandle, Model, ModelRc, VecModel};

use super::document::EditError;
use super::{App, State, Tab};
use crate::{AppState, AppWindow, DialogKind, GridRow, Mode, Thumb};

const CELL_W: f64 = 150.0;
const GAP: f64 = 22.0;
const ROW_H: f64 = 230.0;

pub fn wire(app: &Rc<App>, ui: &AppWindow) {
    let st = ui.global::<AppState>();
    let a = app.clone();
    st.on_organize(move || a.act(|app, s, ui| app.organize(s, ui)));
    let a = app.clone();
    st.on_organize_done(move || a.act(|app, s, ui| app.organize_done(s, ui)));
    let a = app.clone();
    st.on_organize_action(move |what| {
        let what = what.to_string();
        a.act(move |app, s, ui| app.organize_action(s, ui, &what))
    });
    let a = app.clone();
    st.on_grid_resized(move |w| {
        a.act(move |app, s, ui| {
            let Some(t) = s.tab() else { return };
            t.grid.width = f64::from(w);
            let cols =
                (((f64::from(w) - 2.0 * GAP + GAP) / (CELL_W + GAP)).floor() as usize).max(1);
            if cols != t.grid.cols {
                t.grid.cols = cols;
                if ui.global::<AppState>().get_mode() == Mode::Organize {
                    app.organize(s, ui);
                }
            }
        })
    });
    let a = app.clone();
    st.on_grid_scrolled(move |first, last| {
        a.act(move |app, s, ui| {
            let Some(t) = s.tab() else { return };
            let cols = t.grid.cols.max(1);
            let n = t.session.page_count();
            let from = (first.max(0) as usize * cols).min(n);
            let to = ((last.max(0) as usize + 1) * cols).min(n);
            let old = std::mem::replace(&mut t.grid.visible, from..to);
            t.view.view.want_previews(from..to);
            // Cells scrolled off let their previews go; cells scrolled on get cached ones.
            let changed: Vec<usize> = (old.start..old.end.min(n))
                .filter(|p| !(from..to).contains(p))
                .chain((from..to).filter(|p| !old.contains(p)))
                .collect();
            set_cells(t, ui, &changed);
            app.refresh(s, ui);
        })
    });
    let a = app.clone();
    st.on_grid_click(move |i, ctrl, shift| {
        a.act(move |app, s, ui| app.grid_click(s, ui, i as usize, ctrl, shift))
    });
    let a = app.clone();
    st.on_grid_drag(move |i, x, y, phase| {
        a.act(move |app, s, ui| app.grid_drag(s, ui, i as usize, f64::from(x), f64::from(y), phase))
    });
}

/// Cell `i` of the grid, with its preview when on screen.
fn thumb(t: &Tab, i: usize) -> Thumb {
    let (w, h) = t.session.geoms[i].size();
    let image = if t.grid.visible.contains(&i) {
        t.view.image(&Key::preview(i)).unwrap_or_default()
    } else {
        Default::default()
    };
    Thumb {
        index: i as i32,
        image,
        aspect: (h / w.max(1.0)) as f32,
        current: false,
        selected: t.grid.selected.contains(&i),
        label: t.session.page_label(i).into(),
    }
}

/// Puts newly rendered previews into the grid.
pub fn previews_arrived(t: &mut Tab, ui: &AppWindow, pages: &[usize]) {
    let shown: Vec<usize> = pages
        .iter()
        .copied()
        .filter(|p| t.grid.visible.contains(p))
        .collect();
    set_cells(t, ui, &shown);
}

/// Updates the grid cells of `pages`.
fn set_cells(t: &Tab, ui: &AppWindow, pages: &[usize]) {
    if pages.is_empty() {
        return;
    }
    let cols = t.grid.cols.max(1);
    let grid = ui.global::<AppState>().get_grid();
    for &p in pages {
        if let Some(row) = grid.row_data(p / cols)
            && p % cols < row.cells.row_count()
        {
            row.cells.set_row_data(p % cols, thumb(t, p));
        }
    }
}

impl App {
    /// Shows the organize view of the active document.
    pub fn organize(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        if ui.global::<AppState>().get_mode() == Mode::View {
            self.finish_editors(s, ui);
        }
        self.clear_message(s, ui);
        let Some(t) = s.tab() else { return };
        if t.grid.cols == 0 {
            t.grid.cols = 5;
        }
        let cols = t.grid.cols;
        let n = t.session.page_count();
        if t.grid.visible.is_empty() {
            t.grid.visible = 0..n.min(cols * 4);
        }
        let rows: Vec<GridRow> = (0..n)
            .step_by(cols)
            .map(|start| GridRow {
                cells: ModelRc::new(VecModel::from(
                    (start..(start + cols).min(n))
                        .map(|i| thumb(t, i))
                        .collect::<Vec<_>>(),
                )),
            })
            .collect();
        let st = ui.global::<AppState>();
        st.set_grid(ModelRc::new(VecModel::from(rows)));
        st.set_grid_cols(cols as i32);
        st.set_selected_count(t.grid.selected.len() as i32);
        st.set_page_count(n as i32);
        st.set_drop_index(-1);
        st.set_mode(Mode::Organize);
        t.view.view.want_previews(t.grid.visible.clone());
        self.refresh(s, ui);
    }

    pub fn organize_done(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        ui.global::<AppState>().set_mode(Mode::View);
        if let Some(t) = s.tab() {
            t.overlay_dirty = true;
        }
        self.refresh(s, ui);
    }

    fn sync_grid_selection(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab_ref() else { return };
        let cols = t.grid.cols.max(1);
        let grid = ui.global::<AppState>().get_grid();
        for r in 0..grid.row_count() {
            let Some(row) = grid.row_data(r) else {
                continue;
            };
            for c in 0..row.cells.row_count() {
                let i = r * cols + c;
                if let Some(mut cell) = row.cells.row_data(c) {
                    let sel = t.grid.selected.contains(&i);
                    if cell.selected != sel {
                        cell.selected = sel;
                        row.cells.set_row_data(c, cell);
                    }
                }
            }
        }
        ui.global::<AppState>()
            .set_selected_count(t.grid.selected.len() as i32);
    }

    fn grid_click(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        i: usize,
        ctrl: bool,
        shift: bool,
    ) {
        let Some(t) = s.tab() else { return };
        let g = &mut t.grid;
        match (ctrl, shift, g.anchor) {
            (_, true, Some(a)) => {
                if !ctrl {
                    g.selected.clear();
                }
                g.selected.extend(a.min(i)..=a.max(i));
            }
            (true, _, _) => {
                if !g.selected.remove(&i) {
                    g.selected.insert(i);
                }
                g.anchor = Some(i);
            }
            _ => {
                let only = g.selected.len() == 1 && g.selected.contains(&i);
                g.selected.clear();
                if !only {
                    g.selected.insert(i);
                }
                g.anchor = Some(i);
            }
        }
        self.sync_grid_selection(s, ui);
    }

    pub fn grid_select_all(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(t) = s.tab() else { return };
        t.grid.selected = (0..t.session.page_count()).collect();
        self.sync_grid_selection(s, ui);
    }

    /// The gap a point of the grid (content coordinates) falls into: insert before this.
    fn drop_index(t: &Tab, x: f64, y: f64) -> usize {
        let cols = t.grid.cols.max(1);
        let n = t.session.page_count();
        let left = (t.grid.width - (cols as f64 * (CELL_W + GAP) - GAP)) / 2.0;
        let row = (y / ROW_H).floor().max(0.0) as usize;
        let col = ((x - left + GAP / 2.0) / (CELL_W + GAP) + 0.5)
            .floor()
            .max(0.0) as usize;
        (row * cols + col.min(cols)).min(n)
    }

    fn grid_drag(
        self: &Rc<Self>,
        s: &mut State,
        ui: &AppWindow,
        i: usize,
        x: f64,
        y: f64,
        phase: i32,
    ) {
        let Some(t) = s.tab() else { return };
        if phase == 0 && !t.grid.selected.contains(&i) {
            t.grid.selected.clear();
            t.grid.selected.insert(i);
            t.grid.anchor = Some(i);
            self.sync_grid_selection(s, ui);
        }
        let Some(t) = s.tab() else { return };
        let to = Self::drop_index(t, x, y);
        let st = ui.global::<AppState>();
        if phase < 2 {
            t.grid.drop = Some(to);
            st.set_drop_index(to as i32);
            return;
        }
        st.set_drop_index(-1);
        t.grid.drop = None;
        self.move_selected(s, ui, to);
    }

    /// Moves the selected pages to before page `to`.
    fn move_selected(self: &Rc<Self>, s: &mut State, ui: &AppWindow, to: usize) {
        let Some(t) = s.tab() else { return };
        let sel: Vec<usize> = t.grid.selected.iter().copied().collect();
        // Dropping right before or after the moved block changes nothing.
        let contiguous = sel.windows(2).all(|w| w[1] == w[0] + 1);
        if sel.is_empty() || (contiguous && to >= sel[0] && to <= sel[sel.len() - 1] + 1) {
            return;
        }
        let n_before = (0..to).filter(|p| !t.grid.selected.contains(p)).count();
        let label = if sel.len() == 1 {
            tr!("Move page")
        } else {
            tr!("Move pages")
        };
        if self
            .edit(s, ui, &label, |d| {
                Ok(pagebara_pages::move_pages(d, &sel, to)?)
            })
            .is_some()
            && let Some(t) = s.tab()
        {
            t.grid.selected = (n_before..n_before + sel.len()).collect();
            t.grid.anchor = Some(n_before);
            self.organize(s, ui);
        }
    }

    /// The selected pages, or `None` (with a hint) when nothing is selected.
    fn selection(self: &Rc<Self>, s: &mut State) -> Option<Vec<usize>> {
        let t = s.tab_ref()?;
        let sel: Vec<usize> = t.grid.selected.iter().copied().collect();
        if sel.is_empty() {
            self.message(
                s,
                &tr!("Select pages first: click a page, Shift-click for a range."),
            );
            return None;
        }
        Some(sel)
    }

    pub fn organize_action(self: &Rc<Self>, s: &mut State, ui: &AppWindow, what: &str) {
        match what {
            "rotate" | "rotate-left" => {
                let Some(sel) = self.selection(s) else { return };
                let turns = if what == "rotate" { 1 } else { -1 };
                self.edit(s, ui, &tr!("Rotate pages"), |d| {
                    Ok(pagebara_pages::rotate(d, &sel, turns)?)
                });
            }
            "move-back" | "move-forward" => {
                let Some(sel) = self.selection(s) else { return };
                let n = s.tab_ref().map_or(0, |t| t.session.page_count());
                let (first, last) = (sel[0], sel[sel.len() - 1]);
                if what == "move-back" && first > 0 {
                    self.move_selected(s, ui, first - 1);
                } else if what == "move-forward" && last + 1 < n {
                    self.move_selected(s, ui, last + 2);
                }
            }
            "duplicate" => {
                let Some(sel) = self.selection(s) else { return };
                self.edit(s, ui, &tr!("Duplicate pages"), |d| {
                    Ok(pagebara_pages::duplicate(d, &sel)?)
                });
            }
            "remove" => {
                let Some(sel) = self.selection(s) else { return };
                let n = sel.len();
                if self
                    .edit(s, ui, &tr!("Remove pages"), |d| {
                        Ok(pagebara_pages::delete(d, &sel)?)
                    })
                    .is_some()
                {
                    if let Some(t) = s.tab() {
                        t.grid.selected.clear();
                    }
                    self.sync_grid_selection(s, ui);
                    self.message(
                        s,
                        &trn!(
                            "Removed {n} page. Undo brings it back.",
                            "Removed {n} pages. Undo brings them back.",
                            n
                        ),
                    );
                }
            }
            "extract" => self.extract(s, ui),
            "crop" => {
                let st = ui.global::<AppState>();
                let all = s.tab_ref().is_some_and(|t| t.grid.selected.is_empty());
                st.set_crop_all(all);
                st.set_dialog_error("".into());
                st.set_crop_margins(ModelRc::new(VecModel::from(vec![0.0f32; 4])));
                st.set_dialog(DialogKind::Crop);
            }
            "split" => {
                let st = ui.global::<AppState>();
                st.set_dialog_error("".into());
                if st.get_split_ranges().is_empty() {
                    let n = s.tab_ref().map_or(1, |t| t.session.page_count());
                    st.set_split_ranges(
                        format!("1-{}, {}-", n.div_ceil(2), n.div_ceil(2) + 1).into(),
                    );
                }
                st.set_dialog(DialogKind::Split);
            }
            "insert" => self.pick_insert(s, false),
            "merge" => self.pick_insert(s, true),
            _ => {}
        }
        if ui.global::<AppState>().get_mode() == Mode::Organize {
            self.sync_grid_selection(s, ui);
        }
    }

    fn pick_insert(self: &Rc<Self>, s: &mut State, merge: bool) {
        let Some(uid) = s.tab_ref().map(|t| t.uid) else {
            return;
        };
        let app = self.clone();
        let task = slint::spawn_local(async move {
            let mut dlg = rfd::AsyncFileDialog::new().set_title(if merge {
                tr!("Add documents at the end")
            } else {
                tr!("Insert pages from")
            });
            dlg = if merge {
                dlg.add_filter(tr!("PDF documents"), &["pdf", "PDF"])
            } else {
                dlg.add_filter(
                    tr!("PDF documents and images"),
                    &["pdf", "PDF", "png", "jpg", "jpeg"],
                )
            };
            let Some(files) = dlg.pick_files().await else {
                return;
            };
            app.act(move |app, s, ui| {
                let Some(i) = s.tab_index(uid) else { return };
                app.select_tab(s, ui, i);
                for f in files {
                    if merge {
                        app.insert_at(s, ui, f.path().to_owned(), usize::MAX);
                    } else {
                        app.insert_file(s, ui, f.path().to_owned());
                    }
                }
            });
        });
        if let Err(e) = task {
            log::warn!("cannot show the file dialog: {e}");
        }
    }

    /// Inserts a PDF's pages or an image as a page after the selection (or at the end).
    pub fn insert_file(self: &Rc<Self>, s: &mut State, ui: &AppWindow, path: PathBuf) {
        let at = s
            .tab_ref()
            .and_then(|t| t.grid.selected.last().map(|&i| i + 1))
            .unwrap_or(usize::MAX);
        self.insert_at(s, ui, path, at);
    }

    /// An image dropped with no document open: a new one-page document.
    pub fn open_image(self: &Rc<Self>, path: PathBuf) {
        let bytes = load_insertable(&path).and_then(|d| d.save_full().map_err(|e| e.to_string()));
        match bytes {
            // No path: saving asks where.
            Ok(bytes) => self.opened(None, pagebara_app_core::session::open_bytes(None, bytes)),
            Err(e) => self.act(move |app, s, ui| {
                app.show_message(s, ui, &tr!("Could not open the image"), &e)
            }),
        }
    }

    /// Inserts a PDF's pages or an image as a page at `at` (clamped to the end).
    pub fn insert_at(self: &Rc<Self>, s: &mut State, ui: &AppWindow, path: PathBuf, at: usize) {
        let Some(t) = s.tab_ref() else { return };
        let n = t.session.page_count();
        let at = at.min(n);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let src = match load_insertable(&path) {
            Ok(d) => d,
            Err(e) => return self.show_message(s, ui, &tr!("Could not insert"), &e),
        };
        let count = pagebara_pages::PageTree::load(&src).len();
        let pages: Vec<usize> = (0..count).collect();
        let label = tr!("Insert {}", name);
        if self
            .edit(s, ui, &label, |d| {
                Ok(pagebara_pages::insert(d, at, &src, &pages)?)
            })
            .is_some()
            && let Some(t) = s.tab()
        {
            t.grid.selected = (at..at + count).collect();
            t.grid.anchor = Some(at);
            if ui.global::<AppState>().get_mode() == Mode::Organize {
                self.organize(s, ui);
            }
            self.message(
                s,
                &trn!(
                    "Inserted {n} page from {}.",
                    "Inserted {n} pages from {}.",
                    count,
                    name
                ),
            );
        }
    }

    fn extract(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let Some(sel) = self.selection(s) else { return };
        let Some(t) = s.tab_ref() else { return };
        let doc = match pagebara_pages::extract(t.session.doc(), &sel) {
            Ok(d) => d,
            Err(e) => return self.edit_failed(s, ui, EditError::from(e)),
        };
        let name = format!(
            "{} (pages {}).pdf",
            stem(&t.session.title()),
            describe(&sel)
        );
        let dir = t
            .session
            .path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_owned));
        let app = self.clone();
        let task = slint::spawn_local(async move {
            let mut dlg = rfd::AsyncFileDialog::new()
                .set_title(tr!("Extract pages to"))
                .add_filter(tr!("PDF documents"), &["pdf"])
                .set_file_name(&name);
            if let Some(d) = dir {
                dlg = dlg.set_directory(d);
            }
            let Some(f) = dlg.save_file().await else {
                return;
            };
            let path = f.path().to_owned();
            let result = doc
                .save_full()
                .map_err(|e| e.to_string())
                .and_then(|b| write_atomic(&path, &b).map_err(|e| e.to_string()));
            app.act(move |app, s, ui| match result {
                Ok(()) => app.message(s, &tr!("Extracted to {}", path.display())),
                Err(e) => app.show_message(s, ui, &tr!("Could not extract"), &e),
            });
        });
        if let Err(e) = task {
            log::warn!("cannot show the file dialog: {e}");
        }
    }

    /// Writes one file per range into a folder the user picks.
    pub fn split(self: &Rc<Self>, s: &mut State, ui: &AppWindow) {
        let st = ui.global::<AppState>();
        let Some(t) = s.tab_ref() else { return };
        let n = t.session.page_count();
        let ranges = if st.get_split_mode() == 0 {
            page_ops::parse_ranges(&st.get_split_ranges(), n)
        } else {
            let every = st.get_split_every().max(1) as usize;
            Ok(page_ops::every(every, n))
        };
        let ranges = match ranges {
            Ok(r) => r,
            Err(e) => {
                st.set_dialog_error(e.into());
                return;
            }
        };
        let docs = match pagebara_pages::split(t.session.doc(), &ranges) {
            Ok(d) => d,
            Err(e) => {
                st.set_dialog(DialogKind::None);
                return self.edit_failed(s, ui, EditError::from(e));
            }
        };
        st.set_dialog(DialogKind::None);
        let base = stem(&t.session.title());
        let dir = t
            .session
            .path
            .as_ref()
            .and_then(|p| p.parent().map(Path::to_owned));
        let app = self.clone();
        let task = slint::spawn_local(async move {
            let mut dlg = rfd::AsyncFileDialog::new().set_title(tr!("Save the parts in"));
            if let Some(d) = dir {
                dlg = dlg.set_directory(d);
            }
            let Some(folder) = dlg.pick_folder().await else {
                return;
            };
            let folder = folder.path().to_owned();
            let result = write_parts(&folder, &base, &ranges, &docs);
            app.act(move |app, s, ui| match result {
                Ok(n) => app.message(
                    s,
                    &trn!(
                        "Split into {n} file in {}",
                        "Split into {n} files in {}",
                        n,
                        folder.display()
                    ),
                ),
                Err(e) => app.show_message(s, ui, &tr!("Could not split"), &e),
            });
        });
        if let Err(e) = task {
            log::warn!("cannot show the folder dialog: {e}");
        }
    }

    /// Crops the selected pages (or all) by margins in millimetres, or shows them whole.
    pub fn crop(self: &Rc<Self>, s: &mut State, ui: &AppWindow, reset: bool) {
        let st = ui.global::<AppState>();
        let m: Vec<f64> = st
            .get_crop_margins()
            .iter()
            .map(|v| f64::from(v.max(0.0)) * 72.0 / 25.4)
            .collect();
        let Some(t) = s.tab_ref() else { return };
        let pages: Vec<usize> = if t.grid.selected.is_empty() {
            (0..t.session.page_count()).collect()
        } else {
            t.grid.selected.iter().copied().collect()
        };
        let mut rects = Vec::new();
        if !reset {
            for &i in &pages {
                match page_ops::crop_rect(&t.session.geoms[i], [m[0], m[1], m[2], m[3]]) {
                    Some(r) => rects.push((i, Some(r))),
                    None => {
                        st.set_dialog_error(tr!("Those margins leave nothing of the page.").into());
                        return;
                    }
                }
            }
        } else {
            rects = pages.iter().map(|&i| (i, None)).collect();
        }
        st.set_dialog(DialogKind::None);
        let label = if reset {
            tr!("Show whole pages")
        } else {
            tr!("Crop pages")
        };
        self.edit(s, ui, &label, |d| {
            for (i, r) in rects {
                pagebara_pages::crop(d, &[i], r)?;
            }
            Ok::<(), EditError>(())
        });
    }
}

fn load_insertable(path: &Path) -> Result<Document, String> {
    let bytes = std::fs::read(path).map_err(|e| tr!("The file could not be read: {}", e))?;
    if bytes.starts_with(b"\x89PNG") || bytes.starts_with(&[0xFF, 0xD8]) {
        let img = pagebara_signatures::decode_image(&bytes).map_err(|e| e.to_string())?;
        return page_ops::image_document(img.width, img.height, &img.pixels);
    }
    match Document::open(bytes) {
        Ok(d) => Ok(d),
        Err(pagebara_syntax::Error::PasswordRequired(_)) => Err(tr!(
            "That document is protected with a password. Open it in Pagebara first, then drag its pages."
        )),
        Err(e) => Err(tr!("That file could not be read as PDF or image: {}", e)),
    }
}

fn write_parts(
    folder: &Path,
    base: &str,
    ranges: &[std::ops::Range<usize>],
    docs: &[Document],
) -> Result<usize, String> {
    for (r, d) in ranges.iter().zip(docs) {
        let pages: Vec<usize> = r.clone().collect();
        let name = format!("{base} (pages {}).pdf", describe(&pages));
        let bytes = d.save_full().map_err(|e| e.to_string())?;
        write_atomic(&folder.join(name), &bytes).map_err(|e| e.to_string())?;
    }
    Ok(docs.len())
}

fn stem(title: &str) -> String {
    title
        .strip_suffix(".pdf")
        .or_else(|| title.strip_suffix(".PDF"))
        .unwrap_or(title)
        .to_owned()
}

/// "1-3, 5" for 0-based pages.
fn describe(pages: &[usize]) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while i < pages.len() {
        let mut j = i;
        while j + 1 < pages.len() && pages[j + 1] == pages[j] + 1 {
            j += 1;
        }
        out.push(if i == j {
            (pages[i] + 1).to_string()
        } else {
            format!("{}-{}", pages[i] + 1, pages[j] + 1)
        });
        i = j + 1;
    }
    out.join(", ")
}

#[cfg(test)]
mod tests {
    #[test]
    fn describes_pages() {
        assert_eq!(super::describe(&[0, 1, 2, 4, 6, 7]), "1-3, 5, 7-8");
        assert_eq!(super::stem("a.pdf"), "a");
    }
}
