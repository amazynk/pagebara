//! The window's size, position and maximised state per screen layout: on the laptop alone
//! or docked to a monitor, the window comes back as it was last left on that layout, and
//! is never placed on a screen that is no longer there.

use std::rc::Rc;
use std::time::Duration;

use pagebara_app_core::settings::{Settings, WindowState};
use slint::winit_030::{WinitWindowAccessor, winit};
use slint::{ComponentHandle, Timer};

use crate::AppWindow;
use crate::app::App;

/// Screen layouts remembered.
const MAX_LAYOUTS: usize = 8;

/// The connected screens: a key for the layout, and each screen's rectangle (physical).
fn screens(w: &winit::window::Window) -> (String, Vec<(i32, i32, u32, u32)>) {
    let mut list: Vec<(String, (i32, i32, u32, u32))> = w
        .available_monitors()
        .map(|m| {
            let (p, s) = (m.position(), m.size());
            let name = m.name().unwrap_or_default();
            (
                format!("{name} {}x{}@{},{}", s.width, s.height, p.x, p.y),
                (p.x, p.y, s.width, s.height),
            )
        })
        .collect();
    list.sort();
    let key = list
        .iter()
        .map(|(k, _)| k.as_str())
        .collect::<Vec<_>>()
        .join("; ");
    (key, list.into_iter().map(|(_, r)| r).collect())
}

/// The layout key of the screens the window is shown on, once it is shown.
pub fn layout(ui: &AppWindow) -> Option<String> {
    ui.window()
        .with_winit_window(|w| screens(w).0)
        .filter(|k| !k.is_empty())
}

/// Once the window exists: the state saved for this screen layout, or the last one kept
/// on a screen that is there.
pub fn restore(app: &Rc<App>) {
    let app = app.clone();
    Timer::single_shot(Duration::ZERO, move || {
        let Some(ui) = app.ui.upgrade() else { return };
        let Some((key, rects)) = ui.window().with_winit_window(screens) else {
            return;
        };
        let w = ui.window();
        let saved = app.state.borrow().settings.screens.get(&key).copied();
        if let Some(ws) = saved {
            apply(&ui, &ws);
        }
        // A position off every screen (a monitor that was unplugged) is dropped.
        let pos = w.position();
        let visible = rects.iter().any(|&(x, y, width, height)| {
            pos.x + 40 >= x
                && pos.y + 20 >= y
                && pos.x + 40 < x + width as i32
                && pos.y + 20 < y + height as i32
        });
        if !visible && let Some(&(x, y, width, height)) = rects.first() {
            let size = w.size();
            let cx = x + (width as i32 - size.width as i32).max(0) / 2;
            let cy = y + (height as i32 - size.height as i32).max(0) / 2;
            w.set_position(slint::PhysicalPosition::new(cx, cy));
        }
    });
}

/// Applies `ws` to the window (before it shows, or when the layout matches).
pub fn apply(ui: &AppWindow, ws: &WindowState) {
    let w = ui.window();
    if ws.width >= 400.0 && ws.height >= 300.0 {
        w.set_size(slint::LogicalSize::new(ws.width, ws.height));
    }
    if let (Some(x), Some(y)) = (ws.x, ws.y) {
        w.set_position(slint::PhysicalPosition::new(x, y));
    }
    if ws.maximized {
        w.set_maximized(true);
    }
}

/// Keeps `ws` as the last state and as the state for `layout`.
pub fn remember(settings: &mut Settings, ws: WindowState, layout: Option<String>) {
    settings.window = Some(ws);
    if let Some(key) = layout {
        settings.screens.remove(&key);
        if settings.screens.len() >= MAX_LAYOUTS
            && let Some(first) = settings.screens.keys().next().cloned()
        {
            settings.screens.remove(&first);
        }
        settings.screens.insert(key, ws);
    }
}
