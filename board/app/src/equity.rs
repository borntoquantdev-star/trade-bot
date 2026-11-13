//! The equity curve of a backtest, and the shading that keeps it honest.
//!
//! A rising line is the easiest picture in finance to fall in love with, so
//! this draws two more things on top of it:
//!
//! * **every stretch where the account was below its own previous peak** is
//!   shaded. That is the part a person would have had to sit through, and a
//!   plain line hides it completely.
//! * **the starting equity**, as a dashed rule. Without it, a chart of a
//!   losing run and a chart of a winning one look the same.
//!
//! The x axis is the position in the run, not the clock: the server hands
//! over equal-sized buckets of the curve, so one step across is one step of
//! the run. Nothing here invents a point the run did not produce.

use egui::{Align2, Color32, FontId, Rect, Stroke, Ui};

use crate::bot::Point;
use crate::theme;

/// How much of the card's width the equity axis takes. Same as the candle
/// chart, so two cards side by side line up.
const AXIS: f32 = 62.0;

/// The stretches where the account was under water, as index ranges into the
/// curve. Each is `(from, to)` inclusive.
///
/// "Under water" means below the highest equity seen *so far*, which is the
/// only definition that matches what it felt like at the time - the peak at
/// the end of the run had not happened yet.
pub fn underwater(points: &[Point]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut peak = f64::NEG_INFINITY;
    let mut open: Option<usize> = None;

    for (i, point) in points.iter().enumerate() {
        // Back at the old peak counts as recovered, not as still under it:
        // the account is whole again, even if it gained nothing.
        if point.equity >= peak {
            peak = point.equity;
            if let Some(from) = open.take() {
                runs.push((from, i.saturating_sub(1)));
            }
            continue;
        }
        if open.is_none() {
            // The dip is shaded from the peak it fell from, so the stretch on
            // screen starts where the account was last whole.
            open = Some(i.saturating_sub(1));
        }
    }
    if let Some(from) = open {
        runs.push((from, points.len() - 1));
    }
    runs
}

/// Draw the curve. Returns false when there was nothing to draw.
pub fn draw(ui: &Ui, area: Rect, points: &[Point], start_equity: f64) -> bool {
    if points.len() < 2 || area.width() < AXIS + 40.0 || area.height() < 30.0 {
        return false;
    }
    let plot = Rect::from_min_max(area.min, egui::pos2(area.max.x - AXIS, area.max.y));

    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for point in points {
        lo = lo.min(point.equity);
        hi = hi.max(point.equity);
    }
    // The line the account started from belongs on the chart whether or not
    // the run ever came back to it.
    if start_equity.is_finite() && start_equity > 0.0 {
        lo = lo.min(start_equity);
        hi = hi.max(start_equity);
    }
    if !lo.is_finite() || !hi.is_finite() {
        return false;
    }
    let pad = ((hi - lo) * 0.08).max(hi.abs() * 1e-6).max(f64::EPSILON);
    let (lo, hi) = (lo - pad, hi + pad);
    let span = hi - lo;

    let x_of = |i: usize| plot.min.x + (i as f32 / (points.len() - 1) as f32) * plot.width();
    let y_of = |equity: f64| plot.max.y - ((equity - lo) / span) as f32 * plot.height();

    let painter = ui.painter();

    // Under water first, so the line is drawn over its own shading.
    for (from, to) in underwater(points) {
        let rect = Rect::from_min_max(
            egui::pos2(x_of(from), plot.min.y),
            egui::pos2(x_of(to).max(x_of(from) + 1.0), plot.max.y),
        );
        painter.rect_filled(rect, 0, theme::BAD.gamma_multiply(0.10));
    }

    // Where the account began.
    if start_equity.is_finite() && start_equity > 0.0 {
        let y = y_of(start_equity);
        dashed(ui, plot.x_range(), y, theme::DIM);
    }

    // The curve. Green or red by where it ended up against where it began:
    // money, so never the accent colour.
    let last = points[points.len() - 1].equity;
    let colour = if last >= start_equity {
        theme::OK
    } else {
        theme::BAD
    };
    let line: Vec<egui::Pos2> = points
        .iter()
        .enumerate()
        .map(|(i, point)| egui::pos2(x_of(i), y_of(point.equity)))
        .collect();
    painter.add(egui::Shape::line(line, Stroke::new(1.5, colour)));

    // Axis: the top and bottom of the run, and where it ended.
    for value in [hi - pad, lo + pad] {
        let y = y_of(value);
        painter.text(
            egui::pos2(
                area.max.x - 4.0,
                y.clamp(area.min.y + 6.0, area.max.y - 6.0),
            ),
            Align2::RIGHT_CENTER,
            fmt(value),
            FontId::monospace(9.5),
            theme::FAINT,
        );
    }
    tag(
        ui,
        egui::pos2(plot.max.x + 2.0, y_of(last)),
        &fmt(last),
        theme::CANVAS,
        colour,
    );
    true
}

fn dashed(ui: &Ui, x: egui::Rangef, y: f32, colour: Color32) {
    let painter = ui.painter();
    let mut x0 = x.min;
    while x0 < x.max {
        let x1 = (x0 + 4.0).min(x.max);
        painter.hline(egui::Rangef::new(x0, x1), y, Stroke::new(1.0, colour));
        x0 += 8.0;
    }
}

fn tag(ui: &Ui, at: egui::Pos2, text: &str, ink: Color32, fill: Color32) {
    let painter = ui.painter();
    let galley = painter.layout_no_wrap(text.to_owned(), FontId::monospace(9.5), ink);
    let rect = Rect::from_min_size(
        egui::pos2(at.x, at.y - galley.size().y / 2.0 - 2.0),
        galley.size() + egui::vec2(8.0, 4.0),
    );
    painter.rect_filled(rect, 3, fill);
    painter.galley(rect.min + egui::vec2(4.0, 2.0), galley, ink);
}

/// Equity is money, and money on an axis wants no decimals once it is in the
/// thousands.
fn fmt(value: f64) -> String {
    if value.abs() >= 1_000.0 {
        format!("{value:.0}")
    } else if value.abs() >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.3}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn curve(values: &[f64]) -> Vec<Point> {
        values
            .iter()
            .enumerate()
            .map(|(i, v)| Point {
                at: format!("t{i}"),
                equity: *v,
            })
            .collect()
    }

    #[test]
    fn a_run_that_only_ever_rose_was_never_under_water() {
        assert!(underwater(&curve(&[100.0, 101.0, 102.0])).is_empty());
    }

    #[test]
    fn a_dip_is_shaded_from_the_peak_it_fell_from_until_it_is_recovered() {
        // peak at index 1, below it at 2 and 3, back above at 4
        let runs = underwater(&curve(&[100.0, 110.0, 105.0, 90.0, 120.0]));
        assert_eq!(runs, vec![(1, 3)]);
    }

    #[test]
    fn a_dip_that_never_recovers_is_shaded_to_the_end_of_the_run() {
        let runs = underwater(&curve(&[100.0, 110.0, 90.0, 95.0]));
        assert_eq!(runs, vec![(1, 3)]);
    }

    #[test]
    fn two_separate_dips_are_two_separate_shaded_stretches() {
        let runs = underwater(&curve(&[100.0, 90.0, 100.0, 110.0, 95.0, 115.0]));
        assert_eq!(runs, vec![(0, 1), (3, 4)]);
    }

    #[test]
    fn sitting_exactly_at_the_peak_is_not_under_water() {
        assert!(underwater(&curve(&[100.0, 100.0, 100.0])).is_empty());
    }

    #[test]
    fn an_axis_label_keeps_the_decimals_that_matter_at_its_own_size() {
        assert_eq!(fmt(10_123.456), "10123");
        assert_eq!(fmt(99.44), "99.4");
        assert_eq!(fmt(1.5), "1.500");
    }
}
