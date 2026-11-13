//! The market, in the same detail the exchange's own screen shows it.
//!
//! Five things, all public and all read-only, fetched through the board's
//! server (`/api/market/<kind>`): the order book, the trades that just
//! happened, the ticker with its 24-hour numbers and funding, the funding
//! that has actually been charged, and the contract's own specification.
//!
//! Two rules carried over from the rest of the board:
//!
//! 1. **The exchange's numbers travel as the exchange wrote them.** Every
//!    value here keeps its original text for printing and gains an `f64`
//!    only so a bar can be drawn to scale. `68594.0` is printed as
//!    `68594.0`, not as `68594` and not as `68593.99999`.
//! 2. **An answer about another instrument is dropped.** The board switches
//!    coins while requests are in the air; a book for BTC under SOL's
//!    heading is the same lie the mini-ticker feed used to tell.

use std::sync::mpsc::{channel, Receiver, Sender};

use serde_json::Value;

pub use crate::candles::Load;

/// How often each thing is asked for again, in seconds. The book and the
/// trades move constantly; a contract's tick size does not.
const EVERY: [f64; 5] = [1.0, 1.5, 2.0, 60.0, 300.0];

/// How deep the book is fetched. The exchange only accepts 10, 50, 100, 500.
///
/// The whole book, not a slice of it: the buy/sell bar under the card is a
/// number about the **whole** book - that is what the exchange's own screen
/// shows - and a 20 BTC order parked a hundred dollars away belongs in it.
/// On this market that is about 120 levels, a few kilobytes.
const DEPTH: u32 = 500;
/// How many trades and funding rows are asked for.
const TRADES: u32 = 40;
const FUNDINGS: u32 = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Book,
    Trades,
    Ticker,
    Funding,
    Spec,
}

impl Kind {
    const ALL: [Kind; 5] = [
        Kind::Book,
        Kind::Trades,
        Kind::Ticker,
        Kind::Funding,
        Kind::Spec,
    ];

    fn index(self) -> usize {
        match self {
            Kind::Book => 0,
            Kind::Trades => 1,
            Kind::Ticker => 2,
            Kind::Funding => 3,
            Kind::Spec => 4,
        }
    }

    fn route(self, symbol: &str) -> String {
        match self {
            Kind::Book => format!("/api/market/book?symbol={symbol}&depth={DEPTH}"),
            Kind::Trades => format!("/api/market/trade?symbol={symbol}&limit={TRADES}"),
            Kind::Ticker => format!("/api/market/ticker?symbol={symbol}"),
            Kind::Funding => format!("/api/market/funding?symbol={symbol}&limit={FUNDINGS}"),
            Kind::Spec => format!("/api/market/instrument?symbol={symbol}"),
        }
    }
}

// ------------------------------------------------------------------ numbers

/// One number as the exchange sent it, plus the same number as a float.
///
/// The text is what goes on screen; the float is only ever used to decide
/// how long a bar is or where a point sits.
#[derive(Debug, Clone, PartialEq)]
pub struct Num {
    pub text: String,
    pub value: f64,
}

impl Num {
    pub fn from(value: Option<&Value>) -> Option<Self> {
        let text = match value? {
            Value::String(text) => text.clone(),
            Value::Number(number) => number.to_string(),
            _ => return None,
        };
        let parsed: f64 = text.trim().parse().ok()?;
        parsed.is_finite().then_some(Self {
            text,
            value: parsed,
        })
    }

    /// A percentage, for the handful of places that show one.
    pub fn percent(&self) -> String {
        format!("{:.4}%", self.value)
    }
}

fn text_of(value: Option<&Value>) -> String {
    value
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn uint_of(value: Option<&Value>) -> u64 {
    value
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
        .unwrap_or(0)
}

/// The clock part of the ISO timestamp the server adds next to each
/// nanosecond field.
pub fn clock(at: &str) -> String {
    crate::bot::clock(at)
}

// --------------------------------------------------------------- the things

#[derive(Debug, Clone, PartialEq)]
pub struct Level {
    pub price: Num,
    pub size: Num,
    pub orders: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Book {
    /// Best bid first.
    pub bids: Vec<Level>,
    /// Best ask first.
    pub asks: Vec<Level>,
    pub at: String,
}

impl Book {
    /// The gap between the best bid and the best ask, and that gap as a
    /// share of the mid price - which is what the exchange's own screen
    /// prints under the book.
    pub fn spread(&self) -> Option<(f64, f64)> {
        let bid = self.bids.first()?.price.value;
        let ask = self.asks.first()?.price.value;
        let mid = (bid + ask) / 2.0;
        (mid > 0.0).then(|| (ask - bid, (ask - bid) / mid * 100.0))
    }

    /// Running totals down each side, the way a book is read: how much is
    /// available up to and including this level.
    pub fn totals(levels: &[Level]) -> Vec<f64> {
        let mut sum = 0.0;
        levels
            .iter()
            .map(|level| {
                sum += level.size.value;
                sum
            })
            .collect()
    }

    /// How the whole book splits between buyers and sellers, as percentages.
    ///
    /// **Every level, not the handful on screen.** Measured against GRVT's
    /// own screen on 13 Nov 2026: it printed `Buy 51% / 49% Sell` while the
    /// whole book gave 52% and the nine rows it was showing gave 64%. The
    /// rows are what a person can add up; the whole book is what the
    /// exchange means by the number, and matching the exchange is the
    /// point of having it.
    pub fn pressure(&self) -> Option<(f64, f64)> {
        let bids: f64 = self.bids.iter().map(|level| level.size.value).sum();
        let asks: f64 = self.asks.iter().map(|level| level.size.value).sum();
        pressure(bids, asks)
    }

    /// The largest running total on either side, for scaling the bars behind
    /// the rows. Both sides share one scale, or a thin side would look as
    /// deep as a thick one.
    pub fn deepest(&self, rows: usize) -> f64 {
        // The running total of the last row shown, read from the back
        // rather than by walking to it.
        let bid = Self::totals(&self.bids).into_iter().take(rows).next_back();
        let ask = Self::totals(&self.asks).into_iter().take(rows).next_back();
        bid.unwrap_or(0.0)
            .max(ask.unwrap_or(0.0))
            .max(f64::MIN_POSITIVE)
    }
}

#[derive(Debug, Clone)]
pub struct Trade {
    pub at: String,
    pub price: Num,
    pub size: Num,
    /// True when the buyer was the one who crossed the spread, which is what
    /// makes a trade "green" on every exchange screen.
    pub taker_buy: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Ticker {
    pub mark: Option<Num>,
    pub index: Option<Num>,
    pub last: Option<Num>,
    pub mid: Option<Num>,
    pub high_24h: Option<Num>,
    pub low_24h: Option<Num>,
    pub open_24h: Option<Num>,
    pub buy_volume_24h: Option<Num>,
    pub sell_volume_24h: Option<Num>,
    pub open_interest: Option<Num>,
    pub long_short: Option<Num>,
    pub funding_now: Option<Num>,
    pub funding_avg: Option<Num>,
    pub next_funding_at: String,
    pub status: String,
    pub at: String,
    /// The exchange's own clock on this reading, and when it will charge
    /// funding next - both in nanoseconds, both from the same answer.
    ///
    /// The countdown is worked out from these two and never from the
    /// machine's clock: a board open on a laptop whose time is twenty
    /// minutes out would otherwise count down to the wrong minute.
    pub event_nanos: Option<f64>,
    pub next_funding_nanos: Option<f64>,
}

impl Ticker {
    /// Change since the session opened, in percent. The exchange's own
    /// header shows this next to the last price.
    pub fn change_pct(&self) -> Option<f64> {
        let open = self.open_24h.as_ref()?.value;
        let last = self.last.as_ref()?.value;
        (open > 0.0).then(|| (last - open) / open * 100.0)
    }

    /// Both sides of the 24-hour volume added up, in the base coin.
    pub fn volume_24h(&self) -> Option<f64> {
        let buy = self.buy_volume_24h.as_ref()?.value;
        let sell = self.sell_volume_24h.as_ref()?.value;
        Some(buy + sell)
    }

    /// Seconds until funding is charged again.
    pub fn until_funding(&self) -> Option<f64> {
        let now = self.event_nanos?;
        let next = self.next_funding_nanos?;
        let seconds = (next - now) / 1e9;
        (seconds >= 0.0).then_some(seconds)
    }
}

/// The 8-hour rate as a year, the way the exchange's own funding tab shows
/// it next to the rate.
///
/// The rate is already a percentage - the exchange sends `0.01` for the
/// `+0.0100%` on its screen - so this only multiplies by how many times a
/// year it is charged.
pub fn annualised(rate_pct: f64, interval_hours: u64) -> Option<f64> {
    let hours = if interval_hours == 0 {
        8
    } else {
        interval_hours
    };
    let per_day = 24.0 / hours as f64;
    Some(rate_pct * per_day * 365.0)
}

/// How the depth on screen splits between the two sides, as percentages
/// that add to 100.
///
/// **Measured over the levels actually drawn, not the whole book.** A book
/// fetched fifty deep reaches prices nobody will trade at today - one order
/// parked 50% away would swing this number without saying anything about
/// the pressure at the touch. Over the visible rows it is a number a person
/// can check by adding up the column in front of them.
pub fn pressure(bids: f64, asks: f64) -> Option<(f64, f64)> {
    let total = bids + asks;
    if !total.is_finite() || total <= 0.0 || bids < 0.0 || asks < 0.0 {
        return None;
    }
    Some((bids / total * 100.0, asks / total * 100.0))
}

/// Who pays whom. Positive funding means the longs pay the shorts.
pub fn who_pays(rate_pct: f64) -> &'static str {
    if rate_pct > 0.0 {
        "ฝั่ง long จ่ายให้ short"
    } else if rate_pct < 0.0 {
        "ฝั่ง short จ่ายให้ long"
    } else {
        "ไม่มีใครจ่ายใคร"
    }
}

#[derive(Debug, Clone)]
pub struct Funding {
    pub at: String,
    pub rate: Option<Num>,
    pub avg_8h: Option<Num>,
    pub mark: Option<Num>,
    pub interval_hours: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Spec {
    pub instrument: String,
    pub base: String,
    pub quote: String,
    pub tick_size: Option<Num>,
    pub min_size: Option<Num>,
    pub min_notional: Option<Num>,
    pub max_position: Option<Num>,
    pub funding_interval_hours: u64,
    pub funding_cap: Option<Num>,
    pub funding_floor: Option<Num>,
    pub status: String,
    pub kind: String,
}

// ------------------------------------------------------------- the fetching

enum Got {
    Book(Kind, Answer<Book>),
    Trades(Kind, Answer<Vec<Trade>>),
    Ticker(Kind, Answer<Box<Ticker>>),
    Funding(Kind, Answer<Vec<Funding>>),
    Spec(Kind, Answer<Box<Spec>>),
}

enum Answer<T> {
    /// The thing, and which instrument it was about.
    Have(String, T),
    Missing(String),
    Failed(String),
}

pub struct Exchange {
    pub symbol: String,
    pub book: Option<Book>,
    pub trades: Vec<Trade>,
    pub ticker: Option<Ticker>,
    pub funding: Vec<Funding>,
    pub spec: Option<Spec>,
    pub load: [Load; 5],
    /// Answers that arrived about an instrument nobody is looking at.
    pub ignored: u64,
    tx: Sender<Got>,
    rx: Receiver<Got>,
    due: [f64; 5],
    /// When each request went out. A request that never comes back is
    /// written off after a while rather than blocking its card for good.
    flight: [Option<f64>; 5],
}

impl Default for Exchange {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            symbol: "BTC_USDT_Perp".into(),
            book: None,
            trades: Vec::new(),
            ticker: None,
            funding: Vec::new(),
            spec: None,
            load: [Load::Idle, Load::Idle, Load::Idle, Load::Idle, Load::Idle],
            ignored: 0,
            tx,
            rx,
            due: [0.0; 5],
            flight: [None; 5],
        }
    }
}

impl Exchange {
    /// Look at a different contract. Everything on screen belonged to the
    /// old one, so it goes.
    pub fn watch(&mut self, symbol: &str) {
        if symbol == self.symbol {
            return;
        }
        self.symbol = symbol.to_owned();
        self.book = None;
        self.trades.clear();
        self.ticker = None;
        self.funding.clear();
        self.spec = None;
        self.load = [Load::Idle, Load::Idle, Load::Idle, Load::Idle, Load::Idle];
        // Anything already in the air is about the old contract; asking
        // again immediately is what makes the new one appear.
        self.due = [0.0; 5];
    }

    pub fn poll(&mut self, now: f64) {
        for kind in Kind::ALL {
            let index = kind.index();
            if crate::api::still_waiting(self.flight[index], now) || now < self.due[index] {
                continue;
            }
            self.flight[index] = Some(now);
            self.due[index] = now + EVERY[index];
            if self.load[index] == Load::Idle {
                self.load[index] = Load::Loading;
            }
            self.fetch(kind);
        }

        while let Ok(got) = self.rx.try_recv() {
            match got {
                Got::Book(kind, answer) => self.take(kind, answer, |me, book| me.book = Some(book)),
                Got::Trades(kind, answer) => self.take(kind, answer, |me, rows| me.trades = rows),
                Got::Ticker(kind, answer) => {
                    self.take(kind, answer, |me, ticker| me.ticker = Some(*ticker))
                }
                Got::Funding(kind, answer) => self.take(kind, answer, |me, rows| me.funding = rows),
                Got::Spec(kind, answer) => {
                    self.take(kind, answer, |me, spec| me.spec = Some(*spec))
                }
            }
        }
    }

    fn take<T>(&mut self, kind: Kind, answer: Answer<T>, keep: impl FnOnce(&mut Self, T)) {
        let index = kind.index();
        self.flight[index] = None;
        match answer {
            Answer::Have(instrument, thing) => {
                // The guard that stops SOL's book appearing under BTC.
                if instrument != self.symbol && !instrument.is_empty() {
                    self.ignored += 1;
                    return;
                }
                keep(self, thing);
                self.load[index] = Load::Ready;
            }
            Answer::Missing(reason) => self.load[index] = Load::Missing(reason),
            Answer::Failed(reason) => self.load[index] = Load::Failed(reason),
        }
    }

    pub fn load_of(&self, kind: Kind) -> &Load {
        &self.load[kind.index()]
    }

    fn fetch(&self, kind: Kind) {
        let tx = self.tx.clone();
        let url = crate::api::url(&kind.route(&self.symbol));
        let symbol = self.symbol.clone();
        ehttp::fetch(ehttp::Request::get(url), move |result| {
            let _ = tx.send(decode(kind, &symbol, result));
        });
    }
}

/// The body of a reply, or a sentence about why there is none.
fn result_of(result: Result<ehttp::Response, String>) -> Result<Value, String> {
    let response = result.map_err(|err| format!("ขอข้อมูลไม่สำเร็จ: {err}"))?;
    if !response.ok {
        return Err(format!("เซิร์ฟเวอร์ตอบ {}", response.status));
    }
    let text = response
        .text()
        .ok_or_else(|| "คำตอบไม่ใช่ข้อความ".to_string())?;
    serde_json::from_str(text).map_err(|err| format!("อ่าน JSON ไม่ได้: {err}"))
}

fn decode(kind: Kind, asked_for: &str, result: Result<ehttp::Response, String>) -> Got {
    let value = match result_of(result) {
        Ok(value) => value,
        Err(err) => return failed(kind, err),
    };
    if value.get("present").and_then(|p| p.as_bool()) != Some(true) {
        let reason = value
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or("ไม่มีข้อมูล")
            .to_owned();
        return missing(kind, reason);
    }
    let Some(result) = value.get("result") else {
        return missing(kind, "ตลาดไม่ได้ส่งอะไรกลับมา".into());
    };
    match kind {
        Kind::Book => {
            let instrument = text_of(result.get("instrument"));
            Got::Book(kind, Answer::Have(instrument, read_book(result)))
        }
        Kind::Trades => {
            let rows = read_trades(result);
            let instrument = result
                .as_array()
                .and_then(|list| list.first())
                .map(|row| text_of(row.get("instrument")))
                .unwrap_or_else(|| asked_for.to_owned());
            Got::Trades(kind, Answer::Have(instrument, rows))
        }
        Kind::Ticker => {
            let instrument = text_of(result.get("instrument"));
            Got::Ticker(
                kind,
                Answer::Have(instrument, Box::new(read_ticker(result))),
            )
        }
        Kind::Funding => {
            let rows = read_funding(result);
            let instrument = result
                .as_array()
                .and_then(|list| list.first())
                .map(|row| text_of(row.get("instrument")))
                .unwrap_or_else(|| asked_for.to_owned());
            Got::Funding(kind, Answer::Have(instrument, rows))
        }
        Kind::Spec => {
            let instrument = text_of(result.get("instrument"));
            Got::Spec(kind, Answer::Have(instrument, Box::new(read_spec(result))))
        }
    }
}

fn failed(kind: Kind, reason: String) -> Got {
    match kind {
        Kind::Book => Got::Book(kind, Answer::Failed(reason)),
        Kind::Trades => Got::Trades(kind, Answer::Failed(reason)),
        Kind::Ticker => Got::Ticker(kind, Answer::Failed(reason)),
        Kind::Funding => Got::Funding(kind, Answer::Failed(reason)),
        Kind::Spec => Got::Spec(kind, Answer::Failed(reason)),
    }
}

fn missing(kind: Kind, reason: String) -> Got {
    match kind {
        Kind::Book => Got::Book(kind, Answer::Missing(reason)),
        Kind::Trades => Got::Trades(kind, Answer::Missing(reason)),
        Kind::Ticker => Got::Ticker(kind, Answer::Missing(reason)),
        Kind::Funding => Got::Funding(kind, Answer::Missing(reason)),
        Kind::Spec => Got::Spec(kind, Answer::Missing(reason)),
    }
}

fn read_levels(value: Option<&Value>) -> Vec<Level> {
    value
        .and_then(|v| v.as_array())
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    // A level without a readable price or size cannot be
                    // drawn, and a zero in its place would be a lie about
                    // what is for sale.
                    Some(Level {
                        price: Num::from(row.get("price"))?,
                        size: Num::from(row.get("size"))?,
                        orders: uint_of(row.get("num_orders")),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn read_book(result: &Value) -> Book {
    Book {
        bids: read_levels(result.get("bids")),
        asks: read_levels(result.get("asks")),
        at: text_of(result.get("event_time_at")),
    }
}

pub fn read_trades(result: &Value) -> Vec<Trade> {
    result
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    Some(Trade {
                        at: text_of(row.get("event_time_at")),
                        price: Num::from(row.get("price"))?,
                        size: Num::from(row.get("size"))?,
                        taker_buy: row
                            .get("is_taker_buyer")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn read_ticker(result: &Value) -> Ticker {
    Ticker {
        mark: Num::from(result.get("mark_price")),
        index: Num::from(result.get("index_price")),
        last: Num::from(result.get("last_price")),
        mid: Num::from(result.get("mid_price")),
        high_24h: Num::from(result.get("high_price")),
        low_24h: Num::from(result.get("low_price")),
        open_24h: Num::from(result.get("open_price")),
        buy_volume_24h: Num::from(result.get("buy_volume_24h_b")),
        sell_volume_24h: Num::from(result.get("sell_volume_24h_b")),
        open_interest: Num::from(result.get("open_interest")),
        long_short: Num::from(result.get("long_short_ratio")),
        funding_now: Num::from(result.get("funding_rate_8h_curr")),
        funding_avg: Num::from(result.get("funding_rate_8h_avg")),
        next_funding_at: text_of(result.get("next_funding_time_at")),
        status: text_of(result.get("status")),
        at: text_of(result.get("event_time_at")),
        event_nanos: Num::from(result.get("event_time")).map(|n| n.value),
        next_funding_nanos: Num::from(result.get("next_funding_time")).map(|n| n.value),
    }
}

pub fn read_funding(result: &Value) -> Vec<Funding> {
    result
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| Funding {
                    at: text_of(row.get("funding_time_at")),
                    rate: Num::from(row.get("funding_rate")),
                    avg_8h: Num::from(row.get("funding_rate_8_h_avg")),
                    mark: Num::from(row.get("mark_price")),
                    interval_hours: uint_of(row.get("funding_interval_hours")),
                })
                .collect()
        })
        .unwrap_or_default()
}

pub fn read_spec(result: &Value) -> Spec {
    Spec {
        instrument: text_of(result.get("instrument")),
        base: text_of(result.get("base")),
        quote: text_of(result.get("quote")),
        tick_size: Num::from(result.get("tick_size")),
        min_size: Num::from(result.get("min_size")),
        min_notional: Num::from(result.get("min_notional")),
        max_position: Num::from(result.get("max_position_size")),
        funding_interval_hours: uint_of(result.get("funding_interval_hours")),
        funding_cap: Num::from(result.get("adjusted_funding_rate_cap")),
        funding_floor: Num::from(result.get("adjusted_funding_rate_floor")),
        status: text_of(result.get("status")),
        kind: text_of(result.get("kind")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn book() -> Value {
        json!({
            "instrument": "BTC_USDT_Perp",
            "event_time_at": "2026-11-13T05:48:18Z",
            "bids": [
                { "price": "68593.0", "size": "0.948", "num_orders": 2 },
                { "price": "68592.0", "size": "1.500", "num_orders": 1 }
            ],
            "asks": [
                { "price": "68594.0", "size": "0.207", "num_orders": 2 },
                { "price": "68595.0", "size": "0.729", "num_orders": 1 }
            ]
        })
    }

    #[test]
    fn a_price_keeps_the_exchanges_own_text_and_gains_a_number_beside_it() {
        let level = &read_book(&book()).asks[0];
        assert_eq!(level.price.text, "68594.0", "ต้องพิมพ์เหมือนที่ตลาดส่งมา");
        assert_eq!(level.price.value, 68_594.0);
        assert_eq!(level.orders, 2);
    }

    #[test]
    fn the_spread_is_the_gap_and_what_share_of_the_price_it_is() {
        let (gap, pct) = read_book(&book()).spread().expect("มีทั้งสองฝั่ง");
        assert_eq!(gap, 1.0);
        assert!((pct - 0.001458).abs() < 1e-5, "{pct}");
    }

    #[test]
    fn an_empty_side_has_no_spread_rather_than_a_spread_of_zero() {
        let mut raw = book();
        raw["asks"] = json!([]);
        assert_eq!(read_book(&raw).spread(), None);
    }

    #[test]
    fn totals_add_up_down_the_side_the_way_a_book_is_read() {
        let book = read_book(&book());
        // Compared with a tolerance: adding decimals in binary floating
        // point is exactly the reason money never travels as f64 here.
        let close = |got: Vec<f64>, want: [f64; 2]| {
            assert_eq!(got.len(), 2);
            for (got, want) in got.iter().zip(want) {
                assert!((got - want).abs() < 1e-9, "{got} != {want}");
            }
        };
        close(Book::totals(&book.bids), [0.948, 2.448]);
        close(Book::totals(&book.asks), [0.207, 0.936]);
    }

    #[test]
    fn both_sides_of_the_book_share_one_scale() {
        let book = read_book(&book());
        // The bid side is deeper, so that is the scale both sides draw to.
        assert!((book.deepest(10) - 2.448).abs() < 1e-9);
    }

    #[test]
    fn a_level_that_will_not_parse_is_dropped_not_drawn_as_zero() {
        let mut raw = book();
        raw["bids"] = json!([
            { "price": "68593.0", "size": "0.948" },
            { "price": "", "size": "9.0" },
            { "price": "68591.0", "size": "ไม่ใช่ตัวเลข" }
        ]);
        let levels = read_book(&raw).bids;
        assert_eq!(levels.len(), 1);
    }

    #[test]
    fn a_trade_knows_which_side_crossed_the_spread() {
        let rows = read_trades(&json!([
            { "event_time_at": "2026-11-13T05:48:18Z", "price": "68589.0", "size": "0.018", "is_taker_buyer": false },
            { "event_time_at": "2026-11-13T05:48:19Z", "price": "68590.0", "size": "0.746", "is_taker_buyer": true }
        ]));
        assert_eq!(rows.len(), 2);
        assert!(!rows[0].taker_buy);
        assert!(rows[1].taker_buy);
        assert_eq!(rows[1].size.text, "0.746");
    }

    #[test]
    fn the_ticker_carries_the_numbers_the_exchanges_own_header_shows() {
        let ticker = read_ticker(&json!({
            "mark_price": "68519.388953708",
            "index_price": "68565.815621676",
            "last_price": "68584.2",
            "high_price": "73704.0",
            "low_price": "68350.0",
            "open_price": "73589.7",
            "buy_volume_24h_b": "19015.173",
            "sell_volume_24h_b": "21147.127",
            "open_interest": "6725.31",
            "long_short_ratio": "1.571788",
            "funding_rate_8h_curr": "0.01",
            "next_funding_time_at": "2026-11-13T08:00:00Z",
            "status": "ACTIVE"
        }));
        assert_eq!(ticker.last.as_ref().unwrap().text, "68584.2");
        assert!((ticker.volume_24h().unwrap() - 40_162.3).abs() < 1e-6);
        let change = ticker.change_pct().unwrap();
        assert!((change + 6.8).abs() < 0.1, "เปลี่ยนไป {change}%");
        assert_eq!(ticker.next_funding_at, "2026-11-13T08:00:00Z");
    }

    #[test]
    fn the_countdown_to_funding_comes_from_the_exchanges_own_clock() {
        let ticker = read_ticker(&json!({
            "event_time": "1794549048500000000",
            "next_funding_time": "1794556800000000000",
            "next_funding_time_at": "2026-11-13T08:00:00Z"
        }));
        let seconds = ticker.until_funding().expect("นับถอยหลังได้");
        assert!((seconds - 7751.5).abs() < 1.0, "เหลือ {seconds} วินาที");
    }

    #[test]
    fn a_rate_the_exchange_sends_as_a_percent_is_shown_the_way_its_own_screen_shows_it() {
        // GRVT's own funding tab prints +0.0100% / +10.9500% for a
        // funding_rate_8h_curr of 0.01, charged three times a day.
        let year = annualised(0.01, 8).unwrap();
        assert!((year - 10.95).abs() < 1e-9, "{year}");
        assert!((annualised(0.01, 1).unwrap() - 87.6).abs() < 1e-9);
        assert!(
            (annualised(0.01, 0).unwrap() - 10.95).abs() < 1e-9,
            "ไม่มีช่วงเวลาให้ ให้ถือว่า 8 ชั่วโมงตามค่าปกติ ไม่ใช่หารศูนย์"
        );
    }

    #[test]
    fn the_pressure_bar_counts_the_whole_book_not_the_rows_on_screen() {
        let book = read_book(&json!({
            "bids": [
                { "price": "68974.0", "size": "1.0" },
                { "price": "68972.0", "size": "1.0" },
                // The order that sits away from the touch and that the
                // exchange's own number counts.
                { "price": "68949.0", "size": "20.0" }
            ],
            "asks": [
                { "price": "68976.0", "size": "2.0" },
                { "price": "68977.0", "size": "2.0" }
            ]
        }));
        let (buy, _) = book.pressure().unwrap();
        assert!((buy - 84.6).abs() < 0.1, "ทั้งกระดานได้ {buy}%");

        // Two rows a side would have said something quite different, which
        // is exactly the gap that made the board disagree with the web.
        let (near, _) = pressure(2.0, 4.0).unwrap();
        assert!((near - 33.3).abs() < 0.1);
    }

    #[test]
    fn an_empty_book_has_no_pressure_at_all() {
        assert_eq!(Book::default().pressure(), None);
    }

    #[test]
    fn the_two_sides_of_the_book_split_into_two_percentages_that_add_up() {
        let (buy, sell) = pressure(3.0, 1.0).unwrap();
        assert!((buy - 75.0).abs() < 1e-9);
        assert!((sell - 25.0).abs() < 1e-9);
        assert!((buy + sell - 100.0).abs() < 1e-9);

        let (buy, sell) = pressure(2.5, 2.5).unwrap();
        assert!((buy - 50.0).abs() < 1e-9 && (sell - 50.0).abs() < 1e-9);
    }

    #[test]
    fn one_empty_side_is_a_hundred_percent_the_other_way() {
        assert_eq!(pressure(4.0, 0.0), Some((100.0, 0.0)));
        assert_eq!(pressure(0.0, 4.0), Some((0.0, 100.0)));
    }

    #[test]
    fn an_empty_book_has_no_pressure_rather_than_fifty_fifty() {
        assert_eq!(pressure(0.0, 0.0), None, "ไม่มีของทั้งสองฝั่ง ไม่ใช่ 50/50");
        assert_eq!(pressure(f64::NAN, 1.0), None);
    }

    #[test]
    fn the_sign_of_funding_says_who_pays() {
        assert!(who_pays(0.01).contains("long จ่าย"));
        assert!(who_pays(-0.01).contains("short จ่าย"));
        assert!(who_pays(0.0).contains("ไม่มีใคร"));
    }

    #[test]
    fn a_ticker_with_no_open_price_reports_no_change_rather_than_minus_one_hundred() {
        let ticker = read_ticker(&json!({ "last_price": "68584.2" }));
        assert_eq!(ticker.change_pct(), None);
        assert_eq!(ticker.volume_24h(), None);
    }

    #[test]
    fn the_contract_specification_is_read_whole() {
        let spec = read_spec(&json!({
            "instrument": "BTC_USDT_Perp",
            "base": "BTC",
            "quote": "USDT",
            "tick_size": "0.1",
            "min_size": "0.001",
            "min_notional": "100.0",
            "max_position_size": "1000.0",
            "funding_interval_hours": 8,
            "adjusted_funding_rate_cap": "0.3",
            "adjusted_funding_rate_floor": "-0.3",
            "status": "ACTIVE",
            "kind": "PERPETUAL"
        }));
        assert_eq!(spec.tick_size.unwrap().text, "0.1");
        assert_eq!(spec.min_notional.unwrap().text, "100.0");
        assert_eq!(spec.funding_interval_hours, 8);
        assert_eq!(spec.status, "ACTIVE");
    }

    #[test]
    fn funding_history_keeps_the_hour_it_was_charged() {
        let rows = read_funding(&json!([
            { "funding_time_at": "2026-11-13T00:00:00Z", "funding_rate": "0.01",
              "funding_rate_8_h_avg": "0.01", "mark_price": "71432.99", "funding_interval_hours": 8 }
        ]));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].at, "2026-11-13T00:00:00Z");
        assert_eq!(rows[0].rate.as_ref().unwrap().text, "0.01");
        assert_eq!(rows[0].interval_hours, 8);
    }

    // ------------------------------------------------------------ fetching

    fn quiet() -> Exchange {
        let mut exchange = Exchange::default();
        exchange.due = [f64::INFINITY; 5];
        exchange
    }

    #[test]
    fn a_market_request_that_never_came_back_is_asked_again() {
        let mut exchange = Exchange::default();
        exchange.due = [0.0; 5];
        exchange.flight = [Some(0.0); 5];
        exchange.poll(1.0);
        assert_eq!(exchange.flight[0], Some(0.0), "ยังอยู่ในเวลารอ");

        exchange.poll(crate::api::LOST + 1.0);
        assert_eq!(
            exchange.flight[0],
            Some(crate::api::LOST + 1.0),
            "เลยเวลารอแล้วต้องถามใหม่ ไม่ใช่ค้างไปตลอด"
        );
    }

    #[test]
    fn a_book_for_another_contract_never_lands_under_this_ones_heading() {
        let mut exchange = quiet();
        exchange
            .tx
            .send(Got::Book(
                Kind::Book,
                Answer::Have("SOL_USDT_Perp".into(), read_book(&book())),
            ))
            .unwrap();
        exchange.poll(0.0);
        assert!(exchange.book.is_none(), "สมุดของเหรียญอื่นต้องไม่ขึ้นจอ");
        assert_eq!(exchange.ignored, 1);
    }

    #[test]
    fn a_book_for_the_contract_on_screen_lands() {
        let mut exchange = quiet();
        exchange
            .tx
            .send(Got::Book(
                Kind::Book,
                Answer::Have("BTC_USDT_Perp".into(), read_book(&book())),
            ))
            .unwrap();
        exchange.poll(0.0);
        assert_eq!(exchange.book.unwrap().asks.len(), 2);
        assert_eq!(exchange.load[Kind::Book.index()], Load::Ready);
    }

    #[test]
    fn switching_contract_clears_everything_the_old_one_put_on_screen() {
        let mut exchange = quiet();
        exchange.book = Some(read_book(&book()));
        exchange.trades = read_trades(&json!([
            { "price": "1", "size": "1", "is_taker_buyer": true }
        ]));
        exchange.watch("ETH_USDT_Perp");
        assert!(exchange.book.is_none());
        assert!(exchange.trades.is_empty());
        assert_eq!(exchange.symbol, "ETH_USDT_Perp");
        assert_eq!(exchange.due, [0.0; 5], "ต้องรีบถามของใหม่ทันที");
    }

    #[test]
    fn a_route_carries_the_contract_and_the_limits_the_exchange_accepts() {
        assert_eq!(
            Kind::Book.route("BTC_USDT_Perp"),
            "/api/market/book?symbol=BTC_USDT_Perp&depth=500"
        );
        assert!(Kind::Trades.route("ETH_USDT_Perp").contains("limit=40"));
        assert_eq!(
            Kind::Spec.route("BTC_USDT_Perp"),
            "/api/market/instrument?symbol=BTC_USDT_Perp"
        );
    }

    #[test]
    fn a_missing_answer_is_a_sentence_on_that_card_and_nothing_else_breaks() {
        let mut exchange = quiet();
        exchange
            .tx
            .send(Got::Ticker(
                Kind::Ticker,
                Answer::Missing("ตลาดปฏิเสธ".into()),
            ))
            .unwrap();
        exchange.poll(0.0);
        assert_eq!(
            *exchange.load_of(Kind::Ticker),
            Load::Missing("ตลาดปฏิเสธ".into())
        );
        assert_eq!(*exchange.load_of(Kind::Book), Load::Idle);
    }
}
