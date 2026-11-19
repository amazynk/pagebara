//! The markdown summary of a run, optionally against a baseline run.

use std::collections::BTreeMap;
use std::fmt::Write;

use serde_json::Value;

use crate::run::{STATUSES, rank};

fn status(r: &Value) -> &str {
    r["status"].as_str().unwrap_or("?")
}

fn counts(results: &[Value]) -> BTreeMap<String, BTreeMap<&str, usize>> {
    let mut by: BTreeMap<String, BTreeMap<&str, usize>> = BTreeMap::new();
    for r in results {
        let c = r["corpus"].as_str().unwrap_or("?").to_owned();
        *by.entry(c).or_default().entry(status(r)).or_default() += 1;
        *by.entry("(all)".into())
            .or_default()
            .entry(status(r))
            .or_default() += 1;
    }
    by
}

fn mutool_opened(r: &Value) -> bool {
    r["mutool"]["pages"].as_u64().unwrap_or(0) > 0
}

/// A panic message without its variable parts, for grouping.
fn panic_key(p: &str) -> String {
    let (msg, at) = p.rsplit_once(" @ ").unwrap_or((p, ""));
    let msg: String = msg
        .chars()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .take(90)
        .collect();
    format!("{at}: {msg}")
}

fn short(file: &str) -> &str {
    file.rsplit_once("/corpora/").map_or(file, |(_, f)| f)
}

pub fn markdown(results: &[Value], baseline: Option<&[Value]>) -> String {
    let mut s = String::new();
    let _ = writeln!(s, "# Corpus check\n\n{} files.\n", results.len());
    table(&mut s, &counts(results));
    if let Some(b) = baseline {
        let _ = writeln!(s, "\n## Baseline\n\n{} files.\n", b.len());
        table(&mut s, &counts(b));
        let old: BTreeMap<&str, &str> = b
            .iter()
            .map(|r| (r["file"].as_str().unwrap_or(""), status(r)))
            .collect();
        let mut changed: Vec<String> = Vec::new();
        for r in results {
            let f = r["file"].as_str().unwrap_or("");
            if let Some(&o) = old.get(f)
                && o != status(r)
            {
                let dir = if rank(status(r)) > rank(o) {
                    "better"
                } else {
                    "WORSE"
                };
                changed.push(format!("| {dir} | {o} | {} | {} |", status(r), short(f)));
            }
        }
        changed.sort();
        let _ = writeln!(
            s,
            "\n### Changed files\n\n| | before | after | file |\n|---|---|---|---|"
        );
        for c in changed {
            let _ = writeln!(s, "{c}");
        }
    }

    let fails: Vec<&Value> = results
        .iter()
        .filter(|r| rank(status(r)) <= rank("not-opened") && mutool_opened(r))
        .collect();
    let _ = writeln!(
        s,
        "\n## We fail where MuPDF succeeds ({})\n\n| status | last step | file | detail |\n|---|---|---|---|",
        fails.len()
    );
    for r in fails {
        let detail = r["steps"]
            .as_object()
            .and_then(|m| m.values().find_map(|v| v["error"].as_str()))
            .or_else(|| r["panics"][0].as_str())
            .unwrap_or("");
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} |",
            status(r),
            r["last_step"].as_str().unwrap_or(""),
            short(r["file"].as_str().unwrap_or("")),
            detail
                .replace('|', "/")
                .chars()
                .take(160)
                .collect::<String>()
        );
    }

    let mut panics: BTreeMap<String, (usize, &str)> = BTreeMap::new();
    for r in results {
        for p in r["panics"].as_array().into_iter().flatten() {
            let e = panics
                .entry(panic_key(p.as_str().unwrap_or("")))
                .or_insert((0, r["file"].as_str().unwrap_or("")));
            e.0 += 1;
        }
    }
    let _ = writeln!(
        s,
        "\n## Panics by place ({})\n\n| files | place: message | example |\n|---|---|---|",
        panics.len()
    );
    for (k, (n, f)) in &panics {
        let _ = writeln!(s, "| {n} | {} | {} |", k.replace('|', "/"), short(f));
    }

    let mut errors: BTreeMap<String, (usize, &str)> = BTreeMap::new();
    for r in results {
        for (name, st) in r["steps"].as_object().into_iter().flatten() {
            if let Some(e) = st["error"].as_str()
                && name != "open"
                && !e.starts_with("panic")
            {
                let k = format!("{name}: {}", e.chars().take(100).collect::<String>());
                errors
                    .entry(k)
                    .or_insert((0, r["file"].as_str().unwrap_or("")))
                    .0 += 1;
            }
        }
    }
    let _ = writeln!(
        s,
        "\n## Step errors ({})\n\n| files | step: error | example |\n|---|---|---|",
        errors.len()
    );
    for (k, (n, f)) in &errors {
        let _ = writeln!(s, "| {n} | {} | {} |", k.replace('|', "/"), short(f));
    }

    let _ = writeln!(
        s,
        "\n## Invalid output and page-count differences\n\n| file | ours | MuPDF | detail |\n|---|---|---|---|"
    );
    for r in results {
        let ours = r["steps"]["pages"]["pages"].as_u64();
        let theirs = r["mutool"]["pages"].as_u64().unwrap_or(0);
        let render = &r["steps"]["render"];
        let mut detail = String::new();
        if render["changed"] == true {
            let _ = write!(
                detail,
                "re-rendered differently: full save {:.1} (p{}), optimized {:.1} (p{}); ",
                render["full_diff"].as_f64().unwrap_or(0.0),
                render["full_page"],
                render["opt_diff"].as_f64().unwrap_or(0.0),
                render["opt_page"]
            );
        }
        if render["errors"].as_u64().unwrap_or(0) > 0 {
            let _ = write!(detail, "{} pages failed to render; ", render["errors"]);
        }
        if let Some(o) = r["output_rejected"].as_array() {
            let _ = write!(detail, "MuPDF on our output: {}; ", Value::Array(o.clone()));
        }
        let pages_differ = theirs > 0 && ours.is_some_and(|o| o != theirs);
        if detail.is_empty() && !pages_differ {
            continue;
        }
        let _ = writeln!(
            s,
            "| {} | {} | {} | {} |",
            short(r["file"].as_str().unwrap_or("")),
            ours.map_or("-".into(), |o| o.to_string()),
            theirs,
            detail
        );
    }

    let mut slow: Vec<&Value> = results.iter().collect();
    slow.sort_by(|a, b| {
        b["seconds"]
            .as_f64()
            .unwrap_or(0.0)
            .total_cmp(&a["seconds"].as_f64().unwrap_or(0.0))
    });
    let _ = writeln!(s, "\n## Slowest\n\n| s | file |\n|---|---|");
    for r in slow.iter().take(15) {
        let _ = writeln!(
            s,
            "| {} | {} |",
            r["seconds"],
            short(r["file"].as_str().unwrap_or(""))
        );
    }
    s
}

fn table(s: &mut String, by: &BTreeMap<String, BTreeMap<&str, usize>>) {
    let _ = writeln!(s, "| corpus | {} |", STATUSES.join(" | "));
    let _ = writeln!(s, "|---{}|", "|---".repeat(STATUSES.len()));
    for (c, m) in by {
        let row: Vec<String> = STATUSES
            .iter()
            .map(|st| m.get(st).copied().unwrap_or(0).to_string())
            .collect();
        let _ = writeln!(s, "| {c} | {} |", row.join(" | "));
    }
}
