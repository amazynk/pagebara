//! A filter chain chosen by the leading bytes over the rest of the input.

#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_filters::{CcittParams, Filter, Limits, Predictor};

fn filter(sel: &[u8]) -> Filter {
    let [k, a, b, c, d] = *sel else {
        return Filter::AsciiHex;
    };
    let predictor = Predictor::new(
        [1, 2, 10, 12, 15][usize::from(a % 5)],
        b % 6 + 1,
        [1, 2, 4, 8, 16, 3][usize::from(b / 6 % 6)],
        u32::from(c) * u32::from(d) + 1,
    );
    match k % 6 {
        0 => Filter::AsciiHex,
        1 => Filter::Ascii85,
        2 => Filter::Lzw {
            early_change: a & 0x80 != 0,
            predictor,
        },
        3 => Filter::Flate { predictor },
        4 => Filter::RunLength,
        _ => Filter::Ccitt(CcittParams {
            k: i32::from(a as i8),
            columns: u32::from(c) * 16 + 1,
            rows: u32::from(d),
            encoded_byte_align: b & 1 != 0,
            black_is_1: b & 2 != 0,
        }),
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&n, rest)) = data.split_first() else {
        return;
    };
    let n = usize::from(n % 3) + 1;
    if rest.len() < n * 5 {
        return;
    }
    let (sel, input) = rest.split_at(n * 5);
    let chain: Vec<Filter> = sel.as_chunks::<5>().0.iter().map(|s| filter(s)).collect();
    let limits = Limits {
        max_output: 64 << 20,
        ..Limits::default()
    };
    if let Ok(d) = pagebara_filters::decode(input, &chain, limits) {
        assert!(d.data.len() <= limits.max_output);
    }
});
