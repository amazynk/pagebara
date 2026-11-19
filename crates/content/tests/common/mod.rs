#![allow(dead_code)]

use pagebara_content::{DisplayList, Interpreter};
use pagebara_syntax::{Dict, Document, ObjRef};

/// A stream object body.
pub fn stream(dict: &str, data: &str) -> String {
    format!("<<{dict} /Length {}>>stream\n{data}\nendstream", data.len())
}

/// A one-page document: objects 1–4 are catalog, pages, page and contents; `extra` become
/// objects 5, 6, … and `resources` is the page's resource dictionary.
pub fn page_doc(content: &str, resources: &str, extra: &[String], catalog_extra: &str) -> Document {
    let mut objs = vec![
        format!("<</Type/Catalog/Pages 2 0 R {catalog_extra}>>"),
        "<</Type/Pages/Kids[3 0 R]/Count 1>>".to_string(),
        format!(
            "<</Type/Page/Parent 2 0 R/MediaBox[0 0 200 200]/Contents 4 0 R/Resources {resources}>>"
        ),
        stream("", content),
    ];
    objs.extend(extra.iter().cloned());
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
    out.extend(
        format!(
            "trailer\n<</Size {} /Root 1 0 R>>\nstartxref\n{x}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    Document::open(out).expect("test document opens")
}

pub fn page_and_resources(doc: &Document) -> (Dict, Dict) {
    let page = doc.get(ObjRef::new(3, 0)).unwrap();
    let page = page.as_dict().unwrap().clone();
    let res = doc
        .get_in(&page, b"Resources")
        .as_dict()
        .cloned()
        .unwrap_or_default();
    (page, res)
}

pub fn run(doc: &Document) -> DisplayList {
    let (page, res) = page_and_resources(doc);
    Interpreter::new(doc).page(&page, &res)
}

/// Interprets content with resources and extra objects.
pub fn interpret(content: &str, resources: &str, extra: &[String]) -> (Document, DisplayList) {
    let doc = page_doc(content, resources, extra, "");
    let list = run(&doc);
    (doc, list)
}

pub fn kinds(list: &DisplayList) -> Vec<&'static str> {
    list.cmds.iter().map(|c| c.kind()).collect()
}
