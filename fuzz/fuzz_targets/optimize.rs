//! Lossless optimization of a whole file; the output must open with the same page count.

#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_pages::PageTree;
use pagebara_syntax::Document;

fuzz_target!(|data: &[u8]| {
    let Ok(doc) = Document::open(data.to_vec()) else {
        return;
    };
    let pages = PageTree::load(&doc).len();
    let Ok(out) = pagebara_optimize::optimize(&doc, &pagebara_optimize::Options::default(), None) else {
        return;
    };
    let back = Document::open(out.bytes).expect("optimized output opens");
    assert_eq!(PageTree::load(&back).len(), pages, "page count changed");
});
