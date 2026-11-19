//! Digests, public-key signature verification and private keys.

use const_oid::ObjectIdentifier as Oid;
use ecdsa::signature::hazmat::{PrehashSigner, PrehashVerifier};
use rsa::pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey};
use rsa::pkcs8::{DecodePrivateKey, EncodePrivateKey, EncodePublicKey};
use rsa::{Pkcs1v15Sign, Pss};
use sha1::Sha1;
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512};

use crate::asn1::{self, Reader, Tlv};
use crate::oid;
use crate::{Error, Result};

/// A hash function used by signatures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DigestAlgorithm {
    Sha1,
    Sha224,
    Sha256,
    Sha384,
    Sha512,
}

impl DigestAlgorithm {
    pub fn from_oid(o: &Oid) -> Option<Self> {
        Some(match *o {
            oid::SHA1 => Self::Sha1,
            oid::SHA224 => Self::Sha224,
            oid::SHA256 => Self::Sha256,
            oid::SHA384 => Self::Sha384,
            oid::SHA512 => Self::Sha512,
            _ => return None,
        })
    }

    pub fn oid(self) -> Oid {
        match self {
            Self::Sha1 => oid::SHA1,
            Self::Sha224 => oid::SHA224,
            Self::Sha256 => oid::SHA256,
            Self::Sha384 => oid::SHA384,
            Self::Sha512 => oid::SHA512,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Sha1 => "SHA-1",
            Self::Sha224 => "SHA-224",
            Self::Sha256 => "SHA-256",
            Self::Sha384 => "SHA-384",
            Self::Sha512 => "SHA-512",
        }
    }

    /// The hash of the concatenation of `parts`.
    pub fn digest(self, parts: &[&[u8]]) -> Vec<u8> {
        fn run<D: Digest>(parts: &[&[u8]]) -> Vec<u8> {
            let mut h = D::new();
            for p in parts {
                h.update(p);
            }
            h.finalize().to_vec()
        }
        match self {
            Self::Sha1 => run::<Sha1>(parts),
            Self::Sha224 => run::<Sha224>(parts),
            Self::Sha256 => run::<Sha256>(parts),
            Self::Sha384 => run::<Sha384>(parts),
            Self::Sha512 => run::<Sha512>(parts),
        }
    }

    /// The AlgorithmIdentifier, parameters absent (RFC 5754).
    pub(crate) fn algorithm_id(self) -> Vec<u8> {
        asn1::algorithm(&self.oid(), None)
    }

    /// SHA-1 cannot be trusted for new signatures (collisions are practical).
    pub fn is_weak(self) -> bool {
        self == Self::Sha1
    }
}

pub(crate) fn sha1(b: &[u8]) -> Vec<u8> {
    DigestAlgorithm::Sha1.digest(&[b])
}

pub(crate) fn sha256(b: &[u8]) -> Vec<u8> {
    DigestAlgorithm::Sha256.digest(&[b])
}

/// How a signature value is computed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureScheme {
    RsaPkcs1(DigestAlgorithm),
    RsaPss {
        digest: DigestAlgorithm,
        salt: usize,
    },
    Ecdsa(DigestAlgorithm),
}

impl SignatureScheme {
    pub fn digest(self) -> DigestAlgorithm {
        match self {
            Self::RsaPkcs1(d) | Self::Ecdsa(d) | Self::RsaPss { digest: d, .. } => d,
        }
    }

    pub fn name(self) -> String {
        match self {
            Self::RsaPkcs1(d) => format!("RSA PKCS#1 v1.5 with {}", d.name()),
            Self::RsaPss { digest, .. } => format!("RSA-PSS with {}", digest.name()),
            Self::Ecdsa(d) => format!("ECDSA with {}", d.name()),
        }
    }

    /// From an AlgorithmIdentifier. `digest` comes from the CMS signer info, for the bare
    /// key algorithms (`rsaEncryption`, `id-ecPublicKey`) some signers name.
    pub(crate) fn from_algorithm(
        alg: &Oid,
        params: Option<Tlv<'_>>,
        digest: Option<DigestAlgorithm>,
    ) -> std::result::Result<Self, String> {
        let need = || digest.ok_or_else(|| format!("no digest for {alg}"));
        Ok(match *alg {
            oid::RSA_ENCRYPTION => Self::RsaPkcs1(need()?),
            oid::SHA1_WITH_RSA => Self::RsaPkcs1(DigestAlgorithm::Sha1),
            oid::SHA224_WITH_RSA => Self::RsaPkcs1(DigestAlgorithm::Sha224),
            oid::SHA256_WITH_RSA => Self::RsaPkcs1(DigestAlgorithm::Sha256),
            oid::SHA384_WITH_RSA => Self::RsaPkcs1(DigestAlgorithm::Sha384),
            oid::SHA512_WITH_RSA => Self::RsaPkcs1(DigestAlgorithm::Sha512),
            oid::RSASSA_PSS => pss_params(params)?,
            oid::EC_PUBLIC_KEY => Self::Ecdsa(need()?),
            oid::ECDSA_SHA1 => Self::Ecdsa(DigestAlgorithm::Sha1),
            oid::ECDSA_SHA224 => Self::Ecdsa(DigestAlgorithm::Sha224),
            oid::ECDSA_SHA256 => Self::Ecdsa(DigestAlgorithm::Sha256),
            oid::ECDSA_SHA384 => Self::Ecdsa(DigestAlgorithm::Sha384),
            oid::ECDSA_SHA512 => Self::Ecdsa(DigestAlgorithm::Sha512),
            oid::MD5_WITH_RSA => return Err("MD5 signatures are not accepted".into()),
            oid::ED25519 => return Err("Ed25519 signatures are not supported".into()),
            ref o => return Err(format!("unsupported signature algorithm {o}")),
        })
    }

    /// The AlgorithmIdentifier written for this scheme.
    pub(crate) fn algorithm_id(self) -> Vec<u8> {
        let null = asn1::null();
        match self {
            Self::RsaPkcs1(d) => {
                let o = match d {
                    DigestAlgorithm::Sha1 => oid::SHA1_WITH_RSA,
                    DigestAlgorithm::Sha224 => oid::SHA224_WITH_RSA,
                    DigestAlgorithm::Sha256 => oid::SHA256_WITH_RSA,
                    DigestAlgorithm::Sha384 => oid::SHA384_WITH_RSA,
                    DigestAlgorithm::Sha512 => oid::SHA512_WITH_RSA,
                };
                asn1::algorithm(&o, Some(&null))
            }
            Self::RsaPss { digest, salt } => {
                let h = asn1::algorithm(&digest.oid(), None);
                let mgf = asn1::algorithm(&oid::MGF1, Some(&h));
                let params = asn1::seq(&[
                    &asn1::explicit(0, &[&h]),
                    &asn1::explicit(1, &[&mgf]),
                    &asn1::explicit(2, &[&asn1::int(salt as u64)]),
                ]);
                asn1::algorithm(&oid::RSASSA_PSS, Some(&params))
            }
            Self::Ecdsa(d) => {
                let o = match d {
                    DigestAlgorithm::Sha1 => oid::ECDSA_SHA1,
                    DigestAlgorithm::Sha224 => oid::ECDSA_SHA224,
                    DigestAlgorithm::Sha256 => oid::ECDSA_SHA256,
                    DigestAlgorithm::Sha384 => oid::ECDSA_SHA384,
                    DigestAlgorithm::Sha512 => oid::ECDSA_SHA512,
                };
                asn1::algorithm(&o, None)
            }
        }
    }
}

/// RSASSA-PSS-params (RFC 4055): SHA-1 and salt 20 unless given.
fn pss_params(params: Option<Tlv<'_>>) -> std::result::Result<SignatureScheme, String> {
    let mut digest = DigestAlgorithm::Sha1;
    let mut salt = 20;
    if let Some(p) = params.filter(|p| p.tag == asn1::SEQUENCE) {
        for f in p.reader() {
            let inner = f.reader().next();
            match f.tag {
                0xA0 => {
                    let alg = inner.and_then(|a| a.reader().next()?.oid());
                    digest = alg
                        .as_ref()
                        .and_then(DigestAlgorithm::from_oid)
                        .ok_or("unsupported PSS digest")?;
                }
                0xA1 => {
                    // MGF1 must use the same hash: that is all the RSA backend supports.
                    let mgf_hash = inner
                        .and_then(|a| a.reader().nth(1))
                        .and_then(|h| h.reader().next()?.oid());
                    if let Some(h) = mgf_hash
                        && DigestAlgorithm::from_oid(&h) != Some(digest)
                    {
                        return Err("PSS with a different MGF1 hash is not supported".into());
                    }
                }
                0xA2 => {
                    salt = inner
                        .and_then(|i| i.small_int())
                        .and_then(|s| usize::try_from(s).ok())
                        .ok_or("bad PSS salt length")?;
                }
                _ => {}
            }
        }
    }
    Ok(SignatureScheme::RsaPss { digest, salt })
}

/// A public key from a SubjectPublicKeyInfo.
#[derive(Clone, Debug)]
pub enum PublicKey {
    Rsa(rsa::PublicKey),
    P256(p256::ecdsa::VerifyingKey),
    P384(p384::ecdsa::VerifyingKey),
}

impl PublicKey {
    pub fn from_spki(der: &[u8]) -> std::result::Result<Self, String> {
        let (spki, _) = asn1::parse(der).ok_or("bad public key")?;
        let mut r = spki.reader();
        let alg = r.tagged(asn1::SEQUENCE).ok_or("bad public key")?;
        let bits = r.tagged(asn1::BIT_STRING).ok_or("bad public key")?;
        let key = bits.value.get(1..).ok_or("bad public key")?;
        let mut ar = alg.reader();
        let alg_oid = ar.next().and_then(|t| t.oid()).ok_or("bad public key")?;
        match alg_oid {
            oid::RSA_ENCRYPTION | oid::RSASSA_PSS => rsa::PublicKey::from_pkcs1_der(key)
                .map(Self::Rsa)
                .map_err(|e| format!("RSA key: {e}")),
            oid::EC_PUBLIC_KEY => match ar.next().and_then(|t| t.oid()) {
                Some(oid::P256) => p256::ecdsa::VerifyingKey::from_sec1_bytes(key)
                    .map(Self::P256)
                    .map_err(|_| "bad P-256 key".into()),
                Some(oid::P384) => p384::ecdsa::VerifyingKey::from_sec1_bytes(key)
                    .map(Self::P384)
                    .map_err(|_| "bad P-384 key".into()),
                c => Err(format!("unsupported curve {c:?}")),
            },
            o => Err(format!("unsupported key algorithm {o}")),
        }
    }

    /// E.g. "RSA 2048" or "ECDSA P-256".
    pub fn description(&self) -> String {
        match self {
            Self::Rsa(k) => format!("RSA {}", rsa::traits::PublicKeyParts::size(k) * 8),
            Self::P256(_) => "ECDSA P-256".into(),
            Self::P384(_) => "ECDSA P-384".into(),
        }
    }

    /// Checks `sig` over the already hashed `hash`.
    pub fn verify_prehash(
        &self,
        scheme: SignatureScheme,
        hash: &[u8],
        sig: &[u8],
    ) -> std::result::Result<(), String> {
        fn bad<E>(_: E) -> String {
            "signature does not match".into()
        }
        match (self, scheme) {
            (Self::Rsa(k), SignatureScheme::RsaPkcs1(d)) => {
                k.verify(pkcs1(d), hash, sig).map_err(bad)
            }
            (Self::Rsa(k), SignatureScheme::RsaPss { digest, salt }) => {
                let r = match digest {
                    DigestAlgorithm::Sha1 => k.verify(Pss::<Sha1>::new_with_salt(salt), hash, sig),
                    DigestAlgorithm::Sha224 => {
                        k.verify(Pss::<Sha224>::new_with_salt(salt), hash, sig)
                    }
                    DigestAlgorithm::Sha256 => {
                        k.verify(Pss::<Sha256>::new_with_salt(salt), hash, sig)
                    }
                    DigestAlgorithm::Sha384 => {
                        k.verify(Pss::<Sha384>::new_with_salt(salt), hash, sig)
                    }
                    DigestAlgorithm::Sha512 => {
                        k.verify(Pss::<Sha512>::new_with_salt(salt), hash, sig)
                    }
                };
                r.map_err(bad)
            }
            (Self::P256(k), SignatureScheme::Ecdsa(_)) => {
                let s = p256::ecdsa::DerSignature::from_bytes(sig).map_err(bad)?;
                k.verify_prehash(hash, &s).map_err(bad)
            }
            (Self::P384(k), SignatureScheme::Ecdsa(_)) => {
                let s = p384::ecdsa::DerSignature::from_bytes(sig).map_err(bad)?;
                k.verify_prehash(hash, &s).map_err(bad)
            }
            _ => Err("the signature algorithm does not fit the key".into()),
        }
    }

    /// Checks `sig` over `message`.
    pub fn verify(
        &self,
        scheme: SignatureScheme,
        message: &[u8],
        sig: &[u8],
    ) -> std::result::Result<(), String> {
        self.verify_prehash(scheme, &scheme.digest().digest(&[message]), sig)
    }
}

fn pkcs1(d: DigestAlgorithm) -> Pkcs1v15Sign {
    match d {
        DigestAlgorithm::Sha1 => Pkcs1v15Sign::new::<Sha1>(),
        DigestAlgorithm::Sha224 => Pkcs1v15Sign::new::<Sha224>(),
        DigestAlgorithm::Sha256 => Pkcs1v15Sign::new::<Sha256>(),
        DigestAlgorithm::Sha384 => Pkcs1v15Sign::new::<Sha384>(),
        DigestAlgorithm::Sha512 => Pkcs1v15Sign::new::<Sha512>(),
    }
}

/// A private key that stays where it is kept (an operating system's key store, a smart
/// card) and signs on request: keys that cannot be exported.
pub trait Signer: Send + Sync {
    /// The key's SubjectPublicKeyInfo DER.
    fn public_key_der(&self) -> Result<Vec<u8>>;
    /// Signs the already hashed `hash` with `scheme`; ECDSA signatures are DER-encoded.
    fn sign_prehash(&self, scheme: SignatureScheme, hash: &[u8]) -> Result<Vec<u8>>;
    /// Where the key is kept, for people (e.g. "Windows certificate store").
    fn location(&self) -> String;
}

/// A signing key: RSA, ECDSA P-256 or ECDSA P-384, in memory or behind a [`Signer`].
#[derive(Clone)]
pub enum PrivateKey {
    Rsa(Box<rsa::PrivateKey>),
    P256(p256::ecdsa::SigningKey),
    P384(p384::ecdsa::SigningKey),
    /// A key held elsewhere, with its public key.
    External(std::sync::Arc<dyn Signer>, PublicKey),
}

impl std::fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never print key material.
        write!(f, "PrivateKey({})", self.description())
    }
}

impl PrivateKey {
    /// A key that signs through `signer`.
    pub fn external(signer: std::sync::Arc<dyn Signer>) -> Result<Self> {
        let public = PublicKey::from_spki(&signer.public_key_der()?).map_err(Error::Key)?;
        Ok(Self::External(signer, public))
    }

    /// A new RSA key of `bits` (2048 or more).
    pub fn generate_rsa(bits: usize) -> Result<Self> {
        let k = rsa::PrivateKey::new(&mut getrandom::SysRng, bits)
            .map_err(|e| Error::Key(e.to_string()))?;
        Ok(Self::Rsa(Box::new(k)))
    }

    pub fn generate_p256() -> Result<Self> {
        loop {
            let b = random::<32>()?;
            if let Ok(k) = p256::ecdsa::SigningKey::from_slice(&b) {
                return Ok(Self::P256(k));
            }
        }
    }

    pub fn generate_p384() -> Result<Self> {
        loop {
            let b = random::<48>()?;
            if let Ok(k) = p384::ecdsa::SigningKey::from_slice(&b) {
                return Ok(Self::P384(k));
            }
        }
    }

    /// From DER: PKCS #8, PKCS #1 (RSA) or SEC 1 (EC).
    pub fn from_der(der: &[u8]) -> Result<Self> {
        if let Ok(k) = rsa::PrivateKey::from_pkcs8_der(der) {
            return Ok(Self::Rsa(Box::new(k)));
        }
        if let Ok(k) = p256::ecdsa::SigningKey::from_pkcs8_der(der) {
            return Ok(Self::P256(k));
        }
        if let Ok(k) = p384::ecdsa::SigningKey::from_pkcs8_der(der) {
            return Ok(Self::P384(k));
        }
        if let Ok(k) = rsa::PrivateKey::from_pkcs1_der(der) {
            return Ok(Self::Rsa(Box::new(k)));
        }
        Self::from_sec1(der, None)
            .ok_or_else(|| Error::Key("unsupported or damaged private key".into()))
    }

    /// An `ECPrivateKey` (RFC 5915); `curve` when the structure omits it.
    fn from_sec1(der: &[u8], curve: Option<Oid>) -> Option<Self> {
        let (t, _) = asn1::parse(der)?;
        let mut r = t.reader();
        r.tagged(asn1::INTEGER)?;
        let d = r.tagged(asn1::OCTET_STRING)?.value;
        let curve = r
            .tagged(asn1::ctx(0))
            .and_then(|p| p.reader().next()?.oid())
            .or(curve);
        match curve? {
            oid::P256 => p256::ecdsa::SigningKey::from_slice(d).ok().map(Self::P256),
            oid::P384 => p384::ecdsa::SigningKey::from_slice(d).ok().map(Self::P384),
            _ => None,
        }
    }

    /// PKCS #8 DER (unencrypted): handle with care.
    pub fn to_pkcs8_der(&self) -> Result<Vec<u8>> {
        let doc = match self {
            Self::Rsa(k) => k.to_pkcs8_der(),
            Self::P256(k) => k.to_pkcs8_der(),
            Self::P384(k) => k.to_pkcs8_der(),
            Self::External(..) => return Err(Error::Key("the key cannot be exported".into())),
        };
        doc.map(|d| d.as_bytes().to_vec())
            .map_err(|e| Error::Key(e.to_string()))
    }

    /// The SubjectPublicKeyInfo DER.
    pub fn public_key_der(&self) -> Result<Vec<u8>> {
        let doc = match self {
            Self::Rsa(k) => k.as_public_key().to_public_key_der(),
            Self::P256(k) => k.verifying_key().to_public_key_der(),
            Self::P384(k) => k.verifying_key().to_public_key_der(),
            Self::External(s, _) => return s.public_key_der(),
        };
        doc.map(|d| d.as_bytes().to_vec())
            .map_err(|e| Error::Key(e.to_string()))
    }

    pub fn description(&self) -> String {
        match self {
            Self::Rsa(k) => format!("RSA {}", rsa::traits::PublicKeyParts::size(&**k) * 8),
            Self::P256(_) => "ECDSA P-256".into(),
            Self::P384(_) => "ECDSA P-384".into(),
            Self::External(s, k) => format!("{} in the {}", k.description(), s.location()),
        }
    }

    /// The digest matching the key's strength.
    pub fn default_digest(&self) -> DigestAlgorithm {
        match self {
            Self::P384(_) | Self::External(_, PublicKey::P384(_)) => DigestAlgorithm::Sha384,
            _ => DigestAlgorithm::Sha256,
        }
    }

    /// The scheme this key signs with: PKCS #1 v1.5 (or PSS when asked) for RSA.
    pub fn scheme(&self, digest: DigestAlgorithm, pss: bool) -> SignatureScheme {
        match self {
            Self::Rsa(_) | Self::External(_, PublicKey::Rsa(_)) if pss => SignatureScheme::RsaPss {
                digest,
                salt: digest.digest(&[]).len(),
            },
            Self::Rsa(_) | Self::External(_, PublicKey::Rsa(_)) => {
                SignatureScheme::RsaPkcs1(digest)
            }
            _ => SignatureScheme::Ecdsa(digest),
        }
    }

    /// Signs the already hashed `hash`.
    pub fn sign_prehash(&self, scheme: SignatureScheme, hash: &[u8]) -> Result<Vec<u8>> {
        let err = |e: &dyn std::fmt::Display| Error::Key(format!("signing failed: {e}"));
        match (self, scheme) {
            (Self::Rsa(k), SignatureScheme::RsaPkcs1(d)) => {
                k.sign(pkcs1(d), hash).map_err(|e| err(&e))
            }
            (Self::Rsa(k), SignatureScheme::RsaPss { digest, salt }) => {
                let rng = &mut getrandom::SysRng;
                let r = match digest {
                    DigestAlgorithm::Sha1 => {
                        k.sign_with_rng(rng, Pss::<Sha1>::new_with_salt(salt), hash)
                    }
                    DigestAlgorithm::Sha224 => {
                        k.sign_with_rng(rng, Pss::<Sha224>::new_with_salt(salt), hash)
                    }
                    DigestAlgorithm::Sha256 => {
                        k.sign_with_rng(rng, Pss::<Sha256>::new_with_salt(salt), hash)
                    }
                    DigestAlgorithm::Sha384 => {
                        k.sign_with_rng(rng, Pss::<Sha384>::new_with_salt(salt), hash)
                    }
                    DigestAlgorithm::Sha512 => {
                        k.sign_with_rng(rng, Pss::<Sha512>::new_with_salt(salt), hash)
                    }
                };
                r.map_err(|e| err(&e))
            }
            (Self::P256(k), SignatureScheme::Ecdsa(_)) => {
                let s: p256::ecdsa::DerSignature = k.sign_prehash(hash).map_err(|e| err(&e))?;
                Ok(s.as_bytes().to_vec())
            }
            (Self::P384(k), SignatureScheme::Ecdsa(_)) => {
                let s: p384::ecdsa::DerSignature = k.sign_prehash(hash).map_err(|e| err(&e))?;
                Ok(s.as_bytes().to_vec())
            }
            (Self::External(s, public), scheme) => {
                let sig = s.sign_prehash(scheme, hash)?;
                // Key stores answer for whatever key they hold: make sure it is this one.
                public
                    .verify_prehash(scheme, hash, &sig)
                    .map_err(|e| err(&format!("the key store's signature is wrong ({e})")))?;
                Ok(sig)
            }
            _ => Err(Error::Key("the scheme does not fit the key".into())),
        }
    }
}

/// An ECDSA signature given as `r ‖ s` (fixed-size halves) as DER.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
pub(crate) fn ecdsa_der(raw: &[u8]) -> Vec<u8> {
    let (r, s) = raw.split_at(raw.len() / 2);
    asn1::seq(&[&asn1::uint(r), &asn1::uint(s)])
}

pub(crate) fn random<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| Error::Key(format!("no randomness: {e}")))?;
    Ok(b)
}

/// The algorithm OID and parameters of an AlgorithmIdentifier.
pub(crate) fn algorithm<'a>(t: &Tlv<'a>) -> Option<(Oid, Option<Tlv<'a>>)> {
    let mut r: Reader<'a> = t.reader();
    let o = r.next()?.oid()?;
    Some((o, r.next()))
}
