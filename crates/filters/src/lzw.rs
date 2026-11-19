//! LZWDecode: MSB-first codes of 9..12 bits, `EarlyChange` 1 (default) or 0.
//!
//! Table entries are (start, length) references into the output already written: an entry is
//! the previous string plus the first byte of the next, which starts right where the previous
//! one ended, so the bytes are already in place and every code is one back-copy.

use crate::Sink;

const CLEAR: u32 = 256;
const EOD: u32 = 257;
/// Slack past the write position so short strings copy as whole 8-byte words.
const SLACK: usize = 8;

pub(crate) fn decode(input: &[u8], early_change: bool, out: &mut Sink) -> crate::Result<()> {
    if early_change {
        run::<true>(input, out)
    } else {
        run::<false>(input, out)
    }
}

fn run<const EARLY: bool>(input: &[u8], sink: &mut Sink) -> crate::Result<()> {
    let mut start = [0u32; 4096];
    let mut len = [0u32; 4096];
    let mut next = 258u32;
    let mut width = 9u32;
    let mut prev: Option<(u32, u32)> = None;
    let (mut acc, mut nbits, mut i) = (0u64, 0u32, 0usize);
    let mut o = sink.buf.len();
    loop {
        if nbits < width {
            if i + 8 <= input.len() {
                let v = u64::from_be_bytes(input[i..i + 8].try_into().unwrap_or_default());
                let take = (64 - nbits) / 8;
                acc = if take == 8 {
                    v
                } else {
                    (acc << (take * 8)) | (v >> (64 - take * 8))
                };
                nbits += take * 8;
                i += take as usize;
            } else {
                while nbits < width && i < input.len() {
                    acc = (acc << 8) | u64::from(input[i]);
                    nbits += 8;
                    i += 1;
                }
                if nbits < width {
                    break;
                }
            }
        }
        let code = ((acc >> (nbits - width)) & ((1u64 << width) - 1)) as u32;
        nbits -= width;
        if code == CLEAR {
            (next, width, prev) = (258, 9, None);
            continue;
        }
        if code == EOD {
            break;
        }
        let (src, l) = match code {
            0..256 => (None, 1),
            _ if code < next => (
                Some(start[code as usize] as usize),
                len[code as usize] as usize,
            ),
            _ if code == next
                && let Some((ps, pl)) = prev =>
            {
                (Some(ps as usize), pl as usize + 1)
            }
            _ => break, // corrupt code
        };
        sink.grow(o + l, SLACK)?;
        let out = &mut sink.buf;
        let at = o;
        match src {
            None => out[o] = code as u8,
            Some(s) if code == next => {
                copy_back(out, s, o, l - 1);
                out[o + l - 1] = out[s];
            }
            Some(s) => copy_back(out, s, o, l),
        }
        o += l;
        if let Some((ps, pl)) = prev
            && next < 4096
        {
            start[next as usize] = ps;
            len[next as usize] = pl + 1;
            next += 1;
        }
        prev = Some((at as u32, l as u32));
        if next + u32::from(EARLY) >= (1 << width) && width < 12 {
            width += 1;
        }
    }
    sink.buf.truncate(o);
    Ok(())
}

/// Copies `out[s..s + c]` to `o` with `s + c <= o`; whole words while inside `out`.
#[inline(always)]
fn copy_back(out: &mut [u8], s: usize, o: usize, c: usize) {
    if o + c + 8 <= out.len() {
        let mut k = 0;
        while k < c {
            let w: [u8; 8] = out[s + k..s + k + 8].try_into().unwrap_or_default();
            out[o + k..o + k + 8].copy_from_slice(&w);
            k += 8;
        }
    } else {
        out.copy_within(s..s + c, o);
    }
}

#[cfg(test)]
mod tests {
    use crate::{Limits, Sink};

    fn sample(n: u32) -> Vec<u8> {
        (0..n)
            .map(|i| (i * 7 % 13) as u8 ^ (i / 100) as u8)
            .collect()
    }

    fn dec(enc: &[u8], early: bool) -> Vec<u8> {
        let mut s = Sink::new(Limits::default());
        super::decode(enc, early, &mut s).unwrap();
        s.buf
    }

    #[test]
    fn early_change_round_trip() {
        let data = sample(20_000);
        let enc = weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
            .encode(&data)
            .unwrap();
        assert_eq!(dec(&enc, true), data);
        let half = dec(&enc[..enc.len() / 2], true);
        assert_eq!(half, data[..half.len()]);
    }

    #[test]
    fn late_change_round_trip() {
        let data = sample(30_000);
        let enc = weezl::encode::Encoder::new(weezl::BitOrder::Msb, 8)
            .encode(&data)
            .unwrap();
        assert_eq!(dec(&enc, false), data);
    }

    #[test]
    fn spec_example() {
        // ISO 32000-2 §7.4.4.2 example: 45 45 45 45 45 65 45 45 45 66.
        let enc = [0x80, 0x0B, 0x60, 0x50, 0x22, 0x0C, 0x0C, 0x85, 0x01];
        assert_eq!(dec(&enc, true), [45, 45, 45, 45, 45, 65, 45, 45, 45, 66]);
    }

    #[test]
    fn garbage_never_panics() {
        let mut x = 12345u32;
        for n in [1usize, 2, 7, 100, 5000] {
            let input: Vec<u8> = (0..n)
                .map(|_| {
                    x = x.wrapping_mul(1_103_515_245).wrapping_add(12345);
                    (x >> 16) as u8
                })
                .collect();
            let _ = dec(&input, true);
            let _ = dec(&input, false);
        }
    }

    #[test]
    fn output_limit_is_enforced() {
        let enc = weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
            .encode(&vec![0u8; 1 << 20])
            .unwrap();
        let mut s = Sink::new(Limits {
            max_output: 1000,
            ..Limits::default()
        });
        assert!(super::decode(&enc, true, &mut s).is_err());
    }
}
