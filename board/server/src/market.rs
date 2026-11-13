//! The rest of GRVT's public market data, proxied for the board.
//!
//! `kline.rs` already does this for candles; these are the other four things
//! the exchange's own screen shows, and they are public in exactly the same
//! way - no key, no cookie, read only:
//!
//! | kind | what it answers |
//! |---|---|
//! | `book` | the order book, a few levels deep, with how many orders made each level |
//! | `trade` | the trades that just happened, and which side hit |
//! | `ticker` | mark, index, 24h high/low/volume, open interest, funding |
//! | `funding` | what funding has actually been charged, hour by hour |
//! | `instrument` | tick size, minimum size, minimum notional, caps |
//!
//! **One route with an allowlist, not five routes.** Everything here is the
//! same shape of request to the same host, and the only thing that varies is
//! a name and which parameters are allowed. A list of five names is easier
//! to audit than five handlers that drift apart.
//!
//! Numbers stay the exchange's own strings. The only thing rewritten is
//! time: GRVT counts nanoseconds since the epoch, which nothing on a screen
//! wants, so known timestamp fields gain an ISO twin next to them.

use axum::extract::{Path, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::kline::iso_from_nanos;
use crate::Config;

/// What the exchange will accept as a book depth. Anything else comes back
/// as "Depth is invalid", so it is refused here with a sentence that says
/// what to use instead.
const DEPTHS: [u32; 4] = [10, 50, 100, 500];

/// Timestamp fields, wherever they appear. Each gains a sibling with `_at`
/// on the end holding the same moment as ISO 8601.
const TIMES: [&str; 7] = [
    "event_time",
    "funding_time",
    "next_funding_time",
    "create_time",
    // The account side adds these: a position has two ends, and an order
    // changes state after it is created.
    "open_time",
    "close_time",
    "update_time",
];

#[derive(Debug, Deserialize)]
pub struct Ask {
    #[serde(default = "default_symbol")]
    pub symbol: String,
    #[serde(default = "default_depth")]
    pub depth: u32,
    #[serde(default = "default_limit")]
    pub limit: u32,
}

fn default_symbol() -> String {
    "BTC_USDT_Perp".into()
}
fn default_depth() -> u32 {
    10
}
fn default_limit() -> u32 {
    30
}

/// The instrument name is the only thing a request gets to put in the body,
/// so it has to look like one: letters, digits and underscores.
fn safe_symbol(value: &str) -> Option<&str> {
    let value = value.trim();
    let ok = !value.is_empty()
        && value.len() <= 40
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    ok.then_some(value)
}

/// Turn a route name and the query into the exchange's endpoint and body.
pub fn request_for(kind: &str, ask: &Ask) -> Result<(&'static str, Value), String> {
    let Some(symbol) = safe_symbol(&ask.symbol) else {
        return Err("ชื่อสัญญาใช้ไม่ได้".into());
    };
    let limit = ask.limit.clamp(1, 500);
    match kind {
        "book" => {
            if !DEPTHS.contains(&ask.depth) {
                return Err(format!(
                    "ความลึก {} ใช้ไม่ได้ ตลาดรับเฉพาะ {:?}",
                    ask.depth, DEPTHS
                ));
            }
            Ok((
                "book",
                json!({ "instrument": symbol, "depth": ask.depth, "aggregate": 1 }),
            ))
        }
        "trade" => Ok(("trade", json!({ "instrument": symbol, "limit": limit }))),
        "funding" => Ok(("funding", json!({ "instrument": symbol, "limit": limit }))),
        "ticker" => Ok(("ticker", json!({ "instrument": symbol }))),
        "instrument" => Ok(("instrument", json!({ "instrument": symbol }))),
        other => Err(format!(
            "ไม่รู้จัก {other} · มีให้ใช้: book trade ticker funding instrument"
        )),
    }
}

pub async fn market(
    Path(kind): Path<String>,
    Query(ask): Query<Ask>,
    State(config): State<Config>,
) -> Json<Value> {
    let (endpoint, body) = match request_for(&kind, &ask) {
        Ok(pair) => pair,
        Err(reason) => return Json(json!({ "present": false, "reason": reason })),
    };

    let url = format!("{}/full/v1/{endpoint}", config.market_data_rest);
    let sent = config.http.post(&url).json(&body).send().await;
    let response = match sent {
        Ok(response) => response,
        Err(err) => {
            return Json(json!({
                "present": false,
                "reason": format!("ขอข้อมูลจากตลาดไม่สำเร็จ: {err}"),
            }))
        }
    };
    let status = response.status();
    let parsed: Result<Value, _> = response.json().await;
    let Ok(value) = parsed else {
        return Json(json!({
            "present": false,
            "reason": format!("ตลาดตอบ {status} แต่อ่านไม่ออก"),
        }));
    };
    if !status.is_success() {
        // The exchange's own message is worth more than a status code.
        let message = value
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("ไม่ได้บอกเหตุผล");
        return Json(json!({
            "present": false,
            "reason": format!("ตลาดปฏิเสธ ({status}): {message}"),
        }));
    }

    let mut result = value.get("result").cloned().unwrap_or(Value::Null);
    add_times(&mut result);
    Json(json!({ "present": true, "result": result }))
}

/// Walk the answer and give every known nanosecond field an ISO twin.
pub fn add_times(value: &mut Value) {
    match value {
        Value::Array(items) => {
            for item in items {
                add_times(item);
            }
        }
        Value::Object(map) => {
            let found: Vec<(String, String)> = TIMES
                .iter()
                .filter_map(|key| {
                    let nanos = map.get(*key)?.as_str()?;
                    let iso = iso_from_nanos(nanos)?;
                    Some((format!("{key}_at"), iso))
                })
                .collect();
            for (key, iso) in found {
                map.insert(key, Value::String(iso));
            }
            let children: Vec<&mut Value> = map.values_mut().collect();
            for child in children {
                if child.is_array() || child.is_object() {
                    add_times(child);
                }
            }
        }
        _ => {}
    }
}

/// Unused helper kept honest: an empty object is still an object.
#[allow(dead_code)]
fn empty() -> Map<String, Value> {
    Map::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ask(symbol: &str, depth: u32, limit: u32) -> Ask {
        Ask {
            symbol: symbol.into(),
            depth,
            limit,
        }
    }

    #[test]
    fn each_kind_asks_the_exchange_the_question_it_expects() {
        let (endpoint, body) = request_for("book", &ask("BTC_USDT_Perp", 10, 30)).unwrap();
        assert_eq!(endpoint, "book");
        assert_eq!(body["depth"], json!(10));
        assert_eq!(body["aggregate"], json!(1));

        let (endpoint, body) = request_for("trade", &ask("ETH_USDT_Perp", 10, 25)).unwrap();
        assert_eq!(endpoint, "trade");
        assert_eq!(body["limit"], json!(25));
        assert_eq!(body["instrument"], json!("ETH_USDT_Perp"));

        let (endpoint, body) = request_for("instrument", &ask("BTC_USDT_Perp", 10, 30)).unwrap();
        assert_eq!(endpoint, "instrument");
        assert_eq!(body.get("limit"), None, "สเปคสัญญาไม่มี limit");
    }

    #[test]
    fn a_depth_the_exchange_would_refuse_is_refused_here_with_the_choices() {
        let err = request_for("book", &ask("BTC_USDT_Perp", 20, 30)).unwrap_err();
        assert!(err.contains("20"));
        assert!(err.contains("10"), "ต้องบอกด้วยว่าใช้อะไรได้: {err}");
        for depth in DEPTHS {
            assert!(request_for("book", &ask("BTC_USDT_Perp", depth, 30)).is_ok());
        }
    }

    #[test]
    fn a_kind_that_is_not_on_the_list_never_reaches_the_exchange() {
        let err = request_for("positions", &ask("BTC_USDT_Perp", 10, 30)).unwrap_err();
        assert!(err.contains("positions"));
        assert!(err.contains("book"), "บอกด้วยว่ามีอะไรให้ใช้");
    }

    #[test]
    fn a_symbol_that_is_not_a_symbol_is_refused_rather_than_sent_on() {
        for bad in ["", "   ", "BTC/USDT", "BTC_USDT_Perp; drop", "../etc"] {
            assert!(
                request_for("ticker", &ask(bad, 10, 30)).is_err(),
                "ควรปฏิเสธ {bad:?}"
            );
        }
    }

    #[test]
    fn a_limit_out_of_range_is_brought_back_in_rather_than_refused() {
        let (_, body) = request_for("trade", &ask("BTC_USDT_Perp", 10, 100_000)).unwrap();
        assert_eq!(body["limit"], json!(500));
        let (_, body) = request_for("trade", &ask("BTC_USDT_Perp", 10, 0)).unwrap();
        assert_eq!(body["limit"], json!(1));
    }

    #[test]
    fn nanoseconds_gain_a_readable_twin_wherever_they_appear() {
        let mut value = json!({
            "result": {
                "event_time": "1794481500000000000",
                "nested": [{ "funding_time": "1794481500000000000" }]
            }
        });
        add_times(&mut value);
        assert_eq!(
            value["result"]["event_time_at"],
            json!("2026-11-12T11:05:00Z")
        );
        assert_eq!(
            value["result"]["nested"][0]["funding_time_at"],
            json!("2026-11-12T11:05:00Z")
        );
        assert_eq!(
            value["result"]["event_time"],
            json!("1794481500000000000"),
            "เวลาเดิมของตลาดต้องยังอยู่"
        );
    }

    #[test]
    fn a_timestamp_that_will_not_parse_simply_gets_no_twin() {
        let mut value = json!({ "event_time": "ไม่ใช่ตัวเลข", "price": "68500.0" });
        add_times(&mut value);
        assert_eq!(value.get("event_time_at"), None);
        assert_eq!(value["price"], json!("68500.0"));
    }
}
