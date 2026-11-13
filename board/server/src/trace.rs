//! The path a tick took through the bot's code, one tick at a time.
//!
//! Every row the bot writes now carries a trace - which functions it
//! entered, which lines ran and in what order - and a trace is about fifteen
//! times the size of the rest of the row. The board re-reads the tail of the
//! trail every two seconds; handing it 500 traces each time would be two and
//! a half megabytes every two seconds, to draw five cards that only ever show
//! one tick.
//!
//! So the trail travels without them, each row marked `"traced": true`, and
//! the board asks here for the one trace it is showing. A trace never
//! changes once written, so the board asks for each one once.

use axum::extract::{Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::Config;

#[derive(Debug, Deserialize)]
pub struct Ask {
    /// The row's own `at`, exactly as the bot wrote it.
    pub at: String,
}

pub async fn trace(State(config): State<Config>, Query(ask): Query<Ask>) -> Json<Value> {
    let path = config.logs.join("audit.jsonl");
    let Ok(text) = tokio::fs::read_to_string(&path).await else {
        return Json(json!({
            "present": false,
            "reason": format!("ยังไม่มี {}", path.display()),
        }));
    };
    Json(match find(&text, &ask.at) {
        Some(trace) => json!({"present": true, "at": ask.at, "trace": trace}),
        None => json!({
            "present": false,
            "reason": format!("ไม่พบ trace ของ tick {}", ask.at),
        }),
    })
}

/// Take the trace off a row for the trip down the wire, leaving a mark that
/// there was one.
///
/// A row without a trace is left exactly as it was: no mark, because a
/// mark saying "there is no trace" would be a field the bot never wrote.
pub fn slim(row: &mut Value) {
    if let Value::Object(fields) = row {
        if fields.remove("trace").is_some() {
            fields.insert("traced".into(), Value::Bool(true));
        }
    }
}

/// The trace of the row written at `at`, searching from the newest end.
///
/// From the newest end because that is where the board almost always asks,
/// and the substring test comes before the parse because parsing every line
/// of a long trail to find one of them would be most of the cost of the
/// request.
pub fn find(text: &str, at: &str) -> Option<Value> {
    if at.is_empty() {
        return None;
    }
    let needle = format!("\"at\": \"{at}\"");
    let compact = format!("\"at\":\"{at}\"");
    for line in text.lines().rev() {
        if !line.contains(&needle) && !line.contains(&compact) {
            continue;
        }
        let Ok(Value::Object(mut row)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // The substring could in principle sit inside another field; the
        // parsed `at` is the one that decides.
        if row.get("at").and_then(|value| value.as_str()) != Some(at) {
            continue;
        }
        return row.remove("trace");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRAIL: &str = r#"{"at": "2026-11-13T16:47:06+00:00", "price": "1", "trace": {"steps": 3}}
{"at": "2026-11-13T16:47:11+00:00", "price": "2", "order": {"qty": "0.145"}, "trace": {"steps": 9}}
{"at": "2026-11-13T16:47:16+00:00", "price": "3"}
"#;

    #[test]
    fn a_row_travels_without_its_trace_but_says_it_had_one() {
        let mut row: Value =
            serde_json::from_str(r#"{"at":"x","price":"1","trace":{"steps":3}}"#).unwrap();
        slim(&mut row);
        assert!(row.get("trace").is_none(), "the heavy part stays behind");
        assert_eq!(row["traced"], true);
        assert_eq!(row["price"], "1", "everything else is untouched");
    }

    #[test]
    fn a_row_that_never_had_a_trace_gains_no_field() {
        let mut row: Value = serde_json::from_str(r#"{"at":"x","price":"1"}"#).unwrap();
        slim(&mut row);
        assert!(row.get("traced").is_none());
    }

    #[test]
    fn a_tick_s_trace_is_found_by_the_timestamp_the_bot_wrote() {
        let found = find(TRAIL, "2026-11-13T16:47:11+00:00").unwrap();
        assert_eq!(found["steps"], 9);
    }

    #[test]
    fn a_tick_with_no_trace_or_no_row_has_nothing_to_find() {
        assert!(
            find(TRAIL, "2026-11-13T16:47:16+00:00").is_none(),
            "no trace"
        );
        assert!(find(TRAIL, "2026-11-01T00:00:00+00:00").is_none(), "no row");
        assert!(find(TRAIL, "").is_none());
    }

    /// The bot writes `"at": "..."` with a space, a hand-edited or
    /// re-serialised file might not. Both are the same row.
    #[test]
    fn either_spacing_of_the_key_is_found() {
        let compact = r#"{"at":"2026-11-13T16:47:11+00:00","trace":{"steps":1}}"#;
        assert_eq!(
            find(compact, "2026-11-13T16:47:11+00:00").unwrap()["steps"],
            1
        );
    }

    #[test]
    fn a_timestamp_that_only_appears_inside_another_field_is_not_a_match() {
        let tricky = r#"{"at": "2026-11-13T16:00:00+00:00", "note": "\"at\": \"2026-11-13T16:47:11+00:00\"", "trace": {"steps": 7}}"#;
        assert!(find(tricky, "2026-11-13T16:47:11+00:00").is_none());
    }
}
