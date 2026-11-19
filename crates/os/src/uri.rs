//! `file://` URIs: percent-encoding a path, and decoding one back.

use std::path::{Path, PathBuf};

/// Bytes kept as they are in a URI path (RFC 3986 unreserved, plus `/`).
fn keep(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~' | b'/')
}

/// `/a b.pdf` → `file:///a%20b.pdf`. On Windows, `C:\a.pdf` → `file:///C:/a.pdf`.
pub fn from_path(path: &Path) -> String {
    let bytes = path_bytes(path);
    let mut out = String::from("file://");
    if !bytes.starts_with(b"/") {
        out.push('/');
    }
    for &b in &bytes {
        if keep(b) || (cfg!(windows) && b == b':') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The local path of a `file://` URI (`file:///p`, `file://localhost/p`); `None` for other
/// schemes, remote hosts or broken escapes. A `#fragment` must be split off first.
pub fn to_path(uri: &str) -> Option<PathBuf> {
    let rest = strip_prefix_ignore_case(uri, "file://")?;
    let path = if rest.starts_with('/') {
        rest
    } else {
        rest.strip_prefix("localhost")
            .filter(|r| r.starts_with('/'))?
    };
    let path = path.split('?').next().unwrap_or(path);
    bytes_path(decode(path)?)
}

/// Whether `s` starts with a URI scheme (`file:`, `https:`...), not a drive letter.
pub fn has_scheme(s: &str) -> bool {
    match s.split_once(':') {
        Some((scheme, _)) => {
            scheme.len() > 1
                && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
                && scheme
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
        }
        None => false,
    }
}

fn strip_prefix_ignore_case<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &s[prefix.len()..])
}

/// Decodes `%XX` escapes.
pub fn decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.bytes();
    while let Some(b) = it.next() {
        if b == b'%' {
            let hex = [it.next()?, it.next()?];
            let hex = std::str::from_utf8(&hex).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
        } else {
            out.push(b);
        }
    }
    Some(out)
}

#[cfg(unix)]
fn path_bytes(p: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    p.as_os_str().as_bytes().to_vec()
}

#[cfg(not(unix))]
fn path_bytes(p: &Path) -> Vec<u8> {
    p.to_string_lossy().replace('\\', "/").into_bytes()
}

#[cfg(unix)]
fn bytes_path(b: Vec<u8>) -> Option<PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(b)))
}

#[cfg(not(unix))]
fn bytes_path(b: Vec<u8>) -> Option<PathBuf> {
    let s = String::from_utf8(b).ok()?;
    // `/C:/a.pdf` → `C:/a.pdf`
    let s = match s.as_bytes() {
        [b'/', d, b':', ..] if d.is_ascii_alphabetic() => &s[1..],
        _ => &s,
    };
    Some(PathBuf::from(s.replace('/', "\\")))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let p = Path::new("/home/a b/ünï#1%.pdf");
        let u = from_path(p);
        assert_eq!(u, "file:///home/a%20b/%C3%BCn%C3%AF%231%25.pdf");
        assert_eq!(to_path(&u).unwrap(), p);
        assert_eq!(
            to_path("FILE://localhost/tmp/x.pdf").unwrap(),
            Path::new("/tmp/x.pdf")
        );
        assert_eq!(to_path("file://server/x.pdf"), None);
        assert_eq!(to_path("https://x/y.pdf"), None);
        assert_eq!(to_path("file:///bad%2"), None);
    }

    #[test]
    fn schemes() {
        assert!(has_scheme("file:///x"));
        assert!(has_scheme("https://x"));
        assert!(!has_scheme("C:\\x.pdf"));
        assert!(!has_scheme("/a:b.pdf"));
        assert!(!has_scheme("x.pdf"));
    }
}
