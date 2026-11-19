//! Conformance: PDF/A-1b, 2b, 2u, 3b and 3u (ISO 19005) and PDF/UA-1 (ISO 14289-1).
//!
//! - [`validate`] checks a document against a [`Profile`] and returns a [`Report`] of
//!   [`Issue`]s, each with the rule (clause and test, as veraPDF numbers them), the page or
//!   object concerned and a message. The PDF/A checks cover the rules that decide most
//!   real files (file structure, metadata, fonts, colour, transparency, annotations,
//!   actions, forms, attachments, optional content, limits); the PDF/UA checks are the
//!   machine-checkable failure conditions of the Matterhorn Protocol.
//! - [`pdfa::convert`] rewrites a document to a PDF/A part and validates the result.
//! - [`pdfua`] has the safe automatic fix-ups and the edits behind the guided fixes.

mod content;
pub mod icc;
pub mod pdfa;
pub mod pdfua;
mod xmp;

use pagebara_syntax::{Document, ObjRef};
use serde::Serialize;

/// What to check against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Profile {
    PdfA1b,
    PdfA2b,
    PdfA2u,
    PdfA3b,
    PdfA3u,
    PdfUa1,
}

impl Profile {
    pub const ALL: [Profile; 6] = [
        Profile::PdfA1b,
        Profile::PdfA2b,
        Profile::PdfA2u,
        Profile::PdfA3b,
        Profile::PdfA3u,
        Profile::PdfUa1,
    ];

    /// The name on the command line (`pdfa-2b`, `pdfua-1`).
    pub fn name(self) -> &'static str {
        match self {
            Profile::PdfA1b => "pdfa-1b",
            Profile::PdfA2b => "pdfa-2b",
            Profile::PdfA2u => "pdfa-2u",
            Profile::PdfA3b => "pdfa-3b",
            Profile::PdfA3u => "pdfa-3u",
            Profile::PdfUa1 => "pdfua-1",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Profile::PdfA1b => "PDF/A-1b",
            Profile::PdfA2b => "PDF/A-2b",
            Profile::PdfA2u => "PDF/A-2u",
            Profile::PdfA3b => "PDF/A-3b",
            Profile::PdfA3u => "PDF/A-3u",
            Profile::PdfUa1 => "PDF/UA-1",
        }
    }

    /// Reads `pdfa-2b`, `PDF/A-2b`, `2b`, `pdfua-1`, `PDF/UA-1`, `ua1` and the like.
    pub fn parse(s: &str) -> Option<Profile> {
        let mut k: String = s
            .to_ascii_lowercase()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        if let Some(rest) = k.strip_prefix("pdf") {
            k = rest.to_owned();
        }
        if k.starts_with(|c: char| c.is_ascii_digit()) {
            k.insert(0, 'a');
        }
        Some(match k.as_str() {
            "a1b" => Profile::PdfA1b,
            "a2b" => Profile::PdfA2b,
            "a2u" => Profile::PdfA2u,
            "a3b" => Profile::PdfA3b,
            "a3u" => Profile::PdfA3u,
            "ua1" | "ua" => Profile::PdfUa1,
            _ => return None,
        })
    }

    /// The PDF/A part (1, 2, 3), `None` for PDF/UA.
    pub fn part(self) -> Option<u8> {
        match self {
            Profile::PdfA1b => Some(1),
            Profile::PdfA2b | Profile::PdfA2u => Some(2),
            Profile::PdfA3b | Profile::PdfA3u => Some(3),
            Profile::PdfUa1 => None,
        }
    }

    /// Whether text must map to Unicode (level U).
    pub fn unicode(self) -> bool {
        matches!(self, Profile::PdfA2u | Profile::PdfA3u | Profile::PdfUa1)
    }

    /// The conformance letter written to XMP.
    pub fn conformance(self) -> &'static str {
        if matches!(self, Profile::PdfA2u | Profile::PdfA3u) {
            "U"
        } else {
            "B"
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, PartialOrd, Ord)]
pub enum Severity {
    /// The document does not conform.
    Error,
    /// Worth a look, but not a failure (checks a machine cannot decide).
    Warning,
}

/// A fix the app can apply for an issue.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub enum Fix {
    /// Done by [`pdfua::fix_safe`] (language, title, tab order, …).
    Automatic,
    /// Needs a person: alternative text, heading level, table headers.
    Guided,
}

/// One failed check.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Issue {
    /// `clause-test`, e.g. `6.2.11.4.1-1` or Matterhorn `13-004`.
    pub rule: String,
    /// The clause of the standard, e.g. `ISO 19005-2:2011, 6.2.11.4.1`.
    pub clause: String,
    pub severity: Severity,
    /// 0-based page, when the issue is on one.
    pub page: Option<usize>,
    /// The object concerned, as `num gen`.
    #[serde(serialize_with = "ser_ref")]
    pub object: Option<ObjRef>,
    pub message: String,
    pub fix: Option<Fix>,
}

fn ser_ref<S: serde::Serializer>(r: &Option<ObjRef>, s: S) -> Result<S::Ok, S::Error> {
    match r {
        Some(r) => s.serialize_str(&format!("{} {}", r.num, r.generation)),
        None => s.serialize_none(),
    }
}

/// The result of a validation.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Report {
    pub profile: Profile,
    pub issues: Vec<Issue>,
    /// How many rules were checked.
    pub rules_checked: usize,
}

impl Report {
    /// No errors (warnings allowed).
    pub fn passed(&self) -> bool {
        !self.issues.iter().any(|i| i.severity == Severity::Error)
    }

    pub fn errors(&self) -> usize {
        self.issues
            .iter()
            .filter(|i| i.severity == Severity::Error)
            .count()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Plain text, one issue a line.
    pub fn to_text(&self) -> String {
        let mut s = format!(
            "{}: {} ({} errors, {} warnings, {} rules checked)\n",
            self.profile.title(),
            if self.passed() { "passed" } else { "failed" },
            self.errors(),
            self.issues.len() - self.errors(),
            self.rules_checked
        );
        for i in &self.issues {
            let at = match (i.page, i.object) {
                (Some(p), _) => format!(" (page {})", p + 1),
                (None, Some(r)) => format!(" (object {} {})", r.num, r.generation),
                _ => String::new(),
            };
            let sev = if i.severity == Severity::Error {
                "error"
            } else {
                "warning"
            };
            s.push_str(&format!("{sev} {}{at}: {}\n", i.rule, i.message));
        }
        s
    }
}

/// Collects issues, one per rule and place.
pub(crate) struct Issues {
    pub list: Vec<Issue>,
    pub checked: std::collections::HashSet<&'static str>,
    standard: &'static str,
}

impl Issues {
    pub fn new(standard: &'static str) -> Issues {
        Issues {
            list: Vec::new(),
            checked: Default::default(),
            standard,
        }
    }

    /// Notes rule `rule` as checked; with `fail`, adds an issue.
    pub fn check(
        &mut self,
        rule: &'static str,
        fail: bool,
        page: Option<usize>,
        object: Option<ObjRef>,
        msg: impl FnOnce() -> String,
    ) {
        self.checked.insert(rule);
        if !fail {
            return;
        }
        if self
            .list
            .iter()
            .any(|i| i.rule == rule && i.page == page && i.object == object)
        {
            return;
        }
        let clause = rule.rsplit_once('-').map_or(rule, |c| c.0);
        self.list.push(Issue {
            rule: rule.to_owned(),
            clause: format!("{}, {clause}", self.standard),
            severity: Severity::Error,
            page,
            object,
            message: msg(),
            fix: None,
        });
    }

    pub fn finish(mut self, profile: Profile) -> Report {
        self.list.sort_by(|a, b| {
            (a.severity, a.page.unwrap_or(usize::MAX), &a.rule).cmp(&(
                b.severity,
                b.page.unwrap_or(usize::MAX),
                &b.rule,
            ))
        });
        Report {
            profile,
            rules_checked: self.checked.len(),
            issues: self.list,
        }
    }
}

/// Whether text mapped to `s` counts as mapped to Unicode: not empty, and none of the
/// values PDF/A rules out or a reader only guessed (control characters).
pub(crate) fn real_unicode(s: &str) -> bool {
    !s.is_empty()
        && !s
            .chars()
            .any(|c| c < ' ' || matches!(c, '\u{feff}' | '\u{fffe}'))
}

/// Checks `doc` against `profile`.
pub fn validate(doc: &Document, profile: Profile) -> Report {
    match profile {
        Profile::PdfUa1 => pdfua::check(doc),
        p => pdfa::validate(doc, p),
    }
}
