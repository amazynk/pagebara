//! Signature fields (§12.8): whether they are signed, what the signature dictionary says,
//! and which bytes of the file the signature covers. The signature itself is not verified
//! (no cryptography here): a "signed" field only has a signature value.

use pagebara_syntax::{Document, Object};

use crate::form::Form;
use crate::model::FieldKind;
use crate::text::{DateTime, decode};

/// How a signature's `/ByteRange` relates to the file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ByteRangeCheck {
    /// The signed `(offset, length)` ranges.
    pub ranges: Vec<(usize, usize)>,
    /// Where the signed bytes end.
    pub signed_end: usize,
    pub file_len: usize,
    /// Two ranges from the start of the file around exactly the `/Contents` string.
    pub well_formed: bool,
    /// The signed bytes end at a revision's `%%EOF`.
    pub covers_revision: bool,
    /// The signed revision is the file's last: nothing was appended since signing.
    pub covers_whole_file: bool,
    /// Revisions (incremental updates) appended after the signed one.
    pub later_revisions: usize,
}

/// A signature field and its signature dictionary, if signed.
#[derive(Clone, Debug, PartialEq)]
pub struct SignatureInfo {
    pub field: String,
    pub page: Option<usize>,
    pub signed: bool,
    pub filter: Option<String>,
    pub sub_filter: Option<String>,
    /// `/Name`: the signer as the signing software wrote it.
    pub signer: Option<String>,
    /// Common names in the embedded certificates (signer and issuers), unverified.
    pub certificate_names: Vec<String>,
    /// `/M`: the claimed signing time.
    pub signing_time: Option<DateTime>,
    pub reason: Option<String>,
    pub location: Option<String>,
    pub contact_info: Option<String>,
    /// A document timestamp (`/Type /DocTimeStamp`) rather than a signature.
    pub timestamp: bool,
    /// The certification level of a DocMDP signature: 1 no changes, 2 form filling and
    /// signing, 3 also annotations.
    pub doc_mdp: Option<u8>,
    pub byte_range: Option<ByteRangeCheck>,
}

impl Form {
    /// Every signature field with what its signature claims. Byte ranges are checked
    /// against the file as opened ([`Document::source`]).
    pub fn signatures(&self, doc: &Document) -> Vec<SignatureInfo> {
        self.fields
            .iter()
            .filter(|f| f.kind == FieldKind::Signature)
            .map(|f| {
                let v = f.chain.iter().find_map(|&r| {
                    let d = doc.get(r)?;
                    let v = doc.get_in(d.as_dict()?, b"V");
                    v.as_dict().cloned()
                });
                let mut info = SignatureInfo {
                    field: f.name.clone(),
                    page: f.widgets.iter().find_map(|w| w.page),
                    signed: v.is_some(),
                    filter: None,
                    sub_filter: None,
                    signer: None,
                    certificate_names: Vec::new(),
                    signing_time: None,
                    reason: None,
                    location: None,
                    contact_info: None,
                    timestamp: false,
                    doc_mdp: None,
                    byte_range: None,
                };
                let Some(v) = v else { return info };
                let name = |k: &[u8]| v.name(k).map(crate::text::name_text);
                let text = |k: &[u8]| doc.get_in(&v, k).as_string().map(decode);
                info.filter = name(b"Filter");
                info.sub_filter = name(b"SubFilter");
                info.signer = text(b"Name");
                info.signing_time = text(b"M").and_then(|m| DateTime::parse_pdf(&m));
                info.reason = text(b"Reason");
                info.location = text(b"Location");
                info.contact_info = text(b"ContactInfo");
                info.timestamp = v.name(b"Type") == Some(b"DocTimeStamp");
                for r in doc.get_in(&v, b"Reference").as_array().unwrap_or_default() {
                    let r = doc.resolve(r);
                    if let Some(r) = r.as_dict()
                        && r.name(b"TransformMethod") == Some(b"DocMDP")
                    {
                        let p = doc
                            .get_in(r, b"TransformParams")
                            .as_dict()
                            .and_then(|p| p.int(b"P"))
                            .unwrap_or(2);
                        info.doc_mdp = Some(p.clamp(1, 3) as u8);
                    }
                }
                let contents = doc.get_in(&v, b"Contents");
                let contents = contents.as_string().unwrap_or_default();
                info.certificate_names = common_names(contents);
                let ranges: Vec<i64> = doc
                    .get_in(&v, b"ByteRange")
                    .as_array()
                    .unwrap_or_default()
                    .iter()
                    .filter_map(Object::as_int)
                    .collect();
                info.byte_range = check_ranges(doc.source(), &ranges, contents);
                info
            })
            .collect()
    }
}

fn check_ranges(src: &[u8], ranges: &[i64], contents: &[u8]) -> Option<ByteRangeCheck> {
    if ranges.is_empty() || !ranges.len().is_multiple_of(2) {
        return None;
    }
    let pairs: Vec<(usize, usize)> = ranges
        .chunks(2)
        .map(|p| Some((usize::try_from(p[0]).ok()?, usize::try_from(p[1]).ok()?)))
        .collect::<Option<_>>()?;
    let signed_end = pairs.iter().map(|(o, l)| o + l).max()?;
    let well_formed = match pairs.as_slice() {
        [(0, a), (c, _)] if *c > *a && signed_end <= src.len() => {
            let gap = &src[*a..*c];
            gap.first() == Some(&b'<')
                && gap.last() == Some(&b'>')
                && hex(&gap[1..gap.len() - 1]).is_some_and(|h| {
                    // Padding zeros may follow what the reader kept.
                    h.len() >= contents.len()
                        && h[..contents.len()] == *contents
                        && h[contents.len()..].iter().all(|&b| b == 0)
                })
        }
        _ => false,
    };
    let markers = memchr_all(src, b"%%EOF");
    // A revision ends after its marker, with or without the end-of-line.
    let covers_revision = markers.iter().any(|&m| {
        let after = src.get(m + 5..signed_end.max(m + 5)).unwrap_or(b"x");
        signed_end >= m + 5 && after.len() <= 2 && after.iter().all(|&b| b == b'\r' || b == b'\n')
    });
    let covers_whole_file = signed_end == src.len()
        || (signed_end < src.len() && src[signed_end..].iter().all(u8::is_ascii_whitespace));
    Some(ByteRangeCheck {
        ranges: pairs,
        signed_end,
        file_len: src.len(),
        well_formed,
        covers_revision,
        covers_whole_file,
        later_revisions: markers.iter().filter(|&&m| m >= signed_end).count(),
    })
}

fn memchr_all(hay: &[u8], needle: &[u8]) -> Vec<usize> {
    hay.windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle)
        .map(|(i, _)| i)
        .collect()
}

fn hex(b: &[u8]) -> Option<Vec<u8>> {
    let digits: Vec<u8> = b
        .iter()
        .filter(|c| !c.is_ascii_whitespace())
        .map(|&c| (c as char).to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    Some(
        digits
            .chunks(2)
            .map(|p| p[0] << 4 | p.get(1).copied().unwrap_or(0))
            .collect(),
    )
}

/// Common names (OID 2.5.4.3) in a DER blob.
fn common_names(der: &[u8]) -> Vec<String> {
    const OID: [u8; 5] = [0x06, 0x03, 0x55, 0x04, 0x03];
    let mut out: Vec<String> = Vec::new();
    for at in memchr_all(der, &OID) {
        let p = at + OID.len();
        let (Some(&tag), Some(&len)) = (der.get(p), der.get(p + 1)) else {
            continue;
        };
        if len >= 0x80 {
            continue;
        }
        let Some(v) = der.get(p + 2..p + 2 + usize::from(len)) else {
            continue;
        };
        let s = match tag {
            0x0C | 0x13 | 0x14 | 0x16 => String::from_utf8_lossy(v).into_owned(),
            0x1E => {
                let u: Vec<u16> = v
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes(*c))
                    .collect();
                String::from_utf16_lossy(&u)
            }
            _ => continue,
        };
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges() {
        let mut src = b"%PDF-1.7\nAAAA /Contents <0A0B0000> BBB\n%%EOF\n".to_vec();
        let a = src.iter().position(|&b| b == b'<').unwrap();
        let c = src.iter().position(|&b| b == b'>').unwrap() + 1;
        let r = [0, a as i64, c as i64, (src.len() - c) as i64];
        let ck = check_ranges(&src, &r, &[0x0A, 0x0B]).unwrap();
        assert!(ck.well_formed && ck.covers_revision && ck.covers_whole_file);
        assert_eq!(ck.later_revisions, 0);
        assert!(!check_ranges(&src, &r, &[0x0A, 0x0C]).unwrap().well_formed);
        src.extend_from_slice(b"1 0 obj null endobj\n%%EOF\n");
        let ck = check_ranges(&src, &r, &[0x0A, 0x0B]).unwrap();
        assert!(ck.covers_revision && !ck.covers_whole_file);
        assert_eq!(ck.later_revisions, 1);
    }

    #[test]
    fn names() {
        let der = [
            0x30, 0x0A, 0x06, 0x03, 0x55, 0x04, 0x03, 0x0C, 0x03, b'A', b'n', b'n',
        ];
        assert_eq!(common_names(&der), ["Ann"]);
    }
}
