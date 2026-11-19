//! File actions without a window, for file-manager menus and scripts (`pagebara --action NAME
//! FILE...`): merge, split, rotate, compress, pages to images, images to PDF, text
//! recognition, PDF/A conversion and conformance checks; and output tools with options of
//! their own ([`run_pro`]: watermark, header-footer, bates, flatten, compare, form data
//! export and import, field detection). Inputs are
//! never overwritten, except by Rotate, which replaces the file atomically; outputs go
//! next to the (first) input under names that are not taken yet.

mod collect;
mod fill;
mod forms;
mod ops;
mod out;
mod pro;

use std::path::PathBuf;

pub use collect::collect;
pub use fill::{fill_form, form_target, is_form_data};
pub use pro::{NAMES as PRO_TOOLS, ProReport, USAGE as PRO_USAGE, run_pro};

/// One file action.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Action {
    Merge,
    Split,
    RotateRight,
    RotateLeft,
    Compress,
    Images,
    ConvertImages,
    Ocr,
    Pdfa,
    Validate,
}

impl Action {
    pub const ALL: [Action; 10] = [
        Action::Merge,
        Action::Split,
        Action::RotateRight,
        Action::RotateLeft,
        Action::Compress,
        Action::Images,
        Action::ConvertImages,
        Action::Ocr,
        Action::Pdfa,
        Action::Validate,
    ];

    /// The name on the command line (`--action NAME`).
    pub fn name(self) -> &'static str {
        match self {
            Action::Merge => "merge",
            Action::Split => "split",
            Action::RotateRight => "rotate-right",
            Action::RotateLeft => "rotate-left",
            Action::Compress => "compress",
            Action::Images => "images",
            Action::ConvertImages => "convert",
            Action::Ocr => "ocr",
            Action::Pdfa => "pdfa",
            Action::Validate => "validate",
        }
    }

    pub fn parse(s: &str) -> Option<Action> {
        Action::ALL.into_iter().find(|a| a.name() == s)
    }

    /// Menu text.
    pub fn title(self) -> &'static str {
        match self {
            Action::Merge => "Merge into one PDF",
            Action::Split => "Split into single pages",
            Action::RotateRight => "Rotate right",
            Action::RotateLeft => "Rotate left",
            Action::Compress => "Compress",
            Action::Images => "Convert pages to images",
            Action::ConvertImages => "Convert to PDF",
            Action::Ocr => "Make searchable (OCR)",
            Action::Pdfa => "Save as PDF/A",
            Action::Validate => "Check PDF/A or PDF/UA conformance",
        }
    }

    /// Whether all selected files go into one run (one output from several inputs).
    pub fn multi(self) -> bool {
        matches!(self, Action::Merge | Action::ConvertImages)
    }

    /// The file extensions the action takes (lower case).
    pub fn inputs(self) -> &'static [&'static str] {
        match self {
            Action::ConvertImages => &["png", "jpg", "jpeg"],
            _ => &["pdf"],
        }
    }
}

/// Choices for [`run`].
#[derive(Debug, Default, Clone)]
pub struct Options {
    /// Where to write instead of next to the input: a file for one output (or a folder to
    /// put it in), a folder for several.
    pub output: Option<PathBuf>,
    /// For protected inputs.
    pub password: Option<String>,
    /// Resolution of [`Action::Images`] (default 150).
    pub dpi: Option<f32>,
    /// The text recognition models ([`Action::Ocr`]); never downloaded here.
    pub models_dir: Option<PathBuf>,
    /// What [`Action::Compress`] does (the lossless preset when `None`).
    pub optimize: Option<pagebara_optimize::Options>,
    /// The part for [`Action::Pdfa`] or the profile for [`Action::Validate`] (PDF/A-2b
    /// when `None`).
    pub conformance: Option<pagebara_conform::Profile>,
    /// [`Action::Validate`] prints JSON.
    pub json: bool,
}

/// What [`run`] did.
#[derive(Debug, Default)]
pub struct Report {
    /// Files written (for Rotate, the inputs turned in place).
    pub outputs: Vec<PathBuf>,
    /// One line per input left as it was, or a note such as the size saved.
    pub messages: Vec<String>,
    /// Output for standard output (conformance reports).
    pub printed: Vec<String>,
    /// A check failed (a file does not conform).
    pub failed: bool,
}

/// Runs `action` on `files`; `progress(done, total)` is called as steps finish. Fails
/// only when nothing was written; otherwise problems with single files are in
/// [`Report::messages`].
pub fn run(
    action: Action,
    files: &[PathBuf],
    opts: &Options,
    progress: &(dyn Fn(usize, usize) + Sync),
) -> Result<Report, String> {
    if files.is_empty() {
        return Err("no files given".into());
    }
    let mut report = Report::default();
    let errors = match action {
        Action::Merge => ops::merge(files, opts, &mut report, progress),
        Action::ConvertImages => ops::convert(files, opts, &mut report, progress),
        Action::Ocr => ops::ocr(files, opts, &mut report, progress),
        _ => {
            let mut errors = 0;
            for (i, f) in files.iter().enumerate() {
                let r = match action {
                    Action::Split => ops::split(f, files.len(), opts, &mut report),
                    Action::RotateRight => ops::rotate(f, 1, files.len(), opts, &mut report),
                    Action::RotateLeft => ops::rotate(f, -1, files.len(), opts, &mut report),
                    Action::Compress => ops::compress(f, files.len(), opts, &mut report),
                    Action::Pdfa => ops::pdfa(f, files.len(), opts, &mut report),
                    Action::Validate => ops::validate(f, opts, &mut report),
                    _ => ops::images(f, files.len(), opts, &mut report),
                };
                if let Err(e) = r {
                    report.messages.push(format!("{}: {e}", f.display()));
                    errors += 1;
                }
                progress(i + 1, files.len());
            }
            errors
        }
    };
    if report.outputs.is_empty() && report.printed.is_empty() && errors > 0 {
        return Err(report.messages.join("\n"));
    }
    Ok(report)
}
