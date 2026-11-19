//! FDF files: a `/FDF` dictionary with the field tree in `/Fields`.

use std::collections::HashMap;

use pagebara_syntax::parser::Parser;
use pagebara_syntax::{Dict, ObjRef, Object};

use crate::text::{encode, name_text, text_of};
use crate::{Error, FormData, MAX_DEPTH, Result, Tree, Value, tree};

fn fields(t: &Tree) -> Vec<Object> {
    t.kids
        .iter()
        .map(|(name, kid)| {
            let mut d = Dict::new();
            d.set(b"T", encode(name));
            if !kid.kids.is_empty() {
                d.set(b"Kids", fields(kid));
            }
            match &kid.value {
                Some(Value::Text(s)) => d.set(b"V", encode(s)),
                Some(Value::State(s)) => d.set(b"V", Object::name(s.as_bytes())),
                Some(Value::List(l)) => {
                    d.set(b"V", Object::Array(l.iter().map(|s| encode(s)).collect()))
                }
                None => {}
            }
            Object::Dict(d)
        })
        .collect()
}

/// `data` as an FDF file (annotations are not written).
pub fn write_fdf(data: &FormData) -> Vec<u8> {
    let mut fdf = Dict::new();
    if let Some(f) = &data.file {
        fdf.set(b"F", encode(f));
    }
    fdf.set(b"Fields", fields(&tree(&data.fields)));
    let mut root = Dict::new();
    root.set(b"FDF", fdf);
    let mut out = b"%FDF-1.2\n%\xE2\xE3\xCF\xD3\n1 0 obj\n".to_vec();
    pagebara_syntax::write_object(&mut out, &Object::Dict(root));
    out.extend_from_slice(b"\nendobj\ntrailer\n<</Root 1 0 R>>\n%%EOF\n");
    out
}

/// Indirect objects of an FDF file and its root.
fn objects(data: &[u8]) -> (HashMap<u32, Object>, Option<Object>) {
    let mut objs = HashMap::new();
    let mut i = 0;
    while let Some(p) = data[i..]
        .windows(3)
        .position(|w| w == b"obj")
        .map(|p| p + i)
    {
        i = p + 3;
        // Back over `num gen `.
        let mut s = p;
        let mut groups = 0;
        while groups < 2 {
            while s > 0 && data[s - 1].is_ascii_whitespace() {
                s -= 1;
            }
            let end = s;
            while s > 0 && data[s - 1].is_ascii_digit() {
                s -= 1;
            }
            if s == end {
                break;
            }
            groups += 1;
        }
        if groups != 2 {
            continue;
        }
        if let Ok((id, o)) = Parser::new(data, s).indirect(&mut |_| None) {
            objs.insert(id.num, o);
        }
    }
    let root = data
        .windows(7)
        .rposition(|w| w == b"trailer")
        .and_then(|t| Parser::new(data, t + 7).object().ok())
        .and_then(|t| t.as_dict()?.get(b"Root").cloned());
    (objs, root)
}

/// Reads an FDF file's field values.
pub fn parse_fdf(data: &[u8]) -> Result<FormData> {
    if !data.starts_with(b"%FDF") && !data.windows(4).take(1024).any(|w| w == b"%FDF") {
        return Err(Error::BadData("FDF"));
    }
    let (objs, root) = objects(data);
    let resolve = |o: &Object| -> Object {
        let mut o = o.clone();
        for _ in 0..32 {
            match o {
                Object::Ref(ObjRef { num, .. }) => o = objs.get(&num).cloned().unwrap_or_default(),
                _ => break,
            }
        }
        o
    };
    let root = root
        .map(|r| resolve(&r))
        .filter(|r| r.as_dict().is_some_and(|d| d.contains(b"FDF")))
        .or_else(|| {
            objs.values()
                .find(|o| o.as_dict().is_some_and(|d| d.contains(b"FDF")))
                .cloned()
        })
        .ok_or(Error::BadData("FDF"))?;
    let fdf = resolve(
        root.as_dict()
            .and_then(|d| d.get(b"FDF"))
            .unwrap_or(&Object::Null),
    );
    let file = fdf
        .as_dict()
        .and_then(|d| d.get(b"F"))
        .map(resolve)
        .and_then(|f| match &f {
            Object::Dict(fs) => fs.get(b"F").or(fs.get(b"UF")).and_then(text_of),
            o => text_of(o),
        });
    let list = fdf
        .as_dict()
        .and_then(|d| d.get(b"Fields"))
        .map(resolve)
        .unwrap_or_default();
    let mut values: Vec<(String, Value)> = Vec::new();
    let mut stack: Vec<(String, Object, usize)> = list
        .as_array()
        .unwrap_or_default()
        .iter()
        .rev()
        .map(|o| (String::new(), resolve(o), 0))
        .collect();
    while let Some((prefix, node, depth)) = stack.pop() {
        let Some(d) = node.as_dict() else { continue };
        if depth > MAX_DEPTH {
            continue;
        }
        let t = d.get(b"T").map(resolve).and_then(|t| text_of(&t));
        let name = match (t, prefix.is_empty()) {
            (Some(t), true) => t,
            (Some(t), false) => format!("{prefix}.{t}"),
            (None, _) => prefix.clone(),
        };
        if let Some(v) = d.get(b"V").map(resolve) {
            let v = match &v {
                Object::Name(n) => Some(Value::State(name_text(n.as_bytes()))),
                Object::Array(a) => Some(Value::List(
                    a.iter().filter_map(|o| text_of(&resolve(o))).collect(),
                )),
                o => text_of(o).map(Value::Text),
            };
            if let Some(v) = v {
                values.push((name.clone(), v));
            }
        }
        if let Some(kids) = d.get(b"Kids").map(resolve)
            && let Some(kids) = kids.as_array()
        {
            for k in kids.iter().rev() {
                stack.push((name.clone(), resolve(k), depth + 1));
            }
        }
    }
    Ok(FormData {
        file,
        fields: values,
        annotations: Vec::new(),
    })
}
