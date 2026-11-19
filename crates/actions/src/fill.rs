//! Form data files (FDF, XFDF): the PDF form they belong to, filled into a copy.

use std::path::{Path, PathBuf};

use pagebara_fdf::{import_fdf, import_xfdf};
use pagebara_forms::Form;

use crate::ops;
use crate::out::{parent, stem, write_unique};

/// Whether `path` looks like form data by its extension.
pub fn is_form_data(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("fdf") || e.eq_ignore_ascii_case("xfdf"))
}

/// The file named inside the data: `/F (name)` in FDF, `<f href="name"/>` in XFDF.
fn named_file(data: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(data);
    if let Some(at) = text.find("<f ") {
        let rest = &text[at..];
        let q = rest.find("href=")? + 5;
        let quote = rest[q..].chars().next()?;
        let end = rest[q + 1..].find(quote)?;
        return Some(unescape_xml(&rest[q + 1..q + 1 + end]));
    }
    // `/F (` but not `/FDF` or `/Fields`.
    let rest = text
        .match_indices("/F")
        .find_map(|(at, _)| text[at + 2..].trim_start().strip_prefix('('))?;
    let mut name = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            ')' => return Some(name),
            '\\' => name.push(chars.next()?),
            c => name.push(c),
        }
    }
    None
}

fn unescape_xml(s: &str) -> String {
    s.replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// The PDF form `data` (read from `data_path`) belongs to: the file it names, relative
/// to the data file, else the PDF of the same name next to it.
pub fn form_target(data_path: &Path, data: &[u8]) -> Option<PathBuf> {
    let dir = parent(data_path);
    if let Some(name) = named_file(data).filter(|n| !n.is_empty()) {
        let name = name.strip_prefix("file://").unwrap_or(&name).to_owned();
        let p = dir.join(&name);
        if p.is_file() {
            return Some(p);
        }
        // Only the file name, when the data was made elsewhere.
        if let Some(file) = Path::new(&name.replace('\\', "/")).file_name() {
            let p = dir.join(file);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let same = data_path.with_extension("pdf");
    same.is_file().then_some(same)
}

/// Fills the form `data_path` belongs to into `<form> (filled).pdf` next to it.
pub fn fill_form(data_path: &Path, password: Option<&str>) -> Result<PathBuf, String> {
    let data = std::fs::read(data_path).map_err(|e| format!("cannot read: {e}"))?;
    let target = form_target(data_path, &data)
        .ok_or("the PDF form this data belongs to was not found next to it")?;
    let mut doc = ops::open(&target, password)?;
    let mut form = Form::load(&doc);
    let xml = data.trim_ascii_start().starts_with(b"<");
    let report = if xml {
        import_xfdf(&mut form, &mut doc, &String::from_utf8_lossy(&data))
    } else {
        import_fdf(&mut form, &mut doc, &data)
    }
    .map_err(|e| format!("cannot fill the form: {e}"))?;
    if report.change.fields.is_empty() {
        return Err("the data has no values for this form".into());
    }
    let bytes = ops::save(&doc)?;
    write_unique(
        &parent(&target),
        &format!("{} (filled)", stem(&target)),
        "pdf",
        &bytes,
    )
    .map_err(|e| format!("cannot write: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_files() {
        assert_eq!(
            named_file(b"%FDF-1.2\n1 0 obj << /FDF << /F (form\\).pdf) /Fields [] >> >>")
                .as_deref(),
            Some("form).pdf")
        );
        assert_eq!(
            named_file(b"<xfdf><f href=\"a &amp; b.pdf\"/></xfdf>").as_deref(),
            Some("a & b.pdf")
        );
        assert_eq!(named_file(b"%FDF-1.2 <<>>"), None);
    }
}
