//! RFC 3161 timestamps: requests, responses, token contents, and timestamp authorities.

use const_oid::ObjectIdentifier as Oid;

use crate::asn1::{self, Tlv};
use crate::cms::{self, Builder};
use crate::crypto::{self, DigestAlgorithm};
use crate::identity::Identity;
use crate::time::Time;
use crate::{Error, Result, oid};

/// Something that answers RFC 3161 timestamp requests.
pub trait TimestampAuthority {
    /// Sends a DER `TimeStampReq` and returns the DER `TimeStampResp`.
    fn timestamp(&self, request: &[u8]) -> Result<Vec<u8>>;
}

/// A timestamp authority reached over HTTP through a caller-provided transport, so the
/// engine carries no HTTP stack: `post(url, content_type, body)` must POST and return the
/// response body.
pub struct HttpTimestampAuthority<F> {
    pub url: String,
    pub post: F,
}

impl<F> std::fmt::Debug for HttpTimestampAuthority<F> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HttpTimestampAuthority({})", self.url)
    }
}

impl<F> TimestampAuthority for HttpTimestampAuthority<F>
where
    F: Fn(&str, &str, &[u8]) -> std::result::Result<Vec<u8>, String>,
{
    fn timestamp(&self, request: &[u8]) -> Result<Vec<u8>> {
        (self.post)(&self.url, "application/timestamp-query", request).map_err(Error::Timestamp)
    }
}

/// A timestamp authority signing with a local identity (offline use and tests).
#[derive(Debug)]
pub struct LocalTimestampAuthority {
    pub identity: Identity,
    /// The time to stamp; now when `None`.
    pub time: Option<Time>,
    pub policy: Oid,
}

impl LocalTimestampAuthority {
    pub fn new(identity: Identity) -> Self {
        LocalTimestampAuthority {
            identity,
            time: None,
            policy: Oid::new_unwrap("1.3.6.1.4.1.99999.1"),
        }
    }
}

impl TimestampAuthority for LocalTimestampAuthority {
    fn timestamp(&self, request: &[u8]) -> Result<Vec<u8>> {
        let bad = Error::Malformed("timestamp request");
        let (req, _) = asn1::parse(request).ok_or(bad.clone())?;
        let mut r = req.reader();
        r.tagged(asn1::INTEGER).ok_or(bad.clone())?;
        let imprint = r.tagged(asn1::SEQUENCE).ok_or(bad.clone())?;
        let _policy = r.tagged(asn1::OID);
        let nonce = r.tagged(asn1::INTEGER);
        let time = self.time.unwrap_or_else(Time::now);
        let serial = crypto::random::<8>()?;
        let tst = asn1::seq(&[
            &asn1::int(1),
            &asn1::oid(&self.policy),
            imprint.raw,
            &asn1::uint(&serial),
            &asn1::generalized_time(time.0),
            nonce.map(|n| n.raw).unwrap_or_default(),
        ]);
        let digest = DigestAlgorithm::Sha256;
        let hash = digest.digest(&[&tst]);
        let token = Builder {
            key: &self.identity.key,
            cert: &self.identity.certificate,
            chain: &self.identity.chain,
            digest,
            pss: false,
            content_type: oid::TST_INFO,
            content: Some(&tst),
            content_hash: &hash,
            signing_time: Some(time),
            signing_certificate_v2: true,
        }
        .build(&mut |_| Ok(Vec::new()))?;
        Ok(asn1::seq(&[&asn1::seq(&[&asn1::int(0)]), &token]))
    }
}

/// A `TimeStampReq` for `hash` (made with `digest`) asking for the TSA certificate.
pub(crate) fn request(digest: DigestAlgorithm, hash: &[u8], nonce: &[u8]) -> Vec<u8> {
    asn1::seq(&[
        &asn1::int(1),
        &asn1::seq(&[&digest.algorithm_id(), &asn1::octets(hash)]),
        &asn1::uint(nonce),
        &asn1::boolean(true),
    ])
}

/// Asks `tsa` to timestamp `data` and returns the token (a CMS ContentInfo).
pub fn timestamp_data(
    tsa: &dyn TimestampAuthority,
    digest: DigestAlgorithm,
    data: &[&[u8]],
) -> Result<Vec<u8>> {
    let hash = digest.digest(data);
    let mut nonce = crypto::random::<8>()?;
    nonce[0] &= 0x7F;
    let resp = tsa.timestamp(&request(digest, &hash, &nonce))?;
    let token = response_token(&resp)?;
    let info = TstInfo::from_token(&token)?;
    if info.imprint != hash || info.digest != Some(digest) {
        return Err(Error::Timestamp(
            "the timestamp is for different data".into(),
        ));
    }
    if info
        .nonce
        .as_deref()
        .is_some_and(|n| n != asn1_strip(&nonce))
    {
        return Err(Error::Timestamp(
            "the timestamp answers another request".into(),
        ));
    }
    Ok(token)
}

fn asn1_strip(b: &[u8]) -> &[u8] {
    let mut v = b;
    while v.len() > 1 && v[0] == 0 {
        v = &v[1..];
    }
    v
}

/// The token of a granted `TimeStampResp`.
fn response_token(resp: &[u8]) -> Result<Vec<u8>> {
    let bad = || Error::Timestamp("not a timestamp response".into());
    let (t, _) = asn1::parse(resp).ok_or_else(bad)?;
    let mut r = t.reader();
    let status = r.tagged(asn1::SEQUENCE).ok_or_else(bad)?;
    let mut s = status.reader();
    let code = s.next().and_then(|c| c.small_int()).ok_or_else(bad)?;
    if code > 1 {
        let text = s
            .tagged(asn1::SEQUENCE)
            .and_then(|t| t.reader().next()?.text())
            .unwrap_or_default();
        return Err(Error::Timestamp(format!("refused (status {code}) {text}")));
    }
    let token = r.tagged(asn1::SEQUENCE).ok_or_else(bad)?;
    Ok(token.raw.to_vec())
}

/// What a timestamp token asserts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TstInfo {
    pub digest: Option<DigestAlgorithm>,
    pub imprint: Vec<u8>,
    pub time: Time,
    pub serial: String,
    pub nonce: Option<Vec<u8>>,
    pub policy: Option<String>,
}

impl TstInfo {
    pub(crate) fn from_token(token: &[u8]) -> Result<TstInfo> {
        let sd = cms::parse(token)?;
        if sd.content_type != oid::TST_INFO {
            return Err(Error::Malformed("timestamp token without TSTInfo"));
        }
        TstInfo::parse(sd.content.as_deref().unwrap_or_default())
    }

    pub(crate) fn parse(der: &[u8]) -> Result<TstInfo> {
        let bad = Error::Malformed("TSTInfo");
        let (t, _) = asn1::parse(der).ok_or(bad.clone())?;
        let mut r = t.reader();
        r.tagged(asn1::INTEGER).ok_or(bad.clone())?;
        let policy = r
            .tagged(asn1::OID)
            .and_then(|o| o.oid())
            .map(|o| o.to_string());
        let imprint = r.tagged(asn1::SEQUENCE).ok_or(bad.clone())?;
        let mut i = imprint.reader();
        let digest = i
            .tagged(asn1::SEQUENCE)
            .and_then(|a| crypto::algorithm(&a))
            .and_then(|(o, _)| DigestAlgorithm::from_oid(&o));
        let imprint = i
            .tagged(asn1::OCTET_STRING)
            .ok_or(bad.clone())?
            .value
            .to_vec();
        let serial = r.tagged(asn1::INTEGER).ok_or(bad.clone())?;
        let time = r
            .tagged(asn1::GENERALIZED_TIME)
            .and_then(|t: Tlv<'_>| t.time())
            .ok_or(bad)?;
        let _accuracy = r.tagged(asn1::SEQUENCE);
        let _ordering = r.tagged(asn1::BOOLEAN);
        let nonce = r.tagged(asn1::INTEGER).map(|n| n.uint().to_vec());
        Ok(TstInfo {
            digest,
            imprint,
            time: Time(time),
            serial: crate::cert::hex(serial.uint()),
            nonce,
            policy,
        })
    }
}
