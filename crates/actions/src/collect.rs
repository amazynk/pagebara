//! Gathering the files of one context-menu click. Windows starts one process per selected
//! file for a command-line verb; for an action the first of them takes the files of the
//! others (over the single-instance channel, keyed by the action) and runs once.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pagebara_os::{Instance, InstanceKey, OpenFile, OpenRequest};

use crate::Action;

/// How long after the last arrival the first process waits for more.
const QUIET: Duration = Duration::from_millis(700);
/// The longest it gathers in all.
const AT_MOST: Duration = Duration::from_secs(10);

/// The files to run `action` on in this process: these and those of other processes
/// started for the same click, or `None` when this process handed its files to another.
/// `runtime_dir` overrides the per-user folder of the channel (tests).
pub fn collect(
    action: Action,
    files: Vec<PathBuf>,
    runtime_dir: Option<&Path>,
) -> Option<Vec<PathBuf>> {
    let name = format!("action-{}", action.name());
    let dir = match runtime_dir {
        Some(d) => d.to_owned(),
        None => match InstanceKey::new(None)
            .and_then(|k| k.socket_path().parent().map(Path::to_owned))
        {
            Some(d) => d,
            None => return Some(files),
        },
    };
    let key = InstanceKey::in_dir(dir, &name);
    let files: Vec<PathBuf> = files
        .into_iter()
        .map(|f| std::path::absolute(&f).unwrap_or(f))
        .collect();
    let request = OpenRequest {
        files: files
            .iter()
            .map(|p| OpenFile {
                path: p.clone(),
                ..OpenFile::default()
            })
            .collect(),
    };
    match Instance::start(&key, Some(&request)) {
        Instance::Forwarded => None,
        Instance::Alone => Some(files),
        Instance::Primary(server) => {
            let start = Instant::now();
            let gathered = Arc::new(Mutex::new(Gathered {
                files,
                last: Instant::now(),
                closed: false,
            }));
            let g = gathered.clone();
            let registration = server.serve(move |req| {
                let mut g = g.lock().unwrap_or_else(|e| e.into_inner());
                let new: Vec<PathBuf> = req
                    .files
                    .into_iter()
                    .map(|f| f.path)
                    .filter(|p| !g.files.contains(p))
                    .collect();
                if g.closed {
                    // Too late for this run: the files get one of their own.
                    run_apart(action, &new);
                } else {
                    g.files.extend(new);
                    g.last = Instant::now();
                }
            });
            loop {
                std::thread::sleep(Duration::from_millis(50));
                let mut g = gathered.lock().unwrap_or_else(|e| e.into_inner());
                if g.last.elapsed() >= QUIET || start.elapsed() >= AT_MOST {
                    g.closed = true;
                    break;
                }
            }
            // The channel stays this process's until it exits, so no second gatherer starts
            // while this one runs (late files are started apart, above).
            std::mem::forget(registration);
            let g = gathered.lock().unwrap_or_else(|e| e.into_inner());
            Some(g.files.clone())
        }
    }
}

#[derive(Debug)]
struct Gathered {
    files: Vec<PathBuf>,
    last: Instant,
    /// The run has started: later files are not part of it.
    closed: bool,
}

/// Starts this program again as `--action NAME FILE...` (without gathering) for files
/// that came after the run started.
fn run_apart(action: Action, files: &[PathBuf]) {
    if files.is_empty() {
        return;
    }
    let r = std::env::current_exe().and_then(|exe| {
        std::process::Command::new(exe)
            .arg("--action")
            .arg(action.name())
            .args(files)
            .spawn()
    });
    if let Err(e) = r {
        log::warn!(
            "{}: could not start a run for late files: {e}",
            action.name()
        );
    }
}
