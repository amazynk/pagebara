//! Comparing generated pairs of documents, and a large book with edits.

use std::process::Command;
use std::time::Instant;

use pagebara_compare::{ChangeKind, Options, ReportOptions};
use pagebara_syntax::Document;

/// A document of US Letter pages, each a list of lines of 12 pt Helvetica; `extra`
/// operators are added to the page with the same index.
fn doc(pages: &[&[&str]], extra: &[(usize, &str)], title: &str) -> Document {
    let mut objs: Vec<Vec<u8>> = vec![Vec::new(), Vec::new()];
    let mut kids = Vec::new();
    for (i, lines) in pages.iter().enumerate() {
        let mut c = String::from("BT /F1 12 Tf 72 720 Td 16 TL\n");
        for l in *lines {
            let esc = l
                .replace('\\', "\\\\")
                .replace('(', "\\(")
                .replace(')', "\\)");
            c.push_str(&format!("({esc}) Tj T*\n"));
        }
        c.push_str("ET\n");
        for (p, e) in extra {
            if *p == i {
                c.push_str(e);
                c.push('\n');
            }
        }
        objs.push(
            [
                format!("<</Length {}>>stream\n", c.len()).as_bytes(),
                c.as_bytes(),
                b"\nendstream",
            ]
            .concat(),
        );
        let cn = objs.len();
        objs.push(format!(
            "<</Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources <</Font <</F1 <</Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding>>>>>> /Contents {cn} 0 R>>"
        ).into_bytes());
        kids.push(objs.len());
    }
    objs[0] = b"<</Type /Catalog /Pages 2 0 R>>".to_vec();
    let k: String = kids.iter().map(|k| format!("{k} 0 R ")).collect();
    objs[1] = format!("<</Type /Pages /Kids [{k}] /Count {}>>", kids.len()).into_bytes();
    objs.push(format!("<</Title ({title})>>").into_bytes());
    let info = objs.len();
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n", i + 1).bytes());
        out.extend(o);
        out.extend(b"\nendobj\n");
    }
    let x = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objs.len() + 1).bytes());
    for o in offs {
        out.extend(format!("{o:010} 00000 n \n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<</Size {} /Root 1 0 R /Info {info} 0 R>>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    Document::open(out).unwrap()
}

const P1: &[&str] = &[
    "The quick brown fox jumps over the lazy dog.",
    "Payment is due within thirty days of the invoice date.",
    "This sentence will move to the last page of the document.",
    "Late payments incur a fee of five percent per month.",
];
const P2: &[&str] = &[
    "Section two describes the delivery of the goods.",
    "Goods are delivered to the address named in the order.",
    "Risk passes to the buyer on delivery.",
];
const P3: &[&str] = &[
    "Section three is about warranties and their limits.",
    "The seller warrants the goods for twelve months.",
];
const P4: &[&str] = &[
    "Section four covers termination of this agreement.",
    "Either party may terminate with notice in writing.",
];
const NEW: &[&str] = &[
    "A brand new page with an annex about data protection.",
    "Personal data is processed only to fulfil the order.",
];

fn old() -> Document {
    doc(&[P1, P2, P3, P4], &[], "Agreement")
}

fn new() -> Document {
    let p1: &[&str] = &[
        "The quick brown fox jumps over the lazy dog.",
        "Payment is due within fourteen days of the invoice date.",
        "Late payments incur a fee of five percent per month.",
    ];
    let p4: &[&str] = &[
        "Section four covers termination of this agreement.",
        "Either party may terminate with notice in writing.",
        "This sentence will move to the last page of the document.",
        "Notices must be sent by registered mail.",
    ];
    // Page 3 deleted, a new page in its place after page 2, page 2 with a box drawn.
    doc(
        &[p1, P2, NEW, p4],
        &[(1, "0 0 1 rg 400 100 120 80 re f")],
        "Agreement (revised)",
    )
}

#[test]
fn finds_words_pages_moves_and_pictures() {
    let (a, b) = (old(), new());
    let t = Instant::now();
    let c = pagebara_compare::compare(&a, &b, &Options::default(), &|_, _, _| {}, None);
    eprintln!("compared in {:?}: {:#?}", t.elapsed(), c.summary);
    // Pages: 1↔1, 2↔2, old 3 deleted, new 3 inserted, 4↔4.
    assert!(c.pairs.contains(&(Some(0), Some(0))));
    assert!(c.pairs.contains(&(Some(1), Some(1))));
    assert!(c.pairs.contains(&(Some(2), None)));
    assert!(c.pairs.contains(&(None, Some(2))));
    assert!(c.pairs.contains(&(Some(3), Some(3))));
    let of = |k: ChangeKind| c.changes.iter().filter(move |x| x.kind == k);
    let replaced: Vec<_> = of(ChangeKind::Replaced).collect();
    assert_eq!(replaced.len(), 1, "{:#?}", c.changes);
    assert_eq!(replaced[0].old, "thirty");
    assert_eq!(replaced[0].new, "fourteen");
    assert_eq!((replaced[0].page_a, replaced[0].page_b), (Some(0), Some(0)));
    assert_eq!(replaced[0].rects_a.len(), 1);
    // In points from the top left: the second line.
    let r = replaced[0].rects_b[0].1;
    assert!(r[1] > 70.0 && r[3] < 100.0, "{r:?}");
    let moved: Vec<_> = of(ChangeKind::Moved).collect();
    assert_eq!(moved.len(), 1, "{:#?}", c.changes);
    assert_eq!((moved[0].page_a, moved[0].page_b), (Some(0), Some(3)));
    let inserted: Vec<_> = of(ChangeKind::Inserted).collect();
    assert_eq!(inserted.len(), 1, "{:#?}", c.changes);
    assert!(inserted[0].new.contains("registered mail"));
    assert_eq!(of(ChangeKind::PageDeleted).count(), 1);
    assert_eq!(of(ChangeKind::PageInserted).count(), 1);
    // Text on deleted and inserted pages is not listed word by word.
    assert_eq!(of(ChangeKind::Deleted).count(), 0, "{:#?}", c.changes);
    // The blue box on page 2.
    let vis: Vec<_> = of(ChangeKind::Visual).collect();
    assert_eq!(vis.len(), 1, "{:#?}", c.changes);
    assert_eq!(vis[0].page_b, Some(1));
    let b0 = vis[0].rects_b[0].1;
    assert!(b0[0] > 380.0 && b0[2] < 540.0 && b0[1] > 590.0, "{b0:?}");
    // Page 1 and 4 changed text: their text changes explain the pixels.
    assert!(of(ChangeKind::Visual).all(|v| v.page_b == Some(1)));
    assert_eq!(of(ChangeKind::Metadata).count(), 1);
    assert_eq!(c.summary.words_moved, 11);
    let json = serde_json::to_string(&c).unwrap();
    assert!(json.contains("\"kind\":\"replaced\""));
}

#[test]
fn identical_documents() {
    let a = old();
    let c = pagebara_compare::compare(&a, &a.clone(), &Options::default(), &|_, _, _| {}, None);
    assert!(c.changes.is_empty(), "{:#?}", c.changes);
    // Same objects: nothing rendered.
    assert_eq!(c.timing.pages_rendered, 0);
    assert_eq!(c.pairs.len(), 4);
}

#[test]
fn annotations_forms_and_report() {
    let a = Document::open(pagebara_samples::form()).unwrap();
    let mut b = a.clone();
    let mut form = pagebara_forms::Form::load(&b);
    form.set_text(&mut b, "name", "Grace Hopper").unwrap();
    let page = pagebara_pages::PageTree::load(&b).page(0).unwrap().r;
    let mut note = pagebara_annot::Annotation::new(
        pagebara_annot::Kind::Text {
            icon: "Comment".into(),
            open: false,
        },
        pagebara_annot::Rect::new(500.0, 700.0, 520.0, 720.0),
    );
    note.contents = Some("Check this".into());
    pagebara_annot::add(&mut b, page, &note).unwrap();
    let b = Document::open(b.save_full().unwrap()).unwrap();
    let c = pagebara_compare::compare(&a, &b, &Options::default(), &|_, _, _| {}, None);
    let forms: Vec<_> = c
        .changes
        .iter()
        .filter(|x| x.kind == ChangeKind::Form)
        .collect();
    assert_eq!(forms.len(), 1, "{:#?}", c.changes);
    assert_eq!(forms[0].new, "Grace Hopper");
    let notes: Vec<_> = c
        .changes
        .iter()
        .filter(|x| x.kind == ChangeKind::Annotation)
        .collect();
    assert_eq!(notes.len(), 1, "{:#?}", c.changes);
    assert!(notes[0].description.contains("Check this"));
    let r = notes[0].rects_b[0].1;
    assert!(
        (r[0] - 500.0).abs() < 1.0 && (r[1] - 122.0).abs() < 1.0,
        "{r:?}"
    );

    let pdf = pagebara_compare::report(
        &old(),
        &new(),
        &pagebara_compare::compare(&old(), &new(), &Options::default(), &|_, _, _| {}, None),
        &ReportOptions {
            old_name: "old.pdf".into(),
            new_name: "new.pdf".into(),
            only_changed: true,
        },
    )
    .unwrap();
    let rep = Document::open(pdf.clone()).unwrap();
    let n = pagebara_pages::PageTree::load(&rep).len();
    // A summary page and the five pairs (all have changes).
    assert_eq!(n, 6);
    let dir = std::env::temp_dir().join("pagebara-compare-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("report.pdf");
    std::fs::write(&path, &pdf).unwrap();
    if let Ok(out) = Command::new("qpdf").arg("--check").arg(&path).output() {
        let t = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success() && !t.contains("WARNING"), "{t}");
    }
    if let Ok(out) = Command::new("pdftotext").arg(&path).arg("-").output() {
        let t = String::from_utf8_lossy(&out.stdout);
        assert!(t.contains("Comparison") && t.contains("fourteen"), "{t}");
    }
}

fn corpus(name: &str) -> Option<Vec<u8>> {
    let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../../../corpus")
        .join(name);
    let alt = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../corpus")
        .join(name);
    std::fs::read(p).or_else(|_| std::fs::read(alt)).ok()
}

/// A large book against a copy with a page deleted, a word changed and a page added:
/// run with `--ignored` (needs the corpus) for the timings.
#[test]
#[ignore]
fn corpus_book_with_edits() {
    let Some(bytes) = corpus("325462-093-sdm-vol-1-2abcd-3abcd-4.pdf") else {
        eprintln!("corpus missing; skipped");
        return;
    };
    let a = Document::open(bytes).unwrap();
    let n = pagebara_pages::PageTree::load(&a).len();
    let mut b = a.clone();
    pagebara_pages::delete(&mut b, &[100]).unwrap();
    // A word changed on page 500 (of the new document).
    let objs = pagebara_edit::parse(&b, 500).unwrap();
    let run = objs
        .runs
        .iter()
        .find(|r| r.text.split_whitespace().count() >= 3)
        .unwrap();
    let old_text = run.text.clone();
    let new_text = format!("{old_text} pagebarainserted");
    pagebara_edit::replace_text(
        &mut b,
        500,
        pagebara_edit::Target::Run(run.id),
        &new_text,
        &pagebara_edit::SimpleEncoder,
    )
    .unwrap();
    let blank = pagebara_pages::blank((1, 7));
    let _ = blank;
    let b = Document::open(b.save_full().unwrap()).unwrap();
    let t = Instant::now();
    let c = pagebara_compare::compare(&a, &b, &Options::default(), &|_, _, _| {}, None);
    eprintln!(
        "{n} pages, {} + {} words: {:?} total; timing {:?}; summary {:?}",
        c.words_a,
        c.words_b,
        t.elapsed(),
        c.timing,
        c.summary
    );
    for x in &c.changes {
        eprintln!(
            "{:?} {:?}/{:?}: {}",
            x.kind, x.page_a, x.page_b, x.description
        );
    }
    assert!(c.pairs.contains(&(Some(100), None)));
    assert!(
        c.changes
            .iter()
            .any(|x| x.new.contains("pagebarainserted") && x.page_b == Some(500)),
        "{:#?}",
        &c.changes[..c.changes.len().min(20)]
    );
}
