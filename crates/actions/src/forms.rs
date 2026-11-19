//! Form tools on the command line: field values to FDF or XFDF and back, and field
//! detection on printed forms.

use std::path::{Path, PathBuf};

use pagebara_forms::Form;
use serde_json::{Value, json};

use crate::fill::is_form_data;
use crate::ops::{open, save};
use crate::out::{parent, stem, write_output_as};
use crate::pro::{Args, Common, ProReport, finish};

fn write(
    c: &Common,
    inputs: usize,
    input: &Path,
    base: &str,
    ext: &str,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let given = c.output.as_deref().filter(|p| inputs == 1 || p.is_dir());
    write_output_as(given, &parent(input), base, ext, bytes)
        .map_err(|e| format!("cannot write: {e}"))
}

/// `fdf-export` and `xfdf-export`: each form's values next to it.
pub(crate) fn export(name: &str, mut a: Args, c: &Common, cwd: &Path) -> Result<ProReport, String> {
    let annotations = a.flag("--annotations");
    let files = a.files(cwd)?;
    if files.is_empty() {
        return Err("no files given".into());
    }
    let xml = name == "xfdf-export";
    let mut r = ProReport::default();
    let mut items = Vec::new();
    for p in &files {
        let res = open(p, c.password.as_deref()).and_then(|d| {
            let form = Form::load(&d);
            if !form.has_form() {
                return Err("the file has no form".into());
            }
            let file = p.file_name().map(|f| f.to_string_lossy().into_owned());
            let (bytes, ext) = if xml {
                (
                    pagebara_fdf::export_xfdf(&form, &d, file.as_deref(), annotations).into_bytes(),
                    "xfdf",
                )
            } else {
                (pagebara_fdf::export_fdf(&form, file.as_deref()), "fdf")
            };
            let o = write(c, files.len(), p, &stem(p), ext, &bytes)?;
            Ok((o, form.fields.len()))
        });
        match res {
            Ok((o, n)) => {
                items.push(json!({"input": p, "output": o, "fields": n}));
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

/// `fdf-import` and `xfdf-import`: the data filled into a copy of the form.
pub(crate) fn import(name: &str, mut a: Args, c: &Common, cwd: &Path) -> Result<ProReport, String> {
    let data = a.value("--data")?.map(|d| cwd.join(d));
    let files = a.files(cwd)?;
    let (data, forms): (Option<PathBuf>, Vec<PathBuf>) = match data {
        Some(d) => (Some(d), files),
        None => {
            let (d, f): (Vec<PathBuf>, Vec<PathBuf>) =
                files.into_iter().partition(|p| is_form_data(p));
            (d.into_iter().next(), f)
        }
    };
    let data = data.ok_or("no form data given (an .fdf or .xfdf file, or --data FILE)")?;
    let bytes = std::fs::read(&data).map_err(|e| format!("{}: {e}", data.display()))?;
    let form_file = match forms.as_slice() {
        [one] => one.clone(),
        [] => crate::fill::form_target(&data, &bytes).ok_or("no PDF form given")?,
        _ => return Err("give one PDF form".into()),
    };
    let mut doc = open(&form_file, c.password.as_deref())?;
    let mut form = Form::load(&doc);
    let report = if name == "xfdf-import" {
        pagebara_fdf::import_xfdf(&mut form, &mut doc, &String::from_utf8_lossy(&bytes))
    } else {
        pagebara_fdf::import_fdf(&mut form, &mut doc, &bytes)
    }
    .map_err(|e| format!("cannot fill the form: {e}"))?;
    if report.change.fields.is_empty() && report.annotations == 0 {
        return Err("the data has no values for this form".into());
    }
    let out = save(&doc)?;
    let o = write(
        c,
        1,
        &form_file,
        &format!("{}-filled", stem(&form_file)),
        "pdf",
        &out,
    )?;
    let mut r = ProReport::default();
    for (n, e) in &report.rejected {
        r.messages.push(format!("{n}: {e}"));
    }
    if !report.unknown.is_empty() {
        r.messages
            .push(format!("not in the form: {}", report.unknown.join(", ")));
    }
    r.outputs.push(o.clone());
    let v = json!({
        "input": form_file,
        "data": data,
        "output": o,
        "filled": report.change.fields,
        "unknown": report.unknown,
        "rejected": report.rejected.iter().map(|(n, e)| json!({"field": n, "error": e.to_string()})).collect::<Vec<_>>(),
        "annotations": report.annotations,
    });
    Ok(finish(c, r, v))
}

fn suggestion_json(s: &pagebara_formdetect::Suggestion) -> Value {
    json!({
        "page": s.page + 1,
        "name": s.name,
        "type": s.kind.as_str(),
        "rect": [s.rect.x0, s.rect.y0, s.rect.x1, s.rect.y1],
        "label": s.label,
        "comb": s.comb,
        "multiline": s.multiline,
        "date": s.date,
    })
}

/// `detect-fields`: the fields found on each file's pages added to a copy (or listed).
pub(crate) fn detect(mut a: Args, c: &Common, cwd: &Path) -> Result<ProReport, String> {
    let list_only = a.flag("--list");
    let files = a.files(cwd)?;
    if files.is_empty() {
        return Err("no files given".into());
    }
    let mut r = ProReport::default();
    let mut items = Vec::new();
    for p in &files {
        let res = open(p, c.password.as_deref()).and_then(|mut d| {
            let found = pagebara_formdetect::detect_all(&d);
            let v: Vec<Value> = found.iter().map(suggestion_json).collect();
            if list_only {
                for s in &found {
                    r.messages.push(format!(
                        "page {}: {} {:?} at {:.0},{:.0} {:.0}×{:.0}",
                        s.page + 1,
                        s.kind.as_str(),
                        s.name,
                        s.rect.x0,
                        s.rect.y0,
                        s.rect.width(),
                        s.rect.height()
                    ));
                }
                return Ok((None, v));
            }
            if found.is_empty() {
                return Err("no fields found".into());
            }
            let mut form = Form::load(&d);
            pagebara_formdetect::accept(&mut form, &mut d, &found).map_err(|e| e.to_string())?;
            let bytes = save(&d)?;
            let o = write(
                c,
                files.len(),
                p,
                &format!("{}-fields", stem(p)),
                "pdf",
                &bytes,
            )?;
            Ok((Some(o), v))
        });
        match res {
            Ok((o, v)) => {
                items.push(json!({"input": p, "output": o, "fields": v}));
                r.outputs.extend(o);
            }
            Err(e) => {
                items.push(json!({"input": p, "error": e}));
                r.messages.push(format!("{}: {e}", p.display()));
            }
        }
    }
    if r.outputs.is_empty() && !list_only {
        return Err(r.messages.join("\n"));
    }
    Ok(finish(c, r, json!({ "files": items })))
}
