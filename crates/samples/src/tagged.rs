//! A tagged PDF whose content stream draws its text in another order than its structure
//! tree, for testing reading order (screen reader text, reading aloud, the reading view).

use std::fmt::Write as _;

use super::{Pdf, lit, width};

/// `tagged.pdf`: two A4 pages, `/MarkInfo /Marked`, `/Lang (en-GB)`, a parent tree.
///
/// Page 1 in tag order (what a screen reader reads):
/// 1. H1 "Accessible sample"
/// 2. P "First paragraph comes first in tag order."
/// 3. P "Second paragraph is drawn first in the content stream."
/// 4. LI "• Apples", 5. LI "• Pears" (each a `Lbl` and an `LBody`)
/// 6. Figure, alternate text "A blue square"
/// 7. Link "Go to the second page" (its annotation goes to page 2)
/// 8. Note "Sidebar note: tagged last, drawn at the top."
///
/// The content stream draws the second paragraph, then the first, then the heading, …, and
/// the sidebar note sits at the top right, above the heading: the layout analysis reads it
/// first, the tags last. The page number is an artifact. Page 2: H1 "Second page" and a P
/// "This page has a heading and a paragraph."
pub fn tagged() -> Vec<u8> {
    let mut pdf = Pdf::default();
    let catalog = pdf.reserve();
    let pages = pdf.reserve();
    let page1 = pdf.reserve();
    let page2 = pdf.reserve();
    let fonts = pdf.add(
        "<</F1<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>\
         /F2<</Type/Font/Subtype/Type1/BaseFont/Helvetica-Bold/Encoding/WinAnsiEncoding>>>>"
            .into(),
    );
    let r = |n: usize| format!("{n} 0 R");
    // Marked text: (tag, MCID, font, size, x, y, text).
    let show =
        |ops: &mut String, tag: &str, mcid: u32, font: &str, size: f64, x: f64, y: f64, s: &str| {
            let _ = writeln!(
                ops,
                "/{tag} <</MCID {mcid}>> BDC BT /{font} {size} Tf {x} {y} Td {} Tj ET EMC",
                lit(s)
            );
        };

    let link_text = "Go to the second page";
    let mut c1 = String::new();
    show(
        &mut c1,
        "P",
        2,
        "F1",
        12.0,
        72.0,
        680.0,
        "Second paragraph is drawn first in the content stream.",
    );
    show(
        &mut c1,
        "P",
        1,
        "F1",
        12.0,
        72.0,
        704.0,
        "First paragraph comes first in tag order.",
    );
    show(
        &mut c1,
        "H1",
        0,
        "F2",
        24.0,
        72.0,
        750.0,
        "Accessible sample",
    );
    show(&mut c1, "Lbl", 3, "F1", 12.0, 80.0, 640.0, "•");
    show(&mut c1, "LBody", 4, "F1", 12.0, 92.0, 640.0, "Apples");
    show(&mut c1, "Lbl", 5, "F1", 12.0, 80.0, 622.0, "•");
    show(&mut c1, "LBody", 6, "F1", 12.0, 92.0, 622.0, "Pears");
    let _ = writeln!(
        c1,
        "/Figure <</MCID 7>> BDC 0.15 0.39 0.92 rg 72 500 90 90 re f 0 g EMC"
    );
    let _ = writeln!(c1, "0.15 0.39 0.92 rg");
    show(&mut c1, "Link", 8, "F1", 12.0, 72.0, 470.0, link_text);
    let _ = writeln!(c1, "0 g");
    show(
        &mut c1,
        "Note",
        9,
        "F1",
        10.0,
        360.0,
        805.0,
        "Sidebar note: tagged last, drawn at the top.",
    );
    let _ = writeln!(
        c1,
        "/Artifact BMC 0.45 g BT /F1 9 Tf 280 36 Td (Page 1 of 2) Tj ET 0 g EMC"
    );
    let mut c2 = String::new();
    show(&mut c2, "H1", 0, "F2", 24.0, 72.0, 750.0, "Second page");
    show(
        &mut c2,
        "P",
        1,
        "F1",
        12.0,
        72.0,
        704.0,
        "This page has a heading and a paragraph.",
    );
    let contents1 = pdf.stream(&c1);
    let contents2 = pdf.stream(&c2);

    let link = pdf.add(format!(
        "<</Type/Annot/Subtype/Link/Rect[70 466 {:.2} 482]/Border[0 0 0]/Dest[{} /Fit]/StructParent 2>>",
        72.0 + width(link_text, 12.0) + 2.0,
        r(page2)
    ));
    for (page, contents, annots, key) in [
        (page1, contents1, format!("/Annots[{}]", r(link)), 0),
        (page2, contents2, String::new(), 1),
    ] {
        pdf.set(
            page,
            format!(
                "<</Type/Page/Parent {}/MediaBox[0 0 595 842]/Resources<</Font {}>>/Contents {}{annots}/StructParents {key}/Tabs/S>>",
                r(pages),
                r(fonts),
                r(contents)
            ),
        );
    }
    pdf.set(
        pages,
        format!("<</Type/Pages/Kids[{} {}]/Count 2>>", r(page1), r(page2)),
    );

    // The structure tree.
    let root = pdf.reserve();
    let document = pdf.reserve();
    let elem = |pdf: &mut Pdf, parent: usize, s: &str, page: usize, k: String| {
        pdf.add(format!(
            "<</Type/StructElem/S/{s}/P {}/Pg {}/K {k}>>",
            r(parent),
            r(page)
        ))
    };
    let h1 = elem(&mut pdf, document, "H1", page1, "0".into());
    let p1 = elem(&mut pdf, document, "P", page1, "1".into());
    let p2 = elem(&mut pdf, document, "P", page1, "2".into());
    let list = pdf.reserve();
    let li1 = pdf.reserve();
    let li2 = pdf.reserve();
    let lbl1 = elem(&mut pdf, li1, "Lbl", page1, "3".into());
    let body1 = elem(&mut pdf, li1, "LBody", page1, "4".into());
    let lbl2 = elem(&mut pdf, li2, "Lbl", page1, "5".into());
    let body2 = elem(&mut pdf, li2, "LBody", page1, "6".into());
    pdf.set(
        li1,
        format!(
            "<</Type/StructElem/S/LI/P {}/K[{} {}]>>",
            r(list),
            r(lbl1),
            r(body1)
        ),
    );
    pdf.set(
        li2,
        format!(
            "<</Type/StructElem/S/LI/P {}/K[{} {}]>>",
            r(list),
            r(lbl2),
            r(body2)
        ),
    );
    pdf.set(
        list,
        format!(
            "<</Type/StructElem/S/L/P {}/K[{} {}]>>",
            r(document),
            r(li1),
            r(li2)
        ),
    );
    let figure = pdf.add(format!(
        "<</Type/StructElem/S/Figure/P {}/Pg {}/Alt(A blue square)/K 7>>",
        r(document),
        r(page1)
    ));
    let link_elem = elem(
        &mut pdf,
        document,
        "Link",
        page1,
        format!("[8 <</Type/OBJR/Obj {}>>]", r(link)),
    );
    let note = elem(&mut pdf, document, "Note", page1, "9".into());
    let h1b = elem(&mut pdf, document, "H1", page2, "0".into());
    let p3 = elem(&mut pdf, document, "P", page2, "1".into());
    let kids = [h1, p1, p2, list, figure, link_elem, note, h1b, p3]
        .map(r)
        .join(" ");
    pdf.set(
        document,
        format!("<</Type/StructElem/S/Document/P {}/K[{kids}]>>", r(root)),
    );
    let mcids1 = [
        h1, p1, p2, lbl1, body1, lbl2, body2, figure, link_elem, note,
    ]
    .map(r)
    .join(" ");
    let parent_tree = pdf.add(format!(
        "<</Nums[0[{mcids1}] 1[{} {}] 2 {}]>>",
        r(h1b),
        r(p3),
        r(link_elem)
    ));
    pdf.set(
        root,
        format!(
            "<</Type/StructTreeRoot/K {}/ParentTree {}/ParentTreeNextKey 3>>",
            r(document),
            r(parent_tree)
        ),
    );
    pdf.set(
        catalog,
        format!(
            "<</Type/Catalog/Pages {}/StructTreeRoot {}/MarkInfo<</Marked true>>/Lang(en-GB)\
             /ViewerPreferences<</DisplayDocTitle true>>>>",
            r(pages),
            r(root)
        ),
    );
    let info = pdf.add("<</Title(Accessible sample)/Producer(pagebara samples)>>".into());
    pdf.finish(catalog, info)
}

#[cfg(test)]
mod tests {
    use pagebara_app_core::reading_order::ItemKind;
    use pagebara_app_core::session::Session;
    use pagebara_syntax::Document;

    #[test]
    fn tag_order_differs_from_layout_order() {
        let s = Session::new(None, Document::open(super::tagged()).unwrap());
        assert_eq!(s.page_count(), 2);
        let r = s.reading(0);
        assert!(r.tagged);
        let items: Vec<(ItemKind, &str)> =
            r.items.iter().map(|i| (i.kind, i.text.as_str())).collect();
        use ItemKind::*;
        assert_eq!(
            items,
            [
                (Heading(1), "Accessible sample"),
                (Paragraph, "First paragraph comes first in tag order."),
                (
                    Paragraph,
                    "Second paragraph is drawn first in the content stream."
                ),
                (ListItem, "• Apples"),
                (ListItem, "• Pears"),
                (Figure, "A blue square"),
                (Paragraph, "Go to the second page"),
                (Note, "Sidebar note: tagged last, drawn at the top."),
            ]
        );
        assert!(r.items.iter().all(|i| i.lang.as_deref() == Some("en-GB")));
        assert_eq!(r.links.len(), 1);
        assert_eq!(r.links[0].text, "Go to the second page");
        assert_eq!(r.links[0].target, "page 2");
        // The layout analysis reads the sidebar first, then the heading and paragraphs.
        let text = s.text.page(0);
        let at = |t: &str| text.text.find(t).unwrap();
        assert!(at("Sidebar note") < at("Accessible sample"));
        assert!(at("Accessible sample") < at("First paragraph"));
        assert!(at("First paragraph") < at("Second paragraph"));
        let r2 = s.reading(1);
        let texts: Vec<&str> = r2.items.iter().map(|i| i.text.as_str()).collect();
        assert_eq!(
            texts,
            ["Second page", "This page has a heading and a paragraph."]
        );
        // It renders.
        let renderer = pagebara_render::Renderer::new(s.doc().clone());
        assert!(pagebara_ocr::render_page(&renderer, 0, 30.0, &Default::default()).is_ok());
    }
}
