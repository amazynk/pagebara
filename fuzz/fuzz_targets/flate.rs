//! Lenient zlib decoding, cross-checked against miniz_oxide on streams both accept.

#![no_main]

use libfuzzer_sys::fuzz_target;

const MAX: usize = 64 << 20;

fuzz_target!(|data: &[u8]| {
    let r = pagebara_flate::decode_lenient(data, MAX);
    assert!(r.data.len() <= MAX);
    if r.error.is_none()
        && let Ok(theirs) =
            miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(data, MAX)
    {
        assert_eq!(r.data, theirs);
    }
    let _ = pagebara_flate::raw_to_vec(data, MAX);
    let _ = pagebara_flate::gzip_to_vec(data, MAX);
});
