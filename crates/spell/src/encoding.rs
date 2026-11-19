//! Text of dictionary files in the character set their affix file names (`SET`).

use crate::charsets::TABLES;

/// The `SET` of an affix file, read before the file is decoded.
pub(crate) fn declared(aff: &[u8]) -> Option<String> {
    aff.split(|&b| b == b'\n').find_map(|line| {
        let line = line.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(line);
        let rest = line.strip_prefix(b"SET")?;
        let name = String::from_utf8_lossy(rest);
        let name = name.split_whitespace().next()?;
        Some(name.to_ascii_uppercase())
    })
}

/// Decodes `bytes` in the character set `set` (`UTF-8`, `ISO8859-1`, `KOI8-R`, ...). An
/// unknown set is read as UTF-8 when it is valid and as ISO 8859-1 otherwise.
pub(crate) fn decode(bytes: &[u8], set: Option<&str>) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    let set = match set {
        Some(s) => s,
        // Hunspell's default is ISO 8859-1, but a file without SET that is valid UTF-8 is
        // far more likely to be UTF-8.
        None if std::str::from_utf8(bytes).is_ok() => "UTF-8",
        None => "ISO8859-1",
    };
    if matches!(set, "UTF-8" | "UTF8") {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    if let Some((_, table)) = TABLES.iter().find(|(names, _)| names.contains(&set)) {
        return bytes
            .iter()
            .map(|&b| match b {
                0..0x80 => b as char,
                _ => char::from_u32(u32::from(table[usize::from(b - 0x80)])).unwrap_or('\u{FFFD}'),
            })
            .collect();
    }
    match std::str::from_utf8(bytes) {
        Ok(s) if !matches!(set, "ISO8859-1" | "ISO-8859-1") => s.to_owned(),
        _ => bytes.iter().map(|&b| b as char).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn character_sets() {
        assert_eq!(
            declared(b"# x\nSET ISO8859-2\nTRY a").as_deref(),
            Some("ISO8859-2")
        );
        assert_eq!(decode(b"\xb3\xf3d\xbc", Some("ISO8859-2")), "łódź");
        assert_eq!(decode(b"Stra\xdfe", Some("ISO8859-1")), "Straße");
        assert_eq!(decode(b"\xd3\xcc\xcf\xd7\xcf", Some("KOI8-R")), "слово");
        assert_eq!(decode("über".as_bytes(), Some("UTF-8")), "über");
        assert_eq!(decode(b"\xa4", Some("ISO8859-15")), "€");
    }
}
