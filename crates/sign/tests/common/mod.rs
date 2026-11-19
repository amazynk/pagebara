//! A test PKI generated at run time (no keys are committed), sample PDFs, and checks with
//! external tools when they are installed.

#![allow(dead_code)]

pub mod http;

use std::path::PathBuf;
use std::process::Command;
use std::sync::OnceLock;

use pagebara_sign::*;
use pagebara_syntax::{Document, ObjRef, Object};

pub struct Pki {
    pub rsa_root: Identity,
    pub rsa_signer: Identity,
    pub ec_root: Identity,
    pub ec_signer: Identity,
    pub tsa: Identity,
}

/// Two hierarchies (RSA 2048 and P-256) and a timestamp authority, made once per run.
pub fn pki() -> &'static Pki {
    static PKI: OnceLock<Pki> = OnceLock::new();
    PKI.get_or_init(|| {
        let ca = |name: &str, key: PrivateKey| {
            let mut req = CertificateRequest::ca(name, 3650);
            req.organization = Some("pagebara tests".into());
            req.country = Some("DE".into());
            Identity::self_signed(&req, key).unwrap()
        };
        let rsa_root = ca(
            "pagebara Test Root RSA",
            PrivateKey::generate_rsa(2048).unwrap(),
        );
        let ec_root = ca(
            "pagebara Test Root EC",
            PrivateKey::generate_p256().unwrap(),
        );
        let mut req = CertificateRequest::signer("Anna Müller", 365);
        req.email = Some("anna@example.org".into());
        req.ocsp_url = Some("http://ocsp.example.org".into());
        req.crl_url = Some("http://crl.example.org/root.crl".into());
        let rsa_signer = rsa_root
            .issue(&req, PrivateKey::generate_rsa(2048).unwrap())
            .unwrap();
        req.common_name = "Ben Ng".into();
        let ec_signer = ec_root
            .issue(&req, PrivateKey::generate_p256().unwrap())
            .unwrap();
        let treq = CertificateRequest::timestamping("pagebara Test TSA", 365);
        let tsa = ec_root
            .issue(&treq, PrivateKey::generate_p256().unwrap())
            .unwrap();
        Pki {
            rsa_root,
            rsa_signer,
            ec_root,
            ec_signer,
            tsa,
        }
    })
}

/// Both test roots as user anchors.
pub fn trust() -> TrustStore {
    let mut t = TrustStore::new();
    t.add(pki().rsa_root.certificate.clone(), TrustSource::User);
    t.add(pki().ec_root.certificate.clone(), TrustSource::User);
    t
}

pub fn verify_all(bytes: &[u8], trust: &TrustStore) -> Vec<SignatureReport> {
    let doc = Document::open(bytes.to_vec()).unwrap();
    verify(&doc, &VerifyOptions::new(trust))
}

/// A one-page form: a text field `name` and an empty signature field `approval`.
/// Page content is object 4.
pub fn sample_pdf() -> Vec<u8> {
    let content = "BT /F1 24 Tf 72 700 Td (Contract) Tj ET";
    let objs = [
        "<</Type/Catalog/Pages 2 0 R/AcroForm<</Fields[6 0 R 7 0 R]/DA(/Helv 12 Tf 0 g)\
         /DR<</Font<</Helv 5 0 R>>>>>>>>"
            .to_string(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 612 792]/Contents 4 0 R\
         /Resources<</Font<</F1 5 0 R>>>>/Annots[6 0 R 7 0 R]>>"
            .to_string(),
        format!(
            "<</Length {}>>stream\n{content}\nendstream",
            content.len() + 1
        ),
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>".to_string(),
        "<</Type/Annot/Subtype/Widget/FT/Tx/T(name)/P 3 0 R/F 4/Rect[72 600 300 624]\
         /DA(/Helv 12 Tf 0 g)>>"
            .to_string(),
        "<</Type/Annot/Subtype/Widget/FT/Sig/T(approval)/P 3 0 R/F 4/Rect[350 80 550 140]>>"
            .to_string(),
    ];
    let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let x = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objs.len() + 1).bytes());
    for o in offs {
        out.extend(format!("{o:010} 00000 n\r\n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<</Size {} /Root 1 0 R/ID[<0123456789ABCDEF0123456789ABCDEF><0123456789ABCDEF0123456789ABCDEF>]>>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    out
}

pub const CONTENT: ObjRef = ObjRef::new(4, 0);
pub const PAGE: ObjRef = ObjRef::new(3, 0);

/// Applies `f` to an opened copy of `bytes` and saves incrementally.
pub fn update(bytes: &[u8], f: impl FnOnce(&mut Document)) -> Vec<u8> {
    let mut doc = Document::open(bytes.to_vec()).unwrap();
    f(&mut doc);
    doc.save_incremental().unwrap()
}

/// Adds a sticky-note comment to the page.
pub fn add_comment(doc: &mut Document) {
    let mut a = pagebara_syntax::Dict::new();
    a.set(b"Type", Object::name(b"Annot"));
    a.set(b"Subtype", Object::name(b"Text"));
    a.set(
        b"Rect",
        vec![
            Object::Int(10),
            Object::Int(10),
            Object::Int(30),
            Object::Int(30),
        ],
    );
    a.set(b"Contents", Object::string(b"Looks good"));
    let r = doc.add(a);
    let mut page = doc.get(PAGE).unwrap().as_dict().unwrap().clone();
    let mut annots = page.get(b"Annots").unwrap().as_array().unwrap().to_vec();
    annots.push(Object::Ref(r));
    page.set(b"Annots", annots);
    doc.set(PAGE, page);
}

pub fn tool(name: &str) -> Option<PathBuf> {
    let p = PathBuf::from("/usr/bin").join(name);
    p.exists().then_some(p)
}

pub fn scratch(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("sign-tests");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// `qpdf --check` (skipped without qpdf).
pub fn qpdf_check(pdf: &[u8], name: &str) {
    let Some(qpdf) = tool("qpdf") else {
        eprintln!("qpdf not found; skipping");
        return;
    };
    let path = scratch(&format!("{name}.pdf"));
    std::fs::write(&path, pdf).unwrap();
    let out = Command::new(qpdf)
        .arg("--check")
        .arg(&path)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && !text.contains("WARNING"),
        "qpdf --check failed for {name}:\n{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `pdfsig` output (with an empty NSS database of its own), if installed.
pub fn pdfsig(pdf: &[u8], name: &str) -> Option<String> {
    let tool = tool("pdfsig")?;
    let path = scratch(&format!("{name}.pdf"));
    std::fs::write(&path, pdf).unwrap();
    let nss = scratch("nssdb");
    std::fs::create_dir_all(&nss).unwrap();
    let out = Command::new(tool)
        .arg("-nssdir")
        .arg(format!("sql:{}", nss.display()))
        .arg(&path)
        .output()
        .ok()?;
    Some(format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    ))
}

/// The DER signature and the signed bytes of the `n`th signature, for `openssl cms`.
pub fn extract(pdf: &[u8], n: usize) -> (Vec<u8>, Vec<u8>) {
    let doc = Document::open(pdf.to_vec()).unwrap();
    let form = pagebara_forms::Form::load(&doc);
    let sigs: Vec<_> = form
        .signatures(&doc)
        .into_iter()
        .filter(|s| s.signed)
        .collect();
    let br = sigs[n].byte_range.clone().unwrap();
    let mut data = Vec::new();
    for (o, l) in &br.ranges {
        data.extend_from_slice(&pdf[*o..o + l]);
    }
    let (a, c) = (br.ranges[0].1, br.ranges[1].0);
    let hex = &pdf[a + 1..c - 1];
    let mut der: Vec<u8> = hex
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect();
    // Drop the zero padding after the DER element.
    let len = der_len(&der);
    der.truncate(len);
    (der, data)
}

fn der_len(d: &[u8]) -> usize {
    let first = d[1] as usize;
    if first < 0x80 {
        2 + first
    } else {
        let n = first & 0x7F;
        let mut l = 0usize;
        for &b in &d[2..2 + n] {
            l = l << 8 | b as usize;
        }
        2 + n + l
    }
}
