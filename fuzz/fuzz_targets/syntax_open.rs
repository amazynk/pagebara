//! Open a file, resolve every object and decode every stream, then save it.

#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_syntax::{Document, Error, Object, SaveOptions};

fuzz_target!(|data: &[u8]| {
    let doc = match Document::open(data.to_vec()) {
        Ok(d) => d,
        Err(Error::PasswordRequired(l)) => match l.unlock("") {
            Ok(d) => d,
            Err(_) => return,
        },
        Err(_) => return,
    };
    let _ = doc.version();
    let _ = doc.catalog();
    for num in doc.object_numbers() {
        let Some(r) = doc.reference(num) else { continue };
        let Some(o) = doc.get(r) else { continue };
        if let Object::Stream(s) = &*o {
            let _ = doc.decode(s);
        }
    }
    let _ = doc.save_incremental();
    let _ = doc.save_full_with(&SaveOptions::compact());
});
