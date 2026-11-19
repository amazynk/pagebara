use std::path::PathBuf;
use std::time::Duration;

use corpus_check::{conformance, mutool, report, run, walk, worker};

const USAGE: &str = "usage:
  corpus-check run [--out DIR] [--jobs N] [--timeout S] [--mem MB] [--dpi N] [--no-mutool]
                   [--baseline OLD/results.jsonl] PATH...
      checks every *.pdf under PATH in a child process each: open, page count, text,
      annotations and forms, incremental and full save, lossless optimize, rendering of
      every page (also of the rewritten files, compared), and MuPDF's verdict;
      writes DIR/results.jsonl and DIR/report.md (default DIR: corpus-check-out)
  corpus-check summary RESULTS.jsonl [--baseline OLD.jsonl]
      prints the markdown report of an earlier run
  corpus-check conform [--out DIR] [--jobs N] VERAPDF_CORPUS
      compares conformance verdicts with the expected pass/fail of the file names
  corpus-check worker FILE [--dpi N] [--out-dir DIR]
  corpus-check conform-worker FILE PROFILE      (internal: one file)";

fn main() {
    std::process::exit(match real_main() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("corpus-check: {e}\n{USAGE}");
            2
        }
    });
}

struct Flags {
    rest: Vec<String>,
    opts: std::collections::HashMap<String, String>,
    switches: Vec<String>,
}

fn flags(args: impl Iterator<Item = String>) -> Flags {
    let mut f = Flags {
        rest: Vec::new(),
        opts: Default::default(),
        switches: Vec::new(),
    };
    let mut it = args.peekable();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--no-mutool" => f.switches.push(a),
            s if s.starts_with("--") => {
                let v = it.next().unwrap_or_default();
                f.opts.insert(s[2..].to_owned(), v);
            }
            _ => f.rest.push(a),
        }
    }
    f
}

impl Flags {
    fn num<T: std::str::FromStr>(&self, name: &str, default: T) -> Result<T, String> {
        self.opts.get(name).map_or(Ok(default), |v| {
            v.parse().map_err(|_| format!("--{name}: bad number {v}"))
        })
    }

    fn path(&self, name: &str) -> Option<PathBuf> {
        self.opts.get(name).map(PathBuf::from)
    }
}

fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(2, |n| (n.get() / 2).max(1))
}

fn real_main() -> Result<i32, String> {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().ok_or("no command")?;
    let f = flags(args);
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    match cmd.as_str() {
        "worker" => {
            let file = f.rest.first().ok_or("worker needs a file")?;
            let opts = worker::Options {
                dpi: f.num("dpi", 36.0)?,
                max_side: 1600.0,
                out_dir: f.path("out-dir"),
            };
            Ok(worker::run(file.as_ref(), &opts))
        }
        "conform-worker" => {
            let [file, profile] = &f.rest[..] else {
                return Err("conform-worker needs a file and a profile".into());
            };
            let profile = pagebara_conform::Profile::parse(profile)
                .ok_or_else(|| format!("unknown profile {profile}"))?;
            Ok(worker::conform(file.as_ref(), profile))
        }
        "conform" => {
            let root = f.rest.first().ok_or("conform needs the corpus folder")?;
            let out = f.path("out").unwrap_or_else(|| "corpus-check-out".into());
            let md = conformance::run(&exe, root.as_ref(), &out, f.num("jobs", default_jobs())?)
                .map_err(|e| e.to_string())?;
            print!("{md}");
            Ok(0)
        }
        "run" => {
            if f.rest.is_empty() {
                return Err("run needs at least one path".into());
            }
            let cfg = run::Config {
                exe,
                out: f.path("out").unwrap_or_else(|| "corpus-check-out".into()),
                jobs: f.num("jobs", default_jobs())?,
                timeout: Duration::from_secs(f.num("timeout", 60)?),
                memory_mb: f.num("mem", 4096)?,
                dpi: f.num("dpi", 36.0)?,
                mutool: !f.switches.iter().any(|s| s == "--no-mutool") && mutool::available(),
            };
            let roots: Vec<PathBuf> = f.rest.iter().map(PathBuf::from).collect();
            let items = walk::pdfs(&roots);
            eprintln!("{} files, {} jobs", items.len(), cfg.jobs);
            let results = run::run(&items, &cfg).map_err(|e| e.to_string())?;
            let baseline = match f.path("baseline") {
                Some(p) => Some(run::load(&p).map_err(|e| e.to_string())?),
                None => None,
            };
            let md = report::markdown(&results, baseline.as_deref());
            std::fs::write(cfg.out.join("report.md"), &md).map_err(|e| e.to_string())?;
            println!("{}", md.lines().take(20).collect::<Vec<_>>().join("\n"));
            Ok(0)
        }
        "summary" => {
            let p = f.rest.first().ok_or("summary needs results.jsonl")?;
            let results = run::load(p.as_ref()).map_err(|e| e.to_string())?;
            let baseline = match f.path("baseline") {
                Some(p) => Some(run::load(&p).map_err(|e| e.to_string())?),
                None => None,
            };
            print!("{}", report::markdown(&results, baseline.as_deref()));
            Ok(0)
        }
        _ => Err(format!("unknown command {cmd}")),
    }
}
