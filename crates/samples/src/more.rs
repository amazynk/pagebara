//! Samples for signatures, redaction and text recognition: documents signed with test
//! identities made on the spot (never anyone's real keys), personal data to redact, and a
//! page that is only a picture of text.

use std::fmt::Write as _;
use std::sync::atomic::AtomicBool;

use pagebara_sign::{
    Appearance, CertificateRequest, Identity, LocalTimestampAuthority, MdpPermissions, PrivateKey,
    SignOptions, sign,
};
use pagebara_syntax::Document;

use super::{Page, Pdf, build, lease};

/// A test CA, a signer it issued, and a timestamp authority it issued.
#[allow(missing_debug_implementations)]
pub struct TestPki {
    pub ca: Identity,
    pub signer: Identity,
    pub tsa: LocalTimestampAuthority,
}

pub fn pki() -> TestPki {
    let key = || PrivateKey::generate_p256().expect("key");
    let ca = Identity::self_signed(&CertificateRequest::ca("pagebara Test CA", 3650), key())
        .expect("CA");
    let mut req = CertificateRequest::signer("Ann Example", 3650);
    req.email = Some("ann@example.org".into());
    req.organization = Some("Example Lettings Ltd".into());
    let signer = ca.issue(&req, key()).expect("signer");
    let tsa = ca
        .issue(
            &CertificateRequest::timestamping("pagebara Test Timestamps", 3650),
            key(),
        )
        .expect("TSA");
    TestPki {
        ca,
        signer,
        tsa: LocalTimestampAuthority::new(tsa),
    }
}

/// The lease, signed visibly on its last page with a timestamp.
pub fn signed(p: &TestPki) -> Vec<u8> {
    let doc = Document::open(lease()).expect("lease");
    let look = Appearance {
        page: 3,
        rect: [320.0, 90.0, 520.0, 150.0],
        image: None,
        text: true,
    };
    let opts = SignOptions {
        appearance: Some(&look),
        reason: Some("I agree to the terms"),
        location: Some("Portsmouth"),
        timestamp: Some(&p.tsa),
        ..Default::default()
    };
    sign(&doc, &p.signer, &opts).expect("sign")
}

/// The lease certified so that no changes are allowed.
pub fn certified(p: &TestPki) -> Vec<u8> {
    let doc = Document::open(lease()).expect("lease");
    let opts = SignOptions {
        certify: Some(MdpPermissions::NoChanges),
        reason: Some("Final version"),
        ..Default::default()
    };
    sign(&doc, &p.signer, &opts).expect("certify")
}

/// A letter full of personal data to redact.
pub fn personal() -> Vec<u8> {
    let mut p = Page::new(595.0, 842.0);
    p.heading("Customer record", 18.0);
    p.para("Name: Jane Doe. Date of birth: 4 March 1987.", 12.0);
    p.para(
        "E-mail: jane.doe@example.org (private), work: j.doe@northwind.example",
        12.0,
    );
    p.para("Phone: +44 20 7946 0958", 12.0);
    p.para("IBAN: GB82 WEST 1234 5698 7654 32", 12.0);
    p.para("Card: 4111 1111 1111 1111", 12.0);
    p.para(
        "Notes: Jane asked us to send the contract by post. Her e-mail address above must not be shared with partners.",
        12.0,
    );
    p.footer("Confidential");
    build(
        vec![p],
        &[("Title", "Customer record"), ("Author", "Jane Doe")],
        |pdf, _, _| {
            // A bookmark naming an address the redaction removes ("@" written in hex).
            let title: String = "Mail to jane.doe@example.org"
                .bytes()
                .map(|b| format!("{b:02X}"))
                .collect();
            let root = pdf.reserve();
            let item = pdf.add(format!(
                "<</Title<{title}>/Parent {root} 0 R/Dest[@0/Fit]>>"
            ));
            pdf.set(
                root,
                format!("<</Type/Outlines/First {item} 0 R/Last {item} 0 R/Count 1>>"),
            );
            format!("/Outlines {root} 0 R")
        },
    )
}

/// A memo on a portrait page shown in landscape (`/Rotate 90`), blank where it is signed.
pub fn rotated() -> Vec<u8> {
    let mut p = Page::new(612.0, 792.0);
    p.heading("Rotated memo", 18.0);
    p.para(
        "This page is turned a quarter for reading in landscape.",
        12.0,
    );
    let bytes = build(vec![p], &[("Title", "Rotated memo")], |_, _, _| {
        String::new()
    });
    let mut doc = Document::open(bytes).expect("memo");
    let first = doc.catalog().and_then(|c| {
        let pages = doc.get_in(c.as_dict()?, b"Pages");
        let kids = doc.get_in(pages.as_dict()?, b"Kids");
        kids.as_array()?.first()?.as_objref()
    });
    let page = first.expect("a page");
    let mut d = doc
        .get(page)
        .and_then(|o| o.as_dict().cloned())
        .expect("page");
    d.set(b"Rotate", 90);
    doc.set(page, d);
    doc.save_full().expect("save")
}

/// A page of large, plain text rendered to a grey image: pixels, width, height.
pub(crate) fn scan_image() -> (Vec<u8>, usize, usize) {
    let mut p = Page::new(612.0, 792.0);
    p.heading("Greenhouse delivery note", 26.0);
    p.para("Order 4711 for Northwind Garden Supplies", 18.0);
    p.para("Twelve tomato plants and four bags of compost", 18.0);
    p.para("Delivered on Tuesday morning", 18.0);
    let page = build(vec![p], &[("Title", "Delivery note")], |_, _, _| {
        String::new()
    });
    let renderer = pagebara_render::Renderer::new(Document::open(page).expect("page"));
    let (img, _) =
        pagebara_ocr::render_page(&renderer, 0, 150.0, &AtomicBool::new(false)).expect("render");
    let gray: Vec<u8> = img
        .data
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| ((u16::from(p[0]) + u16::from(p[1]) + u16::from(p[2])) / 3) as u8)
        .collect();
    (gray, img.width, img.height)
}

/// A PDF whose one page of `w` × `h` points shows only a grey image.
fn image_page(gray: &[u8], iw: usize, ih: usize, w: u32, h: u32) -> Vec<u8> {
    let packed = pagebara_filters::encode_flate(gray, 9);
    let mut hex = String::with_capacity(packed.len() * 2 + 1);
    for b in packed {
        let _ = write!(hex, "{b:02X}");
    }
    hex.push('>');
    let mut pdf = Pdf::default();
    let catalog = pdf.add("<</Type/Catalog/Pages 2 0 R>>".into());
    let tree = pdf.add("<</Type/Pages/Kids[3 0 R]/Count 1>>".into());
    let page = pdf.add(format!(
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 {w} {h}]/Resources<</XObject<</Scan 5 0 R>>>>/Contents 4 0 R>>"
    ));
    let contents = pdf.stream(&format!("q {w} 0 0 {h} 0 0 cm /Scan Do Q"));
    let image = pdf.add(format!(
        "<</Type/XObject/Subtype/Image/Width {iw}/Height {ih}/ColorSpace/DeviceGray/BitsPerComponent 8/Filter[/ASCIIHexDecode/FlateDecode]/Length {}>>stream\n{hex}\nendstream",
        hex.len()
    ));
    assert_eq!((catalog, tree, page, contents, image), (1, 2, 3, 4, 5));
    let info = pdf.add("<</Title(Scanned delivery note)/Producer(pagebara samples)>>".into());
    pdf.finish(catalog, info)
}

/// A page that is only a picture of text.
pub fn scan() -> Vec<u8> {
    let (gray, w, h) = scan_image();
    image_page(&gray, w, h, 612, 792)
}

/// The same picture scanned sideways (turned a quarter counter-clockwise) and stored that
/// way, without `/Rotate` to show it upright.
pub fn scan_turned() -> Vec<u8> {
    let (gray, w, h) = scan_image();
    let mut out = vec![0; w * h];
    for y in 0..h {
        for x in 0..w {
            out[(w - 1 - x) * h + y] = gray[y * w + x];
        }
    }
    image_page(&out, h, w, 792, 612)
}

/// Every sample of this module: file name and bytes.
pub fn files() -> Vec<(&'static str, Vec<u8>)> {
    let p = pki();
    let mut tampered = signed(&p);
    // Change "1,250" in the signed rent clause to "9,250".
    if let Some(at) = tampered.windows(5).position(|w| w == b"1,250") {
        tampered[at] = b'9';
    }
    vec![
        ("signed.pdf", signed(&p)),
        ("signed-tampered.pdf", tampered),
        ("certified.pdf", certified(&p)),
        ("test-ca.pem", p.ca.certificate.to_pem().into_bytes()),
        ("personal.pdf", personal()),
        ("scan.pdf", scan()),
        ("rotated.pdf", rotated()),
        ("scan-turned.pdf", scan_turned()),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagebara_sign::{Status, TrustSource, TrustStore, VerifyOptions, verify};

    #[test]
    fn signed_samples_verify_and_tampering_shows() {
        let p = pki();
        let mut trust = TrustStore::new();
        trust.add(p.ca.certificate.clone(), TrustSource::User);
        let opts = VerifyOptions::new(&trust);
        let r = verify(&Document::open(signed(&p)).unwrap(), &opts);
        assert_eq!(r[0].status, Status::ValidTrusted, "{:?}", r[0].problems);
        assert!(r[0].timestamp.as_ref().is_some_and(|t| t.valid));
        let r = verify(&Document::open(certified(&p)).unwrap(), &opts);
        assert_eq!(r[0].status, Status::ValidTrusted);
        let files = files();
        let tampered = &files
            .iter()
            .find(|f| f.0 == "signed-tampered.pdf")
            .unwrap()
            .1;
        let r = verify(&Document::open(tampered.clone()).unwrap(), &opts);
        assert_eq!(r[0].status, Status::Invalid);
    }

    #[test]
    fn scan_has_no_text_but_an_image() {
        let d = Document::open(scan()).unwrap();
        assert_eq!(pagebara_ocr::text_chars(&d, 0), 0);
        let r = pagebara_pages::PageTree::load(&d).pages()[0].r;
        assert!(pagebara_app_core::ocr::looks_scanned(&d, r, 0));
        let hits = pagebara_redact::find(
            &Document::open(personal()).unwrap(),
            &pagebara_redact::Pattern::Kind(pagebara_redact::Kind::Email),
            None,
        );
        assert_eq!(hits.len(), 2);
    }
}
