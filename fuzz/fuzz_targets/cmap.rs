//! CMaps and ToUnicode maps: parsed, then used to decode the input as text codes.

#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_content::font::cmap::CMap;

fuzz_target!(|data: &[u8]| {
    let mut m = CMap::parse(data);
    if let Some(parent) = CMap::predefined(b"UniJIS-UCS2-H") {
        m.inherit(&parent);
    }
    let _ = m.is_identity();
    let _ = m.has_codespace();
    let mut rest = &data[..data.len().min(256)];
    let mut text = String::new();
    while !rest.is_empty() {
        let (code, n) = m.next_code(rest);
        let _ = m.cid(code);
        m.unicode(code, &mut text);
        rest = &rest[n.clamp(1, rest.len())..];
    }
});
