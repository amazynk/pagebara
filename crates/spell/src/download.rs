//! Installing a downloadable dictionary: fetch both files, check their SHA-256 and move
//! them into place together, so a folder never holds a partial dictionary.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::catalog::{DictFile, DictInfo};
use crate::{Dictionary, Error};

/// Downloads a URL. The app supplies one (or enables the `download` feature for
/// [`HttpFetch`](crate::HttpFetch)); tests use none.
pub trait Fetch: Send + Sync {
    /// Writes the body of `url` to `out`; `progress` gets the bytes written so far.
    fn fetch(&self, url: &str, out: &mut dyn Write, progress: &dyn Fn(u64)) -> io::Result<()>;
}

/// Download progress over both files of a dictionary.
#[derive(Clone, Copy, Debug)]
pub struct Progress {
    pub done: u64,
    pub total: u64,
}

/// The folder of an installed dictionary under `root` (the app's dictionary folder).
pub fn folder(root: &Path, info: &DictInfo) -> PathBuf {
    root.join(info.code)
}

fn paths(root: &Path, info: &DictInfo) -> (PathBuf, PathBuf) {
    let dir = folder(root, info);
    (
        dir.join(format!("{}.aff", info.code)),
        dir.join(format!("{}.dic", info.code)),
    )
}

/// Whether `info` is installed under `root` (by size; contents are verified on loading).
pub fn is_installed(root: &Path, info: &DictInfo) -> bool {
    let Some(files) = info.files else {
        return false;
    };
    let (aff, dic) = paths(root, info);
    [(aff, files[0]), (dic, files[1])]
        .iter()
        .all(|(p, f)| fs::metadata(p).is_ok_and(|m| m.is_file() && m.len() == f.size))
}

/// Loads an installed dictionary, checking its files' hashes.
pub fn load_installed(root: &Path, info: &DictInfo) -> Result<Dictionary, Error> {
    let files = info.files.ok_or(Error::Unavailable(info.code.into()))?;
    let (aff, dic) = paths(root, info);
    let read = |p: &Path, f: &DictFile| {
        let data = fs::read(p).map_err(|e| Error::Io(p.display().to_string(), e.to_string()))?;
        if verify(&data, f) {
            Ok(data)
        } else {
            Err(Error::Checksum(p.display().to_string()))
        }
    };
    Dictionary::from_bytes(&read(&aff, &files[0])?, &read(&dic, &files[1])?)
}

/// Downloads `info` into `root` through `fetch` (the user agreed).
pub fn install(
    root: &Path,
    info: &DictInfo,
    fetch: &dyn Fetch,
    progress: &dyn Fn(Progress),
    cancel: &AtomicBool,
) -> Result<(), Error> {
    let files = info.files.ok_or(Error::Unavailable(info.code.into()))?;
    let total = info.download_size();
    let mut data = Vec::new();
    let mut before = 0;
    for f in &files {
        let mut sink = Sink {
            data: Vec::with_capacity(f.size as usize),
            limit: f.size + (1 << 20),
            cancel,
        };
        let report = |n| {
            progress(Progress {
                done: before + n,
                total,
            })
        };
        fetch.fetch(f.url, &mut sink, &report).map_err(|e| {
            if cancel.load(Ordering::Relaxed) {
                Error::Cancelled
            } else {
                Error::Download(f.url.into(), e.to_string())
            }
        })?;
        if !verify(&sink.data, f) {
            return Err(Error::Checksum(f.url.into()));
        }
        before += f.size;
        data.push(sink.data);
    }
    let io = |p: &Path, e: io::Error| Error::Io(p.display().to_string(), e.to_string());
    let dir = folder(root, info);
    let tmp = root.join(format!("{}.part", info.code));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp).map_err(|e| io(&tmp, e))?;
    for (ext, bytes) in ["aff", "dic"].iter().zip(&data) {
        let p = tmp.join(format!("{}.{ext}", info.code));
        fs::write(&p, bytes).map_err(|e| io(&p, e))?;
    }
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|e| io(&dir, e))?;
    }
    fs::rename(&tmp, &dir).map_err(|e| io(&dir, e))
}

struct Sink<'a> {
    data: Vec<u8>,
    limit: u64,
    cancel: &'a AtomicBool,
}

impl Write for Sink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            // Not ErrorKind::Interrupted, which write_all retries forever.
            return Err(io::Error::other("cancelled"));
        }
        if (self.data.len() + buf.len()) as u64 > self.limit {
            return Err(io::Error::other("file too large"));
        }
        self.data.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn verify(data: &[u8], f: &DictFile) -> bool {
    data.len() as u64 == f.size && hex(&Sha256::digest(data)) == f.sha256
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    struct Fake(HashMap<&'static str, Vec<u8>>);

    impl Fetch for Fake {
        fn fetch(&self, url: &str, out: &mut dyn Write, progress: &dyn Fn(u64)) -> io::Result<()> {
            let d = self.0.get(url).ok_or_else(|| io::Error::other("404"))?;
            out.write_all(d)?;
            progress(d.len() as u64);
            Ok(())
        }
    }

    fn file(url: &'static str, data: &[u8]) -> DictFile {
        let sha = hex(&Sha256::digest(data));
        DictFile {
            url,
            sha256: Box::leak(sha.into_boxed_str()),
            size: data.len() as u64,
        }
    }

    #[test]
    fn installs_verified_files() {
        let (aff, dic) = (b"SET UTF-8\n".to_vec(), b"1\nhallo\n".to_vec());
        let info = DictInfo {
            code: "xx_XX",
            name: "Test",
            license: "MIT",
            attribution: "",
            homepage: "",
            files: Some([file("https://x/a.aff", &aff), file("https://x/a.dic", &dic)]),
        };
        let root = std::env::temp_dir().join(format!("pagebara-spell-dl-{}", std::process::id()));
        let cancel = AtomicBool::new(false);
        let seen = Mutex::new(Vec::new());
        let bad = Fake(HashMap::from([
            ("https://x/a.aff", aff.clone()),
            ("https://x/a.dic", b"1\nhullo\n".to_vec()),
        ]));
        assert!(matches!(
            install(&root, &info, &bad, &|_| {}, &cancel),
            Err(Error::Checksum(_))
        ));
        assert!(!is_installed(&root, &info));
        let good = Fake(HashMap::from([
            ("https://x/a.aff", aff),
            ("https://x/a.dic", dic),
        ]));
        install(
            &root,
            &info,
            &good,
            &|p| seen.lock().unwrap().push(p.done),
            &cancel,
        )
        .unwrap();
        assert!(is_installed(&root, &info));
        assert_eq!(
            seen.lock().unwrap().last().copied(),
            Some(info.download_size())
        );
        assert!(load_installed(&root, &info).unwrap().check("hallo"));
        cancel.store(true, Ordering::Relaxed);
        assert!(matches!(
            install(&root, &info, &good, &|_| {}, &cancel),
            Err(Error::Cancelled)
        ));
        let _ = fs::remove_dir_all(&root);
    }
}
