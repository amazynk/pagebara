//! Each action on the sample documents, in a temporary folder.

#![allow(clippy::cloned_ref_to_slice_refs)]

use std::path::{Path, PathBuf};

use pagebara_actions::{Action, Options, collect, run};
use pagebara_pages::PageTree;
use pagebara_syntax::Document;

fn samples(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pagebara-actions-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    pagebara_samples::write_all(&dir).unwrap();
    dir
}

fn pages(p: &Path) -> Vec<u16> {
    let d = Document::open(std::fs::read(p).unwrap()).unwrap();
    PageTree::open(&d)
        .pages()
        .iter()
        .map(|p| p.rotate)
        .collect()
}

fn no_progress(_: usize, _: usize) {}

#[test]
fn names_and_parse() {
    for a in Action::ALL {
        assert_eq!(Action::parse(a.name()), Some(a));
        assert!(!a.title().is_empty());
        assert!(!a.inputs().is_empty());
    }
    assert!(Action::Merge.multi() && Action::ConvertImages.multi() && !Action::Split.multi());
    assert_eq!(Action::parse("nope"), None);
}

#[test]
fn merge_keeps_order_and_never_clobbers() {
    let dir = samples("merge");
    let (lease, report) = (dir.join("lease.pdf"), dir.join("report.pdf"));
    let before = std::fs::read(&lease).unwrap();
    let (nl, nr) = (pages(&lease).len(), pages(&report).len());
    let files = [lease.clone(), report.clone()];
    let r = run(Action::Merge, &files, &Options::default(), &no_progress).unwrap();
    assert_eq!(r.outputs, [dir.join("lease (merged).pdf")]);
    assert_eq!(pages(&r.outputs[0]).len(), nl + nr);
    let r2 = run(Action::Merge, &files, &Options::default(), &no_progress).unwrap();
    assert_eq!(r2.outputs, [dir.join("lease (merged) (2).pdf")]);
    assert_eq!(std::fs::read(&lease).unwrap(), before);
    // A protected file needs its password; nothing is written without it.
    let locked = [lease.clone(), dir.join("lease-protected.pdf")];
    let e = run(Action::Merge, &locked, &Options::default(), &no_progress).unwrap_err();
    assert!(e.contains("password"), "{e}");
    let opts = Options {
        password: Some("user".into()),
        ..Options::default()
    };
    let r = run(Action::Merge, &locked, &opts, &no_progress).unwrap();
    assert_eq!(pages(&r.outputs[0]).len(), 2 * nl);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn split_one_file_per_page() {
    let dir = samples("split");
    let lease = dir.join("lease.pdf");
    let n = pages(&lease).len();
    let r = run(Action::Split, &[lease], &Options::default(), &no_progress).unwrap();
    assert_eq!(r.outputs.len(), n);
    assert_eq!(r.outputs[0], dir.join("lease - page 1.pdf"));
    for o in &r.outputs {
        assert_eq!(pages(o).len(), 1);
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn rotate_in_place() {
    let dir = samples("rotate");
    let lease = dir.join("lease.pdf");
    let n = pages(&lease).len();
    let r = run(
        Action::RotateRight,
        &[lease.clone()],
        &Options::default(),
        &no_progress,
    )
    .unwrap();
    assert_eq!(r.outputs, [lease.clone()]);
    assert_eq!(pages(&lease), vec![90; n]);
    run(
        Action::RotateLeft,
        &[lease.clone()],
        &Options::default(),
        &no_progress,
    )
    .unwrap();
    run(
        Action::RotateLeft,
        &[lease.clone()],
        &Options::default(),
        &no_progress,
    )
    .unwrap();
    assert_eq!(pages(&lease), vec![270; n]);
    // No temporary files are left behind.
    assert!(
        std::fs::read_dir(&dir).unwrap().all(|e| !e
            .unwrap()
            .file_name()
            .to_string_lossy()
            .ends_with(".tmp"))
    );
    // A file that is not a PDF stays as it was and fails.
    let png = dir.join("chart.png");
    let before = std::fs::read(&png).unwrap();
    assert!(
        run(
            Action::RotateRight,
            &[png.clone()],
            &Options::default(),
            &no_progress
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&png).unwrap(), before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn compress_writes_a_smaller_copy_or_nothing() {
    let dir = samples("compress");
    let report = dir.join("report.pdf");
    let r = run(
        Action::Compress,
        &[report.clone()],
        &Options::default(),
        &no_progress,
    )
    .unwrap();
    assert_eq!(r.messages.len(), 1, "{:?}", r.messages);
    match r.outputs.first() {
        Some(o) => {
            assert_eq!(o, &dir.join("report (compressed).pdf"));
            assert!(
                std::fs::metadata(o).unwrap().len() < std::fs::metadata(&report).unwrap().len()
            );
            assert_eq!(pages(o).len(), pages(&report).len());
        }
        None => assert!(r.messages[0].contains("already compact")),
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn pages_to_images() {
    let dir = samples("images");
    let lease = dir.join("lease.pdf");
    let n = pages(&lease).len();
    let opts = Options {
        dpi: Some(20.0),
        ..Options::default()
    };
    let r = run(Action::Images, &[lease.clone()], &opts, &no_progress).unwrap();
    assert_eq!(r.outputs.len(), n);
    assert_eq!(r.outputs[0], dir.join("lease pages").join("page-1.png"));
    assert!(
        std::fs::read(&r.outputs[0])
            .unwrap()
            .starts_with(b"\x89PNG")
    );
    let r = run(Action::Images, &[lease], &opts, &no_progress).unwrap();
    assert_eq!(r.outputs[0], dir.join("lease pages (2)").join("page-1.png"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn images_to_pdf_in_order() {
    let dir = samples("convert");
    let files = [dir.join("chart.png"), dir.join("signature-photo.png")];
    let r = run(
        Action::ConvertImages,
        &files,
        &Options::default(),
        &no_progress,
    )
    .unwrap();
    assert_eq!(r.outputs, [dir.join("chart.pdf")]);
    assert_eq!(pages(&r.outputs[0]).len(), 2);
    // A PDF among the images is reported, the rest converted.
    let mixed = [dir.join("chart.png"), dir.join("lease.pdf")];
    let r = run(
        Action::ConvertImages,
        &mixed,
        &Options::default(),
        &no_progress,
    )
    .unwrap();
    assert_eq!(r.outputs, [dir.join("chart (2).pdf")]);
    assert_eq!(pages(&r.outputs[0]).len(), 1);
    assert_eq!(r.messages.len(), 1);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn ocr_needs_models_and_makes_text() {
    let dir = samples("ocr");
    let scan = dir.join("scan.pdf");
    let before = std::fs::read(&scan).unwrap();
    let Some(models) = std::env::var_os("PAGEBARA_OCR_MODELS") else {
        let opts = Options {
            models_dir: Some(dir.join("no-models")),
            ..Options::default()
        };
        if !pagebara_ocr::model::has_bundled() {
            let e = run(Action::Ocr, &[scan], &opts, &no_progress).unwrap_err();
            assert!(e.contains("models"), "{e}");
        }
        eprintln!("skipped recognition: set PAGEBARA_OCR_MODELS");
        std::fs::remove_dir_all(dir).unwrap();
        return;
    };
    let opts = Options {
        models_dir: Some(models.into()),
        ..Options::default()
    };
    let r = run(Action::Ocr, &[scan.clone()], &opts, &no_progress).unwrap();
    assert_eq!(
        r.outputs,
        [dir.join("scan (searchable).pdf")],
        "{:?}",
        r.messages
    );
    assert!(r.messages[0].contains("recognised"), "{:?}", r.messages);
    assert!(!r.messages[0].contains(" 0 words"), "{:?}", r.messages);
    assert_eq!(pages(&r.outputs[0]).len(), pages(&scan).len());
    // Its text layer counts as text: a second run has nothing left to do.
    let again = run(Action::Ocr, &r.outputs, &opts, &no_progress).unwrap();
    assert!(again.outputs.is_empty(), "{:?}", again.messages);
    assert_eq!(std::fs::read(&scan).unwrap(), before);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn collect_gathers_the_files_of_one_click() {
    let run_dir =
        std::env::temp_dir().join(format!("pagebara-actions-collect-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&run_dir);
    let d = run_dir.clone();
    let first = pagebara_thread::spawn("test-collect", move || {
        collect(Action::Merge, vec!["/a.pdf".into()], Some(&d))
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    let mut others = Vec::new();
    for name in ["/b.pdf", "/c.pdf"] {
        let d = run_dir.clone();
        others.push(pagebara_thread::spawn("test-collect", move || {
            collect(Action::Merge, vec![name.into()], Some(&d))
        }));
    }
    for o in others {
        assert_eq!(o.join().unwrap(), None);
    }
    let mut got = first.join().unwrap().unwrap();
    got.sort();
    assert_eq!(
        got,
        [PathBuf::from("/a.pdf"), "/b.pdf".into(), "/c.pdf".into()]
    );
    let _ = std::fs::remove_dir_all(run_dir);
}
