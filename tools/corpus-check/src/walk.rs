//! Finding the PDF files of a corpus.

use std::path::{Path, PathBuf};

/// A file to check and the corpus it belongs to (the name of the root it was found under).
#[derive(Debug, Clone)]
pub struct Item {
    pub path: PathBuf,
    pub corpus: String,
}

/// Every `*.pdf` file under `roots` (files given directly are taken as they are), sorted.
pub fn pdfs(roots: &[PathBuf]) -> Vec<Item> {
    let mut out = Vec::new();
    for root in roots {
        let corpus = root
            .file_name()
            .map_or_else(|| "?".into(), |n| n.to_string_lossy().into_owned());
        if root.is_file() {
            out.push(Item {
                path: root.clone(),
                corpus: "files".into(),
            });
            continue;
        }
        let mut found = Vec::new();
        collect(root, &mut found);
        found.sort();
        out.extend(found.into_iter().map(|path| Item {
            path,
            corpus: corpus.clone(),
        }));
    }
    out
}

fn collect(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        match e.file_type() {
            Ok(t) if t.is_dir() => collect(&p, out),
            Ok(t)
                if t.is_file() && p.extension().is_some_and(|x| x.eq_ignore_ascii_case("pdf")) =>
            {
                out.push(p)
            }
            _ => {}
        }
    }
}
