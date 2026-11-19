//! Certification path building and validation (a practical subset of RFC 5280 §6).

use crate::cert::{Certificate, KeyUsage};
use crate::oid;
use crate::time::Time;
use crate::trust::{TrustSource, TrustStore};

/// Paths longer than this are refused.
const MAX_PATH: usize = 12;

/// Whether a certificate chains to a trust anchor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Trust {
    /// The chain ends at an anchor of the trust store.
    Trusted { anchor: String, source: TrustSource },
    /// The signer's certificate signs itself and is not an anchor.
    SelfSigned,
    /// The chain ends at a root that is not an anchor.
    UntrustedRoot { root: String },
    /// No issuer could be found (the chain is incomplete or its root unknown).
    UnknownIssuer { issuer: String },
    /// A chain was found but breaks the rules (see the problems).
    Invalid,
}

impl Trust {
    pub fn is_trusted(&self) -> bool {
        matches!(self, Trust::Trusted { .. })
    }
}

/// What a certificate is used for: the checks differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Purpose {
    Signing,
    Timestamping,
}

#[derive(Clone, Debug)]
pub(crate) struct Path {
    /// From the end entity upwards.
    pub certs: Vec<Certificate>,
    pub trust: Trust,
    /// Rule violations that make the path unusable at `at`.
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
}

/// Builds and checks the path from `leaf` through `pool` to an anchor of `store`, at time
/// `at`.
pub(crate) fn validate(
    leaf: &Certificate,
    pool: &[Certificate],
    store: &TrustStore,
    at: Time,
    purpose: Purpose,
) -> Path {
    let mut certs = vec![leaf.clone()];
    let mut trust = None;
    loop {
        let cur = certs.last().expect("the path starts with the leaf").clone();
        if let Some((a, s)) = store.anchor_for(&cur) {
            trust = Some(Trust::Trusted {
                anchor: a.display_name(),
                source: s,
            });
            break;
        }
        if certs.len() >= MAX_PATH {
            break;
        }
        // An anchor that signed it, else a certificate from the pool.
        let from_store = store
            .issuers_of(&cur)
            .find(|(a, _)| cur.verify_signed_by(a).is_ok())
            .map(|(a, s)| (a.clone(), Some(s)));
        let from_pool = || {
            pool.iter()
                .filter(|c| **c != cur && !certs.contains(c) && cur.names_issuer(c))
                .find(|c| cur.verify_signed_by(c).is_ok())
                .map(|c| (c.clone(), None))
        };
        match from_store.or_else(from_pool) {
            Some((issuer, Some(s))) => {
                trust = Some(Trust::Trusted {
                    anchor: issuer.display_name(),
                    source: s,
                });
                certs.push(issuer);
                break;
            }
            Some((issuer, None)) => certs.push(issuer),
            None => break,
        }
    }
    let top = certs.last().expect("non-empty").clone();
    let trust = trust.unwrap_or_else(|| {
        if top.is_self_issued() && top.verify_signed_by(&top).is_ok() {
            if certs.len() == 1 {
                Trust::SelfSigned
            } else {
                Trust::UntrustedRoot {
                    root: top.display_name(),
                }
            }
        } else {
            Trust::UnknownIssuer {
                issuer: top.issuer_display_name(),
            }
        }
    });
    let mut path = Path {
        certs,
        trust,
        problems: Vec::new(),
        warnings: Vec::new(),
    };
    check(&mut path, at, purpose);
    if !path.problems.is_empty() && path.trust.is_trusted() {
        path.trust = Trust::Invalid;
    }
    path
}

fn check(path: &mut Path, at: Time, purpose: Purpose) {
    let n = path.certs.len();
    for (i, c) in path.certs.iter().enumerate() {
        let name = c.display_name();
        let anchor = i + 1 == n && path.trust.is_trusted();
        if !c.is_valid_at(at) {
            let when = if at < c.not_before() {
                "not yet valid"
            } else {
                "expired"
            };
            // An anchor's own dates are advisory (RFC 5280 §6.1.1 d).
            if anchor {
                path.warnings
                    .push(format!("the root \"{name}\" was {when} at {at}"));
            } else {
                path.problems
                    .push(format!("the certificate \"{name}\" was {when} at {at}"));
            }
        }
        if let Some(o) = c.unknown_critical_extensions().first() {
            path.warnings
                .push(format!("\"{name}\" has an unknown critical extension {o}"));
        }
        if i == 0 {
            leaf_checks(&mut path.problems, &mut path.warnings, c, purpose);
            continue;
        }
        // Issuers must be CAs allowed to sign certificates, within their path length.
        if !c.is_ca() && !(anchor && c.is_self_issued()) {
            path.problems
                .push(format!("\"{name}\" issued a certificate but is not a CA"));
        }
        if c.key_usage()
            .is_some_and(|k| !k.has(KeyUsage::KEY_CERT_SIGN))
        {
            path.problems
                .push(format!("\"{name}\" may not sign certificates (key usage)"));
        }
        if let Some(max) = c.path_len() {
            // Intermediate CAs below this one, not counting self-issued ones.
            let below = path.certs[1..i]
                .iter()
                .filter(|x| !x.is_self_issued())
                .count();
            if below > max as usize {
                path.problems
                    .push(format!("the path is longer than \"{name}\" allows"));
            }
        }
    }
}

fn leaf_checks(
    problems: &mut Vec<String>,
    warnings: &mut Vec<String>,
    c: &Certificate,
    purpose: Purpose,
) {
    let name = c.display_name();
    match purpose {
        Purpose::Signing => {
            if let Some(k) = c.key_usage()
                && !k.has(KeyUsage::DIGITAL_SIGNATURE)
                && !k.has(KeyUsage::NON_REPUDIATION)
            {
                problems.push(format!("\"{name}\" may not make signatures (key usage)"));
            }
        }
        Purpose::Timestamping => {
            if !c.has_eku(&oid::KP_TIME_STAMPING) {
                warnings.push(format!("\"{name}\" is not a timestamping certificate"));
            }
        }
    }
}
