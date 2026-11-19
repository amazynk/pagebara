mod common;

use common::*;
use pagebara_content::display::{
    BlendMode, Cmd, ColorSpace, ColorSpaceKind, FillRule, ImageSource, LineCap, NONE, Verb,
};
use pagebara_content::oc::OptionalContent;
use pagebara_content::{Interpreter, Matrix, Point};
use pagebara_syntax::{ObjRef, Object};

#[test]
fn paths_transforms_and_colours() {
    let (_, l) = interpret(
        "q 2 0 0 2 10 10 cm 1 0 0 rg 0 0 10 10 re f Q 0 0 m 10 10 l 5 5 15 15 20 0 c S 0.5 g 1 1 m 2 2 3 3 v 4 4 5 5 y h B*",
        "<<>>",
        &[],
    );
    assert_eq!(kinds(&l), ["fill", "stroke", "fill", "stroke"]);
    let Cmd::Fill {
        path,
        ctm,
        paint,
        rule,
    } = l.cmds[0]
    else {
        panic!()
    };
    assert_eq!(rule, FillRule::NonZero);
    assert_eq!(l.matrix(ctm), Matrix::new(2.0, 0.0, 0.0, 2.0, 10.0, 10.0));
    let p = l.path(path);
    assert_eq!(
        p.verbs,
        [Verb::Move, Verb::Line, Verb::Line, Verb::Line, Verb::Close]
    );
    assert_eq!(p.points[2], Point::new(10.0, 10.0));
    let fill = l.paint(paint);
    assert_eq!(fill.space, ColorSpace::DeviceRgb);
    assert_eq!(l.paint_components(fill), [1.0, 0.0, 0.0]);

    let Cmd::Stroke {
        path, ctm, paint, ..
    } = l.cmds[1]
    else {
        panic!()
    };
    assert_eq!(l.matrix(ctm), Matrix::IDENTITY);
    assert_eq!(l.path(path).verbs, [Verb::Move, Verb::Line, Verb::Cubic]);
    assert_eq!(l.paint_components(l.paint(paint)), [0.0]);

    let Cmd::Fill {
        path, paint, rule, ..
    } = l.cmds[2]
    else {
        panic!()
    };
    assert_eq!(rule, FillRule::EvenOdd);
    assert_eq!(l.paint_components(l.paint(paint)), [0.5]);
    let p = l.path(path);
    assert_eq!(p.verbs, [Verb::Move, Verb::Cubic, Verb::Cubic, Verb::Close]);
    // `v` repeats the current point, `y` the end point.
    assert_eq!(
        &p.points[1..4],
        [
            Point::new(1.0, 1.0),
            Point::new(2.0, 2.0),
            Point::new(3.0, 3.0)
        ]
    );
    assert_eq!(
        &p.points[4..7],
        [
            Point::new(4.0, 4.0),
            Point::new(5.0, 5.0),
            Point::new(5.0, 5.0)
        ]
    );
    assert!(matches!(l.cmds[3], Cmd::Stroke { path: p2, .. } if p2 == path));
}

#[test]
fn clips_pop_with_restore() {
    let (_, l) = interpret(
        "q 0 0 5 5 re W n q 1 1 2 2 re W* n 0 0 10 10 re f Q 0 0 1 1 re f Q 0 0 1 1 re f",
        "<<>>",
        &[],
    );
    assert_eq!(
        kinds(&l),
        [
            "clip", "clip", "fill", "pop-clip", "fill", "pop-clip", "fill"
        ]
    );
    assert!(matches!(
        l.cmds[1],
        Cmd::Clip {
            rule: FillRule::EvenOdd,
            ..
        }
    ));
    // Unbalanced clips at the end are popped.
    let (_, l) = interpret("0 0 5 5 re W n q 0 0 1 1 re W n", "<<>>", &[]);
    assert_eq!(kinds(&l), ["clip", "clip", "pop-clip", "pop-clip"]);
    let total: u32 = l
        .cmds
        .iter()
        .map(|c| match c {
            Cmd::PopClip { count } => *count,
            _ => 0,
        })
        .sum();
    assert_eq!(total, 2);
}

#[test]
fn state_stack_is_bounded_and_lenient() {
    let deep = "q ".repeat(1000) + "2 w " + &"Q ".repeat(1200) + "0 0 m 1 1 l S";
    let (_, l) = interpret(&deep, "<<>>", &[]);
    let Cmd::Stroke { style, .. } = l.cmds[0] else {
        panic!()
    };
    assert_eq!(l.strokes[style as usize].width, 1.0);
    // Wrong operand counts and types are skipped; extra leading operands ignored.
    let (_, l) = interpret(
        "1 rg /X 0 1 0 rg (s) 7 3 w 1 J 9 9 9 cm [3 1] /bad d 0 0 m 5 5 l S 9 4 0 0 1 1 re f",
        "<<>>",
        &[],
    );
    assert_eq!(kinds(&l), ["stroke", "fill"]);
    let Cmd::Stroke { style, ctm, .. } = l.cmds[0] else {
        panic!()
    };
    let s = l.strokes[style as usize];
    assert_eq!((s.width, s.cap, s.dash), (3.0, LineCap::Round, (0, 0)));
    assert_eq!(l.matrix(ctm), Matrix::IDENTITY);
    let Cmd::Fill { paint, .. } = l.cmds[1] else {
        panic!()
    };
    assert_eq!(l.paint_components(l.paint(paint)), [0.0, 1.0, 0.0]);
}

#[test]
fn ext_gstate() {
    let (_, l) = interpret(
        "/GS0 gs 0 0 m 1 1 l S 0 0 1 1 re f",
        "<</ExtGState<</GS0<</LW 3/LC 2/D[[2 1]0.5]/CA 0.5/ca 0.25/BM/Multiply/SMask<</S/Luminosity/G 5 0 R>>/OP true>>>>>>",
        &[stream("/Type/XObject/Subtype/Form/BBox[0 0 1 1]", "")],
    );
    let Cmd::Stroke { paint, style, .. } = l.cmds[0] else {
        panic!()
    };
    let s = l.strokes[style as usize];
    assert_eq!((s.width, s.cap, s.dash_phase), (3.0, LineCap::Square, 0.5));
    assert_eq!(l.dash(&s), [2.0, 1.0]);
    let p = l.paint(paint);
    assert_eq!(
        (p.alpha, p.blend, p.overprint),
        (0.5, BlendMode::Multiply, true)
    );
    assert_ne!(p.soft_mask, NONE);
    let Cmd::Fill { paint, .. } = l.cmds[1] else {
        panic!()
    };
    let p = l.paint(paint);
    assert_eq!((p.alpha, p.overprint), (0.25, true));
    assert!(matches!(
        l.soft_masks[p.soft_mask as usize].object,
        Object::Dict(_)
    ));
}

#[test]
fn colour_spaces_and_patterns() {
    let (_, l) = interpret(
        "/Sep cs 0 0 1 1 re f /ICC CS 0.1 0.2 0.3 SC 0 0 m 1 1 l S \
         /Pattern cs /P0 scn 0 0 1 1 re f /UP cs 0.3 /P0 scn 0 0 1 1 re f \
         /Idx cs 7 sc 0 0 1 1 re f",
        "<</ColorSpace<</Sep[/Separation/Spot/DeviceCMYK 6 0 R]/ICC[/ICCBased 5 0 R]\
          /UP[/Pattern/DeviceGray]/Idx[/Indexed/DeviceRGB 1<000000FFFFFF>]>>\
          /Pattern<</P0<</PatternType 2/Shading<<>>/Matrix[1 0 0 1 5 5]>>>>>>",
        &[
            stream("/N 3", "icc"),
            "<</FunctionType 2/Domain[0 1]/C0[0 0 0 0]/C1[0 0 0 1]/N 1>>".to_string(),
        ],
    );
    let paint = |i: usize| match l.cmds[i] {
        Cmd::Fill { paint, .. } | Cmd::Stroke { paint, .. } => l.paint(paint),
        _ => panic!(),
    };
    let space = |p: &pagebara_content::display::Paint| match p.space {
        ColorSpace::Other(i) => &l.color_spaces[i as usize],
        _ => panic!(),
    };
    let sep = paint(0);
    assert_eq!(space(sep).kind, ColorSpaceKind::Separation);
    assert_eq!(space(sep).base, Some(ColorSpace::DeviceCmyk));
    assert_eq!(l.paint_components(sep), [1.0]);
    let icc = paint(1);
    assert_eq!(
        (space(icc).kind, space(icc).components),
        (ColorSpaceKind::IccBased, 3)
    );
    assert_eq!(l.paint_components(icc), [0.1, 0.2, 0.3]);
    let pat = paint(2);
    assert_eq!(space(pat).kind, ColorSpaceKind::Pattern);
    assert_eq!(
        l.patterns[pat.pattern as usize].matrix,
        Matrix::translate(5.0, 5.0)
    );
    let up = paint(3);
    assert_eq!(space(up).base, Some(ColorSpace::DeviceGray));
    assert_eq!(l.paint_components(up), [0.3]);
    assert_ne!(up.pattern, NONE);
    let idx = paint(4);
    assert_eq!(space(idx).kind, ColorSpaceKind::Indexed);
    assert_eq!(l.paint_components(idx), [7.0]);
}

#[test]
fn forms_images_and_groups() {
    let (_, l) = interpret(
        "q 100 0 0 50 0 0 cm /Im1 Do Q /Fm1 Do",
        "<</XObject<</Im1 5 0 R/Fm1 6 0 R>>>>",
        &[
            stream(
                "/Type/XObject/Subtype/Image/Width 4/Height 2/ImageMask true",
                "xxxx",
            ),
            stream(
                "/Type/XObject/Subtype/Form/BBox[0 0 10 10]/Matrix[1 0 0 1 20 30]\
                 /Group<</S/Transparency/I true>>/Resources<</XObject<</Self 6 0 R/Im 5 0 R>>>>",
                "0 0 5 5 re f /Im Do /Self Do",
            ),
        ],
    );
    assert_eq!(
        kinds(&l),
        [
            "image",
            "clip",
            "begin-group",
            "fill",
            "image",
            "end-group",
            "pop-clip"
        ]
    );
    let Cmd::Image { image, ctm, .. } = l.cmds[0] else {
        panic!()
    };
    let im = &l.images[image as usize];
    assert_eq!((im.width, im.height, im.is_mask), (4, 2, true));
    assert!(matches!(im.source, ImageSource::XObject(Object::Ref(_))));
    assert_eq!(l.matrix(ctm), Matrix::new(100.0, 0.0, 0.0, 50.0, 0.0, 0.0));
    let Cmd::Fill { ctm, .. } = l.cmds[3] else {
        panic!()
    };
    assert_eq!(l.matrix(ctm), Matrix::translate(20.0, 30.0));
    let Cmd::BeginGroup { group } = l.cmds[2] else {
        panic!()
    };
    assert!(l.groups[group as usize].isolated);
}

#[test]
fn inline_images() {
    let (doc, l) = interpret(
        "q 2 0 0 2 0 0 cm BI /W 2 /H 1 /CS /RGB /BPC 8 /F [/AHx] /D [0 1 0 1 0 1] ID\nFF000000FF00>\nEI Q\n\
         BI /IM true /W 8 /H 1 ID\n\x01EI q /Cs1 cs BI /W 1 /H 1 /CS /Cs1 /BPC 8 ID\n\x02\nEI Q",
        "<</ColorSpace<</Cs1[/Indexed/DeviceRGB 0 <FF0000>]>>>>",
        &[],
    );
    assert_eq!(kinds(&l), ["image", "image", "image"]);
    let ImageSource::Inline(s) = &l.images[0].source else {
        panic!()
    };
    assert_eq!(s.dict.name(b"ColorSpace"), Some(&b"DeviceRGB"[..]));
    assert_eq!(s.dict.int(b"BitsPerComponent"), Some(8));
    assert_eq!(
        s.dict.get(b"Filter").and_then(Object::as_array).unwrap()[0].as_name(),
        Some(&b"ASCIIHexDecode"[..])
    );
    assert_eq!(doc.decode(s).unwrap().data, [255, 0, 0, 0, 255, 0]);
    assert!(l.images[1].is_mask);
    let ImageSource::Inline(s) = &l.images[1].source else {
        panic!()
    };
    assert_eq!(doc.decode(s).unwrap().data, [1]);
    let ImageSource::Inline(s) = &l.images[2].source else {
        panic!()
    };
    assert!(s.dict.get(b"ColorSpace").unwrap().as_array().is_some());
}

#[test]
fn optional_content() {
    let doc = page_doc(
        "/OC /off BDC 0 0 1 1 re f EMC /OC /on BDC 0 0 2 2 re f EMC \
         /OC /md BDC 0 0 3 3 re f EMC /Hidden Do /Shown Do /P <</MCID 0>> BDC 0 0 4 4 re f EMC",
        "<</Properties<</off 5 0 R/on 6 0 R/md 7 0 R>>/XObject<</Hidden 8 0 R/Shown 9 0 R>>>>",
        &[
            "<</Type/OCG/Name(Off)>>".to_string(),
            "<</Type/OCG/Name(On)>>".to_string(),
            "<</Type/OCMD/OCGs[5 0 R 6 0 R]/P/AllOn>>".to_string(),
            stream("/Subtype/Form/BBox[0 0 1 1]/OC 5 0 R", "0 0 1 1 re f"),
            stream("/Subtype/Form/BBox[0 0 1 1]/OC 6 0 R", "0 0 1 1 re f"),
        ],
        "/OCProperties<</OCGs[5 0 R 6 0 R]/D<</OFF[5 0 R]>>>>",
    );
    let l = run(&doc);
    assert_eq!(
        kinds(&l),
        [
            "begin-marked",
            "fill",
            "end-marked",
            "clip",
            "fill",
            "pop-clip",
            "begin-marked",
            "fill",
            "end-marked"
        ]
    );
    assert_eq!(l.marked[1].mcid(), Some(0));
    let Cmd::Fill { path, .. } = l.cmds[1] else {
        panic!()
    };
    assert_eq!(l.path(path).points[2], Point::new(2.0, 2.0));

    // A viewer's layer choices replace the default configuration.
    let mut oc = OptionalContent::new(&doc);
    oc.set_group(ObjRef::new(5, 0), true);
    oc.set_group(ObjRef::new(6, 0), false);
    let (page, res) = page_and_resources(&doc);
    let l = Interpreter::new(&doc)
        .with_optional_content(oc)
        .page(&page, &res);
    assert_eq!(
        kinds(&l),
        [
            "begin-marked",
            "fill",
            "end-marked",
            "clip",
            "fill",
            "pop-clip",
            "begin-marked",
            "fill",
            "end-marked"
        ]
    );
    let Cmd::Fill { path, .. } = l.cmds[1] else {
        panic!()
    };
    assert_eq!(l.path(path).points[2], Point::new(1.0, 1.0));
}

#[test]
fn type3_glyphs() {
    let doc = page_doc(
        "BT /T3 12 Tf (ab) Tj ET",
        "<</Font<</T3 5 0 R>>>>",
        &[
            "<</Type/Font/Subtype/Type3/FontBBox[0 0 1000 1000]/FontMatrix[0.001 0 0 0.001 0 0]\
              /CharProcs<</a 6 0 R/b 7 0 R>>/Encoding<</Differences[97/a/b]>>/FirstChar 97\
              /Widths[1000 500]>>"
                .to_string(),
            stream("", "1000 0 d0 1 0 0 rg 0 0 1000 1000 re f"),
            stream("", "500 0 0 0 500 500 d1 1 0 0 rg 0 0 500 500 re f"),
        ],
        "",
    );
    let l = run(&doc);
    let run0 = &l.runs[0];
    let g = l.run_glyphs(run0);
    assert_eq!((g[0].x, g[1].x), (0.0, 12.0));
    let font = l.fonts[run0.font as usize].font.clone();
    let (_, res) = page_and_resources(&doc);
    let mut it = Interpreter::new(&doc);
    let a = it.type3_glyph(&font, 97, &res).unwrap();
    assert_eq!(a.glyph_metrics.unwrap().bbox, None);
    let Cmd::Fill { paint, .. } = a.cmds[0] else {
        panic!()
    };
    assert_eq!(a.paint_components(a.paint(paint)), [1.0, 0.0, 0.0]);
    let b = it.type3_glyph(&font, 98, &res).unwrap();
    assert!(b.glyph_metrics.unwrap().bbox.is_some());
    // After d1 colour operators are ignored: the glyph takes the text colour.
    let Cmd::Fill { paint, .. } = b.cmds[0] else {
        panic!()
    };
    assert_eq!(b.paint_components(b.paint(paint)), [0.0]);
    assert!(it.type3_glyph(&font, 99, &res).is_none());
}

#[test]
fn shading_and_marked_content() {
    let (_, l) = interpret(
        "/Span <</ActualText (x)>> BDC /Sh0 sh EMC BX /Foo 1 2 unknownop EX",
        "<</Shading<</Sh0<</ShadingType 2/ColorSpace/DeviceRGB/Coords[0 0 1 0]>>>>>>",
        &[],
    );
    assert_eq!(kinds(&l), ["begin-marked", "shading", "end-marked"]);
    assert_eq!(l.marked[0].tag.as_bytes(), b"Span");
}

#[test]
fn garbage_never_panics() {
    let mut seed = 0x2545F4914F6CDD1Du64;
    let alphabet: &[&str] = &[
        "q", "Q", "cm", "1", "-2.5", "0", "(a)", "<41>", "[", "]", "re", "m", "l", "c", "f", "S",
        "W", "n", "BT", "ET", "Tf", "/F1", "Tj", "TJ", "Td", "'", "\"", "BI", "ID", "EI", "Do",
        "/Fm", "BDC", "EMC", "<<", ">>", "gs", "/G", "cs", "scn", "sh", "d", "Tz", "Tr", "d1",
    ];
    for _ in 0..200 {
        let mut s = String::new();
        for _ in 0..200 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            s.push_str(alphabet[(seed % alphabet.len() as u64) as usize]);
            s.push(' ');
        }
        let _ = interpret(
            &s,
            "<</Font<</F1 5 0 R>>/XObject<</Fm 6 0 R>>/ExtGState<</G<</LW 2>>>>>>",
            &[
                "<</Type/Font/Subtype/Type0/Encoding/Identity-H/DescendantFonts[<</Subtype/CIDFontType2/W[0[1 2 3]]>>]>>".to_string(),
                stream("/Subtype/Form/BBox[0 0 1 1]", "q /Fm Do Q (a) Tj"),
            ],
        );
    }
}
