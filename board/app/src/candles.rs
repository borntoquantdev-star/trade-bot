//! Candle history: fetching it from the board's own server, and drawing it.
//!
//! The bot downloads candles to `data/candles/...` and the server hands over
//! the tail of that file. Nothing here invents a bar: if the store has a gap,
//! the chart has a gap, because a drawn candle that nobody traded is a lie
//! that looks like data.
//!
//! The exchange's numbers arrive as text and are parsed to `f64` here, at the
//! last possible moment, because from here on they are only pixel positions.

use std::sync::mpsc::{channel, Receiver, Sender};

use egui::{Align2, Color32, FontId, Rect, Stroke, Ui};

use crate::theme;

#[derive(Debug, Clone)]
pub struct Candle {
    /// open_time, exactly as the store wrote it
    pub at: String,
    pub open: f64,
    pub high: f64,
    pub low: f64,
    pub close: f64,
    /// True for a bar the market has not finished yet: the live price has
    /// been folded into it, so it will keep moving.
    pub forming: bool,
}

impl Candle {
    pub fn rising(&self) -> bool {
        self.close >= self.open
    }
}

/// Where the bars on screen came from. Shown on the card, because a student
/// must never have to guess whether they are looking at the history their own
/// bot downloaded or at the exchange's public feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// The candle store the bot downloaded into.
    Store,
    /// GRVT's public kline REST. No credentials, and nothing the bot has
    /// seen - it is the market, not the bot's view of it.
    Live,
}

impl Source {
    pub fn label(self) -> &'static str {
        match self {
            Source::Store => "คลังบอท",
            Source::Live => "ตลาดสด",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Load {
    Idle,
    Loading,
    Ready,
    /// Not an error on screen unless it really is one: "the bot has not
    /// downloaded candles yet" is a sentence, not a failure.
    Missing(String),
    Failed(String),
}

/// How often the bars are fetched again.
///
/// Without this the chart is drawn once and then only the newest bar ever
/// moves, because the live price is folded into it: ten minutes later the
/// board is still showing the bar that closed ten minutes ago, stretched.
/// That is the whole of "why is the chart behind the exchange's own".
const REFRESH: f64 = 10.0;

pub struct History {
    pub rows: Vec<Candle>,
    pub status: Load,
    pub total: usize,
    pub symbol: String,
    pub interval: String,
    pub source: Option<Source>,
    tx: Sender<Result<Fetched, String>>,
    rx: Receiver<Result<Fetched, String>>,
    /// When the request on its way was sent, if there is one. `None` means
    /// nothing is in the air.
    sent_at: Option<f64>,
    /// The frame clock, kept current by `tick`, so the fetches that happen
    /// outside it (the fall-back to the public feed) can be timed too.
    clock: f64,
    limit: usize,
    /// Set once the store has been tried and came back empty, so the public
    /// feed is asked exactly once per instrument rather than every frame.
    fell_back: bool,
    /// When the bars are due to be fetched again, in app seconds.
    due: f64,
}

struct Fetched {
    rows: Vec<Candle>,
    total: usize,
    missing: Option<String>,
    source: Source,
}

impl Default for History {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            rows: Vec::new(),
            status: Load::Idle,
            total: 0,
            symbol: "BTC_USDT_Perp".into(),
            interval: "5m".into(),
            source: None,
            tx,
            rx,
            sent_at: None,
            clock: 0.0,
            limit: 400,
            fell_back: false,
            due: 0.0,
        }
    }
}

impl History {
    /// Ask the server for the newest `limit` candles. Does nothing while a
    /// request is already out, so a repaint at 60fps cannot flood it.
    pub fn request(&mut self, symbol: &str, interval: &str, limit: usize) {
        if crate::api::still_waiting(self.sent_at, self.clock) {
            return;
        }
        if symbol != self.symbol || interval != self.interval {
            // The bars on screen belong to the old contract or the old
            // timeframe. Keeping them while the new ones travel would fold
            // this market's live price into another market's candles.
            self.rows.clear();
            self.source = None;
        }
        self.symbol = symbol.to_owned();
        self.interval = interval.to_owned();
        self.limit = limit;
        self.fell_back = false;
        self.fetch(Source::Store);
    }

    /// Ask the exchange's public feed instead of the store.
    pub fn request_live(&mut self, symbol: &str, interval: &str, limit: usize) {
        if crate::api::still_waiting(self.sent_at, self.clock) {
            return;
        }
        self.symbol = symbol.to_owned();
        self.interval = interval.to_owned();
        self.limit = limit;
        self.fell_back = true;
        self.fetch(Source::Live);
    }

    /// What a refresh at `now` should do, if anything.
    ///
    /// Pure, so the cadence can be tested without a socket: `Some(source)`
    /// means fetch from there, `None` means there is nothing due.
    pub fn due_now(&self, now: f64, symbol: &str, interval: &str) -> Option<Source> {
        // A request that has been out for longer than the board is willing
        // to wait is written off, so a dropped connection costs one refresh
        // rather than every refresh from here on.
        if crate::api::still_waiting(self.sent_at, now) {
            return None;
        }
        if symbol != self.symbol || interval != self.interval {
            // A new contract or timeframe is not a refresh; it is a new
            // question, and it is asked at once.
            return Some(Source::Store);
        }
        if now < self.due {
            return None;
        }
        // Keep asking whichever source actually had the bars, rather than
        // walking through an empty store every ten seconds to get there.
        Some(if self.fell_back {
            Source::Live
        } else {
            Source::Store
        })
    }

    /// Ask again when it is time. Call once per frame with egui's frame
    /// time, the same clock the feed runs on.
    pub fn tick(&mut self, now: f64, symbol: &str, interval: &str, limit: usize) {
        self.clock = now;
        let Some(source) = self.due_now(now, symbol, interval) else {
            return;
        };
        self.due = now + REFRESH;
        if symbol != self.symbol || interval != self.interval {
            self.request(symbol, interval, limit);
            return;
        }
        self.limit = limit;
        self.fetch(source);
    }

    fn fetch(&mut self, source: Source) {
        self.sent_at = Some(self.clock);
        self.status = Load::Loading;
        let symbol = &self.symbol;
        let interval = &self.interval;
        let limit = self.limit;
        let url = match source {
            Source::Store => {
                format!("/api/candles?symbol={symbol}&interval={interval}&limit={limit}")
            }
            Source::Live => format!("/api/kline?symbol={symbol}&interval={interval}&limit={limit}"),
        };
        let tx = self.tx.clone();
        ehttp::fetch(ehttp::Request::get(crate::api::url(&url)), move |result| {
            let _ = tx.send(decode_response(result, source));
        });
    }

    /// Take whatever the server sent back. Call once per frame.
    ///
    /// An empty store is not the end of the road: the public feed is asked
    /// next, so a student with no API key still gets a real chart.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        let mut fall_back = false;
        while let Ok(result) = self.rx.try_recv() {
            self.sent_at = None;
            changed = true;
            match result {
                Ok(fetched) => {
                    if let Some(reason) = fetched.missing {
                        self.rows.clear();
                        if fetched.source == Source::Store && !self.fell_back {
                            fall_back = true;
                        } else {
                            self.source = None;
                            self.status = Load::Missing(reason);
                        }
                    } else {
                        self.total = fetched.total;
                        self.rows = fetched.rows;
                        self.source = Some(fetched.source);
                        self.status = Load::Ready;
                    }
                }
                Err(err) => {
                    self.source = None;
                    self.status = Load::Failed(err);
                }
            }
        }
        if fall_back {
            self.fell_back = true;
            self.fetch(Source::Live);
        }
        changed
    }

    /// The bars to draw, with the live price folded into the newest one.
    ///
    /// The store only gains a bar when the bot downloads again, so without
    /// this the right-hand edge of the chart would sit still while the market
    /// moved - the one thing a live chart must not do.
    pub fn with_live(&self, live: Option<f64>) -> Vec<Candle> {
        let mut rows = self.rows.clone();
        if let (Some(price), Some(last)) = (live, rows.last_mut()) {
            if price.is_finite() {
                last.close = price;
                last.high = last.high.max(price);
                last.low = last.low.min(price);
                last.forming = true;
            }
        }
        rows
    }
}

fn decode_response(
    result: Result<ehttp::Response, String>,
    source: Source,
) -> Result<Fetched, String> {
    let response = result.map_err(|err| format!("ขอข้อมูลไม่สำเร็จ: {err}"))?;
    if !response.ok {
        return Err(format!("เซิร์ฟเวอร์ตอบ {}", response.status));
    }
    let text = response
        .text()
        .ok_or_else(|| "คำตอบไม่ใช่ข้อความ".to_string())?;
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|err| format!("อ่าน JSON ไม่ได้: {err}"))?;

    if value.get("present").and_then(|p| p.as_bool()) != Some(true) {
        let reason = value
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("ไม่มีแท่งเทียน")
            .to_owned();
        return Ok(Fetched {
            rows: Vec::new(),
            total: 0,
            missing: Some(reason),
            source,
        });
    }

    let total = value.get("total").and_then(|t| t.as_u64()).unwrap_or(0) as usize;
    let mut rows = Vec::new();
    if let Some(items) = value.get("rows").and_then(|r| r.as_array()) {
        for item in items {
            // A row that will not parse is dropped, not defaulted to zero:
            // a candle at price 0 would rescale the whole chart.
            let (Some(at), Some(open), Some(high), Some(low), Some(close)) = (
                item.get("t").and_then(|v| v.as_str()),
                num(item.get("o")),
                num(item.get("h")),
                num(item.get("l")),
                num(item.get("c")),
            ) else {
                continue;
            };
            rows.push(Candle {
                at: at.to_owned(),
                open,
                high,
                low,
                close,
                forming: false,
            });
        }
    }
    Ok(Fetched {
        rows,
        total,
        missing: None,
        source,
    })
}

fn num(value: Option<&serde_json::Value>) -> Option<f64> {
    let text = value?.as_str()?;
    let parsed: f64 = text.trim().parse().ok()?;
    parsed.is_finite().then_some(parsed)
}

/// How much of the card's width the price axis takes.
const AXIS: f32 = 62.0;

/// Draw the candles. Returns false when there was nothing to draw.
pub fn draw(ui: &Ui, area: Rect, rows: &[Candle], live: Option<f64>) -> bool {
    if rows.len() < 2 || area.width() < AXIS + 40.0 || area.height() < 40.0 {
        return false;
    }
    let plot = Rect::from_min_max(area.min, egui::pos2(area.max.x - AXIS, area.max.y));

    let mut lo = f64::INFINITY;
    let mut hi = f64::NEG_INFINITY;
    for row in rows {
        lo = lo.min(row.low);
        hi = hi.max(row.high);
    }
    if let Some(price) = live {
        lo = lo.min(price);
        hi = hi.max(price);
    }
    if !lo.is_finite() || !hi.is_finite() {
        return false;
    }
    // A flat market would divide by zero and draw a chart with no height.
    let pad = ((hi - lo) * 0.08).max(hi.abs() * 1e-6).max(f64::EPSILON);
    let (lo, hi) = (lo - pad, hi + pad);
    let span = hi - lo;

    let y_of = |price: f64| plot.max.y - ((price - lo) / span) as f32 * plot.height();
    let step = plot.width() / rows.len() as f32;
    let body = (step * 0.62).clamp(1.0, 14.0);

    let painter = ui.painter();

    // Horizontal guides, drawn first so candles sit on top.
    for i in 0..=3 {
        let price = lo + span * (i as f64 / 3.0);
        let y = y_of(price);
        painter.hline(plot.x_range(), y, Stroke::new(1.0, theme::GRID));
        // Keep the label inside the card: the top and bottom guides sit on
        // the edge, and half a line of text would be clipped away there.
        painter.text(
            egui::pos2(
                area.max.x - 4.0,
                y.clamp(area.min.y + 6.0, area.max.y - 6.0),
            ),
            Align2::RIGHT_CENTER,
            fmt_price(price, span),
            FontId::monospace(9.5),
            theme::FAINT,
        );
    }

    for (i, row) in rows.iter().enumerate() {
        let x = plot.min.x + step * (i as f32 + 0.5);
        // Green and red, never the accent: orange on this board means
        // "system", and a candle is money.
        let colour = if row.rising() { theme::OK } else { theme::BAD };
        let colour = if row.forming {
            colour.gamma_multiply(0.75)
        } else {
            colour
        };

        painter.vline(
            x,
            egui::Rangef::new(y_of(row.high), y_of(row.low)),
            Stroke::new(1.0, colour),
        );

        let top = y_of(row.open.max(row.close));
        let bottom = y_of(row.open.min(row.close));
        let rect = Rect::from_min_max(
            egui::pos2(x - body / 2.0, top),
            // A doji would be a zero-height rectangle, which paints nothing.
            egui::pos2(x + body / 2.0, bottom.max(top + 1.0)),
        );
        painter.rect_filled(rect, 1, colour);
    }

    // The live price: a dashed rule and a tag on the axis. Cream, because a
    // price is not a profit and must not borrow the colour of one.
    if let Some(price) = live {
        let y = y_of(price);
        dashed_hline(ui, plot.x_range(), y, theme::MUTED);
        tag(
            ui,
            egui::pos2(plot.max.x + 2.0, y),
            &fmt_price(price, span),
            theme::CANVAS,
            theme::MUTED,
        );
    }

    // The last close, tagged in its own direction's colour.
    if let Some(last) = rows.last() {
        let y = y_of(last.close);
        let colour = if last.rising() { theme::OK } else { theme::BAD };
        tag(
            ui,
            egui::pos2(plot.max.x + 2.0, y),
            &fmt_price(last.close, span),
            theme::CANVAS,
            colour,
        );
    }
    true
}

fn dashed_hline(ui: &Ui, x: egui::Rangef, y: f32, colour: Color32) {
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

/// Enough decimals to tell two gridlines apart, and no more.
fn fmt_price(price: f64, span: f64) -> String {
    let decimals = if span >= 100.0 {
        1
    } else if span >= 1.0 {
        2
    } else {
        4
    };
    format!("{price:.decimals$}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(open: f64, close: f64) -> Candle {
        Candle {
            at: "2026-01-01T00:00:00+00:00".into(),
            open,
            high: open.max(close) + 1.0,
            low: open.min(close) - 1.0,
            close,
            forming: false,
        }
    }

    #[test]
    fn the_bars_are_asked_for_again_so_the_chart_does_not_fall_behind() {
        let mut history = History::default();
        history.status = Load::Ready;
        // Nothing is due straight after a fetch...
        history.due = 100.0;
        assert_eq!(history.due_now(50.0, "BTC_USDT_Perp", "5m"), None);
        // ...and then it is.
        assert_eq!(
            history.due_now(100.0, "BTC_USDT_Perp", "5m"),
            Some(Source::Store)
        );
    }

    #[test]
    fn a_refresh_goes_back_to_whichever_source_had_the_bars() {
        let mut history = History::default();
        history.due = 0.0;
        history.fell_back = true;
        assert_eq!(
            history.due_now(1.0, "BTC_USDT_Perp", "5m"),
            Some(Source::Live),
            "คลังบอทว่างไปแล้ว ไม่ต้องเดินผ่านมันทุกสิบวินาที"
        );
    }

    #[test]
    fn a_new_timeframe_is_asked_at_once_rather_than_at_the_next_refresh() {
        let mut history = History::default();
        history.due = 1_000.0;
        assert_eq!(
            history.due_now(0.0, "BTC_USDT_Perp", "1h"),
            Some(Source::Store)
        );
        assert_eq!(
            history.due_now(0.0, "ETH_USDT_Perp", "5m"),
            Some(Source::Store)
        );
    }

    #[test]
    fn nothing_is_asked_while_an_answer_is_still_on_its_way() {
        let mut history = History::default();
        history.sent_at = Some(95.0);
        history.due = 0.0;
        assert_eq!(history.due_now(100.0, "BTC_USDT_Perp", "1h"), None);
    }

    #[test]
    fn an_answer_that_never_came_does_not_freeze_the_chart_for_ever() {
        let mut history = History::default();
        history.sent_at = Some(0.0);
        history.due = 0.0;
        assert_eq!(
            history.due_now(crate::api::LOST + 1.0, "BTC_USDT_Perp", "5m"),
            Some(Source::Store),
            "เลยเวลารอแล้ว ต้องถามใหม่"
        );
    }

    #[test]
    fn changing_timeframe_throws_away_the_old_timeframes_bars() {
        let mut history = History::default();
        history.rows = vec![row(10.0, 11.0)];
        history.source = Some(Source::Live);
        history.request("BTC_USDT_Perp", "1h", 400);
        assert!(history.rows.is_empty(), "แท่งของ 5m ต้องไม่ค้างอยู่บนกราฟ 1h");
        assert_eq!(history.source, None);
        assert_eq!(history.interval, "1h");
    }

    #[test]
    fn a_candle_knows_which_way_it_went() {
        assert!(row(10.0, 11.0).rising());
        assert!(!row(11.0, 10.0).rising());
        assert!(row(10.0, 10.0).rising(), "แท่งเท่าทุนนับเป็นขึ้น ไม่ใช่ลง");
    }

    #[test]
    fn the_live_price_is_folded_into_the_newest_bar_only() {
        let mut history = History::default();
        history.rows = vec![row(10.0, 11.0), row(11.0, 12.0)];
        let drawn = history.with_live(Some(15.0));
        assert_eq!(drawn.len(), 2);
        assert_eq!(drawn[0].close, 11.0, "แท่งเก่าต้องไม่ถูกแตะ");
        assert_eq!(drawn[1].close, 15.0);
        assert_eq!(drawn[1].high, 15.0, "ราคาสดที่สูงกว่าต้องดันยอดแท่งขึ้น");
        assert!(drawn[1].forming);
    }

    #[test]
    fn without_a_live_price_the_bars_are_left_exactly_as_stored() {
        let mut history = History::default();
        history.rows = vec![row(10.0, 11.0)];
        let drawn = history.with_live(None);
        assert_eq!(drawn[0].close, 11.0);
        assert!(!drawn[0].forming);
    }

    #[test]
    fn a_response_that_says_the_store_is_empty_is_not_a_failure() {
        let body = r#"{"present":false,"reason":"ยังไม่มีไฟล์"}"#;
        let fetched = decode_response(
            Ok(ehttp::Response {
                url: "/api/candles".into(),
                ok: true,
                status: 200,
                status_text: "OK".into(),
                headers: Default::default(),
                bytes: body.as_bytes().to_vec(),
            }),
            Source::Store,
        )
        .expect("ไม่ควรเป็น Err");
        assert!(fetched.missing.is_some());
        assert!(fetched.rows.is_empty());
    }

    #[test]
    fn rows_arrive_as_text_and_become_numbers_here() {
        let body = r#"{"present":true,"total":2,"rows":[
            {"t":"a","o":"10","h":"12","l":"9","c":"11","v":"0"},
            {"t":"b","o":"11","h":"13","l":"10","c":"12.5","v":"0"}]}"#;
        let fetched = decode_response(
            Ok(ehttp::Response {
                url: "/api/candles".into(),
                ok: true,
                status: 200,
                status_text: "OK".into(),
                headers: Default::default(),
                bytes: body.as_bytes().to_vec(),
            }),
            Source::Store,
        )
        .expect("ควรอ่านได้");
        assert_eq!(fetched.total, 2);
        assert_eq!(fetched.rows.len(), 2);
        assert_eq!(fetched.rows[1].close, 12.5);
    }

    #[test]
    fn a_row_that_will_not_parse_is_dropped_not_turned_into_zero() {
        let body = r#"{"present":true,"total":2,"rows":[
            {"t":"a","o":"10","h":"12","l":"9","c":"11","v":"0"},
            {"t":"b","o":"","h":"13","l":"10","c":"x","v":"0"}]}"#;
        let fetched = decode_response(
            Ok(ehttp::Response {
                url: "/api/candles".into(),
                ok: true,
                status: 200,
                status_text: "OK".into(),
                headers: Default::default(),
                bytes: body.as_bytes().to_vec(),
            }),
            Source::Store,
        )
        .expect("ควรอ่านได้");
        assert_eq!(fetched.rows.len(), 1, "แท่งที่พังต้องหายไป ไม่ใช่กลายเป็นราคา 0");
    }

    #[test]
    fn when_neither_source_has_bars_the_card_is_told_why() {
        // The store came back empty and the public feed was already tried,
        // so there is nothing left to fall back to and the reason must reach
        // the screen rather than leaving a blank card.
        let mut history = History::default();
        history.fell_back = true;
        history
            .tx
            .send(Ok(Fetched {
                rows: Vec::new(),
                total: 0,
                missing: Some("ยังไม่มีไฟล์".into()),
                source: Source::Live,
            }))
            .unwrap();
        assert!(history.poll());
        assert_eq!(history.status, Load::Missing("ยังไม่มีไฟล์".into()));
        assert_eq!(history.source, None);
    }

    #[test]
    fn bars_from_the_public_feed_are_labelled_as_the_market_not_the_bot() {
        let mut history = History::default();
        history
            .tx
            .send(Ok(Fetched {
                rows: vec![row(10.0, 11.0), row(11.0, 12.0)],
                total: 2,
                missing: None,
                source: Source::Live,
            }))
            .unwrap();
        assert!(history.poll());
        assert_eq!(history.source, Some(Source::Live));
        assert_eq!(Source::Live.label(), "ตลาดสด");
        assert_eq!(Source::Store.label(), "คลังบอท");
    }

    #[test]
    fn a_price_is_printed_with_enough_decimals_to_tell_gridlines_apart() {
        assert_eq!(fmt_price(73_280.123, 500.0), "73280.1");
        assert_eq!(fmt_price(12.3456, 4.0), "12.35");
        assert_eq!(fmt_price(0.123456, 0.01), "0.1235");
    }
}
