use std::path::PathBuf;

use pagebara_syntax::Document;

use super::system::{Job, lp_args, parse_lpstat};
use super::*;

#[test]
fn selects_pages() {
    assert_eq!(select_pages(&PageRange::All, 3).unwrap(), [0, 1, 2]);
    assert_eq!(select_pages(&PageRange::Current(1), 3).unwrap(), [1]);
    assert!(select_pages(&PageRange::Current(5), 3).is_err());
    assert_eq!(
        select_pages(&PageRange::Ranges("3, 1-2".into()), 5).unwrap(),
        [2, 0, 1]
    );
    assert!(select_pages(&PageRange::Ranges("9".into()), 5).is_err());
    assert_eq!(
        select_pages(&PageRange::Pages(vec![4, 0, 7]), 5).unwrap(),
        [4, 0]
    );
    assert!(select_pages(&PageRange::All, 0).is_err());
}

#[test]
fn places_pages_on_paper() {
    let a4 = Paper::A4.size().unwrap();
    let o = PrintOptions::default();
    // A landscape page turns on portrait paper and fills its width.
    let p = place((842.0, 595.0), &o);
    assert!(p.rotate);
    assert_eq!(p.sheet, a4);
    assert!((p.w - a4.0).abs() < 0.5, "{p:?}");
    assert!((p.y - (a4.1 - p.h) / 2.0).abs() < 1e-9);

    let actual = PrintOptions {
        scale: Scale::Actual,
        auto_rotate: false,
        centre: false,
        ..o.clone()
    };
    let p = place((612.0, 792.0), &actual);
    assert_eq!((p.scale, p.x, p.y, p.rotate), (1.0, 0.0, 0.0, false));

    let half = PrintOptions {
        scale: Scale::Custom(50.0),
        ..o.clone()
    };
    let p = place((595.0, 842.0), &half);
    assert_eq!(p.scale, 0.5);
    assert!((p.x - (a4.0 - 297.5) / 2.0).abs() < 1e-9);

    let doc = PrintOptions {
        paper: Paper::Document,
        ..o
    };
    assert_eq!(place((300.0, 200.0), &doc).sheet, (300.0, 200.0));
    assert_eq!(Paper::for_locale("en_US.UTF-8"), Paper::Letter);
    assert_eq!(Paper::for_locale("de_DE.UTF-8"), Paper::A4);
    assert_eq!(Paper::for_locale("C"), Paper::A4);
}

#[test]
fn cups_commands() {
    let printers = parse_lpstat(
        "printer office is idle.  enabled since Mon 01 Jan\n\
         \tDescription: Office laser (2nd floor)\n\
         printer PDF disabled since Tue 02 Jan -\n\treason unknown\n\
         system default destination: PDF\n",
    );
    assert_eq!(printers.len(), 2);
    assert_eq!(printers[0].name, "PDF");
    assert_eq!(printers[0].description, "");
    assert_eq!(printers[1].description, "Office laser (2nd floor)");
    assert!(printers[0].is_default && !printers[0].enabled);
    assert!(printers[1].enabled && !printers[1].is_default);
    assert!(parse_lpstat("no system default destination\n").is_empty());

    let job = Job {
        printer: "office".into(),
        file: PathBuf::from("/tmp/x y.pdf"),
        title: "Lease".into(),
        copies: 2,
        duplex: Duplex::LongEdge,
        paper: Paper::A4,
        collate: true,
    };
    assert_eq!(
        lp_args(&job),
        [
            "-d",
            "office",
            "-n",
            "2",
            "-o",
            "sides=two-sided-long-edge",
            "-o",
            "media=A4",
            "-o",
            "collate=true",
            "-t",
            "Lease",
            "--",
            "/tmp/x y.pdf"
        ]
    );
}

#[test]
fn writes_a_print_pdf_with_rendered_sheets() {
    let src = Document::open(crate::test_pdf::text(&["Print me", "please"])).unwrap();
    let s = crate::session::Session::new(None, src);
    let doc = prepare_document(s.doc(), &s.tree.refs(), false, false);
    let renderer = pagebara_render::Renderer::new(doc);
    let opts = PrintOptions {
        greyscale: true,
        dpi: 72,
        ..PrintOptions::default()
    };
    let size = renderer.page_size(0).unwrap();
    let placed = place((f64::from(size.0), f64::from(size.1)), &opts);
    let sheet = render_sheet(&renderer, 0, &placed, opts.dpi, true, None).unwrap();
    assert_eq!(sheet.width, 595);
    assert!(sheet.grey);
    // Some ink where the text is, white paper elsewhere.
    assert!(sheet.data.iter().any(|&v| v < 100));
    assert_eq!(sheet.data[0], 255);
    assert_eq!(sheet.to_rgba().len(), sheet.data.len() * 4);

    let dir = std::env::temp_dir().join(format!("pagebara-print-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("out.pdf");
    let pages = [0usize, 0, 0];
    let out = write_pdf(&path, pages.len(), |i| {
        render_sheet(&renderer, pages[i], &placed, 50, i == 1, None)
    })
    .unwrap();
    assert_eq!(out.pages, 3);
    let back = Document::open(std::fs::read(&path).unwrap()).unwrap();
    assert!(!back.was_repaired());
    let s = crate::session::Session::new(None, back);
    assert_eq!(s.page_count(), 3);
    let (w, h) = s.geoms[1].size();
    assert!(
        (w - 595.3).abs() < 0.5 && (h - 841.9).abs() < 0.5,
        "{w}x{h}"
    );
    // The spooler path (Windows) reads the sheets back as they were rendered.
    let file = spool::PrintFile::open(&path).unwrap();
    assert_eq!(file.len(), 3);
    for i in 0..3 {
        let back = file.sheet(i).unwrap();
        let sent = render_sheet(&renderer, 0, &placed, 50, i == 1, None).unwrap();
        assert_eq!(
            (back.width, back.height, back.grey),
            (sent.width, sent.height, sent.grey)
        );
        assert_eq!(back.data, sent.data);
        assert!((back.size_pt.0 - 595.3).abs() < 0.5);
    }
    assert!(file.sheet(3).is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

/// Prints three sheets to the printer named by `PAGEBARA_TEST_PRINTER` ("Microsoft Print to
/// PDF" or "Microsoft XPS Document Writer", which write a file instead of printing) and
/// checks the file. Skipped without the variable: the CI job on Windows sets it.
#[cfg(windows)]
#[test]
fn prints_through_the_windows_spooler() {
    let Ok(printer) = std::env::var("PAGEBARA_TEST_PRINTER") else {
        return;
    };
    let listed = super::system::Backend::Windows.printers().unwrap();
    assert!(
        listed.iter().any(|p| p.name == printer),
        "{printer} is not among {listed:?}"
    );
    let src = Document::open(crate::test_pdf::text(&["Printed by the spooler test"])).unwrap();
    let s = crate::session::Session::new(None, src);
    let renderer = pagebara_render::Renderer::new(s.doc().clone());
    let opts = PrintOptions {
        dpi: 100,
        ..PrintOptions::default()
    };
    let dir = std::env::temp_dir().join(format!("pagebara-spool-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("job.pdf");
    let pages = 3;
    let size = renderer.page_size(0).unwrap();
    let placed = place((f64::from(size.0), f64::from(size.1)), &opts);
    write_pdf(&path, pages, |i| {
        render_sheet(&renderer, 0, &placed, opts.dpi, i == 1, None)
    })
    .unwrap();
    let xps = printer.contains("XPS");
    let out = dir.join(if xps { "printed.oxps" } else { "printed.pdf" });
    let job = Job {
        printer,
        file: path,
        title: "pagebara spooler test".into(),
        copies: 1,
        duplex: Duplex::Off,
        paper: Paper::A4,
        collate: true,
    };
    let said = win32::submit(&job, Some(&out)).unwrap();
    assert!(said.starts_with("job "), "{said}");
    // The spooler writes the file after the job is handed over.
    let mut size = 0;
    for _ in 0..120 {
        let now = std::fs::metadata(&out).map_or(0, |m| m.len());
        if now > 0 && now == size {
            break;
        }
        size = now;
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    assert!(size > 0, "{} was not written", out.display());
    if let Ok(keep) = std::env::var("PAGEBARA_TEST_PRINT_OUT") {
        std::fs::copy(&out, keep).unwrap();
    }
    if !xps {
        let printed = Document::open(std::fs::read(&out).unwrap()).unwrap();
        let s = crate::session::Session::new(None, printed);
        assert_eq!(s.page_count(), pages);
        // A4 paper.
        let (w, h) = s.geoms[0].size();
        assert!(
            (w - 595.3).abs() < 2.0 && (h - 841.9).abs() < 2.0,
            "{w}x{h}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
