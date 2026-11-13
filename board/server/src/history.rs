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
    let recorded = tokio::fs::read_to_string(config.logs.join("runs.jsonl"))
        .await
        .unwrap_or_default();
    let trail = tokio::fs::read_to_string(config.logs.join("audit.jsonl"))
        .await
        .unwrap_or_default();
    if recorded.trim().is_empty() && trail.trim().is_empty() {
        return Json(json!({
            "present": false,
            "reason": "ยังไม่มีทั้งสมุดรอบการรันและ audit · รันบอทหนึ่งรอบแล้วจะมีประวัติ",
        }));
    }
    let mut value = collect_runs(&recorded);
    merge_inferred(&mut value, &trail);
    Json(value)
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

/// The shortest silence that counts as "the bot stopped and was started
/// again", in seconds. Two minutes, or five ticks of the trail's own usual
/// pace, whichever is longer - so a bot looping every five minutes is not
/// split at every tick.
const GAP_FLOOR: i64 = 120;
const GAP_TICKS: i64 = 5;

/// Add the runs the audit trail shows that no line of the run log covers.
///
/// The run log only exists from the version of the bot that writes it; a
/// trail older than that would otherwise have no history at all. Those
/// runs are found by the silences between ticks and marked `inferred`,
/// because a silence is evidence of a stop, not a record of one. What they
/// ran is read off the trace where there is one - the strategy file the
/// tick went into - and left empty where there is not.
pub fn merge_inferred(value: &mut Value, trail: &str) {
    let recorded: Vec<(i64, Option<i64>)> = value["runs"]
        .as_array()
        .map(|runs| {
            runs.iter()
                .filter_map(|run| {
                    let start = epoch_seconds(run["started_at"].as_str()?)?;
                    let end = run["ended_at"].as_str().and_then(epoch_seconds);
                    Some((start, end))
                })
                .collect()
        })
        .unwrap_or_default();

    let sessions = sessions_of(trail);
    let mut added: Vec<Value> = Vec::new();
    for session in sessions {
        let covered = recorded.iter().any(|(start, end)| {
            *start <= session.last && end.map(|end| end >= session.first).unwrap_or(true)
        });
        if !covered {
            added.push(session.into_json());
        }
    }
    if added.is_empty() {
        return;
    }
    let Some(runs) = value["runs"].as_array_mut() else {
        return;
    };
    runs.extend(added);
    // Newest first across both kinds, by when they started.
    runs.sort_by_key(|run| {
        std::cmp::Reverse(
            run["started_at"]
                .as_str()
                .and_then(epoch_seconds)
                .unwrap_or(i64::MIN),
        )
    });
    runs.truncate(RUN_CAP);
    let total = runs.len();
    value["total"] = json!(total);
}

/// One stretch of the trail with no long silence in it.
#[derive(Debug, Default)]
struct Session {
    first: i64,
    last: i64,
    started_at: String,
    ended_at: String,
    ticks: u64,
    orders: u64,
    exits: u64,
    rejects: u64,
    first_equity: String,
    last_equity: String,
    paper: bool,
    live: bool,
    strategy: String,
}

impl Session {
    fn into_json(self) -> Value {
        let change = match (
            self.first_equity.parse::<f64>(),
            self.last_equity.parse::<f64>(),
        ) {
            (Ok(first), Ok(last)) => Some(last - first),
            _ => None,
        };
        let mode = match (self.live, self.paper) {
            (true, false) => "live",
            (false, true) => "paper",
            (true, true) => "live + paper",
            (false, false) => "",
        };
        json!({
            "id": format!("audit-{}", self.first),
            "inferred": true,
            "finished": true,
            "started_at": self.started_at,
            "ended_at": self.ended_at,
            "strategy": self.strategy,
            "mode": mode,
            "ticks": self.ticks,
            "orders": self.orders,
            "exits": self.exits,
            "rejects": self.rejects,
            "first_equity": self.first_equity,
            "last_equity": self.last_equity,
            "change": change,
            "halted": false,
            "reason": "เดาจากช่วงเงียบใน audit",
        })
    }
}

/// Cut the trail at every silence long enough to be a stop.
fn sessions_of(trail: &str) -> Vec<Session> {
    let rows: Vec<(i64, Value)> = trail
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|row| Some((epoch_seconds(row.get("at")?.as_str()?)?, row)))
        .collect();
    if rows.is_empty() {
        return Vec::new();
    }
    let mut gaps: Vec<i64> = rows.windows(2).map(|pair| pair[1].0 - pair[0].0).collect();
    gaps.sort_unstable();
    let usual = gaps.get(gaps.len() / 2).copied().unwrap_or(0);
    let gap = GAP_FLOOR.max(usual * GAP_TICKS);

    let mut out: Vec<Session> = Vec::new();
    let mut previous: Option<i64> = None;
    for (seconds, row) in rows {
        let new_session = match previous {
            Some(before) => seconds - before > gap,
            None => true,
        };
        if new_session {
            out.push(Session {
                first: seconds,
                started_at: text(&row, "at"),
                ..Default::default()
            });
        }
        let session = out.last_mut().expect("a session was just started");
        session.last = seconds;
        session.ended_at = text(&row, "at");
        session.ticks += 1;
        if row.get("order").is_some() {
            session.orders += 1;
        }
        if row.get("exit").is_some() {
            session.exits += 1;
        }
        if row.get("reject").is_some() {
            session.rejects += 1;
        }
        if let Some(receipt) = row.get("receipt") {
            if receipt["simulated"].as_bool().unwrap_or(false) {
                session.paper = true;
            } else {
                session.live = true;
            }
        }
        let equity = text(&row, "equity");
        if equity.parse::<f64>().map(|e| e > 0.0).unwrap_or(false) {
            if session.first_equity.is_empty() {
                session.first_equity = equity.clone();
            }
            session.last_equity = equity;
        }
        if session.strategy.is_empty() {
            session.strategy = strategy_in(&row);
        }
        previous = Some(seconds);
    }
    out
}

fn text(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string()
}

/// The strategy a tick ran, read off the strategy file its trace entered:
/// `tradebot/strategies/ping_pong.py` -> `ping_pong`. The base class is
/// not a strategy, so it does not count.
fn strategy_in(row: &Value) -> String {
    let Some(calls) = row
        .pointer("/trace/calls")
        .and_then(|calls| calls.as_array())
    else {
        return String::new();
    };
    calls
        .iter()
        .filter_map(|call| call["file"].as_str())
        .filter_map(|file| file.strip_prefix("tradebot/strategies/"))
        .filter_map(|file| file.strip_suffix(".py"))
        .find(|name| *name != "base" && *name != "__init__")
        .map(|name| name.trim_end_matches("_strategy").to_string())
        .unwrap_or_default()
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
    let seconds = epoch_seconds(at)? + offset * 3600;
    let (y, m, d) = civil_from_days(seconds.div_euclid(86_400));
    Some(format!("{y:04}-{m:02}-{d:02}"))
}

/// Seconds since 1970-01-01 UTC for an ISO timestamp, reading its own
/// offset (`Z`, `+00:00`, `+0000`, or none - which this project's writers
/// only ever mean as UTC).
pub fn epoch_seconds(at: &str) -> Option<i64> {
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
    let second: i64 = clock_parts
        .next()
        .and_then(|part| part.split('.').next())
        .unwrap_or("0")
        .parse()
        .ok()?;
    Some(
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
            - written * 60,
    )
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

    // ------------------------------------------------- runs from the trail

    /// Two sessions an hour apart, the second one trading.
    const TWO_SESSIONS: &str = r#"{"at":"2026-11-13T08:59:00+00:00","equity":"10000"}
{"at":"2026-11-13T08:59:05+00:00","equity":"10000"}
{"at":"2026-11-13T08:59:10+00:00","equity":"10000"}
{"at":"2026-11-13T16:47:06+00:00","equity":"9966.40","trace":{"calls":[{"file":"tradebot/strategies/base.py"},{"file":"tradebot/strategies/ping_pong.py"}]}}
{"at":"2026-11-13T16:47:11+00:00","equity":"9966.40","order":{"qty":"0.145"},"receipt":{"simulated":false}}
{"at":"2026-11-13T16:48:01+00:00","equity":"9957.44","exit":{"reason":"back"}}
"#;

    #[test]
    fn a_trail_with_no_run_log_still_has_a_history() {
        let mut value = collect_runs("");
        merge_inferred(&mut value, TWO_SESSIONS);
        let runs = value["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2, "split at the long silence");
        assert!(runs.iter().all(|run| run["inferred"] == true));
    }

    #[test]
    fn an_inferred_run_says_what_it_did_and_what_it_ran() {
        let mut value = collect_runs("");
        merge_inferred(&mut value, TWO_SESSIONS);
        let newest = &value["runs"][0];
        assert_eq!(newest["started_at"], "2026-11-13T16:47:06+00:00");
        assert_eq!(newest["ticks"], 3);
        assert_eq!(newest["orders"], 1);
        assert_eq!(newest["exits"], 1);
        assert_eq!(newest["mode"], "live", "a receipt that was not simulated");
        assert_eq!(
            newest["strategy"], "ping_pong",
            "read off the trace, not guessed"
        );
        let change = newest["change"].as_f64().unwrap();
        assert!((change - -8.96).abs() < 1e-6, "{change}");
    }

    #[test]
    fn a_session_with_no_trace_does_not_invent_a_strategy() {
        let mut value = collect_runs("");
        merge_inferred(&mut value, TWO_SESSIONS);
        assert_eq!(value["runs"][1]["strategy"], "");
    }

    /// Where the run log covers a stretch of the trail, the log is the
    /// record and the silence-guess must not appear beside it.
    #[test]
    fn a_stretch_the_run_log_covers_is_not_guessed_at_twice() {
        let log = r#"{"id":"aaa","event":"start","at":"2026-11-13T16:47:00+00:00","strategy":"ping_pong"}
{"id":"aaa","event":"end","at":"2026-11-13T16:48:05+00:00","ticks":3}
"#;
        let mut value = collect_runs(log);
        merge_inferred(&mut value, TWO_SESSIONS);
        let runs = value["runs"].as_array().unwrap();
        assert_eq!(runs.len(), 2, "the logged run and the older guessed one");
        assert_eq!(runs[0]["id"], "aaa", "the record, not a guess");
        assert_eq!(runs[1]["inferred"], true);
    }

    /// A bot looping every five minutes is not stopped between ticks.
    #[test]
    fn a_slow_bot_is_not_split_at_every_tick() {
        let slow: String = (0..6)
            .map(|index| {
                format!(
                    "{{\"at\":\"2026-11-13T{:02}:00:00+00:00\",\"equity\":\"1\"}}\n",
                    10 + index
                )
            })
            .collect();
        // Hourly ticks: the usual gap is an hour, so the cut is five hours.
        let mut value = collect_runs("");
        merge_inferred(&mut value, &slow);
        assert_eq!(value["runs"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn seconds_since_the_epoch_read_every_offset_spelling() {
        let want = Some(1_794_588_431);
        assert_eq!(epoch_seconds("2026-11-13T16:47:11+00:00"), want);
        assert_eq!(epoch_seconds("2026-11-13T16:47:11.971602+00:00"), want);
        assert_eq!(epoch_seconds("2026-11-13T16:47:11Z"), want);
        assert_eq!(epoch_seconds("2026-11-13T23:47:11+07:00"), want);
        assert_eq!(epoch_seconds("nonsense"), None);
    }
}
