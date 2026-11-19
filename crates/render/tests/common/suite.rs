//! The synthetic feature suite: one small PDF per feature area.

#![allow(dead_code)]

use crate::pdf::{Pdf, hex, icc_rgb};

const A4: (f32, f32) = (300.0, 300.0);

pub fn suite() -> Vec<(&'static str, Vec<u8>)> {
    vec![
        ("paths", paths()),
        ("strokes", strokes()),
        ("dashes", dashes()),
        ("blend", blend()),
        ("transparency", transparency()),
        ("softmask", softmask()),
        ("shadings", shadings()),
        ("meshes", meshes()),
        ("images", images()),
        ("imagemasks", image_masks()),
        ("patterns", patterns()),
        ("colorspaces", colorspaces()),
        ("textclip", text_clip()),
        ("clips", clips()),
        ("annotations", annotations()),
        ("optional", optional()),
        ("type3", type3()),
        ("jpeg", jpeg()),
    ]
}

fn paths() -> Vec<u8> {
    let mut p = Pdf::new();
    let c = "\
1 0 0 rg 20 20 100 80 re f\n\
0 0 1 rg 150 20 m 280 20 l 215 120 l h f\n\
0 0.6 0 rg 20 140 m 80 280 l 140 140 l 10 230 l 150 230 l h f\n\
0.8 0.4 0 rg 160 140 m 300 280 l 300 140 l 160 280 l h f*\n\
0.5 0 0.5 rg 210 200 m 230 260 290 260 290 200 c 290 150 230 150 210 200 c h f\n\
0 g 20.3 120.7 m 120.6 120.7 l 120.6 128.2 l 20.3 128.2 l h f\n";
    p.page(A4, "", c);
    // Even-odd ring and nested contours.
    let c2 = "\
0 0 0.5 rg 50 50 m 250 50 l 250 250 l 50 250 l h 100 100 m 200 100 l 200 200 l 100 200 l h f*\n\
1 0.5 0 rg 50 50 m 250 50 l 250 250 l 50 250 l h 100 100 m 100 200 l 200 200 l 200 100 l h 120 120 m 180 120 l 180 180 l 120 180 l h f\n";
    p.page(A4, "", c2);
    p.finish()
}

fn strokes() -> Vec<u8> {
    let mut p = Pdf::new();
    let mut c = String::new();
    for (i, cap) in [0, 1, 2].iter().enumerate() {
        let y = 30.0 + i as f32 * 30.0;
        c += &format!("{cap} J 12 w 0 0 0.8 RG 40 {y} m 140 {y} l S\n");
    }
    for (i, join) in [0, 1, 2].iter().enumerate() {
        let x = 170.0 + i as f32 * 45.0;
        c += &format!(
            "{join} j 0 J 10 w 0.8 0 0 RG {x} 30 m {} 90 l {} 30 l S\n",
            x + 15.0,
            x + 30.0
        );
    }
    c += "0 j 2 M 8 w 0 0.5 0 RG 30 120 m 60 180 l 90 120 l S\n";
    c += "0 j 20 M 8 w 0 0.5 0 RG 110 120 m 125 180 l 140 120 l S\n";
    c += "0 w 0 G 160 120 m 290 180 l S 0.1 w 160 130 m 290 190 l S 0.5 w 160 140 m 290 200 l S\n";
    c += "q 3 0 0 1 0 0 cm 1 J 1 j 4 w 0.5 0 0.5 RG 10 220 m 30 280 l 60 230 l S Q\n";
    c += "1 J 1 j 15 w 0 0.4 0.8 RG 200 230 m 250 300 300 200 240 220 c S\n";
    c += "2 J 0 j 6 w 0.3 0.3 0.3 RG 120 220 m 120 220 l S 1 J 130 260 m 130 260 l S\n";
    p.page(A4, "", &c);
    p.finish()
}

fn dashes() -> Vec<u8> {
    let mut p = Pdf::new();
    let c = "\
4 w 0 0 0 RG [10 5] 0 d 20 30 m 280 30 l S\n\
[10 5] 7 d 20 50 m 280 50 l S\n\
[3] 0 d 20 70 m 280 70 l S\n\
1 J [0 10] 0 d 20 90 m 280 90 l S\n\
0 J [20 4 4 4] 0 d 0.7 0 0 RG 50 150 m 150 250 250 50 280 180 c S\n\
[15 10] 3 d 0 0 1 RG 3 w 40 180 80 80 re S\n\
1 j [8 3] 0 d 0 0.5 0 RG 2 w 160 200 m 200 280 l 240 200 l h S\n";
    p.page(A4, "", c);
    p.finish()
}

fn blend() -> Vec<u8> {
    let mut p = Pdf::new();
    let modes = [
        "Normal",
        "Multiply",
        "Screen",
        "Overlay",
        "Darken",
        "Lighten",
        "ColorDodge",
        "ColorBurn",
        "HardLight",
        "SoftLight",
        "Difference",
        "Exclusion",
        "Hue",
        "Saturation",
        "Color",
        "Luminosity",
    ];
    let mut res = String::from("/ExtGState << ");
    let mut c = String::from("0.2 0.6 0.9 rg 0 0 300 150 re f 0.9 0.8 0.1 rg 0 150 300 150 re f\n");
    c += "1 0.5 0.2 rg 0 0 150 300 re f\n";
    for (i, m) in modes.iter().enumerate() {
        res += &format!("/G{i} << /BM /{m} /ca 0.85 >> ");
        let x = 10.0 + (i % 4) as f32 * 72.0;
        let y = 10.0 + (i / 4) as f32 * 72.0;
        c += &format!(
            "q /G{i} gs 0.8 0.2 0.6 rg {x} {y} 60 60 re f 0.1 0.9 0.4 rg {} {} 30 30 re f Q\n",
            x + 15.0,
            y + 15.0
        );
    }
    res += ">>";
    p.page(A4, &res, &c);
    p.finish()
}

fn transparency() -> Vec<u8> {
    let mut p = Pdf::new();
    // Isolated and knockout groups with constant alpha.
    let grp = |p: &mut Pdf, i: bool, k: bool| {
        p.stream(
            &format!(
                "/Type /XObject /Subtype /Form /BBox [0 0 120 120] /Group << /S /Transparency /I {i} /K {k} >> /Resources << /ExtGState << /A << /ca 0.6 /BM /Multiply >> >> >>"
            ),
            b"/A gs 1 0 0 rg 0 0 80 80 re f 0 0 1 rg 40 40 80 80 re f",
        )
    };
    let g1 = grp(&mut p, false, false);
    let g2 = grp(&mut p, true, false);
    let g3 = grp(&mut p, true, true);
    let g4 = grp(&mut p, false, true);
    let res = format!(
        "/XObject << /G1 {g1} 0 R /G2 {g2} 0 R /G3 {g3} 0 R /G4 {g4} 0 R >> /ExtGState << /H << /ca 0.5 >> /N << /ca 1 >> >>"
    );
    let c = "\
0.9 0.9 0.3 rg 0 0 300 300 re f\n\
q 1 0 0 1 10 10 cm /G1 Do Q\n\
q 1 0 0 1 160 10 cm /G2 Do Q\n\
q 1 0 0 1 10 160 cm /G3 Do Q\n\
q /H gs 1 0 0 1 160 160 cm /G4 Do Q\n\
q /H gs 0 0.5 0 rg 100 100 100 100 re f Q\n";
    p.page(A4, &res, c);
    p.finish()
}

fn softmask() -> Vec<u8> {
    let mut p = Pdf::new();
    // Luminosity mask: a radial gradient group.
    let f = p.add("<< /FunctionType 2 /Domain [0 1] /C0 [1] /C1 [0] /N 1 >>");
    let sh = p.add(format!(
        "<< /ShadingType 3 /ColorSpace /DeviceGray /Coords [150 150 0 150 150 140] /Function {f} 0 R /Extend [true true] >>"
    ));
    let lum = p.stream(
        &format!("/Type /XObject /Subtype /Form /BBox [0 0 300 300] /Group << /S /Transparency /CS /DeviceGray >> /Resources << /Shading << /S {sh} 0 R >> >>"),
        b"/S sh",
    );
    // Alpha mask: shapes at partial opacity.
    let alpha = p.stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 300 300] /Group << /S /Transparency >> /Resources << /ExtGState << /Q << /ca 0.5 >> >> >>",
        b"0 g 20 20 120 260 re f /Q gs 160 20 120 260 re f",
    );
    let tr = p.add("<< /FunctionType 2 /Domain [0 1] /C0 [0] /C1 [1] /N 2 >>");
    let res = format!(
        "/ExtGState << /L << /SMask << /S /Luminosity /G {lum} 0 R >> >> /A << /SMask << /S /Alpha /G {alpha} 0 R >> >> /T << /SMask << /S /Luminosity /G {lum} 0 R /BC [0.5] /TR {tr} 0 R >> >> >>"
    );
    p.page(
        A4,
        &res,
        "0 0 0.8 rg 0 0 300 300 re f q /L gs 1 0.5 0 rg 0 0 300 300 re f Q",
    );
    p.page(
        A4,
        &res,
        "0.9 g 0 0 300 300 re f q /A gs 0.8 0 0 rg 0 0 300 300 re f Q",
    );
    p.page(
        A4,
        &res,
        "1 1 0 rg 0 0 300 300 re f q /T gs 0 0.3 0.6 rg 0 0 300 300 re f Q",
    );
    p.finish()
}

fn shadings() -> Vec<u8> {
    let mut p = Pdf::new();
    let f = p.add("<< /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >>");
    let f2 = p.add("<< /FunctionType 2 /Domain [0 1] /C0 [0 1 0] /C1 [1 1 0] /N 2 >>");
    let st = p.add(format!(
        "<< /FunctionType 3 /Domain [0 1] /Functions [{f} 0 R {f2} 0 R] /Bounds [0.4] /Encode [0 1 0 1] >>"
    ));
    let ax = p.add(format!(
        "<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [20 0 130 0] /Function {st} 0 R /Extend [false true] >>"
    ));
    let rad = p.add(format!(
        "<< /ShadingType 3 /ColorSpace /DeviceRGB /Coords [220 70 5 230 80 60] /Function {f} 0 R /Extend [true false] >>"
    ));
    let ps = p.stream(
        "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]",
        b"{ 2 copy mul 3 1 roll exch pop 0.5 }",
    );
    let fb = p.add(format!(
        "<< /ShadingType 1 /ColorSpace /DeviceRGB /Domain [0 1 0 1] /Matrix [100 0 0 100 20 170] /Function {ps} 0 R >>"
    ));
    let pat = p.add(format!(
        "<< /PatternType 2 /Shading {rad} 0 R /Matrix [1 0 0 1 -60 120] >>"
    ));
    let res =
        format!("/Shading << /A {ax} 0 R /R {rad} 0 R /F {fb} 0 R >> /Pattern << /P {pat} 0 R >>");
    let c = "\
q 10 10 140 120 re W n /A sh Q\n\
q 160 10 130 120 re W n /R sh Q\n\
q /F sh Q\n\
q /Pattern cs /P scn 160 150 m 290 150 l 225 290 l h f Q\n";
    p.page(A4, &res, c);
    p.finish()
}

fn meshes() -> Vec<u8> {
    let mut p = Pdf::new();
    // Type 4: two triangles, 8-bit coordinates and colours.
    let mut t4 = Vec::new();
    for (f, x, y, r, g, b) in [
        (0u8, 10u8, 10u8, 255u8, 0u8, 0u8),
        (0, 140, 10, 0, 255, 0),
        (0, 75, 140, 0, 0, 255),
        (1, 10, 140, 255, 255, 0),
    ] {
        t4.extend([f, x, y, r, g, b]);
    }
    let m4 = p.stream(
        "/ShadingType 4 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 255 0 255 0 1 0 1 0 1]",
        &t4,
    );
    // Type 5 lattice with a function.
    let f = p.add("<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0.5] /C1 [1 0.8 0] /N 1 >>");
    let mut t5 = Vec::new();
    for row in 0..3u8 {
        for col in 0..3u8 {
            let x = 150 + col * 50;
            let y = 10 + row * 60;
            let t = ((row as u32 * 3 + col as u32) * 255 / 8) as u8;
            t5.extend([x, y, t]);
        }
    }
    let m5 = p.stream(
        &format!("/ShadingType 5 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /VerticesPerRow 3 /Decode [0 255 0 255 0 1] /Function {f} 0 R"),
        &t5,
    );
    // Type 6 Coons patch.
    let pts: [(u32, u32); 12] = [
        (20, 160),
        (40, 200),
        (30, 250),
        (20, 290),
        (60, 280),
        (100, 295),
        (140, 290),
        (120, 240),
        (130, 200),
        (140, 160),
        (100, 175),
        (60, 150),
    ];
    let q = |v: u32| (v * 255 / 300) as u8;
    let mut t6 = vec![0u8];
    for (x, y) in pts {
        t6.extend([q(x), q(y)]);
    }
    for c in [[255u8, 0, 0], [0, 255, 0], [0, 0, 255], [255, 255, 0]] {
        t6.extend(c);
    }
    let m6 = p.stream(
        "/ShadingType 6 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 300 0 300 0 1 0 1 0 1]",
        &t6,
    );
    // Type 7 tensor patch.
    let mut t7 = vec![0u8];
    for (x, y) in pts {
        t7.extend([q(x + 150), q(y)]);
    }
    for (x, y) in [(200u32, 200u32), (200, 250), (250, 250), (250, 200)] {
        t7.extend([q(x), q(y)]);
    }
    for c in [[0u8, 255, 255], [255, 0, 255], [255, 255, 255], [0, 0, 0]] {
        t7.extend(c);
    }
    let m7 = p.stream(
        "/ShadingType 7 /ColorSpace /DeviceRGB /BitsPerCoordinate 8 /BitsPerComponent 8 /BitsPerFlag 8 /Decode [0 300 0 300 0 1 0 1 0 1]",
        &t7,
    );
    let res = format!("/Shading << /A {m4} 0 R /B {m5} 0 R /C {m6} 0 R /D {m7} 0 R >>");
    p.page(A4, &res, "/A sh /B sh /C sh /D sh");
    p.finish()
}

fn gradient_rgb(w: usize, h: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            v.extend([
                (x * 255 / w.max(1)) as u8,
                (y * 255 / h.max(1)) as u8,
                ((x + y) % 7 * 36) as u8,
            ]);
        }
    }
    v
}

fn images() -> Vec<u8> {
    let mut p = Pdf::new();
    let rgb = p.stream(
        "/Type /XObject /Subtype /Image /Width 40 /Height 30 /ColorSpace /DeviceRGB /BitsPerComponent 8",
        &gradient_rgb(40, 30),
    );
    let big = p.stream(
        "/Type /XObject /Subtype /Image /Width 400 /Height 300 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Interpolate true",
        &gradient_rgb(400, 300),
    );
    let gray1: Vec<u8> = (0..16)
        .map(|i| if i % 2 == 0 { 0xAA } else { 0x55 })
        .collect();
    let g1 = p.stream(
        "/Type /XObject /Subtype /Image /Width 16 /Height 8 /ColorSpace /DeviceGray /BitsPerComponent 1",
        &gray1,
    );
    let idx_data: Vec<u8> = (0..64)
        .map(|i| (i % 4) as u8 * 0x44 + (i / 16) as u8 * 0x11)
        .collect();
    let pal = hex(&[255, 0, 0, 0, 255, 0, 0, 0, 255, 255, 255, 0]);
    let ix = p.stream(
        &format!("/Type /XObject /Subtype /Image /Width 16 /Height 8 /ColorSpace [/Indexed /DeviceRGB 3 <{pal}>] /BitsPerComponent 4"),
        &idx_data[..64],
    );
    let inv = p.stream(
        "/Type /XObject /Subtype /Image /Width 40 /Height 30 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Decode [1 0 1 0 1 0]",
        &gradient_rgb(40, 30),
    );
    let cmyk: Vec<u8> = (0..20 * 20)
        .flat_map(|i| [(i % 20 * 12) as u8, (i / 20 * 12) as u8, 40, 20])
        .collect();
    let ck = p.stream(
        "/Type /XObject /Subtype /Image /Width 20 /Height 20 /ColorSpace /DeviceCMYK /BitsPerComponent 8",
        &cmyk,
    );
    let res = format!(
        "/XObject << /I {rgb} 0 R /B {big} 0 R /G {g1} 0 R /X {ix} 0 R /V {inv} 0 R /K {ck} 0 R >>"
    );
    let c = "\
q 120 90 0 0 cm 1 0 0 1 0 0 cm Q\n\
q 120 0 0 90 10 10 cm /I Do Q\n\
q 80 0 0 60 150 10 cm /B Do Q\n\
q 0 50 -100 0 280 10 cm /I Do Q\n\
q 120 0 0 60 10 120 cm /G Do Q\n\
q 120 0 0 60 150 120 cm /X Do Q\n\
q 70 20 -20 70 30 200 cm /V Do Q\n\
q 60 0 0 60 150 200 cm /K Do Q\n\
q 30 0 0 22.5 220 230 cm /B Do Q\n";
    p.page(A4, &res, c);
    p.finish()
}

fn image_masks() -> Vec<u8> {
    let mut p = Pdf::new();
    let mask: Vec<u8> = (0..32)
        .map(|i| if (i / 4) % 2 == 0 { 0x0F } else { 0xF0 })
        .collect();
    let m = p.stream(
        "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ImageMask true /BitsPerComponent 1",
        &mask,
    );
    let alpha: Vec<u8> = (0..32 * 32).map(|i| ((i % 32) * 8) as u8).collect();
    let sm = p.stream(
        "/Type /XObject /Subtype /Image /Width 32 /Height 32 /ColorSpace /DeviceGray /BitsPerComponent 8",
        &alpha,
    );
    let img = p.stream(
        &format!("/Type /XObject /Subtype /Image /Width 40 /Height 30 /ColorSpace /DeviceRGB /BitsPerComponent 8 /SMask {sm} 0 R"),
        &gradient_rgb(40, 30),
    );
    let key = p.stream(
        "/Type /XObject /Subtype /Image /Width 40 /Height 30 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Mask [0 120 0 255 0 255]",
        &gradient_rgb(40, 30),
    );
    let stencil = p.stream(
        "/Type /XObject /Subtype /Image /Width 16 /Height 16 /ImageMask true /BitsPerComponent 1 /Decode [1 0]",
        &mask,
    );
    let explicit = p.stream(
        &format!("/Type /XObject /Subtype /Image /Width 40 /Height 30 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Mask {stencil} 0 R"),
        &gradient_rgb(40, 30),
    );
    let res = format!("/XObject << /M {m} 0 R /S {img} 0 R /K {key} 0 R /E {explicit} 0 R >>");
    let c = "\
0.95 0.9 0.7 rg 0 0 300 300 re f\n\
q 0 0 1 rg 120 0 0 120 10 10 cm /M Do Q\n\
q 120 0 0 120 160 10 cm /S Do Q\n\
q 120 0 0 120 10 160 cm /K Do Q\n\
q 120 0 0 120 160 160 cm /E Do Q\n\
q 0.8 0 0 rg 1 0 0 1 100 130 cm BI /W 8 /H 8 /IM true /BPC 1 /D [1 0] /F /AHx ID 0F0FF0F00F0FF0F0> EI Q\n";
    p.page(A4, &res, c);
    // An inline image scaled up.
    let c2 = "q 200 0 0 200 50 50 cm BI /W 4 /H 4 /CS /RGB /BPC 8 /F /AHx ID \
FF000000FF000000FFFFFF00FF00FF00FFFF000000FFFFFF00FF00FF000000FFFF0000FF00000000FF808080FFFFFF000000FF00FF > EI Q";
    p.page(A4, "", c2);
    p.finish()
}

fn patterns() -> Vec<u8> {
    let mut p = Pdf::new();
    let cell = p.stream(
        "/PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 20 20] /XStep 20 /YStep 20 /Resources << >>",
        b"1 0 0 rg 0 0 10 10 re f 0 0 1 rg 10 10 10 10 re f 0 0.6 0 RG 1 w 0 0 m 20 20 l S",
    );
    let unc = p.stream(
        "/PatternType 1 /PaintType 2 /TilingType 1 /BBox [0 0 12 12] /XStep 15 /YStep 15 /Matrix [0.866 0.5 -0.5 0.866 0 0] /Resources << >>",
        b"6 6 m 6 0 l 12 6 l 6 12 l h f",
    );
    let res = format!(
        "/Pattern << /P {cell} 0 R /U {unc} 0 R >> /ColorSpace << /UC [/Pattern /DeviceRGB] >>"
    );
    let c = "\
/Pattern cs /P scn 10 10 130 130 re f\n\
q 0.5 0 0 0.5 150 10 cm /P scn 0 0 280 260 re f Q\n\
/UC cs 0.8 0.2 0.2 /U scn 20 160 260 120 re f\n";
    p.page(A4, &res, c);
    p.finish()
}

fn colorspaces() -> Vec<u8> {
    let mut p = Pdf::new();
    let icc = icc_rgb();
    let prof = p.stream("/N 3", &icc);
    let tint2 = p.add("<< /FunctionType 2 /Domain [0 1] /C0 [0 0 0 0] /C1 [0 0.6 1 0] /N 1 >>");
    let tint4 = p.stream(
        "/FunctionType 4 /Domain [0 1 0 1] /Range [0 1 0 1 0 1]",
        b"{ exch dup 0.5 mul 3 1 roll 1 exch sub }",
    );
    let res = format!(
        "/ColorSpace << /C [/CalRGB << /WhitePoint [0.9505 1 1.089] /Gamma [1.8 1.8 1.8] /Matrix [0.4497 0.2446 0.0252 0.3163 0.672 0.1412 0.1845 0.0833 0.9227] >>] \
/L [/Lab << /WhitePoint [0.9505 1 1.089] /Range [-100 100 -100 100] >>] /I [/ICCBased {prof} 0 R] \
/S [/Separation /Spot /DeviceCMYK {tint2} 0 R] /N [/DeviceN [/A /B] /DeviceRGB {tint4} 0 R] \
/G [/CalGray << /WhitePoint [0.9505 1 1.089] /Gamma 2.2 >>] /X [/Indexed /DeviceCMYK 1 <00FFFF00FF00FF00>] >>"
    );
    let c = "\
/C cs 1 0 0 sc 10 10 60 60 re f 0.2 0.8 0.4 sc 80 10 60 60 re f\n\
/L cs 50 60 -40 sc 150 10 60 60 re f 80 -20 70 sc 220 10 60 60 re f\n\
/I cs 0.9 0.3 0.1 sc 10 90 60 60 re f 0.2 0.5 0.9 sc 80 90 60 60 re f\n\
/S cs 1 sc 150 90 60 60 re f 0.4 sc 220 90 60 60 re f\n\
/N cs 0.3 0.9 sc 10 170 60 60 re f 1 0.2 sc 80 170 60 60 re f\n\
/G cs 0.5 sc 150 170 60 60 re f\n\
/X cs 0 sc 220 170 30 60 re f 1 sc 250 170 30 60 re f\n\
0.2 0.4 0.6 0.1 k 10 250 130 40 re f 0 0 0 1 k 150 250 130 40 re f\n";
    p.page(A4, &res, c);
    p.finish()
}

fn text_clip() -> Vec<u8> {
    let mut p = Pdf::new();
    let f = p.add("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>");
    let f2 = p.add("<< /Type /Font /Subtype /Type1 /BaseFont /Times-Roman >>");
    let res = format!("/Font << /F {f} 0 R /T {f2} 0 R >>");
    let c = "\
q BT /F 64 Tf 7 Tr 10 200 Td (CLIP) Tj ET\n\
0 0 1 rg 0 180 300 30 re f 1 0 0 rg 0 210 300 60 re f Q\n\
BT 0 Tr 0 g /T 20 Tf 10 120 Td (The quick brown fox jumps) Tj 0 -24 Td 1 Tr 0.5 w (over the lazy dog) Tj ET\n\
BT 2 Tr 1 0.5 0 rg 0 0 1 RG /F 36 Tf 10 30 Td (Fill+Stroke) Tj ET\n";
    p.page(A4, &res, c);
    p.finish()
}

fn clips() -> Vec<u8> {
    let mut p = Pdf::new();
    let c = "\
q 150 150 m 280 150 280 280 150 280 c 20 280 20 150 150 150 c h W n\n\
q 50 50 200 120 re W n 0.2 0.5 0.8 rg 0 0 300 300 re f Q\n\
0.9 0.2 0.2 rg 0 170 300 130 re f Q\n\
q 10 10 m 140 10 l 75 140 l h 30 30 m 120 30 l 75 120 l h W* n 0 0.6 0 rg 0 0 150 150 re f Q\n\
q 160.5 10.5 100.25 50.25 re W n 0 g 0 0 300 300 re f Q\n";
    p.page(A4, "", c);
    p.finish()
}

fn annotations() -> Vec<u8> {
    let mut p = Pdf::new();
    let n = p.stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 100 50] /Matrix [0 1 -1 0 0 0]",
        b"1 0 0 RG 4 w 2 2 96 46 re S 0 0 1 rg 30 15 40 20 re f",
    );
    let on = p.stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
        b"0 0.6 0 rg 2 2 16 16 re f",
    );
    let off = p.stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 20 20]",
        b"0 0 0 RG 1 w 2 2 16 16 re S",
    );
    let hidden = p.stream(
        "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
        b"1 0 1 rg 0 0 10 10 re f",
    );
    let a1 = p.add(format!(
        "<< /Type /Annot /Subtype /Square /Rect [20 150 120 280] /F 4 /AP << /N {n} 0 R >> >>"
    ));
    let a2 = p.add(format!(
        "<< /Type /Annot /Subtype /Widget /Rect [150 150 190 190] /F 4 /AS /On /AP << /N << /On {on} 0 R /Off {off} 0 R >> >> >>"
    ));
    let a3 = p.add(format!(
        "<< /Type /Annot /Subtype /Widget /Rect [200 150 240 190] /F 4 /AS /Off /AP << /N << /On {on} 0 R /Off {off} 0 R >> >> >>"
    ));
    let a4 = p.add(format!(
        "<< /Type /Annot /Subtype /Square /Rect [150 20 200 70] /F 2 /AP << /N {hidden} 0 R >> >>"
    ));
    let a5 = p.add(format!(
        "<< /Type /Annot /Subtype /Square /Rect [220 20 270 70] /F 36 /AP << /N {hidden} 0 R >> >>"
    ));
    p.page_with(
        A4,
        "",
        "0.9 g 0 0 300 120 re f",
        &format!("/Annots [{a1} 0 R {a2} 0 R {a3} 0 R {a4} 0 R {a5} 0 R]"),
    );
    p.finish()
}

fn optional() -> Vec<u8> {
    let mut p = Pdf::new();
    let on = p.add("<< /Type /OCG /Name (Shown) >>");
    let off = p.add("<< /Type /OCG /Name (Hidden) >>");
    let md = p.add(format!(
        "<< /Type /OCMD /OCGs [{on} 0 R {off} 0 R] /P /AllOn >>"
    ));
    p.catalog = format!(
        "/OCProperties << /OCGs [{on} 0 R {off} 0 R] /D << /ON [{on} 0 R] /OFF [{off} 0 R] >> >>"
    );
    let res = format!("/Properties << /A {on} 0 R /B {off} 0 R /M {md} 0 R >>");
    p.page(
        A4,
        &res,
        "/OC /A BDC 1 0 0 rg 20 20 100 100 re f EMC /OC /B BDC 0 0 1 rg 150 20 100 100 re f EMC \
         /OC /M BDC 0 1 0 rg 20 150 100 100 re f EMC",
    );
    p.finish()
}

fn type3() -> Vec<u8> {
    let mut p = Pdf::new();
    // Glyph A paints its own colours (d0); glyph B is a mask in the text colour (d1).
    let a = p.stream(
        "",
        b"500 0 d0 1 0 0 rg 0 0 400 700 re f 0 0 1 rg 100 100 200 200 re f",
    );
    let b = p.stream("", b"500 0 0 0 450 700 d1 0 0 m 450 0 l 225 700 l h f");
    let f = p.add(format!(
        "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 500 700] /FontMatrix [0.001 0 0 0.001 0 0] \
         /CharProcs << /a {a} 0 R /b {b} 0 R >> /Encoding << /Type /Encoding /Differences [65 /a /b] >> \
         /FirstChar 65 /LastChar 66 /Widths [500 500] /Resources << >> >>"
    ));
    let res = format!("/Font << /T {f} 0 R >>");
    p.page(
        A4,
        &res,
        "BT /T 60 Tf 20 200 Td 0 0.6 0 rg (ABAB) Tj ET BT /T 30 Tf 1 0 0.2 1 20 100 Tm 0.5 g (BBA) Tj ET",
    );
    p.finish()
}

fn jpeg() -> Vec<u8> {
    let mut p = Pdf::new();
    let (w, h) = (120usize, 90usize);
    let rgb = gradient_rgb(w, h);
    let opts = pagebara_jpeg::encode::EncodeOptions::default();
    let j = pagebara_jpeg::encode::encode(
        w as u32,
        h as u32,
        pagebara_jpeg::encode::Samples::Rgb(&rgb),
        &opts,
    )
    .expect("jpeg");
    let gray: Vec<u8> = rgb.chunks(3).map(|c| c[0] / 2 + c[1] / 2).collect();
    let g = pagebara_jpeg::encode::encode(
        w as u32,
        h as u32,
        pagebara_jpeg::encode::Samples::Gray(&gray),
        &opts,
    )
    .expect("jpeg");
    let i1 = p.stream(
        &format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /DCTDecode"),
        &j,
    );
    let i2 = p.stream(
        &format!("/Type /XObject /Subtype /Image /Width {w} /Height {h} /ColorSpace /DeviceGray /BitsPerComponent 8 /Filter /DCTDecode"),
        &g,
    );
    let res = format!("/XObject << /J {i1} 0 R /G {i2} 0 R >>");
    p.page(
        A4,
        &res,
        "q 260 0 0 130 20 160 cm /J Do Q q 60 0 0 45 20 20 cm /J Do Q q 120 0 0 90 150 20 cm /G Do Q",
    );
    p.finish()
}
