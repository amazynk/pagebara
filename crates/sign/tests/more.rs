//! Field locks, encrypted documents, and a real signed file.

mod common;

use common::*;
use pagebara_forms::Form;
use pagebara_sign::*;
use pagebara_syntax::{Document, Permissions};

#[test]
fn locked_fields() {
    let pki = pki();
    let opts = SignOptions {
        lock_fields: Some(&["name"]),
        ..Default::default()
    };
    let doc = Document::open(sample_pdf()).unwrap();
    let signed = sign(&doc, &pki.rsa_signer, &opts).unwrap();
    let r = &verify_all(&signed, &trust())[0];
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    // Filling the locked field afterwards is not allowed. (The form refuses to fill a
    // read-only field, so edit the value directly, as another program might.)
    let filled = update(&signed, |doc| {
        let form = Form::load(doc);
        let id = form.field("name").unwrap().id;
        let mut d = doc.get(id).unwrap().as_dict().unwrap().clone();
        d.set(b"V", pagebara_syntax::Object::string(b"changed"));
        doc.set(id, d);
    });
    let r = &verify_all(&filled, &trust())[0];
    assert_eq!(r.status, Status::Invalid, "{r:#?}");
    assert!(
        r.disallowed_changes()
            .any(|c| c.description.contains("locked field \"name\"")),
        "{:#?}",
        r.changes
    );
}

#[test]
fn encrypted_document() {
    let pki = pki();
    let mut doc = Document::open(sample_pdf()).unwrap();
    doc.set_password("", "owner secret", Permissions::ALL)
        .unwrap();
    let protected = doc.save_full().unwrap();
    let doc = Document::open(protected).unwrap();
    assert!(doc.is_encrypted());
    let opts = SignOptions {
        reason: Some("Geprüft"),
        ..Default::default()
    };
    let signed = sign(&doc, &pki.ec_signer, &opts).unwrap();
    qpdf_check(&signed, "encrypted");
    let reports = verify_all(&signed, &trust());
    let r = &reports[0];
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    assert_eq!(r.info.reason.as_deref(), Some("Geprüft"));
    // A later form fill in the encrypted file is still an allowed change.
    let filled = update(&signed, |doc| {
        Form::load(doc).set_text(doc, "name", "Z").unwrap();
    });
    let r = &verify_all(&filled, &trust())[0];
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    if let Some(out) = pdfsig(&filled, "encrypted") {
        assert!(out.contains("Signature is Valid"), "{out}");
    }
}

#[test]
fn unsigned_document_has_no_reports() {
    let doc = Document::open(sample_pdf()).unwrap();
    assert!(verify(&doc, &VerifyOptions::new(&trust())).is_empty());
}

/// A real file signed in 2012 with a self-signed certificate (read only if present).
#[test]
fn real_signed_file() {
    let path = "/home/user/projects/pr570/PR570-OSM.pdf";
    let Ok(bytes) = std::fs::read(path) else {
        eprintln!("{path} not present; skipping");
        return;
    };
    let doc = Document::open(bytes).unwrap();
    let reports = verify(&doc, &VerifyOptions::new(&TrustStore::new()));
    assert_eq!(reports.len(), 1);
    let r = &reports[0];
    assert!(r.intact, "{r:#?}");
    assert_eq!(r.status, Status::ValidUntrusted, "{r:#?}");
    assert_eq!(r.trust, Trust::SelfSigned);
    assert_eq!(r.info.sub_filter.as_deref(), Some("adbe.pkcs7.detached"));
    assert_eq!(r.signer.as_ref().unwrap().display_name(), "ARTEKMEDIA");
    assert_eq!(r.signing_time.unwrap().to_iso(), "2012-05-30T01:37:27Z");
    assert!(r.changes.is_empty());
}
