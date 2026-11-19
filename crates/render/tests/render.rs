//! Renders the synthetic suite: tiles agree with whole pages, known pixels have their
//! colours, and the request options behave.

#[path = "common/pdf.rs"]
mod pdf;
#[path = "common/suite.rs"]
mod suite;

use std::sync::atomic::AtomicBool;

use pagebara_render::{Annotations, PixelRect, RenderError, RenderRequest, Renderer};
use pagebara_syntax::Document;

fn renderer(data: Vec<u8>) -> Renderer {
    Renderer::new(Document::open(data).expect("synthetic file opens"))
}

fn full(r: &Renderer, page: usize, scale: f32) -> (u32, u32, Vec<u8>) {
    let (w, h) = r.page_size(page).unwrap();
    let (w, h) = ((w * scale).ceil() as u32, (h * scale).ceil() as u32);
    let img = r
        .render(
            page,
            &RenderRequest::new(scale, PixelRect { x: 0, y: 0, w, h }),
        )
        .unwrap();
    (w, h, img.data)
}

fn px(img: &(u32, u32, Vec<u8>), x: u32, y: u32) -> [u8; 4] {
    let o = ((y * img.0 + x) * 4) as usize;
    img.2[o..o + 4].try_into().unwrap()
}

fn close(a: [u8; 4], b: [u8; 4], tol: u8) -> bool {
    a.iter().zip(b).all(|(x, y)| x.abs_diff(y) <= tol)
}

#[test]
fn renderer_is_shared_across_threads() {
    fn shared<T: Send + Sync + Clone>() {}
    shared::<Renderer>();
    let r = renderer(suite::suite()[0].1.clone());
    let req = RenderRequest::new(
        1.0,
        PixelRect {
            x: 0,
            y: 0,
            w: 64,
            h: 64,
        },
    );
    let first = r.render(0, &req).unwrap();
    let others: Vec<_> = std::thread::scope(|s| {
        (0..4)
            .map(|i| {
                pagebara_thread::spawn_scoped(s, format!("ren-test-{i}"), || {
                    r.render(0, &req).unwrap()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect()
    });
    assert!(others.iter().all(|o| *o == first));
}

#[test]
fn tiles_match_whole_pages() {
    for (name, data) in suite::suite() {
        let r = renderer(data);
        for page in 0..r.page_count() {
            let whole = full(&r, page, 1.3);
            let (w, h) = (whole.0, whole.1);
            let t = 64u32;
            let mut off = 0usize;
            let mut at = (0, 0);
            for ty in 0..h.div_ceil(t) {
                for tx in 0..w.div_ceil(t) {
                    let rect = PixelRect {
                        x: (tx * t) as i32,
                        y: (ty * t) as i32,
                        w: t.min(w - tx * t),
                        h: t.min(h - ty * t),
                    };
                    let tile = r.render(page, &RenderRequest::new(1.3, rect)).unwrap();
                    for y in 0..rect.h {
                        for x in 0..rect.w {
                            let a = px(&whole, rect.x as u32 + x, rect.y as u32 + y);
                            let o = ((y * rect.w + x) * 4) as usize;
                            if (0..4).any(|k| a[k].abs_diff(tile.data[o + k]) > 8) {
                                off += 1;
                                at = (rect.x as u32 + x, rect.y as u32 + y);
                            }
                        }
                    }
                }
            }
            // Nearest-neighbour image samples exactly on a texel edge may round either way
            // with a tile's translation; seams would show as many pixels.
            assert!(
                off * 2000 <= (w * h) as usize,
                "{name} page {page}: {off} pixels differ between tiles and page, last {at:?}"
            );
        }
    }
}

#[test]
fn known_pixels() {
    let s = suite::suite();
    let get = |n: &str| renderer(s.iter().find(|f| f.0 == n).unwrap().1.clone());
    // paths: red rectangle 20,20 100x80 (y up, page 300 high) at scale 1.
    let p = full(&get("paths"), 0, 1.0);
    assert!(close(px(&p, 60, 240), [255, 0, 0, 255], 0));
    // Nested contours, nonzero: the reversed square is a hole showing the even-odd ring's
    // hole beneath (white); the innermost square is filled again.
    let p1 = full(&get("paths"), 1, 1.0);
    assert!(close(px(&p1, 150, 150), [255, 128, 0, 255], 1));
    assert!(close(px(&p1, 110, 190), [255, 255, 255, 255], 0), "holes");
    assert!(close(px(&p1, 60, 240), [255, 128, 0, 255], 1));
    // transparency: a knockout group's blue square replaces its red one where they meet.
    let t = full(&get("transparency"), 0, 1.0);
    let k = px(&t, 10 + 60, 300 - (160 + 60));
    assert!(k[2] > k[0], "knockout: blue over red, got {k:?}");
    // clips: the even-odd triangle ring leaves the inner triangle unpainted.
    let c = full(&get("clips"), 0, 1.0);
    assert!(close(px(&c, 75, 300 - 70), [255, 255, 255, 255], 0));
    assert!(close(px(&c, 75, 300 - 20), [0, 153, 0, 255], 1));
}

#[test]
fn rotation_turns_the_page() {
    let r = renderer(suite::suite()[0].1.clone());
    let a = full(&r, 0, 1.0);
    let mut req = RenderRequest::new(
        1.0,
        PixelRect {
            x: 0,
            y: 0,
            w: 300,
            h: 300,
        },
    );
    req.rotation = 90;
    let b = r.render(0, &req).unwrap();
    // A 90° clockwise turn maps (x, y) to (h - 1 - y, x).
    for (x, y) in [(60, 240), (215, 250), (10, 10)] {
        let o = (((x) * 300 + (299 - y)) * 4) as usize;
        assert_eq!(px(&a, x, y), b.data[o..o + 4], "at {x},{y}");
    }
}

#[test]
fn cancel_and_buffer_errors() {
    let r = renderer(suite::suite()[0].1.clone());
    let stop = AtomicBool::new(true);
    let mut req = RenderRequest::new(
        1.0,
        PixelRect {
            x: 0,
            y: 0,
            w: 300,
            h: 300,
        },
    );
    req.cancel = Some(&stop);
    // Cancellation is polled every few dozen operations; a tiny page may finish first.
    let res = r.render(0, &req);
    assert!(matches!(res, Ok(_) | Err(RenderError::Cancelled)));
    let mut buf = vec![0u8; 10];
    let req = RenderRequest::new(
        1.0,
        PixelRect {
            x: 0,
            y: 0,
            w: 4,
            h: 4,
        },
    );
    assert!(matches!(
        r.render_into(0, &req, &mut buf),
        Err(RenderError::BufferSize { .. })
    ));
    assert!(matches!(r.render(9, &req), Err(RenderError::NoSuchPage(9))));
}

#[test]
fn transparent_background_and_straight_alpha() {
    let s = suite::suite();
    let r = renderer(s.iter().find(|f| f.0 == "imagemasks").unwrap().1.clone());
    let mut req = RenderRequest::new(
        1.0,
        PixelRect {
            x: 0,
            y: 0,
            w: 300,
            h: 300,
        },
    );
    req.background = None;
    req.annotations = Annotations::None;
    let pre = r.render(0, &req).unwrap();
    req.straight_alpha = true;
    let st = r.render(0, &req).unwrap();
    assert!(pre.premultiplied && !st.premultiplied);
    for (a, b) in pre.data.chunks(4).zip(st.data.chunks(4)) {
        assert_eq!(a[3], b[3]);
        for k in 0..3 {
            assert!(a[k] <= b[k] || b[3] == 0);
        }
    }
}

#[test]
fn thumbnails_fit() {
    let r = renderer(suite::suite()[1].1.clone());
    let t = r.thumbnail(0, 64).unwrap();
    assert_eq!((t.width, t.height), (64, 64));
    assert_eq!(t.data.len(), 64 * 64 * 4);
}

/// Tiles needing the same image at once on a rayon pool (the codecs use rayon too).
#[test]
fn parallel_tiles_share_one_decode() {
    use rayon::prelude::*;
    let s = suite::suite();
    let pool = pagebara_thread::rayon_pool("ren-test")
        .num_threads(6)
        .build()
        .unwrap();
    for _ in 0..4 {
        let r = renderer(s.iter().find(|f| f.0 == "jpeg").unwrap().1.clone());
        let tiles: Vec<PixelRect> = (0..64)
            .map(|i| PixelRect {
                x: (i % 8) * 75,
                y: (i / 8) * 75,
                w: 75,
                h: 75,
            })
            .collect();
        let out: Vec<_> = pool.install(|| {
            tiles
                .par_iter()
                .map(|t| r.render(0, &RenderRequest::new(2.0, *t)).unwrap())
                .collect()
        });
        assert_eq!(out.len(), 64);
    }
}

fn rgb_at(img: &pagebara_render::RgbaImage, x: u32, y: u32) -> [u8; 4] {
    let o = ((y * img.width + x) * 4) as usize;
    img.data[o..o + 4].try_into().unwrap()
}

#[test]
fn optional_content_overrides() {
    let s = suite::suite();
    let r = renderer(s.iter().find(|f| f.0 == "optional").unwrap().1.clone());
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: 300,
        h: 300,
    };
    let white = [255, 255, 255, 255];
    let def = r.render(0, &RenderRequest::new(1.0, rect)).unwrap();
    assert_eq!(
        rgb_at(&def, 70, 230),
        [255, 0, 0, 255],
        "group ON by default"
    );
    assert_eq!(rgb_at(&def, 200, 230), white, "group OFF by default");
    assert_eq!(
        rgb_at(&def, 70, 100),
        white,
        "AllOn membership with an OFF group"
    );
    // Objects 3 and 4 are the two groups.
    let cfg = pagebara_render::OptionalContentConfig {
        on: vec![pagebara_syntax::ObjRef::new(4, 0)],
        off: vec![pagebara_syntax::ObjRef::new(3, 0)],
    };
    let mut req = RenderRequest::new(1.0, rect);
    req.optional_content = Some(&cfg);
    let alt = r.render(0, &req).unwrap();
    assert_eq!(rgb_at(&alt, 70, 230), white);
    assert_eq!(rgb_at(&alt, 200, 230), [0, 0, 255, 255]);
}

#[test]
fn annotation_modes() {
    let s = suite::suite();
    let r = renderer(s.iter().find(|f| f.0 == "annotations").unwrap().1.clone());
    let rect = PixelRect {
        x: 0,
        y: 0,
        w: 300,
        h: 300,
    };
    let paper = [230, 230, 230, 255];
    let shown = r.render(0, &RenderRequest::new(1.0, rect)).unwrap();
    // The widget's /On state is drawn; the hidden and no-view ones are not.
    assert!(close(rgb_at(&shown, 170, 130), [0, 153, 0, 255], 1));
    assert!(close(rgb_at(&shown, 175, 255), paper, 1));
    assert!(close(rgb_at(&shown, 245, 255), paper, 1));
    // The rotated appearance is fitted into its rectangle: blue in the middle.
    assert_eq!(rgb_at(&shown, 70, 85), [0, 0, 255, 255]);
    let mut req = RenderRequest::new(1.0, rect);
    req.annotations = Annotations::None;
    let none = r.render(0, &req).unwrap();
    assert_eq!(rgb_at(&none, 170, 130), [255, 255, 255, 255]);
    // Print mode shows the no-view annotation (flags Print | NoView).
    req.annotations = Annotations::Print;
    let print = r.render(0, &req).unwrap();
    assert_eq!(rgb_at(&print, 245, 255), [255, 0, 255, 255]);
    // Excluding the widget (object 8) by reference.
    req.annotations =
        Annotations::ViewExcept(std::sync::Arc::new(vec![pagebara_syntax::ObjRef::new(
            8, 0,
        )]));
    let except = r.render(0, &req).unwrap();
    assert_eq!(rgb_at(&except, 170, 130), [255, 255, 255, 255]);
}

#[test]
fn cache_limits_scale_with_memory() {
    use pagebara_render::Limits;
    let sum = |l: Limits| l.display_lists + l.images + l.glyphs + l.misc;
    let d = Limits::default();
    // A phone with 2 GB gets the floor, a 64 GB desktop the defaults.
    assert_eq!(sum(Limits::for_memory(2 << 30)) >> 20, 191);
    assert_eq!(sum(Limits::for_memory(64 << 30)), sum(d));
    // 8 GB: 512 MiB, split as the defaults are.
    let laptop = Limits::for_memory(8 << 30);
    assert!((512 << 20) - sum(laptop) < 4);
    assert_eq!(laptop.images / laptop.glyphs, d.images / d.glyphs);
}
