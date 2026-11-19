//! Object identifiers used by CMS, X.509, RFC 3161, OCSP and PKCS #12.

use const_oid::ObjectIdentifier as Oid;

const fn o(s: &str) -> Oid {
    Oid::new_unwrap(s)
}

// Digests.
pub const SHA1: Oid = o("1.3.14.3.2.26");
pub const SHA224: Oid = o("2.16.840.1.101.3.4.2.4");
pub const SHA256: Oid = o("2.16.840.1.101.3.4.2.1");
pub const SHA384: Oid = o("2.16.840.1.101.3.4.2.2");
pub const SHA512: Oid = o("2.16.840.1.101.3.4.2.3");

// Public keys and signature algorithms.
pub const RSA_ENCRYPTION: Oid = o("1.2.840.113549.1.1.1");
pub const MD5_WITH_RSA: Oid = o("1.2.840.113549.1.1.4");
pub const SHA1_WITH_RSA: Oid = o("1.2.840.113549.1.1.5");
pub const RSASSA_PSS: Oid = o("1.2.840.113549.1.1.10");
pub const SHA256_WITH_RSA: Oid = o("1.2.840.113549.1.1.11");
pub const SHA384_WITH_RSA: Oid = o("1.2.840.113549.1.1.12");
pub const SHA512_WITH_RSA: Oid = o("1.2.840.113549.1.1.13");
pub const SHA224_WITH_RSA: Oid = o("1.2.840.113549.1.1.14");
pub const MGF1: Oid = o("1.2.840.113549.1.1.8");
pub const EC_PUBLIC_KEY: Oid = o("1.2.840.10045.2.1");
pub const ECDSA_SHA1: Oid = o("1.2.840.10045.4.1");
pub const ECDSA_SHA224: Oid = o("1.2.840.10045.4.3.1");
pub const ECDSA_SHA256: Oid = o("1.2.840.10045.4.3.2");
pub const ECDSA_SHA384: Oid = o("1.2.840.10045.4.3.3");
pub const ECDSA_SHA512: Oid = o("1.2.840.10045.4.3.4");
pub const P256: Oid = o("1.2.840.10045.3.1.7");
pub const P384: Oid = o("1.3.132.0.34");
pub const ED25519: Oid = o("1.3.101.112");

// CMS.
pub const DATA: Oid = o("1.2.840.113549.1.7.1");
pub const SIGNED_DATA: Oid = o("1.2.840.113549.1.7.2");
pub const ENCRYPTED_DATA: Oid = o("1.2.840.113549.1.7.6");
pub const TST_INFO: Oid = o("1.2.840.113549.1.9.16.1.4");
pub const CONTENT_TYPE: Oid = o("1.2.840.113549.1.9.3");
pub const MESSAGE_DIGEST: Oid = o("1.2.840.113549.1.9.4");
pub const SIGNING_TIME: Oid = o("1.2.840.113549.1.9.5");
pub const SIGNING_CERTIFICATE: Oid = o("1.2.840.113549.1.9.16.2.12");
pub const SIGNING_CERTIFICATE_V2: Oid = o("1.2.840.113549.1.9.16.2.47");
pub const SIGNATURE_TIMESTAMP: Oid = o("1.2.840.113549.1.9.16.2.14");
/// Adobe's revocation archival attribute (embedded CRLs and OCSP responses).
pub const ADBE_REVOCATION: Oid = o("1.2.840.113583.1.1.8");

// X.509 extensions and key purposes.
pub const SUBJECT_KEY_ID: Oid = o("2.5.29.14");
pub const KEY_USAGE: Oid = o("2.5.29.15");
pub const BASIC_CONSTRAINTS: Oid = o("2.5.29.19");
pub const CRL_DISTRIBUTION_POINTS: Oid = o("2.5.29.31");
pub const AUTHORITY_KEY_ID: Oid = o("2.5.29.35");
pub const EXT_KEY_USAGE: Oid = o("2.5.29.37");
pub const AUTHORITY_INFO_ACCESS: Oid = o("1.3.6.1.5.5.7.1.1");
pub const AD_OCSP: Oid = o("1.3.6.1.5.5.7.48.1");
pub const KP_TIME_STAMPING: Oid = o("1.3.6.1.5.5.7.3.8");
pub const KP_OCSP_SIGNING: Oid = o("1.3.6.1.5.5.7.3.9");
pub const KP_EMAIL_PROTECTION: Oid = o("1.3.6.1.5.5.7.3.4");
pub const KP_ANY: Oid = o("2.5.29.37.0");
pub const KP_DOCUMENT_SIGNING: Oid = o("1.3.6.1.5.5.7.3.36");
pub const OCSP_BASIC: Oid = o("1.3.6.1.5.5.7.48.1.1");

// Names.
pub const COMMON_NAME: Oid = o("2.5.4.3");
pub const ORGANIZATION: Oid = o("2.5.4.10");
pub const COUNTRY: Oid = o("2.5.4.6");
pub const EMAIL_ADDRESS: Oid = o("1.2.840.113549.1.9.1");

// PKCS #12.
pub const KEY_BAG: Oid = o("1.2.840.113549.1.12.10.1.1");
pub const SHROUDED_KEY_BAG: Oid = o("1.2.840.113549.1.12.10.1.2");
pub const CERT_BAG: Oid = o("1.2.840.113549.1.12.10.1.3");
pub const X509_CERTIFICATE: Oid = o("1.2.840.113549.1.9.22.1");
pub const LOCAL_KEY_ID: Oid = o("1.2.840.113549.1.9.21");
pub const FRIENDLY_NAME: Oid = o("1.2.840.113549.1.9.20");
pub const PBE_SHA1_3DES: Oid = o("1.2.840.113549.1.12.1.3");
pub const PBE_SHA1_2DES: Oid = o("1.2.840.113549.1.12.1.4");
pub const PBE_SHA1_RC2_128: Oid = o("1.2.840.113549.1.12.1.5");
pub const PBE_SHA1_RC2_40: Oid = o("1.2.840.113549.1.12.1.6");
pub const PBES2: Oid = o("1.2.840.113549.1.5.13");
