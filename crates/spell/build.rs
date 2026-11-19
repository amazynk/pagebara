//! Compresses the bundled en_US dictionary (`data/`) into `OUT_DIR`; the library inflates it
//! on first use.

use std::path::{Path, PathBuf};

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    for name in ["en_US.aff", "en_US.dic"] {
        let src = Path::new("data").join(name);
        println!("cargo:rerun-if-changed={}", src.display());
        let data = std::fs::read(&src).expect("bundled dictionary");
        let packed = miniz_oxide::deflate::compress_to_vec(&data, 10);
        std::fs::write(out.join(format!("{name}.deflate")), packed).expect("writing OUT_DIR");
    }
}
