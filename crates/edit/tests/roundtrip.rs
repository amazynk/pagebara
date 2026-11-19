//! Parsing and writing back without changes leaves pages as they were, over the sample
//! documents and generated ones.

mod common;

use common::*;
use pagebara_content::Matrix;
use pagebara_edit::{Kind, Target};
use pagebara_syntax::Document;

fn corpus() -> Vec<(String, Vec<u8>)> {
    static CORPUS: std::sync::OnceLock<Vec<(String, Vec<u8>)>> = std::sync::OnceLock::new();
    CORPUS.get_or_init(load).clone()
}

fn load() -> Vec<(String, Vec<u8>)> {
    let dir = std::env::temp_dir().join(format!("pagebara-edit-samples-{}", std::process::id()));
    pagebara_samples::write_all(&dir).unwrap();
    let mut v: Vec<(String, Vec<u8>)> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| {
            let p = e.ok()?.path();
            let name = p.file_name()?.to_str()?.to_owned();
            name.ends_with(".pdf")
                .then(|| (name, std::fs::read(&p).unwrap()))
        })
        .collect();
    v.sort();
    let _ = std::fs::remove_dir_all(&dir);
    v.extend(docs::all().into_iter().map(|(n, b)| (n.to_string(), b)));
    v
}

fn open_any(bytes: &[u8]) -> Option<Document> {
    match Document::open(bytes.to_vec()) {
        Ok(d) => Some(d),
        Err(pagebara_syntax::Error::PasswordRequired(l)) => l.unlock("").ok(),
        Err(_) => None,
    }
}

fn pages(doc: &Document) -> usize {
    pagebara_pages::PageTree::load(doc).len()
}

#[test]
fn rewriting_keeps_streams_and_pixels() {
    for (name, bytes) in corpus() {
        let Some(doc) = open_any(&bytes) else {
            continue;
        };
        for page in 0..pages(&doc).min(4) {
            let mut d = doc.clone();
            pagebara_edit::rewrite_all(&mut d, page).unwrap();
            assert_eq!(
                page_streams(&doc, page),
                page_streams(&d, page),
                "{name} page {page}: streams changed"
            );
            let (d, _) = reopen(&d);
            assert_eq!(
                diff(&render(&doc, page), &render(&d, page), 0),
                0,
                "{name} page {page}: pixels changed"
            );
        }
    }
}

#[test]
fn committing_nothing_changes_nothing() {
    for (name, bytes) in corpus() {
        let Some(doc) = open_any(&bytes) else {
            continue;
        };
        if !doc.permissions().modify {
            continue;
        }
        for page in 0..pages(&doc).min(4) {
            let mut d = doc.clone();
            pagebara_edit::Editor::open(&mut d, page)
                .unwrap()
                .commit()
                .unwrap();
            assert!(!d.is_modified() || doc.is_modified(), "{name} {page}");
        }
    }
}

/// An identity transform rewrites every object (wrapped, or with its own text matrix and
/// a position fix-up after it) and must draw the same page.
#[test]
fn identity_edits_draw_the_same() {
    for (name, bytes) in corpus() {
        let Some(doc) = open_any(&bytes) else {
            continue;
        };
        if doc.is_encrypted() || !doc.permissions().modify {
            continue;
        }
        for page in 0..pages(&doc).min(3) {
            let objs = pagebara_edit::parse(&doc, page).unwrap();
            let targets: Vec<Target> = objs.objects.iter().map(|o| Target::Object(o.id)).collect();
            let mut d = doc.clone();
            pagebara_edit::transform(&mut d, page, &targets, Matrix::IDENTITY).unwrap();
            let (d, file) = reopen(&d);
            let n = diff(&render(&doc, page), &render(&d, page), 48);
            assert!(n < 40, "{name} page {page}: {n} pixels differ");
            external_checks(&file, &[]);
        }
    }
}

/// The objects' runs hold the page's text as extraction sees it, and every visible
/// drawing operator is some object.
#[test]
fn model_matches_extraction() {
    for (name, bytes) in corpus() {
        let Some(doc) = open_any(&bytes) else {
            continue;
        };
        for page in 0..pages(&doc).min(4) {
            let objs = pagebara_edit::parse(&doc, page).unwrap();
            let mut ours: Vec<char> = objs
                .runs
                .iter()
                .filter(|r| !r.hidden)
                .flat_map(|r| r.text.chars())
                .filter(|c| !c.is_whitespace())
                .collect();
            let mut theirs: Vec<char> = page_text(&doc, page)
                .chars()
                .filter(|c| !c.is_whitespace())
                .collect();
            ours.sort_unstable();
            theirs.sort_unstable();
            assert_eq!(
                ours.iter().collect::<String>(),
                theirs.iter().collect::<String>(),
                "{name} page {page}"
            );
            for o in &objs.objects {
                assert!(
                    o.bbox.x0 <= o.bbox.x1 && o.bbox.y0 <= o.bbox.y1,
                    "{name} {o:?}"
                );
                if o.kind == Kind::Text {
                    assert!(!o.text().unwrap().lines.is_empty());
                }
            }
        }
    }
}

#[test]
fn generated_objects() {
    let doc = open(docs::basic());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let kinds: Vec<Kind> = objs.objects.iter().map(|o| o.kind).collect();
    assert_eq!(
        kinds,
        [Kind::Path, Kind::Path, Kind::Text, Kind::Text, Kind::Image],
        "{:#?}",
        objs.objects
    );
    let t = objs.objects[2].text().unwrap();
    assert_eq!(t.text, "Hello World");
    let t = objs.objects[3].text().unwrap();
    assert_eq!(t.lines.len(), 2);
    assert_eq!(t.text, "Second line\nThird line here");
    let img = objs.objects[4].image().unwrap();
    assert_eq!((img.width, img.height), (2, 2));
    let b = objs.objects[4].bbox;
    assert!((b.x0 - 220.0).abs() < 1e-3 && (b.y1 - 60.0).abs() < 1e-3);

    let doc = open(docs::shared_form());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let forms: Vec<_> = objs
        .objects
        .iter()
        .filter(|o| o.kind == Kind::Form)
        .collect();
    assert_eq!(forms.len(), 2);
    let pagebara_edit::ObjectData::Form(f) = &forms[0].data else {
        unreachable!()
    };
    assert_eq!(f.uses_on_page, 2);
    assert_eq!(f.children.len(), 2);
    assert!(objs.objects.iter().any(|o| o.parent == Some(forms[1].id)));

    let doc = open(docs::marked());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let tagged = &objs.objects[0];
    assert_eq!(tagged.marked[0].mcid, Some(0));
    let oc = objs.objects.iter().find(|o| o.kind == Kind::Path).unwrap();
    assert!(oc.marked[0].optional_content.is_some());
    assert!(!oc.hidden);
    // Both lines are one block; the second reads as its actual text.
    assert_eq!(tagged.text().unwrap().text, "Tagged text\nfi");

    let doc = open(docs::inline_and_clip());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let img = objs.objects.iter().find(|o| o.kind == Kind::Image).unwrap();
    assert!(img.image().unwrap().name.is_none());
    let clipped = objs.objects.iter().filter(|o| o.clip.is_some()).count();
    assert!(clipped >= 2);
}
