//! OCR speed and accuracy on synthetic full pages of 11 pt text scanned at 300 dpi.
//!
//! ```text
//! PAGEBARA_OCR_MODELS=DIR cargo run --release -p pagebara-ocr --example bench-ocr [pages] [dpi]
//! ```

#[path = "../tests/synth/mod.rs"]
mod synth;

use std::sync::atomic::AtomicBool;
use std::time::Instant;

use pagebara_ocr::{Engine, EngineParams, Options, Script, ocr_document, page_text, render_page};
use pagebara_render::Renderer;
use pagebara_syntax::Document;
use synth::*;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let pages: usize = args.first().and_then(|a| a.parse().ok()).unwrap_or(4);
    let dpi: f32 = args.get(1).and_then(|a| a.parse().ok()).unwrap_or(300.0);
    let Some(sources) = models() else {
        eprintln!("set PAGEBARA_OCR_MODELS to the model directory");
        return;
    };
    let cancel = AtomicBool::new(false);

    // A full page: the sample paragraph five times over, 45 lines.
    let mut lines: Vec<&str> = Vec::new();
    for _ in 0..5 {
        lines.extend_from_slice(LINES);
        lines.push("");
    }
    lines.truncate(45);
    let want = {
        let mut t = vec![TITLE];
        t.extend(lines.iter().copied());
        t.join("\n")
    };
    let (gray, w, h) = render_gray(&text_pdf(TITLE, &lines, 11.0), 300.0);
    let gray = degrade(&gray, w, h, 0.3, 20.0, 1);
    let page = scan_pdf(
        &gray,
        w,
        h,
        [0.0, 0.0, 612.0, 792.0],
        [612.0, 0.0, 0.0, 792.0, 0.0, 0.0],
        "",
    );
    let doc_bytes = {
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&gray, 6);
        let kids: String = (0..pages).map(|i| format!("{} 0 R ", 4 + i)).collect();
        let mut objs = vec![
            b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
            format!("<</Type/Pages/Kids[{kids}]/Count {pages}>>").into_bytes(),
        ];
        let mut img = format!(
            "<</Type/XObject/Subtype/Image/Width {w}/Height {h}/ColorSpace/DeviceGray\
             /BitsPerComponent 8/Filter/FlateDecode/Length {}>>stream\n",
            z.len()
        )
        .into_bytes();
        img.extend(&z);
        img.extend(b"\nendstream");
        objs.push(img);
        for _ in 0..pages {
            objs.push(
                format!(
                    "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents {} 0 R\
                     /Resources<</XObject<</Im0 3 0 R>>>>>>",
                    4 + pages
                )
                .into_bytes(),
            );
        }
        objs.push(b"<</Length 32>>stream\nq 612 0 0 792 0 0 cm /Im0 Do Q\nendstream".to_vec());
        pdf(&objs)
    };
    if let Some(dir) = std::env::var_os("PAGEBARA_OCR_KEEP") {
        let _ = std::fs::write(std::path::Path::new(&dir).join("bench-page.pdf"), &page);
    }

    let t = Instant::now();
    let engine = Engine::load(&sources, Script::Latin, &|_| {}, &cancel).expect("models");
    println!("load models: {:.2?}", t.elapsed());

    // Stages of one page.
    let r = Renderer::new(Document::open(page.clone()).unwrap());
    let params = EngineParams::default();
    for side in [1600, 2400, 3400] {
        let p = EngineParams {
            detection_max_side: side,
            ..params.clone()
        };
        let t = Instant::now();
        let (img, _) = render_page(&r, 0, dpi, &cancel).unwrap();
        let render = t.elapsed();
        let t = Instant::now();
        let boxes = engine.detect(&img, &p).unwrap();
        let detect = t.elapsed();
        let t = Instant::now();
        let lines = engine.recognize(&img, &p, &cancel).unwrap();
        let all = t.elapsed();
        let text: Vec<String> = lines
            .iter()
            .map(|l| {
                l.words
                    .iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        println!(
            "detection side {side}: render {render:.2?}, detect {detect:.2?} ({} lines), \
             detect+recognise {all:.2?}, CER {:.4}",
            boxes.len(),
            cer(&text.join("\n"), &want)
        );
    }

    let cpus = std::thread::available_parallelism().map_or(1, |n| n.get());
    let runs = [
        (1usize, 1usize),
        (cpus, 1),
        (cpus, 2),
        (cpus.div_ceil(2), 2),
    ];
    for (threads, parallel) in runs {
        let engine = Engine::load(&sources, Script::Latin, &|_| {}, &cancel)
            .unwrap()
            .with_threads(threads);
        let mut doc = Document::open(doc_bytes.clone()).unwrap();
        let opts = Options {
            dpi,
            parallel_pages: parallel,
            ..Options::default()
        };
        let t = Instant::now();
        let s = ocr_document(&mut doc, &engine, &opts, &|_| {}, &cancel).unwrap();
        let secs = t.elapsed().as_secs_f32();
        let out = Document::open(doc.save_incremental().unwrap()).unwrap();
        let c = cer(&page_text(&out, 0), &want);
        println!(
            "{pages} pages, {threads} inference threads, {parallel} pages at once: \
             {:.2}s/page ({} words/page, CER {c:.4})",
            secs / pages as f32,
            s.words / pages
        );
    }
}
