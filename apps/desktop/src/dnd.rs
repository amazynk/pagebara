//! Files dropped on the window. PDFs open in new tabs. Dropped on the page thumbnails (or
//! anywhere in the organize view), PDFs and images are inserted as pages there; an image
//! dropped on the document goes in after the current page, and with no document open it
//! becomes a new one.
//!
//! winit reports a drop without a position, so the drop lands where the pointer was last
//! seen: on X11 and Windows the pointer moves over the window as the drop ends, and the
//! drop waits a moment for that. (winit 0.30 has no file drops on Wayland.)

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use slint::winit_030::{EventResult, WinitWindowAccessor, winit};
use slint::{ComponentHandle, Timer};

use crate::app::{App, State};
use crate::{AppState, AppWindow, Mode, SidebarTab};

/// How long a drop waits for the pointer position.
const SETTLE: Duration = Duration::from_millis(120);

pub fn install(app: &Rc<App>, ui: &AppWindow) {
    let app = app.clone();
    let pointer: Rc<Cell<Option<(f32, f32)>>> = Rc::default();
    let pending: Rc<RefCell<Vec<PathBuf>>> = Rc::default();
    let timer = Rc::new(Timer::default());
    let buttons = ui.as_weak();
    let first_draw = Cell::new(true);
    ui.window()
        .on_winit_window_event(move |w, event| match event {
            winit::event::WindowEvent::RedrawRequested if first_draw.replace(false) => {
                #[cfg(target_os = "macos")]
                if let Some(ui) = buttons.upgrade() {
                    crate::app::macos_menu::install(&ui);
                }
                // With a renderer that reports no frames, this event is the sign of the
                // first one: the window draws while handling it, so the frame is there
                // when the event loop comes round again.
                if crate::app::frames_unreported() {
                    Timer::single_shot(Duration::ZERO, crate::app::first_frame);
                }
                EventResult::Propagate
            }
            winit::event::WindowEvent::CursorMoved { position, .. } => {
                let p = position.to_logical::<f32>(f64::from(w.scale_factor()));
                pointer.set(Some((p.x, p.y)));
                EventResult::Propagate
            }
            winit::event::WindowEvent::DroppedFile(path) => {
                pending.borrow_mut().push(path.clone());
                let (app, pointer, pending) = (app.clone(), pointer.clone(), pending.clone());
                timer.start(slint::TimerMode::SingleShot, SETTLE, move || {
                    let files = std::mem::take(&mut *pending.borrow_mut());
                    dropped(&app, files, pointer.get());
                });
                EventResult::PreventDefault
            }
            // The window's buttons in a merged title bar: AppKit puts them back where it
            // has them when the window changes.
            winit::event::WindowEvent::Resized(_)
            | winit::event::WindowEvent::ScaleFactorChanged { .. }
            | winit::event::WindowEvent::Focused(_)
            | winit::event::WindowEvent::ThemeChanged(_) => {
                if let Some(ui) = buttons.upgrade() {
                    w.with_winit_window(|win| crate::os::place_window_buttons(&ui, win));
                    #[cfg(target_os = "macos")]
                    if matches!(event, winit::event::WindowEvent::Focused(_)) {
                        // Slint installs its menu while processing the focus event.
                        Timer::single_shot(Duration::ZERO, move || {
                            crate::app::macos_menu::install(&ui)
                        });
                    }
                }
                EventResult::Propagate
            }
            _ => EventResult::Propagate,
        });
}

/// Handles `files` dropped at `at` (logical window coordinates, when known).
pub fn dropped(app: &Rc<App>, files: Vec<PathBuf>, at: Option<(f32, f32)>) {
    app.act(move |app, s, ui| {
        let mode = ui.global::<AppState>().get_mode();
        // Where in the pages list, when dropped on it.
        let mut insert_at = at.and_then(|(x, y)| pages_list_index(s, ui, x, y));
        for path in files {
            let image = is_image(&path);
            if mode == Mode::Organize {
                app.insert_file(s, ui, path);
            } else if let Some(i) = insert_at {
                app.insert_at(s, ui, path, i);
                // The next file goes after this one's pages.
                insert_at = s
                    .tab_ref()
                    .and_then(|t| t.grid.selected.last().map(|&p| p + 1));
            } else if image && !s.tabs.is_empty() {
                let next = s.tab_ref().map_or(0, |t| t.view.current_page + 1);
                app.insert_at(s, ui, path, next);
            } else if image {
                app.open_image(path);
            } else {
                app.open_path(path);
            }
        }
    });
}

fn is_image(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg"))
}

/// The page index a drop at (`x`, `y`) inserts before, when it is over the page
/// thumbnails (one to a row; the upper half of a thumbnail inserts before it).
fn pages_list_index(s: &State, ui: &AppWindow, x: f32, y: f32) -> Option<usize> {
    let st = ui.global::<AppState>();
    let n = s.tab_ref()?.session.page_count();
    if !st.get_sidebar_visible()
        || st.get_sidebar_tab() != SidebarTab::Pages
        || st.get_mode() != Mode::View
    {
        return None;
    }
    let (lx, ly) = (st.get_pages_list_x(), st.get_pages_list_y());
    let (w, h) = (st.get_pages_list_width(), st.get_pages_list_height());
    if x < lx || x > lx + w || y < ly || y > ly + h {
        return None;
    }
    let row_h = st.get_pages_row_height().max(1.0);
    let at = (y - ly + st.get_pages_list_scroll()).max(0.0);
    let page = (at / row_h) as usize;
    if page >= n {
        return Some(n);
    }
    Some(page + usize::from(at % row_h >= row_h / 2.0))
}
