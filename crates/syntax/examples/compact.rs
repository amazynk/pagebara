//! Rewrites a PDF with a full save, for checking the writer against other tools.
//!
//! ```text
//! compact IN OUT [--plain] [--password PW] [--protect USER OWNER] [--decrypt]
//! ```
//!
//! Object streams, stream compression and an xref stream are on unless `--plain` is given.

use pagebara_syntax::{Document, Error, Permissions, SaveOptions};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [input, output, flags @ ..] = &args[..] else {
        return Err(
            "usage: compact IN OUT [--plain] [--password PW] [--protect USER OWNER]".into(),
        );
    };
    let flag = |f: &str| flags.iter().position(|a| a == f);
    let password = flag("--password").and_then(|i| flags.get(i + 1));
    let mut doc = match Document::open(std::fs::read(input)?) {
        Ok(d) => d,
        Err(Error::PasswordRequired(l)) => l
            .unlock(password.map_or("", String::as_str))
            .map_err(|_| "wrong password")?,
        Err(e) => return Err(e.into()),
    };
    if let Some(pw) = password {
        doc.unlock_owner(pw);
    }
    if flag("--decrypt").is_some() {
        doc.remove_security()?;
    }
    if let Some(i) = flag("--protect") {
        let (Some(user), Some(owner)) = (flags.get(i + 1), flags.get(i + 2)) else {
            return Err("--protect USER OWNER".into());
        };
        doc.set_password(user, owner, Permissions::ALL)?;
    }
    let opts = if flag("--plain").is_some() {
        SaveOptions::default()
    } else {
        SaveOptions::compact()
    };
    let t = std::time::Instant::now();
    let out = doc.save_full_with(&opts)?;
    eprintln!(
        "{} -> {} bytes ({:+.1}%) in {:.0} ms",
        doc.source().len(),
        out.len(),
        100.0 * (out.len() as f64 / doc.source().len() as f64 - 1.0),
        t.elapsed().as_secs_f64() * 1e3
    );
    std::fs::write(output, out)?;
    Ok(())
}
