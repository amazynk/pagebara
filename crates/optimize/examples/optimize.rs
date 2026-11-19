//! `cargo run --release -p pagebara-optimize --example optimize -- IN OUT [PRESET] [--linearize]
//! [--maximum]`: optimizes a file and prints the report with the throughput.

use pagebara_optimize::{Effort, Preset, optimize};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (Some(input), Some(output)) = (args.first(), args.get(1)) else {
        eprintln!("usage: optimize IN OUT [lossless|balanced|small] [--linearize] [--maximum]");
        std::process::exit(2);
    };
    let preset = args
        .get(2)
        .and_then(|p| Preset::parse(p))
        .unwrap_or(Preset::Lossless);
    let mut opts = preset.options();
    opts.linearize = args.iter().any(|a| a == "--linearize");
    if args.iter().any(|a| a == "--maximum") {
        opts.effort = Effort::Maximum;
    }
    let bytes = std::fs::read(input).expect("read");
    let t = std::time::Instant::now();
    let doc = pagebara_syntax::Document::open(bytes).expect("open");
    let out = optimize(&doc, &opts, None).expect("optimize");
    let secs = t.elapsed().as_secs_f64();
    std::fs::write(output, &out.bytes).expect("write");
    let r = &out.report;
    println!(
        "{input}: {} -> {} bytes ({:.1}% smaller) in {secs:.2} s, {:.1} MB/s",
        r.before,
        r.after,
        r.saved_percent(),
        r.before as f64 / secs / 1e6
    );
    for c in &r.categories {
        println!(
            "  {:<14} {:>12} -> {:>12}",
            c.category.title(),
            c.before,
            c.after
        );
    }
    println!(
        "  {} streams recompressed, {} objects merged, {} removed, {} resources, {} fonts subset ({} glyphs), {} images resampled, {} re-encoded",
        r.streams_recompressed,
        r.objects_merged,
        r.objects_removed,
        r.resources_removed,
        r.fonts_subset,
        r.glyphs_removed,
        r.images_resampled,
        r.images_reencoded
    );
    for w in &r.warnings {
        println!("  warning: {w}");
    }
}
