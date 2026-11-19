//! Password unlocking: the first byte is the password length, then the password, then the
//! file (seed with encrypted files).

#![no_main]

use libfuzzer_sys::fuzz_target;
use pagebara_syntax::{Document, Error, Object};

fuzz_target!(|data: &[u8]| {
    let Some((&n, rest)) = data.split_first() else {
        return;
    };
    let n = usize::from(n).min(rest.len());
    let (pw, file) = rest.split_at(n);
    let doc = match Document::open(file.to_vec()) {
        Ok(d) => d,
        Err(Error::PasswordRequired(l)) => {
            let l = match l.unlock_bytes(pw) {
                Ok(d) => return check(d, pw),
                Err(l) => l,
            };
            match l.unlock(&String::from_utf8_lossy(pw)) {
                Ok(d) => d,
                Err(_) => return,
            }
        }
        Err(_) => return,
    };
    check(doc, pw);
});

fn check(mut doc: Document, pw: &[u8]) {
    let _ = doc.unlock_owner(&String::from_utf8_lossy(pw));
    let _ = doc.permissions();
    for num in doc.object_numbers().into_iter().take(64) {
        if let Some(o) = doc.reference(num).and_then(|r| doc.get(r))
            && let Object::Stream(s) = &*o
        {
            let _ = doc.decode(s);
        }
    }
}
