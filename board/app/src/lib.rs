//! The board: an infinite canvas for watching one tradebot.
//!
//! Compiles two ways from one source — to WebAssembly for the browser (which
//! is what the Docker image serves) and to a native binary. The browser build
//! is the one students get; the native build exists because it is far quicker
//! to iterate on and because it costs nothing to keep working.

pub mod account;
pub mod api;
pub mod app;
pub mod bot;
pub mod candles;
pub mod card;
pub mod code;
pub mod depth;
pub mod equity;
pub mod exchange;
pub mod feed;
pub mod fonts;
pub mod history;
pub mod journey;
pub mod market;
pub mod marks;
pub mod shape;
pub mod store;
pub mod theme;
pub mod time;
pub mod traces;
pub mod wire;
pub mod zoom;

#[cfg(target_arch = "wasm32")]
mod web;
