use pagebara_sign::{SignOptions, VerifyOptions, sign, verify};
use pagebara_syntax::Document;

use super::*;
use crate::test_pdf;

fn signed(password: &str) -> (Vec<u8>, Identity) {
    let p12 = self_signed_id("Ann Example", "ann@example.org", password).unwrap();
    let id = read_identity(&p12, password).unwrap();
    let doc = Document::open(test_pdf::text(&["Hello signed world"])).unwrap();
    let out = sign(&doc, &id, &SignOptions::default()).unwrap();
    (out, id)
}

fn views(bytes: Vec<u8>, trust: &TrustStore) -> Vec<SigView> {
    let doc = Document::open(bytes).unwrap();
    verify(&doc, &VerifyOptions::new(trust))
        .iter()
        .map(view)
        .collect()
}

#[test]
fn self_signed_ids_round_trip_with_unicode_passwords() {
    let pw = "pässwörd 密码 🔐";
    let (_, id) = signed(pw);
    assert_eq!(id.certificate.display_name(), "Ann Example");
    assert_eq!(id.certificate.email().as_deref(), Some("ann@example.org"));
    let p12 = self_signed_id("Ann", "", pw).unwrap();
    assert_eq!(
        read_identity(&p12, "wrong").unwrap_err(),
        "The password is not right."
    );
    assert!(self_signed_id(" ", "", pw).is_err());
    assert!(self_signed_id("Ann", "", "").is_err());
}

#[test]
fn pem_identities_are_read_from_one_file() {
    let (_, id) = signed("pw");
    let key = id.key.to_pkcs8_der().unwrap();
    let mut pem = String::from("-----BEGIN PRIVATE KEY-----\n");
    pem.push_str(&base64(&key));
    pem.push_str("-----END PRIVATE KEY-----\n");
    pem.push_str(&id.certificate.to_pem());
    let back = read_identity(pem.as_bytes(), "").unwrap();
    assert_eq!(back.certificate, id.certificate);
}

fn base64(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for (i, c) in data.chunks(3).enumerate() {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for k in 0..4 {
            if k <= c.len() {
                out.push(T[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
        if i % 16 == 15 {
            out.push('\n');
        }
    }
    out.push('\n');
    out
}

#[test]
fn untrusted_until_the_certificate_is_trusted() {
    let (bytes, id) = signed("pw");
    let mut trust = TrustStore::new();
    let v = views(bytes.clone(), &trust);
    assert_eq!(v.len(), 1);
    assert_eq!(v[0].verdict, Verdict::Untrusted);
    assert_eq!(v[0].signer, "Ann Example <ann@example.org>");
    assert!(v[0].trust.contains("self-signed"));
    assert!(v[0].time.contains("signer's clock"));
    assert_eq!(v[0].anchor.as_ref(), Some(&id.certificate));
    assert_eq!(
        field_status(&v[0]),
        "Signature: valid, signer not trusted · by Ann Example <ann@example.org>"
    );
    let (verdict, line) = banner(&v).unwrap();
    assert_eq!(verdict, Verdict::Untrusted);
    assert!(line.starts_with("Signed by Ann Example"));

    trust.add(id.certificate.clone(), TrustSource::User);
    let v = views(bytes.clone(), &trust);
    assert_eq!(v[0].verdict, Verdict::Valid);
    assert!(v[0].anchor.is_none());
    assert_eq!(user_anchors(&trust).len(), 1);
    assert_eq!(
        v[0].revision,
        "Revision 2 of 2: the whole document as it is now"
    );
    assert_eq!(v[0].ltv, "No validation data embedded");

    // Validation data in a later revision: allowed, and embedded for the signature.
    struct NoNetwork;
    impl pagebara_sign::RevocationFetcher for NoNetwork {
        fn ocsp(&self, _: &Certificate, _: &Certificate) -> Option<Vec<u8>> {
            None
        }
        fn crl(&self, _: &Certificate) -> Option<Vec<u8>> {
            None
        }
    }
    let doc = Document::open(bytes).unwrap();
    let lt = pagebara_sign::add_ltv(&doc, &trust, &NoNetwork).unwrap();
    let v = views(lt, &trust);
    assert_eq!(v[0].verdict, Verdict::Valid);
    assert_eq!(v[0].revision, "Revision 2 of 3: later changes are allowed");
    assert_eq!(v[0].ltv, "Validation data embedded (long-term validation)");
}

#[test]
fn tampering_makes_it_invalid() {
    let (mut bytes, _) = signed("pw");
    // Change a byte of the signed original content.
    let at = bytes.windows(5).position(|w| w == b"Hello").unwrap();
    bytes[at] = b'J';
    let v = views(bytes, &TrustStore::new());
    assert_eq!(v[0].verdict, Verdict::Invalid);
    assert!(!v[0].problems.is_empty());
    assert_eq!(banner(&v).unwrap().0, Verdict::Invalid);
}

#[test]
fn banners_for_several_signatures() {
    let mk = |verdict| SigView {
        field: String::new(),
        page: None,
        verdict,
        summary: String::new(),
        kind: String::new(),
        signer: String::new(),
        time: String::new(),
        timestamp: String::new(),
        reason: String::new(),
        location: String::new(),
        trust: String::new(),
        chain: Vec::new(),
        revocation: String::new(),
        revision: String::new(),
        ltv: String::new(),
        changes: Vec::new(),
        problems: Vec::new(),
        warnings: Vec::new(),
        anchor: None,
    };
    assert!(banner(&[]).is_none());
    assert_eq!(
        banner(&[mk(Verdict::Valid), mk(Verdict::Valid)]).unwrap().1,
        "2 signatures – all valid"
    );
    let (v, t) = banner(&[
        mk(Verdict::Valid),
        mk(Verdict::Invalid),
        mk(Verdict::Untrusted),
    ])
    .unwrap();
    assert_eq!(v, Verdict::Invalid);
    assert_eq!(t, "3 signatures – 1 INVALID");
}

#[test]
fn user_trust_is_saved_and_removed() {
    let dir = std::env::temp_dir().join(format!("pagebara-trust-{}", std::process::id()));
    let path = dir.join("trusted.pem");
    let (_, id) = signed("pw");
    let mut t = TrustStore::new();
    t.add(id.certificate.clone(), TrustSource::User);
    save_user_trust(&t, &path).unwrap();
    let mut back = TrustStore::new();
    assert_eq!(back.load_user_file(&path).unwrap(), 1);
    t.remove(&id.certificate.fingerprint());
    save_user_trust(&t, &path).unwrap();
    assert!(!path.exists());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn id_file_names_are_safe() {
    assert_eq!(id_file_name("Ann Example"), "Ann Example.p12");
    assert_eq!(id_file_name("a/b:c"), "a_b_c.p12");
    assert_eq!(id_file_name(" .. "), "id.p12");
    assert_eq!(
        when(Time::from_ymd_hms(2026, 3, 5, 13, 30, 9)),
        "2026-03-05 13:30 UTC"
    );
}

#[test]
fn key_store_identities_fit_for_signing_sign_externally() {
    use keystore::{TestKeyStore, signing_identities};
    let store = TestKeyStore::new().unwrap();
    let ids = signing_identities(&store, Time::now()).unwrap();
    assert_eq!(
        ids.len(),
        1,
        "the expired and encryption-only certificates are left out"
    );
    let s = &ids[0];
    assert_eq!(s.subject, "Test Store Signer <store@example.org>");
    assert_eq!(s.issuer, "Test Store CA");
    assert_eq!(s.location, "Test key store");
    assert_eq!(
        s.label(),
        "Test Store Signer <store@example.org> – Test key store"
    );
    assert!(matches!(s.identity.key, PrivateKey::External(..)));
    let doc = Document::open(test_pdf::text(&["Signed from a key store"])).unwrap();
    let out = sign(&doc, &s.identity, &SignOptions::default()).unwrap();
    let mut trust = TrustStore::new();
    assert_eq!(views(out.clone(), &trust)[0].verdict, Verdict::Untrusted);
    trust.add(s.identity.chain[0].clone(), TrustSource::User);
    let v = &views(out, &trust)[0];
    assert_eq!(v.verdict, Verdict::Valid);
    assert!(v.signer.contains("Test Store Signer"), "{}", v.signer);
}
