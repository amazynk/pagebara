//! Structural checks on public manuals found on the developer's machine; skipped elsewhere.
//! Nothing is copied from them and only page counts are printed.

mod common;

use std::process::Command;

use common::*;
use pagebara_pages::{Access, Error, PageTree};

const MANUALS: [&str; 9] = [
    "/home/user/projects/ols/LuaManualEn.pdf",
    "/home/user/projects/kart/GoldenMotor FOC Motor Controller Guide.pdf",
    "/home/user/projects/rubidium/Model FE-5680A Rubidium Atomic Frequency Standards from Frequency Electronics, Inc_.pdf",
    "/home/user/projects/rubidium/rfs_12pg.pdf",
    "/home/user/projects/pr570/NEWPR570.pdf",
    "/home/user/projects/pr570/PR570-OSM.pdf",
    "/home/user/projects/pr570/SENCORE PR570.pdf",
    "/home/user/projects/fileservice/dunt_api.pdf",
    "/home/user/projects/pagebara/no-such-file.pdf",
];

fn qpdf_pages(path: &str) -> Option<usize> {
    let out = Command::new("qpdf")
        .arg("--show-npages")
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8(out.stdout).ok()?.trim().parse().ok()
}

#[test]
fn manuals() {
    for (i, path) in MANUALS.iter().enumerate() {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let doc = open(bytes);
        let t0 = std::time::Instant::now();
        let n = PageTree::load(&doc).len();
        eprintln!("manual {i}: {n} pages, tree in {:?}", t0.elapsed());
        if let Some(q) = qpdf_pages(path) {
            assert_eq!(n, q, "manual {i}");
        }

        let mut ed = doc.clone();
        if pagebara_pages::permitted(&doc, Access::Assemble) {
            pagebara_pages::reorder(&mut ed, &(0..n).rev().collect::<Vec<_>>()).unwrap();
            pagebara_pages::rotate(&mut ed, &[0], 1).unwrap();
            if n > 1 {
                pagebara_pages::delete(&mut ed, &[n - 1]).unwrap();
            }
            pagebara_pages::duplicate(&mut ed, &[0]).unwrap();
            for d in saved(&ed, "") {
                assert_eq!(check_tree(&d), n.max(2), "manual {i}");
                check_structure(&d);
                check_threads(&d);
            }
        } else {
            eprintln!("manual {i}: assembly not permitted");
            assert!(matches!(
                pagebara_pages::rotate(&mut ed, &[0], 1),
                Err(Error::NotPermitted(Access::Assemble))
            ));
        }
        if !pagebara_pages::permitted(&doc, Access::Extract) {
            eprintln!("manual {i}: extraction not permitted");
            continue;
        }

        let sel: Vec<usize> = (n.saturating_sub(2)..n).collect();
        let ex = pagebara_pages::extract(&doc, &sel).unwrap();
        for d in saved(&ex, "") {
            assert_eq!(check_tree(&d), sel.len(), "manual {i}");
            check_structure(&d);
            check_threads(&d);
        }
        let merged = pagebara_pages::merge(&[&doc, &doc]).unwrap();
        for d in saved(&merged, "") {
            assert_eq!(check_tree(&d), 2 * n, "manual {i}");
            check_structure(&d);
            check_threads(&d);
        }
        assert!(ex.save_full().unwrap().len() < doc.source().len() * 2);
    }
}
