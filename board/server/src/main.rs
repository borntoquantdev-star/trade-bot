//! Serves the board.
//!
//! Two jobs, and deliberately no more:
//!
//! 1. Hand the browser the WASM bundle built by trunk.
//! 2. Read the files the bot already writes (`logs/audit.jsonl`,
//!    `logs/state.json`, `logs/backtest.json`) and hand them over as JSON.
//!
//! It never connects to an exchange and never holds a credential. The board
//! in the browser talks to GRVT's **public** market-data stream directly, so
//! nothing that could place an order passes through here. A student can point
//! this at a folder and watch; that is the whole contract.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use tower_http::services::{ServeDir, ServeFile};

mod account;
mod bot;
mod candles;
mod code;
mod history;
mod kline;
mod market;

#[derive(Clone)]
pub struct Config {
    /// Where the bot writes its logs.
    pub logs: PathBuf,
    /// Where the bot keeps the candle history it downloaded.
    pub candles: PathBuf,
    /// Where the bot's own source lives, so the stage cards can show the
    /// function that ran. Read only, `.py` only, and never written to.
    pub code: PathBuf,
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // Railway and most hosts hand the port in through the environment.
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(4340);
    let dist = PathBuf::from(std::env::var("BOARD_DIST").unwrap_or_else(|_| "dist".into()));
    let logs = PathBuf::from(std::env::var("BOARD_LOGS").unwrap_or_else(|_| "logs".into()));
    let candle_root =
        PathBuf::from(std::env::var("BOARD_CANDLES").unwrap_or_else(|_| "data/candles".into()));
    // The folder the bot runs from, because `tradebot/core/engine.py` is
    // written relative to it - the same shape the audit trail uses.
    let code_root = PathBuf::from(std::env::var("BOARD_CODE").unwrap_or_else(|_| ".".into()));
    let config_file = std::env::var("BOARD_CONFIG").unwrap_or_else(|_| "config/config.yaml".into());

    let grvt_env = std::env::var("BOARD_GRVT_ENV").unwrap_or_else(|_| "testnet".into());
    let market_data_rest = match grvt_env.as_str() {
        "prod" | "mainnet" => "https://market-data.grvt.io".to_string(),
        _ => "https://market-data.testnet.grvt.io".to_string(),
    };

    let account = account::Account::from_env(&grvt_env);
    let config = Config {
        logs: logs.clone(),
        candles: candle_root.clone(),
        code: code_root.clone(),
        config_file: config_file.clone(),
        market_data_rest: market_data_rest.clone(),
        http: reqwest::Client::builder()
            .user_agent("tradebot-board")
            .build()?,
        account: account.clone(),
    };

    let index = dist.join("index.html");
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/state", get(state))
        .route("/api/backtest", get(bot::backtest))
        .route("/api/audit", get(audit))
        .route("/api/code", get(code::code))
        .route("/api/stages", get(code::stages))
        .route("/api/runs", get(history::runs))
        .route("/api/days", get(history::days))
        .route("/api/candles", get(candles::candles))
        .route("/api/candles/available", get(candles::available))
        .route("/api/kline", get(kline::kline))
        .route("/api/market/{kind}", get(market::market))
        .route("/api/account/{kind}", get(account::account))
        .with_state(config)
        // Anything not an API route is the single-page app.
        .fallback_service(ServeDir::new(&dist).fallback(ServeFile::new(&index)));

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(
        "board on http://{addr}  dist={}  logs={}  candles={}  code={}",
        dist.display(),
        logs.display(),
        candle_root.display(),
        code_root.display()
    );
    tracing::info!("the five stage cards read {config_file} to know whose code to show");
    tracing::info!("public market data: {market_data_rest} (BOARD_GRVT_ENV={grvt_env})");
    match &account.keys {
        Some(keys) => tracing::info!(
            "account data: {} (key found, read only - this server never sends an order)",
            keys.trades_rest
        ),
        None => tracing::info!(
            "account data: off (no GRVT_API_KEY, and no .env in {:?}) - market data still works",
            account::env_candidates()
        ),
    }
    if !index.exists() {
        tracing::warn!(
            "no {} yet - run `trunk build` in app/ first, or the page will 404",
            index.display()
        );
    }

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("stopping");
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
    read_json(&config.logs.join("state.json")).await
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
            Ok(value) => rows.push(value),
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
