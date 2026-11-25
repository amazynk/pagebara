//! The Windows certificate store through CryptoAPI and CNG (NCrypt): certificates of the
//! "MY" store with a private key, issuers from "CA" and "ROOT", and signing with
//! `NCryptSignHash`, so keys that cannot be exported (TPM, smart cards) work. Also the
//! "ROOT" store on its own, as the system's trust anchors.
//!
//! The only unsafe code of the crate: FFI calls into crypt32 and ncrypt. Every pointer
//! passed is either a certificate context this module holds a reference on, or a local.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::sync::Arc;

use windows::Win32::Security::Cryptography::{
    BCRYPT_PAD_PKCS1, BCRYPT_PAD_PSS, BCRYPT_PKCS1_PADDING_INFO, BCRYPT_PSS_PADDING_INFO,
    BCRYPT_SHA1_ALGORITHM, BCRYPT_SHA256_ALGORITHM, BCRYPT_SHA384_ALGORITHM,
    BCRYPT_SHA512_ALGORITHM, CERT_CONTEXT, CERT_KEY_PROV_INFO_PROP_ID, CERT_KEY_SPEC,
    CRYPT_ACQUIRE_COMPARE_KEY_FLAG, CRYPT_ACQUIRE_ONLY_NCRYPT_KEY_FLAG, CertCloseStore,
    CertDuplicateCertificateContext, CertEnumCertificatesInStore, CertFreeCertificateContext,
    CertGetCertificateContextProperty, CertOpenSystemStoreW, CryptAcquireCertificatePrivateKey,
    HCRYPTPROV_OR_NCRYPT_KEY_HANDLE, NCRYPT_FLAGS, NCRYPT_HANDLE, NCRYPT_KEY_HANDLE,
    NCryptFreeObject, NCryptSignHash,
};
use windows::core::{BOOL, PCWSTR, w};

use crate::cert::Certificate;
use crate::crypto::{DigestAlgorithm, PrivateKey, SignatureScheme, Signer};
use crate::identity::Identity;
use crate::{Error, Result};

/// A certificate context with a reference held on it.
struct Context(*const CERT_CONTEXT);

// SAFETY: certificate contexts are reference counted and immutable once in a store;
// CryptoAPI allows using them from any thread.
unsafe impl Send for Context {}
unsafe impl Sync for Context {}

impl Drop for Context {
    fn drop(&mut self) {
        // SAFETY: we own one reference, taken with CertDuplicateCertificateContext.
        unsafe {
            let _ = CertFreeCertificateContext(Some(self.0));
        }
    }
}

impl Context {
    fn der(&self) -> &[u8] {
        // SAFETY: the context is alive while `self` is; its encoded bytes do not change.
        unsafe {
            let c = &*self.0;
            std::slice::from_raw_parts(c.pbCertEncoded, c.cbCertEncoded as usize)
        }
    }

    /// Whether the store knows a private key for the certificate.
    fn has_key(&self) -> bool {
        let mut n = 0u32;
        // SAFETY: a size query on a live context into a local.
        unsafe {
            CertGetCertificateContextProperty(self.0, CERT_KEY_PROV_INFO_PROP_ID, None, &mut n)
        }
        .is_ok()
    }
}

/// The certificates of the current user's system store `name`.
fn store(name: PCWSTR) -> Vec<Context> {
    // SAFETY: `name` is a static wide string.
    let Ok(store) = (unsafe { CertOpenSystemStoreW(None, name) }) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut prev: Option<*const CERT_CONTEXT> = None;
    loop {
        // SAFETY: `prev` came from the previous call; enumeration releases it.
        let c = unsafe { CertEnumCertificatesInStore(store, prev) };
        if c.is_null() {
            break;
        }
        // SAFETY: `c` is valid until the next enumeration call; keep our own reference.
        out.push(Context(unsafe { CertDuplicateCertificateContext(Some(c)) }));
        prev = Some(c);
    }
    // SAFETY: the store handle is ours; the contexts keep their own references.
    unsafe {
        let _ = CertCloseStore(Some(store), 0);
    }
    out
}

/// The root certificates this user's Windows trusts: the "ROOT" store, which also shows
/// the computer's roots and those set by the organisation.
pub fn roots() -> Vec<Certificate> {
    store(w!("ROOT"))
        .iter()
        .filter_map(|c| Certificate::from_der(c.der()).ok())
        .collect()
}

pub fn identities() -> Result<Vec<Identity>> {
    let pool: Vec<Certificate> = [w!("CA"), w!("ROOT")]
        .into_iter()
        .flat_map(store)
        .filter_map(|c| Certificate::from_der(c.der()).ok())
        .collect();
    let mut out = Vec::new();
    for ctx in store(w!("MY")) {
        if !ctx.has_key() {
            continue;
        }
        let Ok(certificate) = Certificate::from_der(ctx.der()) else {
            continue;
        };
        let signer = StoreKey {
            spki: certificate.spki().to_vec(),
            ctx,
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

/// The private key of a certificate in the store.
struct StoreKey {
    ctx: Context,
    spki: Vec<u8>,
}

impl Signer for StoreKey {
    fn public_key_der(&self) -> Result<Vec<u8>> {
        Ok(self.spki.clone())
    }

    fn location(&self) -> String {
        "Windows certificate store".into()
    }

    fn sign_prehash(&self, scheme: SignatureScheme, hash: &[u8]) -> Result<Vec<u8>> {
        let mut handle = HCRYPTPROV_OR_NCRYPT_KEY_HANDLE::default();
        let mut spec = CERT_KEY_SPEC::default();
        let mut free = BOOL::default();
        // SAFETY: a live context and out-pointers to locals.
        unsafe {
            CryptAcquireCertificatePrivateKey(
                self.ctx.0,
                CRYPT_ACQUIRE_ONLY_NCRYPT_KEY_FLAG | CRYPT_ACQUIRE_COMPARE_KEY_FLAG,
                None,
                &mut handle,
                Some(&mut spec),
                Some(&mut free),
            )
        }
        .map_err(|e| Error::Key(format!("the certificate store has no usable key: {e}")))?;
        let result = sign(NCRYPT_KEY_HANDLE(handle.0), scheme, hash);
        if free.as_bool() {
            // SAFETY: the handle is ours to free, and no longer used.
            unsafe {
                let _ = NCryptFreeObject(NCRYPT_HANDLE(handle.0));
            }
        }
        result
    }
}

fn algorithm(d: DigestAlgorithm) -> Result<PCWSTR> {
    Ok(match d {
        DigestAlgorithm::Sha1 => BCRYPT_SHA1_ALGORITHM,
        DigestAlgorithm::Sha256 => BCRYPT_SHA256_ALGORITHM,
        DigestAlgorithm::Sha384 => BCRYPT_SHA384_ALGORITHM,
        DigestAlgorithm::Sha512 => BCRYPT_SHA512_ALGORITHM,
        DigestAlgorithm::Sha224 => {
            return Err(Error::Unsupported("SHA-224 with Windows keys".into()));
        }
    })
}

fn sign(key: NCRYPT_KEY_HANDLE, scheme: SignatureScheme, hash: &[u8]) -> Result<Vec<u8>> {
    let pkcs1;
    let pss;
    let (info, flags): (Option<*const c_void>, NCRYPT_FLAGS) = match scheme {
        SignatureScheme::RsaPkcs1(d) => {
            pkcs1 = BCRYPT_PKCS1_PADDING_INFO {
                pszAlgId: algorithm(d)?,
            };
            (
                Some((&raw const pkcs1).cast()),
                NCRYPT_FLAGS(BCRYPT_PAD_PKCS1.0),
            )
        }
        SignatureScheme::RsaPss { digest, salt } => {
            pss = BCRYPT_PSS_PADDING_INFO {
                pszAlgId: algorithm(digest)?,
                cbSalt: salt as u32,
            };
            (
                Some((&raw const pss).cast()),
                NCRYPT_FLAGS(BCRYPT_PAD_PSS.0),
            )
        }
        SignatureScheme::Ecdsa(_) => (None, NCRYPT_FLAGS(0)),
    };
    let err = |e: windows::core::Error| Error::Key(format!("signing failed: {e}"));
    let mut len = 0u32;
    // SAFETY: the padding info lives on this frame; sizes and buffers are locals.
    unsafe { NCryptSignHash(key, info, hash, None, &mut len, flags) }.map_err(err)?;
    let mut sig = vec![0; len as usize];
    // SAFETY: as above, with a buffer of the size asked for.
    unsafe { NCryptSignHash(key, info, hash, Some(&mut sig), &mut len, flags) }.map_err(err)?;
    sig.truncate(len as usize);
    Ok(match scheme {
        // CNG returns r and s side by side.
        SignatureScheme::Ecdsa(_) => crate::crypto::ecdsa_der(&sig),
        _ => sig,
    })
}
