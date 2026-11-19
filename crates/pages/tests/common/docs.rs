//! Synthetic documents.

use super::{pdf, pdf_with, stream};

fn marked(page: u32) -> String {
    stream(&format!(
        "/OC /oc1 BDC /P <</MCID 0>> BDC BT /F1 12 Tf 20 150 Td (Page {page}) Tj ET EMC \
         /Span <</MCID 1>> BDC BT /F1 12 Tf 20 100 Td (more) Tj ET EMC EMC"
    ))
}

/// Three tagged pages (objects 3-5, showing `Page 1`..`Page 3`) with a structure tree,
/// article threads, a nested outline, named destinations, attachments, a script,
/// optional content, a radio group and a text field, document information and viewer
/// settings.
///
/// Structure: Document[P1(p1), Span(p1+p2), Link(p2: link annotation + MCID),
/// Sect(p3)[Para(p3), Span(inherits p3)]]. Thread A has beads on pages 1, 2, 3, thread B
/// one on page 2. Outline: Chapter 1 (p1) [Sec 1.1 (p2), Sec 1.2 (named, p3)],
/// Chapter 2 (closed, no destination) [Sec 2.1 (p3)], Web (URI).
pub fn tagged() -> Vec<u8> {
    let mut o: Vec<(u32, String)> = [
        (1, "<</Type/Catalog/Pages 2 0 R/StructTreeRoot 10 0 R/MarkInfo<</Marked true>>/Outlines 50 0 R/Names<</Dests 57 0 R/EmbeddedFiles 58 0 R/JavaScript 59 0 R>>/OCProperties<</OCGs[60 0 R]/D<</ON[60 0 R]/Order[60 0 R]>>>>/Threads[65 0 R 66 0 R]/AcroForm<</Fields[40 0 R 42 0 R]/DA(/Helv 0 Tf 0 g)/DR<</Font<</Helv 9 0 R>>>>>>/Lang(en-US)/PageMode/UseOutlines/PageLayout/OneColumn/ViewerPreferences<</DisplayDocTitle true>>>>".to_string()),
        (2, "<</Type/Pages/Kids[3 0 R 4 0 R 5 0 R]/Count 3/MediaBox[0 0 200 200]/Resources<</Font<</F1 9 0 R>>/Properties<</oc1 60 0 R>>>>>>".into()),
        (3, "<</Type/Page/Parent 2 0 R/Contents 6 0 R/StructParents 0/Annots[41 0 R 43 0 R]/B[70 0 R]>>".into()),
        (4, "<</Type/Page/Parent 2 0 R/Contents 7 0 R/StructParents 1/Annots[42 0 R 30 0 R]/B[71 0 R 73 0 R]>>".into()),
        (5, "<</Type/Page/Parent 2 0 R/Contents 8 0 R/StructParents 2/B[72 0 R]>>".into()),
        (6, marked(1)),
        (7, marked(2)),
        (8, marked(3)),
        (9, "<</Type/Font/Subtype/Type1/BaseFont/Helvetica>>".into()),
        (10, "<</Type/StructTreeRoot/K[11 0 R]/ParentTree 20 0 R/ParentTreeNextKey 4/IDTree 21 0 R/RoleMap<</Para/P>>>>".into()),
        (11, "<</Type/StructElem/S/Document/P 10 0 R/K[12 0 R 13 0 R 14 0 R 15 0 R]>>".into()),
        (12, "<</Type/StructElem/S/P/P 11 0 R/Pg 3 0 R/K 0/ID(p1)>>".into()),
        (13, "<</Type/StructElem/S/Span/P 11 0 R/K[<</Type/MCR/Pg 3 0 R/MCID 1>> <</Type/MCR/Pg 4 0 R/MCID 0>>]>>".into()),
        (14, "<</Type/StructElem/S/Link/P 11 0 R/Pg 4 0 R/K[<</Type/OBJR/Obj 30 0 R>> 1]>>".into()),
        (15, "<</Type/StructElem/S/Sect/P 11 0 R/Pg 5 0 R/K[16 0 R 17 0 R]>>".into()),
        (16, "<</Type/StructElem/S/Para/P 15 0 R/Pg 5 0 R/K 0/ID(p3)>>".into()),
        (17, "<</Type/StructElem/S/Span/P 15 0 R/K 1>>".into()),
        (20, "<</Nums[0[12 0 R 13 0 R]1[13 0 R 14 0 R]2[16 0 R 17 0 R]3 14 0 R]>>".into()),
        (21, "<</Names[(p1)12 0 R(p3)16 0 R]>>".into()),
        (30, "<</Type/Annot/Subtype/Link/Rect[0 0 50 50]/Dest[5 0 R/Fit]/P 4 0 R/StructParent 3>>".into()),
        (40, "<</FT/Btn/Ff 49152/T(choice)/V/A/Kids[41 0 R 43 0 R]>>".into()),
        (41, "<</Type/Annot/Subtype/Widget/Parent 40 0 R/P 3 0 R/Rect[10 10 20 20]/AS/A/AP<</N<</A 44 0 R/Off 45 0 R>>>>>>".into()),
        (43, "<</Type/Annot/Subtype/Widget/Parent 40 0 R/P 3 0 R/Rect[30 10 40 20]/AS/Off/AP<</N<</B 44 0 R/Off 45 0 R>>>>>>".into()),
        (42, "<</Type/Annot/Subtype/Widget/FT/Tx/T(name)/V(x)/P 4 0 R/Rect[10 60 100 80]/AP<</N 44 0 R>>>>".into()),
        (44, "<</Type/XObject/Subtype/Form/BBox[0 0 10 10]/Length 18>>stream\n0 g 0 0 10 10 re f\nendstream".into()),
        (45, "<</Type/XObject/Subtype/Form/BBox[0 0 10 10]/Length 0>>stream\n\nendstream".into()),
        (50, "<</Type/Outlines/First 51 0 R/Last 56 0 R/Count 5>>".into()),
        (51, "<</Title(Chapter 1)/Parent 50 0 R/Next 54 0 R/First 52 0 R/Last 53 0 R/Count 2/Dest[3 0 R/Fit]/SE 12 0 R>>".into()),
        (52, "<</Title(Sec 1.1)/Parent 51 0 R/Next 53 0 R/Dest[4 0 R/XYZ 0 100 0]>>".into()),
        (53, "<</Title(Sec 1.2)/Parent 51 0 R/Prev 52 0 R/Dest(n3)>>".into()),
        (54, "<</Title(Chapter 2)/Parent 50 0 R/Prev 51 0 R/Next 56 0 R/First 55 0 R/Last 55 0 R/Count -1>>".into()),
        (55, "<</Title(Sec 2.1)/Parent 54 0 R/A<</S/GoTo/D[5 0 R/Fit]>>>>".into()),
        (56, "<</Title(Web)/Parent 50 0 R/Prev 54 0 R/A<</S/URI/URI(https://example.com/)>>>>".into()),
        (57, "<</Names[(n3)[5 0 R/Fit]]>>".into()),
        (58, "<</Names[(a.txt)62 0 R]>>".into()),
        (59, "<</Names[(init)64 0 R]>>".into()),
        (60, "<</Type/OCG/Name(Layer A)>>".into()),
        (62, "<</Type/Filespec/F(a.txt)/UF(a.txt)/EF<</F 63 0 R>>>>".into()),
        (63, stream("hello")),
        (64, "<</S/JavaScript/JS(void 0;)>>".into()),
        (65, "<</Type/Thread/F 70 0 R/I<</Title(A)>>>>".into()),
        (66, "<</Type/Thread/F 73 0 R>>".into()),
        (70, "<</Type/Bead/T 65 0 R/N 71 0 R/V 72 0 R/P 3 0 R/R[0 0 100 100]>>".into()),
        (71, "<</Type/Bead/T 65 0 R/N 72 0 R/V 70 0 R/P 4 0 R/R[0 0 100 100]>>".into()),
        (72, "<</Type/Bead/T 65 0 R/N 70 0 R/V 71 0 R/P 5 0 R/R[0 0 100 100]>>".into()),
        (73, "<</Type/Bead/T 66 0 R/N 73 0 R/V 73 0 R/P 4 0 R/R[0 100 100 200]>>".into()),
        (80, "<</Title(Tagged)/Author(pagebara)>>".into()),
    ]
    .into_iter()
    .collect();
    o.sort_by_key(|x| x.0);
    pdf_with(&o, "/Root 1 0 R/Info 80 0 R")
}

/// `n` pages in a tree with `fanout` kids per node, inheriting a media box from the
/// root; page `i` is 100 + i/1000 points wide and shows `Page i+1`.
pub fn balanced(n: u32, fanout: u32) -> Vec<u8> {
    let mut o = vec![(1, "<</Type/Catalog/Pages 2 0 R>>".to_string())];
    // Pages are 3, 5, 7...; contents follow each page; nodes come after.
    let first_node = 3 + 2 * n;
    let mut next = first_node;
    let mut level: Vec<(u32, u32)> = (0..n).map(|i| (3 + 2 * i, 1)).collect();
    let mut parent = vec![0u32; (first_node + n) as usize];
    while level.len() > fanout as usize {
        let mut up = Vec::new();
        for chunk in level.chunks(fanout as usize) {
            let node = next;
            next += 1;
            let kids: Vec<String> = chunk.iter().map(|k| format!("{} 0 R", k.0)).collect();
            let count: u32 = chunk.iter().map(|k| k.1).sum();
            for k in chunk {
                if parent.len() <= k.0 as usize {
                    parent.resize(k.0 as usize + 1, 0);
                }
                parent[k.0 as usize] = node;
            }
            let rotate = if node % 2 == 0 { "/Rotate 90" } else { "" };
            o.push((
                node,
                format!(
                    "<</Type/Pages/Kids[{}]/Count {count}/Parent PARENT{rotate}>>",
                    kids.join(" ")
                ),
            ));
            up.push((node, count));
        }
        level = up;
    }
    for k in &level {
        if parent.len() <= k.0 as usize {
            parent.resize(k.0 as usize + 1, 0);
        }
        parent[k.0 as usize] = 2;
    }
    let kids: Vec<String> = level.iter().map(|k| format!("{} 0 R", k.0)).collect();
    o.push((
        2,
        format!(
            "<</Type/Pages/Kids[{}]/Count {n}/MediaBox[0 0 100 100]/Resources<</Font<</F1<</Type/Font/Subtype/Type1/BaseFont/Courier>>>>>>>>",
            kids.join(" ")
        ),
    ));
    for i in 0..n {
        let r = 3 + 2 * i;
        o.push((
            r,
            format!(
                "<</Type/Page/Parent {} 0 R/MediaBox[0 0 {} 100]/Contents {} 0 R>>",
                parent[r as usize],
                100 + i / 1000,
                r + 1
            ),
        ));
        o.push((r + 1, super::content(&format!("Page {}", i + 1))));
    }
    for (num, body) in o.iter_mut() {
        if body.contains("PARENT") {
            *body = body.replace("PARENT", &format!("{} 0 R", parent[*num as usize]));
        }
    }
    o.sort_by_key(|x| x.0);
    pdf(&o, 1)
}
