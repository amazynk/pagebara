//! Model files: where they come from, how they are verified and cached.
//!
//! Weights are never part of the source tree. A [`ModelSource`] names a directory that
//! already holds them (bundled by packaging, or a test cache) or a cache directory that is
//! filled on first use through a [`Fetch`] implementation. Every file is checked against
//! its SHA-256 before it is used.
//!
//! The models are PaddlePaddle's PP-OCRv5 mobile detection and recognition models and the
//! PP-LCNet orientation classifiers in ONNX form, Apache-2.0 licensed; anything that shows or ships them should carry
//! [`ATTRIBUTION`].

use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::Error;

/// Credit and licence of the model weights, for an about box or a packaged NOTICE file.
pub const ATTRIBUTION: &str = "Text recognition uses the PP-OCRv5 and PP-LCNet models by PaddlePaddle \
(https://github.com/PaddlePaddle/PaddleOCR, https://huggingface.co/PaddlePaddle), \
Copyright (c) PaddlePaddle Authors, licensed under the Apache License, Version 2.0 \
(https://www.apache.org/licenses/LICENSE-2.0), run with RTen \
(https://github.com/robertknight/rten, MIT OR Apache-2.0).";

/// One downloadable file, pinned to a revision and hash.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelFile {
    /// File name in a model directory.
    pub name: &'static str,
    pub url: &'static str,
    /// Lower-case hex SHA-256 of the file.
    pub sha256: &'static str,
    pub size: u64,
}

macro_rules! hf {
    ($repo:literal, $rev:literal, $file:literal) => {
        concat!(
            "https://huggingface.co/PaddlePaddle/",
            $repo,
            "/resolve/",
            $rev,
            "/",
            $file
        )
    };
}

/// PP-OCRv5 mobile text detection (DB), shared by every script. Apache-2.0.
pub const DETECTION: ModelFile = ModelFile {
    name: "ppocrv5-mobile-det.onnx",
    url: hf!(
        "PP-OCRv5_mobile_det_onnx",
        "e6f4fa85f00e168c862bc462aebca69eef9b3d3d",
        "inference.onnx"
    ),
    sha256: "a431985659dc921974177a95adcfbb90fd9e51989a5e04d70d0b75f597b6e61d",
    size: 4_826_518,
};

/// PP-LCNet document orientation classifier (0, 90, 180, 270 degrees). Apache-2.0.
pub const PAGE_ORIENTATION: ModelFile = ModelFile {
    name: "pplcnet-doc-ori.onnx",
    url: hf!(
        "PP-LCNet_x1_0_doc_ori_onnx",
        "7330ab7039123e46af2dc03154b9969aa412c61d",
        "inference.onnx"
    ),
    sha256: "af9a0a4f317ff0709ce752067807f819cb15d883f8ecad89f28df1c6ee2d9c92",
    size: 6_788_069,
};

/// PP-LCNet text line orientation classifier (upright or upside down). Apache-2.0.
pub const LINE_ORIENTATION: ModelFile = ModelFile {
    name: "pplcnet-textline-ori.onnx",
    url: hf!(
        "PP-LCNet_x1_0_textline_ori_onnx",
        "7fdcf3cf7061163eda7183b224aa334bd33068f7",
        "inference.onnx"
    ),
    sha256: "38aa97cd4be591e0ad304e659f07ba30d946f27a63315433f6659c69c8778345",
    size: 6_777_816,
};

/// A writing system with its own recognition model (PP-OCRv5 mobile, Apache-2.0).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum Script {
    /// English and the Latin-script European languages.
    #[default]
    Latin,
    /// Simplified and traditional Chinese, Japanese, and English.
    Chinese,
    Korean,
    Arabic,
    Cyrillic,
    Devanagari,
    Greek,
    Thai,
}

macro_rules! rec {
    ($id:literal, $repo:literal, $rev:literal, $onnx:literal, $size:literal, $yml:literal) => {
        [
            ModelFile {
                name: concat!("ppocrv5-", $id, "-rec.onnx"),
                url: hf!($repo, $rev, "inference.onnx"),
                sha256: $onnx,
                size: $size,
            },
            ModelFile {
                name: concat!("ppocrv5-", $id, "-rec.yml"),
                url: hf!($repo, $rev, "inference.yml"),
                sha256: $yml,
                size: 0,
            },
        ]
    };
}

impl Script {
    pub const ALL: [Script; 8] = [
        Script::Latin,
        Script::Chinese,
        Script::Korean,
        Script::Arabic,
        Script::Cyrillic,
        Script::Devanagari,
        Script::Greek,
        Script::Thai,
    ];

    /// The recognition model and its configuration (holding the character list).
    pub fn recognition(self) -> [ModelFile; 2] {
        match self {
            Script::Latin => rec!(
                "latin",
                "latin_PP-OCRv5_mobile_rec_onnx",
                "89d3a50e2c27e2e7cceeab0e944c25c807d5db4f",
                "7888113072263cb471b93f66dd5e2ad70548dc526fa1ace760d0d973dd121498",
                8_042_023,
                "0bbe984570f597af3638e50bdf2e8276f3ab26a61966096538b3b0d1849f5c84"
            ),
            Script::Chinese => rec!(
                "chinese",
                "PP-OCRv5_mobile_rec_onnx",
                "ed152b8b495f84de93cda5709d768548a9127622",
                "da72dc72ca4dc220df0dfde68c1dedc31c58d3e76a25871122e5056227d50092",
                16_534_782,
                "5dfeb2777f6d0db8177d8128a8acfcf6e6276dc4ac73ea3bf0dc06d6a5e85d8e"
            ),
            Script::Korean => rec!(
                "korean",
                "korean_PP-OCRv5_mobile_rec_onnx",
                "5c6f574b8e2230adf4287b33e736d71b9fabd28e",
                "92f0b7785e64fc9090106a241cf4c1eb97472824558272751b88a2a4476d3a08",
                13_418_787,
                "f757fa1c40e99edcf27e9cce879b93eb2a51fa46f5ef39095689b8c37dd75998"
            ),
            Script::Arabic => rec!(
                "arabic",
                "arabic_PP-OCRv5_mobile_rec_onnx",
                "14aaedcd75825982689ecf5cd64ab33ee083215a",
                "799113ebf267fbe742deb99eb36e8d42c9ddc5291ceacf92add41b4d52a59110",
                7_998_947,
                "21368419e6c016c31db55d316d59e11c128e1913e6e6fe10287084710043d3a6"
            ),
            Script::Cyrillic => rec!(
                "cyrillic",
                "cyrillic_PP-OCRv5_mobile_rec_onnx",
                "2cef88145434beb8afa9dd82d77d799eb1ad7b29",
                "5371ee1ddaa7983cc62d0818d99e982b6804638c85e4f960d59a574094e172e5",
                8_048_799,
                "5c76cc91fa98410178a09f498db10050d0ec1634a660053d3005ab7be581f501"
            ),
            Script::Devanagari => rec!(
                "devanagari",
                "devanagari_PP-OCRv5_mobile_rec_onnx",
                "251aec19e36739540d35e2cc943f6aa7503b98e5",
                "cb789212ce96c69d3e74728ae4309d179281d68cb3945d0616b67cafab41c986",
                7_912_311,
                "9bd172dd26440c8ce94d1cde5d5baea6aefdc7cf3c5c8492e0beedef656d4e54"
            ),
            Script::Greek => rec!(
                "greek",
                "el_PP-OCRv5_mobile_rec_onnx",
                "8152b89d2ee0e1d4c92decab75fe75e5e3d836b6",
                "2acf17fcaea2bc81b878e311e6263b8885f48bb03796f75f9f30ed3242bbaa6d",
                7_808_735,
                "17d85b2fe2d2f24cd4ab07bcbc33e0c126859b956ced36e281dc65e2d0c1f0bf"
            ),
            Script::Thai => rec!(
                "thai",
                "th_PP-OCRv5_mobile_rec_onnx",
                "1d4adbbafb1034a2fd6618498575b81ea7b69f69",
                "27618be66018f8598ac0a526a593f9f1cebf794e7eded93428e8fb016e537f5f",
                7_891_015,
                "f6ba7fefc38ca1ff398ddafa75d67d16e0b3757c4e6c833adffee98a981766c9"
            ),
        }
    }

    /// Every file an engine for this script needs: detection, the orientation
    /// classifiers, recognition and its configuration.
    pub fn files(self) -> [ModelFile; 5] {
        let [rec, dict] = self.recognition();
        [DETECTION, PAGE_ORIENTATION, LINE_ORIENTATION, rec, dict]
    }

    /// Bytes to download when nothing is cached yet (the configuration files are small).
    pub fn download_size(self) -> u64 {
        self.files().iter().map(|f| f.size).sum()
    }
}

/// Downloads a URL. The app supplies one (or enables the `download` feature for
/// [`HttpFetch`](crate::HttpFetch)); tests use none.
pub trait Fetch: Send + Sync {
    /// Writes the body of `url` to `out`; `progress` gets the bytes written so far.
    fn fetch(&self, url: &str, out: &mut dyn Write, progress: &dyn Fn(u64)) -> io::Result<()>;
}

mod bundled {
    include!(concat!(env!("OUT_DIR"), "/bundled.rs"));
}

/// The bytes of a model file built into the program (see the build script), if it is.
pub fn bundled(file: &ModelFile) -> Option<&'static [u8]> {
    bundled::FILES
        .iter()
        .find(|(name, _)| *name == file.name)
        .and_then(|(_, data)| *data)
}

/// Whether the detection and Latin recognition models are built into the program.
pub fn has_bundled() -> bool {
    bundled::FILES.iter().all(|(_, d)| d.is_some())
}

/// Where model files come from.
#[derive(Clone)]
pub enum ModelSource {
    /// The files built into the program (detection and Latin recognition, when packaging
    /// bundled them).
    Bundled,
    /// A directory already holding the files (bundled by packaging, or a test cache).
    Dir(PathBuf),
    /// A cache directory (the app's data directory) filled by `fetch` on first use.
    Download { dir: PathBuf, fetch: Arc<dyn Fetch> },
}

impl fmt::Debug for ModelSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelSource::Bundled => f.write_str("Bundled"),
            ModelSource::Dir(d) => f.debug_tuple("Dir").field(d).finish(),
            ModelSource::Download { dir, .. } => {
                f.debug_struct("Download").field("dir", dir).finish()
            }
        }
    }
}

impl ModelSource {
    /// The directory of the files, for sources on disk.
    pub fn dir(&self) -> Option<&Path> {
        match self {
            ModelSource::Bundled => None,
            ModelSource::Dir(d) | ModelSource::Download { dir: d, .. } => Some(d),
        }
    }

    /// Whether `file` is present (by size; contents are verified when loaded).
    pub fn has(&self, file: &ModelFile) -> bool {
        let Some(dir) = self.dir() else {
            return bundled(file).is_some();
        };
        let p = dir.join(file.name);
        fs::metadata(&p).is_ok_and(|m| m.is_file() && (file.size == 0 || m.len() == file.size))
    }
}

/// Download progress: bytes so far of the file being fetched, and its name.
#[derive(Clone, Copy, Debug)]
pub struct DownloadProgress<'a> {
    pub file: &'a str,
    pub done: u64,
    pub total: u64,
}

/// Reads a verified file from the first source holding it, downloading it into the first
/// [`ModelSource::Download`] cache when none does.
pub fn read_file(
    sources: &[ModelSource],
    file: &ModelFile,
    progress: &dyn Fn(DownloadProgress<'_>),
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    for s in sources {
        let Some(dir) = s.dir() else {
            if let Some(data) = bundled(file).filter(|d| verify(d, file)) {
                return Ok(data.to_vec());
            }
            continue;
        };
        let p = dir.join(file.name);
        if let Ok(data) = fs::read(&p) {
            if verify(&data, file) {
                return Ok(data);
            }
            if matches!(s, ModelSource::Download { .. }) {
                let _ = fs::remove_file(&p);
            }
        }
    }
    for s in sources {
        if let ModelSource::Download { dir, fetch } = s {
            return download(dir, fetch.as_ref(), file, progress, cancel);
        }
    }
    Err(Error::ModelUnavailable(file.name))
}

fn download(
    dir: &Path,
    fetch: &dyn Fetch,
    file: &ModelFile,
    progress: &dyn Fn(DownloadProgress<'_>),
    cancel: &AtomicBool,
) -> Result<Vec<u8>, Error> {
    let mut sink = Sink {
        data: Vec::with_capacity(file.size as usize),
        cancel,
    };
    let report = |done| {
        progress(DownloadProgress {
            file: file.name,
            done,
            total: file.size,
        })
    };
    fetch.fetch(file.url, &mut sink, &report).map_err(|e| {
        if cancel.load(Ordering::Relaxed) {
            Error::Cancelled
        } else {
            Error::Download(file.name, e.to_string())
        }
    })?;
    let data = sink.data;
    if !verify(&data, file) {
        return Err(Error::Download(file.name, "checksum mismatch".into()));
    }
    // Written under a temporary name and renamed, so a cache never holds a partial file.
    let io = |e: io::Error| Error::Io(file.name, e.to_string());
    fs::create_dir_all(dir).map_err(io)?;
    let tmp = dir.join(format!("{}.part", file.name));
    fs::write(&tmp, &data).map_err(io)?;
    fs::rename(&tmp, dir.join(file.name)).map_err(io)?;
    Ok(data)
}

struct Sink<'a> {
    data: Vec<u8>,
    cancel: &'a AtomicBool,
}

impl Write for Sink<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
        }
        // Bounded: the files are a few megabytes.
        if self.data.len() + buf.len() > 256 << 20 {
            return Err(io::Error::other("file too large"));
        }
        self.data.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn verify(data: &[u8], file: &ModelFile) -> bool {
    hex(&Sha256::digest(data)) == file.sha256
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_are_hex() {
        for s in Script::ALL {
            for f in s.files() {
                assert_eq!(f.sha256.len(), 64, "{}", f.name);
                assert!(f.sha256.bytes().all(|b| b.is_ascii_hexdigit()));
                assert!(f.url.starts_with("https://huggingface.co/PaddlePaddle/"));
            }
        }
        assert_eq!(hex(&Sha256::digest(b"abc"))[..8], *"ba7816bf");
    }

    struct Fake(Vec<u8>);

    impl Fetch for Fake {
        fn fetch(&self, _: &str, out: &mut dyn Write, progress: &dyn Fn(u64)) -> io::Result<()> {
            out.write_all(&self.0)?;
            progress(self.0.len() as u64);
            Ok(())
        }
    }

    #[test]
    fn download_verifies_and_caches() {
        let dir = std::env::temp_dir().join(format!("pagebara-ocr-test-{}", std::process::id()));
        let file = ModelFile {
            name: "abc.bin",
            url: "https://example.invalid/abc",
            sha256: "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
            size: 3,
        };
        let cancel = AtomicBool::new(false);
        let bad = [ModelSource::Download {
            dir: dir.clone(),
            fetch: Arc::new(Fake(b"abd".to_vec())),
        }];
        assert!(matches!(
            read_file(&bad, &file, &|_| {}, &cancel),
            Err(Error::Download(..))
        ));
        let good = [ModelSource::Download {
            dir: dir.clone(),
            fetch: Arc::new(Fake(b"abc".to_vec())),
        }];
        assert_eq!(read_file(&good, &file, &|_| {}, &cancel).unwrap(), b"abc");
        assert!(ModelSource::Dir(dir.clone()).has(&file));
        let cached = [ModelSource::Dir(dir.clone())];
        assert_eq!(read_file(&cached, &file, &|_| {}, &cancel).unwrap(), b"abc");
        let _ = fs::remove_dir_all(&dir);
        assert!(matches!(
            read_file(&cached, &file, &|_| {}, &cancel),
            Err(Error::ModelUnavailable(_))
        ));
    }
}
