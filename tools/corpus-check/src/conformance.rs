//! Checks `pagebara-conform`'s verdicts against the veraPDF corpus, whose file names say
//! whether each file should pass (`…-pass-…`) or fail (`…-fail-…`) its profile.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use pagebara_conform::Profile;
use serde_json::{Value, json};

use crate::child::{self, Exit, Limits};
use crate::walk;

/// The profile a corpus file tests, from its folder.
pub fn profile_of(path: &Path) -> Option<Profile> {
    let p = path.to_string_lossy();
    [
        ("/PDF_A-1b/", Profile::PdfA1b),
        ("/Isartor test files/PDFA-1b/", Profile::PdfA1b),
        ("/PDF_A-2b/", Profile::PdfA2b),
        ("/PDF_A-2u/", Profile::PdfA2u),
        ("/PDF_A-3b/", Profile::PdfA3b),
        ("/PDF_UA-1/", Profile::PdfUa1),
    ]
    .into_iter()
    .find(|(dir, _)| p.contains(dir))
    .map(|(_, prof)| prof)
}

/// `Some(true)` for a file that should pass, `Some(false)` for one that should fail.
pub fn expected(path: &Path) -> Option<bool> {
    let name = path.file_name()?.to_string_lossy().to_lowercase();
    if name.contains("-pass-") {
        Some(true)
    } else if name.contains("-fail-") {
        Some(false)
    } else {
        None
    }
}

/// The clause a file tests: `6-1-2` of `veraPDF test suite 6-1-2-t01-fail-a.pdf`.
pub fn clause(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let stem = name.split("-t").next().unwrap_or(&name);
    stem.rsplit(' ')
        .next()
        .unwrap_or(stem)
        .trim_start_matches("isartor-")
        .to_owned()
}

pub fn run(exe: &Path, root: &Path, out: &Path, jobs: usize) -> std::io::Result<String> {
    let items: Vec<(PathBuf, Profile, bool)> = walk::pdfs(&[root.to_path_buf()])
        .into_iter()
        .filter_map(|i| Some((i.path.clone(), profile_of(&i.path)?, expected(&i.path)?)))
        .collect();
    std::fs::create_dir_all(out)?;
    let mut sink = std::io::BufWriter::new(std::fs::File::create(out.join("conform.jsonl"))?);
    let mut results = Vec::new();
    child::parallel(
        &items,
        jobs,
        |(path, profile, want)| {
            let f = path.to_string_lossy().into_owned();
            let limits = Limits {
                timeout: std::time::Duration::from_secs(60),
                memory_mb: 4096,
            };
            let o = child::run(
                &exe.to_string_lossy(),
                &["conform-worker", &f, profile.name()],
                limits,
            );
            let line: Value = o
                .as_ref()
                .ok()
                .and_then(|o| o.stdout.lines().last())
                .and_then(|l| serde_json::from_str(l).ok())
                .unwrap_or(Value::Null);
            let verdict = match (&o, &line["ok"]) {
                (Ok(o), Value::Bool(true)) if o.exit == Exit::Code(0) => {
                    if line["errors"].as_u64() == Some(0) {
                        "pass"
                    } else {
                        "fail"
                    }
                }
                (Ok(o), _) if o.exit == Exit::Timeout => "timeout",
                _ => "error",
            };
            json!({
                "file": f,
                "profile": profile.name(),
                "clause": clause(path),
                "expected": if *want { "pass" } else { "fail" },
                "verdict": verdict,
                "rules": line["rules"],
                "error": line["error"],
            })
        },
        |_, r| {
            let _ = writeln!(sink, "{r}");
            results.push(r);
        },
    );
    sink.flush()?;
    let md = summary(&results);
    std::fs::write(out.join("conform.md"), &md)?;
    Ok(md)
}

pub fn summary(results: &[Value]) -> String {
    #[derive(Default)]
    struct Row {
        agree: usize,
        false_fail: usize,
        false_pass: usize,
        error: usize,
    }
    let mut by: BTreeMap<String, Row> = BTreeMap::new();
    let mut misses: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for r in results {
        let p = r["profile"].as_str().unwrap_or("?").to_owned();
        let (want, got) = (r["expected"].as_str(), r["verdict"].as_str());
        let row = by.entry(p.clone()).or_default();
        let kind = match got {
            _ if got == want => {
                row.agree += 1;
                continue;
            }
            Some("pass") => {
                row.false_pass += 1;
                "passed, should fail"
            }
            Some("fail") => {
                row.false_fail += 1;
                "failed, should pass"
            }
            _ => {
                row.error += 1;
                "error"
            }
        };
        let file = r["file"].as_str().unwrap_or("");
        let name = file.rsplit('/').next().unwrap_or(file);
        let detail = if kind == "failed, should pass" {
            format!("{name} ({})", r["rules"])
        } else {
            name.to_owned()
        };
        misses
            .entry((p, format!("{} {kind}", r["clause"].as_str().unwrap_or(""))))
            .or_default()
            .push(detail);
    }
    let mut s = String::from(
        "# Conformance against the veraPDF corpus\n\n| profile | files | agree | % | failed a passing file | passed a failing file | errors |\n|---|---|---|---|---|---|---|\n",
    );
    let mut all = Row::default();
    for (p, r) in &by {
        let n = r.agree + r.false_fail + r.false_pass + r.error;
        let _ = writeln!(
            s,
            "| {p} | {n} | {} | {:.1} | {} | {} | {} |",
            r.agree,
            100.0 * r.agree as f64 / n.max(1) as f64,
            r.false_fail,
            r.false_pass,
            r.error
        );
        all.agree += r.agree;
        all.false_fail += r.false_fail;
        all.false_pass += r.false_pass;
        all.error += r.error;
    }
    let n = all.agree + all.false_fail + all.false_pass + all.error;
    let _ = writeln!(
        s,
        "| all | {n} | {} | {:.1} | {} | {} | {} |",
        all.agree,
        100.0 * all.agree as f64 / n.max(1) as f64,
        all.false_fail,
        all.false_pass,
        all.error
    );
    let _ = writeln!(s, "\n## Disagreements by clause\n");
    for ((p, k), files) in &misses {
        let _ = writeln!(s, "- {p} {k}: {}", files.join(", "));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_give_profile_clause_and_verdict() {
        let p = Path::new("/c/PDF_A-2b/6.1 File structure/veraPDF test suite 6-1-2-t01-fail-a.pdf");
        assert_eq!(profile_of(p), Some(Profile::PdfA2b));
        assert_eq!(expected(p), Some(false));
        assert_eq!(clause(p), "6-1-2");
        let i = Path::new("/c/Isartor test files/PDFA-1b/6.9 Forms/isartor-6-9-t02-fail-b.pdf");
        assert_eq!(profile_of(i), Some(Profile::PdfA1b));
        assert_eq!(clause(i), "6-9");
        let u = Path::new("/c/PDF_UA-1/7.9 Notes/7.9-t02-pass-a.pdf");
        assert_eq!(profile_of(u), Some(Profile::PdfUa1));
        assert_eq!(expected(u), Some(true));
        assert_eq!(clause(u), "7.9");
        assert_eq!(profile_of(Path::new("/c/PDF_A-4/x-pass-a.pdf")), None);
    }
}
