//! Where the board's own server is.
//!
//! In the browser there is nothing to decide: the page was served by that
//! server, so a relative path is both correct and the only thing that keeps
//! working when the board is deployed behind someone else's hostname.
//!
//! The native window has no page and therefore no origin, and a relative
//! path there is not a URL at all. It reads `BOARD_API` instead, so the
//! desktop build - which compiles in seconds rather than minutes and is the
//! quick way to work on these cards - can watch a real bot through a server
//! that is already running.

/// The board server's address for the native build, when `BOARD_API` says
/// nothing. The same port the server listens on by default.
#[cfg(not(target_arch = "wasm32"))]
const DEFAULT_BASE: &str = "http://127.0.0.1:4340";

/// How long a request may be in the air before the board stops waiting for
/// it and asks again.
///
/// Every fetch on this board is guarded by "one at a time", which is what
/// keeps a 60 fps repaint from flooding the server. If an answer never
/// arrives - a connection that dies mid-flight, a laptop that slept - that
/// guard never lifts and the card behind it is frozen for the rest of the
/// session, with nothing on screen to say so.
pub const LOST: f64 = 20.0;

/// Is a request sent at `sent_at` still worth waiting for?
pub fn still_waiting(sent_at: Option<f64>, now: f64) -> bool {
    matches!(sent_at, Some(at) if now - at < LOST && now >= at)
}

/// Turn a route into something the HTTP client can fetch.
///
/// `path` always starts with a slash, as it appears in the server's router.
pub fn url(path: &str) -> String {
    #[cfg(target_arch = "wasm32")]
    {
        path.to_owned()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        join(&base(), path)
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn base() -> String {
    std::env::var("BOARD_API")
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.to_owned())
}

/// Glue a base and a path together with exactly one slash between them.
#[cfg(not(target_arch = "wasm32"))]
fn join(base: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    #[test]
    fn a_request_that_never_came_back_stops_blocking_the_next_one() {
        assert!(still_waiting(Some(100.0), 105.0), "ห้าวินาที ยังรอได้");
        assert!(
            !still_waiting(Some(100.0), 100.0 + LOST),
            "เกินเวลาแล้วต้องถามใหม่ ไม่ใช่ค้างทั้ง session"
        );
        assert!(!still_waiting(None, 100.0), "ไม่ได้ส่งอะไรออกไป ก็ไม่ได้รออะไร");
    }

    #[test]
    fn a_clock_that_jumped_backwards_does_not_wedge_a_card() {
        // egui's frame clock starts at zero on a fresh window; a stored
        // timestamp from the future would otherwise block for ever.
        assert!(!still_waiting(Some(500.0), 1.0));
    }

    #[test]
    fn a_base_with_a_trailing_slash_does_not_produce_a_double_one() {
        assert_eq!(
            join("http://127.0.0.1:4340/", "/api/state"),
            "http://127.0.0.1:4340/api/state"
        );
        assert_eq!(
            join("http://127.0.0.1:4340", "/api/state"),
            "http://127.0.0.1:4340/api/state"
        );
    }

    #[test]
    fn a_path_with_a_query_string_survives_whole() {
        assert_eq!(
            join("http://host", "/api/candles?symbol=BTC_USDT_Perp&limit=400"),
            "http://host/api/candles?symbol=BTC_USDT_Perp&limit=400"
        );
    }
}
