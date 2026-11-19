#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_jpeg::{DecodeOptions, Parallelism, Region, Scale};

/// What a caller would accept before decoding (the decoder sizes buffers from the header).
const MAX_PEAK: u64 = 256 << 20;

fuzz_target!(|data: &[u8]| {
    let Ok(info) = pagebara_jpeg::read_info(data) else {
        return;
    };
    if pagebara_jpeg::memory_estimate(&info, Scale::Full, None).peak() > MAX_PEAK {
        return;
    }
    let opts = DecodeOptions {
        parallelism: Parallelism::Single,
        ..DecodeOptions::default()
    };
    let _ = pagebara_jpeg::decode(data, &opts);
    let half = DecodeOptions {
        scale: Scale::Half,
        region: Some(Region {
            x: 3,
            y: 5,
            w: 40,
            h: 17,
        }),
        ..opts
    };
    let _ = pagebara_jpeg::decode(data, &half);
    if let Ok(index) = pagebara_jpeg::build_index_with(data, &opts) {
        let _ = pagebara_jpeg::decode_with_index(data, &index, &half);
    }
});
