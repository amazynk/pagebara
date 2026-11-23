use std::sync::Arc;
use std::sync::mpsc::channel;
use std::time::{Duration, Instant};

use super::*;
use crate::tiles::PlaceholderRenderer;

const A4: (f32, f32) = (595.0, 842.0);

fn layout(n: usize) -> Layout {
    Layout::new(std::iter::repeat_n(A4, n))
}

#[test]
fn layout_positions_and_ranges() {
    let l = Layout::new([A4, (842.0, 595.0), A4]);
    let k = 1.0;
    assert_eq!(l.page_top(0, k), GAP);
    assert_eq!(l.page_top(1, k), 2.0 * GAP + 842.0);
    assert_eq!(l.content_height(k), 4.0 * GAP + 842.0 * 2.0 + 595.0);
    assert_eq!(l.content_width(k, 100.0), 842.0 + 2.0 * GAP);
    let r = l.page_rect(0, k, 1000.0);
    assert_eq!((r.x, r.w), ((1000.0 - 595.0) / 2.0, 595.0));
    let rows = |y0: f64, y1: f64| {
        let v = Rect {
            x: 0.0,
            y: y0,
            w: 1000.0,
            h: y1 - y0,
        };
        l.pages_in(&v, k, 1000.0)
    };
    assert_eq!(rows(0.0, 10.0), 0..0);
    assert_eq!(rows(0.0, 17.0), 0..1);
    assert_eq!(rows(850.0, 900.0), 0..2);
    assert_eq!(rows(860.0, 900.0), 1..2);
    assert_eq!(rows(0.0, 1e9), 0..3);
    assert_eq!(l.page_at(500.0, 1e9, k, 1000.0), 2);
}

#[test]
fn zoom_keeps_the_point_under_the_cursor() {
    let mut v = Viewport::new(layout(50));
    v.set_size(1000.0, 700.0);
    v.scroll_to(0.0, 5000.0);
    let probe = |v: &Viewport, ax: f64, ay: f64| {
        let k = v.k();
        let (sx, sy) = v.scroll();
        let cw = v.content_size().0;
        let i = v.layout().page_at(sx + ax, sy + ay, k, cw);
        let r = v.layout().page_rect(i, k, cw);
        (i, (sx + ax - r.x) / k, (sy + ay - r.y) / k)
    };
    let before = probe(&v, 300.0, 200.0);
    v.zoom_at(2.5, 300.0, 200.0);
    let after = probe(&v, 300.0, 200.0);
    assert_eq!(before.0, after.0);
    assert!((before.1 - after.1).abs() < 1e-6, "{before:?} {after:?}");
    assert!((before.2 - after.2).abs() < 1e-6, "{before:?} {after:?}");
}

#[test]
fn fit_and_navigation() {
    let mut v = Viewport::new(layout(10));
    v.set_size(800.0, 600.0);
    v.fit_width();
    let r = v.layout().page_rect(0, v.k(), v.content_size().0);
    assert!((r.w - (800.0 - 2.0 * GAP)).abs() < 1e-6);
    v.go_to_page(4);
    assert_eq!(v.current_page(), 4);
    v.fit_page();
    let r = v.layout().page_rect(4, v.k(), v.content_size().0);
    let view = v.visible();
    assert!(
        r.y >= view.y - 1e-6 && r.bottom() <= view.bottom() + 1e-6,
        "{r:?} {view:?}"
    );
    assert_eq!(v.current_page(), 4);
    assert_eq!(zoom::step_in(1.0), 1.1);
    assert_eq!(zoom::step_out(1.0), 0.9);
    assert_eq!(zoom::step_in(1.05), 1.1);
    assert_eq!(zoom::label(1.1), "110%");
}

#[test]
fn page_view_fills_the_screen_with_tiles() {
    let (tx, rx) = channel();
    let renderer = Arc::new(PlaceholderRenderer { delay_ms: 0..1 });
    let mut view =
        PageView::<crate::tiles::tests::VecBuf>::new(layout(100), renderer, 64 << 20, move || {
            let _ = tx.send(());
        });
    view.viewport.set_dpr(1.5);
    view.viewport.set_size(1200.0, 800.0);
    view.viewport.scroll_to(0.0, 30_000.0);
    let start = Instant::now();
    let frame = loop {
        let (frame, _) = view.update(Instant::now());
        if frame.missing == 0 {
            break frame;
        }
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "tiles never arrived"
        );
        let _ = rx.recv_timeout(Duration::from_millis(100));
    };
    assert!(!frame.pages.is_empty());
    assert!(!frame.tiles.is_empty());
    // Placements are relative to the first visible page and map back to the layout.
    let (first, place) = frame.pages[0];
    assert_eq!(place.di, 0);
    let top = frame.oy + GAP * f64::from(place.di) + frame.k * f64::from(place.by);
    let (_, sy) = view.viewport.scroll();
    assert!((top - (view.viewport.layout().page_top(first, frame.k) - sy)).abs() < 1e-3);

    // Zooming in shows the old tiles scaled until the new bucket arrives.
    view.zoom_gesture(Instant::now());
    view.viewport.zoom_to(1.6);
    let (frame, _) = view.update(Instant::now());
    assert!(frame.missing > 0);
    assert!(!frame.fallback.is_empty());
    assert!(frame.wake_at.is_some());
}

#[test]
fn zooming_shows_one_layer_of_stand_ins() {
    let (tx, rx) = channel();
    let renderer = Arc::new(PlaceholderRenderer { delay_ms: 0..1 });
    let mut view =
        PageView::<crate::tiles::tests::VecBuf>::new(layout(20), renderer, 64 << 20, move || {
            let _ = tx.send(());
        });
    view.viewport.set_size(1200.0, 800.0);
    view.viewport.scroll_to(0.0, 3_000.0);
    // Two buckets cached around the same spot.
    for z in [1.0, 1.5] {
        view.viewport.zoom_to(z);
        settle(&mut view, &rx);
        // And the tiles rendered ahead.
        while view.stats().2 > 0 {
            let _ = rx.recv_timeout(Duration::from_millis(20));
            view.update(Instant::now());
        }
    }
    // Zooming between them: each missing area gets the nearest bucket alone, which
    // covers the pages, so their paper and preview are not drawn.
    view.zoom_gesture(Instant::now());
    view.viewport.zoom_to(1.4);
    let (frame, _) = view.update(Instant::now());
    assert!(frame.missing > 0 && !frame.fallback.is_empty());
    let levels: HashSet<i16> = frame.fallback.iter().map(|(k, _)| k.level).collect();
    assert_eq!(levels.len(), 1, "{levels:?}");
    assert!(frame.covered.iter().all(|&c| c), "{:?}", frame.covered);
    // Zooming out past what is cached: the pages that come into view show their paper.
    view.zoom_gesture(Instant::now());
    view.viewport.zoom_to(0.3);
    let (frame, _) = view.update(Instant::now());
    assert!(frame.covered.iter().any(|&c| !c), "{:?}", frame.covered);
}

/// Updates until every visible tile is shown.
fn settle(view: &mut PageView<crate::tiles::tests::VecBuf>, rx: &std::sync::mpsc::Receiver<()>) {
    let start = Instant::now();
    while view.update(Instant::now()).0.missing > 0 {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "tiles never arrived"
        );
        let _ = rx.recv_timeout(Duration::from_millis(20));
    }
}

#[test]
fn new_tiles_appear_a_few_per_frame_while_moving() {
    let (tx, rx) = channel();
    let renderer = Arc::new(PlaceholderRenderer { delay_ms: 0..1 });
    let mut view =
        PageView::<crate::tiles::tests::VecBuf>::new(layout(100), renderer, 64 << 20, move || {
            let _ = tx.send(());
        });
    // One new tile per frame while moving. The allowance refills with the time since the
    // last frame, which on a slow machine is long enough to show the few tiles coming back
    // all at once: with one per frame they are spread over frames however slow it runs.
    view.tuning.uploads_per_frame = 1;
    let cap = 1;
    view.viewport.set_size(1200.0, 800.0);
    // At rest a whole screen of tiles appears at once.
    settle(&mut view, &rx);
    let (frame, _) = view.update(Instant::now());
    assert!(frame.tiles.len() > cap);
    view.viewport.scroll_to(0.0, 20_000.0);
    settle(&mut view, &rx);
    // A jump back to cached tiles shows the whole screen at once.
    view.viewport.scroll_to(0.0, 0.0);
    let (frame, _) = view.update(Instant::now());
    assert_eq!(frame.deferred, 0);
    assert!(frame.tiles.len() > cap);
    view.viewport.scroll_to(0.0, 20_000.0);
    settle(&mut view, &rx);
    // Scrolling back to them: they come back nearest first, a few per frame.
    view.viewport.scroll_to(0.0, 350.0);
    let before = view.update(Instant::now()).0.tiles.len();
    view.viewport.scroll_to(0.0, 0.0);
    let mut now = Instant::now();
    let (frame, _) = view.update(now);
    assert!(
        frame.tiles.len() <= before + cap,
        "{} tiles",
        frame.tiles.len()
    );
    assert!(frame.deferred > 0);
    assert_eq!(frame.wake_at, Some(now + FRAME));
    let mut shown = frame.tiles.len();
    let mut frames = 1;
    loop {
        now += FRAME;
        let (frame, _) = view.update(now);
        assert!(frame.tiles.len() <= shown + cap);
        shown = frame.tiles.len();
        frames += 1;
        if frame.deferred == 0 {
            break;
        }
        assert!(frames < 100);
    }
    assert!(frames > 1);
    // Once the view rests, the rest appear at once.
    view.viewport.scroll_to(0.0, 20_000.0);
    view.update(now);
    let (frame, _) = view.update(now + MOVING_FOR + FRAME);
    assert_eq!(frame.deferred, 0);
    assert_eq!(frame.missing, 0);
}

fn modes(n: usize, mode: LayoutMode) -> Layout {
    Layout::with_mode(std::iter::repeat_n(A4, n), mode)
}

#[test]
fn spreads_put_pages_side_by_side() {
    let k = 1.0;
    let cw = 2000.0;
    let l = modes(5, LayoutMode::TwoPage);
    assert_eq!(l.row_count(), 3);
    assert_eq!(l.row_pages(1), 2..4);
    let (a, b) = (l.page_rect(0, k, cw), l.page_rect(1, k, cw));
    assert_eq!(a.y, b.y);
    assert!(a.right() < cw / 2.0 && b.x > cw / 2.0);
    // The lone last page is a left-hand page.
    assert!(l.page_rect(4, k, cw).right() < cw / 2.0);
    assert_eq!(l.fit_size(0).1, 842.0);
    assert!(l.fit_size(0).0 > 2.0 * 595.0);

    let b = modes(4, LayoutMode::Book);
    assert_eq!(b.row_count(), 3);
    assert_eq!(b.row_pages(0), 0..1);
    assert_eq!(b.row_pages(1), 1..3);
    // The cover is a right-hand page; spreads meet at the spine.
    assert_eq!(b.page_rect(0, k, cw).x, cw / 2.0);
    assert_eq!(b.page_rect(1, k, cw).right(), b.page_rect(2, k, cw).x);
    assert_eq!(b.page_rect(3, k, cw).right(), cw / 2.0);
    // Placements relative to an anchor map back to the layout.
    let (di, dy, dx) = b.offset(1, 2);
    assert_eq!((di, dy), (0, 0.0));
    assert_eq!(dx, 595.0);
    let (di, dy, _) = b.offset(0, 3);
    assert_eq!((di, dy), (2, 842.0 * 2.0));
}

#[test]
fn horizontal_strip_scrolls_sideways() {
    let k = 1.0;
    let l = modes(10, LayoutMode::Horizontal);
    assert_eq!(l.row_count(), 1);
    let cw = l.content_width(k, 800.0);
    assert!(cw > 10.0 * 595.0);
    assert_eq!(l.content_height(k), 842.0 + 2.0 * GAP);
    let view = Rect {
        x: l.page_rect(3, k, cw).x + 10.0,
        y: 0.0,
        w: 800.0,
        h: 600.0,
    };
    assert_eq!(l.pages_in(&view, k, cw), 3..5);
    assert_eq!(l.page_at(view.x + 1.0, 100.0, k, cw), 3);

    let mut v = Viewport::new(l);
    v.set_size(800.0, 1400.0);
    v.go_to_page(6);
    assert_eq!(v.current_page(), 6);
    v.fit_page();
    assert_eq!(v.current_page(), 6);
    let r = v.layout().page_rect(6, v.k(), v.content_size().0);
    let vis = v.visible();
    assert!(r.x >= vis.x && r.right() <= vis.right(), "{r:?} {vis:?}");
    // Shorter than the view: centred vertically.
    assert!(vis.y < 0.0);
    v.fit_width();
    assert_eq!(v.current_page(), 6);
}

#[test]
fn single_page_shows_one_page_at_a_time() {
    let mut v = Viewport::new(modes(5, LayoutMode::Single));
    v.set_size(800.0, 600.0);
    v.fit_width();
    v.go_to_page(2);
    assert_eq!(v.visible_pages(), 2..3);
    assert!(v.at_edge(false));
    // Scrolling stops at the end of the page.
    v.scroll_by(0.0, 1e6);
    assert_eq!(v.current_page(), 2);
    assert!(v.at_edge(true));
    let b = v.bounds();
    assert!(v.scroll().1 + 600.0 <= b.bottom() + 1e-6);
    v.go_to_page(3);
    assert_eq!(v.current_page(), 3);
    v.fit_page();
    assert_eq!(v.visible_pages(), 3..4);
    let r = v.layout().page_rect(3, v.k(), v.content_size().0);
    let vis = v.visible();
    assert!(r.y >= vis.y && r.bottom() <= vis.bottom(), "{r:?} {vis:?}");
}

#[test]
fn every_layout_fits_and_navigates() {
    for mode in LayoutMode::ALL {
        let mut v = Viewport::new(modes(7, mode));
        v.set_size(1000.0, 700.0);
        for fit in 0..3 {
            v.go_to_page(4);
            match fit {
                0 => v.fit_width(),
                1 => v.fit_page(),
                _ => v.zoom_to(1.0),
            }
            let i = v.current_page();
            // A spread shows the page or its partner.
            assert!(
                v.layout().row_of(i) == v.layout().row_of(4),
                "{mode:?} fit {fit}: page {i}"
            );
            assert!(v.visible_pages().contains(&i), "{mode:?}");
        }
        assert_eq!(LayoutMode::parse(mode.name()), Some(mode));
        assert_eq!(LayoutMode::from_index(mode.index()), mode);
    }
}

#[test]
fn right_to_left_spreads_start_on_the_right() {
    let (k, cw) = (1.0, 2000.0);
    let l = Layout::with_direction(std::iter::repeat_n(A4, 5), LayoutMode::TwoPage, true);
    let (a, b) = (l.page_rect(0, k, cw), l.page_rect(1, k, cw));
    assert!(a.x > cw / 2.0 && b.right() < cw / 2.0);
    assert!(l.page_rect(4, k, cw).x > cw / 2.0);
    let book = Layout::with_direction(std::iter::repeat_n(A4, 3), LayoutMode::Book, true);
    // The cover is a left-hand page in a right-to-left book.
    assert!(book.page_rect(0, k, cw).right() <= cw / 2.0);
    let strip = Layout::with_direction(std::iter::repeat_n(A4, 3), LayoutMode::Horizontal, true);
    assert!(strip.page_rect(0, k, cw).x > strip.page_rect(1, k, cw).x);
}

#[test]
fn turning_pages_shows_the_whole_page_at_once() {
    let (tx, rx) = channel();
    let renderer = Arc::new(PlaceholderRenderer { delay_ms: 0..1 });
    let mut view = PageView::<crate::tiles::tests::VecBuf>::new(
        modes(5, LayoutMode::Single),
        renderer,
        64 << 20,
        move || {
            let _ = tx.send(());
        },
    );
    view.viewport.set_size(1200.0, 900.0);
    view.viewport.fit_page();
    for page in [1, 0] {
        view.viewport.go_to_page(page);
        settle(&mut view, &rx);
    }
    // Back and forth between two cached pages: never a tile held back.
    let mut now = Instant::now();
    for page in [1, 0, 1, 0] {
        view.viewport.go_to_page(page);
        let (frame, _) = view.update(now);
        assert_eq!(frame.deferred, 0, "page {page}");
        assert_eq!(frame.missing, 0, "page {page}");
        now += FRAME;
    }
}
