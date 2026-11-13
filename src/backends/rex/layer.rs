//! The popup's floating layer: its bounds, as window fractions, for a size in
//! cells. Rex reports neither the window's size nor the frame (pane header,
//! padding) it draws around a layer, so the launcher estimates the window in
//! pixels from the pane it starts from, and each popup measures its frame
//! against that estimate for the next one. Pixels, unlike cells, do not depend
//! on the origin pane's font size.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// `[x, y, width, height]` as fractions of the window, as `session.new_layer` takes them.
pub type Bounds = [f64; 4];

/// A width and a height in pixels.
pub type Px = (f64, f64);

/// The window's size in pixels, from the origin block's size in cells, its
/// cells' size in pixels, and its layout rect (`[x, y, width, height]` fractions).
pub fn window_px(origin_cells: (u16, u16), cell: Px, origin_rect: Bounds) -> Px {
    // A missing or degenerate rect means the block is all we know of the window.
    let share = |fraction: f64| {
        if fraction.is_finite() && fraction > 0.0 && fraction <= 1.0 {
            fraction
        } else {
            1.0
        }
    };
    (
        f64::from(origin_cells.0) * cell.0 / share(origin_rect[2]),
        f64::from(origin_cells.1) * cell.1 / share(origin_rect[3]),
    )
}

/// A popup's cell size and what the app's frame takes from its layer, both
/// in pixels against the launcher's window estimate.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Frame {
    pub cell: Px,
    pub frame: Px,
}

const FRAME_FILE: &str = "frame.json";

impl Frame {
    /// Before any popup measured one: cells of `cell` pixels, no frame.
    pub fn unmeasured(cell: Px) -> Self {
        Self {
            cell,
            frame: (0.0, 0.0),
        }
    }

    /// What a popup measured: its layer, `share` (width, height) of a window
    /// estimated at `window` pixels, shows `got` cells of `cell` pixels.
    /// `None` for a measurement that tells nothing.
    pub fn measured(share: (f64, f64), window: Px, got: (u16, u16), cell: Px) -> Option<Self> {
        // A layer shows whole cells: aim at the middle of the last one.
        let axis = |share: f64, window: f64, got: u16, cell: f64| {
            let valid = share > 0.0 && share <= 1.0 && window > 0.0 && got > 0 && cell > 0.0;
            valid.then(|| share * window - (f64::from(got) + 0.5) * cell)
        };
        Some(Self {
            cell,
            frame: (
                axis(share.0, window.0, got.0, cell.0)?,
                axis(share.1, window.1, got.1, cell.1)?,
            ),
        })
    }

    /// The frame to keep after a popup that wanted `want` cells showed `got`
    /// and so `measured` this one; `None` to keep `stored`. Panes estimate the
    /// window a little differently, so a measurement a cell off is averaged in
    /// to settle on a frame that fits them all; anything else replaces it.
    pub fn updated(
        stored: Option<Self>,
        measured: Self,
        want: (u16, u16),
        got: (u16, u16),
    ) -> Option<Self> {
        match stored {
            Some(_) if got == want => None,
            Some(stored) if stored.cell == measured.cell && !needs_resize(want, got) => {
                Some(Self {
                    cell: stored.cell,
                    frame: (
                        (stored.frame.0 + measured.frame.0) / 2.0,
                        (stored.frame.1 + measured.frame.1) / 2.0,
                    ),
                })
            }
            _ => Some(measured),
        }
    }

    /// The frame the popups on this server have measured, if any.
    pub fn load(state_dir: &Path) -> Option<Self> {
        let frame: Self =
            serde_json::from_slice(&fs::read(state_dir.join(FRAME_FILE)).ok()?).ok()?;
        let finite = [frame.cell.0, frame.cell.1, frame.frame.0, frame.frame.1]
            .iter()
            .all(|v| v.is_finite());
        (finite && frame.cell.0 > 0.0 && frame.cell.1 > 0.0).then_some(frame)
    }

    /// Keeps this frame in `state_dir`, replacing the file whole for concurrent readers.
    pub fn save(&self, state_dir: &Path) -> Result<()> {
        let staged = state_dir.join(format!("{FRAME_FILE}.{}", std::process::id()));
        fs::write(&staged, serde_json::to_vec(self)?)?;
        fs::rename(&staged, state_dir.join(FRAME_FILE))?;
        Ok(())
    }
}

/// One axis of the window, as a fraction, clamped and centred.
fn axis(fraction: f64) -> (f64, f64) {
    let size = if fraction.is_finite() {
        fraction.clamp(0.0, 1.0)
    } else {
        1.0
    };
    ((1.0 - size) / 2.0, size)
}

/// Bounds that show `want` cells (columns, rows) inside `frame`, centred and
/// clamped to a window of `window` pixels.
pub fn centered(want: (u16, u16), window: Px, frame: Frame) -> Bounds {
    let fraction = |want: u16, cell: f64, frame: f64, window: f64| {
        let px = (f64::from(want) + 0.5) * cell + frame;
        if window > 0.0 && px > 0.0 {
            px / window
        } else {
            1.0
        }
    };
    let (x, w) = axis(fraction(want.0, frame.cell.0, frame.frame.0, window.0));
    let (y, h) = axis(fraction(want.1, frame.cell.1, frame.frame.1, window.1));
    [x, y, w, h]
}

/// Whether a popup showing `got` cells is far enough from `want` to resize it:
/// a resize shows, a cell less does not.
pub fn needs_resize(want: (u16, u16), got: (u16, u16)) -> bool {
    want.0.abs_diff(got.0) > 1 || want.1.abs_diff(got.1) > 1
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Bounds, b: Bounds) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-9)
    }

    fn valid(b: Bounds) -> bool {
        b.iter().all(|v| v.is_finite() && (0.0..=1.0).contains(v))
            && b[0] + b[2] <= 1.0 + 1e-9
            && b[1] + b[3] <= 1.0 + 1e-9
            && b[2] > 0.0
            && b[3] > 0.0
    }

    /// The app, as far as a popup can tell: a window of `window` pixels whose
    /// layers lose `frame` pixels and show whole cells of `cell` pixels.
    struct App {
        window: Px,
        frame: Px,
        cell: Px,
    }

    impl App {
        fn cells(&self, bounds: Bounds) -> (u16, u16) {
            let cells = |share: f64, window: f64, frame: f64, cell: f64| {
                ((share * window - frame) / cell).floor() as u16
            };
            (
                cells(bounds[2], self.window.0, self.frame.0, self.cell.0),
                cells(bounds[3], self.window.1, self.frame.1, self.cell.1),
            )
        }
    }

    #[test]
    fn the_window_size_comes_from_the_origin_block_in_pixels() {
        let cell = (17.0, 37.0);
        assert_eq!(
            window_px((170, 46), cell, [0.0, 0.0, 1.0, 1.0]),
            (2890.0, 1702.0)
        );
        assert_eq!(
            window_px((85, 23), cell, [0.5, 0.5, 0.5, 0.5]),
            (2890.0, 1702.0)
        );
    }

    #[test]
    fn panes_with_other_font_sizes_agree_on_the_window() {
        // Two halves of one window: one at a larger font size.
        let small = window_px((85, 46), (17.0, 37.0), [0.5, 0.0, 0.5, 1.0]);
        let large = window_px((76, 40), (19.0, 42.0), [0.0, 0.0, 0.5, 1.0]);
        assert!(
            (small.0 - large.0).abs() / small.0 < 0.01,
            "{small:?} {large:?}"
        );
        assert!(
            (small.1 - large.1).abs() / small.1 < 0.02,
            "{small:?} {large:?}"
        );
    }

    #[test]
    fn popups_are_centred_at_their_cell_size_and_clamped_to_the_window() {
        let frame = Frame::unmeasured((10.0, 20.0));
        let bounds = centered((65, 30), (2000.0, 1000.0), frame);
        assert!(close(bounds, [0.33625, 0.195, 0.3275, 0.61]), "{bounds:?}");
        let bounds = centered((70, 18), (600.0, 400.0), frame);
        assert!(close(bounds, [0.0, 0.0375, 1.0, 0.925]), "{bounds:?}");
    }

    #[test]
    fn degenerate_inputs_never_produce_invalid_bounds() {
        for (cells, cell, rect) in [
            ((0, 0), (17.0, 37.0), [0.0, 0.0, 1.0, 1.0]),
            ((80, 24), (0.0, 0.0), [0.0, 0.0, 1.0, 1.0]),
            ((80, 24), (17.0, 37.0), [0.0, 0.0, 0.0, 0.0]),
            ((80, 24), (17.0, 37.0), [0.0, 0.0, -1.0, f64::NAN]),
        ] {
            let window = window_px(cells, cell, rect);
            let bounds = centered((65, 30), window, Frame::unmeasured(cell));
            assert!(valid(bounds), "{cells:?} {cell:?} {rect:?} -> {bounds:?}");
        }
        let shrunk = Frame {
            cell: (17.0, 37.0),
            frame: (-1e6, -1e6),
        };
        let bounds = centered((65, 30), (2890.0, 1702.0), shrunk);
        assert!(valid(bounds), "{bounds:?}");
    }

    #[test]
    fn a_measured_frame_sizes_the_next_popup_exactly() {
        let app = App {
            window: (2900.0, 1750.0),
            frame: (24.0, 70.0),
            cell: (17.0, 37.0),
        };
        // The launcher's estimate misses the origin pane's own frame.
        let estimate = (2890.0, 1702.0);
        let want = (65, 30);
        let first = centered(want, estimate, Frame::unmeasured(app.cell));
        let got = app.cells(first);
        assert_ne!(got, want, "the unmeasured frame shows");
        let frame = Frame::measured((first[2], first[3]), estimate, got, app.cell).unwrap();
        for want in [(65, 30), (70, 18), (100, 40)] {
            let next = centered(want, estimate, frame);
            assert_eq!(app.cells(next), want);
        }
    }

    #[test]
    fn a_frame_measured_from_one_pane_fits_popups_from_another() {
        let app = App {
            window: (2900.0, 1750.0),
            frame: (24.0, 70.0),
            cell: (17.0, 37.0),
        };
        let from_small_font = (2890.0, 1702.0);
        let from_large_font = (2888.0, 1680.0);
        let want = (65, 30);
        let first = centered(want, from_small_font, Frame::unmeasured(app.cell));
        let frame = Frame::measured(
            (first[2], first[3]),
            from_small_font,
            app.cells(first),
            app.cell,
        )
        .unwrap();
        let got = app.cells(centered(want, from_large_font, frame));
        assert!(!needs_resize(want, got), "{got:?}");
    }

    #[test]
    fn popups_from_panes_of_other_font_sizes_settle_on_a_frame_that_fits_both() {
        // Each pane's estimate misses a different part of a cell, so the frame
        // that fits popups from one pane can leave popups from the other a row off.
        let app = App {
            window: (2900.0, 1755.0),
            frame: (24.0, 70.0),
            cell: (17.0, 37.0),
        };
        let panes = [(2888.0, 1680.0), (2890.0, 1702.0)];
        let want = (65, 30);
        let mut stored: Option<Frame> = None;
        let mut shown = Vec::new();
        for n in 0..12 {
            let estimate = panes[n % 2];
            let frame = stored.unwrap_or(Frame::unmeasured(app.cell));
            let bounds = centered(want, estimate, frame);
            let got = app.cells(bounds);
            shown.push(got);
            let measured =
                Frame::measured((bounds[2], bounds[3]), estimate, got, app.cell).unwrap();
            if let Some(next) = Frame::updated(stored, measured, want, got) {
                stored = Some(next);
            }
        }
        assert!(shown[4..].iter().all(|got| *got == want), "{shown:?}");
    }

    #[test]
    fn a_frame_is_replaced_when_far_off_or_for_another_font_and_averaged_when_a_cell_off() {
        let stored = Frame {
            cell: (17.0, 37.0),
            frame: (20.0, 40.0),
        };
        let measured = Frame {
            cell: (17.0, 37.0),
            frame: (30.0, 60.0),
        };
        let want = (65, 30);
        assert_eq!(
            Frame::updated(Some(stored), measured, want, want),
            None,
            "fits"
        );
        assert_eq!(
            Frame::updated(None, measured, want, want),
            Some(measured),
            "first"
        );
        assert_eq!(
            Frame::updated(Some(stored), measured, want, (65, 31)),
            Some(Frame {
                cell: (17.0, 37.0),
                frame: (25.0, 50.0),
            }),
            "a cell off"
        );
        assert_eq!(
            Frame::updated(Some(stored), measured, want, (65, 33)),
            Some(measured),
            "far off"
        );
        let larger_font = Frame {
            cell: (19.0, 42.0),
            ..measured
        };
        assert_eq!(
            Frame::updated(Some(stored), larger_font, want, (58, 29)),
            Some(larger_font),
            "another font"
        );
    }

    #[test]
    fn nothing_is_measured_from_an_unsized_popup() {
        let window = (2890.0, 1702.0);
        assert_eq!(
            Frame::measured((0.3, 0.6), window, (0, 0), (0.0, 0.0)),
            None
        );
        assert_eq!(
            Frame::measured((0.3, 0.6), (0.0, 0.0), (65, 30), (17.0, 37.0)),
            None
        );
        assert!(Frame::measured((1.0, 0.6), window, (65, 30), (17.0, 37.0)).is_some());
    }

    #[test]
    fn only_a_popup_more_than_a_cell_off_is_resized() {
        assert!(!needs_resize((65, 30), (65, 30)));
        assert!(!needs_resize((65, 30), (64, 31)));
        assert!(needs_resize((65, 30), (63, 30)));
        assert!(needs_resize((65, 30), (65, 32)));
    }

    #[test]
    fn the_frame_persists_in_the_state_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(Frame::load(dir.path()), None, "none yet");
        let frame = Frame {
            cell: (17.0, 37.0),
            frame: (24.0, 70.0),
        };
        frame.save(dir.path()).unwrap();
        assert_eq!(Frame::load(dir.path()), Some(frame));
        std::fs::write(dir.path().join("frame.json"), "garbage").unwrap();
        assert_eq!(Frame::load(dir.path()), None, "unreadable");
        frame.save(dir.path()).unwrap();
        assert_eq!(Frame::load(dir.path()), Some(frame), "replaces garbage");
    }
}
