//! The per-file work, run in a child process: every step prints one JSON line as soon as
//! it finishes, so a crash or hang still tells the runner how far the file got.

use std::io::Write;
use std::path::Path;
use std::time::Instant;

use pagebara_content::{Interpreter, extract_text};
use pagebara_pages::PageTree;
use pagebara_render::Renderer;
use pagebara_syntax::{Document, Error, Object, SaveOptions};
use serde_json::{Value, json};

use crate::engine::{self, OpenError};
use crate::guard;

#[derive(Debug, Clone)]
pub struct Options {
    pub dpi: f32,
    pub max_side: f32,
    /// Where to write the rewritten and optimized files for the runner to check.
    pub out_dir: Option<std::path::PathBuf>,
}

/// Renders that differ by more than this mean per channel count as changed.
const MAX_DIFF: f64 = 1.5;

/// Prints one step's line; returns the step's value.
fn step<T>(name: &str, f: impl FnOnce() -> Result<(T, Value), String>) -> Option<T> {
    // A line before the work too, so a hang or crash names its step.
    let _ = writeln!(std::io::stdout().lock(), "{}", json!({ "step": name }));
    let start = Instant::now();
    let r = guard::catch(f).and_then(|r| r);
    let mut line = json!({ "step": name });
    let out = match r {
        Ok((t, v)) => {
            line["ok"] = true.into();
            if let (Some(l), Value::Object(m)) = (line.as_object_mut(), v) {
                l.extend(m);
            }
            Some(t)
        }
        Err(e) => {
            line["ok"] = false.into();
            line["error"] = e.into();
            None
        }
    };
    line["ms"] = (start.elapsed().as_millis() as u64).into();
    let panics = guard::take();
    if !panics.is_empty() {
        line["panics"] = panics.into();
    }
    let mut so = std::io::stdout().lock();
    let _ = writeln!(so, "{line}");
    let _ = so.flush();
    out
}

fn reopen(bytes: Vec<u8>) -> Result<(Document, usize), String> {
    let doc = engine::open(bytes).map_err(|e| format!("reopen: {e:?}"))?;
    let n = PageTree::open(&doc).len();
    Ok((doc, n))
}

pub fn run(path: &Path, opts: &Options) -> i32 {
    guard::install();
    let Some(bytes) = step("read", || {
        let b = std::fs::read(path).map_err(|e| e.to_string())?;
        let n = b.len();
        Ok((b, json!({ "bytes": n })))
    }) else {
        return 1;
    };
    let Some(Some(doc)) = step("open", || match engine::open(bytes) {
        Ok(d) => {
            let (a, b) = d.version();
            Ok((Some(d), json!({ "version": format!("{a}.{b}") })))
        }
        Err(OpenError::Password) => Ok((None, json!({ "password": true }))),
        Err(OpenError::Other(e)) => Err(e),
    }) else {
        return 0;
    };
    let Some(pages) = step("pages", || {
        let t = PageTree::open(&doc);
        let n = t.len();
        Ok((n, json!({ "pages": n, "repaired": t.repaired() })))
    }) else {
        return 0;
    };

    step("text", || {
        let tree = PageTree::open(&doc);
        let mut interp = Interpreter::new(&doc);
        let mut chars = 0;
        for p in tree.pages() {
            let Some(o) = doc.get(p.r) else { continue };
            let Some(d) = o.as_dict() else { continue };
            let res = engine::resources(&doc, d);
            chars += extract_text(&doc, &interp.page(d, &res))
                .text
                .chars()
                .count();
        }
        Ok(((), json!({ "chars": chars })))
    });

    step("annots", || {
        let tree = PageTree::open(&doc);
        let mut annots = 0;
        for p in tree.pages() {
            if let Some(o) = doc.get(p.r)
                && let Some(d) = o.as_dict()
            {
                annots += pagebara_annot::annotations(&doc, d).len();
            }
        }
        let form = pagebara_forms::Form::load(&doc);
        Ok(((), json!({ "annots": annots, "fields": form.fields.len() })))
    });

    step("save-incremental", || {
        let mut d = doc.clone();
        let root = d.trailer().get(b"Root").and_then(Object::as_objref);
        if let Some(r) = root
            && let Some(cat) = d.get(r).and_then(|o| o.as_dict().cloned())
        {
            let mut cat = cat;
            cat.set(b"PagebaraCheck", Object::Bool(true));
            d.set(r, cat);
        }
        match d.save_incremental() {
            Ok(b) => {
                let (_, n) = reopen(b)?;
                check_pages(pages, n)?;
                Ok(((), json!({})))
            }
            Err(Error::NeedsFullSave) => Ok(((), json!({ "needs_full": true }))),
            Err(e) => Err(e.to_string()),
        }
    });

    let full = step("save-full", || {
        let plain = doc.save_full().map_err(|e| e.to_string())?;
        write_out(opts, "full.pdf", &plain);
        check_pages(pages, reopen(plain)?.1)?;
        let compact = doc
            .save_full_with(&SaveOptions::compact())
            .map_err(|e| e.to_string())?;
        write_out(opts, "compact.pdf", &compact);
        let (d, n) = reopen(compact.clone())?;
        check_pages(pages, n)?;
        Ok((d, json!({ "bytes": compact.len() })))
    });

    let optimized = step("optimize", || {
        let o = pagebara_optimize::optimize(&doc, &pagebara_optimize::Options::default(), None)
            .map_err(|e| format!("{e:?}"))?;
        write_out(opts, "optimized.pdf", &o.bytes);
        let (d, n) = reopen(o.bytes.clone())?;
        check_pages(pages, n)?;
        Ok((d, json!({ "bytes": o.bytes.len() })))
    });

    step("render", || {
        let r = Renderer::new(doc.clone());
        let rf = full.map(Renderer::new);
        let ro = optimized.map(Renderer::new);
        let (mut errors, mut worst_full, mut worst_opt) = (0, (0.0f64, 0), (0.0f64, 0));
        for i in 0..pages {
            let img = match engine::render_page(&r, i, opts.dpi, opts.max_side) {
                Ok(img) => img,
                Err(_) => {
                    errors += 1;
                    continue;
                }
            };
            for (other, worst) in [(&rf, &mut worst_full), (&ro, &mut worst_opt)] {
                let Some(other) = other else { continue };
                let d = engine::render_page(other, i, opts.dpi, opts.max_side)
                    .ok()
                    .and_then(|b| engine::mean_abs_diff(&img, &b))
                    .unwrap_or(255.0);
                if d > worst.0 {
                    *worst = (d, i + 1);
                }
            }
        }
        let v = json!({
            "errors": errors,
            "full_diff": worst_full.0, "full_page": worst_full.1,
            "opt_diff": worst_opt.0, "opt_page": worst_opt.1,
            "changed": worst_full.0 > MAX_DIFF || worst_opt.0 > MAX_DIFF,
        });
        Ok(((), v))
    });
    0
}

fn check_pages(want: usize, got: usize) -> Result<(), String> {
    if want == got {
        Ok(())
    } else {
        Err(format!("{got} pages after saving, {want} before"))
    }
}

fn write_out(opts: &Options, name: &str, bytes: &[u8]) {
    if let Some(d) = &opts.out_dir {
        let _ = std::fs::write(d.join(name), bytes);
    }
}

/// Conformance worker: prints the verdict of `profile` for one file.
pub fn conform(path: &Path, profile: pagebara_conform::Profile) -> i32 {
    guard::install();
    step("conform", || {
        let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
        let doc = engine::open(bytes).map_err(|e| format!("{e:?}"))?;
        let report = pagebara_conform::validate(&doc, profile);
        let mut rules: Vec<&str> = report
            .issues
            .iter()
            .filter(|i| i.severity == pagebara_conform::Severity::Error)
            .map(|i| i.rule.as_str())
            .collect();
        rules.sort_unstable();
        rules.dedup();
        let errors = rules.len();
        Ok(((), json!({ "errors": errors, "rules": rules })))
    });
    0
}
