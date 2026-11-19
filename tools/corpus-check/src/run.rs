//! The runner: one worker process per file, classified into a JSON record.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::child::{self, Exit, Limits};
use crate::mutool;
use crate::walk::Item;

#[derive(Debug, Clone)]
pub struct Config {
    /// This program, run as `worker` for each file.
    pub exe: PathBuf,
    pub out: PathBuf,
    pub jobs: usize,
    pub timeout: Duration,
    pub memory_mb: u64,
    pub dpi: f32,
    pub mutool: bool,
}

/// Outcomes from worst to best; a file gets the first that applies.
pub const STATUSES: [&str; 10] = [
    "crash",
    "timeout",
    "oom",
    "panic",
    "not-opened",
    "step-error",
    "invalid-output",
    "caught-panic",
    "password",
    "ok",
];

/// Checks every item, appending one JSON line per file to `out/results.jsonl`.
pub fn run(items: &[Item], cfg: &Config) -> std::io::Result<Vec<Value>> {
    std::fs::create_dir_all(cfg.out.join("tmp"))?;
    let mut sink = std::io::BufWriter::new(std::fs::File::create(cfg.out.join("results.jsonl"))?);
    let mut results = Vec::with_capacity(items.len());
    let total = items.len();
    let mut done = 0;
    child::parallel(
        items,
        cfg.jobs,
        |item| check(item, cfg),
        |_, rec| {
            done += 1;
            let st = rec["status"].as_str().unwrap_or("?");
            if st != "ok" && st != "password" {
                eprintln!(
                    "[{done}/{total}] {st}: {}",
                    rec["file"].as_str().unwrap_or("")
                );
            } else if done % 200 == 0 {
                eprintln!("[{done}/{total}]");
            }
            let _ = writeln!(sink, "{rec}");
            results.push(rec);
        },
    );
    sink.flush()?;
    let _ = std::fs::remove_dir_all(cfg.out.join("tmp"));
    Ok(results)
}

fn check(item: &Item, cfg: &Config) -> Value {
    let tmp = cfg.out.join("tmp").join(format!(
        "{:016x}",
        hash(item.path.to_string_lossy().as_bytes())
    ));
    let _ = std::fs::create_dir_all(&tmp);
    let file = item.path.to_string_lossy().into_owned();
    let dpi = cfg.dpi.to_string();
    let tmp_s = tmp.to_string_lossy().into_owned();
    let args = ["worker", &file, "--dpi", &dpi, "--out-dir", &tmp_s];
    let limits = Limits {
        timeout: cfg.timeout,
        memory_mb: cfg.memory_mb,
    };
    let mut rec = json!({
        "file": file,
        "corpus": item.corpus,
        "bytes": std::fs::metadata(&item.path).map(|m| m.len()).unwrap_or(0),
    });
    let outcome = child::run(&cfg.exe.to_string_lossy(), &args, limits);
    let mut steps = Map::new();
    let mut panics: Vec<Value> = Vec::new();
    let mut last = None;
    let status = match &outcome {
        Err(e) => {
            rec["error"] = e.to_string().into();
            "crash"
        }
        Ok(o) => {
            for line in o.stdout.lines() {
                let Ok(Value::Object(mut v)) = serde_json::from_str::<Value>(line) else {
                    continue;
                };
                if let Some(Value::Array(p)) = v.remove("panics") {
                    panics.extend(p);
                }
                if let Some(Value::String(name)) = v.remove("step") {
                    last = Some(name.clone());
                    steps.insert(name, Value::Object(v));
                }
            }
            rec["seconds"] = ((o.seconds * 100.0).round() / 100.0).into();
            classify(o, &steps, !panics.is_empty())
        }
    };
    if let Ok(o) = &outcome
        && matches!(status, "crash" | "oom")
    {
        let tail: String = o.stderr.chars().rev().take(2000).collect();
        rec["stderr"] = tail.chars().rev().collect::<String>().into();
        if let Exit::Signal(s) = o.exit {
            rec["signal"] = s.into();
        }
    }
    rec["last_step"] = last.into();
    rec["steps"] = Value::Object(steps);
    if !panics.is_empty() {
        rec["panics"] = panics.into();
    }
    let mut status = status.to_owned();
    if cfg.mutool {
        let theirs = mutool::pages(&item.path);
        rec["mutool"] = theirs.to_json();
        // Our rewritten files must read as well in MuPDF as the original did.
        let mut worse = Vec::new();
        for name in ["compact.pdf", "optimized.pdf"] {
            let p = tmp.join(name);
            if !p.exists() {
                continue;
            }
            let ours = mutool::pages(&p);
            if theirs.opened() && (ours.pages != theirs.pages || ours.errors > theirs.errors) {
                worse.push(json!({ "file": name, "mutool": ours.to_json() }));
            }
        }
        if !worse.is_empty() {
            rec["output_rejected"] = worse.into();
            if rank(&status) > rank("invalid-output") {
                status = "invalid-output".into();
            }
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    rec["status"] = status.into();
    rec
}

pub fn rank(status: &str) -> usize {
    STATUSES
        .iter()
        .position(|s| *s == status)
        .unwrap_or(STATUSES.len())
}

fn classify(o: &child::Outcome, steps: &Map<String, Value>, panicked: bool) -> &'static str {
    let failed = |name: &str| steps.get(name).is_some_and(|s| s["ok"] == false);
    let error_of = |s: &Value| s["error"].as_str().unwrap_or("").to_owned();
    match o.exit {
        Exit::Timeout => return "timeout",
        _ if o.out_of_memory() => return "oom",
        Exit::Signal(_) => return "crash",
        Exit::Code(c) if c != 0 => return "crash",
        _ => {}
    }
    if steps.values().any(|s| error_of(s).starts_with("panic")) {
        return "panic";
    }
    if steps.get("open").is_some_and(|s| s["password"] == true) {
        return "password";
    }
    if failed("open") || failed("read") || !steps.contains_key("pages") {
        return "not-opened";
    }
    if steps.values().any(|s| s["ok"] == false) {
        return "step-error";
    }
    if steps
        .get("render")
        .is_some_and(|s| s["changed"] == true || s["errors"].as_u64().unwrap_or(0) > 0)
    {
        return "invalid-output";
    }
    if panicked {
        return "caught-panic";
    }
    "ok"
}

fn hash(b: &[u8]) -> u64 {
    // FNV-1a: only needs to keep temporary directory names apart.
    b.iter().fold(0xcbf29ce484222325, |h, &c| {
        (h ^ u64::from(c)).wrapping_mul(0x100000001b3)
    })
}

/// Reads a `results.jsonl`.
pub fn load(path: &Path) -> std::io::Result<Vec<Value>> {
    Ok(std::fs::read_to_string(path)?
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .collect())
}
