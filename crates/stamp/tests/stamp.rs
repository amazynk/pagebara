//! Watermarks, headers and footers, Bates numbers: rendered, extracted, removed, checked.

use std::process::Command;

use pagebara_content::Interpreter;
use pagebara_render::{Annotations, OptionalContentConfig, PixelRect, RenderRequest, Renderer};
use pagebara_stamp::{
    Anchor, BatesNumber, Clock, DocInfo, HeaderFooter, Kind, Layer, PageSet, Preset, Rgb, Size,
    Slots, Source, TextStyle, Tiling, Visibility, Watermark,
};
use pagebara_syntax::{Document, ObjRef};

type Image = (u32, u32, Vec<u8>);

fn render_with(doc: &Document, page: usize, oc: Option<&OptionalContentConfig>) -> Image {
    let r = Renderer::new(doc.clone());
    let (w, h) = r.page_size(page).unwrap();
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: w.round() as u32,
        h: h.round() as u32,
    };
    let mut req = RenderRequest::new(1.0, rect);
    req.annotations = Annotations::None;
    req.optional_content = oc;
    let img = r.render(page, &req).unwrap();
    (img.width, img.height, img.data)
}

fn render(doc: &Document, page: usize) -> Image {
    render_with(doc, page, None)
}

/// Pixels differing by more than `tol` in some channel, inside a box (fractions of the
/// image: x0, y0, x1, y1 from the top left).
fn diff_in(a: &Image, b: &Image, area: [f32; 4], tol: u8) -> usize {
    assert_eq!((a.0, a.1), (b.0, b.1));
    let (w, h) = (a.0 as f32, a.1 as f32);
    let (x0, y0) = ((area[0] * w) as u32, (area[1] * h) as u32);
    let (x1, y1) = ((area[2] * w) as u32, (area[3] * h) as u32);
    let mut n = 0;
    for y in y0..y1.min(a.1) {
        for x in x0..x1.min(a.0) {
            let i = ((y * a.0 + x) * 4) as usize;
            if (0..3).any(|c| a.2[i + c].abs_diff(b.2[i + c]) > tol) {
                n += 1;
            }
        }
    }
    n
}

const ALL: [f32; 4] = [0.0, 0.0, 1.0, 1.0];

fn reopen(doc: &Document) -> Document {
    Document::open(doc.save_full().unwrap()).unwrap()
}

fn page_text(doc: &Document, page: usize) -> String {
    let tree = pagebara_pages::PageTree::load(doc);
    let p = tree.page(page).unwrap();
    let d = doc.get(p.r).unwrap();
    let d = d.as_dict().unwrap();
    let res = doc
        .get_in(d, b"Resources")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let list = Interpreter::new(doc).page(d, &res);
    pagebara_content::extract_text(doc, &list).text
}

/// `qpdf --check` passes, when qpdf is installed.
fn qpdf_ok(doc: &Document, name: &str) {
    let dir = std::env::temp_dir().join("pagebara-stamp-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, doc.save_full().unwrap()).unwrap();
    let Ok(out) = Command::new("qpdf").arg("--check").arg(&path).output() else {
        return;
    };
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "qpdf --check {name}: {text}");
    assert!(!text.contains("WARNING"), "qpdf --check {name}: {text}");
}

fn text_wm(text: &str) -> Watermark {
    Watermark {
        source: Source::Text {
            text: text.into(),
            style: TextStyle {
                font: "Helvetica-Bold".into(),
                size: 60.0,
                color: Rgb([0.9, 0.0, 0.0]),
                background: None,
            },
        },
        opacity: 0.5,
        ..Watermark::default()
    }
}

fn our_group(doc: &Document) -> Option<ObjRef> {
    let cat = doc.catalog()?;
    let props = doc.get_in(cat.as_dict()?, b"OCProperties");
    let ocgs = doc.get_in(props.as_dict()?, b"OCGs");
    ocgs.as_array()?.first()?.as_objref()
}

#[test]
fn text_watermark_shows_hides_and_goes() {
    let mut doc = Document::open(pagebara_samples::lease()).unwrap();
    let before = render(&doc, 0);
    let report = pagebara_stamp::apply_watermark(&mut doc, &text_wm("DRAFT")).unwrap();
    assert_eq!(
        report.pages.len(),
        pagebara_pages::PageTree::load(&doc).len()
    );
    assert!(report.missing.is_empty());
    let doc = reopen(&doc);
    let after = render(&doc, 0);
    // Red, in the middle of the page.
    assert!(diff_in(&before, &after, [0.3, 0.3, 0.7, 0.7], 30) > 500);
    let reddish = after
        .2
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] > 200 && p[1] < 190 && p[2] < 190)
        .count();
    assert!(reddish > 500, "{reddish} red pixels");
    // The layer hides it.
    let g = our_group(&doc).expect("a Watermark layer");
    let off = OptionalContentConfig {
        on: vec![],
        off: vec![g],
    };
    let hidden = render_with(&doc, 0, Some(&off));
    assert_eq!(diff_in(&before, &hidden, ALL, 2), 0);
    qpdf_ok(&doc, "watermark.pdf");
    // Found as ours, and removed.
    let found = pagebara_stamp::find(&doc, false);
    assert!(
        found
            .iter()
            .all(|f| f.kind == Kind::Watermark && !f.foreign)
    );
    assert_eq!(found.len(), report.pages.len());
    let mut doc = doc;
    let r = pagebara_stamp::remove(&mut doc, &[Kind::Watermark], false).unwrap();
    assert_eq!(r.removed, report.pages.len());
    let doc = reopen(&doc);
    assert_eq!(diff_in(&before, &render(&doc, 0), ALL, 2), 0);
    assert!(
        our_group(&doc).is_none(),
        "the layer goes with the last watermark"
    );
    qpdf_ok(&doc, "watermark-removed.pdf");
}

#[test]
fn behind_tiled_and_settings_kept() {
    let mut doc = Document::open(pagebara_samples::lease()).unwrap();
    let before = render(&doc, 0);
    let wm = Watermark {
        size: Size::Scale(0.4),
        tiling: Some(Tiling { gap: [30.0, 30.0] }),
        layer: Layer::Behind,
        pages: PageSet::list("1"),
        group: None,
        ..text_wm("COPY")
    };
    pagebara_stamp::apply_watermark(&mut doc, &wm).unwrap();
    let doc = reopen(&doc);
    let after = render(&doc, 0);
    // Copies all over the page, top and bottom.
    assert!(diff_in(&before, &after, [0.0, 0.0, 1.0, 0.25], 20) > 200);
    assert!(diff_in(&before, &after, [0.0, 0.75, 1.0, 1.0], 20) > 200);
    // Behind: the text is still black where it is drawn.
    let black_before = before
        .2
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] < 60)
        .count();
    let black_after = after
        .2
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[0] < 60)
        .count();
    assert!(black_after * 10 >= black_before * 9);
    // Only page 1, no layer.
    assert_eq!(pagebara_stamp::find(&doc, false).len(), 1);
    assert!(our_group(&doc).is_none());
    let json = pagebara_stamp::existing(&doc, Kind::Watermark).unwrap();
    let back: Watermark = serde_json::from_str(&json).unwrap();
    assert_eq!(back, wm);
}

#[test]
fn print_only_and_replace() {
    let mut doc = Document::open(pagebara_samples::lease()).unwrap();
    let before = render(&doc, 0);
    let wm = Watermark {
        visibility: Visibility::PrintOnly,
        ..text_wm("PRINTED")
    };
    pagebara_stamp::apply_watermark(&mut doc, &wm).unwrap();
    // Off in the default (viewing) configuration.
    let doc2 = reopen(&doc);
    assert_eq!(diff_in(&before, &render(&doc2, 0), ALL, 2), 0);
    let g = our_group(&doc2).unwrap();
    let on = OptionalContentConfig {
        on: vec![g],
        off: vec![],
    };
    assert!(diff_in(&before, &render_with(&doc2, 0, Some(&on)), ALL, 30) > 500);
    // Replacing leaves one watermark per page.
    let again = Watermark {
        replace: true,
        visibility: Visibility::Always,
        ..text_wm("FINAL")
    };
    let r = pagebara_stamp::apply_watermark(&mut doc, &again).unwrap();
    assert!(r.replaced > 0);
    let doc = reopen(&doc);
    assert_eq!(pagebara_stamp::find(&doc, false).len(), r.pages.len());
    qpdf_ok(&doc, "replaced.pdf");
}

/// Printing (our own print path renders with `OptionalContentConfig::for_print`) shows
/// print-only watermarks and leaves out view-only ones.
#[test]
fn print_layers_follow_usage() {
    let base = Document::open(pagebara_samples::lease()).unwrap();
    let before = render(&base, 0);
    for (vis, prints) in [(Visibility::PrintOnly, true), (Visibility::ViewOnly, false)] {
        let mut doc = base.clone();
        let wm = Watermark {
            visibility: vis,
            ..text_wm("LAYER")
        };
        pagebara_stamp::apply_watermark(&mut doc, &wm).unwrap();
        let doc = reopen(&doc);
        let print = OptionalContentConfig::for_print(&doc);
        let changed = diff_in(&before, &render_with(&doc, 0, Some(&print)), ALL, 30) > 500;
        assert_eq!(changed, prints, "{vis:?}");
        let on_screen = diff_in(&before, &render(&doc, 0), ALL, 30) > 500;
        assert_eq!(on_screen, !prints, "{vis:?} on screen");
    }
}

#[test]
fn watermark_is_an_artifact_for_reading_order() {
    let mut doc = Document::open(pagebara_samples::tagged()).unwrap();
    pagebara_stamp::apply_watermark(&mut doc, &text_wm("SECRETMARK")).unwrap();
    let doc = reopen(&doc);
    // Plain extraction sees it (it is real text), the structure's reading order does not.
    assert!(page_text(&doc, 0).contains("SECRETMARK"));
    let tree = pagebara_tagged::StructTree::load(&doc).expect("tagged");
    let p = pagebara_pages::PageTree::load(&doc).page(0).unwrap();
    let d = doc.get(p.r).unwrap();
    let d = d.as_dict().unwrap();
    let res = doc
        .get_in(d, b"Resources")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    let list = Interpreter::new(&doc).page(d, &res);
    let c = tree.page_content(&doc, p.r, &list);
    assert!(!c.blocks.is_empty());
    assert!(c.blocks.iter().all(|b| !b.text.contains("SECRETMARK")));
    let raw = pagebara_content::page_contents(&doc, d);
    let raw = String::from_utf8_lossy(&raw);
    assert!(raw.contains("/Artifact <</Type /Pagination /Subtype /Watermark>> BDC"));
}

#[test]
fn image_and_pdf_page_watermarks() {
    let mut doc = Document::open(pagebara_samples::lease()).unwrap();
    let before = render(&doc, 0);
    let img = Watermark {
        source: Source::Image {
            path: Default::default(),
            data: pagebara_samples::picture_png(),
        },
        size: Size::Fit(0.3),
        anchor: Anchor::TopRight,
        rotation: 0.0,
        opacity: 1.0,
        pages: PageSet::list("1"),
        ..Watermark::default()
    };
    pagebara_stamp::apply_watermark(&mut doc, &img).unwrap();
    let after = render(&reopen(&doc), 0);
    assert!(diff_in(&before, &after, [0.6, 0.0, 1.0, 0.35], 30) > 1000);
    assert_eq!(diff_in(&before, &after, [0.0, 0.5, 1.0, 1.0], 2), 0);

    let mut doc = Document::open(pagebara_samples::lease()).unwrap();
    let page = Watermark {
        source: Source::Pdf {
            path: Default::default(),
            page: 1,
            data: pagebara_samples::rotated(),
        },
        size: Size::Fit(0.5),
        rotation: 0.0,
        opacity: 1.0,
        anchor: Anchor::BottomLeft,
        ..Watermark::default()
    };
    pagebara_stamp::apply_watermark(&mut doc, &page).unwrap();
    let doc = reopen(&doc);
    let after = render(&doc, 0);
    assert!(diff_in(&before, &after, [0.0, 0.6, 0.5, 1.0], 30) > 50);
    // Vector: the stamped page's text is text.
    assert!(page_text(&doc, 0).contains("Rotated memo"));
    qpdf_ok(&doc, "pdf-watermark.pdf");
}

#[test]
fn foreign_watermarks_found_and_removed() {
    // A watermark as other tools write it: an artifact, and a layer named "Watermark".
    let base = Document::open(pagebara_samples::lease()).unwrap();
    let before = render(&base, 0);
    let mut doc = base.clone();
    let tree = pagebara_pages::PageTree::load(&doc);
    let page = tree.page(0).unwrap().r;
    let mut d = doc.get(page).unwrap().as_dict().unwrap().clone();
    let mut contents = match d.get(b"Contents").unwrap() {
        pagebara_syntax::Object::Array(a) => a.clone(),
        o => vec![o.clone()],
    };
    let s = doc.add(pagebara_syntax::Stream::new(
        Default::default(),
        &b"/Artifact <</Subtype /Watermark /Type /Pagination>> BDC q 1 0 0 rg 100 300 200 200 re f Q EMC\n"[..],
    ));
    contents.push(s.into());
    d.set(b"Contents", contents);
    doc.set(page, d);
    let found = pagebara_stamp::find(&doc, true);
    assert_eq!(found.len(), 1);
    assert!(found[0].foreign && found[0].kind == Kind::Watermark);
    assert_eq!(pagebara_stamp::find(&doc, false).len(), 0);
    let r = pagebara_stamp::remove(&mut doc, &[Kind::Watermark], true).unwrap();
    assert_eq!(r.removed, 1);
    assert_eq!(diff_in(&before, &render(&reopen(&doc), 0), ALL, 2), 0);
}

fn clock() -> Clock {
    Clock {
        secs: 1_709_211_909,
        offset_minutes: 0,
    }
}

#[test]
fn header_footer_tokens_on_rotated_page() {
    let mut doc = Document::open(pagebara_samples::rotated()).unwrap();
    let before = render(&doc, 0);
    let hf = HeaderFooter {
        slots: Slots([
            "{title}".into(),
            String::new(),
            "{date}".into(),
            String::new(),
            "Page {page} of {pages}".into(),
            "{file}".into(),
        ]),
        style: TextStyle {
            size: 12.0,
            ..TextStyle::default()
        },
        clock: Some(clock()),
        ..HeaderFooter::default()
    };
    let info = DocInfo::read(&doc, "memo.pdf");
    assert_eq!(info.title, "Rotated memo");
    let r = pagebara_stamp::apply_header_footer(&mut doc, &hf, &info).unwrap();
    assert_eq!(r.pages, vec![0]);
    let doc = reopen(&doc);
    let text = page_text(&doc, 0);
    for want in ["Rotated memo", "2024-02-29", "Page 1 of 1", "memo.pdf"] {
        assert!(text.contains(want), "{want:?} in {text:?}");
    }
    let after = render(&doc, 0);
    // Landscape as shown: header along the top, footer along the bottom of the shown page.
    assert!(after.0 > after.1);
    assert!(
        diff_in(&before, &after, [0.0, 0.0, 0.3, 0.1], 20) > 20,
        "top left"
    );
    assert!(
        diff_in(&before, &after, [0.7, 0.0, 1.0, 0.1], 20) > 20,
        "top right"
    );
    assert!(
        diff_in(&before, &after, [0.35, 0.9, 0.65, 1.0], 20) > 20,
        "bottom"
    );
    assert_eq!(diff_in(&before, &after, [0.0, 0.15, 1.0, 0.85], 2), 0);
    qpdf_ok(&doc, "rotated-hf.pdf");
}

#[test]
fn header_footer_cropped_first_even_and_replace() {
    let mut doc = Document::open(pagebara_samples::report()).unwrap();
    let n = pagebara_pages::PageTree::load(&doc).len();
    assert!(n >= 3);
    // Crop page 2: the footer follows the crop box.
    pagebara_pages::crop(
        &mut doc,
        &[1],
        Some(pagebara_pages::Rect::new(50.0, 100.0, 500.0, 700.0)),
    )
    .unwrap();
    let hf = HeaderFooter {
        slots: Slots::one(5, "odd {page}"),
        first: Some(Slots::one(4, "cover")),
        even: Some(Slots::one(3, "even {page}")),
        start: 10,
        clock: Some(clock()),
        ..HeaderFooter::default()
    };
    let info = DocInfo::default();
    pagebara_stamp::apply_header_footer(&mut doc, &hf, &info).unwrap();
    let doc = reopen(&doc);
    assert!(page_text(&doc, 0).contains("cover"));
    assert!(page_text(&doc, 1).contains("even 11"));
    assert!(page_text(&doc, 2).contains("odd 12"));
    // Inside the crop box of page 2.
    let img = render(&doc, 1);
    assert_eq!((img.0, img.1), (450, 600));
    let blank = Document::open(pagebara_samples::report()).unwrap();
    let mut cropped = blank.clone();
    pagebara_pages::crop(
        &mut cropped,
        &[1],
        Some(pagebara_pages::Rect::new(50.0, 100.0, 500.0, 700.0)),
    )
    .unwrap();
    let base = render(&cropped, 1);
    assert!(diff_in(&base, &img, [0.0, 0.9, 0.4, 1.0], 20) > 20);
    // Applying again replaces them.
    let mut doc = doc;
    let r = pagebara_stamp::apply_header_footer(&mut doc, &hf, &info).unwrap();
    assert_eq!(r.replaced, n);
    assert_eq!(pagebara_stamp::find(&doc, false).len(), n);
    let back: HeaderFooter =
        serde_json::from_str(&pagebara_stamp::existing(&doc, Kind::HeaderFooter).unwrap()).unwrap();
    assert_eq!(back, hf);
    pagebara_stamp::remove(&mut doc, &[Kind::HeaderFooter], false).unwrap();
    let doc = reopen(&doc);
    assert!(!page_text(&doc, 2).contains("odd"));
    assert_eq!(diff_in(&base, &render(&doc, 1), ALL, 2), 0);
}

#[test]
fn bates_continues_across_files() {
    let dir = std::env::temp_dir().join(format!("pagebara-bates-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut files = Vec::new();
    for (i, bytes) in [
        pagebara_samples::lease(),
        pagebara_samples::report(),
        pagebara_samples::rotated(),
    ]
    .into_iter()
    .enumerate()
    {
        let p = dir.join(format!("in{i}.pdf"));
        std::fs::write(&p, bytes).unwrap();
        files.push(p);
    }
    let counts: Vec<usize> = files
        .iter()
        .map(|f| {
            pagebara_pages::PageTree::load(&Document::open(std::fs::read(f).unwrap()).unwrap())
                .len()
        })
        .collect();
    let hf = HeaderFooter {
        slots: Slots::one(5, "{bates}"),
        bates: Some(BatesNumber {
            prefix: "ACME-".into(),
            suffix: String::new(),
            digits: 6,
            start: 100,
        }),
        style: TextStyle {
            background: Some(Rgb([1.0, 1.0, 0.6])),
            ..TextStyle::default()
        },
        ..HeaderFooter::default()
    };
    let out_dir = dir.clone();
    let done = pagebara_stamp::bates_batch(
        &files,
        &hf,
        None,
        &|i, _, data| {
            let p = out_dir.join(format!("out{i}.pdf"));
            std::fs::write(&p, data).map(|()| p)
        },
        &|_, _| {},
    )
    .unwrap();
    assert_eq!(done.len(), 3);
    let mut next = 100;
    for (f, n) in done.iter().zip(&counts) {
        assert_eq!(f.pages, *n);
        assert_eq!(f.first, format!("ACME-{next:06}"));
        assert_eq!(f.last, format!("ACME-{:06}", next + *n as u64 - 1));
        let doc = Document::open(std::fs::read(&f.output).unwrap()).unwrap();
        assert!(page_text(&doc, 0).contains(&f.first));
        assert!(page_text(&doc, n - 1).contains(&f.last));
        assert!(
            pagebara_stamp::find(&doc, false)
                .iter()
                .all(|s| s.kind == Kind::Bates)
        );
        qpdf_ok(&doc, &format!("bates-{next}.pdf"));
        next += *n as u64;
    }
    let csv = pagebara_stamp::bates_csv(&done);
    assert_eq!(csv.lines().count(), 4);
    assert!(csv.contains("ACME-000100"));
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn presets_round_trip() {
    let list = vec![
        Preset::Watermark {
            name: "Draft".into(),
            spec: Box::new(text_wm("DRAFT")),
        },
        Preset::HeaderFooter {
            name: "Pages".into(),
            spec: Box::default(),
        },
    ];
    let json = Preset::to_json(&list);
    assert_eq!(Preset::from_json(&json), list);
    assert!(Preset::from_json("nonsense").is_empty());
}

#[test]
fn unicode_text_uses_a_fallback_font() {
    let mut doc = Document::open(pagebara_samples::lease()).unwrap();
    let r = pagebara_stamp::apply_watermark(&mut doc, &text_wm("Entwurf Ωμέγα")).unwrap();
    let doc = reopen(&doc);
    let text = page_text(&doc, 0);
    if r.missing.is_empty() {
        assert!(text.contains("Ωμέγα"), "{text}");
    }
    assert!(text.contains("Entwurf"));
}
