//! A startup timeline, logged with `PAGEBARA_TRACE_STARTUP=1`: milliseconds since `main`
//! for each step up to the first window frame and the first page pixels. With the name of
//! a step as the value (`PAGEBARA_TRACE_STARTUP="first frame"`), the app quits at that step,
//! for measuring startup alone.

use std::sync::OnceLock;
use std::time::Instant;

struct Trace {
    start: Instant,
    quit_at: String,
}

static TRACE: OnceLock<Option<Trace>> = OnceLock::new();

/// Starts the clock (first thing in `main`); tracing is on when the variable is set.
pub fn begin() {
    TRACE.get_or_init(|| {
        let quit_at = std::env::var("PAGEBARA_TRACE_STARTUP").ok()?;
        Some(Trace {
            start: Instant::now(),
            quit_at,
        })
    });
}

/// Logs `step` with the time since [`begin`], when tracing.
pub fn mark(step: &str) {
    let Some(Some(t)) = TRACE.get() else { return };
    log::info!(
        "startup {:7.1} ms: {step}",
        t.start.elapsed().as_secs_f64() * 1e3
    );
    if t.quit_at == step {
        let _ = slint::invoke_from_event_loop(|| {
            let _ = slint::quit_event_loop();
        });
    }
}
