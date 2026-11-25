//! Signing identities from the operating system's key store: the Windows certificate store
//! (the user's "Personal" certificates) and the macOS keychain. Their private keys stay
//! there, often unexportable or on a smart card, and sign through a [`crate::Signer`]; the
//! system may ask the user for a PIN or permission when one signs. Other systems have no
//! such store and list none.

#[cfg(target_os = "macos")]
mod mac;
#[cfg(windows)]
mod win;

use crate::Result;
use crate::cert::Certificate;
use crate::identity::Identity;

/// The identities (certificate with a private key) of the system's key store, each with
/// the issuer certificates the store knows.
pub fn system_identities() -> Result<Vec<Identity>> {
    #[cfg(windows)]
    return win::identities();
    #[cfg(target_os = "macos")]
    return mac::identities();
    #[cfg(not(any(windows, target_os = "macos")))]
    Ok(Vec::new())
}

/// The root certificates the system's certificate store trusts (Windows). Empty where the
/// roots are files instead, which [`crate::TrustStore::load_system`] reads itself.
pub(crate) fn system_roots() -> Vec<Certificate> {
    #[cfg(windows)]
    return win::roots();
    #[cfg(not(windows))]
    Vec::new()
}

/// The issuers of `leaf` found in `pool`, nearest first.
#[cfg_attr(not(any(windows, target_os = "macos")), allow(dead_code))]
fn issuers(leaf: &Certificate, pool: &[Certificate]) -> Vec<Certificate> {
    let mut chain: Vec<Certificate> = Vec::new();
    let mut cur = leaf.clone();
    while !cur.is_self_issued() && chain.len() < 16 {
        let Some(next) = pool
            .iter()
            .find(|c| cur.names_issuer(c) && cur.verify_signed_by(c).is_ok() && !chain.contains(c))
        else {
            break;
        };
        chain.push(next.clone());
        cur = next.clone();
    }
    chain
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CertificateRequest, PrivateKey};

    #[test]
    fn issuers_in_order() {
        let root = Identity::self_signed(
            &CertificateRequest::ca("Root", 10),
            PrivateKey::generate_p256().unwrap(),
        )
        .unwrap();
        let mid = root
            .issue(
                &CertificateRequest::ca("Mid", 10),
                PrivateKey::generate_p256().unwrap(),
            )
            .unwrap();
        let leaf = mid
            .issue(
                &CertificateRequest::signer("Leaf", 10),
                PrivateKey::generate_p256().unwrap(),
            )
            .unwrap();
        let other = Identity::self_signed(
            &CertificateRequest::ca("Other", 10),
            PrivateKey::generate_p256().unwrap(),
        )
        .unwrap();
        let pool = [
            other.certificate.clone(),
            root.certificate.clone(),
            mid.certificate.clone(),
        ];
        assert_eq!(
            issuers(&leaf.certificate, &pool),
            [mid.certificate, root.certificate]
        );
        assert!(system_identities().is_ok());
    }

    /// Key stores answering `r ‖ s` (Windows CNG) give the same DER as our own keys.
    #[test]
    fn raw_ecdsa_to_der() {
        let key = PrivateKey::generate_p256().unwrap();
        let hash = crate::crypto::sha256(b"data");
        let der = key
            .sign_prehash(
                crate::SignatureScheme::Ecdsa(crate::DigestAlgorithm::Sha256),
                &hash,
            )
            .unwrap();
        let (seq, _) = crate::asn1::parse(&der).unwrap();
        let mut raw = Vec::new();
        for i in seq.reader() {
            let v = i.uint();
            raw.extend(std::iter::repeat_n(0, 32 - v.len()));
            raw.extend_from_slice(v);
        }
        assert_eq!(crate::crypto::ecdsa_der(&raw), der);
    }
}
