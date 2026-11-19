//! Timings of background removal on a synthetic 12 MP scan:
//! `cargo run --release -p pagebara-signatures --example background_bench`.
//!
//! Prints the fastest wall time per call (the least disturbed by other load) and, on Linux,
//! the mean CPU time per call over all threads (the work done, whatever the load).

#[path = "../tests/common/mod.rs"]
mod common;

use std::time::{Duration, Instant};

use pagebara_signatures::{PREVIEW_SIDE, PROCESS_SIDE, Prepared, Settings};

/// User plus system CPU time of this process, from `/proc/self/stat` (100 Hz ticks).
fn cpu_time() -> Option<Duration> {
    let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
    let mut f = stat.rsplit_once(')')?.1.split_whitespace();
    let utime: u64 = f.nth(11)?.parse().ok()?;
    let stime: u64 = f.next()?.parse().ok()?;
    Some(Duration::from_millis((utime + stime) * 10))
}

/// Fastest wall time and mean CPU time of `runs` calls of `f`.
fn measure(runs: u32, mut f: impl FnMut(u32)) -> (Duration, Option<Duration>) {
    let cpu0 = cpu_time();
    let mut best = Duration::MAX;
    for i in 0..runs {
        let t = Instant::now();
        f(i);
        best = best.min(t.elapsed());
    }
    let cpu = cpu_time().zip(cpu0).map(|(b, a)| (b - a) / runs);
    (best, cpu)
}

fn main() {
    let scan = common::scan(4000, 3000).img;
    let show = |(wall, cpu): (Duration, Option<Duration>)| match cpu {
        Some(cpu) => format!("{wall:>9.2?} (cpu {cpu:>9.2?})"),
        None => format!("{wall:>9.2?}"),
    };
    for side in [PREVIEW_SIDE, PROCESS_SIDE] {
        let new = measure(9, |_| {
            std::hint::black_box(Prepared::new(&scan, side));
        });
        let prep = Prepared::new(&scan, side);
        let apply = measure(60, |i| {
            let s = Settings {
                threshold: [20, 50, 80][i as usize % 3],
                ..Settings::default()
            };
            std::hint::black_box(prep.apply(&s));
        });
        println!(
            "{side:>4} px ({}x{}): Prepared::new {}  apply {}",
            prep.width(),
            prep.height(),
            show(new),
            show(apply)
        );
    }
}
