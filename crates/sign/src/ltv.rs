//! Long-term validation (PAdES B-LT and B-LTA): the Document Security Store with the
//! certificates and revocation data validators need, and document timestamps.

use pagebara_forms::Form;
use pagebara_syntax::{Dict, Document, Object, Stream};

use crate::cert::Certificate;
use crate::crypto::DigestAlgorithm;
use crate::sign::{self, TOO_SMALL};
use crate::trust::TrustStore;
use crate::tsp::{self, TimestampAuthority};
use crate::verify::{self, VerifyOptions};
use crate::{Error, Result};

/// Validation material to store in the document.
#[derive(Clone, Debug, Default)]
pub struct Dss {
    pub certs: Vec<Certificate>,
    /// DER `OCSPResponse`s.
    pub ocsps: Vec<Vec<u8>>,
    /// DER CRLs.
    pub crls: Vec<Vec<u8>>,
    /// What validates each signature (the DSS's `/VRI`); its material is added to the lists
    /// above too.
    pub vri: Vec<Vri>,
}

/// The validation material of one signature (a `/VRI` entry).
#[derive(Clone, Debug, Default)]
pub struct Vri {
    /// The entry's key: [`vri_key`] of the signature's `/Contents`.
    pub key: String,
    pub certs: Vec<Certificate>,
    pub ocsps: Vec<Vec<u8>>,
    pub crls: Vec<Vec<u8>>,
}

/// The `/VRI` key of a signature: the upper-case hex SHA-1 of its `/Contents` bytes (as
/// stored, padding included).
pub fn vri_key(contents: &[u8]) -> String {
    crate::cert::hex(&crate::crypto::sha1(contents))
}

/// Fetches revocation data; the caller does the networking.
pub trait RevocationFetcher {
    /// An `OCSPResponse` for `cert`, e.g. by POSTing [`crate::ocsp_request`] to one of
    /// [`Certificate::ocsp_urls`].
    fn ocsp(&self, cert: &Certificate, issuer: &Certificate) -> Option<Vec<u8>>;
    /// A CRL covering `cert`, e.g. from one of [`Certificate::crl_urls`].
    fn crl(&self, cert: &Certificate) -> Option<Vec<u8>>;
}

/// Adds `dss` to the document's security store (merging with what is there) as an
/// incremental update. Earlier signatures stay valid: LTV data is always an allowed change.
pub fn add_dss(doc: &Document, dss: &Dss) -> Result<Vec<u8>> {
    let mut doc = sign::base_for_update(doc)?;
    let root = sign::catalog_ref(&doc)?;
    let mut cat = sign::dict_of(&doc, root)?;
    let (mut store, store_ref) = match cat.get(b"DSS").cloned() {
        Some(Object::Ref(r)) => (sign::dict_of(&doc, r).unwrap_or_default(), Some(r)),
        Some(Object::Dict(d)) => (d, None),
        _ => (Dict::new(), None),
    };
    store.set(b"Type", Object::name(b"DSS"));
    let der = |c: &[Certificate]| -> Vec<Vec<u8>> { c.iter().map(|c| c.der().to_vec()).collect() };
    let mut all = [der(&dss.certs), dss.ocsps.clone(), dss.crls.clone()];
    for v in &dss.vri {
        for (list, items) in all
            .iter_mut()
            .zip([der(&v.certs), v.ocsps.clone(), v.crls.clone()])
        {
            for i in items {
                if !list.contains(&i) {
                    list.push(i);
                }
            }
        }
    }
    // Each item is stored once; the VRI entries refer to the same streams.
    let mut streams: Vec<(Vec<u8>, Object)> = Vec::new();
    for (key, items) in [&b"Certs"[..], b"OCSPs", b"CRLs"].into_iter().zip(&all) {
        let mut list: Vec<Object> = doc
            .get_in(&store, key)
            .as_array()
            .map(<[Object]>::to_vec)
            .unwrap_or_default();
        for o in &list {
            let s = doc.resolve(o);
            if let Some(d) = s.as_stream().and_then(|s| doc.decode(s).ok()) {
                streams.push((d.data, o.clone()));
            }
        }
        for item in items {
            if streams.iter().any(|(d, _)| d == item) {
                continue;
            }
            let o = Object::Ref(doc.add(flate_stream(item)));
            streams.push((item.clone(), o.clone()));
            list.push(o);
        }
        if !list.is_empty() {
            store.set(key, list);
        }
    }
    if !dss.vri.is_empty() {
        let mut vri = doc
            .get_in(&store, b"VRI")
            .as_dict()
            .cloned()
            .unwrap_or_default();
        let now = Object::string(crate::time::Time::now().to_pdf().as_bytes());
        for v in &dss.vri {
            let mut e = Dict::new();
            for (key, items) in [&b"Cert"[..], b"OCSP", b"CRL"].into_iter().zip([
                der(&v.certs),
                v.ocsps.clone(),
                v.crls.clone(),
            ]) {
                let refs: Vec<Object> = items
                    .iter()
                    .filter_map(|i| streams.iter().find(|(d, _)| d == i).map(|(_, o)| o.clone()))
                    .collect();
                if !refs.is_empty() {
                    e.set(key, refs);
                }
            }
            e.set(b"TU", now.clone());
            vri.set(v.key.as_bytes(), e);
        }
        store.set(b"VRI", vri);
    }
    match store_ref {
        Some(r) => doc.set(r, store),
        None => {
            let r = doc.add(store);
            cat.set(b"DSS", r);
        }
    }
    if cat.get(b"Extensions").is_none() {
        let mut esic = Dict::new();
        esic.set(b"BaseVersion", Object::name(b"1.7"));
        esic.set(b"ExtensionLevel", 5);
        let mut ext = Dict::new();
        ext.set(b"ESIC", esic);
        cat.set(b"Extensions", ext);
    }
    doc.set(root, cat);
    Ok(doc.save_incremental()?)
}

fn flate_stream(data: &[u8]) -> Stream {
    let mut d = Dict::new();
    d.set(b"Filter", Object::name(b"FlateDecode"));
    Stream::new(d, pagebara_filters::encode_flate(data, 6))
}

/// An OCSP response or a CRL fetched for a certificate.
type Answers = (Option<Vec<u8>>, Option<Vec<u8>>);

/// Gathers what every signature's validation needs (certificate chains with their OCSP
/// responses or CRLs, signature timestamps included) and adds it with [`add_dss`], with a
/// `/VRI` entry per signature: PAdES B-LT. Follow with [`timestamp_document`] for B-LTA.
pub fn add_ltv(
    doc: &Document,
    trust: &TrustStore,
    fetcher: &dyn RevocationFetcher,
) -> Result<Vec<u8>> {
    let reports = verify::verify(doc, &VerifyOptions::new(trust));
    let mut dss = Dss::default();
    // Answers per certificate, so chains shared by several signatures are fetched once.
    let mut fetched: Vec<(Certificate, Answers)> = Vec::new();
    for r in &reports {
        let mut chains = vec![r.chain.clone()];
        if let Some(ts) = &r.timestamp {
            chains.push(ts.chain.clone());
        }
        let mut v = Vri {
            key: r.vri_key.clone(),
            ..Vri::default()
        };
        for chain in &chains {
            for (i, cert) in chain.iter().enumerate() {
                if !v.certs.contains(cert) {
                    v.certs.push(cert.clone());
                }
                let Some(issuer) = chain.get(i + 1) else {
                    break;
                };
                let (ocsp, crl) = match fetched.iter().find(|f| &f.0 == cert) {
                    Some(f) => f.1.clone(),
                    None => {
                        let ocsp = fetcher.ocsp(cert, issuer);
                        let crl = if ocsp.is_none() {
                            fetcher.crl(cert)
                        } else {
                            None
                        };
                        fetched.push((cert.clone(), (ocsp.clone(), crl.clone())));
                        (ocsp, crl)
                    }
                };
                v.ocsps.extend(ocsp.filter(|o| !v.ocsps.contains(o)));
                v.crls.extend(crl.filter(|c| !v.crls.contains(c)));
            }
        }
        if !v.key.is_empty() {
            dss.vri.push(v);
        }
    }
    add_dss(doc, &dss)
}

/// Adds a document timestamp (`ETSI.RFC3161`) over the whole file from `tsa`, as an
/// invisible signature field: PAdES B-LTA after [`add_ltv`], or a plain proof of time.
pub fn timestamp_document(
    doc: &Document,
    tsa: &dyn TimestampAuthority,
    digest: DigestAlgorithm,
) -> Result<Vec<u8>> {
    let mut reserve = 10_000;
    loop {
        let mut d = sign::base_for_update(doc)?;
        let form = Form::load(&d);
        let name = sign::unused_name(&form, "Timestamp");
        let mut v = Dict::new();
        v.set(b"Type", Object::name(b"DocTimeStamp"));
        v.set(b"Filter", Object::name(b"Adobe.PPKLite"));
        v.set(b"SubFilter", Object::name(b"ETSI.RFC3161"));
        sign::add_placeholders(&mut v, reserve);
        let sig = d.add(v);
        sign::add_field(&mut d, &name, sig, 0, [0.0; 4], None, false)?;
        let mut made = None;
        let out = sign::finish(&mut d, |data| {
            let token = tsp::timestamp_data(tsa, digest, data)?;
            made = Some(token.len());
            Ok(token)
        });
        match out {
            Err(Error::Unsupported(ref m)) if m == TOO_SMALL => {
                reserve = made.unwrap_or(reserve * 2) + 2048;
            }
            other => return other,
        }
    }
}
