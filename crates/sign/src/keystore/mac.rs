//! The macOS keychain through the Security framework: identities (certificates with a
//! private key), issuers from the keychain's certificates, and signing with
//! `SecKeyCreateSignature`, so keys that cannot be exported (Secure Enclave, smart cards
//! through CryptoTokenKit) work.

use std::sync::Arc;

use security_framework::identity::SecIdentity;
use security_framework::item::{ItemClass, ItemSearchOptions, Limit, Reference, SearchResult};
use security_framework::key::Algorithm;

use crate::cert::Certificate;
use crate::crypto::{DigestAlgorithm, PrivateKey, SignatureScheme, Signer};
use crate::identity::Identity;
use crate::{Error, Result};

/// `errSecItemNotFound`: the search found nothing.
const NOT_FOUND: i32 = -25300;

fn search(class: ItemClass) -> Result<Vec<Reference>> {
    let found = ItemSearchOptions::new()
        .class(class)
        .load_refs(true)
        .limit(Limit::All)
        .search();
    match found {
        Ok(r) => Ok(r
            .into_iter()
            .filter_map(|r| match r {
                SearchResult::Ref(r) => Some(r),
                _ => None,
            })
            .collect()),
        Err(e) if e.code() == NOT_FOUND => Ok(Vec::new()),
        Err(e) => Err(Error::Key(format!("the keychain: {e}"))),
    }
}

pub fn identities() -> Result<Vec<Identity>> {
    let pool: Vec<Certificate> = search(ItemClass::certificate())?
        .into_iter()
        .filter_map(|r| match r {
            Reference::Certificate(c) => Certificate::from_der(&c.to_der()).ok(),
            _ => None,
        })
        .collect();
    let mut out = Vec::new();
    for r in search(ItemClass::identity())? {
        let Reference::Identity(identity) = r else {
            continue;
        };
        let Some(certificate) = identity
            .certificate()
            .ok()
            .and_then(|c| Certificate::from_der(&c.to_der()).ok())
        else {
            continue;
        };
        let signer = KeychainKey {
            spki: certificate.spki().to_vec(),
            identity,
        };
        let Ok(key) = PrivateKey::external(Arc::new(signer)) else {
            continue;
        };
        let chain = super::issuers(&certificate, &pool);
        out.push(Identity {
            key,
            certificate,
            chain,
        });
    }
    Ok(out)
}

/// The private key of a keychain identity.
struct KeychainKey {
    identity: SecIdentity,
    spki: Vec<u8>,
}

impl Signer for KeychainKey {
    fn public_key_der(&self) -> Result<Vec<u8>> {
        Ok(self.spki.clone())
    }

    fn location(&self) -> String {
        "macOS Keychain".into()
    }

    fn sign_prehash(&self, scheme: SignatureScheme, hash: &[u8]) -> Result<Vec<u8>> {
        use DigestAlgorithm as D;
        let algorithm = match scheme {
            SignatureScheme::RsaPkcs1(d) => match d {
                D::Sha1 => Algorithm::RSASignatureDigestPKCS1v15SHA1,
                D::Sha224 => Algorithm::RSASignatureDigestPKCS1v15SHA224,
                D::Sha256 => Algorithm::RSASignatureDigestPKCS1v15SHA256,
                D::Sha384 => Algorithm::RSASignatureDigestPKCS1v15SHA384,
                D::Sha512 => Algorithm::RSASignatureDigestPKCS1v15SHA512,
            },
            // The Security framework salts PSS with the digest's length, as we do.
            SignatureScheme::RsaPss { digest, .. } => match digest {
                D::Sha1 => Algorithm::RSASignatureDigestPSSSHA1,
                D::Sha224 => Algorithm::RSASignatureDigestPSSSHA224,
                D::Sha256 => Algorithm::RSASignatureDigestPSSSHA256,
                D::Sha384 => Algorithm::RSASignatureDigestPSSSHA384,
                D::Sha512 => Algorithm::RSASignatureDigestPSSSHA512,
            },
            SignatureScheme::Ecdsa(d) => match d {
                D::Sha1 => Algorithm::ECDSASignatureDigestX962SHA1,
                D::Sha224 => Algorithm::ECDSASignatureDigestX962SHA224,
                D::Sha256 => Algorithm::ECDSASignatureDigestX962SHA256,
                D::Sha384 => Algorithm::ECDSASignatureDigestX962SHA384,
                D::Sha512 => Algorithm::ECDSASignatureDigestX962SHA512,
            },
        };
        let key = self
            .identity
            .private_key()
            .map_err(|e| Error::Key(format!("the keychain has no usable key: {e}")))?;
        key.create_signature(algorithm, hash)
            .map_err(|e| Error::Key(format!("signing failed: {e}")))
    }
}
