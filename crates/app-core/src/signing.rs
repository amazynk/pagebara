//! Digital signatures as the app shows them: verification reports turned into words for
//! the banner, the signatures panel and the forms panel; the user's trusted certificates;
//! signing identities from files, key stores or made on the spot.

pub mod keystore;

use std::path::Path;

use pagebara_i18n::{tr, trn};
use pagebara_sign::{
    Certificate, CertificateRequest, ChangeKind, Identity, PrivateKey, RevocationStatus,
    SignatureKind, SignatureReport, Status, Time, TimeSource, Trust, TrustSource, TrustStore,
};

/// The verdict on a signature, or on all of a document's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    Valid,
    /// Intact, but the signer's identity is not trusted.
    Untrusted,
    Unknown,
    Invalid,
}

impl Verdict {
    pub fn of(s: Status) -> Verdict {
        match s {
            Status::ValidTrusted => Verdict::Valid,
            Status::ValidUntrusted => Verdict::Untrusted,
            Status::Invalid => Verdict::Invalid,
            Status::Indeterminate => Verdict::Unknown,
        }
    }

    /// 0 valid, 1 untrusted, 2 could not be checked, 3 invalid (the UI's colours).
    pub fn level(self) -> i32 {
        self as i32
    }

    pub fn word(self) -> &'static str {
        match self {
            Verdict::Valid => "valid",
            Verdict::Untrusted => "untrusted",
            Verdict::Unknown => "unknown",
            Verdict::Invalid => "invalid",
        }
    }
}

/// One signature, described for the signatures panel.
#[derive(Clone, Debug, PartialEq)]
pub struct SigView {
    pub field: String,
    pub page: Option<usize>,
    pub verdict: Verdict,
    /// e.g. "Signed by Ann Example – valid".
    pub summary: String,
    pub kind: String,
    pub signer: String,
    /// "2026-11-12 14:03 UTC (from a timestamp)".
    pub time: String,
    pub timestamp: String,
    pub reason: String,
    pub location: String,
    pub trust: String,
    /// The certificate chain, signer first: "Ann Example, issued by Example CA".
    pub chain: Vec<String>,
    pub revocation: String,
    /// Which revision of the file the signature covers, e.g. "Revision 2 of 3: later
    /// changes are allowed".
    pub revision: String,
    /// Whether validation data for the signature is embedded (PAdES LTV).
    pub ltv: String,
    /// What changed after signing, with whether it is allowed.
    pub changes: Vec<(String, bool)>,
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    /// The certificate to trust to make this signature trusted, when it is not.
    pub anchor: Option<Certificate>,
}

/// A time for people: `2026-11-12 14:03 UTC`.
pub fn when(t: Time) -> String {
    let iso = t.to_iso();
    tr!("{} {} UTC", &iso[..10], &iso[11..16])
}

fn trust_text(t: &Trust) -> String {
    match t {
        Trust::Trusted { anchor, source } => match source {
            TrustSource::System => tr!(
                "Trusted: the chain ends at {}, a root certificate of this computer",
                anchor
            ),
            TrustSource::User => {
                tr!(
                    "Trusted: the chain ends at {}, a certificate you trust",
                    anchor
                )
            }
        },
        Trust::SelfSigned => tr!("Not trusted: the certificate is self-signed"),
        Trust::UntrustedRoot { root } => tr!("Not trusted: {} is not a trusted root", root),
        Trust::UnknownIssuer { issuer } => {
            tr!("Not trusted: the issuer {} is unknown", issuer)
        }
        Trust::Invalid => tr!("Not trusted: the certificate chain is not valid"),
    }
}

fn change_kind(k: ChangeKind) -> String {
    match k {
        // TRANSLATORS: kinds of changes made to a document after it was signed
        ChangeKind::FormFill => tr!("Form filling"),
        ChangeKind::Signing => tr!("Signing"),
        ChangeKind::Annotation => tr!("Comments"),
        ChangeKind::Ltv => tr!("Validation data"),
        ChangeKind::Metadata => tr!("Metadata"),
        ChangeKind::Other => tr!("Content"),
    }
}

/// Describes a verification report.
pub fn view(r: &SignatureReport) -> SigView {
    let signer = r
        .signer
        .as_ref()
        .map(|c| match c.email() {
            Some(e) => format!("{} <{e}>", c.display_name()),
            None => c.display_name(),
        })
        .or_else(|| r.info.signer.clone())
        .unwrap_or_else(|| tr!("Unknown signer"));
    let time = match (r.signing_time, r.time_source) {
        (Some(t), TimeSource::Timestamp) => tr!("{} (from a timestamp)", when(t)),
        (Some(t), _) => tr!("{} (the signer's clock)", when(t)),
        // TRANSLATORS: the signing time is not known
        (None, _) => tr!("Unknown"),
    };
    let timestamp = r
        .timestamp
        .as_ref()
        .map(|ts| {
            let by = ts
                .authority
                .as_ref()
                .map(Certificate::display_name)
                .unwrap_or_else(|| tr!("an unknown authority"));
            let at = when(ts.time);
            if !ts.valid {
                // TRANSLATORS: a timestamp: its time, the authority that made it, its validity
                tr!("{} by {} – not valid", at, by)
            } else if ts.trust.is_trusted() {
                tr!("{} by {} – valid", at, by)
            } else {
                tr!("{} by {} – valid, authority not trusted", at, by)
            }
        })
        .unwrap_or_default();
    let kind = match r.kind {
        SignatureKind::Approval => tr!("Signature"),
        SignatureKind::Certification(1) => tr!("Certification: no changes allowed"),
        SignatureKind::Certification(2) => tr!("Certification: form filling and signing allowed"),
        SignatureKind::Certification(_) => {
            tr!("Certification: form filling, signing and comments allowed")
        }
        SignatureKind::DocumentTimestamp => tr!("Document timestamp"),
    };
    let chain = r
        .chain
        .iter()
        .map(|c| {
            if c.is_self_issued() {
                tr!("{} (self-signed)", c.display_name())
            } else {
                tr!(
                    "{}, issued by {}",
                    c.display_name(),
                    c.issuer_display_name()
                )
            }
        })
        .collect();
    let revocation = match &r.revocation {
        RevocationStatus::Good { at, .. } => tr!("Not revoked as of {}", when(*at)),
        RevocationStatus::Revoked { at, .. } => tr!("Revoked on {}", when(*at)),
        RevocationStatus::Unknown => tr!("No revocation information in the document"),
    };
    let later = r.revisions.saturating_sub(r.revision);
    let revision = if r.revision == 0 {
        String::new()
    } else if later == 0 {
        tr!(
            "Revision {} of {}: the whole document as it is now",
            r.revision,
            r.revisions
        )
    } else if r.disallowed_changes().next().is_some() {
        tr!(
            "Revision {} of {}: changed afterwards in ways that are not allowed",
            r.revision,
            r.revisions
        )
    } else {
        tr!(
            "Revision {} of {}: later changes are allowed",
            r.revision,
            r.revisions
        )
    };
    let ltv = if r.vri {
        tr!("Validation data embedded (long-term validation)")
    } else {
        tr!("No validation data embedded")
    };
    let anchor = (!r.trust.is_trusted() && r.trust != Trust::Invalid)
        .then(|| r.chain.last().or(r.signer.as_ref()).cloned())
        .flatten();
    SigView {
        field: r.info.field.clone(),
        page: r.info.page,
        verdict: Verdict::of(r.status),
        summary: summary(r),
        kind,
        signer,
        time,
        timestamp,
        reason: r.info.reason.clone().unwrap_or_default(),
        location: r.info.location.clone().unwrap_or_default(),
        trust: trust_text(&r.trust),
        chain,
        revocation,
        revision,
        ltv,
        changes: r
            .changes
            .iter()
            .map(|c| {
                (
                    format!("{}: {}", change_kind(c.kind), c.description),
                    c.allowed,
                )
            })
            .collect(),
        problems: r.problems.clone(),
        warnings: r.warnings.clone(),
        anchor,
    }
}

/// "Signed by Ann – valid", as [`SignatureReport::summary`] says it in English.
fn summary(r: &SignatureReport) -> String {
    let who = r
        .signer
        .as_ref()
        .map(Certificate::display_name)
        .or_else(|| r.info.signer.clone())
        .unwrap_or_else(|| tr!("unknown signer"));
    // Why it is invalid, in a few words (the details list every problem).
    let why = if r.problems.iter().any(|p| p.contains("after signing")) {
        tr!("the document was changed after it was signed")
    } else {
        tr!("see Signature details for why")
    };
    match (r.kind, r.status) {
        (SignatureKind::DocumentTimestamp, Status::ValidTrusted) => {
            tr!("Timestamped by {} – valid", who)
        }
        (SignatureKind::DocumentTimestamp, Status::ValidUntrusted) => {
            tr!(
                "Timestamped by {} – valid, but the signer's identity is not trusted",
                who
            )
        }
        (SignatureKind::DocumentTimestamp, Status::Invalid) => {
            tr!("Timestamped by {} – INVALID: {}", who, why)
        }
        (SignatureKind::DocumentTimestamp, Status::Indeterminate) => {
            tr!("Timestamped by {} – could not be verified", who)
        }
        (SignatureKind::Certification(_), Status::ValidTrusted) => {
            tr!("Certified by {} – valid", who)
        }
        (SignatureKind::Certification(_), Status::ValidUntrusted) => {
            tr!(
                "Certified by {} – valid, but the signer's identity is not trusted",
                who
            )
        }
        (SignatureKind::Certification(_), Status::Invalid) => {
            tr!("Certified by {} – INVALID: {}", who, why)
        }
        (SignatureKind::Certification(_), Status::Indeterminate) => {
            tr!("Certified by {} – could not be verified", who)
        }
        (SignatureKind::Approval, Status::ValidTrusted) => tr!("Signed by {} – valid", who),
        (SignatureKind::Approval, Status::ValidUntrusted) => {
            tr!(
                "Signed by {} – valid, but the signer's identity is not trusted",
                who
            )
        }
        (SignatureKind::Approval, Status::Invalid) => {
            tr!("Signed by {} – INVALID: {}", who, why)
        }
        (SignatureKind::Approval, Status::Indeterminate) => {
            tr!("Signed by {} – could not be verified", who)
        }
    }
}

/// The worst verdict and a line for the banner; `None` without signatures.
pub fn banner(views: &[SigView]) -> Option<(Verdict, String)> {
    let worst = views.iter().map(|v| v.verdict).max()?;
    if let [one] = views {
        return Some((worst, one.summary.clone()));
    }
    let n = views.len();
    let bad = views.iter().filter(|v| v.verdict == worst).count();
    let text = match worst {
        Verdict::Valid => trn!("{n} signature – all valid", "{n} signatures – all valid", n),
        Verdict::Untrusted => trn!(
            "{n} signatures – intact, {} with a signer that is not trusted",
            "{n} signatures – intact, {} with a signer that is not trusted",
            n,
            bad
        ),
        Verdict::Unknown => trn!(
            "{n} signatures – {} could not be verified",
            "{n} signatures – {} could not be verified",
            n,
            bad
        ),
        Verdict::Invalid => trn!(
            "{n} signatures – {} INVALID",
            "{n} signatures – {} INVALID",
            n,
            bad
        ),
    };
    Some((worst, text))
}

/// The line about a signature field in the forms panel.
pub fn field_status(v: &SigView) -> String {
    match v.verdict {
        Verdict::Valid => tr!("Signature: valid · by {}", v.signer),
        Verdict::Untrusted => tr!("Signature: valid, signer not trusted · by {}", v.signer),
        Verdict::Unknown => tr!("Signature: could not be verified · by {}", v.signer),
        Verdict::Invalid => tr!("Signature: INVALID · by {}", v.signer),
    }
}

/// The system's roots plus the user's own list from `user_file`.
pub fn trust_store(user_file: Option<&Path>) -> TrustStore {
    let mut t = TrustStore::system();
    if let Some(p) = user_file
        && p.exists()
        && let Err(e) = t.load_user_file(p)
    {
        log::warn!("trusted certificates {}: {e}", p.display());
    }
    t
}

/// Writes the user's anchors of `t` to `path` (removing the file when there are none).
pub fn save_user_trust(t: &TrustStore, path: &Path) -> std::io::Result<()> {
    let pem = t.user_pem();
    if pem.is_empty() {
        return match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    crate::session::write_atomic(path, pem.as_bytes())
}

/// The user's anchors: name, issuer, validity and SHA-256 fingerprint.
pub fn user_anchors(t: &TrustStore) -> Vec<(String, String, String)> {
    t.anchors()
        .filter(|(_, s)| *s == TrustSource::User)
        .map(|(c, _)| {
            let detail = tr!(
                "Issued by {} · valid until {}",
                c.issuer_display_name(),
                &c.not_after().to_iso()[..10]
            );
            (c.display_name(), detail, c.fingerprint())
        })
        .collect()
}

/// Reads a signing identity: PKCS #12 (`.p12`, `.pfx`), or PEM with the key and the
/// certificates in one file.
pub fn read_identity(data: &[u8], password: &str) -> Result<Identity, String> {
    let words = |e: pagebara_sign::Error| match e {
        pagebara_sign::Error::WrongPassword => tr!("The password is not right."),
        e => tr!("The certificate file could not be read: {}.", e),
    };
    if data.starts_with(b"-----") || data.windows(10).any(|w| w == b"-----BEGIN") {
        let pw = (!password.is_empty()).then_some(password);
        return Identity::from_pem(data, data, pw).map_err(words);
    }
    Identity::from_pkcs12(data, password).map_err(words)
}

/// A new self-signed signing identity (P-256, valid for five years) as PKCS #12 protected
/// by `password`.
pub fn self_signed_id(name: &str, email: &str, password: &str) -> Result<Vec<u8>, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err(tr!("Enter your name."));
    }
    if password.is_empty() {
        return Err(tr!("Choose a password to protect the ID."));
    }
    let mut req = CertificateRequest::signer(name, 5 * 365);
    let email = email.trim();
    if !email.is_empty() {
        req.email = Some(email.to_owned());
    }
    let key = PrivateKey::generate_p256().map_err(|e| e.to_string())?;
    let id = Identity::self_signed(&req, key).map_err(|e| e.to_string())?;
    id.to_pkcs12(password, Some(name))
        .map_err(|e| e.to_string())
}

/// A file name for an ID: the name without characters file systems dislike.
pub fn id_file_name(name: &str) -> String {
    let base: String = name
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let base = base.trim_matches('.');
    format!("{}.p12", if base.is_empty() { "id" } else { base })
}

#[cfg(test)]
mod tests;
