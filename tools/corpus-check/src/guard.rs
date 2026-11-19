//! Records every panic, caught or not, with its location.

use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;

static PANICS: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Replaces the panic hook with one that records instead of printing.
pub fn install() {
    panic::set_hook(Box::new(|info| {
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "?".to_owned());
        let at = info
            .location()
            .map(|l| format!("{}:{}", l.file(), l.line()))
            .unwrap_or_default();
        if let Ok(mut p) = PANICS.lock()
            && p.len() < 64
        {
            p.push(format!("{msg} @ {at}"));
        }
    }));
}

/// The panics recorded since the last call.
pub fn take() -> Vec<String> {
    PANICS
        .lock()
        .map(|mut p| std::mem::take(&mut *p))
        .unwrap_or_default()
}

/// Runs `f`, turning a panic into an error with its message.
pub fn catch<R>(f: impl FnOnce() -> R) -> Result<R, String> {
    panic::catch_unwind(AssertUnwindSafe(f)).map_err(|e| {
        e.downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| e.downcast_ref::<String>().cloned())
            .map_or_else(|| "panic".to_owned(), |m| format!("panic: {m}"))
    })
}
