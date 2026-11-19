//! Fetching the manifest and the download (ureq, rustls with Mozilla's roots).

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

use crate::{Check, Config, Download, Error, Install, Manifest, hex_decode};

/// Downloads larger than this are refused when the manifest gives no size.
const MAX_DOWNLOAD: u64 = 2 << 30;

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(15)))
        .timeout_per_read(Some(Duration::from_secs(60)))
        .build()
        .into()
}

fn net(e: impl std::fmt::Display) -> Error {
    Error::Network(e.to_string())
}

/// The signed manifest at the configured URL.
pub fn fetch(config: &Config) -> Result<Manifest, Error> {
    let body = agent()
        .get(&config.url)
        .call()
        .map_err(net)?
        .into_body()
        .into_with_config()
        .limit(crate::MAX_MANIFEST)
        .read_to_vec()
        .map_err(net)?;
    crate::verify(&body, &config.key)
}

/// Fetches, verifies and evaluates the manifest.
pub fn check(config: &Config, install: &Install) -> Result<Check, Error> {
    crate::evaluate(&fetch(config)?, config, install)
}

/// Downloads `d` to `dest`, keeping it only when its size and SHA-256 match.
pub fn download(d: &Download, dest: &Path, mut progress: impl FnMut(u64)) -> Result<(), Error> {
    let want = hex_decode(&d.sha256).ok_or(Error::BadHash)?;
    let limit = d.size.unwrap_or(MAX_DOWNLOAD);
    if let Some(dir) = dest.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = dest.with_extension("partial");
    let result = fetch_to(&d.url, &partial, limit, &mut progress).and_then(|(total, hash)| {
        if d.size.is_some_and(|s| s != total) || hash != want.as_slice() {
            return Err(Error::BadHash);
        }
        Ok(std::fs::rename(&partial, dest)?)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&partial);
    }
    result
}

/// Streams `url` into `path`: the size and the SHA-256.
fn fetch_to(
    url: &str,
    path: &Path,
    limit: u64,
    progress: &mut impl FnMut(u64),
) -> Result<(u64, Vec<u8>), Error> {
    let resp = agent().get(url).call().map_err(net)?;
    let mut body = resp
        .into_body()
        .into_with_config()
        .limit(limit + 1)
        .reader();
    let mut file = std::fs::File::create(path)?;
    let mut hash = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut total = 0u64;
    loop {
        let n = body.read(&mut buf).map_err(net)?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > limit {
            return Err(Error::BadHash);
        }
        hash.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        progress(total);
    }
    file.sync_all()?;
    Ok((total, hash.finalize().to_vec()))
}
