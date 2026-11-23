//! Recently opened files, kept in the per-user data directory as one path per line.

use std::path::{Path, PathBuf};

const MAX: usize = 12;
const FILE: &str = "recent.txt";

#[derive(Debug, Default)]
pub struct RecentFiles {
    /// Where the list is stored; `None` keeps it in memory only.
    store: Option<PathBuf>,
    paths: Vec<PathBuf>,
}

impl RecentFiles {
    /// The list in the platform's per-user data directory.
    pub fn load_default() -> Self {
        let dir = pagebara_identity::data_dir();
        match dir {
            Some(dir) => Self::load(dir.join(FILE)),
            None => Self::default(),
        }
    }

    pub fn load(store: PathBuf) -> Self {
        let paths = std::fs::read_to_string(&store)
            .unwrap_or_default()
            .lines()
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .take(MAX)
            .collect();
        RecentFiles {
            store: Some(store),
            paths,
        }
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Moves `path` to the front and saves.
    pub fn add(&mut self, path: &Path) {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_owned());
        self.paths.retain(|p| *p != path);
        self.paths.insert(0, path);
        self.paths.truncate(MAX);
        self.save();
    }

    pub fn remove(&mut self, path: &Path) {
        self.paths.retain(|p| p != path);
        self.save();
    }

    fn save(&self) {
        let Some(store) = &self.store else { return };
        // Paths that are not valid Unicode cannot round-trip through the text file.
        let text: String = self
            .paths
            .iter()
            .filter_map(|p| p.to_str())
            .filter(|p| !p.contains('\n'))
            .map(|p| format!("{p}\n"))
            .collect();
        let write = || -> std::io::Result<()> {
            if let Some(dir) = store.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = store.with_extension("tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(tmp, store)
        };
        if let Err(e) = write() {
            log::warn!("could not save recent files to {}: {e}", store.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_dedup_and_cap() {
        let dir = std::env::temp_dir().join(format!("pagebara-recent-{}", std::process::id()));
        let store = dir.join("recent.txt");
        let mut r = RecentFiles::load(store.clone());
        assert!(r.paths().is_empty());
        for i in 0..20 {
            r.add(Path::new(&format!("/docs/{i}.pdf")));
        }
        r.add(Path::new("/docs/15.pdf"));
        let r = RecentFiles::load(store);
        assert_eq!(r.paths().len(), MAX);
        // Made absolute: on Windows "/docs" is on the current drive.
        let abs = |p: &str| std::path::absolute(p).unwrap();
        assert_eq!(r.paths()[0], abs("/docs/15.pdf"));
        assert_eq!(r.paths()[1], abs("/docs/19.pdf"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
