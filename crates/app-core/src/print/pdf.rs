//! Writes rendered sheets as a PDF, one full-page image per page, streamed to disk so
//! long documents never sit in memory whole.

use pagebara_i18n::tr;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use super::Sheet;

/// A written print file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintDoc {
    pub path: PathBuf,
    pub pages: usize,
}

struct Out<W: Write> {
    w: W,
    at: u64,
    offsets: Vec<u64>,
}

impl<W: Write> Out<W> {
    fn bytes(&mut self, b: &[u8]) -> std::io::Result<()> {
        self.w.write_all(b)?;
        self.at += b.len() as u64;
        Ok(())
    }

    /// Writes object `num`; objects must come in order.
    fn object(&mut self, num: usize, body: &[u8]) -> std::io::Result<()> {
        debug_assert_eq!(self.offsets.len() + 1, num);
        self.offsets.push(self.at);
        self.bytes(format!("{num} 0 obj\n").as_bytes())?;
        self.bytes(body)?;
        self.bytes(b"\nendobj\n")
    }

    fn stream(&mut self, num: usize, dict: &str, data: &[u8]) -> std::io::Result<()> {
        let mut body = format!("<<{dict}/Length {}>>stream\n", data.len()).into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\nendstream");
        self.object(num, &body)
    }
}

/// Writes `count` sheets, `sheet(i)` giving the i-th, to `path`.
pub fn write_pdf(
    path: &Path,
    count: usize,
    mut sheet: impl FnMut(usize) -> Result<Sheet, String>,
) -> Result<PrintDoc, String> {
    let io = |e: std::io::Error| tr!("Could not write {}: {}", path.display(), e);
    let file = std::fs::File::create(path).map_err(io)?;
    let mut o = Out {
        w: BufWriter::new(file),
        at: 0,
        offsets: Vec::new(),
    };
    o.bytes(b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n").map_err(io)?;
    o.object(1, b"<</Type/Catalog/Pages 2 0 R>>").map_err(io)?;
    let kids: Vec<String> = (0..count).map(|i| format!("{} 0 R", 3 + 3 * i)).collect();
    o.object(
        2,
        format!("<</Type/Pages/Kids[{}]/Count {count}>>", kids.join(" ")).as_bytes(),
    )
    .map_err(io)?;
    for i in 0..count {
        let s = sheet(i)?;
        let (page, contents, image) = (3 + 3 * i, 4 + 3 * i, 5 + 3 * i);
        let (w, h) = s.size_pt;
        o.object(
            page,
            format!(
                "<</Type/Page/Parent 2 0 R/MediaBox[0 0 {w:.3} {h:.3}]\
                 /Resources<</XObject<</Im0 {image} 0 R>>>>/Contents {contents} 0 R>>"
            )
            .as_bytes(),
        )
        .map_err(io)?;
        let draw = format!("q {w:.3} 0 0 {h:.3} 0 0 cm /Im0 Do Q");
        o.stream(contents, "", draw.as_bytes()).map_err(io)?;
        let data = pagebara_filters::encode_flate(&s.data, 6);
        let cs = if s.grey { "DeviceGray" } else { "DeviceRGB" };
        o.stream(
            image,
            &format!(
                "/Type/XObject/Subtype/Image/Width {}/Height {}/ColorSpace/{cs}\
                 /BitsPerComponent 8/Interpolate true/Filter/FlateDecode",
                s.width, s.height
            ),
            &data,
        )
        .map_err(io)?;
    }
    let xref = o.at;
    let mut tail = format!("xref\n0 {}\n0000000000 65535 f\r\n", o.offsets.len() + 1);
    for off in &o.offsets {
        tail.push_str(&format!("{off:010} 00000 n\r\n"));
    }
    tail.push_str(&format!(
        "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
        o.offsets.len() + 1
    ));
    o.bytes(tail.as_bytes()).map_err(io)?;
    o.w.flush().map_err(io)?;
    Ok(PrintDoc {
        path: path.to_owned(),
        pages: count,
    })
}
