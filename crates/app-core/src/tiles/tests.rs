use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;
use std::time::Duration;

use super::*;
use crate::view::zoom::{bucket, bucket_scale};

/// A plain `Vec` as tile storage.
pub(crate) struct VecBuf(pub Vec<u8>);

impl TileBuffer for VecBuf {
    fn new(w: u32, h: u32) -> Self {
        VecBuf(vec![0; (w * h * 4) as usize])
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        &mut self.0
    }
}

#[test]
fn buckets_round_up() {
    assert_eq!(bucket(2.0), 16);
    assert_eq!(bucket(2.01), 17);
    assert_eq!(bucket(1.0), 0);
    assert_eq!(bucket(0.5), -16);
    assert_eq!(bucket_scale(16), 2.0);
    for s in [0.07, 0.33, 1.5, 3.7, 160.0] {
        let b = bucket_scale(bucket(s));
        assert!(b >= s && b < s * 1.05, "{s} -> {b}");
    }
}

#[test]
fn grid_geometry() {
    let g = Grid::new(595.0, 842.0, 16);
    assert_eq!((g.w_px, g.h_px), (1190, 1684));
    assert_eq!((g.cols(), g.rows()), (5, 7));
    assert_eq!(
        g.tile_rect(4, 6),
        PixelRect {
            x: 1024,
            y: 1536,
            w: 166,
            h: 148
        }
    );
    assert_eq!(g.tile_pt(1, 0), [128.0, 0.0, 128.0, 128.0]);
    assert_eq!(g.tiles_in([100.0, 0.0, 100.0, 10.0]), (0..2, 0..1));
    assert_eq!(g.tiles_in([590.0, 830.0, 100.0, 100.0]), (4..5, 6..7));
    assert_eq!(g.tiles_in([700.0, 0.0, 10.0, 10.0]).0, 5..5);
    let p = Grid::preview(595.0, 842.0);
    assert_eq!((p.cols(), p.rows(), p.h_px), (1, 1, PREVIEW));
}

#[test]
fn placeholder_draws_paper_grid_and_number() {
    let r = PlaceholderRenderer { delay_ms: 0..1 };
    let g = Grid::new(595.0, 842.0, 0);
    let job = Job {
        key: g.key(0, 0, 0),
        page: 0,
        page_size: (595.0, 842.0),
        scale: g.scale,
        rect: g.tile_rect(0, 0),
    };
    let mut out = vec![0; (job.rect.w * job.rect.h * 4) as usize];
    assert!(r.render(&job, &Default::default(), &mut out));
    let px = |x: u32, y: u32| &out[((y * job.rect.w + x) * 4) as usize..][..4];
    assert_eq!(px(200, 200), [255; 4]);
    assert_eq!(px(36, 200), [228, 228, 231, 255]);
    // The tile label in the corner.
    assert_eq!(px(9, 7), [37, 99, 235, 255]);
}

#[test]
fn cache_evicts_least_recent_untouched() {
    let mut c = TileCache::new(1000);
    let key = |x| Key {
        page: 0,
        level: 0,
        x,
        y: 0,
    };
    for x in 0..4 {
        c.insert(key(x), (), 300);
        c.next_round();
    }
    c.touch(&key(0));
    c.evict();
    assert!(c.contains(&key(0)));
    assert!(!c.contains(&key(1)));
    assert!(c.contains(&key(3)));
    assert_eq!(c.bytes(), 900);
    assert_eq!(c.levels(0).collect::<Vec<_>>(), [0]);
}

#[test]
fn scheduler_renders_wanted_and_skips_stale() {
    let (tx, rx) = channel();
    let pool = Arc::new(
        pagebara_thread::rayon_pool("test-tile")
            .num_threads(1)
            .build()
            .unwrap(),
    );
    let renderer = Arc::new(PlaceholderRenderer { delay_ms: 3..4 });
    let mut s = Scheduler::<VecBuf>::new(pool, renderer, move || {
        let _ = tx.send(());
    });
    let g = Grid::new(600.0, 100.0, 0);
    let job = |x| Job {
        key: g.key(0, x, 0),
        page: 0,
        page_size: (600.0, 100.0),
        scale: g.scale,
        rect: g.tile_rect(x, 0),
    };
    s.want(&[job(0)], |_| false);
    // Replacing the wanted set before the worker gets there skips the first job (most of
    // the time: the single worker is busy for 3 ms with nothing; accept either outcome).
    s.want(&[job(1)], |_| false);
    let mut got = Vec::new();
    while s.in_flight() > 0 {
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        got.extend(s.drain().into_iter().map(|(k, b)| (k.x, b.0.len())));
    }
    assert!(got.contains(&(1, (g.tile_rect(1, 0).w * g.tile_rect(1, 0).h * 4) as usize)));
    assert_eq!(s.rendered().0, got.len() as u64);
}

fn engine(bytes: Vec<u8>) -> EngineRenderer {
    let doc = pagebara_syntax::Document::open(bytes).unwrap();
    let refs = pagebara_pages::PageTree::open(&doc).refs();
    EngineRenderer::new(RenderSource { doc, refs })
}

#[test]
fn engine_draws_page_content() {
    let r = engine(crate::test_pdf::text(&["Hello world"]));
    assert_eq!(r.page_size(0), Some((595.0, 842.0)));
    let g = Grid::new(595.0, 842.0, 0);
    let job = Job {
        key: g.key(0, 0, 0),
        page: 0,
        page_size: (595.0, 842.0),
        scale: g.scale,
        rect: g.tile_rect(0, 0),
    };
    let mut out = vec![0; (job.rect.w * job.rect.h * 4) as usize];
    assert!(r.render(&job, &AtomicBool::new(false), &mut out));
    // "Hello world" starts at x 72, baseline 72 pt from the top: ink there, paper around.
    let dark = |x0: u32, y0: u32, x1: u32, y1: u32| {
        let mut n = 0;
        for y in y0..y1 {
            for x in x0..x1 {
                let i = ((y * job.rect.w + x) * 4) as usize;
                n += usize::from(out[i] < 100);
            }
        }
        n
    };
    assert!(dark(72, 62, 140, 74) > 20);
    assert_eq!(dark(150, 100, 250, 200), 0);
    assert_eq!(out[3], 255);
    assert!(!r.render(&job, &AtomicBool::new(true), &mut out));
}

#[test]
fn engine_leaves_out_annotations_the_app_draws() {
    let mut doc = pagebara_syntax::Document::open(crate::test_pdf::form()).unwrap();
    let page = pagebara_pages::PageTree::open(&doc).refs()[0];
    let a = pagebara_annot::Annotation::new(
        pagebara_annot::Kind::Square { interior: None },
        pagebara_annot::Rect::new(300.0, 700.0, 400.0, 800.0),
    );
    let id = pagebara_annot::add(&mut doc, page, &a).unwrap();
    let r = EngineRenderer::new(RenderSource {
        doc,
        refs: vec![page],
    });
    // The square is the app's to draw; form fields are the renderer's.
    assert_eq!(*r.own(0), [id]);
}

#[test]
fn complete_renderer_draws_every_annotation() {
    let mut doc = pagebara_syntax::Document::open(crate::test_pdf::form()).unwrap();
    let page = pagebara_pages::PageTree::open(&doc).refs()[0];
    let a = pagebara_annot::Annotation::new(
        pagebara_annot::Kind::Square { interior: None },
        pagebara_annot::Rect::new(300.0, 700.0, 400.0, 800.0),
    );
    pagebara_annot::add(&mut doc, page, &a).unwrap();
    let r = EngineRenderer::complete(RenderSource {
        doc,
        refs: vec![page],
    });
    assert!(r.own(0).is_empty());
}

#[test]
fn grids_of_other_tile_sizes() {
    let g = Grid::with_tile(595.0, 842.0, 16, 512);
    assert_eq!((g.cols(), g.rows()), (3, 4));
    assert_eq!(g.tile_pt(1, 0), [256.0, 0.0, 256.0, 256.0]);
    assert_eq!(g.tiles_in([300.0, 0.0, 10.0, 10.0]), (1..2, 0..1));
}
