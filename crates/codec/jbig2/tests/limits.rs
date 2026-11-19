use std::time::{Duration, Instant};

/// Fuzzer finds: a 1 KB stream declaring a 25×3.6M halftone grid over a 399×400 region,
/// one whose empty grid had its bit planes decoded row by row, and a pattern dictionary
/// of a million one-pixel patterns.
#[test]
fn oversized_halftone_grids_are_rejected_quickly() {
    for data in [
        &include_bytes!("data/halftone-grid-bomb.bin")[..],
        &include_bytes!("data/halftone-plane-bomb.bin")[..],
        &include_bytes!("data/pattern-count-bomb.bin")[..],
    ] {
        let (&n, rest) = data.split_first().unwrap();
        let n = usize::from(n).min(rest.len());
        let (globals, stream) = rest.split_at(n);
        let start = Instant::now();
        let _ = pagebara_jbig2::decode_embedded(stream, (n > 0).then_some(globals));
        if pagebara_jbig2::probe(rest) {
            let _ = pagebara_jbig2::decode(rest, 0);
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "{:?}",
            start.elapsed()
        );
    }
}
