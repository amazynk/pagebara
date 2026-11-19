//! Output tools on the command line, with their own options: `pagebara --action watermark |
//! header-footer | bates | flatten | compare | fdf-export | fdf-import | xfdf-export |
//! xfdf-import | detect-fields [OPTION...] FILE...`. See [`USAGE`].

use std::path::{Path, PathBuf};

use pagebara_compare::Options as CompareOptions;
use pagebara_flatten::{Layers, Options as FlattenOptions};
use pagebara_stamp::{
    Anchor, DocInfo, HeaderFooter, Kind, Layer, Margins, PageSet, Rgb, Size, Slots, Source,
    TextStyle, Tiling, Visibility, Watermark,
};
use pagebara_syntax::Document;
use serde_json::{Value, json};

use crate::ops::{open, save};
use crate::out::{self, parent, stem};

pub const NAMES: [&str; 10] = [
    "watermark",
    "header-footer",
    "bates",
    "flatten",
    "compare",
    "fdf-export",
    "fdf-import",
    "xfdf-export",
    "xfdf-import",
    "detect-fields",
];

pub const USAGE: &str = "output tools: pagebara --action TOOL [OPTION...] FILE...
  common:        --out FILE|DIR  --password TEXT  --json (a JSON report on stdout)
                 --pages LIST  --odd  --even (pages to stamp)
  watermark      --text TEXT | --image PNG|JPG | --pdf FILE [--pdf-page N]
                 --font NAME  --size PT  --fit FRACTION (of the page)  --scale FACTOR
                 --color #RRGGBB  --opacity 0-1  --rotation DEG
                 --position center|top|top-left|... --offset X,Y  --margin PT
                 --tile GAP  --behind  --layer NAME | --no-layer  --view-only | --print-only
                 --replace (our earlier watermarks first)
                 --remove [--foreign] (take watermarks off instead)
  header-footer  --top-left T --top-center T --top-right T
                 --bottom-left T --bottom-center T --bottom-right T
                 (tokens: {page} {pages} {label} {date} {date:%d.%m.%Y} {time} {file}
                  {title} {author})
                 --first-page-blank  --font NAME --size PT --color #RRGGBB --box #RRGGBB
                 --margins L,R,T,B  --start N  --keep (do not replace earlier ones)
                 --remove [--foreign]
  bates          --prefix P --suffix S --digits N --start N  --position bottom-right|...
                 --box #RRGGBB  --font NAME --size PT  --log CSV
                 numbers the files in the order given, each continuing the last
  flatten        --annotations --links --forms --signatures --layers apply|merge|keep
                 --transparency DPI  (no choice: everything but links, signatures and
                 transparency)
  compare        OLD NEW  --report PDF  --no-text --no-visual --no-annotations
                 --dpi N --tolerance 0-255  --all-pages (report every page pair)
  fdf-export     FORM.pdf (writes FORM.fdf with the field values)
  xfdf-export    FORM.pdf  --annotations (with the comments too)
  fdf-import     FORM.pdf DATA.fdf (writes FORM-filled.pdf; --data DATA also works)
  xfdf-import    FORM.pdf DATA.xfdf (values and annotations)
  detect-fields  FILE.pdf  --list (only report what is found)
                 (writes FILE-fields.pdf with the fields found on the pages)";

/// What a run did.
#[derive(Debug, Default)]
pub struct ProReport {
    pub outputs: Vec<PathBuf>,
    pub messages: Vec<String>,
    /// The `--json` report.
    pub json: Option<String>,
}

/// The options left to read.
pub(crate) struct Args {
    items: Vec<String>,
}

impl Args {
    pub(crate) fn flag(&mut self, name: &str) -> bool {
        match self.items.iter().position(|a| a == name) {
            Some(i) => {
                self.items.remove(i);
                true
            }
            None => false,
        }
    }

    pub(crate) fn value(&mut self, name: &str) -> Result<Option<String>, String> {
        let Some(i) = self.items.iter().position(|a| a == name) else {
            return Ok(None);
        };
        if i + 1 >= self.items.len() {
            return Err(format!("{name} needs a value"));
        }
        self.items.remove(i);
        Ok(Some(self.items.remove(i)))
    }

    fn number(&mut self, name: &str) -> Result<Option<f32>, String> {
        self.value(name)?
            .map(|v| {
                v.parse::<f32>()
                    .ok()
                    .filter(|f| f.is_finite())
                    .ok_or(format!("{name}: not a number: {v}"))
            })
            .transpose()
    }

    fn colour(&mut self, name: &str) -> Result<Option<Rgb>, String> {
        self.value(name)?
            .map(|v| Rgb::parse(&v).ok_or(format!("{name}: expected #rrggbb, got {v}")))
            .transpose()
    }

    /// The remaining arguments as files; unknown options are an error.
    pub(crate) fn files(self, cwd: &Path) -> Result<Vec<PathBuf>, String> {
        if let Some(o) = self.items.iter().find(|a| a.starts_with("--")) {
            return Err(format!("unknown option {o}\n{USAGE}"));
        }
        Ok(self.items.iter().map(|f| cwd.join(f)).collect())
    }
}

fn anchor(s: &str) -> Result<Anchor, String> {
    let names = [
        "top-left",
        "top",
        "top-right",
        "left",
        "center",
        "right",
        "bottom-left",
        "bottom",
        "bottom-right",
    ];
    let s = s.replace("centre", "center").replace("-center", "");
    names
        .iter()
        .position(|n| *n == s)
        .map(|i| Anchor::ALL[i])
        .ok_or(format!("unknown position {s}"))
}

fn slot_of(s: &str) -> Result<usize, String> {
    match s.replace("centre", "center").as_str() {
        "top-left" => Ok(0),
        "top" | "top-center" => Ok(1),
        "top-right" => Ok(2),
        "bottom-left" => Ok(3),
        "bottom" | "bottom-center" => Ok(4),
        "bottom-right" => Ok(5),
        o => Err(format!("unknown position {o} (top-left … bottom-right)")),
    }
}

fn pair(name: &str, v: &str) -> Result<[f32; 2], String> {
    let (a, b) = v.split_once(',').ok_or(format!("{name}: expected X,Y"))?;
    let p = |s: &str| s.trim().parse::<f32>().map_err(|_| format!("{name}: {v}"));
    Ok([p(a)?, p(b)?])
}

fn pages(a: &mut Args) -> Result<PageSet, String> {
    Ok(PageSet {
        list: a.value("--pages")?.unwrap_or_default(),
        odd: a.flag("--odd"),
        even: a.flag("--even"),
    })
}

pub(crate) struct Common {
    pub output: Option<PathBuf>,
    pub password: Option<String>,
    pub json: bool,
}

fn common(a: &mut Args, cwd: &Path) -> Result<Common, String> {
    Ok(Common {
        output: a.value("--out")?.map(|o| cwd.join(o)),
        password: a.value("--password")?,
        json: a.flag("--json"),
    })
}

fn write(
    c: &Common,
    inputs: usize,
    input: &Path,
    suffix: &str,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let given = c.output.as_deref().filter(|p| inputs == 1 || p.is_dir());
    out::write_output(
        given,
        &parent(input),
        &format!("{}{suffix}", stem(input)),
        bytes,
    )
    .map_err(|e| format!("cannot write: {e}"))
}

pub(crate) fn finish(c: &Common, mut r: ProReport, value: Value) -> ProReport {
    if c.json {
        r.json = Some(serde_json::to_string_pretty(&value).unwrap_or_default());
    }
    r
}

/// Runs output tool `name` with its arguments (everything after the name).
pub fn run_pro(name: &str, args: &[String], cwd: &Path) -> Result<ProReport, String> {
    let mut a = Args {
        items: args.to_vec(),
    };
    let c = common(&mut a, cwd)?;
    match name {
        "watermark" => watermark(a, &c, cwd),
        "header-footer" => header_footer(a, &c, cwd, false),
        "bates" => header_footer(a, &c, cwd, true),
        "flatten" => flatten(a, &c, cwd),
        "compare" => compare(a, &c, cwd),
        "fdf-export" | "xfdf-export" => crate::forms::export(name, a, &c, cwd),
        "fdf-import" | "xfdf-import" => crate::forms::import(name, a, &c, cwd),
        "detect-fields" => crate::forms::detect(a, &c, cwd),
        _ => Err(format!("unknown tool {name}")),
    }
}

/// Runs `f` on each file, writing `-suffix` copies; per-file JSON from `f`.
fn each(
    files: &[PathBuf],
    c: &Common,
    suffix: &str,
    f: &dyn Fn(&mut Document, &Path) -> Result<Value, String>,
) -> Result<ProReport, String> {
    if files.is_empty() {
        return Err("no files given".into());
    }
    let mut r = ProReport::default();
    let mut items = Vec::new();
    for p in files {
        let res = open(p, c.password.as_deref()).and_then(|mut d| {
            let v = f(&mut d, p)?;
            let bytes = save(&d)?;
            let o = write(c, files.len(), p, suffix, &bytes)?;
            Ok((o, v))
        });
        match res {
            Ok((o, mut v)) => {
                v["input"] = json!(p);
                v["output"] = json!(o);
                items.push(v);
                r.outputs.push(o);
            }
            Err(e) => {
                items.push(json!({"input": p, "error": e}));
                r.messages.push(format!("{}: {e}", p.display()));
            }
        }
    }
    if r.outputs.is_empty() {
        return Err(r.messages.join("\n"));
    }
    Ok(finish(c, r, json!({ "files": items })))
}

fn remove_kind(
    mut a: Args,
    c: &Common,
    cwd: &Path,
    kinds: &'static [Kind],
    suffix: &str,
) -> Result<ProReport, String> {
    let foreign = a.flag("--foreign");
    let files = a.files(cwd)?;
    each(&files, c, suffix, &|d, _| {
        let r = pagebara_stamp::remove(d, kinds, foreign).map_err(|e| e.to_string())?;
        Ok(
            json!({"removed": r.removed, "pages": r.pages.iter().map(|p| p + 1).collect::<Vec<_>>()}),
        )
    })
}

fn watermark(mut a: Args, c: &Common, cwd: &Path) -> Result<ProReport, String> {
    if a.flag("--remove") {
        return remove_kind(a, c, cwd, &[Kind::Watermark], "-no-watermark");
    }
    let mut wm = Watermark::default();
    let font = a.value("--font")?;
    let size = a.number("--size")?;
    let color = a.colour("--color")?;
    if let Some(t) = a.value("--text")? {
        let mut style = match wm.source {
            Source::Text { style, .. } => style,
            _ => TextStyle::default(),
        };
        if let Some(f) = font {
            style.font = f;
        }
        if let Some(s) = size {
            style.size = s;
            wm.size = Size::Scale(1.0);
        }
        if let Some(col) = color {
            style.color = col;
        }
        wm.source = Source::Text {
            text: t.replace("\\n", "\n"),
            style,
        };
    } else if let Some(p) = a.value("--image")? {
        wm.source = Source::Image {
            path: cwd.join(p),
            data: Vec::new(),
        };
        wm.rotation = 0.0;
        wm.size = Size::Fit(0.5);
    } else if let Some(p) = a.value("--pdf")? {
        let page = a.number("--pdf-page")?.unwrap_or(1.0).max(1.0) as usize;
        wm.source = Source::Pdf {
            path: cwd.join(p),
            page,
            data: Vec::new(),
        };
        wm.rotation = 0.0;
        wm.size = Size::Fit(0.5);
    } else {
        return Err(format!("watermark: give --text, --image or --pdf\n{USAGE}"));
    }
    if let Some(f) = a.number("--fit")? {
        wm.size = Size::Fit(f);
    }
    if let Some(f) = a.number("--scale")? {
        wm.size = Size::Scale(f);
    }
    if let Some(o) = a.number("--opacity")? {
        wm.opacity = o.clamp(0.0, 1.0);
    }
    if let Some(r) = a.number("--rotation")? {
        wm.rotation = r;
    }
    if let Some(p) = a.value("--position")? {
        wm.anchor = anchor(&p)?;
    }
    if let Some(o) = a.value("--offset")? {
        wm.offset = pair("--offset", &o)?;
    }
    if let Some(m) = a.number("--margin")? {
        wm.margin = m;
    }
    if let Some(g) = a.number("--tile")? {
        wm.tiling = Some(Tiling { gap: [g, g] });
    }
    if a.flag("--behind") {
        wm.layer = Layer::Behind;
    }
    if let Some(l) = a.value("--layer")? {
        wm.group = Some(l);
    }
    if a.flag("--no-layer") {
        wm.group = None;
    }
    if a.flag("--view-only") {
        wm.visibility = Visibility::ViewOnly;
    }
    if a.flag("--print-only") {
        wm.visibility = Visibility::PrintOnly;
    }
    wm.replace = a.flag("--replace");
    wm.pages = pages(&mut a)?;
    let files = a.files(cwd)?;
    each(&files, c, "-watermarked", &|d, _| {
        let r = pagebara_stamp::apply_watermark(d, &wm).map_err(|e| e.to_string())?;
        Ok(json!({
            "pages": r.pages.iter().map(|p| p + 1).collect::<Vec<_>>(),
            "missing": r.missing.iter().collect::<String>(),
            "replaced": r.replaced,
        }))
    })
}

fn header_footer(mut a: Args, c: &Common, cwd: &Path, bates: bool) -> Result<ProReport, String> {
    let kind: &'static [Kind] = if bates {
        &[Kind::Bates]
    } else {
        &[Kind::HeaderFooter]
    };
    if a.flag("--remove") {
        let suffix = if bates { "-no-bates" } else { "-no-header" };
        return remove_kind(a, c, cwd, kind, suffix);
    }
    let mut hf = HeaderFooter::default();
    let mut slots = Slots::default();
    for (i, name) in [
        "--top-left",
        "--top-center",
        "--top-right",
        "--bottom-left",
        "--bottom-center",
        "--bottom-right",
    ]
    .iter()
    .enumerate()
    {
        if let Some(v) = a
            .value(name)?
            .or(a.value(&name.replace("center", "centre"))?)
        {
            slots.0[i] = v.replace("\\n", "\n");
        }
    }
    if let Some(f) = a.value("--font")? {
        hf.style.font = f;
    }
    if let Some(s) = a.number("--size")? {
        hf.style.size = s;
    }
    if let Some(col) = a.colour("--color")? {
        hf.style.color = col;
    }
    hf.style.background = a.colour("--box")?;
    if let Some(m) = a.value("--margins")? {
        let v: Vec<f32> = m.split(',').filter_map(|s| s.trim().parse().ok()).collect();
        let [l, r, t, b] = v[..] else {
            return Err("--margins: expected L,R,T,B".into());
        };
        hf.margins = Margins {
            left: l,
            right: r,
            top: t,
            bottom: b,
        };
    }
    if let Some(s) = a.number("--start")? {
        if bates {
            hf.bates.get_or_insert_default().start = s.max(0.0) as u64;
        } else {
            hf.start = s as i64;
        }
    }
    if let Some(f) = a.value("--date-format")? {
        hf.date_format = f;
    }
    if a.flag("--first-page-blank") {
        hf.first = Some(Slots::default());
    }
    hf.replace = !a.flag("--keep");
    hf.pages = pages(&mut a)?;
    if bates {
        let b = hf.bates.get_or_insert_default();
        b.prefix = a.value("--prefix")?.unwrap_or_default();
        b.suffix = a.value("--suffix")?.unwrap_or_default();
        if let Some(d) = a.number("--digits")? {
            b.digits = d.clamp(1.0, 20.0) as usize;
        }
        let pos = a.value("--position")?.unwrap_or("bottom-right".into());
        if slots.is_empty() {
            slots = Slots::one(slot_of(&pos)?, "{bates}");
        }
        let log = a.value("--log")?.map(|l| cwd.join(l));
        hf.slots = slots;
        let files = a.files(cwd)?;
        if files.is_empty() {
            return Err("no files given".into());
        }
        let n = files.len();
        let done = pagebara_stamp::bates_batch(
            &files,
            &hf,
            c.password.as_deref(),
            &|_, input, data| {
                let given = c.output.as_deref().filter(|p| n == 1 || p.is_dir());
                out::write_output(
                    given,
                    &parent(input),
                    &format!("{}-bates", stem(input)),
                    data,
                )
            },
            &|_, _| {},
        )
        .map_err(|e| e.to_string())?;
        let mut r = ProReport {
            outputs: done.iter().map(|d| d.output.clone()).collect(),
            ..ProReport::default()
        };
        if let Some(l) = log {
            std::fs::write(&l, pagebara_stamp::bates_csv(&done))
                .map_err(|e| format!("{}: {e}", l.display()))?;
            r.messages
                .push(format!("Ranges written to {}", l.display()));
        }
        for d in &done {
            r.messages
                .push(format!("{}: {} – {}", d.input.display(), d.first, d.last));
        }
        return Ok(finish(c, r, json!({ "files": done })));
    }
    if slots.is_empty() {
        return Err(format!(
            "header-footer: give at least one slot text\n{USAGE}"
        ));
    }
    hf.slots = slots;
    let files = a.files(cwd)?;
    each(&files, c, "-header", &|d, p| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let info = DocInfo::read(d, &name);
        let r = pagebara_stamp::apply_header_footer(d, &hf, &info).map_err(|e| e.to_string())?;
        Ok(json!({
            "pages": r.pages.iter().map(|p| p + 1).collect::<Vec<_>>(),
            "missing": r.missing.iter().collect::<String>(),
            "replaced": r.replaced,
        }))
    })
}

fn flatten(mut a: Args, c: &Common, cwd: &Path) -> Result<ProReport, String> {
    let picked = [
        a.flag("--annotations"),
        a.flag("--links"),
        a.flag("--forms"),
        a.flag("--signatures"),
    ];
    let layers = a.value("--layers")?;
    let transparency = a.number("--transparency")?;
    let mut o = if picked.iter().any(|p| *p) || layers.is_some() {
        let mut o = FlattenOptions::nothing();
        o.annotations = picked[0] || picked[1];
        o.links = picked[1];
        o.forms = picked[2] || picked[3];
        o.signatures = picked[3];
        o
    } else {
        FlattenOptions::all()
    };
    if let Some(l) = layers {
        o.layers = match l.as_str() {
            "apply" => Layers::Apply,
            "merge" => Layers::Merge,
            "keep" => Layers::Keep,
            o => return Err(format!("--layers: apply, merge or keep, not {o}")),
        };
    }
    o.transparency = transparency;
    let files = a.files(cwd)?;
    each(&files, c, "-flat", &|d, _| {
        let r = pagebara_flatten::flatten(d, &o).map_err(|e| e.to_string())?;
        Ok(serde_json::to_value(&r).unwrap_or_default())
    })
}

fn compare(mut a: Args, c: &Common, cwd: &Path) -> Result<ProReport, String> {
    let mut o = CompareOptions {
        text: !a.flag("--no-text"),
        visual: !a.flag("--no-visual"),
        annotations: !a.flag("--no-annotations"),
        ..CompareOptions::default()
    };
    if let Some(d) = a.number("--dpi")? {
        o.dpi = d.clamp(10.0, 300.0);
    }
    if let Some(t) = a.number("--tolerance")? {
        o.tolerance = t.clamp(0.0, 255.0) as u8;
    }
    let all = a.flag("--all-pages");
    let report_to = a.value("--report")?.map(|r| cwd.join(r));
    let files = a.files(cwd)?;
    let [old, new] = &files[..] else {
        return Err("compare needs two files: OLD NEW".into());
    };
    let da = open(old, c.password.as_deref()).map_err(|e| format!("{}: {e}", old.display()))?;
    let db = open(new, c.password.as_deref()).map_err(|e| format!("{}: {e}", new.display()))?;
    let cmp = pagebara_compare::compare(&da, &db, &o, &|_, _, _| {}, None);
    let mut r = ProReport::default();
    let s = &cmp.summary;
    r.messages.push(format!(
        "{} changes: {} words inserted, {} deleted, {} moved; {} pages inserted, {} deleted, {} looking different ({} ms)",
        s.changes,
        s.words_inserted,
        s.words_deleted,
        s.words_moved,
        s.pages_inserted,
        s.pages_deleted,
        s.pages_looking_different,
        cmp.timing.total_ms
    ));
    if !c.json {
        for ch in &cmp.changes {
            let at = match (ch.page_a, ch.page_b) {
                (Some(x), Some(y)) => format!("p. {}/{}", x + 1, y + 1),
                (Some(x), None) => format!("p. {}/-", x + 1),
                (None, Some(y)) => format!("p. -/{}", y + 1),
                _ => "doc".into(),
            };
            r.messages
                .push(format!("{at}  {}  {}", ch.kind.label(), ch.description));
        }
    }
    if let Some(dest) = report_to.or(c.output.clone()) {
        let name = |p: &Path| {
            p.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        };
        let pdf = pagebara_compare::report(
            &da,
            &db,
            &cmp,
            &pagebara_compare::ReportOptions {
                old_name: name(old),
                new_name: name(new),
                only_changed: !all,
            },
        )?;
        let base = format!("{} vs {}", stem(old), stem(new));
        let p = out::write_output(Some(&dest), &parent(old), &base, &pdf)
            .map_err(|e| format!("cannot write: {e}"))?;
        r.outputs.push(p);
    }
    Ok(finish(c, r, serde_json::to_value(&cmp).unwrap_or_default()))
}
