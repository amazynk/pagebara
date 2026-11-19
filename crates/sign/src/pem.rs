//! PEM (RFC 7468) blocks and Base64.

/// The `(label, DER)` of every PEM block in `text`.
pub fn blocks(text: &[u8]) -> Vec<(String, Vec<u8>)> {
    let text = String::from_utf8_lossy(text);
    let mut out = Vec::new();
    let mut current: Option<(String, String)> = None;
    for line in text.lines() {
        let line = line.trim();
        if let Some(label) = line
            .strip_prefix("-----BEGIN ")
            .and_then(|l| l.strip_suffix("-----"))
        {
            current = Some((label.to_string(), String::new()));
        } else if line.starts_with("-----END ") {
            if let Some((label, body)) = current.take()
                && let Some(der) = decode_base64(&body)
            {
                out.push((label, der));
            }
        } else if let Some((_, body)) = &mut current
            && !line.contains(':')
        {
            body.push_str(line);
        }
    }
    out
}

pub fn encode(label: &str, der: &[u8]) -> String {
    let b = encode_base64(der);
    let mut s = format!("-----BEGIN {label}-----\n");
    for chunk in b.as_bytes().chunks(64) {
        s.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        s.push('\n');
    }
    s.push_str(&format!("-----END {label}-----\n"));
    s
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            c if c.is_ascii_whitespace() => continue,
            _ => return None,
        };
        acc = acc << 6 | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

pub fn encode_base64(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for chunk in b.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |a, (i, &x)| a | u32::from(x) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                s.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for n in 0..10 {
            let v: Vec<u8> = (0..n).map(|i| (i * 37) as u8).collect();
            assert_eq!(decode_base64(&encode_base64(&v)).unwrap(), v);
        }
        let pem = encode("CERTIFICATE", b"hello");
        assert_eq!(
            blocks(pem.as_bytes()),
            [("CERTIFICATE".into(), b"hello".to_vec())]
        );
    }
}
