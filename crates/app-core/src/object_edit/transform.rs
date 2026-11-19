//! Selection handles and dragging them: moving (with snapping to the page and other
//! objects), scaling from a corner or an edge, rotating about the centre. Pointer
//! positions and rectangles are in view space; the result is a page-space matrix for
//! [`EditOp::Transform`](crate::edit::EditOp::Transform).

use pagebara_edit::{Matrix, PageFrame, Point};
use pagebara_i18n::tr;

/// A view-space rectangle: left, top, width, height.
pub type ViewRect = [f32; 4];

/// A grab handle of the selection frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Handle {
    /// 0 top left, 1 top right, 2 bottom right, 3 bottom left.
    Corner(u8),
    /// 0 top, 1 right, 2 bottom, 3 left.
    Edge(u8),
    /// Above the top edge's middle.
    Rotate,
}

impl Handle {
    /// The cursor that fits: 7 diagonal ↘, 8 diagonal ↗, 10 horizontal, 11 vertical, 12 rotate
    /// (the page view's cursor numbers).
    pub fn cursor(self) -> i32 {
        match self {
            Handle::Corner(0 | 2) => 7,
            Handle::Corner(_) => 8,
            Handle::Edge(1 | 3) => 10,
            Handle::Edge(_) => 11,
            Handle::Rotate => 12,
        }
    }
}

/// `r` grown by `d` on every side. Selection frames sit a little outside the objects'
/// bounds so their handles do not cover what is selected.
pub fn outset(r: ViewRect, d: f32) -> ViewRect {
    [r[0] - d, r[1] - d, r[2] + 2.0 * d, r[3] + 2.0 * d]
}

/// Handle positions of a frame; the rotation handle sits `gap` above the top edge.
pub fn handles(r: ViewRect, gap: f32) -> [(Handle, (f32, f32)); 9] {
    let [x, y, w, h] = r;
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    [
        (Handle::Corner(0), (x, y)),
        (Handle::Corner(1), (x + w, y)),
        (Handle::Corner(2), (x + w, y + h)),
        (Handle::Corner(3), (x, y + h)),
        (Handle::Edge(0), (cx, y)),
        (Handle::Edge(1), (x + w, cy)),
        (Handle::Edge(2), (cx, y + h)),
        (Handle::Edge(3), (x, cy)),
        (Handle::Rotate, (cx, y - gap)),
    ]
}

/// The handle within `tol` of `p`, corners first (small frames have their edge handles
/// on top of the corners).
pub fn handle_at(r: ViewRect, p: (f32, f32), tol: f32, gap: f32) -> Option<Handle> {
    handles(r, gap)
        .into_iter()
        .find(|(_, (hx, hy))| (hx - p.0).abs() <= tol && (hy - p.1).abs() <= tol)
        .map(|(h, _)| h)
}

/// A page-space matrix for a view-space one.
pub fn to_page(frame: &PageFrame, view: &Matrix) -> Matrix {
    frame.to_view().then(view).then(&frame.from_view())
}

/// Moving by `dx, dy` view points, as a page-space matrix.
pub fn move_by_view(frame: &PageFrame, dx: f32, dy: f32) -> Matrix {
    to_page(frame, &Matrix::translate(dx, dy))
}

/// The thinnest scaling makes the selection, in view points.
const MIN_SIZE: f32 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragKind {
    Move,
    Scale(Handle),
    Rotate,
}

/// A snapping guide to draw: a vertical or horizontal line in view space.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Guide {
    pub vertical: bool,
    pub at: f32,
    pub from: f32,
    pub to: f32,
}

/// A drag of the selection in progress.
#[derive(Clone, Debug)]
pub struct Drag {
    pub kind: DragKind,
    pub frame: PageFrame,
    /// Where the press was.
    pub start: (f32, f32),
    /// The selection's frame when the drag started.
    pub rect: ViewRect,
    /// Frames of the other objects on the page, to snap to.
    pub others: Vec<ViewRect>,
}

/// What a drag does at the pointer's current position.
#[derive(Clone, Debug, PartialEq)]
pub struct DragResult {
    /// The page-space matrix to apply.
    pub matrix: Matrix,
    /// The same in view space (for previews).
    pub view: Matrix,
    pub guides: Vec<Guide>,
    /// What changes, for the status line: "Move 12, −4", "120 × 80", "15°".
    pub label: String,
}

impl Drag {
    pub fn new(kind: DragKind, frame: PageFrame, start: (f32, f32), rect: ViewRect) -> Self {
        Drag {
            kind,
            frame,
            start,
            rect,
            others: Vec::new(),
        }
    }

    /// The transform for the pointer at `now`. `shift` keeps proportions when scaling and
    /// rotates in 15° steps; `snap` is the snapping distance (0 for none).
    pub fn update(&self, now: (f32, f32), shift: bool, snap: f32) -> DragResult {
        let (view, guides, label) = match self.kind {
            DragKind::Move => self.moved(now, shift, snap),
            DragKind::Scale(h) => self.scaled(h, now, shift),
            DragKind::Rotate => self.rotated(now, shift),
        };
        DragResult {
            matrix: to_page(&self.frame, &view),
            view,
            guides,
            label,
        }
    }

    fn moved(&self, now: (f32, f32), shift: bool, snap: f32) -> (Matrix, Vec<Guide>, String) {
        let (mut dx, mut dy) = (now.0 - self.start.0, now.1 - self.start.1);
        // Shift keeps the move horizontal or vertical.
        if shift {
            if dx.abs() > dy.abs() {
                dy = 0.0;
            } else {
                dx = 0.0;
            }
        }
        let mut guides = Vec::new();
        if snap > 0.0 {
            let [x, y, w, h] = self.rect;
            let (pw, ph) = self.frame.size();
            let mut xs = vec![0.0, pw / 2.0, pw];
            let mut ys = vec![0.0, ph / 2.0, ph];
            for o in &self.others {
                xs.extend([o[0], o[0] + o[2] / 2.0, o[0] + o[2]]);
                ys.extend([o[1], o[1] + o[3] / 2.0, o[1] + o[3]]);
            }
            let mine_x = [x, x + w / 2.0, x + w];
            let mine_y = [y, y + h / 2.0, y + h];
            if let Some((d, at)) = nearest(&mine_x, dx, &xs, snap) {
                dx += d;
                guides.push(Guide {
                    vertical: true,
                    at,
                    from: (y + dy).min(0.0),
                    to: (y + dy + h).max(ph),
                });
            }
            if let Some((d, at)) = nearest(&mine_y, dy, &ys, snap) {
                dy += d;
                guides.push(Guide {
                    vertical: false,
                    at,
                    from: (x + dx).min(0.0),
                    to: (x + dx + w).max(pw),
                });
            }
        }
        (
            Matrix::translate(dx, dy),
            guides,
            tr!("Move {}, {} points", format!("{dx:.0}"), format!("{dy:.0}")),
        )
    }

    fn scaled(&self, h: Handle, now: (f32, f32), shift: bool) -> (Matrix, Vec<Guide>, String) {
        let [x, y, w, hh] = self.rect;
        let (w, hh) = (w.max(0.01), hh.max(0.01));
        let (mut x0, mut y0, mut x1, mut y1) = (x, y, x + w, y + hh);
        let (dx, dy) = (now.0 - self.start.0, now.1 - self.start.1);
        match h {
            Handle::Corner(0) => (x0, y0) = (x0 + dx, y0 + dy),
            Handle::Corner(1) => (x1, y0) = (x1 + dx, y0 + dy),
            Handle::Corner(2) => (x1, y1) = (x1 + dx, y1 + dy),
            Handle::Corner(_) => (x0, y1) = (x0 + dx, y1 + dy),
            Handle::Edge(0) => y0 += dy,
            Handle::Edge(1) => x1 += dx,
            Handle::Edge(2) => y1 += dy,
            Handle::Edge(_) => x0 += dx,
            Handle::Rotate => {}
        }
        // Dragged past the edge that stays, the selection is mirrored.
        let left_moves = matches!(h, Handle::Corner(0 | 3) | Handle::Edge(3));
        let top_moves = matches!(h, Handle::Corner(0 | 1) | Handle::Edge(0));
        let at_least = |d: f32| {
            if d.abs() >= MIN_SIZE {
                d
            } else {
                MIN_SIZE.copysign(d)
            }
        };
        let (mut sx, mut sy) = (at_least(x1 - x0) / w, at_least(y1 - y0) / hh);
        if shift && matches!(h, Handle::Corner(_)) {
            let s = sx.abs().max(sy.abs());
            (sx, sy) = (s.copysign(sx), s.copysign(sy));
        }
        // Scale about the corner or edge that stays put.
        let ax = if left_moves {
            x + w
        } else if matches!(h, Handle::Edge(0 | 2)) {
            x + w / 2.0
        } else {
            x
        };
        let ay = if top_moves {
            y + hh
        } else if matches!(h, Handle::Edge(1 | 3)) {
            y + hh / 2.0
        } else {
            y
        };
        if matches!(h, Handle::Edge(0 | 2)) {
            sx = if shift { sy.abs() } else { 1.0 };
        }
        if matches!(h, Handle::Edge(1 | 3)) {
            sy = if shift { sx.abs() } else { 1.0 };
        }
        let m = pagebara_edit::geom::scale_about(Point::new(ax, ay), sx, sy);
        let (pw, ph) = (
            format!("{:.0}", (w * sx).abs()),
            format!("{:.0}", (hh * sy).abs()),
        );
        let label = match (sx < 0.0, sy < 0.0) {
            (true, true) => tr!("{} by {} points, flipped both ways", pw, ph),
            (true, false) => tr!("{} by {} points, flipped horizontally", pw, ph),
            (false, true) => tr!("{} by {} points, flipped vertically", pw, ph),
            (false, false) => tr!("{} by {} points", pw, ph),
        };
        (m, Vec::new(), label)
    }

    fn rotated(&self, now: (f32, f32), shift: bool) -> (Matrix, Vec<Guide>, String) {
        let [x, y, w, h] = self.rect;
        let c = Point::new(x + w / 2.0, y + h / 2.0);
        let a0 = (self.start.1 - c.y).atan2(self.start.0 - c.x);
        let a1 = (now.1 - c.y).atan2(now.0 - c.x);
        let mut deg = (a1 - a0).to_degrees();
        if deg > 180.0 {
            deg -= 360.0;
        } else if deg < -180.0 {
            deg += 360.0;
        }
        if shift {
            deg = (deg / 15.0).round() * 15.0;
        }
        // View space is y down: a positive angle turns clockwise as seen.
        let m = pagebara_edit::geom::rotate_about(c, deg);
        (m, Vec::new(), tr!("Rotate {}°", format!("{deg:.0}")))
    }
}

/// The smallest correction within `tol` that puts one of `mine` (moved by `d`) on one of
/// `targets`: the correction and the target.
fn nearest(mine: &[f32], d: f32, targets: &[f32], tol: f32) -> Option<(f32, f32)> {
    let mut best: Option<(f32, f32)> = None;
    for m in mine {
        for t in targets {
            let diff = t - (m + d);
            if diff.abs() <= tol && best.is_none_or(|b| diff.abs() < b.0.abs()) {
                best = Some((diff, *t));
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;
    use pagebara_edit::Rect;

    fn frame() -> PageFrame {
        let r = Rect::new(0.0, 0.0, 600.0, 800.0);
        PageFrame {
            media_box: r,
            crop_box: r,
            rotate: 0,
        }
    }

    fn close(a: Point, b: Point) -> bool {
        (a.x - b.x).abs() < 1e-3 && (a.y - b.y).abs() < 1e-3
    }

    #[test]
    fn moving_in_view_space_moves_up_the_page() {
        let d = Drag::new(
            DragKind::Move,
            frame(),
            (100.0, 100.0),
            [90.0, 90.0, 20.0, 20.0],
        );
        let r = d.update((110.0, 80.0), false, 0.0);
        // Up the screen is up the page.
        assert!(close(
            r.matrix.apply(Point::new(0.0, 0.0)),
            Point::new(10.0, 20.0)
        ));
        assert!(r.guides.is_empty());
    }

    #[test]
    fn moves_snap_to_the_page_centre_and_other_objects() {
        let mut d = Drag::new(
            DragKind::Move,
            frame(),
            (0.0, 0.0),
            [100.0, 100.0, 50.0, 50.0],
        );
        d.others.push([400.0, 500.0, 10.0, 10.0]);
        // The centre (125) lands 3 points from the page's middle (300).
        let r = d.update((172.0, 0.0), false, 5.0);
        assert!(close(
            r.view.apply(Point::new(125.0, 0.0)),
            Point::new(300.0, 0.0)
        ));
        assert_eq!(r.guides.len(), 1);
        assert!(r.guides[0].vertical);
        // The top lands near the other object's top.
        let r = d.update((0.0, 398.0), false, 5.0);
        assert!(close(
            r.view.apply(Point::new(0.0, 100.0)),
            Point::new(0.0, 500.0)
        ));
    }

    #[test]
    fn corners_scale_about_the_opposite_corner() {
        let d = Drag::new(
            DragKind::Scale(Handle::Corner(2)),
            frame(),
            (200.0, 150.0),
            [100.0, 100.0, 100.0, 50.0],
        );
        let r = d.update((300.0, 160.0), false, 0.0);
        assert!(close(
            r.view.apply(Point::new(100.0, 100.0)),
            Point::new(100.0, 100.0)
        ));
        assert!(close(
            r.view.apply(Point::new(200.0, 150.0)),
            Point::new(300.0, 160.0)
        ));
        // Shift keeps the proportions: the larger factor wins.
        let r = d.update((300.0, 160.0), true, 0.0);
        assert!(close(
            r.view.apply(Point::new(200.0, 150.0)),
            Point::new(300.0, 200.0)
        ));
        // An edge scales one way only.
        let d = Drag::new(
            DragKind::Scale(Handle::Edge(3)),
            frame(),
            (100.0, 125.0),
            [100.0, 100.0, 100.0, 50.0],
        );
        let r = d.update((50.0, 400.0), false, 0.0);
        assert!(close(
            r.view.apply(Point::new(100.0, 100.0)),
            Point::new(50.0, 100.0)
        ));
        assert!(close(
            r.view.apply(Point::new(200.0, 150.0)),
            Point::new(200.0, 150.0)
        ));
    }

    #[test]
    fn dragging_past_the_opposite_edge_mirrors() {
        let d = Drag::new(
            DragKind::Scale(Handle::Edge(1)),
            frame(),
            (200.0, 125.0),
            [100.0, 100.0, 100.0, 50.0],
        );
        // The right edge dragged 50 points left of the left one: mirrored about it.
        let r = d.update((50.0, 125.0), false, 0.0);
        assert!(close(
            r.view.apply(Point::new(200.0, 150.0)),
            Point::new(50.0, 150.0)
        ));
        assert!(r.view.determinant() < 0.0);
        assert_eq!(r.label, "50 by 50 points, flipped horizontally");
        // Never thinner than a point, on either side.
        let r = d.update((100.0, 125.0), false, 0.0);
        assert!((r.view.a.abs() * 100.0 - 1.0).abs() < 1e-3);
        // A corner past the opposite one with Shift: both ways, proportions kept.
        let d = Drag::new(
            DragKind::Scale(Handle::Corner(2)),
            frame(),
            (200.0, 150.0),
            [100.0, 100.0, 100.0, 50.0],
        );
        let r = d.update((0.0, 90.0), true, 0.0);
        assert!(close(
            r.view.apply(Point::new(200.0, 150.0)),
            Point::new(0.0, 50.0)
        ));
        assert!(r.label.ends_with("flipped both ways"), "{}", r.label);
    }

    #[test]
    fn rotation_turns_about_the_centre_in_steps_with_shift() {
        let d = Drag::new(
            DragKind::Rotate,
            frame(),
            (150.0, 50.0),
            [100.0, 100.0, 100.0, 100.0],
        );
        // From straight above the centre to straight right: a quarter turn clockwise.
        let r = d.update((260.0, 150.0), false, 0.0);
        assert!(close(
            r.view.apply(Point::new(150.0, 150.0)),
            Point::new(150.0, 150.0)
        ));
        assert!(close(
            r.view.apply(Point::new(150.0, 100.0)),
            Point::new(200.0, 150.0)
        ));
        let r = d.update((262.0, 140.0), true, 0.0);
        assert_eq!(r.label, "Rotate 90°");
    }

    #[test]
    fn handles_are_found_corners_first() {
        let r = [0.0, 0.0, 4.0, 4.0];
        assert_eq!(handle_at(r, (0.5, 0.5), 3.0, 20.0), Some(Handle::Corner(0)));
        assert_eq!(handle_at(r, (2.0, -20.0), 3.0, 20.0), Some(Handle::Rotate));
        assert_eq!(handle_at(r, (50.0, 50.0), 3.0, 20.0), None);
    }
}
