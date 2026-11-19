use pagebara_redact::{
    ApplyOptions, add_mark, apply, find, marks, read_mark, sanitize, save, verify,
};
use pagebara_syntax::Document;

use super::*;
use crate::test_pdf;
use crate::text_index::TextIndex;

fn doc() -> Document {
    Document::open(test_pdf::text(&[
        "Contact jane.doe@example.org today",
        "IBAN DE89 3704 0044 0532 0130 00 please",
    ]))
    .unwrap()
}

#[test]
fn patterns_from_the_panel() {
    assert!(pattern(0, "  ", false, false).is_err());
    assert!(pattern(1, "(", false, false).is_err());
    let d = doc();
    let hits = |p: Pattern| find(&d, &p, None);
    assert_eq!(
        hits(pattern(2, "", false, false).unwrap())[0].text,
        "jane.doe@example.org"
    );
    assert_eq!(hits(pattern(4, "", false, false).unwrap()).len(), 1);
    assert_eq!(hits(pattern(0, "CONTACT", false, false).unwrap()).len(), 1);
    assert!(hits(pattern(0, "CONTACT", true, false).unwrap()).is_empty());
    assert!(hits(pattern(0, "jane", false, true).unwrap()).len() == 1);
    assert!(hits(pattern(0, "jan", false, true).unwrap()).is_empty());
    assert_eq!(
        hits(pattern(1, "TODAY|PLEASE", false, false).unwrap()).len(),
        2
    );
}

#[test]
fn styles_round_trip_through_the_annotation() {
    let mut d = doc();
    let page = pagebara_pages::PageTree::load(&d).pages()[0].r;
    let m = styled(mark_rect([72.0, 760.0, 200.0, 775.0]), 1, " Redacted ");
    assert_eq!(m.fill, Some([1.0; 3]));
    assert_eq!(m.text_color, [0.0; 3]);
    add_mark(&mut d, page, &m).unwrap();
    let (r, back) = marks(&d, page).pop().unwrap();
    assert_eq!(fill_index(&back), 1);
    assert_eq!(back.overlay_text.as_deref(), Some("Redacted"));
    let again = read_mark(&d, d.get(r).unwrap().as_dict().unwrap());
    assert_eq!(again, back);
    assert_eq!(styled(Mark::default(), 2, "").fill, None);
}

#[test]
fn covered_text_is_verified_gone_after_applying() {
    let mut d = doc();
    let page = pagebara_pages::PageTree::load(&d).pages()[0].r;
    let hit = find(&d, &pattern(2, "", false, false).unwrap(), None).remove(0);
    let m = styled(hit.mark(), 0, "");
    let idx = TextIndex::new(d.clone(), vec![page]);
    let covered = text_under(&idx.page(0), &m);
    assert_eq!(covered, "jane.doe@example.org");
    add_mark(&mut d, page, &m).unwrap();
    let report = apply(&mut d, &ApplyOptions::default()).unwrap();
    sanitize(
        &mut d,
        &sanitize_from(&sanitize_flags(&Sanitize::default())),
    );
    let out = save(&d).unwrap();
    let secrets = secrets([covered, "ab".into()]);
    assert_eq!(secrets, ["jane.doe@example.org"]);
    let refs: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let v = verify(&out, None, &report.regions(), &refs);
    let (clean, lines) = report_lines(&v);
    assert!(clean, "{lines:?}");
    // The same check on the unredacted file finds the address.
    let v = verify(&doc().save_full().unwrap(), None, &report.regions(), &refs);
    let (clean, lines) = report_lines(&v);
    assert!(!clean);
    assert!(
        lines.iter().any(|l| l.contains("jane.doe@example.org")),
        "{lines:?}"
    );
}

#[test]
fn glyph_boxes_join_per_line() {
    let m = mark_boxes(&[
        [0.0, 0.0, 5.0, 10.0],
        [5.0, 0.0, 10.0, 10.0],
        [10.5, 0.2, 15.0, 10.0],
        [0.0, 20.0, 5.0, 30.0],
    ]);
    let j = joined(m);
    assert_eq!(j.quads.len(), 2);
    assert_eq!(quad_bbox(&j.quads[0]), [0.0, 0.0, 15.0, 10.0]);
    let d = doc();
    let hit = find(&d, &pattern(2, "", false, false).unwrap(), None).remove(0);
    assert_eq!(joined(hit.mark()).quads.len(), 1);
}

#[test]
fn boxes_become_quads() {
    let m = mark_boxes(&[[1.0, 2.0, 3.0, 4.0]]);
    assert_eq!(quad_bbox(&m.quads[0]), [1.0, 2.0, 3.0, 4.0]);
    assert_eq!(
        quad_bbox(&mark_rect([5.0, 9.0, 1.0, 2.0]).quads[0]),
        [1.0, 2.0, 5.0, 9.0]
    );
    assert_eq!(sanitize_from(&[true; 8]), Sanitize::all());
}
