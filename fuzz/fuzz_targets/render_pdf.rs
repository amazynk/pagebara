//! A whole file as the viewer sees it: open, render the first pages small, extract their
//! text, read annotations and form fields.

#![no_main]

#[path = "common/mod.rs"]
mod common;

use libfuzzer_sys::fuzz_target;
use pagebara_content::{Interpreter, extract_text};
use pagebara_pages::PageTree;
use pagebara_syntax::Document;

fuzz_target!(|data: &[u8]| {
    let Ok(doc) = Document::open(data.to_vec()) else {
        return;
    };
    common::render(&doc, 3, 48.0);
    let tree = PageTree::open(&doc);
    let mut interp = Interpreter::new(&doc);
    for p in tree.pages().iter().take(3) {
        let Some(o) = doc.get(p.r) else { continue };
        let Some(d) = o.as_dict() else { continue };
        let res = doc
            .get_in(d, b"Resources")
            .as_dict()
            .cloned()
            .unwrap_or_default();
        let _ = extract_text(&doc, &interp.page(d, &res));
        let _ = pagebara_annot::annotations(&doc, d);
    }
    let _ = pagebara_forms::Form::load(&doc);
});
