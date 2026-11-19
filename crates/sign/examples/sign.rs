//! Signs a PDF.
//!
//! ```text
//! cargo run -p pagebara-sign --example sign -- IN.pdf OUT.pdf
//!     (--p12 ID.p12 --password PW | --key KEY.pem --cert CERTS.pem [--password PW])
//!     [--field NAME] [--visible PAGE,X0,Y0,X1,Y1] [--reason TEXT] [--location TEXT]
//!     [--certify 1|2|3] [--pkcs7] [--pss] [--tsa URL] [--ltv]
//! ```
//!
//! `--tsa` posts the timestamp request with the `curl` command: the engine takes the HTTP
//! transport as a closure rather than carrying an HTTP stack.

use pagebara_sign::{
    Appearance, HttpTimestampAuthority, Identity, MdpPermissions, SignOptions, SubFilter,
    TimestampAuthority, sign, timestamp_document,
};
use pagebara_syntax::Document;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let flag = |name: &str| args.iter().any(|a| a == name);
    let positional: Vec<&String> = args
        .iter()
        .enumerate()
        .filter(|(i, a)| {
            !a.starts_with("--")
                && (*i == 0 || !args[i - 1].starts_with("--") || is_flag(&args[i - 1]))
        })
        .map(|(_, a)| a)
        .collect();
    let [input, output] = positional[..] else {
        fail(
            "usage: sign IN.pdf OUT.pdf (--p12 FILE --password PW | --key KEY.pem --cert CERTS.pem) …",
        )
    };
    let password = opt("--password");
    let identity = if let Some(p12) = opt("--p12") {
        let data = read(&p12);
        Identity::from_pkcs12(&data, password.as_deref().unwrap_or_default())
    } else if let (Some(k), Some(c)) = (opt("--key"), opt("--cert")) {
        Identity::from_pem(&read(&k), &read(&c), password.as_deref())
    } else {
        fail("give --p12 or --key and --cert")
    }
    .unwrap_or_else(|e| fail(&format!("identity: {e}")));
    println!(
        "signing as {} ({}), issued by {}",
        identity.certificate.display_name(),
        identity.key.description(),
        identity.certificate.issuer_display_name()
    );
    let appearance = opt("--visible").map(|v| {
        let n: Vec<f64> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if n.len() != 5 {
            fail("--visible PAGE,X0,Y0,X1,Y1");
        }
        Appearance {
            page: n[0] as usize,
            rect: [n[1], n[2], n[3], n[4]],
            image: None,
            text: true,
        }
    });
    let tsa = opt("--tsa").map(|url| HttpTimestampAuthority {
        url,
        post: curl_post,
    });
    let field = opt("--field");
    let reason = opt("--reason");
    let location = opt("--location");
    let certify = opt("--certify").map(|p| match p.as_str() {
        "1" => MdpPermissions::NoChanges,
        "2" => MdpPermissions::FormFilling,
        "3" => MdpPermissions::Annotations,
        _ => fail("--certify 1, 2 or 3"),
    });
    let opts = SignOptions {
        field: field.as_deref(),
        appearance: appearance.as_ref(),
        reason: reason.as_deref(),
        location: location.as_deref(),
        sub_filter: if flag("--pkcs7") {
            SubFilter::Pkcs7Detached
        } else {
            SubFilter::CadesDetached
        },
        pss: flag("--pss"),
        certify,
        timestamp: tsa.as_ref().map(|t| t as &dyn TimestampAuthority),
        ..Default::default()
    };
    let doc = Document::open(read(input)).unwrap_or_else(|e| fail(&e.to_string()));
    let mut out = sign(&doc, &identity, &opts).unwrap_or_else(|e| fail(&format!("signing: {e}")));
    if flag("--ltv") {
        let Some(tsa) = &tsa else {
            fail("--ltv needs --tsa for the document timestamp")
        };
        let doc = Document::open(out).unwrap_or_else(|e| fail(&e.to_string()));
        out = timestamp_document(&doc, tsa, identity.key.default_digest())
            .unwrap_or_else(|e| fail(&format!("document timestamp: {e}")));
    }
    std::fs::write(output, &out).unwrap_or_else(|e| fail(&format!("{output}: {e}")));
    println!("wrote {output} ({} bytes)", out.len());
}

fn is_flag(a: &str) -> bool {
    matches!(a, "--pkcs7" | "--pss" | "--ltv")
}

/// The HTTP transport: `curl` posting the request body from stdin.
fn curl_post(url: &str, content_type: &str, body: &[u8]) -> Result<Vec<u8>, String> {
    use std::io::Write as _;
    use std::process::{Command, Stdio};
    let mut child = Command::new("curl")
        .args(["-sS", "--fail", "-H"])
        .arg(format!("Content-Type: {content_type}"))
        .args(["--data-binary", "@-", url])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| format!("curl: {e}"))?;
    child
        .stdin
        .take()
        .ok_or("curl: no stdin")?
        .write_all(body)
        .map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    Ok(out.stdout)
}

fn read(path: &str) -> Vec<u8> {
    std::fs::read(path).unwrap_or_else(|e| fail(&format!("{path}: {e}")))
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2)
}
