//! Update checks end to end against a local HTTP server, with a key made at run time by
//! the `sign-update-manifest` tool: a valid update replaces an AppImage; a bad signature,
//! a bad hash, an older version and no network are refused.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use pagebara_update::install::{Applied, apply};
use pagebara_update::{Check, Config, Error, Install, net};
use sha2::Digest;

type Routes = Arc<Mutex<HashMap<String, Vec<u8>>>>;

/// Serves `routes` over HTTP/1.1 on a free local port.
fn server(routes: Routes) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    pagebara_thread::spawn("test-http", move || {
        for conn in l.incoming() {
            let Ok(mut conn) = conn else { continue };
            let mut r = BufReader::new(conn.try_clone().unwrap());
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            let path = line.split(' ').nth(1).unwrap_or("/").to_owned();
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).unwrap() <= 2 {
                    break;
                }
            }
            let body = routes.lock().unwrap().get(&path).cloned();
            let (status, body) = match body {
                Some(b) => ("200 OK", b),
                None => ("404 Not Found", b"missing".to_vec()),
            };
            let _ = write!(
                conn,
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = conn.write_all(&body);
        }
    });
    format!("http://{addr}")
}

fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pagebara-upd-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn tool(args: &[&Path]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_sign-update-manifest"))
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

struct Fixture {
    dir: PathBuf,
    base: String,
    routes: Routes,
    public: String,
    key: PathBuf,
}

impl Fixture {
    fn new(name: &str) -> Fixture {
        let dir = temp(name);
        let key = dir.join("update.key");
        let public = tool(&[Path::new("--generate-key"), &key]);
        assert_eq!(public.len(), 64);
        let routes = Routes::default();
        let base = server(routes.clone());
        Fixture {
            dir,
            base,
            routes,
            public,
            key,
        }
    }

    /// Publishes `payload` as version `version`, signed with `key`.
    fn publish(&self, version: &str, payload: &[u8], advertised: &[u8], key: &Path) {
        let manifest = format!(
            r#"{{"app_id": "{}", "version": "{version}", "notes": "Better.",
  "downloads": {{"linux-x86_64-appimage": {{"url": "{}/new.AppImage", "sha256": "{}", "size": {}}}}}}}"#,
            pagebara_identity::APP_ID,
            self.base,
            pagebara_update::hex_encode(&sha2::Sha256::digest(advertised)),
            advertised.len()
        );
        let mpath = self.dir.join("manifest.json");
        std::fs::write(&mpath, manifest).unwrap();
        let envelope = tool(&[Path::new("--key"), key, &mpath]);
        let mut r = self.routes.lock().unwrap();
        r.insert("/update.json".into(), envelope.into_bytes());
        r.insert("/new.AppImage".into(), payload.to_vec());
    }

    fn config(&self) -> Config {
        Config::new(&format!("{}/update.json", self.base), &self.public, "0.1.0").unwrap()
    }

    fn appimage(&self) -> Install {
        let path = self.dir.join("app.AppImage");
        std::fs::write(&path, b"old version").unwrap();
        Install::AppImage {
            path,
            arch: "x86_64".into(),
        }
    }
}

#[test]
fn valid_update_replaces_the_appimage() {
    let f = Fixture::new("valid");
    let payload = b"#!/bin/sh\necho new version\n";
    f.publish("0.2.0", payload, payload, &f.key);
    let install = f.appimage();
    let Check::Available(u) = net::check(&f.config(), &install).unwrap() else {
        panic!("0.2.0 is newer than 0.1.0")
    };
    assert_eq!((u.version.as_str(), u.notes.as_str()), ("0.2.0", "Better."));
    let d = u.download.unwrap();
    let staged = install.staging_path(&d, &f.dir.join("cache"));
    let mut seen = 0;
    net::download(&d, &staged, |n| seen = n).unwrap();
    assert_eq!(seen, payload.len() as u64);
    let Applied::Restart(path) = apply(&install, &staged).unwrap() else {
        panic!()
    };
    assert_eq!(std::fs::read(&path).unwrap(), payload);
    std::fs::remove_dir_all(&f.dir).unwrap();
}

#[test]
fn bad_signature_is_refused() {
    let f = Fixture::new("badsig");
    let other = f.dir.join("other.key");
    tool(&[Path::new("--generate-key"), &other]);
    f.publish("0.2.0", b"x", b"x", &other);
    let r = net::check(&f.config(), &f.appimage());
    assert!(matches!(r, Err(Error::BadSignature)), "{r:?}");
    std::fs::remove_dir_all(&f.dir).unwrap();
}

#[test]
fn bad_hash_is_refused_and_nothing_is_kept() {
    let f = Fixture::new("badhash");
    f.publish("0.2.0", b"tampered payload", b"genuine payload!", &f.key);
    let install = f.appimage();
    let Check::Available(u) = net::check(&f.config(), &install).unwrap() else {
        panic!()
    };
    let d = u.download.unwrap();
    let staged = install.staging_path(&d, &f.dir);
    let r = net::download(&d, &staged, |_| {});
    assert!(matches!(r, Err(Error::BadHash)), "{r:?}");
    assert!(!staged.exists() && !staged.with_extension("partial").exists());
    let Install::AppImage { path, .. } = &install else {
        unreachable!()
    };
    assert_eq!(std::fs::read(path).unwrap(), b"old version");
    std::fs::remove_dir_all(&f.dir).unwrap();
}

#[test]
fn downgrade_is_refused() {
    let f = Fixture::new("downgrade");
    f.publish("0.0.9", b"x", b"x", &f.key);
    let r = net::check(&f.config(), &f.appimage());
    assert!(matches!(r, Err(Error::Older { .. })), "{r:?}");
    f.publish("0.1.0", b"x", b"x", &f.key);
    assert_eq!(
        net::check(&f.config(), &f.appimage()).unwrap(),
        Check::UpToDate
    );
    std::fs::remove_dir_all(&f.dir).unwrap();
}

#[test]
fn no_network_is_an_error() {
    let f = Fixture::new("offline");
    // A port nobody listens on.
    let port = TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let c = Config::new(
        &format!("http://127.0.0.1:{port}/u.json"),
        &f.public,
        "0.1.0",
    )
    .unwrap();
    let r = net::check(&c, &Install::Deb);
    assert!(matches!(r, Err(Error::Network(_))), "{r:?}");
    std::fs::remove_dir_all(&f.dir).unwrap();
}
