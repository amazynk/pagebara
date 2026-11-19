//! Finding font files in the standard font directories and reading their names and style
//! without loading whole files.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::FaceInfo;
use crate::reader::{u16_at, u32_at};
use crate::sfnt;

const MAX_DEPTH: u32 = 8;
const MAX_FILES: usize = 100_000;

/// The standard font directories of the running OS that exist. iOS has no shared font
/// directory: apps bundle fonts and add them with [`super::FontDb::add_dir`].
pub fn system_font_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let mut dirs: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "windows") {
        let windir = std::env::var_os("WINDIR")
            .or_else(|| std::env::var_os("SystemRoot"))
            .map_or_else(|| PathBuf::from(r"C:\Windows"), PathBuf::from);
        dirs.push(windir.join("Fonts"));
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(local).join(r"Microsoft\Windows\Fonts"));
        }
    } else if cfg!(target_os = "macos") {
        dirs.extend(
            [
                "/System/Library/Fonts",
                "/System/Library/Fonts/Supplemental",
                "/Library/Fonts",
                "/Network/Library/Fonts",
            ]
            .map(PathBuf::from),
        );
        if let Some(h) = &home {
            dirs.push(h.join("Library/Fonts"));
        }
    } else if cfg!(target_os = "android") {
        dirs.extend(["/system/fonts", "/product/fonts", "/system/font"].map(PathBuf::from));
    } else if cfg!(target_os = "ios") {
        // Fonts are app-bundled on iOS.
    } else {
        let mut data_dirs: Vec<PathBuf> = std::env::var_os("XDG_DATA_DIRS")
            .map(|v| std::env::split_paths(&v).collect())
            .unwrap_or_default();
        data_dirs.extend(["/usr/local/share", "/usr/share"].map(PathBuf::from));
        for d in data_dirs {
            dirs.push(d.join("fonts"));
        }
        if let Some(d) = std::env::var_os("XDG_DATA_HOME") {
            dirs.push(PathBuf::from(d).join("fonts"));
        }
        if let Some(h) = &home {
            dirs.push(h.join(".local/share/fonts"));
            dirs.push(h.join(".fonts"));
        }
        dirs.push("/usr/X11R6/lib/X11/fonts".into());
    }
    let mut seen = Vec::new();
    dirs.retain(|d| {
        let ok = d.is_dir() && !seen.contains(d);
        seen.push(d.clone());
        ok
    });
    dirs
}

/// All font files below `dir`.
pub fn font_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut visited = Vec::new();
    walk(dir, 0, &mut out, &mut visited);
    out.sort();
    out
}

fn walk(dir: &Path, depth: u32, out: &mut Vec<PathBuf>, visited: &mut Vec<PathBuf>) {
    if depth > MAX_DEPTH || out.len() >= MAX_FILES {
        return;
    }
    // Symlink loops.
    if let Ok(c) = dir.canonicalize() {
        if visited.contains(&c) {
            return;
        }
        visited.push(c);
    }
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let Ok(ft) = std::fs::metadata(&p) else {
            continue;
        };
        if ft.is_dir() {
            walk(&p, depth + 1, out, visited);
        } else if is_font_file(&p) {
            out.push(p);
        }
    }
}

fn is_font_file(p: &Path) -> bool {
    let Some(ext) = p.extension().and_then(|e| e.to_str()) else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "ttf" | "otf" | "ttc" | "otc" | "pfb" | "pfa" | "t1"
    )
}

fn read_at(f: &mut impl Source, off: u64, len: usize) -> Option<Vec<u8>> {
    f.seek(SeekFrom::Start(off)).ok()?;
    let mut b = Vec::new();
    f.take(len as u64).read_to_end(&mut b).ok()?;
    (b.len() == len).then_some(b)
}

/// Font data to read faces from: a file or bytes in memory.
trait Source: Read + Seek {}

impl<T: Read + Seek> Source for T {}

/// The faces in a font file.
pub fn faces(path: &Path) -> Vec<FaceInfo> {
    match File::open(path) {
        Ok(mut f) => faces_in(&mut f, path),
        Err(_) => Vec::new(),
    }
}

/// The faces in font data held in memory, recorded under `path`.
pub fn faces_of(data: &[u8], path: &Path) -> Vec<FaceInfo> {
    faces_in(&mut std::io::Cursor::new(data), path)
}

fn faces_in(f: &mut impl Source, path: &Path) -> Vec<FaceInfo> {
    let Some(head) = read_at(f, 0, 12).or_else(|| {
        let mut b = Vec::new();
        f.seek(SeekFrom::Start(0)).ok()?;
        f.read_to_end(&mut b).ok()?;
        Some(b)
    }) else {
        return Vec::new();
    };
    match head.get(..4) {
        Some(b"ttcf") => {
            let n = u32_at(&head, 8).unwrap_or(0).min(256);
            let Some(offs) = read_at(f, 12, n as usize * 4) else {
                return Vec::new();
            };
            (0..n)
                .filter_map(|i| {
                    let off = u32_at(&offs, i as usize * 4)?;
                    sfnt_face(f, u64::from(off), path, i)
                })
                .collect()
        }
        Some([0, 1, 0, 0] | b"OTTO" | b"true") => sfnt_face(f, 0, path, 0).into_iter().collect(),
        _ => type1_face(f, path).into_iter().collect(),
    }
}

fn sfnt_face(f: &mut impl Source, off: u64, path: &Path, index: u32) -> Option<FaceInfo> {
    let hdr = read_at(f, off, 12)?;
    let n = usize::from(u16_at(&hdr, 4)?);
    let dir = read_at(f, off + 12, n * 16)?;
    let mut table = |tag: &[u8; 4]| -> Option<Vec<u8>> {
        (0..n).find_map(|i| {
            let r = i * 16;
            if dir.get(r..r + 4)? != tag {
                return None;
            }
            let (o, l) = (u32_at(&dir, r + 8)?, u32_at(&dir, r + 12)?);
            read_at(f, u64::from(o), (l as usize).min(1 << 20))
        })
    };
    if bitmap_only(&dir, n, |tag| table(tag)) {
        return None;
    }
    let name = table(b"name")?;
    let os2 = table(b"OS/2").and_then(|t| sfnt::os2(&t));
    let head = table(b"head");
    let post = table(b"post");
    let family = sfnt::name_string(&name, 16).or_else(|| sfnt::name_string(&name, 1))?;
    let subfamily = sfnt::name_string(&name, 17)
        .or_else(|| sfnt::name_string(&name, 2))
        .unwrap_or_default();
    let mac_style = head.as_deref().and_then(|h| u16_at(h, 44)).unwrap_or(0);
    let sub = subfamily.to_ascii_lowercase();
    let weight = os2.map_or_else(
        || {
            if mac_style & 1 != 0 || sub.contains("bold") {
                700
            } else {
                400
            }
        },
        |o| o.weight_class.clamp(1, 1000),
    );
    let italic = os2.is_some_and(|o| o.is_italic())
        || mac_style & 2 != 0
        || sub.contains("italic")
        || sub.contains("oblique");
    let monospace = post
        .as_deref()
        .and_then(|p| u32_at(p, 12))
        .is_some_and(|v| v != 0)
        || os2.is_some_and(|o| o.is_monospaced());
    Some(FaceInfo {
        path: path.to_owned(),
        index,
        postscript_name: sfnt::name_string(&name, 6).unwrap_or_default(),
        full_name: sfnt::name_string(&name, 4).unwrap_or_default(),
        family,
        weight,
        italic,
        monospace,
        serif: os2.and_then(|o| o.is_serif()),
        sfnt: true,
        fs_type: os2.map_or(0, |o| o.fs_type),
        unicode_ranges: os2.map_or([0; 4], |o| o.unicode_ranges),
    })
}

/// Faces whose `glyf` is (almost) empty only draw through embedded bitmaps,
/// which we don't render, so they must never be picked as substitutes.
fn bitmap_only(dir: &[u8], n: usize, mut table: impl FnMut(&[u8; 4]) -> Option<Vec<u8>>) -> bool {
    let len = |tag: &[u8; 4]| {
        (0..n).find_map(|i| {
            let r = i * 16;
            (dir.get(r..r + 4)? == tag).then(|| u32_at(dir, r + 12))?
        })
    };
    if len(b"CFF ").is_some() || len(b"CFF2").is_some() {
        return false;
    }
    let Some(glyf) = len(b"glyf") else {
        return len(b"EBLC").is_some() || len(b"CBLC").is_some();
    };
    let glyphs = table(b"maxp").and_then(|m| u16_at(&m, 4)).unwrap_or(0);
    glyf < 2 * u32::from(glyphs)
}

fn type1_face(f: &mut impl Source, path: &Path) -> Option<FaceInfo> {
    f.seek(SeekFrom::Start(0)).ok()?;
    let mut b = Vec::new();
    f.take(16 * 1024).read_to_end(&mut b).ok()?;
    if !(b.starts_with(b"%!") || b.starts_with(&[0x80, 1])) {
        return None;
    }
    let value = |key: &[u8]| -> Option<String> {
        let p = b.windows(key.len()).position(|w| w == key)? + key.len();
        let rest = b.get(p..)?;
        let rest = &rest[rest.iter().position(|c| !c.is_ascii_whitespace())?..];
        let (s, e) = match rest.first()? {
            b'(' => (1, rest.iter().position(|&c| c == b')')?),
            b'/' => (
                1,
                rest.iter()
                    .skip(1)
                    .position(|c| c.is_ascii_whitespace() || b"/[{(".contains(c))?
                    + 1,
            ),
            _ => (0, rest.iter().position(|c| c.is_ascii_whitespace())?),
        };
        Some(String::from_utf8_lossy(rest.get(s..e)?).into_owned())
    };
    let ps = value(b"/FontName")?;
    let family = value(b"/FamilyName").unwrap_or_else(|| ps.clone());
    let weight_name = value(b"/Weight").unwrap_or_default().to_ascii_lowercase();
    let angle: f32 = value(b"/ItalicAngle")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.0);
    Some(FaceInfo {
        path: path.to_owned(),
        index: 0,
        full_name: value(b"/FullName").unwrap_or_else(|| ps.clone()),
        postscript_name: ps,
        family,
        weight: super::weight_from_name(&weight_name).unwrap_or(400),
        italic: angle != 0.0,
        monospace: value(b"/isFixedPitch").is_some_and(|v| v == "true"),
        serif: None,
        sfnt: false,
        fs_type: 0,
        unicode_ranges: [0; 4],
    })
}

#[cfg(test)]
mod tests {
    use super::bitmap_only;

    fn dir(tables: &[(&[u8; 4], u32)]) -> Vec<u8> {
        let mut d = Vec::new();
        for (tag, len) in tables {
            d.extend_from_slice(*tag);
            d.extend_from_slice(&[0; 8]);
            d.extend_from_slice(&len.to_be_bytes());
        }
        d
    }

    fn maxp(glyphs: u16) -> Option<Vec<u8>> {
        let mut m = vec![0, 0, 0x50, 0];
        m.extend_from_slice(&glyphs.to_be_bytes());
        Some(m)
    }

    #[test]
    fn empty_glyf_with_bitmaps_is_skipped() {
        let d = dir(&[(b"EBLC", 100), (b"glyf", 88)]);
        assert!(bitmap_only(&d, 2, |_| maxp(501)));
    }

    #[test]
    fn outline_fonts_are_kept() {
        let d = dir(&[(b"EBLC", 100), (b"glyf", 54012)]);
        assert!(!bitmap_only(&d, 2, |_| maxp(988)));
        let d = dir(&[(b"CFF ", 10)]);
        assert!(!bitmap_only(&d, 1, |_| None));
    }
}
