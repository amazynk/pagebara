//! Pictures of text for the live text benchmark, made here at run time: screenshots of an
//! app and of code, small print, two columns, a table, a photographed page, accented
//! Latin, text drawn as outlines and a full scanned page, each with the text it shows.

#![allow(dead_code)]

use std::fmt::Write as _;
use std::sync::atomic::AtomicBool;

use pagebara_content::Interpreter;
use pagebara_ocr::RgbImage;
use pagebara_render::{DefaultGlyphs, GlyphSource, PixelRect, RenderRequest, Renderer};
use pagebara_syntax::Document;

use crate::synth;

/// Characters outside WinAnsi, given codes 1.. by the fonts' `/Differences`.
const EXTRA: [(char, &str); 15] = [
    ('ł', "lslash"),
    ('Ł', "Lslash"),
    ('ż', "zdotaccent"),
    ('ą', "aogonek"),
    ('ę', "eogonek"),
    ('ś', "sacute"),
    ('ź', "zacute"),
    ('ń', "nacute"),
    ('ć', "cacute"),
    ('ě', "ecaron"),
    ('ř', "rcaron"),
    ('č', "ccaron"),
    ('ť', "tcaron"),
    ('ů', "uring"),
    ('ď', "dcaron"),
];

/// A PDF string (hex) of `s` in the fonts' encoding.
fn encode(s: &str) -> String {
    let mut out = String::from("<");
    for c in s.chars() {
        let b: u8 = match c {
            '‘' => 0x91,
            '’' => 0x92,
            '“' => 0x93,
            '”' => 0x94,
            '•' => 0x95,
            '–' => 0x96,
            '—' => 0x97,
            '€' => 0x80,
            'Š' => 0x8A,
            'š' => 0x9A,
            'Ž' => 0x8E,
            'ž' => 0x9E,
            'œ' => 0x9C,
            c if (c as u32) < 0x7F || (0xA0..=0xFF).contains(&(c as u32)) => c as u8,
            c => EXTRA
                .iter()
                .position(|e| e.0 == c)
                .map_or(b'?', |i| i as u8 + 1),
        };
        let _ = write!(out, "{b:02X}");
    }
    out.push('>');
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Font {
    Sans,
    Bold,
    Mono,
    Serif,
}

impl Font {
    fn res(self) -> &'static str {
        match self {
            Font::Sans => "F1",
            Font::Bold => "F2",
            Font::Mono => "F3",
            Font::Serif => "F4",
        }
    }
}

/// Page content built up from text and boxes; y grows downwards from the top here.
pub struct Canvas {
    pub w: f32,
    pub h: f32,
    content: String,
}

impl Canvas {
    pub fn new(w: f32, h: f32) -> Canvas {
        Canvas {
            w,
            h,
            content: String::new(),
        }
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, rgb: [f32; 3]) {
        let [r, g, b] = rgb;
        let _ = writeln!(
            self.content,
            "{r} {g} {b} rg {x} {} {w} {h} re f",
            self.h - y - h
        );
    }

    /// Text with its baseline at `y` from the top.
    pub fn text(&mut self, x: f32, y: f32, size: f32, font: Font, rgb: [f32; 3], s: &str) {
        let [r, g, b] = rgb;
        let _ = writeln!(
            self.content,
            "BT /{} {size} Tf {r} {g} {b} rg 1 0 0 1 {x} {} Tm {} Tj ET",
            font.res(),
            self.h - y,
            encode(s)
        );
    }

    /// Raw content (PDF y up).
    pub fn raw(&mut self, s: &str) {
        self.content.push_str(s);
        self.content.push('\n');
    }

    pub fn pdf(&self) -> Vec<u8> {
        page_pdf(self.w, self.h, &self.content, None)
    }
}

fn stream(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut o = format!("<<{dict}/Length {}>>stream\n", data.len()).into_bytes();
    o.extend(data);
    o.extend(b"\nendstream");
    o
}

/// One page of `w` × `h` points with the four fonts and, optionally, an RGB image `/Im0`.
pub fn page_pdf(w: f32, h: f32, content: &str, image: Option<&RgbImage>) -> Vec<u8> {
    let diffs: String = EXTRA.iter().map(|e| format!("/{}", e.1)).collect();
    let font = |base: &str| {
        format!(
            "<</Type/Font/Subtype/Type1/BaseFont/{base}/Encoding<</BaseEncoding/WinAnsiEncoding\
             /Differences[1{diffs}]>>>>"
        )
        .into_bytes()
    };
    let xobj = if image.is_some() {
        "/XObject<</Im0 9 0 R>>"
    } else {
        ""
    };
    let mut objs = vec![
        b"<</Type/Catalog/Pages 2 0 R>>".to_vec(),
        b"<</Type/Pages/Kids[3 0 R]/Count 1>>".to_vec(),
        format!(
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 {w} {h}]/Contents 4 0 R\
             /Resources<</Font<</F1 5 0 R/F2 6 0 R/F3 7 0 R/F4 8 0 R>>{xobj}>>>>"
        )
        .into_bytes(),
        stream("", content.as_bytes()),
        font("Helvetica"),
        font("Helvetica-Bold"),
        // Not Courier: its substitute on some systems is a Type 1 font drawn as nothing.
        font("CourierNewPSMT"),
        font("Times-Roman"),
    ];
    if let Some(img) = image {
        let z = miniz_oxide::deflate::compress_to_vec_zlib(&img.data, 6);
        objs.push(stream(
            &format!(
                "/Type/XObject/Subtype/Image/Width {}/Height {}/ColorSpace/DeviceRGB\
                 /BitsPerComponent 8/Filter/FlateDecode",
                img.width, img.height
            ),
            &z,
        ));
    }
    synth::pdf(&objs)
}

/// Page 0 of a PDF rendered at `dpi`.
pub fn render(pdf: &[u8], dpi: f32) -> RgbImage {
    let doc = Document::open(pdf.to_vec()).expect("pdf");
    let r = Renderer::new(doc);
    let (w, h) = r.page_size(0).expect("page");
    let s = dpi / 72.0;
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: (w * s).round() as u32,
        h: (h * s).round() as u32,
    };
    let img = r.render(0, &RenderRequest::new(s, rect)).expect("render");
    RgbImage::from_rgba(img.width as usize, img.height as usize, &img.data)
}

/// A page of `w` × `h` points showing `img` in `[x, y, width, height]` (PDF y up), with
/// `content` drawn before it.
pub fn image_page(img: &RgbImage, w: f32, h: f32, place: [f32; 4], content: &str) -> Vec<u8> {
    let [x, y, iw, ih] = place;
    let c = format!("{content}\nq {iw} 0 0 {ih} {x} {y} cm /Im0 Do Q\n");
    page_pdf(w, h, &c, Some(img))
}

fn gray_to_rgb(gray: &[u8], w: usize, h: usize) -> RgbImage {
    RgbImage::from_gray(w, h, gray)
}

fn to_gray(img: &RgbImage) -> Vec<u8> {
    img.data
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| ((u32::from(p[0]) * 3 + u32::from(p[1]) * 6 + u32::from(p[2])) / 10) as u8)
        .collect()
}

/// The page with its text drawn as filled glyph outlines instead of text.
pub fn outlined(pdf: &[u8]) -> Vec<u8> {
    let doc = Document::open(pdf.to_vec()).expect("pdf");
    let tree = pagebara_pages::PageTree::open(&doc);
    let p = tree.page(0).expect("page");
    let d = doc.get(p.r).expect("page").as_dict().expect("dict").clone();
    let res = doc.get_in(&d, b"Resources").as_dict().expect("res").clone();
    let list = Interpreter::new(&doc).page(&d, &res);
    let mut c = String::new();
    // Boxes first (anything but text), then the glyphs as paths.
    for cmd in &list.cmds {
        if let pagebara_content::Cmd::Fill { path, ctm, .. } = *cmd {
            let m = list.matrix(ctm);
            let path = list.path(path);
            c.push_str("0.95 0.95 0.95 rg\n");
            emit_path(&mut c, &path, &m);
            c.push_str("f\n");
        }
    }
    for run in &list.runs {
        let fr = &list.fonts[run.font as usize];
        let Some(glyphs) = DefaultGlyphs.load(&doc, &fr.font) else {
            continue;
        };
        c.push_str("0 0 0 rg\n");
        for g in list.run_glyphs(run) {
            let Some(o) = glyphs.outline(&doc, &fr.font, g.code) else {
                continue;
            };
            emit_path(&mut c, &o.path(), &run.glyph_matrix(g));
            c.push_str("f\n");
        }
    }
    let (w, h) = (p.media_box.width() as f32, p.media_box.height() as f32);
    page_pdf(w, h, &c, None)
}

fn emit_path(
    c: &mut String,
    path: &pagebara_content::display::Path<'_>,
    m: &pagebara_content::Matrix,
) {
    use pagebara_content::display::Verb;
    let mut pts = path.points.iter().map(|p| m.apply(*p));
    for v in path.verbs {
        match v {
            Verb::Move => {
                if let Some(p) = pts.next() {
                    let _ = writeln!(c, "{:.2} {:.2} m", p.x, p.y);
                }
            }
            Verb::Line => {
                if let Some(p) = pts.next() {
                    let _ = writeln!(c, "{:.2} {:.2} l", p.x, p.y);
                }
            }
            Verb::Cubic => {
                let (Some(a), Some(b), Some(e)) = (pts.next(), pts.next(), pts.next()) else {
                    return;
                };
                let _ = writeln!(
                    c,
                    "{:.2} {:.2} {:.2} {:.2} {:.2} {:.2} c",
                    a.x, a.y, b.x, b.y, e.x, e.y
                );
            }
            Verb::Close => c.push_str("h\n"),
        }
    }
}

/// How a case's text is compared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Check {
    /// Whitespace runs as one space, in order.
    Ordered,
    /// Lines in any order (layouts whose reading order is a matter of taste).
    Lines,
    /// Exactly, spaces and line breaks included (code).
    Exact,
}

pub struct Case {
    pub name: &'static str,
    pub pdf: Vec<u8>,
    pub truth: String,
    pub check: Check,
    /// Highest acceptable character error rate.
    pub max_cer: f32,
}

const BLACK: [f32; 3] = [0.0, 0.0, 0.0];
const WHITE: [f32; 3] = [1.0, 1.0, 1.0];
const GREY: [f32; 3] = [0.35, 0.35, 0.38];

/// An app window, 1000 × 700 pixels, embedded beside real text.
pub fn screenshot() -> Case {
    let mut c = Canvas::new(750.0, 525.0);
    c.rect(0.0, 0.0, 750.0, 525.0, [0.98, 0.98, 0.99]);
    c.rect(0.0, 0.0, 750.0, 28.0, [0.18, 0.18, 0.19]);
    c.text(
        285.0,
        18.5,
        10.0,
        Font::Bold,
        WHITE,
        "Invoice Manager - Settings",
    );
    c.rect(0.0, 28.0, 170.0, 497.0, [0.93, 0.93, 0.95]);
    let side = ["Dashboard", "Invoices", "Customers", "Reports", "Settings"];
    for (i, s) in side.iter().enumerate() {
        c.text(20.0, 60.0 + i as f32 * 28.0, 10.0, Font::Sans, GREY, s);
    }
    c.text(200.0, 66.0, 15.0, Font::Bold, BLACK, "Account settings");
    let rows = [
        ("Company name", "Northwind Traders Ltd."),
        ("Email address", "billing@northwind.example"),
        ("Default currency", "EUR (Euro)"),
        ("Payment terms", "30 days net"),
    ];
    for (i, (k, v)) in rows.iter().enumerate() {
        let y = 104.0 + i as f32 * 30.0;
        c.text(200.0, y, 10.0, Font::Sans, GREY, k);
        c.rect(340.0, y - 14.0, 280.0, 20.0, WHITE);
        c.text(348.0, y, 10.0, Font::Sans, BLACK, v);
    }
    let para = [
        "Invoices are numbered in the order they are created. Changing the",
        "prefix only affects new invoices; existing ones keep their numbers.",
    ];
    for (i, l) in para.iter().enumerate() {
        c.text(200.0, 246.0 + i as f32 * 14.0, 10.0, Font::Sans, BLACK, l);
    }
    c.rect(200.0, 290.0, 110.0, 26.0, [0.15, 0.39, 0.92]);
    c.text(218.0, 307.0, 10.0, Font::Bold, WHITE, "Save changes");
    c.rect(322.0, 290.0, 80.0, 26.0, [0.88, 0.88, 0.9]);
    c.text(343.0, 307.0, 10.0, Font::Sans, BLACK, "Cancel");
    let img = render(&c.pdf(), 96.0);
    assert_eq!((img.width, img.height), (1000, 700));
    // On a Letter page under a real heading and above a real caption.
    let heading = "BT /F1 14 Tf 72 740 Td (Setting up invoices) Tj ET\n\
                   BT /F1 9 Tf 72 360 Td (Figure 2: the settings window.) Tj ET";
    let pdf = image_page(&img, 612.0, 792.0, [72.0, 380.0, 468.0, 327.6], heading);
    let mut truth = vec!["Invoice Manager - Settings".to_owned()];
    truth.extend(side.iter().map(|s| s.to_string()));
    truth.push("Account settings".into());
    for (k, v) in rows {
        truth.push(format!("{k}\t{v}"));
    }
    truth.extend(para.iter().map(|s| s.to_string()));
    truth.push("Save changes\tCancel".into());
    Case {
        name: "screenshot",
        pdf,
        truth: truth.join("\n"),
        check: Check::Lines,
        max_cer: 0.03,
    }
}

pub const RUST: &str = "use std::collections::HashMap;

/// Counts the words in `text`.
fn count_words(text: &str) -> HashMap<&str, usize> {
    let mut counts = HashMap::new();
    for word in text.split_whitespace() {
        *counts.entry(word).or_insert(0) += 1;
    }
    counts
}

fn main() {
    let c = count_words(\"a b a\");
    assert_eq!(c[\"a\"], 2); // two of them
    println!(\"{:?} -> {}\", c, c.len());
}";

pub const PYTHON: &str = "def fib(n: int) -> list[int]:
    \"\"\"Return the first n numbers.\"\"\"
    a, b = 0, 1
    out = []
    while len(out) < n:
        out.append(a)
        a, b = b, a + b
    return out

if __name__ == \"__main__\":
    print(fib(10))  # [0, 1, 1, 2, ...]";

/// A screenshot of code in a monospaced font, light or dark.
fn code(name: &'static str, src: &str, dark: bool) -> Case {
    let lines: Vec<&str> = src.lines().collect();
    let (bg, fg) = if dark {
        ([0.12, 0.12, 0.13], [0.86, 0.86, 0.82])
    } else {
        ([0.99, 0.99, 0.98], [0.1, 0.1, 0.12])
    };
    let w = 660.0;
    let h = 24.0 + lines.len() as f32 * 14.0;
    let mut c = Canvas::new(w, h);
    c.rect(0.0, 0.0, w, h, bg);
    for (i, l) in lines.iter().enumerate() {
        c.text(14.0, 22.0 + i as f32 * 14.0, 10.0, Font::Mono, fg, l);
    }
    let img = render(&c.pdf(), 96.0);
    let pdf = image_page(
        &img,
        612.0,
        792.0,
        [60.0, 300.0, w * 0.75, h * 0.75],
        "BT /F1 11 Tf 60 720 Td (Listing 1) Tj ET",
    );
    Case {
        name,
        pdf,
        truth: src.to_owned(),
        check: Check::Exact,
        // The recogniser still misses a comma or a backtick now and then.
        max_cer: 0.02,
    }
}

pub fn code_light() -> Case {
    code("code", RUST, false)
}

pub fn code_dark() -> Case {
    code("code-dark", PYTHON, true)
}

const SMALL: &[&str] = &[
    "Terms of carriage. Goods are carried at the owner's risk unless a higher",
    "value is declared in writing before dispatch and the extra charge is paid.",
    "Claims for damage must reach us within seven days of delivery; claims for",
    "loss within twenty-eight days of dispatch. Our liability is limited to",
    "the declared value or 25.00 EUR per kilogram, whichever is lower.",
    "Perishable goods, live animals and currency are not accepted for carriage.",
];

/// Small print: 8.5 point text scanned at 150 dpi.
pub fn small() -> Case {
    let mut c = Canvas::new(420.0, 120.0);
    for (i, l) in SMALL.iter().enumerate() {
        c.text(12.0, 20.0 + i as f32 * 11.0, 8.5, Font::Sans, BLACK, l);
    }
    let img = render(&c.pdf(), 150.0);
    let pdf = image_page(&img, 420.0, 120.0, [0.0, 0.0, 420.0, 120.0], "");
    Case {
        name: "small-8.5pt-150dpi",
        pdf,
        truth: SMALL.join("\n"),
        check: Check::Ordered,
        max_cer: 0.03,
    }
}

const LEFT: &[&str] = &[
    "The harbour was rebuilt in",
    "1871 after a storm destroyed",
    "the old wooden pier. Stone",
    "came from the quarry above",
    "the town by a narrow railway",
    "that still runs in summer.",
];

const RIGHT: &[&str] = &[
    "Today the harbour serves",
    "fishing boats and a ferry",
    "to the islands, which sails",
    "twice a day from April to",
    "October and once a day in",
    "the winter months.",
];

/// A page in two columns, scanned at 200 dpi.
pub fn two_columns() -> Case {
    let mut c = Canvas::new(400.0, 150.0);
    c.text(20.0, 22.0, 13.0, Font::Bold, BLACK, "The Harbour");
    for (i, (l, r)) in LEFT.iter().zip(RIGHT).enumerate() {
        let y = 44.0 + i as f32 * 14.0;
        c.text(20.0, y, 10.5, Font::Serif, BLACK, l);
        c.text(215.0, y, 10.5, Font::Serif, BLACK, r);
    }
    let img = render(&c.pdf(), 200.0);
    let pdf = image_page(&img, 400.0, 150.0, [0.0, 0.0, 400.0, 150.0], "");
    let mut truth = vec!["The Harbour"];
    truth.extend(LEFT);
    truth.extend(RIGHT);
    Case {
        name: "two-columns",
        pdf,
        truth: truth.join("\n"),
        check: Check::Ordered,
        max_cer: 0.03,
    }
}

pub const TABLE: [[&str; 5]; 5] = [
    ["Region", "Q1", "Q2", "Q3", "Total"],
    ["North", "1,204", "1,398", "1,512", "4,114"],
    ["South", "987", "1,045", "1,120", "3,152"],
    ["East", "2,310", "2,005", "2,488", "6,803"],
    ["West", "640", "712", "699", "2,051"],
];

/// A table, scanned at 200 dpi; rows read across with tabs between cells.
pub fn table() -> Case {
    let mut c = Canvas::new(380.0, 130.0);
    let xs = [20.0, 120.0, 190.0, 260.0, 330.0];
    for (r, row) in TABLE.iter().enumerate() {
        let y = 26.0 + r as f32 * 20.0;
        for (k, cell) in row.iter().enumerate() {
            let font = if r == 0 { Font::Bold } else { Font::Sans };
            c.text(xs[k], y, 10.0, font, BLACK, cell);
        }
        if r == 0 {
            c.rect(16.0, y + 5.0, 350.0, 0.8, BLACK);
        }
    }
    let img = render(&c.pdf(), 200.0);
    let pdf = image_page(&img, 380.0, 130.0, [0.0, 0.0, 380.0, 130.0], "");
    let truth: Vec<String> = TABLE.iter().map(|r| r.join("\t")).collect();
    Case {
        name: "table",
        pdf,
        truth: truth.join("\n"),
        check: Check::Ordered,
        max_cer: 0.03,
    }
}

/// A photographed page: grey paper, uneven light, tilted 2.5°, noisy and soft.
pub fn photo() -> Case {
    let mut c = Canvas::new(480.0, 170.0);
    for (i, l) in synth::LINES.iter().enumerate() {
        c.text(16.0, 22.0 + i as f32 * 18.0, 11.0, Font::Serif, BLACK, l);
    }
    let img = render(&c.pdf(), 200.0);
    let (w, h) = (img.width, img.height);
    let gray = to_gray(&img);
    let tilted = synth::degrade(&gray, w, h, 2.5, 30.0, 7);
    // Paper grey and darker towards one corner; a little blur.
    let mut lit = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let shade = 0.88 - 0.25 * (x as f32 / w as f32) * (y as f32 / h as f32);
            lit[y * w + x] = (f32::from(tilted[y * w + x]) * shade) as u8;
        }
    }
    let mut soft = lit.clone();
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            let s: u32 = [
                (0, 0),
                (1, 0),
                (0, 1),
                (2, 1),
                (1, 2),
                (1, 1),
                (1, 1),
                (1, 1),
                (1, 1),
            ]
            .iter()
            .map(|(dx, dy)| u32::from(lit[(y + dy - 1) * w + x + dx - 1]))
            .sum();
            soft[y * w + x] = (s / 9) as u8;
        }
    }
    let img = gray_to_rgb(&soft, w, h);
    let pdf = image_page(&img, 480.0, 170.0, [0.0, 0.0, 480.0, 170.0], "");
    Case {
        name: "photo-scan",
        pdf,
        truth: synth::LINES.join("\n"),
        check: Check::Ordered,
        max_cer: 0.04,
    }
}

const ACCENTED: &[&str] = &[
    "Ça m’a étonné : où êtes-vous allés cet été ?",
    "Die Größe der Straße war übermäßig breit.",
    "Mañana el niño comerá en la estación.",
    "Zażółć gęślą jaźń, łódź płynie po rzece.",
    "Příliš žluťoučký kůň úpěl ďábelské ódy.",
    "Não há açúcar no pão do avô João.",
];

/// Accented Latin of six languages, scanned at 200 dpi.
pub fn accented() -> Case {
    let mut c = Canvas::new(360.0, 120.0);
    for (i, l) in ACCENTED.iter().enumerate() {
        c.text(14.0, 22.0 + i as f32 * 17.0, 11.0, Font::Sans, BLACK, l);
    }
    let img = render(&c.pdf(), 200.0);
    let pdf = image_page(&img, 360.0, 120.0, [0.0, 0.0, 360.0, 120.0], "");
    Case {
        name: "accented",
        pdf,
        truth: ACCENTED.join("\n"),
        check: Check::Ordered,
        max_cer: 0.04,
    }
}

/// A heading and a paragraph drawn as glyph outlines: no text, no picture.
pub fn outlines() -> Case {
    let mut c = Canvas::new(612.0, 300.0);
    c.text(40.0, 40.0, 18.0, Font::Bold, BLACK, "Annual Meeting");
    let body = [
        "The annual meeting takes place on 14 March at the town hall.",
        "Members may bring one guest; please register by 1 March.",
        "Coffee is served from 9:30, and the meeting starts at 10:00.",
    ];
    for (i, l) in body.iter().enumerate() {
        c.text(40.0, 70.0 + i as f32 * 16.0, 11.0, Font::Sans, BLACK, l);
    }
    let pdf = outlined(&c.pdf());
    let mut truth = vec!["Annual Meeting"];
    truth.extend(body);
    Case {
        name: "outlines",
        pdf,
        truth: truth.join("\n"),
        check: Check::Ordered,
        max_cer: 0.03,
    }
}

/// A full Letter page of 11 point text scanned at 300 dpi.
pub fn full_scan() -> Case {
    let mut lines: Vec<&str> = Vec::new();
    for _ in 0..5 {
        lines.extend_from_slice(synth::LINES);
        lines.push("");
    }
    lines.truncate(44);
    let (gray, w, h) = synth::render_gray(&synth::text_pdf(synth::TITLE, &lines, 11.0), 300.0);
    let gray = synth::degrade(&gray, w, h, 0.3, 20.0, 1);
    let img = gray_to_rgb(&gray, w, h);
    let pdf = image_page(&img, 612.0, 792.0, [0.0, 0.0, 612.0, 792.0], "");
    let mut truth = vec![synth::TITLE];
    truth.extend(lines.iter().copied().filter(|l| !l.is_empty()));
    Case {
        name: "a4-scan-300dpi",
        pdf,
        truth: truth.join("\n"),
        check: Check::Ordered,
        max_cer: 0.03,
    }
}

pub fn all() -> Vec<Case> {
    vec![
        screenshot(),
        code_light(),
        code_dark(),
        small(),
        two_columns(),
        table(),
        photo(),
        accented(),
        outlines(),
        full_scan(),
    ]
}

/// Never cancelled.
pub fn go() -> AtomicBool {
    AtomicBool::new(false)
}
