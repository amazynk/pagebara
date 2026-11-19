//! X.509 certificates (RFC 5280), read leniently: only what validation and display need.

use std::fmt;
use std::sync::Arc;

use const_oid::ObjectIdentifier as Oid;

use crate::asn1::{self, Tlv};
use crate::crypto::{self, PublicKey, SignatureScheme};
use crate::time::Time;
use crate::{Error, Result, oid};

/// Key usage bits (RFC 5280 §4.2.1.3), bit `n` for `KeyUsage` bit `n`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct KeyUsage(pub u16);

impl KeyUsage {
    pub const DIGITAL_SIGNATURE: u16 = 1 << 0;
    pub const NON_REPUDIATION: u16 = 1 << 1;
    pub const KEY_ENCIPHERMENT: u16 = 1 << 2;
    pub const KEY_CERT_SIGN: u16 = 1 << 5;
    pub const CRL_SIGN: u16 = 1 << 6;

    pub fn has(self, bit: u16) -> bool {
        self.0 & bit != 0
    }

    /// The DER BIT STRING.
    pub(crate) fn encode(self) -> Vec<u8> {
        let bytes = [
            (self.0 as u8).reverse_bits(),
            ((self.0 >> 8) as u8).reverse_bits(),
        ];
        let len = if bytes[1] != 0 { 2 } else { 1 };
        let last = bytes[len - 1];
        let unused = if last == 0 {
            0
        } else {
            last.trailing_zeros() as u8
        };
        let mut v = vec![unused];
        v.extend_from_slice(&bytes[..len]);
        asn1::tlv(asn1::BIT_STRING, &v)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Extensions {
    ca: Option<bool>,
    path_len: Option<u32>,
    key_usage: Option<KeyUsage>,
    ext_key_usage: Option<Vec<Oid>>,
    subject_key_id: Option<Vec<u8>>,
    authority_key_id: Option<Vec<u8>>,
    ocsp: Vec<String>,
    crl: Vec<String>,
    /// Critical extensions this reader does not understand.
    unknown_critical: Vec<Oid>,
}

#[derive(Debug, PartialEq, Eq)]
struct Inner {
    der: Vec<u8>,
    tbs: Vec<u8>,
    sig_alg: Vec<u8>,
    signature: Vec<u8>,
    serial: Vec<u8>,
    issuer: Vec<u8>,
    subject: Vec<u8>,
    spki: Vec<u8>,
    not_before: Time,
    not_after: Time,
    ext: Extensions,
}

/// An X.509 certificate. Cloning is cheap.
#[derive(Clone, PartialEq, Eq)]
pub struct Certificate(Arc<Inner>);

impl fmt::Debug for Certificate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Certificate({})", self.subject())
    }
}

impl Certificate {
    pub fn from_der(der: &[u8]) -> Result<Certificate> {
        parse(der).ok_or(Error::Malformed("certificate"))
    }

    /// Every certificate in PEM text (`-----BEGIN CERTIFICATE-----` blocks).
    pub fn from_pem(pem: &[u8]) -> Result<Vec<Certificate>> {
        crate::pem::blocks(pem)
            .into_iter()
            .filter(|(label, _)| label == "CERTIFICATE" || label == "TRUSTED CERTIFICATE")
            .map(|(_, der)| Certificate::from_der(&der))
            .collect()
    }

    pub fn to_pem(&self) -> String {
        crate::pem::encode("CERTIFICATE", &self.0.der)
    }

    pub fn der(&self) -> &[u8] {
        &self.0.der
    }

    /// The subject's distinguished name, e.g. `C=DE, O=Example, CN=Ann`.
    pub fn subject(&self) -> String {
        name_to_string(&self.0.subject)
    }

    pub fn issuer(&self) -> String {
        name_to_string(&self.0.issuer)
    }

    /// The subject's common name, else its organisation, else the whole name.
    pub fn display_name(&self) -> String {
        name_attr(&self.0.subject, &oid::COMMON_NAME)
            .or_else(|| name_attr(&self.0.subject, &oid::ORGANIZATION))
            .unwrap_or_else(|| self.subject())
    }

    pub fn issuer_display_name(&self) -> String {
        name_attr(&self.0.issuer, &oid::COMMON_NAME)
            .or_else(|| name_attr(&self.0.issuer, &oid::ORGANIZATION))
            .unwrap_or_else(|| self.issuer())
    }

    pub fn email(&self) -> Option<String> {
        name_attr(&self.0.subject, &oid::EMAIL_ADDRESS)
    }

    /// The serial number in upper-case hex.
    pub fn serial_hex(&self) -> String {
        hex(&self.0.serial)
    }

    pub(crate) fn serial(&self) -> &[u8] {
        &self.0.serial
    }

    pub(crate) fn issuer_raw(&self) -> &[u8] {
        &self.0.issuer
    }

    pub(crate) fn subject_raw(&self) -> &[u8] {
        &self.0.subject
    }

    pub(crate) fn spki(&self) -> &[u8] {
        &self.0.spki
    }

    /// The subjectPublicKey bits (for OCSP key hashes).
    pub(crate) fn public_key_bits(&self) -> Vec<u8> {
        asn1::parse(&self.0.spki)
            .and_then(|(t, _)| t.reader().nth(1))
            .and_then(|b| b.value.get(1..).map(<[u8]>::to_vec))
            .unwrap_or_default()
    }

    pub fn not_before(&self) -> Time {
        self.0.not_before
    }

    pub fn not_after(&self) -> Time {
        self.0.not_after
    }

    pub fn is_valid_at(&self, t: Time) -> bool {
        self.0.not_before <= t && t <= self.0.not_after
    }

    /// A CA certificate (basic constraints `cA`).
    pub fn is_ca(&self) -> bool {
        self.0.ext.ca == Some(true)
    }

    pub fn path_len(&self) -> Option<u32> {
        self.0.ext.path_len
    }

    pub fn key_usage(&self) -> Option<KeyUsage> {
        self.0.ext.key_usage
    }

    pub fn extended_key_usage(&self) -> Option<&[Oid]> {
        self.0.ext.ext_key_usage.as_deref()
    }

    pub(crate) fn has_eku(&self, purpose: &Oid) -> bool {
        self.extended_key_usage()
            .is_some_and(|e| e.contains(purpose) || e.contains(&oid::KP_ANY))
    }

    pub fn subject_key_id(&self) -> Option<&[u8]> {
        self.0.ext.subject_key_id.as_deref()
    }

    pub fn authority_key_id(&self) -> Option<&[u8]> {
        self.0.ext.authority_key_id.as_deref()
    }

    /// OCSP responder URLs (Authority Information Access).
    pub fn ocsp_urls(&self) -> &[String] {
        &self.0.ext.ocsp
    }

    /// CRL distribution point URLs.
    pub fn crl_urls(&self) -> &[String] {
        &self.0.ext.crl
    }

    pub fn unknown_critical_extensions(&self) -> &[Oid] {
        &self.0.ext.unknown_critical
    }

    /// SHA-256 of the DER, upper-case hex.
    pub fn fingerprint(&self) -> String {
        hex(&crypto::sha256(&self.0.der))
    }

    pub fn public_key(&self) -> std::result::Result<PublicKey, String> {
        PublicKey::from_spki(&self.0.spki)
    }

    /// Issuer and subject are the same name.
    pub fn is_self_issued(&self) -> bool {
        names_equal(&self.0.issuer, &self.0.subject)
    }

    /// Whether `issuer` is named as this certificate's issuer (and key identifiers agree).
    pub fn names_issuer(&self, issuer: &Certificate) -> bool {
        if !names_equal(&self.0.issuer, &issuer.0.subject) {
            return false;
        }
        match (self.authority_key_id(), issuer.subject_key_id()) {
            (Some(a), Some(s)) => a == s,
            _ => true,
        }
    }

    /// Checks the certificate's signature with `issuer`'s key.
    pub fn verify_signed_by(&self, issuer: &Certificate) -> std::result::Result<(), String> {
        let key = issuer.public_key()?;
        verify_signed(&self.0.sig_alg, &self.0.tbs, &self.0.signature, &key)
    }

    /// The signature algorithm, for display.
    pub fn signature_algorithm(&self) -> String {
        asn1::parse(&self.0.sig_alg)
            .and_then(|(t, _)| crypto::algorithm(&t))
            .map(
                |(o, p)| match SignatureScheme::from_algorithm(&o, p, None) {
                    Ok(s) => s.name(),
                    Err(_) => o.to_string(),
                },
            )
            .unwrap_or_default()
    }
}

/// Verifies a signature given as an encoded AlgorithmIdentifier over `tbs`.
pub(crate) fn verify_signed(
    sig_alg: &[u8],
    tbs: &[u8],
    signature: &[u8],
    key: &PublicKey,
) -> std::result::Result<(), String> {
    let (alg, _) = asn1::parse(sig_alg).ok_or("bad algorithm")?;
    let (o, params) = crypto::algorithm(&alg).ok_or("bad algorithm")?;
    let scheme = SignatureScheme::from_algorithm(&o, params, None)?;
    key.verify(scheme, tbs, signature)
}

fn parse(der: &[u8]) -> Option<Certificate> {
    let (cert, _) = asn1::parse(der)?;
    let der = cert.raw;
    let mut r = cert.reader();
    let tbs = r.tagged(asn1::SEQUENCE)?;
    let sig_alg = r.tagged(asn1::SEQUENCE)?;
    let sig = r.tagged(asn1::BIT_STRING)?;
    let mut t = tbs.reader();
    let _version = t.tagged(asn1::ctx(0));
    let serial = t.tagged(asn1::INTEGER)?;
    t.tagged(asn1::SEQUENCE)?;
    let issuer = t.tagged(asn1::SEQUENCE)?;
    let validity = t.tagged(asn1::SEQUENCE)?;
    let subject = t.tagged(asn1::SEQUENCE)?;
    let spki = t.tagged(asn1::SEQUENCE)?;
    let mut ext = Extensions::default();
    for f in t {
        if f.tag == asn1::ctx(3)
            && let Some(list) = f.reader().next()
        {
            for e in list.reader() {
                extension(&e, &mut ext);
            }
        }
    }
    let mut v = validity.reader();
    let not_before = Time(v.next()?.time()?);
    let not_after = Time(v.next()?.time()?);
    Some(Certificate(Arc::new(Inner {
        der: der.to_vec(),
        tbs: tbs.raw.to_vec(),
        sig_alg: sig_alg.raw.to_vec(),
        signature: sig.value.get(1..)?.to_vec(),
        serial: serial.value.to_vec(),
        issuer: issuer.raw.to_vec(),
        subject: subject.raw.to_vec(),
        spki: spki.raw.to_vec(),
        not_before,
        not_after,
        ext,
    })))
}

fn extension(e: &Tlv<'_>, ext: &mut Extensions) {
    let mut r = e.reader();
    let Some(id) = r.next().and_then(|t| t.oid()) else {
        return;
    };
    let critical = r
        .tagged(asn1::BOOLEAN)
        .is_some_and(|b| b.value.first().is_some_and(|&v| v != 0));
    let Some(value) = r.tagged(asn1::OCTET_STRING) else {
        return;
    };
    let inner = asn1::parse(value.value).map(|(t, _)| t);
    match id {
        oid::BASIC_CONSTRAINTS => {
            let mut ca = false;
            if let Some(s) = inner {
                for f in s.reader() {
                    match f.tag {
                        asn1::BOOLEAN => ca = f.value.first().is_some_and(|&v| v != 0),
                        asn1::INTEGER => {
                            ext.path_len = f.small_int().and_then(|n| u32::try_from(n).ok())
                        }
                        _ => {}
                    }
                }
            }
            ext.ca = Some(ca);
        }
        oid::KEY_USAGE => {
            if let Some(b) = inner.filter(|b| b.tag == asn1::BIT_STRING) {
                let bytes = b.value.get(1..).unwrap_or_default();
                let mut bits = 0u16;
                for (i, byte) in bytes.iter().take(2).enumerate() {
                    bits |= u16::from(byte.reverse_bits()) << (8 * i);
                }
                ext.key_usage = Some(KeyUsage(bits));
            }
        }
        oid::EXT_KEY_USAGE => {
            ext.ext_key_usage = Some(
                inner
                    .map(|s| s.reader().filter_map(|t| t.oid()).collect())
                    .unwrap_or_default(),
            );
        }
        oid::SUBJECT_KEY_ID => ext.subject_key_id = inner.map(|t| t.value.to_vec()),
        oid::AUTHORITY_KEY_ID => {
            ext.authority_key_id = inner
                .and_then(|s| s.reader().tagged(asn1::ctx_prim(0)))
                .map(|t| t.value.to_vec());
        }
        oid::AUTHORITY_INFO_ACCESS => {
            for ad in inner.iter().flat_map(|s| s.reader()) {
                let mut a = ad.reader();
                if a.next().and_then(|t| t.oid()) == Some(oid::AD_OCSP)
                    && let Some(uri) = a.tagged(asn1::ctx_prim(6))
                {
                    ext.ocsp
                        .push(String::from_utf8_lossy(uri.value).into_owned());
                }
            }
        }
        oid::CRL_DISTRIBUTION_POINTS => {
            if let Some(s) = inner {
                collect_uris(&s, &mut ext.crl, 0);
            }
        }
        // Understood well enough not to stop validation.
        o if [
            "2.5.29.17",
            "2.5.29.18",
            "2.5.29.32",
            "2.5.29.30",
            "2.5.29.36",
            "2.5.29.54",
            "1.3.6.1.5.5.7.1.3",
        ]
        .iter()
        .any(|k| o.to_string() == *k) => {}
        o if critical => ext.unknown_critical.push(o),
        _ => {}
    }
}

fn collect_uris(t: &Tlv<'_>, out: &mut Vec<String>, depth: u32) {
    if t.tag == asn1::ctx_prim(6) {
        out.push(String::from_utf8_lossy(t.value).into_owned());
    } else if t.tag & 0x20 != 0 && depth < 8 {
        for c in t.reader() {
            collect_uris(&c, out, depth + 1);
        }
    }
}

/// RFC 5280 name matching, simplified: byte equality, else the same attributes with
/// case-insensitive, whitespace-collapsed values.
pub(crate) fn names_equal(a: &[u8], b: &[u8]) -> bool {
    a == b || {
        let norm = |n: &[u8]| -> Vec<(Oid, String)> {
            attributes(n)
                .into_iter()
                .map(|(o, v)| {
                    let v = v.split_whitespace().collect::<Vec<_>>().join(" ");
                    (o, v.to_lowercase())
                })
                .collect()
        };
        let (x, y) = (norm(a), norm(b));
        !x.is_empty() && x == y
    }
}

fn attributes(name: &[u8]) -> Vec<(Oid, String)> {
    let Some((n, _)) = asn1::parse(name) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for rdn in n.reader() {
        for atv in rdn.reader() {
            let mut r = atv.reader();
            if let (Some(o), Some(v)) = (r.next().and_then(|t| t.oid()), r.next()) {
                let text = v.text().unwrap_or_else(|| hex(v.value));
                out.push((o, text));
            }
        }
    }
    out
}

fn name_attr(name: &[u8], which: &Oid) -> Option<String> {
    attributes(name)
        .into_iter()
        .rev()
        .find(|(o, _)| o == which)
        .map(|(_, v)| v)
}

fn name_to_string(name: &[u8]) -> String {
    attributes(name)
        .into_iter()
        .map(|(o, v)| format!("{}={v}", short_name(&o)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn short_name(o: &Oid) -> String {
    let s = o.to_string();
    let n = match s.as_str() {
        "2.5.4.3" => "CN",
        "2.5.4.4" => "SN",
        "2.5.4.5" => "serialNumber",
        "2.5.4.6" => "C",
        "2.5.4.7" => "L",
        "2.5.4.8" => "ST",
        "2.5.4.9" => "street",
        "2.5.4.10" => "O",
        "2.5.4.11" => "OU",
        "2.5.4.12" => "title",
        "2.5.4.42" => "GN",
        "2.5.4.97" => "organizationIdentifier",
        "1.2.840.113549.1.9.1" => "E",
        "0.9.2342.19200300.100.1.25" => "DC",
        "0.9.2342.19200300.100.1.1" => "UID",
        _ => return s,
    };
    n.to_string()
}

pub(crate) fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// What a new certificate says; see [`crate::Identity::issue`].
#[derive(Clone, Debug)]
pub struct CertificateRequest {
    pub common_name: String,
    pub organization: Option<String>,
    pub email: Option<String>,
    /// Two-letter country code.
    pub country: Option<String>,
    pub not_before: Time,
    pub not_after: Time,
    /// A CA certificate (may issue others).
    pub ca: bool,
    pub key_usage: KeyUsage,
    pub ext_key_usage: Vec<Oid>,
    pub ocsp_url: Option<String>,
    pub crl_url: Option<String>,
}

impl CertificateRequest {
    /// A document-signing end-entity certificate valid for `days` from now.
    pub fn signer(common_name: &str, days: u32) -> Self {
        let now = Time::now();
        CertificateRequest {
            common_name: common_name.into(),
            organization: None,
            email: None,
            country: None,
            not_before: Time(now.0 - 3600),
            not_after: Time(now.0 + i64::from(days) * 86400),
            ca: false,
            key_usage: KeyUsage(KeyUsage::DIGITAL_SIGNATURE | KeyUsage::NON_REPUDIATION),
            ext_key_usage: vec![oid::KP_EMAIL_PROTECTION, oid::KP_DOCUMENT_SIGNING],
            ocsp_url: None,
            crl_url: None,
        }
    }

    /// A timestamp authority's certificate valid for `days` from now.
    pub fn timestamping(common_name: &str, days: u32) -> Self {
        CertificateRequest {
            key_usage: KeyUsage(KeyUsage::DIGITAL_SIGNATURE),
            ext_key_usage: vec![oid::KP_TIME_STAMPING],
            ..Self::signer(common_name, days)
        }
    }

    /// A CA certificate valid for `days` from now.
    pub fn ca(common_name: &str, days: u32) -> Self {
        CertificateRequest {
            ca: true,
            key_usage: KeyUsage(KeyUsage::KEY_CERT_SIGN | KeyUsage::CRL_SIGN),
            ext_key_usage: Vec::new(),
            ..Self::signer(common_name, days)
        }
    }

    pub(crate) fn name(&self) -> Vec<u8> {
        let rdn = |o: &Oid, v: Vec<u8>| asn1::set_of(vec![asn1::seq(&[&asn1::oid(o), &v])]);
        let mut parts = Vec::new();
        if let Some(c) = &self.country {
            parts.push(rdn(
                &oid::COUNTRY,
                asn1::tlv(asn1::PRINTABLE_STRING, c.as_bytes()),
            ));
        }
        if let Some(o) = &self.organization {
            parts.push(rdn(&oid::ORGANIZATION, asn1::utf8(o)));
        }
        parts.push(rdn(&oid::COMMON_NAME, asn1::utf8(&self.common_name)));
        if let Some(e) = &self.email {
            parts.push(rdn(
                &oid::EMAIL_ADDRESS,
                asn1::tlv(asn1::IA5_STRING, e.as_bytes()),
            ));
        }
        asn1::tlv(asn1::SEQUENCE, &parts.concat())
    }

    /// The TBSCertificate for `spki`, issued under `issuer_name` with `issuer_key_id`.
    pub(crate) fn tbs(
        &self,
        serial: &[u8],
        sig_alg: &[u8],
        issuer_name: &[u8],
        issuer_key_id: Option<&[u8]>,
        spki: &[u8],
    ) -> Vec<u8> {
        let ext = |o: &Oid, critical: bool, v: Vec<u8>| {
            let c = if critical {
                asn1::boolean(true)
            } else {
                Vec::new()
            };
            asn1::seq(&[&asn1::oid(o), &c, &asn1::octets(&v)])
        };
        let key_bits = asn1::parse(spki)
            .and_then(|(t, _)| t.reader().nth(1))
            .map(|b| b.value.get(1..).unwrap_or_default().to_vec())
            .unwrap_or_default();
        let ski = crypto::sha1(&key_bits);
        let mut exts = vec![
            ext(
                &oid::BASIC_CONSTRAINTS,
                true,
                if self.ca {
                    asn1::seq(&[&asn1::boolean(true)])
                } else {
                    asn1::seq(&[])
                },
            ),
            ext(&oid::KEY_USAGE, true, self.key_usage.encode()),
            ext(&oid::SUBJECT_KEY_ID, false, asn1::octets(&ski)),
        ];
        if let Some(k) = issuer_key_id {
            exts.push(ext(
                &oid::AUTHORITY_KEY_ID,
                false,
                asn1::seq(&[&asn1::tlv(asn1::ctx_prim(0), k)]),
            ));
        }
        if !self.ext_key_usage.is_empty() {
            let list: Vec<Vec<u8>> = self.ext_key_usage.iter().map(asn1::oid).collect();
            // RFC 3161 §2.3: a timestamping-only purpose is critical.
            let critical = self.ext_key_usage == [oid::KP_TIME_STAMPING];
            exts.push(ext(
                &oid::EXT_KEY_USAGE,
                critical,
                asn1::tlv(asn1::SEQUENCE, &list.concat()),
            ));
        }
        if let Some(u) = &self.ocsp_url {
            let ad = asn1::seq(&[
                &asn1::oid(&oid::AD_OCSP),
                &asn1::tlv(asn1::ctx_prim(6), u.as_bytes()),
            ]);
            exts.push(ext(&oid::AUTHORITY_INFO_ACCESS, false, asn1::seq(&[&ad])));
        }
        if let Some(u) = &self.crl_url {
            let uri = asn1::tlv(asn1::ctx_prim(6), u.as_bytes());
            let dp = asn1::seq(&[&asn1::explicit(0, &[&asn1::explicit(0, &[&uri])])]);
            exts.push(ext(&oid::CRL_DISTRIBUTION_POINTS, false, asn1::seq(&[&dp])));
        }
        asn1::seq(&[
            &asn1::explicit(0, &[&asn1::int(2)]),
            &asn1::uint(serial),
            sig_alg,
            issuer_name,
            &asn1::seq(&[
                &asn1::x509_time(self.not_before.0),
                &asn1::x509_time(self.not_after.0),
            ]),
            &self.name(),
            spki,
            &asn1::explicit(3, &[&asn1::tlv(asn1::SEQUENCE, &exts.concat())]),
        ])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_usage_bits() {
        for bits in [1u16, 2, 3, 0x21, 0x60, 0x100] {
            let enc = KeyUsage(bits).encode();
            let (t, _) = asn1::parse(&enc).unwrap();
            let bytes = &t.value[1..];
            let mut back = 0u16;
            for (i, b) in bytes.iter().enumerate() {
                back |= u16::from(b.reverse_bits()) << (8 * i);
            }
            assert_eq!(back, bits);
        }
    }
}
