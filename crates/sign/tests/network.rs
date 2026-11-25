//! Timestamps and revocation data fetched over HTTP from responders started here:
//! OpenSSL's timestamp authority (`openssl ts -reply`) and OCSP responder (`openssl ocsp`),
//! and CRLs made by `openssl ca -gencrl`. Every CA and key is made at run time in a
//! temporary folder. Skipped when OpenSSL is not installed (macOS's own `openssl` is
//! LibreSSL, which does not count).

mod common;

use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant};

use common::http;
use common::*;
use pagebara_sign::*;
use pagebara_syntax::Document;

/// The one OpenSSL every command here runs, so a test never mixes two versions.
fn openssl() -> Option<PathBuf> {
    static FOUND: OnceLock<Option<PathBuf>> = OnceLock::new();
    let p = FOUND.get_or_init(common::openssl).clone();
    if p.is_none() {
        eprintln!("openssl not found; skipping");
    }
    p
}

fn command() -> Command {
    openssl_command(&openssl().expect("the test checked for openssl"))
}

/// A fresh folder for one test.
fn folder(name: &str) -> PathBuf {
    let d = scratch(&format!("net-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn run(dir: &Path, args: &[&str]) {
    let out = command().args(args).current_dir(dir).output().unwrap();
    assert!(
        out.status.success(),
        "openssl {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn pem_key(key: &PrivateKey) -> String {
    let der = key.to_pkcs8_der().unwrap();
    pem_block("PRIVATE KEY", &der)
}

fn pem_block(label: &str, der: &[u8]) -> String {
    const B64: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::new();
    for c in der.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= c.len() {
                s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    let lines: Vec<&str> = s
        .as_bytes()
        .chunks(64)
        .map(|l| std::str::from_utf8(l).unwrap())
        .collect();
    format!(
        "-----BEGIN {label}-----\n{}\n-----END {label}-----\n",
        lines.join("\n")
    )
}

/// An OpenSSL timestamp authority: CA and TSA certificates made by `openssl`, requests
/// answered by `openssl ts -reply`. Returns the URL and the CA.
fn openssl_tsa(dir: &Path) -> (String, Certificate) {
    run(
        dir,
        &[
            "req",
            "-x509",
            "-newkey",
            "ec",
            "-pkeyopt",
            "ec_paramgen_curve:P-256",
            "-nodes",
            "-keyout",
            "ca.key",
            "-out",
            "ca.pem",
            "-days",
            "30",
            "-subj",
            "/CN=OpenSSL Test TSA Root",
            "-addext",
            "basicConstraints=critical,CA:TRUE",
            "-addext",
            "keyUsage=critical,keyCertSign,cRLSign",
        ],
    );
    std::fs::write(
        dir.join("tsa.ext"),
        "basicConstraints=CA:FALSE\nkeyUsage=critical,digitalSignature\n\
         extendedKeyUsage=critical,timeStamping\n",
    )
    .unwrap();
    run(
        dir,
        &[
            "req",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-keyout",
            "tsa.key",
            "-out",
            "tsa.csr",
            "-subj",
            "/CN=OpenSSL Test TSA",
        ],
    );
    run(
        dir,
        &[
            "x509",
            "-req",
            "-in",
            "tsa.csr",
            "-CA",
            "ca.pem",
            "-CAkey",
            "ca.key",
            "-CAcreateserial",
            "-out",
            "tsa.pem",
            "-days",
            "30",
            "-extfile",
            "tsa.ext",
        ],
    );
    std::fs::write(
        dir.join("tsa.cnf"),
        "[tsa]\ndefault_tsa = tsa_config\n[tsa_config]\ndir = .\nserial = ./tsaserial\n\
         signer_cert = ./tsa.pem\ncerts = ./ca.pem\nsigner_key = ./tsa.key\n\
         signer_digest = sha256\ndefault_policy = 1.2.3.4.1\n\
         digests = sha256, sha384, sha512\naccuracy = secs:1\nordering = no\n\
         tsa_name = yes\ness_cert_id_chain = no\ness_cert_id_alg = sha256\n",
    )
    .unwrap();
    std::fs::write(dir.join("tsaserial"), "01\n").unwrap();
    let ca = Certificate::from_pem(&std::fs::read(dir.join("ca.pem")).unwrap())
        .unwrap()
        .remove(0);
    let dir = dir.to_path_buf();
    let lock = Arc::new(Mutex::new(()));
    let url = http::serve(Arc::new(move |method, path, body| {
        if method != "POST" || path != "/tsa" {
            return None;
        }
        // One request at a time: they share the serial file.
        let _g = lock.lock().unwrap();
        std::fs::write(dir.join("req.tsq"), body).ok()?;
        let ok = command()
            .args([
                "ts",
                "-reply",
                "-config",
                "tsa.cnf",
                "-queryfile",
                "req.tsq",
                "-out",
                "resp.tsr",
            ])
            .current_dir(&dir)
            .stderr(Stdio::null())
            .status()
            .ok()?
            .success();
        ok.then(|| {
            (
                "application/timestamp-reply".into(),
                std::fs::read(dir.join("resp.tsr")).unwrap(),
            )
        })
    }));
    (format!("{url}/tsa"), ca)
}

type Post = fn(&str, &str, &[u8]) -> std::result::Result<Vec<u8>, String>;

fn http_tsa(url: &str) -> HttpTimestampAuthority<Post> {
    HttpTimestampAuthority {
        url: url.into(),
        post: http::post,
    }
}

#[test]
fn openssl_timestamp_authority_over_http() {
    if openssl().is_none() {
        return;
    }
    let dir = folder("tsa");
    let (url, ca) = openssl_tsa(&dir);
    let tsa = http_tsa(&url);
    let pki = pki();
    let opts = SignOptions {
        timestamp: Some(&tsa),
        ..Default::default()
    };
    let doc = Document::open(sample_pdf()).unwrap();
    let signed = sign(&doc, &pki.rsa_signer, &opts).unwrap();
    let mut t = trust();
    let r = &verify_all(&signed, &t)[0];
    let ts = r.timestamp.as_ref().expect("a signature timestamp");
    assert!(ts.valid, "{ts:#?}");
    assert_eq!(
        ts.authority.as_ref().unwrap().display_name(),
        "OpenSSL Test TSA"
    );
    assert_eq!(r.time_source, TimeSource::Timestamp);
    assert!((ts.time.0 - Time::now().0).abs() < 120);
    // Valid, but the OpenSSL root is not trusted until added.
    assert!(!ts.trust.is_trusted());
    t.add(ca, TrustSource::User);
    let r = &verify_all(&signed, &t)[0];
    assert!(r.timestamp.as_ref().unwrap().trust.is_trusted());
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");

    // A document timestamp from the same server (PAdES B-LTA's last step).
    let doc = Document::open(signed).unwrap();
    let stamped = timestamp_document(&doc, &tsa, DigestAlgorithm::Sha384).unwrap();
    let reports = verify_all(&stamped, &t);
    let dts = reports
        .iter()
        .find(|r| r.kind == SignatureKind::DocumentTimestamp)
        .unwrap();
    assert_eq!(dts.status, Status::ValidTrusted, "{dts:#?}");
    assert!(reports.iter().all(|r| r.status == Status::ValidTrusted));

    // A server answering errors fails the signing cleanly.
    let broken = http_tsa(&url.replace("/tsa", "/nothing"));
    let opts = SignOptions {
        timestamp: Some(&broken),
        ..Default::default()
    };
    let doc = Document::open(sample_pdf()).unwrap();
    assert!(matches!(
        sign(&doc, &pki.rsa_signer, &opts),
        Err(Error::Timestamp(_))
    ));
}

/// A CA made here whose OCSP responder is `openssl ocsp` and whose CRL, made by
/// `openssl ca -gencrl`, is served over HTTP.
struct OpensslCa {
    dir: PathBuf,
    ca: Identity,
    signer: Identity,
    ocsp: Option<Child>,
    crl_url: String,
}

impl Drop for OpensslCa {
    fn drop(&mut self) {
        if let Some(c) = &mut self.ocsp {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// How long the OCSP responder gets to start listening.
const RESPONDER_START: Duration = Duration::from_secs(30);

/// Starts `openssl ocsp` on `port` and waits until it listens. OpenSSL 3 and later print
/// "ACCEPT host:port" on standard output; 1.1.1 says only "waiting for OCSP client
/// connections..." on standard error, so both are read. (Probing with a connection that
/// sends nothing makes some OpenSSL versions spin.) A responder that says neither fails
/// the test with what it did say, instead of leaving it waiting.
fn responder(dir: &Path, port: u16) -> Child {
    let mut child = command()
        .args([
            "ocsp",
            "-index",
            "index.txt",
            "-port",
            &port.to_string(),
            "-rsigner",
            "ca.pem",
            "-rkey",
            "ca.key",
            "-CA",
            "ca.pem",
            "-ndays",
            "1",
        ])
        .current_dir(dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // The readers run until the responder ends, so its pipes never fill.
    let (tx, rx) = mpsc::channel();
    let streams: [(&str, Box<dyn Read + Send>); 2] = [
        ("ocsp-out", Box::new(child.stdout.take().unwrap())),
        ("ocsp-err", Box::new(child.stderr.take().unwrap())),
    ];
    for (name, stream) in streams {
        let tx = tx.clone();
        pagebara_thread::spawn(name, move || {
            for line in BufReader::new(stream).lines().map_while(|l| l.ok()) {
                let _ = tx.send(line);
            }
        });
    }
    drop(tx);
    let deadline = Instant::now() + RESPONDER_START;
    let mut said = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        let Ok(line) = rx.recv_timeout(left) else {
            let _ = child.kill();
            let _ = child.wait();
            panic!("the OCSP responder did not start; it said: {said:#?}");
        };
        let listening = line.starts_with("ACCEPT")
            || line
                .to_ascii_lowercase()
                .contains("waiting for ocsp client connections");
        said.push(line);
        if listening {
            return child;
        }
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// An index line of `openssl ca`'s database for `cert`.
fn index_line(cert: &Certificate, revoked: Option<Time>) -> String {
    let t = |t: Time| {
        let iso = t.to_iso(); // 2026-11-12T10:00:00Z
        let d: String = iso.chars().filter(char::is_ascii_digit).collect();
        format!("{}Z", &d[2..14])
    };
    let (flag, when) = match revoked {
        Some(r) => ("R", t(r)),
        None => ("V", String::new()),
    };
    format!(
        "{flag}\t{}\t{when}\t{}\tunknown\t/CN={}\n",
        t(cert.not_after()),
        cert.serial_hex(),
        cert.display_name()
    )
}

impl OpensslCa {
    /// `ocsp`: run an OCSP responder (else only the CRL is offered); `revoked`: the
    /// signer's revocation time.
    fn new(name: &str, ocsp: bool, revoked: Option<Time>) -> OpensslCa {
        let dir = folder(name);
        let mut req = CertificateRequest::ca("OpenSSL Test CA", 30);
        req.organization = Some("pagebara tests".into());
        let ca = Identity::self_signed(&req, PrivateKey::generate_p256().unwrap()).unwrap();
        std::fs::write(dir.join("ca.pem"), ca.certificate.to_pem()).unwrap();
        std::fs::write(dir.join("ca.key"), pem_key(&ca.key)).unwrap();

        let crl_dir = dir.clone();
        let base = http::serve(Arc::new(move |method, path, _| {
            (method == "GET" && path == "/ca.crl").then(|| {
                (
                    "application/pkix-crl".into(),
                    std::fs::read(crl_dir.join("ca.crl")).unwrap(),
                )
            })
        }));
        let crl_url = format!("{base}/ca.crl");
        let port = free_port();
        let mut sreq = CertificateRequest::signer("Olga Openssl", 30);
        if ocsp {
            sreq.ocsp_url = Some(format!("http://127.0.0.1:{port}"));
        }
        sreq.crl_url = Some(crl_url.clone());
        let signer = ca
            .issue(&sreq, PrivateKey::generate_rsa(2048).unwrap())
            .unwrap();

        std::fs::write(
            dir.join("index.txt"),
            index_line(&signer.certificate, revoked),
        )
        .unwrap();
        std::fs::write(dir.join("crlnumber"), "01\n").unwrap();
        std::fs::write(
            dir.join("ca.cnf"),
            "[ca]\ndefault_ca = CA_default\n[CA_default]\ndir = .\ndatabase = ./index.txt\n\
             crlnumber = ./crlnumber\ncertificate = ./ca.pem\nprivate_key = ./ca.key\n\
             default_md = sha256\ndefault_crl_days = 1\n",
        )
        .unwrap();
        run(
            &dir,
            &[
                "ca",
                "-gencrl",
                "-config",
                "ca.cnf",
                "-out",
                "ca.crl.pem",
                "-batch",
            ],
        );
        run(
            &dir,
            &[
                "crl",
                "-in",
                "ca.crl.pem",
                "-outform",
                "DER",
                "-out",
                "ca.crl",
            ],
        );
        let child = ocsp.then(|| responder(&dir, port));
        OpensslCa {
            dir,
            ca,
            signer,
            ocsp: child,
            crl_url,
        }
    }

    fn trust(&self) -> TrustStore {
        let mut t = TrustStore::new();
        t.add(self.ca.certificate.clone(), TrustSource::User);
        t
    }
}

/// Revocation data from the URLs in the certificates, over HTTP.
struct HttpFetcher;

impl RevocationFetcher for HttpFetcher {
    fn ocsp(&self, cert: &Certificate, issuer: &Certificate) -> Option<Vec<u8>> {
        let req = ocsp_request(cert, issuer);
        cert.ocsp_urls().iter().find_map(|u| {
            http::post(u, "application/ocsp-request", &req)
                .inspect_err(|e| eprintln!("{e}"))
                .ok()
        })
    }

    fn crl(&self, cert: &Certificate) -> Option<Vec<u8>> {
        cert.crl_urls()
            .iter()
            .find_map(|u| http::get(u).inspect_err(|e| eprintln!("{e}")).ok())
    }
}

fn signed_by(id: &Identity) -> Vec<u8> {
    let opts = SignOptions {
        time: Some(Time(Time::now().0 - 60)),
        ..Default::default()
    };
    sign(&Document::open(sample_pdf()).unwrap(), id, &opts).unwrap()
}

#[test]
fn openssl_ocsp_responder() {
    if openssl().is_none() {
        return;
    }
    let ca = OpensslCa::new("ocsp-good", true, None);
    let signed = signed_by(&ca.signer);
    let lt = add_ltv(&Document::open(signed).unwrap(), &ca.trust(), &HttpFetcher).unwrap();
    let r = &verify_all(&lt, &ca.trust())[0];
    assert!(
        matches!(
            r.revocation,
            RevocationStatus::Good {
                source: RevocationSource::Ocsp,
                ..
            }
        ),
        "{r:#?}"
    );
    assert!(r.vri, "the signature has its VRI entry");
    assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
    let _ = &ca.dir;
}

#[test]
fn openssl_ocsp_responder_revoked() {
    if openssl().is_none() {
        return;
    }
    let ca = OpensslCa::new("ocsp-revoked", true, Some(Time(Time::now().0 - 3600)));
    let signed = signed_by(&ca.signer);
    let lt = add_ltv(&Document::open(signed).unwrap(), &ca.trust(), &HttpFetcher).unwrap();
    let r = &verify_all(&lt, &ca.trust())[0];
    assert!(
        matches!(
            r.revocation,
            RevocationStatus::Revoked {
                source: RevocationSource::Ocsp,
                ..
            }
        ),
        "{r:#?}"
    );
    assert_eq!(r.status, Status::Invalid, "{r:#?}");
}

#[test]
fn openssl_crl_over_http() {
    if openssl().is_none() {
        return;
    }
    for revoked in [None, Some(Time(Time::now().0 - 3600))] {
        let ca = OpensslCa::new("crl", false, revoked);
        assert_eq!(
            ca.signer.certificate.crl_urls(),
            std::slice::from_ref(&ca.crl_url)
        );
        let signed = signed_by(&ca.signer);
        let lt = add_ltv(&Document::open(signed).unwrap(), &ca.trust(), &HttpFetcher).unwrap();
        let r = &verify_all(&lt, &ca.trust())[0];
        match revoked {
            None => {
                assert!(
                    matches!(
                        r.revocation,
                        RevocationStatus::Good {
                            source: RevocationSource::Crl,
                            ..
                        }
                    ),
                    "{r:#?}"
                );
                assert_eq!(r.status, Status::ValidTrusted, "{r:#?}");
            }
            Some(_) => {
                assert!(
                    matches!(
                        r.revocation,
                        RevocationStatus::Revoked {
                            source: RevocationSource::Crl,
                            ..
                        }
                    ),
                    "{r:#?}"
                );
                assert_eq!(r.status, Status::Invalid, "{r:#?}");
            }
        }
    }
}
