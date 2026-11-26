//! The lossless preset never changes what a page shows: every page of every sample renders
//! to the same pixels before and after, and the files stay valid.

use std::path::{Path, PathBuf};

use pagebara_optimize::{Effort, Options, Preset, optimize};
use pagebara_render::{PixelRect, RenderRequest, Renderer};
use pagebara_syntax::{Dict, Document, Object, Stream};

fn open(bytes: &[u8]) -> Option<Document> {
    match Document::open(bytes.to_vec()) {
        Ok(d) => Some(d),
        Err(pagebara_syntax::Error::PasswordRequired(l)) => l.unlock("user").ok(),
        Err(e) => panic!("{e}"),
    }
}

fn render_all(bytes: &[u8]) -> Vec<Vec<u8>> {
    render_doc(open(bytes).unwrap())
}

fn render_doc(doc: Document) -> Vec<Vec<u8>> {
    let r = Renderer::new(doc);
    (0..r.page_count())
        .map(|p| {
            let (w, h) = r.page_size(p).unwrap();
            let scale = 1.5;
            let rect = PixelRect {
                x: 0,
                y: 0,
                w: ((w * scale).ceil() as u32).max(1),
                h: ((h * scale).ceil() as u32).max(1),
            };
            r.render(p, &RenderRequest::new(scale, rect)).unwrap().data
        })
        .collect()
}

fn check(name: &str, bytes: &[u8], opts: &Options) -> pagebara_optimize::Report {
    let Some(doc) = open(bytes) else {
        return pagebara_optimize::Report::default();
    };
    let out = optimize(&doc, opts, None).unwrap();
    let before = render_doc(doc);
    let reopened = open(&out.bytes).unwrap();
    assert!(!reopened.was_repaired(), "{name}: output needed repair");
    let after = render_all(&out.bytes);
    assert_eq!(before.len(), after.len(), "{name}: page count");
    for (i, (a, b)) in before.iter().zip(&after).enumerate() {
        assert!(a == b, "{name}: page {} differs", i + 1);
    }
    out.report
}

/// The samples, written once for all tests of this run.
fn sample_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("optimize-samples");
        pagebara_samples::write_all(&dir).unwrap();
        dir
    })
    .clone()
}

fn sample_pdfs() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let dir = sample_dir();
    for e in std::fs::read_dir(&dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "pdf") {
            out.push((
                p.file_name().unwrap().to_string_lossy().into_owned(),
                std::fs::read(&p).unwrap(),
            ));
        }
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for f in [
        "crates/pages/tests/fixtures/rich.pdf",
        "crates/pages/tests/fixtures/rich-objstm.pdf",
        "crates/syntax/tests/fixtures/crypt/plain.pdf",
        "crates/syntax/tests/fixtures/crypt/aes-256-r6.pdf",
    ] {
        out.push((f.to_owned(), std::fs::read(root.join(f)).unwrap()));
    }
    out.sort();
    out
}

#[test]
fn lossless_keeps_every_sample_pixel_identical() {
    let opts = Preset::Lossless.options();
    for (name, bytes) in sample_pdfs() {
        let r = check(&name, &bytes, &opts);
        eprintln!(
            "{name}: {} -> {} ({:.1}%), {} merged, {} removed, {} resources, {} fonts subset",
            r.before,
            r.after,
            r.saved_percent(),
            r.objects_merged,
            r.objects_removed,
            r.resources_removed,
            r.fonts_subset
        );
    }
}

#[test]
fn every_lossless_option_alone_keeps_pixels() {
    let samples = sample_pdfs();
    let base = Options {
        object_streams: false,
        effort: Effort::Fast,
        dedupe: false,
        remove_unused: false,
        subset_fonts: false,
        ..Options::default()
    };
    let mut variants = vec![base];
    variants.push(Options {
        object_streams: true,
        ..base
    });
    variants.push(Options {
        effort: Effort::Maximum,
        ..base
    });
    variants.push(Options {
        dedupe: true,
        ..base
    });
    variants.push(Options {
        remove_unused: true,
        ..base
    });
    variants.push(Options {
        subset_fonts: true,
        ..base
    });
    let mut linear = base;
    linear.linearize = true;
    variants.push(linear);
    for v in variants {
        for (name, bytes) in samples.iter().filter(|s| {
            s.0.ends_with("lease.pdf") || s.0.ends_with("flyer.pdf") || s.0.ends_with("rich.pdf")
        }) {
            check(&format!("{name} {v:?}"), bytes, &v);
        }
    }
}

/// A page drawing one image twice from two identical copies, with an unused font and a
/// bilevel image in Flate.
fn generated() -> Vec<u8> {
    let mut doc = pagebara_pages::blank((1, 7));
    let px: Vec<u8> = (0..64 * 64 * 3).map(|i| (i * 13 % 256) as u8).collect();
    let mut img = Dict::new();
    img.set(b"Type", Object::name(b"XObject"));
    img.set(b"Subtype", Object::name(b"Image"));
    img.set(b"Width", 64);
    img.set(b"Height", 64);
    img.set(b"ColorSpace", Object::name(b"DeviceRGB"));
    img.set(b"BitsPerComponent", 8);
    let a = doc.add(Stream::new(img.clone(), px.clone()));
    let b = doc.add(Stream::new(img, px));
    // Scan-like: white with irregular black strokes.
    let mut bits = vec![0xffu8; 25 * 200];
    let mut s = 12345u32;
    for _ in 0..60 {
        s = s.wrapping_mul(1103515245).wrapping_add(12345);
        let (x0, y0, w, h) = (
            (s >> 8) % 180,
            (s >> 16) % 185,
            2 + (s >> 4) % 18,
            6 + (s >> 24) % 9,
        );
        for y in y0..y0 + h {
            for x in x0..x0 + w {
                bits[y as usize * 25 + x as usize / 8] &= !(0x80 >> (x % 8));
            }
        }
    }
    let mut bl = Dict::new();
    bl.set(b"Type", Object::name(b"XObject"));
    bl.set(b"Subtype", Object::name(b"Image"));
    bl.set(b"Width", 200);
    bl.set(b"Height", 200);
    bl.set(b"ColorSpace", Object::name(b"DeviceGray"));
    bl.set(b"BitsPerComponent", 1);
    bl.set(b"Filter", Object::name(b"FlateDecode"));
    let c = doc.add(Stream::new(bl, pagebara_filters::encode_flate(&bits, 1)));
    let mut font = Dict::new();
    font.set(b"Type", Object::name(b"Font"));
    font.set(b"Subtype", Object::name(b"Type1"));
    font.set(b"BaseFont", Object::name(b"Helvetica"));
    let f = doc.add(font);
    let mut xo = Dict::new();
    xo.set(b"A", a);
    xo.set(b"B", b);
    xo.set(b"C", c);
    let mut fonts = Dict::new();
    fonts.set(b"F1", f);
    fonts.set(b"Unused", f);
    let mut res = Dict::new();
    res.set(b"XObject", xo);
    res.set(b"Font", fonts);
    let content = b"q 100 0 0 100 50 600 cm /A Do Q q 100 0 0 100 200 600 cm /B Do Q q 200 0 0 200 50 300 cm /C Do Q BT /F1 24 Tf 50 200 Td (Hello) Tj ET".to_vec();
    let cs = doc.add(Stream::new(Dict::new(), content));
    let mut pd = Dict::new();
    pd.set(b"Type", Object::name(b"Page"));
    pd.set(b"Parent", pagebara_syntax::ObjRef::new(2, 0));
    pd.set(
        b"MediaBox",
        vec![
            Object::Int(0),
            Object::Int(0),
            Object::Int(612),
            Object::Int(792),
        ],
    );
    pd.set(b"Resources", res);
    pd.set(b"Contents", cs);
    let page = doc.add(pd);
    let mut pages = Dict::new();
    pages.set(b"Type", Object::name(b"Pages"));
    pages.set(b"Kids", vec![Object::Ref(page)]);
    pages.set(b"Count", 1);
    doc.set(pagebara_syntax::ObjRef::new(2, 0), pages);
    doc.save_full().unwrap()
}

#[test]
fn generated_duplicates_unused_resources_and_bilevel_images() {
    let bytes = generated();
    let mut opts = Preset::Lossless.options();
    opts.images.bitonal_g4 = true;
    let r = check("generated", &bytes, &opts);
    assert!(r.objects_merged >= 1, "{r:?}");
    assert!(r.resources_removed >= 1, "{r:?}");
    assert_eq!(r.images_reencoded, 1, "{r:?}");
    assert!(r.after < r.before);
}

#[test]
fn lossy_presets_shrink_images_and_stay_readable() {
    let bytes = generated();
    let doc = Document::open(bytes).unwrap();
    for p in [Preset::Balanced, Preset::Small] {
        let mut o = p.options();
        o.images.target_ppi = Some(20.0);
        o.grayscale = true;
        let out = optimize(&doc, &o, None).unwrap();
        let after = Document::open(out.bytes.clone()).unwrap();
        assert_eq!(render_all(&out.bytes).len(), 1);
        assert!(out.report.images_resampled >= 1, "{p:?} {:?}", out.report);
        assert!(!after.was_repaired());
    }
}

#[test]
fn signed_documents_warn_or_refuse() {
    let p = pagebara_samples_pki();
    let doc = Document::open(p).unwrap();
    assert!(pagebara_optimize::is_signed(&doc));
    let out = optimize(&doc, &Options::default(), None).unwrap();
    assert!(out.report.signed && !out.report.warnings.is_empty());
    let keep = Options {
        keep_signatures: true,
        ..Options::default()
    };
    assert_eq!(
        optimize(&doc, &keep, None).unwrap_err(),
        pagebara_optimize::Error::Signed
    );
}

fn pagebara_samples_pki() -> Vec<u8> {
    std::fs::read(sample_dir().join("signed.pdf")).unwrap()
}

/// A book from the local corpus (skipped when it is not there; the corpus is never
/// committed): smaller, and a spread of pages renders the same.
#[test]
fn corpus_book_shrinks_losslessly() {
    const BOOK: &str = "corpus/325462-093-sdm-vol-1-2abcd-3abcd-4.pdf";
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let Some(bytes) = here
        .ancestors()
        .find_map(|d| std::fs::read(d.join(BOOK)).ok())
    else {
        eprintln!("skipped: {BOOK} is missing");
        return;
    };
    let doc = Document::open(bytes).unwrap();
    let out = optimize(&doc, &Preset::Lossless.options(), None).unwrap();
    let r = &out.report;
    eprintln!(
        "{BOOK}: {} -> {} ({:.1}% smaller) in {:.2} s",
        r.before,
        r.after,
        r.saved_percent(),
        r.seconds
    );
    assert!(r.after < r.before);
    let a = Renderer::new(doc);
    let b = Renderer::new(Document::open(out.bytes).unwrap());
    let n = a.page_count();
    assert_eq!(n, b.page_count());
    for p in (0..n).step_by((n / 12).max(1)) {
        let (w, h) = a.page_size(p).unwrap();
        let rect = PixelRect {
            x: 0,
            y: 0,
            w: w.ceil() as u32,
            h: h.ceil() as u32,
        };
        let req = RenderRequest::new(1.0, rect);
        assert!(
            a.render(p, &req).unwrap().data == b.render(p, &req).unwrap().data,
            "page {}",
            p + 1
        );
    }
}

/// A classic file from object bodies, object `i + 1` being `objs[i]`, catalog first.
fn classic(objs: &[String]) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let x = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objs.len() + 1).bytes());
    for o in offs {
        out.extend(format!("{o:010} 00000 n\r\n").bytes());
    }
    let n = objs.len() + 1;
    out.extend(format!("trailer\n<</Size {n}/Root 1 0 R>>\nstartxref\n{x}\n%%EOF\n").bytes());
    out
}

fn stream(content: &str) -> String {
    format!("<</Length {}>>stream\n{content}\nendstream", content.len())
}

/// Inline images naming a colour space resource under a duplicate key (the PDF
/// Association's inline image abbreviation test) or as the base of an indexed space: the
/// resource stays.
#[test]
fn inline_image_colour_spaces_are_kept() {
    let objs = [
        "<</Type/Catalog/Pages 2 0 R>>".to_owned(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".into(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<</ColorSpace<</Pal[/Indexed/DeviceRGB 1<ff000000ff00>]/Base/DeviceGray>>>>/Contents 4 0 R>>".into(),
        stream(
            "q 40 0 0 40 5 5 cm BI /W 2 /H 1 /BPC 8 /CS /RGB /ColorSpace /Pal ID \x00\x01 EI Q \
             q 40 0 0 40 55 55 cm BI /W 2 /H 1 /BPC 8 /CS [/I /Base 1 <00ff>] ID \x00\x01 EI Q",
        ),
    ];
    check("inline", &classic(&objs), &Preset::Lossless.options());
}

/// Two optional content groups with equal dictionaries are still two groups (pdf.js
/// issue 269): merging them showed the hidden one.
#[test]
fn equal_optional_content_groups_stay_apart() {
    let objs = [
        "<</Type/Catalog/Pages 2 0 R/OCProperties<</OCGs[5 0 R 6 0 R]/D<</OFF[6 0 R]>>>>>>"
            .to_owned(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".into(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 100 100]/Resources<</Properties<</A 5 0 R/B 6 0 R>>>>/Contents 4 0 R>>".into(),
        stream("/OC /A BDC 1 0 0 rg 0 0 50 100 re f EMC /OC /B BDC 0 0 1 rg 50 0 50 100 re f EMC"),
        "<</Type/OCG/Name(Layer)>>".into(),
        "<</Type/OCG/Name(Layer)>>".into(),
    ];
    check("ocg", &classic(&objs), &Preset::Lossless.options());
}

/// A fuzzer find: a page found only by scanning (the catalog has no page tree) was
/// dropped as unreachable, leaving a file without pages.
#[test]
fn pages_found_by_scanning_are_kept() {
    // The page's resources come from a parent node no catalog leads to.
    let objs = [
        "<</Type/Catalog>>".to_owned(),
        "<</Type/Page/Parent 4 0 R/MediaBox[0 0 100 100]/Contents 3 0 R>>".into(),
        stream("1 0 0 rg 10 10 50 50 re f /F Do"),
        "<</Type/Pages/Count 1/Kids[2 0 R]/Resources<</XObject<</F 5 0 R>>>>>>".into(),
        "<</Type/XObject/Subtype/Form/BBox[0 0 100 100]/Length 22>>stream\n0 0 1 rg 60 60 30 30 re f\nendstream".into(),
    ];
    check("orphan page", &classic(&objs), &Preset::Lossless.options());
}

/// A fuzzer find: two pages whose damaged dictionaries are equal and have neither `/Type`
/// nor `/Parent` were merged into one object listed twice, which is one page.
#[test]
fn equal_pages_without_type_stay_apart() {
    let objs = [
        "<</Type/Catalog/Pages 2 0 R>>".to_owned(),
        "<</Type/Pages/Kids[3 0 R 4 0 R]/Count 2/MediaBox[0 0 100 100]>>".into(),
        "<</Contents 5 0 R>>".into(),
        "<</Contents 5 0 R>>".into(),
        stream("1 0 0 rg 10 10 50 50 re f"),
    ];
    let bytes = classic(&objs);
    let pages = |b: &[u8]| pagebara_pages::PageTree::load(&open(b).unwrap()).len();
    assert_eq!(pages(&bytes), 2);
    let out = optimize(&open(&bytes).unwrap(), &Preset::Lossless.options(), None).unwrap();
    assert_eq!(pages(&out.bytes), 2);
    check("equal pages", &bytes, &Preset::Lossless.options());
}

/// A fuzzer find: the root of a damaged page tree carried `/Type /ObjStm`, so the writer
/// took it for an object stream and left it out. The pages were then found by scanning,
/// which finds other ones: here it misses the page that has no `/Type`.
#[test]
fn a_page_tree_root_typed_as_an_object_stream_is_kept() {
    let objs = [
        "<</Type/Catalog/Pages 2 0 R>>".to_owned(),
        "<</Type/ObjStm/Kids[3 0 R 4 0 R]/Count 2/MediaBox[0 0 100 100]/Length 0>>stream\n\nendstream".into(),
        "<</Type/Page/Parent 2 0 R/Contents 5 0 R>>".into(),
        "<</Parent 2 0 R/Contents 5 0 R>>".into(),
        stream("1 0 0 rg 10 10 50 50 re f"),
    ];
    let bytes = classic(&objs);
    let pages = |b: &[u8]| pagebara_pages::PageTree::load(&open(b).unwrap()).len();
    assert_eq!(pages(&bytes), 2);
    for preset in [Preset::Lossless, Preset::Small] {
        let out = optimize(&open(&bytes).unwrap(), &preset.options(), None).unwrap();
        assert_eq!(pages(&out.bytes), 2, "{preset:?}");
    }
    check("object stream root", &bytes, &Preset::Lossless.options());
}

/// A fuzzer find: a catalog numbered 0 whose page tree is missing. The catalog was moved
/// to the number its `/Pages` names, became its own page tree, and the output had a page.
#[test]
fn a_catalog_numbered_zero_does_not_become_its_own_page_tree() {
    let bytes = b"%PDF-1.7\n1 0 obj\n<</Kind/Other>>\nendobj\n0 0 obj\n<</Type/Catalog/Pages 2 0 R>>\nendobj\n";
    let pages = |b: &[u8]| pagebara_pages::PageTree::load(&open(b).unwrap()).len();
    assert_eq!(pages(bytes), 0);
    for preset in [Preset::Lossless, Preset::Small] {
        let out = optimize(&open(bytes).unwrap(), &preset.options(), None).unwrap();
        assert_eq!(pages(&out.bytes), 0, "{preset:?}");
    }
}
