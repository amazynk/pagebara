//! Frame time statistics for the performance log.

use std::fmt;
use std::time::Duration;

#[derive(Debug, Default, Clone)]
pub struct Samples(Vec<Duration>);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Summary {
    pub count: usize,
    pub mean: Duration,
    pub p50: Duration,
    pub p95: Duration,
    pub p99: Duration,
    pub max: Duration,
}

impl Samples {
    pub fn push(&mut self, d: Duration) {
        self.0.push(d);
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn clear(&mut self) {
        self.0.clear();
    }

    /// Share of samples above `limit`.
    pub fn over(&self, limit: Duration) -> f64 {
        if self.0.is_empty() {
            return 0.0;
        }
        self.0.iter().filter(|&&d| d > limit).count() as f64 / self.0.len() as f64
    }

    pub fn summary(&self) -> Option<Summary> {
        if self.0.is_empty() {
            return None;
        }
        let mut v = self.0.clone();
        v.sort_unstable();
        let at = |q: f64| v[((v.len() - 1) as f64 * q).round() as usize];
        Some(Summary {
            count: v.len(),
            mean: v.iter().sum::<Duration>() / v.len() as u32,
            p50: at(0.5),
            p95: at(0.95),
            p99: at(0.99),
            max: v[v.len() - 1],
        })
    }
}

impl fmt::Display for Summary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ms = |d: Duration| d.as_secs_f64() * 1000.0;
        write!(
            f,
            "n={} mean={:.2}ms p50={:.2}ms p95={:.2}ms p99={:.2}ms max={:.2}ms",
            self.count,
            ms(self.mean),
            ms(self.p50),
            ms(self.p95),
            ms(self.p99),
            ms(self.max)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles() {
        let mut s = Samples::default();
        for i in 1..=100 {
            s.push(Duration::from_millis(i));
        }
        let m = s.summary().unwrap();
        assert_eq!(m.p50, Duration::from_millis(51));
        assert_eq!(m.p95, Duration::from_millis(95));
        assert_eq!(m.max, Duration::from_millis(100));
        assert_eq!(s.over(Duration::from_millis(90)), 0.1);
    }
}
