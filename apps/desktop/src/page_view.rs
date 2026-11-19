//! The page view of the window: `pagebara_ui::ViewBridge` (shared with the mobile app)
//! showing its slots through the `ViewState` global.

use pagebara_app_core::view::Placement;
use pagebara_ui::{Models, ViewSink};
use slint::{ComponentHandle, Image};

use crate::{AppWindow, PageSlot, TileSlot, ViewState};

pub use pagebara_ui::REDUCE_MOTION;

pub type ViewBridge = pagebara_ui::ViewBridge<AppWindow>;

impl ViewSink for AppWindow {
    type Page = PageSlot;
    type Tile = TileSlot;

    fn page_slot(p: &Placement, number: i32, preview: Image) -> PageSlot {
        PageSlot {
            di: p.di,
            by: p.by,
            bx: p.bx,
            w: p.w,
            h: p.h,
            visible: true,
            number,
            preview,
        }
    }

    fn tile_slot(p: &Placement) -> TileSlot {
        TileSlot {
            di: p.di,
            by: p.by,
            bx: p.bx,
            w: p.w,
            h: p.h,
            visible: true,
        }
    }

    fn scale_factor(&self) -> f64 {
        f64::from(self.window().scale_factor())
    }

    fn show_models(&self, m: Models<PageSlot, TileSlot>) {
        let vs = self.global::<ViewState>();
        vs.set_pages(m.pages);
        vs.set_page_covered(m.covered);
        vs.set_tiles(m.tiles);
        vs.set_tile_images(m.tile_images);
        vs.set_fallback(m.fallback);
        vs.set_fallback_images(m.fallback_images);
    }

    fn show_transform(&self, k: f32, cx: f32, oy: f32, dpr: f32) {
        let vs = self.global::<ViewState>();
        vs.set_k(k);
        vs.set_cx(cx);
        vs.set_oy(oy);
        vs.set_dpr(dpr);
    }

    fn show_scrollbars(&self, v_start: f32, v_size: f32, h_start: f32, h_size: f32) {
        let vs = self.global::<ViewState>();
        vs.set_v_start(v_start);
        vs.set_v_size(v_size);
        vs.set_h_start(h_start);
        vs.set_h_size(h_size);
    }
}
