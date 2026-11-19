//! A signing identity: a private key with its certificate and chain.

use crate::cert::{Certificate, CertificateRequest};
use crate::crypto::{self, PrivateKey};
use crate::{Error, Result};

/// A private key, its certificate and the certificates up to (and possibly including) the
/// root, which are embedded in signatures.
#[derive(Clone, Debug)]
pub struct Identity {
    pub key: PrivateKey,
    pub certificate: Certificate,
    /// Issuer certificates, nearest first.
    pub chain: Vec<Certificate>,
}

impl Identity {
    /// From a PKCS #12 file (`.p12`, `.pfx`) and its password (any Unicode).
    pub fn from_pkcs12(data: &[u8], password: &str) -> Result<Identity> {
        let c = crate::pkcs12::read(data, password)?;
        let chain = order_chain(&c.certificate, c.others);
        Ok(Identity {
            key: c.key,
            certificate: c.certificate,
            chain,
        })
    }

    /// From PEM: a private key (PKCS #8, encrypted PKCS #8 with `password`, PKCS #1 or
    /// SEC 1) and certificates (the key's own and its chain, in any order).
    pub fn from_pem(key_pem: &[u8], certs_pem: &[u8], password: Option<&str>) -> Result<Identity> {
        let blocks = crate::pem::blocks(key_pem);
        let (label, der) = blocks
            .iter()
            .find(|(l, _)| l.ends_with("PRIVATE KEY"))
            .ok_or(Error::Key("no private key in the PEM text".into()))?;
        let key = if label == "ENCRYPTED PRIVATE KEY" {
            let pw = password.ok_or(Error::WrongPassword)?;
            let info = pkcs8::EncryptedPrivateKeyInfoRef::try_from(der.as_slice())
                .map_err(|e| Error::Key(e.to_string()))?;
            let plain = info
                .decrypt(pw.as_bytes())
                .map_err(|_| Error::WrongPassword)?;
            PrivateKey::from_der(plain.as_bytes())?
        } else {
            PrivateKey::from_der(der)?
        };
        let certs = Certificate::from_pem(certs_pem)?;
        let public = key.public_key_der()?;
        let pos = certs
            .iter()
            .position(|c| c.spki() == public)
            .ok_or(Error::Key("no certificate matches the private key".into()))?;
        let mut certs = certs;
        let certificate = certs.remove(pos);
        let chain = order_chain(&certificate, certs);
        Ok(Identity {
            key,
            certificate,
            chain,
        })
    }

    /// A new self-signed identity, like the "digital ID" readers create for people without
    /// a certificate. Others see its signatures as untrusted until they add it.
    pub fn self_signed(req: &CertificateRequest, key: PrivateKey) -> Result<Identity> {
        let spki = key.public_key_der()?;
        let ski = key_id(&spki);
        let cert = sign_certificate(req, &key, &req.name(), Some(&ski), &spki)?;
        Ok(Identity {
            key,
            certificate: cert,
            chain: Vec::new(),
        })
    }

    /// Issues a certificate for `subject_key` signed by this identity (which should be a
    /// CA). The new identity's chain is this one's certificate and chain.
    pub fn issue(&self, req: &CertificateRequest, subject_key: PrivateKey) -> Result<Identity> {
        let spki = subject_key.public_key_der()?;
        let issuer_ski = self
            .certificate
            .subject_key_id()
            .map(<[u8]>::to_vec)
            .unwrap_or_else(|| key_id(self.certificate.spki()));
        let cert = sign_certificate(
            req,
            &self.key,
            self.certificate.subject_raw(),
            Some(&issuer_ski),
            &spki,
        )?;
        let mut chain = vec![self.certificate.clone()];
        chain.extend(self.chain.iter().cloned());
        Ok(Identity {
            key: subject_key,
            certificate: cert,
            chain,
        })
    }

    /// A PKCS #12 file protecting the key with `password`.
    pub fn to_pkcs12(&self, password: &str, friendly_name: Option<&str>) -> Result<Vec<u8>> {
        crate::pkcs12::write(
            &self.key,
            &self.certificate,
            &self.chain,
            password,
            friendly_name,
        )
    }
}

/// The SHA-1 key identifier (RFC 5280 §4.2.1.2 method 1).
fn key_id(spki: &[u8]) -> Vec<u8> {
    let bits = crate::asn1::parse(spki)
        .and_then(|(t, _)| t.reader().nth(1))
        .map(|b| b.value.get(1..).unwrap_or_default().to_vec())
        .unwrap_or_default();
    crypto::sha1(&bits)
}

fn sign_certificate(
    req: &CertificateRequest,
    issuer_key: &PrivateKey,
    issuer_name: &[u8],
    issuer_key_id: Option<&[u8]>,
    spki: &[u8],
) -> Result<Certificate> {
    let scheme = issuer_key.scheme(issuer_key.default_digest(), false);
    let alg = scheme.algorithm_id();
    let mut serial = crypto::random::<16>()?;
    serial[0] &= 0x7F;
    serial[0] |= 0x01;
    let tbs = req.tbs(&serial, &alg, issuer_name, issuer_key_id, spki);
    let sig = issuer_key.sign_prehash(scheme, &scheme.digest().digest(&[&tbs]))?;
    let der = crate::asn1::seq(&[&tbs, &alg, &crate::asn1::bit_string(&sig)]);
    Certificate::from_der(&der)
}

/// `others` ordered as a chain from `leaf` upwards; certificates outside the chain follow.
pub(crate) fn order_chain(leaf: &Certificate, mut others: Vec<Certificate>) -> Vec<Certificate> {
    let mut chain = Vec::new();
    let mut cur = leaf.clone();
    while let Some(i) = others.iter().position(|c| cur.names_issuer(c) && *c != cur) {
        let next = others.remove(i);
        chain.push(next.clone());
        if next.is_self_issued() {
            break;
        }
        cur = next;
    }
    chain.extend(others);
    chain
}
