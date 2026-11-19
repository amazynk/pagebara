//! A minimal PDF writer for synthetic test files.

#![allow(dead_code)]

use std::fmt::Write as _;

#[derive(Clone)]
pub struct Pdf {
    objects: Vec<Vec<u8>>,
    pages: Vec<usize>,
    /// Extra catalog entries.
    pub catalog: String,
}

impl Default for Pdf {
    fn default() -> Self {
        Self::new()
    }
}

impl Pdf {
    pub fn new() -> Self {
        // 1: catalog, 2: page tree, filled in at the end.
        Pdf {
            objects: vec![Vec::new(), Vec::new()],
            pages: Vec::new(),
            catalog: String::new(),
        }
    }

    /// Adds an object (its body, without `n 0 obj`); returns its number.
    pub fn add(&mut self, body: impl AsRef<[u8]>) -> usize {
        self.objects.push(body.as_ref().to_vec());
        self.objects.len()
    }

    /// Adds a stream with extra dictionary entries.
    pub fn stream(&mut self, dict: &str, data: &[u8]) -> usize {
        let mut b = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
        b.extend_from_slice(data);
        b.extend_from_slice(b"\nendstream");
        self.add(b)
    }

    /// Adds a page with a content stream and resources (a dictionary body).
    pub fn page(&mut self, size: (f32, f32), resources: &str, content: &str) -> usize {
        self.page_with(size, resources, content, "")
    }

    pub fn page_with(
        &mut self,
        size: (f32, f32),
        resources: &str,
        content: &str,
        extra: &str,
    ) -> usize {
        let c = self.stream("", content.as_bytes());
        let p = self.add(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Resources << {resources} >> /Contents {c} 0 R {extra} >>",
            size.0, size.1
        ));
        self.pages.push(p);
        p
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.objects[0] =
            format!("<< /Type /Catalog /Pages 2 0 R {} >>", self.catalog).into_bytes();
        let kids: String = self.pages.iter().fold(String::new(), |mut s, p| {
            let _ = write!(s, "{p} 0 R ");
            s
        });
        self.objects[1] = format!(
            "<< /Type /Pages /Kids [{kids}] /Count {} >>",
            self.pages.len()
        )
        .into_bytes();
        let mut out = b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n".to_vec();
        let mut offsets = Vec::new();
        for (i, o) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            out.extend_from_slice(o);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        let mut x = format!("xref\n0 {}\n0000000000 65535 f \n", self.objects.len() + 1);
        for o in offsets {
            let _ = writeln!(x, "{o:010} 00000 n ");
        }
        let _ = write!(
            x,
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            self.objects.len() + 1
        );
        out.extend_from_slice(x.as_bytes());
        out
    }
}

/// Hex string of bytes for inline data.
pub fn hex(b: &[u8]) -> String {
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, v| {
            let _ = write!(s, "{v:02X}");
            s
        })
}

/// A matrix/TRC ICC profile (sRGB primaries, gamma 2.2) for ICCBased tests.
pub fn icc_rgb() -> Vec<u8> {
    fn s15(v: f64) -> [u8; 4] {
        ((v * 65536.0).round() as i32).to_be_bytes()
    }
    let xyz = |x: f64, y: f64, z: f64| {
        let mut t = b"XYZ \0\0\0\0".to_vec();
        t.extend(s15(x));
        t.extend(s15(y));
        t.extend(s15(z));
        t
    };
    let curv = {
        let mut t = b"curv\0\0\0\0".to_vec();
        t.extend(1u32.to_be_bytes());
        t.extend(((2.2 * 256.0) as u16).to_be_bytes());
        t.extend([0, 0]);
        t
    };
    let tags: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"rXYZ", xyz(0.4361, 0.2225, 0.0139)),
        (b"gXYZ", xyz(0.3851, 0.7169, 0.0971)),
        (b"bXYZ", xyz(0.1431, 0.0606, 0.7141)),
        (b"wtpt", xyz(0.9642, 1.0, 0.8249)),
        (b"rTRC", curv.clone()),
        (b"gTRC", curv.clone()),
        (b"bTRC", curv),
    ];
    let mut header = vec![0u8; 128];
    header[12..16].copy_from_slice(b"mntr");
    header[16..20].copy_from_slice(b"RGB ");
    header[20..24].copy_from_slice(b"XYZ ");
    header[36..40].copy_from_slice(b"acsp");
    header[8] = 2;
    header[9] = 0x10;
    let mut table = (tags.len() as u32).to_be_bytes().to_vec();
    let mut data = Vec::new();
    let base = 128 + 4 + 12 * tags.len();
    for (sig, body) in &tags {
        table.extend_from_slice(*sig);
        table.extend(((base + data.len()) as u32).to_be_bytes());
        table.extend((body.len() as u32).to_be_bytes());
        data.extend_from_slice(body);
        while data.len() % 4 != 0 {
            data.push(0);
        }
    }
    let mut out = header;
    out.extend(table);
    out.extend(data);
    let n = out.len() as u32;
    out[0..4].copy_from_slice(&n.to_be_bytes());
    out
}
