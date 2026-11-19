//! Output files: names that are not taken (`name.pdf`, `name (2).pdf`, ...), claimed with
//! `create_new` so two runs never write the same file, and the atomic in-place replace.

use std::fs::OpenOptions;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// Tries `make(n)` for n = 1, 2, ... until `claim` succeeds on a name not taken yet.
fn first_free<T>(
    make: impl Fn(usize) -> PathBuf,
    claim: impl Fn(&Path) -> io::Result<T>,
) -> io::Result<(PathBuf, T)> {
    for n in 1..10_000 {
        let p = make(n);
        match claim(&p) {
            Ok(t) => return Ok((p, t)),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(io::Error::other("no free file name"))
}

fn numbered(dir: &Path, base: &str, ext: &str, n: usize) -> PathBuf {
    let ext = if ext.is_empty() {
        String::new()
    } else {
        format!(".{ext}")
    };
    if n == 1 {
        dir.join(format!("{base}{ext}"))
    } else {
        dir.join(format!("{base} ({n}){ext}"))
    }
}

/// Writes `bytes` to `dir/base.ext`, or the first numbered name free.
pub fn write_unique(dir: &Path, base: &str, ext: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    let (path, mut file) = first_free(
        |n| numbered(dir, base, ext, n),
        |p| OpenOptions::new().write(true).create_new(true).open(p),
    )?;
    if let Err(e) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(&path);
        return Err(e);
    }
    Ok(path)
}

/// Writes to `path` exactly when given (an output the user named), else as
/// [`write_unique`] in `dir`. A given path that is an existing folder takes the default
/// name inside it.
pub fn write_output(
    given: Option<&Path>,
    dir: &Path,
    base: &str,
    bytes: &[u8],
) -> io::Result<PathBuf> {
    write_output_as(given, dir, base, "pdf", bytes)
}

/// [`write_output`] with extension `ext` for default names.
pub fn write_output_as(
    given: Option<&Path>,
    dir: &Path,
    base: &str,
    ext: &str,
    bytes: &[u8],
) -> io::Result<PathBuf> {
    match given {
        Some(p) if p.is_dir() => write_unique(p, base, ext, bytes),
        Some(p) => {
            let mut f = OpenOptions::new().write(true).create_new(true).open(p)?;
            f.write_all(bytes)?;
            Ok(p.to_owned())
        }
        None => write_unique(dir, base, ext, bytes),
    }
}

/// Creates `dir/base` or the first numbered folder free.
pub fn create_dir_unique(dir: &Path, base: &str) -> io::Result<PathBuf> {
    first_free(|n| numbered(dir, base, "", n), |p| std::fs::create_dir(p)).map(|(p, ())| p)
}

/// Replaces `path` with `bytes`: written beside it first, then renamed over it, so the
/// original stays whole if anything fails.
pub fn replace_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = parent(path);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (tmp, mut file) = first_free(
        |n| dir.join(format!(".{name}.{}-{n}.tmp", std::process::id())),
        |p| OpenOptions::new().write(true).create_new(true).open(p),
    )?;
    let r = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        if let Ok(m) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, m.permissions())?;
        }
        drop(file);
        std::fs::rename(&tmp, path)
    })();
    if r.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    r
}

/// The folder of `path` (the current one for a bare name).
pub fn parent(path: &Path) -> PathBuf {
    match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_owned(),
        _ => PathBuf::from("."),
    }
}

/// The file name without its extension.
pub fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "document".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_never_clobber() {
        let dir = std::env::temp_dir().join(format!("pagebara-actions-out-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let a = write_unique(&dir, "x (merged)", "pdf", b"1").unwrap();
        let b = write_unique(&dir, "x (merged)", "pdf", b"2").unwrap();
        assert_eq!(a.file_name().unwrap(), "x (merged).pdf");
        assert_eq!(b.file_name().unwrap(), "x (merged) (2).pdf");
        assert_eq!(std::fs::read(&a).unwrap(), b"1");
        let d1 = create_dir_unique(&dir, "x pages").unwrap();
        let d2 = create_dir_unique(&dir, "x pages").unwrap();
        assert_ne!(d1, d2);
        replace_atomic(&a, b"3").unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), b"3");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 4);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
