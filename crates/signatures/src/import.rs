//! Decoding imported image files (PNG, JPEG) into upright RGBA.

use crate::raster::Rgba;
use crate::{Error, Result};

/// Imported images are decoded no larger than this on their long side (JPEG decodes are
/// scaled in the DCT domain); processing works at a smaller size anyway.
pub const MAX_DECODE: u32 = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
}

impl Format {
    pub fn sniff(b: &[u8]) -> Option<Format> {
        if b.starts_with(b"\x89PNG\r\n\x1a\n") {
            Some(Format::Png)
        } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(Format::Jpeg)
        } else {
            None
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            Format::Png => "png",
            Format::Jpeg => "jpg",
        }
    }
}

/// Decodes a PNG or JPEG file, upright (Exif orientation applied).
pub fn decode(bytes: &[u8]) -> Result<Rgba> {
    let img = match Format::sniff(bytes).ok_or(Error::UnsupportedImage)? {
        Format::Png => png(bytes)?,
        Format::Jpeg => jpeg(bytes)?,
    };
    if img.width == 0 || img.height == 0 {
        return Err(Error::UnsupportedImage);
    }
    Ok(img)
}

fn png(bytes: &[u8]) -> Result<Rgba> {
    let opts = pagebara_png::DecodeOptions {
        output: pagebara_png::Output::Rgba8,
        ..Default::default()
    };
    let img = pagebara_png::decode(bytes, &opts).map_err(|e| Error::Decode(e.to_string()))?;
    let pixels = img.to_rgba8();
    let orientation = pagebara_png::read_info(bytes)
        .ok()
        .and_then(|info| info.exif(bytes).and_then(exif_orientation))
        .unwrap_or(1);
    Ok(Rgba {
        width: img.width,
        height: img.height,
        pixels,
    }
    .oriented(orientation))
}

fn jpeg(bytes: &[u8]) -> Result<Rgba> {
    let info = pagebara_jpeg::read_info(bytes).map_err(|e| Error::Decode(e.to_string()))?;
    let opts = pagebara_jpeg::DecodeOptions {
        scale: pagebara_jpeg::choose_scale(&info, MAX_DECODE, MAX_DECODE),
        ..Default::default()
    };
    let planes = pagebara_jpeg::decode(bytes, &opts).map_err(|e| Error::Decode(e.to_string()))?;
    let pixels = pagebara_jpeg::planes_to_rgba(&planes);
    let orientation = info
        .exif
        .and_then(|r| bytes.get(r))
        .and_then(exif_orientation)
        .unwrap_or(1);
    Ok(Rgba {
        width: planes.width,
        height: planes.height,
        pixels,
    }
    .oriented(orientation))
}

/// The orientation tag (0x0112) of a TIFF-structured Exif block.
pub(crate) fn exif_orientation(tiff: &[u8]) -> Option<u16> {
    let le = match tiff.get(..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| {
        let b: [u8; 2] = tiff.get(o..o + 2)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |o: usize| {
        let b: [u8; 4] = tiff.get(o..o + 4)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    let ifd = u32_at(4)? as usize;
    let n = u16_at(ifd)? as usize;
    (0..n.min(256)).find_map(|i| {
        let e = ifd + 2 + i * 12;
        (u16_at(e)? == 0x0112).then(|| u16_at(e + 8)).flatten()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orientation_tag() {
        let mut t = b"MM\0\x2a\0\0\0\x08\0\x01".to_vec();
        t.extend_from_slice(&[0x01, 0x12, 0, 3, 0, 0, 0, 1, 0, 6, 0, 0]);
        assert_eq!(exif_orientation(&t), Some(6));
        assert_eq!(exif_orientation(b"xx"), None);
    }
}
