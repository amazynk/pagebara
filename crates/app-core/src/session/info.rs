//! Document properties for the properties dialog.

use pagebara_i18n::{tr, tr_noop};

use super::{Session, denied, page_size_label};
use crate::text;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DocInfo {
    /// Label and value, in display order; empty values are left out.
    pub rows: Vec<(String, String)>,
}

pub(super) fn read(s: &Session) -> DocInfo {
    let doc = s.doc();
    let info = doc.get_in(doc.trailer(), b"Info");
    let field = |key: &[u8]| {
        info.as_dict()
            .map(|d| doc.get_in(d, key))
            .and_then(|v| v.as_string().map(text::decode))
            .unwrap_or_default()
    };
    let date = |key: &[u8]| {
        let raw = field(key);
        format_pdf_date(&raw).unwrap_or(raw)
    };
    let (major, minor) = doc.version();
    let size = s
        .geoms
        .first()
        .map(|g| {
            let (w, h) = g.size();
            page_size_label(w, h)
        })
        .unwrap_or_default();
    let p = doc.permissions();
    let encrypted = s.encrypted();
    let security = if !encrypted {
        // TRANSLATORS: the document's security: no protection
        tr!("None")
    } else if doc.is_owner() {
        tr!("Password protected (opened with full rights)")
    } else {
        tr!("Password protected")
    };
    let allowed = if !encrypted || doc.is_owner() {
        // TRANSLATORS: what a document allows: all actions
        tr!("Everything")
    } else {
        match denied(p).as_slice() {
            [] => tr!("Everything"),
            d => tr!("Everything except {}", d.join(", ")),
        }
    };
    let file = s
        .path
        .as_ref()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    let bytes = doc.source().len();
    let rows = [
        (tr_noop!("Title"), field(b"Title")),
        (tr_noop!("Author"), field(b"Author")),
        (tr_noop!("Subject"), field(b"Subject")),
        (tr_noop!("Keywords"), field(b"Keywords")),
        // TRANSLATORS: the application that made the document
        (tr_noop!("Created by"), field(b"Creator")),
        // TRANSLATORS: the software that wrote the PDF file
        (tr_noop!("Producer"), field(b"Producer")),
        (tr_noop!("Created"), date(b"CreationDate")),
        (tr_noop!("Modified"), date(b"ModDate")),
        // TRANSLATORS: a document property: the file's location
        (tr_noop!("File"), file),
        (tr_noop!("File size"), human_size(bytes)),
        (tr_noop!("PDF version"), format!("{major}.{minor}")),
        (tr_noop!("Pages"), s.page_count().to_string()),
        (tr_noop!("Page size"), size),
        (tr_noop!("Security"), security),
        // TRANSLATORS: a document property: what the document's protection allows
        (tr_noop!("Allowed"), allowed),
        (
            // TRANSLATORS: a document property: whether a damaged file was repaired
            tr_noop!("Repaired"),
            if doc.was_repaired() {
                tr!("Yes, the file was damaged")
            } else {
                String::new()
            },
        ),
    ];
    DocInfo {
        rows: rows
            .into_iter()
            .filter(|(_, v)| !v.is_empty())
            .map(|(k, v)| (pagebara_i18n::translate(k, &[]), v))
            .collect(),
    }
}

fn human_size(n: usize) -> String {
    match n {
        0 => String::new(),
        n => pagebara_i18n::bytes(n as u64),
    }
}

/// "D:20261111093000+01'00'" as "2026-11-11 09:30".
pub fn format_pdf_date(s: &str) -> Option<String> {
    let d = s.strip_prefix("D:").unwrap_or(s);
    let digits: String = d.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 8 {
        return None;
    }
    let part = |a: usize, b: usize| digits.get(a..b).and_then(|p| p.parse().ok()).unwrap_or(0);
    Some(pagebara_i18n::date_time(
        part(0, 4),
        part(4, 6),
        part(6, 8),
        part(8, 10),
        part(10, 12),
    ))
}

#[cfg(test)]
mod tests {
    #[test]
    fn dates() {
        assert_eq!(
            super::format_pdf_date("D:20261111093000+01'00'").as_deref(),
            Some("2026-11-11 09:30")
        );
        assert_eq!(
            super::format_pdf_date("D:20200101").as_deref(),
            Some("2020-01-01 00:00")
        );
        assert_eq!(super::format_pdf_date("yesterday"), None);
    }
}
