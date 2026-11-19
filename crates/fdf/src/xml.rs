//! Just enough XML for XFDF: elements, attributes, text, CDATA and entities. Namespace
//! prefixes are dropped.

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Node {
    Element(Element),
    Text(String),
}

#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Element {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    pub fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    /// All text below this element.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for n in &self.children {
            match n {
                Node::Text(t) => s.push_str(t),
                Node::Element(e) => s.push_str(&e.text()),
            }
        }
        s
    }
}

const MAX_DEPTH: usize = 256;

fn local(name: &str) -> String {
    name.rsplit(':').next().unwrap_or(name).to_owned()
}

pub(crate) fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let Some(end) = rest.find(';').filter(|&e| e <= 12) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let ent = &rest[1..end];
        let c = match ent {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => ent
                .strip_prefix("#x")
                .or_else(|| ent.strip_prefix("#X"))
                .and_then(|h| u32::from_str_radix(h, 16).ok())
                .or_else(|| ent.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match c {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

pub(crate) fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\r' => out.push_str("&#13;"),
            c => out.push(c),
        }
    }
    out
}

/// Parses a document into its root element.
pub(crate) fn parse(src: &str) -> Option<Element> {
    let mut stack: Vec<Element> = vec![Element::default()];
    let mut rest = src;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            push_text(&mut stack, rest);
            break;
        };
        push_text(&mut stack, &rest[..lt]);
        rest = &rest[lt..];
        if let Some(r) = rest.strip_prefix("<!--") {
            rest = &r[r.find("-->")? + 3..];
        } else if let Some(r) = rest.strip_prefix("<![CDATA[") {
            let end = r.find("]]>")?;
            let top = stack.last_mut()?;
            top.children.push(Node::Text(r[..end].to_owned()));
            rest = &r[end + 3..];
        } else if rest.starts_with("<?") || rest.starts_with("<!") {
            rest = &rest[rest.find('>')? + 1..];
        } else if let Some(r) = rest.strip_prefix("</") {
            let end = r.find('>')?;
            let done = stack.pop()?;
            if stack.is_empty() || local(r[..end].trim()) != done.name {
                return None;
            }
            stack.last_mut()?.children.push(Node::Element(done));
            rest = &r[end + 1..];
        } else {
            let (el, closed, after) = tag(&rest[1..])?;
            rest = after;
            if closed {
                stack.last_mut()?.children.push(Node::Element(el));
            } else {
                if stack.len() > MAX_DEPTH {
                    return None;
                }
                stack.push(el);
            }
        }
    }
    if stack.len() != 1 {
        return None;
    }
    let doc = stack.pop()?;
    doc.elements().next().cloned()
}

fn push_text(stack: &mut [Element], t: &str) {
    if t.is_empty() {
        return;
    }
    if let Some(top) = stack.last_mut() {
        top.children.push(Node::Text(unescape(t)));
    }
}

/// An opening tag after `<`: the element, whether it closes itself, and what follows.
fn tag(s: &str) -> Option<(Element, bool, &str)> {
    let name_end = s.find(|c: char| c.is_whitespace() || c == '>' || c == '/')?;
    let mut el = Element {
        name: local(&s[..name_end]),
        ..Element::default()
    };
    let mut rest = &s[name_end..];
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("/>") {
            return Some((el, true, r));
        }
        if let Some(r) = rest.strip_prefix('>') {
            return Some((el, false, r));
        }
        let eq = rest.find('=')?;
        let key = local(rest[..eq].trim());
        let r = rest[eq + 1..].trim_start();
        let q = r.chars().next().filter(|c| *c == '"' || *c == '\'')?;
        let end = r[1..].find(q)?;
        el.attrs.push((key, unescape(&r[1..1 + end])));
        rest = &r[end + 2..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        let x = parse(
            "<?xml version=\"1.0\"?><!-- c --><a xmlns='x' b:c=\"1 &amp; 2\"><v>t&lt;&#233;</v>\
             <e/><![CDATA[<raw>]]></a>",
        )
        .unwrap();
        assert_eq!(x.name, "a");
        assert_eq!(x.attr("c"), Some("1 & 2"));
        assert_eq!(x.elements().count(), 2);
        assert_eq!(x.text(), "t<é<raw>");
        assert!(parse("<a><b></a>").is_none());
        assert_eq!(escape("a<\"&"), "a&lt;&quot;&amp;");
    }
}
