//! The wires between cards, and the dots that travel along them.
//!
//! Same language as the web dashboard in `../dashboard/`: a bezier from the
//! edge of one card to the edge of the next, an arrow head where it lands,
//! and a dashed grey line when nothing is happening.
//!
//! **A wire only comes alive while data is really walking down it.** The
//! dots are not decoration: an animated wire means that stage produced
//! something and the next one took it. A board where everything always
//! glowed would be a screensaver, and a person could not tell a working bot
//! from a stuck one by glancing at it - which is the entire job of this
//! screen.

use egui::{Color32, Painter, Pos2, Rect, Stroke};

use crate::theme;

/// How fast a dot travels, in scene pixels per second, and how far apart
/// the dots are spaced along the wire.
///
/// Speed is per pixel rather than per wire: a short hop between two stages
/// and a long wire across the board have to look like the same material
/// moving at the same rate, or the picture says something about distance
/// that is not true.
const PIXELS_PER_SECOND: f32 = 85.0;
const PIXELS_PER_DOT: f32 = 55.0;
const MAX_DOTS: usize = 5;

/// Where a wire leaves one card and where it arrives, plus the two control
/// points that bend it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ends {
    pub from: Pos2,
    pub c1: Pos2,
    pub c2: Pos2,
    pub to: Pos2,
    /// True when the wire runs side to side rather than up and down.
    pub horizontal: bool,
}

/// Pick the two edges that face each other.
///
/// The wire leaves the side of the card that actually points at the other
/// card, so it never crosses the card it came from.
pub fn ends(a: Rect, b: Rect) -> Ends {
    let dx = b.center().x - a.center().x;
    let dy = b.center().y - a.center().y;
    let horizontal = dy.abs() < dx.abs();

    if horizontal {
        let left_to_right = b.min.x >= a.min.x;
        let (x1, x2) = if left_to_right {
            (a.max.x, b.min.x)
        } else {
            (a.min.x, b.max.x)
        };
        let (y1, y2) = (a.center().y, b.center().y);
        let mid = (x1 + x2) / 2.0;
        Ends {
            from: egui::pos2(x1, y1),
            c1: egui::pos2(mid, y1),
            c2: egui::pos2(mid, y2),
            to: egui::pos2(x2, y2),
            horizontal,
        }
    } else {
        let top_to_bottom = b.min.y >= a.min.y;
        let (y1, y2) = if top_to_bottom {
            (a.max.y, b.min.y)
        } else {
            (a.min.y, b.max.y)
        };
        let (x1, x2) = (a.center().x, b.center().x);
        let mid = (y1 + y2) / 2.0;
        Ends {
            from: egui::pos2(x1, y1),
            c1: egui::pos2(x1, mid),
            c2: egui::pos2(x2, mid),
            to: egui::pos2(x2, y2),
            horizontal,
        }
    }
}

/// A point on the wire. `t` runs 0 at the source to 1 at the target.
pub fn point_at(ends: &Ends, t: f32) -> Pos2 {
    let t = t.clamp(0.0, 1.0);
    let u = 1.0 - t;
    let (a, b, c, d) = (ends.from, ends.c1, ends.c2, ends.to);
    let x = u * u * u * a.x + 3.0 * u * u * t * b.x + 3.0 * u * t * t * c.x + t * t * t * d.x;
    let y = u * u * u * a.y + 3.0 * u * u * t * b.y + 3.0 * u * t * t * c.y + t * t * t * d.y;
    egui::pos2(x, y)
}

/// Where each dot sits right now, as positions along a wire `length` pixels
/// long.
///
/// Time is passed in rather than read from a clock: there is no
/// `Instant::now` in a browser, and the whole board already runs on egui's
/// frame time.
pub fn dots(now: f64, length: f32) -> Vec<f32> {
    // A 26-pixel hop between two touching cards cannot hold three dots: they
    // land on top of each other and the wire looks like it has a bead stuck
    // to it rather than something travelling down it.
    let count = ((length / PIXELS_PER_DOT).round() as usize).clamp(1, MAX_DOTS);
    let trip = (length / PIXELS_PER_SECOND).max(0.45) as f64;
    (0..count)
        .map(|index| {
            let offset = index as f64 / count as f64;
            // `rem_euclid` rather than `fract`, so the dots keep marching
            // forwards instead of jumping at a negative frame time.
            ((now / trip + offset).rem_euclid(1.0)) as f32
        })
        .collect()
}

/// How long the wire is, measured along the curve it actually draws.
fn length_of(line: &[Pos2]) -> f32 {
    line.windows(2).map(|pair| pair[0].distance(pair[1])).sum()
}

/// Draw one wire. `flowing` is what turns the dots on.
pub fn draw(painter: &Painter, a: Rect, b: Rect, flowing: bool, now: f64) {
    let ends = ends(a, b);
    let line: Vec<Pos2> = (0..=24).map(|i| point_at(&ends, i as f32 / 24.0)).collect();

    let colour = if flowing { theme::ACCENT } else { theme::WIRE };
    if flowing {
        // The line itself stays dim and the dots are the bright thing on it.
        // A wire drawn at full strength swallows its own dots, and then the
        // board only says "this is connected" rather than "this is moving".
        painter.add(egui::Shape::line(
            line.clone(),
            Stroke::new(1.5, colour.gamma_multiply(0.45)),
        ));
    } else {
        dashed(painter, &line, colour);
    }

    arrow_head(painter, &ends, colour);

    if flowing {
        for t in dots(now, length_of(&line)) {
            let at = point_at(&ends, t);
            // A soft halo under the dot, so it reads as something moving
            // rather than a pixel stuck on the line.
            painter.circle_filled(at, 7.0, colour.gamma_multiply(0.20));
            painter.circle_filled(at, 3.2, colour);
        }
    }
}

fn dashed(painter: &Painter, line: &[Pos2], colour: Color32) {
    for pair in line.chunks(2) {
        if let [a, b] = pair {
            painter.line_segment([*a, *b], Stroke::new(1.5, colour.gamma_multiply(0.75)));
        }
    }
}

fn arrow_head(painter: &Painter, ends: &Ends, colour: Color32) {
    let tip = ends.to;
    let before = point_at(ends, 0.93);
    let dir = (tip - before).normalized();
    if !dir.x.is_finite() || !dir.y.is_finite() {
        return;
    }
    let side = egui::vec2(-dir.y, dir.x);
    let back = tip - dir * 9.0;
    painter.add(egui::Shape::convex_polygon(
        vec![tip, back + side * 4.0, back - side * 4.0],
        colour,
        Stroke::NONE,
    ));
}

/// Does data move down this wire right now?
///
/// Ported from the web dashboard so the two screens cannot disagree: the
/// source has to have produced something (`Ok`), and the target has to have
/// done something with it (anything but `Idle`). A refusal counts - being
/// turned away at the gate is still the data arriving there.
pub fn flowing(from: theme::State, to: theme::State) -> bool {
    from == theme::State::Ok && to != theme::State::Idle
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::pos2;

    fn card(x: f32, y: f32) -> Rect {
        Rect::from_min_size(pos2(x, y), egui::vec2(100.0, 60.0))
    }

    #[test]
    fn a_wire_leaves_the_side_that_faces_the_other_card() {
        let left = card(0.0, 0.0);
        let right = card(300.0, 0.0);
        let e = ends(left, right);
        assert!(e.horizontal);
        assert_eq!(e.from.x, 100.0, "ต้องออกจากขอบขวาของการ์ดซ้าย");
        assert_eq!(e.to.x, 300.0, "ต้องเข้าที่ขอบซ้ายของการ์ดขวา");

        // ...and the other way round when the target is to the left.
        let e = ends(right, left);
        assert_eq!(e.from.x, 300.0);
        assert_eq!(e.to.x, 100.0);
    }

    #[test]
    fn a_card_below_another_is_wired_top_to_bottom_not_side_to_side() {
        let top = card(0.0, 0.0);
        let bottom = card(10.0, 400.0);
        let e = ends(top, bottom);
        assert!(!e.horizontal);
        assert_eq!(e.from.y, 60.0, "ออกจากขอบล่าง");
        assert_eq!(e.to.y, 400.0, "เข้าที่ขอบบน");
    }

    #[test]
    fn the_wire_starts_at_one_card_and_finishes_at_the_other() {
        let e = ends(card(0.0, 0.0), card(300.0, 120.0));
        assert_eq!(point_at(&e, 0.0), e.from);
        assert_eq!(point_at(&e, 1.0), e.to);
    }

    #[test]
    fn a_dot_halfway_along_is_somewhere_between_the_two_cards() {
        let a = card(0.0, 0.0);
        let b = card(300.0, 0.0);
        let e = ends(a, b);
        let mid = point_at(&e, 0.5);
        assert!(mid.x > 100.0 && mid.x < 300.0, "จุดกลางต้องอยู่ระหว่างการ์ด");
    }

    #[test]
    fn the_dots_are_spread_out_along_the_wire_and_keep_moving() {
        let first = dots(0.0, 220.0);
        assert_eq!(first.len(), 4, "เส้นยาว 220 px ใส่จุดได้สี่จุด");
        assert!((first[0] - 0.0).abs() < 1e-6);
        assert!((first[1] - 0.25).abs() < 1e-5, "ต้องห่างเท่าๆ กัน");

        let later = dots(0.5, 220.0);
        assert!(later[0] > first[0], "จุดต้องเดินหน้าเมื่อเวลาผ่านไป");

        // One full trip brings them back to where they started.
        let trip = (220.0 / PIXELS_PER_SECOND) as f64;
        let round = dots(trip, 220.0);
        assert!((round[0] - first[0]).abs() < 1e-4);
    }

    #[test]
    fn a_short_hop_between_two_cards_carries_one_dot_not_a_cluster() {
        assert_eq!(dots(0.0, 26.0).len(), 1);
        assert_eq!(dots(0.0, 0.0).len(), 1, "เส้นสั้นมากก็ยังต้องมีจุดเดียว ไม่ใช่ศูนย์");
        assert_eq!(
            dots(0.0, 5_000.0).len(),
            MAX_DOTS,
            "เส้นยาวมากก็ไม่กลายเป็นสายไฟประดับ"
        );
    }

    #[test]
    fn dots_travel_at_the_same_speed_whatever_the_wire_is_like() {
        // Two wires, one twice as long. After the same second, the dot on
        // the long wire has covered the same number of pixels, not the same
        // fraction of the trip.
        let short = dots(1.0, 200.0)[0] * 200.0;
        let long = dots(1.0, 400.0)[0] * 400.0;
        assert!((short - long).abs() < 1.0, "{short} vs {long}");
    }

    #[test]
    fn every_dot_stays_on_the_wire_however_long_the_board_has_been_open() {
        for seconds in [0.0, 1.0, 97.3, 86_400.0] {
            for t in dots(seconds, 180.0) {
                assert!((0.0..=1.0).contains(&t), "t={t} ที่เวลา {seconds}");
            }
        }
    }

    #[test]
    fn a_wires_length_is_measured_along_the_curve_it_draws() {
        let line = [pos2(0.0, 0.0), pos2(3.0, 4.0), pos2(3.0, 14.0)];
        assert!((length_of(&line) - 15.0).abs() < 1e-5);
        assert_eq!(length_of(&[]), 0.0);
    }

    #[test]
    fn a_wire_only_runs_when_data_really_walked_down_it() {
        use theme::State::*;
        assert!(flowing(Ok, Wip));
        assert!(flowing(Ok, Bad), "ถูกด่านปฏิเสธ ก็คือข้อมูลเดินไปถึงด่านแล้ว");
        assert!(!flowing(Ok, Idle), "ปลายทางยังไม่ได้ทำอะไร เส้นต้องไม่วิ่ง");
        assert!(!flowing(Idle, Ok));
        assert!(!flowing(Wip, Ok), "ต้นทางยังไม่เสร็จ");
    }
}
