//! Signing and verifying: integrity, trust, changes after signing, certification,
//! timestamps, LTV, and cross-checks with poppler's pdfsig, qpdf and OpenSSL.

mod common;

use common::*;
use pagebara_forms::Form;
use pagebara_sign::*;
use pagebara_syntax::{Document, Object, Stream};

fn signed_with(id: &Identity, opts: &SignOptions<'_>) -> Vec<u8> {
    let doc = Document::open(sample_pdf()).unwrap();
    sign(&doc, id, opts).unwrap()
}

fn one(reports: &[SignatureReport]) -> &SignatureReport {
    assert_eq!(reports.len(), 1, "{reports:#?}");
    &reports[0]
}

#[test]
fn rsa_cades_valid_and_trusted() {
    let pki = pki();
    let opts = SignOptions {
        reason: Some("Agreed"),
        location: Some("Köln"),
        ..Default::default()
    };
    let bytes = signed_with(&pki.rsa_signer, &opts);
    qpdf_check(&bytes, "rsa-cades");
    let reports = verify_all(&bytes, &trust());
    let r = one(&reports);
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    assert!(r.intact && r.problems.is_empty(), "{r:#?}");
    assert_eq!(r.kind, SignatureKind::Approval);
    assert_eq!(r.info.field, "approval", "the empty field is signed first");
    assert_eq!(r.info.sub_filter.as_deref(), Some("ETSI.CAdES.detached"));
    assert_eq!(r.info.reason.as_deref(), Some("Agreed"));
    assert_eq!(r.info.location.as_deref(), Some("Köln"));
    assert_eq!(r.signer.as_ref().unwrap().display_name(), "Anna Müller");
    assert_eq!(r.chain.len(), 2);
    assert_eq!(r.algorithm.as_deref(), Some("RSA PKCS#1 v1.5 with SHA-256"));
    assert_eq!(r.time_source, TimeSource::SignerClock);
    assert_eq!(r.revision, 2);
    assert!(r.changes.is_empty());
    assert!(r.summary().contains("valid"), "{}", r.summary());

    // Without the root among the anchors the signature is intact but untrusted.
    let r = &verify_all(&bytes, &TrustStore::new())[0];
    assert_eq!(r.status, Status::ValidUntrusted);
    assert!(
        matches!(r.trust, Trust::UntrustedRoot { .. }),
        "{:?}",
        r.trust
    );

    if let Some(out) = pdfsig(&bytes, "rsa-cades") {
        assert!(out.contains("Signature is Valid"), "{out}");
        assert!(out.contains("ETSI.CAdES.detached"), "{out}");
    }
}

#[test]
fn p256_pkcs7_visible_appearance() {
    let pki = pki();
    let appearance = Appearance {
        page: 0,
        rect: [72.0, 200.0, 320.0, 260.0],
        image: None,
        text: true,
    };
    let opts = SignOptions {
        field: Some("Sign here"),
        appearance: Some(&appearance),
        sub_filter: SubFilter::Pkcs7Detached,
        reason: Some("Approval (final)"),
        ..Default::default()
    };
    let bytes = signed_with(&pki.ec_signer, &opts);
    qpdf_check(&bytes, "p256-pkcs7");
    let r = verify_all(&bytes, &trust())
        .into_iter()
        .find(|r| r.info.field == "Sign here")
        .unwrap();
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    assert_eq!(r.algorithm.as_deref(), Some("ECDSA with SHA-256"));
    assert_eq!(r.info.page, Some(0));
    // The widget has a drawn appearance with the signer's name.
    let doc = Document::open(bytes.clone()).unwrap();
    let form = Form::load(&doc);
    let w = &form.field("Sign here").unwrap().widgets[0];
    assert!(w.has_appearance);
    let wd = doc.get(w.id).unwrap();
    let ap = doc.get_in(wd.as_dict().unwrap(), b"AP");
    let n = doc.get_in(ap.as_dict().unwrap(), b"N");
    let text = doc.decode(n.as_stream().unwrap()).unwrap().data;
    assert!(String::from_utf8_lossy(&text).contains("Ben Ng"));
    if let Some(out) = pdfsig(&bytes, "p256-pkcs7") {
        assert!(out.contains("Signature is Valid"), "{out}");
    }
}

#[test]
fn ink_image_in_appearance() {
    let pki = pki();
    let mut doc = Document::open(sample_pdf()).unwrap();
    let mut d = pagebara_syntax::Dict::new();
    d.set(b"Type", Object::name(b"XObject"));
    d.set(b"Subtype", Object::name(b"Image"));
    d.set(b"Width", 2);
    d.set(b"Height", 1);
    d.set(b"ColorSpace", Object::name(b"DeviceGray"));
    d.set(b"BitsPerComponent", 8);
    let img = doc.add(Stream::new(d, vec![0u8, 255]));
    let appearance = Appearance {
        page: 0,
        rect: [350.0, 80.0, 550.0, 140.0],
        image: Some(img),
        text: true,
    };
    let opts = SignOptions {
        field: Some("approval"),
        appearance: Some(&appearance),
        ..Default::default()
    };
    let bytes = sign(&doc, &pki.ec_signer, &opts).unwrap();
    qpdf_check(&bytes, "ink");
    let r = one(&verify_all(&bytes, &trust())).clone();
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    let doc = Document::open(bytes).unwrap();
    let form = Form::load(&doc);
    let w = &form.field("approval").unwrap().widgets[0];
    let wd = doc.get(w.id).unwrap();
    let ap = doc.get_in(wd.as_dict().unwrap(), b"AP");
    let n = doc.get_in(ap.as_dict().unwrap(), b"N");
    let s = String::from_utf8_lossy(&doc.decode(n.as_stream().unwrap()).unwrap().data).into_owned();
    assert!(s.contains("/Img Do"), "{s}");
}

#[test]
fn tampering_is_detected() {
    let pki = pki();
    let bytes = signed_with(&pki.rsa_signer, &SignOptions::default());
    // A byte of the page content, inside the signed range.
    let at = bytes.windows(8).position(|w| w == b"Contract").unwrap();
    let mut bad = bytes.clone();
    bad[at] = b'K';
    let r = one(&verify_all(&bad, &trust())).clone();
    assert_eq!(r.status, Status::Invalid, "{r:#?}");
    assert!(!r.intact);
    assert!(r.problems.iter().any(|p| p.contains("altered")), "{r:#?}");
    // A byte of the signature value, which ends the CMS structure. (A byte of the
    // embedded certificates would not matter: they are not signed.)
    let doc = Document::open(bytes.clone()).unwrap();
    let br = Form::load(&doc).signatures(&doc)[0]
        .byte_range
        .clone()
        .unwrap();
    let (der, _) = extract(&bytes, 0);
    let mut bad = bytes.clone();
    let p = br.ranges[0].1 + 1 + 2 * (der.len() - 10);
    bad[p] = if bad[p] == b'0' { b'1' } else { b'0' };
    let r = one(&verify_all(&bad, &trust())).clone();
    assert_eq!(r.status, Status::Invalid, "{r:#?}");
    if let Some(out) = pdfsig(&bad, "tampered") {
        assert!(!out.contains("Signature is Valid"), "{out}");
    }
}

#[test]
fn form_fill_after_signing_is_allowed() {
    let pki = pki();
    let signed = signed_with(&pki.rsa_signer, &SignOptions::default());
    let filled = update(&signed, |doc| {
        let mut form = Form::load(doc);
        form.set_text(doc, "name", "Anna Müller").unwrap();
    });
    qpdf_check(&filled, "filled");
    let r = one(&verify_all(&filled, &trust())).clone();
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    assert_eq!(r.revisions, 3);
    assert!(!r.changes.is_empty());
    assert!(r.changes.iter().all(|c| c.allowed), "{:#?}", r.changes);
    assert!(
        r.changes
            .iter()
            .any(|c| c.kind == ChangeKind::FormFill && c.description.contains("\"name\"")),
        "{:#?}",
        r.changes
    );
    if let Some(out) = pdfsig(&filled, "filled") {
        assert!(out.contains("Signature is Valid"), "{out}");
    }
}

#[test]
fn page_content_change_is_disallowed() {
    let pki = pki();
    let signed = signed_with(&pki.rsa_signer, &SignOptions::default());
    let changed = update(&signed, |doc| {
        let s = doc.get(CONTENT).unwrap().as_stream().unwrap().clone();
        let new = Stream::new(
            s.dict.clone(),
            b"BT /F1 24 Tf 72 700 Td (Contrast) Tj ET".to_vec(),
        );
        doc.set(CONTENT, new);
    });
    let r = one(&verify_all(&changed, &trust())).clone();
    assert_eq!(r.status, Status::Invalid, "{r:#?}");
    assert!(r.intact, "the signed revision itself is intact");
    let bad: Vec<_> = r.disallowed_changes().collect();
    assert!(
        bad.iter()
            .any(|c| c.kind == ChangeKind::Other && c.description.contains("page content")),
        "{bad:#?}"
    );
}

#[test]
fn comments_after_approval_and_certification() {
    let pki = pki();
    // An approval signature allows comments.
    let signed = signed_with(&pki.rsa_signer, &SignOptions::default());
    let commented = update(&signed, add_comment);
    let r = one(&verify_all(&commented, &trust())).clone();
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    assert!(
        r.changes
            .iter()
            .any(|c| c.kind == ChangeKind::Annotation && c.allowed)
    );

    // Certification level 2 (form filling) does not.
    let opts = SignOptions {
        certify: Some(MdpPermissions::FormFilling),
        ..Default::default()
    };
    let certified = signed_with(&pki.rsa_signer, &opts);
    let r = one(&verify_all(&certified, &trust())).clone();
    assert_eq!(r.kind, SignatureKind::Certification(2));
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    let commented = update(&certified, add_comment);
    let r = one(&verify_all(&commented, &trust())).clone();
    assert_eq!(r.status, Status::Invalid, "{r:#?}");
    assert!(
        r.disallowed_changes()
            .any(|c| c.kind == ChangeKind::Annotation)
    );
    // But it does allow form filling.
    let filled = update(&certified, |doc| {
        Form::load(doc).set_text(doc, "name", "X").unwrap();
    });
    assert_eq!(
        one(&verify_all(&filled, &trust())).status,
        Status::ValidTrusted
    );

    // Level 3 allows comments.
    let opts = SignOptions {
        certify: Some(MdpPermissions::Annotations),
        ..Default::default()
    };
    let certified = signed_with(&pki.rsa_signer, &opts);
    let commented = update(&certified, add_comment);
    assert_eq!(
        one(&verify_all(&commented, &trust())).status,
        Status::ValidTrusted
    );
    if let Some(out) = pdfsig(&certified, "certified") {
        assert!(out.contains("Signature is Valid"), "{out}");
    }
}

#[test]
fn certified_no_changes() {
    let pki = pki();
    let opts = SignOptions {
        certify: Some(MdpPermissions::NoChanges),
        ..Default::default()
    };
    let certified = signed_with(&pki.ec_signer, &opts);
    let doc = Document::open(certified.clone()).unwrap();
    // No further signatures.
    assert_eq!(
        sign(&doc, &pki.rsa_signer, &SignOptions::default()).unwrap_err(),
        Error::NotPermitted
    );
    let filled = update(&certified, |doc| {
        Form::load(doc).set_text(doc, "name", "X").unwrap();
    });
    let r = one(&verify_all(&filled, &trust())).clone();
    assert_eq!(r.status, Status::Invalid, "{r:#?}");
    // A document timestamp and LTV data are still allowed.
    let tsa = LocalTimestampAuthority::new(pki.tsa.clone());
    let stamped = timestamp_document(&doc, &tsa, DigestAlgorithm::Sha256).unwrap();
    let reports = verify_all(&stamped, &trust());
    assert_eq!(reports.len(), 2);
    assert!(
        reports.iter().all(|r| r.status == Status::ValidTrusted),
        "{reports:#?}"
    );
}

#[test]
fn two_signatures() {
    let pki = pki();
    let first = signed_with(&pki.rsa_signer, &SignOptions::default());
    let doc = Document::open(first).unwrap();
    let appearance = Appearance {
        page: 0,
        rect: [72.0, 80.0, 300.0, 140.0],
        image: None,
        text: true,
    };
    let opts = SignOptions {
        appearance: Some(&appearance),
        digest: Some(DigestAlgorithm::Sha512),
        ..Default::default()
    };
    let both = sign(&doc, &pki.ec_signer, &opts).unwrap();
    qpdf_check(&both, "two");
    let reports = verify_all(&both, &trust());
    assert_eq!(reports.len(), 2);
    for r in &reports {
        assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    }
    let first = reports.iter().find(|r| r.info.field == "approval").unwrap();
    assert!(
        first
            .changes
            .iter()
            .any(|c| c.kind == ChangeKind::Signing && c.allowed),
        "{:#?}",
        first.changes
    );
    let second = reports
        .iter()
        .find(|r| r.info.field == "Signature1")
        .unwrap();
    assert_eq!(second.digest, Some(DigestAlgorithm::Sha512));
    assert!(second.changes.is_empty());
    if let Some(out) = pdfsig(&both, "two") {
        assert_eq!(out.matches("Signature is Valid").count(), 2, "{out}");
    }
}

#[test]
fn rsa_pss() {
    let pki = pki();
    let opts = SignOptions {
        pss: true,
        ..Default::default()
    };
    let bytes = signed_with(&pki.rsa_signer, &opts);
    let r = one(&verify_all(&bytes, &trust())).clone();
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    assert_eq!(r.algorithm.as_deref(), Some("RSA-PSS with SHA-256"));
}

#[test]
fn signature_timestamp() {
    let pki = pki();
    let mut tsa = LocalTimestampAuthority::new(pki.tsa.clone());
    let when = Time(Time::now().0 - 600);
    tsa.time = Some(when);
    let opts = SignOptions {
        timestamp: Some(&tsa),
        time: Some(Time(when.0 - 10)),
        ..Default::default()
    };
    let bytes = signed_with(&pki.rsa_signer, &opts);
    let r = one(&verify_all(&bytes, &trust())).clone();
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    let ts = r.timestamp.clone().unwrap();
    assert!(ts.valid && ts.trust.is_trusted(), "{ts:#?}");
    assert_eq!(ts.time, when);
    assert_eq!(r.time_source, TimeSource::Timestamp);
    assert_eq!(r.signing_time, Some(when));
    if let Some(out) = pdfsig(&bytes, "timestamped") {
        assert!(out.contains("Signature is Valid"), "{out}");
    }
}

/// Answers from the test CAs, as an OCSP responder and CRL server would.
struct Fetcher {
    revoked: Option<Time>,
}

impl RevocationFetcher for Fetcher {
    fn ocsp(&self, cert: &Certificate, issuer: &Certificate) -> Option<Vec<u8>> {
        let pki = pki();
        // Only the RSA hierarchy has a responder here; the EC one uses CRLs.
        (issuer == &pki.rsa_root.certificate).then(|| {
            issue_ocsp_response(&pki.rsa_root, cert, issuer, self.revoked, Time::now()).unwrap()
        })
    }

    fn crl(&self, cert: &Certificate) -> Option<Vec<u8>> {
        let ca = &pki().ec_root;
        let now = Time::now();
        let revoked: Vec<(&Certificate, Time)> =
            self.revoked.map(|t| (cert, t)).into_iter().collect();
        Some(issue_crl(ca, &revoked, now, Time(now.0 + 86400)).unwrap())
    }
}

#[test]
fn ltv_and_document_timestamp() {
    let pki = pki();
    let tsa = LocalTimestampAuthority::new(pki.tsa.clone());
    let opts = SignOptions {
        timestamp: Some(&tsa),
        ..Default::default()
    };
    let signed = signed_with(&pki.rsa_signer, &opts);
    let doc = Document::open(signed).unwrap();
    let lt = add_ltv(&doc, &trust(), &Fetcher { revoked: None }).unwrap();
    let doc = Document::open(lt).unwrap();
    let lta = timestamp_document(&doc, &tsa, DigestAlgorithm::Sha256).unwrap();
    qpdf_check(&lta, "lta");
    let reports = verify_all(&lta, &trust());
    assert_eq!(reports.len(), 2);
    let sig = reports
        .iter()
        .find(|r| r.kind == SignatureKind::Approval)
        .unwrap();
    assert_eq!(sig.status, Status::ValidTrusted, "{sig:#?}");
    assert!(
        matches!(
            sig.revocation,
            RevocationStatus::Good {
                source: RevocationSource::Ocsp,
                ..
            }
        ),
        "{:?}",
        sig.revocation
    );
    assert!(sig.changes.iter().all(|c| c.allowed), "{:#?}", sig.changes);
    assert!(sig.changes.iter().any(|c| c.kind == ChangeKind::Ltv));
    let dts = reports
        .iter()
        .find(|r| r.kind == SignatureKind::DocumentTimestamp)
        .unwrap();
    assert_eq!(dts.status, Status::ValidTrusted, "{dts:#?}");
    assert_eq!(dts.info.sub_filter.as_deref(), Some("ETSI.RFC3161"));
    if let Some(out) = pdfsig(&lta, "lta") {
        // Poppler checks the signature but does not verify document timestamps.
        assert_eq!(out.matches("Signature is Valid").count(), 1, "{out}");
        assert!(
            !out.contains("Invalid") && !out.contains("Mismatch"),
            "{out}"
        );
    }

    // The EC signer gets a CRL.
    let signed = signed_with(&pki.ec_signer, &SignOptions::default());
    let lt = add_ltv(
        &Document::open(signed).unwrap(),
        &trust(),
        &Fetcher { revoked: None },
    )
    .unwrap();
    let r = one(&verify_all(&lt, &trust())).clone();
    assert!(
        matches!(
            r.revocation,
            RevocationStatus::Good {
                source: RevocationSource::Crl,
                ..
            }
        ),
        "{r:#?}"
    );
}

#[test]
fn revoked_before_signing_is_invalid() {
    let pki = pki();
    let opts = SignOptions {
        time: Some(Time(Time::now().0 - 60)),
        ..Default::default()
    };
    let signed = signed_with(&pki.rsa_signer, &opts);
    let fetch = Fetcher {
        revoked: Some(Time(Time::now().0 - 3600)),
    };
    let lt = add_ltv(&Document::open(signed).unwrap(), &trust(), &fetch).unwrap();
    let r = one(&verify_all(&lt, &trust())).clone();
    assert!(
        matches!(r.revocation, RevocationStatus::Revoked { .. }),
        "{r:#?}"
    );
    assert_eq!(r.status, Status::Invalid, "{r:#?}");
}

#[test]
fn self_signed_identity() {
    let key = PrivateKey::generate_p384().unwrap();
    let mut req = CertificateRequest::signer("Chris Self", 30);
    req.email = Some("chris@example.org".into());
    let id = Identity::self_signed(&req, key).unwrap();
    let bytes = signed_with(&id, &SignOptions::default());
    let r = one(&verify_all(&bytes, &trust())).clone();
    assert_eq!(r.status, Status::ValidUntrusted, "{r:#?}");
    assert_eq!(r.trust, Trust::SelfSigned);
    assert_eq!(r.digest, Some(DigestAlgorithm::Sha384));
    // Trusting it explicitly.
    let mut t = trust();
    t.add(id.certificate.clone(), TrustSource::User);
    assert_eq!(one(&verify_all(&bytes, &t)).status, Status::ValidTrusted);
    // The user list survives saving as PEM.
    let mut back = TrustStore::new();
    assert_eq!(
        back.add_pem(t.user_pem().as_bytes(), TrustSource::User)
            .unwrap(),
        3
    );
}

#[test]
fn openssl_cms_verify() {
    let Some(openssl) = tool("openssl") else {
        eprintln!("openssl not found; skipping");
        return;
    };
    let pki = pki();
    for (name, id, sub) in [
        ("rsa", &pki.rsa_signer, SubFilter::CadesDetached),
        ("ec", &pki.ec_signer, SubFilter::Pkcs7Detached),
    ] {
        let opts = SignOptions {
            sub_filter: sub,
            ..Default::default()
        };
        let bytes = signed_with(id, &opts);
        let (der, data) = extract(&bytes, 0);
        let sig = scratch(&format!("cms-{name}.der"));
        let content = scratch(&format!("cms-{name}.bin"));
        let ca = scratch(&format!("cms-{name}-ca.pem"));
        std::fs::write(&sig, der).unwrap();
        std::fs::write(&content, data).unwrap();
        let root = id.chain.last().unwrap();
        std::fs::write(&ca, root.to_pem()).unwrap();
        let out = std::process::Command::new(&openssl)
            .args([
                "cms", "-verify", "-binary", "-inform", "DER", "-purpose", "any", "-in",
            ])
            .arg(&sig)
            .arg("-content")
            .arg(&content)
            .arg("-CAfile")
            .arg(&ca)
            .args(["-out", "/dev/null"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.success() && text.contains("Verification successful"),
            "{name}: {text}"
        );
    }
}

/// Every signature is checked on its own: the revision it covers, and the later updates
/// judged by the certification's permissions.
#[test]
fn every_signature_with_its_revision() {
    let pki = pki();
    let opts = SignOptions {
        certify: Some(MdpPermissions::FormFilling),
        ..Default::default()
    };
    let certified = signed_with(&pki.ec_signer, &opts);
    let filled = update(&certified, |doc| {
        Form::load(doc).set_text(doc, "name", "Filled").unwrap();
    });
    let opts = SignOptions {
        field: Some("second"),
        ..Default::default()
    };
    let approved = sign(&Document::open(filled).unwrap(), &pki.rsa_signer, &opts).unwrap();
    let reports = verify_all(&approved, &trust());
    assert_eq!(reports.len(), 2);
    let cert = reports.iter().find(|r| r.info.field == "approval").unwrap();
    let appr = reports.iter().find(|r| r.info.field == "second").unwrap();
    assert_eq!(cert.kind, SignatureKind::Certification(2));
    assert_eq!((cert.revision, appr.revision, appr.revisions), (2, 4, 4));
    for r in &reports {
        assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    }
    assert!(cert.changes.iter().any(|c| c.kind == ChangeKind::FormFill));
    assert!(cert.changes.iter().any(|c| c.kind == ChangeKind::Signing));
    assert!(appr.changes.is_empty());

    // A comment is beyond what the certification allows: both signatures say so.
    let commented = update(&approved, add_comment);
    let reports = verify_all(&commented, &trust());
    for r in &reports {
        assert_eq!(r.status, Status::Invalid, "{r:#?}");
        assert!(
            r.disallowed_changes()
                .any(|c| c.kind == ChangeKind::Annotation),
            "{:#?}",
            r.changes
        );
    }
    // A document timestamp changes nothing for either.
    let tsa = LocalTimestampAuthority::new(pki.tsa.clone());
    let stamped = timestamp_document(
        &Document::open(approved).unwrap(),
        &tsa,
        DigestAlgorithm::Sha256,
    )
    .unwrap();
    let reports = verify_all(&stamped, &trust());
    assert_eq!(reports.len(), 3);
    assert!(reports.iter().all(|r| r.status == Status::ValidTrusted));
}

#[test]
fn ltv_writes_vri_per_signature() {
    let pki = pki();
    let tsa = LocalTimestampAuthority::new(pki.tsa.clone());
    let opts = SignOptions {
        timestamp: Some(&tsa),
        ..Default::default()
    };
    let first = signed_with(&pki.rsa_signer, &opts);
    let both = sign(
        &Document::open(first).unwrap(),
        &pki.ec_signer,
        &SignOptions::default(),
    )
    .unwrap();
    assert!(verify_all(&both, &trust()).iter().all(|r| !r.vri));
    let lt = add_ltv(
        &Document::open(both).unwrap(),
        &trust(),
        &Fetcher { revoked: None },
    )
    .unwrap();
    let lta = timestamp_document(
        &Document::open(lt.clone()).unwrap(),
        &tsa,
        DigestAlgorithm::Sha256,
    )
    .unwrap();
    qpdf_check(&lta, "vri");
    let reports = verify_all(&lta, &trust());
    assert_eq!(reports.len(), 3);
    for r in &reports {
        assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
        // The document timestamp came after the DSS.
        assert_eq!(r.vri, r.kind != SignatureKind::DocumentTimestamp, "{r:#?}");
        assert_eq!(r.vri_key.len(), 40);
        assert!(
            r.vri_key
                .bytes()
                .all(|b| b.is_ascii_digit() || b.is_ascii_uppercase())
        );
    }

    // The VRI entries point at the streams of the DSS's own lists.
    let doc = Document::open(lt).unwrap();
    let cat = doc.catalog().unwrap();
    let dss = doc.get_in(cat.as_dict().unwrap(), b"DSS");
    let dss = dss.as_dict().unwrap();
    assert_eq!(dss.name(b"Type"), Some(&b"DSS"[..]));
    let refs = |d: &pagebara_syntax::Dict, k: &[u8]| -> Vec<Object> {
        doc.get_in(d, k).as_array().unwrap_or_default().to_vec()
    };
    let (certs, ocsps, crls) = (refs(dss, b"Certs"), refs(dss, b"OCSPs"), refs(dss, b"CRLs"));
    assert!(!ocsps.is_empty() && !crls.is_empty());
    let vri = doc.get_in(dss, b"VRI");
    let vri = vri.as_dict().unwrap();
    for r in reports.iter().filter(|r| r.vri) {
        let e = doc.get_in(vri, r.vri_key.as_bytes());
        let e = e.as_dict().expect("a VRI entry");
        assert!(e.get(b"TU").is_some());
        let cert = refs(e, b"Cert");
        assert!(cert.len() >= 2, "{e:?}");
        assert!(cert.iter().all(|c| certs.contains(c)));
        assert!(refs(e, b"OCSP").iter().all(|c| ocsps.contains(c)));
        assert!(refs(e, b"CRL").iter().all(|c| crls.contains(c)));
        assert!(!refs(e, b"OCSP").is_empty() || !refs(e, b"CRL").is_empty());
    }
    // The key is the SHA-1 of the /Contents bytes.
    let info = Form::load(&doc)
        .signatures(&doc)
        .into_iter()
        .find(|i| i.field == "approval")
        .unwrap();
    let br = info.byte_range.unwrap();
    let (a, c) = (br.ranges[0].1, br.ranges[1].0);
    let contents: Vec<u8> = doc.source()[a + 1..c - 1]
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect();
    let key = vri_key(&contents);
    assert!(
        reports
            .iter()
            .any(|r| r.vri_key == key && r.info.field == "approval")
    );
}

/// A key that signs from elsewhere and never gives out its material.
struct HeldKey(PrivateKey);

impl Signer for HeldKey {
    fn public_key_der(&self) -> Result<Vec<u8>> {
        self.0.public_key_der()
    }

    fn sign_prehash(&self, scheme: SignatureScheme, hash: &[u8]) -> Result<Vec<u8>> {
        self.0.sign_prehash(scheme, hash)
    }

    fn location(&self) -> String {
        "test token".into()
    }
}

#[test]
fn external_signer() {
    let pki = pki();
    for (id, pss) in [
        (&pki.rsa_signer, false),
        (&pki.rsa_signer, true),
        (&pki.ec_signer, false),
    ] {
        let key = PrivateKey::external(std::sync::Arc::new(HeldKey(id.key.clone()))).unwrap();
        assert!(key.to_pkcs8_der().is_err());
        assert!(key.description().ends_with("in the test token"));
        let held = Identity {
            key,
            certificate: id.certificate.clone(),
            chain: id.chain.clone(),
        };
        let opts = SignOptions {
            pss,
            ..Default::default()
        };
        let r = one(&verify_all(&signed_with(&held, &opts), &trust())).clone();
        assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    }
    // A store answering with another key's signature is caught before anything is saved.
    let wrong = PrivateKey::External(
        std::sync::Arc::new(HeldKey(PrivateKey::generate_p256().unwrap())),
        pki.ec_signer.certificate.public_key().unwrap(),
    );
    let lying = Identity {
        key: wrong,
        certificate: pki.ec_signer.certificate.clone(),
        chain: pki.ec_signer.chain.clone(),
    };
    let doc = Document::open(sample_pdf()).unwrap();
    assert!(matches!(
        sign(&doc, &lying, &SignOptions::default()),
        Err(Error::Key(_))
    ));
}
