//! pagebara-mobile as a program: on iOS, and on desktops in a phone-sized window for
//! development and UI tests.
//!
//! ```text
//! pagebara-mobile [--size WxH] [--dark | --light] [--profile DIR] [--script FILE] [FILE...]
//! ```

use std::path::PathBuf;
use std::rc::Rc;

fn main() {
    #[cfg(not(target_os = "ios"))]
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let (opts, size) = match parse(std::env::args().skip(1)) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    pagebara_mobile::init_threads();
    #[cfg(target_os = "ios")]
    let platform = Rc::new(pagebara_mobile::platform::ios::Ios::default());
    #[cfg(not(target_os = "ios"))]
    let platform = Rc::new(pagebara_mobile::platform::host::Host::new(
        opts.profile.clone(),
    ));
    let result = pagebara_mobile::run_sized(platform, opts, size);
    if let Err(e) = result {
        log::error!("{e}");
        std::process::exit(1);
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    std::process::exit(pagebara_mobile::script::exit_code());
}

type Size = Option<(f32, f32)>;

fn parse(args: impl Iterator<Item = String>) -> Result<(pagebara_mobile::Options, Size), String> {
    let mut opts = pagebara_mobile::Options::default();
    let mut size = None;
    let mut args = args.peekable();
    while let Some(a) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match a.as_str() {
            "--size" => {
                let v = value("--size")?;
                let (w, h) = v.split_once('x').ok_or("--size is WIDTHxHEIGHT")?;
                let n = |s: &str| s.parse::<f32>().map_err(|_| "--size is WIDTHxHEIGHT");
                size = Some((n(w)?, n(h)?));
            }
            "--dark" => opts.dark = Some(true),
            "--light" => opts.dark = Some(false),
            "--profile" => opts.profile = Some(PathBuf::from(value("--profile")?)),
            "--script" => opts.script = Some(PathBuf::from(value("--script")?)),
            #[cfg(not(target_os = "ios"))]
            "--make-samples" => {
                let dir = PathBuf::from(value("--make-samples")?);
                if let Err(e) = pagebara_samples::write_all(&dir) {
                    return Err(format!(
                        "could not write the samples to {}: {e}",
                        dir.display()
                    ));
                }
                std::process::exit(0);
            }
            "-h" | "--help" => {
                return Err("usage: pagebara-mobile [--size WxH] [--dark | --light] [--profile DIR] [--script FILE] [FILE...]".into());
            }
            s if s.starts_with("--") => return Err(format!("unknown option {s}")),
            f => opts.files.push(PathBuf::from(f)),
        }
    }
    Ok((opts, size))
}
