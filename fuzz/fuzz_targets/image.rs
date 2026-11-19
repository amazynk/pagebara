//! An image XObject whose dictionary is chosen by the first bytes (size, depth, colour
//! space, filter, decode array, masks) over the rest as its data, drawn at two sizes.

#![no_main]

#[path = "common/mod.rs"]
mod common;

use common::stream;
use libfuzzer_sys::fuzz_target;
use pagebara_syntax::Document;

const SPACES: [&str; 10] = [
    "/DeviceGray",
    "/DeviceRGB",
    "/DeviceCMYK",
    "[/Indexed/DeviceRGB 3<ff0000 00ff00 0000ff ffffff>]",
    "[/Lab<</WhitePoint[0.95 1 1.09]/Range[-128 127 -128 127]>>]",
    "[/CalRGB<</WhitePoint[0.95 1 1.09]/Gamma[2.2 2.2 2.2]>>]",
    "[/Separation/Spot/DeviceRGB<</FunctionType 2/Domain[0 1]/C0[1 1 1]/C1[1 0 0]/N 1>>]",
    "[/DeviceN[/A/B]/DeviceCMYK<</FunctionType 4/Domain[0 1 0 1]/Range[0 1 0 1 0 1 0 1]/Length 17>>]",
    "[/ICCBased<</N 3/Alternate/DeviceRGB/Length 0>>]",
    "/Pattern",
];

const FILTERS: [&str; 10] = [
    "",
    "/Filter/FlateDecode",
    "/Filter/LZWDecode",
    "/Filter/RunLengthDecode",
    "/Filter/ASCIIHexDecode",
    "/Filter/DCTDecode",
    "/Filter/JPXDecode",
    "/Filter/JBIG2Decode",
    "/Filter/CCITTFaxDecode/DecodeParms<</K -1/Columns 17>>",
    "/Filter/FlateDecode/DecodeParms<</Predictor 15/Colors 3/BitsPerComponent 8/Columns 5>>",
];

fuzz_target!(|data: &[u8]| {
    let Some((head, body)) = data.split_first_chunk::<6>() else {
        return;
    };
    let [w, h, bpc, cs, filter, flags] = *head;
    let w = u32::from(w) % 48 + 1;
    let h = u32::from(h) % 48 + 1;
    let bpc = [1, 2, 4, 8, 16][usize::from(bpc % 5)];
    let mut dict = format!(
        "/Type/XObject/Subtype/Image/Width {w}/Height {h}/BitsPerComponent {bpc}{}",
        FILTERS[usize::from(filter % 10)]
    );
    if flags & 1 != 0 {
        dict += "/ImageMask true";
    } else {
        dict += &format!("/ColorSpace {}", SPACES[usize::from(cs % 10)]);
    }
    if flags & 2 != 0 {
        dict += "/Decode[1 0 1 0 1 0 1 0]";
    }
    if flags & 4 != 0 {
        dict += "/Interpolate true";
    }
    if flags & 8 != 0 {
        dict += "/SMask 6 0 R";
    }
    if flags & 16 != 0 {
        dict += "/Mask[0 3 0 3 0 3]";
    }
    if flags & 32 != 0 {
        dict += "/SMaskInData 1";
    }
    let mask = stream(
        &format!("/Type/XObject/Subtype/Image/Width {h}/Height {w}/ColorSpace/DeviceGray/BitsPerComponent 8"),
        body,
    );
    let file = common::one_page(
        "/XObject<</I 5 0 R>>",
        b"q 50 0 0 30 7 20 cm /I Do Q q 0 20 -10 0 30 5 cm /I Do Q",
        &[stream(&dict, body), mask],
    );
    let Ok(doc) = Document::open(file) else {
        return;
    };
    common::render(&doc, 1, 32.0);
    common::render(&doc, 1, 300.0);
});
