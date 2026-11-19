//! Damaged files: truncations and bit flips of the fixtures must open (or fail cleanly),
//! resolve, decode and save without panicking or hanging.

use std::time::{Duration, Instant};

use pagebara_syntax::{Document, Error, Object, SaveOptions};

/// Wall-clock budget per fixture and mutation kind; the loops stop early when it runs out.
const BUDGET: Duration = Duration::from_millis(400);
/// One mutated file taking longer than this is a hang.
const SLOW: Duration = Duration::from_secs(2);

fn fixtures() -> Vec<(String, Vec<u8>)> {
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/crypt");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "pdf"))
        .map(|p| {
            let name = p.file_stem().unwrap().to_string_lossy().into_owned();
            (name, std::fs::read(p).unwrap())
        })
        .collect();
    v.sort();
    // The same files written with object streams and an xref stream.
    let compact: Vec<_> = v
        .iter()
        .filter_map(|(name, bytes)| {
            let doc = open(bytes.clone())?;
            let out = doc.save_full_with(&SaveOptions::compact()).ok()?;
            Some((format!("{name}-compact"), out))
        })
        .collect();
    v.extend(compact);
    v
}

fn open(bytes: Vec<u8>) -> Option<Document> {
    match Document::open(bytes) {
        Ok(d) => Some(d),
        Err(Error::PasswordRequired(l)) => l.unlock("user").ok(),
        Err(_) => None,
    }
}

fn exercise(bytes: Vec<u8>) {
    let Some(doc) = open(bytes) else { return };
    let _ = doc.catalog();
    for num in doc.object_numbers() {
        if let Some(o) = doc.reference(num).and_then(|r| doc.get(r))
            && let Object::Stream(s) = &*o
        {
            let _ = doc.decode(s);
        }
    }
    let _ = doc.save_incremental();
    if let Ok(out) = doc.save_full_with(&SaveOptions::compact()) {
        let re = Document::open(out);
        assert!(
            !matches!(re, Err(Error::NotPdf)),
            "a saved file must open again"
        );
    }
}

fn run(name: &str, what: &str, cases: impl Iterator<Item = Vec<u8>>) {
    let start = Instant::now();
    for (i, bytes) in cases.enumerate() {
        if start.elapsed() > BUDGET {
            eprintln!("{name} {what}: budget spent after {i} cases");
            break;
        }
        let t = Instant::now();
        exercise(bytes);
        assert!(
            t.elapsed() < SLOW,
            "{name} {what} case {i} took {:?}",
            t.elapsed()
        );
    }
}

#[test]
fn truncations() {
    for (name, f) in fixtures() {
        let step = (f.len() / 150).max(1);
        let lens = (0..f.len())
            .step_by(step)
            .chain(f.len().saturating_sub(64)..f.len());
        run(&name, "truncation", lens.map(|n| f[..n].to_vec()));
    }
}

#[test]
fn bit_flips() {
    for (name, f) in fixtures() {
        let mut x = 0x2545_f491_u32 ^ f.len() as u32;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as usize
        };
        let cases = (0..150).map(|_| {
            let mut g = f.clone();
            for _ in 0..1 + next() % 4 {
                let at = next() % g.len();
                g[at] ^= 1 << (next() % 8);
            }
            g
        });
        run(&name, "bit flip", cases);
    }
}

#[test]
fn byte_overwrites() {
    // Digits and delimiters turned into others: offsets, lengths and structure change.
    const BYTES: &[u8] = b"0 9<>[]/()R\nobj";
    for (name, f) in fixtures() {
        let mut x = 0x9e37_79b9_u32 ^ f.len() as u32;
        let mut next = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as usize
        };
        let cases = (0..100).map(|_| {
            let mut g = f.clone();
            let at = next() % g.len();
            g[at] = BYTES[next() % BYTES.len()];
            g
        });
        run(&name, "overwrite", cases);
    }
}
