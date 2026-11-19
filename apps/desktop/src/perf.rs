//! Frame timing: CPU time per frame on the UI thread (our update plus Slint's rendering,
//! up to handing the frame to the GPU) and the interval between frames, logged while the
//! view moves.

use std::time::{Duration, Instant};

use pagebara_app_core::stats::Samples;

/// The 120 Hz budget.
pub const BUDGET: Duration = Duration::from_micros(8_333);
const REPORT_EVERY: Duration = Duration::from_secs(2);
/// Frames this long after the last movement still count as "moving".
const ACTIVE_FOR: Duration = Duration::from_millis(250);

#[derive(Debug, Default)]
pub struct FrameStats {
    pub cpu: Samples,
    pub interval: Samples,
    pub update: Samples,
    frame_start: Option<Instant>,
    last_frame: Option<Instant>,
    last_report: Option<Instant>,
    active_until: Option<Instant>,
    /// Keep samples for a benchmark instead of logging and clearing them.
    pub hold: bool,
}

impl FrameStats {
    /// Something moved; frames until shortly after count.
    pub fn activity(&mut self, now: Instant) {
        self.active_until = Some(now + ACTIVE_FOR);
    }

    fn active(&self, now: Instant) -> bool {
        self.hold || self.active_until.is_some_and(|t| now <= t)
    }

    pub fn before_render(&mut self, now: Instant) {
        if self.active(now)
            && let Some(last) = self.last_frame
        {
            self.interval.push(now - last);
        }
        self.frame_start = Some(now);
        self.last_frame = Some(now);
    }

    pub fn update_took(&mut self, d: Duration) {
        if self.active(Instant::now()) {
            self.update.push(d);
        }
    }

    pub fn after_render(&mut self, now: Instant) {
        if let Some(start) = self.frame_start.take()
            && self.active(start)
        {
            self.cpu.push(now - start);
        }
        if self.hold || self.cpu.is_empty() {
            return;
        }
        let due = *self.last_report.get_or_insert(now);
        if now - due >= REPORT_EVERY {
            self.last_report = Some(now);
            self.log("moving");
            self.cpu.clear();
            self.interval.clear();
            self.update.clear();
        }
    }

    pub fn log(&self, what: &str) {
        let (Some(cpu), Some(interval)) = (self.cpu.summary(), self.interval.summary()) else {
            return;
        };
        let update = self
            .update
            .summary()
            .map(|s| s.to_string())
            .unwrap_or_default();
        log::info!(
            "frames ({what}): UI thread cpu {cpu}, {:.1}% over 8.33ms | interval {interval}, \
             {:.1}% over 8.33ms | view update {update}",
            self.cpu.over(BUDGET) * 100.0,
            self.interval.over(BUDGET + Duration::from_micros(500)) * 100.0,
        );
    }
}
