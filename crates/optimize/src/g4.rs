//! Group 4 (T.6) encoding, for writing bilevel images as `CCITTFaxDecode` with `K -1`.
//! The code tables are those of T.4 (as in `pagebara-ccitt`, which decodes them).

/// White terminating codes 0..=63 (T.4 table 2).
const WHITE_TERM: [&str; 64] = [
    "00110101", "000111", "0111", "1000", "1011", "1100", "1110", "1111", "10011", "10100",
    "00111", "01000", "001000", "000011", "110100", "110101", "101010", "101011", "0100111",
    "0001100", "0001000", "0010111", "0000011", "0000100", "0101000", "0101011", "0010011",
    "0100100", "0011000", "00000010", "00000011", "00011010", "00011011", "00010010", "00010011",
    "00010100", "00010101", "00010110", "00010111", "00101000", "00101001", "00101010", "00101011",
    "00101100", "00101101", "00000100", "00000101", "00001010", "00001011", "01010010", "01010011",
    "01010100", "01010101", "00100100", "00100101", "01011000", "01011001", "01011010", "01011011",
    "01001010", "01001011", "00110010", "00110011", "00110100",
];

/// White makeup codes 64..=1728 in steps of 64.
const WHITE_MAKEUP: [&str; 27] = [
    "11011",
    "10010",
    "010111",
    "0110111",
    "00110110",
    "00110111",
    "01100100",
    "01100101",
    "01101000",
    "01100111",
    "011001100",
    "011001101",
    "011010010",
    "011010011",
    "011010100",
    "011010101",
    "011010110",
    "011010111",
    "011011000",
    "011011001",
    "011011010",
    "011011011",
    "010011000",
    "010011001",
    "010011010",
    "011000",
    "010011011",
];

/// Black terminating codes 0..=63 (T.4 table 2).
const BLACK_TERM: [&str; 64] = [
    "0000110111",
    "010",
    "11",
    "10",
    "011",
    "0011",
    "0010",
    "00011",
    "000101",
    "000100",
    "0000100",
    "0000101",
    "0000111",
    "00000100",
    "00000111",
    "000011000",
    "0000010111",
    "0000011000",
    "0000001000",
    "00001100111",
    "00001101000",
    "00001101100",
    "00000110111",
    "00000101000",
    "00000010111",
    "00000011000",
    "000011001010",
    "000011001011",
    "000011001100",
    "000011001101",
    "000001101000",
    "000001101001",
    "000001101010",
    "000001101011",
    "000011010010",
    "000011010011",
    "000011010100",
    "000011010101",
    "000011010110",
    "000011010111",
    "000001101100",
    "000001101101",
    "000011011010",
    "000011011011",
    "000001010100",
    "000001010101",
    "000001010110",
    "000001010111",
    "000001100100",
    "000001100101",
    "000001010010",
    "000001010011",
    "000000100100",
    "000000110111",
    "000000111000",
    "000000100111",
    "000000101000",
    "000001011000",
    "000001011001",
    "000000101011",
    "000000101100",
    "000001011010",
    "000001100110",
    "000001100111",
];

/// Black makeup codes 64..=1728 in steps of 64.
const BLACK_MAKEUP: [&str; 27] = [
    "0000001111",
    "000011001000",
    "000011001001",
    "000001011011",
    "000000110011",
    "000000110100",
    "000000110101",
    "0000001101100",
    "0000001101101",
    "0000001001010",
    "0000001001011",
    "0000001001100",
    "0000001001101",
    "0000001110010",
    "0000001110011",
    "0000001110100",
    "0000001110101",
    "0000001110110",
    "0000001110111",
    "0000001010010",
    "0000001010011",
    "0000001010100",
    "0000001010101",
    "0000001011010",
    "0000001011011",
    "0000001100100",
    "0000001100101",
];

/// Extended makeup codes 1792..=2560 in steps of 64, shared by both colours.
const EXT_MAKEUP: [&str; 13] = [
    "00000001000",
    "00000001100",
    "00000001101",
    "000000010010",
    "000000010011",
    "000000010100",
    "000000010101",
    "000000010110",
    "000000010111",
    "000000011100",
    "000000011101",
    "000000011110",
    "000000011111",
];

const EOL_CODE: &str = "000000000001";

#[derive(Default)]
struct Bits {
    out: Vec<u8>,
    acc: u32,
    n: u32,
}

impl Bits {
    fn put(&mut self, code: &str) {
        for c in code.bytes() {
            self.acc = self.acc << 1 | u32::from(c == b'1');
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }

    fn run(&mut self, black: bool, mut len: u32) {
        let (term, makeup) = if black {
            (&BLACK_TERM, &BLACK_MAKEUP)
        } else {
            (&WHITE_TERM, &WHITE_MAKEUP)
        };
        while len >= 2560 {
            self.put(EXT_MAKEUP[12]);
            len -= 2560;
        }
        if len >= 64 {
            let m = (len / 64) as usize;
            self.put(if m <= 27 {
                makeup[m - 1]
            } else {
                EXT_MAKEUP[m - 28]
            });
            len -= m as u32 * 64;
        }
        self.put(term[len as usize]);
    }

    fn finish(mut self) -> Vec<u8> {
        if self.n > 0 {
            self.out.push((self.acc << (8 - self.n)) as u8);
        }
        self.out
    }
}

/// The changing elements of a row (1 bits are black), then two `w` sentinels.
fn changes(row: &[u8], w: u32, out: &mut Vec<u32>) {
    out.clear();
    let mut color = 0u8;
    for x in 0..w {
        let bit = row[(x / 8) as usize] >> (7 - x % 8) & 1;
        if bit != color {
            out.push(x);
            color = bit;
        }
    }
    out.extend([w, w]);
}

const VERTICAL: [&str; 7] = ["0000010", "000010", "010", "1", "011", "000011", "0000011"];

fn row(bits: &mut Bits, cur: &[u32], refl: &[u32], w: u32) {
    let mut a0 = 0u32;
    let mut start = true;
    let mut color = 0usize;
    let (mut ci, mut ri) = (0usize, 0usize);
    // Past a0: at the start of a row a0 is before the first pixel.
    let past = |x: u32, a0: u32, start: bool| if start { x >= a0 } else { x > a0 };
    loop {
        while cur[ci] < w && !past(cur[ci], a0, start) {
            ci += 1;
        }
        let a1 = cur[ci];
        // b1: past a0 and of the colour opposite a0's (index parity is the colour after).
        ri = ri.saturating_sub(2);
        while refl[ri] < w && (!past(refl[ri], a0, start) || ri % 2 != color) {
            ri += 1;
        }
        let b1 = refl[ri];
        let b2 = refl.get(ri + 1).copied().unwrap_or(w);
        if b2 < a1 {
            bits.put("0001");
            a0 = b2;
        } else {
            let d = i64::from(a1) - i64::from(b1);
            if d.abs() <= 3 {
                bits.put(VERTICAL[(d + 3) as usize]);
                a0 = a1;
                color ^= 1;
            } else {
                let a2 = cur.get(ci + 1).copied().unwrap_or(w);
                bits.put("001");
                bits.run(color == 1, a1 - a0);
                bits.run(color == 0, a2 - a1);
                a0 = a2;
            }
        }
        start = false;
        if a0 >= w {
            break;
        }
    }
}

/// Encodes `rows` rows of `w` pixels (MSB first, `ceil(w / 8)` bytes a row, 1 bits black)
/// as T.6 with an end-of-block. To keep PDF image samples as they are, pass them inverted
/// (or not) to match the `BlackIs1` written.
pub fn encode_g4(data: &[u8], w: u32, rows: u32) -> Vec<u8> {
    let rb = (w as usize).div_ceil(8);
    let mut bits = Bits::default();
    let mut refl = vec![w, w];
    let mut cur = Vec::new();
    for y in 0..rows as usize {
        let Some(r) = data.get(y * rb..(y + 1) * rb) else {
            break;
        };
        changes(r, w, &mut cur);
        row(&mut bits, &cur, &refl, w);
        std::mem::swap(&mut cur, &mut refl);
    }
    bits.put(EOL_CODE);
    bits.put(EOL_CODE);
    bits.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagebara_ccitt::{Mode, decode};

    fn round_trip(w: u32, h: u32, data: &[u8]) {
        let enc = encode_g4(data, w, h);
        let mut out = Vec::new();
        let damaged = decode(&enc, Mode::G4 { align: false }, w, h as usize, &mut out);
        assert!(!damaged, "{w}x{h}");
        let rb = (w as usize).div_ceil(8);
        let pad = (rb * 8 - w as usize) as u32;
        let mask = 0xffu8 << pad;
        for y in 0..h as usize {
            for i in 0..rb {
                let m = if i == rb - 1 { mask } else { 0xff };
                assert_eq!(
                    out[y * rb + i] & m,
                    data[y * rb + i] & m,
                    "{w}x{h} row {y} byte {i}"
                );
            }
        }
    }

    #[test]
    fn random_and_structured_rows_round_trip() {
        let mut s = 0x1234_5678u32;
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 17;
            s ^= s << 5;
            s
        };
        for &(w, h) in &[
            (1u32, 1u32),
            (7, 3),
            (13, 9),
            (100, 40),
            (1728, 20),
            (3000, 5),
        ] {
            let rb = (w as usize).div_ceil(8);
            let noise: Vec<u8> = (0..rb * h as usize).map(|_| rnd() as u8).collect();
            round_trip(w, h, &noise);
            let sparse: Vec<u8> = (0..rb * h as usize)
                .map(|_| if rnd() % 11 == 0 { rnd() as u8 } else { 0 })
                .collect();
            round_trip(w, h, &sparse);
            round_trip(w, h, &vec![0xff; rb * h as usize]);
            round_trip(w, h, &vec![0; rb * h as usize]);
        }
    }
}
