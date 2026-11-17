//! `board-server`: the board's server, configured from the environment.
//!
//! This is the binary Railway and Docker run, and what `board/start.ps1`
//! starts. It binds `0.0.0.0` unless told otherwise and is read-only: it
//! serves the page and the files the bot wrote, and never sends an order.
//!
//! Control mode (start and stop the bot from the board) needs **both**
//! `BOARD_CONTROL=1` and a loopback bind (`BOARD_BIND=127.0.0.1`); on the
//! default `0.0.0.0` it is refused, so a hosted board cannot grow it by
//! accident. Students do not use this binary for that - they double-click
//! `TradeBot.exe`, which is the same server with control mode on.

use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use board_server::control::Control;
use board_server::{account, control_enabled, serve_on, Settings};

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
    let ip: IpAddr = std::env::var("BOARD_BIND")
        .ok()
        .and_then(|bind| bind.trim().parse().ok())
        .unwrap_or_else(|| IpAddr::from([0, 0, 0, 0]));
    let addr = SocketAddr::new(ip, port);

    let dist = PathBuf::from(std::env::var("BOARD_DIST").unwrap_or_else(|_| "dist".into()));
    let logs = PathBuf::from(std::env::var("BOARD_LOGS").unwrap_or_else(|_| "logs".into()));
    let candle_root =
        PathBuf::from(std::env::var("BOARD_CANDLES").unwrap_or_else(|_| "data/candles".into()));
    // The folder the bot runs from, because `tradebot/core/engine.py` is
    // written relative to it - the same shape the audit trail uses.
    let code_root = PathBuf::from(std::env::var("BOARD_CODE").unwrap_or_else(|_| ".".into()));
    let config_file = std::env::var("BOARD_CONFIG").unwrap_or_else(|_| "config/config.yaml".into());

    let grvt_env = std::env::var("BOARD_GRVT_ENV").unwrap_or_else(|_| "testnet".into());

    let wants_control = std::env::var("BOARD_CONTROL").is_ok_and(|value| value.trim() == "1");
    let control = if wants_control {
        // BOARD_BOT_ROOT is the folder the bot runs from, which is what
        // BOARD_CODE already says; either names it.
        let root = std::env::var("BOARD_BOT_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|_| code_root.clone());
        Some(Control::new(root))
    } else {
        None
    };

    // Keys written from the board go to <root>/config/.env, so that is the
    // file read first when control is on - otherwise a key saved from the
    // page would land somewhere the server does not look.
    let mut env_files = account::env_candidates();
    if let Some(control) = &control {
        env_files.insert(0, control.env_path());
    }
    let account = account::Account::from_files(&grvt_env, env_files);

    // On a non-loopback bind `serve_on` refuses control mode again, and says
    // so; this only decides whether to announce it.
    if let (true, Some(control)) = (control_enabled(wants_control, addr), &control) {
        control.refresh_setup();
        tracing::info!(
            "control mode ON: the board can start and stop the bot in {}",
            control.root().display()
        );
    }

    let index = dist.join("index.html");
    tracing::info!(
        "board on http://{addr}  dist={}  logs={}  candles={}  code={}",
        dist.display(),
        logs.display(),
        candle_root.display(),
        code_root.display()
    );
    tracing::info!("the five stage cards read {config_file} to know whose code to show");
    tracing::info!(
        "public market data: {} (BOARD_GRVT_ENV={grvt_env})",
        board_server::market_data_rest(&grvt_env)
    );
    match account.keys() {
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

    let settings = Settings {
        dist,
        logs,
        candles: candle_root,
        code: code_root,
        config_file,
        utc_offset_hours: std::env::var("BOARD_UTC_OFFSET")
            .ok()
            .and_then(|hours| hours.trim().parse().ok())
            .unwrap_or(7),
        grvt_env,
        account,
        control,
    };

    let listener = tokio::net::TcpListener::bind(addr).await?;
    serve_on(listener, settings, shutdown()).await
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
    tracing::info!("stopping");
}
