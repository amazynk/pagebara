//! Files named on the command line are read while the window is being made, so the first
//! frame can show the document instead of the welcome screen.

use std::path::PathBuf;
use std::rc::Rc;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use pagebara_app_core::session::{self, OpenError, Opened};
use pagebara_i18n::tr;
use slint::ComponentHandle;

use super::{App, post};
use crate::AppState;

/// How long after starting to read the files the first frame may wait for them.
const WAIT_FOR: Duration = Duration::from_millis(150);

type Outcome = Result<Opened, OpenError>;

#[derive(Debug)]
pub struct PreOpen {
    started: Instant,
    files: Vec<(PathBuf, Receiver<Outcome>)>,
}

impl PreOpen {
    /// Starts reading `files` (each once) on background threads. With files to show, also
    /// scans the system fonts now, as their pages may need substitutes.
    pub fn start(files: &[PathBuf]) -> PreOpen {
        let mut seen = Vec::new();
        let mut out = Vec::new();
        for path in files {
            let abs = std::path::absolute(path).ok();
            if seen.contains(&abs) {
                continue;
            }
            seen.push(abs);
            let (tx, rx) = channel();
            let p = path.clone();
            pagebara_thread::spawn("io-open", move || {
                crate::startup::mark("reading the document");
                let _ = tx.send(session::open_file(&p));
                crate::startup::mark("document parsed");
            });
            out.push((path.clone(), rx));
        }
        if !out.is_empty() {
            warm_fonts();
        }
        PreOpen {
            started: Instant::now(),
            files: out,
        }
    }

    /// Shows the files read by now as tabs; the others open when they are ready.
    pub fn finish(self, app: &Rc<App>) {
        let deadline = self.started + WAIT_FOR;
        for (path, rx) in self.files {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(result) => {
                    app.state.borrow_mut().opening = Some(self.started);
                    app.opened(Some(path), result);
                }
                Err(RecvTimeoutError::Timeout) => {
                    let started = self.started;
                    app.act(move |app, s, ui| {
                        ui.global::<AppState>().set_busy(true);
                        s.opening = Some(started);
                        app.message(s, &tr!("Opening…"));
                    });
                    pagebara_thread::spawn("io-open-wait", move || {
                        if let Ok(result) = rx.recv() {
                            post(move |app| app.opened(Some(path), result));
                        }
                    });
                }
                Err(RecvTimeoutError::Disconnected) => {
                    log::error!("opening {} failed", path.display());
                }
            }
        }
    }
}

/// Scans the system fonts on a background thread (see `DefaultGlyphs::warm_up`).
pub fn warm_fonts() {
    pagebara_thread::spawn("font-scan", pagebara_render::DefaultGlyphs::warm_up);
}
