//! PDF/A conversion of the samples passes validation (and `qpdf --check` and `mutool draw`
//! when installed); each validator rule fails on a fixture made to break it and passes on
//! the converted original; the PDF/UA checks and fixes work on tagged and untagged files.

use std::path::{Path, PathBuf};
use std::process::Command;

use pagebara_conform::{Profile, pdfa, pdfua, validate};
use pagebara_syntax::{Dict, Document, ObjRef, Object, Stream};

fn sample_dir() -> PathBuf {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("conform-samples");
        pagebara_samples::write_all(&dir).unwrap();
        dir
    })
    .clone()
}

fn sample(name: &str) -> Document {
    Document::open(std::fs::read(sample_dir().join(name)).unwrap()).unwrap()
}

fn tool_ok(tool: &str, args: &[&str]) -> Option<bool> {
    let out = Command::new(tool).args(args).output().ok()?;
    Some(out.status.success())
}

#[test]
fn samples_convert_to_every_part_and_pass() {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("conform-out");
    std::fs::create_dir_all(&dir).unwrap();
    let names = [
        "lease.pdf",
        "report.pdf",
        "form.pdf",
        "tagged.pdf",
        "flyer.pdf",
        "scan.pdf",
        "live-text.pdf",
        "personal.pdf",
        "rotated.pdf",
        "signed.pdf",
    ];
    for name in names {
        let doc = sample(name);
        for profile in [
            Profile::PdfA1b,
            Profile::PdfA2b,
            Profile::PdfA2u,
            Profile::PdfA3b,
            Profile::PdfA3u,
        ] {
            let c = pdfa::convert(&doc, profile).unwrap();
            assert!(
                c.report.passed(),
                "{name} {profile:?}:\n{}",
                c.report.to_text()
            );
            let out = dir.join(format!(
                "{}-{}.pdf",
                name.trim_end_matches(".pdf"),
                profile.name()
            ));
            std::fs::write(&out, &c.bytes).unwrap();
            let p = out.to_str().unwrap();
            if let Some(ok) = tool_ok("qpdf", &["--check", p]) {
                assert!(ok, "qpdf --check {p}");
            }
            let png = dir.join("page-%d.png");
            if let Some(ok) = tool_ok(
                "mutool",
                &["draw", "-q", "-r", "20", "-o", png.to_str().unwrap(), p],
            ) {
                assert!(ok, "mutool draw {p}");
            }
        }
    }
}

#[test]
fn transparency_is_refused_for_part_1() {
    let doc = with_gs(sample("flyer.pdf"), "ca", Object::Real(0.5));
    match pdfa::convert(&doc, Profile::PdfA1b) {
        Err(pdfa::ConvertError::Transparency(pages)) => assert_eq!(pages, [0]),
        other => panic!("{other:?}"),
    }
    assert!(
        pdfa::convert(&doc, Profile::PdfA2b)
            .unwrap()
            .report
            .passed()
    );
}

/// A converted document, opened again.
fn good(profile: Profile, name: &str) -> Document {
    let c = pdfa::convert(&sample(name), profile).unwrap();
    assert!(c.report.passed(), "{}", c.report.to_text());
    Document::open(c.bytes).unwrap()
}

/// `doc` changed by `f`, saved and opened again.
fn broken(doc: &Document, f: impl FnOnce(&mut Document)) -> Document {
    let mut d = doc.clone();
    f(&mut d);
    Document::open(d.save_full().unwrap()).unwrap()
}

fn rules(doc: &Document, p: Profile) -> Vec<String> {
    validate(doc, p)
        .issues
        .into_iter()
        .map(|i| i.rule)
        .collect()
}

fn catalog_ref(d: &Document) -> ObjRef {
    d.trailer()
        .get(b"Root")
        .and_then(Object::as_objref)
        .unwrap()
}

fn edit(d: &mut Document, r: ObjRef, f: impl FnOnce(&mut Dict)) {
    let mut o = (*d.get(r).unwrap()).clone();
    match &mut o {
        Object::Dict(dd) => f(dd),
        Object::Stream(s) => f(&mut s.dict),
        _ => panic!(),
    }
    d.set(r, o);
}

fn first_page(d: &Document) -> ObjRef {
    pagebara_pages::PageTree::load(d).refs()[0]
}

fn page_resources(d: &mut Document) -> Dict {
    let p = first_page(d);
    let pd = d.get(p).unwrap().as_dict().unwrap().clone();
    d.get_in(&pd, b"Resources")
        .as_dict()
        .cloned()
        .unwrap_or_default()
}

/// Draws `extra` content on the first page with `res` merged into its resources.
fn draw(d: &mut Document, extra: &[u8], add: impl FnOnce(&mut Dict, &mut Document)) {
    let p = first_page(d);
    let mut res = page_resources(d);
    add(&mut res, d);
    let mut contents: Vec<Object> = {
        let pd = d.get(p).unwrap().as_dict().unwrap().clone();
        match d.get_in(&pd, b"Contents").as_array() {
            Some(a) => a.to_vec(),
            None => pd.get(b"Contents").cloned().into_iter().collect(),
        }
    };
    let s = d.add(Stream::new(Dict::new(), extra.to_vec()));
    contents.push(Object::Ref(s));
    edit(d, p, |pd| {
        pd.set(b"Resources", res);
        pd.set(b"Contents", contents);
    });
}

fn with_gs(mut d: Document, key: &str, v: Object) -> Document {
    let key = key.as_bytes().to_vec();
    draw(&mut d, b"q /GSx gs 0 0 10 10 re f Q", |res, d| {
        let mut gs = Dict::new();
        gs.set(b"Type", Object::name(b"ExtGState"));
        gs.set(&key, v);
        let g = d.add(gs);
        let mut all = d
            .get_in(res, b"ExtGState")
            .as_dict()
            .cloned()
            .unwrap_or_default();
        all.set(b"GSx", g);
        res.set(b"ExtGState", all);
    });
    d
}

fn assert_rule(good: &Document, bad: &Document, p: Profile, rule: &str) {
    assert!(
        !rules(good, p).iter().any(|r| r == rule),
        "{rule} on the good file"
    );
    let got = rules(bad, p);
    assert!(
        got.iter().any(|r| r == rule),
        "{rule} not reported: {got:?}"
    );
}

fn first_annot(d: &Document) -> ObjRef {
    for p in pagebara_pages::PageTree::load(d).refs() {
        let pd = d.get(p).unwrap().as_dict().unwrap().clone();
        if let Some(a) = d
            .get_in(&pd, b"Annots")
            .as_array()
            .and_then(|a| a.first().and_then(Object::as_objref))
        {
            return a;
        }
    }
    panic!("no annotation")
}

#[test]
fn pdfa2_rules_fail_on_broken_fixtures() {
    let p = Profile::PdfA2b;
    let g = good(p, "lease.pdf");
    let case = |rule: &str, f: &dyn Fn(&mut Document)| {
        let b = broken(&g, f);
        assert_rule(&g, &b, p, rule);
    };
    case("6.6.2.1-1", &|d| {
        edit(d, catalog_ref(d), |c| drop(c.remove(b"Metadata")))
    });
    case("6.6.4-1", &|d| {
        let m = d.add(Stream::new(Dict::new(), b"<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><pdfaid:part>1</pdfaid:part><pdfaid:conformance>B</pdfaid:conformance></x:xmpmeta>".to_vec()));
        edit(d, catalog_ref(d), |c| c.set(b"Metadata", m));
    });
    case("6.6.2.1-2", &|d| {
        let m = d.add(Stream::new(Dict::new(), b"<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\" bytes=\"1\"?><pdfaid:part>2</pdfaid:part><pdfaid:conformance>B</pdfaid:conformance><?xpacket end=\"w\"?>".to_vec()));
        edit(d, catalog_ref(d), |c| c.set(b"Metadata", m));
    });
    // Only PDF/A-1 asks the document information to agree with the metadata (veraPDF
    // passes a PDF/A-2 file whose title differs).
    assert!(
        !rules(&broken(&g, retitle), p)
            .iter()
            .any(|r| r == "6.6.3-1")
    );
    case("6.1.7.2-1", &|d| {
        let mut s = Dict::new();
        s.set(b"Filter", Object::name(b"LZWDecode"));
        let r = d.add(Stream::new(
            s,
            vec![0x80, 0x0b, 0x60, 0x50, 0x22, 0x0c, 0x0c, 0x85, 0x01],
        ));
        edit(d, catalog_ref(d), |c| c.set(b"Junk", r));
    });
    case("6.1.7.1-1", &|d| {
        let mut s = Dict::new();
        s.set(b"F", Object::string(b"external.dat"));
        let r = d.add(Stream::new(s, Vec::<u8>::new()));
        edit(d, catalog_ref(d), |c| c.set(b"Junk", r));
    });
    case("6.2.5-1", &|d| {
        *d = with_gs(d.clone(), "TR", Object::name(b"Identity"));
    });
    case("6.2.5-3", &|d| {
        *d = with_gs(
            d.clone(),
            "HTP",
            vec![Object::Int(0), Object::Int(0)].into(),
        );
    });
    case("6.2.10-2", &|d| {
        *d = with_gs(d.clone(), "BM", Object::name(b"Dissolve"));
    });
    case("6.3.2-1", &|d| {
        let a = first_annot(d);
        edit(d, a, |ad| ad.set(b"F", 0));
    });
    case("6.3.2-2", &|d| {
        let a = first_annot(d);
        edit(d, a, |ad| ad.set(b"F", 4 | 2));
    });
    case("6.3.3-1", &|d| {
        let a = first_annot(d);
        edit(d, a, |ad| {
            ad.set(b"Subtype", Object::name(b"Square"));
            ad.remove(b"AP");
        });
    });
    case("6.3.1-1", &|d| {
        let a = first_annot(d);
        edit(d, a, |ad| ad.set(b"Subtype", Object::name(b"Sound")));
    });
    case("6.5.1-1", &|d| {
        let a = first_annot(d);
        let mut act = Dict::new();
        act.set(b"S", Object::name(b"Launch"));
        act.set(b"F", Object::string(b"calc.exe"));
        edit(d, a, |ad| ad.set(b"A", act));
    });
    case("6.5.1-2", &|d| {
        let a = first_annot(d);
        let mut act = Dict::new();
        act.set(b"S", Object::name(b"Named"));
        act.set(b"N", Object::name(b"Print"));
        edit(d, a, |ad| ad.set(b"A", act));
    });
    case("6.5.1-3", &|d| {
        let mut names = Dict::new();
        names.set(b"JavaScript", Dict::new());
        edit(d, catalog_ref(d), |c| c.set(b"Names", names));
    });
    case("6.5.2-1", &|d| {
        edit(d, catalog_ref(d), |c| c.set(b"AA", Dict::new()))
    });
    case("6.5.2-2", &|d| {
        let p = first_page(d);
        edit(d, p, |pd| pd.set(b"AA", Dict::new()));
    });
    case("6.4.1-2", &|d| {
        let mut f = Dict::new();
        f.set(b"Fields", Vec::<Object>::new());
        f.set(b"NeedAppearances", true);
        edit(d, catalog_ref(d), |c| c.set(b"AcroForm", f));
    });
    case("6.4.2-1", &|d| {
        let mut f = Dict::new();
        f.set(b"Fields", Vec::<Object>::new());
        f.set(b"XFA", Object::string(b"<xdp/>"));
        edit(d, catalog_ref(d), |c| c.set(b"AcroForm", f));
    });
    case("6.2.4.3-2", &|d| {
        edit(d, catalog_ref(d), |c| drop(c.remove(b"OutputIntents")))
    });
    case("6.2.4.3-3", &|d| {
        draw(d, b"0 0 0 1 k 0 0 5 5 re f", |_, _| {})
    });
    case("6.2.3-1", &|d| {
        let mut oi = Dict::new();
        oi.set(b"S", Object::name(b"GTS_PDFA1"));
        oi.set(
            b"DestOutputProfile",
            d.add(Stream::new(Dict::new(), b"not a profile".to_vec())),
        );
        let oi = d.add(oi);
        edit(d, catalog_ref(d), |c| {
            c.set(b"OutputIntents", vec![Object::Ref(oi)])
        });
    });
    case("6.2.8-3", &|d| {
        let mut img = Dict::new();
        img.set(b"Subtype", Object::name(b"Image"));
        img.set(b"Width", 1);
        img.set(b"Height", 1);
        img.set(b"ColorSpace", Object::name(b"DeviceGray"));
        img.set(b"BitsPerComponent", 8);
        img.set(b"Interpolate", true);
        let r = d.add(Stream::new(img, vec![0]));
        edit(d, catalog_ref(d), |c| c.set(b"Junk", r));
    });
    case("6.2.9-2", &|d| {
        let mut f = Dict::new();
        f.set(b"Subtype", Object::name(b"Form"));
        f.set(
            b"BBox",
            vec![
                Object::Int(0),
                Object::Int(0),
                Object::Int(1),
                Object::Int(1),
            ],
        );
        f.set(b"Ref", Dict::new());
        let r = d.add(Stream::new(f, Vec::<u8>::new()));
        edit(d, catalog_ref(d), |c| c.set(b"Junk", r));
    });
    case("6.1.13-4", &|d| {
        edit(d, catalog_ref(d), |c| {
            c.set(b"Junk", Object::name(&[b'n'; 200]))
        })
    });
    case("6.1.13-1", &|d| {
        edit(d, catalog_ref(d), |c| c.set(b"Junk", Object::Int(1 << 40)))
    });
    case("6.1.11-1", &|d| {
        let mut oc = Dict::new();
        oc.set(b"OCGs", Vec::<Object>::new());
        oc.set(b"D", Dict::new());
        let r = d.add(oc);
        edit(d, catalog_ref(d), |c| c.set(b"OCProperties", r));
    });
    case("6.8-1", &|d| {
        let ef = d.add(Stream::new(Dict::new(), b"plain text".to_vec()));
        let mut efd = Dict::new();
        efd.set(b"F", ef);
        let mut fs = Dict::new();
        fs.set(b"Type", Object::name(b"Filespec"));
        fs.set(b"F", Object::string(b"a.txt"));
        fs.set(b"EF", efd);
        let fs = d.add(fs);
        edit(d, catalog_ref(d), |c| c.set(b"Junk", fs));
    });
    case("6.1.12-1", &|d| {
        let mut perms = Dict::new();
        perms.set(b"Other", Dict::new());
        edit(d, catalog_ref(d), |c| c.set(b"Perms", perms));
    });
    case("6.1.13-10", &|d| {
        let p = first_page(d);
        edit(d, p, |pd| {
            pd.set(
                b"MediaBox",
                vec![
                    Object::Int(0),
                    Object::Int(0),
                    Object::Int(2),
                    Object::Int(2),
                ],
            )
        });
    });
    // The originals: not embedded, no identifier, no metadata.
    let orig = sample("lease.pdf");
    let got = rules(&orig, p);
    for r in ["6.2.11.4.1-1", "6.1.3-1", "6.6.2.1-1"] {
        assert!(got.iter().any(|x| x == r), "{r}: {got:?}");
    }
    let enc = Document::open(
        std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../syntax/tests/fixtures/crypt/aes-256-empty-user.pdf"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(rules(&enc, p).iter().any(|r| r == "6.1.3-2"));
}

/// Gives the document information a title the metadata does not have.
fn retitle(d: &mut Document) {
    let info = d
        .trailer()
        .get(b"Info")
        .and_then(Object::as_objref)
        .unwrap();
    edit(d, info, |i| {
        i.set(b"Title", Object::string(b"Something else"))
    });
}

#[test]
fn pdfa1_and_pdfa3_rules() {
    let p = Profile::PdfA1b;
    let g = good(p, "flyer.pdf");
    for (rule, f) in [
        ("6.7.3-1", Box::new(retitle) as Box<dyn Fn(&mut Document)>),
        (
            "6.4-4",
            Box::new(|d: &mut Document| *d = with_gs(d.clone(), "CA", Object::Real(0.5))),
        ),
        (
            "6.4-1",
            Box::new(|d: &mut Document| *d = with_gs(d.clone(), "SMask", Dict::new().into())),
        ),
        (
            "6.4-3",
            Box::new(|d: &mut Document| *d = with_gs(d.clone(), "BM", Object::name(b"Multiply"))),
        ),
        (
            "6.1.13-1",
            Box::new(|d: &mut Document| {
                edit(d, catalog_ref(d), |c| c.set(b"OCProperties", Dict::new()))
            }),
        ),
        (
            "6.1.12-2",
            Box::new(|d: &mut Document| {
                edit(d, catalog_ref(d), |c| {
                    c.set(b"Junk", Object::Real(1e6 + 0.5))
                })
            }),
        ),
        (
            "6.7.2-2",
            Box::new(|d: &mut Document| {
                let c = d.catalog().unwrap();
                let m = c
                    .as_dict()
                    .unwrap()
                    .get(b"Metadata")
                    .and_then(Object::as_objref)
                    .unwrap();
                let s = d.get(m).unwrap().as_stream().unwrap().clone();
                let data = d.decode(&s).unwrap().data;
                let mut sd = s.dict.clone();
                sd.set(b"Filter", Object::name(b"FlateDecode"));
                d.set(m, Stream::new(sd, pagebara_filters::encode_flate(&data, 6)));
            }),
        ),
    ] {
        let b = broken(&g, f);
        assert_rule(&g, &b, p, rule);
    }
    // Object streams are PDF/A-2 and later.
    let a2 = good(Profile::PdfA2b, "flyer.pdf");
    assert!(rules(&a2, p).iter().any(|r| r == "6.1.4-1"));
    // Part 3: attachments need a relationship and a type.
    let p3 = Profile::PdfA3b;
    let g3 = good(p3, "flyer.pdf");
    let b3 = broken(&g3, |d| {
        let ef = d.add(Stream::new(Dict::new(), b"data".to_vec()));
        let mut efd = Dict::new();
        efd.set(b"F", ef);
        let mut fs = Dict::new();
        fs.set(b"Type", Object::name(b"Filespec"));
        fs.set(b"F", Object::string(b"a.bin"));
        fs.set(b"EF", efd);
        let fs = d.add(fs);
        edit(d, catalog_ref(d), |c| c.set(b"AF", vec![Object::Ref(fs)]));
    });
    let got = rules(&b3, p3);
    assert!(
        got.iter().any(|r| r == "6.8-2") && got.iter().any(|r| r == "6.8-3"),
        "{got:?}"
    );
    let fixed = pdfa::convert(&b3, p3).unwrap();
    assert!(fixed.report.passed(), "{}", fixed.report.to_text());
}

#[test]
fn unicode_level_needs_mapped_text() {
    let p = Profile::PdfA2u;
    let g = good(p, "flyer.pdf");
    // A font whose codes map to nothing: a symbolic Type 3 font.
    let b = broken(&g, |d| {
        draw(d, b"BT /T3x 12 Tf 10 10 Td (\x01) Tj ET", |res, d| {
            let proc_ = d.add(Stream::new(Dict::new(), b"0 0 d0 0 0 1 1 re f".to_vec()));
            let mut procs = Dict::new();
            procs.set(b"g1", proc_);
            let mut enc = Dict::new();
            enc.set(b"Differences", vec![Object::Int(1), Object::name(b"g1")]);
            let mut f = Dict::new();
            f.set(b"Type", Object::name(b"Font"));
            f.set(b"Subtype", Object::name(b"Type3"));
            f.set(
                b"FontBBox",
                vec![
                    Object::Int(0),
                    Object::Int(0),
                    Object::Int(1),
                    Object::Int(1),
                ],
            );
            f.set(
                b"FontMatrix",
                vec![
                    Object::Real(0.001),
                    Object::Int(0),
                    Object::Int(0),
                    Object::Real(0.001),
                    Object::Int(0),
                    Object::Int(0),
                ],
            );
            f.set(b"CharProcs", procs);
            f.set(b"Encoding", enc);
            f.set(b"FirstChar", 1);
            f.set(b"LastChar", 1);
            f.set(b"Widths", vec![Object::Int(1000)]);
            let f = d.add(f);
            let mut fonts = d
                .get_in(res, b"Font")
                .as_dict()
                .cloned()
                .unwrap_or_default();
            fonts.set(b"T3x", f);
            res.set(b"Font", fonts);
        });
    });
    assert_rule(&g, &b, p, "6.2.11.7-1");
    assert!(!rules(&b, Profile::PdfA2b).iter().any(|r| r == "6.2.11.7-1"));
}

#[test]
fn device_cmyk_gets_a_default_space() {
    let doc = broken(&sample("flyer.pdf"), |d| {
        draw(d, b"0.1 0.2 0.3 0.4 k 0 0 50 50 re f", |_, _| {})
    });
    let c = pdfa::convert(&doc, Profile::PdfA2b).unwrap();
    assert!(c.report.passed(), "{}", c.report.to_text());
    assert!(c.changes.iter().any(|s| s.contains("default colour space")));
}

#[test]
fn reports_serialise() {
    let r = validate(&sample("lease.pdf"), Profile::PdfA2b);
    let j: serde_json_value::Value = serde_json_value::from_str(&r.to_json());
    assert!(j.0.contains("\"rule\"") && j.0.contains("\"clause\""));
    assert!(!r.passed() && r.rules_checked > 10);
    assert_eq!(Profile::parse("pdfa-2b"), Some(Profile::PdfA2b));
    assert_eq!(Profile::parse("PDF/UA-1"), Some(Profile::PdfUa1));
    assert_eq!(Profile::parse("2u"), Some(Profile::PdfA2u));
}

/// A tiny stand-in for parsing JSON in a test.
mod serde_json_value {
    pub struct Value(pub String);
    pub fn from_str(s: &str) -> Value {
        assert!(s.trim_start().starts_with('{') && s.trim_end().ends_with('}'));
        Value(s.to_owned())
    }
}

// PDF/UA.

fn ua_rules(d: &Document) -> Vec<String> {
    rules(d, Profile::PdfUa1)
}

fn ua_fixed(d: &Document) -> Document {
    let mut d = d.clone();
    pdfua::fix_safe(
        &mut d,
        &pdfua::FixOptions {
            lang: Some("en-US".into()),
            title: Some("Sample".into()),
        },
    );
    Document::open(d.save_full().unwrap()).unwrap()
}

#[test]
fn tagged_sample_passes_pdfua_after_safe_fixes() {
    let d = sample("tagged.pdf");
    assert!(!validate(&d, Profile::PdfUa1).passed());
    let f = ua_fixed(&d);
    let r = validate(&f, Profile::PdfUa1);
    assert!(r.passed(), "{}", r.to_text());
}

#[test]
fn untagged_documents_fail_and_safe_fixes_cover_the_document_level() {
    let d = sample("lease.pdf");
    let before = ua_rules(&d);
    for r in ["01-002", "11-006", "07-001", "28-009", "31-001"] {
        assert!(before.iter().any(|x| x == r), "{r}: {before:?}");
    }
    let after = ua_rules(&ua_fixed(&d));
    assert_eq!(after, ["01-002"], "{after:?}");
}

#[test]
fn guided_fixes_resolve_structure_issues() {
    let base = ua_fixed(&sample("tagged.pdf"));
    let els = pdfua::elements(&base);
    let p = els.iter().find(|e| e.role == "P").unwrap().obj;
    // A figure without alternative text.
    let fig = broken(&base, |d| assert!(pdfua::set_tag(d, p, "Figure")));
    assert!(ua_rules(&fig).iter().any(|r| r == "13-004"));
    let fixed = broken(&fig, |d| assert!(pdfua::set_alt(d, p, "A chart of sales")));
    assert!(ua_rules(&fixed).is_empty(), "{:?}", ua_rules(&fixed));
    // Heading levels.
    let h = els.iter().find(|e| e.role == "H1").map(|e| e.obj);
    if let Some(h) = h {
        let skip = broken(&base, |d| assert!(pdfua::set_tag(d, h, "H3")));
        assert!(ua_rules(&skip).iter().any(|r| r == "14-002"));
        let back = broken(&skip, |d| assert!(pdfua::set_tag(d, h, "H1")));
        assert!(!ua_rules(&back).iter().any(|r| r.starts_with("14-")));
    }
    // An annotation outside the structure tree, and a form field without a name.
    let annot = broken(&base, |d| {
        let page = first_page(d);
        let mut a = Dict::new();
        a.set(b"Type", Object::name(b"Annot"));
        a.set(b"Subtype", Object::name(b"Square"));
        a.set(
            b"Rect",
            vec![
                Object::Int(10),
                Object::Int(10),
                Object::Int(50),
                Object::Int(50),
            ],
        );
        a.set(b"F", 4);
        a.set(b"P", page);
        let a = d.add(a);
        edit(d, page, |pd| pd.set(b"Annots", vec![Object::Ref(a)]));
    });
    let got = ua_rules(&annot);
    assert!(
        got.iter().any(|r| r == "28-002") && got.iter().any(|r| r == "28-004"),
        "{got:?}"
    );
    let a = first_annot(&annot);
    let mut fixed = ua_fixed(&annot);
    assert!(
        !ua_rules(&fixed)
            .iter()
            .any(|r| r == "28-002" || r == "28-009")
    );
    assert!(pdfua::set_annotation_text(
        &mut fixed,
        a,
        "A highlighted area"
    ));
    let fixed = Document::open(fixed.save_full().unwrap()).unwrap();
    assert!(ua_rules(&fixed).is_empty(), "{:?}", ua_rules(&fixed));
    // Untagged drawing becomes an artifact.
    let lines = broken(&base, |d| draw(d, b"0 0 m 100 100 l S", |_, _| {}));
    assert!(ua_rules(&lines).iter().any(|r| r == "01-005"));
    assert!(!ua_rules(&ua_fixed(&lines)).iter().any(|r| r == "01-005"));
}

#[test]
fn tables_need_header_cells() {
    let base = ua_fixed(&sample("tagged.pdf"));
    let els = pdfua::elements(&base);
    let Some(t) = els.iter().find(|e| e.role == "Table") else {
        return;
    };
    let _ = t;
    let cells: Vec<ObjRef> = els
        .iter()
        .filter(|e| e.role == "TH")
        .map(|e| e.obj)
        .collect();
    let no_th = broken(&base, |d| {
        for &c in &cells {
            pdfua::set_tag(d, c, "TD");
        }
    });
    assert!(ua_rules(&no_th).iter().any(|r| r == "15-003"));
    let back = broken(&no_th, |d| {
        pdfua::set_tag(d, cells[0], "TH");
    });
    assert!(!ua_rules(&back).iter().any(|r| r == "15-003"));
}

// Verdicts the veraPDF corpus expects differently from what the checks first said.

/// A UTF-16 `/Lang` is valid, and without a document language the structure elements
/// may give one to all tagged content.
#[test]
fn ua_languages_in_utf16_and_in_the_structure() {
    let base = ua_fixed(&sample("tagged.pdf"));
    let cat = catalog_ref(&base);
    let utf16 = broken(&base, |d| {
        let mut s = vec![0xfe, 0xff];
        s.extend("en-US".encode_utf16().flat_map(u16::to_be_bytes));
        edit(d, cat, |c| c.set(b"Lang", Object::string(&s)));
    });
    assert!(
        !ua_rules(&utf16)
            .iter()
            .any(|r| r == "11-005" || r == "11-006")
    );
    let none = broken(&base, |d| {
        edit(d, cat, |c| {
            c.remove(b"Lang");
        })
    });
    assert!(ua_rules(&none).iter().any(|r| r == "11-006"));
    let in_structure = broken(&none, |d| {
        let c = d.catalog().unwrap();
        let root = d.get_in(c.as_dict().unwrap(), b"StructTreeRoot");
        let kids: Vec<ObjRef> = match root.as_dict().unwrap().get(b"K").unwrap() {
            Object::Array(a) => a.iter().filter_map(Object::as_objref).collect(),
            k => k.as_objref().into_iter().collect(),
        };
        for k in kids {
            edit(d, k, |e| e.set(b"Lang", Object::string(b"en-US")));
        }
    });
    assert!(!ua_rules(&in_structure).iter().any(|r| r == "11-006"));
}

/// XMP in UTF-16, values with significant spaces, and PDF/A-1's 65535-byte strings.
#[test]
fn pdfa_metadata_encodings_and_string_limits() {
    let p = Profile::PdfA1b;
    let g = good(p, "flyer.pdf");
    let utf16 = broken(&g, |d| {
        let c = d.catalog().unwrap();
        let m = d.get_in(c.as_dict().unwrap(), b"Metadata");
        let s = m.as_stream().unwrap().clone();
        let text = String::from_utf8(d.decode(&s).unwrap().data).unwrap();
        let data: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let mut dict = s.dict.clone();
        dict.remove(b"Filter");
        let mr = c
            .as_dict()
            .unwrap()
            .get(b"Metadata")
            .and_then(Object::as_objref)
            .unwrap();
        d.set(mr, Stream::new(dict, data));
    });
    let got = rules(&utf16, p);
    assert!(!got.iter().any(|r| r.starts_with("6.7.11")), "{got:?}");
    let long = |n: usize| {
        broken(&g, |d| {
            let cat = catalog_ref(d);
            edit(d, cat, |c| {
                c.set(b"PagebaraLong", Object::string(&vec![b'x'; n]))
            });
        })
    };
    assert!(!rules(&long(40_000), p).iter().any(|r| r == "6.1.12-3"));
    assert!(rules(&long(70_000), p).iter().any(|r| r == "6.1.12-3"));
    let a2 = good(Profile::PdfA2b, "flyer.pdf");
    let long2 = broken(&a2, |d| {
        let cat = catalog_ref(d);
        edit(d, cat, |c| {
            c.set(b"PagebaraLong", Object::string(&[b'x'; 40_000]))
        });
    });
    assert!(
        rules(&long2, Profile::PdfA2b)
            .iter()
            .any(|r| r == "6.1.13-3")
    );
}
