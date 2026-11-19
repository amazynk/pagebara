mod common;

use std::time::Instant;

use common::*;
use pagebara_signatures::*;

#[test]
fn scanned_signature_background_removed() {
    let s = scan(1200, 800);
    let prep = Prepared::new(&s.img, 1200);
    assert_eq!((prep.width(), prep.height()), (1200, 800));
    let alpha = prep.alpha(&Settings::default());

    // Paper: well away from the stroke.
    let (w, h) = (1200usize, 800usize);
    let r = s.radius.ceil() as usize + 3;
    let near_ink = |x: usize, y: usize| {
        (y.saturating_sub(r)..(y + r).min(h))
            .any(|yy| (x.saturating_sub(r)..(x + r).min(w)).any(|xx| s.coverage[yy * w + xx] > 0.0))
    };
    let (mut paper, mut paper_clear, mut paper_sum) = (0u64, 0u64, 0u64);
    let (mut ink, mut ink_solid) = (0u64, 0u64);
    for y in 0..h {
        for x in 0..w {
            let k = y * w + x;
            let a = alpha[k];
            if s.coverage[k] >= 1.0 {
                ink += 1;
                ink_solid += u64::from(a >= 250);
            } else if !near_ink(x, y)
                && s.specks
                    .iter()
                    .all(|&(sx, sy)| (sx as usize).abs_diff(x) > 3 || (sy as usize).abs_diff(y) > 3)
            {
                paper += 1;
                paper_clear += u64::from(a <= 5);
                paper_sum += u64::from(a);
            }
        }
    }
    assert!(
        paper_clear as f64 / paper as f64 > 0.999,
        "{paper_clear}/{paper}"
    );
    assert!((paper_sum as f64 / paper as f64) < 0.5);
    assert!(ink_solid as f64 / ink as f64 > 0.98, "{ink_solid}/{ink}");
    for &(x, y) in &s.specks {
        assert_eq!(alpha[y as usize * w + x as usize], 0, "speck at {x},{y}");
    }

    // Applied: cropped around the stroke; uniform ink colour when asked.
    let out = prep.apply(&Settings {
        ink: Ink::DARK_BLUE,
        ..Settings::default()
    });
    assert!(out.width < 1000 && out.width > 800, "{}", out.width);
    assert!(out.height < 500);
    let opaque: Vec<_> = out
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 0)
        .collect();
    assert!(opaque.iter().all(|p| p[..3] == [0x0b, 0x24, 0x7a]));
    // Original colour: the ink, not the paper tint.
    let orig = prep.apply(&Settings::default());
    let solid: Vec<_> = orig
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] == 255)
        .collect();
    let mean_b = solid.iter().map(|p| f64::from(p[2])).sum::<f64>() / solid.len() as f64;
    let mean_r = solid.iter().map(|p| f64::from(p[0])).sum::<f64>() / solid.len() as f64;
    assert!(mean_b > mean_r + 30.0, "{mean_r} {mean_b}");
}

#[test]
fn threshold_controls_how_much_is_kept() {
    let s = scan(600, 400);
    let prep = Prepared::new(&s.img, 600);
    let kept = |t| {
        let a = prep.alpha(&Settings {
            threshold: t,
            ..Settings::default()
        });
        a.iter().filter(|&&a| a > 128).count()
    };
    let (lo, mid, hi) = (kept(0), kept(50), kept(100));
    assert!(lo >= mid && mid >= hi && lo > hi, "{lo} {mid} {hi}");
    assert!(hi > 0);
    let off = prep.apply(&Settings {
        remove_background: false,
        ..Settings::default()
    });
    assert_eq!(off, s.img);
}

#[test]
fn preview_is_fast_on_a_12_megapixel_photo() {
    let s = scan(4000, 3000);
    let t = Instant::now();
    let prep = Prepared::new(&s.img, PREVIEW_SIDE);
    let prepare = t.elapsed();
    let mut worst = std::time::Duration::ZERO;
    for threshold in [20, 50, 80] {
        // Best of a few, so a busy machine does not fail the test.
        let mut best = std::time::Duration::MAX;
        for _ in 0..3 {
            let t = Instant::now();
            let out = prep.apply(&Settings {
                threshold,
                ..Settings::default()
            });
            best = best.min(t.elapsed());
            assert!(out.width > 100);
        }
        worst = worst.max(best);
    }
    eprintln!("12 MP: prepare {prepare:?}, slider update {worst:?}");
    // Generous: tests are not built with full optimisation. Release timings come from
    // `examples/background_bench.rs`.
    assert!(worst.as_millis() < 500);
}

#[test]
fn drawn_signature() {
    let d = Drawing {
        strokes: vec![
            (0..50)
                .map(|i| StrokePoint {
                    x: i as f32 * 4.0,
                    y: 40.0 + (i as f32 / 5.0).sin() * 20.0,
                    pressure: Some(i as f32 / 50.0),
                })
                .collect(),
            vec![StrokePoint::new(100.0, 90.0)],
        ],
        pen_width: 3.0,
    };
    let img = render_drawing(&d, [0, 0, 0x80]);
    assert!(img.width <= STORED_MAX && img.width > 500);
    let ink = img
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] == 255)
        .count();
    let partial = img
        .pixels
        .as_chunks::<4>()
        .0
        .iter()
        .filter(|p| p[3] > 0 && p[3] < 255)
        .count();
    assert!(ink > 1000 && partial > 100);
}
