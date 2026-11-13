//! Holds one live connection to GRVT and what it has told us so far.
//!
//! Time is passed in rather than read from a clock, because
//! `std::time::Instant` does not exist in the browser. The caller hands us
//! egui's frame time, which is the same number on both builds.

use std::collections::VecDeque;

use ewebsock::{WsEvent, WsMessage, WsReceiver, WsSender};

use crate::market::{decode, subscribe_frame, Env, Frame, Ticker, RATE_MS};

/// How many price points the chart keeps. At one tick per 200ms this is a
/// little over 8 minutes, which is what fits on the screen at a useful zoom.
const HISTORY: usize = 2_500;
const NOTES: usize = 40;

/// A stream that has said nothing for this long is gone, whatever the
/// socket claims.
///
/// At one reading every 200 ms this is forty missed readings. A socket that
/// is still "open" but silent is the ordinary shape of a dropped wifi
/// connection, and it is what leaves a price board showing a price that
/// stopped being true minutes ago - the worst failure this screen has,
/// because nothing on it looks wrong.
const SILENCE: f64 = 8.0;

/// How long to wait for a connection to open before giving up on it.
const HANDSHAKE: f64 = 12.0;

/// The longest wait between attempts. Growing the wait keeps a board that
/// is offline for an hour from hammering the exchange, and the ceiling
/// keeps it from waiting ten minutes once the network is back.
const RETRY_MAX: f64 = 30.0;

/// A price is called stale after this long without a reading. Well short
/// of `SILENCE`, because the screen should say "this stopped moving" before
/// the board gives up on the socket, not after.
const QUIET: f64 = 2.0;

/// What the board should say about the connection.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Health {
    /// Readings are arriving.
    Live,
    /// Connected, but nothing has arrived for a while: seconds of silence.
    Quiet(f64),
    /// Trying to open a connection.
    Connecting,
    /// No connection; seconds until the next attempt, when one is booked.
    Down(Option<f64>),
}

/// Judge the connection from what can be seen of it.
///
/// Pure, because this is the sentence a person reads before deciding
/// whether to believe the price next to it.
pub fn health(status: Status, silence: f64, retry_in: Option<f64>) -> Health {
    match status {
        Status::Live if silence > QUIET => Health::Quiet(silence),
        Status::Live => Health::Live,
        Status::Connecting => Health::Connecting,
        Status::Offline | Status::Failed => Health::Down(retry_in),
    }
}

/// Whether a connection in this state should be thrown away.
///
/// Free of the socket on purpose: a `WsSender` cannot be built in a test,
/// and the branch that matters most - open, connected, and silent - would
/// otherwise be the one branch nothing could check.
pub fn failure_of(
    connected: bool,
    status: Status,
    alive_since: f64,
    last_tick: Option<f64>,
    now: f64,
) -> Option<&'static str> {
    if !connected {
        return None;
    }
    match status {
        Status::Connecting if now - alive_since > HANDSHAKE => Some("ต่อไม่ติดสักที"),
        Status::Live => {
            let quiet = now - last_tick.unwrap_or(alive_since);
            (quiet > SILENCE).then_some("ไม่มีราคาเข้ามานาน")
        }
        _ => None,
    }
}

/// How long to wait before the next attempt, after this many failures.
pub fn backoff(attempts: u32) -> f64 {
    let seconds = 2.0_f64 * 2.0_f64.powi(attempts.saturating_sub(1).min(8) as i32);
    seconds.clamp(2.0, RETRY_MAX)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Offline,
    Connecting,
    Live,
    Failed,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Status::Offline => "ยังไม่ต่อ",
            Status::Connecting => "กำลังต่อ",
            Status::Live => "ต่ออยู่",
            Status::Failed => "หลุด",
        }
    }
}

pub struct Feed {
    pub env: Env,
    pub instrument: String,
    sender: Option<WsSender>,
    receiver: Option<WsReceiver>,
    pub status: Status,
    pub last: Option<Ticker>,
    /// (time in seconds since the app started, mark price)
    pub history: VecDeque<(f64, f64)>,
    pub notes: VecDeque<String>,
    pub ticks: u64,
    /// Ticks that arrived for an instrument nobody is looking at.
    pub ignored: u64,
    /// When the last reading arrived, in app seconds.
    pub last_tick: Option<f64>,
    /// When the current attempt started, so a handshake that never finishes
    /// is caught as well as a stream that goes quiet.
    alive_since: f64,
    /// Failures in a row, for the backoff.
    attempts: u32,
    /// The last instrument that was ignored, so the note is written once per
    /// instrument instead of twice a second.
    ignoring: Option<String>,
    /// When to try again after a failure, in app seconds.
    retry_at: Option<f64>,
}

impl Feed {
    pub fn new(env: Env, instrument: impl Into<String>) -> Self {
        Self {
            env,
            instrument: instrument.into(),
            sender: None,
            receiver: None,
            status: Status::Offline,
            last: None,
            history: VecDeque::new(),
            notes: VecDeque::new(),
            ticks: 0,
            ignored: 0,
            ignoring: None,
            last_tick: None,
            alive_since: 0.0,
            attempts: 0,
            retry_at: None,
        }
    }

    fn note(&mut self, text: impl Into<String>) {
        self.notes.push_front(text.into());
        while self.notes.len() > NOTES {
            self.notes.pop_back();
        }
    }

    pub fn connect(&mut self, now: f64) {
        self.status = Status::Connecting;
        self.retry_at = None;
        self.alive_since = now;
        self.last_tick = None;
        let url = self.env.market_data_url();
        match ewebsock::connect(url, ewebsock::Options::default()) {
            Ok((sender, receiver)) => {
                self.sender = Some(sender);
                self.receiver = Some(receiver);
                self.note(format!("[{now:.0}s] ต่อไปที่ {url}"));
            }
            Err(err) => {
                self.attempts += 1;
                self.status = Status::Failed;
                self.retry_at = Some(now + backoff(self.attempts));
                self.note(format!("ต่อไม่ได้: {err}"));
            }
        }
    }

    /// Switch instrument without tearing the socket down.
    pub fn watch(&mut self, instrument: impl Into<String>) {
        let instrument = instrument.into();
        if instrument == self.instrument {
            return;
        }
        self.instrument = instrument;
        self.history.clear();
        self.last = None;
        self.ignoring = None;
        if let Some(sender) = self.sender.as_mut() {
            sender.send(WsMessage::Text(subscribe_frame(
                1,
                &self.instrument,
                RATE_MS,
            )));
            self.note(format!("ขอ {} เพิ่ม", self.instrument));
        }
    }

    /// Why the connection on hand should be thrown away, if it should.
    ///
    /// Pure, so both ways of dying can be tested without a network: a
    /// handshake that never completes, and a stream that stops speaking
    /// while the socket still says it is open.
    pub fn failure(&self, now: f64) -> Option<&'static str> {
        failure_of(
            self.sender.is_some(),
            self.status,
            self.alive_since,
            self.last_tick,
            now,
        )
    }

    /// Throw the socket away and arrange to try again.
    fn give_up(&mut self, now: f64, why: &str) {
        self.status = Status::Failed;
        self.sender = None;
        self.receiver = None;
        self.attempts += 1;
        let wait = backoff(self.attempts);
        self.retry_at = Some(now + wait);
        self.note(format!("{why} · ต่อใหม่ในอีก {wait:.0} วินาที"));
    }

    /// How long the stream has been quiet, for the screen to show.
    /// What to say about this connection right now.
    pub fn health(&self, now: f64) -> Health {
        health(self.status, self.silence(now), self.retry_in(now))
    }

    /// Is the last price old enough that it should not be drawn as live?
    ///
    /// The chart stops folding a stale price into the newest candle: the
    /// candles themselves keep arriving over HTTP every ten seconds, so a
    /// dead stream costs the last few seconds of movement rather than the
    /// whole chart.
    pub fn stale(&self, now: f64) -> bool {
        !matches!(self.health(now), Health::Live)
    }

    pub fn silence(&self, now: f64) -> f64 {
        now - self.last_tick.unwrap_or(self.alive_since)
    }

    /// Seconds until the next attempt, while waiting to reconnect.
    pub fn retry_in(&self, now: f64) -> Option<f64> {
        self.retry_at.map(|at| (at - now).max(0.0))
    }

    /// Drain whatever arrived. Returns true when something changed on screen.
    pub fn poll(&mut self, now: f64) -> bool {
        if self.sender.is_none() {
            if let Some(at) = self.retry_at {
                if now >= at {
                    self.connect(now);
                }
            }
            return false;
        }

        // A socket that is open and silent has to be noticed here: no event
        // will ever arrive to announce it.
        if let Some(why) = self.failure(now) {
            self.give_up(now, why);
            return true;
        }

        let mut changed = false;
        // The receiver is drained through a raw pointer dance in some
        // designs; here it is simpler to take it out and put it back.
        let Some(receiver) = self.receiver.take() else {
            return false;
        };
        while let Some(event) = receiver.try_recv() {
            changed = true;
            match event {
                WsEvent::Opened => {
                    self.status = Status::Live;
                    self.alive_since = now;
                    if let Some(sender) = self.sender.as_mut() {
                        sender.send(WsMessage::Text(subscribe_frame(
                            1,
                            &self.instrument,
                            RATE_MS,
                        )));
                    }
                    self.note(format!("เปิดแล้ว ขอ {}", self.instrument));
                }
                WsEvent::Message(WsMessage::Text(text)) => match decode(&text) {
                    Frame::Tick(ticker) => {
                        self.accept(ticker, now);
                    }
                    Frame::Subscribed { stream, subs } => {
                        self.note(format!("รับแล้ว {stream} {subs:?}"));
                    }
                    Frame::Refused { code, message } => {
                        self.note(format!("ตลาดปฏิเสธ {code}: {message}"));
                    }
                    Frame::Unknown(what) => self.note(format!("ไม่รู้จัก: {what}")),
                },
                WsEvent::Message(_) => {}
                // An error used to set the status and keep the dead
                // socket, so the board sat on a corpse for ever. Both ways
                // out now go through the same door.
                WsEvent::Error(err) => {
                    self.give_up(now, &format!("ผิดพลาด: {err}"));
                    break;
                }
                WsEvent::Closed => {
                    self.give_up(now, "ตลาดปิดการเชื่อมต่อ");
                    break;
                }
            }
        }
        self.receiver = Some(receiver);
        changed
    }

    /// Take one tick, if it is about the instrument on screen.
    ///
    /// **This guard is the whole reason this is a method.** Switching coins
    /// adds a subscription to the socket that is already open; the exchange
    /// keeps sending the old one, so the stream carries several instruments
    /// at once. Without the guard, whichever tick arrived last wins, and the
    /// card prints SOL's price under BTC's name - a trading screen that is
    /// confidently wrong about which market it is showing, which is worse
    /// than a blank one.
    fn accept(&mut self, ticker: Ticker, now: f64) -> bool {
        if ticker.instrument != self.instrument {
            self.ignored += 1;
            if self.ignoring.as_deref() != Some(ticker.instrument.as_str()) {
                self.note(format!(
                    "ข้าม tick ของ {} เพราะกำลังดู {}",
                    ticker.instrument, self.instrument
                ));
                self.ignoring = Some(ticker.instrument);
            }
            return false;
        }
        self.ticks += 1;
        // Proof of life: both the watchdog and the backoff reset here.
        self.last_tick = Some(now);
        self.attempts = 0;
        if let Some(mark) = ticker.mark() {
            self.history.push_back((now, mark));
            while self.history.len() > HISTORY {
                self.history.pop_front();
            }
        }
        self.last = Some(ticker);
        true
    }

    /// Lowest and highest mark price on record, for scaling a chart.
    pub fn range(&self) -> Option<(f64, f64)> {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for (_, price) in &self.history {
            lo = lo.min(*price);
            hi = hi.max(*price);
        }
        (lo <= hi).then_some((lo, hi))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ticker(instrument: &str, mark: &str) -> Ticker {
        serde_json::from_value(serde_json::json!({
            "instrument": instrument,
            "mark_price": mark,
            "index_price": mark,
            "last_price": mark,
        }))
        .expect("ตัวอย่าง ticker ต้องอ่านได้")
    }

    #[test]
    fn the_screen_says_quiet_before_the_board_gives_up_on_the_socket() {
        assert_eq!(health(Status::Live, 0.4, None), Health::Live);
        assert_eq!(health(Status::Live, 3.0, None), Health::Quiet(3.0));
        // Quiet is reached well before the socket is thrown away, so the
        // price is marked as doubtful before anything is torn down.
        assert!(QUIET < SILENCE);
    }

    #[test]
    fn a_connection_that_is_down_says_when_it_will_try_again() {
        assert_eq!(
            health(Status::Failed, 99.0, Some(4.0)),
            Health::Down(Some(4.0))
        );
        assert_eq!(health(Status::Offline, 0.0, None), Health::Down(None));
        assert_eq!(health(Status::Connecting, 99.0, None), Health::Connecting);
    }

    #[test]
    fn a_stale_price_is_not_drawn_as_a_live_one() {
        let mut feed = Feed::new(Env::Testnet, "BTC_USDT_Perp");
        feed.status = Status::Live;
        feed.last_tick = Some(100.0);
        assert!(!feed.stale(100.5), "เพิ่งมา ยังสดอยู่");
        assert!(feed.stale(104.0), "เงียบสี่วินาที ห้ามวาดเป็นราคาสด");
        feed.status = Status::Failed;
        assert!(feed.stale(100.1));
    }

    #[test]
    fn the_wait_between_attempts_grows_and_then_stops_growing() {
        assert_eq!(backoff(0), 2.0);
        assert_eq!(backoff(1), 2.0);
        assert_eq!(backoff(2), 4.0);
        assert_eq!(backoff(3), 8.0);
        assert_eq!(backoff(4), 16.0);
        assert_eq!(backoff(5), RETRY_MAX, "เพดานกันไม่ให้รอเป็นสิบนาที");
        assert_eq!(backoff(99), RETRY_MAX);
    }

    #[test]
    fn an_open_socket_that_went_quiet_is_treated_as_gone() {
        // Nine seconds without a reading, on a stream that sends five a
        // second. The socket still says it is open; it is not.
        assert_eq!(
            failure_of(true, Status::Live, 0.0, Some(91.0), 100.0),
            Some("ไม่มีราคาเข้ามานาน")
        );
        // Three seconds is a hiccup, not a death.
        assert_eq!(failure_of(true, Status::Live, 0.0, Some(97.0), 100.0), None);
    }

    #[test]
    fn a_stream_that_has_never_spoken_is_judged_from_when_it_opened() {
        assert_eq!(
            failure_of(true, Status::Live, 90.0, None, 100.0),
            Some("ไม่มีราคาเข้ามานาน"),
            "เปิดมาสิบวินาทีแล้วยังเงียบ ก็คือไม่มา"
        );
        assert_eq!(failure_of(true, Status::Live, 97.0, None, 100.0), None);
    }

    #[test]
    fn a_handshake_that_never_finishes_is_given_up_on() {
        assert_eq!(
            failure_of(true, Status::Connecting, 80.0, None, 100.0),
            Some("ต่อไม่ติดสักที")
        );
        assert_eq!(
            failure_of(true, Status::Connecting, 95.0, None, 100.0),
            None
        );
    }

    #[test]
    fn nothing_is_thrown_away_when_there_is_no_connection_to_throw() {
        assert_eq!(failure_of(false, Status::Live, 0.0, None, 1_000.0), None);
        assert_eq!(failure_of(false, Status::Failed, 0.0, None, 1_000.0), None);
        // Offline is the state before the first attempt; the retry clock
        // owns that one, not the watchdog.
        assert_eq!(failure_of(true, Status::Offline, 0.0, None, 1_000.0), None);
    }

    #[test]
    fn silence_is_measured_from_the_last_reading_or_from_the_connection() {
        let mut feed = Feed::new(Env::Testnet, "BTC_USDT_Perp");
        feed.alive_since = 10.0;
        assert_eq!(feed.silence(18.0), 8.0, "ยังไม่เคยมี tick ให้นับจากตอนต่อ");
        feed.last_tick = Some(17.0);
        assert_eq!(feed.silence(18.0), 1.0);
    }

    #[test]
    fn a_tick_resets_both_the_watchdog_and_the_backoff() {
        let mut feed = Feed::new(Env::Testnet, "BTC_USDT_Perp");
        feed.attempts = 4;
        feed.accept(ticker("BTC_USDT_Perp", "68969.5"), 42.0);
        assert_eq!(feed.last_tick, Some(42.0));
        assert_eq!(feed.attempts, 0, "ต่อติดแล้วต้องเริ่มนับใหม่");
    }

    #[test]
    fn giving_up_drops_the_socket_and_books_another_attempt() {
        let mut feed = Feed::new(Env::Testnet, "BTC_USDT_Perp");
        feed.status = Status::Live;
        feed.give_up(100.0, "ทดสอบ");
        assert_eq!(feed.status, Status::Failed);
        assert!(feed.sender.is_none() && feed.receiver.is_none());
        assert_eq!(feed.retry_at, Some(102.0));
        assert_eq!(feed.retry_in(100.5), Some(1.5));
        assert!(feed.notes[0].contains("ต่อใหม่ในอีก"));

        // Each failure in a row waits longer than the last.
        feed.give_up(200.0, "ทดสอบ");
        assert_eq!(feed.retry_at, Some(204.0));
    }

    #[test]
    fn a_tick_for_another_coin_is_not_proof_that_this_one_is_alive() {
        let mut feed = Feed::new(Env::Testnet, "BTC_USDT_Perp");
        feed.accept(ticker("SOL_USDT_Perp", "78.38"), 50.0);
        assert_eq!(feed.last_tick, None, "เหรียญอื่นไม่นับว่าสายนี้ยังมีชีวิต");
    }

    #[test]
    fn a_tick_for_the_coin_on_screen_is_taken() {
        let mut feed = Feed::new(Env::Testnet, "BTC_USDT_Perp");
        assert!(feed.accept(ticker("BTC_USDT_Perp", "68969.5"), 1.0));
        assert_eq!(feed.ticks, 1);
        assert_eq!(feed.last.as_ref().unwrap().mark(), Some(68969.5));
    }

    #[test]
    fn a_tick_for_another_coin_never_becomes_this_coins_price() {
        // The socket keeps carrying an instrument after the board has moved
        // on. SOL at 78 must not appear under the BTC heading.
        let mut feed = Feed::new(Env::Testnet, "BTC_USDT_Perp");
        feed.accept(ticker("BTC_USDT_Perp", "68969.5"), 1.0);
        assert!(!feed.accept(ticker("SOL_USDT_Perp", "78.38"), 2.0));
        assert_eq!(feed.last.as_ref().unwrap().mark(), Some(68969.5));
        assert_eq!(feed.ticks, 1, "tick ของเหรียญอื่นต้องไม่ถูกนับ");
        assert_eq!(feed.ignored, 1);
        assert_eq!(feed.history.len(), 1, "ราคาของเหรียญอื่นต้องไม่เข้ากราฟ");
    }

    #[test]
    fn the_board_says_once_which_coin_it_is_ignoring_not_twice_a_second() {
        let mut feed = Feed::new(Env::Testnet, "BTC_USDT_Perp");
        for _ in 0..50 {
            feed.accept(ticker("SOL_USDT_Perp", "78.38"), 1.0);
        }
        assert_eq!(feed.ignored, 50);
        assert_eq!(feed.notes.len(), 1);
        assert!(feed.notes[0].contains("SOL_USDT_Perp"));
    }

    #[test]
    fn switching_coins_throws_away_the_old_coins_numbers() {
        let mut feed = Feed::new(Env::Testnet, "BTC_USDT_Perp");
        feed.accept(ticker("BTC_USDT_Perp", "68969.5"), 1.0);
        feed.watch("SOL_USDT_Perp");
        assert!(feed.last.is_none(), "ราคาเก่าต้องไม่ค้างอยู่ใต้ชื่อเหรียญใหม่");
        assert!(feed.history.is_empty());
        assert!(feed.accept(ticker("SOL_USDT_Perp", "78.38"), 2.0));
    }
}
