//! Compresses the hyphenation patterns in `data/hyph` (raw deflate, decoded on first use):
//! each `hyph-<tag>.pat.txt`, with its `hyph-<tag>.hyp.txt` exceptions after a NUL byte,
//! becomes `$OUT_DIR/hyph-<tag>.deflate`.

use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from("data/hyph");
    println!("cargo:rerun-if-changed={}", dir.display());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .expect("data/hyph")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".pat.txt").map(str::to_owned))
        .collect();
    names.sort();
    for name in names {
        let pat = dir.join(format!("{name}.pat.txt"));
        let hyp = dir.join(format!("{name}.hyp.txt"));
        println!("cargo:rerun-if-changed={}", pat.display());
        println!("cargo:rerun-if-changed={}", hyp.display());
        let mut data = std::fs::read(&pat).expect("patterns");
        data.push(0);
        if let Ok(h) = std::fs::read(&hyp) {
            data.extend(h);
        }
        let z = miniz_oxide::deflate::compress_to_vec(&data, 10);
        std::fs::write(out.join(format!("{name}.deflate")), z).expect("write patterns");
    }
}
