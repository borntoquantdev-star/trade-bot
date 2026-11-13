//! Serving the candle history the Python bot already downloaded.
//!
//! The bot writes `data/candles/<exchange>/<symbol>/<interval>.csv` with the
//! header `open_time,open,high,low,close,volume`. Nothing here downloads or
//! computes anything: if a candle is on screen, the bot put it on disk.
//!
//! **Numbers stay strings all the way to the browser.** They are text in the
//! CSV because they came out of a Decimal, and the board parses them to `f64`
//! only to place a pixel. Turning them into JSON numbers here would quietly
//! round money on the way past.

use std::path::{Path, PathBuf};

use axum::extract::{Query, State};
use serde::{Deserialize, Serialize};

use crate::Config;

/// One row of the CSV, with short keys because a few hundred of these travel
/// over the wire on every chart load.
#[derive(Debug, Serialize)]
pub struct Row {
    /// open_time, ISO 8601, exactly as written
    pub t: String,
    pub o: String,
    pub h: String,
    pub l: String,
    pub c: String,
    pub v: String,
}

#[derive(Debug, Deserialize)]
pub struct Ask {
    #[serde(default = "default_exchange")]
    pub exchange: String,
    #[serde(default = "default_symbol")]
    pub symbol: String,
    #[serde(default = "default_interval")]
    pub interval: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

fn default_exchange() -> String {
    "grvt".into()
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

/// The bot's own rule for turning a symbol into a filename, and our guard
/// against a query parameter walking out of the candle directory.
///
/// This is the only place a request gets to influence a path, so it refuses
/// anything that is not already a plain name rather than trying to clean it:
/// a request for `../../etc/passwd` is a bug or an attack, and neither
/// deserves a best effort.
fn safe_name(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value.len() > 64 {
        return None;
    }
    let ok = value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    // A leading dot would let "." and ".." through the check above.
    if !ok || value.starts_with('.') {
        return None;
    }
    Some(value.to_owned())
}

fn path_for(root: &Path, exchange: &str, symbol: &str, interval: &str) -> Option<PathBuf> {
    Some(
        root.join(safe_name(exchange)?)
            .join(safe_name(symbol)?)
            .join(format!("{}.csv", safe_name(interval)?)),
    )
}

/// Split one CSV line. The bot never quotes or escapes these fields - they
/// are timestamps and decimals - so a full CSV parser would be weight for
/// nothing. A line with the wrong shape is skipped, not guessed at.
fn parse_line(line: &str) -> Option<Row> {
    let mut parts = line.trim().split(',');
    let row = Row {
        t: parts.next()?.to_owned(),
        o: parts.next()?.to_owned(),
        h: parts.next()?.to_owned(),
        l: parts.next()?.to_owned(),
        c: parts.next()?.to_owned(),
        v: parts.next()?.to_owned(),
    };
    if parts.next().is_some() || row.t.is_empty() {
        return None;
    }
    Some(row)
}

pub async fn candles(
    State(config): State<Config>,
    Query(ask): Query<Ask>,
) -> axum::Json<serde_json::Value> {
    let limit = ask.limit.clamp(1, 5_000);

    let Some(path) = path_for(&config.candles, &ask.exchange, &ask.symbol, &ask.interval) else {
        return axum::Json(serde_json::json!({
            "present": false,
            "reason": "ชื่อตลาด/สัญลักษณ์/timeframe มีอักขระที่ใช้เป็นชื่อไฟล์ไม่ได้",
        }));
    };

    let Ok(text) = tokio::fs::read_to_string(&path).await else {
        return axum::Json(serde_json::json!({
            "present": false,
            "reason": format!("ยังไม่มี {}", path.display()),
            "fix": format!(
                "tradebot --fetch-candles --days 90   (แล้วแท่งจะไปอยู่ที่ {})",
                path.display()
            ),
        }));
    };

    // The file is append-only and sorted, so the newest candles are the last
    // lines. A year of 5m bars is ~115k lines; only the tail is sent.
    let mut lines = text.lines();
    let header = lines.next().unwrap_or_default();
    if !header.starts_with("open_time,") {
        return axum::Json(serde_json::json!({
            "present": false,
            "reason": format!("หัวตารางไม่ใช่ของคลังแท่งเทียน: {header}"),
        }));
    }

    let all: Vec<&str> = lines.filter(|l| !l.trim().is_empty()).collect();
    let total = all.len();
    let start = total.saturating_sub(limit);
    let mut rows = Vec::with_capacity(total - start);
    let mut skipped = 0usize;
    for line in &all[start..] {
        match parse_line(line) {
            Some(row) => rows.push(row),
            None => skipped += 1,
        }
    }

    axum::Json(serde_json::json!({
        "present": true,
        "exchange": ask.exchange,
        "symbol": ask.symbol,
        "interval": ask.interval,
        "total": total,
        "skipped": skipped,
        "rows": rows,
    }))
}

/// What the candle store actually holds, so the board can offer real choices
/// instead of a dropdown of guesses.
pub async fn available(State(config): State<Config>) -> axum::Json<serde_json::Value> {
    let mut found: Vec<serde_json::Value> = Vec::new();
    let root = config.candles.clone();

    let mut exchanges = match tokio::fs::read_dir(&root).await {
        Ok(dir) => dir,
        Err(_) => {
            return axum::Json(serde_json::json!({
                "present": false,
                "reason": format!("ยังไม่มีคลังแท่งเทียนที่ {}", root.display()),
                "rows": [],
            }))
        }
    };

    while let Ok(Some(exchange)) = exchanges.next_entry().await {
        let Ok(mut symbols) = tokio::fs::read_dir(exchange.path()).await else {
            continue;
        };
        while let Ok(Some(symbol)) = symbols.next_entry().await {
            let Ok(mut files) = tokio::fs::read_dir(symbol.path()).await else {
                continue;
            };
            while let Ok(Some(file)) = files.next_entry().await {
                let path = file.path();
                if path.extension().and_then(|e| e.to_str()) != Some("csv") {
                    continue;
                }
                let bytes = file.metadata().await.map(|m| m.len()).unwrap_or(0);
                found.push(serde_json::json!({
                    "exchange": exchange.file_name().to_string_lossy(),
                    "symbol": symbol.file_name().to_string_lossy(),
                    "interval": path.file_stem().map(|s| s.to_string_lossy().into_owned()),
                    "bytes": bytes,
                }));
            }
        }
    }

    axum::Json(serde_json::json!({ "present": true, "rows": found }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_name_is_allowed() {
        assert_eq!(safe_name("BTC_USDT_Perp").as_deref(), Some("BTC_USDT_Perp"));
        assert_eq!(safe_name("5m").as_deref(), Some("5m"));
        assert_eq!(safe_name(" grvt ").as_deref(), Some("grvt"));
    }

    #[test]
    fn a_name_that_could_leave_the_candle_directory_is_refused() {
        for attack in ["..", ".", "../etc", "a/b", "a\\b", "", "   ", ".hidden"] {
            assert!(safe_name(attack).is_none(), "ควรปฏิเสธ: {attack:?}");
        }
        assert!(safe_name(&"x".repeat(65)).is_none());
    }

    #[test]
    fn a_csv_line_becomes_a_row_with_its_text_untouched() {
        let row = parse_line("2025-10-08T07:11:22.060019+00:00,100000,100001,99999,100000.5,0")
            .expect("should parse");
        assert_eq!(row.t, "2025-10-08T07:11:22.060019+00:00");
        assert_eq!(row.c, "100000.5", "ราคาต้องไม่ถูกปัดระหว่างทาง");
        assert_eq!(row.v, "0");
    }

    #[test]
    fn a_line_with_the_wrong_shape_is_skipped_not_guessed_at() {
        assert!(parse_line("only,three,fields").is_none());
        assert!(parse_line("a,b,c,d,e,f,g").is_none());
        assert!(parse_line(",1,1,1,1,1").is_none());
        assert!(parse_line("").is_none());
    }

    #[test]
    fn the_path_is_built_the_same_way_the_bot_builds_it() {
        let root = Path::new("data/candles");
        let path = path_for(root, "grvt", "BTC_USDT_Perp", "5m").unwrap();
        assert!(
            path.ends_with("grvt/BTC_USDT_Perp/5m.csv")
                || path.ends_with("grvt\\BTC_USDT_Perp\\5m.csv")
        );
    }
}
