//! Writing `/ToUnicode` CMaps (§9.10.3): new ones, and entries added to existing ones.

use std::fmt::Write as _;

/// A code (`len` bytes) and the text it stands for.
pub(crate) type Entry = (u32, u8, String);

fn write_entries(s: &mut String, entries: &[Entry]) {
    for chunk in entries.chunks(100) {
        let _ = writeln!(s, "{} beginbfchar", chunk.len());
        for (code, len, text) in chunk {
            let w = usize::from(*len) * 2;
            let _ = write!(s, "<{code:0w$X}> <");
            for u in text.encode_utf16() {
                let _ = write!(s, "{u:04X}");
            }
            s.push_str(">\n");
        }
        s.push_str("endbfchar\n");
    }
}

/// A complete CMap with codes of `code_len` bytes.
pub(crate) fn new_cmap(code_len: u8, entries: &[Entry]) -> Vec<u8> {
    let (lo, hi) = if code_len == 1 {
        ("00", "FF")
    } else {
        ("0000", "FFFF")
    };
    let mut s = format!(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n\
         1 begincodespacerange\n<{lo}> <{hi}>\nendcodespacerange\n"
    );
    write_entries(&mut s, entries);
    s.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    s.into_bytes()
}

/// `existing` with `entries` added before its last `endcmap` (or a new CMap when it has
/// none).
pub(crate) fn append(existing: &[u8], code_len: u8, entries: &[Entry]) -> Vec<u8> {
    let Some(at) = existing
        .windows(7)
        .rposition(|w| w == b"endcmap")
        .filter(|&p| p == 0 || !existing[p - 1].is_ascii_alphanumeric())
    else {
        return new_cmap(code_len, entries);
    };
    let mut s = String::new();
    if at > 0 && !existing[at - 1].is_ascii_whitespace() {
        s.push('\n');
    }
    write_entries(&mut s, entries);
    let mut out = existing[..at].to_vec();
    out.extend_from_slice(s.as_bytes());
    out.extend_from_slice(&existing[at..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn appends_before_endcmap() {
        let base = new_cmap(1, &[(0x41, 1, "A".into())]);
        let out = append(&base, 1, &[(0x80, 1, "Ж".into()), (0x81, 1, "😀".into())]);
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("<41> <0041>"));
        assert!(s.contains("<80> <0416>\n<81> <D83DDE00>\nendbfchar\nendcmap"));
        assert_eq!(s.matches("endcmap").count(), 1);
    }

    #[test]
    fn two_byte_codes() {
        let s = String::from_utf8(new_cmap(2, &[(0x1F, 2, "fi".into())])).unwrap();
        assert!(s.contains("<0000> <FFFF>"));
        assert!(s.contains("<001F> <00660069>"));
    }
}
