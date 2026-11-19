//! Prints each glyph's name and control-point bounds: `bounds <font> [index]`.

use pagebara_font::{Font, Path};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).expect("read");
    let index = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let font = Font::parse_index(data, index).expect("parse");
    println!(
        "# {:?} {} glyphs {:?}",
        font.format(),
        font.num_glyphs(),
        font.font_matrix()
    );
    let mut p = Path::new();
    for g in 0..font.num_glyphs() {
        p.clear();
        let ok = font.outline(g, &mut p);
        let name = font.glyph_name(g).unwrap_or("-");
        match p.bounds() {
            Some(b) => println!("{g} {name} {ok} {} {} {} {}", b[0], b[1], b[2], b[3]),
            None => println!("{g} {name} {ok} empty"),
        }
    }
}
