//! The account, as the exchange sees it.
//!
//! Everything else on this board is either the public market or the bot's
//! own opinion of itself, read out of the files it writes. This is the
//! third thing, and the one that settles arguments: the balance, the
//! positions, the orders and the fees **as GRVT holds them**.
//!
//! Why it matters enough to exist: the bot's audit trail says what the bot
//! believed. If an order was rejected, if a fill came back at a worse price,
//! if funding was charged overnight, the bot's number and the exchange's
//! number part company - and the exchange's is the one with the money in it.
//! A student who only ever sees the bot's own figure learns to trust the
//! wrong one.
//!
//! Rules carried over from `exchange.rs`:
//!
//! * **Numbers travel as the exchange wrote them.** The text is printed; the
//!   `f64` only decides a colour or the length of a bar.
//! * **Nothing here can place an order.** The board asks the board's server,
//!   the server asks six read-only endpoints, and the key never leaves it.
//!   See `server/src/account.rs` for the allowlist.

use std::sync::mpsc::{channel, Receiver, Sender};

use serde_json::Value;

pub use crate::candles::Load;
use crate::exchange::{clock, Num};

/// How often each thing is asked for again, in seconds.
///
/// Money and positions move with the market; a list of closed trades does
/// not, and asking for it every second would be noise on somebody's
/// connection for no gain on screen.
// Funding last: it is charged on the hour at most, so a minute is plenty.
const EVERY: [f64; 7] = [3.0, 3.0, 4.0, 20.0, 20.0, 30.0, 60.0];

/// How often the order lists are asked for while an order is on its way.
const HURRY: f64 = 2.0;

/// How many rows of each history. Enough to fill a card and scroll a little.
const ROWS: u32 = 40;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Balance,
    Positions,
    OpenOrders,
    Orders,
    Fills,
    Closed,
    /// Funding the account was charged or paid - not the rate, which is
    /// public and lives in the market zone, but the money that moved.
    Funding,
}

impl Kind {
    pub const ALL: [Kind; 7] = [
        Kind::Balance,
        Kind::Positions,
        Kind::OpenOrders,
        Kind::Orders,
        Kind::Fills,
        Kind::Closed,
        Kind::Funding,
    ];

    fn index(self) -> usize {
        match self {
            Kind::Balance => 0,
            Kind::Positions => 1,
            Kind::OpenOrders => 2,
            Kind::Orders => 3,
            Kind::Fills => 4,
            Kind::Closed => 5,
            Kind::Funding => 6,
        }
    }

    /// The route on the board's own server. The names are the server's
    /// allowlist, not GRVT's endpoint names.
    fn route(self) -> String {
        match self {
            Kind::Balance => "/api/account/summary".into(),
            Kind::Positions => "/api/account/positions".into(),
            Kind::OpenOrders => "/api/account/open_orders".into(),
            Kind::Orders => format!("/api/account/order_history?limit={ROWS}"),
            Kind::Fills => format!("/api/account/fills?limit={ROWS}"),
            Kind::Closed => format!("/api/account/closed?limit={ROWS}"),
            Kind::Funding => format!("/api/account/funding?limit={ROWS}"),
        }
    }
}

// --------------------------------------------------------------- the things

/// What the account is worth, and how much of it is already committed.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Balance {
    pub equity: Option<Num>,
    pub available: Option<Num>,
    pub unrealized: Option<Num>,
    pub initial_margin: Option<Num>,
    pub maintenance_margin: Option<Num>,
    pub currency: String,
    pub margin_type: String,
    pub at: String,
}

impl Balance {
    /// How much of the account is being used as margin right now, 0..1.
    ///
    /// The number a person actually wants from a margin account, and the one
    /// neither field gives on its own.
    pub fn used(&self) -> Option<f64> {
        let equity = self.equity.as_ref()?.value;
        let margin = self.initial_margin.as_ref()?.value;
        (equity > 0.0).then(|| (margin / equity).clamp(0.0, 1.0))
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Position {
    pub instrument: String,
    /// GRVT signs the size: negative is short. The board prints the sign.
    pub size: Option<Num>,
    pub entry: Option<Num>,
    pub mark: Option<Num>,
    pub unrealized: Option<Num>,
    pub realized: Option<Num>,
    pub leverage: Option<Num>,
    pub liquidation: Option<Num>,
    pub margin_type: String,
}

impl Position {
    pub fn is_long(&self) -> bool {
        self.size
            .as_ref()
            .map(|size| size.value >= 0.0)
            .unwrap_or(true)
    }

    pub fn side(&self) -> &'static str {
        if self.is_long() {
            "ซื้อ"
        } else {
            "ขาย"
        }
    }
}

/// An order, whether still working or finished. One shape for both: the
/// only difference on screen is which card it sits in and what `status`
/// says.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Order {
    pub instrument: String,
    pub is_buy: bool,
    pub size: Option<Num>,
    pub price: Option<Num>,
    pub filled: Option<Num>,
    pub avg_price: Option<Num>,
    pub status: String,
    pub reject: String,
    pub is_market: bool,
    pub reduce_only: bool,
    pub client_order_id: String,
    pub at: String,
    /// The exchange's own id, once it has given one.
    pub order_id: String,
    /// When the exchange took the order in, and when its state last moved,
    /// as Unix seconds to the nanosecond the exchange wrote. These are what
    /// "how long did it wait" is measured with; `at` is only for printing.
    pub created: Option<f64>,
    pub updated: Option<f64>,
}

impl Order {
    pub fn side(&self) -> &'static str {
        if self.is_buy {
            "ซื้อ"
        } else {
            "ขาย"
        }
    }

    /// How much of it traded, 0..1, for the little bar on the row.
    pub fn done(&self) -> Option<f64> {
        let size = self.size.as_ref()?.value;
        let filled = self.filled.as_ref()?.value;
        (size > 0.0).then(|| (filled / size).clamp(0.0, 1.0))
    }
}

/// One trade that actually happened, with what it cost.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Fill {
    pub instrument: String,
    pub is_buy: bool,
    pub is_taker: bool,
    pub size: Option<Num>,
    pub price: Option<Num>,
    pub fee: Option<Num>,
    pub fee_rate: Option<Num>,
    pub realized: Option<Num>,
    pub currency: String,
    pub at: String,
    /// Which order this trade filled: the bot's id for it, and the exchange's.
    pub client_order_id: String,
    pub order_id: String,
    /// When it traded, as Unix seconds.
    pub when: Option<f64>,
}

/// One funding payment, as the exchange recorded it.
///
/// GRVT's own definition (`FundingPayment` in its SDK): `amount` is
/// **positive when the account paid and negative when it received** - the
/// opposite of the sign most people expect from a balance. The card turns it
/// round so that money out reads as a minus, like every other figure here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FundingPayment {
    pub instrument: String,
    pub currency: String,
    pub amount: Option<Num>,
    pub at: String,
}

impl FundingPayment {
    /// What this payment did to the account: negative when it cost money.
    pub fn effect(&self) -> Option<f64> {
        self.amount.as_ref().map(|amount| -amount.value)
    }
}

/// A position that is over, with the whole story in one row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Closed {
    pub instrument: String,
    pub is_long: bool,
    pub size: Option<Num>,
    pub entry: Option<Num>,
    pub exit: Option<Num>,
    pub realized: Option<Num>,
    pub fees: Option<Num>,
    pub funding: Option<Num>,
    pub status: String,
    pub opened_at: String,
    pub closed_at: String,
}

impl Closed {
    /// What the trade made after what it cost to make it.
    ///
    /// `realized_pnl` already has the fees out of it on GRVT, so this is not
    /// a subtraction - it is here to be named, because "I made 12 dollars"
    /// and "I made 12 dollars and paid 9 in fees" are different lessons.
    pub fn after_costs(&self) -> Option<f64> {
        Some(self.realized.as_ref()?.value)
    }

    /// Fees as a share of what the trade made or lost, when both are known.
    pub fn fee_share(&self) -> Option<f64> {
        let fees = self.fees.as_ref()?.value;
        let pnl = self.realized.as_ref()?.value.abs();
        (pnl > 0.0).then(|| fees / pnl)
    }
}

// ------------------------------------------------------------- the readers

fn text_of(value: Option<&Value>) -> String {
    value
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned()
}

/// GRVT writes its times as nanoseconds since 1970, in a string. As Unix
/// seconds, or `None` for anything that is not one.
pub fn nanos_of(value: Option<&Value>) -> Option<f64> {
    let text = match value? {
        Value::String(text) => text.trim().to_owned(),
        Value::Number(number) => number.to_string(),
        _ => return None,
    };
    let nanos: i128 = text.parse().ok()?;
    (nanos > 0).then(|| nanos as f64 / 1e9)
}

fn flag_of(value: Option<&Value>) -> bool {
    value.and_then(|v| v.as_bool()).unwrap_or(false)
}

/// The exchange answers some questions with a bare list and others with an
/// object holding one. Both mean the same thing here.
fn rows_of(result: &Value) -> Vec<&Value> {
    match result {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => result
            .get("result")
            .and_then(|inner| inner.as_array())
            .map(|items| items.iter().collect())
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

pub fn read_balance(result: &Value) -> Balance {
    Balance {
        equity: Num::from(result.get("total_equity")),
        available: Num::from(result.get("available_balance")),
        unrealized: Num::from(result.get("unrealized_pnl")),
        initial_margin: Num::from(result.get("initial_margin")),
        maintenance_margin: Num::from(result.get("maintenance_margin")),
        currency: text_of(result.get("settle_currency")),
        margin_type: text_of(result.get("margin_type")),
        at: clock(&text_of(result.get("event_time_at"))),
    }
}

pub fn read_positions(result: &Value) -> Vec<Position> {
    rows_of(result)
        .into_iter()
        .map(|row| Position {
            instrument: text_of(row.get("instrument")),
            size: Num::from(row.get("size")),
            entry: Num::from(row.get("entry_price")),
            mark: Num::from(row.get("mark_price")),
            unrealized: Num::from(row.get("unrealized_pnl")),
            realized: Num::from(row.get("realized_pnl")),
            leverage: Num::from(row.get("leverage")),
            liquidation: Num::from(row.get("est_liquidation_price")),
            margin_type: text_of(row.get("margin_type")),
        })
        .collect()
}

/// An order's numbers live three levels down: the instrument and price are
/// on its first leg, the status is in `state`, and the id is in `metadata`.
pub fn read_orders(result: &Value) -> Vec<Order> {
    rows_of(result)
        .into_iter()
        .map(|row| {
            let leg = row.get("legs").and_then(|legs| legs.get(0));
            let state = row.get("state");
            let meta = row.get("metadata");
            // GRVT reports these per leg, as lists; every order this board
            // sends has one leg.
            let first = |name: &str| {
                state
                    .and_then(|state| state.get(name))
                    .and_then(|value| value.get(0))
                    .cloned()
            };
            Order {
                instrument: text_of(leg.and_then(|leg| leg.get("instrument"))),
                is_buy: flag_of(leg.and_then(|leg| leg.get("is_buying_asset"))),
                size: Num::from(leg.and_then(|leg| leg.get("size"))),
                price: Num::from(leg.and_then(|leg| leg.get("limit_price"))),
                filled: Num::from(first("traded_size").as_ref()),
                avg_price: Num::from(first("avg_fill_price").as_ref()),
                status: text_of(state.and_then(|state| state.get("status"))),
                reject: text_of(state.and_then(|state| state.get("reject_reason"))),
                is_market: flag_of(row.get("is_market")),
                reduce_only: flag_of(row.get("reduce_only")),
                client_order_id: text_of(meta.and_then(|meta| meta.get("client_order_id"))),
                at: clock(&text_of(meta.and_then(|meta| meta.get("create_time_at")))),
                order_id: text_of(row.get("order_id")),
                created: nanos_of(meta.and_then(|meta| meta.get("create_time"))),
                updated: nanos_of(state.and_then(|state| state.get("update_time"))),
            }
        })
        .collect()
}

pub fn read_fills(result: &Value) -> Vec<Fill> {
    rows_of(result)
        .into_iter()
        .map(|row| Fill {
            instrument: text_of(row.get("instrument")),
            is_buy: flag_of(row.get("is_buyer")),
            is_taker: flag_of(row.get("is_taker")),
            size: Num::from(row.get("size")),
            price: Num::from(row.get("price")),
            fee: Num::from(row.get("fee")),
            fee_rate: Num::from(row.get("fee_rate")),
            realized: Num::from(row.get("realized_pnl")),
            currency: text_of(row.get("fee_currency")),
            at: clock(&text_of(row.get("event_time_at"))),
            client_order_id: text_of(row.get("client_order_id")),
            order_id: text_of(row.get("order_id")),
            when: nanos_of(row.get("event_time")),
        })
        .collect()
}

pub fn read_funding(result: &Value) -> Vec<FundingPayment> {
    rows_of(result)
        .into_iter()
        .map(|row| FundingPayment {
            instrument: text_of(row.get("instrument")),
            currency: text_of(row.get("currency")),
            amount: Num::from(row.get("amount")),
            at: clock(&text_of(row.get("event_time_at"))),
        })
        .collect()
}

pub fn read_closed(result: &Value) -> Vec<Closed> {
    rows_of(result)
        .into_iter()
        .map(|row| Closed {
            instrument: text_of(row.get("instrument")),
            is_long: flag_of(row.get("is_long")),
            size: Num::from(row.get("closed_volume_base")),
            entry: Num::from(row.get("entry_price")),
            exit: Num::from(row.get("exit_price")),
            realized: Num::from(row.get("realized_pnl")),
            fees: Num::from(row.get("cumulative_fee")),
            funding: Num::from(row.get("cumulative_realized_funding_payment")),
            status: text_of(row.get("status")),
            opened_at: clock(&text_of(row.get("open_time_at"))),
            closed_at: clock(&text_of(row.get("close_time_at"))),
        })
        .collect()
}

/// How far the figure the bot kept has drifted from the one the exchange
/// holds, when that is worth saying.
///
/// They are never identical and should not be expected to be. The bot marks
/// its equity when it last looked at a price; the exchange marks it
/// continuously, charges funding on the hour and takes a fee out of every
/// fill. A cent of difference is arithmetic. A dollar of it is a fill the
/// bot did not see, an order it thinks is still working, or a fee nobody
/// budgeted for - and on a teaching board that is worth a line of its own
/// rather than a number quietly replaced.
pub fn drift(exchange: f64, bot: f64) -> Option<f64> {
    if !exchange.is_finite() || !bot.is_finite() || bot <= 0.0 {
        return None;
    }
    let gap = exchange - bot;
    (gap.abs() >= 0.01).then_some(gap)
}

// ------------------------------------------------------------- the fetching

enum Got {
    Balance(Answer<Box<Balance>>),
    Positions(Answer<Vec<Position>>),
    OpenOrders(Answer<Vec<Order>>),
    Orders(Answer<Vec<Order>>),
    Fills(Answer<Vec<Fill>>),
    Closed(Answer<Vec<Closed>>),
    Funding(Answer<Vec<FundingPayment>>),
}

enum Answer<T> {
    Have(T),
    Missing(String),
    Failed(String),
}

pub struct Account {
    pub balance: Option<Balance>,
    pub positions: Vec<Position>,
    pub open_orders: Vec<Order>,
    pub orders: Vec<Order>,
    pub fills: Vec<Fill>,
    pub closed: Vec<Closed>,
    pub funding: Vec<FundingPayment>,
    pub load: [Load; 7],
    tx: Sender<Got>,
    rx: Receiver<Got>,
    due: [f64; 7],
    flight: [Option<f64>; 7],
}

impl Default for Account {
    fn default() -> Self {
        let (tx, rx) = channel();
        Self {
            balance: None,
            positions: Vec::new(),
            open_orders: Vec::new(),
            orders: Vec::new(),
            fills: Vec::new(),
            closed: Vec::new(),
            funding: Vec::new(),
            load: [
                Load::Idle,
                Load::Idle,
                Load::Idle,
                Load::Idle,
                Load::Idle,
                Load::Idle,
                Load::Idle,
            ],
            tx,
            rx,
            due: [0.0; 7],
            flight: [None; 7],
        }
    }
}

impl Account {
    pub fn poll(&mut self, now: f64) {
        for kind in Kind::ALL {
            let index = kind.index();
            if crate::api::still_waiting(self.flight[index], now) || now < self.due[index] {
                continue;
            }
            self.flight[index] = Some(now);
            self.due[index] = now + EVERY[index];
            if self.load[index] == Load::Idle {
                self.load[index] = Load::Loading;
            }
            self.fetch(kind);
        }

        while let Ok(got) = self.rx.try_recv() {
            match got {
                Got::Balance(answer) => self.take(Kind::Balance, answer, |me, balance| {
                    me.balance = Some(*balance)
                }),
                Got::Positions(answer) => {
                    self.take(Kind::Positions, answer, |me, rows| me.positions = rows)
                }
                Got::OpenOrders(answer) => {
                    self.take(Kind::OpenOrders, answer, |me, rows| me.open_orders = rows)
                }
                Got::Orders(answer) => self.take(Kind::Orders, answer, |me, rows| me.orders = rows),
                Got::Fills(answer) => self.take(Kind::Fills, answer, |me, rows| me.fills = rows),
                Got::Closed(answer) => self.take(Kind::Closed, answer, |me, rows| me.closed = rows),
                Got::Funding(answer) => {
                    self.take(Kind::Funding, answer, |me, rows| me.funding = rows)
                }
            }
        }
    }

    fn take<T>(&mut self, kind: Kind, answer: Answer<T>, keep: impl FnOnce(&mut Self, T)) {
        let index = kind.index();
        self.flight[index] = None;
        match answer {
            Answer::Have(thing) => {
                keep(self, thing);
                self.load[index] = Load::Ready;
            }
            Answer::Missing(reason) => self.load[index] = Load::Missing(reason),
            Answer::Failed(reason) => self.load[index] = Load::Failed(reason),
        }
    }

    /// Ask again soon for the three lists an order moves through. Called
    /// while one of the bot's orders is still on its way, so the board sees
    /// it land in the book and fill within a couple of seconds instead of a
    /// history refresh later. Only ever brings a fetch forward.
    pub fn hurry(&mut self, now: f64) {
        for kind in [Kind::OpenOrders, Kind::Orders, Kind::Fills] {
            let index = kind.index();
            self.due[index] = self.due[index].min(now + HURRY);
        }
    }

    pub fn load_of(&self, kind: Kind) -> &Load {
        &self.load[kind.index()]
    }

    /// Is there a key at all? Every card answers the same way when there is
    /// not, so the board asks once and says it once.
    pub fn has_key(&self) -> bool {
        !matches!(self.load_of(Kind::Balance), Load::Missing(_))
    }

    /// Why the account side is empty, when it is.
    pub fn why_not(&self) -> Option<&str> {
        match self.load_of(Kind::Balance) {
            Load::Missing(reason) => Some(reason),
            _ => None,
        }
    }

    fn fetch(&self, kind: Kind) {
        let tx = self.tx.clone();
        let url = crate::api::url(&kind.route());
        ehttp::fetch(ehttp::Request::get(url), move |result| {
            let _ = tx.send(decode(kind, result));
        });
    }
}

fn result_of(result: Result<ehttp::Response, String>) -> Result<Value, String> {
    let response = result.map_err(|err| format!("ขอข้อมูลไม่สำเร็จ: {err}"))?;
    if !response.ok {
        return Err(format!("เซิร์ฟเวอร์ตอบ {}", response.status));
    }
    let text = response
        .text()
        .ok_or_else(|| "คำตอบไม่ใช่ข้อความ".to_string())?;
    serde_json::from_str(text).map_err(|err| format!("อ่าน JSON ไม่ได้: {err}"))
}

fn decode(kind: Kind, result: Result<ehttp::Response, String>) -> Got {
    let value = match result_of(result) {
        Ok(value) => value,
        Err(err) => return failed(kind, err),
    };
    if value.get("present").and_then(|present| present.as_bool()) != Some(true) {
        let reason = value
            .get("reason")
            .and_then(|reason| reason.as_str())
            .unwrap_or("ยังไม่มีข้อมูลบัญชี")
            .to_owned();
        return missing(kind, reason);
    }
    let result = value.get("result").cloned().unwrap_or(Value::Null);
    match kind {
        Kind::Balance => Got::Balance(Answer::Have(Box::new(read_balance(&result)))),
        Kind::Positions => Got::Positions(Answer::Have(read_positions(&result))),
        Kind::OpenOrders => Got::OpenOrders(Answer::Have(read_orders(&result))),
        Kind::Orders => Got::Orders(Answer::Have(read_orders(&result))),
        Kind::Fills => Got::Fills(Answer::Have(read_fills(&result))),
        Kind::Closed => Got::Closed(Answer::Have(read_closed(&result))),
        Kind::Funding => Got::Funding(Answer::Have(read_funding(&result))),
    }
}

fn missing(kind: Kind, reason: String) -> Got {
    match kind {
        Kind::Balance => Got::Balance(Answer::Missing(reason)),
        Kind::Positions => Got::Positions(Answer::Missing(reason)),
        Kind::OpenOrders => Got::OpenOrders(Answer::Missing(reason)),
        Kind::Orders => Got::Orders(Answer::Missing(reason)),
        Kind::Fills => Got::Fills(Answer::Missing(reason)),
        Kind::Closed => Got::Closed(Answer::Missing(reason)),
        Kind::Funding => Got::Funding(Answer::Missing(reason)),
    }
}

fn failed(kind: Kind, reason: String) -> Got {
    match kind {
        Kind::Balance => Got::Balance(Answer::Failed(reason)),
        Kind::Positions => Got::Positions(Answer::Failed(reason)),
        Kind::OpenOrders => Got::OpenOrders(Answer::Failed(reason)),
        Kind::Orders => Got::Orders(Answer::Failed(reason)),
        Kind::Fills => Got::Fills(Answer::Failed(reason)),
        Kind::Closed => Got::Closed(Answer::Failed(reason)),
        Kind::Funding => Got::Funding(Answer::Failed(reason)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The real reply from GRVT testnet on 13 Nov 2026, trimmed.
    fn summary() -> Value {
        json!({
            "event_time": "1794560051486337934",
            "event_time_at": "2026-11-13T09:54:11Z",
            "sub_account_id": "5421993573613261",
            "margin_type": "SIMPLE_CROSS_MARGIN",
            "settle_currency": "USDT",
            "total_equity": "9966.404007",
            "available_balance": "9966.404007",
            "unrealized_pnl": "0.0",
            "initial_margin": "996.260248",
            "maintenance_margin": "0.0",
            "spot_balances": [{ "currency": "USDT", "balance": "9966.404007" }]
        })
    }

    #[test]
    fn the_balance_is_read_as_the_exchange_wrote_it() {
        let balance = read_balance(&summary());
        assert_eq!(balance.equity.as_ref().unwrap().text, "9966.404007");
        assert_eq!(balance.currency, "USDT");
        assert_eq!(balance.at, "16:54:11", "เวลาไทย = UTC+7");
    }

    #[test]
    fn how_much_of_the_account_is_committed_is_a_share_of_equity() {
        let balance = read_balance(&summary());
        let used = balance.used().unwrap();
        assert!(
            (used - 0.1).abs() < 0.001,
            "996 ใน 9966 คือราวหนึ่งในสิบ: {used}"
        );
    }

    #[test]
    fn an_account_with_no_equity_reports_no_share_rather_than_dividing_by_zero() {
        let mut balance = read_balance(&summary());
        balance.equity = Num::from(Some(&json!("0")));
        assert_eq!(balance.used(), None);
    }

    #[test]
    fn an_order_is_read_out_of_its_leg_its_state_and_its_metadata() {
        // The shape GRVT really returns: one leg, lists in state.
        let orders = read_orders(&json!([{
            "is_market": false,
            "reduce_only": false,
            "legs": [{
                "instrument": "BTC_USDT_Perp",
                "size": "0.145",
                "limit_price": "68769.2",
                "is_buying_asset": true
            }],
            "state": {
                "status": "FILLED",
                "reject_reason": "UNSPECIFIED",
                "traded_size": ["0.145"],
                "avg_fill_price": ["68769.2"]
            },
            "metadata": {
                "client_order_id": "2724522454",
                "create_time_at": "2026-11-13T08:59:45Z"
            }
        }]));
        let order = &orders[0];
        assert_eq!(order.instrument, "BTC_USDT_Perp");
        assert_eq!(order.side(), "ซื้อ");
        assert_eq!(order.price.as_ref().unwrap().text, "68769.2");
        assert_eq!(order.filled.as_ref().unwrap().text, "0.145");
        assert_eq!(order.status, "FILLED");
        assert_eq!(order.at, "15:59:45", "เวลาไทย = UTC+7");
        assert_eq!(order.done(), Some(1.0));
    }

    #[test]
    fn an_order_that_has_not_traded_yet_shows_an_empty_bar_not_a_full_one() {
        let orders = read_orders(&json!([{
            "legs": [{ "instrument": "BTC_USDT_Perp", "size": "0.145", "limit_price": "1.0" }],
            "state": { "status": "OPEN", "traded_size": ["0.0"] },
            "metadata": {}
        }]));
        assert_eq!(orders[0].done(), Some(0.0));
        assert_eq!(orders[0].side(), "ขาย", "ไม่มี is_buying_asset คือไม่ได้ซื้อ");
    }

    #[test]
    fn a_fill_carries_what_it_cost_which_is_the_whole_point_of_the_card() {
        let fills = read_fills(&json!([{
            "instrument": "BTC_USDT_Perp",
            "is_buyer": true,
            "is_taker": true,
            "size": "0.145",
            "price": "68836.7",
            "fee": "4.491594",
            "fee_rate": "0.045",
            "fee_currency": "USDT",
            "realized_pnl": "-17.7915",
            "event_time_at": "2026-11-13T09:03:06Z"
        }]));
        let fill = &fills[0];
        assert_eq!(fill.fee.as_ref().unwrap().text, "4.491594");
        assert_eq!(fill.fee_rate.as_ref().unwrap().value, 0.045);
        assert!(fill.is_taker);
        assert_eq!(fill.at, "16:03:06", "เวลาไทย = UTC+7");
    }

    #[test]
    fn a_closed_position_carries_both_ends_and_what_it_cost_in_between() {
        let rows = read_closed(&json!([{
            "instrument": "BTC_USDT_Perp",
            "is_long": false,
            "closed_volume_base": "0.145",
            "entry_price": "68714.0",
            "exit_price": "68836.7",
            "realized_pnl": "-17.7915",
            "cumulative_fee": "8.975182",
            "cumulative_realized_funding_payment": "0.0",
            "status": "CLOSED",
            "open_time_at": "2026-11-13T09:00:50Z",
            "close_time_at": "2026-11-13T09:03:06Z"
        }]));
        let closed = &rows[0];
        assert!(!closed.is_long);
        assert_eq!(closed.after_costs(), Some(-17.7915));
        assert_eq!(closed.opened_at, "16:00:50", "เวลาไทย = UTC+7");
        // Fees were half the loss: the lesson the demo strategy exists to teach.
        let share = closed.fee_share().unwrap();
        assert!((share - 0.5046).abs() < 0.001, "{share}");
    }

    #[test]
    fn a_position_that_is_short_says_so_from_the_sign_of_its_size() {
        let rows = read_positions(&json!([{
            "instrument": "BTC_USDT_Perp",
            "size": "-0.145",
            "entry_price": "68714.0",
            "unrealized_pnl": "-12.5"
        }]));
        assert!(!rows[0].is_long());
        assert_eq!(rows[0].side(), "ขาย");
    }

    #[test]
    fn an_empty_answer_is_an_empty_list_not_a_row_of_blanks() {
        assert!(read_positions(&json!([])).is_empty());
        assert!(read_orders(&Value::Null).is_empty());
        assert!(read_fills(&json!({ "nothing": true })).is_empty());
    }

    #[test]
    fn a_list_wrapped_in_an_object_is_still_a_list() {
        let rows = read_positions(&json!({ "result": [{ "instrument": "ETH_USDT_Perp" }] }));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].instrument, "ETH_USDT_Perp");
    }

    #[test]
    fn a_difference_worth_a_line_is_reported_and_rounding_is_not() {
        // The run on 13 Nov: the bot had 10000 on its books while the
        // exchange had already taken the fees out.
        assert_eq!(drift(9966.404007, 9966.404007), None);
        assert_eq!(drift(9966.40, 9966.404), None, "เศษสตางค์ไม่ใช่เรื่อง");
        let gap = drift(9966.40, 10_000.0).unwrap();
        assert!((gap + 33.6).abs() < 0.01, "{gap}");
    }

    #[test]
    fn a_bot_that_has_not_written_a_figure_yet_is_not_a_disagreement() {
        assert_eq!(drift(9966.40, 0.0), None);
        assert_eq!(drift(f64::NAN, 100.0), None);
    }

    #[test]
    fn a_board_with_no_key_knows_it_and_can_say_why() {
        let mut account = Account::default();
        assert!(account.has_key(), "ยังไม่ได้ถาม ก็ยังไม่รู้ว่าไม่มี");
        account.load[0] = Load::Missing("ยังไม่ได้ตั้งกุญแจ".into());
        assert!(!account.has_key());
        assert_eq!(account.why_not(), Some("ยังไม่ได้ตั้งกุญแจ"));
    }

    // ----------------------------------------------------------- funding

    /// Shaped exactly like GRVT's `FundingPayment`, as the board's server
    /// passes it on with the readable twin of `event_time` added.
    fn payments() -> Value {
        json!([
            {
                "event_time": "1794588431000000000",
                "event_time_at": "2026-11-13T16:47:11Z",
                "sub_account_id": "1234",
                "instrument": "BTC_USDT_Perp",
                "currency": "USDT",
                "amount": "0.4213",
                "tx_id": "77"
            },
            {
                "event_time": "1794592031000000000",
                "event_time_at": "2026-11-13T17:47:11Z",
                "instrument": "BTC_USDT_Perp",
                "currency": "USDT",
                "amount": "-0.1000",
                "tx_id": "78"
            }
        ])
    }

    #[test]
    fn a_funding_payment_is_read_as_the_exchange_wrote_it() {
        let rows = read_funding(&payments());
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].instrument, "BTC_USDT_Perp");
        assert_eq!(rows[0].amount.as_ref().unwrap().text, "0.4213");
        assert_eq!(rows[0].at, "23:47:11", "เวลาไทย");
    }

    /// GRVT counts a payment as positive when the account paid. The card
    /// must not repeat that sign, or paying funding reads as earning it.
    #[test]
    fn paying_funding_is_a_minus_on_the_card_and_receiving_it_is_a_plus() {
        let rows = read_funding(&payments());
        assert!((rows[0].effect().unwrap() - -0.4213).abs() < 1e-12, "paid");
        assert!((rows[1].effect().unwrap() - 0.1).abs() < 1e-12, "received");
    }
}
