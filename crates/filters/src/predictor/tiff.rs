//! TIFF predictor 2 (horizontal differencing) for any bit depth.

/// Undo the horizontal-differencing predictor (2) on one row in file byte order.
/// `stride` is the samples per pixel in this row (1 for planar data).
pub(crate) fn unpredict_horizontal(row: &mut [u8], bits: u16, stride: usize, le: bool) {
    match (bits, stride) {
        // The previous pixel kept in registers, not loaded back from the row just stored.
        (8, 1) => unpredict8::<1>(row),
        (8, 2) => unpredict8::<2>(row),
        (8, 3) => unpredict8::<3>(row),
        (8, 4) => unpredict8::<4>(row),
        (16, 1) if !le => unpredict16::<false, 1>(row),
        (16, 3) if !le => unpredict16::<false, 3>(row),
        (16, 4) if !le => unpredict16::<false, 4>(row),
        (16, 1) => unpredict16::<true, 1>(row),
        (16, 3) => unpredict16::<true, 3>(row),
        (16, 4) => unpredict16::<true, 4>(row),
        _ => unpredict_any(row, bits, stride, le),
    }
}

fn unpredict8<const S: usize>(row: &mut [u8]) {
    let Some((first, rest)) = row.split_at_mut_checked(S) else {
        return;
    };
    let mut prev: [u8; S] = first.try_into().unwrap_or([0; S]);
    let (px, tail) = rest.as_chunks_mut::<S>();
    for p in px {
        for k in 0..S {
            prev[k] = prev[k].wrapping_add(p[k]);
            p[k] = prev[k];
        }
    }
    for (k, b) in tail.iter_mut().enumerate() {
        *b = b.wrapping_add(prev[k]);
    }
}

fn unpredict16<const LE: bool, const S: usize>(row: &mut [u8]) {
    let rd = |b: &[u8]| {
        if LE {
            u16::from_le_bytes([b[0], b[1]])
        } else {
            u16::from_be_bytes([b[0], b[1]])
        }
    };
    let n = row.len() / 2 * 2;
    let Some((first, rest)) = row[..n].split_at_mut_checked(2 * S) else {
        return;
    };
    let mut prev = [0u16; S];
    for (k, p) in prev.iter_mut().enumerate() {
        *p = rd(&first[2 * k..]);
    }
    let mut px = rest.chunks_exact_mut(2 * S);
    for p in &mut px {
        for k in 0..S {
            prev[k] = prev[k].wrapping_add(rd(&p[2 * k..]));
            let b = if LE {
                prev[k].to_le_bytes()
            } else {
                prev[k].to_be_bytes()
            };
            p[2 * k..2 * k + 2].copy_from_slice(&b);
        }
    }
    for (k, b) in px
        .into_remainder()
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .enumerate()
    {
        let v = rd(b).wrapping_add(prev[k]);
        b.copy_from_slice(&if LE { v.to_le_bytes() } else { v.to_be_bytes() });
    }
}

/// [`unpredict_horizontal`] for any depth and stride.
fn unpredict_any(row: &mut [u8], bits: u16, stride: usize, le: bool) {
    match bits {
        8 => {
            for i in stride..row.len() {
                row[i] = row[i].wrapping_add(row[i - stride]);
            }
        }
        16 => {
            let n = row.len() / 2;
            let rd = |r: &[u8], i: usize| {
                let b = [r[2 * i], r[2 * i + 1]];
                if le {
                    u16::from_le_bytes(b)
                } else {
                    u16::from_be_bytes(b)
                }
            };
            for i in stride..n {
                let v = rd(row, i).wrapping_add(rd(row, i - stride));
                let b = if le { v.to_le_bytes() } else { v.to_be_bytes() };
                row[2 * i..2 * i + 2].copy_from_slice(&b);
            }
        }
        24 => {
            let n = row.len() / 3;
            let rd = |r: &[u8], i: usize| {
                let b = &r[3 * i..3 * i + 3];
                if le {
                    u32::from_le_bytes([b[0], b[1], b[2], 0])
                } else {
                    u32::from_be_bytes([0, b[0], b[1], b[2]])
                }
            };
            for i in stride..n {
                let v = rd(row, i).wrapping_add(rd(row, i - stride)) & 0xff_ffff;
                let b = if le { v.to_le_bytes() } else { v.to_be_bytes() };
                if le {
                    row[3 * i..3 * i + 3].copy_from_slice(&b[..3]);
                } else {
                    row[3 * i..3 * i + 3].copy_from_slice(&b[1..]);
                }
            }
        }
        32 => {
            let n = row.len() / 4;
            let rd = |r: &[u8], i: usize| {
                let b = [r[4 * i], r[4 * i + 1], r[4 * i + 2], r[4 * i + 3]];
                if le {
                    u32::from_le_bytes(b)
                } else {
                    u32::from_be_bytes(b)
                }
            };
            for i in stride..n {
                let v = rd(row, i).wrapping_add(rd(row, i - stride));
                let b = if le { v.to_le_bytes() } else { v.to_be_bytes() };
                row[4 * i..4 * i + 4].copy_from_slice(&b);
            }
        }
        64 => {
            let n = row.len() / 8;
            let rd = |r: &[u8], i: usize| {
                let mut b = [0u8; 8];
                b.copy_from_slice(&r[8 * i..8 * i + 8]);
                if le {
                    u64::from_le_bytes(b)
                } else {
                    u64::from_be_bytes(b)
                }
            };
            for i in stride..n {
                let v = rd(row, i).wrapping_add(rd(row, i - stride));
                let b = if le { v.to_le_bytes() } else { v.to_be_bytes() };
                row[8 * i..8 * i + 8].copy_from_slice(&b);
            }
        }
        // 40, 48 and 56 bits: whole bytes in the file's byte order.
        b if b.is_multiple_of(8) && b < 64 => {
            let nb = b as usize / 8;
            let mask = (1u64 << b) - 1;
            let rd = |r: &[u8], i: usize| {
                let s = &r[nb * i..nb * i + nb];
                if le {
                    s.iter().rev().fold(0u64, |a, &x| (a << 8) | x as u64)
                } else {
                    s.iter().fold(0u64, |a, &x| (a << 8) | x as u64)
                }
            };
            for i in stride..row.len() / nb {
                let v = rd(row, i).wrapping_add(rd(row, i - stride)) & mask;
                for k in 0..nb {
                    let byte = (v >> (8 * if le { k } else { nb - 1 - k })) as u8;
                    row[nb * i + k] = byte;
                }
            }
        }
        b if b < 64 => unpredict_packed(row, b as usize, stride),
        _ => {}
    }
}

/// The horizontal predictor on samples that are not whole bytes (bit-packed MSB first, as the
/// row stores them): each sample adds the one `stride` samples before it, modulo `2^bits`.
/// libtiff refuses these depths; the arithmetic is the TIFF 6.0 predictor's, on the samples.
fn unpredict_packed(row: &mut [u8], bits: usize, stride: usize) {
    let n = row.len() * 8 / bits;
    let mask = (1u64 << bits) - 1;
    let get = |row: &[u8], i: usize| -> u64 {
        let p = i * bits;
        let mut v = 0u64;
        let mut got = 0;
        while got < bits {
            let off = (p + got) % 8;
            let take = (8 - off).min(bits - got);
            let byte = row[(p + got) / 8] as u64;
            v = (v << take) | ((byte >> (8 - off - take)) & ((1 << take) - 1));
            got += take;
        }
        v
    };
    let put = |row: &mut [u8], i: usize, v: u64| {
        let p = i * bits;
        let mut left = bits;
        while left > 0 {
            let pos = p + bits - left;
            let off = pos % 8;
            let take = (8 - off).min(left);
            let part = ((v >> (left - take)) & ((1 << take) - 1)) as u8;
            let shift = 8 - off - take;
            let m = (((1u16 << take) - 1) as u8) << shift;
            row[pos / 8] = (row[pos / 8] & !m) | (part << shift);
            left -= take;
        }
    };
    for i in stride..n {
        let v = get(row, i).wrapping_add(get(row, i - stride)) & mask;
        put(row, i, v);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predictor_fast_paths_match_the_general_one() {
        let mut x = 0x1234_5678u32;
        let mut rnd = || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        };
        for bits in [8u16, 16] {
            for stride in 1..=5 {
                for le in [false, true] {
                    // Odd lengths too: a partial pixel at the end of the row.
                    for len in [0, 1, 2, 7, 64, 101] {
                        let row: Vec<u8> = (0..len).map(|_| rnd()).collect();
                        let (mut a, mut b) = (row.clone(), row);
                        unpredict_horizontal(&mut a, bits, stride, le);
                        unpredict_any(&mut b, bits, stride, le);
                        assert_eq!(a, b, "bits {bits} stride {stride} le {le} len {len}");
                    }
                }
            }
        }
    }
}
