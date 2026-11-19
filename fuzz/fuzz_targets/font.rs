//! Font programs: TrueType, OpenType (CFF and CFF2), bare CFF and Type 1, by the first
//! byte. Outlines, metrics, names and character maps of the first glyphs, then subsetting.

#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_font::cff::Cff;
use pagebara_font::subset::{Embedder, cff as cff_subset, truetype};
use pagebara_font::type1::Type1;
use pagebara_font::{Font, OutlineSink};

/// Counts path elements; outlines are not drawn.
struct Count(usize);

impl OutlineSink for Count {
    fn move_to(&mut self, _: f32, _: f32) {
        self.0 += 1;
    }
    fn line_to(&mut self, _: f32, _: f32) {
        self.0 += 1;
    }
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
        self.0 += 1;
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
        self.0 += 1;
    }
    fn close(&mut self) {}
}

const GLYPHS: u16 = 64;

fn exercise(f: &Font) {
    let mut sink = Count(0);
    let _ = f.font_matrix();
    let _ = f.postscript_name();
    for gid in 0..f.num_glyphs().min(GLYPHS) {
        let _ = f.outline(gid, &mut sink);
        let _ = f.advance(gid);
        if let Some(name) = f.glyph_name(gid) {
            let _ = f.glyph_by_name(name.as_bytes());
        }
    }
    if let Some(s) = f.as_sfnt() {
        for c in ['A', 'z', '€', '\u{1F600}'] {
            let _ = s.glyph_for_char(c);
        }
        for t in s.cmap_subtables() {
            let mut n = 0;
            s.cmap_for_each(t, &mut |_, _| n += 1);
        }
        let _ = truetype::subset(s, 0..8, &[('A', 1)]);
    }
    if let Some(c) = f.as_cff() {
        let _ = cff_subset::subset(c, 0..8);
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&k, data)) = data.split_first() else {
        return;
    };
    match k % 4 {
        0 => {
            if let Some(f) = Font::parse_index(data.to_vec(), u32::from(k >> 2) % 3) {
                exercise(&f);
            }
        }
        1 => {
            if let Some(f) = Font::parse(data.to_vec()) {
                exercise(&f);
            }
            if let Some(mut e) = Embedder::new(data.to_vec(), 0) {
                let _ = e.encode("Hello, wörld €");
                let mut doc = pagebara_pages::blank((1, 7));
                let _ = e.write(&mut doc);
            }
        }
        2 => {
            if let Some(c) = Cff::parse(data.to_vec()) {
                exercise(&Font::Cff(Box::new(c)));
            }
        }
        _ => {
            if let Some(t) = Type1::parse(data) {
                exercise(&Font::Type1(Box::new(t)));
            }
        }
    }
});
