//! A generated document for benchmarks: `n` pages, mostly A4 with a few landscape and
//! Letter pages mixed in, so the layout is not uniform.

pub fn pdf(n: usize) -> Vec<u8> {
    let n = n.max(1);
    let mut objs = vec![
        "<</Type/Catalog/Pages 2 0 R>>".to_owned(),
        format!(
            "<</Type/Pages/Count {n}/Kids[{}]>>",
            (0..n)
                .map(|i| format!("{} 0 R", i + 3))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    ];
    for i in 0..n {
        let size = match i % 17 {
            5 => "842 595",
            11 => "612 792",
            _ => "595 842",
        };
        objs.push(format!("<</Type/Page/Parent 2 0 R/MediaBox[0 0 {size}]>>"));
    }
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::with_capacity(objs.len());
    for (i, o) in objs.iter().enumerate() {
        offsets.push(out.len());
        out.extend(format!("{} 0 obj\n{o}\nendobj\n", i + 1).bytes());
    }
    let xref = out.len();
    out.extend(format!("xref\n0 {}\n0000000000 65535 f\r\n", objs.len() + 1).bytes());
    for o in offsets {
        out.extend(format!("{o:010} 00000 n\r\n").bytes());
    }
    out.extend(
        format!(
            "trailer\n<</Size {} /Root 1 0 R>>\nstartxref\n{xref}\n%%EOF\n",
            objs.len() + 1
        )
        .bytes(),
    );
    out
}
