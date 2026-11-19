//! Windows shortcut files (MS-SHLLINK), written as data: a link with a local target path,
//! arguments, a description and the target's icon. No shell COM calls needed.

fn u16le(v: u16) -> [u8; 2] {
    v.to_le_bytes()
}

fn u32le(v: u32) -> [u8; 4] {
    v.to_le_bytes()
}

/// A counted UTF-16 string of the StringData section.
fn string_data(s: &str) -> Vec<u8> {
    let units: Vec<u16> = s.encode_utf16().collect();
    let mut out = u16le(units.len() as u16).to_vec();
    for u in units {
        out.extend(u16le(u));
    }
    out
}

/// A `.lnk` to `target` (a full Windows path) with `args`.
pub fn shortcut(target: &str, args: &str, description: &str) -> Vec<u8> {
    const HAS_LINK_INFO: u32 = 0x2;
    const HAS_NAME: u32 = 0x4;
    const HAS_WORKING_DIR: u32 = 0x10;
    const HAS_ARGUMENTS: u32 = 0x20;
    const HAS_ICON_LOCATION: u32 = 0x40;
    const IS_UNICODE: u32 = 0x80;
    let mut flags = HAS_LINK_INFO | HAS_NAME | HAS_WORKING_DIR | HAS_ICON_LOCATION | IS_UNICODE;
    if !args.is_empty() {
        flags |= HAS_ARGUMENTS;
    }
    let mut out = Vec::new();
    // ShellLinkHeader.
    out.extend(u32le(0x4c));
    // CLSID 00021401-0000-0000-C000-000000000046.
    out.extend([
        0x01, 0x14, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0xc0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x46,
    ]);
    out.extend(u32le(flags));
    out.extend(u32le(0x20)); // FILE_ATTRIBUTE_ARCHIVE
    out.extend([0; 24]); // creation, access, write times
    out.extend(u32le(0)); // file size
    out.extend(u32le(0)); // icon index
    out.extend(u32le(1)); // SW_SHOWNORMAL
    out.extend(u16le(0)); // hot key
    out.extend([0; 10]); // reserved
    debug_assert_eq!(out.len(), 0x4c);

    // LinkInfo with a volume ID and the local base path, ANSI and Unicode.
    let ansi: Vec<u8> = target
        .chars()
        .map(|c| if c.is_ascii() { c as u8 } else { b'?' })
        .chain([0])
        .collect();
    let mut unicode: Vec<u8> = target.encode_utf16().flat_map(u16le).collect();
    unicode.extend([0, 0]);
    let header = 0x24u32;
    let volume: Vec<u8> = [u32le(0x11), u32le(3), u32le(0), u32le(0x10)]
        .concat()
        .into_iter()
        .chain([0])
        .collect();
    let volume_at = header;
    let base_at = volume_at + volume.len() as u32;
    let suffix_at = base_at + ansi.len() as u32;
    let base_unicode_at = suffix_at + 1;
    let suffix_unicode_at = base_unicode_at + unicode.len() as u32;
    let size = suffix_unicode_at + 2;
    out.extend(u32le(size));
    out.extend(u32le(header));
    out.extend(u32le(1)); // VolumeIDAndLocalBasePath
    out.extend(u32le(volume_at));
    out.extend(u32le(base_at));
    out.extend(u32le(0)); // no network path
    out.extend(u32le(suffix_at));
    out.extend(u32le(base_unicode_at));
    out.extend(u32le(suffix_unicode_at));
    out.extend(&volume);
    out.extend(&ansi);
    out.push(0); // empty common path suffix
    out.extend(&unicode);
    out.extend([0, 0]);

    // StringData: name, working dir, arguments, icon location.
    let dir = target.rsplit_once('\\').map_or("", |(d, _)| d);
    out.extend(string_data(description));
    out.extend(string_data(dir));
    if !args.is_empty() {
        out.extend(string_data(args));
    }
    out.extend(string_data(target));
    // Terminal block.
    out.extend(u32le(0));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout() {
        let l = shortcut(
            r"C:\Program Files\pagebara\pagebara.exe",
            "--action merge",
            "Merge",
        );
        assert_eq!(&l[0..4], &[0x4c, 0, 0, 0]);
        assert_eq!(l[4], 0x01);
        let info_size = u32::from_le_bytes(l[0x4c..0x50].try_into().unwrap()) as usize;
        let info = &l[0x4c..0x4c + info_size];
        let base = u32::from_le_bytes(info[16..20].try_into().unwrap()) as usize;
        assert!(info[base..].starts_with(b"C:\\Program Files\\pagebara\\pagebara.exe\0"));
        // The description follows LinkInfo as a counted UTF-16 string.
        let rest = &l[0x4c + info_size..];
        assert_eq!(u16::from_le_bytes([rest[0], rest[1]]), 5);
        assert_eq!(&l[l.len() - 4..], &[0, 0, 0, 0]);
    }
}
