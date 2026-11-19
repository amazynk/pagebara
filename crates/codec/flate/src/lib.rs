//! DEFLATE, zlib and gzip decompression in safe Rust.
//!
//! [`decode_lenient`] is the PDF entry point: real-world FlateDecode streams are often
//! truncated, carry a broken zlib header or a wrong Adler-32, and readers are expected to
//! show what decodes.

mod crc;
mod inflate;

pub use crc::crc32;
pub use inflate::{
    InflateError, Inflater, MAX_MATCH, OUT_SLACK, Progress, WINDOW, adler32, gzip_members_to_vec,
    gzip_prefix, gzip_to_vec, raw_to_vec, raw_with_dictionary_to_vec, zlib_to_vec,
};

use inflate::grown;

/// Output of [`decode_lenient`]: everything that decoded, and why decoding stopped early.
#[derive(Debug)]
pub struct Lenient {
    pub data: Vec<u8>,
    pub error: Option<InflateError>,
}

/// Inflates a zlib stream, or a raw DEFLATE stream when the zlib header is missing or broken.
/// Never fails outright: corrupt input yields the bytes decoded so far. The trailer is not
/// checked. Only exceeding `max_out` or the memory budget is reported with no data kept.
pub fn decode_lenient(input: &[u8], max_out: usize) -> Lenient {
    let mut inf = Inflater::new_zlib(input).unwrap_or_else(|_| Inflater::new(input));
    match run(&mut inf, input.len(), max_out) {
        Ok(data) => Lenient { data, error: None },
        Err((data, e)) => Lenient {
            data,
            error: Some(e),
        },
    }
}

fn run(
    inf: &mut Inflater<'_>,
    input_len: usize,
    max_out: usize,
) -> Result<Vec<u8>, (Vec<u8>, InflateError)> {
    let first = (input_len * 4).clamp(1024, max_out.max(1024)) + OUT_SLACK;
    let mut res = pagebara_budget::reserve(first as u64).map_err(|e| (Vec::new(), e.into()))?;
    let mut out = vec![0u8; first];
    let mut pos = 0;
    loop {
        let limit = out.len() - OUT_SLACK;
        match inf.inflate(&mut out, &mut pos, limit) {
            Ok(Progress::Done) if pos > max_out => {
                return Err((Vec::new(), InflateError::TooLarge));
            }
            Ok(Progress::Done) => {
                out.truncate(pos);
                return Ok(out);
            }
            Ok(Progress::OutputFull) => {
                let Some(n) = grown(out.len(), max_out) else {
                    return Err((Vec::new(), InflateError::TooLarge));
                };
                res.grow_to(n as u64).map_err(|e| (Vec::new(), e.into()))?;
                out.resize(n, 0);
            }
            Err(e) => {
                out.truncate(pos);
                return Err((out, e));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // zlib of "hello hello hello hello" (level 9).
    const HELLO: [u8; 16] = [
        0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0xc8, 0x40, 0x27, 0x01, 0x68, 0x03, 0x08,
        0xb1,
    ];

    #[test]
    fn truncated_stream_keeps_prefix() {
        let full = decode_lenient(&HELLO, 1 << 20);
        assert_eq!(full.data, b"hello hello hello hello");
        let part = decode_lenient(&HELLO[..9], 1 << 20);
        assert!(part.error.is_some());
        assert!(b"hello hello hello hello".starts_with(&part.data));
    }

    #[test]
    fn raw_deflate_without_header() {
        let raw = &HELLO[2..];
        assert_eq!(
            decode_lenient(raw, 1 << 20).data,
            b"hello hello hello hello"
        );
    }

    #[test]
    fn output_cap() {
        let bomb = decode_lenient(&HELLO, 4);
        assert!(matches!(bomb.error, Some(InflateError::TooLarge)));
        assert!(bomb.data.is_empty());
    }
}
