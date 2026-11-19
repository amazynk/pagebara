mod common;

use common::*;
use pagebara_annot::*;
use pagebara_syntax::{Dict, Document, ObjRef, Object};

const PAGE: ObjRef = ObjRef::new(3, 0);

fn link(action: LinkAction, rect: Rect) -> Annotation {
    Annotation::new(
        Kind::Link {
            action: Some(action),
            quads: vec![],
        },
        rect,
    )
}

fn action_of(a: &Annotation) -> Option<&LinkAction> {
    match &a.kind {
        Kind::Link { action, .. } => action.as_ref(),
        _ => None,
    }
}

fn reopen(doc: &Document, name: &str) -> (Document, Vec<u8>) {
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, name);
    (Document::open(bytes.clone()).unwrap(), bytes)
}

#[test]
fn writes_and_reads_link_targets() {
    let mut doc = Document::open(one_page(400, 400, "")).unwrap();
    let to = |view| {
        LinkAction::GoTo(Dest::Explicit(Destination {
            page: DestPage::Ref(PAGE),
            view,
        }))
    };
    let actions = [
        LinkAction::Uri("https://example.com/a?b=c".into()),
        LinkAction::GoTo(Dest::Explicit(Destination::page(PAGE))),
        to(View::Xyz {
            left: Some(10.0),
            top: Some(380.5),
            zoom: Some(1.5),
        }),
        to(View::Fit),
        to(View::FitH { top: Some(200.0) }),
        to(View::FitV { left: None }),
        to(View::FitR(Rect::new(10.0, 20.0, 110.0, 220.0))),
        to(View::FitB),
        to(View::FitBH { top: None }),
        to(View::FitBV { left: Some(5.0) }),
        LinkAction::GoTo(Dest::Named("chapter-2".into())),
    ];
    let mut refs = Vec::new();
    for (i, act) in actions.iter().enumerate() {
        let y = 10.0 + i as f64 * 30.0;
        refs.push(
            add(
                &mut doc,
                PAGE,
                &link(act.clone(), Rect::new(10.0, y, 100.0, y + 20.0)),
            )
            .unwrap(),
        );
    }
    // A bordered link over two text runs.
    let mut bordered = Annotation::new(
        Kind::Link {
            action: Some(LinkAction::Uri("https://example.org/".into())),
            quads: vec![
                Quad::from_rect(Rect::new(200.0, 300.0, 380.0, 316.0)),
                Quad::from_rect(Rect::new(200.0, 280.0, 260.0, 296.0)),
            ],
        },
        Rect::default(),
    );
    bordered.color = Some(Color::Rgb(0.0, 0.0, 1.0));
    bordered.border.width = 2.0;
    let b = add(&mut doc, PAGE, &bordered).unwrap();

    let (doc, bytes) = reopen(&doc, "links");
    for (r, act) in refs.iter().zip(&actions) {
        let a = pagebara_annot::read(&doc, *r).unwrap();
        assert_eq!(action_of(&a), Some(act));
        // An empty appearance: readers do not draw their own border.
        let d = doc.get(*r).unwrap();
        let ap = doc.get_in(d.as_dict().unwrap(), b"AP");
        let n = doc.resolve(ap.as_dict().unwrap().get(b"N").unwrap());
        assert!(doc.decode(n.as_stream().unwrap()).unwrap().data.is_empty());
    }
    let a = pagebara_annot::read(&doc, b).unwrap();
    assert_eq!(a.rect, Rect::new(200.0, 280.0, 380.0, 316.0));
    assert_eq!(a.kind, bordered.kind);
    // mutool does not paint links at all; poppler paints their appearances.
    if let Some(img) = render_poppler(&bytes, "links") {
        assert!(img.any(199.0, 279.0, 202.0, 316.0, is_blue), "border drawn");
        assert!(
            !img.any(5.0, 5.0, 105.0, 345.0, |p| !is_white(p)),
            "no border on plain links"
        );
    }
}

/// Adds a raw annotation dictionary to the page.
fn raw(doc: &mut Document, d: Dict) -> ObjRef {
    let r = doc.add(d);
    let mut page = doc.get(PAGE).unwrap().as_dict().unwrap().clone();
    let mut list = doc
        .get_in(&page, b"Annots")
        .as_array()
        .map(<[Object]>::to_vec)
        .unwrap_or_default();
    list.push(r.into());
    page.set(b"Annots", list);
    doc.set(PAGE, page);
    r
}

fn raw_link(extra: &[(&[u8], Object)]) -> Dict {
    let mut d = Dict::new();
    d.set(b"Type", Object::name(b"Annot"));
    d.set(b"Subtype", Object::name(b"Link"));
    d.set(b"Rect", Rect::new(0.0, 0.0, 50.0, 20.0).to_object());
    for (k, v) in extra {
        d.set(k, v.clone());
    }
    d
}

fn dict(entries: &[(&[u8], Object)]) -> Object {
    let mut d = Dict::new();
    for (k, v) in entries {
        d.set(k, v.clone());
    }
    Object::Dict(d)
}

#[test]
fn reads_foreign_link_targets() {
    let mut doc = Document::open(one_page(400, 400, "")).unwrap();
    let n = Object::name;
    let s = |t: &str| Object::string(t.as_bytes());
    let fit_h = Object::Array(vec![PAGE.into(), n(b"FitH"), Object::Null]);
    let dest = raw(&mut doc, raw_link(&[(b"Dest", fit_h)]));
    let by_name = raw(&mut doc, raw_link(&[(b"Dest", n(b"Intro"))]));
    let gotor = raw(
        &mut doc,
        raw_link(&[(
            b"A",
            dict(&[
                (b"S", n(b"GoToR")),
                (
                    b"F",
                    dict(&[
                        (b"Type", n(b"Filespec")),
                        (b"F", s("old.pdf")),
                        (b"UF", text_string("Ünïcode.pdf")),
                    ]),
                ),
                (b"D", Object::Array(vec![Object::Int(2), n(b"Fit")])),
                (b"NewWindow", Object::Bool(true)),
            ]),
        )]),
    );
    let launch = raw(
        &mut doc,
        raw_link(&[(
            b"A",
            dict(&[
                (b"S", n(b"Launch")),
                (b"Win", dict(&[(b"F", s("notepad.exe"))])),
            ]),
        )]),
    );
    let js = raw(
        &mut doc,
        raw_link(&[(
            b"A",
            dict(&[(b"S", n(b"JavaScript")), (b"JS", s("app.alert(1)"))]),
        )]),
    );

    // Named destinations: one in the catalog's /Dests, one in a /Names tree with kids.
    let mut cat = doc.catalog().unwrap().as_dict().unwrap().clone();
    let target = Object::Array(vec![
        PAGE.into(),
        n(b"XYZ"),
        Object::Int(0),
        Object::Int(400),
        Object::Null,
    ]);
    cat.set(b"Dests", dict(&[(b"Old", dict(&[(b"D", target.clone())]))]));
    let mut leaf = |lo: &str, hi: &str, names: Vec<Object>| -> Object {
        doc.add(dict(&[
            (b"Limits", Object::Array(vec![s(lo), s(hi)])),
            (b"Names", Object::Array(names)),
        ]))
        .into()
    };
    let kids = Object::Array(vec![
        leaf(
            "A",
            "B",
            vec![s("A"), Object::Array(vec![PAGE.into(), n(b"Fit")])],
        ),
        leaf("Intro", "Z", vec![s("Intro"), target.clone()]),
    ]);
    cat.set(b"Names", dict(&[(b"Dests", dict(&[(b"Kids", kids)]))]));
    doc.set(ObjRef::new(1, 0), cat);

    let (mut doc, _) = reopen(&doc, "foreign-links");
    let get = |doc: &Document, r| pagebara_annot::read(doc, r).unwrap();
    assert_eq!(
        action_of(&get(&doc, dest)),
        Some(&LinkAction::GoTo(Dest::Explicit(Destination {
            page: DestPage::Ref(PAGE),
            view: View::FitH { top: None },
        })))
    );
    assert_eq!(
        action_of(&get(&doc, by_name)),
        Some(&LinkAction::GoTo(Dest::Named("Intro".into())))
    );
    let remote = LinkAction::GoToR {
        file: "Ünïcode.pdf".into(),
        dest: Some(Dest::Explicit(Destination {
            page: DestPage::Index(2),
            view: View::Fit,
        })),
        new_window: Some(true),
    };
    assert_eq!(action_of(&get(&doc, gotor)), Some(&remote));
    assert_eq!(
        action_of(&get(&doc, launch)),
        Some(&LinkAction::Launch {
            file: "notepad.exe".into(),
            new_window: None,
        })
    );
    assert_eq!(
        action_of(&get(&doc, js)),
        Some(&LinkAction::Other("JavaScript".into()))
    );
    let xyz = Destination {
        page: DestPage::Ref(PAGE),
        view: View::Xyz {
            left: Some(0.0),
            top: Some(400.0),
            zoom: None,
        },
    };
    assert_eq!(named_destination(&doc, "Intro"), Some(xyz));
    assert_eq!(named_destination(&doc, "Old"), Some(xyz));
    assert_eq!(
        named_destination(&doc, "A").map(|d| d.view),
        Some(View::Fit)
    );
    assert_eq!(named_destination(&doc, "Missing"), None);

    // Read-only targets survive edits, but cannot be written or swapped.
    let mut a = get(&doc, gotor);
    a.rect = Rect::new(100.0, 100.0, 200.0, 130.0);
    update(&mut doc, gotor, &a).unwrap();
    let d = doc.get(gotor).unwrap();
    let act = doc.get_in(d.as_dict().unwrap(), b"A");
    assert_eq!(act.as_dict().unwrap().name(b"S"), Some(&b"GoToR"[..]));
    assert_eq!(action_of(&get(&doc, gotor)), Some(&remote));
    let mut other = a.clone();
    other.kind = Kind::Link {
        action: Some(LinkAction::Launch {
            file: "x".into(),
            new_window: None,
        }),
        quads: vec![],
    };
    assert!(matches!(
        update(&mut doc, gotor, &other),
        Err(Error::ReadOnly)
    ));
    assert!(matches!(add(&mut doc, PAGE, &other), Err(Error::ReadOnly)));

    // Replacing a /Dest with a URI drops the destination.
    let mut a = get(&doc, dest);
    a.kind = Kind::Link {
        action: Some(LinkAction::Uri("https://example.com/".into())),
        quads: vec![],
    };
    update(&mut doc, dest, &a).unwrap();
    let (doc, _) = reopen(&doc, "foreign-links-edited");
    let d = doc.get(dest).unwrap();
    assert!(!d.as_dict().unwrap().contains(b"Dest"));
    assert_eq!(action_of(&get(&doc, dest)), action_of(&a));
}

fn text_string(s: &str) -> Object {
    let mut b = vec![0xFE, 0xFF];
    b.extend(s.encode_utf16().flat_map(u16::to_be_bytes));
    Object::string(&b)
}
