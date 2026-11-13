//! The depth of the book, drawn the way the exchange's own Depth tab draws
//! it: how much is on offer between here and a given price.
//!
//! Two staircases that start at the middle and climb outwards. The left one
//! is what buyers have put up, the right one what sellers have. The height
//! at any price is **everything between the middle and that price**, which
//! is the question a person actually has: "if I had to move the price this
//! far, how much would I be buying?"
//!
//! Drawn as one bar per level rather than as a filled outline: a staircase
//! is not a convex shape, and a polygon fill of one is at the mercy of
//! whatever the tessellator decides to do with it.

use egui::{Align2, Color32, FontId, Rect, Stroke, Ui};

use crate::exchange::{Book, Level};
use crate::theme;

/// How far either side of the middle the chart looks, as a share of the
/// price.
///
/// A book fifty levels deep on a quiet market reaches prices nobody will
/// trade at today - the far side of this book sits 50% above the middle.
/// Scaling to those turns everything that matters into a flat line against
/// the axis, so the chart shows a band around the middle and says so.
pub const BAND: f64 = 0.015;

/// The levels within the band, with everything past it left out.
///
/// Falls back to the whole side when the band would leave too little to
/// draw: a thin book is still worth looking at.
pub fn within_band(levels: &[Level], mid: f64) -> &[Level] {
    if mid <= 0.0 {
        return levels;
    }
    let reach = mid * BAND;
    let kept = levels
        .iter()
        .take_while(|level| (level.price.value - mid).abs() <= reach)
        .count();
    if kept >= 2 {
        &levels[..kept]
    } else {
        levels
    }
}

/// A side's staircase: the price at each step and the total available up to
/// it. Returned rather than drawn so it can be tested.
pub fn stairs(levels: &[Level]) -> Vec<(f64, f64)> {
    let mut total = 0.0;
    levels
        .iter()
        .map(|level| {
            total += level.size.value;
            (level.price.value, total)
        })
        .collect()
}

/// Draw both sides. Returns false when there was nothing to draw.
pub fn draw(ui: &Ui, area: Rect, book: &Book) -> bool {
    let middle = match (book.bids.first(), book.asks.first()) {
        (Some(bid), Some(ask)) => (bid.price.value + ask.price.value) / 2.0,
        _ => return false,
    };
    let bids = stairs(within_band(&book.bids, middle));
    let asks = stairs(within_band(&book.asks, middle));
    if bids.len() < 2 || asks.len() < 2 || area.width() < 80.0 || area.height() < 40.0 {
        return false;
    }

    // The price axis spans from the deepest bid to the highest ask, with the
    // middle of the book in the middle of the card.
    let lowest = bids.last().map(|(price, _)| *price).unwrap_or_default();
    let highest = asks.last().map(|(price, _)| *price).unwrap_or_default();
    let mid = (bids[0].0 + asks[0].0) / 2.0;
    let reach = (mid - lowest).max(highest - mid);
    if reach <= 0.0 {
        return false;
    }
    let (left, right) = (mid - reach, mid + reach);

    let tallest = bids
        .last()
        .map(|(_, total)| *total)
        .unwrap_or(0.0)
        .max(asks.last().map(|(_, total)| *total).unwrap_or(0.0));
    if tallest <= 0.0 {
        return false;
    }

    let plot = Rect::from_min_max(
        egui::pos2(area.min.x, area.min.y + 12.0),
        egui::pos2(area.max.x, area.max.y - 12.0),
    );
    let x_of = |price: f64| plot.min.x + ((price - left) / (right - left)) as f32 * plot.width();
    let y_of = |total: f64| plot.max.y - (total / tallest) as f32 * plot.height();

    let painter = ui.painter();

    // The two staircases.
    side(ui, plot, &bids, mid, theme::OK, &x_of, &y_of);
    side(ui, plot, &asks, mid, theme::BAD, &x_of, &y_of);

    // The middle of the book.
    let middle = x_of(mid);
    painter.vline(
        middle,
        egui::Rangef::new(plot.min.y, plot.max.y),
        Stroke::new(1.0, theme::DIM),
    );
    painter.text(
        egui::pos2(middle, area.min.y),
        Align2::CENTER_TOP,
        fmt_price(mid),
        FontId::monospace(9.5),
        theme::MUTED,
    );

    // The prices at the two ends, and how much is stacked up by then.
    painter.text(
        egui::pos2(area.min.x + 2.0, area.min.y),
        Align2::LEFT_TOP,
        fmt_price(lowest),
        FontId::monospace(9.5),
        theme::FAINT,
    );
    painter.text(
        egui::pos2(area.max.x - 2.0, area.min.y),
        Align2::RIGHT_TOP,
        fmt_price(highest),
        FontId::monospace(9.5),
        theme::FAINT,
    );
    painter.text(
        egui::pos2(area.min.x + 2.0, area.max.y),
        Align2::LEFT_BOTTOM,
        format!("สะสมสูงสุด {}", fmt_size(tallest)),
        FontId::monospace(9.5),
        theme::FAINT,
    );
    true
}

/// One staircase, as a bar per level plus the step line over the top.
fn side(
    ui: &Ui,
    plot: Rect,
    steps: &[(f64, f64)],
    mid: f64,
    colour: Color32,
    x_of: &impl Fn(f64) -> f32,
    y_of: &impl Fn(f64) -> f32,
) {
    let painter = ui.painter();
    let mut from = mid;
    let mut line: Vec<egui::Pos2> = vec![egui::pos2(x_of(mid), plot.max.y)];

    for (price, total) in steps {
        let (x0, x1) = (x_of(from), x_of(*price));
        let (left, right) = (x0.min(x1), x0.max(x1));
        let top = y_of(*total);
        painter.rect_filled(
            Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, plot.max.y)),
            0,
            colour.gamma_multiply(0.18),
        );
        line.push(egui::pos2(x0, top));
        line.push(egui::pos2(x1, top));
        from = *price;
    }
    painter.add(egui::Shape::line(line, Stroke::new(1.2, colour)));
}

fn fmt_price(price: f64) -> String {
    if price >= 1000.0 {
        format!("{price:.0}")
    } else if price >= 1.0 {
        format!("{price:.2}")
    } else {
        format!("{price:.4}")
    }
}

fn fmt_size(size: f64) -> String {
    if size >= 100.0 {
        format!("{size:.0}")
    } else {
        format!("{size:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::exchange::read_book;
    use serde_json::json;

    fn book() -> Book {
        read_book(&json!({
            "bids": [
                { "price": "100.0", "size": "1.0" },
                { "price": "99.0", "size": "2.0" },
                { "price": "98.0", "size": "3.0" }
            ],
            "asks": [
                { "price": "101.0", "size": "0.5" },
                { "price": "102.0", "size": "1.5" }
            ]
        }))
    }

    #[test]
    fn each_step_holds_everything_between_the_middle_and_that_price() {
        let book = book();
        assert_eq!(
            stairs(&book.bids),
            vec![(100.0, 1.0), (99.0, 3.0), (98.0, 6.0)]
        );
        assert_eq!(stairs(&book.asks), vec![(101.0, 0.5), (102.0, 2.0)]);
    }

    #[test]
    fn a_side_with_nothing_on_it_is_an_empty_staircase_not_a_zero_step() {
        assert!(stairs(&[]).is_empty());
    }

    #[test]
    fn the_staircase_only_ever_climbs() {
        let totals: Vec<f64> = stairs(&book().bids).into_iter().map(|(_, t)| t).collect();
        for pair in totals.windows(2) {
            assert!(pair[1] >= pair[0], "ความลึกสะสมลดลงไม่ได้");
        }
    }

    #[test]
    fn levels_far_from_the_middle_are_left_out_so_the_rest_can_be_seen() {
        let book = read_book(&json!({
            "bids": [
                { "price": "68000.0", "size": "1.0" },
                { "price": "67900.0", "size": "2.0" },
                { "price": "50000.0", "size": "9.0" }
            ],
            "asks": [
                { "price": "68010.0", "size": "1.0" },
                { "price": "68100.0", "size": "1.0" },
                { "price": "103750.0", "size": "5.0" }
            ]
        }));
        let mid = 68_005.0;
        assert_eq!(within_band(&book.bids, mid).len(), 2, "ไม้ที่ -26% ต้องไม่ถูกนับ");
        assert_eq!(within_band(&book.asks, mid).len(), 2);
    }

    #[test]
    fn a_book_that_is_all_far_away_is_still_drawn_rather_than_blanked() {
        let book = read_book(&json!({
            "bids": [
                { "price": "50000.0", "size": "1.0" },
                { "price": "40000.0", "size": "1.0" }
            ],
            "asks": [{ "price": "90000.0", "size": "1.0" }]
        }));
        // Nothing is inside the band, so the band is dropped instead of
        // leaving an empty card.
        assert_eq!(within_band(&book.bids, 68_000.0).len(), 2);
    }

    #[test]
    fn prices_are_printed_with_the_decimals_their_size_calls_for() {
        assert_eq!(fmt_price(68_594.0), "68594");
        assert_eq!(fmt_price(99.44), "99.44");
        assert_eq!(fmt_price(0.123456), "0.1235");
        // Rust rounds a tie to the even number, so 1234.5 prints as 1234.
        // Nothing here depends on which way a tie goes, but a test that
        // claimed otherwise would be wrong about the language.
        assert_eq!(fmt_size(1234.6), "1235");
        assert_eq!(fmt_size(1234.5), "1234");
        assert_eq!(fmt_size(2.448), "2.45");
    }
}
