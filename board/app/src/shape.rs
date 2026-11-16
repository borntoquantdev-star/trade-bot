//! What the strategy looks like: the price, the lines the strategy compares
//! it with, and the moments it acted - drawn tick by tick from the audit
//! trail.
//!
//! Every line here is one the strategy itself reported (`view` on the row,
//! from `Strategy.chart`). The board does not work out an average of its own
//! and call it the strategy's: a picture that disagreed with the bot by one
//! tick would teach the wrong rule.

use egui::{Align2, Color32, FontId, Rect, Stroke, Ui};

use crate::bot::Row;
use crate::theme;

/// How many ticks the card draws. At one tick every few seconds this is a
/// few minutes - long enough to hold a trade or two from start to finish.
pub const SHOWN: usize = 180;

/// Something the bot did on a tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    /// Sent an order that opened a position.
    Open { buy: bool },
    /// Closed the position it held.
    Close { buy: bool },
    /// The strategy wanted in and the risk gate said no.
    Refused,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Tick {
    pub at: String,
    pub price: f64,
    pub average: Option<f64>,
    pub low: Option<f64>,
    pub high: Option<f64>,
    pub stop: Option<f64>,
    pub act: Option<Act>,
}

/// A silence this long between two ticks means they belong to different
/// runs, when the run log cannot say so itself.
const RUN_GAP: f64 = 300.0;

/// The newest `last` ticks of the bot's own contract, oldest first - and of
/// the newest run only.
///
/// `since` is when that run started, from the run log. Without it, a long
/// silence is taken as the boundary. Either way, ticks from an earlier run
/// are left out: a paper run at a made-up 100,000 drawn beside a testnet run
/// at 64,000 squashes the run being watched into a flat line (seen on the
/// board, 16 Nov).
pub fn ticks(rows: &[Row], last: usize, since: Option<f64>) -> Vec<Tick> {
    let Some(symbol) = rows.last().map(|row| row.symbol.clone()) else {
        return Vec::new();
    };
    let mut newer: Option<f64> = None;
    let mut out: Vec<Tick> = rows
        .iter()
        .rev()
        .filter(|row| row.symbol == symbol)
        .take_while(|row| {
            let Some(at) = crate::time::unix_of(&row.at) else {
                return true;
            };
            if since.is_some_and(|start| at < start - 1.0) {
                return false;
            }
            let close = newer.is_none_or(|next| next - at <= RUN_GAP);
            newer = Some(at);
            since.is_some() || close
        })
        .filter_map(|row| {
            Some(Tick {
                at: row.at.clone(),
                price: row.price_value()?,
                average: row.level("average"),
                low: row.level("buy_below"),
                high: row.level("sell_above"),
                stop: row.level("stop"),
                act: act_of(row),
            })
        })
        .take(last)
        .collect();
    out.reverse();
    out
}

fn act_of(row: &Row) -> Option<Act> {
    if let (Some(order), Some(_)) = (&row.order, &row.receipt) {
        return Some(Act::Open {
            buy: order.side == "buy",
        });
    }
    if let Some(exit) = &row.exit {
        if !exit.broker_order_id.is_empty() || exit.simulated {
            let held_long = row.position.as_ref().is_some_and(|p| p.side == "buy");
            return Some(Act::Close { buy: !held_long });
        }
    }
    if row.signal.is_some() && row.reject.is_some() {
        return Some(Act::Refused);
    }
    None
}

/// The strategy's settings, as it reported them on the newest row.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Settings {
    pub window: Option<f64>,
    pub edge_pct: Option<f64>,
    pub stop_pct: Option<f64>,
    pub max_hold_s: Option<f64>,
}

pub fn settings(rows: &[Row]) -> Settings {
    let Some(row) = rows.iter().rev().find(|row| row.view.is_some()) else {
        return Settings::default();
    };
    Settings {
        window: row.level("window"),
        edge_pct: row.level("edge_pct"),
        stop_pct: row.level("stop_pct"),
        max_hold_s: row.level("max_hold_s"),
    }
}

/// Did this strategy draw a band - is it the kind of picture this card
/// knows how to explain?
pub fn has_band(ticks: &[Tick]) -> bool {
    ticks
        .iter()
        .any(|t| t.average.is_some() && t.low.is_some() && t.high.is_some())
}

/// What the strategy is waiting for right now, in one sentence.
///
/// The question a student actually has while watching: "why has it not
/// bought yet?" The answer is a distance, so the sentence is one.
pub fn waiting_for(row: &Row) -> Option<String> {
    let price = row.price_value()?;
    let average = row.level("average")?;
    let low = row.level("buy_below")?;
    let high = row.level("sell_above")?;
    let away = |target: f64| (target - price) / price * 100.0;
    match &row.position {
        None => Some(format!(
            "ราคาห่างเส้นกลาง {:+.3}% · ซื้อเมื่อลงถึง {} (อีก {:+.3}%) · ขายเมื่อขึ้นถึง {} (อีก {:+.3}%)",
            (price - average) / average * 100.0,
            fmt(low),
            away(low),
            fmt(high),
            away(high),
        )),
        Some(held) => {
            let long = held.side == "buy";
            let mut text = format!(
                "ถือ{}อยู่ · ปิดเมื่อราคากลับถึงเส้นกลาง {} (อีก {:+.3}%)",
                if long { "ซื้อ" } else { "ขาย" },
                fmt(average),
                away(average),
            );
            if let Some(stop) = row.level("stop") {
                text.push_str(&format!(
                    " · หรือโดน stop {} (อีก {:+.3}%)",
                    fmt(stop),
                    away(stop)
                ));
            }
            if let (Some(held_s), Some(max)) = (row.level("held_s"), row.level("max_hold_s")) {
                text.push_str(&format!(" · หรือถือครบ {max:.0} วิ (ผ่านไป {held_s:.0})"));
            }
            Some(text)
        }
    }
}

/// How long without a tick before the card stops claiming the bot is
/// running, when the run log does not say it ended. Long enough for a slow
/// loop (the default is 60 s), short enough that a dead bot is noticed.
pub const QUIET_AFTER: f64 = 150.0;

/// Why the picture is not moving, or `None` while the bot is still running.
///
/// The picture is the bot's trail, so it stops when the bot stops - while
/// the candles beside it keep moving, because they come from the market.
/// Without saying so, a finished run looked like a frozen card (asked about
/// on 16 Nov, after a run that had simply reached `--iterations`).
pub fn stopped(ended_at: Option<&str>, newest_at: &str, now: Option<f64>) -> Option<String> {
    if let Some(ended) = ended_at.filter(|ended| !ended.trim().is_empty()) {
        return Some(format!(
            "บอทหยุดแล้ว · รอบนี้จบตอน {} · ภาพจึงไม่ขยับ",
            crate::time::clock(ended)
        ));
    }
    let quiet = now? - crate::time::unix_of(newest_at)?;
    (quiet > QUIET_AFTER).then(|| {
        format!(
            "ไม่มี tick ใหม่มา {:.0} นาที (ล่าสุด {}) · บอทน่าจะหยุดไปแล้ว",
            (quiet / 60.0).floor().max(1.0),
            crate::time::clock(newest_at)
        )
    })
}

/// A price with as many decimals as its size deserves.
pub fn fmt(price: f64) -> String {
    if price.abs() >= 1000.0 {
        format!("{price:.1}")
    } else if price.abs() >= 1.0 {
        format!("{price:.2}")
    } else {
        format!("{price:.5}")
    }
}

const AXIS: f32 = 60.0;

/// Draw it. Returns false when there is too little to draw.
pub fn draw(ui: &Ui, area: Rect, ticks: &[Tick], focus: Option<&str>) -> bool {
    if ticks.len() < 2 || area.width() < AXIS + 60.0 || area.height() < 50.0 {
        return false;
    }
    let plot = Rect::from_min_max(area.min, egui::pos2(area.max.x - AXIS, area.max.y));

    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for t in ticks {
        // The stop is left out of the frame on purpose: it sits ten times
        // further away than the band, and a frame that reached it squashed
        // the band - the thing this card exists to show - flat. A stop off
        // the frame gets a tag on the edge instead (see below).
        for value in [Some(t.price), t.average, t.low, t.high]
            .into_iter()
            .flatten()
        {
            lo = lo.min(value);
            hi = hi.max(value);
        }
    }
    if !lo.is_finite() || !hi.is_finite() {
        return false;
    }
    let pad = ((hi - lo) * 0.1).max(hi.abs() * 1e-6).max(f64::EPSILON);
    let (lo, hi) = (lo - pad, hi + pad);
    let span = hi - lo;
    let y_of = |price: f64| plot.max.y - ((price - lo) / span) as f32 * plot.height();
    let step = plot.width() / (ticks.len() - 1) as f32;
    let x_of = |index: usize| plot.min.x + step * index as f32;
    let painter = ui.painter();

    for i in 0..=3 {
        let price = lo + span * (i as f64 / 3.0);
        let y = y_of(price);
        painter.hline(plot.x_range(), y, Stroke::new(1.0, theme::GRID));
        painter.text(
            egui::pos2(
                area.max.x - 4.0,
                y.clamp(area.min.y + 6.0, area.max.y - 6.0),
            ),
            Align2::RIGHT_CENTER,
            fmt(price),
            FontId::monospace(9.5),
            theme::FAINT,
        );
    }

    // The band: the zone where the strategy does nothing. Filled, because
    // "inside the band" is the idea a student has to see at a glance.
    let band = theme::INFO.gamma_multiply(0.14);
    for i in 0..ticks.len() - 1 {
        let (a, b) = (&ticks[i], &ticks[i + 1]);
        let (Some(a_low), Some(a_high), Some(b_low), Some(b_high)) = (a.low, a.high, b.low, b.high)
        else {
            continue;
        };
        painter.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(x_of(i), y_of(a_high)),
                egui::pos2(x_of(i + 1), y_of(b_high)),
                egui::pos2(x_of(i + 1), y_of(b_low)),
                egui::pos2(x_of(i), y_of(a_low)),
            ],
            band,
            Stroke::NONE,
        ));
    }
    let edge = Stroke::new(1.0, theme::INFO.gamma_multiply(0.45));
    line(painter, ticks, |t| t.high, &x_of, &y_of, edge);
    line(painter, ticks, |t| t.low, &x_of, &y_of, edge);
    line(
        painter,
        ticks,
        |t| t.average,
        &x_of,
        &y_of,
        Stroke::new(1.5, theme::INFO),
    );
    let inside = |price: f64| (lo..=hi).contains(&price);
    line(
        painter,
        ticks,
        |t| t.stop.filter(|stop| inside(*stop)),
        &x_of,
        &y_of,
        Stroke::new(1.0, theme::BAD),
    );
    if let Some(stop) = ticks
        .last()
        .and_then(|t| t.stop)
        .filter(|stop| !inside(*stop))
    {
        let above = stop > hi;
        let y = if above {
            plot.min.y + 7.0
        } else {
            plot.max.y - 7.0
        };
        painter.text(
            egui::pos2(plot.max.x - 4.0, y),
            Align2::RIGHT_CENTER,
            format!(
                "stop {} {}กรอบ",
                fmt(stop),
                if above {
                    "เหนือ"
                } else {
                    "ใต้"
                }
            ),
            FontId::proportional(10.0),
            theme::BAD,
        );
    }
    line(
        painter,
        ticks,
        |t| Some(t.price),
        &x_of,
        &y_of,
        Stroke::new(1.6, theme::TEXT),
    );

    if let Some(at) = focus {
        if let Some(index) = ticks.iter().position(|t| t.at == at) {
            let x = x_of(index);
            painter.vline(
                x,
                plot.y_range(),
                Stroke::new(1.0, theme::ACCENT.gamma_multiply(0.7)),
            );
        }
    }

    for (index, t) in ticks.iter().enumerate() {
        let Some(act) = t.act else { continue };
        let x = x_of(index);
        let y = y_of(t.price);
        match act {
            Act::Open { buy } | Act::Close { buy } => {
                let opening = matches!(act, Act::Open { .. });
                let colour = if buy { theme::OK } else { theme::BAD };
                triangle(painter, x, y, buy, colour);
                let word = match (opening, buy) {
                    (true, true) => "ซื้อ",
                    (true, false) => "ขาย",
                    (false, _) => "ปิด",
                };
                let below = buy;
                painter.text(
                    egui::pos2(x, if below { y + 13.0 } else { y - 13.0 }),
                    if below {
                        Align2::CENTER_TOP
                    } else {
                        Align2::CENTER_BOTTOM
                    },
                    word,
                    FontId::proportional(10.0),
                    colour,
                );
            }
            Act::Refused => {
                painter.circle_stroke(egui::pos2(x, y), 3.5, Stroke::new(1.2, theme::WARN));
            }
        }
    }
    true
}

fn line(
    painter: &egui::Painter,
    ticks: &[Tick],
    pick: impl Fn(&Tick) -> Option<f64>,
    x_of: &impl Fn(usize) -> f32,
    y_of: &impl Fn(f64) -> f32,
    stroke: Stroke,
) {
    // A level that is missing on some ticks (no stop while flat) breaks the
    // line there instead of joining across the gap.
    let mut run: Vec<egui::Pos2> = Vec::new();
    for (index, t) in ticks.iter().enumerate() {
        match pick(t) {
            Some(value) => run.push(egui::pos2(x_of(index), y_of(value))),
            None => {
                if run.len() >= 2 {
                    painter.add(egui::Shape::line(std::mem::take(&mut run), stroke));
                }
                run.clear();
            }
        }
    }
    if run.len() >= 2 {
        painter.add(egui::Shape::line(run, stroke));
    }
}

fn triangle(painter: &egui::Painter, x: f32, y: f32, up: bool, colour: Color32) {
    let size = 5.5;
    let points = if up {
        vec![
            egui::pos2(x, y),
            egui::pos2(x + size, y + size * 1.6),
            egui::pos2(x - size, y + size * 1.6),
        ]
    } else {
        vec![
            egui::pos2(x, y),
            egui::pos2(x - size, y - size * 1.6),
            egui::pos2(x + size, y - size * 1.6),
        ]
    };
    painter.add(egui::Shape::convex_polygon(
        points,
        colour,
        Stroke::new(1.0, theme::CANVAS),
    ));
}

/// A swatch and its words, for the legend: a short stroke of the line's own
/// colour, so the legend needs no symbol the font might not have.
pub fn legend(ui: &mut Ui, colour: Color32, filled: bool, words: &str) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(16.0, 10.0), egui::Sense::hover());
    if filled {
        ui.painter()
            .rect_filled(rect.shrink2(egui::vec2(0.0, 1.0)), 1, colour);
    } else {
        ui.painter()
            .hline(rect.x_range(), rect.center().y, Stroke::new(2.0, colour));
    }
    ui.label(egui::RichText::new(words).size(10.5).color(theme::MUTED));
    ui.add_space(6.0);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(json: &str) -> Row {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn each_tick_carries_the_lines_the_strategy_reported() {
        let rows = vec![
            row(r#"{"at":"a","symbol":"BTC","price":"100"}"#),
            row(r#"{"at":"b","symbol":"BTC","price":"99.9",
                "view":{"average":"100","buy_below":"99.97","sell_above":"100.03"},
                "signal":{"side":"buy","stop_price":"99.5"},
                "order":{"side":"buy","limit_price":"99.95"},"receipt":{"broker_order_id":"0x1"}}"#),
        ];
        let ticks = ticks(&rows, 10, None);
        assert_eq!(ticks.len(), 2);
        assert_eq!(ticks[0].average, None, "แถวเก่าไม่มีเส้น ไม่เดาให้");
        assert_eq!(ticks[1].low, Some(99.97));
        assert_eq!(ticks[1].act, Some(Act::Open { buy: true }));
        assert!(has_band(&ticks));
    }

    #[test]
    fn only_the_newest_ticks_of_the_bot_s_own_contract_are_kept() {
        let rows = vec![
            row(r#"{"at":"a","symbol":"BTC","price":"1"}"#),
            row(r#"{"at":"b","symbol":"ETH","price":"2"}"#),
            row(r#"{"at":"c","symbol":"BTC","price":"3"}"#),
            row(r#"{"at":"d","symbol":"BTC","price":"4"}"#),
        ];
        let ticks = ticks(&rows, 2, None);
        let at: Vec<&str> = ticks.iter().map(|t| t.at.as_str()).collect();
        assert_eq!(at, ["c", "d"]);
    }

    #[test]
    fn a_close_is_drawn_on_the_side_that_closes() {
        let rows = vec![row(r#"{"at":"a","symbol":"BTC","price":"100",
                "position":{"side":"sell","qty":"1","entry_price":"101"},
                "exit":{"reason":"back","broker_order_id":"0x00"}}"#)];
        assert_eq!(
            ticks(&rows, 5, None)[0].act,
            Some(Act::Close { buy: true }),
            "ปิด short คือซื้อ"
        );
    }

    #[test]
    fn a_refused_signal_is_marked_too() {
        let rows = vec![row(
            r#"{"at":"a","symbol":"BTC","price":"100","signal":{"side":"buy"},
                "reject":{"code":"already_in_position"}}"#,
        )];
        assert_eq!(ticks(&rows, 5, None)[0].act, Some(Act::Refused));
    }

    fn at(seconds: &str) -> String {
        format!(r#"{{"at":"2026-11-16T05:{seconds}+00:00","symbol":"BTC","price":"1"}}"#)
    }

    #[test]
    fn an_earlier_run_is_not_drawn_beside_this_one() {
        let rows: Vec<Row> = ["00:00", "00:05", "06:00", "06:05", "06:10"]
            .iter()
            .map(|s| row(&at(s)))
            .collect();
        // No run log: the six-minute silence is the boundary.
        assert_eq!(ticks(&rows, 50, None).len(), 3);
        // The run log knows better: this run started at 06:05.
        let start = crate::time::unix_of("2026-11-16T05:06:05+00:00");
        assert_eq!(ticks(&rows, 50, start).len(), 2);
    }

    #[test]
    fn a_run_that_ended_says_so_with_the_time_it_ended() {
        let text = stopped(
            Some("2026-11-16T05:18:07+00:00"),
            "2026-11-16T05:18:07+00:00",
            None,
        )
        .unwrap();
        assert!(text.contains("บอทหยุดแล้ว"), "{text}");
        assert!(text.contains("12:18:07"), "เวลาไทย: {text}");
    }

    #[test]
    fn a_running_bot_with_fresh_ticks_is_not_called_stopped() {
        let newest = "2026-11-16T05:18:07+00:00";
        let now = crate::time::unix_of(newest).unwrap() + 30.0;
        assert_eq!(stopped(None, newest, Some(now)), None);
        assert_eq!(
            stopped(Some(""), newest, Some(now)),
            None,
            "ยังไม่มีเวลาจบ = ยังรัน"
        );
    }

    #[test]
    fn a_bot_that_went_quiet_without_ending_is_suspected_stopped() {
        let newest = "2026-11-16T05:18:07+00:00";
        let now = crate::time::unix_of(newest).unwrap() + 600.0;
        let text = stopped(None, newest, Some(now)).unwrap();
        assert!(text.contains("10 นาที"), "{text}");
    }

    #[test]
    fn flat_it_says_how_far_the_price_has_to_go_either_way() {
        let r = row(r#"{"at":"a","symbol":"BTC","price":"100",
                "view":{"average":"100.01","buy_below":"99.98","sell_above":"100.04"}}"#);
        let text = waiting_for(&r).unwrap();
        assert!(text.contains("ซื้อเมื่อลงถึง 99.98 (อีก -0.020%)"), "{text}");
        assert!(text.contains("ขายเมื่อขึ้นถึง 100.04 (อีก +0.040%)"), "{text}");
    }

    #[test]
    fn holding_it_says_the_three_ways_out() {
        let r = row(
            r#"{"at":"a","symbol":"BTC","price":"100","position":{"side":"buy","qty":"1","entry_price":"99.9"},
                "view":{"average":"100.05","buy_below":"100.02","sell_above":"100.08",
                        "stop":"99.5","held_s":"45","max_hold_s":"180"}}"#,
        );
        let text = waiting_for(&r).unwrap();
        assert!(text.starts_with("ถือซื้ออยู่"), "{text}");
        assert!(text.contains("เส้นกลาง 100.05"), "{text}");
        assert!(text.contains("stop 99.50"), "{text}");
        assert!(text.contains("ถือครบ 180 วิ (ผ่านไป 45)"), "{text}");
    }

    #[test]
    fn a_strategy_that_reports_nothing_has_no_sentence() {
        let r = row(r#"{"at":"a","symbol":"BTC","price":"100"}"#);
        assert_eq!(waiting_for(&r), None);
    }

    #[test]
    fn the_settings_come_from_the_newest_row_that_had_any() {
        let rows = vec![
            row(
                r#"{"at":"a","symbol":"BTC","price":"1","view":{"window":"12","edge_pct":"0.03","stop_pct":"0.4","max_hold_s":"180"}}"#,
            ),
            row(r#"{"at":"b","symbol":"BTC","price":"1"}"#),
        ];
        let s = settings(&rows);
        assert_eq!(s.window, Some(12.0));
        assert_eq!(s.edge_pct, Some(0.03));
        assert_eq!(s.max_hold_s, Some(180.0));
    }
}
