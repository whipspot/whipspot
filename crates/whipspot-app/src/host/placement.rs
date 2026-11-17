//! Where a saved window may reopen (MD90). The OS places the window when its saved corner would
//! leave it off every monitor, for example after the monitor it was on was unplugged.

/// How far right of the saved corner the probe point lies (the title bar's grab area).
const PROBE_DX: i64 = 48;
/// How far below the saved corner the probe point lies.
const PROBE_DY: i64 = 16;

/// Whether a window whose top-left corner is `corner` (physical px) reopens on a monitor: the
/// probe point, 48 px right of and 16 px below the corner, lies inside one of `monitors`, given as
/// `(x, y, width, height)` in physical px. Left and top are inclusive, right and bottom exclusive.
pub(crate) fn visible(corner: (i32, i32), monitors: &[(i32, i32, u32, u32)]) -> bool {
    let px = i64::from(corner.0) + PROBE_DX;
    let py = i64::from(corner.1) + PROBE_DY;
    monitors.iter().any(|&(x, y, width, height)| {
        let (x, y) = (i64::from(x), i64::from(y));
        px >= x && py >= y && px < x + i64::from(width) && py < y + i64::from(height)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The primary monitor at the origin, 1920 × 1080.
    const PRIMARY: (i32, i32, u32, u32) = (0, 0, 1920, 1080);
    /// A second monitor left of the primary, with negative x.
    const LEFT: (i32, i32, u32, u32) = (-2560, -200, 2560, 1440);

    #[test]
    fn p1_a_corner_inside_the_primary_is_visible() {
        assert!(visible((100, 100), &[PRIMARY]));
    }

    #[test]
    fn p2_a_corner_on_a_monitor_left_of_the_primary_is_visible() {
        assert!(visible((-2000, 0), &[PRIMARY, LEFT]));
    }

    #[test]
    fn p3_a_corner_right_of_every_monitor_is_not_visible() {
        assert!(!visible((1900, 100), &[PRIMARY]));
        assert!(!visible((1920 - PROBE_DX as i32, 0), &[PRIMARY]));
    }

    #[test]
    fn p4_no_monitors_means_not_visible() {
        assert!(!visible((0, 0), &[]));
    }

    #[test]
    fn p5_the_minimised_marker_is_not_visible() {
        assert!(!visible((-32000, -32000), &[PRIMARY, LEFT]));
    }

    #[test]
    fn p6_the_largest_corner_is_refused_without_overflow() {
        assert!(!visible((i32::MAX, i32::MAX), &[PRIMARY, LEFT]));
        assert!(!visible((i32::MAX, 0), &[PRIMARY, LEFT]));
    }
}
