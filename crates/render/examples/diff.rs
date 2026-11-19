//! Renders pages with pagebara-render and with `mutool draw` at the same resolution and
//! compares them.
//!
//! ```text
//! cargo run --release -p pagebara-render --example diff -- [--dpi 100] [--pages 5]
//!     [--save DIR] [--threshold 32] file.pdf...
//! ```
//!
//! Per page: PSNR (dB, RGB) and the share of pixels whose largest channel difference
//! exceeds the threshold. `--save` writes ours, MuPDF's and a difference image as PNG.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use pagebara_render::{PixelRect, RenderRequest, Renderer};

struct Args {
    dpi: f32,
    pages: usize,
    first: usize,
    save: Option<PathBuf>,
    threshold: u8,
    files: Vec<PathBuf>,
}

fn args() -> Args {
    let mut a = Args {
        dpi: 100.0,
        pages: 5,
        first: 0,
        save: None,
        threshold: 32,
        files: Vec::new(),
    };
    let mut it = std::env::args().skip(1);
    while let Some(s) = it.next() {
        match s.as_str() {
            "--dpi" => a.dpi = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.dpi),
            "--pages" => a.pages = it.next().and_then(|v| v.parse().ok()).unwrap_or(a.pages),
            "--first" => a.first = it.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--save" => a.save = it.next().map(PathBuf::from),
            "--threshold" => a.threshold = it.next().and_then(|v| v.parse().ok()).unwrap_or(32),
            _ => a.files.push(PathBuf::from(s)),
        }
    }
    a
}

/// Reads a binary PPM (P6) file.
pub fn read_ppm(data: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let mut fields = Vec::new();
    let mut i = 0;
    while fields.len() < 4 {
        while i < data.len() && data[i].is_ascii_whitespace() {
            i += 1;
        }
        if data.get(i) == Some(&b'#') {
            while i < data.len() && data[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        let s = i;
        while i < data.len() && !data[i].is_ascii_whitespace() {
            i += 1;
        }
        fields.push(std::str::from_utf8(&data[s..i]).ok()?.to_owned());
    }
    i += 1;
    if fields[0] != "P6" {
        return None;
    }
    let w: u32 = fields[1].parse().ok()?;
    let h: u32 = fields[2].parse().ok()?;
    let px = data.get(i..i + w as usize * h as usize * 3)?.to_vec();
    Some((w, h, px))
}

/// A decoded image: width, height, RGB pixels.
type Rgb = (u32, u32, Vec<u8>);

fn mutool(file: &Path, page: usize, dpi: f32, out: &Path) -> Option<(Rgb, f64)> {
    let t = Instant::now();
    let st = Command::new("mutool")
        .args(["draw", "-q", "-r", &dpi.to_string(), "-c", "rgb", "-o"])
        .arg(out)
        .arg(file)
        .arg((page + 1).to_string())
        .status()
        .ok()?;
    let took = t.elapsed().as_secs_f64();
    if !st.success() {
        return None;
    }
    Some((read_ppm(&std::fs::read(out).ok()?)?, took))
}

fn save_png(path: &Path, w: u32, h: u32, rgb: &[u8]) {
    if let Ok(png) = pagebara_png::encode(w, h, pagebara_png::ColorType::Rgb, rgb) {
        let _ = std::fs::write(path, png);
    }
}

fn main() {
    let a = args();
    let tmp = std::env::temp_dir().join(format!("pagebara-diff-{}.ppm", std::process::id()));
    println!(
        "{:<40} {:>5} {:>8} {:>8} {:>9} {:>9}",
        "file", "page", "PSNR", "%>thr", "ours ms", "mupdf ms"
    );
    let mut all = Vec::new();
    for f in &a.files {
        let Ok(bytes) = std::fs::read(f) else {
            eprintln!("cannot read {}", f.display());
            continue;
        };
        let Ok(doc) = pagebara_syntax::Document::open(bytes) else {
            eprintln!("cannot open {}", f.display());
            continue;
        };
        let r = Renderer::new(doc);
        let name = f
            .file_stem()
            .map_or("?".into(), |s| s.to_string_lossy().into_owned());
        let short: String = name.chars().take(40).collect();
        let n = r.page_count();
        for page in a.first..n.min(a.first + a.pages) {
            let Some(((w, h, theirs), their_t)) = mutool(f, page, a.dpi, &tmp) else {
                println!("{short:<40} {page:>5} mutool failed");
                continue;
            };
            let req = RenderRequest::new(a.dpi / 72.0, PixelRect { x: 0, y: 0, w, h });
            let t = Instant::now();
            let img = match r.render(page, &req) {
                Ok(i) => i,
                Err(e) => {
                    println!("{short:<40} {page:>5} error {e}");
                    continue;
                }
            };
            let ours_t = t.elapsed().as_secs_f64();
            let ours: Vec<u8> = img
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| [p[0], p[1], p[2]])
                .collect();
            let mut se = 0f64;
            let mut over = 0usize;
            let mut diff = vec![0u8; ours.len()];
            for (i, (p, q)) in ours
                .as_chunks::<3>()
                .0
                .iter()
                .zip(theirs.as_chunks::<3>().0)
                .enumerate()
            {
                let mut m = 0u8;
                for k in 0..3 {
                    let d = p[k].abs_diff(q[k]);
                    se += f64::from(d) * f64::from(d);
                    m = m.max(d);
                }
                if m > a.threshold {
                    over += 1;
                }
                let v = 255 - m.saturating_mul(4);
                diff[i * 3..i * 3 + 3].copy_from_slice(&[255, v, v]);
            }
            let px = (w as usize * h as usize).max(1);
            let mse = se / (px * 3) as f64;
            let psnr = if mse == 0.0 {
                99.0
            } else {
                10.0 * (255.0f64 * 255.0 / mse).log10()
            };
            let pct = over as f64 * 100.0 / px as f64;
            println!(
                "{short:<40} {page:>5} {psnr:>8.2} {pct:>8.3} {:>9.1} {:>9.1}",
                ours_t * 1e3,
                their_t * 1e3
            );
            all.push((psnr, pct));
            if let Some(dir) = &a.save {
                let _ = std::fs::create_dir_all(dir);
                let base = dir.join(format!("{name}-{page}"));
                save_png(&base.with_extension("ours.png"), w, h, &ours);
                save_png(&base.with_extension("mupdf.png"), w, h, &theirs);
                save_png(&base.with_extension("diff.png"), w, h, &diff);
            }
        }
    }
    let _ = std::fs::remove_file(&tmp);
    if !all.is_empty() {
        let mean_psnr = all.iter().map(|v| v.0).sum::<f64>() / all.len() as f64;
        let mean_pct = all.iter().map(|v| v.1).sum::<f64>() / all.len() as f64;
        let worst = all.iter().map(|v| v.0).fold(f64::MAX, f64::min);
        println!(
            "pages {}: mean PSNR {mean_psnr:.2} dB (worst {worst:.2}), mean {mean_pct:.3}% pixels over threshold",
            all.len()
        );
    }
}
