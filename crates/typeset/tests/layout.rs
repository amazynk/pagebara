//! Paragraph layout: line breaks, justification, hyphenation, CJK and bidi rules, and
//! paragraph inference.

mod common;

use common::*;
use pagebara_syntax::Document;
use pagebara_typeset::hyphen::Lang;
use pagebara_typeset::{
    Align, Encoded, EncodedGlyph, EncodedRun, FontId, FontSession, Layout, LayoutWarning,
    LineHeight, LineSample, ParagraphStyle, RunFont, Shaper, Span, WriteMode, infer_paragraph,
    layout, write_content,
};

#[test]
fn content_with_synthetic_styles() {
    let mut span = Span::new("Bold slant", FontId(0), 10.0);
    span.synthetic = pagebara_typeset::Synthetic {
        bold: true,
        italic: true,
    };
    span.color = pagebara_typeset::Color::Rgb([1.0, 0.0, 0.0]);
    let spans = [span, Span::new(" plain", FontId(0), 10.0)];
    let l = layout(
        &mut Fixed { tj_spaces: false },
        &spans,
        &style(400.0, Align::Left),
    );
    let c = String::from_utf8(write_content(&l, &spans, 10.0, 100.0, &|_| "F1".into())).unwrap();
    assert!(c.contains("1 0 0 rg\n1 0 0 RG\n2 Tr\n0.3 w\n"), "{c}");
    assert!(c.contains("1 0 0.213 1 10 92 Tm"), "{c}");
    assert!(c.contains("0 Tr\n"), "{c}");
    assert!(c.starts_with("BT\n/F1 10 Tf\n") && c.ends_with("ET\n"));
}

/// Fixed metrics: Latin 0.5 em, space 0.25 em, ideographs 1 em; one-byte codes for ASCII
/// (spaces take `Tw` unless `tj_spaces`).
struct Fixed {
    tj_spaces: bool,
}

fn width_of(c: char) -> f32 {
    if c == ' ' {
        0.25
    } else if (c as u32) >= 0x2E80 {
        1.0
    } else {
        0.5
    }
}

impl Shaper for Fixed {
    fn shape(&mut self, font: FontId, text: &str) -> Encoded {
        let glyphs = text
            .char_indices()
            .filter(|(_, c)| !c.is_control())
            .map(|(i, c)| EncodedGlyph {
                code: c as u32 & 0xFFFF,
                code_len: if c.is_ascii() { 1 } else { 2 },
                cluster: i..i + c.len_utf8(),
                advance: width_of(c),
                kern: 0.0,
                word_space: c == ' ' && !self.tj_spaces,
            })
            .collect();
        Encoded {
            text: text.to_string(),
            runs: vec![EncodedRun {
                font: RunFont::Font(font),
                glyphs,
            }],
            missing: Vec::new(),
        }
    }
}

fn run(text: &str, style: &ParagraphStyle) -> Layout {
    layout(
        &mut Fixed { tj_spaces: false },
        &[Span::new(text, FontId(0), 10.0)],
        style,
    )
}

fn line_texts(l: &Layout) -> Vec<String> {
    l.lines
        .iter()
        .map(|x| l.text[x.range.clone()].to_string())
        .collect()
}

fn style(width: f32, align: Align) -> ParagraphStyle {
    ParagraphStyle {
        width,
        align,
        ..ParagraphStyle::default()
    }
}

/// The visible right end of a line.
fn right(l: &pagebara_typeset::Line) -> f32 {
    let g = l.fragments.last().unwrap().glyphs.last().unwrap();
    g.x + g.advance
}

#[test]
fn greedy_breaks() {
    let l = run("aaaa bbbb cccc dddd", &style(50.0, Align::Left));
    assert_eq!(line_texts(&l), ["aaaa bbbb ", "cccc dddd"]);
    assert!((l.lines[0].width - 42.5).abs() < 1e-4);
    assert_eq!(l.lines[1].baseline - l.lines[0].baseline, 12.0);
    let l = run("aaaa bbbb cccc dddd", &style(64.9, Align::Left));
    assert_eq!(line_texts(&l), ["aaaa bbbb ", "cccc dddd"]);
    let l = run("aaaa bbbb cccc dddd", &style(65.0, Align::Left));
    assert_eq!(line_texts(&l), ["aaaa bbbb cccc ", "dddd"]);
}

#[test]
fn golden_paragraph() {
    let text =
        "The quick brown fox jumps over the lazy dog. Pack my box with five dozen liquor jugs!";
    let l = run(text, &style(120.0, Align::Left));
    assert_eq!(
        line_texts(&l),
        [
            "The quick brown fox jumps ",
            "over the lazy dog. Pack my ",
            "box with five dozen liquor ",
            "jugs!"
        ]
    );
    // Every line fits, and none could take the next word.
    for (i, line) in l.lines.iter().enumerate() {
        assert!(line.width <= 120.0 + 1e-3);
        if let Some(next) = l.lines.get(i + 1) {
            let word = l.text[next.range.clone()].split(' ').next().unwrap();
            let w = line.width + 2.5 + word.chars().count() as f32 * 5.0;
            assert!(w > 120.0, "line {i} could take {word}");
        }
    }
}

#[test]
fn alignment() {
    let t = "aaaa bbbb cccc dddd";
    let l = run(t, &style(50.0, Align::Right));
    assert!((right(&l.lines[0]) - 50.0).abs() < 0.01);
    assert!((right(&l.lines[1]) - 50.0).abs() < 0.01);
    let l = run(t, &style(50.0, Align::Center));
    assert!((l.lines[0].x - 3.75).abs() < 0.01);
    let mut s = style(50.0, Align::Left);
    s.first_indent = 10.0;
    let l = run(t, &s);
    assert_eq!(line_texts(&l), ["aaaa ", "bbbb cccc ", "dddd"]);
    assert_eq!(l.lines[0].x, 10.0);
}

#[test]
fn justification_word_spacing_exact() {
    let text = "The quick brown fox jumps over the lazy dog and keeps running far away";
    for tj in [false, true] {
        let l = layout(
            &mut Fixed { tj_spaces: tj },
            &[Span::new(text, FontId(0), 10.0)],
            &style(137.3, Align::Justify),
        );
        let n = l.lines.len();
        assert!(n >= 3);
        for (i, line) in l.lines.iter().enumerate() {
            let first = &line.fragments[0].glyphs[0];
            assert!(first.x.abs() < 0.01);
            if i + 1 < n {
                assert!(line.justified);
                assert!(
                    (right(line) - 137.3).abs() < 0.01,
                    "line {i}: {}",
                    right(line)
                );
                let f = &line.fragments[0];
                if tj {
                    assert_eq!(f.word_spacing, 0.0);
                    assert!(f.glyphs.iter().any(|g| g.extra > 0.0));
                    assert!(f.tj_operand().contains('-'));
                } else {
                    assert!(f.word_spacing > 0.0);
                    assert!(f.glyphs.iter().all(|g| g.extra == 0.0));
                }
            } else {
                assert!(!line.justified);
                assert!(right(line) < 137.3);
            }
        }
    }
}

#[test]
fn justification_with_character_spacing() {
    let mut s = style(100.0, Align::Justify);
    s.max_char_spacing = 0.5;
    s.max_word_stretch = 0.5;
    let l = run("aaaaaa bbbbbb cccccccc", &s);
    let line = &l.lines[0];
    assert_eq!(line_texts(&l)[0], "aaaaaa bbbbbb ");
    // 62.5 pt of text in 100 pt: words stretch 1.25 pt, letters take the rest up to 0.5.
    let f = &line.fragments[0];
    assert!(
        f.char_spacing > 0.0 && f.char_spacing <= 0.5 + 1e-6,
        "{}",
        f.char_spacing
    );
    assert!((right(line) - 100.0).abs() < 0.01, "{}", right(line));
    // Character spacing alone for a line without spaces, capped.
    let l = run("aaaaaaaaaaaaaaaaaaaa b", &s);
    let line = &l.lines[0];
    assert!((line.fragments[0].char_spacing - 0.0).abs() < 1e-6 || right(line) <= 100.0 + 0.01);
}

#[test]
fn hyphenation() {
    let mut s = style(120.0, Align::Left);
    s.hyphenate = Some(Lang::EnUs);
    let text = "Typesetting with hyphenation looks nicer";
    let l = run(text, &s);
    let lines = line_texts(&l);
    assert_eq!(lines[0], "Typesetting with hyphen");
    assert!(l.lines[0].hyphenated);
    let last = l.lines[0].fragments.last().unwrap().glyphs.last().unwrap();
    assert_eq!(last.glyph.code, u32::from(b'-'));
    assert!(last.glyph.cluster.is_empty());
    assert!(right(&l.lines[0]) <= 120.0 + 1e-3);
    assert_eq!(lines.concat(), text);
    // Without hyphenation the word moves down whole.
    s.hyphenate = None;
    let l = run(text, &s);
    assert_eq!(line_texts(&l)[0], "Typesetting with ");
    // Other languages by their patterns, Unicode letters included.
    s.width = 80.0;
    s.hyphenate = Some(Lang::De);
    let l = run("Die Silbentrennung und Größenänderung", &s);
    assert_eq!(
        line_texts(&l),
        ["Die Silbentren", "nung und Größen", "änderung"]
    );
    assert!(l.lines[0].hyphenated && l.lines[1].hyphenated);
    s.hyphenate = Some(Lang::Uk);
    s.width = 50.0;
    let l = run("Україна перенесення", &s);
    assert_eq!(line_texts(&l), ["Україна ", "перенесен", "ня"]);
    // Soft hyphens are break points, invisible unless used.
    let l = run("ab\u{AD}cdefgh ijkl", &style(35.0, Align::Left));
    assert_eq!(line_texts(&l), ["ab", "cdefgh ", "ijkl"]);
    assert!(l.lines[0].hyphenated);
    let l = run("ab\u{AD}cd", &style(400.0, Align::Left));
    assert_eq!(l.text, "abcd");
    assert_eq!(l.lines[0].fragments[0].glyphs.len(), 4);
}

#[test]
fn cjk_rules() {
    let l = run("漢字漢字。漢字", &style(40.0, Align::Left));
    assert_eq!(line_texts(&l), ["漢字漢", "字。漢字"]);
    let l = run("「日本語」です", &style(30.0, Align::Left));
    for t in line_texts(&l) {
        assert!(!t.starts_with('」') && !t.ends_with('「'), "{t}");
    }
    let l = run("コーヒーです", &style(20.0, Align::Left));
    for t in line_texts(&l) {
        assert!(!t.starts_with('ー'), "{t}");
    }
}

#[test]
fn closing_punctuation_stays() {
    // "word)," would overflow by the punctuation: the whole word moves.
    let l = run("aaaa (bbb), cc", &style(50.0, Align::Left));
    assert_eq!(line_texts(&l), ["aaaa ", "(bbb), cc"]);
    let l = run("aaaa bbbb !", &style(49.0, Align::Left));
    assert_eq!(line_texts(&l), ["aaaa ", "bbbb !"]);
}

#[test]
fn long_words_break_inside() {
    let l = run("aaaaaaaaaaaaaaaaaaaa bb", &style(50.0, Align::Left));
    assert_eq!(line_texts(&l), ["aaaaaaaaaa", "aaaaaaaaaa ", "bb"]);
    assert!(l.warnings.is_empty());
}

#[test]
fn forced_breaks_and_empty_lines() {
    let l = run("one\n\ntwo three", &style(400.0, Align::Justify));
    assert_eq!(line_texts(&l), ["one\n", "\n", "two three"]);
    assert!(l.lines.iter().all(|x| !x.justified));
    assert_eq!(l.lines[1].fragments.len(), 0);
    assert_eq!(l.lines[2].baseline - l.lines[1].baseline, 12.0);
}

#[test]
fn spans_keep_styles() {
    let spans = [
        Span::new("Big ", FontId(0), 20.0),
        Span::new("small text", FontId(1), 10.0),
    ];
    let l = layout(
        &mut Fixed { tj_spaces: false },
        &spans,
        &style(400.0, Align::Left),
    );
    let f = &l.lines[0].fragments;
    assert_eq!(f.len(), 2);
    assert_eq!((f[0].size, f[1].size), (20.0, 10.0));
    assert!((f[1].x - 35.0).abs() < 1e-4);
    assert_eq!(l.lines[0].ascent, 16.0);
    let mut s = style(60.0, Align::Left);
    s.line_height = LineHeight::Fixed(15.0);
    let l = layout(&mut Fixed { tj_spaces: false }, &spans, &s);
    assert_eq!(l.lines[1].baseline - l.lines[0].baseline, 15.0);
}

#[test]
fn bidi_reorders_rtl_runs() {
    let l = run("abc אבג def", &style(400.0, Align::Left));
    let order: String = l.lines[0]
        .fragments
        .iter()
        .flat_map(|f| {
            f.glyphs
                .iter()
                .map(|g| l.text[g.glyph.cluster.clone()].to_string())
        })
        .collect();
    assert_eq!(order, "abc גבא def");
    assert!(l.lines[0].fragments.iter().any(|f| f.rtl));
    // Positions increase left to right.
    let xs: Vec<f32> = l.lines[0]
        .fragments
        .iter()
        .flat_map(|f| f.glyphs.iter().map(|g| g.x))
        .collect();
    assert!(xs.windows(2).all(|w| w[1] > w[0]));
    let l = run("مرحبا", &style(400.0, Align::Left));
    assert!(
        l.warnings
            .iter()
            .any(|w| matches!(w, LayoutWarning::NeedsShaping(_)))
    );
}

/// Line samples of a layout in a box at (left, top), as a PDF reader would measure them.
fn samples(l: &Layout, left: f32, top: f32, stretched: bool) -> Vec<LineSample> {
    l.lines
        .iter()
        .map(|line| {
            let glyphs: Vec<_> = line
                .fragments
                .iter()
                .flat_map(|f| f.glyphs.iter())
                .collect();
            let text = &l.text[line.range.clone()];
            let word = text.split(' ').next().unwrap_or("");
            LineSample {
                x0: left + line.x,
                x1: left + right(line),
                baseline: top - line.baseline,
                size: 10.0,
                first_word: 2.5 + word.chars().map(width_of).sum::<f32>() * 10.0,
                stretched: stretched && line.justified && glyphs.len() > 1,
            }
        })
        .collect()
}

#[test]
fn infers_paragraph_properties() {
    let text = "The quick brown fox jumps over the lazy dog. Pack my box with five dozen liquor jugs and go.";
    for (align, indent) in [
        (Align::Left, 0.0),
        (Align::Left, 15.0),
        (Align::Justify, 12.0),
        (Align::Right, 0.0),
        (Align::Center, 0.0),
    ] {
        let mut s = style(130.0, align);
        s.first_indent = indent;
        s.line_height = LineHeight::Fixed(13.0);
        let l = run(text, &s);
        let p = infer_paragraph(&samples(&l, 72.0, 700.0, true)).unwrap();
        assert_eq!(p.align, align, "{align:?}");
        assert!((p.line_height - 13.0).abs() < 1e-3);
        assert!(
            (p.first_indent - indent).abs() < 0.01,
            "{align:?}: {}",
            p.first_indent
        );
        if align == Align::Justify {
            assert!((p.width - 130.0).abs() < 0.01);
            assert!((p.left - 72.0).abs() < 0.01);
        }
        // Re-laying out with the inferred box gives the same lines.
        let mut s2 = style(p.width, p.align);
        s2.first_indent = p.first_indent;
        let l2 = run(text, &s2);
        assert_eq!(
            line_texts(&l2),
            line_texts(&l),
            "{align:?} width {}",
            p.width
        );
    }
}

/// End to end: a justified paragraph in non-embedded Helvetica with a word needing a
/// fallback, written as content, extracted and measured.
#[test]
fn paragraph_in_pdf() {
    let (mut doc, f) = doc_with_font(
        &[],
        &|_| {
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
                .into()
        },
        b"Hello",
    );
    let mut s = FontSession::new(Some(db()));
    let id = s.open_font(&doc, f).unwrap();
    let text = "Justified text in Helvetica keeps its codes, adds Łódź through Differences and borrows Ωμέγα from a fallback font so that nothing is lost.";
    let spans = [Span::new(text, id, 11.0)];
    let mut st = style(300.0, Align::Justify);
    st.hyphenate = Some(Lang::EnUs);
    let l = layout(&mut s, &spans, &st);
    assert!(l.warnings.is_empty(), "{:?}", l.warnings);
    let changes = s.finish(&mut doc, WriteMode::InPlace);
    let content = write_content(&l, &spans, 50.0, 220.0, &|f| res_name(f));
    let file = add_to_page(
        &mut doc,
        &changes,
        &format!("q\n{}Q\n", String::from_utf8(content).unwrap()),
    );
    let doc = Document::open(file.clone()).unwrap();
    let page = page_text(&doc);
    let n = l.lines.len();
    assert!(n >= 3);
    let mut all = String::new();
    for (i, line) in l.lines.iter().enumerate() {
        let chars = line_chars(&page, 220.0 - line.baseline);
        let got: String = chars.iter().map(|c| c.2.as_str()).collect();
        let want: String = l.text[line.range.clone()].trim_end().to_string()
            + if line.hyphenated { "-" } else { "" };
        assert_eq!(got.replace(' ', ""), want.replace(' ', ""), "line {i}");
        all.push_str(&want);
        // Justified lines end at the box edge.
        if i + 1 < n {
            let end = chars.last().unwrap().1;
            assert!((end - 350.0).abs() < 0.01, "line {i} ends at {end}");
        }
        assert!((chars[0].0 - 50.0).abs() < 0.01);
    }
    assert!(all.contains("Łódź") && all.contains("Ωμέγα"));
    external_checks(&file, &["Justified"]);
}
