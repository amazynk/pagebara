//! End-to-end OCR of synthetic scans. Tests needing the models skip (with a note) unless
//! `PAGEBARA_OCR_MODELS` names a directory holding the Latin model files (other scripts skip
//! without theirs, and without an installed font for their sample text); set
//! `PAGEBARA_OCR_KEEP` to a directory to keep the PDFs they write.

mod synth;

use std::process::Command;
use std::sync::OnceLock;
use std::sync::atomic::AtomicBool;

use pagebara_content::{Interpreter, Matrix, Rect, extract_text};
use pagebara_ocr::{Engine, Options, Summary, ocr_document, page_text};
use pagebara_pages::PageTree;
use pagebara_render::page_matrix;
use pagebara_syntax::Document;
use synth::*;

fn engine() -> Option<&'static Engine> {
    static E: OnceLock<Option<Engine>> = OnceLock::new();
    E.get_or_init(|| {
        let sources = models()?;
        let cancel = AtomicBool::new(false);
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

fn truth() -> String {
    let mut t = vec![TITLE];
    t.extend_from_slice(LINES);
    t.join("\n")
}

/// The word boxes of the text page, in its page space.
fn truth_words() -> Vec<(String, Rect)> {
    let data = text_pdf(TITLE, LINES, 11.0);
    let doc = Document::open(data).unwrap();
    let tree = PageTree::open(&doc);
    let p = tree.page(0).unwrap();
    let d = doc.get(p.r).unwrap();
    let d = d.as_dict().unwrap();
    let res = doc.get_in(d, b"Resources").as_dict().unwrap().clone();
    let list = Interpreter::new(&doc).page(d, &res);
    let t = extract_text(&doc, &list);
    t.words
        .iter()
        .map(|w| (t.word_text(w).to_owned(), w.bbox))
        .collect()
}

/// OCRs `data` and returns the saved PDF and what was done.
fn run(e: &Engine, data: Vec<u8>, opts: &Options) -> (Vec<u8>, Summary) {
    let mut doc = Document::open(data).unwrap();
    let cancel = AtomicBool::new(false);
    let s = ocr_document(&mut doc, e, opts, &|_| {}, &cancel).unwrap();
    let out = doc.save_incremental().unwrap();
    (out, s)
}

fn keep(name: &str, data: &[u8]) -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(std::env::var_os("PAGEBARA_OCR_KEEP")?);
    std::fs::create_dir_all(&dir).ok()?;
    let p = dir.join(name);
    std::fs::write(&p, data).ok()?;
    Some(p)
}

/// Checks the result with poppler, MuPDF and qpdf where they are installed.
fn external_checks(name: &str, out: &[u8], want: &str, max_cer: f32) {
    external_checks_with(name, out, want, max_cer, cer);
}

/// Character error rate of the lines sorted, for text whose line order other tools choose
/// differently (turned pages).
fn line_cer(got: &str, want: &str) -> f32 {
    let sorted = |s: &str| {
        let mut l: Vec<String> = s.lines().map(normalize).filter(|l| !l.is_empty()).collect();
        l.sort();
        l.join("\n")
    };
    cer(&sorted(got), &sorted(want))
}

fn external_checks_with(
    name: &str,
    out: &[u8],
    want: &str,
    max_cer: f32,
    cer: fn(&str, &str) -> f32,
) {
    let dir = std::env::temp_dir().join(format!("pagebara-ocr-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, out).unwrap();
    if let Ok(o) = Command::new("pdftotext").arg(&path).arg("-").output() {
        let got = String::from_utf8_lossy(&o.stdout);
        let c = cer(&got, want);
        assert!(c <= max_cer, "pdftotext CER {c}:\n{got}");
        assert!(
            o.stderr.is_empty(),
            "{}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
    if let Ok(o) = Command::new("mutool")
        .args(["draw", "-q", "-F", "txt", "-o", "-"])
        .arg(&path)
        .output()
    {
        let got = String::from_utf8_lossy(&o.stdout);
        let c = cer(&got, want);
        assert!(c <= max_cer, "mutool CER {c}:\n{got}");
        let err = String::from_utf8_lossy(&o.stderr);
        assert!(!err.contains("error"), "{err}");
    }
    if let Ok(o) = Command::new("qpdf").arg("--check").arg(&path).output() {
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stdout));
        let msg = String::from_utf8_lossy(&o.stdout);
        assert!(!msg.contains("WARNING"), "{msg}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// Mean intersection over union of each true word box with the recognised word of the
/// same text (by order of appearance), and the share of true words matched.
fn box_agreement(doc: &Document, to_truth: &Matrix) -> (f32, f32) {
    let tree = PageTree::open(doc);
    let p = tree.page(0).unwrap();
    let d = doc.get(p.r).unwrap();
    let d = d.as_dict().unwrap();
    let res = doc.get_in(d, b"Resources").as_dict().unwrap().clone();
    let list = Interpreter::new(doc).page(d, &res);
    let t = extract_text(doc, &list);
    let got: Vec<(String, Rect)> = t
        .words
        .iter()
        .map(|w| {
            let b = w.bbox;
            let r = Rect::from_points(&b.corners().map(|c| to_truth.apply(c)));
            (t.word_text(w).to_owned(), r)
        })
        .collect();
    let want = truth_words();
    let mut used = vec![false; got.len()];
    let (mut sum, mut n) = (0.0, 0);
    for (text, r) in &want {
        let Some(i) = (0..got.len()).find(|&i| !used[i] && got[i].0 == *text) else {
            continue;
        };
        used[i] = true;
        let g = got[i].1;
        let ix = (r.x1.min(g.x1) - r.x0.max(g.x0)).max(0.0);
        let iy = (r.y1.min(g.y1) - r.y0.max(g.y0)).max(0.0);
        let inter = ix * iy;
        let union = r.width() * r.height() + g.width() * g.height() - inter;
        sum += inter / union;
        n += 1;
    }
    (sum / n.max(1) as f32, n as f32 / want.len() as f32)
}

fn scan(dpi: f32) -> (Vec<u8>, usize, usize) {
    render_gray(&text_pdf(TITLE, LINES, 11.0), dpi)
}

#[test]
fn clean_scan_round_trip() {
    let e = need_engine!();
    let (gray, w, h) = scan(300.0);
    let data = scan_pdf(
        &gray,
        w,
        h,
        [0.0, 0.0, 612.0, 792.0],
        [612.0, 0.0, 0.0, 792.0, 0.0, 0.0],
        "",
    );
    let (out, s) = run(e, data, &Options::default());
    keep("clean.pdf", &out);
    assert_eq!(s.recognized, [0]);
    let doc = Document::open(out.clone()).unwrap();
    let got = page_text(&doc, 0);
    let c = cer(&got, &truth());
    assert!(c < 0.01, "CER {c}:\n{got}");
    let (iou, matched) = box_agreement(&doc, &Matrix::IDENTITY);
    assert!(matched > 0.95 && iou > 0.6, "IoU {iou}, matched {matched}");
    external_checks("clean.pdf", &out, &truth(), 0.01);

    // A second run finds the text layer and leaves the page alone.
    let (_, s) = run(e, out, &Options::default());
    assert_eq!(s.skipped, [0]);
}

#[test]
fn noisy_skewed_scan() {
    let e = need_engine!();
    let (gray, w, h) = scan(300.0);
    let bad = degrade(&gray, w, h, 1.5, 60.0, 7);
    let data = scan_pdf(
        &bad,
        w,
        h,
        [0.0, 0.0, 612.0, 792.0],
        [612.0, 0.0, 0.0, 792.0, 0.0, 0.0],
        "",
    );
    let (out, _) = run(e, data, &Options::default());
    keep("noisy.pdf", &out);
    let doc = Document::open(out.clone()).unwrap();
    let got = page_text(&doc, 0);
    let c = cer(&got, &truth());
    assert!(c < 0.03, "CER {c}:\n{got}");
    external_checks("noisy.pdf", &out, &truth(), 0.03);
}

#[test]
fn low_resolution_scan() {
    let e = need_engine!();
    let (gray, w, h) = scan(150.0);
    let data = scan_pdf(
        &gray,
        w,
        h,
        [0.0, 0.0, 612.0, 792.0],
        [612.0, 0.0, 0.0, 792.0, 0.0, 0.0],
        "",
    );
    let (out, _) = run(e, data, &Options::default());
    let doc = Document::open(out).unwrap();
    let got = page_text(&doc, 0);
    let c = cer(&got, &truth());
    assert!(c < 0.02, "CER {c}:\n{got}");
}

/// The scan turned a quarter counter-clockwise, as stored by a scanner that relies on
/// `/Rotate 90` to show it upright.
fn rotate_ccw(gray: &[u8], w: usize, h: usize) -> Vec<u8> {
    let mut out = vec![0; w * h];
    for y in 0..h {
        for x in 0..w {
            // Old (x, y) lands at (y, w - 1 - x) in the h-wide result.
            out[(w - 1 - x) * h + y] = gray[y * w + x];
        }
    }
    out
}

/// Page geometry variants: the text must read the same and land where the picture shows it.
#[test]
fn rotate_cropbox_userunit() {
    let e = need_engine!();
    let (gray, w, h) = scan(300.0);
    let turned = rotate_ccw(&gray, w, h);
    let cases: Vec<(&str, Vec<u8>)> = vec![
        (
            "rotate",
            scan_pdf(
                &turned,
                h,
                w,
                [0.0, 0.0, 792.0, 612.0],
                [792.0, 0.0, 0.0, 612.0, 0.0, 0.0],
                "/Rotate 90",
            ),
        ),
        (
            "cropbox",
            scan_pdf(
                &gray,
                w,
                h,
                [0.0, 0.0, 712.0, 892.0],
                [612.0, 0.0, 0.0, 792.0, 50.0, 60.0],
                "/CropBox[50 60 662 852]",
            ),
        ),
        (
            "userunit",
            scan_pdf(
                &gray,
                w,
                h,
                [0.0, 0.0, 306.0, 396.0],
                [306.0, 0.0, 0.0, 396.0, 0.0, 0.0],
                "/UserUnit 2",
            ),
        ),
    ];
    for (name, data) in cases {
        let (out, s) = run(e, data, &Options::default());
        keep(&format!("{name}.pdf"), &out);
        assert_eq!(s.recognized, [0], "{name}");
        let doc = Document::open(out.clone()).unwrap();
        let got = page_text(&doc, 0);
        let c = cer(&got, &truth());
        assert!(c < 0.01, "{name}: CER {c}:\n{got}");
        // Page space → displayed points at scale 1 (user unit included) → the text page.
        let tree = PageTree::open(&doc);
        let p = tree.page(0).unwrap();
        let cb = p.crop_box;
        let crop = Rect::new(cb.x0 as f32, cb.y0 as f32, cb.x1 as f32, cb.y1 as f32);
        let shown = page_matrix(crop, p.rotate, p.user_unit as f32, 0, 0);
        let text_page = page_matrix(Rect::new(0.0, 0.0, 612.0, 792.0), 0, 1.0, 0, 0);
        let to_truth = shown.then(&text_page.invert().unwrap());
        let (iou, matched) = box_agreement(&doc, &to_truth);
        assert!(
            matched > 0.95 && iou > 0.6,
            "{name}: IoU {iou}, matched {matched}"
        );
        external_checks(&format!("{name}.pdf"), &out, &truth(), 0.01);
    }
}

#[test]
fn pages_with_text_are_skipped_unless_forced() {
    let e = need_engine!();
    let data = text_pdf(TITLE, LINES, 11.0);
    let (_, s) = run(e, data.clone(), &Options::default());
    assert_eq!((s.skipped.as_slice(), s.recognized.len()), (&[0][..], 0));
    let opts = Options {
        force: true,
        ..Options::default()
    };
    let (out, s) = run(e, data, &opts);
    assert_eq!(s.recognized, [0]);
    let doc = Document::open(out).unwrap();
    assert!(pagebara_ocr::layer_font(&doc, 0).is_some());
}

#[test]
fn cancelled_before_start() {
    let e = need_engine!();
    let (gray, w, h) = scan(100.0);
    let data = scan_pdf(
        &gray,
        w,
        h,
        [0.0, 0.0, 612.0, 792.0],
        [612.0, 0.0, 0.0, 792.0, 0.0, 0.0],
        "",
    );
    let mut doc = Document::open(data).unwrap();
    let cancel = AtomicBool::new(true);
    let r = ocr_document(&mut doc, e, &Options::default(), &|_| {}, &cancel);
    assert!(matches!(r, Err(pagebara_ocr::Error::Cancelled)), "{r:?}");
    assert!(!doc.is_modified());
}

/// Several pages in parallel, with progress reports.
#[test]
fn parallel_pages_and_progress() {
    let e = need_engine!();
    let (gray, w, h) = scan(200.0);
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&gray, 6);
    let mut objs = vec![
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[4 0 R 5 0 R 6 0 R]/Count 3>>".to_vec(),
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
    for _ in 0..3 {
        objs.push(
            b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 7 0 R\
              /Resources<</XObject<</Im0 3 0 R>>>>>>"
                .to_vec(),
        );
    }
    objs.push(b"<</Length 33>>stream\nq 612 0 0 792 0 0 cm /Im0 Do Q\n\nendstream".to_vec());
    let mut doc = Document::open(pdf(&objs)).unwrap();
    let seen = std::sync::Mutex::new(Vec::new());
    let cancel = AtomicBool::new(false);
    let opts = Options {
        parallel_pages: 3,
        ..Options::default()
    };
    let s = ocr_document(
        &mut doc,
        e,
        &opts,
        &|p| seen.lock().unwrap().push((p.done, p.total)),
        &cancel,
    )
    .unwrap();
    assert_eq!(s.recognized, [0, 1, 2]);
    let mut seen = seen.into_inner().unwrap();
    seen.sort();
    assert_eq!(seen, [(1, 3), (2, 3), (3, 3)]);
    let out = doc.save_incremental().unwrap();
    let doc = Document::open(out.clone()).unwrap();
    for p in 0..3 {
        let c = cer(&page_text(&doc, p), &truth());
        assert!(c < 0.01, "page {p}: CER {c}");
    }
    // One font serves every page.
    let fonts: Vec<_> = (0..3).map(|p| pagebara_ocr::layer_font(&doc, p)).collect();
    assert!(fonts[0].is_some() && fonts.iter().all(|f| *f == fonts[0]));
}

/// Scans stored turned (no `/Rotate` to fix them): the text reads right and lies where
/// the picture shows it.
#[test]
fn sideways_and_upside_down_scans() {
    let e = need_engine!();
    let (gray, w, h) = scan(300.0);
    let once = rotate_ccw(&gray, w, h);
    let twice = rotate_ccw(&once, h, w);
    let thrice = rotate_ccw(&twice, w, h);
    // Each case: image, its size, the page size, and the matrix from page space to the
    // upright text page's space.
    let cases = [
        (
            "ccw",
            once,
            (h, w),
            (792.0, 612.0),
            Matrix::new(0.0, -1.0, 1.0, 0.0, 0.0, 792.0),
        ),
        (
            "half",
            twice,
            (w, h),
            (612.0, 792.0),
            Matrix::new(-1.0, 0.0, 0.0, -1.0, 612.0, 792.0),
        ),
        (
            "cw",
            thrice,
            (h, w),
            (792.0, 612.0),
            Matrix::new(0.0, 1.0, -1.0, 0.0, 612.0, 0.0),
        ),
    ];
    for (name, img, (iw, ih), (pw, ph), to_truth) in cases {
        let data = scan_pdf(
            &img,
            iw,
            ih,
            [0.0, 0.0, pw, ph],
            [pw, 0.0, 0.0, ph, 0.0, 0.0],
            "",
        );
        let (out, s) = run(e, data, &Options::default());
        keep(&format!("turned-{name}.pdf"), &out);
        assert_eq!(s.recognized, [0], "{name}");
        let doc = Document::open(out.clone()).unwrap();
        let got = page_text(&doc, 0);
        let c = cer(&got, &truth());
        assert!(c < 0.01, "{name}: CER {c}:\n{got}");
        let (iou, matched) = box_agreement(&doc, &to_truth);
        assert!(
            matched > 0.95 && iou > 0.6,
            "{name}: IoU {iou}, matched {matched}"
        );
        external_checks_with(
            &format!("turned-{name}.pdf"),
            &out,
            &truth(),
            0.01,
            line_cer,
        );
    }
}

/// Orientation can be turned off: an upside-down scan then reads as nonsense.
#[test]
fn orientation_off_reads_as_shown() {
    let e = need_engine!();
    let (gray, w, h) = scan(200.0);
    let once = rotate_ccw(&gray, w, h);
    let twice = rotate_ccw(&once, h, w);
    let data = scan_pdf(
        &twice,
        w,
        h,
        [0.0, 0.0, 612.0, 792.0],
        [612.0, 0.0, 0.0, 792.0, 0.0, 0.0],
        "",
    );
    let mut opts = Options::default();
    opts.engine.orientation = false;
    let (out, _) = run(e, data, &opts);
    let doc = Document::open(out).unwrap();
    assert!(cer(&page_text(&doc, 0), &truth()) > 0.3);
}

/// A page of `lines` in a system font covering them (embedded, with `/ToUnicode`), or
/// `None` when none is installed.
fn script_pdf(lines: &[&str], size: f32) -> Option<Vec<u8>> {
    font_page(lines, &[], size)
}

/// [`script_pdf`] with `columns` below the lines, in vertical writing: read top to bottom,
/// right to left, one upright character under the other.
fn font_page(lines: &[&str], columns: &[&str], size: f32) -> Option<Vec<u8>> {
    use pagebara_font::fallback::{FontDb, find_for_char};
    static DB: OnceLock<FontDb> = OnceLock::new();
    let db = DB.get_or_init(FontDb::system);
    let all: String = lines.concat() + &columns.concat();
    let c = all.chars().find(|c| !c.is_ascii())?;
    let face = find_for_char(db, c)?;
    let font = db.load(face)?;
    let covered = font.as_sfnt().is_some_and(|s| {
        all.chars()
            .all(|c| c == ' ' || s.glyph_for_char(c).is_some_and(|g| g != 0))
    });
    if !covered {
        return None;
    }
    let data = std::fs::read(&face.path).ok()?;
    let mut emb = pagebara_font::subset::Embedder::new(data, face.index)?;
    let mut content = String::new();
    let hex = |b: Vec<u8>| -> String { b.iter().map(|b| format!("{b:02X}")).collect() };
    for (i, l) in lines.iter().enumerate() {
        let codes = hex(emb.encode(l));
        let y = 720.0 - i as f32 * size * 1.6;
        content.push_str(&format!("BT /F1 {size} Tf 72 {y} Td <{codes}> Tj ET\n"));
    }
    let top = 720.0 - lines.len() as f32 * size * 1.6;
    for (i, l) in columns.iter().enumerate() {
        let x = 520.0 - i as f32 * size * 1.8;
        for (j, c) in l.chars().enumerate() {
            let c = c.to_string();
            let x = x - emb.text_width(&c) * size / 2000.0;
            let y = top - (j + 1) as f32 * size * 1.1;
            let codes = hex(emb.encode(&c));
            content.push_str(&format!("BT /F1 {size} Tf {x} {y} Td <{codes}> Tj ET\n"));
        }
    }
    let mut doc = Document::open(pdf(&[
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
        b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]>>".to_vec(),
    ]))
    .ok()?;
    let f = emb.write(&mut doc)?;
    let page = PageTree::open(&doc).page(0)?.r;
    let mut d = doc.get(page)?.as_dict()?.clone();
    let mut fonts = pagebara_syntax::Dict::new();
    fonts.set(b"F1", pagebara_syntax::Object::Ref(f));
    let mut res = pagebara_syntax::Dict::new();
    res.set(b"Font", pagebara_syntax::Object::Dict(fonts));
    d.set(b"Resources", pagebara_syntax::Object::Dict(res));
    let c = doc.add(pagebara_syntax::Stream::new(
        pagebara_syntax::Dict::new(),
        content.into_bytes(),
    ));
    d.set(b"Contents", pagebara_syntax::Object::Ref(c));
    doc.set(page, d);
    doc.save_full().ok()
}

fn script_engine(script: pagebara_ocr::Script) -> Option<&'static Engine> {
    use std::collections::HashMap;
    use std::sync::Mutex;
    static E: OnceLock<Mutex<HashMap<pagebara_ocr::Script, &'static Engine>>> = OnceLock::new();
    let dir = std::path::PathBuf::from(std::env::var_os("PAGEBARA_OCR_MODELS")?);
    let src = pagebara_ocr::ModelSource::Dir(dir);
    if !script.files().iter().all(|f| src.has(f)) {
        return None;
    }
    let mut map = E.get_or_init(Mutex::default).lock().unwrap();
    if let Some(e) = map.get(&script) {
        return Some(e);
    }
    let cancel = AtomicBool::new(false);
    let e = Engine::load(&[src], script, &|_| {}, &cancel).expect("models load");
    let e: &'static Engine = Box::leak(Box::new(e));
    map.insert(script, e);
    Some(e)
}

/// Recognises a scan of `lines` set in an installed font for `script`; the text layer
/// must extract (here and with poppler) as the lines read.
fn script_round_trip(script: pagebara_ocr::Script, name: &str, lines: &[&str], max_cer: f32) {
    let Some(e) = script_engine(script) else {
        eprintln!(
            "skipped {name}: set PAGEBARA_OCR_MODELS to a directory with the {script:?} models"
        );
        return;
    };
    let Some(page) = script_pdf(lines, 16.0) else {
        eprintln!("skipped {name}: no installed font covers the {name} sample text");
        return;
    };
    let (gray, w, h) = render_gray(&page, 300.0);
    let data = scan_pdf(
        &gray,
        w,
        h,
        [0.0, 0.0, 612.0, 792.0],
        [612.0, 0.0, 0.0, 792.0, 0.0, 0.0],
        "",
    );
    let (out, s) = run(e, data, &Options::default());
    keep(&format!("{name}.pdf"), &out);
    assert_eq!(s.recognized, [0], "{name}");
    let doc = Document::open(out.clone()).unwrap();
    let got = page_text(&doc, 0);
    let want = lines.join("\n");
    let c = cer(&got, &want);
    assert!(c <= max_cer, "{name}: CER {c}:\n{got}");
    // Every line, searched as a whole, is found in the extracted text.
    let flat = normalize(&got);
    let found = lines
        .iter()
        .filter(|l| flat.contains(&normalize(l)))
        .count();
    assert!(
        found * 2 >= lines.len(),
        "{name}: {found} of {} lines found:\n{got}",
        lines.len()
    );
    external_checks(&format!("{name}.pdf"), &out, &want, max_cer);
}

#[test]
fn cyrillic_scan() {
    script_round_trip(
        pagebara_ocr::Script::Cyrillic,
        "cyrillic",
        &[
            "Квартальный отчёт о состоянии рек",
            "Река берёт начало в северных холмах",
            "и течёт около девяноста миль до моря.",
            "В долине находятся три торговых города.",
        ],
        0.03,
    );
}

#[test]
fn greek_scan() {
    script_round_trip(
        pagebara_ocr::Script::Greek,
        "greek",
        &[
            "Τριμηνιαία έκθεση για τα ποτάμια",
            "Ο ποταμός πηγάζει από τους βόρειους λόφους",
            "και κυλά περίπου ενενήντα μίλια ως τη θάλασσα.",
            "Στην κοιλάδα του υπάρχουν τρεις πόλεις.",
        ],
        0.03,
    );
}

#[test]
fn chinese_scan() {
    script_round_trip(
        pagebara_ocr::Script::Chinese,
        "chinese",
        &[
            "河流季度报告",
            "这条河发源于北部的丘陵地带",
            "全长约九十英里，最后流入大海。",
            "河谷中有三个集镇和一座石桥。",
        ],
        0.05,
    );
}

#[test]
fn japanese_scan() {
    script_round_trip(
        pagebara_ocr::Script::Chinese,
        "japanese",
        &[
            "川の四半期報告",
            "この川は北の丘陵から流れ出し、",
            "約九十マイル先で海に注ぎます。",
            "谷には三つの町と石の橋があります。",
        ],
        0.05,
    );
}

/// Without the page classifier the lines' shape and the line classifier decide.
#[test]
fn turned_scans_with_the_line_classifier_only() {
    if engine().is_none() {
        eprintln!("skipped: set PAGEBARA_OCR_MODELS to a directory with the OCR models");
        return;
    }
    let dir = std::path::PathBuf::from(std::env::var_os("PAGEBARA_OCR_MODELS").unwrap());
    let read = |f: &pagebara_ocr::ModelFile| std::fs::read(dir.join(f.name)).unwrap();
    let [det, _, line, rec, dict] = pagebara_ocr::Script::Latin.files();
    let e = Engine::from_bytes(
        read(&det),
        read(&rec),
        &String::from_utf8(read(&dict)).unwrap(),
    )
    .unwrap()
    .with_orientation(None, Some(read(&line)))
    .unwrap();
    let (gray, w, h) = scan(200.0);
    let once = rotate_ccw(&gray, w, h);
    let twice = rotate_ccw(&once, h, w);
    let thrice = rotate_ccw(&twice, w, h);
    for (name, img, (iw, ih)) in [
        ("ccw", once, (h, w)),
        ("half", twice, (w, h)),
        ("cw", thrice, (h, w)),
    ] {
        let (pw, ph) = if iw > ih {
            (792.0, 612.0)
        } else {
            (612.0, 792.0)
        };
        let data = scan_pdf(
            &img,
            iw,
            ih,
            [0.0, 0.0, pw, ph],
            [pw, 0.0, 0.0, ph, 0.0, 0.0],
            "",
        );
        let (out, _) = run(&e, data, &Options::default());
        let doc = Document::open(out).unwrap();
        let got = page_text(&doc, 0);
        let c = cer(&got, &truth());
        assert!(c < 0.02, "{name}: CER {c}:\n{got}");
    }
}

/// The Chinese-script engine, with or without the page orientation classifier.
fn chinese_engine(page_classifier: bool) -> Option<Engine> {
    let dir = std::path::PathBuf::from(std::env::var_os("PAGEBARA_OCR_MODELS")?);
    let files = pagebara_ocr::Script::Chinese.files();
    if !files.iter().all(|f| dir.join(f.name).exists()) {
        return None;
    }
    let read = |f: &pagebara_ocr::ModelFile| std::fs::read(dir.join(f.name)).unwrap();
    let [det, page, line, rec, dict] = files;
    let e = Engine::from_bytes(
        read(&det),
        read(&rec),
        &String::from_utf8(read(&dict)).unwrap(),
    )
    .unwrap();
    let page = page_classifier.then(|| read(&page));
    Some(e.with_orientation(page, Some(read(&line))).unwrap())
}

const HEADING_JA: &str = "第三章川と町の記録";

const VERTICAL_JA: &[&str] = &[
    "川の四半期報告",
    "この川は北の丘陵から流れ出し",
    "約九十マイル先で海に注ぎます",
    "谷には三つの町と石の橋があります",
    "水位は三月から六月まで毎日測られた",
];

/// What `e` reads on a 300 dpi scan of `page` turned `quarters` counter-clockwise: the
/// lines in reading order, and how many read downwards.
fn read_scan(e: &Engine, page: &[u8], quarters: usize) -> (String, usize) {
    let (mut gray, mut w, mut h) = render_gray(page, 300.0);
    for _ in 0..quarters {
        gray = rotate_ccw(&gray, w, h);
        (w, h) = (h, w);
    }
    let img = pagebara_ocr::RgbImage::from_gray(w, h, &gray);
    let cancel = AtomicBool::new(false);
    let lines = e
        .recognize(&img, &pagebara_ocr::EngineParams::default(), &cancel)
        .unwrap();
    let down = lines
        .iter()
        .filter(|l| l.rect.u[1] > l.rect.u[0].abs() * 2.0)
        .count();
    let text: Vec<String> = lines
        .iter()
        .map(|l| {
            let words: Vec<&str> = l.words.iter().map(|w| w.text.as_str()).collect();
            words.join(" ")
        })
        .collect();
    (text.join("\n"), down)
}

/// Vertical Japanese (columns right to left, under a horizontal heading) reads in order
/// with and without the page classifier, as columns read downwards beside a horizontal
/// heading; the same text set horizontally and scanned sideways still reads as turned.
/// The text layer extracts with the lines in some order (the columns' direction first).
#[test]
fn vertical_cjk_writing() {
    let Some(page) = font_page(&[HEADING_JA], VERTICAL_JA, 20.0) else {
        eprintln!("skipped: no installed font covers the Japanese sample text");
        return;
    };
    let mut lines = vec![HEADING_JA];
    lines.extend_from_slice(VERTICAL_JA);
    let horizontal = font_page(&lines, &[], 20.0).unwrap();
    let want = lines.join("\n");
    let (gray, w, h) = render_gray(&page, 300.0);
    let scan = scan_pdf(
        &gray,
        w,
        h,
        [0.0, 0.0, 612.0, 792.0],
        [612.0, 0.0, 0.0, 792.0, 0.0, 0.0],
        "",
    );
    for page_classifier in [true, false] {
        let Some(e) = chinese_engine(page_classifier) else {
            eprintln!("skipped: set PAGEBARA_OCR_MODELS to a directory with the Chinese models");
            return;
        };
        let tag = if page_classifier { "page" } else { "lines" };
        let (got, down) = read_scan(&e, &page, 0);
        let c = cer(&got, &want);
        assert!(c < 0.03, "vertical ({tag}): CER {c}:\n{got}");
        assert_eq!(
            down,
            VERTICAL_JA.len(),
            "vertical ({tag}): columns read down"
        );

        let (got, down) = read_scan(&e, &horizontal, 1);
        let c = cer(&got, &want);
        assert!(c < 0.03, "turned ({tag}): CER {c}:\n{got}");
        // Turned back: every line runs up the scan.
        assert_eq!(down, 0, "turned ({tag})");

        let (out, s) = run(&e, scan.clone(), &Options::default());
        keep(&format!("vertical-{tag}.pdf"), &out);
        assert_eq!(s.recognized, [0]);
        let got = page_text(&Document::open(out.clone()).unwrap(), 0);
        let c = line_cer(&got, &want);
        assert!(c < 0.03, "vertical text layer ({tag}): CER {c}:\n{got}");
        external_checks_with(&format!("vertical-{tag}.pdf"), &out, &want, 0.03, line_cer);
    }
}

/// Horizontal Japanese with one- and two-character lines, scanned turned a quarter either
/// way, is a turned page rather than vertical writing: every line, the short ones too,
/// reads as set.
#[test]
fn turned_cjk_with_short_lines() {
    let lines = [
        "川の四半期報告",
        "目次",
        "この川は北の丘陵から流れ出し",
        "山",
        "約九十マイル先で海に注ぎます",
        "町",
        "谷には三つの町と石の橋があります",
        "第二",
    ];
    let Some(page) = font_page(&lines, &[], 20.0) else {
        eprintln!("skipped: no installed font covers the Japanese sample text");
        return;
    };
    let want = lines.join("\n");
    for page_classifier in [true, false] {
        let Some(e) = chinese_engine(page_classifier) else {
            eprintln!("skipped: set PAGEBARA_OCR_MODELS to a directory with the Chinese models");
            return;
        };
        for quarters in [1, 3] {
            let tag = format!(
                "{} {quarters}",
                if page_classifier { "page" } else { "lines" }
            );
            let (got, down) = read_scan(&e, &page, quarters);
            let c = cer(&got, &want);
            assert!(c < 0.03, "turned ({tag}): CER {c}:\n{got}");
            // Every line runs along the text: up the scan, or down it when turned clockwise.
            let along = if quarters == 3 { lines.len() } else { 0 };
            assert_eq!(down, along, "turned ({tag}):\n{got}");
            let read: Vec<&str> = got.lines().map(str::trim).collect();
            for short in lines.iter().filter(|l| l.chars().count() <= 2) {
                assert!(
                    read.contains(short),
                    "turned ({tag}): {short} not read:\n{got}"
                );
            }
        }
    }
}
