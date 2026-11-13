//! GRVT public market data over WebSocket.
//!
//! The public stream needs **no credentials**: no API key, no signature, no
//! session cookie. That matters more than it sounds - it means a student can
//! open the board and watch a real market before they have an account, and it
//! means the board never holds a secret that could place an order.
//!
//! Wire protocol (GRVT `v1.mini.s`):
//!
//! ```text
//! connect  wss://market-data.testnet.grvt.io/ws
//! send     {"request_id":1,"stream":"v1.mini.s",
//!           "feed":["BTC_USDT_Perp@500"],"method":"subscribe","is_full":true}
//! recv     {"stream":"v1.mini.s","selector":"...","feed":{...ticker...}}
//! ```
//!
//! The `@200` suffix is the update interval in milliseconds, and the
//! exchange only takes a few values - see `RATE_MS`.
//!
//! **Numbers arrive as strings and are kept as strings for display.** They are
//! parsed to `f64` only to place a pixel on a chart. A price shown to a person
//! is the exact text the exchange sent; nothing here re-formats money.

use serde::{Deserialize, Serialize};

/// Which GRVT deployment to watch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Env {
    Testnet,
    Prod,
}

impl Env {
    pub fn market_data_url(self) -> &'static str {
        match self {
            Env::Testnet => "wss://market-data.testnet.grvt.io/ws",
            Env::Prod => "wss://market-data.grvt.io/ws",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Env::Testnet => "testnet",
            Env::Prod => "prod",
        }
    }
}

#[derive(Debug, Serialize)]
struct SubscribeRequest {
    request_id: u32,
    stream: String,
    feed: Vec<String>,
    method: String,
    is_full: bool,
}

/// The subscribe frame for one instrument, as JSON text.
/// How often the exchange is asked to push a new reading, in milliseconds.
///
/// **Not any number.** Measured against GRVT testnet on 13 Nov 2026: 50,
/// 100 and 250 come back as `Feed rate is invalid` (code 3030), while 200,
/// 500 and 1000 are accepted. 200 is the fastest the exchange will give,
/// which is what the price on this board moves at.
pub const RATE_MS: u32 = 200;

/// The rates the exchange took when they were last tried.
pub const RATES_ACCEPTED: [u32; 3] = [200, 500, 1000];

pub fn subscribe_frame(request_id: u32, instrument: &str, rate_ms: u32) -> String {
    let request = SubscribeRequest {
        request_id,
        stream: "v1.mini.s".to_string(),
        feed: vec![format!("{instrument}@{rate_ms}")],
        method: "subscribe".to_string(),
        is_full: true,
    };
    serde_json::to_string(&request).expect("subscribe frame is always serialisable")
}

/// One reading of the market. Every number is the exchange's own text.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Ticker {
    pub instrument: String,
    pub mark_price: String,
    #[serde(default)]
    pub index_price: String,
    #[serde(default)]
    pub last_price: String,

    #[serde(default)]
    pub open: Option<String>,
    #[serde(default)]
    pub high: Option<String>,
    #[serde(default)]
    pub low: Option<String>,
    #[serde(default)]
    pub close: Option<String>,
    #[serde(default)]
    pub volume: Option<String>,

    #[serde(default)]
    pub funding_rate: Option<String>,
    #[serde(default)]
    pub open_interest: Option<String>,

    #[serde(default)]
    pub best_bid_price: Option<String>,
    #[serde(default)]
    pub best_bid_size: Option<String>,
    #[serde(default)]
    pub best_ask_price: Option<String>,
    #[serde(default)]
    pub best_ask_size: Option<String>,
}

impl Ticker {
    /// The price to draw, as a number. `None` when it cannot be parsed, which
    /// is a missing point on the chart rather than a zero.
    pub fn mark(&self) -> Option<f64> {
        parse(&self.mark_price)
    }

    /// The price something actually traded at.
    ///
    /// This, not the mark, is what belongs on a candle chart: the candles
    /// come from the exchange's TRADE series, and the mark is an index with
    /// a basis on top - the two sit tens of dollars apart, so folding the
    /// mark into the newest bar draws a step that no trade ever made.
    pub fn last(&self) -> Option<f64> {
        parse(&self.last_price)
    }

    pub fn spread(&self) -> Option<f64> {
        let bid = self.best_bid_price.as_deref().and_then(parse)?;
        let ask = self.best_ask_price.as_deref().and_then(parse)?;
        Some(ask - bid)
    }
}

/// Parse a price for drawing only. Never use this for money arithmetic.
pub fn parse(text: &str) -> Option<f64> {
    let value: f64 = text.trim().parse().ok()?;
    value.is_finite().then_some(value)
}

/// What one frame from the stream turned out to be.
#[derive(Debug, Clone)]
pub enum Frame {
    /// A market reading.
    Tick(Ticker),
    /// The exchange confirmed a subscription.
    Subscribed { stream: String, subs: Vec<String> },
    /// The exchange refused something.
    Refused { code: i32, message: String },
    /// A frame this build does not understand. Kept rather than dropped so a
    /// protocol change shows up on screen instead of looking like silence.
    Unknown(String),
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RawFrame {
    Data {
        stream: String,
        #[allow(dead_code)]
        selector: String,
        feed: serde_json::Value,
    },
    Subscription {
        #[allow(dead_code)]
        request_id: u32,
        stream: String,
        subs: Vec<String>,
        #[allow(dead_code)]
        unsubs: Vec<String>,
    },
    Error {
        code: i32,
        message: String,
        #[allow(dead_code)]
        status: i32,
    },
}

/// Turn one text frame from the socket into something the board can use.
pub fn decode(text: &str) -> Frame {
    match serde_json::from_str::<RawFrame>(text) {
        Ok(RawFrame::Data { stream, feed, .. }) if stream == "v1.mini.s" => {
            match serde_json::from_value::<Ticker>(feed) {
                Ok(ticker) => Frame::Tick(ticker),
                Err(err) => Frame::Unknown(format!("ticker did not parse: {err}")),
            }
        }
        Ok(RawFrame::Data { stream, .. }) => Frame::Unknown(format!("unhandled stream {stream}")),
        Ok(RawFrame::Subscription { stream, subs, .. }) => Frame::Subscribed { stream, subs },
        Ok(RawFrame::Error { code, message, .. }) => Frame::Refused { code, message },
        Err(err) => Frame::Unknown(format!("{err}: {}", &text[..text.len().min(160)])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_subscribe_frame_matches_what_grvt_expects() {
        let frame = subscribe_frame(1, "BTC_USDT_Perp", 500);
        // (500 here only because this test checks the shape of the frame,
        // not the rate the board actually asks for.)
        let value: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(value["stream"], "v1.mini.s");
        assert_eq!(value["feed"][0], "BTC_USDT_Perp@500");
        assert_eq!(value["method"], "subscribe");
        assert_eq!(value["is_full"], true);
    }

    #[test]
    fn the_price_for_a_candle_is_the_traded_one_not_the_mark() {
        let ticker = Ticker {
            mark_price: "68055.29".into(),
            last_price: "68114.70".into(),
            ..Ticker::default()
        };
        assert_eq!(ticker.last(), Some(68_114.70));
        assert_eq!(ticker.mark(), Some(68_055.29));
    }

    #[test]
    fn a_reading_with_no_trade_yet_has_no_traded_price_rather_than_a_zero() {
        let ticker = Ticker {
            mark_price: "68055.29".into(),
            ..Ticker::default()
        };
        assert_eq!(ticker.last(), None);
    }

    #[test]
    fn the_rate_the_board_asks_for_is_one_the_exchange_will_give() {
        assert!(
            RATES_ACCEPTED.contains(&RATE_MS),
            "{RATE_MS} ms ไม่อยู่ในชุดที่ตลาดรับ {RATES_ACCEPTED:?} จะโดน code 3030"
        );
    }

    #[test]
    fn a_ticker_frame_decodes_and_keeps_the_exchanges_own_text() {
        let text = r#"{"stream":"v1.mini.s","selector":"BTC_USDT_Perp@500","feed":{
            "instrument":"BTC_USDT_Perp","mark_price":"73296.0","index_price":"73280.1",
            "last_price":"73280.0","best_bid_price":"73307.0","best_ask_price":"73308.0"}}"#;
        match decode(text) {
            Frame::Tick(ticker) => {
                assert_eq!(ticker.instrument, "BTC_USDT_Perp");
                assert_eq!(ticker.mark_price, "73296.0");
                assert_eq!(ticker.mark(), Some(73296.0));
                assert_eq!(ticker.spread(), Some(1.0));
            }
            other => panic!("expected a tick, got {other:?}"),
        }
    }

    #[test]
    fn a_subscription_confirmation_is_not_mistaken_for_a_tick() {
        let text =
            r#"{"request_id":1,"stream":"v1.mini.s","subs":["BTC_USDT_Perp@500"],"unsubs":[]}"#;
        match decode(text) {
            Frame::Subscribed { stream, subs } => {
                assert_eq!(stream, "v1.mini.s");
                assert_eq!(subs, vec!["BTC_USDT_Perp@500"]);
            }
            other => panic!("expected a subscription, got {other:?}"),
        }
    }

    #[test]
    fn a_refusal_carries_the_exchanges_reason() {
        let text = r#"{"code":1001,"message":"invalid instrument","status":400}"#;
        match decode(text) {
            Frame::Refused { code, message } => {
                assert_eq!(code, 1001);
                assert_eq!(message, "invalid instrument");
            }
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_frame_this_build_does_not_know_is_kept_not_dropped() {
        match decode("{\"something\":\"new\"}") {
            Frame::Unknown(_) => {}
            other => panic!("expected unknown, got {other:?}"),
        }
        match decode("not json at all") {
            Frame::Unknown(_) => {}
            other => panic!("expected unknown, got {other:?}"),
        }
    }

    #[test]
    fn a_price_that_cannot_be_parsed_is_a_gap_not_a_zero() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("n/a"), None);
        assert_eq!(parse("inf"), None);
        assert_eq!(parse(" 12.5 "), Some(12.5));
    }
}
