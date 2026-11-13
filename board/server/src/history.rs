//! What happened before now: past runs, and what each day came to.
//!
//! Two routes over the two files the bot appends to.
//!
//! `/api/runs` pairs the opening and closing lines of `logs/runs.jsonl` by
//! id. A run with no closing line is not an error to be hidden - it is a run
//! that was killed, and saying so is the only way a person can tell "stopped
//! cleanly" from "the machine went away".
//!
//! `/api/days` walks **the whole** `logs/audit.jsonl` rather than the tail
//! the board is normally handed. A calendar whose older squares were empty
//! because the board only reads 500 rows would be a calendar that lies about
//! the past, which is the one thing a calendar is for.
//!
//! Money stays as the bot wrote it: strings at full precision. The `change`
//! field is the only number parsed here, and only because a day has to be
//! subtracted from itself to have a colour.

use axum::extract::State;
use axum::Json;
use serde_json::{json, Map, Value};

use crate::Config;

/// How many runs travel. Enough to scroll through an afternoon of them.
const RUN_CAP: usize = 200;

/// How many days travel. Half a year of squares is more than a card can draw.
const DAY_CAP: usize = 200;

pub async fn runs(State(config): State<Config>) -> Json<Value> {
    let path = config.logs.join("runs.jsonl");
    let Ok(text) = tokio::fs::read_to_string(&path).await else {
        return Json(json!({
            "present": false,
            "reason": format!("ยังไม่มี {} · รันบอทหนึ่งรอบแล้วจะมีประวัติ", path.display()),
        }));
    };
    Json(collect_runs(&text))
}

pub async fn days(State(config): State<Config>) -> Json<Value> {
    let path = config.logs.join("audit.jsonl");
    let Ok(text) = tokio::fs::read_to_string(&path).await else {
        return Json(json!({
            "present": false,
            "reason": format!("ยังไม่มี {}", path.display()),
        }));
    };
    Json(collect_days(&text, config.utc_offset_hours))
}

/// Pair the start and end lines, newest first.
pub fn collect_runs(text: &str) -> Value {
    let mut order: Vec<String> = Vec::new();
    let mut runs: std::collections::BTreeMap<String, Map<String, Value>> = Default::default();
    let mut unreadable = 0usize;

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(Value::Object(row)) = serde_json::from_str::<Value>(line) else {
            unreadable += 1;
            continue;
        };
        let Some(id) = row.get("id").and_then(|id| id.as_str()).map(String::from) else {
            unreadable += 1;
            continue;
        };
        let event = row.get("event").and_then(|e| e.as_str()).unwrap_or("");
        let run = runs.entry(id.clone()).or_insert_with(|| {
            order.push(id.clone());
            let mut fresh = Map::new();
            fresh.insert("id".into(), Value::String(id.clone()));
            // Until an end line turns up, this run is either still going or
            // was killed. Either way it has not finished.
            fresh.insert("finished".into(), Value::Bool(false));
            fresh
        });
        match event {
            "start" => {
                for key in [
                    "at",
                    "strategy",
                    "exchange",
                    "symbol",
                    "timeframe",
                    "mode",
                    "config",
                    "iterations",
                ] {
                    if let Some(value) = row.get(key) {
                        let name = if key == "at" { "started_at" } else { key };
                        run.insert(name.into(), value.clone());
                    }
                }
            }
            "end" => {
                run.insert("finished".into(), Value::Bool(true));
                for key in [
                    "at",
                    "ticks",
                    "orders",
                    "exits",
                    "rejects",
                    "first_equity",
                    "last_equity",
                    "halted",
                    "reason",
                ] {
                    if let Some(value) = row.get(key) {
                        let name = if key == "at" { "ended_at" } else { key };
                        run.insert(name.into(), value.clone());
                    }
                }
                let change = change_of(run);
                if let Some(change) = change {
                    run.insert("change".into(), json!(change));
                }
            }
            _ => unreadable += 1,
        }
    }

    // Newest first, which is the end a person reads from.
    let mut out: Vec<Value> = order
        .iter()
        .rev()
        .filter_map(|id| runs.get(id).cloned().map(Value::Object))
        .collect();
    let total = out.len();
    out.truncate(RUN_CAP);
    json!({
        "present": true,
        "runs": out,
        "total": total,
        "unreadable": unreadable,
    })
}

fn change_of(run: &Map<String, Value>) -> Option<f64> {
    let first: f64 = run.get("first_equity")?.as_str()?.parse().ok()?;
    let last: f64 = run.get("last_equity")?.as_str()?.parse().ok()?;
    Some(last - first)
}

/// One entry per calendar day, oldest first, in the timezone `offset`
/// hours east of UTC.
pub fn collect_days(text: &str, offset: i64) -> Value {
    let mut order: Vec<String> = Vec::new();
    let mut days: std::collections::BTreeMap<String, Day> = Default::default();

    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let Some(at) = row.get("at").and_then(|at| at.as_str()) else {
            continue;
        };
        // The date in the reader's timezone, not the one in the string: the
        // bot writes UTC, and in Bangkok a UTC day ends at seven in the
        // morning - so a calendar of UTC days puts every evening's trading
        // on the wrong square.
        let Some(date) = local_date(at, offset) else {
            continue;
        };
        let day = days.entry(date.clone()).or_insert_with(|| {
            order.push(date.clone());
            Day::default()
        });
        day.ticks += 1;
        if row.get("order").is_some() {
            day.orders += 1;
        }
        if row.get("exit").is_some() {
            day.exits += 1;
        }
        if row.get("reject").is_some() {
            day.rejects += 1;
        }
        if row.get("error").is_some() {
            day.errors += 1;
        }
        if let Some(equity) = row.get("equity").and_then(|e| e.as_str()) {
            // A tick that could not read the account writes a zero, and a
            // zero would make the day look like a total loss.
            if equity.parse::<f64>().unwrap_or(0.0) > 0.0 {
                if day.first.is_empty() {
                    day.first = equity.to_string();
                }
                day.last = equity.to_string();
            }
        }
    }

    let mut out: Vec<Value> = Vec::new();
    for date in &order {
        let day = &days[date];
        let change = match (day.first.parse::<f64>(), day.last.parse::<f64>()) {
            (Ok(first), Ok(last)) => Some(last - first),
            _ => None,
        };
        out.push(json!({
            "date": date,
            "ticks": day.ticks,
            "orders": day.orders,
            "exits": day.exits,
            "rejects": day.rejects,
            "errors": day.errors,
            "first_equity": day.first,
            "last_equity": day.last,
            "change": change,
        }));
    }
    let total = out.len();
    if total > DAY_CAP {
        out.drain(..total - DAY_CAP);
    }
    json!({
        "present": true,
        "days": out,
        "total": total,
    })
}

/// `2026-11-13T17:05:00+00:00` -> `2026-11-14` for an offset of +7.
///
/// Reads the timestamp's own offset (`Z`, `+00:00`, `+0000` or none, which
/// this project's writers only ever mean as UTC) and moves it `offset`
/// hours east. `None` for anything that is not a timestamp.
pub fn local_date(at: &str, offset: i64) -> Option<String> {
    let (date, rest) = at.trim().split_once('T')?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let (clock, written) = match rest.strip_suffix('Z') {
        Some(clock) => (clock, 0),
        None => match rest.rfind(['+', '-']) {
            Some(at) => {
                let (clock, sign_and_digits) = rest.split_at(at);
                let sign = if sign_and_digits.starts_with('-') {
                    -1
                } else {
                    1
                };
                let digits = sign_and_digits[1..].replace(':', "");
                let hours: i64 = digits.get(..2)?.parse().ok()?;
                let minutes: i64 = digits.get(2..4).unwrap_or("00").parse().ok()?;
                (clock, sign * (hours * 60 + minutes))
            }
            None => (rest, 0),
        },
    };
    let mut clock_parts = clock.split(':');
    let hour: i64 = clock_parts.next()?.parse().ok()?;
    let minute: i64 = clock_parts.next()?.parse().ok()?;
    let minutes =
        days_from_civil(year, month, day) * 1440 + hour * 60 + minute - written + offset * 60;
    let (y, m, d) = civil_from_days(minutes.div_euclid(1440));
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Days since 1970-01-01. Howard Hinnant's algorithm, the same one the
/// board's `time.rs` uses, so the two can never disagree about a date.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (if month <= 2 { y + 1 } else { y }, month, day)
}

#[derive(Default)]
struct Day {
    ticks: u64,
    orders: u64,
    exits: u64,
    rejects: u64,
    errors: u64,
    first: String,
    last: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = r#"
{"id":"aaa","event":"start","at":"2026-11-13T16:47:00+00:00","strategy":"ping_pong","exchange":"grvt","mode":"grvt testnet","iterations":10}
{"id":"aaa","event":"end","at":"2026-11-13T16:48:01+00:00","ticks":10,"orders":1,"exits":1,"rejects":0,"first_equity":"9966.404007","last_equity":"9957.442304","halted":false,"reason":"finished"}
{"id":"bbb","event":"start","at":"2026-11-13T17:00:00+00:00","strategy":"ma_cross","exchange":"grvt","mode":"paper","iterations":null}
"#;

    #[test]
    fn a_run_is_its_opening_line_and_its_closing_one_joined() {
        let value = collect_runs(LOG);
        let runs = value["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2);
        let done = runs.iter().find(|run| run["id"] == "aaa").unwrap();
        assert_eq!(done["strategy"], "ping_pong");
        assert_eq!(done["ticks"], 10);
        assert_eq!(done["finished"], true);
        assert_eq!(done["started_at"], "2026-11-13T16:47:00+00:00");
        assert_eq!(done["ended_at"], "2026-11-13T16:48:01+00:00");
    }

    #[test]
    fn the_newest_run_comes_first() {
        let value = collect_runs(LOG);
        assert_eq!(value["runs"][0]["id"], "bbb");
    }

    /// A process that was killed never wrote its second line. Saying
    /// "unfinished" is the point; hiding it would turn a killed bot into a
    /// bot that was never started.
    #[test]
    fn a_run_with_no_closing_line_is_marked_unfinished_rather_than_dropped() {
        let value = collect_runs(LOG);
        let open = value["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["id"] == "bbb")
            .unwrap();
        assert_eq!(open["finished"], false);
        assert!(open.get("ticks").is_none(), "ยังไม่รู้ว่าเดินไปกี่รอบ");
    }

    #[test]
    fn what_a_run_made_is_worked_out_from_the_two_equities() {
        let value = collect_runs(LOG);
        let done = value["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|run| run["id"] == "aaa")
            .unwrap();
        let change = done["change"].as_f64().unwrap();
        assert!((change - -8.961703).abs() < 1e-6, "{change}");
    }

    #[test]
    fn a_half_written_line_is_counted_rather_than_fatal() {
        let value = collect_runs("{\"id\":\"aaa\",\"event\":\"start\"}\n{not json\n");
        assert_eq!(value["unreadable"], 1);
        assert_eq!(value["runs"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn no_runs_at_all_is_an_empty_list_not_a_failure() {
        let value = collect_runs("");
        assert_eq!(value["present"], true);
        assert!(value["runs"].as_array().unwrap().is_empty());
    }

    const TRAIL: &str = r#"
{"at":"2026-11-12T10:00:00+00:00","equity":"10000","symbol":"BTC"}
{"at":"2026-11-12T10:00:05+00:00","equity":"10010","order":{"side":"buy"}}
{"at":"2026-11-13T09:00:00+00:00","equity":"9966.40"}
{"at":"2026-11-13T09:00:05+00:00","equity":"0","error":"broker read failed"}
{"at":"2026-11-13T09:00:10+00:00","equity":"9957.44","exit":{"reason":"back to the average"}}
"#;

    #[test]
    fn a_day_is_counted_from_the_date_the_bot_wrote() {
        let value = collect_days(TRAIL, 0);
        let days = value["days"].as_array().unwrap();
        assert_eq!(days.len(), 2);
        assert_eq!(days[0]["date"], "2026-11-12");
        assert_eq!(days[1]["date"], "2026-11-13");
    }

    #[test]
    fn a_day_counts_what_the_bot_did_that_day() {
        let value = collect_days(TRAIL, 0);
        let second = &value["days"][1];
        assert_eq!(second["ticks"], 3);
        assert_eq!(second["exits"], 1);
        assert_eq!(second["errors"], 1);
        assert_eq!(second["orders"], 0);
    }

    /// A tick that could not read the account writes a zero. Letting that
    /// in would draw the day as a total loss.
    #[test]
    fn an_unreadable_equity_does_not_become_the_end_of_the_day() {
        let value = collect_days(TRAIL, 0);
        let second = &value["days"][1];
        assert_eq!(second["first_equity"], "9966.40");
        assert_eq!(second["last_equity"], "9957.44");
        let change = second["change"].as_f64().unwrap();
        assert!((change - -8.96).abs() < 1e-6, "{change}");
    }

    #[test]
    fn a_day_with_no_usable_equity_has_no_change_rather_than_a_zero_one() {
        let value = collect_days("{\"at\":\"2026-11-13T09:00:00+00:00\",\"equity\":\"0\"}", 0);
        assert!(value["days"][0]["change"].is_null(), "วัดไม่ได้ ไม่ใช่ศูนย์");
        assert_eq!(value["days"][0]["ticks"], 1);
    }

    // ------------------------------------------------------ Thai days

    /// Five past five in the evening UTC is five past midnight in Bangkok.
    /// A calendar of UTC days would put that trade on yesterday's square.
    #[test]
    fn an_evening_trade_in_utc_lands_on_tomorrow_s_square_in_thailand() {
        let trail = "{\"at\":\"2026-11-13T16:00:00+00:00\",\"equity\":\"100\"}
                     {\"at\":\"2026-11-13T17:05:00+00:00\",\"equity\":\"101\"}
";
        let thai = collect_days(trail, 7);
        let dates: Vec<&str> = thai["days"]
            .as_array()
            .unwrap()
            .iter()
            .map(|day| day["date"].as_str().unwrap())
            .collect();
        assert_eq!(dates, vec!["2026-11-13", "2026-11-14"]);
        let utc = collect_days(trail, 0);
        assert_eq!(
            utc["days"].as_array().unwrap().len(),
            1,
            "ใน UTC เป็นวันเดียวกัน"
        );
    }

    #[test]
    fn every_offset_spelling_reaches_the_same_thai_date() {
        for at in [
            "2026-11-13T17:05:00Z",
            "2026-11-13T17:05:00+00:00",
            "2026-11-13T17:05:00.123456+00:00",
            "2026-11-13T17:05:00",
            "2026-11-14T00:05:00+07:00",
            "2026-11-13T12:05:00-05:00",
        ] {
            assert_eq!(local_date(at, 7).as_deref(), Some("2026-11-14"), "{at}");
        }
    }

    #[test]
    fn months_and_years_roll_over_with_the_offset() {
        assert_eq!(
            local_date("2026-11-30T20:00:00Z", 7).as_deref(),
            Some("2026-12-01")
        );
        assert_eq!(
            local_date("2026-12-31T23:00:00Z", 7).as_deref(),
            Some("2027-01-01")
        );
        assert_eq!(
            local_date("2028-02-28T20:00:00Z", 7).as_deref(),
            Some("2028-02-29")
        );
    }

    #[test]
    fn a_timestamp_that_is_not_one_has_no_date() {
        assert_eq!(local_date("nonsense", 7), None);
        assert_eq!(local_date("2026-13-01T00:00:00Z", 7), None);
    }
}
