//! Accessibility preferences of the desktop: high contrast and reduced motion, read with
//! the system's own command-line tools (`gsettings`, `defaults`, `reg`). Unknown is `None`.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct OsPrefs {
    pub high_contrast: Option<bool>,
    pub reduce_motion: Option<bool>,
}

/// Reads the preferences; takes a few milliseconds, so call it off the UI thread.
pub fn read() -> OsPrefs {
    if cfg!(target_os = "macos") {
        let flag = |key: &str| {
            run("defaults", &["read", "com.apple.universalaccess", key])
                .map(|s| parse_bool(&s).unwrap_or(false))
        };
        OsPrefs {
            high_contrast: flag("increaseContrast"),
            reduce_motion: flag("reduceMotion"),
        }
    } else if cfg!(windows) {
        let hc = run(
            "reg",
            &[
                "query",
                r"HKCU\Control Panel\Accessibility\HighContrast",
                "/v",
                "Flags",
            ],
        )
        .and_then(|s| reg_value(&s))
        .map(|flags| flags & 1 != 0);
        let motion = run(
            "reg",
            &[
                "query",
                r"HKCU\Control Panel\Desktop\WindowMetrics",
                "/v",
                "MinAnimate",
            ],
        )
        .and_then(|s| reg_value(&s))
        .map(|v| v == 0);
        OsPrefs {
            high_contrast: hc,
            reduce_motion: motion,
        }
    } else {
        let get = |schema: &str, key: &str| run("gsettings", &["get", schema, key]);
        let hc = get("org.gnome.desktop.a11y.interface", "high-contrast")
            .and_then(|s| parse_bool(&s))
            .or_else(|| {
                get("org.gnome.desktop.interface", "gtk-theme")
                    .map(|t| t.to_ascii_lowercase().contains("highcontrast"))
            });
        let motion = get("org.gnome.desktop.interface", "enable-animations")
            .and_then(|s| parse_bool(&s))
            .map(|on| !on);
        OsPrefs {
            high_contrast: hc,
            reduce_motion: motion,
        }
    }
}

/// The output of a command that finished in time and successfully.
fn run(program: &str, args: &[&str]) -> Option<String> {
    let mut c = Command::new(program);
    c.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW: the app has no console, so `reg` would open one of its own.
        c.creation_flags(0x0800_0000);
    }
    let mut child = c.spawn().ok()?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if start.elapsed() > Duration::from_secs(2) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
    Some(out.trim().to_owned())
}

fn parse_bool(s: &str) -> Option<bool> {
    match s.trim().trim_matches('\'') {
        "true" | "1" => Some(true),
        "false" | "0" => Some(false),
        _ => None,
    }
}

/// The value of `reg query` output: `Flags    REG_SZ    126` or `REG_DWORD 0x1`.
fn reg_value(out: &str) -> Option<u32> {
    let v = out
        .lines()
        .find(|l| l.contains("REG_"))?
        .split_whitespace()
        .last()?;
    match v.strip_prefix("0x") {
        Some(h) => u32::from_str_radix(h, 16).ok(),
        None => v.parse().ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_output() {
        assert_eq!(parse_bool("true"), Some(true));
        assert_eq!(parse_bool("'false'"), Some(false));
        assert_eq!(parse_bool("1\n"), Some(true));
        assert_eq!(parse_bool("maybe"), None);
        let reg = "\r\nHKEY_CURRENT_USER\\Control Panel\\Accessibility\\HighContrast\r\n    Flags    REG_SZ    127\r\n";
        assert_eq!(reg_value(reg), Some(127));
        assert_eq!(reg_value("    MinAnimate    REG_DWORD    0x0"), Some(0));
        assert_eq!(reg_value("nothing"), None);
    }
}
