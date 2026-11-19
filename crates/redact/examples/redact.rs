//! Redacts a PDF from the command line and verifies the result.
//!
//! ```text
//! redact IN.pdf OUT.pdf [--text S]... [--regex RE]... [--email] [--phone] [--iban]
//!        [--card] [--rect PAGE,X0,Y0,X1,Y1]... [--overlay TEXT] [--comments]
//!        [--password PW]
//! ```
//!
//! Marks every match (and rectangle, pages counted from 1, in points from the bottom
//! left), applies the marks already in the file too, sanitises, saves a full rewrite and
//! verifies it. Exits with 1 when verification finds anything.

use std::process::ExitCode;

use pagebara_redact::{
    ApplyOptions, Kind, Mark, Pattern, Rect, Sanitize, add_mark, apply, find, sanitize, save,
    verify,
};
use pagebara_syntax::Document;

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<bool, Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(input), Some(output)) = (args.next(), args.next()) else {
        return Err(
            "usage: redact IN.pdf OUT.pdf [--text S] [--regex RE] [--email] [--phone] \
                    [--iban] [--card] [--rect PAGE,X0,Y0,X1,Y1] [--overlay TEXT] [--comments] \
                    [--password PW]"
                .into(),
        );
    };
    let mut patterns = Vec::new();
    let mut secrets: Vec<String> = Vec::new();
    let mut rects = Vec::new();
    let mut overlay = None;
    let mut clean = Sanitize::default();
    let mut password = None;
    while let Some(a) = args.next() {
        let mut value = || args.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--text" => {
                let v = value()?;
                patterns.push(Pattern::text(&v));
                secrets.push(v);
            }
            "--regex" => patterns.push(Pattern::regex(&value()?)?),
            "--email" => patterns.push(Pattern::Kind(Kind::Email)),
            "--phone" => patterns.push(Pattern::Kind(Kind::Phone)),
            "--iban" => patterns.push(Pattern::Kind(Kind::Iban)),
            "--card" => patterns.push(Pattern::Kind(Kind::CreditCard)),
            "--rect" => {
                let v: Vec<f64> = value()?
                    .split(',')
                    .map(str::parse)
                    .collect::<Result<_, _>>()?;
                let [p, x0, y0, x1, y1] = v[..] else {
                    return Err("--rect takes PAGE,X0,Y0,X1,Y1".into());
                };
                rects.push(((p as usize).saturating_sub(1), Rect::new(x0, y0, x1, y1)));
            }
            "--overlay" => overlay = Some(value()?),
            "--comments" => clean.comments = true,
            "--password" => password = Some(value()?),
            _ => return Err(format!("unknown option {a}").into()),
        }
    }
    let bytes = std::fs::read(&input)?;
    let mut doc = match Document::open(bytes) {
        Err(pagebara_syntax::Error::PasswordRequired(locked)) => {
            let pw = password.as_deref().ok_or("the file needs --password")?;
            locked.unlock(pw).map_err(|_| "wrong password")?
        }
        d => d?,
    };
    let pages = pagebara_pages::PageTree::load(&doc);
    let style = |m: Mark| Mark {
        overlay_text: overlay.clone(),
        ..m
    };
    let mut marked = 0;
    for p in &patterns {
        for hit in find(&doc, p, None) {
            println!("page {}: {:?}", hit.page + 1, hit.text);
            if !matches!(p, Pattern::Text { .. }) {
                secrets.push(hit.text.clone());
            }
            add_mark(&mut doc, pages.pages()[hit.page].r, &style(hit.mark()))?;
            marked += 1;
        }
    }
    for (page, r) in rects {
        let p = pages.pages().get(page).ok_or("no such page")?;
        add_mark(&mut doc, p.r, &style(Mark::rect(r)))?;
        marked += 1;
    }
    let report = apply(&mut doc, &ApplyOptions::default())?;
    let s = sanitize(&mut doc, &clean);
    let out = save(&doc)?;
    std::fs::write(&output, &out)?;
    let st = report.stats();
    println!(
        "{marked} new marks on {} pages: {} glyphs, {} paths, {} images blanked ({} pixels), \
         {} images removed, {} forms copied, {} annotations removed, {} fields cleared",
        report.pages.len(),
        st.glyphs_removed,
        st.paths_changed,
        st.images_blanked,
        st.pixels_blanked,
        st.images_removed,
        st.forms_copied,
        report
            .pages
            .iter()
            .map(|p| p.annotations_removed)
            .sum::<usize>(),
        report
            .pages
            .iter()
            .map(|p| p.fields_cleared.len())
            .sum::<usize>(),
    );
    println!("sanitised: {s:?}");
    for w in &report.warnings {
        println!("warning: {w}");
    }
    let secrets: Vec<&str> = secrets.iter().map(String::as_str).collect();
    let v = verify(&out, password.as_deref(), &report.regions(), &secrets);
    if v.is_clean() {
        println!("verified: nothing left under the marks, secrets absent, one revision");
    } else {
        println!("VERIFICATION FAILED: {v:#?}");
    }
    Ok(v.is_clean())
}
