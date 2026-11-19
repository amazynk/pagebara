//! The command line output for scripts and AI tools (`--text`, `--text --json`,
//! `--images`), run as the built binary on the generated sample documents.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::OnceLock;

fn pagebara() -> Command {
    Command::new(env!("CARGO_BIN_EXE_pagebara"))
}

/// A fresh directory for this test run.
fn temp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pagebara-cli-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The sample documents, written once by `pagebara --make-samples`.
fn samples() -> &'static Path {
    static DIR: OnceLock<PathBuf> = OnceLock::new();
    DIR.get_or_init(|| {
        let d = temp("samples");
        let o = pagebara().arg("--make-samples").arg(&d).output().unwrap();
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        d
    })
}

fn run(args: &[&str]) -> Output {
    let mut c = pagebara();
    for a in args {
        // Sample names are given bare.
        if a.ends_with(".pdf") {
            c.arg(samples().join(a));
        } else {
            c.arg(a);
        }
    }
    c.output().unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8(o.stdout.clone()).unwrap()
}

fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn text_page_by_page() {
    let o = run(&["--text", "lease.pdf"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let s = stdout(&o);
    assert!(s.starts_with("--- page 1 ---\n"), "{s}");
    assert!(s.contains("RESIDENTIAL LEASE AGREEMENT"));
    assert_eq!(s.matches("--- page ").count(), 4);

    let o = run(&["--text", "lease.pdf", "--pages", "2-3"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let s = stdout(&o);
    assert!(s.starts_with("--- page 2 ---\n") && s.contains("--- page 3 ---\n"));
    assert!(!s.contains("--- page 1 ---") && !s.contains("--- page 4 ---"));
}

#[test]
fn text_as_json_lines() {
    let o = run(&["--text", "lease.pdf", "--json", "--pages", "1,3-"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let pages: Vec<serde_json::Value> = stdout(&o)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let numbers: Vec<u64> = pages.iter().map(|p| p["page"].as_u64().unwrap()).collect();
    assert_eq!(numbers, [1, 3, 4]);
    let first = &pages[0];
    assert!(
        first["text"]
            .as_str()
            .unwrap()
            .contains("RESIDENTIAL LEASE AGREEMENT")
    );
    for p in &pages {
        let (w, h) = (p["width"].as_f64().unwrap(), p["height"].as_f64().unwrap());
        assert!(w > 100.0 && h > 100.0, "{p}");
        let blocks = p["blocks"].as_array().unwrap();
        assert!(!blocks.is_empty(), "{p}");
        for b in blocks {
            let r: Vec<f64> = b["bbox"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_f64().unwrap())
                .collect();
            assert!(r[0] <= r[2] && r[1] <= r[3], "{b}");
            assert!(r[0] >= -1.0 && r[2] <= w + 1.0 && r[1] >= -1.0 && r[3] <= h + 1.0);
            assert!(!b["text"].as_str().unwrap().is_empty());
        }
    }
    // Top-left origin: the title block is the first and lies near the top.
    let title = &first["blocks"].as_array().unwrap()[0];
    assert!(
        title["text"]
            .as_str()
            .unwrap()
            .contains("RESIDENTIAL LEASE AGREEMENT"),
        "{title}"
    );
    assert!(title["bbox"][1].as_f64().unwrap() < first["height"].as_f64().unwrap() / 4.0);

    // --json without --text is a usage error.
    let o = run(&["--json", "lease.pdf"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
}

/// Reads the width and height from a PNG's header.
fn png_size(data: &[u8]) -> (u32, u32) {
    assert_eq!(&data[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&data[12..16], b"IHDR");
    let be = |i: usize| u32::from_be_bytes(data[i..i + 4].try_into().unwrap());
    (be(16), be(20))
}

#[test]
fn images_at_a_resolution() {
    let out = temp("images");
    let o = pagebara()
        .arg("--images")
        .arg(samples().join("lease.pdf"))
        .args(["--pages", "1,4", "--dpi", "36", "--out"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let printed: Vec<PathBuf> = stdout(&o).lines().map(PathBuf::from).collect();
    assert_eq!(printed, [out.join("page-1.png"), out.join("page-4.png")]);
    // The page size in points from --json; at 36 dpi a pixel is two points.
    let o = run(&["--text", "lease.pdf", "--json", "--pages", "1"]);
    let p: serde_json::Value = serde_json::from_str(stdout(&o).trim()).unwrap();
    let (w, h) = (p["width"].as_f64().unwrap(), p["height"].as_f64().unwrap());
    let want = ((w / 2.0).ceil() as u32, (h / 2.0).ceil() as u32);
    for f in &printed {
        assert_eq!(
            png_size(&std::fs::read(f).unwrap()),
            want,
            "{}",
            f.display()
        );
    }
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn bad_page_lists() {
    for (pages, msg) in [
        ("0", "not a page number"),
        ("5", "past the end"),
        ("3-2", "backwards range"),
        ("x", "not a page number"),
    ] {
        let o = run(&["--text", "lease.pdf", "--pages", pages]);
        assert_eq!(o.status.code(), Some(1), "{pages}");
        assert!(stderr(&o).contains(msg), "{pages}: {}", stderr(&o));
        assert!(o.stdout.is_empty());
    }
    // Options for the export without one.
    let o = run(&["--pages", "1", "lease.pdf"]);
    assert_eq!(o.status.code(), Some(2));
    let o = run(&["--text", "lease.pdf", "--pages"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("--pages needs a value"));
    let o = run(&["--text", "missing.pdf"]);
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn password_protected_files() {
    let o = run(&["--text", "lease-protected.pdf"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("--password"), "{}", stderr(&o));
    let o = run(&["--text", "lease-protected.pdf", "--password", "nope"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("wrong password"), "{}", stderr(&o));
    let o = run(&["--text", "lease-protected.pdf", "--password", "user"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(stdout(&o).contains("RESIDENTIAL LEASE AGREEMENT"));
    let o = run(&[
        "--text",
        "lease-unicode.pdf",
        "--json",
        "--password",
        "pässwörd 密码 🔐",
    ]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o).lines().count(), 4);
    let out = temp("protected-images");
    let o = pagebara()
        .arg("--images")
        .arg(samples().join("lease-protected.pdf"))
        .args(["--password", "user", "--pages", "1", "--dpi", "20", "--out"])
        .arg(&out)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(out.join("page-1.png").exists());
    let _ = std::fs::remove_dir_all(&out);
}

/// `pagebara --text big.pdf | head`: the reader going away early is not an error.
#[test]
fn broken_pipe_is_not_an_error() {
    // The same pages many times over: far more than a pipe buffer holds.
    let pages = vec!["1-"; 2000].join(",");
    for json in [false, true] {
        let mut c = pagebara();
        c.arg("--text").arg(samples().join("lease.pdf"));
        c.args(["--pages", &pages]);
        if json {
            c.arg("--json");
        }
        let mut child = c
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let mut head = [0; 100];
        child.stdout.take().unwrap().read_exact(&mut head).unwrap();
        // The pipe's read end is closed here.
        let o = child.wait_with_output().unwrap();
        assert_eq!(o.status.code(), Some(0), "json {json}: {}", stderr(&o));
        assert!(o.stderr.is_empty(), "json {json}: {}", stderr(&o));
    }
}

/// `--thumbnail IN OUT SIZE` (the file managers' thumbnailer): page 1, fitted into SIZE,
/// fast enough for a folder full of PDFs.
#[test]
fn thumbnail_size_and_speed() {
    let out = temp("thumb");
    for (file, size) in [
        ("lease.pdf", 128),
        ("report.pdf", 256),
        ("rotated.pdf", 512),
    ] {
        let png = out.join(format!("{file}.png"));
        let mut best = std::time::Duration::MAX;
        for _ in 0..3 {
            let start = std::time::Instant::now();
            let o = run(&[
                "--thumbnail",
                file,
                png.to_str().unwrap(),
                &size.to_string(),
            ]);
            best = best.min(start.elapsed());
            assert!(o.status.success(), "{file}: {}", stderr(&o));
        }
        let (w, h) = png_size(&std::fs::read(&png).unwrap());
        assert_eq!(w.max(h), size, "{file}: {w}x{h}");
        assert!(w.min(h) > size / 3, "{file}: {w}x{h}");
        assert!(best.as_millis() < 150, "{file}: {best:?}");
    }
    let o = run(&[
        "--thumbnail",
        "missing.pdf",
        out.join("x.png").to_str().unwrap(),
        "128",
    ]);
    assert_eq!(o.status.code(), Some(1));
    let o = run(&["--thumbnail", "lease.pdf", "x.png", "0"]);
    assert_eq!(o.status.code(), Some(2), "{}", stderr(&o));
    let _ = std::fs::remove_dir_all(&out);
}

/// The desktop file and AppStream metainfo as staged for packages pass the
/// freedesktop validators (when installed). Linux only: `stage.sh` needs GNU `install -D`.
#[cfg(target_os = "linux")]
#[test]
fn linux_desktop_files_validate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let dir = temp("stage");
    let bin = dir.join("bin");
    std::fs::write(&bin, b"").unwrap();
    let stage = dir.join("root");
    let o = Command::new("sh")
        .arg(root.join("packaging/linux/stage.sh"))
        .arg(&bin)
        .arg(&stage)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let share = stage.join("usr/share");
    let one = |sub: &str| {
        std::fs::read_dir(share.join(sub))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    };
    let desktop = std::fs::read_to_string(one("applications")).unwrap();
    assert!(desktop.contains("MimeType=application/pdf;application/x-pdf;"));
    assert!(desktop.contains(" %U"));
    // The thumbnailer is the user's choice (Settings > System integration), not the package's.
    assert!(!share.join("thumbnailers").exists());
    for (tool, args, file) in [
        ("desktop-file-validate", vec![], one("applications")),
        (
            "appstreamcli",
            vec!["validate", "--no-net"],
            one("metainfo"),
        ),
    ] {
        match Command::new(tool).args(&args).arg(&file).output() {
            Ok(o) => assert!(o.status.success(), "{tool}: {}{}", stdout(&o), stderr(&o)),
            Err(_) => eprintln!("skipped {tool}: not installed"),
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// `--integration` against a `--profile` folder: never the user's own settings. Linux only:
/// the items and the files they write (an XDG thumbnailer) are the Linux ones.
#[cfg(target_os = "linux")]
#[test]
fn integration_from_the_command_line() {
    let profile = temp("integration");
    let p = profile.to_str().unwrap();
    let int = |args: &[&str]| {
        let mut c = pagebara();
        c.args(["--profile", p, "--integration"]).args(args);
        c.output().unwrap()
    };
    let o = int(&["status"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let line = |out: &str, id: &str| {
        out.lines()
            .find(|l| l.starts_with(&format!("{id} ")))
            .unwrap_or_else(|| panic!("no {id} in {out}"))
            .to_owned()
    };
    assert!(line(&stdout(&o), "thumbnails").contains(" off "));
    assert!(line(&stdout(&o), "single-instance").contains(" on "));
    assert!(line(&stdout(&o), "tray").contains(" - "));
    let o = int(&["enable", "thumbnails", "type-images"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let thumbs = profile.join("xdg/data/thumbnailers");
    let file = std::fs::read_dir(&thumbs)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(
        std::fs::read_to_string(&file)
            .unwrap()
            .contains("--thumbnail %i %o %s")
    );
    let o = int(&["status", "--json"]);
    let rows: Vec<serde_json::Value> = stdout(&o)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let row = rows.iter().find(|r| r["id"] == "thumbnails").unwrap();
    assert_eq!(row["on"], true);
    let o = int(&["disable", "single-instance", "recent"]);
    assert!(o.status.success(), "{}", stderr(&o));
    let settings = std::fs::read_to_string(profile.join("settings.json")).unwrap();
    assert!(
        settings.contains("\"single_instance\": false"),
        "{settings}"
    );
    let o = int(&["enable", "no-such-item"]);
    assert_eq!(o.status.code(), Some(1));
    let o = int(&["remove-all"]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert!(!thumbs.exists());
    assert!(!profile.join("integration.json").exists());
    assert!(!profile.join("xdg/data/applications").exists());
    let _ = std::fs::remove_dir_all(&profile);
}

/// `--action`: a file action without a window, outputs next to the input.
#[test]
fn actions_from_the_command_line() {
    let dir = temp("actions");
    for f in ["lease.pdf", "form.pdf"] {
        std::fs::copy(samples().join(f), dir.join(f)).unwrap();
    }
    let o = pagebara()
        .args(["--action", "merge"])
        .arg(dir.join("lease.pdf"))
        .arg(dir.join("form.pdf"))
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", stderr(&o));
    let merged = PathBuf::from(stdout(&o).trim());
    assert_eq!(merged, dir.join("lease (merged).pdf"));
    assert!(merged.is_file());
    let o = pagebara()
        .args(["--action", "explode"])
        .arg(&merged)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("merge"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "requires a macOS window session and Automator"]
fn macos_application_menu_and_finder_workflow() {
    let dir = temp("finder");
    let profile = dir.join("profile");
    let output = pagebara()
        .arg("--profile")
        .arg(&profile)
        .args(["--size", "1440x900", "--script"])
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/e2e/macos/app-menu.pdfs"))
        .env("PAGEBARA_BACKGROUND", "1")
        .env("PAGEBARA_LANG", "en")
        .env("PAGEBARA_KEYSTORE", "test")
        .env("PAGEBARA_SHOTS", &dir)
        .env("SAMPLES", samples())
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        dir.join("macos-quit-cancelled.png").is_file(),
        "Quit bypassed the save prompt"
    );

    let input = dir.join("A & B's chart.png");
    std::fs::copy(samples().join("chart.png"), &input).unwrap();
    let workflow = profile.join(format!(
        "home/Library/Services/Convert to PDF ({}).workflow",
        pagebara_identity::NAME
    ));
    let output = Command::new("/usr/bin/automator")
        .arg("-i")
        .arg(&input)
        .arg(&workflow)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        std::fs::read(input.with_extension("pdf"))
            .unwrap()
            .starts_with(b"%PDF-")
    );
    let output = pagebara()
        .arg("--profile")
        .arg(&profile)
        .args(["--integration", "disable", "type-images"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(!workflow.exists());
    std::fs::remove_dir_all(dir).unwrap();
}
