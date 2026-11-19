//! Object streams: the input is the decoded content of an object stream (object numbers
//! and offsets, then the objects) holding the catalog, page tree and page of the file,
//! listed by a cross-reference stream. Every object is resolved and the file saved.

#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_syntax::{Document, Object, SaveOptions};

fn file(objstm: &[u8], n: u8, first: u16) -> Vec<u8> {
    let mut out = b"%PDF-1.5\n".to_vec();
    let at = out.len();
    out.extend_from_slice(
        format!(
            "4 0 obj\n<</Type/ObjStm/N {n}/First {first}/Length {}>>stream\n",
            objstm.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(objstm);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    let x = out.len();
    // W [1 2 1]: objects 1–3 in stream 4, then the stream and this xref stream.
    let mut rows = vec![0u8, 0, 0, 255];
    for i in 0..3 {
        rows.extend([2, 0, 4, i]);
    }
    rows.extend([1, (at >> 8) as u8, at as u8, 0]);
    rows.extend([1, (x >> 8) as u8, x as u8, 0]);
    out.extend_from_slice(
        format!(
            "5 0 obj\n<</Type/XRef/Size 6/W[1 2 1]/Root 1 0 R/Length {}>>stream\n",
            rows.len()
        )
        .as_bytes(),
    );
    out.extend_from_slice(&rows);
    out.extend_from_slice(format!("\nendstream\nendobj\nstartxref\n{x}\n%%EOF\n").as_bytes());
    out
}

fuzz_target!(|data: &[u8]| {
    let Some(([n, f0, f1], body)) = data.split_first_chunk::<3>().map(|(h, b)| (*h, b)) else {
        return;
    };
    let Ok(doc) = Document::open(file(body, n, u16::from_le_bytes([f0, f1]))) else {
        return;
    };
    for num in doc.object_numbers() {
        if let Some(o) = doc.reference(num).and_then(|r| doc.get(r))
            && let Object::Stream(s) = &*o
        {
            let _ = doc.decode(s);
        }
    }
    let _ = pagebara_pages::PageTree::load(&doc).len();
    if let Ok(out) = doc.save_full_with(&SaveOptions::compact()) {
        let _ = Document::open(out);
    }
});
