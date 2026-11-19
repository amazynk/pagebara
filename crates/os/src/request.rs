//! What to open: files with an optional page and search, from command-line arguments
//! (`a.pdf`, `file:///a.pdf`, `a.pdf#page=3`, `file:///a.pdf#page=3&search=word`), sent to
//! the running instance as one JSON line.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::uri;

/// A file to open and where to start in it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenFile {
    /// Absolute.
    pub path: PathBuf,
    /// 0-based.
    pub page: Option<usize>,
    pub search: Option<String>,
}

/// Files to open in the running instance (an empty list just raises its window).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenRequest {
    pub files: Vec<OpenFile>,
}

impl OpenRequest {
    /// The files named by `args`, resolved against `cwd`; `page` (0-based) and `search`
    /// apply to files that name none of their own. Arguments that are not local files
    /// (other URI schemes) are returned apart.
    pub fn from_args<S: AsRef<OsStr>>(
        args: &[S],
        cwd: &Path,
        page: Option<usize>,
        search: Option<&str>,
    ) -> (OpenRequest, Vec<String>) {
        let mut files = Vec::new();
        let mut skipped = Vec::new();
        for a in args {
            match parse_target(a.as_ref(), cwd) {
                Some(mut f) => {
                    f.page = f.page.or(page);
                    f.search = f.search.or_else(|| search.map(str::to_owned));
                    files.push(f);
                }
                None => skipped.push(a.as_ref().to_string_lossy().into_owned()),
            }
        }
        (OpenRequest { files }, skipped)
    }

    pub fn to_line(&self) -> Option<String> {
        let mut s = serde_json::to_string(self).ok()?;
        s.push('\n');
        Some(s)
    }

    pub fn from_line(line: &str) -> Option<OpenRequest> {
        let r: OpenRequest = serde_json::from_str(line.trim()).ok()?;
        r.files.iter().all(|f| f.path.is_absolute()).then_some(r)
    }
}

/// One argument: a path (a file literally named `x#page=2` wins over the fragment), or a
/// `file://` URI; `None` for other URIs.
pub fn parse_target(arg: &OsStr, cwd: &Path) -> Option<OpenFile> {
    let whole = cwd.join(arg);
    let Some(s) = arg.to_str() else {
        return Some(OpenFile {
            path: whole,
            ..OpenFile::default()
        });
    };
    if uri::has_scheme(s) && !whole.exists() {
        let (u, fragment) = s.split_once('#').unwrap_or((s, ""));
        let mut f = fragment_options(fragment);
        f.path = uri::to_path(u)?;
        return Some(f);
    }
    if !whole.exists()
        && let Some((path, fragment)) = s.rsplit_once('#')
    {
        let f = fragment_options(fragment);
        if f.page.is_some() || f.search.is_some() {
            return Some(OpenFile {
                path: cwd.join(path),
                ..f
            });
        }
    }
    Some(OpenFile {
        path: whole,
        ..OpenFile::default()
    })
}

/// PDF open parameters (`page=3&search=word`, also `nameddest`-free forms like `page=3`).
fn fragment_options(fragment: &str) -> OpenFile {
    let mut f = OpenFile::default();
    for part in fragment.split('&') {
        let (k, v) = part.split_once('=').unwrap_or((part, ""));
        match k.to_ascii_lowercase().as_str() {
            "page" => f.page = v.trim().parse::<usize>().ok().map(|n| n.saturating_sub(1)),
            "search" => {
                let v = uri::decode(v.trim_matches('"'))
                    .and_then(|b| String::from_utf8(b).ok())
                    .unwrap_or_default();
                let v = v.trim_matches('"').trim().to_owned();
                f.search = (!v.is_empty()).then_some(v);
            }
            _ => {}
        }
    }
    f
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn one(arg: &str) -> OpenFile {
        parse_target(OsStr::new(arg), Path::new("/work")).unwrap()
    }

    #[test]
    fn paths_uris_and_fragments() {
        assert_eq!(one("a.pdf").path, Path::new("/work/a.pdf"));
        assert_eq!(one("/x/a.pdf").path, Path::new("/x/a.pdf"));
        let f = one("a.pdf#page=3");
        assert_eq!(
            (f.path.as_path(), f.page),
            (Path::new("/work/a.pdf"), Some(2))
        );
        let f = one("file:///x/a%20b.pdf#page=10&zoom=200");
        assert_eq!(f.path, Path::new("/x/a b.pdf"));
        assert_eq!(f.page, Some(9));
        let f = one("file:///x/a.pdf#search=%22net%20rent%22");
        assert_eq!(f.search.as_deref(), Some("net rent"));
        // An unknown fragment is part of the name.
        assert_eq!(one("a#b.pdf").path, Path::new("/work/a#b.pdf"));
        assert!(parse_target(OsStr::new("https://example.com/a.pdf"), Path::new("/")).is_none());
    }

    #[test]
    fn an_existing_file_with_a_hash_is_not_split() {
        let dir = std::env::temp_dir().join(format!("pagebara-os-req-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("x#page=2"), b"").unwrap();
        let f = parse_target(OsStr::new("x#page=2"), &dir).unwrap();
        assert_eq!((f.path, f.page), (dir.join("x#page=2"), None));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn defaults_and_wire_format() {
        let (r, skipped) = OpenRequest::from_args(
            &["a.pdf", "b.pdf#page=2", "ftp://x/y.pdf"],
            Path::new("/w"),
            Some(4),
            Some("rent"),
        );
        assert_eq!(skipped, ["ftp://x/y.pdf"]);
        assert_eq!(r.files[0].page, Some(4));
        assert_eq!(r.files[1].page, Some(1));
        assert_eq!(r.files[1].search.as_deref(), Some("rent"));
        let line = r.to_line().unwrap();
        assert!(line.ends_with('\n') && !line[..line.len() - 1].contains('\n'));
        assert_eq!(OpenRequest::from_line(&line), Some(r));
        assert_eq!(
            OpenRequest::from_line(r#"{"files":[{"path":"rel.pdf"}]}"#),
            None
        );
        assert_eq!(OpenRequest::from_line("garbage"), None);
    }
}
