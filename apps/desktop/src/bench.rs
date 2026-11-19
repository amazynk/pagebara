//! `--bench`: scripted scrolling and zooming, one step per rendered frame, with frame time
//! statistics per phase. Exits when done.

use std::time::{Duration, Instant};

use crate::page_view::ViewBridge;
use crate::perf::FrameStats;

#[derive(Clone, Copy, Debug)]
enum Phase {
    Warm,
    /// Constant-speed scrolling, pixels per second.
    Scroll(f64),
    /// Continuous zooming as with a trackpad pinch.
    Pinch,
    /// Zoom button steps every 250 ms.
    Steps,
}

const PHASES: [(Phase, &str, Duration); 6] = [
    (Phase::Warm, "warm-up", Duration::from_millis(800)),
    (
        Phase::Scroll(1500.0),
        "scroll 1500 px/s",
        Duration::from_secs(4),
    ),
    (
        Phase::Scroll(6000.0),
        "scroll 6000 px/s",
        Duration::from_secs(3),
    ),
    (
        Phase::Scroll(-20000.0),
        "fling up 20000 px/s",
        Duration::from_secs(1),
    ),
    (Phase::Pinch, "pinch zoom 35%-280%", Duration::from_secs(4)),
    (Phase::Steps, "zoom steps", Duration::from_secs(3)),
];

#[derive(Debug)]
pub struct Bench {
    phase: usize,
    phase_start: Option<Instant>,
    last: Option<Instant>,
    base_zoom: f64,
    steps: u32,
}

impl Bench {
    pub fn new() -> Self {
        Bench {
            phase: 0,
            phase_start: None,
            last: None,
            base_zoom: 1.0,
            steps: 0,
        }
    }

    /// Moves the view for the frame at `now`; returns false when finished.
    pub fn step(&mut self, now: Instant, view: &mut ViewBridge, stats: &mut FrameStats) -> bool {
        let Some(&(phase, name, length)) = PHASES.get(self.phase) else {
            return false;
        };
        let start = *self.phase_start.get_or_insert_with(|| {
            stats.hold = true;
            self.base_zoom = view.view.viewport.zoom();
            self.steps = 0;
            now
        });
        let t = (now - start).as_secs_f64();
        let dt = self.last.map_or(0.0, |l| (now - l).as_secs_f64());
        self.last = Some(now);
        let vp = &mut view.view.viewport;
        let (w, h) = vp.size();
        match phase {
            Phase::Warm => {}
            Phase::Scroll(speed) => vp.scroll_by(0.0, speed * dt),
            Phase::Pinch => {
                let z = self.base_zoom * (1.5 * (t * std::f64::consts::TAU / 4.0).sin()).exp2();
                view.view.zoom_gesture(now);
                view.view.viewport.zoom_at(z, w * 0.4, h * 0.4);
            }
            Phase::Steps => {
                let due = (t / 0.25) as u32;
                if due > self.steps {
                    self.steps = due;
                    if due % 4 < 2 {
                        view.zoom_in();
                    } else {
                        view.zoom_out();
                    }
                }
            }
        }
        if now - start >= length {
            let (n, mean, in_flight, cached, bytes) = view.view.stats();
            stats.log(name);
            log::info!(
                "bench {name}: tiles rendered {n} (mean {:.1} ms on the pool), {in_flight} in flight, \
                 {cached} cached ({} MiB), {} missing on screen",
                mean.as_secs_f64() * 1000.0,
                bytes >> 20,
                view.last.missing,
            );
            stats.cpu.clear();
            stats.interval.clear();
            stats.update.clear();
            self.phase += 1;
            self.phase_start = None;
        }
        true
    }
}
