//! Shadings of every type with functions of every type: the first bytes choose the type,
//! colour space, function and mesh layout; the rest is the function or mesh data (a
//! PostScript calculator program for type 4 functions).

#![no_main]

#[path = "common/mod.rs"]
mod common;

use common::stream;
use libfuzzer_sys::fuzz_target;
use pagebara_syntax::Document;

const SPACES: [(&str, usize); 4] = [
    ("/DeviceGray", 1),
    ("/DeviceRGB", 3),
    ("/DeviceCMYK", 4),
    ("[/Indexed/DeviceRGB 1<ff0000 0000ff>]", 1),
];

/// The function: inline, or `6 0 R` with its stream dictionary (types 0 and 4 read the
/// data as samples or as a program).
fn function(kind: u8, inputs: usize, outputs: usize) -> (String, Option<String>) {
    let domain = "0 1 ".repeat(inputs);
    let range = "0 1 ".repeat(outputs);
    let zeros = "0 ".repeat(outputs);
    let ones = "1 ".repeat(outputs);
    match kind % 4 {
        0 => (
            "6 0 R".into(),
            Some(format!(
                "/FunctionType 0/Domain[{domain}]/Range[{range}]/Size[{}]/BitsPerSample 8",
                "3 ".repeat(inputs)
            )),
        ),
        1 => (
            format!("<</FunctionType 2/Domain[0 1]/C0[{zeros}]/C1[{ones}]/N 1.5>>"),
            None,
        ),
        2 => (
            format!(
                "<</FunctionType 3/Domain[0 1]/Functions[<</FunctionType 2/Domain[0 1]/C0[{zeros}]/C1[{ones}]/N 1>> <</FunctionType 2/Domain[0 1]/C0[{ones}]/C1[{zeros}]/N 2>>]/Bounds[0.4]/Encode[0 1 1 0]>>"
            ),
            None,
        ),
        _ => (
            "6 0 R".into(),
            Some(format!("/FunctionType 4/Domain[{domain}]/Range[{range}]")),
        ),
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((head, body)) = data.split_first_chunk::<5>() else {
        return;
    };
    let [ty, cs, func, bits, flags] = *head;
    let ty = ty % 7 + 1;
    let (space, n) = SPACES[usize::from(cs % 4)];
    let bpc = [1, 2, 4, 8, 12, 16, 24, 32][usize::from(bits % 8)];
    let bpf = [1, 2, 4, 8][usize::from(bits / 8 % 4)];
    let with_fn = flags & 1 != 0 || matches!(ty, 1..=3);
    let inputs = if ty == 1 { 2 } else { 1 };
    let (f, fn_stream) = function(func, inputs, n);
    let fn_entry = if with_fn {
        format!("/Function {f}")
    } else {
        String::new()
    };
    let decode_c = if with_fn {
        "0 1".to_owned()
    } else {
        "0 1 ".repeat(n)
    };
    let extend = if flags & 2 != 0 { "/Extend[true true]" } else { "" };
    let bbox = if flags & 4 != 0 { "/BBox[10 10 50 50]" } else { "" };
    let common = format!("/ShadingType {ty}/ColorSpace {space}{fn_entry}{bbox}");
    let shading = match ty {
        1 => format!("<<{common}/Domain[0 1 0 1]/Matrix[64 0 0 64 0 0]>>").into_bytes(),
        2 => format!("<<{common}/Coords[0 0 64 {}]{extend}>>", flags >> 3).into_bytes(),
        3 => format!("<<{common}/Coords[20 20 0 40 40 {}]{extend}>>", flags >> 3).into_bytes(),
        _ => {
            let per_row = if matches!(ty, 5) { "/VerticesPerRow 3" } else { "" };
            stream(
                &format!(
                    "{common}/BitsPerCoordinate {bpc}/BitsPerComponent {}/BitsPerFlag {bpf}/Decode[0 64 0 64 {decode_c}]{per_row}",
                    [1, 2, 4, 8, 12, 16][usize::from(func % 6)]
                ),
                body,
            )
        }
    };
    let file = common::one_page(
        "/Shading<</S 5 0 R>>/Pattern<</P<</PatternType 2/Shading 5 0 R/Matrix[0.5 0.2 0 1 3 3]>>>>",
        b"q /S sh Q /Pattern cs /P scn 0 0 30 64 re f",
        &[
            shading,
            stream(fn_stream.as_deref().unwrap_or(""), body),
        ],
    );
    let Ok(doc) = Document::open(file) else {
        return;
    };
    common::render(&doc, 1, 40.0);
    common::render(&doc, 1, 200.0);
});
