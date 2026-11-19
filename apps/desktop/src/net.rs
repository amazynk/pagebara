//! HTTP for the few features that need a server, each only when the user asks: timestamp
//! authorities, certificate revocation data (OCSP, CRLs) and the text recognition models.
//! ureq with rustls and Mozilla's roots, like `pagebara_ocr::HttpFetch`; without the `network`
//! feature every request fails with an explanation.

use pagebara_sign::{Certificate, RevocationFetcher};

/// Largest response accepted (CRLs of big CAs run to a few MB).
#[cfg(feature = "network")]
const LIMIT: u64 = 32 << 20;

#[cfg(feature = "network")]
fn read(resp: Result<ureq::http::Response<ureq::Body>, ureq::Error>) -> Result<Vec<u8>, String> {
    resp.map_err(|e| e.to_string())?
        .into_body()
        .into_with_config()
        .limit(LIMIT)
        .read_to_vec()
        .map_err(|e| e.to_string())
}

/// POSTs `body` and returns the response body.
#[cfg(feature = "network")]
pub fn post(url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    read(
        ureq::post(url)
            .header("Content-Type", content_type)
            .send(body),
    )
}

#[cfg(feature = "network")]
pub fn get(url: &str) -> Result<Vec<u8>, String> {
    read(ureq::get(url).call())
}

#[cfg(not(feature = "network"))]
pub fn post(_url: &str, _content_type: &str, _body: &[u8]) -> Result<Vec<u8>, String> {
    Err(pagebara_i18n::tr!(
        "this build of Pagebara has no network support"
    ))
}

#[cfg(not(feature = "network"))]
pub fn get(_url: &str) -> Result<Vec<u8>, String> {
    Err(pagebara_i18n::tr!(
        "this build of Pagebara has no network support"
    ))
}

/// Revocation data from the URLs in the certificates.
#[derive(Debug, Default)]
pub struct HttpRevocation;

impl RevocationFetcher for HttpRevocation {
    fn ocsp(&self, cert: &Certificate, issuer: &Certificate) -> Option<Vec<u8>> {
        let req = pagebara_sign::ocsp_request(cert, issuer);
        cert.ocsp_urls().iter().find_map(|url| {
            post(url, "application/ocsp-request", &req)
                .inspect_err(|e| log::warn!("OCSP {url}: {e}"))
                .ok()
        })
    }

    fn crl(&self, cert: &Certificate) -> Option<Vec<u8>> {
        cert.crl_urls()
            .iter()
            .find_map(|url| get(url).inspect_err(|e| log::warn!("CRL {url}: {e}")).ok())
    }
}

/// Where the OCR models come from: `$PAGEBARA_OCR_MODELS` when set, then the cache in the app's
/// data folder, filled from the network when allowed.
pub fn model_sources(cache: Option<std::path::PathBuf>) -> Vec<pagebara_ocr::ModelSource> {
    let mut out = vec![pagebara_ocr::ModelSource::Bundled];
    if let Some(d) = std::env::var_os("PAGEBARA_OCR_MODELS") {
        out.push(pagebara_ocr::ModelSource::Dir(d.into()));
    }
    if let Some(dir) = cache {
        #[cfg(feature = "network")]
        out.push(pagebara_ocr::ModelSource::Download {
            dir,
            fetch: std::sync::Arc::new(pagebara_ocr::HttpFetch),
        });
        #[cfg(not(feature = "network"))]
        out.push(pagebara_ocr::ModelSource::Dir(dir));
    }
    out
}

/// The real transport against responders started here: a timestamp authority, an OCSP
/// responder and a CRL server built with the engine's own ASN.1, behind a tiny HTTP server.
#[cfg(all(test, feature = "network"))]
mod tests {
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::Arc;

    use pagebara_sign::*;
    use pagebara_syntax::Document;

    use super::*;

    struct Pki {
        ca: Identity,
        signer: Identity,
        tsa: Identity,
    }

    /// Serves `/tsa`, `/ocsp` and `/ca.crl` for a CA whose signer points there; returns the
    /// base URL and the PKI.
    fn responders(revoked: Option<Time>) -> (String, Arc<Pki>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let ca = Identity::self_signed(
            &CertificateRequest::ca("Net Test CA", 30),
            PrivateKey::generate_p256().unwrap(),
        )
        .unwrap();
        let mut req = CertificateRequest::signer("Nina Net", 30);
        req.ocsp_url = Some(format!("{base}/ocsp"));
        req.crl_url = Some(format!("{base}/ca.crl"));
        let signer = ca
            .issue(&req, PrivateKey::generate_p256().unwrap())
            .unwrap();
        let tsa = ca
            .issue(
                &CertificateRequest::timestamping("Net Test TSA", 30),
                PrivateKey::generate_p256().unwrap(),
            )
            .unwrap();
        let pki = Arc::new(Pki { ca, signer, tsa });
        let p = pki.clone();
        pagebara_thread::spawn("test-responders", move || {
            for stream in listener.incoming().flatten() {
                let p = p.clone();
                pagebara_thread::spawn("test-answer", move || {
                    let mut r = BufReader::new(stream.try_clone().unwrap());
                    let mut line = String::new();
                    r.read_line(&mut line).unwrap();
                    let path = line
                        .split_whitespace()
                        .nth(1)
                        .unwrap_or_default()
                        .to_owned();
                    let mut len = 0;
                    loop {
                        let mut h = String::new();
                        r.read_line(&mut h).unwrap();
                        if h.trim().is_empty() {
                            break;
                        }
                        if let Some(v) = h.to_ascii_lowercase().strip_prefix("content-length:") {
                            len = v.trim().parse().unwrap();
                        }
                    }
                    let mut body = vec![0; len];
                    r.read_exact(&mut body).unwrap();
                    let now = Time::now();
                    let answer = match path.as_str() {
                        "/tsa" => LocalTimestampAuthority::new(p.tsa.clone())
                            .timestamp(&body)
                            .ok(),
                        "/ocsp" => issue_ocsp_response(
                            &p.ca,
                            &p.signer.certificate,
                            &p.ca.certificate,
                            revoked,
                            now,
                        )
                        .ok(),
                        "/ca.crl" => {
                            let list: Vec<(&Certificate, Time)> = revoked
                                .map(|t| (&p.signer.certificate, t))
                                .into_iter()
                                .collect();
                            issue_crl(&p.ca, &list, now, Time(now.0 + 86400)).ok()
                        }
                        _ => None,
                    };
                    let mut w = stream;
                    match answer {
                        Some(b) => {
                            write!(
                                w,
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                b.len()
                            )
                            .unwrap();
                            w.write_all(&b).unwrap();
                        }
                        None => write!(
                            w,
                            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                        .unwrap(),
                    }
                });
            }
        });
        (base, pki)
    }

    fn blank() -> Document {
        Document::open(pagebara_samples::form()).unwrap()
    }

    #[test]
    fn timestamp_and_revocation_over_http() {
        let (base, pki) = responders(None);
        let tsa = HttpTimestampAuthority {
            url: format!("{base}/tsa"),
            post,
        };
        let opts = SignOptions {
            timestamp: Some(&tsa),
            ..Default::default()
        };
        let signed = sign(&blank(), &pki.signer, &opts).unwrap();
        let mut trust = TrustStore::new();
        trust.add(pki.ca.certificate.clone(), TrustSource::User);

        // OCSP and CRL each through the transport.
        let (cert, issuer) = (&pki.signer.certificate, &pki.ca.certificate);
        assert!(HttpRevocation.ocsp(cert, issuer).is_some());
        assert!(HttpRevocation.crl(cert).is_some());

        let lt = add_ltv(&Document::open(signed).unwrap(), &trust, &HttpRevocation).unwrap();
        let lta = timestamp_document(&Document::open(lt).unwrap(), &tsa, DigestAlgorithm::Sha256)
            .unwrap();
        let reports = verify(&Document::open(lta).unwrap(), &VerifyOptions::new(&trust));
        assert_eq!(reports.len(), 2);
        let r = reports
            .iter()
            .find(|r| r.kind == SignatureKind::Approval)
            .unwrap();
        assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
        assert!(r.timestamp.as_ref().is_some_and(|t| t.valid));
        assert!(r.vri);
        assert!(matches!(
            r.revocation,
            RevocationStatus::Good {
                source: RevocationSource::Ocsp,
                ..
            }
        ));
        assert!(reports.iter().all(|r| r.status == Status::ValidTrusted));
    }

    #[test]
    fn revoked_and_errors() {
        let (base, pki) = responders(Some(Time(Time::now().0 - 3600)));
        let opts = SignOptions {
            time: Some(Time(Time::now().0 - 60)),
            ..Default::default()
        };
        let signed = sign(&blank(), &pki.signer, &opts).unwrap();
        let mut trust = TrustStore::new();
        trust.add(pki.ca.certificate.clone(), TrustSource::User);
        let lt = add_ltv(&Document::open(signed).unwrap(), &trust, &HttpRevocation).unwrap();
        let r = &verify(&Document::open(lt).unwrap(), &VerifyOptions::new(&trust))[0];
        assert!(matches!(r.revocation, RevocationStatus::Revoked { .. }));
        assert_eq!(r.status, Status::Invalid);

        // HTTP errors and unreachable servers come back as errors, not panics.
        assert!(post(&format!("{base}/nothing"), "text/plain", b"x").is_err());
        assert!(get("http://127.0.0.1:1/").is_err());
        let tsa = HttpTimestampAuthority {
            url: format!("{base}/nothing"),
            post,
        };
        let opts = SignOptions {
            timestamp: Some(&tsa),
            ..Default::default()
        };
        assert!(matches!(
            sign(&blank(), &pki.signer, &opts),
            Err(pagebara_sign::Error::Timestamp(_))
        ));
    }
}
