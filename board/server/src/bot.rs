//! What the bot wrote, cut down to what a screen can hold.
//!
//! Two of the three files the board reads are small: `logs/state.json` is a
//! dozen lines, and only the tail of `logs/audit.jsonl` is ever wanted.
//! `logs/backtest.json` is the problem - it holds one equity point per bar,
//! so a 400-day run on 5m candles is 115,198 points in an 11 MB file. Handing
//! that to a browser that is going to draw a card 400 pixels wide is a waste
//! on both ends, so it is thinned here, once, on the way out.
//!
//! **The thinning keeps the lowest and highest point of every bucket.**
//! Taking every nth point would drop the bottom of a drawdown that happened
//! between two samples, and the curve on screen would then be smoother than
//! the account ever was. A chart may be coarse; it may not flatter.
//!
//! Numbers stay exactly as the file wrote them: strings, full Decimal
//! precision. Nothing here parses money except to compare two points, and
//! even then it is the original string that travels on.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Map, Value};

use crate::Config;

/// How many equity points survive. Two per bucket (the low and the high), so
/// this is the cap, not the count - about 600 points draws a curve that is
/// indistinguishable from the full one at any size a card can be.
const CURVE_CAP: usize = 300;

/// How many closed trades travel. The newest ones, because that is the end a
/// person reads from.
const TRADE_CAP: usize = 200;

pub async fn backtest(State(config): State<Config>) -> Json<Value> {
    let path = config.logs.join("backtest.json");
    let text = match tokio::fs::read_to_string(&path).await {
        Ok(text) => text,
        Err(_) => {
            return Json(json!({
                "present": false,
                "reason": format!("ยังไม่มี {}", path.display()),
            }))
        }
    };
    // A half-written file is the normal state of affairs while a new run is
    // in progress, not a failure of the board. Say which it is and let the
    // page keep showing the last result it managed to read.
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            return Json(json!({
                "present": false,
                "reason": format!("ไฟล์มีแต่อ่านไม่ออก (อาจกำลังถูกเขียนใหม่): {err}"),
            }))
        }
    };
    Json(shrink(parsed))
}

/// Cut the two long lists down and say how much was cut. Pure, so the shape
/// of the answer can be tested without a filesystem or a socket.
pub fn shrink(raw: Value) -> Value {
    let Value::Object(mut data) = raw else {
        return json!({
            "present": false,
            "reason": "ไฟล์ไม่ใช่ออบเจกต์ JSON".to_string(),
        });
    };

    let (curve_total, curve_kept) = shrink_curve(&mut data);
    let (trades_total, trades_kept) = shrink_trades(&mut data);

    json!({
        "present": true,
        "data": Value::Object(data),
        "curve_total": curve_total,
        "curve_kept": curve_kept,
        "trades_total": trades_total,
        "trades_kept": trades_kept,
    })
}

fn shrink_curve(data: &mut Map<String, Value>) -> (usize, usize) {
    let Some(Value::Array(points)) = data.get("equity_curve") else {
        return (0, 0);
    };
    // A point whose equity will not parse cannot be placed on a chart, and
    // defaulting it to zero would rescale the whole card.
    let usable: Vec<Value> = points
        .iter()
        .filter(|point| equity_of(point).is_some())
        .cloned()
        .collect();
    let total = usable.len();
    let kept = thin(usable, CURVE_CAP);
    let count = kept.len();
    data.insert("equity_curve".into(), Value::Array(kept));
    (total, count)
}

fn shrink_trades(data: &mut Map<String, Value>) -> (usize, usize) {
    let Some(Value::Array(trades)) = data.get("trade_list") else {
        return (0, 0);
    };
    let total = trades.len();
    let kept: Vec<Value> = trades
        .iter()
        .skip(total.saturating_sub(TRADE_CAP))
        .cloned()
        .collect();
    let count = kept.len();
    data.insert("trade_list".into(), Value::Array(kept));
    (total, count)
}

/// The equity of one point, for comparison only.
///
/// Accepts a JSON number as well as the string the bot writes, because a
/// reader that only understood one of the two would be a trap for whoever
/// changes the writer.
fn equity_of(point: &Value) -> Option<f64> {
    let value = point.get("equity")?;
    let parsed = match value {
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        Value::Number(number) => number.as_f64()?,
        _ => return None,
    };
    parsed.is_finite().then_some(parsed)
}

/// Keep the lowest and the highest point of each bucket, in the order they
/// happened, plus the first and last point of the run.
pub fn thin(points: Vec<Value>, cap: usize) -> Vec<Value> {
    if cap == 0 || points.len() <= cap {
        return points;
    }
    let size = points.len() as f64 / cap as f64;
    let mut out: Vec<Value> = Vec::with_capacity(cap * 2 + 2);

    for i in 0..cap {
        let from = (i as f64 * size).floor() as usize;
        let to = (((i + 1) as f64 * size).floor() as usize).min(points.len());
        if to <= from {
            continue;
        }
        let mut lo = from;
        let mut hi = from;
        for j in from + 1..to {
            let value = equity_of(&points[j]).unwrap_or(f64::NAN);
            if value < equity_of(&points[lo]).unwrap_or(f64::INFINITY) {
                lo = j;
            }
            if value > equity_of(&points[hi]).unwrap_or(f64::NEG_INFINITY) {
                hi = j;
            }
        }
        let (first, second) = (lo.min(hi), lo.max(hi));
        out.push(points[first].clone());
        if second != first {
            out.push(points[second].clone());
        }
    }

    // The ends of the run are the two points a person reads first: where the
    // account started and where it ended up.
    if out.first() != points.first() {
        out.insert(0, points[0].clone());
    }
    if out.last() != points.last() {
        out.push(points[points.len() - 1].clone());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(at: &str, equity: &str) -> Value {
        json!({ "at": at, "equity": equity })
    }

    fn curve(values: &[f64]) -> Vec<Value> {
        values
            .iter()
            .enumerate()
            .map(|(i, v)| point(&format!("t{i}"), &v.to_string()))
            .collect()
    }

    #[test]
    fn a_curve_short_enough_to_draw_is_left_alone() {
        let points = curve(&[1.0, 2.0, 3.0]);
        assert_eq!(thin(points.clone(), 300), points);
    }

    #[test]
    fn thinning_keeps_the_bottom_of_a_drawdown_that_happens_between_samples() {
        // 100 flat points with one crash in the middle. Taking every nth
        // point would miss the crash entirely; this must not.
        let mut values = vec![10_000.0; 100];
        values[47] = 4_000.0;
        let kept = thin(curve(&values), 10);
        let lowest = kept
            .iter()
            .filter_map(equity_of)
            .fold(f64::INFINITY, f64::min);
        assert_eq!(lowest, 4_000.0, "ก้นของ drawdown ต้องไม่หายไปตอนย่อ");
    }

    #[test]
    fn thinning_keeps_the_first_and_last_point() {
        let values: Vec<f64> = (0..500).map(|i| 10_000.0 + i as f64).collect();
        let kept = thin(curve(&values), 50);
        assert_eq!(equity_of(&kept[0]), Some(10_000.0));
        assert_eq!(equity_of(kept.last().unwrap()), Some(10_499.0));
    }

    #[test]
    fn thinning_keeps_time_moving_forwards() {
        let values: Vec<f64> = (0..400)
            .map(|i| (i as f64 * 0.7).sin() * 100.0 + 10_000.0)
            .collect();
        let kept = thin(curve(&values), 40);
        let order: Vec<usize> = kept
            .iter()
            .map(|p| {
                p.get("at")
                    .and_then(|v| v.as_str())
                    .and_then(|s| s.trim_start_matches('t').parse().ok())
                    .expect("ทุกจุดต้องมีเวลา")
            })
            .collect();
        let mut sorted = order.clone();
        sorted.sort_unstable();
        assert_eq!(order, sorted, "จุดที่เก็บไว้ต้องเรียงตามเวลาเดิม");
    }

    #[test]
    fn the_numbers_that_travel_are_the_file_s_own_strings() {
        let raw = json!({
            "net_pnl": "-9.0000000",
            "equity_curve": [point("t0", "10000.0000001")],
            "trade_list": [],
        });
        let out = shrink(raw);
        let data = &out["data"];
        assert_eq!(data["net_pnl"], json!("-9.0000000"));
        assert_eq!(
            data["equity_curve"][0]["equity"],
            json!("10000.0000001"),
            "ตัวเลขต้องไม่ถูกแปลงเป็น number แล้วปัดทิ้ง"
        );
    }

    #[test]
    fn a_point_with_an_unreadable_equity_is_dropped_not_turned_into_zero() {
        let raw = json!({
            "equity_curve": [point("t0", "10000"), point("t1", ""), point("t2", "x")],
        });
        let out = shrink(raw);
        assert_eq!(out["curve_total"], json!(1));
        assert_eq!(out["data"]["equity_curve"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn only_the_newest_trades_travel_and_the_count_says_how_many_did_not() {
        let trades: Vec<Value> = (0..250).map(|i| json!({ "side": "buy", "n": i })).collect();
        let out = shrink(json!({ "trade_list": trades }));
        assert_eq!(out["trades_total"], json!(250));
        assert_eq!(out["trades_kept"], json!(TRADE_CAP));
        let kept = out["data"]["trade_list"].as_array().unwrap();
        assert_eq!(kept[0]["n"], json!(50), "ต้องเก็บไม้ใหม่สุด ไม่ใช่เก่าสุด");
        assert_eq!(kept[kept.len() - 1]["n"], json!(249));
    }

    #[test]
    fn a_report_with_neither_list_still_comes_back_whole() {
        let out = shrink(json!({ "symbol": "BTC_USDT_Perp", "trades": 0 }));
        assert_eq!(out["present"], json!(true));
        assert_eq!(out["data"]["symbol"], json!("BTC_USDT_Perp"));
        assert_eq!(out["curve_total"], json!(0));
    }

    #[test]
    fn something_that_is_not_a_json_object_is_refused_rather_than_reshaped() {
        let out = shrink(json!([1, 2, 3]));
        assert_eq!(out["present"], json!(false));
        assert!(out["reason"].as_str().unwrap().contains("ออบเจกต์"));
    }
}
