//! ASCIIHexDecode and ASCII85Decode. Both return whether the data was damaged.

use crate::{Result, Sink};

const fn hex_table() -> [u8; 256] {
    let mut t = [0xff; 256];
    let mut i = 0;
    while i < 10 {
        t[b'0' as usize + i] = i as u8;
        i += 1;
    }
    let mut i = 0;
    while i < 6 {
        t[b'a' as usize + i] = 10 + i as u8;
        t[b'A' as usize + i] = 10 + i as u8;
        i += 1;
    }
    t
}

static HEX: [u8; 256] = hex_table();

#[inline]
fn is_white(b: u8) -> bool {
    matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'\0')
}

pub(crate) fn hex(src: &[u8], out: &mut Sink) -> Result<bool> {
    out.buf.reserve(src.len() / 2);
    let mut hi: Option<u8> = None;
    for &b in src {
        if b == b'>' {
            break;
        }
        let v = HEX[b as usize];
        if v == 0xff {
            if is_white(b) {
                continue;
            }
            // Invalid character: keep what decoded.
            flush_hex(hi, out)?;
            return Ok(true);
        }
        match hi.take() {
            None => hi = Some(v),
            Some(h) => out.push(h << 4 | v)?,
        }
    }
    flush_hex(hi, out)?;
    Ok(false)
}

/// An odd final digit is followed by an implied 0.
fn flush_hex(hi: Option<u8>, out: &mut Sink) -> Result<()> {
    hi.map_or(Ok(()), |h| out.push(h << 4))
}

pub(crate) fn a85(src: &[u8], out: &mut Sink) -> Result<bool> {
    out.buf.reserve(src.len() / 5 * 4);
    let src = src.strip_prefix(b"<~").unwrap_or(src);
    let (mut acc, mut n) = (0u32, 0usize);
    let mut damaged = false;
    for &b in src {
        match b {
            b'~' => break,
            b'z' if n == 0 => out.extend(&[0; 4])?,
            b'!'..=b'u' => {
                let Some(v) = acc
                    .checked_mul(85)
                    .and_then(|a| a.checked_add(u32::from(b - b'!')))
                else {
                    damaged = true;
                    break;
                };
                acc = v;
                n += 1;
                if n == 5 {
                    out.extend(&acc.to_be_bytes())?;
                    (acc, n) = (0, 0);
                }
            }
            _ if is_white(b) => {}
            _ => {
                damaged = true;
                break;
            }
        }
    }
    match n {
        0 => {}
        // A single trailing character encodes nothing.
        1 => damaged = true,
        _ => {
            for _ in n..5 {
                acc = acc.wrapping_mul(85).wrapping_add(84);
            }
            out.extend(&acc.to_be_bytes()[..n - 1])?;
        }
    }
    Ok(damaged)
}

#[cfg(test)]
mod tests {
    use crate::{Limits, Sink};

    fn run(f: fn(&[u8], &mut Sink) -> crate::Result<bool>, s: &[u8]) -> (Vec<u8>, bool) {
        let mut k = Sink::new(Limits::default());
        let d = f(s, &mut k).unwrap();
        (k.buf, d)
    }

    #[test]
    fn hex_basics() {
        assert_eq!(
            run(super::hex, b"48 65\n6c6C6f>"),
            (b"Hello".to_vec(), false)
        );
        assert_eq!(run(super::hex, b"901FA>"), (vec![0x90, 0x1f, 0xa0], false));
        assert_eq!(run(super::hex, b"41x42"), (b"A".to_vec(), true));
    }

    #[test]
    fn a85_basics() {
        assert_eq!(
            run(super::a85, b"87cURD]i,\"Ebo80~>"),
            (b"Hello World!".to_vec(), false)
        );
        assert_eq!(
            run(super::a85, b"<~z87cUR~>"),
            (b"\0\0\0\0Hell".to_vec(), false)
        );
        // Partial final group: "Hello" -> 87cURDZ.
        assert_eq!(run(super::a85, b"87cURDZ~>"), (b"Hello".to_vec(), false));
        // Overflowing group ("s8W-\"" = 2^32) is damage.
        assert!(run(super::a85, b"uuuuu~>").1);
    }
}
