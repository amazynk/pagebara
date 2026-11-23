//! Loading signing identities: PKCS #12 (ours and OpenSSL's, modern and legacy), PEM keys
//! in every common format, non-ASCII passwords.

mod common;

use std::path::Path;
use std::process::Command;

use common::*;
use pagebara_sign::*;
use pagebara_syntax::Document;

const PASSWORD: &str = "pässwörd ✓ 签名";

fn sign_and_check(id: &Identity, name: &str) {
    let doc = Document::open(sample_pdf()).unwrap();
    let bytes = sign(&doc, id, &SignOptions::default()).unwrap();
    let r = &verify_all(&bytes, &TrustStore::new())[0];
    assert!(
        matches!(r.status, Status::ValidUntrusted | Status::ValidTrusted),
        "{name}: {r:#?}"
    );
}

#[test]
fn pkcs12_round_trip() {
    let pki = pki();
    for (name, id) in [("rsa", &pki.rsa_signer), ("ec", &pki.ec_signer)] {
        let p12 = id.to_pkcs12(PASSWORD, Some("My ID")).unwrap();
        let back = Identity::from_pkcs12(&p12, PASSWORD).unwrap();
        assert_eq!(back.certificate, id.certificate, "{name}");
        assert_eq!(back.chain, id.chain, "{name}");
        assert_eq!(
            back.key.public_key_der().unwrap(),
            id.key.public_key_der().unwrap()
        );
        assert_eq!(
            Identity::from_pkcs12(&p12, "pässwörd").unwrap_err(),
            Error::WrongPassword
        );
        sign_and_check(&back, name);
        if let Some(openssl) = common::openssl() {
            // OpenSSL reads what we write.
            let path = scratch(&format!("ours-{name}.p12"));
            std::fs::write(&path, &p12).unwrap();
            let out = Command::new(openssl)
                .args(["pkcs12", "-noout", "-passin"])
                .arg(format!("pass:{PASSWORD}"))
                .arg("-in")
                .arg(&path)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{name}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    }
}

#[test]
fn pem_keys() {
    let pki = pki();
    let id = &pki.ec_signer;
    let key = pem("PRIVATE KEY", &id.key.to_pkcs8_der().unwrap());
    let mut certs = id.certificate.to_pem();
    for c in &id.chain {
        certs.push_str(&c.to_pem());
    }
    // Chain first: the order of certificates does not matter.
    let reordered = format!("{}{}", id.chain[0].to_pem(), id.certificate.to_pem());
    for c in [&certs, &reordered] {
        let back = Identity::from_pem(key.as_bytes(), c.as_bytes(), None).unwrap();
        assert_eq!(back.certificate, id.certificate);
        assert_eq!(back.chain.len(), 1);
    }
    // A certificate that does not belong to the key is refused.
    let other = pki.rsa_signer.certificate.to_pem();
    assert!(Identity::from_pem(key.as_bytes(), other.as_bytes(), None).is_err());
}

fn pem(label: &str, der: &[u8]) -> String {
    // Through a certificate's PEM writer would need a certificate; do it by hand.
    let b64 = base64(der);
    let mut s = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        s.push_str(std::str::from_utf8(chunk).unwrap());
        s.push('\n');
    }
    s + &format!("-----END {label}-----\n")
}

fn base64(b: &[u8]) -> String {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in b.chunks(3) {
        let n = c
            .iter()
            .enumerate()
            .fold(0u32, |a, (i, &x)| a | u32::from(x) << (16 - 8 * i));
        for i in 0..4 {
            s.push(if i <= c.len() {
                A[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    s
}

fn openssl(args: &[&str], dir: &Path) -> bool {
    let Some(o) = common::openssl() else {
        return false;
    };
    let out = Command::new(o)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    if !out.status.success() {
        eprintln!("openssl {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    }
    out.status.success()
}

/// Keys and PKCS #12 files made by OpenSSL at test time.
#[test]
fn openssl_made_identities() {
    if common::openssl().is_none() {
        eprintln!("openssl not found; skipping");
        return;
    }
    let dir = scratch("openssl-ids");
    std::fs::create_dir_all(&dir).unwrap();
    let pass = format!("pass:{PASSWORD}");
    let read = |f: &str| std::fs::read(dir.join(f)).unwrap();
    assert!(openssl(
        &[
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "rsa.key",
            "-out",
            "rsa.crt",
            "-days",
            "30",
            "-subj",
            "/CN=OpenSSL RSA/O=Test",
            "-addext",
            "keyUsage=critical,digitalSignature,nonRepudiation",
        ],
        &dir
    ));
    // PKCS #8, unencrypted.
    let id = Identity::from_pem(&read("rsa.key"), &read("rsa.crt"), None).unwrap();
    assert_eq!(id.certificate.display_name(), "OpenSSL RSA");
    sign_and_check(&id, "openssl rsa pkcs8");
    // PKCS #1 ("RSA PRIVATE KEY").
    if openssl(
        &["rsa", "-in", "rsa.key", "-traditional", "-out", "rsa1.key"],
        &dir,
    ) {
        let id = Identity::from_pem(&read("rsa1.key"), &read("rsa.crt"), None).unwrap();
        sign_and_check(&id, "openssl rsa pkcs1");
    }
    // Encrypted PKCS #8 (PBES2, AES-256-CBC) with a non-ASCII password.
    assert!(openssl(
        &[
            "pkcs8",
            "-topk8",
            "-in",
            "rsa.key",
            "-out",
            "rsa-enc.key",
            "-passout",
            &pass
        ],
        &dir
    ));
    let id = Identity::from_pem(&read("rsa-enc.key"), &read("rsa.crt"), Some(PASSWORD)).unwrap();
    sign_and_check(&id, "openssl encrypted pkcs8");
    assert_eq!(
        Identity::from_pem(&read("rsa-enc.key"), &read("rsa.crt"), Some("nope")).unwrap_err(),
        Error::WrongPassword
    );
    // PKCS #12, OpenSSL 3 defaults (PBES2 AES-256, HMAC-SHA-256).
    assert!(openssl(
        &[
            "pkcs12",
            "-export",
            "-inkey",
            "rsa.key",
            "-in",
            "rsa.crt",
            "-out",
            "modern.p12",
            "-passout",
            &pass,
        ],
        &dir
    ));
    let id = Identity::from_pkcs12(&read("modern.p12"), PASSWORD).unwrap();
    assert_eq!(id.certificate.display_name(), "OpenSSL RSA");
    sign_and_check(&id, "openssl modern p12");
    assert_eq!(
        Identity::from_pkcs12(&read("modern.p12"), "wrong").unwrap_err(),
        Error::WrongPassword
    );
    // Legacy PKCS #12 (3DES key, RC2-40 certificates, SHA-1 MAC), as older Windows and
    // OpenSSL 1.x wrote them; needs OpenSSL's legacy provider.
    if openssl(
        &[
            "pkcs12",
            "-export",
            "-legacy",
            "-inkey",
            "rsa.key",
            "-in",
            "rsa.crt",
            "-out",
            "legacy.p12",
            "-passout",
            &pass,
        ],
        &dir,
    ) {
        let id = Identity::from_pkcs12(&read("legacy.p12"), PASSWORD).unwrap();
        sign_and_check(&id, "openssl legacy p12");
    } else {
        eprintln!("OpenSSL legacy provider unavailable; legacy PKCS #12 not tested");
    }
    // Explicit 3DES key and certificate encryption with a SHA-1 MAC, without the legacy
    // provider.
    if openssl(
        &[
            "pkcs12",
            "-export",
            "-keypbe",
            "PBE-SHA1-3DES",
            "-certpbe",
            "PBE-SHA1-3DES",
            "-macalg",
            "sha1",
            "-inkey",
            "rsa.key",
            "-in",
            "rsa.crt",
            "-out",
            "3des.p12",
            "-passout",
            &pass,
        ],
        &dir,
    ) {
        let id = Identity::from_pkcs12(&read("3des.p12"), PASSWORD).unwrap();
        sign_and_check(&id, "openssl 3des p12");
    }
    // EC P-256 with a SEC 1 key ("EC PRIVATE KEY") and as PKCS #12; P-384 too.
    for curve in ["prime256v1", "secp384r1"] {
        let key = format!("{curve}.key");
        let crt = format!("{curve}.crt");
        assert!(openssl(
            &["ecparam", "-genkey", "-name", curve, "-noout", "-out", &key],
            &dir
        ));
        assert!(openssl(
            &[
                "req",
                "-x509",
                "-key",
                &key,
                "-out",
                &crt,
                "-days",
                "30",
                "-subj",
                "/CN=OpenSSL EC",
            ],
            &dir
        ));
        let id = Identity::from_pem(&read(&key), &read(&crt), None).unwrap();
        sign_and_check(&id, curve);
        let p12 = format!("{curve}.p12");
        assert!(openssl(
            &[
                "pkcs12", "-export", "-inkey", &key, "-in", &crt, "-out", &p12, "-passout", &pass
            ],
            &dir
        ));
        let id = Identity::from_pkcs12(&read(&p12), PASSWORD).unwrap();
        sign_and_check(&id, &p12);
    }
}
