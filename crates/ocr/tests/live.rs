//! Live text accuracy and speed on pictures of text made at run time (see `livegen`): each
//! picture is found among the page's regions, recognised as hovering over it would, and
//! the copied text compared with what the picture shows. Needs the models in
//! `PAGEBARA_OCR_MODELS` (skips with a note otherwise). Run in release with `--nocapture`
//! for the latency table:
//!
//! ```text
//! PAGEBARA_OCR_MODELS=DIR cargo test --release -p pagebara-ocr --test live -- --nocapture
//! ```

mod livegen;
mod synth;

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use livegen::{Case, Check};
use pagebara_content::{Interpreter, Point, Rect, copy_text};
use pagebara_ocr::{
    Engine, LiveParams, Region, page_with_live, recognize_region, recognize_region_live, regions,
};
use pagebara_pages::PageTree;
use pagebara_render::Renderer;
use pagebara_syntax::Document;
use synth::{cer, levenshtein, models, normalize};

fn engine() -> Option<&'static Engine> {
    static E: OnceLock<Option<Engine>> = OnceLock::new();
    E.get_or_init(|| {
        let sources = models()?;
        let cancel = livegen::go();
        Some(
            Engine::load(&sources, pagebara_ocr::Script::Latin, &|_| {}, &cancel)
                .expect("models load"),
        )
    })
    .as_ref()
}

macro_rules! need_engine {
    () => {
        match engine() {
            Some(e) => e,
            None => {
                eprintln!("skipped: set PAGEBARA_OCR_MODELS to a directory with the OCR models");
                return;
            }
        }
    };
}

/// What reading a case gave.
struct Outcome {
    text: String,
    cer: f32,
    /// From pointing at the picture to the text under the pointer being selectable, and
    /// to all of it being selectable (warm engine).
    first: Duration,
    latency: Duration,
    regions: usize,
}

/// The regions of page 0 that the case's text is in: its pictures, else its drawings.
fn case_regions(doc: &Document) -> Vec<Region> {
    let tree = PageTree::open(doc);
    let p = tree.page(0).unwrap();
    let d = doc.get(p.r).unwrap().as_dict().unwrap().clone();
    let res = doc.get_in(&d, b"Resources").as_dict().unwrap().clone();
    let list = Interpreter::new(doc).page(&d, &res);
    let c = p.crop_box;
    let crop = Rect::new(c.x0 as f32, c.y0 as f32, c.x1 as f32, c.y1 as f32);
    let all = regions(&list, crop);
    let pictures: Vec<Region> = all
        .iter()
        .copied()
        .filter(|r| r.px_per_pt.is_some())
        .collect();
    if pictures.is_empty() { all } else { pictures }
}

fn lines_sorted(s: &str) -> String {
    let mut l: Vec<String> = s.lines().map(normalize).filter(|l| !l.is_empty()).collect();
    l.sort();
    l.join("\n")
}

fn score(case: &Case, text: &str) -> f32 {
    match case.check {
        Check::Ordered => cer(text, &case.truth),
        Check::Lines => cer(&lines_sorted(text), &lines_sorted(&case.truth)),
        Check::Exact => {
            levenshtein(text, &case.truth) as f32 / case.truth.chars().count().max(1) as f32
        }
    }
}

/// The same pictures read as the whole-document recognition reads pages: rendered at
/// 300 dpi, lines in the engine's order, words joined by spaces.
fn read_before(e: &Engine, case: &Case) -> (f32, Duration) {
    let doc = Document::open(case.pdf.clone()).unwrap();
    let renderer = Renderer::new(doc.clone());
    let cancel = livegen::go();
    let start = Instant::now();
    let mut text = String::new();
    for r in case_regions(&doc) {
        let (img, to_page) =
            pagebara_ocr::live::render_region(&renderer, 0, r.rect, 300.0 / 72.0, &cancel).unwrap();
        let lines = e
            .recognize(&img, &pagebara_ocr::EngineParams::default(), &cancel)
            .unwrap();
        text.push_str(&pagebara_ocr::to_page_space(0, lines, &to_page).text());
        text.push('\n');
    }
    (score(case, &text), start.elapsed())
}

fn read(e: &Engine, case: &Case) -> Outcome {
    let doc = Document::open(case.pdf.clone()).unwrap();
    let renderer = Renderer::new(doc.clone());
    let params = LiveParams::default();
    let cancel = livegen::go();
    let regions = case_regions(&doc);
    assert!(!regions.is_empty(), "{}: no region found", case.name);
    let start = Instant::now();
    let mut lines = Vec::new();
    let mut area: Option<Rect> = None;
    let mut first = None;
    for r in &regions {
        // The pointer rests in the middle of the picture.
        let c = r.rect;
        let at = Point::new((c.x0 + c.x1) / 2.0, (c.y0 + c.y1) / 2.0);
        let found = recognize_region_live(
            e,
            &renderer,
            0,
            r,
            &params,
            &|| Some(at),
            &mut |so_far| {
                if first.is_none() && !so_far.is_empty() {
                    // Selectable: the page laid out with what was read so far.
                    let _ = page_with_live(&doc, 0, so_far, Some(c));
                    first = Some(start.elapsed());
                }
            },
            &cancel,
        )
        .unwrap();
        lines.extend(found);
        area = Some(area.map_or(r.rect, |a| a.union(&r.rect)));
    }
    let page = page_with_live(&doc, 0, &lines, area);
    let latency = start.elapsed();
    let text = copy_text(&page, 0..page.chars.len());
    let cer = score(case, &text);
    if let Some(dir) = std::env::var_os("PAGEBARA_OCR_KEEP") {
        let dir = std::path::PathBuf::from(dir);
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join(format!("live-{}.pdf", case.name)), &case.pdf);
        let _ = std::fs::write(dir.join(format!("live-{}.txt", case.name)), &text);
    }
    Outcome {
        text,
        cer,
        first: first.unwrap_or(latency),
        latency,
        regions: regions.len(),
    }
}

#[test]
fn live_text_benchmark() {
    let e = need_engine!();
    let mut failures = Vec::new();
    // With PAGEBARA_LIVE_BASELINE, also as whole-document recognition reads pages.
    let baseline = std::env::var_os("PAGEBARA_LIVE_BASELINE").is_some();
    eprintln!(
        "{:<20} {:>7} {:>9} {:>9} {:>8} {:>12} {:>12}",
        "case", "CER", "first", "all", "regions", "CER (300dpi)", "ms (300dpi)"
    );
    // Warm up: the first inference sets up the runtime's buffers.
    let _ = read(e, &livegen::small());
    for case in livegen::all() {
        let o = read(e, &case);
        let before = if baseline {
            let (c, t) = read_before(e, &case);
            format!("{:>11.2}% {:>10.0}ms", c * 100.0, t.as_secs_f64() * 1000.0)
        } else {
            String::new()
        };
        eprintln!(
            "{:<20} {:>6.2}% {:>7.0}ms {:>7.0}ms {:>8} {before}",
            case.name,
            o.cer * 100.0,
            o.first.as_secs_f64() * 1000.0,
            o.latency.as_secs_f64() * 1000.0,
            o.regions
        );
        if case.name == "a4-scan-300dpi" && o.first * 2 > o.latency {
            failures.push(format!(
                "{}: the text under the pointer took {:?} of {:?}",
                case.name, o.first, o.latency
            ));
        }
        if o.cer > case.max_cer {
            failures.push(format!(
                "{}: CER {:.3} > {}\n--- got\n{}\n--- want\n{}",
                case.name, o.cer, case.max_cer, o.text, case.truth
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

#[test]
fn code_keeps_its_indentation() {
    let e = need_engine!();
    for case in [livegen::code_light(), livegen::code_dark()] {
        let o = read(e, &case);
        let want: Vec<&str> = case.truth.lines().collect();
        let got: Vec<&str> = o.text.lines().collect();
        assert_eq!(got.len(), want.len(), "{}:\n{}", case.name, o.text);
        for (g, w) in got.iter().zip(&want) {
            let indent = |s: &str| s.len() - s.trim_start().len();
            assert_eq!(indent(g), indent(w), "{}: {g:?} for {w:?}", case.name);
        }
    }
}

#[test]
fn code_keeps_underscores_and_braces() {
    let e = need_engine!();
    let dark = read(e, &livegen::code_dark()).text;
    assert!(dark.contains("if __name__ == \"__main__\":"), "{dark}");
    let light = read(e, &livegen::code_light()).text;
    for line in ["fn main() {", "for word in text.split_whitespace() {"] {
        assert!(light.contains(line), "{line:?} in\n{light}");
    }
}

#[test]
fn table_rows_read_across_with_tabs() {
    let e = need_engine!();
    let o = read(e, &livegen::table());
    let rows: Vec<&str> = o.text.lines().collect();
    assert_eq!(rows.len(), livegen::TABLE.len(), "{}", o.text);
    for (got, want) in rows.iter().zip(livegen::TABLE) {
        let cells: Vec<&str> = got.split('\t').map(str::trim).collect();
        assert_eq!(cells.len(), want.len(), "{got:?}");
        let errors: usize = cells.iter().zip(want).map(|(g, w)| levenshtein(g, w)).sum();
        assert!(errors <= 1, "{got:?} for {want:?}");
    }
}

#[test]
fn columns_read_one_after_the_other() {
    let e = need_engine!();
    let o = read(e, &livegen::two_columns());
    let first_right = o.text.find("Today").expect("right column");
    let last_left = o.text.find("summer").expect("left column");
    assert!(last_left < first_right, "{}", o.text);
}

#[test]
fn real_text_beside_a_picture_wins() {
    let e = need_engine!();
    let case = livegen::screenshot();
    let doc = Document::open(case.pdf.clone()).unwrap();
    let renderer = Renderer::new(doc.clone());
    let r = case_regions(&doc)[0];
    let cancel = livegen::go();
    let lines = recognize_region(e, &renderer, 0, &r, &LiveParams::default(), &cancel).unwrap();
    let page = page_with_live(&doc, 0, &lines, None);
    // The whole page: the real heading first, the picture's text, the real caption last.
    assert!(
        page.text.starts_with("Setting up invoices"),
        "{}",
        page.text
    );
    assert!(
        page.text.trim_end().ends_with("the settings window."),
        "{}",
        page.text
    );
    assert!(page.text.contains("Save changes"));
}
