//! PDF stream filters (ISO 32000-2 §7.4) that produce bytes, and the predictors.
//!
//! Decoding is lenient, as readers must be: damaged data yields what decoded, flagged as
//! [`Decoded::damaged`]. Only resource limits are hard errors. Image filters (DCT, JPX,
//! JBIG2) are terminal and handled by the image layer.

#![feature(portable_simd)]

mod ascii;
mod lzw;
mod predictor;
mod runlength;

pub use pagebara_ccitt::Params as CcittParams;
pub use predictor::{Kind as PredictorKind, Predictor};

/// Output below this is never refused for its ratio to the input.
pub const RATIO_FLOOR: usize = 4 << 20;

/// Hard caps on decoding one stream. Each filter of a chain is held to them, and its output
/// is also reserved from the [`pagebara_budget`] memory budget as it grows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    /// Bytes one filter may produce.
    pub max_output: usize,
    /// One filter's output may be at most this many times its input (or [`RATIO_FLOOR`]),
    /// which stops decompression bombs: Flate cannot pass about 1032:1 and LZW about 2700:1,
    /// but CCITT rows of a wide page cost one bit each.
    pub max_ratio: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_output: 1 << 30,
            max_ratio: 1 << 15,
        }
    }
}

impl Limits {
    /// The output cap of one filter reading `input` bytes.
    pub fn output_cap(&self, input: usize) -> usize {
        input
            .saturating_mul(self.max_ratio)
            .max(RATIO_FLOOR)
            .min(self.max_output)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum Error {
    #[error("decoded stream exceeds {0} bytes")]
    TooLarge(usize),
    #[error("{0}")]
    NeedsMemory(#[from] pagebara_budget::NeedsMemory),
}

pub type Result<T> = std::result::Result<T, Error>;

/// One filter of a stream's `/Filter` chain, with its `/DecodeParms`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Filter {
    AsciiHex,
    Ascii85,
    Lzw {
        early_change: bool,
        predictor: Predictor,
    },
    Flate {
        predictor: Predictor,
    },
    RunLength,
    Ccitt(CcittParams),
}

/// Bytes out of a filter chain.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Decoded {
    pub data: Vec<u8>,
    /// Some filter met corrupt or truncated data; `data` is what decoded before that.
    pub damaged: bool,
}

/// Runs `chain` over `input`, first filter first. Every filter's output is capped by the
/// ratio to the chain's input, so stacked filters cannot multiply their expansions.
pub fn decode(input: &[u8], chain: &[Filter], limits: Limits) -> Result<Decoded> {
    let mut damaged = false;
    let mut cur: Option<Vec<u8>> = None;
    let cap = limits.output_cap(input.len());
    for f in chain {
        let src = cur.as_deref().unwrap_or(input);
        let mut sink = Sink::new(Limits {
            max_output: cap,
            ..limits
        });
        damaged |= apply(*f, src, &mut sink)?;
        cur = Some(sink.buf);
    }
    let data = cur.unwrap_or_else(|| input.to_vec());
    Ok(Decoded { data, damaged })
}

fn apply(f: Filter, src: &[u8], sink: &mut Sink) -> Result<bool> {
    let max = sink.limits.max_output;
    Ok(match f {
        Filter::AsciiHex => ascii::hex(src, sink)?,
        Filter::Ascii85 => ascii::a85(src, sink)?,
        Filter::RunLength => runlength::decode(src, sink)?,
        Filter::Lzw {
            early_change,
            predictor,
        } => {
            lzw::decode(src, early_change, sink)?;
            predictor.apply(&mut sink.buf)
        }
        Filter::Flate { predictor } => {
            let r = pagebara_flate::decode_lenient(src, max);
            match r.error {
                Some(pagebara_flate::InflateError::TooLarge) => return Err(Error::TooLarge(max)),
                Some(pagebara_flate::InflateError::NeedsMemory(n)) => return Err(n.into()),
                _ => {}
            }
            sink.buf = r.data;
            predictor.apply(&mut sink.buf) | r.error.is_some()
        }
        Filter::Ccitt(p) => {
            let max = pagebara_budget::cap(max as u64) as usize;
            let d = pagebara_ccitt::decode_pdf(src, &p, max);
            sink.buf = d.data;
            d.damaged
        }
    })
}

/// Output buffer of one filter, bounded by [`Limits::max_output`] and the memory budget.
#[derive(Debug)]
pub(crate) struct Sink {
    pub(crate) buf: Vec<u8>,
    pub(crate) limits: Limits,
    res: pagebara_budget::Reservation,
    /// Bytes that fit without checking: `min(max_output, reserved)`.
    room: usize,
}

impl Sink {
    pub(crate) fn new(limits: Limits) -> Self {
        Sink {
            buf: Vec::new(),
            limits,
            res: pagebara_budget::Reservation::none(),
            room: 0,
        }
    }

    /// Makes room for `len` bytes in total: past the limit is an error, past the reservation
    /// reserves more (doubling).
    #[cold]
    fn make_room(&mut self, len: usize) -> Result<()> {
        let max = self.limits.max_output;
        if len > max {
            return Err(Error::TooLarge(max));
        }
        let want = len.max(self.room.saturating_mul(2)).max(64 << 10).min(max);
        self.res.grow_to(want as u64)?;
        self.room = want;
        Ok(())
    }

    /// Appends one byte, enforcing the limit.
    #[inline]
    pub(crate) fn push(&mut self, b: u8) -> Result<()> {
        if self.buf.len() >= self.room {
            self.make_room(self.buf.len() + 1)?;
        }
        self.buf.push(b);
        Ok(())
    }

    pub(crate) fn extend(&mut self, b: &[u8]) -> Result<()> {
        self.fits(b.len())?;
        self.buf.extend_from_slice(b);
        Ok(())
    }

    /// Appends `n` copies of `b`.
    pub(crate) fn fill(&mut self, b: u8, n: usize) -> Result<()> {
        self.fits(n)?;
        self.buf.resize(self.buf.len() + n, b);
        Ok(())
    }

    #[inline]
    fn fits(&mut self, n: usize) -> Result<()> {
        let len = self.buf.len().saturating_add(n);
        if len > self.room {
            self.make_room(len)?;
        }
        Ok(())
    }

    /// Grows the buffer (zero-filled, doubling) to hold `len` bytes plus `slack`, for filters
    /// that write in place.
    #[inline]
    pub(crate) fn grow(&mut self, len: usize, slack: usize) -> Result<()> {
        let max = self.limits.max_output;
        if len > max {
            return Err(Error::TooLarge(max));
        }
        if len + slack > self.buf.len() {
            let n = (len + slack)
                .max(self.buf.len() * 2)
                .max(4096)
                .min(max + slack);
            self.fits(n - self.buf.len())?;
            self.buf.resize(n, 0);
        }
        Ok(())
    }
}

/// Compresses `data` as a zlib stream for `/FlateDecode`. `level` runs from 0 (store) to 10;
/// 6 balances speed and size.
pub fn encode_flate(data: &[u8], level: u8) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, level)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_hex_then_runlength() {
        // RunLength "02 41 42 43 FE 44 80" -> ABCDDD, hex-encoded.
        let hex = b"02414243 FE4480>";
        let d = decode(
            hex,
            &[Filter::AsciiHex, Filter::RunLength],
            Limits::default(),
        )
        .unwrap();
        assert_eq!(d.data, b"ABCDDD");
        assert!(!d.damaged);
    }

    #[test]
    fn ccitt_bomb_is_refused() {
        // G4 all-white rows of a page 2^20 pixels wide cost one bit each.
        let p = CcittParams {
            k: -1,
            columns: 1 << 20,
            ..CcittParams::default()
        };
        let r = decode(&[0xff; 4096], &[Filter::Ccitt(p)], Limits::default());
        let d = r.unwrap();
        assert!(d.data.len() <= RATIO_FLOOR.max(4096 << 15));
    }

    #[test]
    fn flate_bomb_is_refused_by_ratio() {
        let enc = encode_flate(&vec![0; 64 << 20], 6);
        let chain = [Filter::Flate {
            predictor: Predictor::default(),
        }];
        let strict = Limits {
            max_ratio: 100,
            ..Limits::default()
        };
        let cap = strict.output_cap(enc.len());
        assert_eq!(decode(&enc, &chain, strict), Err(Error::TooLarge(cap)));
        assert_eq!(
            decode(&enc, &chain, Limits::default()).unwrap().data.len(),
            64 << 20
        );
    }

    #[test]
    fn output_is_reserved_from_the_budget() {
        let hex = vec![b'0'; 8 << 20];
        let r = pagebara_budget::with_limit(1 << 20, || {
            decode(&hex, &[Filter::AsciiHex], Limits::default())
        });
        assert!(matches!(r, Err(Error::NeedsMemory(_))), "{r:?}");
    }

    #[test]
    fn flate_encode_round_trip() {
        let data: Vec<u8> = (0..100_000u32).map(|i| (i % 251) as u8).collect();
        let enc = encode_flate(&data, 6);
        assert!(enc.len() < data.len() / 10);
        let chain = [Filter::Flate {
            predictor: Predictor::default(),
        }];
        assert_eq!(decode(&enc, &chain, Limits::default()).unwrap().data, data);
    }

    #[test]
    fn empty_chain_is_identity() {
        assert_eq!(decode(b"x", &[], Limits::default()).unwrap().data, b"x");
    }
}
