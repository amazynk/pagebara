//! `--script FILE`: runs a timed list of actions against the real window, with eased pointer
//! motion and an optional synthetic cursor, for UI tests and demo recordings. Pointer and
//! key actions go through the window's event queue like real input; `call` reaches app
//! commands that would otherwise need a native file dialog. Exits with status 1 when an
//! assertion failed.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, Instant};

use i_slint_backend_testing::ElementHandle;
use pagebara_app_core::script::{self, Action, Step, Target, along, ease};
use pagebara_app_core::text_index;
use slint::platform::{Key, PointerEventButton, WindowEvent};
use slint::{ComponentHandle, LogicalPosition, Model, Timer, TimerMode};

use crate::app::{App, State};
use crate::{AppState, AppWindow, DialogKind, Mode, ViewState};

/// The exit status once the script ends.
pub static EXIT: AtomicI32 = AtomicI32::new(0);

const TICK: Duration = Duration::from_millis(16);
/// Longest wait for a document to open or a target to appear.
const PATIENCE: Duration = Duration::from_secs(20);

struct Runner {
    steps: Vec<Step>,
    at: usize,
    started: Option<Instant>,
    cursor: (f32, f32),
    /// Resolved motion of the current step: window points along the way.
    path: Vec<(f64, f64)>,
    pressed: bool,
    last_scroll: f64,
    typed: usize,
    zoom_from: f64,
    shots: PathBuf,
    failures: usize,
    tabs_before: usize,
    timer: Timer,
}

pub fn start(app: &Rc<App>, file: &Path) {
    let src = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => {
            log::error!("script {}: {e}", file.display());
            EXIT.store(2, Ordering::Release);
            let _ = slint::quit_event_loop();
            return;
        }
    };
    let dir = file.parent().map(Path::to_owned).unwrap_or_default();
    let dir_s = dir.to_string_lossy().into_owned();
    let env = move |name: &str| match name {
        "SCRIPT_DIR" => Some(dir_s.clone()),
        n => std::env::var(n).ok(),
    };
    let steps = match script::parse(&src, &env) {
        Ok(s) => s,
        Err(e) => {
            log::error!("script {}: {e}", file.display());
            EXIT.store(2, Ordering::Release);
            let _ = slint::quit_event_loop();
            return;
        }
    };
    let shots = std::env::var_os("PAGEBARA_SHOTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("shots"));
    let runner = Rc::new(RefCell::new(Runner {
        steps,
        at: 0,
        started: None,
        cursor: (400.0, 300.0),
        path: Vec::new(),
        pressed: false,
        last_scroll: 0.0,
        typed: 0,
        zoom_from: 1.0,
        shots,
        failures: 0,
        tabs_before: 0,
        timer: Timer::default(),
    }));
    let r = runner.clone();
    let app = app.clone();
    runner
        .borrow()
        .timer
        .start(TimerMode::Repeated, TICK, move || tick(&app, &r));
}

thread_local! {
    /// Input made while the app state is borrowed; delivered at the end of the tick, so
    /// the app handles it like real input.
    static QUEUE: RefCell<Vec<WindowEvent>> = const { RefCell::new(Vec::new()) };
}

fn send(e: WindowEvent) {
    QUEUE.with(|q| q.borrow_mut().push(e));
}

fn tick(app: &Rc<App>, runner: &Rc<RefCell<Runner>>) {
    // Steps act on a drawn window: a snapshot before the first frame fails.
    if !crate::app::first_frame_drawn() {
        return;
    }
    let Some(ui) = app.ui.upgrade() else { return };
    steps(app, runner, &ui);
    for e in QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut())) {
        ui.window().dispatch_event(e);
    }
}

fn steps(app: &Rc<App>, runner: &Rc<RefCell<Runner>>, ui: &AppWindow) {
    // The app may be busy in a callback; try again on the next tick.
    let Ok(mut s) = app.state.try_borrow_mut() else {
        return;
    };
    let mut r = runner.borrow_mut();
    for _ in 0..64 {
        let Some(step) = r.steps.get(r.at).cloned() else {
            finish(&mut r);
            return;
        };
        let now = Instant::now();
        let first = r.started.is_none();
        let started = *r.started.get_or_insert(now);
        let t = if step.ms == 0 {
            1.0
        } else {
            (now - started).as_secs_f64() / (step.ms as f64 / 1000.0)
        };
        let done = match run(
            app,
            &mut s,
            ui,
            &mut r,
            &step,
            first,
            t.min(1.0),
            now - started,
        ) {
            Ok(done) => done,
            Err(e) => {
                log::error!("script line {}: {e}", step.line);
                r.failures += 1;
                true
            }
        };
        if !done {
            return;
        }
        r.at += 1;
        r.started = None;
        r.path.clear();
        // Instant steps run back to back; timed ones give the UI a frame first, and keys
        // are delivered before the next step runs.
        if step.ms > 0 || matches!(step.action, Action::Key(_)) {
            return;
        }
    }
}

fn finish(r: &mut Runner) {
    r.timer.stop();
    if r.failures > 0 {
        log::error!("script finished with {} failure(s)", r.failures);
        EXIT.store(1, Ordering::Release);
    } else {
        log::info!("script finished");
    }
    let _ = slint::quit_event_loop();
}

fn set_cursor(ui: &AppWindow, r: &mut Runner, x: f64, y: f64) {
    r.cursor = (x as f32, y as f32);
    let st = ui.global::<AppState>();
    st.set_cursor_x(x as f32);
    st.set_cursor_y(y as f32);
    st.set_cursor_pressed(r.pressed);
    send(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x as f32, y as f32),
    });
}

fn press(ui: &AppWindow, r: &mut Runner, down: bool) {
    press_button(ui, r, down, PointerEventButton::Left);
}

fn press_button(ui: &AppWindow, r: &mut Runner, down: bool, button: PointerEventButton) {
    r.pressed = down;
    ui.global::<AppState>().set_cursor_pressed(down);
    let position = LogicalPosition::new(r.cursor.0, r.cursor.1);
    send(if down {
        WindowEvent::PointerPressed { position, button }
    } else {
        WindowEvent::PointerReleased { position, button }
    });
}

/// Where a target is in the window, or `None` while it is not there yet.
fn resolve(
    app: &Rc<App>,
    s: &mut State,
    ui: &AppWindow,
    r: &Runner,
    t: &Target,
) -> Result<Option<(f64, f64)>, String> {
    Ok(match t {
        Target::Window(x, y) => Some((f64::from(*x), f64::from(*y))),
        Target::Here => Some((f64::from(r.cursor.0), f64::from(r.cursor.1))),
        Target::Misspelling(n) => crate::app::spell::script_target(s, ui, *n)?,
        Target::Label { label, nth } => {
            let found: Vec<ElementHandle> = ElementHandle::find_by_accessible_label(ui, label)
                .filter(|e| e.is_valid() && e.size().width > 0.0 && e.computed_opacity() > 0.0)
                .collect();
            found.get(*nth).map(center)
        }
        Target::Id(id) => ElementHandle::find_by_element_id(ui, id)
            .find(|e| e.is_valid() && e.size().width > 0.0)
            .map(|e| center(&e)),
        Target::Text { page, text, end } => {
            let Some(t) = s.tab() else {
                return Err("no document is open".into());
            };
            let tp = t.session.sel_text(*page);
            let hit = text_index::find(&tp, text)
                .into_iter()
                .next()
                .ok_or_else(|| format!("“{text}” is not on page {}", page + 1))?;
            let boxes = text_index::line_boxes(&tp, text_index::chars_of(&tp, hit));
            let b = if *end { boxes.last() } else { boxes.first() }.ok_or("no text box")?;
            let g = t.session.geoms[*page];
            let [x, y, w, h] = g.rect_to_view(b.map(f64::from));
            let x = if *end { x + w - 0.5 } else { x + 0.5 };
            let at = Target::Page {
                page: *page,
                x,
                y: y + h / 2.0,
            };
            return resolve(app, s, ui, r, &at);
        }
        Target::Annot { page, n } => {
            let Some(t) = s.tab() else {
                return Err("no document is open".into());
            };
            let list = t.annots(*page);
            let a = list
                .get(n.saturating_sub(1))
                .ok_or_else(|| format!("page {} has no annotation {n}", page + 1))?;
            let g = t.session.geoms[*page];
            let [x, y, w, h] = g.rect_to_view([a.rect.x0, a.rect.y0, a.rect.x1, a.rect.y1]);
            let at = Target::Page {
                page: *page,
                x: x + w / 2.0,
                y: y + h / 2.0,
            };
            return resolve(app, s, ui, r, &at);
        }
        Target::Object { page, n } => {
            let t = s.tab().ok_or("no document is open")?;
            let (x, y) = pagebara_app_core::object_edit::object_center(t.session.doc(), *page, *n)?;
            return resolve(app, s, ui, r, &Target::Page { page: *page, x, y });
        }
        Target::Handle(name) => {
            let Some((page, x, y)) = crate::app::edit_handle(s, name)? else {
                return Ok(None);
            };
            return resolve(app, s, ui, r, &Target::Page { page, x, y });
        }
        Target::Page { page, x, y } => {
            let Some(t) = s.tab() else {
                return Err("no document is open".into());
            };
            // Positions are only known once a scroll has finished.
            if t.view.animating() {
                return Ok(None);
            }
            if *page >= t.session.page_count() {
                return Err(format!("there is no page {}", page + 1));
            }
            let origin = ElementHandle::find_by_element_id(ui, "AppWindow::view")
                .next()
                .map(|e| e.absolute_position())
                .ok_or("the page view is not shown")?;
            match t.view.page_to_screen(*page, *x, *y) {
                Some((px, py)) => {
                    let (_, vh) = t.view.view.viewport.size();
                    if py < 0.0 || py > vh {
                        t.view.glide_to(*page, (*y - 200.0).max(0.0), 0.0);
                        app.view_op(s, ui, |_| {});
                        None
                    } else {
                        Some((px + f64::from(origin.x), py + f64::from(origin.y)))
                    }
                }
                None => {
                    t.view.glide_to(*page, (*y - 200.0).max(0.0), 0.0);
                    app.view_op(s, ui, |_| {});
                    None
                }
            }
        }
    })
}

fn center(e: &ElementHandle) -> (f64, f64) {
    let p = e.absolute_position();
    let z = e.size();
    (
        f64::from(p.x + z.width / 2.0),
        f64::from(p.y + z.height / 2.0),
    )
}

/// Moves the pointer along `r.path` at `t`; true when there.
fn follow(ui: &AppWindow, r: &mut Runner, t: f64) -> bool {
    let (x, y) = along(&r.path, ease(t));
    set_cursor(ui, r, x, y);
    t >= 1.0
}

#[allow(clippy::too_many_arguments)]
fn run(
    app: &Rc<App>,
    s: &mut State,
    ui: &AppWindow,
    r: &mut Runner,
    step: &Step,
    first: bool,
    t: f64,
    elapsed: Duration,
) -> Result<bool, String> {
    let st = ui.global::<AppState>();
    match &step.action {
        Action::Open(path) => {
            if first {
                r.tabs_before = s.tabs.len();
                app.open_path(PathBuf::from(path));
                return Ok(false);
            }
            let same = |p: &PathBuf| std::path::absolute(p).ok() == std::path::absolute(path).ok();
            let shown = s
                .tab_ref()
                .and_then(|t| t.session.path.as_ref())
                .is_some_and(same);
            let opened = s.tabs.len() > r.tabs_before || shown || st.get_password_visible();
            if opened && !st.get_busy() {
                return Ok(true);
            }
            if st.get_dialog() == DialogKind::Message {
                return Err(format!("could not open {path}: {}", st.get_dialog_text()));
            }
            if elapsed > PATIENCE {
                return Err(format!("{path} did not open"));
            }
            Ok(false)
        }
        Action::Wait => Ok(t >= 1.0),
        Action::Cursor(on) => {
            st.set_cursor_visible(*on);
            let (x, y) = r.cursor;
            set_cursor(ui, r, f64::from(x), f64::from(y));
            Ok(true)
        }
        Action::Tool(name) => {
            let i = [
                "select",
                "highlight",
                "draw",
                "text",
                "sign",
                "forms",
                "redact",
                "edit",
                "prepare",
            ]
            .iter()
            .position(|n| n == name)
            .ok_or_else(|| format!("unknown tool {name}"))?;
            st.set_tool(i as i32);
            app.tool_changed(s, ui);
            Ok(true)
        }
        Action::Move(target) | Action::Click { target, .. } | Action::RightClick(target) => {
            if r.path.is_empty() {
                let Some(to) = resolve(app, s, ui, r, target)? else {
                    return wait_for(elapsed, target);
                };
                r.path = vec![(f64::from(r.cursor.0), f64::from(r.cursor.1)), to];
                r.started = Some(Instant::now());
                return Ok(step.ms == 0 && follow(ui, r, 1.0) && click(ui, r, &step.action));
            }
            if !follow(ui, r, t) {
                return Ok(false);
            }
            Ok(click(ui, r, &step.action))
        }
        Action::Release => {
            press(ui, r, false);
            Ok(true)
        }
        Action::Drag(a, b) | Action::Hold(a, b) => {
            if r.path.is_empty() {
                let Some(from) = resolve(app, s, ui, r, a)? else {
                    return wait_for(elapsed, a);
                };
                // Get to the start first, then press.
                set_cursor(ui, r, from.0, from.1);
                press(ui, r, true);
                let Some(to) = resolve(app, s, ui, r, b)? else {
                    press(ui, r, false);
                    return Err(format!("drag target {b:?} is not on screen"));
                };
                r.path = vec![from, to];
                r.started = Some(Instant::now());
                return Ok(false);
            }
            if !follow(ui, r, t) {
                return Ok(false);
            }
            if matches!(step.action, Action::Drag(..)) {
                press(ui, r, false);
            }
            Ok(true)
        }
        Action::Stroke { .. } | Action::StrokeAt { .. } => {
            if r.path.is_empty() {
                let mut path = Vec::new();
                match &step.action {
                    Action::Stroke { page, points } => {
                        for &(x, y) in points {
                            let target = Target::Page { page: *page, x, y };
                            let Some(p) = resolve(app, s, ui, r, &target)? else {
                                return wait_for(elapsed, &target);
                            };
                            path.push(p);
                        }
                    }
                    Action::StrokeAt { target, points } => {
                        let Some(c) = resolve(app, s, ui, r, target)? else {
                            return wait_for(elapsed, target);
                        };
                        path.extend(points.iter().map(|&(dx, dy)| (c.0 + dx, c.1 + dy)));
                    }
                    _ => {}
                }
                set_cursor(ui, r, path[0].0, path[0].1);
                press(ui, r, true);
                r.path = path;
                r.started = Some(Instant::now());
                return Ok(false);
            }
            // Strokes move at an even pace, like a hand.
            let (x, y) = along(&r.path, t);
            set_cursor(ui, r, x, y);
            if t < 1.0 {
                return Ok(false);
            }
            press(ui, r, false);
            Ok(true)
        }
        Action::Type(text) => {
            let n = text.chars().count().max(1);
            let due = ((t * n as f64).ceil() as usize).min(n);
            if first {
                r.typed = 0;
            }
            for c in text.chars().skip(r.typed).take(due.saturating_sub(r.typed)) {
                key(&c.to_string());
            }
            r.typed = due;
            Ok(t >= 1.0)
        }
        Action::Key(combo) => {
            key_combo(combo)?;
            Ok(true)
        }
        Action::Scroll(dy) => {
            if first {
                r.last_scroll = 0.0;
            }
            let want = dy * ease(t);
            let delta = want - r.last_scroll;
            r.last_scroll = want;
            if delta.abs() > 0.0 {
                send(WindowEvent::PointerScrolled {
                    position: LogicalPosition::new(r.cursor.0, r.cursor.1),
                    delta_x: 0.0,
                    // Not a multiple of a wheel line, so it scrolls directly.
                    delta_y: -(delta as f32) + 0.001,
                });
            }
            if t >= 1.0 {
                r.last_scroll = 0.0;
            }
            Ok(t >= 1.0)
        }
        Action::Zoom(z) => {
            let Some(tab) = s.tab() else {
                return Err("no document is open".into());
            };
            if first {
                r.zoom_from = tab.view.view.viewport.zoom();
            }
            let zz = r.zoom_from * (z / r.zoom_from).powf(ease(t));
            app.view_op(s, ui, move |v| v.zoom_to(zz));
            Ok(t >= 1.0)
        }
        Action::Page(p) => {
            app.jump(s, ui, *p, None);
            Ok(true)
        }
        Action::Screenshot(name) => {
            let path = r.shots.join(name);
            if !crate::screenshot::take(ui, &path) {
                return Err(format!("screenshot {name} failed"));
            }
            Ok(true)
        }
        Action::Assert { key, op, value } => {
            let actual = query(app, s, ui, key)?;
            if op.holds(&actual, value) {
                log::info!("ok: {key} {op} {value}");
                Ok(true)
            } else if elapsed < Duration::from_millis(step.ms.max(1500)) {
                // State settles over a few frames (workers, timers); retry briefly.
                Ok(false)
            } else {
                Err(format!("assert {key} {op} {value}: got “{actual}”"))
            }
        }
        Action::Call(cmd, args) => {
            call(app, s, ui, cmd, args)?;
            Ok(true)
        }
        Action::Quit => {
            r.at = r.steps.len();
            Ok(true)
        }
    }
}

fn wait_for(elapsed: Duration, t: &Target) -> Result<bool, String> {
    if elapsed > PATIENCE {
        Err(format!("target {t:?} did not appear"))
    } else {
        Ok(false)
    }
}

fn click(ui: &AppWindow, r: &mut Runner, a: &Action) -> bool {
    if let Action::RightClick(_) = a {
        press_button(ui, r, true, PointerEventButton::Right);
        press_button(ui, r, false, PointerEventButton::Right);
    }
    if let Action::Click { count, .. } = a {
        for _ in 0..*count {
            press(ui, r, true);
            press(ui, r, false);
        }
    }
    true
}

fn key(text: &str) {
    send(WindowEvent::KeyPressed { text: text.into() });
    send(WindowEvent::KeyReleased { text: text.into() });
}

fn key_combo(combo: &str) -> Result<(), String> {
    let (pc, mac) = combo
        .split_once('|')
        .filter(|(pc, mac)| !pc.is_empty() && !mac.is_empty())
        .unwrap_or((combo, combo));
    let combo = if cfg!(target_os = "macos") { mac } else { pc };
    let parts: Vec<&str> = combo.split('+').collect();
    let (mods, last) = parts.split_at(parts.len() - 1);
    let named = |n: &str| -> Option<String> {
        let k = match n.to_lowercase().as_str() {
            "ctrl" | "control" => Key::Control,
            // Slint's logical Control is Command on macOS. Tab cycling uses physical Control.
            "physical-ctrl" if cfg!(target_os = "macos") => Key::Meta,
            "physical-ctrl" => Key::Control,
            "meta" => Key::Meta,
            "shift" => Key::Shift,
            "alt" => Key::Alt,
            "escape" | "esc" => Key::Escape,
            "return" | "enter" => Key::Return,
            "tab" => Key::Tab,
            "delete" => Key::Delete,
            "backspace" => Key::Backspace,
            "left" => Key::LeftArrow,
            "right" => Key::RightArrow,
            "up" => Key::UpArrow,
            "down" => Key::DownArrow,
            "pageup" => Key::PageUp,
            "pagedown" => Key::PageDown,
            "home" => Key::Home,
            "end" => Key::End,
            "f1" => Key::F1,
            "f3" => Key::F3,
            "f5" => Key::F5,
            "f6" => Key::F6,
            "f10" => Key::F10,
            "menu" => Key::Menu,
            "space" => return Some(" ".into()),
            _ => return None,
        };
        Some(char::from(k).to_string())
    };
    let mut held = Vec::new();
    for m in mods {
        let k = named(m).ok_or_else(|| format!("unknown modifier {m}"))?;
        send(WindowEvent::KeyPressed {
            text: k.as_str().into(),
        });
        held.push(k);
    }
    let k = named(last[0]).unwrap_or_else(|| last[0].to_string());
    key(&k);
    for k in held.iter().rev() {
        send(WindowEvent::KeyReleased {
            text: k.as_str().into(),
        });
    }
    Ok(())
}

/// The value of an app property for `assert`.
fn query(_app: &Rc<App>, s: &mut State, ui: &AppWindow, key: &str) -> Result<String, String> {
    if let Some(area) = key.strip_prefix("ink:") {
        return ink(s, ui, area);
    }
    if let Some(v) = crate::script_a11y::query(s, ui, key) {
        return v;
    }
    if let Some(v) = crate::script_stamps::query(s, ui, key) {
        return v;
    }
    if let Some(v) = crate::script_forms::query(s, ui, key) {
        return v;
    }
    if let Some(v) = crate::script_ext::query(s, ui, key) {
        return v;
    }
    if let Some(v) = crate::script_output::query(s, ui, key) {
        return v;
    }
    if let Some(v) = crate::script_os::query(s, ui, key) {
        return v;
    }
    if let Some(v) = crate::app::spell::query(s, ui, key) {
        return v;
    }
    if let Some(v) = crate::app::edit::script::query(s, ui, key) {
        return v;
    }
    if let Some(v) = crate::app::view_script::query(s, ui, key) {
        return v;
    }
    let st = ui.global::<AppState>();
    let (key, arg) = key.split_once(':').unwrap_or((key, ""));
    let tab = s.tabs.get_mut(s.active);
    Ok(match key {
        "tabs" => s.tabs.len().to_string(),
        "vim.active" => ui.get_vim_active().to_string(),
        "vim.mode" => st.get_vim_mode().to_string(),
        "vim.line" => st.get_vim_line().to_string(),
        "mode" => match st.get_mode() {
            Mode::Welcome => "welcome",
            Mode::View => "view",
            Mode::Organize => "organize",
        }
        .into(),
        "tool" => st.get_tool().to_string(),
        "colour" => st.get_colour().to_string(),
        "message" => st.get_message().to_string(),
        "dialog" => format!("{:?}", st.get_dialog()).to_lowercase(),
        "dialog.text" => st.get_dialog_text().to_string(),
        "signatures" => s
            .sign
            .library
            .as_ref()
            .map_or(0, |l| l.signatures().len())
            .to_string(),
        "clipboard" => s.input.copied.clone().unwrap_or_default(),
        "find.status" => st.get_find_status().to_string(),
        "find.selected-result" => ElementHandle::find_by_element_id(ui, "FindResults::keys")
            .find_map(|list| list.accessible_value())
            .unwrap_or_default()
            .to_string(),
        "find.results-visible" => (st.get_find_panel() && st.get_sidebar_shown()).to_string(),
        "find.match-case" => st.get_find_match_case().to_string(),
        "find.whole-words" => st.get_find_whole_words().to_string(),
        "find.visible" => st.get_find_visible().to_string(),
        "editing" => ui.global::<ViewState>().get_editing().to_string(),
        "field.editing" => ui.global::<ViewState>().get_field_editing().to_string(),
        "field.text" => ui.global::<ViewState>().get_field_text().to_string(),
        "form.notice" => st.get_form_notice().to_string(),
        "form.warnings" => st.get_form_warnings().iter().collect::<Vec<_>>().join(" "),
        "fields" => st.get_fields().row_count().to_string(),
        // Sidebar thumbnails that have their picture.
        "thumbs.drawn" => st
            .get_thumbs()
            .iter()
            .filter(|t| t.image.size().width > 0)
            .count()
            .to_string(),
        "fields.text" => st
            .get_fields()
            .iter()
            .map(|f| format!("{}={}", f.name, f.value))
            .collect::<Vec<_>>()
            .join("; "),
        "password.visible" => st.get_password_visible().to_string(),
        "file.exists" => Path::new(arg).exists().to_string(),
        "file.size" => std::fs::metadata(arg)
            .map(|m| m.len())
            .unwrap_or(0)
            .to_string(),
        "settings.author" => s.settings.author.clone(),
        "ui.language" => pagebara_i18n::language().code.to_owned(),
        "ui.rtl" => ui.global::<crate::Theme>().get_rtl().to_string(),
        "settings.language" => s.settings.language.clone().unwrap_or_default(),
        // The sidebar button sits at the right end of the toolbar in a mirrored window.
        "mirrored" => {
            let b = ElementHandle::find_by_element_id(ui, "TopBar::sidebar-btn")
                .find(|e| e.is_valid() && e.size().width > 0.0)
                .ok_or("no sidebar button")?;
            let w = ui
                .window()
                .size()
                .to_logical(ui.window().scale_factor())
                .width;
            (center(&b).0 > f64::from(w) / 2.0).to_string()
        }
        "settings.theme" => format!("{:?}", s.settings.theme).to_lowercase(),
        _ => {
            let t = tab.ok_or("no document is open")?;
            match key {
                "pages" => t.session.page_count().to_string(),
                "title" => t.session.title(),
                "dirty" => t.session.is_dirty().to_string(),
                "encrypted" => t.session.encrypted().to_string(),
                "protection" => t.session.protection(),
                "page" => (t.view.current_page + 1).to_string(),
                "label" => t.session.page_label(t.view.current_page),
                "zoom" => format!("{:.0}", t.view.view.viewport.zoom() * 100.0),
                "outline" => t.session.outline.len().to_string(),
                "outline.text" => t
                    .session
                    .outline
                    .iter()
                    .map(|o| o.title.as_str())
                    .collect::<Vec<_>>()
                    .join(" | "),
                "info" => t
                    .session
                    .info()
                    .rows
                    .into_iter()
                    .find(|(k, _)| k == arg)
                    .map(|(_, v)| v)
                    .unwrap_or_default(),
                "field" => {
                    let backend = s.forms.clone();
                    let f = t
                        .form(&*backend)
                        .field(arg)
                        .ok_or_else(|| format!("no field {arg}"))?;
                    match f.kind {
                        pagebara_app_core::forms::FieldKind::CheckBox => f.checked.to_string(),
                        _ => f.value,
                    }
                }
                "undo" => t.session.undo_label().unwrap_or_default().to_owned(),
                "annotations" => {
                    let mut n = 0;
                    for i in 0..t.session.page_count() {
                        n += t
                            .annots(i)
                            .iter()
                            .filter(|a| pagebara_app_core::annots::is_visible_markup(a))
                            .count();
                    }
                    n.to_string()
                }
                "annotation.kinds" => {
                    let mut kinds = Vec::new();
                    for i in 0..t.session.page_count() {
                        for a in t
                            .annots(i)
                            .iter()
                            .filter(|a| pagebara_app_core::annots::is_visible_markup(a))
                        {
                            kinds.push(pagebara_app_core::annots::kind_label(a).to_owned());
                        }
                    }
                    kinds.join(",")
                }
                "replies" => {
                    let mut n = 0;
                    for i in 0..t.session.page_count() {
                        n += t
                            .annots(i)
                            .iter()
                            .filter(|a| a.in_reply_to.is_some())
                            .count();
                    }
                    n.to_string()
                }
                "selected" => match t.selected {
                    Some(sel) => t
                        .annot(sel)
                        .map(|a| pagebara_app_core::annots::kind_label(&a).to_owned())
                        .unwrap_or_default(),
                    None => String::new(),
                },
                "selected.contents" => t
                    .selected
                    .and_then(|sel| t.annot(sel))
                    .and_then(|a| a.contents)
                    .unwrap_or_default(),
                "selected.rect" => match t.selected.and_then(|sel| t.annot(sel)) {
                    Some(a) => format!(
                        "{:.0},{:.0},{:.0},{:.0}",
                        a.rect.x0, a.rect.y0, a.rect.x1, a.rect.y1
                    ),
                    None => String::new(),
                },
                "selection.text" => match t.text_sel {
                    Some(sel) => pagebara_app_core::text_index::text_of(
                        &t.session.sel_text(sel.page),
                        sel.range(),
                    ),
                    None => String::new(),
                },
                "search.hits" => t.search.hits.len().to_string(),
                "search.current" => t.search.current.map_or(0, |c| c + 1).to_string(),
                "search.done" => t.search.done.to_string(),
                "grid.selected" => t.grid.selected.len().to_string(),
                "comments" => t.comments.len().to_string(),
                "rotation" => {
                    let p: usize = arg.parse().unwrap_or(1);
                    t.session
                        .geoms
                        .get(p.saturating_sub(1))
                        .map_or(0, |g| g.rotate)
                        .to_string()
                }
                "page.size" => {
                    let p: usize = arg.parse().unwrap_or(1);
                    t.session
                        .geoms
                        .get(p.saturating_sub(1))
                        .map(|g| {
                            let (w, h) = g.size();
                            format!("{w:.0}x{h:.0}")
                        })
                        .unwrap_or_default()
                }
                "page.text" => {
                    let p: usize = arg.parse().unwrap_or(1);
                    t.session.text.page(p.saturating_sub(1)).text.clone()
                }
                _ => return Err(format!("unknown property {key}")),
            }
        }
    })
}

/// `ink:P,x,y,w,h`: how many window pixels in that area of page P (displayed points from
/// its top left) are dark, to check that page content is really drawn.
fn ink(s: &mut State, ui: &AppWindow, arg: &str) -> Result<String, String> {
    let v: Vec<f64> = arg
        .split(',')
        .filter_map(|v| v.trim().parse().ok())
        .collect();
    let [page, x, y, w, h] = v[..] else {
        return Err(format!("ink needs page,x,y,w,h: {arg}"));
    };
    let t = s.tab().ok_or("no document is open")?;
    let page = (page as usize).saturating_sub(1);
    let origin = ElementHandle::find_by_element_id(ui, "AppWindow::view")
        .next()
        .map(|e| e.absolute_position())
        .ok_or("the page view is not shown")?;
    let (Some(a), Some(b)) = (
        t.view.page_to_screen(page, x, y),
        t.view.page_to_screen(page, x + w, y + h),
    ) else {
        return Err(format!("page {} is not on screen", page + 1));
    };
    let shot = ui.window().take_snapshot().map_err(|e| e.to_string())?;
    let dpr = f64::from(ui.window().scale_factor());
    let to_px = |v: f64, o: f32| ((v + f64::from(o)) * dpr).round().max(0.0) as u32;
    let (x0, y0) = (to_px(a.0, origin.x), to_px(a.1, origin.y));
    let (x1, y1) = (
        to_px(b.0, origin.x).min(shot.width()),
        to_px(b.1, origin.y).min(shot.height()),
    );
    let px = shot.as_slice();
    let mut dark = 0;
    for yy in y0..y1 {
        for xx in x0..x1 {
            let p = px[(yy * shot.width() + xx) as usize];
            let lum = (u32::from(p.r) * 3 + u32::from(p.g) * 6 + u32::from(p.b)) / 10;
            dark += usize::from(lum < 110);
        }
    }
    Ok(dark.to_string())
}

/// App commands for scripts, mostly those behind native file dialogs.
fn call(
    app: &Rc<App>,
    s: &mut State,
    ui: &AppWindow,
    cmd: &str,
    args: &[String],
) -> Result<(), String> {
    if let Some(r) = crate::script_a11y::call(s, ui, cmd, args) {
        return r;
    }
    if let Some(r) = crate::script_stamps::call(app, s, ui, cmd, args) {
        return r;
    }
    if let Some(r) = crate::script_forms::call(app, s, ui, cmd, args) {
        return r;
    }
    if let Some(r) = crate::script_ext::call(app, s, ui, cmd, args) {
        return r;
    }
    if let Some(r) = crate::script_output::call(app, s, ui, cmd, args) {
        return r;
    }
    if let Some(r) = crate::script_os::call(app, s, ui, cmd, args) {
        return r;
    }
    if let Some(r) = crate::app::edit::script::call(app, s, ui, cmd, args) {
        return r;
    }
    let st = ui.global::<AppState>();
    let arg = |i: usize| -> Result<&str, String> {
        args.get(i)
            .map(String::as_str)
            .ok_or_else(|| format!("{cmd} needs an argument"))
    };
    match cmd {
        "answer-file" => crate::app::pick::answer_next(PathBuf::from(arg(0)?)),
        "save" => app.save(s, ui, None),
        "save-as" | "save-compact" => {
            let mode = if cmd == "save-as" {
                pagebara_app_core::session::SaveMode::Full
            } else {
                pagebara_app_core::session::SaveMode::Compact
            };
            app.start_save(s, ui, PathBuf::from(arg(0)?), mode, None);
        }
        "undo" => app.undo(s, ui, false),
        "redo" => app.undo(s, ui, true),
        "organize" => app.organize(s, ui),
        "vim-keys" => app.set_vim_keys(s, ui, arg(0)? == "on"),
        "organize-done" => app.organize_done(s, ui),
        "organize-action" => app.organize_action(s, ui, arg(0)?),
        "grid-select" => {
            let t = s.tab().ok_or("no document")?;
            let n = t.session.page_count();
            let ranges = pagebara_app_core::page_ops::parse_ranges(arg(0)?, n)?;
            t.grid.selected = ranges.into_iter().flatten().collect();
            app.organize(s, ui);
        }
        "insert" => app.insert_file(s, ui, PathBuf::from(arg(0)?)),
        "extract" => {
            let t = s.tab_ref().ok_or("no document")?;
            let sel: Vec<usize> = t.grid.selected.iter().copied().collect();
            let d = pagebara_pages::extract(t.session.doc(), &sel).map_err(|e| e.to_string())?;
            let b = d.save_full().map_err(|e| e.to_string())?;
            pagebara_app_core::session::write_atomic(Path::new(arg(0)?), &b)
                .map_err(|e| e.to_string())?;
        }
        "split-to" => {
            let t = s.tab_ref().ok_or("no document")?;
            let n = t.session.page_count();
            let ranges = pagebara_app_core::page_ops::parse_ranges(arg(1)?, n)?;
            let docs =
                pagebara_pages::split(t.session.doc(), &ranges).map_err(|e| e.to_string())?;
            for (i, d) in docs.iter().enumerate() {
                let b = d.save_full().map_err(|e| e.to_string())?;
                let p = Path::new(arg(0)?).join(format!("part-{}.pdf", i + 1));
                pagebara_app_core::session::write_atomic(&p, &b).map_err(|e| e.to_string())?;
            }
        }
        "show-dialog" => {
            let kind = match arg(0)? {
                "protect" => DialogKind::Protect,
                "properties" => DialogKind::Properties,
                "settings" => DialogKind::Settings,
                "split" => DialogKind::Split,
                "crop" => DialogKind::Crop,
                k => return Err(format!("unknown dialog {k}")),
            };
            st.invoke_show_dialog(kind);
        }
        "dialog-action" => {
            app.dialog_action(s, ui, arg(0)?, args.get(1).map_or("", String::as_str))
        }
        "protect" => {
            let user = arg(0)?.to_owned();
            let owner = args.get(1).cloned().unwrap_or_default();
            app.dialog_action(s, ui, "protect", &format!("{user}\u{1f}{owner}"));
        }
        "import-signature" => app.import_signature_file(s, ui, PathBuf::from(arg(0)?)),
        "author" => {
            st.set_author(arg(0)?.into());
            app.dialog_action(s, ui, "settings", arg(0)?);
        }
        "language" => {
            let code = arg(0)?;
            let i = pagebara_i18n::LANGUAGES
                .iter()
                .position(|l| l.code == code)
                .map_or(0, |i| i as i32 + 1);
            st.invoke_set_language(i);
        }
        "theme" => {
            let i = match arg(0)? {
                "light" => 1,
                "dark" => 2,
                "high-contrast" => 3,
                "high-contrast-light" => 4,
                _ => 0,
            };
            st.invoke_set_theme(i);
        }
        "page-theme" => {
            let t = pagebara_app_core::page_theme::PageTheme::parse(arg(0)?)
                .ok_or("unknown page theme")?;
            ui.global::<crate::ReadingState>()
                .invoke_set_page_theme(t.index() as i32);
        }
        "set" => {
            let v = arg(1)?;
            let f: f32 = v.parse().unwrap_or(0.0);
            match arg(0)? {
                "colour" => {
                    st.set_colour(f as i32);
                    app.option_changed(s, ui, "colour");
                }
                "shape" => st.set_shape(f as i32),
                "markup" => st.set_markup_kind(f as i32),
                "text-mode" => st.set_text_mode(f as i32),
                "font-size" => st.set_font_size(f),
                "pen-width" => st.set_pen_width(f),
                "sidebar" => st.set_sidebar_tab(match v {
                    "outline" => crate::SidebarTab::Outline,
                    "comments" => crate::SidebarTab::Comments,
                    _ => crate::SidebarTab::Pages,
                }),
                "sig-threshold" => st.set_sig_threshold(f),
                "sig-remove-bg" => st.set_sig_remove_bg(v == "true"),
                "sig-ink" => st.set_sig_ink(f as i32),
                "sig-width" => st.set_sig_width_mm(f),
                "flatten" => st.set_flatten_signatures(v == "true"),
                "flatten-forms" => st.set_flatten_forms(v == "true"),
                p => return Err(format!("unknown property {p}")),
            }
            app.option_changed(s, ui, arg(0)?);
        }
        "selection" => app.selection_action(s, ui, arg(0)?),
        "set-crop" => {
            let m: Vec<f32> = args.iter().map(|a| a.parse().unwrap_or(0.0)).collect();
            st.set_crop_margins(slint::ModelRc::new(slint::VecModel::from(m)));
        }
        "sig-select" => {
            st.invoke_sig_select(arg(0)?.parse().map_err(|_| "sig-select needs a number")?)
        }
        "sig-action" => st.invoke_sig_action(arg(0)?.into()),
        "close-window" => {
            send(WindowEvent::CloseRequested);
        }
        "copy" => app.copy_selection(s),
        "text-scale" => st.invoke_set_text_scale(arg(0)?.parse().map_err(|_| "a number")?),
        "motion" => st.invoke_set_motion(match arg(0)? {
            "reduce" => 1,
            "allow" => 2,
            _ => 0,
        }),
        _ => return crate::app::view_script::call(app, s, ui, cmd, args),
    }
    Ok(())
}
