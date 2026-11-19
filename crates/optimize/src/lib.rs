//! Making PDF files smaller.
//!
//! [`optimize`] rewrites a document as [`Options`] ask: object and cross-reference streams,
//! Flate recompression (up to Zopfli), merging identical objects, dropping unreachable
//! objects and unused resources, subsetting embedded TrueType fonts, stripping optional
//! parts, and for the lossy presets downsampling and re-encoding images, Group 4 for
//! bilevel images and grey conversion. [`Preset::Lossless`] never changes what a page
//! shows. The [`Report`] gives sizes before and after, per category.
//!
//! Any rewrite invalidates digital signatures: [`Report::signed`] says so, and
//! [`Options::keep_signatures`] refuses to touch signed files instead.

mod dedupe;
mod fonts;
mod g4;
mod gray;
mod images;
mod resample;
mod resources;
mod scan;
mod stats;
mod streams;
mod strip;
mod write;

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use pagebara_i18n::tr;
use pagebara_syntax::{Document, SaveOptions};

pub use write::{WriteOptions, write};

pub use resample::Filter as ResampleFilter;
pub use stats::{Category, CategorySize};

/// Ready-made choices.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Preset {
    /// Pages look exactly the same; only the file's structure and coding change.
    #[default]
    Lossless,
    /// Images above 225 ppi go to 150, photos become JPEG at quality 80.
    Balanced,
    /// Images above 150 ppi go to 100, JPEG quality 60, metadata and private data go.
    Small,
    /// Whatever the options say.
    Custom,
}

impl Preset {
    pub const ALL: [Preset; 4] = [
        Preset::Lossless,
        Preset::Balanced,
        Preset::Small,
        Preset::Custom,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Preset::Lossless => "lossless",
            Preset::Balanced => "balanced",
            Preset::Small => "small",
            Preset::Custom => "custom",
        }
    }

    pub fn parse(s: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.name() == s)
    }

    /// The options of the preset (`Custom` starts from `Lossless`).
    pub fn options(self) -> Options {
        let lossless = Options {
            object_streams: true,
            effort: Effort::Best,
            dedupe: true,
            remove_unused: true,
            subset_fonts: true,
            strip: Strip::default(),
            images: Images {
                bitonal_g4: true,
                ..Images::default()
            },
            grayscale: false,
            linearize: false,
            keep_signatures: false,
        };
        match self {
            Preset::Lossless | Preset::Custom => lossless,
            Preset::Balanced => Options {
                strip: Strip {
                    thumbnails: true,
                    piece_info: true,
                    ..Strip::default()
                },
                images: Images {
                    target_ppi: Some(150.0),
                    jpeg_quality: 80,
                    recompress_jpeg: false,
                    bitonal_g4: true,
                    filter: ResampleFilter::Lanczos3,
                },
                ..lossless
            },
            Preset::Small => Options {
                strip: Strip {
                    metadata: true,
                    thumbnails: true,
                    piece_info: true,
                    ..Strip::default()
                },
                images: Images {
                    target_ppi: Some(100.0),
                    jpeg_quality: 60,
                    recompress_jpeg: true,
                    bitonal_g4: true,
                    filter: ResampleFilter::Lanczos3,
                },
                ..lossless
            },
        }
    }
}

/// How hard Flate compression tries.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Effort {
    /// Level 6.
    Fast,
    /// The best level of the regular compressor.
    #[default]
    Best,
    /// Zopfli for streams up to a few megabytes: 3–8% smaller again, far slower.
    Maximum,
}

/// Optional parts to remove.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Strip {
    /// XMP metadata everywhere and document information other than the title.
    pub metadata: bool,
    /// Page thumbnails.
    pub thumbnails: bool,
    /// Private data of the creating application (`/PieceInfo`).
    pub piece_info: bool,
    /// Scripts and the actions that run them.
    pub javascript: bool,
    /// Attached files.
    pub embedded_files: bool,
    /// Comments and markup (links and form fields stay).
    pub annotations: bool,
}

/// What happens to images. Images are only touched when a choice here asks for it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Images {
    /// Downsample images drawn above 1.5 × this resolution down to it.
    pub target_ppi: Option<f32>,
    /// JPEG quality for photos that are re-encoded (1–100).
    pub jpeg_quality: u8,
    /// Re-encode JPEG photos at `jpeg_quality` even when not downsampled, if much smaller.
    pub recompress_jpeg: bool,
    /// Bilevel images as CCITT Group 4 (lossless).
    pub bitonal_g4: bool,
    pub filter: ResampleFilter,
}

impl Default for Images {
    fn default() -> Self {
        Images {
            target_ppi: None,
            jpeg_quality: 80,
            recompress_jpeg: false,
            bitonal_g4: false,
            filter: ResampleFilter::Lanczos3,
        }
    }
}

impl Images {
    fn lossy(&self) -> bool {
        self.target_ppi.is_some() || self.recompress_jpeg
    }
}

/// Everything [`optimize`] may do.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    /// Objects in compressed object streams, with a cross-reference stream (PDF 1.5).
    pub object_streams: bool,
    pub effort: Effort,
    /// Merge identical objects (fonts, images and other streams included).
    pub dedupe: bool,
    /// Drop unreachable objects and resources no content uses.
    pub remove_unused: bool,
    /// Remove the outlines of glyphs no text uses from embedded TrueType fonts.
    pub subset_fonts: bool,
    pub strip: Strip,
    pub images: Images,
    /// Images and simple colour operators to grey.
    pub grayscale: bool,
    /// Lay the file out for fast web view (first page first, with hint tables).
    pub linearize: bool,
    /// Refuse signed documents instead of invalidating their signatures.
    pub keep_signatures: bool,
}

impl Default for Options {
    fn default() -> Self {
        Preset::Lossless.options()
    }
}

impl Options {
    /// Whether pages may look different afterwards.
    pub fn changes_appearance(&self) -> bool {
        self.images.lossy() || self.grayscale || self.strip.annotations
    }
}

/// What [`optimize`] did.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub before: u64,
    pub after: u64,
    pub categories: Vec<CategorySize>,
    pub images_resampled: usize,
    pub images_reencoded: usize,
    pub fonts_subset: usize,
    pub glyphs_removed: usize,
    pub objects_merged: usize,
    pub objects_removed: usize,
    pub resources_removed: usize,
    pub streams_recompressed: usize,
    pub parts_stripped: usize,
    /// The document was signed: its signatures no longer verify in the output.
    pub signed: bool,
    pub warnings: Vec<String>,
    pub seconds: f64,
}

impl Report {
    /// How much smaller, in percent of the original (negative when larger).
    pub fn saved_percent(&self) -> f64 {
        if self.before == 0 {
            return 0.0;
        }
        100.0 * (1.0 - self.after as f64 / self.before as f64)
    }
}

/// The optimized file and what was done.
#[derive(Debug)]
pub struct Outcome {
    pub bytes: Vec<u8>,
    pub report: Report,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// [`Options::keep_signatures`] and the document is signed.
    Signed,
    /// Stopped through the cancel flag.
    Cancelled,
    Save(String),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Signed => f.write_str(&tr!(
                "the document is signed; optimizing would break the signatures"
            )),
            Error::Cancelled => f.write_str(&tr!("cancelled")),
            Error::Save(e) => f.write_str(&tr!("cannot write the file: {}", e)),
        }
    }
}

impl std::error::Error for Error {}

/// Whether the document carries a filled-in signature field.
pub fn is_signed(doc: &Document) -> bool {
    let Some(cat) = doc.catalog() else {
        return false;
    };
    let Some(cat) = cat.as_dict() else {
        return false;
    };
    let form = doc.get_in(cat, b"AcroForm");
    let Some(form) = form.as_dict() else {
        return false;
    };
    let fields = doc.get_in(form, b"Fields");
    let mut stack: Vec<_> = fields.as_array().unwrap_or_default().to_vec();
    let mut seen = std::collections::HashSet::new();
    while let Some(f) = stack.pop() {
        if let Some(r) = f.as_objref()
            && !seen.insert(r.num)
        {
            continue;
        }
        let fo = doc.resolve(&f);
        let Some(d) = fo.as_dict() else { continue };
        if d.name(b"FT") == Some(b"Sig") && doc.get_in(d, b"V").as_dict().is_some() {
            return true;
        }
        if let Some(k) = doc.get_in(d, b"Kids").as_array() {
            stack.extend(k.iter().cloned());
        }
    }
    false
}

/// Rewrites `doc` smaller. `cancel` stops between steps.
pub fn optimize(
    doc: &Document,
    opts: &Options,
    cancel: Option<&AtomicBool>,
) -> Result<Outcome, Error> {
    let t = Instant::now();
    let trace = std::env::var_os("PAGEBARA_OPTIMIZE_TRACE").is_some();
    let stop = || {
        if trace {
            eprintln!("optimize: {:.3} s", t.elapsed().as_secs_f64());
        }
        cancel.is_some_and(|c| c.load(Ordering::Relaxed))
    };
    let signed = is_signed(doc);
    if signed && opts.keep_signatures {
        return Err(Error::Signed);
    }
    let mut report = Report {
        before: doc.source().len() as u64,
        signed,
        ..Report::default()
    };
    let before_sizes = stats::sizes(doc);
    if signed {
        // Not translated: the desktop app filters it out by the word "signed".
        report
            .warnings
            .push("The document was signed: its signatures will no longer be valid.".into());
    }
    let mut d = doc.clone();
    // Damage the writer would drop: a catalog as object 0, pages found only by scanning.
    d.move_root_off_zero();
    pagebara_pages::repair_tree(&mut d);
    report.parts_stripped = strip::strip(&mut d, &opts.strip);
    if opts.strip != Strip::default() || opts.remove_unused {
        report.objects_removed += pagebara_pages::remove_unreachable(&mut d);
    }
    if stop() {
        return Err(Error::Cancelled);
    }
    // A scan is still valid after a round of pruning that removed nothing.
    let mut usage = None;
    if opts.remove_unused {
        for _ in 0..2 {
            let u = scan::Scan::run(&d);
            let n = resources::prune(&mut d, &u);
            report.resources_removed += n;
            if n == 0 {
                usage = Some(u);
                break;
            }
            report.objects_removed += pagebara_pages::remove_unreachable(&mut d);
        }
    }
    let needs_scan = opts.subset_fonts || opts.images.target_ppi.is_some();
    if needs_scan && usage.is_none() {
        usage = Some(scan::Scan::run(&d));
    }
    if stop() {
        return Err(Error::Cancelled);
    }
    if opts.subset_fonts
        && let Some(u) = &usage
    {
        if u.uncertain {
            report.warnings.push(tr!(
                "Some page content could not be read; fonts were left whole."
            ));
        } else {
            let (fonts, glyphs) = fonts::subset(&mut d, u);
            report.fonts_subset = fonts;
            report.glyphs_removed = glyphs;
        }
    }
    if stop() {
        return Err(Error::Cancelled);
    }
    if opts.grayscale {
        gray::convert(&mut d, &mut report);
    }
    if opts.images.lossy() || opts.images.bitonal_g4 || opts.grayscale {
        images::process(
            &mut d,
            &opts.images,
            opts.grayscale,
            usage.as_ref(),
            &mut report,
        );
    }
    if stop() {
        return Err(Error::Cancelled);
    }
    report.streams_recompressed = streams::recompress_all(&mut d, opts.effort, cancel);
    if stop() {
        return Err(Error::Cancelled);
    }
    if opts.dedupe {
        report.objects_merged = dedupe::merge(&mut d);
        if report.objects_merged > 0 {
            report.objects_removed += pagebara_pages::remove_unreachable(&mut d);
        }
    }
    if stop() {
        return Err(Error::Cancelled);
    }
    let level = if opts.effort == Effort::Fast { 6 } else { 10 };
    let bytes = if d.is_encrypted() && d.trailer().contains(b"Encrypt") {
        // Protection stays; the syntax layer writes encrypted files.
        if opts.linearize {
            report
                .warnings
                .push(tr!("Protected files are not linearized."));
        }
        let save = SaveOptions {
            object_streams: opts.object_streams,
            compress_streams: true,
            xref_stream: opts.object_streams,
        };
        d.save_full_with(&save)
            .map_err(|e| Error::Save(e.to_string()))?
    } else {
        if opts.linearize && opts.object_streams {
            report.warnings.push(tr!(
                "Fast web view keeps objects out of compressed object streams, which can make files with many objects larger."
            ));
        }
        write::write(
            &d,
            &WriteOptions {
                object_streams: opts.object_streams && !opts.linearize,
                level,
                linearize: opts.linearize,
                min_version: (1, 4),
            },
        )
    };
    report.after = bytes.len() as u64;
    let after_sizes = stats::sizes(&d);
    report.categories = stats::compare(&before_sizes, report.before, &after_sizes, report.after);
    report.seconds = t.elapsed().as_secs_f64();
    Ok(Outcome { bytes, report })
}
