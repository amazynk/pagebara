//! Revocation information: CRLs (RFC 5280 §5) and OCSP responses (RFC 6960). Checking
//! them, building OCSP requests for a fetcher, and issuing both (tests, self-hosted CAs).

use crate::asn1::{self, Tlv};
use crate::cert::{self, Certificate};
use crate::crypto::{self, DigestAlgorithm};
use crate::identity::Identity;
use crate::time::Time;
use crate::{Result, oid};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevocationSource {
    Ocsp,
    Crl,
}

/// What revocation data says about a certificate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevocationStatus {
    /// Not revoked as of `at` (OCSP production or CRL issue time).
    Good {
        source: RevocationSource,
        at: Time,
    },
    Revoked {
        source: RevocationSource,
        at: Time,
    },
    /// No usable data: none embedded, or none for this certificate.
    Unknown,
}

/// Raw CRLs and OCSP responses gathered from a document.
#[derive(Clone, Debug, Default)]
pub(crate) struct RevocationData {
    pub crls: Vec<Vec<u8>>,
    /// Full `OCSPResponse`s or bare `BasicOCSPResponse`s.
    pub ocsps: Vec<Vec<u8>>,
    /// Certificates delegated OCSP responders may come with.
    pub certs: Vec<Certificate>,
}

impl RevocationData {
    /// The status of `cert` issued by `issuer`. OCSP answers win over CRLs; a revocation
    /// found anywhere wins over both.
    pub fn status(&self, cert: &Certificate, issuer: &Certificate) -> RevocationStatus {
        let mut good = None;
        for o in &self.ocsps {
            match ocsp_status(o, cert, issuer, &self.certs) {
                Some(s @ RevocationStatus::Revoked { .. }) => return s,
                Some(s @ RevocationStatus::Good { .. }) => good = good.or(Some(s)),
                _ => {}
            }
        }
        for c in &self.crls {
            match crl_status(c, cert, issuer) {
                Some(s @ RevocationStatus::Revoked { .. }) => return s,
                Some(s @ RevocationStatus::Good { .. }) => good = good.or(Some(s)),
                _ => {}
            }
        }
        good.unwrap_or(RevocationStatus::Unknown)
    }
}

/// The status a CRL gives `cert`, if the CRL is `issuer`'s and its signature verifies.
fn crl_status(der: &[u8], cert: &Certificate, issuer: &Certificate) -> Option<RevocationStatus> {
    let (crl, _) = asn1::parse(der)?;
    let mut r = crl.reader();
    let tbs = r.tagged(asn1::SEQUENCE)?;
    let alg = r.tagged(asn1::SEQUENCE)?;
    let sig = r.tagged(asn1::BIT_STRING)?;
    let mut t = tbs.reader();
    t.tagged(asn1::INTEGER);
    t.tagged(asn1::SEQUENCE)?;
    let name = t.tagged(asn1::SEQUENCE)?;
    if !cert::names_equal(name.raw, cert.issuer_raw()) {
        return None;
    }
    let this_update = t.next()?.time()?;
    let _next_update = t
        .tagged(asn1::UTC_TIME)
        .or_else(|| t.tagged(asn1::GENERALIZED_TIME));
    let key = issuer.public_key().ok()?;
    cert::verify_signed(alg.raw, tbs.raw, sig.value.get(1..)?, &key).ok()?;
    if let Some(list) = t.tagged(asn1::SEQUENCE) {
        for entry in list.reader() {
            let mut e = entry.reader();
            let serial = e.tagged(asn1::INTEGER)?;
            if serial.uint() == strip(cert.serial()) {
                let at = e.next().and_then(|d| d.time()).unwrap_or(this_update);
                return Some(RevocationStatus::Revoked {
                    source: RevocationSource::Crl,
                    at: Time(at),
                });
            }
        }
    }
    Some(RevocationStatus::Good {
        source: RevocationSource::Crl,
        at: Time(this_update),
    })
}

fn strip(b: &[u8]) -> &[u8] {
    let mut v = b;
    while v.len() > 1 && v[0] == 0 {
        v = &v[1..];
    }
    v
}

/// The `BasicOCSPResponse` inside an `OCSPResponse` (or the input, if already one).
fn basic_response(der: &[u8]) -> Option<Tlv<'_>> {
    let (t, _) = asn1::parse(der)?;
    let mut r = t.reader();
    match r.peek_tag()? {
        asn1::ENUMERATED => {
            if r.next()?.small_int()? != 0 {
                return None;
            }
            let rb = r.tagged(asn1::ctx(0))?.reader().next()?;
            let mut b = rb.reader();
            if b.next()?.oid()? != oid::OCSP_BASIC {
                return None;
            }
            let octets = b.tagged(asn1::OCTET_STRING)?;
            Some(asn1::parse(octets.value)?.0)
        }
        _ => Some(t),
    }
}

fn ocsp_status(
    der: &[u8],
    cert: &Certificate,
    issuer: &Certificate,
    extra: &[Certificate],
) -> Option<RevocationStatus> {
    let basic = basic_response(der)?;
    let mut r = basic.reader();
    let tbs = r.tagged(asn1::SEQUENCE)?;
    let alg = r.tagged(asn1::SEQUENCE)?;
    let sig = r.tagged(asn1::BIT_STRING)?;
    let certs: Vec<Certificate> = r
        .tagged(asn1::ctx(0))
        .and_then(|c| c.reader().next())
        .map(|s| {
            s.reader()
                .filter_map(|c| Certificate::from_der(c.raw).ok())
                .collect()
        })
        .unwrap_or_default();
    let mut t = tbs.reader();
    t.tagged(asn1::ctx(0));
    let _responder = t.next()?;
    let produced = t.tagged(asn1::GENERALIZED_TIME)?.time()?;
    let responses = t.tagged(asn1::SEQUENCE)?;
    // The response is signed by the issuer itself or by a responder it delegated to.
    let sig = sig.value.get(1..)?;
    let signed_by = |c: &Certificate| {
        c.public_key()
            .is_ok_and(|k| cert::verify_signed(alg.raw, tbs.raw, sig, &k).is_ok())
    };
    let delegated = |c: &Certificate| {
        c.has_eku(&oid::KP_OCSP_SIGNING)
            && c.names_issuer(issuer)
            && c.verify_signed_by(issuer).is_ok()
    };
    let trusted = signed_by(issuer)
        || certs
            .iter()
            .chain(extra)
            .any(|c| delegated(c) && signed_by(c));
    if !trusted {
        return None;
    }
    let key_bits = issuer.public_key_bits();
    for single in responses.reader() {
        let mut s = single.reader();
        let id = s.tagged(asn1::SEQUENCE)?;
        let mut i = id.reader();
        let digest = i
            .tagged(asn1::SEQUENCE)
            .and_then(|a| crypto::algorithm(&a))
            .and_then(|(o, _)| DigestAlgorithm::from_oid(&o))?;
        let _name_hash = i.tagged(asn1::OCTET_STRING)?;
        let key_hash = i.tagged(asn1::OCTET_STRING)?;
        let serial = i.tagged(asn1::INTEGER)?;
        if serial.uint() != strip(cert.serial()) || key_hash.value != digest.digest(&[&key_bits]) {
            continue;
        }
        let status = s.next()?;
        return Some(match status.tag {
            0x80 => RevocationStatus::Good {
                source: RevocationSource::Ocsp,
                at: Time(produced),
            },
            0xA1 => RevocationStatus::Revoked {
                source: RevocationSource::Ocsp,
                at: Time(
                    status
                        .reader()
                        .next()
                        .and_then(|t| t.time())
                        .unwrap_or(produced),
                ),
            },
            _ => RevocationStatus::Unknown,
        });
    }
    None
}

fn cert_id(cert: &Certificate, issuer: &Certificate) -> Vec<u8> {
    let d = DigestAlgorithm::Sha1;
    asn1::seq(&[
        &asn1::algorithm(&d.oid(), Some(&asn1::null())),
        &asn1::octets(&d.digest(&[issuer.subject_raw()])),
        &asn1::octets(&d.digest(&[&issuer.public_key_bits()])),
        &asn1::uint(cert.serial()),
    ])
}

/// A DER `OCSPRequest` for `cert`, to POST (`application/ocsp-request`) to one of
/// [`Certificate::ocsp_urls`].
pub fn ocsp_request(cert: &Certificate, issuer: &Certificate) -> Vec<u8> {
    let req = asn1::seq(&[&cert_id(cert, issuer)]);
    asn1::seq(&[&asn1::seq(&[&asn1::seq(&[&req])])])
}

/// A CRL issued by `ca` listing `revoked` certificates with their revocation times.
pub fn issue_crl(
    ca: &Identity,
    revoked: &[(&Certificate, Time)],
    this_update: Time,
    next_update: Time,
) -> Result<Vec<u8>> {
    let scheme = ca.key.scheme(ca.key.default_digest(), false);
    let alg = scheme.algorithm_id();
    let entries: Vec<Vec<u8>> = revoked
        .iter()
        .map(|(c, t)| asn1::seq(&[&asn1::uint(c.serial()), &asn1::x509_time(t.0)]))
        .collect();
    let list = if entries.is_empty() {
        Vec::new()
    } else {
        asn1::tlv(asn1::SEQUENCE, &entries.concat())
    };
    let tbs = asn1::seq(&[
        &asn1::int(1),
        &alg,
        ca.certificate.subject_raw(),
        &asn1::x509_time(this_update.0),
        &asn1::x509_time(next_update.0),
        &list,
    ]);
    let sig = ca
        .key
        .sign_prehash(scheme, &scheme.digest().digest(&[&tbs]))?;
    Ok(asn1::seq(&[&tbs, &alg, &asn1::bit_string(&sig)]))
}

/// An OCSP response from `responder` (the issuing CA, or a delegated responder whose
/// chain starts at the CA) about `cert`: good, or revoked at the given time.
pub fn issue_ocsp_response(
    responder: &Identity,
    cert: &Certificate,
    issuer: &Certificate,
    revoked: Option<Time>,
    produced: Time,
) -> Result<Vec<u8>> {
    let status = match revoked {
        None => vec![0x80, 0],
        Some(t) => asn1::explicit(1, &[&asn1::generalized_time(t.0)]),
    };
    let single = asn1::seq(&[
        &cert_id(cert, issuer),
        &status,
        &asn1::generalized_time(produced.0),
    ]);
    let tbs = asn1::seq(&[
        &asn1::explicit(1, &[responder.certificate.subject_raw()]),
        &asn1::generalized_time(produced.0),
        &asn1::seq(&[&single]),
    ]);
    let scheme = responder.key.scheme(responder.key.default_digest(), false);
    let alg = scheme.algorithm_id();
    let sig = responder
        .key
        .sign_prehash(scheme, &scheme.digest().digest(&[&tbs]))?;
    let basic = asn1::seq(&[
        &tbs,
        &alg,
        &asn1::bit_string(&sig),
        &asn1::explicit(0, &[&asn1::seq(&[responder.certificate.der()])]),
    ]);
    Ok(asn1::seq(&[
        &asn1::tlv(asn1::ENUMERATED, &[0]),
        &asn1::explicit(
            0,
            &[&asn1::seq(&[
                &asn1::oid(&oid::OCSP_BASIC),
                &asn1::octets(&basic),
            ])],
        ),
    ]))
}
