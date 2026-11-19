//! FDF and XFDF imported into a form with text, check box, radio, list and signature
//! fields; the filled document is saved.

#![no_main]

#[path = "common/mod.rs"]
mod common;

use std::sync::OnceLock;

use libfuzzer_sys::fuzz_target;
use pagebara_forms::Form;
use pagebara_syntax::Document;

fn form_pdf() -> &'static Vec<u8> {
    static PDF: OnceLock<Vec<u8>> = OnceLock::new();
    PDF.get_or_init(|| {
        let w = |name: &str, extra: &str, y: u32| {
            format!(
                "<</Type/Annot/Subtype/Widget/FT/{extra}/T({name})/Rect[10 {y} 90 {}]/P 3 0 R/F 4/DA(/Helv 10 Tf 0 g)>>",
                y + 15
            )
            .into_bytes()
        };
        common::classic(&[
            b"<</Type/Catalog/Pages 2 0 R/AcroForm<</Fields[5 0 R 6 0 R 7 0 R 8 0 R 9 0 R]/DA(/Helv 10 Tf 0 g)/DR<</Font<</Helv<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>>>>>>>>>".to_vec(),
            b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
            b"<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Annots[5 0 R 6 0 R 7 0 R 8 0 R 9 0 R]>>".to_vec(),
            common::stream("", b""),
            w("name", "Tx/V(Ann)/MaxLen 20", 10),
            w("agree", "Btn/V/Off/AS/Off/AP<</N<</Yes 4 0 R/Off 4 0 R>>>>", 40),
            w("size", "Ch/Ff 131072/Opt[(S)(M)(L)]/V(M)", 70),
            w("pick", "Ch/Opt[[(a)(Apple)][(b)(Banana)]]/Ff 2097152", 100),
            w("sig", "Sig", 130),
        ])
    })
}

fuzz_target!(|data: &[u8]| {
    let Ok(mut doc) = Document::open(form_pdf().clone()) else {
        return;
    };
    let mut form = Form::load(&doc);
    let imported = match std::str::from_utf8(data) {
        Ok(s) if s.trim_start().starts_with('<') => {
            pagebara_fdf::import_xfdf(&mut form, &mut doc, s).is_ok()
        }
        _ => pagebara_fdf::import_fdf(&mut form, &mut doc, data).is_ok(),
    };
    if imported {
        let _ = pagebara_fdf::export_xfdf(&form, &doc, None, true);
        if let Ok(out) = doc.save_incremental() {
            let _ = Document::open(out);
        }
    }
});
