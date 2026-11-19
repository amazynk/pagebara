//! Glyph outline throughput: `cargo run --release -p pagebara-font --example bench-font [font|pdf…]`.
//! Without arguments it uses a few system fonts and the fonts embedded in the sample PDFs.

use std::time::{Duration, Instant};

use pagebara_font::{Font, OutlineSink};
use pagebara_syntax::{Document, Object};

/// Counts segments so the work cannot be optimised away.
#[derive(Default)]
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

/// Glyphs per second over all glyphs of `f`.
fn rate(f: &Font) -> (f64, usize) {
    let n = f.num_glyphs().max(1);
    let mut sink = Count::default();
    let mut done = 0usize;
    let t = Instant::now();
    while t.elapsed() < Duration::from_millis(400) {
        for g in 0..n {
            f.outline(g, &mut sink);
        }
        done += usize::from(n);
    }
    (
        done as f64 / t.elapsed().as_secs_f64(),
        sink.0 / done.max(1),
    )
}

fn report(name: &str, data: Vec<u8>) {
    let t = Instant::now();
    let Some(f) = Font::parse(data) else {
        println!("{name}: unparsable");
        return;
    };
    let parse = t.elapsed();
    let (r, segs) = rate(&f);
    println!(
        "{name:60} {:?} {:6} glyphs  parse {:7.2?}  {:>10.0} glyphs/s  ({segs} segments/glyph)",
        f.format(),
        f.num_glyphs(),
        parse,
        r
    );
}

fn embedded(path: &str) {
    let Ok(d) = std::fs::read(path) else { return };
    let Ok(doc) = Document::open(d) else { return };
    let short = path.rsplit('/').next().unwrap_or(path);
    let mut total = (0usize, Duration::ZERO, 0usize);
    for n in doc.object_numbers() {
        let Some(o) = doc.reference(n).and_then(|r| doc.get(r)) else {
            continue;
        };
        let Object::Dict(fd) = &*o else { continue };
        if fd.name(b"Type") != Some(b"FontDescriptor") {
            continue;
        }
        for key in [&b"FontFile"[..], b"FontFile2", b"FontFile3"] {
            let o = doc.get_in(fd, key);
            let Some(s) = o.as_stream() else { continue };
            let Ok(dec) = doc.decode(s) else { continue };
            let Some(f) = Font::parse(dec.data) else {
                continue;
            };
            // Subsets keep unused glyph ids empty: time the drawn glyphs only.
            let mut sink = Count::default();
            let gids: Vec<u16> = (0..f.num_glyphs())
                .filter(|&g| {
                    let before = sink.0;
                    f.outline(g, &mut sink);
                    sink.0 > before
                })
                .collect();
            if gids.is_empty() {
                continue;
            }
            let t = Instant::now();
            let mut k = 0;
            while t.elapsed() < Duration::from_millis(50) {
                for &g in &gids {
                    f.outline(g, &mut sink);
                }
                k += gids.len();
            }
            total.0 += k;
            total.1 += t.elapsed();
            total.2 += 1;
        }
    }
    if total.0 > 0 {
        println!(
            "{short:60} {} embedded fonts: {:>10.0} glyphs/s",
            total.2,
            total.0 as f64 / total.1.as_secs_f64()
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let fonts = if args.is_empty() {
        [
            "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
            "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf",
            "/usr/share/fonts/opentype/urw-base35/NimbusSans-Regular.otf",
            "/usr/share/fonts/X11/Type1/NimbusRoman-Regular.pfb",
            "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            "/home/user/projects/ols/LuaManualEn.pdf",
            "/home/user/projects/fileservice/dunt_api.pdf",
            "/home/user/projects/rubidium/rfs_12pg.pdf",
            "/home/user/projects/pr570/PR570-OSM.pdf",
        ]
        .map(String::from)
        .to_vec()
    } else {
        args
    };
    for p in fonts {
        if p.ends_with(".pdf") {
            embedded(&p);
        } else if let Ok(d) = std::fs::read(&p) {
            report(&p, d);
        }
    }
}
