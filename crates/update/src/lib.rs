//! Opt-in update checks. The release server publishes a signed envelope:
//!
//! ```json
//! {"manifest": "<the manifest, as a JSON string>", "signature": "<Ed25519, 128 hex digits>"}
//! ```
//!
//! The signature covers [`CONTEXT`] followed by the manifest's bytes, and is checked against
//! the public key compiled in (`pagebara_identity::UPDATE_PUBLIC_KEY`). The manifest:
//!
//! ```json
//! {"app_id": "com.amazynk.Pagebara", "version": "0.2.0", "notes": "What changed",
//!  "downloads": {"linux-x86_64-appimage": {"url": "https://...", "sha256": "...", "size": 123}}}
//! ```
//!
//! Download keys are [`Install::download_key`]: `linux-<arch>-appimage`,
//! `windows-<arch>-msi`, `macos-<arch>-dmg`. Only a newer version is offered; a download is
//! used only when its size and SHA-256 match. Nothing here runs unless the user turned
//! update checks on or pressed "Check now".

pub mod install;
#[cfg(feature = "network")]
pub mod net;

use std::collections::BTreeMap;

use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};

pub use install::Install;

/// Signed before the manifest, so the key signs nothing else by accident.
pub const CONTEXT: &[u8] = b"update-manifest-v1\n";
/// Largest envelope read.
pub const MAX_MANIFEST: u64 = 1 << 20;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("updates are not set up in this build")]
    NotConfigured,
    #[error("the update information is malformed: {0}")]
    Malformed(String),
    #[error("the update information is not signed by the app's key")]
    BadSignature,
    #[error("the update information is for another app ({0})")]
    WrongApp(String),
    #[error("the offered version {offered} is older than this one ({current})")]
    Older { offered: String, current: String },
    #[error("the download does not match the update information")]
    BadHash,
    #[error("network: {0}")]
    Network(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// Where to look and whom to trust.
#[derive(Clone, Debug)]
pub struct Config {
    pub url: String,
    pub key: VerifyingKey,
    pub current: semver::Version,
    pub app_id: String,
}

impl Config {
    /// From the app's identity; `None` when no URL or key is set (the feature is hidden).
    pub fn from_identity(current: &str) -> Option<Config> {
        Config::new(
            pagebara_identity::UPDATE_URL,
            pagebara_identity::UPDATE_PUBLIC_KEY,
            current,
        )
        .ok()
    }

    pub fn new(url: &str, key_hex: &str, current: &str) -> Result<Config, Error> {
        if url.is_empty() || key_hex.is_empty() {
            return Err(Error::NotConfigured);
        }
        let key = parse_public_key(key_hex)?;
        let current = semver::Version::parse(current)
            .map_err(|e| Error::Malformed(format!("current version: {e}")))?;
        Ok(Config {
            url: url.to_owned(),
            key,
            current,
            app_id: pagebara_identity::APP_ID.to_owned(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Download {
    pub url: String,
    /// 64 hex digits.
    pub sha256: String,
    pub size: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub app_id: String,
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub downloads: BTreeMap<String, Download>,
}

#[derive(Serialize, Deserialize)]
struct Envelope {
    manifest: String,
    signature: String,
}

/// A newer version, and the download for this install (none for packages updated by the
/// system's package manager or store).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Update {
    pub version: String,
    pub notes: String,
    pub download: Option<Download>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Check {
    UpToDate,
    Available(Update),
}

/// The manifest inside `envelope`, if `key` signed it.
pub fn verify(envelope: &[u8], key: &VerifyingKey) -> Result<Manifest, Error> {
    let env: Envelope =
        serde_json::from_slice(envelope).map_err(|e| Error::Malformed(e.to_string()))?;
    let sig: [u8; 64] = hex_decode(&env.signature)
        .and_then(|b| b.try_into().ok())
        .ok_or(Error::BadSignature)?;
    let mut signed = CONTEXT.to_vec();
    signed.extend_from_slice(env.manifest.as_bytes());
    key.verify_strict(&signed, &Signature::from_bytes(&sig))
        .map_err(|_| Error::BadSignature)?;
    serde_json::from_str(&env.manifest).map_err(|e| Error::Malformed(e.to_string()))
}

/// Whether `m` offers a newer version of this app, with the download for `install`.
pub fn evaluate(m: &Manifest, config: &Config, install: &Install) -> Result<Check, Error> {
    if m.app_id != config.app_id {
        return Err(Error::WrongApp(m.app_id.clone()));
    }
    let offered = semver::Version::parse(&m.version)
        .map_err(|e| Error::Malformed(format!("version: {e}")))?;
    if offered < config.current {
        return Err(Error::Older {
            offered: m.version.clone(),
            current: config.current.to_string(),
        });
    }
    if offered == config.current {
        return Ok(Check::UpToDate);
    }
    let download = install
        .download_key()
        .and_then(|k| m.downloads.get(&k).cloned());
    if let Some(d) = &download
        && hex_decode(&d.sha256).is_none_or(|h| h.len() != 32)
    {
        return Err(Error::Malformed(format!("sha256 of {}", d.url)));
    }
    Ok(Check::Available(Update {
        version: m.version.clone(),
        notes: m.notes.clone(),
        download,
    }))
}

/// Wraps `manifest` (JSON text) in a signed envelope (for `sign-update-manifest`).
pub fn sign(manifest: &str, seed: &[u8; 32]) -> Result<String, Error> {
    let _: Manifest =
        serde_json::from_str(manifest).map_err(|e| Error::Malformed(e.to_string()))?;
    let key = SigningKey::from_bytes(seed);
    let mut signed = CONTEXT.to_vec();
    signed.extend_from_slice(manifest.as_bytes());
    let sig = key.sign(&signed);
    let env = Envelope {
        manifest: manifest.to_owned(),
        signature: hex_encode(&sig.to_bytes()),
    };
    serde_json::to_string_pretty(&env).map_err(|e| Error::Malformed(e.to_string()))
}

/// A fresh signing key (32 random bytes) and its public key in hex.
pub fn generate_key() -> Result<([u8; 32], String), Error> {
    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).map_err(|e| Error::Io(std::io::Error::other(e.to_string())))?;
    let public = public_key_hex(&seed);
    Ok((seed, public))
}

pub fn public_key_hex(seed: &[u8; 32]) -> String {
    hex_encode(SigningKey::from_bytes(seed).verifying_key().as_bytes())
}

/// A signing key file: 64 hex digits (whitespace ignored).
pub fn parse_seed(text: &str) -> Option<[u8; 32]> {
    let digits: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    hex_decode(&digits)?.try_into().ok()
}

pub fn parse_public_key(hex: &str) -> Result<VerifyingKey, Error> {
    let bytes: [u8; 32] = hex_decode(hex.trim())
        .and_then(|b| b.try_into().ok())
        .ok_or_else(|| Error::Malformed("public key".into()))?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| Error::Malformed("public key".into()))
}

pub fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn hex_decode(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEED: [u8; 32] = [7; 32];

    fn manifest(version: &str) -> String {
        format!(
            r#"{{"app_id":"{}","version":"{version}","notes":"Faster.",
                "downloads":{{"linux-x86_64-appimage":{{"url":"http://x/a","sha256":"{}","size":3}}}}}}"#,
            pagebara_identity::APP_ID,
            "ab".repeat(32)
        )
    }

    fn config() -> Config {
        Config::new("http://x/m.json", &public_key_hex(&SEED), "0.1.0").unwrap()
    }

    fn appimage() -> Install {
        Install::AppImage {
            path: "/a.AppImage".into(),
            arch: "x86_64".into(),
        }
    }

    #[test]
    fn signed_newer_version_is_offered() {
        let env = sign(&manifest("0.2.0"), &SEED).unwrap();
        let m = verify(env.as_bytes(), &config().key).unwrap();
        let Check::Available(u) = evaluate(&m, &config(), &appimage()).unwrap() else {
            panic!()
        };
        assert_eq!(u.version, "0.2.0");
        assert_eq!(u.download.unwrap().size, Some(3));
        // A .deb install hears of it but downloads nothing.
        let Check::Available(u) = evaluate(&m, &config(), &Install::Deb).unwrap() else {
            panic!()
        };
        assert!(u.download.is_none());
    }

    #[test]
    fn tampering_and_other_keys_fail() {
        let env = sign(&manifest("0.2.0"), &SEED).unwrap();
        let tampered = env.replace("0.2.0", "0.9.0");
        assert!(matches!(
            verify(tampered.as_bytes(), &config().key),
            Err(Error::BadSignature)
        ));
        let other = sign(&manifest("0.2.0"), &[8; 32]).unwrap();
        assert!(matches!(
            verify(other.as_bytes(), &config().key),
            Err(Error::BadSignature)
        ));
        assert!(matches!(
            verify(b"{}", &config().key),
            Err(Error::Malformed(_))
        ));
    }

    #[test]
    fn same_or_older_is_not_offered() {
        let c = config();
        let same = verify(sign(&manifest("0.1.0"), &SEED).unwrap().as_bytes(), &c.key).unwrap();
        assert_eq!(evaluate(&same, &c, &appimage()).unwrap(), Check::UpToDate);
        let old = verify(sign(&manifest("0.0.9"), &SEED).unwrap().as_bytes(), &c.key).unwrap();
        assert!(matches!(
            evaluate(&old, &c, &appimage()),
            Err(Error::Older { .. })
        ));
        let mut other = same.clone();
        other.app_id = "org.other".into();
        other.version = "9.0.0".into();
        assert!(matches!(
            evaluate(&other, &c, &appimage()),
            Err(Error::WrongApp(_))
        ));
    }

    #[test]
    fn keys_and_hex() {
        let (seed, public) = generate_key().unwrap();
        assert_eq!(public, public_key_hex(&seed));
        assert_eq!(parse_seed(&format!(" {}\n", hex_encode(&seed))), Some(seed));
        assert!(parse_seed("abc").is_none());
        assert!(Config::new("", &public, "0.1.0").is_err());
        assert!(Config::new("http://x", "", "0.1.0").is_err());
        assert!(
            Config::from_identity("0.1.0").is_none() || !pagebara_identity::UPDATE_URL.is_empty()
        );
    }
}
