//! "My signatures": a local, per-user library of signature images and their settings.
//!
//! # On-disk format (version 1)
//!
//! ```text
//! <root>/                     e.g. ~/.local/share/pagebara/signatures
//!   index.json                {"version": 1, "signatures": [<entry>, …]}  (list order)
//!   <id>/meta.json            the same <entry> for this signature
//!   <id>/original.png|jpg     an imported file, byte for byte
//!   <id>/drawing.json         or the strokes of a drawn one: {"strokes": [[{"x","y","pressure"?}…]…], "pen_width"}
//!   <id>/signature.png        the processed RGBA image that gets placed
//! ```
//!
//! An entry: `id`, `name`, `source` (`{"kind": "imported", "file": "original.jpg"}` or
//! `{"kind": "drawn"}`), `settings` (`remove_background`, `threshold` 0–100, `ink`:
//! `"original"` or `{"color": [r, g, b]}`, `despeckle`), `default_width_mm`, `width` and
//! `height` of signature.png, and `created` / `modified` (Unix seconds).
//!
//! Every file is written to a temporary name and renamed into place. The originals are
//! kept, so settings can be changed later without loss. A missing or unreadable index is
//! rebuilt from the folders (`meta.json`, else defaults); folders missing from the index are
//! appended and entries without a folder are dropped.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::background::{self, Ink, Settings};
use crate::draw::{self, Drawing};
use crate::import::{self, Format};
use crate::raster::Rgba;
use crate::{Error, Result};

const INDEX: &str = "index.json";
const META: &str = "meta.json";
const DRAWING: &str = "drawing.json";
const IMAGE: &str = "signature.png";
const VERSION: u32 = 1;

/// A typical handwritten signature width on paper.
pub const DEFAULT_WIDTH_MM: f64 = 50.0;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Source {
    /// From an image file, kept as `file` in the signature's folder.
    Imported {
        file: String,
    },
    Drawn,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Signature {
    pub id: String,
    pub name: String,
    pub source: Source,
    pub settings: Settings,
    /// Width of a newly placed copy.
    pub default_width_mm: f64,
    /// Size of the processed image.
    pub width: u32,
    pub height: u32,
    pub created: u64,
    pub modified: u64,
}

impl Signature {
    /// Height for a placed width, keeping the aspect ratio.
    pub fn height_for(&self, width: f64) -> f64 {
        width * f64::from(self.height.max(1)) / f64::from(self.width.max(1))
    }
}

#[derive(Serialize, Deserialize)]
struct Index {
    version: u32,
    signatures: Vec<Signature>,
}

#[derive(Debug)]
pub struct Library {
    dir: PathBuf,
    items: Vec<Signature>,
}

impl Library {
    /// The per-user location: `signatures` in the platform's local data directory
    /// ([`pagebara_identity::data_dir`]). Mobile apps pass their own directory to
    /// [`Library::open`].
    pub fn default_dir() -> Option<PathBuf> {
        pagebara_identity::data_dir().map(|d| d.join("signatures"))
    }

    pub fn open_default() -> Result<Library> {
        Library::open(Library::default_dir().ok_or(Error::NoDataDir)?)
    }

    /// Opens (creating if needed) the library in `dir`.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Library> {
        let dir = dir.into();
        fs::create_dir_all(&dir)?;
        let index = fs::read(dir.join(INDEX))
            .ok()
            .and_then(|b| serde_json::from_slice::<Index>(&b).ok());
        let rebuilt = index.is_none();
        let mut items = index.map(|i| i.signatures).unwrap_or_default();
        items.retain(|s| valid_id(&s.id) && dir.join(&s.id).join(IMAGE).is_file());
        let mut changed = rebuilt;
        let mut found: Vec<Signature> = Vec::new();
        for e in fs::read_dir(&dir)?.flatten() {
            let id = e.file_name().to_string_lossy().into_owned();
            if !valid_id(&id) || !e.path().is_dir() || items.iter().any(|s| s.id == id) {
                continue;
            }
            if let Some(s) = recover(&e.path(), &id) {
                found.push(s);
            }
        }
        found.sort_by(|a, b| (a.created, &a.id).cmp(&(b.created, &b.id)));
        changed |= !found.is_empty();
        items.extend(found);
        let lib = Library { dir, items };
        if changed {
            lib.save_index()?;
        }
        Ok(lib)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The signatures in display order.
    pub fn signatures(&self) -> &[Signature] {
        &self.items
    }

    pub fn get(&self, id: &str) -> Option<&Signature> {
        self.items.iter().find(|s| s.id == id)
    }

    /// Imports a PNG or JPEG file with default settings (background removed).
    pub fn import(&mut self, bytes: &[u8], name: &str) -> Result<Signature> {
        let format = Format::sniff(bytes).ok_or(Error::UnsupportedImage)?;
        let original = import::decode(bytes)?;
        let settings = Settings::default();
        let image = background::process(&original, &settings);
        let file = format!("original.{}", format.extension());
        self.insert(
            name,
            Source::Imported { file: file.clone() },
            settings,
            &image,
            |dir| write_atomic(&dir.join(&file), bytes),
        )
    }

    /// Adds a drawn signature in `color`.
    pub fn add_drawn(
        &mut self,
        drawing: &Drawing,
        name: &str,
        color: [u8; 3],
    ) -> Result<Signature> {
        let settings = Settings {
            remove_background: false,
            ink: Ink::Color(color),
            ..Settings::default()
        };
        let image =
            draw::render(drawing, color).fit_within(background::STORED_MAX, background::STORED_MAX);
        let json = serde_json::to_vec(drawing)?;
        self.insert(name, Source::Drawn, settings, &image, |dir| {
            write_atomic(&dir.join(DRAWING), &json)
        })
    }

    fn insert(
        &mut self,
        name: &str,
        source: Source,
        settings: Settings,
        image: &Rgba,
        write_source: impl FnOnce(&Path) -> Result<()>,
    ) -> Result<Signature> {
        let id = self.new_id();
        let dir = self.dir.join(&id);
        fs::create_dir_all(&dir)?;
        let now = now();
        let s = Signature {
            id,
            name: name.to_string(),
            source,
            settings,
            default_width_mm: DEFAULT_WIDTH_MM,
            width: image.width,
            height: image.height,
            created: now,
            modified: now,
        };
        let done = write_source(&dir)
            .and_then(|()| write_png(&dir.join(IMAGE), image))
            .and_then(|()| write_meta(&dir, &s));
        if let Err(e) = done {
            let _ = fs::remove_dir_all(&dir);
            return Err(e);
        }
        self.items.push(s.clone());
        self.save_index()?;
        Ok(s)
    }

    pub fn rename(&mut self, id: &str, name: &str) -> Result<()> {
        self.modify(id, |s| s.name = name.to_string())
    }

    pub fn set_default_width(&mut self, id: &str, mm: f64) -> Result<()> {
        if !(mm.is_finite() && mm > 0.0) {
            return Err(Error::Invalid("width must be positive"));
        }
        self.modify(id, |s| s.default_width_mm = mm)
    }

    /// Re-processes the kept original with new settings.
    pub fn set_settings(&mut self, id: &str, settings: Settings) -> Result<()> {
        let s = self.get(id).ok_or(Error::NotFound)?.clone();
        let image = match &s.source {
            Source::Drawn => {
                let color = match settings.ink {
                    Ink::Color(c) => c,
                    Ink::Original => [0, 0, 0],
                };
                draw::render(&self.drawing(id)?, color)
                    .fit_within(background::STORED_MAX, background::STORED_MAX)
            }
            Source::Imported { .. } => background::process(&self.original(id)?, &settings),
        };
        write_png(&self.dir.join(id).join(IMAGE), &image)?;
        self.modify(id, |s| {
            s.settings = settings;
            s.width = image.width;
            s.height = image.height;
        })
    }

    /// Moves a signature to position `to` in the list.
    pub fn move_to(&mut self, id: &str, to: usize) -> Result<()> {
        let from = self.position(id)?;
        let s = self.items.remove(from);
        self.items.insert(to.min(self.items.len()), s);
        self.save_index()
    }

    pub fn delete(&mut self, id: &str) -> Result<()> {
        let i = self.position(id)?;
        self.items.remove(i);
        self.save_index()?;
        fs::remove_dir_all(self.dir.join(id))?;
        Ok(())
    }

    /// The processed image, as placed on pages.
    pub fn image(&self, id: &str) -> Result<Rgba> {
        self.position(id)?;
        import::decode(&fs::read(self.dir.join(id).join(IMAGE))?)
    }

    /// The source image for re-processing and previews: the imported file decoded, or the
    /// drawing rendered in black.
    pub fn original(&self, id: &str) -> Result<Rgba> {
        let s = self.get(id).ok_or(Error::NotFound)?;
        match &s.source {
            Source::Imported { file } => import::decode(&fs::read(self.dir.join(id).join(file))?),
            Source::Drawn => Ok(draw::render(&self.drawing(id)?, [0, 0, 0])),
        }
    }

    /// The strokes of a drawn signature.
    pub fn drawing(&self, id: &str) -> Result<Drawing> {
        self.position(id)?;
        Ok(serde_json::from_slice(&fs::read(
            self.dir.join(id).join(DRAWING),
        )?)?)
    }

    fn position(&self, id: &str) -> Result<usize> {
        self.items
            .iter()
            .position(|s| s.id == id)
            .ok_or(Error::NotFound)
    }

    fn modify(&mut self, id: &str, f: impl FnOnce(&mut Signature)) -> Result<()> {
        let i = self.position(id)?;
        let s = &mut self.items[i];
        f(s);
        s.modified = now();
        write_meta(&self.dir.join(id), s)?;
        self.save_index()
    }

    fn save_index(&self) -> Result<()> {
        let index = Index {
            version: VERSION,
            signatures: self.items.clone(),
        };
        write_atomic(&self.dir.join(INDEX), &serde_json::to_vec_pretty(&index)?)
    }

    fn new_id(&self) -> String {
        loop {
            let mut b = [0u8; 8];
            if getrandom::fill(&mut b).is_err() {
                let n = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos() as u64);
                b = n.to_le_bytes();
            }
            let id: String = b.iter().map(|x| format!("{x:02x}")).collect();
            if !self.dir.join(&id).exists() {
                return id;
            }
        }
    }
}

/// Signature folders are named by 16 hex digits.
fn valid_id(id: &str) -> bool {
    id.len() == 16 && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// An entry for a folder the index does not list.
fn recover(dir: &Path, id: &str) -> Option<Signature> {
    let png = fs::read(dir.join(IMAGE)).ok()?;
    let info = pagebara_png::read_info(&png).ok()?;
    let meta = fs::read(dir.join(META))
        .ok()
        .and_then(|b| serde_json::from_slice::<Signature>(&b).ok());
    if let Some(mut s) = meta {
        s.id = id.to_string();
        (s.width, s.height) = (info.width, info.height);
        return Some(s);
    }
    let created = fs::metadata(dir.join(IMAGE))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    let source = if dir.join(DRAWING).is_file() {
        Source::Drawn
    } else {
        let file = ["original.png", "original.jpg"]
            .into_iter()
            .find(|f| dir.join(f).is_file())?;
        Source::Imported { file: file.into() }
    };
    Some(Signature {
        id: id.to_string(),
        name: "Signature".into(),
        source,
        settings: Settings::default(),
        default_width_mm: DEFAULT_WIDTH_MM,
        width: info.width,
        height: info.height,
        created,
        modified: created,
    })
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn write_meta(dir: &Path, s: &Signature) -> Result<()> {
    write_atomic(&dir.join(META), &serde_json::to_vec_pretty(s)?)
}

fn write_png(path: &Path, img: &Rgba) -> Result<()> {
    let png = pagebara_png::encode(
        img.width,
        img.height,
        pagebara_png::ColorType::Rgba,
        &img.pixels,
    )
    .map_err(|e| Error::Decode(e.to_string()))?;
    write_atomic(path, &png)
}

/// Writes `data` to a temporary file next to `path`, syncs it and renames it over `path`.
fn write_atomic(path: &Path, data: &[u8]) -> Result<()> {
    use std::io::Write as _;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let tmp = path.with_file_name(format!(".{name}.tmp-{}", std::process::id()));
    let mut f = fs::File::create(&tmp)?;
    f.write_all(data)?;
    f.sync_all()?;
    drop(f);
    fs::rename(&tmp, path).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })?;
    Ok(())
}
