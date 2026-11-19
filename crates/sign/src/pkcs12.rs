//! PKCS #12 (RFC 7292) files (`.p12`, `.pfx`): reading a key and its certificates, and
//! writing one with current algorithms (PBES2 AES-256-CBC, HMAC-SHA-256).

use cbc::cipher::block_padding::Pkcs7;
use cbc::cipher::{BlockModeDecrypt, InnerIvInit, KeyInit, KeyIvInit};
use der::{Decode, Encode};
use hmac::{Hmac, Mac};

use crate::asn1::{self, Tlv};
use crate::cert::Certificate;
use crate::crypto::{self, DigestAlgorithm, PrivateKey};
use crate::{Error, Result, oid};

/// Iterations for the key derivation and the MAC, as OpenSSL writes.
const ITERATIONS: u32 = 2048;

/// The key, its certificate and the other certificates of a PKCS #12 file.
pub(crate) struct Contents {
    pub key: PrivateKey,
    pub certificate: Certificate,
    pub others: Vec<Certificate>,
}

pub(crate) fn read(data: &[u8], password: &str) -> Result<Contents> {
    let bad = Error::Malformed("PKCS #12 file");
    let (pfx, _) = asn1::parse(data).ok_or(bad.clone())?;
    let mut r = pfx.reader();
    r.tagged(asn1::INTEGER).ok_or(bad.clone())?;
    let auth_safe = r.tagged(asn1::SEQUENCE).ok_or(bad.clone())?;
    let mac = r.tagged(asn1::SEQUENCE);
    let (ct, content) = content_info(&auth_safe).ok_or(bad.clone())?;
    if ct != oid::DATA {
        return Err(Error::Unsupported("public-key protected PKCS #12".into()));
    }
    let content = content.ok_or(bad.clone())?;
    if let Some(mac) = mac {
        check_mac(&mac, &content, password)?;
    }
    let (safes, _) = asn1::parse(&content).ok_or(bad.clone())?;
    let mut keys: Vec<(Option<Vec<u8>>, Vec<u8>)> = Vec::new();
    let mut certs: Vec<(Option<Vec<u8>>, Certificate)> = Vec::new();
    for ci in safes.reader() {
        let (ct, content) = content_info(&ci).ok_or(bad.clone())?;
        let bags = match ct {
            oid::DATA => content.ok_or(bad.clone())?,
            oid::ENCRYPTED_DATA => {
                let ed = asn1::parse(content.as_deref().unwrap_or_default())
                    .ok_or(bad.clone())?
                    .0;
                encrypted_data(&ed, password)?
            }
            _ => continue,
        };
        let (bags, _) = asn1::parse(&bags).ok_or(bad.clone())?;
        for bag in bags.reader() {
            safe_bag(&bag, password, &mut keys, &mut certs)?;
        }
    }
    let (key_id, key_der) = keys
        .into_iter()
        .next()
        .ok_or(Error::Key("the file holds no private key".into()))?;
    let key = PrivateKey::from_der(&key_der)?;
    let public = key.public_key_der()?;
    // The certificate for the key: by local key id, else by public key.
    let pos = certs
        .iter()
        .position(|(id, _)| key_id.is_some() && *id == key_id)
        .or_else(|| certs.iter().position(|(_, c)| c.spki() == public))
        .ok_or(Error::Key("no certificate matches the private key".into()))?;
    let certificate = certs.remove(pos).1;
    Ok(Contents {
        key,
        certificate,
        others: certs.into_iter().map(|(_, c)| c).collect(),
    })
}

/// A ContentInfo's type and, for `data`, its octets.
fn content_info(t: &Tlv<'_>) -> Option<(const_oid::ObjectIdentifier, Option<Vec<u8>>)> {
    let mut r = t.reader();
    let ct = r.next()?.oid()?;
    let content = r.tagged(asn1::ctx(0)).and_then(|c| c.reader().next());
    let octets = match ct {
        oid::DATA => content.map(|c| c.octets().into_owned()),
        _ => content.map(|c| c.raw.to_vec()),
    };
    Some((ct, octets))
}

fn check_mac(mac: &Tlv<'_>, content: &[u8], password: &str) -> Result<()> {
    let bad = Error::Malformed("PKCS #12 MAC");
    let mut r = mac.reader();
    let digest_info = r.tagged(asn1::SEQUENCE).ok_or(bad.clone())?;
    let salt = r.tagged(asn1::OCTET_STRING).ok_or(bad.clone())?.value;
    let iterations = r
        .tagged(asn1::INTEGER)
        .and_then(|i| i.small_int())
        .unwrap_or(1);
    let mut d = digest_info.reader();
    let alg = d
        .tagged(asn1::SEQUENCE)
        .and_then(|a| crypto::algorithm(&a))
        .ok_or(bad.clone())?
        .0;
    let expected = d.tagged(asn1::OCTET_STRING).ok_or(bad)?.value;
    let digest = DigestAlgorithm::from_oid(&alg)
        .ok_or_else(|| Error::Unsupported(format!("PKCS #12 MAC algorithm {alg}")))?;
    let iterations = u32::try_from(iterations).unwrap_or(1).clamp(1, 10_000_000);
    // Empty passwords are encoded with or without the terminating zero, depending on the
    // writer.
    let candidates = if password.is_empty() {
        vec![bmp_password(password), Vec::new()]
    } else {
        vec![bmp_password(password)]
    };
    for p in candidates {
        let key = kdf(digest, &p, salt, 3, iterations, digest.digest(&[]).len());
        if hmac(digest, &key, content) == expected {
            return Ok(());
        }
    }
    Err(Error::WrongPassword)
}

fn encrypted_data(ed: &Tlv<'_>, password: &str) -> Result<Vec<u8>> {
    let bad = Error::Malformed("PKCS #12 encrypted data");
    let mut r = ed.reader();
    r.tagged(asn1::INTEGER).ok_or(bad.clone())?;
    let eci = r.tagged(asn1::SEQUENCE).ok_or(bad.clone())?;
    let mut e = eci.reader();
    e.tagged(asn1::OID).ok_or(bad.clone())?;
    let alg = e.tagged(asn1::SEQUENCE).ok_or(bad.clone())?;
    let data = e.next().ok_or(bad)?;
    decrypt(&alg, &data.octets(), password)
}

fn safe_bag(
    bag: &Tlv<'_>,
    password: &str,
    keys: &mut Vec<(Option<Vec<u8>>, Vec<u8>)>,
    certs: &mut Vec<(Option<Vec<u8>>, Certificate)>,
) -> Result<()> {
    let bad = Error::Malformed("PKCS #12 bag");
    let mut r = bag.reader();
    let kind = r.next().and_then(|t| t.oid()).ok_or(bad.clone())?;
    let value = r
        .tagged(asn1::ctx(0))
        .and_then(|v| v.reader().next())
        .ok_or(bad.clone())?;
    let attrs = r.tagged(asn1::SET);
    let key_id =
        crate::cms::attribute(attrs.as_ref(), &oid::LOCAL_KEY_ID).map(|v| v.octets().into_owned());
    match kind {
        oid::KEY_BAG => keys.push((key_id, value.raw.to_vec())),
        oid::SHROUDED_KEY_BAG => {
            let mut e = value.reader();
            let alg = e.tagged(asn1::SEQUENCE).ok_or(bad.clone())?;
            let data = e.tagged(asn1::OCTET_STRING).ok_or(bad)?;
            keys.push((key_id, decrypt(&alg, data.value, password)?));
        }
        oid::CERT_BAG => {
            let mut c = value.reader();
            if c.next().and_then(|t| t.oid()) == Some(oid::X509_CERTIFICATE)
                && let Some(der) = c.tagged(asn1::ctx(0)).and_then(|v| v.reader().next())
                && let Ok(cert) = Certificate::from_der(&der.octets())
            {
                certs.push((key_id, cert));
            }
        }
        // Nested safe contents, CRL and secret bags: not needed for signing.
        _ => {}
    }
    Ok(())
}

/// Decrypts with a PBES2 or PKCS #12 password-based scheme.
fn decrypt(alg: &Tlv<'_>, data: &[u8], password: &str) -> Result<Vec<u8>> {
    let (o, params) = crypto::algorithm(alg).ok_or(Error::Malformed("PKCS #12 algorithm"))?;
    if o == oid::PBES2 {
        let scheme = pkcs5::EncryptionScheme::from_der(alg.raw)
            .map_err(|e| Error::Unsupported(format!("PBES2 parameters: {e}")))?;
        return scheme
            .decrypt(password.as_bytes(), data)
            .map_err(|_| Error::WrongPassword);
    }
    let params = params.ok_or(Error::Malformed("PBE parameters"))?;
    let mut p = params.reader();
    let salt = p
        .tagged(asn1::OCTET_STRING)
        .ok_or(Error::Malformed("PBE salt"))?
        .value;
    let iterations = p
        .tagged(asn1::INTEGER)
        .and_then(|i| i.small_int())
        .unwrap_or(1);
    let iterations = u32::try_from(iterations).unwrap_or(1).clamp(1, 10_000_000);
    let pw = bmp_password(password);
    let derive = |id: u8, n: usize| kdf(DigestAlgorithm::Sha1, &pw, salt, id, iterations, n);
    let wrong = |_| Error::WrongPassword;
    let out = match o {
        oid::PBE_SHA1_3DES => {
            let (k, iv) = (derive(1, 24), derive(2, 8));
            cbc::Decryptor::<des::TdesEde3>::new_from_slices(&k, &iv)
                .map_err(|_| Error::WrongPassword)?
                .decrypt_padded_vec::<Pkcs7>(data)
                .map_err(wrong)?
        }
        oid::PBE_SHA1_2DES => {
            let (k, iv) = (derive(1, 16), derive(2, 8));
            cbc::Decryptor::<des::TdesEde2>::new_from_slices(&k, &iv)
                .map_err(|_| Error::WrongPassword)?
                .decrypt_padded_vec::<Pkcs7>(data)
                .map_err(wrong)?
        }
        oid::PBE_SHA1_RC2_40 | oid::PBE_SHA1_RC2_128 => {
            let bits = if o == oid::PBE_SHA1_RC2_40 { 40 } else { 128 };
            let (k, iv) = (derive(1, bits / 8), derive(2, 8));
            let cipher = rc2::Rc2::new_with_eff_key_len(&k, bits);
            cbc::Decryptor::<rc2::Rc2>::inner_iv_slice_init(cipher, &iv)
                .map_err(|_| Error::WrongPassword)?
                .decrypt_padded_vec::<Pkcs7>(data)
                .map_err(wrong)?
        }
        o => return Err(Error::Unsupported(format!("PKCS #12 encryption {o}"))),
    };
    Ok(out)
}

/// The password as a NUL-terminated big-endian BMPString (UTF-16), as PKCS #12 hashes it.
fn bmp_password(p: &str) -> Vec<u8> {
    let mut v: Vec<u8> = p.encode_utf16().flat_map(u16::to_be_bytes).collect();
    v.extend_from_slice(&[0, 0]);
    v
}

/// The PKCS #12 key derivation (RFC 7292 Appendix B.2).
fn kdf(
    digest: DigestAlgorithm,
    password: &[u8],
    salt: &[u8],
    id: u8,
    iterations: u32,
    n: usize,
) -> Vec<u8> {
    let v = match digest {
        DigestAlgorithm::Sha384 | DigestAlgorithm::Sha512 => 128,
        _ => 64,
    };
    let fill = |s: &[u8]| -> Vec<u8> {
        if s.is_empty() {
            return Vec::new();
        }
        let len = v * s.len().div_ceil(v);
        s.iter().copied().cycle().take(len).collect()
    };
    let d = vec![id; v];
    let mut i = fill(salt);
    i.extend(fill(password));
    let mut out = Vec::with_capacity(n + 64);
    while out.len() < n {
        let mut a = digest.digest(&[&d, &i]);
        for _ in 1..iterations {
            a = digest.digest(&[&a]);
        }
        let b: Vec<u8> = a.iter().copied().cycle().take(v).collect();
        for block in i.chunks_mut(v) {
            // block = block + b + 1 (mod 2^(8v)), big-endian.
            let mut carry = 1u16;
            for k in (0..v).rev() {
                let s = u16::from(block[k]) + u16::from(b[k]) + carry;
                block[k] = s as u8;
                carry = s >> 8;
            }
        }
        out.extend_from_slice(&a);
    }
    out.truncate(n);
    out
}

fn hmac(digest: DigestAlgorithm, key: &[u8], data: &[u8]) -> Vec<u8> {
    fn run<M: Mac + KeyInit>(key: &[u8], data: &[u8]) -> Vec<u8> {
        let mut m = <M as KeyInit>::new_from_slice(key).expect("HMAC takes any key length");
        m.update(data);
        m.finalize().into_bytes().to_vec()
    }
    match digest {
        DigestAlgorithm::Sha1 => run::<Hmac<sha1::Sha1>>(key, data),
        DigestAlgorithm::Sha224 => run::<Hmac<sha2::Sha224>>(key, data),
        DigestAlgorithm::Sha256 => run::<Hmac<sha2::Sha256>>(key, data),
        DigestAlgorithm::Sha384 => run::<Hmac<sha2::Sha384>>(key, data),
        DigestAlgorithm::Sha512 => run::<Hmac<sha2::Sha512>>(key, data),
    }
}

/// Writes a PKCS #12 file: the key shrouded with PBES2 (PBKDF2-SHA-256, AES-256-CBC), the
/// certificates in clear, and an HMAC-SHA-256 integrity MAC.
pub(crate) fn write(
    key: &PrivateKey,
    cert: &Certificate,
    others: &[Certificate],
    password: &str,
    friendly_name: Option<&str>,
) -> Result<Vec<u8>> {
    let key_id = crypto::sha1(cert.der());
    let mut attrs = vec![crate::cms::attr(&oid::LOCAL_KEY_ID, &asn1::octets(&key_id))];
    if let Some(n) = friendly_name {
        let bmp: Vec<u8> = n.encode_utf16().flat_map(u16::to_be_bytes).collect();
        attrs.push(crate::cms::attr(
            &oid::FRIENDLY_NAME,
            &asn1::tlv(asn1::BMP_STRING, &bmp),
        ));
    }
    let attrs = asn1::set_of(attrs);
    let salt = crypto::random::<16>()?;
    let iv = crypto::random::<16>()?;
    let params = pkcs5::pbes2::Parameters::generate_pbkdf2_sha256_aes256cbc(ITERATIONS, &salt, iv)
        .map_err(|e| Error::Key(e.to_string()))?;
    let scheme = pkcs5::EncryptionScheme::Pbes2(params);
    let pkcs8 = key.to_pkcs8_der()?;
    let encrypted = scheme
        .encrypt(password.as_bytes(), &pkcs8)
        .map_err(|e| Error::Key(e.to_string()))?;
    let alg = scheme.to_der().map_err(|e| Error::Key(e.to_string()))?;
    let epki = asn1::seq(&[&alg, &asn1::octets(&encrypted)]);
    let key_bag = asn1::seq(&[
        &asn1::oid(&oid::SHROUDED_KEY_BAG),
        &asn1::explicit(0, &[&epki]),
        &attrs,
    ]);
    let cert_bag = |c: &Certificate, attrs: &[u8]| {
        let v = asn1::seq(&[
            &asn1::oid(&oid::X509_CERTIFICATE),
            &asn1::explicit(0, &[&asn1::octets(c.der())]),
        ]);
        asn1::seq(&[&asn1::oid(&oid::CERT_BAG), &asn1::explicit(0, &[&v]), attrs])
    };
    let mut cert_bags = vec![cert_bag(cert, &attrs)];
    cert_bags.extend(others.iter().map(|c| cert_bag(c, &[])));
    let data_ci = |content: &[u8]| {
        asn1::seq(&[
            &asn1::oid(&oid::DATA),
            &asn1::explicit(0, &[&asn1::octets(content)]),
        ])
    };
    let safes = asn1::seq(&[
        &data_ci(&asn1::tlv(asn1::SEQUENCE, &cert_bags.concat())),
        &data_ci(&asn1::seq(&[&key_bag])),
    ]);
    let mac_salt = crypto::random::<16>()?;
    let digest = DigestAlgorithm::Sha256;
    let mac_key = kdf(
        digest,
        &bmp_password(password),
        &mac_salt,
        3,
        ITERATIONS,
        32,
    );
    let mac = hmac(digest, &mac_key, &safes);
    let mac_data = asn1::seq(&[
        &asn1::seq(&[&digest.algorithm_id(), &asn1::octets(&mac)]),
        &asn1::octets(&mac_salt),
        &asn1::int(u64::from(ITERATIONS)),
    ]);
    Ok(asn1::seq(&[&asn1::int(3), &data_ci(&safes), &mac_data]))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 7292 KDF test vector from OpenSSL's test suite (`smeg`, SHA-1, id 1).
    #[test]
    fn kdf_vector() {
        let pw = bmp_password("smeg");
        let salt = [0x0A, 0x58, 0xCF, 0x64, 0x53, 0x0D, 0x82, 0x3F];
        let k = kdf(DigestAlgorithm::Sha1, &pw, &salt, 1, 1, 24);
        let expected = [
            0x8A, 0xAA, 0xE6, 0x29, 0x7B, 0x6C, 0xB0, 0x46, 0x42, 0xAB, 0x5B, 0x07, 0x78, 0x51,
            0x28, 0x4E, 0xB7, 0x12, 0x8F, 0x1A, 0x2A, 0x7F, 0xBC, 0xA3,
        ];
        assert_eq!(k, expected);
    }
}
