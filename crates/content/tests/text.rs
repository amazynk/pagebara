mod common;

use common::*;
use pagebara_content::display::{Cmd, TextMode};
use pagebara_content::{Matrix, Point, extract_text};

const F1: &str = "<</Type/Font/Subtype/Type1/BaseFont/Custom/FirstChar 32/LastChar 66\
    /Widths[250 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 500 600]\
    /FontDescriptor<</Flags 32/Ascent 700/Descent -300/MissingWidth 111/FontFile 9 0 R>>>>";

fn close(a: f32, b: f32) -> bool {
    (a - b).abs() < 1e-4
}

fn glyph_xs(l: &pagebara_content::DisplayList, run: usize) -> Vec<f32> {
    l.run_glyphs(&l.runs[run]).iter().map(|g| g.x).collect()
}

#[test]
fn positioning_and_spacing() {
    let (_, l) = interpret(
        "BT /F1 10 Tf 2 Tc 100 100 Td (AB) Tj [(A) -1000 (B)] TJ ET",
        "<</Font<</F1 5 0 R>>>>",
        &[F1.to_string()],
    );
    assert_eq!(l.runs.len(), 2);
    assert_eq!(glyph_xs(&l, 0), [0.0, 7.0]);
    assert_eq!(l.runs[0].matrix, Matrix::translate(100.0, 100.0));
    // After (AB): 5 + 2 + 6 + 2.
    assert_eq!(l.runs[1].matrix, Matrix::translate(115.0, 100.0));
    assert_eq!(glyph_xs(&l, 1), [0.0, 17.0]);
    assert_eq!(l.run_glyphs(&l.runs[1])[1].width, 6.0);
    assert_eq!(l.runs[0].size, 10.0);
}

#[test]
fn word_spacing_scaling_leading_and_quotes() {
    let (_, l) = interpret(
        "BT /F1 10 Tf 3 Tw 50 Tz 12 TL 0 100 Td (A A) Tj T* (B) Tj (B) ' 1 2 (A) \" ET",
        "<</Font<</F1 5 0 R>>>>",
        &[F1.to_string()],
    );
    // Tz 50: advances halve; Tw applies to the space.
    assert_eq!(glyph_xs(&l, 0), [0.0, 2.5, 5.25]);
    assert_eq!(l.runs[0].hscale, 0.5);
    assert_eq!(l.runs[1].matrix, Matrix::translate(0.0, 88.0));
    assert_eq!(l.runs[2].matrix, Matrix::translate(0.0, 76.0));
    assert_eq!(l.runs[3].matrix, Matrix::translate(0.0, 64.0));
    // `"` set Tw 1 and Tc 2 for good.
    let (_, l2) = interpret(
        "BT /F1 10 Tf 1 2 (A) \" (A A) Tj ET",
        "<</Font<</F1 5 0 R>>>>",
        &[F1.to_string()],
    );
    assert_eq!(glyph_xs(&l2, 1), [0.0, 7.0, 7.0 + 2.5 + 2.0 + 1.0]);
}

#[test]
fn text_matrix_ctm_rise_and_modes() {
    let (_, l) = interpret(
        "2 0 0 2 0 0 cm BT /F1 10 Tf 0 1 -1 0 50 60 Tm 3 Ts 7 Tr (A) Tj 2 Tr (C) Tj ET",
        "<</Font<</F1 5 0 R>>>>",
        &[F1.to_string()],
    );
    let r = &l.runs[0];
    assert_eq!(r.matrix, Matrix::new(0.0, 2.0, -2.0, 0.0, 100.0, 120.0));
    assert_eq!((r.rise, r.mode), (3.0, TextMode::Clip));
    let g = l.run_glyphs(r)[0];
    assert_eq!(
        r.glyph_matrix(&g).apply(Point::new(0.0, 0.0)),
        Point::new(94.0, 120.0)
    );
    assert_eq!(l.runs[1].mode, TextMode::FillStroke);
    // MissingWidth for codes outside /Widths.
    assert!(close(l.run_glyphs(&l.runs[1])[0].width, 1.11));
    assert_eq!(kinds(&l), ["text", "text", "clip-text", "pop-clip"]);
    assert!(matches!(l.cmds[2], Cmd::ClipText { start: 0, end: 2 }));
}

#[test]
fn standard_fonts_without_widths() {
    let (_, l) = interpret(
        "BT /H 10 Tf (AV) Tj /C 10 Tf (iW) Tj /S 10 Tf (a) Tj ET",
        "<</Font<</H<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>\
          /C<</Type/Font/Subtype/Type1/BaseFont/Courier-Bold>>/S<</Subtype/Type1/BaseFont/Symbol>>>>>>",
        &[],
    );
    assert!(close(glyph_xs(&l, 0)[1], 6.67));
    assert!(close(glyph_xs(&l, 1)[1], 6.0));
    assert!(close(l.run_glyphs(&l.runs[2])[0].width, 6.31));
}

const TO_UNICODE: &str = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap
1 begincodespacerange <0000> <FFFF> endcodespacerange
3 beginbfchar <0001> <0048> <0002> <0069> <0003> <0020> endbfchar
1 beginbfrange <0010> <0012> <0061> endbfrange
endcmap CMapName currentdict /CMap defineresource pop end end";

fn type0(encoding: &str, w: &str) -> String {
    format!(
        "<</Type/Font/Subtype/Type0/BaseFont/X/Encoding {encoding}/ToUnicode 6 0 R\
          /DescendantFonts[<</Type/Font/Subtype/CIDFontType2/DW 1000/W {w}\
          /FontDescriptor<</Ascent 800/Descent -200/Flags 4>>>>]>>"
    )
}

#[test]
fn composite_fonts_and_to_unicode() {
    let (doc, l) = interpret(
        "BT /F0 10 Tf 0 100 Td <0001000200030010> Tj 0 -20 Td <00110012> Tj ET",
        "<</Font<</F0 5 0 R>>>>",
        &[
            type0("/Identity-H", "[1 [500 250] 16 18 600]"),
            stream("", TO_UNICODE),
        ],
    );
    let codes: Vec<u32> = l.run_glyphs(&l.runs[0]).iter().map(|g| g.code).collect();
    assert_eq!(codes, [1, 2, 3, 16]);
    assert_eq!(glyph_xs(&l, 0), [0.0, 5.0, 7.5, 17.5]);
    let t = extract_text(&doc, &l);
    assert_eq!(t.text, "Hi a\nbc");
    assert_eq!(t.lines.len(), 2);
    let c = &t.chars[0];
    assert_eq!(t.char_text(c), "H");
    let b = c.bbox();
    assert_eq!((b.x0, b.y0, b.x1, b.y1), (0.0, 98.0, 5.0, 108.0));
    assert_eq!(t.char_at(1).map(|c| t.char_text(c)), Some("i"));
    assert_eq!(t.quads(5..7).len(), 2);
}

#[test]
fn embedded_cmap_and_vertical_writing() {
    let cmap = "/CIDInit /ProcSet findresource begin 12 dict begin begincmap /WMode 1 def
2 begincodespacerange <00> <7F> <8000> <FFFF> endcodespacerange
1 begincidrange <41> <43> 10 endcidrange 1 begincidchar <8001> 20 endcidchar endcmap";
    let (_, l) = interpret(
        "BT /F0 10 Tf (A\\200\\001C) Tj ET",
        "<</Font<</F0 5 0 R>>>>",
        &[
            "<</Subtype/Type0/Encoding 7 0 R/DescendantFonts[<</Subtype/CIDFontType0/DW 1000\
              /W[10[400]20[900]]/W2[20[-500 450 880]]/DW2[880 -1000]>>]/ToUnicode 6 0 R>>"
                .to_string(),
            stream("", TO_UNICODE),
            stream("/Type/CMap", cmap),
        ],
    );
    let r = &l.runs[0];
    assert!(r.vertical);
    let g = l.run_glyphs(r);
    assert_eq!(
        g.iter().map(|g| g.code).collect::<Vec<_>>(),
        [0x41, 0x8001, 0x43]
    );
    // Default vertical metrics: v = (w0 / 2, 880), w1 = -1000.
    assert_eq!((g[0].x, g[0].y), (-2.0, -8.8));
    assert_eq!((g[1].x, g[1].y), (-4.5, -10.0 - 8.8));
    assert_eq!(g[2].y, -15.0 - 8.8);
}

#[test]
fn simple_encodings_and_differences() {
    let (doc, l) = interpret(
        "BT /W 10 Tf (\\200\\221Caf\\351) Tj /D 10 Tf (\\001\\002\\003) Tj /M 10 Tf (\\216) Tj ET",
        "<</Font<</W<</Subtype/TrueType/BaseFont/Arial/Encoding/WinAnsiEncoding>>\
          /D<</Subtype/Type1/BaseFont/Times-Roman/Encoding<</Differences[1/fi/uni20AC/g33]>>>>\
          /M<</Subtype/Type1/BaseFont/Helvetica/Encoding/MacRomanEncoding>>>>>>",
        &[],
    );
    let t = extract_text(&doc, &l);
    assert_eq!(t.text, "€‘Caféfi€\u{FFFD}é");
    assert_eq!(
        t.chars
            .iter()
            .filter(|c| c.glyph == pagebara_content::display::NONE)
            .count(),
        0
    );
}

#[test]
fn reading_order_and_actual_text() {
    let (doc, l) = interpret(
        "BT /F1 10 Tf 0 50 Td (B) Tj ET BT /F1 10 Tf 0 100 Td (A) Tj 30 0 Td (A) Tj ET \
         /Span <</ActualText <FEFF00660069>>> BDC BT /F1 10 Tf 0 20 Td (AB) Tj ET EMC \
         BT /F1 10 Tf 2 Tr 0 50 Td 5 0 Td (A) Tj 5.3 0 Td (A) Tj 5.3 0 Td (A) Tj ET",
        "<</Font<</F1 5 0 R>>>>",
        &[F1.to_string()],
    );
    let t = extract_text(&doc, &l);
    // Overlapping glyphs and small gaps read without spaces; lines far apart are paragraphs.
    assert_eq!(t.text, "A A\n\nBAAA\n\nfi");
    let last = t.lines.last().unwrap();
    assert_eq!(last.chars.1 - last.chars.0, 2);
    assert_eq!(t.char_text(&t.chars[t.chars.len() - 1]), "");
}

#[test]
fn text_per_marked_content_id() {
    // MCID 1 is drawn first, lower on the page; nested sequences belong to the innermost
    // MCID; unmarked text is left out.
    let (doc, l) = interpret(
        "/P <</MCID 1>> BDC BT /F1 10 Tf 0 50 Td (B) Tj ET EMC \
         /P <</MCID 0>> BDC BT /F1 10 Tf 0 100 Td (A) Tj 30 0 Td (A) Tj ET \
         /Span <</MCID 2>> BDC /Em BMC BT /F1 10 Tf 0 80 Td (AB) Tj ET EMC EMC EMC \
         BT /F1 10 Tf 0 20 Td (BB) Tj ET",
        "<</Font<</F1 5 0 R>>>>",
        &[F1.to_string()],
    );
    let m = pagebara_content::extract_marked(&doc, &l);
    let texts: Vec<(i64, &str)> = m.iter().map(|(k, t)| (*k, t.text.as_str())).collect();
    assert_eq!(texts, [(0, "A A"), (1, "B"), (2, "AB")]);
    assert_eq!(m[&2].chars.len(), 2);
    assert!(m[&2].chars[0].quad[0].y > m[&1].chars[0].quad[0].y);
}

#[test]
fn drawing_bounds_per_marked_content_id() {
    // A filled square in MCID 3, a stroke nested in MCID 4 inside an unmarked sequence, text
    // (which has no drawing bounds) in MCID 5 and an unmarked fill.
    let (_, l) = interpret(
        "/Figure <</MCID 3>> BDC q 2 0 0 2 0 0 cm 10 10 20 20 re f Q EMC \
         /Em BMC /P <</MCID 4>> BDC 100 100 m 150 120 l S EMC EMC \
         /P <</MCID 5>> BDC BT /F1 10 Tf 0 50 Td (B) Tj ET EMC 0 0 5 5 re f",
        "<</Font<</F1 5 0 R>>>>",
        &[F1.to_string()],
    );
    let b = pagebara_content::marked_bounds(&l);
    assert_eq!(b.keys().copied().collect::<Vec<_>>(), [3, 4]);
    let r = b[&3];
    assert_eq!((r.x0, r.y0, r.x1, r.y1), (20.0, 20.0, 60.0, 60.0));
    let r = b[&4];
    assert_eq!((r.x0, r.y0, r.x1, r.y1), (100.0, 100.0, 150.0, 120.0));
}

#[test]
fn hidden_and_invisible_text() {
    let doc = page_doc(
        "BT /F1 10 Tf 3 Tr (A) Tj ET /OC /off BDC BT /F1 10 Tf (B) Tj ET EMC",
        "<</Font<</F1 5 0 R>>/Properties<</off 6 0 R>>>>",
        &[F1.to_string(), "<</Type/OCG/Name(x)>>".to_string()],
        "/OCProperties<</OCGs[6 0 R]/D<</BaseState/OFF>>>>",
    );
    let l = run(&doc);
    assert_eq!(l.runs.len(), 1);
    assert_eq!(l.runs[0].mode, TextMode::Invisible);
    assert_eq!(extract_text(&doc, &l).text, "A");
}

#[test]
fn font_from_ext_gstate_and_missing_fonts() {
    let (_, l) = interpret(
        "BT /G gs (A) Tj /Nope 10 Tf (A) Tj ET",
        "<</ExtGState<</G<</Font[5 0 R 20]>>>>>>",
        &[F1.to_string()],
    );
    assert_eq!(l.runs.len(), 1);
    assert_eq!(l.runs[0].size, 20.0);
}
