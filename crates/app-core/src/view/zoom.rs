//! Zoom limits, the preset steps of the zoom buttons, and zoom buckets for tiles.

pub const MIN: f64 = 0.05;
pub const MAX: f64 = 32.0;

const STEPS: [f64; 24] = [
    0.05, 0.1, 0.25, 0.33, 0.5, 0.67, 0.75, 0.9, 1.0, 1.1, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0, 4.0,
    5.0, 6.4, 8.0, 12.0, 16.0, 24.0, 32.0,
];

/// Buckets per doubling of the tile scale.
pub const BUCKETS_PER_OCTAVE: f64 = 16.0;

pub fn clamp(z: f64) -> f64 {
    if z.is_finite() {
        z.clamp(MIN, MAX)
    } else {
        1.0
    }
}

/// The next preset above `z`.
pub fn step_in(z: f64) -> f64 {
    STEPS.into_iter().find(|&s| s > z * 1.001).unwrap_or(MAX)
}

/// The next preset below `z`.
pub fn step_out(z: f64) -> f64 {
    STEPS
        .into_iter()
        .rev()
        .find(|&s| s < z / 1.001)
        .unwrap_or(MIN)
}

/// The bucket for a scale of `s` device pixels per point: the smallest bucket at least as
/// sharp, so tiles are only ever scaled down a little.
pub fn bucket(s: f64) -> i16 {
    (s.log2() * BUCKETS_PER_OCTAVE - 1e-6).ceil() as i16
}

/// Device pixels per point of bucket `b`.
pub fn bucket_scale(b: i16) -> f64 {
    (f64::from(b) / BUCKETS_PER_OCTAVE).exp2()
}

/// "110%".
pub fn label(z: f64) -> String {
    format!("{}%", (z * 100.0).round())
}
