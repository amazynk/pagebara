//! Redaction of real-world files: public manuals and data sheets that happen to be on the
//! development machine. Each is skipped when missing.

mod common;

use pagebara_redact::{
    ApplyOptions, Mark, Pattern, Rect, Sanitize, add_mark, apply, find, sanitize, save, verify,
};
use pagebara_render::{Annotations, PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;

struct Case {
    path: &'static str,
    /// Text to redact on the given pages; checked absent from the whole file when `strict`.
    text: Option<(&'static str, &'static [usize], bool)>,
    /// Rectangles (page, area).
    rects: &'static [(usize, [f64; 4])],
    /// Render scale for comparing: JPEG images are compared at their own resolution, as the
    /// renderer decodes them reduced when drawn smaller (and a blanked copy is Flate).
    scale: f32,
}

const CASES: &[Case] = &[
    Case {
        path: "/home/user/projects/ols/LuaManualEn.pdf",
        text: Some(("Lua", &[0, 1, 2, 5], false)),
        rects: &[(3, [72.0, 300.0, 400.0, 500.0])],
        scale: 1.0,
    },
    Case {
        path: "/home/user/projects/kart/GoldenMotor FOC Motor Controller Guide.pdf",
        text: Some(("Golden", &[0, 1, 2], false)),
        rects: &[(1, [100.0, 300.0, 500.0, 450.0])],
        scale: 1.0,
    },
    Case {
        path: "/home/user/projects/fileservice/dunt_api.pdf",
        text: Some(("api", &[0, 1], false)),
        rects: &[(2, [50.0, 200.0, 550.0, 400.0])],
        scale: 1.0,
    },
    Case {
        path: "/home/user/projects/pr570/NEWPR570.pdf",
        text: None,
        rects: &[(0, [100.0, 500.0, 400.0, 700.0])],
        scale: 4.2,
    },
    Case {
        path: "/home/user/projects/pr570/PR570-OSM.pdf",
        text: Some(("SENCORE", &[0, 1], false)),
        rects: &[(0, [150.0, 300.0, 450.0, 420.0])],
        scale: 1.0,
    },
    Case {
        path: "/home/user/projects/rubidium/rfs_12pg.pdf",
        text: Some(("Rubidium", &[0, 1], false)),
        rects: &[(0, [50.0, 100.0, 350.0, 300.0])],
        scale: 4.2,
    },
    Case {
        path: "/home/user/projects/rubidium/Model FE-5680A Rubidium Atomic Frequency Standards from Frequency Electronics, Inc_.pdf",
        text: Some(("Rubidium", &[0, 1], true)),
        rects: &[],
        scale: 1.0,
    },
];

fn render(doc: &Document, page: usize, scale: f32) -> (u32, u32, Vec<u8>) {
    let r = Renderer::new(doc.clone());
    let (w, h) = r.page_size(page).unwrap();
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: (w * scale).round() as u32,
        h: (h * scale).round() as u32,
    };
    let mut req = RenderRequest::new(scale, rect);
    req.annotations = Annotations::None;
    let img = r.render(page, &req).unwrap();
    (img.width, img.height, img.data)
}

fn device_box(
    page: &pagebara_pages::Page,
    r: &Rect,
    scale: f32,
    margin: i64,
) -> (i64, i64, i64, i64) {
    let c = page.crop_box;
    let m = pagebara_render::page_matrix(
        pagebara_content::Rect::new(c.x0 as f32, c.y0 as f32, c.x1 as f32, c.y1 as f32),
        page.rotate,
        scale,
        0,
        0,
    );
    let ps: Vec<_> = r
        .corners()
        .iter()
        .map(|p| m.apply(pagebara_content::Point::new(p.x as f32, p.y as f32)))
        .collect();
    let b = pagebara_content::Rect::from_points(&ps);
    (
        b.x0.floor() as i64 - margin,
        b.y0.floor() as i64 - margin,
        b.x1.ceil() as i64 + margin,
        b.y1.ceil() as i64 + margin,
    )
}

#[test]
fn public_files() {
    for case in CASES {
        let Ok(bytes) = std::fs::read(case.path) else {
            eprintln!("skipping missing {}", case.path);
            continue;
        };
        eprintln!("{}", case.path);
        let mut before = Document::open(bytes).unwrap();
        if !before.permissions().modify {
            // Protected against changes: refused without the owner password. The rest
            // runs on a copy qpdf decrypted, for its JBIG2 scans and OCR text.
            let mut d = before.clone();
            assert!(matches!(
                apply(&mut d, &ApplyOptions::default()),
                Err(pagebara_redact::Error::NotPermitted)
            ));
            let tmp = std::env::temp_dir()
                .join(format!("pagebara-redact-dec-{}.pdf", std::process::id()));
            let ok = std::process::Command::new("qpdf")
                .arg("--decrypt")
                .arg(case.path)
                .arg(&tmp)
                .status()
                .is_ok_and(|s| s.success() || s.code() == Some(3));
            let Some(dec) = ok.then(|| std::fs::read(&tmp).ok()).flatten() else {
                continue;
            };
            let _ = std::fs::remove_file(&tmp);
            before = Document::open(dec).unwrap();
        }
        let mut doc = before.clone();
        let tree = pagebara_pages::PageTree::load(&doc);
        let mut secrets = Vec::new();
        if let Some((t, pages, strict)) = case.text {
            let hits = find(&doc, &Pattern::text(t), Some(pages));
            assert!(!hits.is_empty(), "{t} not found");
            for h in hits {
                add_mark(&mut doc, tree.pages()[h.page].r, &h.mark()).unwrap();
            }
            if strict {
                secrets.push(t);
            }
        }
        for (p, r) in case.rects {
            let m = Mark {
                overlay_text: Some("REDACTED".into()),
                ..Mark::rect(Rect::new(r[0], r[1], r[2], r[3]))
            };
            add_mark(&mut doc, tree.pages()[*p].r, &m).unwrap();
        }
        let report = apply(&mut doc, &ApplyOptions::default()).unwrap();
        sanitize(&mut doc, &Sanitize::default());
        let out = save(&doc).unwrap();
        let v = verify(&out, None, &report.regions(), &secrets);
        assert!(v.is_clean(), "{}: {v:#?}", case.path);
        common::external_checks(&out, &secrets);
        let after = Document::open(out).unwrap();
        for pr in &report.pages {
            let page = &tree.pages()[pr.page];
            let boxes: Vec<_> = pr
                .regions
                .iter()
                .map(|r| {
                    device_box(
                        page,
                        &r.bbox(),
                        case.scale,
                        (4.0 * case.scale).ceil() as i64,
                    )
                })
                .collect();
            let n = common::diff_outside(
                &render(&before, pr.page, case.scale),
                &render(&after, pr.page, case.scale),
                &boxes,
                48,
            );
            assert_eq!(
                n, 0,
                "{} page {}: renders differ outside regions",
                case.path, pr.page
            );
        }
        // (Curves cut at a region flatten at other points: edge anti-aliasing moves.)
        // A page without marks renders the same.
        if let Some(other) = (0..tree.len()).find(|i| report.pages.iter().all(|p| p.page != *i)) {
            let n = common::diff_outside(
                &render(&before, other, 1.0),
                &render(&after, other, 1.0),
                &[],
                2,
            );
            assert_eq!(n, 0, "{} page {other} changed", case.path);
        }
    }
}
