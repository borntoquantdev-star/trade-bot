//! Candles straight from GRVT's public REST, for when the bot has not
//! downloaded any.
//!
//! `POST market-data.<env>.grvt.io/full/v1/kline` needs **no credentials**,
//! the same as the websocket stream. That is what lets a student open the
//! board on a fresh machine and see a real market immediately, instead of an
//! empty chart and a lesson about API keys they have not had yet.
//!
//! The reply is reshaped into exactly the rows `/api/candles` returns, so the
//! board draws both sources with one code path and the only difference on
//! screen is a label saying where the bars came from.
//!
//! This is the one place the board's server talks to the outside world. It
//! sends no credential and can only read, so there is still no path from this
//! process to an order.

use axum::extract::{Query, State};
use serde::Deserialize;

use crate::Config;

/// Our timeframe names, and what GRVT calls them.
///
/// An unknown timeframe is refused by name rather than guessed at: passing a
/// made-up interval through would come back as an opaque exchange error that
/// looks like the board is broken.
fn grvt_interval(ours: &str) -> Option<&'static str> {
    Some(match ours {
        "1m" => "CI_1_M",
        "3m" => "CI_3_M",
        "5m" => "CI_5_M",
        "15m" => "CI_15_M",
        "30m" => "CI_30_M",
        "1h" => "CI_1_H",
        "2h" => "CI_2_H",
        "4h" => "CI_4_H",
        "8h" => "CI_8_H",
        "1d" => "CI_1_D",
        _ => return None,
    })
}

#[derive(Debug, Deserialize)]
pub struct Ask {
    #[serde(default = "default_symbol")]
    pub symbol: String,
    #[serde(default = "default_interval")]
    pub interval: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_symbol() -> String {
    "BTC_USDT_Perp".into()
}
fn default_interval() -> String {
    "5m".into()
}
fn default_limit() -> usize {
    400
}

#[derive(Debug, Deserialize)]
struct KlineReply {
    #[serde(default)]
    result: Vec<KlineRow>,
}

#[derive(Debug, Deserialize)]
struct KlineRow {
    /// nanoseconds since the epoch, as a string
    open_time: String,
    open: String,
    high: String,
    low: String,
    close: String,
    #[serde(default)]
    volume_b: String,
}

/// Nanoseconds since the epoch to the same ISO 8601 shape the candle store
/// writes, so both sources look identical to whatever reads them next.
pub fn iso_from_nanos(nanos: &str) -> Option<String> {
    let nanos: i128 = nanos.trim().parse().ok()?;
    let secs = i64::try_from(nanos.div_euclid(1_000_000_000)).ok()?;
    let rem = u32::try_from(nanos.rem_euclid(1_000_000_000)).ok()?;
    let stamp = chrono::DateTime::from_timestamp(secs, rem)?;
    Some(stamp.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
}

pub async fn kline(
    State(config): State<Config>,
    Query(ask): Query<Ask>,
) -> axum::Json<serde_json::Value> {
    let limit = ask.limit.clamp(1, 1_000);

    let Some(interval) = grvt_interval(&ask.interval) else {
        return axum::Json(serde_json::json!({
            "present": false,
            "reason": format!(
                "timeframe {} ไม่รองรับ (ใช้ได้: 1m 3m 5m 15m 30m 1h 2h 4h 8h 1d)",
                ask.interval
            ),
        }));
    };

    let url = format!("{}/full/v1/kline", config.market_data_rest);
    let body = serde_json::json!({
        "instrument": ask.symbol,
        "interval": interval,
        "type": "TRADE",
        "limit": limit,
    });

    let send = config
        .http
        .post(&url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(12))
        .send()
        .await;

    let response = match send {
        Ok(response) => response,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "present": false,
                "reason": format!("ต่อตลาดไม่ได้: {err}"),
            }))
        }
    };

    if !response.status().is_success() {
        return axum::Json(serde_json::json!({
            "present": false,
            "reason": format!("ตลาดตอบ {}", response.status()),
        }));
    }

    let reply: KlineReply = match response.json().await {
        Ok(reply) => reply,
        Err(err) => {
            return axum::Json(serde_json::json!({
                "present": false,
                "reason": format!("อ่านคำตอบของตลาดไม่ได้: {err}"),
            }))
        }
    };

    let mut rows = Vec::with_capacity(reply.result.len());
    let mut skipped = 0usize;
    for row in &reply.result {
        // Numbers stay as the exchange's own text, exactly as the CSV path
        // does. Only the timestamp is rewritten, and only into another text.
        match iso_from_nanos(&row.open_time) {
            Some(at) => rows.push(serde_json::json!({
                "t": at,
                "o": row.open,
                "h": row.high,
                "l": row.low,
                "c": row.close,
                "v": row.volume_b,
            })),
            None => skipped += 1,
        }
    }

    // GRVT returns newest first; the chart draws left to right in time.
    rows.reverse();

    axum::Json(serde_json::json!({
        "present": true,
        "source": "kline",
        "symbol": ask.symbol,
        "interval": ask.interval,
        "total": rows.len(),
        "skipped": skipped,
        "rows": rows,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_timeframe_names_map_onto_grvts() {
        assert_eq!(grvt_interval("5m"), Some("CI_5_M"));
        assert_eq!(grvt_interval("1h"), Some("CI_1_H"));
        assert_eq!(grvt_interval("1d"), Some("CI_1_D"));
    }

    #[test]
    fn a_timeframe_we_do_not_know_is_refused_rather_than_passed_through() {
        assert_eq!(grvt_interval("7s"), None);
        assert_eq!(grvt_interval("CI_5_M"), None, "ชื่อฝั่ง GRVT ต้องไม่ผ่านตรงๆ");
        assert_eq!(grvt_interval(""), None);
    }

    #[test]
    fn nanoseconds_become_the_same_iso_shape_the_candle_store_writes() {
        // 1794481500000000000 ns = 1794481500 s = 2026-11-12T11:05:00Z
        assert_eq!(
            iso_from_nanos("1794481500000000000").as_deref(),
            Some("2026-11-12T11:05:00Z")
        );
        // Sub-second nanoseconds are truncated to the second, not rounded up
        // into the next bar.
        assert_eq!(
            iso_from_nanos("1794481500999999999").as_deref(),
            Some("2026-11-12T11:05:00Z")
        );
    }

    #[test]
    fn a_timestamp_that_will_not_parse_is_dropped_not_defaulted_to_1970() {
        assert_eq!(iso_from_nanos(""), None);
        assert_eq!(iso_from_nanos("ไม่ใช่ตัวเลข"), None);
    }
}
