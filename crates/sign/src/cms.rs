//! CMS SignedData (RFC 5652): reading what a signature holds, checking a signer, and
//! building detached signatures and timestamp tokens.

use const_oid::ObjectIdentifier as Oid;

use crate::asn1::{self, Tlv};
use crate::cert::Certificate;
use crate::crypto::{self, DigestAlgorithm, PrivateKey, SignatureScheme};
use crate::time::Time;
use crate::{Error, Result, oid};

/// How a signer names its certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum SignerId {
    IssuerSerial { issuer: Vec<u8>, serial: Vec<u8> },
    KeyId(Vec<u8>),
}

#[derive(Clone, Debug)]
pub(crate) struct SignerInfo<'a> {
    pub sid: SignerId,
    pub digest: std::result::Result<DigestAlgorithm, Oid>,
    /// The `[0]` signed attributes as received.
    pub signed_attrs: Option<Tlv<'a>>,
    pub sig_alg: Tlv<'a>,
    pub signature: Vec<u8>,
    pub unsigned_attrs: Option<Tlv<'a>>,
}

#[derive(Clone, Debug)]
pub(crate) struct SignedData<'a> {
    pub content_type: Oid,
    pub content: Option<Vec<u8>>,
    pub certificates: Vec<Certificate>,
    pub crls: Vec<Vec<u8>>,
    pub signers: Vec<SignerInfo<'a>>,
}

/// Reads a ContentInfo holding SignedData.
pub(crate) fn parse(der: &[u8]) -> Result<SignedData<'_>> {
    let bad = Error::Malformed("CMS signature");
    let (ci, _) = asn1::parse(der).ok_or(bad.clone())?;
    let mut r = ci.reader();
    if r.next().and_then(|t| t.oid()) != Some(oid::SIGNED_DATA) {
        return Err(Error::Malformed("not CMS SignedData"));
    }
    let sd = r
        .tagged(asn1::ctx(0))
        .and_then(|t| t.reader().next())
        .ok_or(bad.clone())?;
    let mut s = sd.reader();
    s.tagged(asn1::INTEGER).ok_or(bad.clone())?;
    s.tagged(asn1::SET).ok_or(bad.clone())?;
    let eci = s.tagged(asn1::SEQUENCE).ok_or(bad.clone())?;
    let mut e = eci.reader();
    let content_type = e.next().and_then(|t| t.oid()).ok_or(bad.clone())?;
    let content = e
        .tagged(asn1::ctx(0))
        .and_then(|t| t.reader().next())
        .map(|o| o.octets().into_owned());
    let mut certificates = Vec::new();
    if let Some(certs) = s.tagged(asn1::ctx(0)) {
        // Other certificate formats (attribute certificates) are skipped.
        for c in certs.reader().filter(|c| c.tag == asn1::SEQUENCE) {
            if let Ok(c) = Certificate::from_der(c.raw) {
                certificates.push(c);
            }
        }
    }
    let crls = s
        .tagged(asn1::ctx(1))
        .map(|c| {
            c.reader()
                .filter(|c| c.tag == asn1::SEQUENCE)
                .map(|c| c.raw.to_vec())
                .collect()
        })
        .unwrap_or_default();
    let infos = s.tagged(asn1::SET).ok_or(bad.clone())?;
    let signers = infos
        .reader()
        .map(|si| signer_info(&si).ok_or(bad.clone()))
        .collect::<Result<Vec<_>>>()?;
    Ok(SignedData {
        content_type,
        content,
        certificates,
        crls,
        signers,
    })
}

fn signer_info<'a>(t: &Tlv<'a>) -> Option<SignerInfo<'a>> {
    let mut r = t.reader();
    r.tagged(asn1::INTEGER)?;
    let sid = match r.next()? {
        s if s.tag == asn1::SEQUENCE => {
            let mut i = s.reader();
            let issuer = i.tagged(asn1::SEQUENCE)?.raw.to_vec();
            let serial = i.tagged(asn1::INTEGER)?.uint().to_vec();
            SignerId::IssuerSerial { issuer, serial }
        }
        s if s.tag == asn1::ctx_prim(0) => SignerId::KeyId(s.value.to_vec()),
        _ => return None,
    };
    let digest_alg = r.tagged(asn1::SEQUENCE)?;
    let digest_oid = crypto::algorithm(&digest_alg)?.0;
    let digest = DigestAlgorithm::from_oid(&digest_oid).ok_or(digest_oid);
    let signed_attrs = r.tagged(asn1::ctx(0));
    let sig_alg = r.tagged(asn1::SEQUENCE)?;
    let signature = r.tagged(asn1::OCTET_STRING)?.octets().into_owned();
    let unsigned_attrs = r.tagged(asn1::ctx(1));
    Some(SignerInfo {
        sid,
        digest,
        signed_attrs,
        sig_alg,
        signature,
        unsigned_attrs,
    })
}

/// The first value of attribute `which` in an attribute set.
pub(crate) fn attribute<'a>(attrs: Option<&Tlv<'a>>, which: &Oid) -> Option<Tlv<'a>> {
    attributes(attrs, which).into_iter().next()
}

/// All values of attribute `which`.
pub(crate) fn attributes<'a>(attrs: Option<&Tlv<'a>>, which: &Oid) -> Vec<Tlv<'a>> {
    let Some(attrs) = attrs else {
        return Vec::new();
    };
    attrs
        .reader()
        .filter_map(|a| {
            let mut r = a.reader();
            (r.next()?.oid()? == *which).then(|| r.next())?
        })
        .flat_map(|values| values.reader())
        .collect()
}

/// The outcome of checking one signer against its content.
#[derive(Clone, Debug)]
pub(crate) struct SignerCheck {
    pub scheme: std::result::Result<SignatureScheme, String>,
    /// The content's hash matches (the messageDigest attribute, when present).
    pub digest_ok: bool,
    /// The signature value verifies with the certificate's key.
    pub signature: std::result::Result<(), String>,
    /// The signing-certificate(-v2) attribute names this certificate (`None`: absent).
    pub signing_certificate: Option<bool>,
}

impl SignerInfo<'_> {
    /// The certificate this signer names, among `certs`.
    pub fn find_certificate<'c>(&self, certs: &'c [Certificate]) -> Option<&'c Certificate> {
        certs.iter().find(|c| match &self.sid {
            SignerId::IssuerSerial { issuer, serial } => {
                crate::cert::names_equal(issuer, c.issuer_raw()) && strip(c.serial()) == &serial[..]
            }
            SignerId::KeyId(k) => c.subject_key_id() == Some(k),
        })
    }

    pub fn scheme(&self) -> std::result::Result<SignatureScheme, String> {
        let (o, params) = crypto::algorithm(&self.sig_alg).ok_or("bad signature algorithm")?;
        SignatureScheme::from_algorithm(&o, params, self.digest.ok())
    }

    /// Checks the signer over content whose hash (with this signer's digest) is
    /// `content_hash`.
    pub fn check(&self, cert: &Certificate, content_hash: &[u8]) -> SignerCheck {
        let scheme = self.scheme();
        let mut out = SignerCheck {
            scheme: scheme.clone(),
            digest_ok: false,
            signature: Err("not checked".into()),
            signing_certificate: None,
        };
        let Ok(scheme) = scheme else {
            out.signature = Err(scheme.err().unwrap_or_default());
            return out;
        };
        let key = match cert.public_key() {
            Ok(k) => k,
            Err(e) => {
                out.signature = Err(e);
                return out;
            }
        };
        let digest = match self.digest {
            Ok(d) => d,
            Err(o) => {
                out.signature = Err(format!("unsupported digest {o}"));
                return out;
            }
        };
        match &self.signed_attrs {
            Some(attrs) => {
                let md = attribute(Some(attrs), &oid::MESSAGE_DIGEST);
                out.digest_ok = md.is_some_and(|m| *m.octets() == *content_hash);
                let der = attrs.retagged(asn1::SET);
                let h = scheme.digest().digest(&[&der]);
                out.signature = key.verify_prehash(scheme, &h, &self.signature);
                out.signing_certificate = signing_certificate_matches(attrs, cert);
            }
            None => {
                out.digest_ok = true;
                let h = if scheme.digest() == digest {
                    content_hash.to_vec()
                } else {
                    // Without attributes the content is signed directly with the scheme's hash.
                    out.digest_ok = false;
                    Vec::new()
                };
                out.signature = key.verify_prehash(scheme, &h, &self.signature);
            }
        }
        out
    }

    /// The signing-time attribute.
    pub fn signing_time(&self) -> Option<Time> {
        attribute(self.signed_attrs.as_ref(), &oid::SIGNING_TIME)
            .and_then(|t| t.time())
            .map(Time)
    }

    /// The RFC 3161 signature timestamp token (unsigned attribute).
    pub fn timestamp_token(&self) -> Option<Tlv<'_>> {
        attribute(self.unsigned_attrs.as_ref(), &oid::SIGNATURE_TIMESTAMP)
    }
}

fn strip(b: &[u8]) -> &[u8] {
    let mut v = b;
    while v.len() > 1 && v[0] == 0 {
        v = &v[1..];
    }
    v
}

/// Whether ESS signing-certificate(-v2) names `cert` (`None` when neither is present).
fn signing_certificate_matches(attrs: &Tlv<'_>, cert: &Certificate) -> Option<bool> {
    let (v2, v) = (
        attribute(Some(attrs), &oid::SIGNING_CERTIFICATE_V2),
        attribute(Some(attrs), &oid::SIGNING_CERTIFICATE),
    );
    let (sc, v2) = match (v2, v) {
        (Some(s), _) => (s, true),
        (None, Some(s)) => (s, false),
        _ => return None,
    };
    // The first ESSCertID names the signing certificate.
    let id = sc.reader().next().and_then(|certs| certs.reader().next());
    let Some(id) = id else { return Some(false) };
    let mut r = id.reader();
    let digest = if v2 {
        match r.tagged(asn1::SEQUENCE) {
            Some(a) => crypto::algorithm(&a).and_then(|(o, _)| DigestAlgorithm::from_oid(&o)),
            None => Some(DigestAlgorithm::Sha256),
        }
    } else {
        Some(DigestAlgorithm::Sha1)
    };
    let hash = r.tagged(asn1::OCTET_STRING);
    Some(match (digest, hash) {
        (Some(d), Some(h)) => d.digest(&[cert.der()]) == h.value,
        _ => false,
    })
}

// Building.

/// One attribute: `SEQUENCE { type, SET { value } }`.
pub(crate) fn attr(o: &Oid, value: &[u8]) -> Vec<u8> {
    asn1::seq(&[&asn1::oid(o), &asn1::set_of(vec![value.to_vec()])])
}

/// Receives the signature value and returns unsigned attributes to add.
pub(crate) type Unsigned<'a> = &'a mut dyn FnMut(&[u8]) -> Result<Vec<Vec<u8>>>;

/// A signer and what to put in a SignedData.
pub(crate) struct Builder<'a> {
    pub key: &'a PrivateKey,
    pub cert: &'a Certificate,
    /// Further certificates to embed (the chain).
    pub chain: &'a [Certificate],
    pub digest: DigestAlgorithm,
    pub pss: bool,
    pub content_type: Oid,
    /// Encapsulated content (timestamp tokens); `None` for a detached signature.
    pub content: Option<&'a [u8]>,
    /// The content's hash under `digest`.
    pub content_hash: &'a [u8],
    pub signing_time: Option<Time>,
    /// Add ESS signing-certificate-v2 (CAdES, timestamp tokens).
    pub signing_certificate_v2: bool,
}

impl Builder<'_> {
    /// The encoded ContentInfo. `unsigned` receives the signature value and returns
    /// unsigned attributes to add (e.g. a timestamp token).
    pub fn build(&self, unsigned: Unsigned<'_>) -> Result<Vec<u8>> {
        let mut attrs = vec![
            attr(&oid::CONTENT_TYPE, &asn1::oid(&self.content_type)),
            attr(&oid::MESSAGE_DIGEST, &asn1::octets(self.content_hash)),
        ];
        if let Some(t) = self.signing_time {
            attrs.push(attr(&oid::SIGNING_TIME, &asn1::x509_time(t.0)));
        }
        if self.signing_certificate_v2 {
            attrs.push(attr(&oid::SIGNING_CERTIFICATE_V2, &self.ess_cert_id_v2()));
        }
        let signed = asn1::set_of(attrs);
        let scheme = self.key.scheme(self.digest, self.pss);
        let hash = scheme.digest().digest(&[&signed]);
        let signature = self.key.sign_prehash(scheme, &hash)?;
        let unsigned = unsigned(&signature)?;
        let mut signed_ctx = signed.clone();
        signed_ctx[0] = asn1::ctx(0);
        let sid = asn1::seq(&[self.cert.issuer_raw(), &asn1::uint(self.cert.serial())]);
        let unsigned_part = if unsigned.is_empty() {
            Vec::new()
        } else {
            let mut u = asn1::set_of(unsigned);
            u[0] = asn1::ctx(1);
            u
        };
        let signer = asn1::seq(&[
            &asn1::int(1),
            &sid,
            &self.digest.algorithm_id(),
            &signed_ctx,
            &scheme.algorithm_id(),
            &asn1::octets(&signature),
            &unsigned_part,
        ]);
        let mut certs: Vec<Vec<u8>> = vec![self.cert.der().to_vec()];
        for c in self.chain {
            if !certs.iter().any(|d| d == c.der()) {
                certs.push(c.der().to_vec());
            }
        }
        let mut cert_set = asn1::set_of(certs);
        cert_set[0] = asn1::ctx(0);
        let econtent = match self.content {
            Some(c) => asn1::explicit(0, &[&asn1::octets(c)]),
            None => Vec::new(),
        };
        let sd = asn1::seq(&[
            &asn1::int(if self.content_type == oid::DATA { 1 } else { 3 }),
            &asn1::set_of(vec![self.digest.algorithm_id()]),
            &asn1::seq(&[&asn1::oid(&self.content_type), &econtent]),
            &cert_set,
            &asn1::set_of(vec![signer]),
        ]);
        Ok(asn1::seq(&[
            &asn1::oid(&oid::SIGNED_DATA),
            &asn1::explicit(0, &[&sd]),
        ]))
    }

    fn ess_cert_id_v2(&self) -> Vec<u8> {
        let hash = crypto::sha256(self.cert.der());
        let general_names = asn1::seq(&[&asn1::explicit(4, &[self.cert.issuer_raw()])]);
        let issuer_serial = asn1::seq(&[&general_names, &asn1::uint(self.cert.serial())]);
        let id = asn1::seq(&[&asn1::octets(&hash), &issuer_serial]);
        asn1::seq(&[&asn1::seq(&[&id])])
    }
}
