//! Builds small classic PDF files for tests.

/// A file whose object `i + 1` is `objs[i]`, with a correct cross-reference table. The last
/// object is the info dictionary.
pub fn classic(objs: &[&str], root: u32) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offs = Vec::new();
    for (i, o) in objs.iter().enumerate() {
        offs.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let x = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objs.len() + 1).bytes());
    for o in offs {
        out.extend(format!("{o:010} 00000 n\r\n").bytes());
    }
    let trailer = format!(
        "trailer\n<</Size {} /Root {root} 0 R /Info {} 0 R>>\nstartxref\n{x}\n%%EOF\n",
        objs.len() + 1,
        objs.len()
    );
    out.extend(trailer.bytes());
    out
}

/// One A4 page showing `lines` in Helvetica 12 from the top left, 16 pt apart.
pub fn text(lines: &[&str]) -> Vec<u8> {
    let mut content = String::from("BT /F1 12 Tf 16 TL 72 770 Td\n");
    for l in lines {
        content.push_str(&format!("({l}) Tj T*\n"));
    }
    content.push_str("ET");
    let stream = format!("<</Length {}>>stream\n{content}\nendstream", content.len());
    classic(
        &[
            "<</Type/Catalog/Pages 2 0 R>>",
            "<</Type/Pages/Kids[3 0 R]/Count 1>>",
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>>>",
            &stream,
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>",
            "<</Title(Text)>>",
        ],
        1,
    )
}

/// A4 pages each showing its lines as [`text`] does, sharing one font.
pub fn pages(pages: &[&[&str]]) -> Vec<u8> {
    let n = pages.len();
    // Objects: catalog, pages, font, then a page and its contents for each page.
    let kids: Vec<String> = (0..n).map(|i| format!("{} 0 R", 4 + 2 * i)).collect();
    let mut objs = vec![
        "<</Type/Catalog/Pages 2 0 R>>".to_owned(),
        format!("<</Type/Pages/Kids[{}]/Count {n}>>", kids.join(" ")),
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>".to_owned(),
    ];
    for (i, lines) in pages.iter().enumerate() {
        let mut content = String::from("BT /F1 12 Tf 16 TL 72 770 Td\n");
        for l in *lines {
            content.push_str(&format!("({l}) Tj T*\n"));
        }
        content.push_str("ET");
        objs.push(format!(
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]/Contents {} 0 R/Resources<</Font<</F1 3 0 R>>>>>>",
            5 + 2 * i
        ));
        objs.push(format!(
            "<</Length {}>>stream\n{content}\nendstream",
            content.len()
        ));
    }
    objs.push("<</Title(Pages)>>".to_owned());
    let objs: Vec<&str> = objs.iter().map(String::as_str).collect();
    classic(&objs, 1)
}

/// Two tagged A4 pages. Page 1 draws its text in another order than the tags'; its tag
/// order is: heading "Tagged title" (a custom type role mapped to H1), a paragraph over two
/// hyphenated lines, a list item "Apples", a figure "A red circle", an H5 "Deep heading",
/// a link "Go to page two" (with its annotation) and last a note at the top right of the
/// page. An untagged URI link covers the note. Page 2 has one paragraph "Second page".
pub fn tagged() -> Vec<u8> {
    let bdc = |tag: &str, mcid: u32, size: u32, x: u32, y: u32, s: &str| {
        format!("/{tag} <</MCID {mcid}>> BDC BT /F1 {size} Tf {x} {y} Td ({s}) Tj ET EMC\n")
    };
    let content = [
        "/P <</MCID 1>> BDC BT /F1 12 Tf 14 TL 72 720 Td \
         (First paragraph of text that is hyphen-) Tj T* (ated.) Tj ET EMC\n"
            .to_string(),
        bdc("H1", 0, 24, 72, 760, "Tagged title"),
        bdc("LI", 2, 12, 72, 660, "Apples"),
        "/Figure <</MCID 3>> BDC 1 0 0 rg 72 560 60 60 re f EMC\n".into(),
        bdc("H5", 5, 12, 72, 520, "Deep heading"),
        bdc("Link", 6, 12, 72, 500, "Go to page two"),
        bdc("P", 4, 10, 400, 800, "Note at the top, tagged last."),
        "/Artifact BMC BT /F1 9 Tf 290 30 Td (1) Tj ET EMC".into(),
    ]
    .concat();
    let page2 = bdc("P", 0, 12, 72, 760, "Second page");
    let stream = |s: &str| format!("<</Length {}>>stream\n{s}\nendstream", s.len());
    classic(
        &[
            "<</Type/Catalog/Pages 2 0 R/StructTreeRoot 8 0 R/MarkInfo<</Marked true>>/Lang(en-GB)>>",
            "<</Type/Pages/Kids[3 0 R 6 0 R]/Count 2/MediaBox[0 0 595 842]>>",
            "<</Type/Page/Parent 2 0 R/Contents 4 0 R/Resources<</Font<</F1 5 0 R>>>>/StructParents 0/Annots[7 0 R 20 0 R]>>",
            &stream(&content),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>",
            "<</Type/Page/Parent 2 0 R/Contents 19 0 R/Resources<</Font<</F1 5 0 R>>>>/StructParents 1>>",
            "<</Type/Annot/Subtype/Link/Rect[70 495 160 512]/Border[0 0 0]/Dest[6 0 R/Fit]/StructParent 2>>",
            "<</Type/StructTreeRoot/K 9 0 R/ParentTree 18 0 R/ParentTreeNextKey 3/RoleMap<</Chapter/H1>>>>",
            "<</S/Document/Pg 3 0 R/K[10 0 R 11 0 R 12 0 R 14 0 R 15 0 R 16 0 R 17 0 R 21 0 R]>>",
            "<</S/Chapter/P 9 0 R/K 0>>",
            "<</S/P/P 9 0 R/K 1>>",
            "<</S/L/P 9 0 R/K 13 0 R>>",
            "<</S/LI/P 12 0 R/K 2>>",
            "<</S/Figure/P 9 0 R/Alt(A red circle)/K 3>>",
            "<</S/H5/P 9 0 R/K 5>>",
            "<</S/Link/P 9 0 R/K[6 <</Type/OBJR/Obj 7 0 R>>]>>",
            "<</S/P/P 9 0 R/K 4>>",
            "<</Nums[0[10 0 R 11 0 R 13 0 R 14 0 R 17 0 R 15 0 R 16 0 R]1[21 0 R]2 16 0 R]>>",
            &stream(&page2),
            "<</Type/Annot/Subtype/Link/Rect[398 795 560 812]/Border[0 0 0]/A<</S/URI/URI(https://example.org/)>>>>",
            "<</S/P/P 9 0 R/Pg 6 0 R/K 0>>",
            "<</Title(Tagged)>>",
        ],
        1,
    )
}

/// Catalog, a two-level page tree with inherited boxes and rotation, an outline with a
/// named destination, and an info dictionary.
pub fn sample() -> Vec<u8> {
    classic(
        &[
            "<</Type/Catalog/Pages 2 0 R/Outlines 7 0 R/Names 11 0 R>>",
            "<</Type/Pages/Kids[3 0 R 4 0 R]/Count 3/MediaBox[0 0 595 842]>>",
            "<</Type/Page/Parent 2 0 R>>",
            "<</Type/Pages/Parent 2 0 R/Kids[5 0 R 6 0 R]/Count 2/Rotate 90>>",
            "<</Type/Page/Parent 4 0 R/CropBox[10 10 310 410]>>",
            "<</Type/Page/Parent 4 0 R/MediaBox[0 0 612 792]/Rotate -90>>",
            "<</Type/Outlines/First 8 0 R/Last 9 0 R/Count 2>>",
            "<</Title(Intro)/Parent 7 0 R/Next 9 0 R/Dest[3 0 R/Fit]>>",
            "<</Title<FEFF00C9007400E9>/Parent 7 0 R/Prev 8 0 R/A<</S/GoTo/D[6 0 R/XYZ 0 0 0]>>/First 10 0 R/Last 10 0 R/Count 1>>",
            "<</Title(Named)/Parent 9 0 R/Dest(chap)>>",
            "<</Dests 12 0 R>>",
            "<</Kids[13 0 R]>>",
            "<</Limits[(a)(z)]/Names[(appendix) [3 0 R /Fit] (chap) [5 0 R /Fit]]>>",
            "<</Title(Sample Title)>>",
        ],
        1,
    )
}

/// One A4 page with a form of every kind of field: text (plain, multiline, comb), a check
/// box, a radio group, a combo box, a multi-select list box and a signature field. Field
/// appearances are left to the reader (`/NeedAppearances`), except the buttons' states.
pub fn form() -> Vec<u8> {
    let content = "BT /F1 12 Tf 72 780 Td (Application) Tj ET";
    let stream = format!("<</Length {}>>stream\n{content}\nendstream", content.len());
    let w = |rest: &str, rect: &str| {
        format!("<</Type/Annot/Subtype/Widget/P 3 0 R/F 4/Rect[{rect}]/MK<</BC[0 0 0]>>{rest}>>")
    };
    let off = "/Off 16 0 R";
    classic(
        &[
            "<</Type/Catalog/Pages 2 0 R/AcroForm 5 0 R>>",
            "<</Type/Pages/Kids[3 0 R]/Count 1>>",
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]/Contents 4 0 R/Resources<</Font<</F1 15 0 R>>>>/Annots[6 0 R 7 0 R 8 0 R 9 0 R 10 0 R 11 0 R 13 0 R 14 0 R 17 0 R]>>",
            &stream,
            "<</Fields[6 0 R 7 0 R 8 0 R 9 0 R 12 0 R 13 0 R 14 0 R 17 0 R]/NeedAppearances true/DA(/Helv 0 Tf 0 g)/DR<</Font<</Helv 15 0 R>>>>>>",
            &w(
                "/FT/Tx/T(name)/TU(Full name)/DA(/Helv 12 Tf 0 g)",
                "72 700 300 720",
            ),
            &w(
                "/FT/Tx/T(notes)/Ff 4096/DA(/Helv 10 Tf 0 g)",
                "72 600 300 680",
            ),
            &w(
                "/FT/Tx/T(code)/Ff 16777216/MaxLen 5/DA(/Helv 12 Tf 0 g)",
                "72 560 172 580",
            ),
            &w(
                &format!("/FT/Btn/T(agree)/V/Off/AS/Off/AP<</N<</Yes 16 0 R{off}>>>>"),
                "72 520 86 534",
            ),
            &w(
                &format!("/Parent 12 0 R/AS/Off/AP<</N<</S 16 0 R{off}>>>>"),
                "72 480 86 494",
            ),
            &w(
                &format!("/Parent 12 0 R/AS/Off/AP<</N<</L 16 0 R{off}>>>>"),
                "120 480 134 494",
            ),
            "<</FT/Btn/T(size)/Ff 49152/V/Off/Kids[10 0 R 11 0 R]>>",
            &w(
                "/FT/Ch/T(colour)/Ff 131072/Opt[(Red)(Green)(Blue)]/DA(/Helv 12 Tf 0 g)",
                "72 440 200 460",
            ),
            &w(
                "/FT/Ch/T(sizes)/Ff 2097152/Opt[[(s)(Small)][(l)(Large)][(x)(Extra large)]]/DA(/Helv 10 Tf 0 g)",
                "72 360 200 420",
            ),
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>",
            "<</Type/XObject/Subtype/Form/BBox[0 0 14 14]/Length 0>>stream\n\nendstream",
            &w("/FT/Sig/T(signature)", "72 280 300 320"),
            "<</Title(Form)>>",
        ],
        1,
    )
}
