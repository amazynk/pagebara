//! Verifying every signature of a document.

use std::collections::HashMap;

use pagebara_forms::{Form, SignatureInfo};
use pagebara_syntax::{Dict, Document, Object};

use crate::cert::Certificate;
use crate::chain::{self, Purpose, Trust};
use crate::cms;
use crate::crypto::{DigestAlgorithm, PublicKey, SignatureScheme};
use crate::mdp::{self, Change, Locks};
use crate::revocation::{RevocationData, RevocationStatus};
use crate::time::Time;
use crate::trust::TrustStore;
use crate::tsp::TstInfo;
use crate::{asn1, oid};

/// The verdict on one signature.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Intact, allowed changes only, and the signer chains to a trusted root.
    ValidTrusted,
    /// Intact and allowed changes only, but the signer is not trusted (unknown issuer,
    /// self-signed, or no trust anchors configured).
    ValidUntrusted,
    /// The document was altered, the signature does not match, or the changes made after
    /// signing are not allowed.
    Invalid,
    /// It cannot be decided: an unsupported algorithm, a missing certificate, a certificate
    /// that was not valid when signing, damaged data.
    Indeterminate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureKind {
    Approval,
    /// A certification (DocMDP) signature with its permission level 1–3.
    Certification(u8),
    /// A document timestamp (`ETSI.RFC3161`).
    DocumentTimestamp,
}

/// Where the reported signing time comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimeSource {
    /// A verified RFC 3161 timestamp.
    Timestamp,
    /// The signer's own clock (signed attribute or the `/M` entry).
    SignerClock,
    Unknown,
}

/// An RFC 3161 timestamp token and its checks.
#[derive(Clone, Debug)]
pub struct TimestampReport {
    pub time: Time,
    pub authority: Option<Certificate>,
    /// The token's signature verifies and it covers the right data.
    pub valid: bool,
    pub trust: Trust,
    /// From the authority's certificate upwards.
    pub chain: Vec<Certificate>,
    pub problems: Vec<String>,
}

/// Everything checked about one signature.
#[derive(Clone, Debug)]
pub struct SignatureReport {
    /// The field and what its signature dictionary says (from `pagebara-forms`).
    pub info: SignatureInfo,
    pub kind: SignatureKind,
    pub status: Status,
    /// The signed bytes are unchanged and the signature value matches them.
    pub intact: bool,
    pub signer: Option<Certificate>,
    /// From the signer up to the root as far as it could be built.
    pub chain: Vec<Certificate>,
    pub trust: Trust,
    pub signing_time: Option<Time>,
    pub time_source: TimeSource,
    pub timestamp: Option<TimestampReport>,
    pub algorithm: Option<String>,
    pub digest: Option<DigestAlgorithm>,
    /// What embedded revocation data says about the signer's certificate.
    pub revocation: RevocationStatus,
    /// Revision (1-based) the signature covers, of `revisions` in the file.
    pub revision: usize,
    pub revisions: usize,
    /// Changes made after this signature (later incremental updates).
    pub changes: Vec<Change>,
    /// The key of the signature's `/VRI` entry ([`crate::vri_key`]); empty without
    /// `/Contents`.
    pub vri_key: String,
    /// The Document Security Store has a `/VRI` entry for this signature (PAdES B-LT).
    pub vri: bool,
    /// Why the status is not valid.
    pub problems: Vec<String>,
    /// Things worth knowing that do not change the status.
    pub warnings: Vec<String>,
}

impl SignatureReport {
    /// Changes after signing that the signature does not allow.
    pub fn disallowed_changes(&self) -> impl Iterator<Item = &Change> {
        self.changes.iter().filter(|c| !c.allowed)
    }

    /// One line for people, e.g. "Signed by Ann Example – valid".
    pub fn summary(&self) -> String {
        let who = self
            .signer
            .as_ref()
            .map(Certificate::display_name)
            .or_else(|| self.info.signer.clone())
            .unwrap_or_else(|| "unknown signer".into());
        let what = match self.kind {
            SignatureKind::DocumentTimestamp => "Timestamped by",
            SignatureKind::Certification(_) => "Certified by",
            SignatureKind::Approval => "Signed by",
        };
        let verdict = match self.status {
            Status::ValidTrusted => "valid",
            Status::ValidUntrusted => "valid, but the signer's identity is not trusted",
            Status::Invalid => "INVALID",
            Status::Indeterminate => "could not be verified",
        };
        format!("{what} {who} – {verdict}")
    }
}

/// What [`verify`] checks against.
#[derive(Clone, Copy, Debug)]
pub struct VerifyOptions<'a> {
    pub trust: &'a TrustStore,
    /// The present time (for "expired now" warnings); the system clock when `None`.
    pub now: Option<Time>,
    /// The password the document was opened with, to read its earlier revisions.
    pub password: Option<&'a str>,
}

impl<'a> VerifyOptions<'a> {
    pub fn new(trust: &'a TrustStore) -> Self {
        VerifyOptions {
            trust,
            now: None,
            password: None,
        }
    }
}

/// Verifies every signed signature field of `doc` (as opened: byte ranges refer to
/// [`Document::source`]).
pub fn verify(doc: &Document, opts: &VerifyOptions<'_>) -> Vec<SignatureReport> {
    let form = Form::load(doc);
    let infos = form.signatures(doc);
    let (_, nodes) = mdp::field_nodes(doc);
    let by_name: HashMap<&str, u32> = nodes
        .iter()
        .filter(|(_, f)| f.is_signature)
        .map(|(&n, f)| (f.name.as_str(), n))
        .collect();
    let dss = dss_contents(doc);
    let vri = vri_keys(doc);
    let revisions = eof_markers(doc.source()).len().max(1);
    // The certification signature's level governs changes after it.
    let certification = infos
        .iter()
        .filter(|i| i.signed && !i.timestamp)
        .find_map(|i| i.doc_mdp);
    let mut revision_docs: HashMap<usize, Option<Document>> = HashMap::new();
    let mut out = Vec::new();
    for info in infos.into_iter().filter(|i| i.signed) {
        let field = by_name.get(info.field.as_str()).copied();
        let node = field.and_then(|n| doc.get(pagebara_syntax::ObjRef::new(n, 0)));
        let field_dict = node.as_deref().and_then(Object::as_dict).cloned();
        let v = field_dict
            .as_ref()
            .and_then(|d| doc.get_in(d, b"V").as_dict().cloned())
            .unwrap_or_default();
        let ctx = Ctx {
            doc,
            opts,
            dss: &dss,
            vri: &vri,
            revisions,
        };
        let mut report = ctx.verify_one(info, &v);
        // Later revisions: classify what changed.
        if let Some(br) = report.info.byte_range.clone()
            && br.later_revisions > 0
            && br.covers_revision
        {
            let old = revision_docs
                .entry(br.signed_end)
                .or_insert_with(|| open_revision(doc, br.signed_end, opts.password));
            match old {
                Some(old) => {
                    let p = match report.kind {
                        SignatureKind::Certification(p) => p,
                        _ => certification.unwrap_or(3),
                    };
                    let mut locks = Locks::None;
                    if let Some(l) = field_dict.as_ref().map(|d| doc.get_in(d, b"Lock"))
                        && let Some(l) = l.as_dict()
                    {
                        locks = locks.merge(Locks::from_dict(doc, l));
                    }
                    locks = locks.merge(field_mdp(doc, &v));
                    report.changes = mdp::diff(old, doc, p, &locks);
                }
                None => report
                    .warnings
                    .push("the signed revision could not be read to compare changes".into()),
            }
            if report.disallowed_changes().next().is_some() {
                report.problems.push(
                    "the document was changed after signing in ways that are not allowed".into(),
                );
                report.status = Status::Invalid;
            }
        }
        out.push(report);
    }
    out
}

/// The document as it was when its first `end` bytes were the whole file.
fn open_revision(doc: &Document, end: usize, password: Option<&str>) -> Option<Document> {
    let src = doc.source().get(..end)?.to_vec();
    match Document::open(src) {
        Ok(d) => Some(d),
        Err(pagebara_syntax::Error::PasswordRequired(locked)) => locked.unlock(password?).ok(),
        Err(_) => None,
    }
}

fn eof_markers(src: &[u8]) -> Vec<usize> {
    memchr_all(src, b"%%EOF")
}

fn memchr_all(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    hay.windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle)
        .map(|(i, _)| i)
        .collect()
}

/// Locks named by FieldMDP references in a signature dictionary.
fn field_mdp(doc: &Document, v: &Dict) -> Locks {
    let mut locks = Locks::None;
    for r in doc.get_in(v, b"Reference").as_array().unwrap_or_default() {
        let r = doc.resolve(r);
        if let Some(r) = r.as_dict()
            && r.name(b"TransformMethod") == Some(b"FieldMDP")
            && let Some(p) = doc.get_in(r, b"TransformParams").as_dict()
        {
            locks = locks.merge(Locks::from_dict(doc, p));
        }
    }
    locks
}

/// The keys of the Document Security Store's `/VRI` entries.
fn vri_keys(doc: &Document) -> Vec<String> {
    let Some(cat) = doc.catalog() else {
        return Vec::new();
    };
    let Some(dss) = cat.as_dict().map(|c| doc.get_in(c, b"DSS")) else {
        return Vec::new();
    };
    let Some(vri) = dss.as_dict().map(|d| doc.get_in(d, b"VRI")) else {
        return Vec::new();
    };
    vri.as_dict()
        .map(|v| {
            v.iter()
                .map(|(k, _)| String::from_utf8_lossy(k.as_bytes()).to_ascii_uppercase())
                .collect()
        })
        .unwrap_or_default()
}

/// Certificates and revocation data in the Document Security Store.
fn dss_contents(doc: &Document) -> RevocationData {
    let mut out = RevocationData::default();
    let Some(cat) = doc.catalog() else { return out };
    let Some(cat) = cat.as_dict() else { return out };
    let dss = doc.get_in(cat, b"DSS");
    let Some(dss) = dss.as_dict() else { return out };
    let streams = |d: &Dict, k: &[u8]| -> Vec<Vec<u8>> {
        doc.get_in(d, k)
            .as_array()
            .unwrap_or_default()
            .iter()
            .filter_map(|o| {
                let s = doc.resolve(o);
                let s = s.as_stream()?;
                doc.decode(s).ok().map(|d| d.data)
            })
            .collect()
    };
    let mut add = |d: &Dict, certs: &[u8], ocsps: &[u8], crls: &[u8]| {
        for c in streams(d, certs) {
            if let Ok(c) = Certificate::from_der(&c)
                && !out.certs.contains(&c)
            {
                out.certs.push(c);
            }
        }
        out.ocsps.extend(streams(d, ocsps));
        out.crls.extend(streams(d, crls));
    };
    add(dss, b"Certs", b"OCSPs", b"CRLs");
    if let Some(vri) = doc.get_in(dss, b"VRI").as_dict() {
        for (_, v) in vri.iter() {
            if let Some(v) = doc.resolve(v).as_dict() {
                add(v, b"Cert", b"OCSP", b"CRL");
            }
        }
    }
    out.ocsps.sort();
    out.ocsps.dedup();
    out.crls.sort();
    out.crls.dedup();
    out
}

struct Ctx<'a> {
    doc: &'a Document,
    opts: &'a VerifyOptions<'a>,
    dss: &'a RevocationData,
    vri: &'a [String],
    revisions: usize,
}

impl Ctx<'_> {
    fn verify_one(&self, info: SignatureInfo, v: &Dict) -> SignatureReport {
        let kind = if info.timestamp || info.sub_filter.as_deref() == Some("ETSI.RFC3161") {
            SignatureKind::DocumentTimestamp
        } else if let Some(p) = info.doc_mdp {
            SignatureKind::Certification(p)
        } else {
            SignatureKind::Approval
        };
        let revision = info.byte_range.as_ref().map_or(0, |b| {
            eof_markers(&self.doc.source()[..b.signed_end.min(self.doc.source().len())]).len()
        });
        let mut r = SignatureReport {
            info,
            kind,
            status: Status::Indeterminate,
            intact: false,
            signer: None,
            chain: Vec::new(),
            trust: Trust::UnknownIssuer {
                issuer: String::new(),
            },
            signing_time: None,
            time_source: TimeSource::Unknown,
            timestamp: None,
            algorithm: None,
            digest: None,
            revocation: RevocationStatus::Unknown,
            revision,
            revisions: self.revisions,
            changes: Vec::new(),
            vri_key: String::new(),
            vri: false,
            problems: Vec::new(),
            warnings: Vec::new(),
        };
        let contents = self.doc.get_in(v, b"Contents");
        let contents = contents.as_string().unwrap_or_default().to_vec();
        if !contents.is_empty() {
            r.vri_key = crate::ltv::vri_key(&contents);
            r.vri = self.vri.contains(&r.vri_key);
        }
        let Some(br) = r.info.byte_range.clone() else {
            r.problems
                .push("the signature has no valid byte range".into());
            return r;
        };
        let src = self.doc.source();
        let parts: Vec<&[u8]> = br
            .ranges
            .iter()
            .filter_map(|&(o, l)| src.get(o..o.checked_add(l)?))
            .collect();
        if parts.len() != br.ranges.len() {
            r.problems
                .push("the byte range lies outside the file".into());
            return r;
        }
        let mut fatal = false;
        if !br.well_formed {
            r.problems
                .push("the signed byte range does not cover the whole revision".into());
            fatal = true;
        }
        if !br.covers_revision {
            r.warnings
                .push("the signed bytes do not end at a revision boundary".into());
        }
        let sub = r.info.sub_filter.clone().unwrap_or_default();
        let claimed = self
            .doc
            .get_in(v, b"M")
            .as_string()
            .and_then(|b| Time::parse_pdf(&mdp::decode_text(b)));
        match sub.as_str() {
            "ETSI.RFC3161" => self.document_timestamp(&mut r, &contents, &parts),
            "adbe.x509.rsa_sha1" => self.x509_rsa_sha1(&mut r, v, &contents, &parts, claimed),
            _ => self.cms_signature(&mut r, &sub, &contents, &parts, claimed),
        }
        if fatal && r.status != Status::Indeterminate {
            r.status = Status::Invalid;
        }
        r
    }

    /// Settles the status from what was found.
    fn conclude(&self, r: &mut SignatureReport, intact: bool, indeterminate: bool) {
        r.intact = intact;
        r.status = if !intact {
            Status::Invalid
        } else if indeterminate {
            Status::Indeterminate
        } else if r.trust.is_trusted() {
            Status::ValidTrusted
        } else {
            Status::ValidUntrusted
        };
        if let Trust::UnknownIssuer { issuer } = &r.trust {
            r.warnings.push(format!(
                "the issuer \"{issuer}\" is not among the trusted certificates"
            ));
        } else if r.trust == Trust::SelfSigned {
            r.warnings
                .push("the signer's certificate is self-signed".into());
        } else if let Trust::UntrustedRoot { root } = &r.trust {
            r.warnings.push(format!(
                "the root \"{root}\" is not among the trusted certificates"
            ));
        }
    }

    fn now(&self) -> Time {
        self.opts.now.unwrap_or_else(Time::now)
    }

    fn cms_signature(
        &self,
        r: &mut SignatureReport,
        sub: &str,
        contents: &[u8],
        data: &[&[u8]],
        claimed_m: Option<Time>,
    ) {
        let sd = match cms::parse(contents) {
            Ok(sd) => sd,
            Err(e) => {
                r.problems
                    .push(format!("the signature cannot be read: {e}"));
                return;
            }
        };
        if !matches!(
            sub,
            "adbe.pkcs7.detached" | "ETSI.CAdES.detached" | "adbe.pkcs7.sha1"
        ) {
            r.warnings.push(format!("unknown signature format {sub:?}"));
        }
        let Some(si) = sd.signers.first() else {
            r.problems.push("the signature has no signer".into());
            return;
        };
        if sd.signers.len() > 1 {
            r.warnings
                .push("only the first of several signers is checked".into());
        }
        let mut pool = sd.certificates.clone();
        for c in &self.dss.certs {
            if !pool.contains(c) {
                pool.push(c.clone());
            }
        }
        let Some(cert) = si.find_certificate(&pool).cloned() else {
            r.problems
                .push("the signer's certificate is not in the signature".into());
            return;
        };
        r.signer = Some(cert.clone());
        let digest = match si.digest {
            Ok(d) => d,
            Err(o) => {
                r.problems.push(format!("unsupported digest algorithm {o}"));
                return;
            }
        };
        r.digest = Some(digest);
        if digest.is_weak() {
            r.warnings
                .push(format!("{} is too weak for new signatures", digest.name()));
        }
        let mut intact = true;
        let content_hash = if sub == "adbe.pkcs7.sha1" {
            // The signed content is the SHA-1 of the byte range.
            let ec = sd.content.clone().unwrap_or_default();
            if ec != DigestAlgorithm::Sha1.digest(data) {
                r.problems
                    .push("the document was altered after signing".into());
                intact = false;
            }
            digest.digest(&[&ec])
        } else {
            digest.digest(data)
        };
        let check = si.check(&cert, &content_hash);
        if let Ok(s) = &check.scheme {
            r.algorithm = Some(s.name());
        }
        let mut indeterminate = false;
        if intact && !check.digest_ok {
            r.problems
                .push("the document was altered after signing".into());
            intact = false;
        }
        match (&check.scheme, &check.signature) {
            (Err(e), _) => {
                r.problems.push(e.clone());
                indeterminate = true;
            }
            (Ok(_), Err(e)) => {
                if e.starts_with("unsupported") || e.contains("not supported") {
                    indeterminate = true;
                } else {
                    intact = false;
                }
                r.problems.push(format!("signature check failed: {e}"));
            }
            _ => {}
        }
        if check.signing_certificate == Some(false) {
            r.problems
                .push("the signing-certificate attribute names another certificate".into());
            intact = false;
        }
        // Signing time: a valid timestamp beats the signer's clock.
        let claimed = si.signing_time().or(claimed_m);
        if let Some(token) = si.timestamp_token() {
            let (ts, ts_intact) = self.check_token(token.raw, &[&si.signature]);
            if !ts.valid {
                r.problems
                    .extend(ts.problems.iter().map(|p| format!("timestamp: {p}")));
                if ts_intact {
                    indeterminate = true;
                } else {
                    intact = false;
                }
            } else if !ts.trust.is_trusted() {
                r.warnings
                    .push("the timestamp authority is not trusted".into());
            }
            r.timestamp = Some(ts);
        }
        let ts_time = r.timestamp.as_ref().filter(|t| t.valid).map(|t| t.time);
        (r.signing_time, r.time_source) = match (ts_time, claimed) {
            (Some(t), _) => (Some(t), TimeSource::Timestamp),
            (None, Some(c)) => (Some(c), TimeSource::SignerClock),
            (None, None) => (None, TimeSource::Unknown),
        };
        if let (Some(t), Some(c)) = (ts_time, claimed)
            && c.0 > t.0 + 300
        {
            r.warnings
                .push("the claimed signing time is later than the timestamp".into());
        }
        let at = r.signing_time.unwrap_or_else(|| self.now());
        let revoked =
            self.chain_and_revocation(r, &cert, &pool, &sd.crls, si, at, &mut indeterminate);
        self.conclude(r, intact, indeterminate);
        if revoked {
            r.status = Status::Invalid;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn chain_and_revocation(
        &self,
        r: &mut SignatureReport,
        cert: &Certificate,
        pool: &[Certificate],
        crls: &[Vec<u8>],
        si: &cms::SignerInfo<'_>,
        at: Time,
        indeterminate: &mut bool,
    ) -> bool {
        let path = chain::validate(cert, pool, self.opts.trust, at, Purpose::Signing);
        if !path.problems.is_empty() {
            *indeterminate = true;
        }
        r.problems.extend(path.problems.iter().cloned());
        r.warnings.extend(path.warnings.iter().cloned());
        if !cert.is_valid_at(self.now()) && cert.is_valid_at(at) {
            r.warnings.push(format!(
                "the signer's certificate has expired since signing ({})",
                cert.not_after()
            ));
        }
        r.chain = path.certs.clone();
        r.trust = path.trust.clone();
        // Revocation: the DSS, CRLs in the signature, Adobe's archival attribute.
        let mut data = self.dss.clone();
        data.crls.extend(crls.iter().cloned());
        if let Some(arch) = cms::attribute(si.signed_attrs.as_ref(), &oid::ADBE_REVOCATION) {
            for part in arch.reader() {
                let Some(list) = part.reader().next() else {
                    continue;
                };
                for item in list.reader() {
                    match part.tag {
                        0xA0 => data.crls.push(item.raw.to_vec()),
                        0xA1 => data.ocsps.push(item.raw.to_vec()),
                        _ => {}
                    }
                }
            }
        }
        data.certs.extend(pool.iter().cloned());
        if let Some(issuer) = path.certs.get(1) {
            r.revocation = data.status(cert, issuer);
        }
        if let RevocationStatus::Revoked { at: when, .. } = r.revocation {
            if when <= at {
                r.problems
                    .push(format!("the signer's certificate was revoked on {when}"));
                return true;
            } else if r.time_source == TimeSource::Timestamp {
                r.warnings.push(format!(
                    "the signer's certificate was revoked on {when}, after the timestamped signing time"
                ));
            } else {
                r.problems.push(format!(
                    "the signer's certificate was revoked on {when}; without a timestamp the signing time cannot be proven earlier"
                ));
                *indeterminate = true;
            }
        }
        false
    }

    /// Checks a timestamp token over `data`; also whether the token itself is intact (its
    /// imprint and signature match), as opposed to merely untrusted or unreadable.
    fn check_token(&self, token: &[u8], data: &[&[u8]]) -> (TimestampReport, bool) {
        let mut out = TimestampReport {
            time: Time(0),
            authority: None,
            valid: false,
            trust: Trust::UnknownIssuer {
                issuer: String::new(),
            },
            chain: Vec::new(),
            problems: Vec::new(),
        };
        let sd = match cms::parse(token) {
            Ok(sd) => sd,
            Err(e) => {
                out.problems.push(format!("cannot be read: {e}"));
                return (out, true);
            }
        };
        let tst = match TstInfo::parse(sd.content.as_deref().unwrap_or_default()) {
            Ok(t) if sd.content_type == oid::TST_INFO => t,
            _ => {
                out.problems.push("not a timestamp token".into());
                return (out, true);
            }
        };
        out.time = tst.time;
        let Some(d) = tst.digest else {
            out.problems.push("unsupported imprint digest".into());
            return (out, true);
        };
        let imprint_ok = d.digest(data) == tst.imprint;
        if !imprint_ok {
            out.problems.push("the timestamp is for other data".into());
        }
        let Some(si) = sd.signers.first() else {
            out.problems.push("no signer".into());
            return (out, true);
        };
        let mut pool = sd.certificates.clone();
        pool.extend(self.dss.certs.iter().cloned());
        let Some(cert) = si.find_certificate(&pool).cloned() else {
            out.problems
                .push("the authority's certificate is missing".into());
            return (out, imprint_ok);
        };
        let content = sd.content.as_deref().unwrap_or_default();
        let check = match si.digest {
            Ok(d) => si.check(&cert, &d.digest(&[content])),
            Err(o) => {
                out.problems.push(format!("unsupported digest {o}"));
                return (out, true);
            }
        };
        if !check.digest_ok {
            out.problems.push("the token's content was altered".into());
        }
        if let Err(e) = &check.signature {
            out.problems.push(format!("signature check failed: {e}"));
        }
        if check.signing_certificate == Some(false) {
            out.problems
                .push("the signing-certificate attribute names another certificate".into());
        }
        let path = chain::validate(
            &cert,
            &pool,
            self.opts.trust,
            tst.time,
            Purpose::Timestamping,
        );
        out.problems.extend(path.problems.iter().cloned());
        out.valid = imprint_ok
            && check.digest_ok
            && check.signature.is_ok()
            && check.signing_certificate != Some(false)
            && path.problems.is_empty();
        out.trust = path.trust;
        out.chain = path.certs;
        out.authority = Some(cert);
        let intact = imprint_ok && check.digest_ok && check.signature.is_ok();
        (out, intact)
    }

    fn document_timestamp(&self, r: &mut SignatureReport, contents: &[u8], data: &[&[u8]]) {
        let (ts, intact) = self.check_token(contents, data);
        r.signer = ts.authority.clone();
        r.trust = ts.trust.clone();
        r.signing_time = Some(ts.time);
        r.time_source = TimeSource::Timestamp;
        r.problems.extend(ts.problems.iter().cloned());
        if let Some(a) = &ts.authority
            && let Ok(sd) = cms::parse(contents)
        {
            let mut pool = sd.certificates.clone();
            pool.extend(self.dss.certs.iter().cloned());
            r.chain =
                chain::validate(a, &pool, self.opts.trust, ts.time, Purpose::Timestamping).certs;
            r.digest = sd.signers.first().and_then(|s| s.digest.ok());
            r.algorithm = sd
                .signers
                .first()
                .and_then(|s| s.scheme().ok())
                .map(SignatureScheme::name);
        }
        let indeterminate = !ts.valid;
        r.timestamp = Some(ts);
        self.conclude(r, intact, indeterminate);
    }

    /// `adbe.x509.rsa_sha1`: a bare PKCS #1 signature of the SHA-1 of the byte range,
    /// certificates in `/Cert`.
    fn x509_rsa_sha1(
        &self,
        r: &mut SignatureReport,
        v: &Dict,
        contents: &[u8],
        data: &[&[u8]],
        claimed: Option<Time>,
    ) {
        let certs: Vec<Certificate> = match &*self.doc.get_in(v, b"Cert") {
            Object::String(s) => Certificate::from_der(&s.bytes).into_iter().collect(),
            Object::Array(a) => a
                .iter()
                .filter_map(|o| Certificate::from_der(self.doc.resolve(o).as_string()?).ok())
                .collect(),
            _ => Vec::new(),
        };
        let Some(cert) = certs.first().cloned() else {
            r.problems
                .push("the signer's certificate is missing".into());
            return;
        };
        r.signer = Some(cert.clone());
        let sig = asn1::parse(contents)
            .filter(|(t, _)| t.tag == asn1::OCTET_STRING)
            .map(|(t, _)| t.value.to_vec())
            .unwrap_or_else(|| contents.to_vec());
        let digest = DigestAlgorithm::Sha1;
        r.digest = Some(digest);
        r.algorithm = Some(SignatureScheme::RsaPkcs1(digest).name());
        r.warnings
            .push("SHA-1 is too weak for new signatures".into());
        let ok = match cert.public_key() {
            Ok(k @ PublicKey::Rsa(_)) => k
                .verify_prehash(
                    SignatureScheme::RsaPkcs1(digest),
                    &digest.digest(data),
                    &sig,
                )
                .is_ok(),
            _ => false,
        };
        if !ok {
            r.problems
                .push("the document was altered or the signature does not match".into());
        }
        r.signing_time = claimed;
        r.time_source = if r.signing_time.is_some() {
            TimeSource::SignerClock
        } else {
            TimeSource::Unknown
        };
        let at = r.signing_time.unwrap_or_else(|| self.now());
        let path = chain::validate(&cert, &certs, self.opts.trust, at, Purpose::Signing);
        let indeterminate = !path.problems.is_empty();
        r.problems.extend(path.problems);
        r.chain = path.certs;
        r.trust = path.trust;
        self.conclude(r, ok, indeterminate);
    }
}

#[cfg(test)]
mod tests {
    use pagebara_syntax::{Dict, Document, Object};

    use super::*;
    use crate::cert::CertificateRequest;
    use crate::cms::Builder;
    use crate::crypto::PrivateKey;
    use crate::identity::Identity;
    use crate::sign;

    /// A one-page file with nothing else.
    fn blank() -> Document {
        let objs = [
            "<</Type/Catalog/Pages 2 0 R>>",
            "<</Type/Pages/Kids[3 0 R]/Count 1>>",
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]>>",
        ];
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offs = Vec::new();
        for (i, o) in objs.iter().enumerate() {
            offs.push(out.len());
            out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
        }
        let x = out.len();
        out.extend(b"xref\n0 4\n0000000000 65535 f\r\n");
        for o in offs {
            out.extend(format!("{o:010} 00000 n\r\n").bytes());
        }
        out.extend(format!("trailer\n<</Size 4/Root 1 0 R>>\nstartxref\n{x}\n%%EOF\n").bytes());
        Document::open(out).unwrap()
    }

    /// `adbe.pkcs7.sha1`: the signature encapsulates the SHA-1 of the byte range.
    #[test]
    fn pkcs7_sha1() {
        let req = CertificateRequest::signer("Old Signer", 30);
        let id = Identity::self_signed(&req, PrivateKey::generate_p256().unwrap()).unwrap();
        let mut doc = blank();
        let mut v = Dict::new();
        v.set(b"Type", Object::name(b"Sig"));
        v.set(b"Filter", Object::name(b"Adobe.PPKLite"));
        v.set(b"SubFilter", Object::name(b"adbe.pkcs7.sha1"));
        sign::add_placeholders(&mut v, 4000);
        let sig = doc.add(v);
        sign::add_field(&mut doc, "old", sig, 0, [0.0; 4], None, false).unwrap();
        let signed = sign::finish(&mut doc, |data| {
            let content = DigestAlgorithm::Sha1.digest(data);
            let hash = DigestAlgorithm::Sha256.digest(&[&content]);
            Builder {
                key: &id.key,
                cert: &id.certificate,
                chain: &[],
                digest: DigestAlgorithm::Sha256,
                pss: false,
                content_type: oid::DATA,
                content: Some(&content),
                content_hash: &hash,
                signing_time: Some(Time::now()),
                signing_certificate_v2: false,
            }
            .build(&mut |_| Ok(Vec::new()))
        })
        .unwrap();
        let check = |bytes: Vec<u8>| {
            let doc = Document::open(bytes).unwrap();
            verify(&doc, &VerifyOptions::new(&TrustStore::new())).remove(0)
        };
        let r = check(signed.clone());
        assert_eq!(r.status, Status::ValidUntrusted, "{r:#?}");
        let at = signed.windows(9).position(|w| w == b"/MediaBox").unwrap();
        let mut bad = signed;
        bad[at + 1] = b'm';
        assert_eq!(check(bad).status, Status::Invalid);
    }
}
