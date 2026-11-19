//! Bates numbering: one running number over the pages of several files, in order.

use std::path::{Path, PathBuf};

use pagebara_syntax::Document;
use serde::{Deserialize, Serialize};

use crate::header::{self, DocInfo, HeaderFooter};
use crate::{Error, Result};

/// `prefix` + the number with at least `digits` digits + `suffix`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BatesNumber {
    pub prefix: String,
    pub suffix: String,
    pub digits: usize,
    pub start: u64,
}

impl Default for BatesNumber {
    fn default() -> Self {
        BatesNumber {
            prefix: String::new(),
            suffix: String::new(),
            digits: 6,
            start: 1,
        }
    }
}

impl BatesNumber {
    pub fn format(&self, n: u64) -> String {
        format!(
            "{}{:0width$}{}",
            self.prefix,
            n,
            self.suffix,
            width = self.digits.min(20)
        )
    }
}

/// The numbers one file got.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct BatesFile {
    pub input: PathBuf,
    pub output: PathBuf,
    pub pages: usize,
    pub first: String,
    pub last: String,
}

/// Stores one numbered file: (index, input, PDF) to where it went.
pub type WriteOutput<'a> = dyn Fn(usize, &Path, &[u8]) -> std::io::Result<PathBuf> + 'a;

/// Numbers `files` in order, each continuing where the last stopped; `write(index,
/// input, pdf)` stores each result and says where. The header/footer settings place the
/// number (`{bates}`); its [`HeaderFooter::bates`] gives the format and the first number.
pub fn bates_batch(
    files: &[PathBuf],
    hf: &HeaderFooter,
    password: Option<&str>,
    write: &WriteOutput<'_>,
    progress: &dyn Fn(usize, usize),
) -> Result<Vec<BatesFile>> {
    let mut spec = hf.clone();
    let mut bates = spec.bates.clone().unwrap_or_default();
    let mut out = Vec::new();
    for (i, f) in files.iter().enumerate() {
        let bytes = std::fs::read(f).map_err(|e| Error::Io(format!("{}: {e}", f.display())))?;
        let mut doc =
            open(bytes, password).map_err(|e| Error::Io(format!("{}: {e}", f.display())))?;
        spec.bates = Some(bates.clone());
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let info = DocInfo::read(&doc, &name);
        let (report, last) = header::apply(&mut doc, &spec, &info)?;
        let data = doc
            .save_full()
            .map_err(|e| Error::Io(format!("{}: {e}", f.display())))?;
        let dest = write(i, f, &data).map_err(|e| Error::Io(format!("{}: {e}", f.display())))?;
        let last = last.unwrap_or(bates.start);
        out.push(BatesFile {
            input: f.clone(),
            output: dest,
            pages: report.pages.len(),
            first: bates.format(bates.start),
            last: bates.format(last),
        });
        bates.start = last + 1;
        progress(i + 1, files.len());
    }
    Ok(out)
}

fn open(bytes: Vec<u8>, password: Option<&str>) -> std::result::Result<Document, String> {
    match Document::open(bytes) {
        Ok(d) => Ok(d),
        Err(pagebara_syntax::Error::PasswordRequired(l)) => match password {
            Some(p) => l.unlock(p).map_err(|_| "wrong password".to_owned()),
            None => Err("a password is required".into()),
        },
        Err(e) => Err(e.to_string()),
    }
}

/// The ranges as CSV: file, output, pages, first, last.
pub fn bates_csv(files: &[BatesFile]) -> String {
    let field = |s: &str| {
        if s.contains([',', '"', '\n']) {
            format!("\"{}\"", s.replace('"', "\"\""))
        } else {
            s.to_owned()
        }
    };
    let mut out = String::from("file,output,pages,first,last\n");
    for f in files {
        out.push_str(&format!(
            "{},{},{},{},{}\n",
            field(&f.input.display().to_string()),
            field(&f.output.display().to_string()),
            f.pages,
            field(&f.first),
            field(&f.last)
        ));
    }
    out
}
