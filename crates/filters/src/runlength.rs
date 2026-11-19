//! RunLengthDecode: PackBits with 128 as end of data.

use crate::{Result, Sink};

/// Returns whether the data was damaged (a run cut short).
pub(crate) fn decode(src: &[u8], out: &mut Sink) -> Result<bool> {
    let mut i = 0;
    while let Some(&n) = src.get(i) {
        i += 1;
        match n {
            0..=127 => {
                let len = n as usize + 1;
                let end = (i + len).min(src.len());
                out.extend(&src[i..end])?;
                if end - i < len {
                    return Ok(true);
                }
                i = end;
            }
            128 => return Ok(false),
            _ => {
                let Some(&b) = src.get(i) else {
                    return Ok(true);
                };
                i += 1;
                out.fill(b, 257 - n as usize)?;
            }
        }
    }
    // No EOD marker: accepted, as other readers do.
    Ok(false)
}

#[cfg(test)]
mod tests {
    use crate::{Limits, Sink};

    #[test]
    fn literal_run_and_eod() {
        let mut s = Sink::new(Limits::default());
        assert!(!super::decode(&[1, b'a', b'b', 0xfd, b'z', 128, 0, b'x'], &mut s).unwrap());
        assert_eq!(s.buf, b"abzzzz");
    }

    #[test]
    fn truncated_literal_is_damage() {
        let mut s = Sink::new(Limits::default());
        assert!(super::decode(&[5, b'a'], &mut s).unwrap());
        assert_eq!(s.buf, b"a");
    }
}
