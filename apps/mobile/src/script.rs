//! `--script FILE` on desktop hosts: drives the phone UI with a timed list of actions (the
//! format of `pagebara_app_core::script`), for UI tests and screenshots. Taps, drags and
//! strokes go through the window's event queue like touches; two-finger pinches are
//! `call pinch SCALE`. Exits with status 1 when an assertion failed.

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
use crate::platform::Event;
use crate::{AppState, MobileWindow, Sheet, Tool};

static EXIT: AtomicI32 = AtomicI32::new(0);

/// The exit status once the script ended.
pub fn exit_code() -> i32 {
    EXIT.load(Ordering::Acquire)
}

const TICK: Duration = Duration::from_millis(16);
/// Longest wait for a document to open or a target to appear.
const PATIENCE: Duration = Duration::from_secs(20);

struct Runner {
    steps: Vec<Step>,
    at: usize,
    started: Option<Instant>,
    cursor: (f32, f32),
    /// The motion of the current step: window points along the way.
    path: Vec<(f64, f64)>,
    typed: usize,
    zoom_from: f64,
    shots: PathBuf,
    failures: usize,
    timer: Timer,
}

pub(crate) fn start(app: &Rc<App>, file: &Path) {
    let fail = |e: String| {
        log::error!("script {}: {e}", file.display());
        EXIT.store(2, Ordering::Release);
        let _ = slint::quit_event_loop();
    };
    let src = match std::fs::read_to_string(file) {
        Ok(s) => s,
        Err(e) => return fail(e.to_string()),
    };
    let dir = file.parent().map(Path::to_owned).unwrap_or_default();
    let env = |name: &str| std::env::var(name).ok();
    let steps = match script::parse(&src, &env) {
        Ok(s) => s,
        Err(e) => return fail(e),
    };
    let shots = std::env::var_os("PAGEBARA_SHOTS")
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.join("shots"));
    let runner = Rc::new(RefCell::new(Runner {
        steps,
        at: 0,
        started: None,
        cursor: (0.0, 0.0),
        path: Vec::new(),
        typed: 0,
        zoom_from: 1.0,
        shots,
        failures: 0,
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
    /// Input made while the app state is borrowed; delivered at the end of the tick.
    static QUEUE: RefCell<Vec<WindowEvent>> = const { RefCell::new(Vec::new()) };
}

fn send(e: WindowEvent) {
    QUEUE.with(|q| q.borrow_mut().push(e));
}

fn tick(app: &Rc<App>, runner: &Rc<RefCell<Runner>>) {
    let Some(ui) = app.ui.upgrade() else { return };
    steps(app, runner, &ui);
    for e in QUEUE.with(|q| std::mem::take(&mut *q.borrow_mut())) {
        ui.window().dispatch_event(e);
    }
}

fn steps(app: &Rc<App>, runner: &Rc<RefCell<Runner>>, ui: &MobileWindow) {
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
        let ctx = Ctx {
            app,
            ui,
            first,
            t: t.min(1.0),
            elapsed: now - started,
        };
        let done = match run(&ctx, &mut s, &mut r, &step) {
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
        if step.ms > 0 || matches!(step.action, Action::Key(_) | Action::Type(_)) {
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

fn move_to(r: &mut Runner, x: f64, y: f64) {
    r.cursor = (x as f32, y as f32);
    send(WindowEvent::PointerMoved {
        position: LogicalPosition::new(x as f32, y as f32),
    });
}

fn press(r: &Runner, down: bool) {
    let position = LogicalPosition::new(r.cursor.0, r.cursor.1);
    let button = PointerEventButton::Left;
    send(if down {
        WindowEvent::PointerPressed { position, button }
    } else {
        WindowEvent::PointerReleased { position, button }
    });
}

struct Ctx<'a> {
    app: &'a Rc<App>,
    ui: &'a MobileWindow,
    first: bool,
    t: f64,
    elapsed: Duration,
}

fn center(e: &ElementHandle) -> (f64, f64) {
    let p = e.absolute_position();
    let z = e.size();
    (
        f64::from(p.x + z.width / 2.0),
        f64::from(p.y + z.height / 2.0),
    )
}

/// Where a target is in the window, or `None` while it is not there yet.
fn resolve(c: &Ctx, s: &mut State, r: &Runner, t: &Target) -> Result<Option<(f64, f64)>, String> {
    Ok(match t {
        Target::Window(x, y) => Some((f64::from(*x), f64::from(*y))),
        Target::Here => Some((f64::from(r.cursor.0), f64::from(r.cursor.1))),
        Target::Misspelling(n) => {
            if let Some((page, x, y)) = crate::app::spell::page_word(s, n.saturating_sub(1)) {
                return resolve(c, s, r, &Target::Page { page, x, y });
            }
            spell_marks(c.ui).get(n.saturating_sub(1)).map(center)
        }
        Target::Label { label, nth } => {
            let found: Vec<ElementHandle> = ElementHandle::find_by_accessible_label(c.ui, label)
                .filter(|e| e.is_valid() && e.size().width > 0.0 && e.computed_opacity() > 0.0)
                .collect();
            found.get(*nth).map(center)
        }
        Target::Id(id) => ElementHandle::find_by_element_id(c.ui, id)
            .find(|e| e.is_valid() && e.size().width > 0.0)
            .map(|e| center(&e)),
        Target::Text { page, text, end } => {
            let d = s.doc.as_mut().ok_or("no document is open")?;
            let tp = d.session.sel_text(*page);
            let hit = text_index::find(&tp, text)
                .into_iter()
                .next()
                .ok_or_else(|| format!("“{text}” is not on page {}", page + 1))?;
            let boxes = text_index::line_boxes(&tp, text_index::chars_of(&tp, hit));
            let b = if *end { boxes.last() } else { boxes.first() }.ok_or("no text box")?;
            let [x, y, w, h] = d.session.geoms[*page].rect_to_view(b.map(f64::from));
            let x = if *end { x + w - 0.5 } else { x + 0.5 };
            let at = Target::Page {
                page: *page,
                x,
                y: y + h / 2.0,
            };
            return resolve(c, s, r, &at);
        }
        Target::Annot { page, n } => {
            let d = s.doc.as_mut().ok_or("no document is open")?;
            let list = d.annots(*page);
            let a = list
                .get(n.saturating_sub(1))
                .ok_or_else(|| format!("page {} has no annotation {n}", page + 1))?;
            let g = d.session.geoms[*page];
            let [x, y, w, h] = g.rect_to_view([a.rect.x0, a.rect.y0, a.rect.x1, a.rect.y1]);
            let at = Target::Page {
                page: *page,
                x: x + w / 2.0,
                y: y + h / 2.0,
            };
            return resolve(c, s, r, &at);
        }
        Target::Object { page, n } => {
            let d = s.doc.as_mut().ok_or("no document is open")?;
            let (x, y) = pagebara_app_core::object_edit::object_center(d.session.doc(), *page, *n)?;
            return resolve(c, s, r, &Target::Page { page: *page, x, y });
        }
        Target::Handle(h) => return Err(format!("no handle {h} on a phone")),
        Target::Page { page, x, y } => {
            let d = s.doc.as_mut().ok_or("no document is open")?;
            if d.view.animating() {
                return Ok(None);
            }
            if *page >= d.session.page_count() {
                return Err(format!("there is no page {}", page + 1));
            }
            let origin = ElementHandle::find_by_element_id(c.ui, "MobileWindow::pages")
                .next()
                .map(|e| e.absolute_position())
                .ok_or("the page view is not shown")?;
            let (_, vh) = d.view.view.viewport.size();
            match d.view.page_to_screen(*page, *x, *y) {
                Some((px, py)) if py >= 0.0 && py <= vh => {
                    Some((px + f64::from(origin.x), py + f64::from(origin.y)))
                }
                _ => {
                    d.view.glide_to(*page, (*y - 150.0).max(0.0), 0.0);
                    c.app.animate(s, c.ui);
                    None
                }
            }
        }
    })
}

fn wait_for(elapsed: Duration, t: &Target) -> Result<bool, String> {
    if elapsed > PATIENCE {
        Err(format!("target {t:?} did not appear"))
    } else {
        Ok(false)
    }
}

fn run(c: &Ctx, s: &mut State, r: &mut Runner, step: &Step) -> Result<bool, String> {
    let st = c.ui.global::<AppState>();
    let t = c.t;
    match &step.action {
        Action::Open(path) => {
            if c.first {
                c.app.open_path(PathBuf::from(path));
                return Ok(false);
            }
            let same = |p: &PathBuf| std::path::absolute(p).ok() == std::path::absolute(path).ok();
            let shown = s
                .doc
                .as_ref()
                .and_then(|d| d.session.path.as_ref())
                .is_some_and(same);
            if (shown || st.get_password_needed()) && !st.get_busy() {
                return Ok(true);
            }
            if c.elapsed > PATIENCE {
                return Err(format!("{path} did not open"));
            }
            Ok(false)
        }
        Action::Wait | Action::Cursor(_) => Ok(t >= 1.0),
        Action::Tool(name) => {
            let tool = match name.as_str() {
                "read" => Tool::Read,
                "highlight" => Tool::Highlight,
                "draw" => Tool::Draw,
                "sign" => Tool::Sign,
                "forms" => Tool::Forms,
                "edit" => Tool::Edit,
                "prepare" => Tool::Prepare,
                _ => return Err(format!("unknown tool {name}")),
            };
            st.set_tool(tool);
            c.app.tool_changed(s, c.ui);
            Ok(true)
        }
        Action::Move(target) | Action::Click { target, .. } => {
            if r.path.is_empty() {
                let Some(to) = resolve(c, s, r, target)? else {
                    return wait_for(c.elapsed, target);
                };
                r.path = vec![(f64::from(r.cursor.0), f64::from(r.cursor.1)), to];
                r.started = Some(Instant::now());
                if step.ms > 0 {
                    return Ok(false);
                }
            }
            let (x, y) = along(&r.path, ease(t));
            move_to(r, x, y);
            if t < 1.0 {
                return Ok(false);
            }
            if let Action::Click { count, .. } = &step.action {
                for _ in 0..*count {
                    press(r, true);
                    press(r, false);
                }
            }
            Ok(true)
        }
        Action::Release => {
            press(r, false);
            Ok(true)
        }
        Action::Drag(..) | Action::Hold(..) | Action::Stroke { .. } | Action::StrokeAt { .. } => {
            if r.path.is_empty() {
                let path = match &step.action {
                    Action::Drag(a, b) | Action::Hold(a, b) => {
                        let Some(from) = resolve(c, s, r, a)? else {
                            return wait_for(c.elapsed, a);
                        };
                        let Some(to) = resolve(c, s, r, b)? else {
                            return wait_for(c.elapsed, b);
                        };
                        vec![from, to]
                    }
                    Action::Stroke { page, points } => {
                        let mut path = Vec::new();
                        for &(x, y) in points {
                            let target = Target::Page { page: *page, x, y };
                            let Some(p) = resolve(c, s, r, &target)? else {
                                return wait_for(c.elapsed, &target);
                            };
                            path.push(p);
                        }
                        path
                    }
                    Action::StrokeAt { target, points } => {
                        let Some(o) = resolve(c, s, r, target)? else {
                            return wait_for(c.elapsed, target);
                        };
                        points
                            .iter()
                            .map(|&(dx, dy)| (o.0 + dx, o.1 + dy))
                            .collect()
                    }
                    _ => unreachable!(),
                };
                move_to(r, path[0].0, path[0].1);
                press(r, true);
                r.path = path;
                r.started = Some(Instant::now());
                return Ok(false);
            }
            // A finger moves at an even pace.
            let (x, y) = along(&r.path, t);
            move_to(r, x, y);
            if t < 1.0 {
                return Ok(false);
            }
            // `drag-hold` keeps the finger down until `release`.
            if !matches!(step.action, Action::Hold(..)) {
                press(r, false);
            }
            Ok(true)
        }
        Action::Type(text) => {
            let n = text.chars().count().max(1);
            let due = ((t * n as f64).ceil() as usize).min(n);
            if c.first {
                r.typed = 0;
            }
            for ch in text.chars().skip(r.typed).take(due.saturating_sub(r.typed)) {
                key(&ch.to_string());
            }
            r.typed = due;
            Ok(t >= 1.0)
        }
        Action::Key(name) => {
            let k = match name.to_lowercase().as_str() {
                "back" => char::from(Key::Back).to_string(),
                "escape" | "esc" => char::from(Key::Escape).to_string(),
                "return" | "enter" => char::from(Key::Return).to_string(),
                "backspace" => char::from(Key::Backspace).to_string(),
                _ => name.clone(),
            };
            key(&k);
            Ok(true)
        }
        Action::Scroll(dy) => {
            let d = s.doc.as_mut().ok_or("no document is open")?;
            if c.first {
                r.zoom_from = d.view.view.viewport.scroll().1;
            }
            let (x, _) = d.view.view.viewport.scroll();
            d.view
                .view
                .viewport
                .scroll_to(x, r.zoom_from + dy * ease(t));
            c.app.refresh(s, c.ui);
            Ok(t >= 1.0)
        }
        Action::Zoom(z) => {
            let d = s.doc.as_mut().ok_or("no document is open")?;
            if c.first {
                r.zoom_from = d.view.view.viewport.zoom();
            }
            let zz = r.zoom_from * (z / r.zoom_from).powf(ease(t));
            d.view.zoom_to(zz);
            c.app.refresh(s, c.ui);
            Ok(t >= 1.0)
        }
        Action::Page(p) => {
            st.invoke_go_to_page(*p as i32);
            Ok(true)
        }
        Action::Screenshot(name) => {
            if pagebara_ui::snapshot::take(c.ui.window(), &r.shots.join(name)) {
                Ok(true)
            } else {
                Err(format!("screenshot {name} failed"))
            }
        }
        Action::Assert { key, op, value } => {
            let actual = query(s, c.ui, key)?;
            if op.holds(&actual, value) {
                log::info!("ok: {key} {op} {value}");
                Ok(true)
            } else if c.elapsed < Duration::from_millis(step.ms.max(1500)) {
                Ok(false)
            } else {
                Err(format!("assert {key} {op} {value}: got “{actual}”"))
            }
        }
        Action::Call(cmd, args) => {
            call(c, s, cmd, args)?;
            Ok(true)
        }
        Action::Quit => {
            r.at = r.steps.len();
            Ok(true)
        }
        Action::RightClick(_) => Err("phones have no right click: tap the word".into()),
    }
}

fn key(text: &str) {
    send(WindowEvent::KeyPressed { text: text.into() });
    send(WindowEvent::KeyReleased { text: text.into() });
}

fn sheet_name(s: Sheet) -> &'static str {
    match s {
        Sheet::None => "none",
        Sheet::Pages => "pages",
        Sheet::Signatures => "signatures",
        Sheet::More => "more",
        Sheet::DrawPad => "draw-pad",
        Sheet::Choices => "choices",
        Sheet::Tools => "tools",
        Sheet::FieldProps => "field-props",
        Sheet::Language => "language",
        Sheet::Licences => "licences",
    }
}

/// The value of an app property for `assert`.
/// The underlines shown under the form field's misspelled words.
fn spell_marks(ui: &MobileWindow) -> Vec<ElementHandle> {
    ElementHandle::find_by_element_id(ui, "FieldEditor::field-marks")
        .flat_map(|m| m.query_descendants().match_id("Mark::word").find_all())
        .filter(|e| e.is_valid() && e.size().width > 0.0)
        .collect()
}

fn query(s: &mut State, ui: &MobileWindow, key: &str) -> Result<String, String> {
    if let Some(ids) = key.strip_prefix("gap-y:") {
        let (above, below) = ids.split_once(',').ok_or("gap-y needs two element ids")?;
        let element = |id: &str| {
            ElementHandle::find_by_element_id(ui, id)
                .find(|e| e.is_valid() && e.size().height > 0.0)
                .ok_or_else(|| format!("cannot find {id}"))
        };
        let above = element(above)?;
        let gap = element(below)?.absolute_position().y
            - above.absolute_position().y
            - above.size().height;
        return Ok(gap.to_string());
    }
    if let Some(v) = spell_query(s, ui, key) {
        return Ok(v);
    }
    let st = ui.global::<AppState>();
    let simple = match key {
        "reading" => Some(st.get_reading().to_string()),
        "tablet" => Some(st.get_tablet().to_string()),
        "sheet" => Some(sheet_name(st.get_sheet()).into()),
        "tool" => Some(format!("{:?}", st.get_tool()).to_lowercase()),
        "message" => Some(st.get_message().to_string()),
        "busy" => Some(st.get_busy().to_string()),
        "tools.output" => Some(
            s.tools_output
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        ),
        "tools.output-exists" => Some(
            s.tools_output
                .as_ref()
                .is_some_and(|p| p.exists())
                .to_string(),
        ),
        "access.errors" => Some(s.tools_access.map_or(0, |a| a.0).to_string()),
        "access.fixable" => Some(s.tools_access.map_or(0, |a| a.1).to_string()),
        "offer" => Some(st.get_offer().to_string()),
        "signatures" => Some(st.get_signatures().row_count().to_string()),
        "selection" => Some(st.get_has_selection().to_string()),
        "selected" => Some(st.get_has_selected_annot().to_string()),
        "field.editing" => Some(st.get_field_editing().to_string()),
        "password" => Some(st.get_password_needed().to_string()),
        "find.status" => Some(st.get_find_status().to_string()),
        "recent" => Some(st.get_recent().row_count().to_string()),
        "undo" => s
            .doc
            .as_ref()
            .map(|d| d.session.undo_label().unwrap_or_default().to_owned()),
        _ => None,
    };
    if let Some(v) = simple {
        return Ok(v);
    }
    if let Some(p) = key.strip_prefix("file.exists:") {
        return Ok(Path::new(p).exists().to_string());
    }
    match key {
        "clipboard" => return Ok(s.input.copied.clone().unwrap_or_default()),
        "live.available" => return Ok(pagebara_app_core::live_text::available().to_string()),
        "edit.text-editing" => return Ok(st.get_edit_text_editing().to_string()),
        "edit.text-bold" => return Ok(st.get_edit_text_bold().to_string()),
        "edit.bar" => return Ok(st.get_edit_selected().to_string()),
        "edit.description" => return Ok(st.get_edit_description().to_string()),
        "page-theme" => return Ok(s.settings.page_theme.name().to_owned()),
        "ui.language" => return Ok(pagebara_i18n::language().code.to_owned()),
        "ui.rtl" => return Ok(ui.global::<crate::Theme>().get_rtl().to_string()),
        "offer.state" => return Ok(s.live.state().to_owned()),
        "prep.type" => return Ok(st.get_prep_type().to_string()),
        "prep.found" => return Ok(st.get_prep_found().to_string()),
        "prep.bar" => return Ok(st.get_prep_description().to_string()),
        "prop.name" => return Ok(st.get_prop_name().to_string()),
        "prop.tooltip" => return Ok(st.get_prop_tooltip().to_string()),
        _ => {}
    }
    let d = s.doc.as_mut().ok_or("no document is open")?;
    if let Some(v) = crate::app::edit_query(d, key)? {
        return Ok(v);
    }
    if key == "prep.selected" {
        return Ok(d
            .prep
            .sel
            .as_ref()
            .map(|s| s.name.clone())
            .unwrap_or_default());
    }
    if key == "fields" {
        let backend = s.forms.clone();
        return Ok(d.form(&*backend).fields().len().to_string());
    }
    // Field properties for Prepare form: `field.kind:NAME` and the like.
    if let Some((what, name)) = key.strip_prefix("field.").and_then(|k| k.split_once(':')) {
        let backend = s.forms.clone();
        let doc = d.session.doc().clone();
        let form = d.form(&*backend);
        let f = form.field(name).ok_or_else(|| format!("no field {name}"))?;
        let p = form
            .props(&doc, name)
            .ok_or_else(|| format!("no field {name}"))?;
        return Ok(match what {
            "kind" => f.kind.as_str().to_owned(),
            "label" => f.label,
            "required" => p.required.to_string(),
            "read-only" => p.read_only.to_string(),
            "multiline" => f.multiline.to_string(),
            "font-size" => p.font_size.to_string(),
            "options" => f.options.join(", "),
            "widgets" => f.widgets.len().to_string(),
            "exports" => f
                .widgets
                .iter()
                .filter_map(|w| w.export.clone())
                .collect::<Vec<_>>()
                .join(", "),
            "rect" => f
                .widgets
                .first()
                .map(|w| {
                    format!(
                        "{:.0},{:.0},{:.0},{:.0}",
                        w.rect[0], w.rect[1], w.rect[2], w.rect[3]
                    )
                })
                .unwrap_or_default(),
            "date" => matches!(p.format, pagebara_app_core::forms::Format::Date(_)).to_string(),
            o => return Err(format!("unknown field property {o}")),
        });
    }
    if let Some(name) = key.strip_prefix("field:") {
        let backend = s.forms.clone();
        let f = d.form(&*backend).field(name);
        return f
            .map(|f| if f.checked { "on".into() } else { f.value })
            .ok_or_else(|| format!("no field {name}"));
    }
    let page_arg = |p: &str| p.parse::<usize>().unwrap_or(1).saturating_sub(1);
    if let Some(p) = key.strip_prefix("live.words:") {
        return Ok(d
            .session
            .live_ref()
            .map_or(0, |l| l.words(page_arg(p)))
            .to_string());
    }
    Ok(match key {
        "selection.text" => match d.text_sel {
            Some(sel) => text_index::text_of(&d.session.sel_text(sel.page), sel.range()),
            None => String::new(),
        },
        "pages" => d.session.page_count().to_string(),
        "page" => (d.view.current_page + 1).to_string(),
        "title" => d.session.title(),
        "dirty" => d.session.is_dirty().to_string(),
        "zoom" => format!("{:.2}", d.view.view.viewport.zoom()),
        "fit-width" => d.view.is_fit_width().to_string(),
        "annotations" => (0..d.session.page_count())
            .map(|i| {
                d.annots(i)
                    .iter()
                    .filter(|a| pagebara_app_core::annots::is_visible_markup(a))
                    .count()
            })
            .sum::<usize>()
            .to_string(),
        "search.hits" => d.search.hits.len().to_string(),
        "search.current" => d.search.current.map_or(0, |c| c + 1).to_string(),
        "tiles" => d.view.last.tiles.len().to_string(),
        "missing" => d.view.last.missing.to_string(),
        _ => return Err(format!("unknown query {key}")),
    })
}

/// App commands by name.
fn call(c: &Ctx, s: &mut State, cmd: &str, args: &[String]) -> Result<(), String> {
    let st = c.ui.global::<AppState>();
    let arg = |i: usize| {
        args.get(i)
            .ok_or_else(|| format!("{cmd} needs an argument"))
    };
    let num = |i: usize| -> Result<f64, String> {
        arg(i)?.parse().map_err(|_| format!("{cmd}: not a number"))
    };
    match cmd {
        "back" => {
            c.app.back(s, c.ui);
        }
        "undo" => st.invoke_undo(),
        "redo" => st.invoke_redo(),
        "dark" => c.ui.global::<crate::Theme>().set_dark(arg(0)? == "on"),
        "page-theme" => {
            let t = pagebara_app_core::page_theme::PageTheme::parse(arg(0)?)
                .ok_or("unknown page theme")?;
            c.app.set_page_theme(s, c.ui, t);
        }
        "language" => {
            let code = arg(0)?;
            let i = pagebara_i18n::LANGUAGES
                .iter()
                .position(|l| l.code == code.as_str())
                .map_or(0, |i| i as i32 + 1);
            st.invoke_set_language(i);
        }
        "save" => st.invoke_save(),
        "optimize" => st.invoke_optimize(arg(0)?.parse().map_err(|_| "preset 0-2")?),
        "save-pdfa" => st.invoke_save_pdfa(),
        "check-access" => st.invoke_check_access(),
        "fix-access" => st.invoke_fix_access(),
        "find" => {
            st.set_searching(true);
            st.set_find_text(arg(0)?.as_str().into());
            c.app.find(s, c.ui, arg(0)?);
        }
        "find-next" => c.app.find_next(s, c.ui, true),
        "sheet" => {
            let sheet = match arg(0)?.as_str() {
                "none" => Sheet::None,
                "pages" => Sheet::Pages,
                "signatures" => Sheet::Signatures,
                "more" => Sheet::More,
                "tools" => Sheet::Tools,
                other => return Err(format!("unknown sheet {other}")),
            };
            st.set_sheet(sheet);
            st.invoke_sheet_changed();
        }
        // A photo picked for a signature, without the platform's picker.
        "signature-photo" => {
            c.app
                .platform_event(s, c.ui, Event::Image(PathBuf::from(arg(0)?)));
        }
        "unlock" => st.invoke_unlock(arg(0)?.as_str().into()),
        // Two fingers on the pages: the scale, at a point of the view (its centre by
        // default), in `steps` updates.
        "pinch" => {
            let scale = num(0)?;
            let (w, h) = s.view_size;
            let (x, y) = (
                args.get(1).and_then(|v| v.parse().ok()).unwrap_or(w / 2.0),
                args.get(2).and_then(|v| v.parse().ok()).unwrap_or(h / 2.0),
            );
            c.app.pinch_start(s, c.ui);
            for i in 1..=8 {
                let f = f64::from(i) / 8.0;
                c.app.pinch(s, c.ui, 1.0 + (scale - 1.0) * f, x, y);
            }
            c.app.pinch_end(s, c.ui);
        }
        // Edit mode: object N of the current page selected without a tap; the selection
        // pinched about its centre (scale, then degrees); text being edited selected
        // (character offsets: caret, then anchor); the selected image replaced.
        "edit-select" => {
            let page = s
                .doc
                .as_ref()
                .ok_or("no document is open")?
                .view
                .current_page;
            c.app.edit_select_nth(s, c.ui, page, num(0)? as usize)?;
        }
        "edit-pinch" => {
            let rot = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(0.0);
            c.app.edit_pinch_script(s, c.ui, num(0)?, rot)?;
        }
        "edit-caret" => {
            let caret = num(0)? as usize;
            let anchor = args.get(1).and_then(|v| v.parse().ok()).unwrap_or(caret);
            c.app.edit_set_caret(s, c.ui, caret, anchor);
        }
        "replace-image" => c.app.edit_replace_image(s, c.ui, Path::new(arg(0)?)),
        // Prepare form: a field selected without a tap; a property of the sheet set as if
        // typed or switched; form data picked for "Import form data".
        "prep-select" => c.app.prep_select(s, c.ui, arg(0)?)?,
        "prop-set" => {
            let v = arg(1)?.as_str();
            match arg(0)?.as_str() {
                "name" => st.set_prop_name(v.into()),
                "tooltip" => st.set_prop_tooltip(v.into()),
                "font-size" => st.set_prop_font_size(v.into()),
                "options" => st.set_prop_options(v.into()),
                "export" => st.set_prop_export(v.into()),
                "required" => st.set_prop_required(v == "true"),
                "read-only" => st.set_prop_read_only(v == "true"),
                "multiline" => st.set_prop_multiline(v == "true"),
                o => return Err(format!("unknown property {o}")),
            }
        }
        // `output` stands for the copy the tools wrote last.
        "form-data" => {
            let path = match arg(0)?.as_str() {
                "output" => s.tools_output.clone().ok_or("no copy was written")?,
                p => PathBuf::from(p),
            };
            c.app.platform_event(s, c.ui, Event::Data(path));
        }
        "fling" => {
            let d = s.doc.as_mut().ok_or("no document is open")?;
            d.view.fling((0.0, -num(0)?));
            c.app.animate(s, c.ui);
        }
        _ => return Err(format!("unknown command {cmd}")),
    }
    Ok(())
}

/// `spell.words`, `spell.marks`, `spell.drawn`, `spell.word` (the bar's), `spell.suggestions`,
/// `spell.enabled`, `spell.offer` and `spell.dictionary`.
fn spell_query(s: &State, ui: &MobileWindow, key: &str) -> Option<String> {
    let sp = ui.global::<crate::SpellState>();
    let page = crate::app::spell::page_words(s);
    let words = page.unwrap_or_else(|| s.spell.field_words());
    Some(match key {
        "spell.words" => words.join(" "),
        "spell.marks" => words.len().to_string(),
        "spell.drawn" => match crate::app::spell::page_words(s) {
            Some(w) => w.len().to_string(),
            None => spell_marks(ui).len().to_string(),
        },
        "spell.word" => sp.get_word().to_string(),
        "spell.suggestions" => sp.get_suggestions().iter().collect::<Vec<_>>().join(", "),
        "spell.enabled" => sp.get_enabled().to_string(),
        "spell.offer" => sp.get_offer().to_string(),
        "spell.dictionary" => s.spell.personal_words().join(" "),
        _ => return None,
    })
}
