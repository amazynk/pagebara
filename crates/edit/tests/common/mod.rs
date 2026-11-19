//! Synthetic test files and checks shared by the editing tests.

#![allow(dead_code)]

use std::fmt::Write as _;

/// A minimal PDF writer: objects 1 and 2 are the catalog and page tree.
pub struct Pdf {
    objects: Vec<Vec<u8>>,
    pages: Vec<usize>,
    pub catalog: String,
    pub trailer: String,
}

impl Pdf {
    pub fn new() -> Self {
        Pdf {
            objects: vec![Vec::new(), Vec::new()],
            pages: Vec::new(),
            catalog: String::new(),
            trailer: String::new(),
        }
    }

    pub fn add(&mut self, body: impl AsRef<[u8]>) -> usize {
        self.objects.push(body.as_ref().to_vec());
        self.objects.len()
    }

    /// Reserves an object number to fill with [`Pdf::set`].
    pub fn reserve(&mut self) -> usize {
        self.add(b"null")
    }

    pub fn set(&mut self, n: usize, body: impl AsRef<[u8]>) {
        self.objects[n - 1] = body.as_ref().to_vec();
    }

    pub fn stream(&mut self, dict: &str, data: &[u8]) -> usize {
        let b = stream_body(dict, data);
        self.add(b)
    }

    pub fn page(&mut self, resources: &str, content: &str, extra: &str) -> usize {
        let c = self.stream("", content.as_bytes());
        let p = self.add(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 200] /Resources << {resources} >> /Contents {c} 0 R {extra} >>"
        ));
        self.pages.push(p);
        p
    }

    pub fn pages_push(&mut self, p: usize) {
        self.pages.push(p);
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.objects[0] =
            format!("<< /Type /Catalog /Pages 2 0 R {} >>", self.catalog).into_bytes();
        let kids: String = self.pages.iter().map(|p| format!("{p} 0 R ")).collect();
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
            "trailer\n<< /Size {} /Root 1 0 R {} >>\nstartxref\n{xref}\n%%EOF\n",
            self.objects.len() + 1,
            self.trailer
        );
        out.extend_from_slice(x.as_bytes());
        out
    }
}

pub fn stream_body(dict: &str, data: &[u8]) -> Vec<u8> {
    let mut b = format!("<< {dict} /Length {} >>\nstream\n", data.len()).into_bytes();
    b.extend_from_slice(data);
    b.extend_from_slice(b"\nendstream");
    b
}

/// Appends an incremental update replacing or adding objects `(num, body)`.
pub fn append_update(mut file: Vec<u8>, objects: &[(usize, Vec<u8>)], size: usize) -> Vec<u8> {
    let prev = {
        let s = String::from_utf8_lossy(&file);
        let i = s.rfind("startxref").unwrap();
        s[i + 9..]
            .trim()
            .lines()
            .next()
            .unwrap()
            .trim()
            .parse::<usize>()
            .unwrap()
    };
    let mut rows = Vec::new();
    for (n, body) in objects {
        rows.push((*n, file.len()));
        file.extend_from_slice(format!("{n} 0 obj\n").as_bytes());
        file.extend_from_slice(body);
        file.extend_from_slice(b"\nendobj\n");
    }
    let xref = file.len();
    let mut x = String::from("xref\n");
    for (n, off) in rows {
        let _ = write!(x, "{n} 1\n{off:010} 00000 n \n");
    }
    let _ = write!(
        x,
        "trailer\n<< /Size {size} /Root 1 0 R /Prev {prev} >>\nstartxref\n{xref}\n%%EOF\n"
    );
    file.extend_from_slice(x.as_bytes());
    file
}

mod util;
pub use util::*;
pub mod docs;
