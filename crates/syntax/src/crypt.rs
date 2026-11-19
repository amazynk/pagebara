//! The standard security handler (§7.6.4): RC4 and AES-128 (revisions 2–4) and AES-256
//! (revisions 5–6), user and owner passwords, and per-object string/stream encryption.

use aes::cipher::{BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use md5::{Digest, Md5};
use sha2::{Sha256, Sha384, Sha512};

use crate::object::{Dict, ObjRef, Object};

const PAD: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Identity,
    Rc4,
    Aes128,
    Aes256,
}

/// What a password-protected file allows (§7.6.4.2, table 22). Unlocking with the owner
/// password allows everything.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Permissions {
    pub print: bool,
    pub print_high_quality: bool,
    pub modify: bool,
    pub copy: bool,
    pub annotate: bool,
    pub fill_forms: bool,
    pub extract_for_accessibility: bool,
    pub assemble: bool,
}

impl Permissions {
    pub const ALL: Permissions = Permissions {
        print: true,
        print_high_quality: true,
        modify: true,
        copy: true,
        annotate: true,
        fill_forms: true,
        extract_for_accessibility: true,
        assemble: true,
    };

    /// `/P` for revision 6: reserved bits 7–8 and 13–32 set.
    fn to_bits(self) -> u32 {
        let mut p = 0xFFFF_F0C0u32;
        for (on, bit) in [
            (self.print, 3),
            (self.modify, 4),
            (self.copy, 5),
            (self.annotate, 6),
            (self.fill_forms, 9),
            (self.extract_for_accessibility, 10),
            (self.assemble, 11),
            (self.print_high_quality, 12),
        ] {
            if on {
                p |= 1 << (bit - 1);
            }
        }
        p
    }

    fn from_bits(p: u32) -> Self {
        let bit = |n: u32| p & (1 << (n - 1)) != 0;
        Permissions {
            print: bit(3),
            modify: bit(4),
            copy: bit(5),
            annotate: bit(6),
            fill_forms: bit(9) || bit(6),
            extract_for_accessibility: bit(10) || bit(5),
            assemble: bit(11) || bit(4),
            print_high_quality: bit(12) && bit(3),
        }
    }
}

/// The parsed `/Encrypt` dictionary, before a password is known.
#[derive(Clone, Debug)]
pub struct Handler {
    revision: i64,
    key_len: usize,
    o: Vec<u8>,
    u: Vec<u8>,
    oe: Vec<u8>,
    ue: Vec<u8>,
    p: u32,
    id0: Vec<u8>,
    encrypt_metadata: bool,
    stream: Method,
    string: Method,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unsupported {
    /// A handler other than `/Standard` (e.g. public-key certificates).
    Handler,
    Revision,
}

impl Handler {
    pub fn new(enc: &Dict, id0: &[u8]) -> Result<Self, Unsupported> {
        if enc.name(b"Filter").is_some_and(|f| f != b"Standard") {
            return Err(Unsupported::Handler);
        }
        let v = enc.int(b"V").unwrap_or(0);
        let revision = enc.int(b"R").unwrap_or(2);
        let bytes = |k: &[u8]| {
            enc.get(k)
                .and_then(Object::as_string)
                .unwrap_or_default()
                .to_vec()
        };
        let (stream, string) = match v {
            1 => (Method::Rc4, Method::Rc4),
            2 | 3 => (Method::Rc4, Method::Rc4),
            4 | 5 => {
                let method = |key: &[u8]| {
                    let name = enc.name(key).unwrap_or(b"Identity");
                    if name == b"Identity" {
                        return Method::Identity;
                    }
                    let cf = enc
                        .get(b"CF")
                        .and_then(Object::as_dict)
                        .and_then(|cf| cf.get(name));
                    match cf.and_then(Object::as_dict).and_then(|d| d.name(b"CFM")) {
                        Some(b"V2") => Method::Rc4,
                        Some(b"AESV2") => Method::Aes128,
                        Some(b"AESV3") => Method::Aes256,
                        _ => Method::Identity,
                    }
                };
                (method(b"StmF"), method(b"StrF"))
            }
            _ => return Err(Unsupported::Revision),
        };
        if !(2..=6).contains(&revision) {
            return Err(Unsupported::Revision);
        }
        let key_len = match (v, revision) {
            (_, 5 | 6) => 32,
            (1, _) | (_, 2) => 5,
            // Crypt filters carry the length, in bits or (as Acrobat writes it) bytes;
            // the document's `/Length` is only for V 2 and 3 and often missing here.
            (4, _) if stream == Method::Aes128 || string == Method::Aes128 => 16,
            (4, _) => {
                let cf = enc
                    .get(b"CF")
                    .and_then(Object::as_dict)
                    .and_then(|cf| cf.get(enc.name(b"StmF").unwrap_or(b"StdCF")))
                    .and_then(Object::as_dict)
                    .and_then(|d| d.int(b"Length"));
                match cf {
                    Some(n @ 5..=16) => n as usize,
                    Some(n @ 40..=128) => (n / 8) as usize,
                    _ => 16,
                }
            }
            _ => (enc.int(b"Length").unwrap_or(40).clamp(40, 128) / 8) as usize,
        };
        Ok(Handler {
            revision,
            key_len,
            o: bytes(b"O"),
            u: bytes(b"U"),
            oe: bytes(b"OE"),
            ue: bytes(b"UE"),
            p: enc.int(b"P").unwrap_or(-1) as u32,
            id0: id0.to_vec(),
            encrypt_metadata: enc
                .get(b"EncryptMetadata")
                .and_then(Object::as_bool)
                .unwrap_or(true),
            stream,
            string,
        })
    }

    /// Tries `password` as typed, in each encoding writers are known to use: SASLprep'd
    /// UTF-8 for AES-256, PDFDocEncoding for older revisions, and raw UTF-8 as a fallback.
    pub fn unlock_str(&self, password: &str) -> Option<Security> {
        let mut tries: Vec<Vec<u8>> = Vec::with_capacity(3);
        if self.revision >= 5 {
            if let Some(p) = crate::text::saslprep(password) {
                tries.push(truncate_utf8(&p).to_vec());
            }
        } else if let Some(p) = crate::text::pdfdoc_encode(password) {
            tries.push(p);
        }
        tries.push(password.as_bytes().to_vec());
        tries.dedup();
        tries.iter().find_map(|p| self.unlock(p))
    }

    /// Tries `password` bytes as the owner password, then as the user password.
    pub fn unlock(&self, password: &[u8]) -> Option<Security> {
        let (key, owner) = if self.revision >= 5 {
            self.unlock_aes256(password)?
        } else if let Some(k) = self.owner_r4(password) {
            (k, true)
        } else {
            (self.user_r4(password)?, false)
        };
        Some(Security {
            key,
            stream: self.stream,
            string: self.string,
            encrypt_metadata: self.encrypt_metadata,
            permissions: if owner {
                Permissions::ALL
            } else {
                Permissions::from_bits(self.p)
            },
            owner,
        })
    }

    /// Algorithm 2.
    fn key_r4(&self, password: &[u8]) -> Vec<u8> {
        let mut h = Md5::new();
        h.update(padded(password));
        h.update(&self.o[..self.o.len().min(32)]);
        h.update(self.p.to_le_bytes());
        h.update(&self.id0);
        if self.revision >= 4 && !self.encrypt_metadata {
            h.update([0xff; 4]);
        }
        let mut k = h.finalize().to_vec();
        if self.revision >= 3 {
            for _ in 0..50 {
                k = Md5::digest(&k[..self.key_len]).to_vec();
            }
        }
        k.truncate(self.key_len);
        k
    }

    /// Algorithms 4–6.
    fn user_r4(&self, password: &[u8]) -> Option<Vec<u8>> {
        let key = self.key_r4(password);
        let ok = if self.revision == 2 {
            let mut u = PAD.to_vec();
            rc4(&key, &mut u);
            self.u.get(..32) == Some(&u[..])
        } else {
            let mut h = Md5::new();
            h.update(PAD);
            h.update(&self.id0);
            let mut u = h.finalize().to_vec();
            for i in 0..20u8 {
                let k: Vec<u8> = key.iter().map(|b| b ^ i).collect();
                rc4(&k, &mut u);
            }
            self.u.get(..16) == Some(&u[..])
        };
        ok.then_some(key)
    }

    /// Algorithm 7: the owner password decrypts `/O` to the user password.
    fn owner_r4(&self, password: &[u8]) -> Option<Vec<u8>> {
        let mut k = Md5::digest(padded(password)).to_vec();
        if self.revision >= 3 {
            for _ in 0..50 {
                k = Md5::digest(&k).to_vec();
            }
        }
        k.truncate(self.key_len);
        let mut user = self.o.get(..32)?.to_vec();
        if self.revision == 2 {
            rc4(&k, &mut user);
        } else {
            for i in (0..20u8).rev() {
                let ki: Vec<u8> = k.iter().map(|b| b ^ i).collect();
                rc4(&ki, &mut user);
            }
        }
        self.user_r4(&user)
    }

    /// Algorithm 2.A. Passwords are UTF-8; SASLprep is the caller's concern.
    fn unlock_aes256(&self, password: &[u8]) -> Option<(Vec<u8>, bool)> {
        let pw = &password[..password.len().min(127)];
        let (o, u) = (self.o.get(..48)?, self.u.get(..48)?);
        let hash = |salt: &[u8], udata: &[u8]| hash_2b(self.revision, pw, salt, udata);
        if hash(&o[32..40], u)[..] == o[..32] {
            let k = hash(&o[40..48], u);
            return Some((aes256_unwrap(&k, &self.oe)?, true));
        }
        if hash(&u[32..40], &[])[..] == u[..32] {
            let k = hash(&u[40..48], &[]);
            return Some((aes256_unwrap(&k, &self.ue)?, false));
        }
        None
    }
}

/// Algorithm 2.B (revision 6); plain SHA-256 for revision 5.
fn hash_2b(revision: i64, pw: &[u8], salt: &[u8], udata: &[u8]) -> [u8; 32] {
    let mut k: Vec<u8> = Sha256::new()
        .chain_update(pw)
        .chain_update(salt)
        .chain_update(udata)
        .finalize()
        .to_vec();
    if revision >= 6 {
        let mut round = 0u32;
        loop {
            let mut k1 = Vec::with_capacity(64 * (pw.len() + k.len() + udata.len()));
            for _ in 0..64 {
                k1.extend_from_slice(pw);
                k1.extend_from_slice(&k);
                k1.extend_from_slice(udata);
            }
            let mut iv: [u8; 16] = k[16..32].try_into().unwrap_or_default();
            let cipher = aes::Aes128::new_from_slice(&k[..16]).ok();
            let Some(cipher) = cipher else { break };
            for block in k1.as_chunks_mut::<16>().0 {
                for (b, v) in block.iter_mut().zip(iv) {
                    *b ^= v;
                }
                let mut blk = (*block).into();
                cipher.encrypt_block(&mut blk);
                *block = blk.into();
                iv = *block;
            }
            let e = k1;
            let m = e[..16].iter().map(|&b| u32::from(b)).sum::<u32>() % 3;
            k = match m {
                0 => Sha256::digest(&e).to_vec(),
                1 => Sha384::digest(&e).to_vec(),
                _ => Sha512::digest(&e).to_vec(),
            };
            round += 1;
            if round >= 64 && u32::from(*e.last().unwrap_or(&0)) <= round - 32 {
                break;
            }
        }
    }
    k[..32].try_into().unwrap_or_default()
}

/// `/UE` or `/OE`: the file key under AES-256 with a zero IV and no padding.
fn aes256_unwrap(k: &[u8; 32], wrapped: &[u8]) -> Option<Vec<u8>> {
    let mut data = wrapped.get(..32)?.to_vec();
    cbc_decrypt_in_place(k, &[0; 16], &mut data)?;
    Some(data)
}

fn padded(pw: &[u8]) -> [u8; 32] {
    let mut out = PAD;
    let n = pw.len().min(32);
    out[..n].copy_from_slice(&pw[..n]);
    out[n..].copy_from_slice(&PAD[..32 - n]);
    out
}

/// An unlocked document's keys.
#[derive(Clone, Debug)]
pub struct Security {
    key: Vec<u8>,
    stream: Method,
    string: Method,
    encrypt_metadata: bool,
    pub permissions: Permissions,
    pub owner: bool,
}

impl Security {
    /// Algorithm 1: the key for one object.
    fn object_key(&self, m: Method, r: ObjRef) -> Vec<u8> {
        if m == Method::Aes256 {
            return self.key.clone();
        }
        let mut h = Md5::new();
        h.update(&self.key);
        h.update(&r.num.to_le_bytes()[..3]);
        h.update(r.generation.to_le_bytes());
        if m == Method::Aes128 {
            h.update(b"sAlT");
        }
        let mut k = h.finalize().to_vec();
        k.truncate((self.key.len() + 5).min(16));
        k
    }

    fn apply(&self, m: Method, r: ObjRef, data: &[u8], encrypt: bool) -> Vec<u8> {
        let key = self.object_key(m, r);
        match m {
            Method::Identity => data.to_vec(),
            Method::Rc4 => {
                let mut d = data.to_vec();
                rc4(&key, &mut d);
                d
            }
            Method::Aes128 | Method::Aes256 if encrypt => aes_encrypt(&key, data),
            Method::Aes128 | Method::Aes256 => aes_decrypt(&key, data),
        }
    }

    /// Whether a stream with this dictionary is encrypted at all.
    fn stream_method(&self, d: &Dict) -> Method {
        if d.name(b"Type") == Some(b"XRef")
            || (!self.encrypt_metadata && d.name(b"Type") == Some(b"Metadata"))
        {
            return Method::Identity;
        }
        // A stream-level Identity crypt filter opts out.
        let identity_crypt = d.get(b"Filter").is_some_and(|f| {
            f.as_name() == Some(b"Crypt")
                || f.as_array()
                    .is_some_and(|a| a.first().and_then(Object::as_name) == Some(b"Crypt"))
        });
        if identity_crypt {
            Method::Identity
        } else {
            self.stream
        }
    }

    /// Decrypts the strings and stream data of indirect object `r`, in place.
    pub fn decrypt(&self, r: ObjRef, o: &mut Object) {
        self.walk(r, o, false);
    }

    pub fn encrypt(&self, r: ObjRef, o: &mut Object) {
        self.walk(r, o, true);
    }

    fn walk(&self, r: ObjRef, o: &mut Object, enc: bool) {
        match o {
            Object::String(s) => {
                s.bytes = self.apply(self.string, r, &s.bytes, enc);
            }
            Object::Array(a) => a.iter_mut().for_each(|o| self.walk(r, o, enc)),
            Object::Dict(d) => self.walk_dict(r, d, enc),
            Object::Stream(s) => {
                let m = self.stream_method(&s.dict);
                self.walk_dict(r, &mut s.dict, enc);
                if m != Method::Identity
                    && let crate::StreamData::Owned(b) = &s.data
                {
                    s.data = crate::StreamData::Owned(self.apply(m, r, b, enc).into());
                }
            }
            _ => {}
        }
    }

    fn walk_dict(&self, r: ObjRef, d: &mut Dict, enc: bool) {
        // Signature values are never encrypted (§7.6.2).
        let sig = d.contains(b"ByteRange")
            && matches!(d.name(b"Type"), Some(b"Sig" | b"DocTimeStamp") | None);
        for (k, v) in &mut d.0 {
            if sig && k == &b"Contents" {
                continue;
            }
            self.walk(r, v, enc);
        }
    }
}

/// A password that SASLprep (RFC 4013) rejects, e.g. one with control characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidPassword;

/// AES-256 (revision 6) protection with fresh random keys (algorithms 8–10). Returns the keys
/// and the `/Encrypt` dictionary.
pub fn protect(
    user: &str,
    owner: &str,
    permissions: Permissions,
) -> Result<(Security, Dict), InvalidPassword> {
    let prep = |p: &str| {
        let p = crate::text::saslprep(p).ok_or(InvalidPassword)?;
        Ok(truncate_utf8(&p).to_vec())
    };
    let (user, owner) = (prep(user)?, prep(owner)?);
    let mut rnd = [0u8; 32 + 16 + 16 + 4];
    let _ = getrandom::fill(&mut rnd);
    let key = &rnd[..32];
    let (u_salts, o_salts, perms_rnd) = (&rnd[32..48], &rnd[48..64], &rnd[64..68]);

    let mut u = hash_2b(6, &user, &u_salts[..8], &[]).to_vec();
    u.extend_from_slice(u_salts);
    let ue = aes256_wrap(&hash_2b(6, &user, &u_salts[8..], &[]), key);
    let mut o = hash_2b(6, &owner, &o_salts[..8], &u).to_vec();
    o.extend_from_slice(o_salts);
    let oe = aes256_wrap(&hash_2b(6, &owner, &o_salts[8..], &u), key);

    let p = permissions.to_bits();
    let mut perms = [0u8; 16];
    perms[..4].copy_from_slice(&p.to_le_bytes());
    perms[4..8].fill(0xff);
    perms[8..12].copy_from_slice(b"Tadb");
    perms[12..].copy_from_slice(perms_rnd);
    if let Some(c) = Aes::new(key) {
        c.encrypt(&mut perms);
    }

    let s = |b: &[u8]| {
        Object::String(crate::PdfString {
            bytes: b.to_vec(),
            hex: true,
        })
    };
    let mut cf = Dict::new();
    cf.set(b"CFM", Object::name(b"AESV3"));
    cf.set(b"AuthEvent", Object::name(b"DocOpen"));
    cf.set(b"Length", 32);
    let mut cfs = Dict::new();
    cfs.set(b"StdCF", cf);
    let mut d = Dict::new();
    d.set(b"Filter", Object::name(b"Standard"));
    d.set(b"V", 5);
    d.set(b"R", 6);
    d.set(b"Length", 256);
    d.set(b"CF", cfs);
    d.set(b"StmF", Object::name(b"StdCF"));
    d.set(b"StrF", Object::name(b"StdCF"));
    d.set(b"O", s(&o));
    d.set(b"U", s(&u));
    d.set(b"OE", s(&oe));
    d.set(b"UE", s(&ue));
    d.set(b"Perms", s(&perms));
    d.set(b"P", i64::from(p as i32));
    let sec = Security {
        key: key.to_vec(),
        stream: Method::Aes256,
        string: Method::Aes256,
        encrypt_metadata: true,
        permissions: Permissions::ALL,
        owner: true,
    };
    Ok((sec, d))
}

/// AES-256 passwords are at most 127 bytes; cut on a character boundary.
fn truncate_utf8(s: &str) -> &[u8] {
    let mut n = s.len().min(127);
    while !s.is_char_boundary(n) {
        n -= 1;
    }
    &s.as_bytes()[..n]
}

fn aes256_wrap(k: &[u8; 32], key: &[u8]) -> Vec<u8> {
    let mut data = key.to_vec();
    if let Some(c) = Aes::new(k) {
        let mut prev = [0u8; 16];
        for block in data.as_chunks_mut::<16>().0 {
            for (b, p) in block.iter_mut().zip(prev) {
                *b ^= p;
            }
            c.encrypt(block);
            prev = *block;
        }
    }
    data
}

pub(crate) fn rc4(key: &[u8], data: &mut [u8]) {
    if key.is_empty() {
        return;
    }
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let (mut i, mut j) = (0u8, 0u8);
    for b in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        *b ^= s[s[i as usize].wrapping_add(s[j as usize]) as usize];
    }
}

enum Aes {
    A128(aes::Aes128),
    A256(aes::Aes256),
}

impl Aes {
    fn new(key: &[u8]) -> Option<Self> {
        match key.len() {
            16 => aes::Aes128::new_from_slice(key).ok().map(Aes::A128),
            32 => aes::Aes256::new_from_slice(key).ok().map(Aes::A256),
            _ => None,
        }
    }

    fn encrypt(&self, b: &mut [u8; 16]) {
        let mut blk = (*b).into();
        match self {
            Aes::A128(c) => c.encrypt_block(&mut blk),
            Aes::A256(c) => c.encrypt_block(&mut blk),
        }
        *b = blk.into();
    }

    fn decrypt(&self, b: &mut [u8; 16]) {
        let mut blk = (*b).into();
        match self {
            Aes::A128(c) => c.decrypt_block(&mut blk),
            Aes::A256(c) => c.decrypt_block(&mut blk),
        }
        *b = blk.into();
    }
}

fn cbc_decrypt_in_place(key: &[u8], iv: &[u8; 16], data: &mut [u8]) -> Option<()> {
    let c = Aes::new(key)?;
    let mut prev = *iv;
    for block in data.as_chunks_mut::<16>().0 {
        let ct = *block;
        c.decrypt(block);
        for (b, p) in block.iter_mut().zip(prev) {
            *b ^= p;
        }
        prev = ct;
    }
    Some(())
}

/// IV, then CBC with PKCS#7 padding. Damaged input decrypts as far as it goes.
fn aes_decrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    let Some((iv, body)) = data.split_first_chunk::<16>() else {
        return Vec::new();
    };
    let mut out = body[..body.len() / 16 * 16].to_vec();
    if cbc_decrypt_in_place(key, iv, &mut out).is_none() {
        return Vec::new();
    }
    if let Some(&pad) = out.last()
        && (1..=16).contains(&pad)
        && out.len() >= usize::from(pad)
    {
        out.truncate(out.len() - usize::from(pad));
    }
    out
}

fn aes_encrypt(key: &[u8], data: &[u8]) -> Vec<u8> {
    let Some(c) = Aes::new(key) else {
        return data.to_vec();
    };
    let mut iv = [0u8; 16];
    // Without randomness a fixed IV is still decryptable; it only weakens secrecy.
    let _ = getrandom::fill(&mut iv);
    let pad = 16 - data.len() % 16;
    let mut out = Vec::with_capacity(16 + data.len() + pad);
    out.extend_from_slice(&iv);
    out.extend_from_slice(data);
    out.resize(16 + data.len() + pad, pad as u8);
    let mut prev = iv;
    for block in out[16..].as_chunks_mut::<16>().0 {
        for (b, p) in block.iter_mut().zip(prev) {
            *b ^= p;
        }
        c.encrypt(block);
        prev = *block;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_vector() {
        let mut d = *b"Plaintext";
        rc4(b"Key", &mut d);
        assert_eq!(d, [0xBB, 0xF3, 0x16, 0xE8, 0xD9, 0x40, 0xAF, 0x0A, 0xD3]);
    }

    #[test]
    fn aes_roundtrip() {
        for key in [&[7u8; 16][..], &[9u8; 32]] {
            for len in [0, 1, 15, 16, 17, 100] {
                let data: Vec<u8> = (0..len as u8).collect();
                assert_eq!(aes_decrypt(key, &aes_encrypt(key, &data)), data);
            }
        }
    }
}
