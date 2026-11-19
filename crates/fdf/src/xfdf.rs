//! XFDF documents: `<fields>` with nested `<field>`/`<value>` elements and `<annots>`.

use crate::xml::{self, Element, escape};
use crate::{Error, FormData, MAX_DEPTH, Result, Tree, Value, annots, tree};

fn fields(out: &mut String, t: &Tree, indent: usize) {
    for (name, kid) in &t.kids {
        let pad = " ".repeat(indent);
        out.push_str(&format!("{pad}<field name=\"{}\">", escape(name)));
        let values: Vec<&str> = match &kid.value {
            Some(Value::Text(s) | Value::State(s)) => vec![s.as_str()],
            Some(Value::List(l)) => l.iter().map(String::as_str).collect(),
            None => Vec::new(),
        };
        for v in values {
            out.push_str(&format!("<value>{}</value>", escape(v)));
        }
        if !kid.kids.is_empty() {
            out.push('\n');
            fields(out, kid, indent + 1);
            out.push_str(&pad);
        }
        out.push_str("</field>\n");
    }
}

/// `data` as an XFDF document.
pub fn write_xfdf(data: &FormData) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <xfdf xmlns=\"http://ns.adobe.com/xfdf/\" xml:space=\"preserve\">\n",
    );
    if let Some(f) = &data.file {
        out.push_str(&format!("<f href=\"{}\"/>\n", escape(f)));
    }
    out.push_str("<fields>\n");
    fields(&mut out, &tree(&data.fields), 0);
    out.push_str("</fields>\n");
    if !data.annotations.is_empty() {
        out.push_str("<annots>\n");
        for a in &data.annotations {
            annots::write(&mut out, a);
        }
        out.push_str("</annots>\n");
    }
    out.push_str("</xfdf>\n");
    out
}

fn collect(el: &Element, prefix: &str, out: &mut Vec<(String, Value)>, depth: usize) {
    if depth > MAX_DEPTH {
        return;
    }
    for f in el.elements().filter(|e| e.name == "field") {
        let Some(n) = f.attr("name") else { continue };
        let name = if prefix.is_empty() {
            n.to_owned()
        } else {
            format!("{prefix}.{n}")
        };
        let vals: Vec<String> = f
            .elements()
            .filter(|e| e.name == "value" || e.name == "value-richtext")
            .map(Element::text)
            .collect();
        match vals.len() {
            0 => {}
            1 => out.push((name.clone(), Value::Text(vals[0].clone()))),
            _ => out.push((name.clone(), Value::List(vals))),
        }
        collect(f, &name, out, depth + 1);
    }
}

/// Reads an XFDF document's field values and annotations.
pub fn parse_xfdf(data: &str) -> Result<FormData> {
    let root = xml::parse(data).ok_or(Error::BadData("XFDF"))?;
    if root.name != "xfdf" {
        return Err(Error::BadData("XFDF"));
    }
    let mut d = FormData {
        file: root
            .elements()
            .find(|e| e.name == "f")
            .and_then(|f| f.attr("href"))
            .map(str::to_owned),
        ..FormData::default()
    };
    for fields in root.elements().filter(|e| e.name == "fields") {
        collect(fields, "", &mut d.fields, 0);
    }
    for list in root.elements().filter(|e| e.name == "annots") {
        d.annotations
            .extend(list.elements().filter_map(annots::read));
    }
    Ok(d)
}
