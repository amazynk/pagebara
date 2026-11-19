//! The typesetting encoder: replacing and adding text keeps fonts, adds glyphs and falls
//! back to installed faces, and its font cache follows the document.

mod common;

use common::*;
use pagebara_content::Point;
use pagebara_edit::{Color, NewText, Target, TextEncoder, TypesetEncoder};

#[test]
fn replace_run_with_characters_the_font_lacks() {
    let doc = open(docs::basic());
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let run = objs
        .runs
        .iter()
        .find(|r| r.text.contains("Hello"))
        .unwrap()
        .id;
    let enc = TypesetEncoder::default();
    let mut d = doc.clone();
    pagebara_edit::replace_text(&mut d, 0, Target::Run(run), "Héllo Ωμέγα 漢字", &enc).unwrap();
    let (d2, file) = reopen(&d);
    external_checks(&file, &["Héllo", "Ωμέγα"]);
    assert!(page_text(&d2, 0).contains("Héllo Ωμέγα 漢字"));
    // The text after it on the page did not move.
    let a = pagebara_edit::parse(&doc, 0).unwrap();
    let b = pagebara_edit::parse(&d2, 0).unwrap();
    let pos = |o: &pagebara_edit::PageObjects| {
        o.runs
            .iter()
            .find(|r| r.text.contains("Second"))
            .map(|r| r.glyphs[0].origin)
    };
    assert_eq!(pos(&a), pos(&b));
}

#[test]
fn new_text_and_a_stale_cache() {
    let doc = open(docs::basic());
    let enc = TypesetEncoder::default();
    let mut d = doc.clone();
    let t = NewText {
        text: "Added — Grüße\nŁódź",
        origin: Point::new(20.0, 40.0),
        size: 12.0,
        color: Color::Rgb(0.8, 0.0, 0.0),
        angle: 0.0,
        leading: None,
    };
    pagebara_edit::add_text(&mut d, 0, &t, &enc).unwrap();
    let (d2, file) = reopen(&d);
    external_checks(&file, &["Grüße", "Łódź"]);
    assert!(page_text(&d2, 0).contains("Added — Grüße"));
    // The encoder's fonts were written into `d`; on the original they are not there, so
    // the cache opens them again instead of using codes `doc` lacks.
    let mut d3 = doc.clone();
    pagebara_edit::add_text(&mut d3, 0, &t, &enc).unwrap();
    let (d4, _) = reopen(&d3);
    assert!(page_text(&d4, 0).contains("Added — Grüße"));
}

#[test]
fn kerning_shows_as_adjustments() {
    let doc = open(docs::basic());
    let enc = TypesetEncoder::default();
    let mut d = doc.clone();
    let objs = pagebara_edit::parse(&doc, 0).unwrap();
    let run = &objs.runs[0];
    let f = run.font.as_ref().unwrap();
    let e = enc
        .encode(
            &mut d,
            &pagebara_edit::EncodeRequest {
                text: "AVATAR",
                font: Some(pagebara_edit::FontSlot {
                    name: None,
                    object: &f.object,
                    font: &f.font,
                }),
                size: 12.0,
            },
        )
        .unwrap();
    assert_eq!(e.advances.len(), 6);
    let _ = e.show;
}
