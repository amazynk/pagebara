//! Tiles are rendered straight into Slint pixel buffers, so showing one costs no copy.

use pagebara_app_core::tiles::TileBuffer;
use slint::{Image, Rgba8Pixel, SharedPixelBuffer};

pub struct SlintTile(SharedPixelBuffer<Rgba8Pixel>);

impl std::fmt::Debug for SlintTile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SlintTile({}×{})", self.0.width(), self.0.height())
    }
}

impl TileBuffer for SlintTile {
    fn new(w: u32, h: u32) -> Self {
        SlintTile(SharedPixelBuffer::new(w, h))
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        self.0.make_mut_bytes()
    }
}

impl SlintTile {
    pub fn image(&self) -> Image {
        Image::from_rgba8_premultiplied(self.0.clone())
    }
}
