//! Touch gesture math: telling taps, long presses and drags apart, kinetic scrolling after
//! a drag (a fling), and resizing a box with a pinch. Positions are logical pixels.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How far a finger may wander and still tap or long-press.
pub const SLOP: f64 = 10.0;
/// How long a still finger takes to become a long press.
pub const LONG_PRESS: Duration = Duration::from_millis(450);
/// Two taps closer than this in time (and [`SLOP`] × 3 in space) are a double tap.
pub const DOUBLE_TAP: Duration = Duration::from_millis(300);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PressState {
    /// Down, not moved beyond the slop yet.
    Pending,
    Drag,
    LongPress,
}

/// One finger from down to up.
#[derive(Clone, Copy, Debug)]
pub struct Press {
    pub start: (f64, f64),
    pub at: Instant,
    pub state: PressState,
}

impl Press {
    pub fn down(x: f64, y: f64, now: Instant) -> Press {
        Press {
            start: (x, y),
            at: now,
            state: PressState::Pending,
        }
    }

    /// The state after the finger moved to `(x, y)`: a drag once it left the slop, unless it
    /// was a long press already (which then extends, e.g. a text selection).
    pub fn moved(&mut self, x: f64, y: f64, now: Instant) -> PressState {
        self.poll(now);
        let far = (x - self.start.0).hypot(y - self.start.1) > SLOP;
        if far && self.state == PressState::Pending {
            self.state = PressState::Drag;
        }
        self.state
    }

    /// Turns a still press into a long press once it is held long enough.
    pub fn poll(&mut self, now: Instant) -> PressState {
        if self.state == PressState::Pending && now.duration_since(self.at) >= LONG_PRESS {
            self.state = PressState::LongPress;
        }
        self.state
    }

    /// A tap: up again quickly without leaving the slop.
    pub fn is_tap(&mut self, now: Instant) -> bool {
        self.poll(now) == PressState::Pending
    }
}

/// Recognises the second of two quick taps at one place.
#[derive(Clone, Copy, Debug, Default)]
pub struct Taps {
    last: Option<(Instant, f64, f64)>,
}

impl Taps {
    /// Records a tap; true when it completes a double tap.
    pub fn tap(&mut self, x: f64, y: f64, now: Instant) -> bool {
        let double = self.last.is_some_and(|(t, lx, ly)| {
            now.duration_since(t) <= DOUBLE_TAP && (x - lx).hypot(y - ly) <= SLOP * 3.0
        });
        self.last = if double { None } else { Some((now, x, y)) };
        double
    }
}

/// The finger's recent speed, from the moves of the last [`Velocity::WINDOW`].
#[derive(Clone, Debug, Default)]
pub struct Velocity {
    samples: VecDeque<(Instant, f64, f64)>,
}

impl Velocity {
    const WINDOW: Duration = Duration::from_millis(100);

    pub fn clear(&mut self) {
        self.samples.clear();
    }

    pub fn push(&mut self, x: f64, y: f64, now: Instant) {
        self.samples.push_back((now, x, y));
        while self
            .samples
            .front()
            .is_some_and(|&(t, ..)| now.duration_since(t) > Self::WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// Pixels per second; zero when the finger rested before `now`.
    pub fn get(&self, now: Instant) -> (f64, f64) {
        let (Some(&(t0, x0, y0)), Some(&(t1, x1, y1))) =
            (self.samples.front(), self.samples.back())
        else {
            return (0.0, 0.0);
        };
        let dt = t1.duration_since(t0).as_secs_f64();
        if dt < 0.005 || now.duration_since(t1) > Self::WINDOW / 2 {
            return (0.0, 0.0);
        }
        ((x1 - x0) / dt, (y1 - y0) / dt)
    }
}

/// Scrolling that carries on after a fast drag and slows down.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Fling {
    v: (f64, f64),
    last: Instant,
}

impl Fling {
    /// Slower releases just stop.
    pub const MIN_SPEED: f64 = 120.0;
    pub const MAX_SPEED: f64 = 9000.0;
    /// The speed falls by e every this long.
    const TAU: f64 = 0.325;
    const STOP_SPEED: f64 = 15.0;

    /// A fling at `v` pixels per second, if fast enough.
    pub fn start(v: (f64, f64), now: Instant) -> Option<Fling> {
        let speed = v.0.hypot(v.1);
        if !speed.is_finite() || speed < Self::MIN_SPEED {
            return None;
        }
        let f = (Self::MAX_SPEED / speed).min(1.0);
        Some(Fling {
            v: (v.0 * f, v.1 * f),
            last: now,
        })
    }

    pub fn velocity(&self) -> (f64, f64) {
        self.v
    }

    /// The distance travelled since the last step, or `None` once it stopped.
    pub fn step(&mut self, now: Instant) -> Option<(f64, f64)> {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = now;
        let decay = (-dt / Self::TAU).exp();
        // The integral of v·e^(−t/τ) over the step.
        let travel = Self::TAU * (1.0 - decay);
        let d = (self.v.0 * travel, self.v.1 * travel);
        self.v = (self.v.0 * decay, self.v.1 * decay);
        if self.v.0.hypot(self.v.1) < Self::STOP_SPEED {
            return None;
        }
        Some(d)
    }

    /// How far the whole fling goes from now.
    pub fn distance(&self) -> (f64, f64) {
        (self.v.0 * Self::TAU, self.v.1 * Self::TAU)
    }
}

/// `[x, y, w, h]` scaled by `scale` about its centre, keeping its width within
/// `min_w..=max_w` (and its proportions).
pub fn scale_box(b: [f64; 4], scale: f64, min_w: f64, max_w: f64) -> [f64; 4] {
    let [x, y, w, h] = b;
    if w <= 0.0 || !scale.is_finite() || scale <= 0.0 {
        return b;
    }
    let nw = (w * scale).clamp(min_w.min(max_w), max_w.max(min_w));
    let nh = h * nw / w;
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    [cx - nw / 2.0, cy - nh / 2.0, nw, nh]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn press_becomes_drag_or_long_press() {
        let t = Instant::now();
        let mut p = Press::down(100.0, 100.0, t);
        assert_eq!(p.moved(104.0, 103.0, t + ms(50)), PressState::Pending);
        assert_eq!(p.moved(120.0, 100.0, t + ms(80)), PressState::Drag);
        // A drag stays a drag even when held long.
        assert_eq!(p.poll(t + ms(2000)), PressState::Drag);

        let mut p = Press::down(100.0, 100.0, t);
        assert!(p.clone().is_tap(t + ms(200)));
        assert_eq!(p.poll(t + LONG_PRESS), PressState::LongPress);
        // Moving after a long press extends it.
        assert_eq!(p.moved(200.0, 100.0, t + ms(600)), PressState::LongPress);
        assert!(!p.is_tap(t + ms(700)));
    }

    #[test]
    fn double_taps_are_quick_and_close() {
        let t = Instant::now();
        let mut taps = Taps::default();
        assert!(!taps.tap(50.0, 50.0, t));
        assert!(taps.tap(55.0, 52.0, t + ms(200)));
        // A third tap starts over.
        assert!(!taps.tap(55.0, 52.0, t + ms(350)));
        assert!(!taps.tap(55.0, 52.0, t + ms(800)));
        assert!(!taps.tap(200.0, 52.0, t + ms(900)));
    }

    #[test]
    fn velocity_from_recent_moves() {
        let t = Instant::now();
        let mut v = Velocity::default();
        assert_eq!(v.get(t), (0.0, 0.0));
        // Old moves fall out of the window.
        v.push(0.0, 0.0, t);
        for i in 0..=5 {
            v.push(
                0.0,
                500.0 - 20.0 * f64::from(i),
                t + ms(210 + 10 * i as u64),
            );
        }
        let (vx, vy) = v.get(t + ms(260));
        assert_eq!(vx, 0.0);
        assert!((vy + 2000.0).abs() < 1.0, "{vy}");
        // Resting before release: no fling.
        assert_eq!(v.get(t + ms(400)), (0.0, 0.0));
    }

    #[test]
    fn fling_slows_down_and_stops() {
        let t = Instant::now();
        assert!(Fling::start((50.0, 0.0), t).is_none());
        let mut f = Fling::start((0.0, 20000.0), t).expect("fast enough");
        assert_eq!(f.velocity(), (0.0, Fling::MAX_SPEED));
        let total = f.distance().1;
        let mut went = 0.0;
        let mut now = t;
        let mut steps = 0;
        while let Some((dx, dy)) = f.step(now + ms(16)) {
            now += ms(16);
            assert_eq!(dx, 0.0);
            assert!(dy > 0.0);
            went += dy;
            steps += 1;
            assert!(steps < 1000);
        }
        // About 3 s at 60 fps, covering nearly the whole distance.
        assert!((100..300).contains(&steps), "{steps}");
        assert!(went > total * 0.99 && went <= total, "{went} of {total}");
    }

    #[test]
    fn pinching_a_box_keeps_centre_and_proportions() {
        let b = [100.0, 200.0, 200.0, 100.0];
        assert_eq!(scale_box(b, 1.5, 20.0, 1000.0), [50.0, 175.0, 300.0, 150.0]);
        assert_eq!(scale_box(b, 0.01, 20.0, 1000.0), [190.0, 245.0, 20.0, 10.0]);
        assert_eq!(scale_box(b, 10.0, 20.0, 400.0), [0.0, 150.0, 400.0, 200.0]);
        assert_eq!(scale_box(b, f64::NAN, 20.0, 400.0), b);
    }
}
