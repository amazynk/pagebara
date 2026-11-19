//! Opens PDFs, loads every object and decodes every stream; prints structure counts only.

use std::time::Instant;

use pagebara_syntax::{Document, ObjRef, Object};

fn main() {
    for path in std::env::args().skip(1) {
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{path}: {e}");
                continue;
            }
        };
        let t = Instant::now();
        let doc = match Document::open(bytes) {
            Ok(d) => d,
            Err(e) => {
                println!("{path}: open failed: {e}");
                continue;
            }
        };
        let opened = t.elapsed();
        let (mut objs, mut streams, mut decoded, mut images, mut failed, mut bytes_out) =
            (0, 0, 0, 0, 0, 0usize);
        for n in doc.object_numbers() {
            let Some(o) = doc.reference(n).and_then(|r| doc.get(r)) else {
                continue;
            };
            objs += 1;
            if let Object::Stream(s) = &*o {
                streams += 1;
                match doc.decode(s) {
                    Ok(d) if d.image.is_some() => images += 1,
                    Ok(d) => {
                        decoded += 1;
                        bytes_out += d.data.len();
                    }
                    Err(_) => failed += 1,
                }
            }
        }
        let root = doc
            .trailer()
            .get(b"Root")
            .and_then(Object::as_objref)
            .unwrap_or(ObjRef::new(0, 0));
        println!(
            "{path}: v{:?} repaired={} encrypted={} root={root:?} objects={objs} streams={streams} \
             decoded={decoded} ({bytes_out} B) image={images} failed={failed} open={opened:?} total={:?}",
            doc.version(),
            doc.was_repaired(),
            doc.is_encrypted(),
            t.elapsed()
        );
    }
}
