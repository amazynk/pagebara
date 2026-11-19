#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_jp2::{DecodeOptions, Parallelism, Region};

fuzz_target!(|data: &[u8]| {
    pagebara_budget::with_limit(512 << 20, || {
        let Ok(info) = pagebara_jp2::read_info(data) else {
            return;
        };
        let opts = DecodeOptions {
            parallelism: Parallelism::Single,
            ..DecodeOptions::default()
        };
        if u64::from(info.width) * u64::from(info.height) <= 1 << 24 {
            let _ = pagebara_jp2::decode(data, &opts);
        }
        let small = DecodeOptions {
            reduce: 2,
            region: Some(Region {
                x: 1,
                y: 2,
                w: 33,
                h: 9,
            }),
            ..opts
        };
        let _ = pagebara_jp2::decode(data, &small);
    });
});
