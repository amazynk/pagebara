//! Markdown and HTML reports of a run, optionally against a baseline run, with one section
//! per reference (MuPDF, and PDFium's expected images).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::Path;

use serde_json::Value;

/// Worst pages listed per reference (with thumbnails when they have them).
const WORST: usize = 60;

const REFS: [(&str, &str); 2] = [
    ("mupdf", "MuPDF (`mutool draw` at the same resolution)"),
    (
        "pdfium-expected",
        "PDFium's expected images (its test suite's `*_expected.pdf.N.png`, 72 dpi)",
    ),
];

pub fn load(p: &Path) -> Result<Vec<Value>, String> {
    Ok(std::fs::read_to_string(p)
        .map_err(|e| format!("{}: {e}", p.display()))?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}

/// Known, intentional differences: `path substring<TAB>reason` lines, `#` comments.
pub fn known(p: Option<&Path>) -> Vec<(String, String)> {
    let Some(text) = p.and_then(|p| std::fs::read_to_string(p).ok()) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.split_once('\t') {
            Some((p, r)) => (p.trim().to_owned(), r.trim().to_owned()),
            None => (l.to_owned(), String::new()),
        })
        .collect()
}

fn known_reason<'a>(known: &'a [(String, String)], file: &str) -> Option<&'a str> {
    known
        .iter()
        .find(|(p, _)| file.contains(p.as_str()))
        .map(|(_, r)| r.as_str())
}

fn short(file: &str) -> &str {
    file.rsplit_once("/corpora/").map_or(file, |(_, f)| f)
}

fn mae(r: &Value) -> Option<f64> {
    r["mae"].as_f64()
}

fn file(r: &Value) -> &str {
    r["file"].as_str().unwrap_or("")
}

/// The reference of a record (runs before references were recorded compared with MuPDF).
fn reference(r: &Value) -> &str {
    r["ref"].as_str().unwrap_or("mupdf")
}

#[derive(Default)]
struct Stats {
    pages: usize,
    errors: usize,
    maes: Vec<f64>,
    ssim: f64,
    over: f64,
    size_mismatch: usize,
}

impl Stats {
    fn add(&mut self, r: &Value) {
        self.pages += 1;
        let Some(m) = mae(r) else {
            self.errors += 1;
            return;
        };
        self.maes.push(m);
        self.ssim += r["ssim"].as_f64().unwrap_or(0.0);
        self.over += r["over"].as_f64().unwrap_or(0.0);
        let theirs = if r["theirs"].is_array() {
            &r["theirs"]
        } else {
            &r["mupdf"]
        };
        let dim = |v: &Value, i: usize| v[i].as_u64().unwrap_or(0);
        if dim(&r["ours"], 0).abs_diff(dim(theirs, 0)) > 2
            || dim(&r["ours"], 1).abs_diff(dim(theirs, 1)) > 2
        {
            self.size_mismatch += 1;
        }
    }

    fn row(&mut self, name: &str) -> String {
        let n = self.maes.len().max(1) as f64;
        self.maes.sort_by(f64::total_cmp);
        let median = self.maes.get(self.maes.len() / 2).copied().unwrap_or(0.0);
        let mean = self.maes.iter().sum::<f64>() / n;
        let at = |t: f64| self.maes.iter().filter(|&&m| m >= t).count();
        format!(
            "| {name} | {} | {} | {mean:.2} | {median:.2} | {:.4} | {:.2} | {} | {} | {} | {} |",
            self.pages,
            self.errors,
            self.ssim / n,
            self.over / n,
            at(2.0),
            at(5.0),
            at(10.0),
            self.size_mismatch
        )
    }
}

const HEAD: &str = "| set | pages | errors | mean MAE | median MAE | mean SSIM | mean % over | MAE>=2 | MAE>=5 | MAE>=10 | size differs |\n|---|---|---|---|---|---|---|---|---|---|---|";

fn stats_table(results: &[&Value], known: &[(String, String)]) -> String {
    let mut by: BTreeMap<String, Stats> = BTreeMap::new();
    let mut all = Stats::default();
    let mut unknown = Stats::default();
    for r in results {
        all.add(r);
        if known_reason(known, file(r)).is_none() {
            unknown.add(r);
        }
        by.entry(r["corpus"].as_str().unwrap_or("?").to_owned())
            .or_default()
            .add(r);
    }
    let mut s = format!(
        "{HEAD}\n{}\n{}\n",
        all.row("all"),
        unknown.row("all but known")
    );
    for (c, st) in &mut by {
        let _ = writeln!(s, "{}", st.row(c));
    }
    s
}

/// The markdown report, the HTML report, and the thumbnails it refers to.
pub fn render(
    results: &[Value],
    known: &[(String, String)],
    baseline: Option<&[Value]>,
) -> (String, String, HashSet<String>) {
    let mut md = String::from("# Pixel differences\n\n");
    let _ = writeln!(
        md,
        "MAE: mean absolute difference per channel (0-255); % over: pixels with a channel differing by more than {}; SSIM over 8x8 windows of the grey image. {}\n",
        crate::image::THRESHOLD,
        if which("pdfium_test") {
            "pdfium_test is installed but not used."
        } else {
            "pdfium_test is not installed; PDFium is represented by its test suite's expected images."
        },
    );
    let mut worst_all: Vec<&Value> = Vec::new();
    for (name, title) in REFS {
        let rs: Vec<&Value> = results.iter().filter(|r| reference(r) == name).collect();
        if rs.is_empty() {
            continue;
        }
        let _ = writeln!(md, "## Against {title}\n");
        md += &stats_table(&rs, known);
        if let Some(b) = baseline {
            let bs: Vec<&Value> = b.iter().filter(|r| reference(r) == name).collect();
            if !bs.is_empty() {
                md += "\n### Against the baseline\n\n";
                md += &compare_baseline(&rs, &bs);
                md += "\nBaseline:\n\n";
                md += &stats_table(&bs, known);
            }
        }
        let mut worst: Vec<&Value> = rs
            .iter()
            .copied()
            .filter(|r| mae(r).is_some() && known_reason(known, file(r)).is_none())
            .collect();
        worst.sort_by(|a, b| mae(b).unwrap_or(0.0).total_cmp(&mae(a).unwrap_or(0.0)));
        worst.truncate(WORST);
        let _ = writeln!(
            md,
            "\n### Worst pages\n\n| MAE | % over | SSIM | page | file |\n|---|---|---|---|---|"
        );
        for r in &worst {
            let _ = writeln!(
                md,
                "| {:.2} | {:.2} | {:.3} | {} | {} |",
                mae(r).unwrap_or(0.0),
                r["over"].as_f64().unwrap_or(0.0),
                r["ssim"].as_f64().unwrap_or(0.0),
                r["page"],
                short(file(r))
            );
        }
        let errors: Vec<&&Value> = rs.iter().filter(|r| r["error"].is_string()).collect();
        let _ = writeln!(
            md,
            "\n### Pages without a comparison ({})\n\n| page | file | why |\n|---|---|---|",
            errors.len()
        );
        for r in errors.iter().take(200) {
            let _ = writeln!(
                md,
                "| {} | {} | {} |",
                r["page"],
                short(file(r)),
                r["error"].as_str().unwrap_or("").replace('|', "/")
            );
        }
        md.push('\n');
        worst_all.extend(worst);
    }
    if !known.is_empty() {
        let _ = writeln!(md, "## Known differences\n");
        for (p, why) in known {
            let _ = writeln!(md, "- `{p}`: {why}");
        }
    }
    let keep: HashSet<String> = worst_all
        .iter()
        .filter_map(|r| r["thumb"].as_str().map(str::to_owned))
        .collect();
    let html = html(&md, &worst_all);
    (md, html, keep)
}

fn compare_baseline(results: &[&Value], base: &[&Value]) -> String {
    let key = |r: &Value| (file(r).to_owned(), r["page"].as_u64());
    let old: HashMap<_, f64> = base
        .iter()
        .filter_map(|r| Some((key(r), mae(r)?)))
        .collect();
    let mut pairs: Vec<(f64, f64, &Value)> = results
        .iter()
        .filter_map(|r| Some((*old.get(&key(r))?, mae(r)?, *r)))
        .collect();
    let n = pairs.len().max(1) as f64;
    let (before, after) = (
        pairs.iter().map(|p| p.0).sum::<f64>() / n,
        pairs.iter().map(|p| p.1).sum::<f64>() / n,
    );
    let better = pairs.iter().filter(|p| p.1 + 1.0 < p.0).count();
    let worse = pairs.iter().filter(|p| p.1 > p.0 + 1.0).count();
    let mut s = format!(
        "{} pages in both runs: mean MAE {before:.3} before, {after:.3} after; {better} pages better and {worse} worse by more than 1.\n\n| before | after | page | file |\n|---|---|---|---|\n",
        pairs.len()
    );
    pairs.sort_by(|a, b| (b.1 - b.0).abs().total_cmp(&(a.1 - a.0).abs()));
    for (b, a, r) in pairs.iter().take(40) {
        if (a - b).abs() <= 1.0 {
            break;
        }
        let _ = writeln!(
            s,
            "| {b:.2} | {a:.2} | {} | {} |",
            r["page"],
            short(file(r))
        );
    }
    s
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn html(md: &str, worst: &[&Value]) -> String {
    let mut h = String::from(
        "<!doctype html><meta charset=utf-8><title>Pixel differences</title><style>\
body{font:14px system-ui,sans-serif;margin:16px;background:#fff;color:#111}\
pre{white-space:pre-wrap;background:#f4f4f4;padding:8px}\
.row{display:flex;gap:8px;flex-wrap:wrap;margin:12px 0;border-top:1px solid #ccc;padding-top:8px}\
figure{margin:0}figcaption{font-size:12px;color:#555}img{border:1px solid #ccc;max-width:100%}\
@media (prefers-color-scheme:dark){body{background:#111;color:#eee}pre{background:#222}}\
</style><h1>Pixel differences</h1>",
    );
    let _ = write!(h, "<pre>{}</pre><h2>Worst pages</h2>", esc(md));
    for r in worst {
        let _ = write!(
            h,
            "<div class=row><div><b>{}</b> page {} against {}<br>MAE {:.2}, {:.2}% over, SSIM {:.3}</div>",
            esc(short(file(r))),
            r["page"],
            reference(r),
            mae(r).unwrap_or(0.0),
            r["over"].as_f64().unwrap_or(0.0),
            r["ssim"].as_f64().unwrap_or(0.0)
        );
        if let Some(t) = r["thumb"].as_str() {
            let _ = write!(
                h,
                "<figure><img src=\"thumbs/{t}.png\" loading=lazy><figcaption>pagebara | {} | difference</figcaption></figure>",
                reference(r)
            );
        }
        h += "</div>";
    }
    h
}

/// Deletes thumbnails the report does not show.
pub fn prune_thumbs(dir: &Path, keep: &HashSet<String>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let stem = name.strip_suffix(".png").unwrap_or(&name);
        if !keep.contains(stem) {
            let p = e.path();
            if p.is_dir() {
                let _ = std::fs::remove_dir_all(p);
            } else {
                let _ = std::fs::remove_file(p);
            }
        }
    }
}

fn which(name: &str) -> bool {
    std::env::var_os("PATH")
        .is_some_and(|p| std::env::split_paths(&p).any(|d| d.join(name).is_file()))
}
