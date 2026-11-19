//! Bounds-checked big-endian reading.

pub fn u8_at(d: &[u8], off: usize) -> Option<u8> {
    d.get(off).copied()
}

pub fn u16_at(d: &[u8], off: usize) -> Option<u16> {
    let b = d.get(off..off.checked_add(2)?)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

pub fn i16_at(d: &[u8], off: usize) -> Option<i16> {
    u16_at(d, off).map(|v| v as i16)
}

pub fn u24_at(d: &[u8], off: usize) -> Option<u32> {
    let b = d.get(off..off.checked_add(3)?)?;
    Some(u32::from_be_bytes([0, b[0], b[1], b[2]]))
}

pub fn u32_at(d: &[u8], off: usize) -> Option<u32> {
    let b = d.get(off..off.checked_add(4)?)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// A cursor over a byte slice.
#[derive(Clone, Copy, Debug)]
pub struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Reader { data, pos }
    }

    pub fn pos(&self) -> usize {
        self.pos
    }

    pub fn skip(&mut self, n: usize) -> Option<()> {
        self.pos = self.pos.checked_add(n)?;
        (self.pos <= self.data.len()).then_some(())
    }

    pub fn u8(&mut self) -> Option<u8> {
        let v = u8_at(self.data, self.pos)?;
        self.pos += 1;
        Some(v)
    }

    pub fn i8(&mut self) -> Option<i8> {
        self.u8().map(|v| v as i8)
    }

    pub fn u16(&mut self) -> Option<u16> {
        let v = u16_at(self.data, self.pos)?;
        self.pos += 2;
        Some(v)
    }

    pub fn i16(&mut self) -> Option<i16> {
        self.u16().map(|v| v as i16)
    }

    pub fn u24(&mut self) -> Option<u32> {
        let v = u24_at(self.data, self.pos)?;
        self.pos += 3;
        Some(v)
    }

    pub fn u32(&mut self) -> Option<u32> {
        let v = u32_at(self.data, self.pos)?;
        self.pos += 4;
        Some(v)
    }

    /// An unsigned integer of `size` (1..=4) bytes.
    pub fn uint(&mut self, size: u8) -> Option<u32> {
        match size {
            1 => self.u8().map(u32::from),
            2 => self.u16().map(u32::from),
            3 => self.u24(),
            4 => self.u32(),
            _ => None,
        }
    }
}

/// Big-endian writing helpers for the subsetters.
pub trait Write {
    fn u8(&mut self, v: u8);
    fn u16(&mut self, v: u16);
    fn i16(&mut self, v: i16);
    fn u32(&mut self, v: u32);
}

impl Write for Vec<u8> {
    fn u8(&mut self, v: u8) {
        self.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn i16(&mut self, v: i16) {
        self.extend_from_slice(&v.to_be_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.extend_from_slice(&v.to_be_bytes());
    }
}
