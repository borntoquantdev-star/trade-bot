//! Serves the board.
//!
//! Two jobs, and deliberately no more:
//!
//! 1. Hand the browser the WASM bundle built by trunk.
//! 2. Read the files the bot already writes (`logs/audit.jsonl`,
//!    `logs/state.json`, `logs/backtest.json`) and hand them over as JSON.
//!
//! It never connects to an exchange to trade and never sends an order. The
//! board in the browser talks to GRVT's **public** market-data stream
//! directly, so nothing that could place an order passes through here. A
//! student can point this at a folder and watch; that is the whole contract.
//!
//! # Control mode
//!
//! One exception, and it is opt-in twice over. On a student's own laptop the
//! board can also *start and stop the bot* (`control.rs`), so that nobody has
//! to open a terminal. Those routes exist only when the caller asks for them
//! **and** the listener is bound to loopback; on any other address - the
//! Railway deployment - they are not registered at all. Even then this
//! server only runs the `tradebot` program the student already has: it still
//! never holds an order, a signature or a private key in memory.

use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{any, get};
use axum::{Json, Router};
use tower_http::services::{ServeDir, ServeFile};

pub mod account;
mod bot;
mod candles;
mod code;
pub mod control;
mod history;
mod kline;
pub mod launcher;
mod market;
mod trace;

#[derive(Clone)]
pub struct Config {
    /// Where the bot writes its logs.
    pub logs: PathBuf,
    /// Where the bot keeps the candle history it downloaded.
    pub candles: PathBuf,
    /// Where the bot's own source lives, so the stage cards can show the
    /// function that ran. Read only, `.py` only, and never written to.
    pub code: PathBuf,
    /// Hours east of UTC that a calendar day starts at. Thailand, +7, by
    /// default: a day that began at midnight UTC would begin at seven in the
    /// morning for everyone reading this board.
    pub utc_offset_hours: i64,
    /// The bot's config file, relative to `code`. Read for two strings: the
    /// exchange and the strategy, which is what lets the cards show the
    /// right code before the bot has ever run.
    pub config_file: String,
    /// GRVT's public market-data REST root, e.g.
    /// `https://market-data.testnet.grvt.io`.
    pub market_data_rest: String,
    /// One client, reused: a new one per request would open a new TLS
    /// connection every time the chart refreshes.
    pub http: reqwest::Client,
    /// The credential, if there is one, and the session it bought. Lives
    /// here and only here: see `account.rs`.
    pub account: account::Account,
}

/// Everything `serve_on` needs to know, gathered in one place so that the two
/// programs that start a server (`board-server` from the environment,
/// `TradeBot` from the folder it sits in) say the same things in the same
/// words.
pub struct Settings {
    pub dist: PathBuf,
    pub logs: PathBuf,
    pub candles: PathBuf,
    pub code: PathBuf,
    pub config_file: String,
    pub utc_offset_hours: i64,
    /// `testnet` or `prod`; picks the public market-data host.
    pub grvt_env: String,
    pub account: account::Account,
    /// `Some` asks for control mode. It is honoured only on a loopback
    /// listener - see `build_router`.
    pub control: Option<control::Control>,
}

/// GRVT's public market-data REST root for an environment name.
pub fn market_data_rest(grvt_env: &str) -> &'static str {
    match grvt_env {
        "prod" | "mainnet" => "https://market-data.grvt.io",
        _ => "https://market-data.testnet.grvt.io",
    }
}

/// Is control mode actually on for a server bound to `bound`?
///
/// The one rule that keeps a hosted board read-only: asking is not enough,
/// the listener must be loopback too.
pub fn control_enabled(requested: bool, bound: SocketAddr) -> bool {
    requested && bound.ip().is_loopback()
}

pub fn build_router(settings: Settings, bound: SocketAddr) -> anyhow::Result<Router> {
    let config = Config {
        logs: settings.logs.clone(),
        candles: settings.candles.clone(),
        code: settings.code.clone(),
        config_file: settings.config_file.clone(),
        utc_offset_hours: settings.utc_offset_hours,
        market_data_rest: market_data_rest(&settings.grvt_env).to_string(),
        http: reqwest::Client::builder()
            .user_agent("tradebot-board")
            .build()?,
        account: settings.account.clone(),
    };

    let index = settings.dist.join("index.html");
    let mut app = Router::new()
        .route("/api/health", get(health))
        .route("/api/state", get(state))
        .route("/api/backtest", get(bot::backtest))
        .route("/api/audit", get(audit))
        .route("/api/code", get(code::code))
        .route("/api/stages", get(code::stages))
        .route("/api/runs", get(history::runs))
        .route("/api/days", get(history::days))
        .route("/api/trace", get(trace::trace))
        .route("/api/candles", get(candles::candles))
        .route("/api/candles/available", get(candles::available))
        .route("/api/kline", get(kline::kline))
        .route("/api/market/{kind}", get(market::market))
        .route("/api/account/{kind}", get(account::account))
        // An API path nobody registered is a 404, not the page: without this
        // the single-page fallback below answers `/api/control` with HTML and
        // a 200, and a board could not tell "control is off" from "control is
        // on and replied with something odd".
        .route("/api/{*rest}", any(no_such_api))
        .with_state(config);

    if let Some(control) = settings.control {
        if control_enabled(true, bound) {
            app = app.merge(control::routes(control, settings.account));
        } else {
            tracing::warn!(
                "control mode was asked for but {bound} is not a loopback address - \
                 the control routes are NOT registered (this server stays read-only)"
            );
        }
    }

    // Anything not an API route is the single-page app.
    Ok(app.fallback_service(ServeDir::new(&settings.dist).fallback(ServeFile::new(&index))))
}

/// Serve on an already-bound listener until `shutdown` resolves.
///
/// Taking the listener rather than an address lets the launcher bind first -
/// and find out the port is taken - before it promises anyone a board.
pub async fn serve_on(
    listener: tokio::net::TcpListener,
    settings: Settings,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> anyhow::Result<()> {
    let bound = listener.local_addr()?;
    let control = settings.control.clone();
    let app = build_router(settings, bound)?;
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            shutdown.await;
            // A bot the board started should not be left running by a board
            // that is going away: ask it to stop first, while the page can
            // still show "stopping".
            if let Some(control) = control {
                let _ = tokio::task::spawn_blocking(move || {
                    control.shutdown_blocking(Duration::from_secs(8))
                })
                .await;
            }
        })
        .await?;
    Ok(())
}

async fn no_such_api() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({ "ok": false, "error": "ไม่มี API นี้" })),
    )
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "ok": true }))
}

/// A missing file is not an error: the bot may simply not have run yet.
/// Saying so plainly beats a 500 that looks like the board is broken.
async fn read_json(path: &Path) -> Json<serde_json::Value> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(value) => Json(serde_json::json!({ "present": true, "data": value })),
            Err(err) => Json(serde_json::json!({
                "present": false,
                "reason": format!("ไฟล์มีแต่อ่านไม่ออก: {err}"),
            })),
        },
        Err(_) => Json(serde_json::json!({
            "present": false,
            "reason": format!("ยังไม่มี {}", path.display()),
        })),
    }
}

async fn state(State(config): State<Config>) -> Json<serde_json::Value> {
    read_json(&bot::state_path(&config.logs)).await
}

/// The audit trail is JSON Lines, newest last. Only the tail is sent: the
/// board draws a few hundred rows, and shipping a 50 MB file to a browser
/// helps nobody.
async fn audit(State(config): State<Config>) -> Result<Json<serde_json::Value>, StatusCode> {
    const KEEP: usize = 500;
    let path = config.logs.join("audit.jsonl");
    let Ok(text) = tokio::fs::read_to_string(&path).await else {
        return Ok(Json(serde_json::json!({
            "present": false,
            "reason": format!("ยังไม่มี {}", path.display()),
            "rows": [],
        })));
    };

    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut unreadable = 0usize;
    for line in text.lines().rev() {
        if rows.len() >= KEEP {
            break;
        }
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str(line) {
            Ok(mut value) => {
                // Each trace is fifteen times the rest of its row, and the
                // board shows one tick at a time: it asks /api/trace for
                // the one it is showing. See trace.rs.
                trace::slim(&mut value);
                rows.push(value)
            }
            // One broken line must not make the whole trail unreadable.
            Err(_) => unreadable += 1,
        }
    }
    rows.reverse();

    Ok(Json(serde_json::json!({
        "present": true,
        "rows": rows,
        "unreadable": unreadable,
    })))
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
