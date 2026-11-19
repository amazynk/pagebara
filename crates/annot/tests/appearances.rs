//! Line endings, text beyond WinAnsi and pluggable fonts, popups, and appearance reuse.

mod common;

use common::*;
use pagebara_annot::*;
use pagebara_syntax::{Document, ObjRef, Object, Stream};

const PAGE: ObjRef = ObjRef::new(3, 0);

fn page_doc() -> Document {
    Document::open(one_page(400, 400, "")).unwrap()
}

fn reopen(doc: &Document, name: &str) -> (Document, Vec<u8>) {
    let bytes = doc.save_incremental().unwrap();
    qpdf_check(&bytes, name);
    (Document::open(bytes.clone()).unwrap(), bytes)
}

fn appearance_stream(doc: &Document, r: ObjRef) -> (ObjRef, Stream) {
    let d = doc.get(r).unwrap();
    let ap = doc.get_in(d.as_dict().unwrap(), b"AP");
    let n = ap
        .as_dict()
        .unwrap()
        .get(b"N")
        .unwrap()
        .as_objref()
        .unwrap();
    let s = doc.get(n).unwrap().as_stream().unwrap().clone();
    (n, s)
}

fn appearance_text(doc: &Document, r: ObjRef) -> Vec<u8> {
    let (_, s) = appearance_stream(doc, r);
    doc.decode(&s).unwrap().data.to_vec()
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn closed_endings_take_the_stroke_colour() {
    let mut doc = page_doc();
    let p = Point::new;
    let arrow = Annotation::arrow(p(20.0, 300.0), p(200.0, 300.0), Color::BLUE, 2.0);
    let a = add(&mut doc, PAGE, &arrow).unwrap();
    // No interior given: closed endings are filled with the stroke colour, and /IC says so.
    let mut line = Annotation::new(
        Kind::Line {
            start: p(20.0, 200.0),
            end: p(200.0, 200.0),
            endings: [LineEnding::Diamond, LineEnding::RClosedArrow],
            interior: None,
        },
        Rect::default(),
    );
    line.border.width = 2.0;
    let l = add(&mut doc, PAGE, &line).unwrap();
    let mut poly = Annotation::new(
        Kind::PolyLine {
            vertices: vec![p(250.0, 100.0), p(300.0, 150.0), p(350.0, 100.0)],
            endings: [LineEnding::Square, LineEnding::Circle],
            interior: None,
        },
        Rect::default(),
    );
    poly.border.width = 2.0;
    let pl = add(&mut doc, PAGE, &poly).unwrap();
    // Open endings are not filled.
    let mut open = line.clone();
    open.kind = Kind::Line {
        start: p(20.0, 100.0),
        end: p(200.0, 100.0),
        endings: [LineEnding::None, LineEnding::OpenArrow],
        interior: None,
    };
    let o = add(&mut doc, PAGE, &open).unwrap();

    let (mut doc, bytes) = reopen(&doc, "line-endings");
    let interior = |doc: &Document, r| match pagebara_annot::read(doc, r).unwrap().kind {
        Kind::Line { interior, .. } | Kind::PolyLine { interior, .. } => interior,
        _ => unreachable!(),
    };
    assert_eq!(interior(&doc, a), Some(Color::BLUE));
    assert_eq!(interior(&doc, l), Some(Color::RED));
    assert_eq!(interior(&doc, pl), Some(Color::RED));
    assert_eq!(interior(&doc, o), None);
    for (renderer, img) in render_all(&bytes, "line-endings") {
        // Inside the arrow head, behind the tip, off the line itself.
        assert!(is_blue(img.at(190.0, 302.5)), "{renderer} arrow head");
        // Diamond centred on the start, the reversed arrow beyond the end.
        assert!(is_red(img.at(20.0, 203.0)), "{renderer} diamond");
        assert!(is_red(img.at(208.0, 202.5)), "{renderer} reversed arrow");
        // Square and circle on the polyline's ends.
        assert!(is_red(img.at(253.0, 103.0)), "{renderer} square");
        assert!(is_red(img.at(347.0, 103.0)), "{renderer} circle");
        // Open arrow: hollow.
        assert!(is_white(img.at(189.0, 103.0)), "{renderer} open arrow");
        assert!(
            img.any(188.0, 104.0, 190.0, 107.0, is_red),
            "{renderer} open arrow"
        );
    }

    // Recolouring a line brings its heads along.
    let mut b = pagebara_annot::read(&doc, a).unwrap();
    b.set_color(Some(Color::RED));
    update(&mut doc, a, &b).unwrap();
    assert_eq!(interior(&doc, a), Some(Color::RED));
}

/// A stand-in for an embedded font: Helvetica's metrics under another name, showing every
/// character, and recording what it was asked to provide.
#[derive(Debug, Default)]
struct Recording(std::cell::RefCell<Vec<char>>);

impl TextFont for Recording {
    fn resource_name(&self) -> &str {
        "F0"
    }
    fn family(&self) -> &str {
        "Test Sans"
    }
    fn glyph(&self, c: char) -> Option<Glyph> {
        Helvetica.glyph(c).or(Some(Glyph {
            code: u32::from(b'#'),
            len: 1,
            width: 0.6,
            face: 0,
        }))
    }
    fn fallback(&self) -> Glyph {
        Helvetica.fallback()
    }
    fn ascent(&self) -> f64 {
        0.7
    }
    fn resource(&self, doc: &mut Document, used: &[(char, Glyph)]) -> Object {
        self.0.borrow_mut().extend(used.iter().map(|(c, _)| *c));
        Helvetica.resource(doc, used)
    }
}

fn utf16(o: &Object) -> String {
    let b = o.as_string().unwrap();
    assert!(b.starts_with(&[0xFE, 0xFF]));
    let units: Vec<u16> = b[2..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_be_bytes(*c))
        .collect();
    String::from_utf16(&units).unwrap()
}

#[test]
fn free_text_beyond_win_ansi() {
    let mut doc = page_doc();
    let text = "Привет, € 1 < 2\nok";
    let mut a = Annotation::new(
        Kind::FreeText {
            font_size: 12.0,
            text_color: Color::Rgb(0.0, 0.0, 1.0),
            align: Align::Left,
        },
        Rect::new(20.0, 200.0, 300.0, 260.0),
    );
    a.contents = Some(text.into());
    let w = add_with(&mut doc, PAGE, &a, &Helvetica).unwrap();
    assert_eq!(w.missing, ['П', 'р', 'и', 'в', 'е', 'т']);
    assert_eq!(unsupported_chars(&Helvetica, text), w.missing);

    let font = Recording::default();
    let w2 = add_with(&mut doc, PAGE, &a, &font).unwrap();
    assert!(w2.missing.is_empty());

    let (doc, bytes) = reopen(&doc, "free-text-unicode");
    let got = pagebara_annot::read(&doc, w.id).unwrap();
    assert_eq!(got.contents.as_deref(), Some(text));
    let d = doc.get(w.id).unwrap();
    let d = d.as_dict().unwrap();
    let rc = utf16(&doc.get_in(d, b"RC"));
    assert!(
        rc.contains("<p dir=\"ltr\">Привет, € 1 &lt; 2</p><p dir=\"ltr\">ok</p>"),
        "{rc}"
    );
    assert_eq!(
        doc.get_in(d, b"DS").as_string(),
        Some(&b"font: Helvetica 12pt; text-align:left; color:#0000FF"[..])
    );
    assert!(
        contains(&appearance_text(&doc, w.id), b"(??????, "),
        "fallback glyphs drawn"
    );

    // The plugged-in font is named in /DA and the resources, and was given every glyph.
    let d2 = doc.get(w2.id).unwrap();
    let da = doc.get_in(d2.as_dict().unwrap(), b"DA");
    assert!(da.as_string().unwrap().starts_with(b"/F0 12 Tf"));
    let (_, s) = appearance_stream(&doc, w2.id);
    let res = doc.get_in(&s.dict, b"Resources");
    let fonts = doc.get_in(res.as_dict().unwrap(), b"Font");
    assert!(fonts.as_dict().unwrap().contains(b"F0"));
    let used = font.0.borrow();
    for c in text.chars().filter(|c| !c.is_control()) {
        assert!(used.contains(&c), "{c}");
    }
    assert!(contains(&appearance_text(&doc, w2.id), b"(######"));

    for (renderer, img) in render_all(&bytes, "free-text-unicode") {
        assert!(img.any(22.0, 240.0, 120.0, 258.0, is_blue), "{renderer}");
    }
}

fn has_appearance(doc: &Document, r: ObjRef) -> bool {
    doc.get(r).unwrap().as_dict().unwrap().contains(b"AP")
}

#[test]
fn popup_appearance_and_replies() {
    let mut doc = page_doc();
    let mut note = Annotation::new(
        Kind::Text {
            icon: "Comment".into(),
            open: true,
        },
        Rect::new(10.0, 10.0, 30.0, 30.0),
    );
    note.author = Some("Ann".into());
    note.contents = Some("First thought".into());
    let n = add(&mut doc, PAGE, &note).unwrap();
    let popup = add_popup(
        &mut doc,
        PAGE,
        n,
        Rect::new(150.0, 100.0, 350.0, 200.0),
        true,
    )
    .unwrap();
    assert!(
        !has_appearance(&doc, popup),
        "viewers draw popups by default"
    );
    assert!(draw_popup(&mut doc, popup, &Helvetica).unwrap().is_empty());
    let (ap_ref, _) = appearance_stream(&doc, popup);
    assert!(contains(&appearance_text(&doc, popup), b"(Ann)"));

    let r = reply(&mut doc, PAGE, n, "Bob", "Second").unwrap();
    assert!(appearance_text(&doc, r).is_empty());

    // Editing the parent redraws its popup in place; editing the reply keeps it empty.
    let mut a = pagebara_annot::read(&doc, n).unwrap();
    a.contents = Some("Changed mind".into());
    update(&mut doc, n, &a).unwrap();
    assert_eq!(appearance_stream(&doc, popup).0, ap_ref);
    assert!(contains(&appearance_text(&doc, popup), b"(Changed"));
    let mut b = pagebara_annot::read(&doc, r).unwrap();
    b.contents = Some("Agreed".into());
    update(&mut doc, r, &b).unwrap();
    assert!(appearance_text(&doc, r).is_empty());

    // mutool and poppler draw popups themselves; flattened, the appearance shows.
    let (mut doc, _) = reopen(&doc, "popup-drawn");
    flatten(&mut doc, PAGE, popup).unwrap();
    assert_eq!(pagebara_annot::read(&doc, n).unwrap().popup, None);
    let (_, bytes) = reopen(&doc, "popup-flattened");
    for (renderer, img) in render_all(&bytes, "popup-flattened") {
        assert!(
            img.any(152.0, 188.0, 348.0, 198.0, is_yellow),
            "{renderer} title bar"
        );
        assert!(
            img.any(152.0, 150.0, 300.0, 185.0, is_dark),
            "{renderer} contents"
        );
    }
}

#[test]
fn updates_rewrite_their_own_appearance() {
    let mut doc = page_doc();
    let a = Annotation::new(
        Kind::Square { interior: None },
        Rect::new(50.0, 50.0, 150.0, 150.0),
    );
    let r = add(&mut doc, PAGE, &a).unwrap();
    let (ap, _) = appearance_stream(&doc, r);
    let count = doc.object_numbers().len();
    let mut b = pagebara_annot::read(&doc, r).unwrap();
    for i in 0..5 {
        b.rect = Rect::from_size(50.0 + f64::from(i) * 10.0, 50.0, 100.0, 100.0);
        update(&mut doc, r, &b).unwrap();
    }
    assert_eq!(appearance_stream(&doc, r).0, ap);
    assert_eq!(doc.object_numbers().len(), count);

    // A foreign annotation's appearance may be shared, so it gets a new one.
    let mut d = doc.get(r).unwrap().as_dict().unwrap().clone();
    d.set(b"NM", Object::string(b"other-tool-1"));
    doc.set(r, d);
    let b = pagebara_annot::read(&doc, r).unwrap();
    update(&mut doc, r, &b).unwrap();
    assert_ne!(appearance_stream(&doc, r).0, ap);
    reopen(&doc, "appearance-reuse");
}
