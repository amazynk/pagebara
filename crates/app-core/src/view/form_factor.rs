//! Phone or tablet layout for a window size in logical pixels: phones get a header, a
//! bottom tool bar and bottom sheets; tablets the desktop-like layout with the pages
//! sidebar and side panels, when there is room for it next to a readable page.

/// The narrowest side of a tablet (Android's "sw600dp").
const TABLET_MIN_SIDE: f64 = 600.0;
/// The width the tablet layout needs: a sidebar and a page at a readable size.
const TABLET_MIN_WIDTH: f64 = 720.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FormFactor {
    Phone,
    Tablet,
}

impl FormFactor {
    pub fn of(width: f64, height: f64) -> FormFactor {
        if width.min(height) >= TABLET_MIN_SIDE && width >= TABLET_MIN_WIDTH {
            FormFactor::Tablet
        } else {
            FormFactor::Phone
        }
    }

    /// Width of the tablet's page sidebar and side panels.
    pub fn side_width(self, width: f64) -> f64 {
        match self {
            FormFactor::Phone => 0.0,
            FormFactor::Tablet => (width * 0.22).clamp(200.0, 300.0),
        }
    }

    /// Thumbnail columns in the pages sheet (phones) or sidebar (tablets).
    pub fn thumb_columns(self, width: f64) -> usize {
        match self {
            FormFactor::Phone => ((width - 16.0) / 130.0).floor().clamp(2.0, 6.0) as usize,
            FormFactor::Tablet => 2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phones_and_tablets() {
        // Phones, upright and on their side.
        assert_eq!(FormFactor::of(390.0, 844.0), FormFactor::Phone);
        assert_eq!(FormFactor::of(915.0, 412.0), FormFactor::Phone);
        // A small tablet upright is too narrow for the sidebar.
        assert_eq!(FormFactor::of(600.0, 960.0), FormFactor::Phone);
        assert_eq!(FormFactor::of(960.0, 600.0), FormFactor::Tablet);
        assert_eq!(FormFactor::of(820.0, 1180.0), FormFactor::Tablet);
        assert_eq!(FormFactor::of(1366.0, 1024.0), FormFactor::Tablet);
    }

    #[test]
    fn side_panels_and_thumbnail_columns() {
        assert_eq!(FormFactor::Phone.side_width(390.0), 0.0);
        assert_eq!(FormFactor::Tablet.side_width(820.0), 200.0);
        assert!((FormFactor::Tablet.side_width(1180.0) - 259.6).abs() < 1e-9);
        assert_eq!(FormFactor::Tablet.side_width(2000.0), 300.0);
        assert_eq!(FormFactor::Phone.thumb_columns(390.0), 2);
        assert_eq!(FormFactor::Phone.thumb_columns(915.0), 6);
        assert_eq!(FormFactor::Tablet.thumb_columns(1180.0), 2);
    }
}
