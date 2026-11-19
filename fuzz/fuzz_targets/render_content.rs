//! A content stream drawn with resources of every kind: standard and Type 3 fonts, images
//! and an image mask, a form with a transparency group, tiling and shading patterns, a
//! shading, and graphics states with blend modes and a soft mask.

#![no_main]

#[path = "common/mod.rs"]
mod common;

use common::stream;
use libfuzzer_sys::fuzz_target;
use pagebara_syntax::Document;

const RESOURCES: &str = "/Font<</F1<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>\
/F2<</Type/Font/Subtype/Type1/BaseFont/Times-Bold/Encoding/WinAnsiEncoding>>/F3 5 0 R>>\
/XObject<</Im 7 0 R/Mk 8 0 R/Fm 9 0 R>>\
/Pattern<</P1 10 0 R/P2<</PatternType 2/Shading 11 0 R>>>>\
/Shading<</Sh 11 0 R/Sh2<</ShadingType 3/ColorSpace/DeviceCMYK/Coords[10 10 0 30 30 20]/Function<</FunctionType 2/Domain[0 1]/C0[0 0 0 0]/C1[1 0 1 0]/N 1>>/Extend[true true]>>>>\
/ExtGState<</G1<</BM/Multiply/ca 0.5/CA 0.7>>/G2<</SMask<</S/Luminosity/G 9 0 R>>>>/G3<</BM/Difference/LW 3/D[[2 1]0]>>>>\
/ColorSpace<</Cs1[/Indexed/DeviceRGB 1<ff0000 0000ff>]/Cs2[/Separation/Spot/DeviceCMYK<</FunctionType 2/Domain[0 1]/C0[0 0 0 0]/C1[0 1 0 0]/N 1>>]/Cs3[/Lab<</WhitePoint[0.95 1 1.09]/Range[-100 100 -100 100]>>]>>";

fn extra() -> Vec<Vec<u8>> {
    vec![
        // 5: Type 3 font with two glyphs.
        b"<</Type/Font/Subtype/Type3/FontBBox[0 0 10 10]/FontMatrix[0.1 0 0 0.1 0 0]/CharProcs<</a 6 0 R/b 6 0 R>>/Encoding<</Differences[97/a/b]>>/FirstChar 97/LastChar 98/Widths[10 10]/Resources<<>>>>".to_vec(),
        // 6: its glyph.
        stream("", b"10 0 0 0 10 10 d1 0 0 10 10 re f"),
        // 7: 2 x 2 RGB image.
        stream("/Type/XObject/Subtype/Image/Width 2/Height 2/ColorSpace/DeviceRGB/BitsPerComponent 8", &[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]),
        // 8: 8 x 2 stencil mask.
        stream("/Type/XObject/Subtype/Image/Width 8/Height 2/ImageMask true/BitsPerComponent 1", &[0x5a, 0xa5]),
        // 9: form with a transparency group.
        stream("/Type/XObject/Subtype/Form/BBox[0 0 64 64]/Group<</S/Transparency/I true/K false>>/Resources<<>>", b"0.5 g 4 4 40 40 re f 1 0 0 RG 2 w 0 0 m 64 64 l S"),
        // 10: tiling pattern.
        stream("/PatternType 1/PaintType 1/TilingType 1/BBox[0 0 8 8]/XStep 8/YStep 8/Resources<<>>", b"1 0 0 rg 0 0 4 4 re f"),
        // 11: axial shading.
        b"<</ShadingType 2/ColorSpace/DeviceRGB/Coords[0 0 64 64]/Function<</FunctionType 2/Domain[0 1]/C0[1 0 0]/C1[0 0 1]/N 1>>>>".to_vec(),
    ]
}

fuzz_target!(|data: &[u8]| {
    let file = common::one_page(RESOURCES, data, &extra());
    let Ok(doc) = Document::open(file) else {
        return;
    };
    common::render(&doc, 1, 40.0);
    common::render(&doc, 1, 160.0);
});
