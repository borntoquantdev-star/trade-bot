//! How much of a chart is on screen, and where that window sits.
//!
//! Both charts on this board draw a slice of a series that is longer than
//! the card: 400 candles, or an equity curve of several hundred points. The
//! window is kept here rather than in the drawing code, because panning and
//! zooming are about what a person wants to look at, not about pixels.
//!
//! Two rules that keep the window honest:
//!
//! * **The newest end is the home position.** A chart of a live market that
//!   opens somewhere in the middle of last week is a chart nobody asked for.
//! * **The window never runs off either end.** Scrolling past the last bar
//!   would show empty space and make a person think data is missing.

/// Fewer bars than this on screen and the chart is a magnifying glass over
/// noise rather than a chart.
pub const MIN_SHOWN: usize = 15;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Zoom {
    /// How many points are on screen. `None` means "all of them", which is
    /// also what a fresh board shows.
    shown: Option<usize>,
    /// How many points are hidden to the right of the window. Zero means the
    /// window is parked at the newest end.
    from_right: usize,
}

impl Zoom {
    /// The slice to draw, as `(start, end)` indices into the series.
    pub fn window(&self, total: usize) -> (usize, usize) {
        if total == 0 {
            return (0, 0);
        }
        let shown = self
            .shown
            .unwrap_or(total)
            .clamp(MIN_SHOWN.min(total), total);
        let from_right = self.from_right.min(total - shown);
        let end = total - from_right;
        (end - shown, end)
    }

    /// True when nothing has been zoomed or panned, so the card can leave the
    /// hint off its header.
    pub fn is_whole(&self) -> bool {
        self.shown.is_none() && self.from_right == 0
    }

    pub fn shown(&self, total: usize) -> usize {
        let (start, end) = self.window(total);
        end - start
    }

    /// Zoom about the right-hand edge of the window. `factor` below 1 shows
    /// fewer points (closer in), above 1 shows more.
    pub fn zoom_by(&mut self, factor: f32, total: usize) {
        if total == 0 || !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let shown = self.shown(total) as f32;
        let wanted = (shown * factor).round() as i64;
        let low = MIN_SHOWN.min(total) as i64;
        let next = wanted.clamp(low, total as i64) as usize;
        self.shown = if next >= total { None } else { Some(next) };
        // Zooming out at the end of the series must not push the window off
        // it; clamping here keeps the newest point in view.
        self.from_right = self.from_right.min(total - self.shown(total));
    }

    /// Slide the window. Positive `points` moves towards older data, which is
    /// what dragging the chart to the right feels like.
    pub fn pan_by(&mut self, points: i32, total: usize) {
        let shown = self.shown(total);
        if total == 0 || shown >= total {
            return;
        }
        let room = (total - shown) as i32;
        let next = self.from_right as i32 + points;
        self.from_right = next.clamp(0, room) as usize;
    }

    /// Back to the whole series, newest end on the right.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_chart_shows_everything_it_has() {
        let zoom = Zoom::default();
        assert_eq!(zoom.window(400), (0, 400));
        assert!(zoom.is_whole());
    }

    #[test]
    fn zooming_in_keeps_the_newest_point_on_screen() {
        let mut zoom = Zoom::default();
        zoom.zoom_by(0.5, 400);
        assert_eq!(zoom.window(400), (200, 400), "ขอบขวาต้องยังเป็นแท่งล่าสุด");
        assert!(!zoom.is_whole());
    }

    #[test]
    fn a_chart_never_zooms_in_past_a_handful_of_points() {
        let mut zoom = Zoom::default();
        for _ in 0..40 {
            zoom.zoom_by(0.5, 400);
        }
        assert_eq!(zoom.shown(400), MIN_SHOWN);
    }

    #[test]
    fn zooming_out_stops_at_the_whole_series_and_forgets_the_window() {
        let mut zoom = Zoom::default();
        zoom.zoom_by(0.25, 400);
        for _ in 0..40 {
            zoom.zoom_by(2.0, 400);
        }
        assert_eq!(zoom.window(400), (0, 400));
        assert!(zoom.is_whole(), "ซูมออกจนสุดแล้ว ต้องกลับไปเป็นทั้งชุด");
    }

    #[test]
    fn panning_stops_at_the_oldest_point_instead_of_leaving_the_series() {
        let mut zoom = Zoom::default();
        zoom.zoom_by(0.25, 400); // 100 points on screen
        zoom.pan_by(10_000, 400);
        assert_eq!(zoom.window(400), (0, 100), "เลื่อนไปซ้ายสุดได้แค่จุดแรก");
    }

    #[test]
    fn panning_stops_at_the_newest_point_too() {
        let mut zoom = Zoom::default();
        zoom.zoom_by(0.25, 400);
        zoom.pan_by(-10_000, 400);
        assert_eq!(zoom.window(400), (300, 400));
    }

    #[test]
    fn a_window_zoomed_out_again_does_not_hang_off_the_end() {
        let mut zoom = Zoom::default();
        zoom.zoom_by(0.1, 400);
        zoom.pan_by(200, 400);
        zoom.zoom_by(8.0, 400);
        let (start, end) = zoom.window(400);
        assert!(end <= 400 && start < end);
        assert_eq!(end - start, zoom.shown(400));
    }

    #[test]
    fn a_series_shorter_than_the_zoom_floor_is_still_drawn_whole() {
        let mut zoom = Zoom::default();
        zoom.zoom_by(0.1, 5);
        assert_eq!(zoom.window(5), (0, 5));
        assert_eq!(zoom.window(0), (0, 0), "ชุดว่างต้องไม่ทำให้ดัชนีติดลบ");
    }

    #[test]
    fn resetting_puts_the_whole_series_back() {
        let mut zoom = Zoom::default();
        zoom.zoom_by(0.2, 400);
        zoom.pan_by(50, 400);
        zoom.reset();
        assert_eq!(zoom.window(400), (0, 400));
    }
}
