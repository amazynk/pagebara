#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_png::{DecodeOptions, Parallelism};

fuzz_target!(|data: &[u8]| {
    pagebara_budget::with_limit(512 << 20, || {
        let Ok(info) = pagebara_png::read_info(data) else {
            return;
        };
        if u64::from(info.width) * u64::from(info.height) > 1 << 24 {
            return;
        }
        let opts = DecodeOptions {
            parallelism: Parallelism::Single,
            ..DecodeOptions::default()
        };
        let _ = pagebara_png::decode(data, &opts);
        let _ = pagebara_png::decode(
            data,
            &DecodeOptions {
                scale: 3,
                verify_checksums: true,
                ..opts
            },
        );
        for i in 0..info.frames.len().min(4) {
            let _ = pagebara_png::decode_frame(data, &info, i, &opts);
        }
    });
});
