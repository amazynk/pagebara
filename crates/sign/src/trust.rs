//! Trust anchors: the operating system's root certificates and the user's own list.
//!
//! On Linux and the BSDs the system roots are read from the usual PEM bundles. macOS ships
//! a bundle at `/etc/ssl/cert.pem`, which is read; the Keychain (including certificates the
//! user trusted there) and the Windows certificate store need platform APIs, which this
//! engine does not call: the app reads them (e.g. with the `security-framework` or
//! `schannel` crates) and hands the DER to [`TrustStore::add`].
//!
//! System roots are made for TLS and include many CAs that never issue signing
//! certificates; document signing lists (Adobe AATL, the EU trusted lists) are kept as
//! user anchors.

use std::path::Path;

use crate::Result;
use crate::cert::Certificate;

/// Where a trust anchor came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrustSource {
    System,
    User,
}

/// Certificates trusted as roots of signing chains.
#[derive(Clone, Debug, Default)]
pub struct TrustStore {
    anchors: Vec<(Certificate, TrustSource)>,
}

/// Bundles of the system's root certificates, first found wins.
const BUNDLES: [&str; 7] = [
    "/etc/ssl/certs/ca-certificates.crt",
    "/etc/pki/tls/certs/ca-bundle.crt",
    "/etc/pki/ca-trust/extracted/pem/tls-ca-bundle.pem",
    "/etc/ssl/ca-bundle.pem",
    "/etc/ssl/cert.pem",
    "/usr/local/share/certs/ca-root-nss.crt",
    "/usr/local/etc/ssl/cert.pem",
];

impl TrustStore {
    /// No anchors: every signature verifies as untrusted at best.
    pub fn new() -> Self {
        Self::default()
    }

    /// The system's root certificates (see the module documentation).
    pub fn system() -> Self {
        let mut s = Self::new();
        s.load_system();
        s
    }

    /// Adds the system's roots; returns how many were added.
    pub fn load_system(&mut self) -> usize {
        for path in BUNDLES {
            if let Ok(pem) = std::fs::read(path)
                && let Ok(n) = self.add_pem(&pem, TrustSource::System)
                && n > 0
            {
                return n;
            }
        }
        // Some systems keep one PEM file per root instead.
        let mut n = 0;
        if let Ok(dir) = std::fs::read_dir("/etc/ssl/certs") {
            for e in dir.flatten() {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "pem" || x == "crt")
                    && let Ok(pem) = std::fs::read(&p)
                {
                    n += self.add_pem(&pem, TrustSource::System).unwrap_or(0);
                }
            }
        }
        n
    }

    /// Adds `cert` unless already present. Returns whether it was added.
    pub fn add(&mut self, cert: Certificate, source: TrustSource) -> bool {
        if self.anchors.iter().any(|(c, _)| *c == cert) {
            return false;
        }
        self.anchors.push((cert, source));
        true
    }

    /// Adds every certificate in PEM text; returns how many were new.
    pub fn add_pem(&mut self, pem: &[u8], source: TrustSource) -> Result<usize> {
        Ok(Certificate::from_pem(pem)?
            .into_iter()
            .filter(|c| self.add(c.clone(), source))
            .count())
    }

    /// Reads a PEM file of user anchors, e.g. one written from [`TrustStore::user_pem`].
    pub fn load_user_file(&mut self, path: &Path) -> Result<usize> {
        let pem = std::fs::read(path)?;
        self.add_pem(&pem, TrustSource::User)
    }

    /// The user's anchors as PEM, for saving.
    pub fn user_pem(&self) -> String {
        self.anchors
            .iter()
            .filter(|(_, s)| *s == TrustSource::User)
            .map(|(c, _)| c.to_pem())
            .collect()
    }

    /// Removes the anchor with this SHA-256 fingerprint; returns whether one was removed.
    pub fn remove(&mut self, fingerprint: &str) -> bool {
        let before = self.anchors.len();
        self.anchors
            .retain(|(c, _)| !c.fingerprint().eq_ignore_ascii_case(fingerprint));
        self.anchors.len() != before
    }

    pub fn anchors(&self) -> impl Iterator<Item = (&Certificate, TrustSource)> {
        self.anchors.iter().map(|(c, s)| (c, *s))
    }

    pub fn len(&self) -> usize {
        self.anchors.len()
    }

    pub fn is_empty(&self) -> bool {
        self.anchors.is_empty()
    }

    /// The anchor that is `cert` itself (same certificate, or same name and key).
    pub(crate) fn anchor_for(&self, cert: &Certificate) -> Option<(&Certificate, TrustSource)> {
        self.anchors().find(|(a, _)| {
            *a == cert || (a.subject_raw() == cert.subject_raw() && a.spki() == cert.spki())
        })
    }

    /// Anchors that may have issued `cert`.
    pub(crate) fn issuers_of<'a>(
        &'a self,
        cert: &'a Certificate,
    ) -> impl Iterator<Item = (&'a Certificate, TrustSource)> {
        self.anchors().filter(move |(a, _)| cert.names_issuer(a))
    }
}
