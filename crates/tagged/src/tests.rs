use pagebara_content::Interpreter;
use pagebara_syntax::{Document, ObjRef};

use crate::{Block, ItemKind, PageContent, StructTree};

/// A classic file whose object `i + 1` is `objs[i]`; object 1 is the catalog.
fn classic(objs: &[String]) -> Document {
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
    let n = objs.len() + 1;
    out.extend(format!("trailer\n<</Size {n} /Root 1 0 R>>\nstartxref\n{x}\n%%EOF\n").bytes());
    Document::open(out).expect("test document opens")
}

/// One page (object 3) drawing `content` in Helvetica as /F1; the structure tree root is
/// object 6, `structure` are objects 6, 7, …
fn tagged(content: &str, structure: &[&str]) -> Document {
    let mut objs = vec![
        "<</Type/Catalog/Pages 2 0 R/StructTreeRoot 6 0 R/MarkInfo<</Marked true>>/Lang(en-GB)>>"
            .to_string(),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".into(),
        "<</Type/Page/Parent 2 0 R/MediaBox[0 0 595 842]/Contents 4 0 R/StructParents 0\
          /Resources<</Font<</F1 5 0 R>>>>>>"
            .into(),
        format!("<</Length {}>>stream\n{content}\nendstream", content.len()),
        "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>".into(),
    ];
    objs.extend(structure.iter().map(|s| s.to_string()));
    classic(&objs)
}

const PAGE: ObjRef = ObjRef::new(3, 0);

fn page_content(doc: &Document) -> PageContent {
    let tree = StructTree::load(doc).expect("structure tree");
    let page = doc.get(PAGE).unwrap();
    let page = page.as_dict().unwrap();
    let res = doc.get_in(page, b"Resources").as_dict().cloned().unwrap();
    let list = Interpreter::new(doc).page(page, &res);
    tree.page_content(doc, PAGE, &list)
}

fn summary(blocks: &[Block]) -> Vec<(ItemKind, &str)> {
    blocks.iter().map(|b| (b.kind, b.text.as_str())).collect()
}

fn text(mcid: u32, tag: &str, size: u32, y: u32, s: &str) -> String {
    format!("/{tag} <</MCID {mcid}>> BDC BT /F1 {size} Tf 72 {y} Td ({s}) Tj ET EMC\n")
}

#[test]
fn blocks_in_tree_order_with_roles_alt_actual_text_and_lang() {
    // Drawn in another order than the tags': the paragraph before the heading.
    let content = [
        text(1, "P", 12, 650, "Body text of the page."),
        text(0, "H1", 24, 700, "Title"),
        text(2, "LI", 12, 600, "First item"),
        text(3, "LI", 12, 580, "Second item"),
        "/Figure <</MCID 4>> BDC 72 400 100 100 re f EMC\n".into(),
        text(5, "Span", 12, 350, "Tel"),
        text(6, "P", 12, 300, "Bonjour"),
    ]
    .concat();
    let doc = tagged(
        &content,
        &[
            "<</Type/StructTreeRoot/K 7 0 R/RoleMap<</Heading/H1/Para/MyPara/MyPara/P>>>>",
            "<</Type/StructElem/S/Document/P 6 0 R/Pg 3 0 R\
              /K[8 0 R 9 0 R 10 0 R 13 0 R 14 0 R 15 0 R]>>",
            "<</S/Heading/K 0>>",
            "<</S/Para/K[1]>>",
            "<</S/L/K[11 0 R 12 0 R]>>",
            "<</S/LI/K 2>>",
            "<</S/LI/K<</Type/MCR/MCID 3/Pg 3 0 R>>>>",
            "<</S/Figure/Alt(A black square)/K 4>>",
            "<</S/P/K<</S/Span/ActualText(Telephone)/K 5>>>>",
            "<</S/P/Lang(fr-FR)/K 6>>",
        ],
    );
    let c = page_content(&doc);
    use ItemKind::*;
    assert_eq!(
        summary(&c.blocks),
        [
            (Heading(1), "Title"),
            (Paragraph, "Body text of the page."),
            (ListItem, "First item"),
            (ListItem, "Second item"),
            (Figure, "A black square"),
            (Paragraph, "Telephone"),
            (Paragraph, "Bonjour"),
        ]
    );
    assert_eq!(c.blocks[0].lang.as_deref(), Some("en-GB"));
    assert_eq!(c.blocks[6].lang.as_deref(), Some("fr-FR"));
    // Per-character boxes, in page space.
    let title = &c.blocks[0];
    assert_eq!(title.chars.len(), 5);
    assert!(
        title
            .chars
            .iter()
            .all(|(_, q)| q[0].y > 690.0 && q[0].y < 700.0)
    );
    // Replacement text has one box around what it replaces.
    let tel = &c.blocks[5];
    assert_eq!(tel.chars.len(), 1);
    assert_eq!(tel.chars[0].0, 0..9);
    // A figure's alternate text has one box around what it draws.
    let fig = &c.blocks[4];
    assert_eq!(fig.chars.len(), 1);
    let q = fig.chars[0].1;
    assert_eq!(
        (q[0].x, q[0].y, q[2].x, q[2].y),
        (72.0, 400.0, 172.0, 500.0)
    );
}

#[test]
fn tag_order_wins_over_layout_and_inline_pieces_join() {
    // The note at the top of the page is tagged last; one paragraph has two pieces on a
    // line and a third on the next, the heading `H` takes its level from sections.
    let content = [
        text(0, "Note", 10, 800, "Sidebar"),
        text(1, "Span", 12, 700, "Hello "),
        "/Span <</MCID 2>> BDC BT /F1 12 Tf 108 700 Td (world) Tj ET EMC\n".into(),
        text(3, "Span", 12, 686, "again"),
        text(4, "H", 14, 750, "Section heading"),
    ]
    .concat();
    let doc = tagged(
        &content,
        &[
            "<</Type/StructTreeRoot/K[7 0 R]>>",
            "<</S/Document/Pg 3 0 R/K[8 0 R 11 0 R]>>",
            "<</S/Sect/K[10 0 R 9 0 R]>>",
            "<</S/P/K[1 2 3]>>",
            "<</S/H/K 4>>",
            "<</S/Note/K 0>>",
        ],
    );
    let c = page_content(&doc);
    use ItemKind::*;
    assert_eq!(
        summary(&c.blocks),
        [
            (Heading(1), "Section heading"),
            (Paragraph, "Hello world\nagain"),
            (Note, "Sidebar"),
        ]
    );
    let p = &c.blocks[1];
    for (r, _) in &p.chars {
        assert!(!p.text[r.start as usize..r.end as usize].contains('\n'));
    }
}

#[test]
fn links_and_direct_content() {
    let content = [
        text(0, "P", 12, 700, "See page two"),
        text(1, "P", 12, 650, "Loose"),
    ]
    .concat();
    let doc = tagged(
        &content,
        &[
            "<</Type/StructTreeRoot/K 7 0 R>>",
            "<</S/Document/Pg 3 0 R/K[8 0 R 1]>>",
            "<</S/P/K<</S/Link/K[0 <</Type/OBJR/Obj 20 0 R>>]>>>>",
        ],
    );
    let c = page_content(&doc);
    assert_eq!(
        summary(&c.blocks),
        [
            (ItemKind::Paragraph, "See page two"),
            (ItemKind::Paragraph, "Loose")
        ]
    );
    assert_eq!(c.objects, [ObjRef::new(20, 0)]);
}

#[test]
fn cycles_and_garbage_are_survived() {
    let content = text(0, "P", 12, 700, "Still here");
    let doc = tagged(
        &content,
        &[
            "<</Type/StructTreeRoot/K[7 0 R 7 0 R (junk) 99 0 R]/RoleMap<</A/B/B/A>>>>",
            "<</S/A/Pg 3 0 R/K[8 0 R 7 0 R 9 0 R]>>",
            "<</S/P/K[0 7 0 R 8 0 R [[[[0]]]] <</Type/MCR>> <</Type/OBJR>>]>>",
            // Nesting by direct dictionaries, deeper than is read.
            &format!(
                "<</S/Div/K {}>>",
                "<</S/Div/K ".repeat(400) + &">>".repeat(400)
            ),
        ],
    );
    let c = page_content(&doc);
    assert_eq!(c.blocks.len(), 1);
    assert_eq!(c.blocks[0].text, "Still here\nStill here");

    // No tree, an empty one, or one with nothing on the page.
    let plain = tagged(&content, &["<</Type/StructTreeRoot>>"]);
    assert!(StructTree::load(&plain).is_none());
    let other = tagged(
        &content,
        &["<</Type/StructTreeRoot/K<</S/P/Pg 1 0 R/K 0>>>>"],
    );
    let tree = StructTree::load(&other).unwrap();
    assert!(!tree.has_page(PAGE));
}
