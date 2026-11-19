//! Decode timing for PDF-embedded JBIG2 streams (raw `/JBIG2Decode` data, no globals).
//!
//! ```text
//! cargo run --release -p pagebara-jbig2 --example bench-jbig2 -- [--runs 10] stream.jb2...
//! ```

use std::time::Instant;

fn main() {
    let mut runs = 10;
    let mut files = Vec::new();
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        if a == "--runs" {
            runs = it.next().and_then(|v| v.parse().ok()).unwrap_or(runs);
        } else {
            files.push(a);
        }
    }
    for f in files {
        let data = std::fs::read(&f).expect("read");
        let mut best = f64::MAX;
        let mut sum = 0u64;
        for _ in 0..runs {
            let t = Instant::now();
            let p = pagebara_jbig2::decode_embedded(&data, None).expect("decode");
            best = best.min(t.elapsed().as_secs_f64() * 1e3);
            sum = p
                .data
                .iter()
                .fold(0, |s: u64, &b| s.wrapping_mul(31).wrapping_add(b as u64));
        }
        println!("{f}: {best:.1} ms (checksum {sum:x})");
    }
}
