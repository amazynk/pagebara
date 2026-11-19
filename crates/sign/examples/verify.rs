//! Verifies every signature of a PDF.
//!
//! ```text
//! cargo run -p pagebara-sign --example verify -- file.pdf [--trust roots.pem] [--no-system]
//!     [--password PW]
//! ```

use pagebara_sign::{Status, TrustSource, TrustStore, VerifyOptions, verify};
use pagebara_syntax::Document;

fn main() {
    let mut args = std::env::args().skip(1);
    let mut file = None;
    let mut trust_files = Vec::new();
    let mut system = true;
    let mut password = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--trust" => trust_files.extend(args.next()),
            "--no-system" => system = false,
            "--password" => password = args.next(),
            _ => file = Some(a),
        }
    }
    let Some(file) = file else {
        eprintln!("usage: verify FILE.pdf [--trust roots.pem] [--no-system] [--password PW]");
        std::process::exit(2);
    };
    let mut trust = if system {
        TrustStore::system()
    } else {
        TrustStore::new()
    };
    for t in &trust_files {
        let pem = std::fs::read(t).unwrap_or_else(|e| fail(&format!("{t}: {e}")));
        let n = trust
            .add_pem(&pem, TrustSource::User)
            .unwrap_or_else(|e| fail(&format!("{t}: {e}")));
        println!("trusting {n} certificate(s) from {t}");
    }
    println!("{} trust anchor(s)", trust.len());
    let bytes = std::fs::read(&file).unwrap_or_else(|e| fail(&format!("{file}: {e}")));
    let doc = match Document::open(bytes) {
        Ok(d) => d,
        Err(pagebara_syntax::Error::PasswordRequired(locked)) => locked
            .unlock(password.as_deref().unwrap_or_default())
            .unwrap_or_else(|_| fail("wrong or missing password")),
        Err(e) => fail(&e.to_string()),
    };
    let opts = VerifyOptions {
        password: password.as_deref(),
        ..VerifyOptions::new(&trust)
    };
    let reports = verify(&doc, &opts);
    if reports.is_empty() {
        println!("no signatures");
    }
    let mut ok = true;
    for r in &reports {
        ok &= matches!(r.status, Status::ValidTrusted | Status::ValidUntrusted);
        println!("\n{}", r.summary());
        println!("  field:      {}", r.info.field);
        println!("  status:     {:?}", r.status);
        println!(
            "  format:     {}",
            r.info.sub_filter.as_deref().unwrap_or("?")
        );
        if let Some(a) = &r.algorithm {
            println!("  algorithm:  {a}");
        }
        if let Some(t) = r.signing_time {
            println!("  signed at:  {t} ({:?})", r.time_source);
        }
        for (i, c) in r.chain.iter().enumerate() {
            println!(
                "  {}{} (valid {} to {})",
                if i == 0 {
                    "signer:     "
                } else {
                    "  issued by "
                },
                c.subject(),
                c.not_before(),
                c.not_after()
            );
        }
        println!("  trust:      {:?}", r.trust);
        println!("  revocation: {:?}", r.revocation);
        if let Some(ts) = &r.timestamp {
            println!(
                "  timestamp:  {} by {} (valid: {}, trust: {:?})",
                ts.time,
                ts.authority
                    .as_ref()
                    .map(|c| c.display_name())
                    .unwrap_or_default(),
                ts.valid,
                ts.trust
            );
        }
        println!("  revision:   {} of {}", r.revision, r.revisions);
        for c in &r.changes {
            let verdict = if c.allowed { "allowed" } else { "NOT ALLOWED" };
            println!("  change:     {} ({:?}, {verdict})", c.description, c.kind);
        }
        for p in &r.problems {
            println!("  problem:    {p}");
        }
        for w in &r.warnings {
            println!("  note:       {w}");
        }
    }
    std::process::exit(if ok { 0 } else { 1 });
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2)
}
