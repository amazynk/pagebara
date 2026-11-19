#![allow(dead_code)]

use pagebara_syntax::{Dict, Document, Object};

/// A system font, or `None` (the test skips it) when not installed.
pub fn system(path: &str) -> Option<Vec<u8>> {
    let d = std::fs::read(path).ok();
    if d.is_none() {
        eprintln!("skipping: {path} not installed");
    }
    d
}

/// Public sample PDFs used for embedded fonts (read at run time, never committed).
pub const PDFS: &[&str] = &[
    "/home/user/projects/ols/LuaManualEn.pdf",
    "/home/user/projects/fileservice/dunt_api.pdf",
    "/home/user/projects/rubidium/rfs_12pg.pdf",
    "/home/user/projects/rubidium/Model FE-5680A Rubidium Atomic Frequency Standards from Frequency Electronics, Inc_.pdf",
    "/home/user/projects/pr570/PR570-OSM.pdf",
];

pub fn open(path: &str) -> Option<Document> {
    let d = std::fs::read(path).ok()?;
    Document::open(d).ok()
}

/// Every font dictionary of a document.
pub fn font_dicts(doc: &Document) -> Vec<Dict> {
    let mut out = Vec::new();
    for n in doc.object_numbers() {
        let Some(r) = doc.reference(n) else { continue };
        let Some(o) = doc.get(r) else { continue };
        if let Object::Dict(d) = &*o
            && d.name(b"Type") == Some(b"Font")
            && matches!(
                d.name(b"Subtype"),
                Some(b"Type1" | b"TrueType" | b"Type0" | b"MMType1")
            )
        {
            out.push(d.clone());
        }
    }
    out
}

/// The raw embedded font programs of all public PDFs found.
pub fn embedded_programs() -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for p in PDFS {
        let Some(doc) = open(p) else { continue };
        for n in doc.object_numbers() {
            let Some(r) = doc.reference(n) else { continue };
            let Some(o) = doc.get(r) else { continue };
            let Object::Dict(d) = &*o else { continue };
            if d.name(b"Type") != Some(b"FontDescriptor") {
                continue;
            }
            for key in [&b"FontFile"[..], b"FontFile2", b"FontFile3"] {
                if let Some(s) = doc.get_in(d, key).as_stream()
                    && let Ok(dec) = doc.decode(s)
                {
                    out.push((format!("{p}#{n}"), dec.data));
                }
            }
        }
    }
    out
}

/// Deterministic pseudo-random numbers.
pub struct Lcg(pub u64);

impl Lcg {
    pub fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}
