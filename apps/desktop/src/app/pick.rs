//! Native file dialogs that continue with the app state. Scripts preset the answer with
//! [`answer_next`], so tests never wait on a dialog.

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use pagebara_i18n::tr;

use super::{App, State};
use crate::AppWindow;

thread_local! {
    static NEXT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

/// The next file dialog answers `path` without showing.
pub fn answer_next(path: PathBuf) {
    NEXT.with(|n| *n.borrow_mut() = Some(path));
}

pub(super) fn take_answer() -> Option<PathBuf> {
    NEXT.with(|n| n.borrow_mut().take())
}

type Then = Box<dyn FnOnce(&Rc<App>, &mut State, &AppWindow, PathBuf)>;

/// A file dialog to fill in. On Windows it belongs to the app's window: it stays in front
/// of it, and the window takes no input until the dialog is closed. Without an owner it
/// would be a window of its own, which can end up behind the app.
pub(super) fn dialog(app: &App) -> rfd::AsyncFileDialog {
    #[cfg(windows)]
    if let Some(ui) = app.ui.upgrade() {
        use slint::ComponentHandle;
        use slint::winit_030::WinitWindowAccessor;
        if let Some(d) = ui
            .window()
            .with_winit_window(|w| rfd::AsyncFileDialog::new().set_parent(w))
        {
            return d;
        }
    }
    let _ = app;
    rfd::AsyncFileDialog::new()
}

fn run(app: &Rc<App>, dialog: rfd::AsyncFileDialog, save: bool, then: Then) {
    if let Some(p) = take_answer() {
        app.act(move |app, s, ui| then(app, s, ui, p));
        return;
    }
    let app = app.clone();
    let task = slint::spawn_local(async move {
        let picked = if save {
            dialog.save_file().await
        } else {
            dialog.pick_file().await
        };
        if let Some(f) = picked {
            let p = f.path().to_owned();
            app.act(move |app, s, ui| then(app, s, ui, p));
        }
    });
    if let Err(e) = task {
        log::warn!("cannot show the file dialog: {e}");
    }
}

/// Asks where to save a PDF, starting next to `near` with the name `name`.
pub fn save_pdf(
    app: &Rc<App>,
    title: &str,
    near: Option<PathBuf>,
    name: &str,
    then: impl FnOnce(&Rc<App>, &mut State, &AppWindow, PathBuf) + 'static,
) {
    let mut d = dialog(app)
        .set_title(title)
        .add_filter(tr!("PDF documents"), &["pdf"])
        .set_file_name(name);
    if let Some(dir) = near.as_ref().and_then(|p| p.parent()) {
        d = d.set_directory(dir);
    }
    run(app, d, true, Box::new(then));
}

/// Asks where to save a file of type `filter` (its name marked with `tr_noop!`), named
/// `name`.
pub fn save_file(
    app: &Rc<App>,
    title: &str,
    filter: (&str, &[&str]),
    name: &str,
    then: impl FnOnce(&Rc<App>, &mut State, &AppWindow, PathBuf) + 'static,
) {
    let d = dialog(app)
        .set_title(title)
        .add_filter(pagebara_i18n::translate(filter.0, &[]), filter.1)
        .set_file_name(name);
    run(app, d, true, Box::new(then));
}

/// Asks for a file to open; `filter` is named as for [`save_file`].
pub fn open(
    app: &Rc<App>,
    title: &str,
    filter: (&str, &[&str]),
    then: impl FnOnce(&Rc<App>, &mut State, &AppWindow, PathBuf) + 'static,
) {
    let d = dialog(app)
        .set_title(title)
        .add_filter(pagebara_i18n::translate(filter.0, &[]), filter.1);
    run(app, d, false, Box::new(then));
}

/// "Report.pdf" → "Report (signed).pdf".
pub fn derived_name(title: &str, what: &str) -> String {
    let stem = title
        .strip_suffix(".pdf")
        .or_else(|| title.strip_suffix(".PDF"))
        .unwrap_or(title);
    format!("{stem} ({what}).pdf")
}

impl App {
    /// Shows the file at `path` in a tab, reading it again when a tab already has it.
    pub fn open_fresh(self: &Rc<Self>, s: &mut State, ui: &AppWindow, path: PathBuf) {
        let abs = std::path::absolute(&path).ok();
        if let Some(i) = s.tabs.iter().position(|t| {
            t.session
                .path
                .as_ref()
                .and_then(|p| std::path::absolute(p).ok())
                == abs
        }) {
            self.close_tab(s, ui, i, true);
        }
        self.open_path(path);
    }
}
