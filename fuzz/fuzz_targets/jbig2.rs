//! Embedded streams (the first byte splits off a globals segment) and standalone files.

#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let Some((&n, rest)) = data.split_first() else {
        return;
    };
    let n = usize::from(n).min(rest.len());
    let (globals, stream) = rest.split_at(n);
    let _ = pagebara_jbig2::decode_embedded(stream, (n > 0).then_some(globals));
    if pagebara_jbig2::probe(rest) {
        let _ = pagebara_jbig2::info(rest, 0);
        let _ = pagebara_jbig2::decode(rest, 0);
    }
});
