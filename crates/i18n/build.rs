//! Packs the translations that Rust code uses (`po/<lang>.po`, entries referenced from
//! `.rs` files) into one deflated blob per language.

use std::fmt::Write;
use std::path::Path;

#[path = "src/po.rs"]
#[allow(dead_code)]
mod po;

fn main() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../po");
    println!("cargo:rerun-if-changed={}", dir.display());
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .map(|d| d.filter_map(Result::ok).map(|e| e.path()).collect())
        .unwrap_or_default();
    files.retain(|p| p.extension().is_some_and(|e| e == "po"));
    files.sort();
    let mut table = String::from("pub static CATALOGS: &[(&str, &[u8])] = &[\n");
    for path in files {
        println!("cargo:rerun-if-changed={}", path.display());
        let lang = path
            .file_stem()
            .and_then(|s| s.to_str())
            .expect("file name")
            .to_owned();
        let src = std::fs::read_to_string(&path).expect("reading a .po file");
        let entries = po::parse(&src);
        let any_refs = entries.iter().any(|e| !e.refs.is_empty());
        let mut packed = String::new();
        for e in entries
            .iter()
            .filter(|e| e.translated() && e.context.is_none())
        {
            if any_refs && !e.refs.iter().any(|r| r.contains(".rs:")) {
                continue;
            }
            packed.push_str(&e.id);
            for s in &e.strs {
                packed.push('\u{1f}');
                packed.push_str(s);
            }
            packed.push('\u{1e}');
        }
        let blob = miniz_oxide::deflate::compress_to_vec(packed.as_bytes(), 9);
        let file = out.join(format!("{lang}.bin"));
        std::fs::write(&file, blob).expect("writing a catalog");
        writeln!(
            table,
            "    ({lang:?}, include_bytes!({:?})),",
            file.display().to_string()
        )
        .unwrap();
    }
    table.push_str("];\n");
    std::fs::write(out.join("catalogs.rs"), table).expect("writing catalogs.rs");
}
