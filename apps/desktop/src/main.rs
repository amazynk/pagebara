//! pagebara, the desktop app: a Slint window around `pagebara-app-core`.

// Windows: a GUI program, so starting it from Explorer or the Start menu opens no console
// window; `main` attaches to the terminal's console for the command-line modes. Test builds
// stay console programs, so the test harness's output reaches the terminal.
#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

mod app;
mod args;
mod bench;
mod clipboard;
mod cpu;
mod dnd;
mod export;
mod fonts;
mod integration;
mod jobs;
mod keys;
mod links;
mod locale;
mod net;
mod os;
mod page_view;
mod perf;
mod profile;
mod screenshot;
mod script;
mod script_a11y;
mod script_ext;
mod script_forms;
mod script_os;
mod script_output;
mod script_stamps;
mod startup;
mod synthetic;
mod thumbnail;
mod window_state;

use std::io::Write;

use slint::ComponentHandle;

/// The UI compiled from `ui/*.slint`. Generated code: the one place that may use unsafe.
#[allow(unsafe_code, missing_debug_implementations, clippy::all)]
mod ui {
    slint::include_modules!();
}

pub use ui::*;

fn main() {
    // Windows: print to the terminal the app was started from (a GUI program has no console
    // of its own); before anything, so the processor check's message reaches it too.
    let console = pagebara_native::attach_parent_console();
    // First: everything below (and every crate) is compiled for x86-64-v3.
    cpu::require();
    startup::begin();
    init_log();
    startup::mark("logging ready");
    let args = match args::Args::parse(std::env::args_os()) {
        Ok(a) => a,
        Err(args::Exit::Print(text)) => {
            println!("{text}");
            return;
        }
        Err(args::Exit::Usage(msg)) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    if let Some(dir) = &args.make_samples {
        if let Err(e) = pagebara_samples::write_all(dir) {
            eprintln!("could not write the samples to {}: {e}", dir.display());
            std::process::exit(1);
        }
        return;
    }
    if let Some((input, output, size)) = &args.thumbnail {
        std::process::exit(thumbnail::write(input, output, *size));
    }
    if let Some(cmd) = &args.integration {
        std::process::exit(integration::cli(
            cmd,
            &args.files,
            args.json,
            args.profile.clone(),
        ));
    }
    // Windows groups the windows and the jump list under this ID.
    pagebara_native::set_app_user_model_id(pagebara_identity::APP_ID);
    if let Err(e) = pagebara_thread::rayon_pool(pagebara_thread::GLOBAL_POOL).build_global() {
        log::warn!("rayon global pool: {e}");
    }
    if let Some(file) = args.text.as_deref().or(args.images.as_deref()) {
        let e = export::Export {
            file,
            pages: args.pages.as_deref(),
            password: args.password.as_deref(),
            json: args.json,
        };
        let code = if args.text.is_some() {
            export::text(&e)
        } else {
            let dir = args.out.as_deref().unwrap_or(std::path::Path::new("."));
            export::images(&e, dir, args.dpi.unwrap_or(150.0))
        };
        std::process::exit(code);
    }
    if args.action.is_some() {
        std::process::exit(run_action(&args));
    }
    // From here on there is a window, which must not depend on the terminal: Ctrl+C typed
    // there would end the app. Scripted and logged runs keep it, for their output.
    let scripted = args.script.is_some() || args.screenshot.is_some() || args.bench;
    if console && !scripted && std::env::var_os("RUST_LOG").is_none() {
        pagebara_native::detach_console();
    }
    // Only for the window: the command-line modes above report on the terminal, and must
    // never stop on a message box (an installer or a script may be waiting for them).
    note_panics();
    let (files, server) = match os::launch(&args) {
        os::Launch::Forwarded => return,
        os::Launch::Run { files, server } => (files, server),
    };
    startup::mark("instance checked");
    let ended = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run(args, files, server)));
    match ended {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            log::error!("{e}");
            // No window came (no usable renderer, no display): say so where nobody reads logs.
            error_box(&format!(
                "{}\n\n{e}",
                pagebara_i18n::tr!("{} could not start.", pagebara_identity::NAME)
            ));
            std::process::exit(1);
        }
        // A panic that got this far ends the app. Without a terminal nobody would learn
        // why, so a message box says what the report on the terminal says. English: the
        // panic may have come from the translations, or left them unusable.
        Err(panic) => {
            let what = lock(&LAST_PANIC).take().unwrap_or_default();
            error_box(&format!(
                "{} {} has to close because of an internal error.\n\n{what}",
                pagebara_identity::NAME,
                env!("CARGO_PKG_VERSION")
            ));
            std::panic::resume_unwind(panic);
        }
    }
    std::process::exit(script::EXIT.load(std::sync::atomic::Ordering::Acquire));
}

fn run(
    mut args: args::Args,
    files: Vec<pagebara_os::OpenFile>,
    server: Option<pagebara_os::Server>,
) -> Result<(), slint::PlatformError> {
    // FemtoVG on OpenGL unless SLINT_BACKEND says otherwise (see the README of the crate).
    if std::env::var_os("SLINT_BACKEND").is_none() {
        let selector = slint::BackendSelector::new().renderer_name("femtovg".into());
        let selector = if os::background() {
            os::in_background(selector)
        } else {
            selector
        };
        // After the background hook: this one stands in for it.
        let selector = if os::unified_titlebar() {
            os::with_unified_titlebar(selector)
        } else {
            selector
        };
        selector.select()?;
    }
    startup::mark("backend selected");
    fonts::register();
    let cache = match &args.profile {
        Some(p) => Some(p.clone()),
        None => pagebara_identity::cache_dir(),
    };
    pagebara_app_core::text_index::configure(pagebara_app_core::text_index::Config {
        background: true,
        cache_dir: cache.map(|d| d.join("text-index")),
    });
    // Read the files while the window is made (most of that is loading the system fonts).
    let paths: Vec<_> = files.iter().map(|f| f.path.clone()).collect();
    let preopen = app::PreOpen::start(&paths);
    // Each file carries its own start page.
    if !files.is_empty() {
        args.page = None;
    }
    let ui = AppWindow::new()?;
    ui.global::<Keys>()
        .on_label(|text, physical| keys::label(&text, physical).into());
    os::wire_titlebar(&ui);
    // The Wayland app ID / X11 class that desktop files match; read when the window shows.
    if let Err(e) = slint::set_xdg_app_id(pagebara_identity::APP_ID) {
        log::warn!("app ID: {e}");
    }
    startup::mark("window created");
    let app = app::App::start(&ui, args, preopen, files);
    if let Some(server) = server {
        app.listen(server);
    }
    app.install_open_handler();
    startup::mark("app started");
    ui.run()
}

/// `--action TOOL ARGS...`: an output tool (watermark, compare, …) on the command line.
fn run_tool(name: &str, tool_args: &[String]) -> i32 {
    if tool_args.iter().any(|a| a == "-h" || a == "--help") {
        println!("{}", pagebara_actions::PRO_USAGE);
        return 0;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    match pagebara_actions::run_pro(name, tool_args, &cwd) {
        Ok(r) => {
            match &r.json {
                Some(j) => println!("{j}"),
                None => r.outputs.iter().for_each(|o| println!("{}", o.display())),
            }
            for m in &r.messages {
                eprintln!("{m}");
            }
            0
        }
        Err(e) => {
            eprintln!("pagebara: {e}");
            1
        }
    }
}

/// `--action NAME …`, the one entry for every action: file actions without a window
/// (file-manager menus, compress, pdfa, validate) and the output tools with options of
/// their own.
fn run_action(args: &args::Args) -> i32 {
    let name = args.action.as_deref().unwrap_or_default();
    if let Some(tool_args) = &args.tool_args {
        return run_tool(name, tool_args);
    }
    let Some(action) = pagebara_actions::Action::parse(name) else {
        let names: Vec<&str> = pagebara_actions::Action::ALL
            .iter()
            .map(|a| a.name())
            .chain(pagebara_actions::PRO_TOOLS)
            .collect();
        eprintln!("unknown action {name} (one of {})", names.join(", "));
        return 2;
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    let files: Vec<std::path::PathBuf> = args.files.iter().map(|f| cwd.join(f)).collect();
    let files = if args.collect {
        match pagebara_actions::collect(action, files, None) {
            Some(f) => f,
            None => return 0,
        }
    } else {
        files
    };
    let profile = profile::Profile::new(args.profile.clone());
    let notify = profile.settings().system.notifications;
    let opts = pagebara_actions::Options {
        output: args.out.clone(),
        password: args.password.clone(),
        dpi: args.dpi,
        models_dir: profile.models_dir.clone(),
        optimize: args.optimize.map(|mut o| {
            // `--dpi N` alone downsamples to N.
            if o.images.target_ppi.is_none() {
                o.images.target_ppi = args.dpi;
            }
            o
        }),
        conformance: args.conformance,
        json: args.json,
    };
    match pagebara_actions::run(action, &files, &opts, &|_, _| {}) {
        Ok(report) => {
            for o in &report.outputs {
                println!("{}", o.display());
            }
            for m in &report.messages {
                eprintln!("{m}");
            }
            for p in &report.printed {
                println!("{p}");
            }
            if action == pagebara_actions::Action::Validate {
                return i32::from(report.failed);
            }
            let body = match report.outputs.as_slice() {
                [one] => pagebara_i18n::tr!("Wrote {}", one.display()),
                many => pagebara_i18n::trn!("Wrote {n} file", "Wrote {n} files", many.len()),
            };
            jobs::notify(&profile, notify, action.title(), &body);
            0
        }
        Err(e) => {
            eprintln!("pagebara: {e}");
            jobs::notify(
                &profile,
                notify,
                &pagebara_i18n::tr!("{} failed", action.title()),
                &e,
            );
            1
        }
    }
}

/// Windows: `text` in an error message box, when there is no terminal to print it to
/// (started from Explorer or the Start menu, or let go of the terminal for the window).
fn error_box(text: &str) {
    if !pagebara_native::has_terminal() {
        pagebara_native::error_box(
            &pagebara_native::wide(pagebara_identity::NAME),
            &pagebara_native::wide(text),
        );
    }
}

/// The message and place of the last panic on the main thread, for the message box `main`
/// shows when that panic ends the app.
static LAST_PANIC: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Keeps what a panic on the main thread said, besides the usual report. Only kept, not
/// shown: a panic is not always the end (a page drawn on this thread catches its
/// decoder's panic and carries on), and `main` knows when it is.
fn note_panics() {
    let main = std::thread::current().id();
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        if std::thread::current().id() == main {
            let what = info
                .payload()
                .downcast_ref::<&str>()
                .copied()
                .or_else(|| info.payload().downcast_ref::<String>().map(String::as_str))
                .unwrap_or("unknown error");
            let at = info
                .location()
                .map(|l| format!("\n\n{}:{}", l.file(), l.line()))
                .unwrap_or_default();
            *lock(&LAST_PANIC) = Some(format!("{what}{at}"));
        }
        default(info);
    }));
}

fn init_log() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format(|f, r| {
            let thread = std::thread::current();
            writeln!(
                f,
                "{} {:5} [{}] {}",
                f.timestamp_millis(),
                r.level(),
                thread.name().unwrap_or("?"),
                r.args()
            )
        })
        .init();
}
