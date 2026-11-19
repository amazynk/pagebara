//! Signature fields: signed or not, what the dictionary says, and byte-range coverage
//! before and after an incremental update. (Nothing is verified cryptographically.)

mod common;

use common::*;
use pagebara_forms::*;
use pagebara_syntax::{Document, Object, PdfString};

const RANGE_PLACEHOLDER: &str = "[0 1111111111 2222222222 3333333333]";

/// A sample form whose "sig" field carries a (fake) signature over the whole file.
fn signed() -> Vec<u8> {
    let mut doc = sample_form("/SigFlags 3");
    let form = Form::load(&doc);
    let sig = form.field("sig").unwrap().id;
    let mut der = vec![
        0x30, 0x0C, 0x06, 0x03, 0x55, 0x04, 0x03, 0x0C, 0x05, b'B', b'o', b'b', b' ', b'Q',
    ];
    der.resize(64, 0);
    let mut v = o(&format!(
        "<</Type/Sig/Filter/Adobe.PPKLite/SubFilter/adbe.pkcs7.detached/Name(Bob Q)\
         /M(D:20240305143000+01'00')/Reason(Approval)/Location(Berlin)/ByteRange{RANGE_PLACEHOLDER}\
         /Reference[<</Type/SigRef/TransformMethod/DocMDP/TransformParams<</P 2/V/1.2>>>>]>>"
    ))
    .as_dict()
    .unwrap()
    .clone();
    v.set(
        b"Contents",
        Object::String(PdfString {
            bytes: der,
            hex: true,
        }),
    );
    let vr = doc.add(v);
    edit(&mut doc, sig, |d| d.set(b"V", vr));
    let mut bytes = doc.save_full().unwrap();
    // Patch the byte range around the /Contents string, as a signer does.
    let find = |b: &[u8], n: &[u8]| b.windows(n.len()).position(|w| w == n).unwrap();
    let at = find(&bytes, b"/Contents <");
    let a = at + 9 + bytes[at + 9..].iter().position(|&c| c == b'<').unwrap();
    let c = a + bytes[a..].iter().position(|&c| c == b'>').unwrap() + 1;
    let len = bytes.len();
    let range = format!("[0 {a:010} {c:010} {:010}]", len - c);
    assert_eq!(range.len(), RANGE_PLACEHOLDER.len());
    let p = find(&bytes, RANGE_PLACEHOLDER.as_bytes());
    bytes[p..p + range.len()].copy_from_slice(range.as_bytes());
    bytes
}

#[test]
fn signed_field() {
    let bytes = signed();
    qpdf_check(&bytes, "sig-signed");
    let mut doc = Document::open(bytes).unwrap();
    let mut form = Form::load(&doc);
    assert_eq!(form.sig_flags, 3);
    let sigs = form.signatures(&doc);
    assert_eq!(sigs.len(), 1);
    let s = &sigs[0];
    assert!(s.signed);
    assert_eq!(s.field, "sig");
    assert_eq!(s.page, Some(0));
    assert_eq!(s.sub_filter.as_deref(), Some("adbe.pkcs7.detached"));
    assert_eq!(s.signer.as_deref(), Some("Bob Q"));
    assert_eq!(s.certificate_names, ["Bob Q"]);
    assert_eq!(
        s.signing_time.unwrap().to_iso(),
        "2024-03-05T14:30:00+01:00"
    );
    assert_eq!(s.reason.as_deref(), Some("Approval"));
    assert_eq!(s.location.as_deref(), Some("Berlin"));
    assert_eq!(s.doc_mdp, Some(2));
    let br = s.byte_range.clone().unwrap();
    assert!(
        br.well_formed && br.covers_revision && br.covers_whole_file,
        "{br:?}"
    );
    assert_eq!(br.later_revisions, 0);

    // Filling afterwards appends a revision: the signature still covers its own.
    form.set_text(&mut doc, "name", "after signing").unwrap();
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, "sig-after");
    let doc = Document::open(bytes).unwrap();
    let form = Form::load(&doc);
    let br = form.signatures(&doc)[0].byte_range.clone().unwrap();
    assert!(br.well_formed && br.covers_revision, "{br:?}");
    assert!(!br.covers_whole_file);
    assert_eq!(br.later_revisions, 1);
}

#[test]
fn unsigned_and_tampered() {
    let doc = sample_form("");
    let form = Form::load(&doc);
    let s = &form.signatures(&doc)[0];
    assert!(!s.signed && s.byte_range.is_none());

    // A byte range that does not frame /Contents is reported as malformed.
    let mut bytes = signed();
    let at = bytes
        .windows(14)
        .position(|w| w == b"/ByteRange [0 ")
        .unwrap();
    // The last digit of the first range's length.
    let d = &mut bytes[at + 14 + 9];
    *d = if *d == b'0' { b'1' } else { *d - 1 };
    let doc = Document::open(bytes).unwrap();
    let form = Form::load(&doc);
    let br = form.signatures(&doc)[0].byte_range.clone().unwrap();
    assert!(!br.well_formed);
}
