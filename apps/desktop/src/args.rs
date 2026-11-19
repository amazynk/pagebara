//! Command line: files to open, plus developer options for benchmarks, screenshots and
//! scripted UI runs.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "usage: pagebara [OPTION...] [FILE...]
  FILE                a path or file:// URI; FILE#page=N starts at page N
  --page N            start at page N (1-based)
  --search TEXT       find TEXT in the files opened
  --new-window        open a window of its own instead of a tab in the running one
  -h, --help          show this help
  -V, --version       show the version
command line output (no window):
  --text FILE         print the text of FILE, page by page
  --json              with --text: one JSON object per line and page,
                        {\"page\":N,\"width\":W,\"height\":H,\"text\":\"...\",
                         \"blocks\":[{\"bbox\":[x0,y0,x1,y1],\"text\":\"...\"}]}
                      sizes and boxes in points of the page as shown, origin top left
  --images FILE       write the pages of FILE as PNG files (page-N.png), printing each path
  --out DIR           where --images writes (default: the current directory)
  --dpi N             resolution for --images (default 150)
  --pages LIST        only these pages: 5-10, 3, 1,4,7-9, 8- (to the end)
  --password TEXT     try this password on a protected file
  --thumbnail IN OUT SIZE
                      write page 1 of IN as a PNG of at most SIZE pixels (file managers)
  --action NAME FILE...
                      merge, split, rotate-right, rotate-left, compress, images, convert
                        (images to PDF), ocr, pdfa or validate; outputs go next to the
                        (first) file (--out FILE or DIR elsewhere, --password, --dpi)
  with --action compress:
    --preset NAME     lossless (default; pages look the same), balanced or small
    --dpi N           downsample images drawn above 1.5 x N pixels per inch to N
    --quality N       JPEG quality of re-encoded photos (1-100)
    --strip LIST      remove metadata,thumbnails,private,javascript,attachments,annotations
    --gray            convert images and colours to grey
    --effort NAME     fast, best (default) or max (Zopfli, slow)
    --linearize       fast web view: the first page loads first
  with --action pdfa: --part 1b|2b|2u|3b|3u (default 2b)
  with --action validate: --profile pdfa-1b|pdfa-2b|pdfa-2u|pdfa-3b|pdfa-3u|pdfua-1
                        [--json]; exits with 1 when a file does not conform
  --action TOOL [OPTION...] FILE...
                      output tools: watermark, header-footer, bates, flatten, compare
                        (--action TOOL --help lists their options; --json for reports)
  --collect           with --action: gather the files of one file-manager click that
                        arrive as one process per file (Windows)
  --integration status [--json] | enable ITEM... | disable ITEM... | remove-all
                      the system integration of Settings > System > System integration
  --sign, --print     open the file ready to sign, or with the print dialog
developer options:
  --synthetic N       open N generated A4 pages instead of a file
  --bench             scripted scrolling and zooming with frame times, then exit
  --screenshot PNG    save the window as PNG after --after milliseconds, then exit
  --after MS          delay before the screenshot (default 2000)
  --dark, --light     force the colour scheme
  --tool N            select tool N (0 = Select)
  --organize          start in the organize-pages view
  --zoom PERCENT      start at this zoom
  --script FILE       run a UI script (see pagebara_app_core::script), then exit
  --profile DIR       keep settings, recent files and signatures in DIR (with
                        --action validate: the conformance profile)
  --size WxH          initial window size in logical pixels
  --make-samples DIR  write the sample documents used by the UI tests, then exit
environment:
  PAGEBARA_TRACE_STARTUP=1      log a startup timeline (ms since start, per step)
  PAGEBARA_TRACE_STARTUP=STEP   the same, and quit at that step (\"first frame\", ...)";

#[derive(Debug, Default)]
pub struct Args {
    pub files: Vec<PathBuf>,
    pub synthetic: Option<usize>,
    pub bench: bool,
    pub screenshot: Option<PathBuf>,
    pub after: Option<Duration>,
    pub dark: Option<bool>,
    pub tool: Option<i32>,
    pub organize: bool,
    pub zoom: Option<f64>,
    pub page: Option<usize>,
    pub password: Option<String>,
    pub script: Option<PathBuf>,
    pub profile: Option<PathBuf>,
    pub size: Option<(f32, f32)>,
    pub make_samples: Option<PathBuf>,
    pub text: Option<PathBuf>,
    pub images: Option<PathBuf>,
    pub out: Option<PathBuf>,
    pub dpi: Option<f32>,
    pub pages: Option<String>,
    pub json: bool,
    pub search: Option<String>,
    pub new_window: bool,
    /// `--thumbnail IN OUT SIZE`.
    pub thumbnail: Option<(PathBuf, PathBuf, u32)>,
    /// `--action NAME`: the files are its inputs.
    pub action: Option<String>,
    pub collect: bool,
    /// `--action TOOL ARGS...` for an output tool: everything after its name.
    pub tool_args: Option<Vec<String>>,
    /// `--integration CMD`: the files are the item IDs.
    pub integration: Option<String>,
    pub sign: bool,
    pub print: bool,
    /// `--action compress` options.
    pub optimize: Option<pagebara_optimize::Options>,
    /// `--part` of `--action pdfa`, `--profile` of `--action validate`.
    pub conformance: Option<pagebara_conform::Profile>,
}

/// Why parsing stopped short of starting the app.
#[derive(Debug, PartialEq, Eq)]
pub enum Exit {
    /// `--help` or `--version`: print this and exit successfully.
    Print(String),
    /// A bad command line: print this and fail.
    Usage(String),
}

impl From<String> for Exit {
    fn from(msg: String) -> Self {
        Exit::Usage(msg)
    }
}

impl Args {
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Args, Exit> {
        let mut out = Args::default();
        let mut it = args.into_iter().skip(1);
        while let Some(a) = it.next() {
            // An output tool takes everything after its name as its own arguments.
            if a.to_str() == Some("--action") {
                let name = it
                    .next()
                    .and_then(|v| v.into_string().ok())
                    .ok_or("--action needs a value".to_owned())?;
                if pagebara_actions::PRO_TOOLS.contains(&name.as_str()) {
                    out.tool_args =
                        Some(it.by_ref().filter_map(|v| v.into_string().ok()).collect());
                }
                out.action = Some(name);
                continue;
            }
            let mut value = |name: &str| {
                it.next()
                    .and_then(|v| v.into_string().ok())
                    .ok_or_else(|| format!("{name} needs a value"))
            };
            let number = |name: &str, v: String| {
                v.parse::<f64>()
                    .map_err(|_| format!("{name}: not a number: {v}"))
            };
            match a.to_str() {
                Some("--synthetic") => {
                    out.synthetic = Some(number("--synthetic", value("--synthetic")?)? as usize)
                }
                Some("--bench") => out.bench = true,
                Some("--screenshot") => out.screenshot = Some(value("--screenshot")?.into()),
                Some("--after") => {
                    let ms = number("--after", value("--after")?)?;
                    out.after = Some(Duration::from_millis(ms as u64));
                }
                Some("--dark") => out.dark = Some(true),
                Some("--light") => out.dark = Some(false),
                Some("--tool") => out.tool = Some(number("--tool", value("--tool")?)? as i32),
                Some("--organize") => out.organize = true,
                Some("--zoom") => out.zoom = Some(number("--zoom", value("--zoom")?)? / 100.0),
                Some("--page") => {
                    out.page =
                        Some((number("--page", value("--page")?)? as usize).saturating_sub(1))
                }
                Some("--password") => out.password = Some(value("--password")?),
                Some("--script") => out.script = Some(value("--script")?.into()),
                Some("--profile") => out.profile = Some(value("--profile")?.into()),
                Some("--make-samples") => out.make_samples = Some(value("--make-samples")?.into()),
                Some("--text") => out.text = Some(value("--text")?.into()),
                Some("--images") => out.images = Some(value("--images")?.into()),
                Some("--out") => out.out = Some(value("--out")?.into()),
                Some("--dpi") => {
                    let d = number("--dpi", value("--dpi")?)?;
                    if !(1.0..=2400.0).contains(&d) {
                        return Err(format!("--dpi: {d} is outside 1-2400").into());
                    }
                    out.dpi = Some(d as f32);
                }
                Some("--pages") => out.pages = Some(value("--pages")?),
                Some("--json") => out.json = true,
                Some("--search") => out.search = Some(value("--search")?),
                Some("--new-window") => out.new_window = true,
                Some("--collect") => out.collect = true,
                Some("--integration") => out.integration = Some(value("--integration")?),
                Some("--sign") => out.sign = true,
                Some("--preset") => {
                    let v = value("--preset")?;
                    let p = pagebara_optimize::Preset::parse(&v).ok_or_else(|| {
                        format!("--preset: {v} is not lossless, balanced or small")
                    })?;
                    let o = out.optimize.get_or_insert_with(Default::default);
                    let keep = (o.images.target_ppi, o.linearize, o.grayscale);
                    *o = p.options();
                    if keep.0.is_some() {
                        o.images.target_ppi = keep.0;
                    }
                    o.linearize |= keep.1;
                    o.grayscale |= keep.2;
                }
                Some("--quality") => {
                    let q = number("--quality", value("--quality")?)?;
                    if !(1.0..=100.0).contains(&q) {
                        return Err(format!("--quality: {q} is outside 1-100").into());
                    }
                    let o = out.optimize.get_or_insert_with(Default::default);
                    o.images.jpeg_quality = q as u8;
                    o.images.recompress_jpeg = true;
                }
                Some("--strip") => {
                    let v = value("--strip")?;
                    let o = out.optimize.get_or_insert_with(Default::default);
                    for part in v.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                        match part {
                            "metadata" => o.strip.metadata = true,
                            "thumbnails" => o.strip.thumbnails = true,
                            "private" => o.strip.piece_info = true,
                            "javascript" => o.strip.javascript = true,
                            "attachments" => o.strip.embedded_files = true,
                            "annotations" => o.strip.annotations = true,
                            _ => return Err(format!("--strip: unknown part {part}").into()),
                        }
                    }
                }
                Some("--gray" | "--grey") => {
                    out.optimize.get_or_insert_with(Default::default).grayscale = true
                }
                Some("--linearize") => {
                    out.optimize.get_or_insert_with(Default::default).linearize = true
                }
                Some("--effort") => {
                    let v = value("--effort")?;
                    out.optimize.get_or_insert_with(Default::default).effort = match v.as_str() {
                        "fast" => pagebara_optimize::Effort::Fast,
                        "best" => pagebara_optimize::Effort::Best,
                        "max" | "maximum" => pagebara_optimize::Effort::Maximum,
                        _ => return Err(format!("--effort: {v} is not fast, best or max").into()),
                    };
                }
                Some("--part") => {
                    let v = value("--part")?;
                    let p = pagebara_conform::Profile::parse(&v)
                        .filter(|p| p.part().is_some())
                        .ok_or_else(|| format!("--part: {v} is not 1b, 2b, 2u, 3b or 3u"))?;
                    out.conformance = Some(p);
                }
                Some("--print") => out.print = true,
                Some("--thumbnail") => {
                    let input = value("--thumbnail")?;
                    let output = value("--thumbnail")?;
                    let size = value("--thumbnail")?;
                    let size = size
                        .parse::<u32>()
                        .ok()
                        .filter(|s| (1..=4096).contains(s))
                        .ok_or_else(|| format!("--thumbnail: bad size {size}"))?;
                    out.thumbnail = Some((input.into(), output.into(), size));
                }
                Some("--size") => {
                    let v = value("--size")?;
                    let (w, h) = v
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                        .ok_or_else(|| format!("--size: expected WxH, got {v}"))?;
                    out.size = Some((w, h));
                }
                Some("-h" | "--help") => return Err(Exit::Print(USAGE.into())),
                Some("-V" | "--version") => {
                    let v = format!("{} {}", pagebara_identity::NAME, env!("CARGO_PKG_VERSION"));
                    return Err(Exit::Print(v));
                }
                Some(s) if s.starts_with("--") => {
                    return Err(format!("unknown option {s}\n{USAGE}").into());
                }
                _ => out.files.push(PathBuf::from(a)),
            }
        }
        // `--profile` names a conformance profile for validation, else a settings folder.
        if matches!(out.action.as_deref(), Some("validate"))
            && let Some(p) = out
                .profile
                .as_ref()
                .and_then(|d| d.to_str())
                .and_then(pagebara_conform::Profile::parse)
        {
            out.conformance = Some(p);
            out.profile = None;
        }
        if let Some(o) = &mut out.optimize
            && let Some(dpi) = out.dpi
            && matches!(out.action.as_deref(), Some("compress"))
        {
            o.images.target_ppi = Some(dpi);
        }
        if out.optimize.is_some() && !matches!(out.action.as_deref(), Some("compress")) {
            return Err("--preset, --quality, --strip, --gray, --effort and --linearize go with --action compress"
                .to_owned()
                .into());
        }
        if out.conformance.is_some() && !matches!(out.action.as_deref(), Some("pdfa" | "validate"))
        {
            return Err("--part goes with --action pdfa".to_owned().into());
        }
        let export = out.text.is_some() || out.images.is_some();
        if out.pages.is_some() && !export {
            return Err("--pages goes with --text or --images".to_owned().into());
        }
        if (out.out.is_some() || out.dpi.is_some()) && !export && out.action.is_none() {
            return Err("--out and --dpi go with --text, --images or --action"
                .to_owned()
                .into());
        }
        let validate = matches!(out.action.as_deref(), Some("validate"));
        if out.json && out.text.is_none() && out.integration.is_none() && !validate {
            return Err(
                "--json goes with --text, --integration or --action validate"
                    .to_owned()
                    .into(),
            );
        }
        if out.collect && out.action.is_none() {
            return Err("--collect goes with --action".to_owned().into());
        }
        Ok(out)
    }

    /// Options that run the app for a test, a benchmark or a screenshot: such runs never
    /// hand their files to a running window.
    pub fn developer_run(&self) -> bool {
        self.script.is_some()
            || self.bench
            || self.screenshot.is_some()
            || self.synthetic.is_some()
            || self.make_samples.is_some()
    }
}
