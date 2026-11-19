use std::path::{Path, PathBuf};

use pagebara_syntax::Permissions;

use super::*;
use crate::test_pdf;

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../syntax/tests/fixtures/crypt")
        .join(name)
}

fn ready(o: Opened) -> Session {
    match o {
        Opened::Ready(s) => *s,
        Opened::Locked(_) => panic!("locked"),
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("pagebara-session-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

#[test]
fn opens_plain_file() {
    let s = ready(open_bytes(Some("dir/Report.pdf".into()), test_pdf::sample()).unwrap());
    assert_eq!(s.title(), "Report.pdf");
    assert_eq!(s.info_title(), "Sample Title");
    assert_eq!(s.page_count(), 3);
    assert_eq!(s.protection(), "");
    assert!(!s.is_dirty());
    assert!(s.info().rows.iter().any(|(k, v)| k == "Pages" && v == "3"));
}

#[test]
fn password_flow() {
    let Opened::Locked(p) = open_file(&fixture("aes-256-unicode.pdf")).unwrap() else {
        panic!("opened without a password")
    };
    assert_eq!(p.title(), "aes-256-unicode.pdf");
    let p = p.unlock("wrong").unwrap_err();
    let s = p.unlock("pässwörd 密码 🔐").unwrap();
    assert!(s.doc().is_encrypted());
    assert!(s.page_count() >= 1);
}

#[test]
fn rejects_non_pdf() {
    let e = open_bytes(None, b"hello".to_vec()).unwrap_err();
    assert_eq!(e.to_string(), "This is not a PDF file.");
}

#[test]
fn undo_redo_and_dirty() {
    let mut s = ready(open_bytes(None, test_pdf::sample()).unwrap());
    let layout = s.layout_rev;
    let (_, ch) = s
        .edit("Remove page", |d| pagebara_pages::delete(d, &[0]))
        .unwrap();
    assert!(ch.pages);
    assert_eq!(s.page_count(), 2);
    assert!(s.is_dirty());
    assert_ne!(s.layout_rev, layout);
    assert_eq!(s.undo_label(), Some("Remove page"));
    assert!(s.undo().unwrap().pages);
    assert_eq!(s.page_count(), 3);
    assert!(!s.is_dirty());
    assert!(s.redo().is_some());
    assert_eq!(s.page_count(), 2);
    // A failed edit changes nothing.
    let err = s.edit("Remove all", |d| pagebara_pages::delete(d, &[0, 1]));
    assert!(err.is_err());
    assert_eq!(s.page_count(), 2);
    assert!(s.can_undo() && !s.can_redo());
}

#[test]
fn annotation_edit_keeps_layout() {
    let mut s = ready(open_bytes(None, test_pdf::sample()).unwrap());
    let layout = s.layout_rev;
    let page = s.page_ref(0).unwrap();
    let a = pagebara_annot::Annotation::new(
        pagebara_annot::Kind::Square { interior: None },
        pagebara_annot::Rect::new(10.0, 10.0, 50.0, 50.0),
    );
    let (_, ch) = s.edit("Add", |d| pagebara_annot::add(d, page, &a)).unwrap();
    assert!(!ch.pages);
    assert_eq!(s.layout_rev, layout);
    assert_eq!(crate::annots::on_page(s.doc(), page).len(), 1);
}

#[test]
fn saves_incrementally_and_reopens() {
    let path = temp("saved.pdf");
    std::fs::write(&path, test_pdf::sample()).unwrap();
    let mut s = ready(open_file(&path).unwrap());
    let page = s.page_ref(1).unwrap();
    let a = pagebara_annot::Annotation::new(
        pagebara_annot::Kind::Circle { interior: None },
        pagebara_annot::Rect::new(10.0, 10.0, 50.0, 50.0),
    );
    s.edit("Add", |d| pagebara_annot::add(d, page, &a)).unwrap();
    let done = s
        .save_job(path.clone(), SaveMode::Incremental)
        .run()
        .unwrap();
    assert!(!done.full);
    assert!(done.doc.is_some());
    s.saved(done);
    assert!(!s.is_dirty());
    assert!(!s.doc().is_modified());
    let again = ready(open_file(&path).unwrap());
    let page = again.page_ref(1).unwrap();
    assert_eq!(crate::annots::on_page(again.doc(), page).len(), 1);
    // Undo after saving makes the document differ from the file again.
    s.undo();
    assert!(s.is_dirty());
}

#[test]
fn protect_then_save_needs_full_and_reopens_with_password() {
    let path = temp("protected.pdf");
    let mut s = ready(open_bytes(Some(path.clone()), test_pdf::sample()).unwrap());
    let mut p = Permissions::ALL;
    p.print = false;
    s.edit("Protect", |d| d.set_password("ü 密", "owner 🔐", p))
        .unwrap();
    s.remember_password("owner 🔐");
    let done = s
        .save_job(path.clone(), SaveMode::Incremental)
        .run()
        .unwrap();
    assert!(done.full);
    let doc = done.doc.as_ref().unwrap();
    assert!(doc.is_encrypted() && doc.is_owner());
    s.saved(done);
    let Opened::Locked(l) = open_file(&path).unwrap() else {
        panic!("not protected")
    };
    let user = l.unlock("ü 密").unwrap();
    assert!(!user.doc().permissions().print);
    assert_eq!(user.protection(), "Protected: printing not allowed");
}

#[test]
fn flattens_signatures_on_save() {
    let mut s = ready(open_bytes(None, test_pdf::sample()).unwrap());
    let page = s.page_ref(0).unwrap();
    let px = vec![0u8; 4 * 4 * 4];
    let img = pagebara_annot::RgbaImage {
        width: 4,
        height: 4,
        pixels: &px,
    };
    let opts = pagebara_annot::StampOptions {
        subject: Some("Signature".into()),
        ..Default::default()
    };
    s.edit("Sign", |d| {
        pagebara_annot::add_image_stamp(
            d,
            page,
            img,
            pagebara_annot::Rect::new(0.0, 0.0, 40.0, 40.0),
            &opts,
        )
    })
    .unwrap();
    let mut d = s.doc().clone();
    assert_eq!(flatten_signatures(&mut d), 1);
    assert!(crate::annots::on_page(&d, page).is_empty());
}

#[test]
fn labels() {
    assert_eq!(page_size_label(595.0, 842.0), "A4 · 210 × 297 mm");
    assert_eq!(page_size_label(842.0, 595.0), "A4 · 297 × 210 mm");
    assert_eq!(page_size_label(612.0, 792.0), "Letter · 216 × 279 mm");
    assert_eq!(page_size_label(100.0, 100.0), "35 × 35 mm");
    let mut p = Permissions::ALL;
    assert_eq!(protection_label(false, p), "");
    assert_eq!(protection_label(true, p), "Protected");
    p.print = false;
    assert_eq!(protection_label(true, p), "Protected: printing not allowed");
    p.copy = false;
    p.modify = false;
    assert_eq!(
        protection_label(true, p),
        "Protected: printing, copying and editing not allowed"
    );
}

#[test]
fn text_edits_reindex_and_undo_restores_the_text() {
    let mut s = ready(open_bytes(None, test_pdf::text(&["Before"])).unwrap());
    assert!(s.text.page(0).text.contains("Before"));
    let page = s.page_ref(0).unwrap();
    s.edit_text("Recognise", |d| -> Result<(), pagebara_syntax::Error> {
        let content = b"BT /F1 12 Tf 72 700 Td (After) Tj ET".to_vec();
        let c = d.add(pagebara_syntax::Stream::new(
            pagebara_syntax::Dict::new(),
            content,
        ));
        let mut pd = d.get(page).unwrap().as_dict().unwrap().clone();
        let old = pd.get(b"Contents").cloned().unwrap();
        pd.set(
            b"Contents",
            pagebara_syntax::Object::Array(vec![old, pagebara_syntax::Object::Ref(c)]),
        );
        d.set(page, pd);
        Ok(())
    })
    .unwrap();
    assert!(s.text.page(0).text.contains("After"));
    s.undo();
    assert!(!s.text.page(0).text.contains("After"));
    s.redo();
    assert!(s.text.page(0).text.contains("After"));
}

/// Five pages, each naming itself, with every page's text indexed.
fn indexed() -> Session {
    let names = ["alpha", "bravo", "charlie", "delta", "echo"].map(|n| [n]);
    let pages: Vec<&[&str]> = names.iter().map(|n| &n[..]).collect();
    let s = ready(open_bytes(None, test_pdf::pages(&pages)).unwrap());
    assert!(s.text.fill(0..5, &|| false));
    s
}

/// Each page's folded text as the index has it, `None` where it is to be extracted again;
/// what it has must be what the page says now.
fn folded(s: &Session) -> Vec<Option<String>> {
    let fresh = TextIndex::new(s.doc().clone(), s.tree.refs());
    (0..s.page_count())
        .map(|i| {
            let f = s.text.folded(i)?;
            assert_eq!(
                f,
                crate::text_index::fold_text(&fresh.page(i).text),
                "page {i}"
            );
            Some(f.to_owned())
        })
        .collect()
}

fn known(names: &[&str]) -> Vec<Option<String>> {
    names.iter().map(|n| Some((*n).to_owned())).collect()
}

#[test]
fn page_edits_keep_the_text_of_unchanged_pages() {
    let mut s = indexed();
    // Page 3's full text is kept too.
    s.text.page(2);
    let all = ["alpha", "bravo", "charlie", "delta", "echo"];
    let (_, ch) = s
        .edit("Rotate", |d| pagebara_pages::rotate(d, &[1, 2], 1))
        .unwrap();
    assert!(ch.pages);
    assert_eq!(folded(&s), known(&all));
    assert!(s.text.cached(2).is_some());

    let rect = pagebara_pages::Rect {
        x0: 10.0,
        y0: 10.0,
        x1: 300.0,
        y1: 400.0,
    };
    s.edit("Crop", |d| pagebara_pages::crop(d, &[0, 2], Some(rect)))
        .unwrap();
    assert_eq!(folded(&s), known(&all));

    s.edit("Move", |d| pagebara_pages::move_pages(d, &[0], 5))
        .unwrap();
    let moved = ["bravo", "charlie", "delta", "echo", "alpha"];
    assert_eq!(folded(&s), known(&moved));
    assert!(s.text.cached(1).is_some_and(|t| t.text.contains("charlie")));

    s.edit("Delete", |d| pagebara_pages::delete(d, &[2]))
        .unwrap();
    assert_eq!(folded(&s), known(&["bravo", "charlie", "echo", "alpha"]));
    assert!(s.text.is_indexed());

    // Undo brings the page back with its text.
    s.undo();
    assert_eq!(folded(&s), known(&moved));

    // Inserted and duplicated pages are new page objects: only they are extracted again.
    let other = Document::open(test_pdf::text(&["foxtrot"])).unwrap();
    s.edit("Insert", |d| pagebara_pages::insert(d, 1, &other, &[0]))
        .unwrap();
    let mut want = known(&moved);
    want.insert(1, None);
    assert_eq!(folded(&s), want);
    s.edit("Duplicate", |d| pagebara_pages::duplicate(d, &[0]))
        .unwrap();
    want.insert(1, None);
    assert_eq!(folded(&s), want);
}

#[test]
fn content_edits_extract_their_pages_again() {
    let mut s = indexed();
    let page = s.page_ref(1).unwrap();
    // A text layer added to page 2, as recognising text does.
    s.edit_text("Recognise", |d| -> Result<(), pagebara_syntax::Error> {
        let c = d.add(pagebara_syntax::Stream::new(
            pagebara_syntax::Dict::new(),
            b"BT /F1 12 Tf 72 700 Td (golf) Tj ET".to_vec(),
        ));
        let mut pd = d.get(page).unwrap().as_dict().unwrap().clone();
        let old = pd.get(b"Contents").cloned().unwrap();
        pd.set(
            b"Contents",
            pagebara_syntax::Object::Array(vec![old, pagebara_syntax::Object::Ref(c)]),
        );
        d.set(page, pd);
        Ok(())
    })
    .unwrap();
    let mut want = known(&["alpha", "bravo", "charlie", "delta", "echo"]);
    want[1] = None;
    assert_eq!(folded(&s), want);
    assert!(s.text.page(1).text.contains("golf"));
    want[1] = Some("bravo golf".into());

    // A content stream rewritten in place (same object, new data), as redaction does.
    let page = s.page_ref(3).unwrap();
    s.edit_text("Redact", |d| -> Result<(), pagebara_syntax::Error> {
        let pd = d.get(page).unwrap();
        let c = pd
            .as_dict()
            .unwrap()
            .get(b"Contents")
            .unwrap()
            .as_objref()
            .unwrap();
        d.set(
            c,
            pagebara_syntax::Stream::new(pagebara_syntax::Dict::new(), b"".to_vec()),
        );
        Ok(())
    })
    .unwrap();
    want[3] = None;
    assert_eq!(folded(&s), want);
    assert!(s.text.page(3).text.is_empty());

    // A shared font changed: every page uses it.
    s.edit_text("Font", |d| -> Result<(), pagebara_syntax::Error> {
        let font = pagebara_syntax::ObjRef::new(3, 0);
        let f = d.get(font).unwrap().as_ref().clone();
        d.set(font, f);
        Ok(())
    })
    .unwrap();
    assert!(folded(&s).iter().all(Option::is_none));
}
