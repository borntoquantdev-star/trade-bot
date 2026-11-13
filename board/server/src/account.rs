//! The account side: balance, positions, open orders, history, fees.
//!
//! Everything in `market.rs` is public - anybody may ask the exchange what
//! BTC costs. Nothing here is. These answers belong to one account, and
//! reaching them needs a credential, which is why this module exists on the
//! **server** and not in the board.
//!
//! The rule that shapes the whole file:
//!
//! > The browser never receives the key, the cookie, or the account id.
//! > It asks this server a question by name, and gets numbers back.
//!
//! A board is something a student opens, a teacher screen-shares, and a
//! laptop leaves on a desk. Anything it could leak, it will eventually leak.
//! So the key is read once from the environment (or a `.env` file beside the
//! bot's config), exchanged for a session cookie that stays in this process,
//! and never travels onward.
//!
//! **Read only, by construction.** The allowlist below holds six names and
//! every one of them is a question. `create_order`, `cancel_order` and
//! `transfer` are not reachable through this route no matter what is typed
//! into it - a board that can place an order is a board that will place one
//! by accident.
//!
//! The one write the exchange does require is the login itself, and it costs
//! only the api key: GRVT hands back a session cookie without ever seeing
//! the private key that signs orders. So what this process holds cannot move
//! money even if it escaped.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path as UrlPath, Query, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

use crate::market::add_times;
use crate::Config;

/// How long a login is assumed good before it is renewed anyway. The cookie
/// lives longer than this; the point is to not discover it expired in front
/// of a class.
const SESSION_SECONDS: i64 = 20 * 60;

/// The six questions this route will ask, and nothing else.
///
/// The first name is what the board asks for, the second is GRVT's endpoint.
/// Keeping them apart means the board's vocabulary is ours - words a person
/// reads - rather than the exchange's.
const ALLOWED: [(&str, &str); 7] = [
    ("summary", "account_summary"),
    ("positions", "positions"),
    ("open_orders", "open_orders"),
    ("order_history", "order_history"),
    ("fills", "fill_history"),
    ("funding", "funding_payment_history"),
    ("closed", "position_history"),
];

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Ask {
    /// How many rows of history. Ignored by the questions that have no
    /// history to limit.
    pub limit: Option<u32>,
}

/// What this server knows about the account, read once at start-up.
#[derive(Debug, Clone)]
pub struct Keys {
    pub api_key: String,
    /// The sub account the questions are asked about. Taken from the
    /// environment when it says, otherwise from what the login replies.
    pub sub_account_id: Option<String>,
    /// `https://trades.testnet.grvt.io`
    pub trades_rest: String,
    /// `https://edge.testnet.grvt.io`
    pub edge_rest: String,
}

/// A live session: what the exchange gave back for the key.
#[derive(Debug, Clone, Default)]
pub struct Session {
    pub cookie: String,
    pub account_id: String,
    pub sub_account_id: String,
    pub at: i64,
}

#[derive(Clone, Default)]
pub struct Account {
    pub keys: Option<Keys>,
    pub session: Arc<Mutex<Option<Session>>>,
}

impl Account {
    /// Build from the environment. No key is a normal state, not a failure:
    /// the board is useful without one, it just cannot show an account.
    pub fn from_env(grvt_env: &str) -> Self {
        let file = read_env_file();
        let get = |name: &str| -> Option<String> {
            std::env::var(name)
                .ok()
                .or_else(|| {
                    file.iter()
                        .find(|(key, _)| key == name)
                        .map(|(_, value)| value.clone())
                })
                .map(|value| value.trim().to_owned())
                .filter(|value| !value.is_empty())
        };

        let Some(api_key) = get("GRVT_API_KEY") else {
            return Self::default();
        };
        let prod = matches!(grvt_env, "prod" | "mainnet");
        let host = |name: &str| {
            if prod {
                format!("https://{name}.grvt.io")
            } else {
                format!("https://{name}.testnet.grvt.io")
            }
        };
        Self {
            keys: Some(Keys {
                api_key,
                sub_account_id: get("GRVT_TRADING_ACCOUNT_ID")
                    .or_else(|| get("GRVT_SUB_ACCOUNT_ID")),
                trades_rest: host("trades"),
                edge_rest: host("edge"),
            }),
            session: Arc::new(Mutex::new(None)),
        }
    }
}

/// Where a `.env` might be, in the order a person would expect.
///
/// The board usually runs from `board/` while the bot's keys live in
/// `config/.env` one level up, so that is on the list: asking somebody to
/// keep the same secret in two files is asking for one of them to go stale.
pub fn env_candidates() -> Vec<PathBuf> {
    let mut found = Vec::new();
    if let Ok(named) = std::env::var("BOARD_ENV_FILE") {
        if !named.trim().is_empty() {
            found.push(PathBuf::from(named.trim()));
        }
    }
    found.push(PathBuf::from(".env"));
    found.push(PathBuf::from("..").join("config").join(".env"));
    found.push(PathBuf::from("config").join(".env"));
    found
}

fn read_env_file() -> Vec<(String, String)> {
    for path in env_candidates() {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let pairs = parse_env(&text);
        if !pairs.is_empty() {
            // The path, never a value: this line goes in a log that ends up
            // in a terminal somebody is sharing on a projector.
            tracing::info!("account keys read from {}", path.display());
            return pairs;
        }
    }
    Vec::new()
}

/// `KEY=value` lines. Deliberately literal: no shell, no expansion, one
/// matching pair of quotes stripped and nothing else, because a key is a
/// string and nothing should be clever about it.
pub fn parse_env(text: &str) -> Vec<(String, String)> {
    let mut found: Vec<(String, String)> = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let line = line.strip_prefix("export ").unwrap_or(line).trim_start();
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        let name = name.trim().to_owned();
        let mut value = value.trim();
        let quoted = value.len() >= 2
            && value.starts_with(['"', '\''])
            && value.ends_with(value.chars().next().unwrap());
        if quoted {
            value = &value[1..value.len() - 1];
        }
        if !name.is_empty() && !found.iter().any(|(key, _)| *key == name) {
            found.push((name, value.to_owned()));
        }
    }
    found
}

/// Pull `gravity=...` out of whatever the login put in `set-cookie`.
///
/// There may be several cookies, and each carries attributes we neither need
/// nor want to send back (`Path`, `HttpOnly`, an expiry). Only the one pair
/// is kept.
pub fn gravity_from(set_cookie: &[String]) -> Option<String> {
    for header in set_cookie {
        for part in header.split(';') {
            if let Some(value) = part.trim().strip_prefix("gravity=") {
                if !value.is_empty() {
                    return Some(value.to_owned());
                }
            }
        }
    }
    None
}

/// Turn a board question into the exchange's endpoint and body, or a
/// sentence saying why not.
pub fn request_for(
    kind: &str,
    ask: &Ask,
    sub_account_id: &str,
) -> Result<(&'static str, Value), String> {
    let Some((_, endpoint)) = ALLOWED.iter().find(|(name, _)| *name == kind) else {
        let names: Vec<&str> = ALLOWED.iter().map(|(name, _)| *name).collect();
        return Err(format!("ไม่รู้จัก {kind} · ฝั่งบัญชีถามได้แค่: {}", names.join(" ")));
    };
    if sub_account_id.is_empty() {
        return Err("ยังไม่รู้เลขบัญชีย่อย (sub account) ของกุญแจนี้".into());
    }
    let limit = ask.limit.unwrap_or(50).clamp(1, 500);
    let body = match kind {
        "summary" | "positions" => json!({ "sub_account_id": sub_account_id }),
        "open_orders" => json!({ "sub_account_id": sub_account_id, "kind": ["PERPETUAL"] }),
        // The histories come back newest first already, so one page is the
        // recent one - which is all a board shows.
        _ => json!({ "sub_account_id": sub_account_id, "limit": limit }),
    };
    Ok((endpoint, body))
}

/// A card that cannot be filled says why, in words the person can act on.
fn absent(reason: impl Into<String>) -> Json<Value> {
    Json(json!({ "present": false, "reason": reason.into() }))
}

const NO_KEY: &str = "ยังไม่ได้ตั้งกุญแจให้บอร์ด · ตั้ง GRVT_API_KEY หรือวางไว้ใน \
config/.env ของบอท แล้วเปิดเซิร์ฟเวอร์ใหม่ · ข้อมูลตลาดยังดูได้ตามปกติโดยไม่ต้องมีกุญแจ";

pub async fn account(
    UrlPath(kind): UrlPath<String>,
    Query(ask): Query<Ask>,
    State(config): State<Config>,
) -> Json<Value> {
    let Some(keys) = config.account.keys.clone() else {
        return absent(NO_KEY);
    };

    let session = match ensure_session(&config, &keys, false).await {
        Ok(session) => session,
        Err(reason) => return absent(reason),
    };
    let (endpoint, body) = match request_for(&kind, &ask, &session.sub_account_id) {
        Ok(pair) => pair,
        Err(reason) => return absent(reason),
    };

    match ask_exchange(&config, &keys, &session, endpoint, &body).await {
        Ok(value) => value,
        // One retry, and only for "your session is gone": a cookie that
        // expires mid-lesson should heal itself rather than need a restart.
        Err(Expired) => {
            let session = match ensure_session(&config, &keys, true).await {
                Ok(session) => session,
                Err(reason) => return absent(reason),
            };
            match ask_exchange(&config, &keys, &session, endpoint, &body).await {
                Ok(value) => value,
                Err(Expired) => absent("ตลาดไม่รับ session นี้ ลองตรวจว่ากุญแจยังใช้ได้อยู่"),
            }
        }
    }
}

/// The one failure the caller is allowed to retry.
struct Expired;

async fn ask_exchange(
    config: &Config,
    keys: &Keys,
    session: &Session,
    endpoint: &str,
    body: &Value,
) -> Result<Json<Value>, Expired> {
    let url = format!("{}/full/v1/{endpoint}", keys.trades_rest);
    let sent = config
        .http
        .post(&url)
        .header("Cookie", format!("gravity={}", session.cookie))
        .header("X-Grvt-Account-Id", session.account_id.clone())
        .json(body)
        .send()
        .await;

    let response = match sent {
        Ok(response) => response,
        Err(err) => return Ok(absent(format!("ถามบัญชีกับตลาดไม่สำเร็จ: {err}"))),
    };
    let status = response.status();
    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(Expired);
    }
    let parsed: Result<Value, _> = response.json().await;
    let Ok(value) = parsed else {
        return Ok(absent(format!("ตลาดตอบ {status} แต่อ่านไม่ออก")));
    };
    if !status.is_success() {
        let message = value
            .get("message")
            .and_then(|message| message.as_str())
            .unwrap_or("ไม่ได้บอกเหตุผล");
        return Ok(absent(format!("ตลาดปฏิเสธ ({status}): {message}")));
    }

    let mut result = value.get("result").cloned().unwrap_or(Value::Null);
    add_times(&mut result);
    Ok(Json(json!({ "present": true, "result": result })))
}

/// Log in if there is no session, or if the one held is old.
async fn ensure_session(config: &Config, keys: &Keys, force: bool) -> Result<Session, String> {
    let now = seconds_now();
    let mut held = config.account.session.lock().await;
    if !force {
        if let Some(session) = held.as_ref() {
            if now - session.at < SESSION_SECONDS {
                return Ok(session.clone());
            }
        }
    }

    let url = format!("{}/auth/api_key/login", keys.edge_rest);
    let response = config
        .http
        .post(&url)
        .header("Cookie", "rm=true")
        .json(&json!({ "api_key": keys.api_key }))
        .send()
        .await
        .map_err(|err| format!("ล็อกอินเข้าบัญชีไม่สำเร็จ: {err}"))?;

    let status = response.status();
    let set_cookie: Vec<String> = response
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|value| value.to_str().ok().map(|text| text.to_owned()))
        .collect();
    let account_id = response
        .headers()
        .get("x-grvt-account-id")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let body: Value = response.json().await.unwrap_or(Value::Null);

    if !status.is_success() {
        let message = body
            .get("message")
            .and_then(|message| message.as_str())
            .unwrap_or("ไม่ได้บอกเหตุผล");
        return Err(format!(
            "ตลาดไม่รับกุญแจนี้ ({status}): {message} \
             · ส่วนใหญ่คือกุญแจของ prod ไปใช้กับ testnet หรือกลับกัน"
        ));
    }
    let Some(cookie) = gravity_from(&set_cookie) else {
        return Err("ล็อกอินผ่านแต่ตลาดไม่ได้ให้คุกกี้มา".into());
    };

    let sub_account_id = keys
        .sub_account_id
        .clone()
        .unwrap_or_else(|| sub_account_from(&body));

    let session = Session {
        cookie,
        account_id,
        sub_account_id,
        at: now,
    };
    tracing::info!(
        "account session opened (sub account {})",
        session.sub_account_id
    );
    *held = Some(session.clone());
    Ok(session)
}

/// Seconds since the epoch, for deciding when a session is old.
///
/// `std` rather than chrono's clock: the only thing being measured here is
/// the gap between two of these, and a clock that stepped backwards (a
/// laptop waking up, an NTP correction) should renew a session early rather
/// than keep a stale one.
fn seconds_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs() as i64)
        .unwrap_or(0)
}

/// The sub account the login replied with, wherever it put it.
///
/// Worth having because it is the number people cannot find on the website:
/// the key knows its own account, so nobody has to go hunting for it.
pub fn sub_account_from(body: &Value) -> String {
    let found = body.get("sub_account_id").or_else(|| {
        body.get("result")
            .and_then(|result| result.get("sub_account_id"))
    });
    match found {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_questions_are_reachable_never_an_order() {
        for forbidden in [
            "create_order",
            "cancel_order",
            "cancel_all_orders",
            "transfer",
            "withdrawal",
            "bulk_orders",
            "set_position_config",
        ] {
            let err = request_for(forbidden, &Ask::default(), "123").unwrap_err();
            assert!(err.contains(forbidden), "{err}");
        }
    }

    #[test]
    fn every_allowed_name_asks_about_the_sub_account() {
        for (kind, endpoint) in ALLOWED {
            let (asked, body) = request_for(kind, &Ask::default(), "5421993573613261").unwrap();
            assert_eq!(asked, endpoint);
            assert_eq!(body["sub_account_id"], json!("5421993573613261"));
        }
    }

    #[test]
    fn a_history_is_limited_and_a_snapshot_is_not() {
        let ask = Ask { limit: Some(20) };
        let (_, body) = request_for("fills", &ask, "1").unwrap();
        assert_eq!(body["limit"], json!(20));
        let (_, body) = request_for("summary", &ask, "1").unwrap();
        assert_eq!(body.get("limit"), None, "ยอดเงินไม่มีประวัติให้จำกัด");
    }

    #[test]
    fn a_limit_out_of_range_is_brought_back_in_rather_than_refused() {
        let (_, body) = request_for(
            "order_history",
            &Ask {
                limit: Some(99_999),
            },
            "1",
        )
        .unwrap();
        assert_eq!(body["limit"], json!(500));
        let (_, body) = request_for("order_history", &Ask { limit: Some(0) }, "1").unwrap();
        assert_eq!(body["limit"], json!(1));
    }

    #[test]
    fn without_a_sub_account_nothing_is_sent_at_all() {
        let err = request_for("summary", &Ask::default(), "").unwrap_err();
        assert!(err.contains("บัญชีย่อย"), "{err}");
    }

    #[test]
    fn the_session_cookie_is_pulled_out_of_whatever_else_was_set() {
        let headers = vec![
            "rm=true; Path=/; HttpOnly".to_owned(),
            "gravity=abc123xyz; Path=/; Secure; HttpOnly; Max-Age=3600".to_owned(),
        ];
        assert_eq!(gravity_from(&headers).as_deref(), Some("abc123xyz"));
        assert_eq!(gravity_from(&["rm=true".to_owned()]), None);
        assert_eq!(
            gravity_from(&["gravity=".to_owned()]),
            None,
            "คุกกี้ว่างไม่ใช่คุกกี้"
        );
    }

    #[test]
    fn an_env_file_is_read_literally_and_comments_are_not_values() {
        let pairs = parse_env(
            "# กุญแจ\nGRVT_API_KEY=abc\nexport GRVT_SUB_ACCOUNT_ID=\"0\"\n\nBAD LINE\nGRVT_API_KEY=second\n",
        );
        assert_eq!(pairs[0], ("GRVT_API_KEY".into(), "abc".into()));
        assert_eq!(pairs[1], ("GRVT_SUB_ACCOUNT_ID".into(), "0".into()));
        assert_eq!(pairs.len(), 2, "บรรทัดที่ไม่มี = ไม่ใช่ค่า และค่าแรกชนะ");
    }

    #[test]
    fn a_key_with_an_equals_sign_in_it_survives_being_read() {
        let pairs = parse_env("GRVT_PRIVATE_KEY=aa=bb==\n");
        assert_eq!(pairs[0].1, "aa=bb==");
    }

    #[test]
    fn the_login_reply_can_supply_the_number_nobody_can_find_on_the_website() {
        assert_eq!(
            sub_account_from(&json!({ "sub_account_id": "5421993573613261" })),
            "5421993573613261"
        );
        assert_eq!(
            sub_account_from(&json!({ "result": { "sub_account_id": 42 } })),
            "42"
        );
        assert_eq!(sub_account_from(&json!({ "status": "success" })), "");
    }

    #[test]
    fn no_key_in_the_environment_is_a_board_without_an_account_not_an_error() {
        assert!(Account::default().keys.is_none());
    }
}
