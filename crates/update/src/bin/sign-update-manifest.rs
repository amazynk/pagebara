//! Signs an update manifest for publishing (see the `pagebara_update` docs for the format).
//!
//! ```text
//! sign-update-manifest --generate-key KEY_FILE      new key; prints the public key
//! sign-update-manifest --key KEY_FILE MANIFEST.json [OUT]
//! sign-update-manifest --public-key KEY_FILE        prints the public key
//! ```
//!
//! The key file holds the 32-byte secret as 64 hex digits. Keep it out of the repository;
//! put the public key in `crates/support/identity/src/lib.rs` (`UPDATE_PUBLIC_KEY`).

use std::io::Write;
use std::path::Path;
use std::process::ExitCode;

const USAGE: &str = "usage:
  sign-update-manifest --generate-key KEY_FILE
  sign-update-manifest --key KEY_FILE MANIFEST.json [OUT]   (default OUT: stdout)
  sign-update-manifest --public-key KEY_FILE";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let r = match args.as_slice() {
        ["--generate-key", file] => generate(Path::new(file)),
        ["--public-key", file] => read_seed(Path::new(file))
            .map(|seed| println!("{}", pagebara_update::public_key_hex(&seed))),
        ["--key", key, manifest] => sign(Path::new(key), Path::new(manifest), None),
        ["--key", key, manifest, out] => {
            sign(Path::new(key), Path::new(manifest), Some(Path::new(out)))
        }
        _ => Err(USAGE.to_owned()),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

fn generate(file: &Path) -> Result<(), String> {
    let (seed, public) = pagebara_update::generate_key().map_err(|e| e.to_string())?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    let mut f = opts
        .open(file)
        .map_err(|e| format!("{}: {e}", file.display()))?;
    writeln!(f, "{}", pagebara_update::hex_encode(&seed)).map_err(|e| e.to_string())?;
    println!("{public}");
    Ok(())
}

fn read_seed(file: &Path) -> Result<[u8; 32], String> {
    let text = std::fs::read_to_string(file).map_err(|e| format!("{}: {e}", file.display()))?;
    pagebara_update::parse_seed(&text)
        .ok_or_else(|| format!("{}: expected 64 hex digits", file.display()))
}

fn sign(key: &Path, manifest: &Path, out: Option<&Path>) -> Result<(), String> {
    let seed = read_seed(key)?;
    let text =
        std::fs::read_to_string(manifest).map_err(|e| format!("{}: {e}", manifest.display()))?;
    let envelope = pagebara_update::sign(&text, &seed).map_err(|e| e.to_string())? + "\n";
    match out {
        Some(p) => std::fs::write(p, envelope).map_err(|e| format!("{}: {e}", p.display())),
        None => std::io::stdout()
            .write_all(envelope.as_bytes())
            .map_err(|e| e.to_string()),
    }
}
