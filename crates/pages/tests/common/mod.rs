#![allow(dead_code, unused_imports)]

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;

use pagebara_pages::PageTree;
use pagebara_syntax::{Dict, Document, Error, ObjRef, Object};

mod checks;
mod docs;
pub use checks::*;
pub use docs::*;

/// A classic file from numbered object bodies, with a correct xref table.
pub fn pdf(objs: &[(u32, String)], root: u32) -> Vec<u8> {
    pdf_with(objs, &format!("/Root {root} 0 R"))
}

/// Like [`pdf`] with the trailer entries given (besides `/Size`).
pub fn pdf_with(objs: &[(u32, String)], trailer: &str) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let size = objs.iter().map(|o| o.0).max().unwrap_or(0) + 1;
    let mut offs = vec![None; size as usize];
    for (n, body) in objs {
        offs[*n as usize] = Some(out.len());
        out.extend(format!("{n} 0 obj\n{body}\nendobj\n").bytes());
    }
    let x = out.len();
    out.extend(format!("xref\n0 {size}\n").bytes());
    for o in offs {
        match o {
            Some(o) => out.extend(format!("{o:010} 00000 n\r\n").bytes()),
            None => out.extend(b"0000000000 65535 f\r\n"),
        }
    }
    out.extend(format!("trailer\n<</Size {size}{trailer}>>\nstartxref\n{x}\n%%EOF\n").bytes());
    out
}

pub fn stream(data: &str) -> String {
    format!("<</Length {}>>stream\n{data}\nendstream", data.len())
}

pub fn content(label: &str) -> String {
    stream(&format!("BT /F1 12 Tf 72 72 Td ({label}) Tj ET"))
}

/// Six pages in two subtrees with inherited attributes, outlines, links, named
/// destinations, form fields and page labels. Pages are objects 5-10, showing
/// `Page 1`..`Page 6`; their labels are i, ii, 1, 2, 3, 4.
pub fn rich() -> Vec<u8> {
    let mut o: Vec<(u32, String)> = [
        (1, "<</Type/Catalog/Pages 2 0 R/Outlines 20 0 R/Names<</Dests 30 0 R>>/Dests 31 0 R/AcroForm<</Fields[40 0 R 44 0 R]/DA(/Helv 0 Tf 0 g)>>/PageLabels<</Nums[0<</S/r>>2<</S/D>>]>>/OpenAction[5 0 R/Fit]>>"),
        (2, "<</Type/Pages/Kids[3 0 R 4 0 R]/Count 6/Resources 50 0 R/MediaBox[0 0 612 792]>>"),
        (3, "<</Type/Pages/Parent 2 0 R/Kids[5 0 R 6 0 R 7 0 R]/Count 3/Rotate 90>>"),
        (4, "<</Type/Pages/Parent 2 0 R/Kids[8 0 R 9 0 R 10 0 R]/Count 3/MediaBox[0 0 300 400]/Resources<</Font<</F1 51 0 R>>/ProcSet[/PDF/Text]>>>>"),
        (5, "<</Type/Page/Parent 3 0 R/Contents 11 0 R/Annots[60 0 R 61 0 R]>>"),
        (6, "<</Type/Page/Parent 3 0 R/Contents 12 0 R/Annots[41 0 R]>>"),
        (7, "<</Type/Page/Parent 3 0 R/Contents 13 0 R/CropBox[10 10 500 700]>>"),
        (8, "<</Type/Page/Parent 4 0 R/Contents 14 0 R/Annots[44 0 R]>>"),
        (9, "<</Type/Page/Parent 4 0 R/Contents 15 0 R/Annots[42 0 R]>>"),
        (10, "<</Type/Page/Parent 4 0 R/Contents 16 0 R>>"),
        (20, "<</Type/Outlines/First 21 0 R/Last 22 0 R/Count 2>>"),
        (21, "<</Title(One)/Parent 20 0 R/Next 22 0 R/Dest[5 0 R/Fit]>>"),
        (22, "<</Title(Four)/Parent 20 0 R/Prev 21 0 R/A<</S/GoTo/D[8 0 R/Fit]>>>>"),
        (30, "<</Kids[32 0 R]>>"),
        (31, "<</d2[6 0 R/Fit]>>"),
        (32, "<</Limits[(chap1)(chap4)]/Names[(chap1)[5 0 R/Fit](chap4)[8 0 R/XYZ 0 0 0]]>>"),
        (40, "<</FT/Tx/T(name)/Kids[41 0 R 42 0 R]>>"),
        (41, "<</Type/Annot/Subtype/Widget/Parent 40 0 R/P 6 0 R/Rect[10 10 100 30]>>"),
        (42, "<</Type/Annot/Subtype/Widget/Parent 40 0 R/P 9 0 R/Rect[10 10 100 30]>>"),
        (44, "<</Type/Annot/Subtype/Widget/FT/Btn/T(check)/P 8 0 R/Rect[0 0 10 10]>>"),
        (50, "<</Font<</F1 51 0 R>>>>"),
        (51, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>"),
        (60, "<</Type/Annot/Subtype/Link/Rect[0 0 50 50]/Dest[9 0 R/Fit]/P 5 0 R>>"),
        (61, "<</Type/Annot/Subtype/Link/Rect[0 50 50 100]/A<</S/GoTo/D(chap4)>>/P 5 0 R>>"),
    ]
    .into_iter()
    .map(|(n, s)| (n, s.to_string()))
    .collect();
    for i in 0..6 {
        o.push((11 + i, content(&format!("Page {}", i + 1))));
    }
    o.sort_by_key(|x| x.0);
    pdf(&o, 1)
}

/// `n` pages directly under the root, showing `Page 1`..`Page n`.
pub fn flat(n: u32) -> Vec<u8> {
    let mut o = vec![
        (1, "<</Type/Catalog/Pages 2 0 R>>".to_string()),
        (
            3,
            "<</Font<</F1<</Type/Font/Subtype/Type1/BaseFont/Courier>>>>>>".to_string(),
        ),
    ];
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 10 + 2 * i)).collect();
    o.push((
        2,
        format!(
            "<</Type/Pages/Kids[{}]/Count {n}/Resources 3 0 R/MediaBox[0 0 200 300]>>",
            kids.join(" ")
        ),
    ));
    for i in 0..n {
        o.push((
            10 + 2 * i,
            format!("<</Type/Page/Parent 2 0 R/Contents {} 0 R>>", 11 + 2 * i),
        ));
        o.push((11 + 2 * i, content(&format!("Page {}", i + 1))));
    }
    o.sort_by_key(|x| x.0);
    pdf(&o, 1)
}

pub fn open(bytes: Vec<u8>) -> Document {
    open_pw(bytes, "")
}

pub fn open_pw(bytes: Vec<u8>, pw: &str) -> Document {
    match Document::open(bytes) {
        Ok(d) => d,
        Err(Error::PasswordRequired(l)) => l.unlock(pw).expect("password accepted"),
        Err(e) => panic!("{e}"),
    }
}

pub fn dict(doc: &Document, r: ObjRef) -> Dict {
    doc.get(r)
        .and_then(|o| o.as_dict().cloned())
        .unwrap_or_default()
}

pub fn catalog(doc: &Document) -> Dict {
    doc.catalog().unwrap().as_dict().unwrap().clone()
}

/// The `Page N` text each page shows, in order.
pub fn texts(doc: &Document) -> Vec<String> {
    PageTree::load(doc)
        .pages()
        .iter()
        .map(|p| {
            let d = dict(doc, p.r);
            let c = doc.get_in(&d, b"Contents");
            let data = doc.decode(c.as_stream().expect("contents")).unwrap().data;
            let s = String::from_utf8_lossy(&data).into_owned();
            let a = s.find('(').unwrap() + 1;
            s[a..a + s[a..].find(')').unwrap()].to_string()
        })
        .collect()
}

/// Checks `/Parent`, `/Count`, fan-out and depth of the page tree; returns the leaves.
pub fn check_tree(doc: &Document) -> usize {
    let root = catalog(doc).get(b"Pages").unwrap().as_objref().unwrap();
    let mut seen = HashSet::new();
    fn walk(
        doc: &Document,
        r: ObjRef,
        parent: Option<ObjRef>,
        depth: u32,
        seen: &mut HashSet<u32>,
    ) -> usize {
        assert!(seen.insert(r.num), "{r:?} twice");
        assert!(depth < 8, "tree too deep");
        let d = dict(doc, r);
        assert_eq!(
            d.get(b"Parent").and_then(Object::as_objref),
            parent,
            "parent of {r:?}"
        );
        if d.name(b"Type") == Some(b"Page") {
            return 1;
        }
        assert_eq!(d.name(b"Type"), Some(&b"Pages"[..]));
        let kids = doc.get_in(&d, b"Kids");
        let kids = kids.as_array().unwrap();
        assert!(kids.len() <= 32, "{} kids", kids.len());
        let n: usize = kids
            .iter()
            .map(|k| walk(doc, k.as_objref().unwrap(), Some(r), depth + 1, seen))
            .sum();
        assert_eq!(d.int(b"Count"), Some(n as i64), "count of {r:?}");
        n
    }
    walk(doc, root, None, 0, &mut seen)
}

/// The document after saving incrementally and fully, each reopened.
pub fn saved(doc: &Document, pw: &str) -> Vec<Document> {
    let mut v = Vec::new();
    if let Ok(b) = doc.save_incremental() {
        v.push(open_pw(b, pw));
    }
    let full = doc.save_full().unwrap();
    qpdf_check(&full, pw);
    v.push(open_pw(full, pw));
    v
}

fn qpdf() -> Option<&'static str> {
    let p = "/usr/bin/qpdf";
    std::path::Path::new(p).exists().then_some(p).or_else(|| {
        Command::new("qpdf")
            .arg("--version")
            .output()
            .ok()
            .map(|_| "qpdf")
    })
}

/// Runs `qpdf --check` when qpdf is installed.
pub fn qpdf_check(bytes: &[u8], pw: &str) {
    let Some(q) = qpdf() else { return };
    let path = tmp(&format!(
        "check-{}.pdf",
        std::process::id() as u64 * 1000 + rand_suffix()
    ));
    std::fs::write(&path, bytes).unwrap();
    let out = Command::new(q)
        .arg(format!("--password={pw}"))
        .arg("--check")
        .arg(&path)
        .output()
        .unwrap();
    let _ = std::fs::remove_file(&path);
    assert!(
        out.status.success(),
        "qpdf --check failed:\n{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

pub fn tmp(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(name)
}

pub fn fixture(name: &str) -> Vec<u8> {
    std::fs::read(format!(
        "{}/tests/fixtures/{name}",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
}

/// The outline as `title -> page index` lines, indented by level; closed items end in `+`.
/// Checks the links and counts on the way.
pub fn outline(doc: &Document) -> Vec<String> {
    let index = PageTree::load(doc).index();
    let cat = catalog(doc);
    let root = doc.get_in(&cat, b"Outlines");
    let Some(root) = root.as_dict() else {
        return vec![];
    };
    fn level(
        doc: &Document,
        index: &pagebara_syntax::xref::NumMap<usize>,
        first: Option<ObjRef>,
        parent: ObjRef,
        depth: usize,
        out: &mut Vec<String>,
    ) -> i64 {
        let mut item = first;
        let mut prev = None;
        let mut visible = 0;
        while let Some(r) = item {
            let d = dict(doc, r);
            assert_eq!(d.get(b"Parent").and_then(Object::as_objref), Some(parent));
            assert_eq!(d.get(b"Prev").and_then(Object::as_objref), prev);
            let dest = d
                .get(b"Dest")
                .map(|o| (*doc.resolve(o)).clone())
                .or_else(|| {
                    let a = doc.get_in(&d, b"A");
                    a.as_dict().and_then(|a| a.get(b"D").cloned())
                });
            let page = match &dest {
                Some(Object::String(s)) => format!("({})", String::from_utf8_lossy(&s.bytes)),
                _ => dest
                    .as_ref()
                    .and_then(|o| o.as_array()?.first()?.as_objref())
                    .and_then(|p| index.get(&p.num))
                    .map_or("-".to_string(), |i| i.to_string()),
            };
            let title =
                String::from_utf8_lossy(d.get(b"Title").unwrap().as_string().unwrap()).into_owned();
            let at = out.len();
            out.push(format!("{}{title} -> {page}", "  ".repeat(depth)));
            visible += 1;
            let first = d.get(b"First").and_then(Object::as_objref);
            if first.is_some() {
                let n = level(doc, index, first, r, depth + 1, out);
                let c = d.int(b"Count").unwrap();
                assert_eq!(c.abs(), n, "count of {title}");
                if c < 0 {
                    out[at].push('+');
                } else {
                    visible += n;
                }
            }
            prev = Some(r);
            item = d.get(b"Next").and_then(Object::as_objref);
        }
        visible
    }
    let root_ref = cat.get(b"Outlines").unwrap().as_objref().unwrap();
    let first = root.get(b"First").and_then(Object::as_objref);
    let mut out = Vec::new();
    let n = level(doc, &index, first, root_ref, 0, &mut out);
    if let Some(c) = root.int(b"Count") {
        assert_eq!(c, n, "outline count");
    }
    out
}

/// The keys of the catalog's `/Names` tree `key`, sorted.
pub fn name_keys(doc: &Document, key: &[u8]) -> Vec<String> {
    fn walk(doc: &Document, n: &Object, v: &mut Vec<String>) {
        let n = doc.resolve(n);
        let Some(n) = n.as_dict() else { return };
        if let Some(a) = doc.get_in(n, b"Names").as_array() {
            for p in a.chunks(2) {
                let k = doc.resolve(&p[0]);
                v.push(String::from_utf8_lossy(k.as_string().unwrap()).into_owned());
            }
        }
        if let Some(k) = doc.get_in(n, b"Kids").as_array() {
            for k in k {
                walk(doc, k, v);
            }
        }
    }
    let cat = catalog(doc);
    let names = doc.get_in(&cat, b"Names");
    let mut v = Vec::new();
    if let Some(root) = names.as_dict().and_then(|n| n.get(key)) {
        walk(doc, root, &mut v);
    }
    v.sort();
    v
}
