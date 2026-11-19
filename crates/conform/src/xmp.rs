//! Just enough XMP: reading the properties PDF/A and PDF/UA compare with the document
//! information dictionary, and writing a packet from them.

/// The document information that has XMP equivalents.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Info {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub creator: Option<String>,
    pub producer: Option<String>,
    /// As ISO 8601 (`2024-05-01T10:00:00+02:00`).
    pub created: Option<String>,
    pub modified: Option<String>,
}

/// A packet's identification and properties.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Xmp {
    pub info: Info,
    pub pdfa_part: Option<String>,
    pub pdfa_conformance: Option<String>,
    pub pdfua_part: Option<String>,
    /// The `<?xpacket begin …?>` header has `bytes` or `encoding` attributes.
    pub bad_header: bool,
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The text of the first `<tag>…</tag>`, or of the attribute `tag="…"`; for array
/// properties, of their first `rdf:li`.
fn property(x: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}");
    if let Some(i) = x.find(&open) {
        let rest = &x[i + open.len()..];
        if rest.starts_with([' ', '>', '/', '\n', '\r', '\t']) {
            let gt = rest.find('>')?;
            if rest[..gt].ends_with('/') {
                return Some(String::new());
            }
            let body = &rest[gt + 1..];
            let end = body.find(&format!("</{tag}>"))?;
            let inner = &body[..end];
            let inner = match inner.find("<rdf:li") {
                Some(li) => {
                    let li = &inner[li..];
                    let gt = li.find('>')?;
                    let end = li.find("</rdf:li>")?;
                    &li[gt + 1..end]
                }
                None => inner,
            };
            return Some(unescape(inner));
        }
    }
    let attr = format!("{tag}=\"");
    let i = x.find(&attr)?;
    let rest = &x[i + attr.len()..];
    Some(unescape(&rest[..rest.find('"')?]))
}

/// The packet as text: UTF-8, or UTF-16 or UTF-32 (either byte order) as XMP allows,
/// told by a byte order mark or by where the zero bytes of the first `<` fall.
fn decode(d: &[u8]) -> String {
    let units = |n: usize, be: bool| -> String {
        let word = |v: u32, &b: &u8| v << 8 | u32::from(b);
        let chars = d.chunks_exact(n).map(|c| {
            if be {
                c.iter().fold(0, word)
            } else {
                c.iter().rev().fold(0, word)
            }
        });
        if n == 2 {
            char::decode_utf16(chars.map(|v| v as u16))
                .map(|c| c.unwrap_or('\u{fffd}'))
                .collect()
        } else {
            chars
                .map(|v| char::from_u32(v).unwrap_or('\u{fffd}'))
                .collect()
        }
    };
    match d {
        [0, 0, 0xfe, 0xff, ..] | [0, 0, 0, b'<', ..] => units(4, true),
        [0xff, 0xfe, 0, 0, ..] | [b'<', 0, 0, 0, ..] => units(4, false),
        [0xfe, 0xff, ..] | [0, b'<', ..] => units(2, true),
        [0xff, 0xfe, ..] | [b'<', 0, ..] => units(2, false),
        _ => String::from_utf8_lossy(d).into_owned(),
    }
}

pub(crate) fn parse(data: &[u8]) -> Xmp {
    let x = decode(data);
    let trimmed = |v: Option<String>| v.map(|s| s.trim().to_owned());
    let bad_header = x
        .find("<?xpacket begin")
        .and_then(|i| x[i..].find("?>").map(|e| &x[i..i + e]))
        .is_some_and(|h| h.contains("bytes=") || h.contains("encoding="));
    Xmp {
        info: Info {
            title: property(&x, "dc:title"),
            author: property(&x, "dc:creator"),
            subject: property(&x, "dc:description"),
            keywords: property(&x, "pdf:Keywords"),
            creator: property(&x, "xmp:CreatorTool"),
            producer: property(&x, "pdf:Producer"),
            created: trimmed(property(&x, "xmp:CreateDate")),
            modified: trimmed(property(&x, "xmp:ModifyDate")),
        },
        pdfa_part: trimmed(property(&x, "pdfaid:part")),
        pdfa_conformance: trimmed(property(&x, "pdfaid:conformance")),
        pdfua_part: trimmed(property(&x, "pdfuaid:part")),
        bad_header,
    }
}

/// A PDF date (`D:YYYYMMDDHHmmSSOHH'mm'`) as ISO 8601.
pub(crate) fn pdf_date_to_iso(s: &str) -> Option<String> {
    let s = s.strip_prefix("D:").unwrap_or(s);
    let digits: String = s.chars().take_while(char::is_ascii_digit).collect();
    if digits.len() < 4 {
        return None;
    }
    let part = |a: usize, b: usize, d: &str| digits.get(a..b).unwrap_or(d).to_owned();
    let (y, mo, da) = (part(0, 4, "0000"), part(4, 6, "01"), part(6, 8, "01"));
    let (h, mi, se) = (part(8, 10, "00"), part(10, 12, "00"), part(12, 14, "00"));
    let tz = &s[digits.len()..];
    let zone = match tz.chars().next() {
        Some('Z') => "Z".to_owned(),
        Some(c @ ('+' | '-')) => {
            let t: String = tz[1..].chars().filter(char::is_ascii_digit).collect();
            let th = t.get(0..2).unwrap_or("00");
            let tm = t.get(2..4).unwrap_or("00");
            format!("{c}{th}:{tm}")
        }
        _ => "Z".to_owned(),
    };
    Some(format!("{y}-{mo}-{da}T{h}:{mi}:{se}{zone}"))
}

/// Dates compared as written by different tools: seconds and zones may be left out.
pub(crate) fn same_date(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> String {
        let s = s.replace('Z', "+00:00");
        let (body, zone) = match s.rfind(['+', '-']).filter(|&i| i > 10) {
            Some(i) => (s[..i].to_owned(), s[i..].to_owned()),
            None => (s.clone(), "+00:00".to_owned()),
        };
        let mut body = body;
        if body.len() == 16 {
            body.push_str(":00");
        }
        let body: String = body.chars().take(19).collect();
        format!("{body}{zone}")
    };
    norm(a) == norm(b)
}

/// Writes a packet with `info` and the given identification.
pub(crate) fn write(info: &Info, pdfa: Option<(u8, &str)>, pdfua: bool, now: &str) -> Vec<u8> {
    let mut d = String::new();
    let mut add = |s: String| {
        d.push_str("   ");
        d.push_str(&s);
        d.push('\n');
    };
    add("<dc:format>application/pdf</dc:format>".into());
    if let Some(t) = &info.title {
        add(format!(
            "<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>",
            escape(t)
        ));
    }
    if let Some(a) = &info.author {
        add(format!(
            "<dc:creator><rdf:Seq><rdf:li>{}</rdf:li></rdf:Seq></dc:creator>",
            escape(a)
        ));
    }
    if let Some(s) = &info.subject {
        add(format!(
            "<dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:description>",
            escape(s)
        ));
    }
    if let Some(k) = &info.keywords {
        add(format!("<pdf:Keywords>{}</pdf:Keywords>", escape(k)));
    }
    if let Some(p) = &info.producer {
        add(format!("<pdf:Producer>{}</pdf:Producer>", escape(p)));
    }
    if let Some(c) = &info.creator {
        add(format!("<xmp:CreatorTool>{}</xmp:CreatorTool>", escape(c)));
    }
    if let Some(c) = &info.created {
        add(format!("<xmp:CreateDate>{c}</xmp:CreateDate>"));
    }
    if let Some(m) = &info.modified {
        add(format!("<xmp:ModifyDate>{m}</xmp:ModifyDate>"));
    }
    add(format!("<xmp:MetadataDate>{now}</xmp:MetadataDate>"));
    if let Some((part, conf)) = pdfa {
        add(format!("<pdfaid:part>{part}</pdfaid:part>"));
        add(format!("<pdfaid:conformance>{conf}</pdfaid:conformance>"));
    }
    if pdfua {
        add("<pdfuaid:part>1</pdfuaid:part>".into());
    }
    let extension = if pdfua && pdfa.is_some() {
        EXTENSION
    } else {
        ""
    };
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
 <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
  <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\" xmlns:pdfaid=\"http://www.aiim.org/pdfa/ns/id/\" xmlns:pdfuaid=\"http://www.aiim.org/pdfua/ns/id/\">\n\
{d}  </rdf:Description>\n{extension}\
 </rdf:RDF>\n\
</x:xmpmeta>\n\
{pad}\n<?xpacket end=\"w\"?>",
        pad = " ".repeat(1024)
    )
    .into_bytes()
}

/// The PDF/A extension schema describing `pdfuaid`, which PDF/A does not predefine.
const EXTENSION: &str = "  <rdf:Description rdf:about=\"\" xmlns:pdfaExtension=\"http://www.aiim.org/pdfa/ns/extension/\" xmlns:pdfaSchema=\"http://www.aiim.org/pdfa/ns/schema#\" xmlns:pdfaProperty=\"http://www.aiim.org/pdfa/ns/property#\">
   <pdfaExtension:schemas><rdf:Bag><rdf:li rdf:parseType=\"Resource\">
    <pdfaSchema:schema>PDF/UA Universal Accessibility Schema</pdfaSchema:schema>
    <pdfaSchema:namespaceURI>http://www.aiim.org/pdfua/ns/id/</pdfaSchema:namespaceURI>
    <pdfaSchema:prefix>pdfuaid</pdfaSchema:prefix>
    <pdfaSchema:property><rdf:Seq><rdf:li rdf:parseType=\"Resource\">
     <pdfaProperty:name>part</pdfaProperty:name>
     <pdfaProperty:valueType>Integer</pdfaProperty:valueType>
     <pdfaProperty:category>internal</pdfaProperty:category>
     <pdfaProperty:description>Indicates, which part of ISO 14289 standard is followed</pdfaProperty:description>
    </rdf:li></rdf:Seq></pdfaSchema:property>
   </rdf:li></rdf:Bag></pdfaExtension:schemas>
  </rdf:Description>
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packets_in_utf16_and_utf32_keep_their_spaces() {
        let x = "<x:xmpmeta><pdf:Producer>Builder 1.0 </pdf:Producer><pdfaid:part>\n 1 \n</pdfaid:part></x:xmpmeta>";
        let le16: Vec<u8> = x.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let be16: Vec<u8> = [0xfe, 0xff]
            .into_iter()
            .chain(x.encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        let be32: Vec<u8> = x.chars().flat_map(|c| u32::from(c).to_be_bytes()).collect();
        for data in [x.as_bytes().to_vec(), le16, be16, be32] {
            let p = parse(&data);
            assert_eq!(p.info.producer.as_deref(), Some("Builder 1.0 "));
            assert_eq!(p.pdfa_part.as_deref(), Some("1"));
        }
    }

    #[test]
    fn written_packets_read_back() {
        let info = Info {
            title: Some("A & B".into()),
            author: Some("Ann".into()),
            created: pdf_date_to_iso("D:20240501100000+02'00'"),
            ..Info::default()
        };
        let x = write(&info, Some((2, "U")), true, "2026-11-15T00:00:00Z");
        let p = parse(&x);
        assert_eq!(p.info.title.as_deref(), Some("A & B"));
        assert_eq!(p.info.author.as_deref(), Some("Ann"));
        assert_eq!(p.pdfa_part.as_deref(), Some("2"));
        assert_eq!(p.pdfa_conformance.as_deref(), Some("U"));
        assert_eq!(p.pdfua_part.as_deref(), Some("1"));
        assert!(!p.bad_header);
        assert_eq!(p.info.created.as_deref(), Some("2024-05-01T10:00:00+02:00"));
        assert!(same_date(
            "2024-05-01T10:00+02:00",
            "2024-05-01T10:00:00+02:00"
        ));
    }
}
