mod common;

use std::sync::Arc;

use common::*;
use pagebara_content::font::FontProgram;
use pagebara_content::{Interpreter, extract_text};

fn cid_font(encoding: &str, ordering: &str) -> String {
    format!(
        "<</Type/Font/Subtype/Type0/BaseFont/X/Encoding/{encoding}\
          /DescendantFonts[<</Type/Font/Subtype/CIDFontType0/BaseFont/X\
          /CIDSystemInfo<</Registry(Adobe)/Ordering({ordering})/Supplement 6>>/DW 1000>>]>>"
    )
}

fn text(content: &str, font: String) -> String {
    let (doc, l) = interpret(content, "<</Font<</F0 5 0 R>>>>", &[font]);
    extract_text(&doc, &l).text
}

#[test]
fn predefined_cjk_cmaps_without_to_unicode() {
    // Shift-JIS あい and an ASCII letter through 90ms-RKSJ-H, then Adobe-Japan1 to Unicode.
    let t = text(
        "BT /F0 10 Tf 0 100 Td <82A082A241> Tj ET",
        cid_font("90ms-RKSJ-H", "Japan1"),
    );
    assert_eq!(t, "あいA");
    // GB2312 啊 through GB-EUC-H.
    let t = text(
        "BT /F0 10 Tf 0 100 Td <B0A1> Tj ET",
        cid_font("GB-EUC-H", "GB1"),
    );
    assert_eq!(t, "啊");
    // Unicode-coded CMaps read their codes directly, UTF-8 included.
    let t = text(
        "BT /F0 10 Tf 0 100 Td <E697A5E69CAC> Tj ET",
        cid_font("UniJIS-UTF8-H", "Japan1"),
    );
    assert_eq!(t, "日本");
    let t = text(
        "BT /F0 10 Tf 0 100 Td <D840DC0B> Tj ET",
        cid_font("UniJIS-UTF16-H", "Japan1"),
    );
    assert_eq!(t, "\u{2000B}");
}

#[test]
fn identity_cids_of_a_known_collection() {
    // CID 843 of Adobe-Japan1 is あ; 940 of Adobe-GB1 is 啊.
    let t = text(
        "BT /F0 10 Tf 0 100 Td <034B> Tj ET",
        cid_font("Identity-H", "Japan1"),
    );
    assert_eq!(t, "あ");
    let t = text(
        "BT /F0 10 Tf 0 100 Td <03AC> Tj ET",
        cid_font("Identity-H", "GB1"),
    );
    assert_eq!(t, "啊");
    let t = text(
        "BT /F0 10 Tf 0 100 Td <034B> Tj ET",
        cid_font("Identity-H", "Identity"),
    );
    assert_eq!(t, "\u{FFFD}");
}

#[test]
fn vertical_predefined_cmap() {
    let (_, l) = interpret(
        "BT /F0 10 Tf 0 100 Td <8141> Tj ET",
        "<</Font<</F0 5 0 R>>>>",
        &[cid_font("90ms-RKSJ-V", "Japan1")],
    );
    let r = &l.runs[0];
    assert!(r.vertical);
    let font = &l.fonts[r.font as usize].font;
    assert_eq!(font.cid(0x8141), 7887);
}

#[test]
fn mac_expert_encoding() {
    let t = text(
        "BT /F0 10 Tf 0 100 Td (\\141\\142\\060\\126) Tj ET",
        "<</Type/Font/Subtype/Type1/BaseFont/X-Expert/Encoding/MacExpertEncoding>>".into(),
    );
    assert_eq!(t, "ab0ff");
}

/// A font program whose built-in encoding names codes 1–3.
#[derive(Debug)]
struct Program;

impl FontProgram for Program {
    fn glyph_name(&self, code: u32) -> Option<&[u8]> {
        match code {
            1 => Some(b"H"),
            2 => Some(b"uni00E9"),
            3 => Some(b"g42"),
            _ => None,
        }
    }

    fn glyph_id(&self, code: u32) -> Option<u32> {
        Some(code + 100)
    }

    fn unicode(&self, code: u32, out: &mut String) -> bool {
        if code == 3 {
            out.push('Z');
        }
        code == 3
    }
}

#[test]
fn font_program_hook() {
    let font = "<</Type/Font/Subtype/Type1/BaseFont/Embedded\
        /FirstChar 1/Widths[500 500 500 500]/FontDescriptor<</Flags 4/FontFile 9 0 R>>\
        /Encoding<</Differences[2/A]>>>>";
    let content = "BT /F0 10 Tf 0 100 Td <01020304> Tj ET";
    let doc = page_doc(content, "<</Font<</F0 5 0 R>>>>", &[font.into()], "");
    let (page, res) = page_and_resources(&doc);

    // Without a loader, only `/Differences` names a glyph (the default loader finds no
    // program in the missing `/FontFile`).
    for it in [
        Interpreter::new(&doc),
        Interpreter::new(&doc).without_font_programs(),
    ] {
        let l = { it }.page(&page, &res);
        assert_eq!(extract_text(&doc, &l).text, "\u{FFFD}A\u{FFFD}\u{FFFD}");
    }
    let l = Interpreter::new(&doc).page(&page, &res);
    assert_eq!(extract_text(&doc, &l).text, "\u{FFFD}A\u{FFFD}\u{FFFD}");

    // With one, the built-in encoding fills in under `/Differences`, then its Unicode.
    let mut it = Interpreter::new(&doc).with_font_programs(Arc::new(|_, d| {
        (d.name(b"BaseFont") == Some(b"Embedded"))
            .then(|| Arc::new(Program) as Arc<dyn FontProgram>)
    }));
    let l = it.page(&page, &res);
    assert_eq!(extract_text(&doc, &l).text, "HAZ\u{FFFD}");
    let font = &l.fonts[0].font;
    assert_eq!(font.glyph_name(3), Some(&b"g42"[..]));
    assert_eq!(font.glyph_id(4), Some(104));
}
