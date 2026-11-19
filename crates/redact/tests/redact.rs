mod common;

use common::*;
use pagebara_redact::{
    ApplyOptions, Mark, Pattern, Rect, Region, Report, Sanitize, add_mark, apply, apply_marks,
    find, sanitize, save, verify,
};
use pagebara_syntax::Document;

struct Out {
    before: Document,
    after: Document,
    bytes: Vec<u8>,
    report: Report,
}

/// Marks every match of `secrets` (or the given marks), applies, sanitises, saves.
fn run(file: Vec<u8>, secrets: &[&str], marks: &[(usize, Mark)], opts: &ApplyOptions) -> Out {
    let before = Document::open(file).unwrap();
    let mut doc = before.clone();
    let tree = pagebara_pages::PageTree::load(&doc);
    for s in secrets {
        let hits = find(&doc, &Pattern::text(s), None);
        assert!(!hits.is_empty(), "{s} is not on the page");
        for h in hits {
            add_mark(&mut doc, tree.pages()[h.page].r, &h.mark()).unwrap();
        }
    }
    let report = apply_marks(&mut doc, marks, opts).unwrap();
    sanitize(&mut doc, &Sanitize::default());
    let bytes = save(&doc).unwrap();
    let after = Document::open(bytes.clone()).unwrap();
    Out {
        before,
        after,
        bytes,
        report,
    }
}

/// The checks every redaction must pass.
fn assert_redacted(o: &Out, secrets: &[&str]) {
    let v = verify(&o.bytes, None, &o.report.regions(), secrets);
    assert!(v.is_clean(), "{v:#?}");
    let all = all_bytes(&o.bytes);
    for s in secrets {
        assert!(!contains_ci(&all, s), "{s} is still in the file");
    }
    external_checks(&o.bytes, secrets);
}

/// Renders before and after are the same away from the regions.
fn assert_same_outside(o: &Out, page: usize, rotate: u16, margin: i64) {
    assert_similar_outside(o, page, rotate, margin, 2);
}

/// As [`assert_same_outside`], allowing anti-aliasing differences up to `tolerance`.
fn assert_similar_outside(o: &Out, page: usize, rotate: u16, margin: i64, tolerance: u8) {
    let a = render(&o.before, page);
    let b = render(&o.after, page);
    let boxes: Vec<_> = o
        .report
        .pages
        .iter()
        .filter(|p| p.page == page)
        .flat_map(|p| {
            p.regions
                .iter()
                .map(|r| device_box(&r.bbox(), rotate, margin))
        })
        .collect();
    assert_eq!(
        diff_outside(&a, &b, &boxes, tolerance),
        0,
        "renders differ outside the regions"
    );
}

/// Glyphs away from the regions keep their text and boxes.
fn assert_other_glyphs_kept(o: &Out, page: usize, secret_chars: usize) {
    let before = glyphs(&o.before, page);
    let after = glyphs(&o.after, page);
    let regions: Vec<Region> = o
        .report
        .pages
        .iter()
        .flat_map(|p| p.regions.clone())
        .collect();
    let kept: Vec<_> = before
        .iter()
        .filter(|(_, b)| {
            // Boxes rounded to 0.01 pt; neighbours only touch the regions.
            let r = Rect::new(
                b[0] as f64 / 100.0 + 0.02,
                b[1] as f64 / 100.0 + 0.02,
                b[2] as f64 / 100.0 - 0.02,
                b[3] as f64 / 100.0 - 0.02,
            );
            !regions.iter().any(|g| {
                g.bbox().x0 < r.x1 && r.x0 < g.bbox().x1 && g.bbox().y0 < r.y1 && r.y0 < g.bbox().y1
            })
        })
        .cloned()
        .collect();
    assert_eq!(before.len() - kept.len(), secret_chars, "{before:?}");
    assert_eq!(after, kept);
}

#[test]
fn plain_text() {
    let mut p = Pdf::new();
    p.page(
        HELV,
        "BT /F1 12 Tf 20 150 Td (Public before SECRET-1234 after public) Tj ET\n\
         BT /F1 12 Tf 20 100 Td (Second line stays) Tj ET",
        "",
    );
    let o = run(p.finish(), &["SECRET-1234"], &[], &ApplyOptions::default());
    assert_redacted(&o, &["SECRET-1234"]);
    assert_other_glyphs_kept(&o, 0, 11);
    assert_same_outside(&o, 0, 0, 2);
    let t = page_text(&o.after, 0).text;
    assert!(
        t.contains("Public before") && t.contains("after public"),
        "{t}"
    );
    assert!(t.contains("Second line stays"));
    assert_eq!(o.report.pages[0].stats.glyphs_removed, 11);
}

#[test]
fn kerning_spacing_and_line_operators() {
    let mut p = Pdf::new();
    p.page(
        HELV,
        "BT /F1 11 Tf 2 Tc 3 Tw 90 Tz 14 TL 20 160 Td\n\
         [(Name: ) -120 (Jo) 30 (hn) -250 (SECRET) 80 (VALUE) -300 (tail text)] TJ\n\
         (next line SECRET too) '\n\
         5 1 (third SECRET line) \"\n\
         (after the quote) Tj\n\
         ET",
        "",
    );
    let o = run(p.finish(), &["SECRET"], &[], &ApplyOptions::default());
    assert_redacted(&o, &["SECRET"]);
    assert_other_glyphs_kept(&o, 0, 18);
    assert_same_outside(&o, 0, 0, 2);
}

#[test]
fn vertical_cid_text() {
    // Identity-V text: the TJ adjustments of removed glyphs move vertically.
    let font = "/Font << /F1 << /Type /Font /Subtype /Type0 /BaseFont /Helvetica /Encoding /Identity-V \
                /DescendantFonts [<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Helvetica \
                /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /DW 1000 >>] >> >>";
    let mut p = Pdf::new();
    p.page(
        font,
        "BT /F1 12 Tf 100 180 Td <00410042004300440045004600470048> Tj ET",
        "",
    );
    let file = p.finish();
    let doc = Document::open(file.clone()).unwrap();
    let before = glyphs(&doc, 0);
    assert_eq!(before.len(), 8);
    // A region over the third and fourth glyphs.
    let b2 = before[2].1;
    let b3 = before[3].1;
    let r = Rect::new(
        b2[0] as f64 / 100.0 + 1.0,
        b3[1] as f64 / 100.0 + 1.0,
        b2[2] as f64 / 100.0 - 1.0,
        b2[3] as f64 / 100.0 - 1.0,
    );
    let o = run(file, &[], &[(0, Mark::rect(r))], &ApplyOptions::default());
    let after = glyphs(&o.after, 0);
    assert_eq!(after.len(), 6);
    assert_eq!(after[..2], before[..2]);
    assert_eq!(after[2..], before[4..]);
}

#[test]
fn form_shared_by_two_pages() {
    let mut p = Pdf::new();
    let form = p.stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 250 40] /Matrix [1 0 0 1 20 100] \
         /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>",
        b"BT /F1 12 Tf 10 10 Td (Shared FORMSECRET in a form) Tj ET",
    );
    let res = format!("{HELV} /XObject << /Fm {form} 0 R >>");
    p.page(
        &res,
        "q /Fm Do Q BT /F1 12 Tf 20 160 Td (Page one) Tj ET",
        "",
    );
    p.page(
        &res,
        "q /Fm Do Q BT /F1 12 Tf 20 160 Td (Page two) Tj ET",
        "",
    );
    let file = p.finish();
    let doc = Document::open(file.clone()).unwrap();
    let hit = find(&doc, &Pattern::text("FORMSECRET"), Some(&[0]));
    assert_eq!(hit.len(), 1);
    let o = run(file, &[], &[(0, hit[0].mark())], &ApplyOptions::default());
    let v = verify(&o.bytes, None, &o.report.regions(), &[]);
    assert!(v.is_clean(), "{v:#?}");
    assert!(!page_text(&o.after, 0).text.contains("FORMSECRET"));
    assert!(page_text(&o.after, 1).text.contains("FORMSECRET"));
    assert_eq!(o.report.pages[0].stats.forms_copied, 1);
    assert_same_outside(&o, 0, 0, 2);
    // The other page is untouched.
    assert_eq!(
        diff_outside(&render(&o.before, 1), &render(&o.after, 1), &[], 0),
        0
    );
    assert_eq!(glyphs(&o.before, 1), glyphs(&o.after, 1));
    external_checks(&o.bytes, &[]);
}

#[test]
fn rotated_page() {
    let mut p = Pdf::new();
    p.page(
        HELV,
        "BT /F1 12 Tf 20 150 Td (Rotated ROTSECRET text) Tj ET\n\
         BT /F1 12 Tf 0 1 -1 0 280 20 Tm (Sideways SIDESECRET) Tj ET",
        "/Rotate 90",
    );
    let o = run(
        p.finish(),
        &["ROTSECRET", "SIDESECRET"],
        &[],
        &ApplyOptions::default(),
    );
    assert_redacted(&o, &["ROTSECRET", "SIDESECRET"]);
    assert_other_glyphs_kept(&o, 0, 19);
    assert_same_outside(&o, 0, 90, 2);
}

/// A 40 × 20 RGB image whose every pixel differs, and a 16 × 8 gray inline image.
fn image_page(smask: bool) -> Vec<u8> {
    let mut p = Pdf::new();
    let mut rgb = Vec::new();
    for y in 0..20u8 {
        for x in 0..40u8 {
            rgb.extend_from_slice(&[x * 6, y * 12, 255 - x * 3]);
        }
    }
    let sm = if smask {
        let alpha: Vec<u8> = (0..40 * 20).map(|i| 128 + (i % 128) as u8).collect();
        let s = p.stream(
            "/Type /XObject /Subtype /Image /Width 40 /Height 20 /ColorSpace /DeviceGray \
             /BitsPerComponent 8 /Filter /FlateDecode",
            &flate(&alpha),
        );
        format!("/SMask {s} 0 R")
    } else {
        String::new()
    };
    let im = p.stream(
        &format!(
            "/Type /XObject /Subtype /Image /Width 40 /Height 20 /ColorSpace /DeviceRGB \
             /BitsPerComponent 8 /Filter /FlateDecode {sm}"
        ),
        &flate(&rgb),
    );
    let gray: Vec<u8> = (0..16 * 8).map(|i| (i * 2) as u8).collect();
    let content = format!(
        "q 120 0 0 60 20 120 cm /Im1 Do Q\n\
         q 80 0 0 40 180 20 cm BI /W 16 /H 8 /BPC 8 /CS /G /F /AHx ID {}> EI Q",
        hex(&gray)
    );
    p.page(&format!("/XObject << /Im1 {im} 0 R >>"), &content, "");
    p.finish()
}

#[test]
fn images_are_blanked_in_their_data() {
    for smask in [false, true] {
        let marks = [
            (0, Mark::rect(Rect::new(50.0, 130.0, 90.0, 160.0))),
            (0, Mark::rect(Rect::new(200.0, 30.0, 230.0, 50.0))),
        ];
        let o = run(image_page(smask), &[], &marks, &ApplyOptions::default());
        assert_redacted(&o, &[]);
        let s = &o.report.pages[0].stats;
        assert_eq!((s.images_blanked, s.images_removed), (2, 0));
        // Pixels are 3 pt (image) and 5 pt (inline) wide: those partly inside are blanked.
        assert_same_outside(&o, 0, 0, 11);
        // The original samples are gone.
        let orig_row: Vec<u8> = (0..40u8)
            .flat_map(|x| [x * 6, 9 * 12, 255 - x * 3])
            .collect();
        assert!(common::find(&all_bytes(&o.bytes), &orig_row).is_none());
    }
}

#[test]
fn image_wholly_covered_is_dropped_everywhere() {
    let mut p = Pdf::new();
    let data = b"SECRETPIXELS".repeat(20);
    let im = p.stream(
        "/Type /XObject /Subtype /Image /Width 12 /Height 20 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &data,
    );
    // Both pages share the resources; only the first draws the image.
    let res = p.add(format!("<< /XObject << /Im1 {im} 0 R >> {HELV} >>"));
    let c1 = p.stream(
        "",
        b"q 60 0 0 100 50 50 cm /Im1 Do Q BT /F1 9 Tf 150 50 Td (kept) Tj ET",
    );
    let c2 = p.stream("", b"BT /F1 9 Tf 20 50 Td (other page) Tj ET");
    for c in [c1, c2] {
        let pg = p.add(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources {res} 0 R /Contents {c} 0 R >>"
        ));
        p.pages_push(pg);
    }
    let o = run(
        p.finish(),
        &[],
        &[(0, Mark::rect(Rect::new(40.0, 40.0, 120.0, 160.0)))],
        &ApplyOptions::default(),
    );
    assert_eq!(o.report.pages[0].stats.images_removed, 1);
    assert_redacted(&o, &["SECRETPIXELS"]);
    assert_same_outside(&o, 0, 0, 2);
}

#[test]
fn vector_art_is_cut() {
    let mut p = Pdf::new();
    p.page(
        "",
        "0.8 0.9 1 rg 10 10 280 180 re f\n\
         1 0 0 RG 3 w 20 20 m 280 180 l S\n\
         0 0.5 0 rg 100 60 m 100 140 200 140 200 60 c h f\n\
         0 0 1 RG 2 w [6 3] 1 d 20 100 m 280 100 l S\n\
         0.2 g 130 90 30 20 re f",
        "",
    );
    let region = Rect::new(120.0, 80.0, 180.0, 130.0);
    let marks = [(0, Mark::rect(region))];
    let o = run(p.finish(), &[], &marks, &ApplyOptions::default());
    assert_redacted(&o, &[]);
    // Butt ends of cut diagonal strokes lose a sliver up to half the width beyond the edge,
    // and curve pieces flatten at other points than the whole curve (edge anti-aliasing).
    assert_similar_outside(&o, 0, 0, 5, 24);
    // Without the overlay the region is empty paper: the geometry is gone, not covered.
    let bare = run(
        o.before.source().to_vec(),
        &[],
        &marks,
        &ApplyOptions {
            overlay: false,
            ..ApplyOptions::default()
        },
    );
    let img = render(&bare.after, 0);
    assert_eq!(non_white_inside(&img, device_box(&region, 0, -2)), 0);
    assert!(bare.report.pages[0].stats.paths_changed >= 4);
    // The small square inside the region is dropped.
    assert!(!String::from_utf8_lossy(&all_bytes(&bare.bytes)).contains("130 90 30 20 re"));
}

#[test]
fn text_without_overlay_leaves_paper() {
    let mut p = Pdf::new();
    p.page(HELV, "BT /F1 20 Tf 20 100 Td (GONE-TEXT) Tj ET", "");
    let opts = ApplyOptions {
        overlay: false,
        ..ApplyOptions::default()
    };
    let o = run(p.finish(), &["GONE-TEXT"], &[], &opts);
    let r = o.report.pages[0].regions[0].bbox();
    assert_eq!(
        non_white_inside(&render(&o.after, 0), device_box(&r, 0, 2)),
        0
    );
    assert_redacted(&o, &["GONE-TEXT"]);
}

#[test]
fn annotations_in_regions_are_removed() {
    let mut p = Pdf::new();
    let popup = p.reserve();
    let note = p.add(format!(
        "<< /Type /Annot /Subtype /Text /Rect [100 140 120 160] /Contents (NOTESECRET) /Popup {popup} 0 R >>"
    ));
    p.set(
        popup,
        format!("<< /Type /Annot /Subtype /Popup /Rect [200 100 290 190] /Parent {note} 0 R >>"),
    );
    let reply = p.add(format!(
        "<< /Type /Annot /Subtype /Text /Rect [250 10 270 30] /Contents (REPLYSECRET) /IRT {note} 0 R >>"
    ));
    let link = p.add("<< /Type /Annot /Subtype /Link /Rect [90 145 130 155] /A << /S /URI /URI (http://LINKSECRET.example) >> >>");
    let hl = p.add(
        "<< /Type /Annot /Subtype /Highlight /Rect [10 10 60 30] /QuadPoints [10 30 60 30 10 10 60 10] /Contents (kept) >>",
    );
    p.page(
        HELV,
        "BT /F1 12 Tf 20 150 Td (Annotated) Tj ET",
        &format!("/Annots [{note} 0 R {popup} 0 R {reply} 0 R {link} 0 R {hl} 0 R]"),
    );
    let o = run(
        p.finish(),
        &[],
        &[(0, Mark::rect(Rect::new(95.0, 135.0, 140.0, 165.0)))],
        &ApplyOptions::default(),
    );
    assert_redacted(&o, &["NOTESECRET", "REPLYSECRET", "LINKSECRET"]);
    assert_eq!(o.report.pages[0].annotations_removed, 4);
    let page = o
        .after
        .get(pagebara_pages::PageTree::load(&o.after).pages()[0].r)
        .unwrap();
    let left = pagebara_annot::annotations(&o.after, page.as_dict().unwrap());
    assert_eq!(left.len(), 1);
}

#[test]
fn form_fields_in_regions_are_cleared() {
    let mut p = Pdf::new();
    let ap1 = p.stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /Helv << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>",
        b"/Tx BMC BT /Helv 10 Tf 2 5 Td (FIELDSECRET) Tj ET EMC",
    );
    let ap2 = p.stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /Helv << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >>",
        b"/Tx BMC BT /Helv 10 Tf 2 5 Td (Paris) Tj ET EMC",
    );
    let pg = p.reserve();
    let f1 = p.add(format!(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (name) /V (FIELDSECRET) /DV (FIELDSECRET) /Rect [50 150 150 170] /P {pg} 0 R /AP << /N {ap1} 0 R >> >>"
    ));
    let f2 = p.add(format!(
        "<< /Type /Annot /Subtype /Widget /FT /Tx /T (city) /V (Paris) /Rect [50 50 150 70] /P {pg} 0 R /AP << /N {ap2} 0 R >> >>"
    ));
    let c = p.stream("", b"BT /F1 12 Tf 20 180 Td (Form) Tj ET");
    p.set(
        pg,
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << {HELV} >> /Contents {c} 0 R /Annots [{f1} 0 R {f2} 0 R] >>"
        ),
    );
    p.pages_push(pg);
    p.catalog = format!(
        "/AcroForm << /Fields [{f1} 0 R {f2} 0 R] /DA (/Helv 0 Tf 0 g) /XFA (<xdp>FIELDSECRET</xdp>) >>"
    );
    let o = run(
        p.finish(),
        &[],
        &[(0, Mark::rect(Rect::new(40.0, 140.0, 160.0, 180.0)))],
        &ApplyOptions::default(),
    );
    assert_redacted(&o, &["FIELDSECRET"]);
    assert_eq!(o.report.pages[0].fields_cleared, ["name"]);
    let form = pagebara_forms::Form::load(&o.after);
    assert!(form.field("name").is_none());
    assert_eq!(form.field("city").unwrap().text(), "Paris");
}

#[test]
fn incremental_history_is_dropped() {
    let mut p = Pdf::new();
    p.page(
        HELV,
        "BT /F1 12 Tf 20 150 Td (Old OLDSECRET revision) Tj ET",
        "",
    );
    let rev1 = p.finish();
    // Revision 2 replaces the contents (object 3).
    let content = b"BT /F1 12 Tf 20 120 Td (New NEWSECRET revision) Tj ET";
    let rev2 = append_update(rev1, &[(3, stream_body("", content))], 5);
    let before = verify(&rev2, None, &[], &["OLDSECRET"]);
    assert_eq!(before.revisions, 2);
    assert!(!before.secrets_found.is_empty());
    let o = run(rev2, &["NEWSECRET"], &[], &ApplyOptions::default());
    assert_redacted(&o, &["OLDSECRET", "NEWSECRET"]);
    assert!(page_text(&o.after, 0).text.contains("New"));
}

#[test]
fn type3_glyphs() {
    let mut p = Pdf::new();
    let a = p.stream("", b"500 0 0 0 400 700 d1 0 0 400 700 re f");
    let b = p.stream("", b"500 0 0 0 400 500 d1 0 0 400 500 re f");
    let font = format!(
        "/Font << /T3 << /Type /Font /Subtype /Type3 /FontBBox [0 0 400 700] /FontMatrix [0.001 0 0 0.001 0 0] \
         /CharProcs << /a {a} 0 R /b {b} 0 R >> /Encoding << /Type /Encoding /Differences [97 /a /b] >> \
         /FirstChar 97 /LastChar 98 /Widths [500 500] >> >>"
    );
    p.page(&font, "BT /T3 20 Tf 20 100 Td (abba ab) Tj ET", "");
    let file = p.finish();
    let doc = Document::open(file.clone()).unwrap();
    let before = glyphs(&doc, 0);
    let r = Rect::new(
        before[1].1[0] as f64 / 100.0 + 1.0,
        95.0,
        before[2].1[2] as f64 / 100.0 - 1.0,
        120.0,
    );
    let o = run(file, &[], &[(0, Mark::rect(r))], &ApplyOptions::default());
    assert_eq!(o.report.pages[0].stats.glyphs_removed, 2);
    assert_other_glyphs_kept(&o, 0, 2);
    assert_same_outside(&o, 0, 0, 2);
    assert_redacted(&o, &[]);
}

#[test]
fn hidden_layers_and_actual_text() {
    let mut p = Pdf::new();
    let ocg = p.add("<< /Type /OCG /Name (Hidden) >>");
    p.catalog = format!("/OCProperties << /OCGs [{ocg} 0 R] /D << /OFF [{ocg} 0 R] >> >>");
    let res = format!("{HELV} /Properties << /oc1 {ocg} 0 R >>");
    p.page(
        &res,
        "/OC /oc1 BDC BT /F1 12 Tf 20 50 Td (HIDDENSECRET layer) Tj ET EMC\n\
         /Span << /ActualText (ACTUALSECRET) >> BDC BT /F1 12 Tf 20 150 Td (Visible ATSECRET) Tj ET EMC\n\
         BT /F1 12 Tf 20 100 Td (Plain line) Tj ET",
        "",
    );
    // The span's actual text replaces its glyphs in extraction, so mark by area.
    let mark = Mark::rect(Rect::new(64.0, 145.0, 200.0, 165.0));
    let o = run(p.finish(), &[], &[(0, mark)], &ApplyOptions::default());
    assert_redacted(&o, &["HIDDENSECRET", "ACTUALSECRET", "ATSECRET"]);
    assert!(page_text(&o.after, 0).text.contains("Plain line"));
}

#[test]
fn structure_alt_text_and_metadata() {
    let mut p = Pdf::new();
    let pg = p.reserve();
    let elem = p.reserve();
    let root = p.reserve();
    let c = p.stream(
        "",
        b"/Figure << /MCID 0 >> BDC BT /F1 12 Tf 20 150 Td (ALTSECRET) Tj ET EMC BT /F1 12 Tf 20 100 Td (kept) Tj ET",
    );
    p.set(
        pg,
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << {HELV} >> /Contents {c} 0 R /StructParents 0 >>"
        ),
    );
    p.pages_push(pg);
    p.set(
        elem,
        format!("<< /Type /StructElem /S /Figure /P {root} 0 R /Pg {pg} 0 R /K 0 /Alt (Photo of ALTSECRET) >>"),
    );
    p.set(
        root,
        format!(
            "<< /Type /StructTreeRoot /K [{elem} 0 R] /ParentTree << /Nums [0 [{elem} 0 R]] >> >>"
        ),
    );
    let xmp = p.stream(
        "/Type /Metadata /Subtype /XML",
        b"<x:xmpmeta><dc:title>XMPSECRET</dc:title></x:xmpmeta>",
    );
    let info = p.add("<< /Title (INFOSECRET) /Author (Someone) >>");
    p.catalog =
        format!("/StructTreeRoot {root} 0 R /MarkInfo << /Marked true >> /Metadata {xmp} 0 R");
    p.trailer = format!("/Info {info} 0 R");
    let o = run(p.finish(), &["ALTSECRET"], &[], &ApplyOptions::default());
    assert_eq!(o.report.structure_cleaned, 1);
    assert_redacted(&o, &["ALTSECRET", "XMPSECRET", "INFOSECRET"]);
}

#[test]
fn search_patterns_find_personal_data() {
    let mut p = Pdf::new();
    p.page(
        HELV,
        "BT /F1 10 Tf 20 170 Td (Mail jane.doe@example.com today) Tj ET\n\
         BT /F1 10 Tf 20 150 Td (Card 4111 1111 1111 1111 expires) Tj ET\n\
         BT /F1 10 Tf 20 130 Td (IBAN DE89 3704 0044 0532 0130 00 ok) Tj ET\n\
         BT /F1 10 Tf 20 110 Td (Call +49 30 1234567 later) Tj ET",
        "",
    );
    let file = p.finish();
    let mut doc = Document::open(file).unwrap();
    let page = pagebara_pages::PageTree::load(&doc).pages()[0].r;
    let mut found = Vec::new();
    for k in [
        pagebara_redact::Kind::Email,
        pagebara_redact::Kind::CreditCard,
        pagebara_redact::Kind::Iban,
        pagebara_redact::Kind::Phone,
    ] {
        for h in find(&doc, &Pattern::Kind(k), None) {
            found.push(h.text.clone());
            add_mark(&mut doc, page, &h.mark()).unwrap();
        }
    }
    assert_eq!(
        found,
        [
            "jane.doe@example.com",
            "4111 1111 1111 1111",
            "DE89 3704 0044 0532 0130 00",
            "+49 30 1234567"
        ]
    );
    let report = apply(&mut doc, &ApplyOptions::default()).unwrap();
    let out = save(&doc).unwrap();
    let secrets: Vec<&str> = found.iter().map(String::as_str).collect();
    let v = verify(&out, None, &report.regions(), &secrets);
    assert!(v.is_clean(), "{v:#?}");
    let t = page_text(&Document::open(out).unwrap(), 0).text;
    for w in [
        "Mail", "today", "Card", "expires", "IBAN", "ok", "Call", "later",
    ] {
        assert!(t.contains(w), "{w} missing from {t}");
    }
}

#[test]
fn marks_round_trip() {
    let mut p = Pdf::new();
    p.page(HELV, "BT /F1 12 Tf 20 150 Td (Text) Tj ET", "");
    let mut doc = Document::open(p.finish()).unwrap();
    let page = pagebara_pages::PageTree::load(&doc).pages()[0].r;
    let m = Mark {
        overlay_text: Some("Redacted §4".into()),
        repeat: true,
        fill: Some([0.2, 0.2, 0.2]),
        text_color: [1.0, 1.0, 0.0],
        font_size: 9.0,
        ..Mark::rect(Rect::new(10.0, 10.0, 200.0, 60.0))
    };
    let r = add_mark(&mut doc, page, &m).unwrap();
    let doc = Document::open(doc.save_incremental().unwrap()).unwrap();
    let read = pagebara_redact::marks(&doc, page);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].0, r);
    assert_eq!(read[0].1.regions(), m.regions());
    assert_eq!(
        Mark {
            quads: m.quads.clone(),
            ..read[0].1.clone()
        },
        m
    );
}

#[test]
fn overlay_text_is_drawn_and_ignored_by_verify() {
    let mut p = Pdf::new();
    p.page(
        HELV,
        "BT /F1 12 Tf 20 150 Td (Hide OVERSECRET here) Tj ET",
        "/Rotate 270",
    );
    let mut doc = Document::open(p.finish()).unwrap();
    let page = pagebara_pages::PageTree::load(&doc).pages()[0].r;
    for h in find(&doc, &Pattern::text("OVERSECRET"), None) {
        let m = Mark {
            overlay_text: Some("X".into()),
            repeat: true,
            ..h.mark()
        };
        add_mark(&mut doc, page, &m).unwrap();
    }
    let report = apply(&mut doc, &ApplyOptions::default()).unwrap();
    let out = save(&doc).unwrap();
    let after = Document::open(out.clone()).unwrap();
    assert!(page_text(&after, 0).text.contains('X'));
    let v = verify(&out, None, &report.regions(), &["OVERSECRET"]);
    assert!(v.is_clean(), "{v:#?}");
    external_checks(&out, &["OVERSECRET"]);
}

#[test]
fn encrypted_documents_stay_encrypted() {
    let mut p = Pdf::new();
    p.page(
        HELV,
        "BT /F1 12 Tf 20 150 Td (Locked LOCKSECRET text) Tj ET",
        "",
    );
    let mut doc = Document::open(p.finish()).unwrap();
    doc.set_password("user", "owner", pagebara_syntax::Permissions::ALL)
        .unwrap();
    let locked = doc.save_full().unwrap();
    let mut doc = match Document::open(locked) {
        Err(pagebara_syntax::Error::PasswordRequired(l)) => l.unlock("user").unwrap(),
        _ => panic!("not encrypted"),
    };
    let page = pagebara_pages::PageTree::load(&doc).pages()[0].r;
    for h in find(&doc, &Pattern::text("LOCKSECRET"), None) {
        add_mark(&mut doc, page, &h.mark()).unwrap();
    }
    let report = apply(&mut doc, &ApplyOptions::default()).unwrap();
    let out = save(&doc).unwrap();
    assert!(matches!(
        Document::open(out.clone()),
        Err(pagebara_syntax::Error::PasswordRequired(_))
    ));
    let v = verify(&out, Some("user"), &report.regions(), &["LOCKSECRET"]);
    assert!(v.is_clean(), "{v:#?}");
    assert!(!verify(&out, None, &report.regions(), &[]).is_clean());
}

#[test]
fn incremental_save_after_redaction_is_detected() {
    // What the app must never do: an incremental save keeps the original content, and
    // verification says so.
    let mut p = Pdf::new();
    p.page(
        HELV,
        "BT /F1 12 Tf 20 150 Td (Keep INCSECRET out) Tj ET",
        "",
    );
    let mut doc = Document::open(p.finish()).unwrap();
    let page = pagebara_pages::PageTree::load(&doc).pages()[0].r;
    for h in find(&doc, &Pattern::text("INCSECRET"), None) {
        add_mark(&mut doc, page, &h.mark()).unwrap();
    }
    let report = apply(&mut doc, &ApplyOptions::default()).unwrap();
    let bad = doc.save_incremental().unwrap();
    let v = verify(&bad, None, &report.regions(), &["INCSECRET"]);
    assert_eq!(v.revisions, 2);
    assert!(!v.secrets_found.is_empty());
    assert!(v.text_in_regions.is_empty());
}

/// Mesh vertices as 8-bit samples (decode `[0 255 0 255 …]`): flag, x, y, r, g, b.
fn mesh_vertices(vs: &[(u8, u8, u8)]) -> Vec<u8> {
    vs.iter()
        .flat_map(|&(f, x, y)| [f, x, y, 200, x, y])
        .collect()
}

/// A Coons patch over the square `x0..x1` × `y0..y1` with flat edges: flag, twelve
/// boundary points, four colours.
fn coons_square(x0: u8, y0: u8, x1: u8, y1: u8) -> Vec<u8> {
    let (w, h) = ((x1 - x0) / 3, (y1 - y0) / 3);
    let pts = [
        (x0, y0),
        (x0, y0 + h),
        (x0, y1 - h),
        (x0, y1),
        (x0 + w, y1),
        (x1 - w, y1),
        (x1, y1),
        (x1, y1 - h),
        (x1, y0 + h),
        (x1, y0),
        (x1 - w, y0),
        (x0 + w, y0),
    ];
    let mut out = vec![0];
    for (x, y) in pts {
        out.extend([x, y]);
    }
    for c in [[250, 0, 0], [0, 250, 0], [0, 0, 250], [250, 250, 0]] {
        out.extend(c);
    }
    out
}

/// Mesh shadings keep no parts inside a region: drawn with `sh` and as a pattern fill.
#[test]
fn mesh_shadings_lose_parts_inside_regions() {
    let mut p = Pdf::new();
    let tri = mesh_vertices(&[
        (0, 120, 60),
        (0, 180, 60),
        (0, 150, 140),
        (0, 10, 10),
        (0, 80, 10),
        (0, 40, 90),
    ]);
    let sh = p.stream(
        "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 \
         /BitsPerFlag 8 /Decode [0 255 0 255 0 1 0 1 0 1]",
        &tri,
    );
    let mut patches = coons_square(110, 60, 190, 140);
    patches.extend(coons_square(10, 100, 90, 190));
    let psh = p.stream(
        "/ShadingType 6 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 \
         /BitsPerFlag 8 /Decode [0 255 0 255 0 1 0 1 0 1]",
        &patches,
    );
    let pat = p.add(format!("<< /PatternType 2 /Shading {psh} 0 R >>"));
    p.page(
        &format!("/Shading << /Sh0 {sh} 0 R >> /Pattern << /P0 {pat} 0 R >>"),
        "q /Sh0 sh Q q /Pattern cs /P0 scn 0 0 300 200 re f Q",
        "",
    );
    let mark = Mark::rect(Rect::new(100.0, 50.0, 200.0, 150.0));
    let o = run(p.finish(), &[], &[(0, mark)], &ApplyOptions::default());
    assert_eq!(o.report.stats().mesh_parts_removed, 2);
    // What is left of each mesh: one triangle, one patch.
    let mut sizes = Vec::new();
    for n in o.after.object_numbers() {
        let ob = o.after.get(o.after.reference(n).unwrap()).unwrap();
        if let Some(s) = ob.as_stream()
            && let Some(t) = s.dict.get(b"ShadingType").and_then(|t| t.as_int())
        {
            sizes.push((t, o.after.decode(s).unwrap().data.len()));
        }
    }
    sizes.sort();
    assert_eq!(sizes, [(4, 18), (6, 37)]);
    assert_redacted(&o, &[]);
    assert_similar_outside(&o, 0, 0, 2, 4);
}

/// Streams of `doc` whose dictionary has `key` with integer value `v`.
fn streams_with(doc: &Document, key: &[u8], v: i64) -> Vec<pagebara_syntax::Stream> {
    doc.object_numbers()
        .into_iter()
        .filter_map(|n| doc.get(doc.reference(n)?))
        .filter_map(|o| o.as_stream().cloned())
        .filter(|s| s.dict.get(key).and_then(|t| t.as_int()) == Some(v))
        .collect()
}

/// `count` big-endian samples of `bits` bits each.
fn read_bits(data: &[u8], bits: u32, count: usize) -> Vec<u32> {
    let mut pos = 0usize;
    (0..count)
        .map(|_| {
            let mut v = 0u64;
            for _ in 0..bits {
                let b = data.get(pos / 8).copied().unwrap_or(0);
                v = v << 1 | u64::from(b >> (7 - pos % 8) & 1);
                pos += 1;
            }
            v as u32
        })
        .collect()
}

/// RGB samples of a 20 × 10 picture, none of them zero.
fn picture() -> Vec<u8> {
    (0..10u8)
        .flat_map(|j| (0..20u8).flat_map(move |i| [10 + i * 12, 10 + j * 24, 255 - i * 5]))
        .collect()
}

/// A function-based shading over (50, 50)–(250, 150) whose sampled function holds a
/// picture, drawn with `sh` and as a pattern: the samples that only take part in what
/// lies under the region are zeroed, the rest stays, and the page draws the same outside.
#[test]
fn sampled_function_shadings_lose_samples_under_regions() {
    for as_pattern in [false, true] {
        let mut p = Pdf::new();
        let f = p.stream(
            "/FunctionType 0 /Domain [0 1 0 1] /Range [0 1 0 1 0 1] /Size [20 10] \
             /BitsPerSample 8",
            &picture(),
        );
        let sh = p.add(format!(
            "<< /ShadingType 1 /ColorSpace /DeviceRGB /Matrix [200 0 0 100 50 50] \
             /Function {f} 0 R >>"
        ));
        let (res, content) = if as_pattern {
            let pat = p.add(format!("<< /PatternType 2 /Shading {sh} 0 R >>"));
            (
                format!("/Pattern << /P0 {pat} 0 R >>"),
                "/Pattern cs /P0 scn 50 50 200 100 re f",
            )
        } else {
            (format!("/Shading << /Sh0 {sh} 0 R >>"), "q /Sh0 sh Q")
        };
        p.page(&res, content, "");
        let area = Rect::new(100.0, 80.0, 180.0, 130.0);
        let o = run(
            p.finish(),
            &[],
            &[(0, Mark::rect(area))],
            &ApplyOptions::default(),
        );
        // Samples 6 to 11 across and 4 to 6 up have their whole cells inside.
        let gone = |i: usize, j: usize| (6..=11).contains(&i) && (4..=6).contains(&j);
        assert_eq!(o.report.stats().shading_samples_blanked, 18, "{as_pattern}");
        let fs = streams_with(&o.after, b"FunctionType", 0);
        assert_eq!(fs.len(), 1);
        let got = read_bits(&o.after.decode(&fs[0]).unwrap().data, 8, 600);
        let want = picture();
        for j in 0..10 {
            for i in 0..20 {
                let k = (j * 20 + i) * 3;
                if gone(i, j) {
                    assert_eq!(got[k..k + 3], [0, 0, 0], "sample {i}, {j}");
                } else {
                    let w: Vec<u32> = want[k..k + 3].iter().map(|&v| u32::from(v)).collect();
                    assert_eq!(got[k..k + 3], w[..], "sample {i}, {j}");
                }
            }
        }
        assert_redacted(&o, &[]);
        assert_similar_outside(&o, 0, 0, 2, 2);
    }
}

/// Decoded vertices of a type 4 mesh with RGB colours: point and colour.
fn triangle_vertices(doc: &Document, s: &pagebara_syntax::Stream) -> Vec<([f64; 2], [f64; 3])> {
    let int = |k: &[u8]| s.dict.get(k).and_then(|v| v.as_int()).unwrap() as u32;
    let (bpc, bpcomp, bpf) = (
        int(b"BitsPerCoordinate"),
        int(b"BitsPerComponent"),
        int(b"BitsPerFlag"),
    );
    let d: Vec<f64> = s
        .dict
        .get(b"Decode")
        .and_then(|d| d.as_array())
        .unwrap()
        .iter()
        .map(|v| v.as_f64().unwrap())
        .collect();
    let data = doc.decode(s).unwrap().data;
    let per = (bpf + 2 * bpc + 3 * bpcomp).next_multiple_of(8);
    let n = data.len() * 8 / per as usize;
    let mut out = Vec::new();
    for k in 0..n {
        let bits = &data[k * per as usize / 8..];
        let mut pos = bpf as usize;
        let mut take = |b: u32| {
            let x = read_bits(&bits[pos / 8..], b, 1)[0];
            pos += b as usize;
            x
        };
        let cmax = ((1u64 << bpc) - 1) as f64;
        let x = d[0] + f64::from(take(bpc)) / cmax * (d[1] - d[0]);
        let y = d[2] + f64::from(take(bpc)) / cmax * (d[3] - d[2]);
        let kmax = ((1u64 << bpcomp) - 1) as f64;
        let c = [0, 1, 2]
            .map(|i| d[4 + 2 * i] + f64::from(take(bpcomp)) / kmax * (d[5 + 2 * i] - d[4 + 2 * i]));
        out.push(([x, y], c));
    }
    out
}

/// A triangle with a vertex deep inside the region is cut at the region's boundary: no
/// vertex (and so not the colour it had) is left inside, and outside it draws the same.
#[test]
fn mesh_triangles_reaching_into_regions_are_cut() {
    let mut p = Pdf::new();
    // Vertices: flag, x, y (page units), r, g, b; the one at (150, 100) inside.
    let vs: [(u8, u8, [u8; 3]); 3] = [
        (20, 20, [250, 30, 30]),
        (250, 40, [30, 250, 30]),
        (150, 100, [13, 77, 201]),
    ];
    let data: Vec<u8> = vs
        .iter()
        .flat_map(|&(x, y, c)| [0, x, y, c[0], c[1], c[2]])
        .collect();
    let sh = p.stream(
        "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 \
         /BitsPerFlag 8 /Decode [0 255 0 255 0 1 0 1 0 1]",
        &data,
    );
    p.page(&format!("/Shading << /Sh0 {sh} 0 R >>"), "q /Sh0 sh Q", "");
    let area = Rect::new(120.0, 70.0, 190.0, 130.0);
    let o = run(
        p.finish(),
        &[],
        &[(0, Mark::rect(area))],
        &ApplyOptions::default(),
    );
    let meshes = streams_with(&o.after, b"ShadingType", 4);
    assert_eq!(meshes.len(), 1);
    let vs = triangle_vertices(&o.after, &meshes[0]);
    assert!(vs.len() >= 3 && vs.len() % 3 == 0);
    let secret = [13.0 / 255.0, 77.0 / 255.0, 201.0 / 255.0];
    for (p, c) in &vs {
        let inside = p[0] > area.x0 + 1e-3
            && p[0] < area.x1 - 1e-3
            && p[1] > area.y0 + 1e-3
            && p[1] < area.y1 - 1e-3;
        assert!(!inside, "vertex {p:?} inside");
        assert!(
            c.iter().zip(&secret).any(|(a, b)| (a - b).abs() > 0.01),
            "the inside colour is left at {p:?}"
        );
    }
    assert_redacted(&o, &[]);
    assert_similar_outside(&o, 0, 0, 2, 4);
}

/// A Coons patch with a corner inside the region is split until the pieces left that
/// reach into it are small: no corner colour of the inside is left, and the page draws the
/// same outside.
#[test]
fn mesh_patches_reaching_into_regions_are_split() {
    let mut p = Pdf::new();
    let mut data = coons_square(40, 40, 160, 160);
    // The corner at (160, 160) gets a colour of its own.
    let n = data.len();
    data[n - 6..n - 3].copy_from_slice(&[13, 77, 201]);
    let sh = p.stream(
        "/ShadingType 6 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 \
         /BitsPerFlag 8 /Decode [0 255 0 255 0 1 0 1 0 1]",
        &data,
    );
    p.page(&format!("/Shading << /Sh0 {sh} 0 R >>"), "q /Sh0 sh Q", "");
    let area = Rect::new(120.0, 120.0, 190.0, 190.0);
    let o = run(
        p.finish(),
        &[],
        &[(0, Mark::rect(area))],
        &ApplyOptions::default(),
    );
    let patches = streams_with(&o.after, b"ShadingType", 7);
    assert_eq!(patches.len(), 1);
    let s = &patches[0];
    let data = o.after.decode(s).unwrap().data;
    // Flag, 16 points of two 32-bit coordinates, 4 colours of three 16-bit components.
    let per = 8 + 16 * 64 + 4 * 48;
    assert_eq!(data.len() * 8 % per, 0);
    let count = data.len() * 8 / per;
    assert!(count > 4, "{count} patches");
    let max = f64::from(u32::MAX);
    for k in 0..count {
        let v = read_bits(&data[k * per / 8..], 8, 1);
        assert_eq!(v[0], 0);
        let pts = read_bits(&data[k * per / 8 + 1..], 32, 32);
        let cs = read_bits(&data[k * per / 8 + 129..], 16, 12);
        // Corners: stream points 0, 3, 6 and 9.
        for (c, at) in [0, 3, 6, 9].into_iter().enumerate() {
            let (x, y) = (
                f64::from(pts[2 * at]) / max * 255.0,
                f64::from(pts[2 * at + 1]) / max * 255.0,
            );
            let deep = x > area.x0 + 1.0 && y > area.y0 + 1.0;
            assert!(!deep, "corner ({x}, {y}) inside");
            let col = &cs[3 * c..3 * c + 3];
            assert!(
                col.iter()
                    .zip([13.0, 77.0, 201.0])
                    .any(|(&a, b)| (f64::from(a) / 65535.0 * 255.0 - b).abs() > 3.0),
                "the inside colour is left"
            );
        }
    }
    assert_redacted(&o, &[]);
    assert_similar_outside(&o, 0, 0, 2, 4);
}

/// A mesh with a parametric sampled function: the samples for values that only the
/// triangle under the region used are zeroed.
#[test]
fn mesh_function_samples_only_used_inside_go() {
    let mut p = Pdf::new();
    // Vertices: flag, x, y, t (8 bits over 0..1).
    let tri = |pts: [(u8, u8); 3], t: u8| -> Vec<u8> {
        pts.iter().flat_map(|&(x, y)| [0, x, y, t]).collect()
    };
    let mut data = tri([(10, 10), (80, 10), (40, 80)], 0);
    data.extend(tri([(130, 70), (170, 70), (150, 110)], 255));
    let samples: Vec<u8> = (0..16u8)
        .flat_map(|k| [k * 16 + 8, 255 - k * 16, 100])
        .collect();
    let f = p.stream(
        "/FunctionType 0 /Domain [0 1] /Range [0 1 0 1 0 1] /Size [16] /BitsPerSample 8",
        &samples,
    );
    let sh = p.stream(
        &format!(
            "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 \
             /BitsPerFlag 8 /Decode [0 255 0 255 0 1] /Function {f} 0 R"
        ),
        &data,
    );
    p.page(&format!("/Shading << /Sh0 {sh} 0 R >>"), "q /Sh0 sh Q", "");
    let area = Rect::new(120.0, 60.0, 180.0, 120.0);
    let o = run(
        p.finish(),
        &[],
        &[(0, Mark::rect(area))],
        &ApplyOptions::default(),
    );
    assert_eq!(o.report.stats().mesh_parts_removed, 1);
    // Only t = 0 is left: sample 0 and its neighbour stay.
    assert_eq!(o.report.stats().shading_samples_blanked, 14);
    let fs = streams_with(&o.after, b"FunctionType", 0);
    assert_eq!(fs.len(), 1);
    let got = read_bits(&o.after.decode(&fs[0]).unwrap().data, 8, 48);
    for k in 0..16 {
        let s = &got[3 * k..3 * k + 3];
        if k < 2 {
            assert_eq!(s, [u32::from(k as u8 * 16 + 8), 255 - k as u32 * 16, 100]);
        } else {
            assert_eq!(s, [0, 0, 0], "sample {k}");
        }
    }
    assert_redacted(&o, &[]);
    assert_similar_outside(&o, 0, 0, 2, 2);
}

/// Words found by search and redacted also leave bookmarks, destination names, document
/// information, XMP metadata, link targets and annotation texts.
#[test]
fn document_level_text_is_scrubbed() {
    let mut p = Pdf::new();
    let pg = p.reserve();
    let outlines = p.reserve();
    let item = p.add(format!(
        "<< /Title (Contract with Jane Doe) /Parent {outlines} 0 R /Dest (Jane Doe section) >>"
    ));
    p.set(
        outlines,
        format!("<< /Type /Outlines /First {item} 0 R /Last {item} 0 R /Count 1 >>"),
    );
    let dests = p.add(format!(
        "<< /Names [(Appendix) [{pg} 0 R /Fit] (Jane Doe section) [{pg} 0 R /XYZ 0 200 0]] >>"
    ));
    let link = p.add(
        "<< /Type /Annot /Subtype /Link /Rect [20 20 120 40] /A << /S /GoTo /D (Jane Doe section) >> >>",
    );
    let uri = p.add(
        "<< /Type /Annot /Subtype /Link /Rect [150 20 280 40] /A << /S /URI /URI (https://example.org/jane%20doe?n=JANE DOE) >> >>",
    );
    let note = p.add(
        "<< /Type /Annot /Subtype /Text /Rect [250 150 270 170] /Contents <FEFF00430061006C006C0020004A0061006E006500200044006F0065> >>",
    );
    let c = p.stream(
        "",
        b"BT /F1 12 Tf 20 150 Td (Party: Jane Doe, born 1970) Tj ET BT /F1 12 Tf 20 100 Td (Other text) Tj ET",
    );
    p.set(
        pg,
        format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << {HELV} >> /Contents {c} 0 R \
             /Annots [{link} 0 R {uri} 0 R {note} 0 R] >>"
        ),
    );
    p.pages_push(pg);
    let xmp = p.stream(
        "/Type /Metadata /Subtype /XML",
        "<x:xmpmeta><dc:title>Contract – jane doe</dc:title></x:xmpmeta>".as_bytes(),
    );
    let info = p.add("<< /Title (Contract with JANE DOE) /Author (Someone) >>");
    p.catalog =
        format!("/Outlines {outlines} 0 R /Names << /Dests {dests} 0 R >> /Metadata {xmp} 0 R");
    p.trailer = format!("/Info {info} 0 R");
    let mut doc = Document::open(p.finish()).unwrap();
    let tree = pagebara_pages::PageTree::load(&doc);
    for h in find(&doc, &Pattern::text("Jane Doe"), None) {
        add_mark(&mut doc, tree.pages()[h.page].r, &h.mark()).unwrap();
    }
    let report = apply(&mut doc, &ApplyOptions::default()).unwrap();
    // Metadata kept on purpose: the words must still go.
    let n = pagebara_redact::scrub(&mut doc, &["Jane Doe"]);
    assert!(n >= 7, "{n}");
    let bytes = save(&doc).unwrap();
    let v = verify(&bytes, None, &report.regions(), &["Jane Doe"]);
    assert!(v.is_clean(), "{v:#?}");
    let after = Document::open(bytes).unwrap();
    let cat = after.catalog().unwrap();
    let cat = cat.as_dict().unwrap();
    let ol = after.get_in(cat, b"Outlines");
    let first = after.get_in(ol.as_dict().unwrap(), b"First");
    let first = first.as_dict().unwrap();
    let title = pagebara_content::extract::text_string(&after.get_in(first, b"Title"));
    assert_eq!(title, "Contract with [redacted]");
    // The renamed destination still resolves, from the bookmark and from the link.
    let dest = after.get_in(first, b"Dest");
    assert_eq!(dest.as_string(), Some(&b"[redacted] section"[..]));
    let names = after.get_in(cat, b"Names");
    let dests = after.get_in(names.as_dict().unwrap(), b"Dests");
    let pairs = after.get_in(dests.as_dict().unwrap(), b"Names");
    let keys: Vec<Vec<u8>> = pairs
        .as_array()
        .unwrap()
        .iter()
        .step_by(2)
        .map(|k| k.as_string().unwrap().to_vec())
        .collect();
    assert_eq!(keys, [b"Appendix".to_vec(), b"[redacted] section".to_vec()]);
    let info = after.get_in(after.trailer(), b"Info");
    let t =
        pagebara_content::extract::text_string(&after.get_in(info.as_dict().unwrap(), b"Title"));
    assert_eq!(t, "Contract with [redacted]");
    assert!(page_text(&after, 0).text.contains("Other text"));
}

/// Links over a region go; others stay with their targets.
#[test]
fn links_over_regions_are_removed() {
    let mut p = Pdf::new();
    let over = p.add(
        "<< /Type /Annot /Subtype /Link /Rect [10 140 200 160] /QuadPoints [100 160 140 160 100 140 140 140] \
         /A << /S /URI /URI (https://example.org/SECRETLINK) >> >>",
    );
    let away = p.add(
        "<< /Type /Annot /Subtype /Link /Rect [10 140 90 160] /A << /S /URI /URI (https://example.org/kept) >> >>",
    );
    p.page(
        HELV,
        "BT /F1 12 Tf 20 150 Td (Visit here or SECRET page) Tj ET",
        &format!("/Annots [{over} 0 R {away} 0 R]"),
    );
    let o = run(
        p.finish(),
        &[],
        &[(0, Mark::rect(Rect::new(105.0, 140.0, 135.0, 160.0)))],
        &ApplyOptions::default(),
    );
    assert_redacted(&o, &["SECRETLINK"]);
    assert_eq!(o.report.pages[0].annotations_removed, 1);
    let page = o
        .after
        .get(pagebara_pages::PageTree::load(&o.after).pages()[0].r)
        .unwrap();
    let left = pagebara_annot::annotations(&o.after, page.as_dict().unwrap());
    assert_eq!(left.len(), 1);
}
