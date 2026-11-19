#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_ccitt::Params;

const MAX: usize = 16 << 20;

fuzz_target!(|data: &[u8]| {
    let Some((&[k, c, r, flags], input)) = data.split_first_chunk::<4>() else {
        return;
    };
    let p = Params {
        k: i32::from(k as i8),
        columns: u32::from(c) * 8 + 1,
        rows: u32::from(r),
        encoded_byte_align: flags & 1 != 0,
        black_is_1: flags & 2 != 0,
    };
    let d = pagebara_ccitt::decode_pdf(input, &p, MAX);
    assert!(d.data.len() <= MAX);
});
